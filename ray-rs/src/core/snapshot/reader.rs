//! CSR Snapshot Reader - mmap-based snapshot reading
//!
//! Ported from src/core/snapshot-reader.ts

use crate::constants::*;
use crate::error::{RayError, Result};
use crate::types::*;
use crate::util::binary::*;
use crate::util::compression::{decompress_with_size, CompressionType};
use crate::util::crc::crc32c;
use crate::util::hash::xxhash64_string;
use crate::util::mmap::{map_file, Mmap};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;

fn section_count_for_version(version: u32) -> usize {
  if version >= 3 {
    SectionId::COUNT
  } else if version >= 2 {
    SectionId::COUNT_V2
  } else {
    SectionId::COUNT_V1
  }
}

// ============================================================================
// Snapshot Data Structure
// ============================================================================

/// Parsed snapshot data with cached section views
pub struct SnapshotData {
  /// Memory-mapped file data
  mmap: Arc<Mmap>,
  /// Parsed header
  pub header: SnapshotHeaderV1,
  /// Section table
  sections: Vec<SectionEntry>,
  /// Cache for decompressed sections
  decompressed_cache: RwLock<HashMap<SectionId, Vec<u8>>>,
}

/// Options for parsing a snapshot
#[derive(Debug, Clone, Default)]
pub struct ParseSnapshotOptions {
  /// Skip CRC validation (for performance when reading cached/trusted data)
  pub skip_crc_validation: bool,
}

impl SnapshotData {
  /// Load and mmap a snapshot file
  pub fn load(path: impl AsRef<Path>) -> Result<Self> {
    let file = File::open(path.as_ref())?;
    let mmap = map_file(&file)?;
    Self::parse(Arc::new(mmap), &ParseSnapshotOptions::default())
  }

  /// Load with options
  pub fn load_with_options(path: impl AsRef<Path>, options: &ParseSnapshotOptions) -> Result<Self> {
    let file = File::open(path.as_ref())?;
    let mmap = map_file(&file)?;
    Self::parse(Arc::new(mmap), options)
  }

  /// Parse snapshot from mmap buffer
  pub fn parse(mmap: Arc<Mmap>, options: &ParseSnapshotOptions) -> Result<Self> {
    let buffer = &mmap[..];

    if buffer.len() < SNAPSHOT_HEADER_SIZE {
      return Err(RayError::InvalidSnapshot(format!(
        "Snapshot too small: {} bytes",
        buffer.len()
      )));
    }

    // Parse header
    let magic = read_u32(buffer, 0);
    if magic != MAGIC_SNAPSHOT {
      return Err(RayError::InvalidMagic {
        expected: MAGIC_SNAPSHOT,
        got: magic,
      });
    }

    let version = read_u32(buffer, 4);
    let min_reader_version = read_u32(buffer, 8);

    if MIN_READER_SNAPSHOT < min_reader_version {
      return Err(RayError::VersionMismatch {
        required: min_reader_version,
        current: MIN_READER_SNAPSHOT,
      });
    }

    let flags = SnapshotFlags::from_bits_truncate(read_u32(buffer, 12));
    let generation = read_u64(buffer, 16);
    let created_unix_ns = read_u64(buffer, 24);
    let num_nodes = read_u64(buffer, 32);
    let num_edges = read_u64(buffer, 40);
    let max_node_id = read_u64(buffer, 48);
    let num_labels = read_u64(buffer, 56);
    let num_etypes = read_u64(buffer, 64);
    let num_propkeys = read_u64(buffer, 72);
    let num_strings = read_u64(buffer, 80);

    let header = SnapshotHeaderV1 {
      magic,
      version,
      min_reader_version,
      flags,
      generation,
      created_unix_ns,
      num_nodes,
      num_edges,
      max_node_id,
      num_labels,
      num_etypes,
      num_propkeys,
      num_strings,
    };

    let section_count = section_count_for_version(version);
    let section_table_size = section_count * SECTION_ENTRY_SIZE;

    if buffer.len() < SNAPSHOT_HEADER_SIZE + section_table_size {
      return Err(RayError::InvalidSnapshot(format!(
        "Snapshot too small for section table: {} bytes",
        buffer.len()
      )));
    }

    // Parse section table
    let mut sections = Vec::with_capacity(section_count);
    let mut offset = SNAPSHOT_HEADER_SIZE;

    for _ in 0..section_count {
      let section_offset = read_u64(buffer, offset);
      let section_length = read_u64(buffer, offset + 8);
      let compression = read_u32(buffer, offset + 16);
      let uncompressed_size = read_u32(buffer, offset + 20);

      sections.push(SectionEntry {
        offset: section_offset,
        length: section_length,
        compression,
        uncompressed_size,
      });
      offset += SECTION_ENTRY_SIZE;
    }

    // Calculate actual snapshot size from section table
    let mut max_section_end = SNAPSHOT_HEADER_SIZE + section_table_size;
    for section in &sections {
      if section.length > 0 {
        let section_end = section.offset as usize + section.length as usize;
        if section_end > max_section_end {
          max_section_end = section_end;
        }
      }
    }
    // Round up to 64-byte alignment
    let aligned_end = align_up(max_section_end, SECTION_ALIGNMENT);
    let actual_snapshot_size = aligned_end + 4; // +4 for CRC

    // Verify footer CRC
    if !options.skip_crc_validation {
      let crc_offset = if actual_snapshot_size <= buffer.len() {
        actual_snapshot_size - 4
      } else {
        buffer.len() - 4
      };
      let footer_crc = read_u32(buffer, crc_offset);
      let computed_crc = crc32c(&buffer[..crc_offset]);
      if footer_crc != computed_crc {
        return Err(RayError::CrcMismatch {
          stored: footer_crc,
          computed: computed_crc,
        });
      }
    }

    Ok(Self {
      mmap,
      header,
      sections,
      decompressed_cache: RwLock::new(HashMap::new()),
    })
  }

  /// Parse snapshot from mmap buffer at a specific byte offset
  /// Used for single-file format where snapshot is embedded after header+WAL
  pub fn parse_at_offset(
    mmap: Arc<Mmap>,
    offset: usize,
    options: &ParseSnapshotOptions,
  ) -> Result<Self> {
    let buffer = &mmap[offset..];

    if buffer.len() < SNAPSHOT_HEADER_SIZE {
      return Err(RayError::InvalidSnapshot(format!(
        "Snapshot too small: {} bytes",
        buffer.len()
      )));
    }

    // Parse header
    let magic = read_u32(buffer, 0);
    if magic != MAGIC_SNAPSHOT {
      return Err(RayError::InvalidMagic {
        expected: MAGIC_SNAPSHOT,
        got: magic,
      });
    }

    let version = read_u32(buffer, 4);
    let min_reader_version = read_u32(buffer, 8);

    if MIN_READER_SNAPSHOT < min_reader_version {
      return Err(RayError::VersionMismatch {
        required: min_reader_version,
        current: MIN_READER_SNAPSHOT,
      });
    }

    let flags = SnapshotFlags::from_bits_truncate(read_u32(buffer, 12));
    let generation = read_u64(buffer, 16);
    let created_unix_ns = read_u64(buffer, 24);
    let num_nodes = read_u64(buffer, 32);
    let num_edges = read_u64(buffer, 40);
    let max_node_id = read_u64(buffer, 48);
    let num_labels = read_u64(buffer, 56);
    let num_etypes = read_u64(buffer, 64);
    let num_propkeys = read_u64(buffer, 72);
    let num_strings = read_u64(buffer, 80);

    let header = SnapshotHeaderV1 {
      magic,
      version,
      min_reader_version,
      flags,
      generation,
      created_unix_ns,
      num_nodes,
      num_edges,
      max_node_id,
      num_labels,
      num_etypes,
      num_propkeys,
      num_strings,
    };

    let section_count = section_count_for_version(version);
    let section_table_size = section_count * SECTION_ENTRY_SIZE;

    if buffer.len() < SNAPSHOT_HEADER_SIZE + section_table_size {
      return Err(RayError::InvalidSnapshot(format!(
        "Snapshot too small for section table: {} bytes",
        buffer.len()
      )));
    }

    // Parse section table
    let mut sections = Vec::with_capacity(section_count);
    let mut table_offset = SNAPSHOT_HEADER_SIZE;

    for _ in 0..section_count {
      let section_offset = read_u64(buffer, table_offset);
      let section_length = read_u64(buffer, table_offset + 8);
      let compression = read_u32(buffer, table_offset + 16);
      let uncompressed_size = read_u32(buffer, table_offset + 20);

      // Adjust section offset to be relative to file start
      sections.push(SectionEntry {
        offset: section_offset + offset as u64,
        length: section_length,
        compression,
        uncompressed_size,
      });
      table_offset += SECTION_ENTRY_SIZE;
    }

    // Verify footer CRC (optional)
    if !options.skip_crc_validation {
      // Calculate actual snapshot size from section table
      let mut max_section_end = SNAPSHOT_HEADER_SIZE + section_table_size;
      for section in &sections {
        if section.length > 0 {
          // Section offsets are now absolute (includes base offset)
          let section_end = section.offset as usize - offset + section.length as usize;
          if section_end > max_section_end {
            max_section_end = section_end;
          }
        }
      }
      let aligned_end = align_up(max_section_end, SECTION_ALIGNMENT);
      let actual_snapshot_size = aligned_end + 4;

      if actual_snapshot_size <= buffer.len() {
        let footer_crc = read_u32(buffer, actual_snapshot_size - 4);
        let computed_crc = crc32c(&buffer[..actual_snapshot_size - 4]);
        if footer_crc != computed_crc {
          return Err(RayError::CrcMismatch {
            stored: footer_crc,
            computed: computed_crc,
          });
        }
      }
    }

    Ok(Self {
      mmap,
      header,
      sections,
      decompressed_cache: RwLock::new(HashMap::new()),
    })
  }

  /// Get raw section bytes (possibly compressed)
  fn raw_section_bytes(&self, id: SectionId) -> Option<&[u8]> {
    let section = self.sections.get(id as usize)?;
    if section.length == 0 {
      return None;
    }
    let start = section.offset as usize;
    let end = start + section.length as usize;
    Some(&self.mmap[start..end])
  }

  /// Get decompressed section bytes
  pub fn section_bytes(&self, id: SectionId) -> Option<Vec<u8>> {
    let section = self.sections.get(id as usize)?;
    if section.length == 0 {
      return None;
    }

    // Check cache first
    {
      let cache = self.decompressed_cache.read();
      if let Some(cached) = cache.get(&id) {
        return Some(cached.clone());
      }
    }

    let raw_bytes = self.raw_section_bytes(id)?;

    // If not compressed, return copy of raw bytes
    let compression =
      CompressionType::from_u32(section.compression).unwrap_or(CompressionType::None);

    if compression == CompressionType::None {
      return Some(raw_bytes.to_vec());
    }

    // Decompress
    let decompressed =
      decompress_with_size(raw_bytes, compression, section.uncompressed_size as usize).ok()?;

    // Cache the result
    {
      let mut cache = self.decompressed_cache.write();
      cache.insert(id, decompressed.clone());
    }

    Some(decompressed)
  }

  /// Get section bytes as a slice (for uncompressed or already-cached sections)
  /// Returns None if section doesn't exist or is compressed and not cached
  pub fn section_slice(&self, id: SectionId) -> Option<&[u8]> {
    let section = self.sections.get(id as usize)?;
    if section.length == 0 {
      return None;
    }

    // Only return direct slice for uncompressed sections
    if section.compression == 0 {
      return self.raw_section_bytes(id);
    }

    None
  }

  // ========================================================================
  // Node accessors
  // ========================================================================

  /// Get NodeID for a physical node index
  #[inline]
  pub fn get_node_id(&self, phys: PhysNode) -> Option<NodeId> {
    let section = self.section_slice(SectionId::PhysToNodeId)?;
    if (phys as usize) * 8 + 8 > section.len() {
      return None;
    }
    Some(read_u64_at(section, phys as usize))
  }

  /// Get physical node index for a NodeID, or None if not present
  #[inline]
  pub fn get_phys_node(&self, node_id: NodeId) -> Option<PhysNode> {
    let section = self.section_slice(SectionId::NodeIdToPhys)?;
    let idx = node_id as usize;
    if idx * 4 + 4 > section.len() {
      return None;
    }
    let phys = read_i32_at(section, idx);
    if phys < 0 {
      None
    } else {
      Some(phys as PhysNode)
    }
  }

  /// Check if a NodeID exists in the snapshot
  #[inline]
  pub fn has_node(&self, node_id: NodeId) -> bool {
    self.get_phys_node(node_id).is_some()
  }

  /// Get the number of nodes in the snapshot
  #[inline]
  pub fn num_nodes(&self) -> u64 {
    self.header.num_nodes
  }

  /// Get the number of edges in the snapshot
  #[inline]
  pub fn num_edges(&self) -> u64 {
    self.header.num_edges
  }

  /// Get max node ID in the snapshot
  #[inline]
  pub fn max_node_id(&self) -> u64 {
    self.header.max_node_id
  }

  // ========================================================================
  // String table accessors
  // ========================================================================

  /// Get string by StringID
  pub fn get_string(&self, string_id: StringId) -> Option<String> {
    if string_id == 0 {
      return Some(String::new());
    }

    let offsets = self.section_slice(SectionId::StringOffsets)?;
    let bytes = self.section_slice(SectionId::StringBytes)?;

    let idx = string_id as usize;
    if idx * 4 + 8 > offsets.len() {
      return None;
    }

    let start = read_u32_at(offsets, idx) as usize;
    let end = read_u32_at(offsets, idx + 1) as usize;

    if end > bytes.len() {
      return None;
    }

    String::from_utf8(bytes[start..end].to_vec()).ok()
  }

  // ========================================================================
  // Edge accessors
  // ========================================================================

  /// Get out-edge offset range for a physical node
  fn out_edge_range(&self, phys: PhysNode) -> Option<(usize, usize)> {
    let offsets = self.section_slice(SectionId::OutOffsets)?;
    let idx = phys as usize;
    if idx * 4 + 8 > offsets.len() {
      return None;
    }
    let start = read_u32_at(offsets, idx) as usize;
    let end = read_u32_at(offsets, idx + 1) as usize;
    Some((start, end))
  }

  /// Get out-degree for a physical node
  pub fn get_out_degree(&self, phys: PhysNode) -> Option<usize> {
    let (start, end) = self.out_edge_range(phys)?;
    Some(end - start)
  }

  /// Check if an edge exists in the snapshot (binary search)
  pub fn has_edge(&self, src_phys: PhysNode, etype: ETypeId, dst_phys: PhysNode) -> bool {
    let (start, end) = match self.out_edge_range(src_phys) {
      Some(range) => range,
      None => return false,
    };

    let out_etype = match self.section_slice(SectionId::OutEtype) {
      Some(s) => s,
      None => return false,
    };
    let out_dst = match self.section_slice(SectionId::OutDst) {
      Some(s) => s,
      None => return false,
    };

    // Binary search since edges are sorted by (etype, dst)
    let mut lo = start;
    let mut hi = end;

    while lo < hi {
      let mid = (lo + hi) / 2;
      let mid_etype = read_u32_at(out_etype, mid);
      let mid_dst = read_u32_at(out_dst, mid);

      if mid_etype < etype || (mid_etype == etype && mid_dst < dst_phys) {
        lo = mid + 1;
      } else {
        hi = mid;
      }
    }

    if lo < end {
      let found_etype = read_u32_at(out_etype, lo);
      let found_dst = read_u32_at(out_dst, lo);
      found_etype == etype && found_dst == dst_phys
    } else {
      false
    }
  }

  /// Find edge index for a specific edge (returns None if not found)
  pub fn find_edge_index(
    &self,
    src_phys: PhysNode,
    etype: ETypeId,
    dst_phys: PhysNode,
  ) -> Option<usize> {
    let (start, end) = self.out_edge_range(src_phys)?;
    let out_etype = self.section_slice(SectionId::OutEtype)?;
    let out_dst = self.section_slice(SectionId::OutDst)?;

    // Binary search
    let mut lo = start;
    let mut hi = end;

    while lo < hi {
      let mid = (lo + hi) / 2;
      let mid_etype = read_u32_at(out_etype, mid);
      let mid_dst = read_u32_at(out_dst, mid);

      if mid_etype < etype || (mid_etype == etype && mid_dst < dst_phys) {
        lo = mid + 1;
      } else {
        hi = mid;
      }
    }

    if lo < end {
      let found_etype = read_u32_at(out_etype, lo);
      let found_dst = read_u32_at(out_dst, lo);
      if found_etype == etype && found_dst == dst_phys {
        return Some(lo);
      }
    }

    None
  }

  /// Iterate out-edges for a physical node
  pub fn iter_out_edges(&self, phys: PhysNode) -> OutEdgeIter<'_> {
    OutEdgeIter::new(self, phys)
  }

  /// Get in-edge offset range for a physical node
  fn in_edge_range(&self, phys: PhysNode) -> Option<(usize, usize)> {
    if !self.header.flags.contains(SnapshotFlags::HAS_IN_EDGES) {
      return None;
    }
    let offsets = self.section_slice(SectionId::InOffsets)?;
    let idx = phys as usize;
    if idx * 4 + 8 > offsets.len() {
      return None;
    }
    let start = read_u32_at(offsets, idx) as usize;
    let end = read_u32_at(offsets, idx + 1) as usize;
    Some((start, end))
  }

  /// Get in-degree for a physical node
  pub fn get_in_degree(&self, phys: PhysNode) -> Option<usize> {
    let (start, end) = self.in_edge_range(phys)?;
    Some(end - start)
  }

  /// Iterate in-edges for a physical node
  pub fn iter_in_edges(&self, phys: PhysNode) -> InEdgeIter<'_> {
    InEdgeIter::new(self, phys)
  }

  // ========================================================================
  // Key index lookup
  // ========================================================================

  /// Look up a node by key in the snapshot
  pub fn lookup_by_key(&self, key: &str) -> Option<NodeId> {
    let hash64 = xxhash64_string(key);

    let key_entries = self.section_slice(SectionId::KeyEntries)?;
    let num_entries = key_entries.len() / KEY_INDEX_ENTRY_SIZE;
    if num_entries == 0 {
      return None;
    }

    let (lo, hi) = if let Some(buckets) = self.section_slice(SectionId::KeyBuckets) {
      if buckets.len() > 4 {
        let num_buckets = buckets.len() / 4 - 1;
        let bucket = (hash64 % num_buckets as u64) as usize;
        let lo = read_u32_at(buckets, bucket) as usize;
        let hi = read_u32_at(buckets, bucket + 1) as usize;
        (lo, hi)
      } else {
        self.binary_search_key_hash(key_entries, hash64, num_entries)
      }
    } else {
      self.binary_search_key_hash(key_entries, hash64, num_entries)
    };

    // Check all entries in range with matching hash (handle collisions)
    for i in lo..hi {
      let offset = i * KEY_INDEX_ENTRY_SIZE;
      let entry_hash = read_u64(key_entries, offset);

      if entry_hash != hash64 {
        continue;
      }

      let string_id = read_u32(key_entries, offset + 8);
      let node_id = read_u64(key_entries, offset + 16);

      // Compare actual key
      if let Some(entry_key) = self.get_string(string_id) {
        if entry_key == key {
          return Some(node_id);
        }
      }
    }

    None
  }

  /// Binary search for first entry with matching hash
  fn binary_search_key_hash(
    &self,
    entries: &[u8],
    hash64: u64,
    num_entries: usize,
  ) -> (usize, usize) {
    let mut lo = 0;
    let mut hi = num_entries;

    while lo < hi {
      let mid = (lo + hi) / 2;
      let mid_hash = read_u64(entries, mid * KEY_INDEX_ENTRY_SIZE);
      if mid_hash < hash64 {
        lo = mid + 1;
      } else {
        hi = mid;
      }
    }

    (lo, num_entries)
  }

  /// Get the key for a node, if any
  pub fn get_node_key(&self, phys: PhysNode) -> Option<String> {
    let node_key_string = self.section_slice(SectionId::NodeKeyString)?;
    let idx = phys as usize;
    if idx * 4 + 4 > node_key_string.len() {
      return None;
    }
    let string_id = read_u32_at(node_key_string, idx);
    if string_id == 0 {
      return None;
    }
    self.get_string(string_id)
  }

  // ========================================================================
  // Label access
  // ========================================================================

  /// Get all labels for a node
  pub fn get_node_labels(&self, phys: PhysNode) -> Option<Vec<LabelId>> {
    if !self.header.flags.contains(SnapshotFlags::HAS_NODE_LABELS) {
      return None;
    }

    let offsets = self.section_slice(SectionId::NodeLabelOffsets)?;
    let labels = self.section_slice(SectionId::NodeLabelIds)?;

    let idx = phys as usize;
    if idx * 4 + 8 > offsets.len() {
      return None;
    }

    let start = read_u32_at(offsets, idx) as usize;
    let end = read_u32_at(offsets, idx + 1) as usize;

    let mut out = Vec::with_capacity(end.saturating_sub(start));
    for i in start..end {
      if i * 4 + 4 > labels.len() {
        break;
      }
      out.push(read_u32_at(labels, i) as LabelId);
    }

    Some(out)
  }

  // ========================================================================
  // Property access
  // ========================================================================

  /// Get all properties for a node
  pub fn get_node_props(&self, phys: PhysNode) -> Option<HashMap<PropKeyId, PropValue>> {
    if !self.header.flags.contains(SnapshotFlags::HAS_PROPERTIES) {
      return None;
    }

    let offsets = self.section_slice(SectionId::NodePropOffsets)?;
    let keys = self.section_slice(SectionId::NodePropKeys)?;
    let vals = self.section_slice(SectionId::NodePropVals)?;

    let idx = phys as usize;
    if idx * 4 + 8 > offsets.len() {
      return None;
    }

    let start = read_u32_at(offsets, idx) as usize;
    let end = read_u32_at(offsets, idx + 1) as usize;

    let mut props = HashMap::new();
    for i in start..end {
      if i * 4 + 4 > keys.len() {
        break;
      }
      let key_id = read_u32_at(keys, i);
      if let Some(value) = self.decode_prop_value(vals, i * PROP_VALUE_DISK_SIZE) {
        props.insert(key_id, value);
      }
    }

    Some(props)
  }

  /// Get a specific property for a node
  pub fn get_node_prop(&self, phys: PhysNode, prop_key_id: PropKeyId) -> Option<PropValue> {
    if !self.header.flags.contains(SnapshotFlags::HAS_PROPERTIES) {
      return None;
    }

    let offsets = self.section_slice(SectionId::NodePropOffsets)?;
    let keys = self.section_slice(SectionId::NodePropKeys)?;
    let vals = self.section_slice(SectionId::NodePropVals)?;

    let idx = phys as usize;
    if idx * 4 + 8 > offsets.len() {
      return None;
    }

    let start = read_u32_at(offsets, idx) as usize;
    let end = read_u32_at(offsets, idx + 1) as usize;

    for i in start..end {
      if i * 4 + 4 > keys.len() {
        break;
      }
      let key_id = read_u32_at(keys, i);
      if key_id == prop_key_id {
        return self.decode_prop_value(vals, i * PROP_VALUE_DISK_SIZE);
      }
    }

    None
  }

  /// Get all properties for an edge by edge index
  pub fn get_edge_props(&self, edge_idx: usize) -> Option<HashMap<PropKeyId, PropValue>> {
    if !self.header.flags.contains(SnapshotFlags::HAS_PROPERTIES) {
      return None;
    }

    let offsets = self.section_slice(SectionId::EdgePropOffsets)?;
    let keys = self.section_slice(SectionId::EdgePropKeys)?;
    let vals = self.section_slice(SectionId::EdgePropVals)?;

    if edge_idx * 4 + 8 > offsets.len() {
      return None;
    }

    let start = read_u32_at(offsets, edge_idx) as usize;
    let end = read_u32_at(offsets, edge_idx + 1) as usize;

    let mut props = HashMap::new();
    for i in start..end {
      if i * 4 + 4 > keys.len() {
        break;
      }
      let key_id = read_u32_at(keys, i);
      if let Some(value) = self.decode_prop_value(vals, i * PROP_VALUE_DISK_SIZE) {
        props.insert(key_id, value);
      }
    }

    Some(props)
  }

  /// Decode a property value from disk format
  fn decode_prop_value(&self, vals: &[u8], offset: usize) -> Option<PropValue> {
    if offset + PROP_VALUE_DISK_SIZE > vals.len() {
      return None;
    }

    let tag = vals[offset];
    let payload = read_u64(vals, offset + 8);

    match PropValueTag::from_u8(tag)? {
      PropValueTag::Null => Some(PropValue::Null),
      PropValueTag::Bool => Some(PropValue::Bool(payload != 0)),
      PropValueTag::I64 => Some(PropValue::I64(payload as i64)),
      PropValueTag::F64 => Some(PropValue::F64(f64::from_bits(payload))),
      PropValueTag::String => {
        let s = self.get_string(payload as u32)?;
        Some(PropValue::String(s))
      }
      PropValueTag::VectorF32 => {
        if !self.header.flags.contains(SnapshotFlags::HAS_VECTORS) {
          return None;
        }

        let offsets = self.section_slice(SectionId::VectorOffsets)?;
        let data = self.section_slice(SectionId::VectorData)?;

        let idx = payload as usize;
        if (idx + 1) * 8 > offsets.len() {
          return None;
        }

        let start = read_u64_at(offsets, idx) as usize;
        let end = read_u64_at(offsets, idx + 1) as usize;
        if start > end || end > data.len() {
          return None;
        }
        let bytes = &data[start..end];
        if bytes.len() % 4 != 0 {
          return None;
        }

        let mut vec = Vec::with_capacity(bytes.len() / 4);
        for chunk in bytes.chunks_exact(4) {
          let val = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
          vec.push(val);
        }

        Some(PropValue::VectorF32(vec))
      }
    }
  }
}

// ============================================================================
// Edge Iterators
// ============================================================================

/// Iterator over out-edges
pub struct OutEdgeIter<'a> {
  snapshot: &'a SnapshotData,
  out_etype: Option<&'a [u8]>,
  out_dst: Option<&'a [u8]>,
  current: usize,
  end: usize,
}

impl<'a> OutEdgeIter<'a> {
  fn new(snapshot: &'a SnapshotData, phys: PhysNode) -> Self {
    let (current, end) = snapshot.out_edge_range(phys).unwrap_or((0, 0));
    Self {
      snapshot,
      out_etype: snapshot.section_slice(SectionId::OutEtype),
      out_dst: snapshot.section_slice(SectionId::OutDst),
      current,
      end,
    }
  }
}

impl<'a> Iterator for OutEdgeIter<'a> {
  type Item = (PhysNode, ETypeId); // (dst, etype)

  fn next(&mut self) -> Option<Self::Item> {
    if self.current >= self.end {
      return None;
    }

    let out_etype = self.out_etype?;
    let out_dst = self.out_dst?;

    if self.current * 4 + 4 > out_etype.len() || self.current * 4 + 4 > out_dst.len() {
      return None;
    }

    let dst = read_u32_at(out_dst, self.current);
    let etype = read_u32_at(out_etype, self.current);
    self.current += 1;

    Some((dst, etype))
  }

  fn size_hint(&self) -> (usize, Option<usize>) {
    let remaining = self.end.saturating_sub(self.current);
    (remaining, Some(remaining))
  }
}

impl<'a> ExactSizeIterator for OutEdgeIter<'a> {}

/// Iterator over in-edges
pub struct InEdgeIter<'a> {
  snapshot: &'a SnapshotData,
  in_etype: Option<&'a [u8]>,
  in_src: Option<&'a [u8]>,
  in_out_index: Option<&'a [u8]>,
  current: usize,
  end: usize,
}

impl<'a> InEdgeIter<'a> {
  fn new(snapshot: &'a SnapshotData, phys: PhysNode) -> Self {
    let (current, end) = snapshot.in_edge_range(phys).unwrap_or((0, 0));
    Self {
      snapshot,
      in_etype: snapshot.section_slice(SectionId::InEtype),
      in_src: snapshot.section_slice(SectionId::InSrc),
      in_out_index: snapshot.section_slice(SectionId::InOutIndex),
      current,
      end,
    }
  }
}

impl<'a> Iterator for InEdgeIter<'a> {
  type Item = (PhysNode, ETypeId, u32); // (src, etype, out_index)

  fn next(&mut self) -> Option<Self::Item> {
    if self.current >= self.end {
      return None;
    }

    let in_etype = self.in_etype?;
    let in_src = self.in_src?;

    if self.current * 4 + 4 > in_etype.len() || self.current * 4 + 4 > in_src.len() {
      return None;
    }

    let src = read_u32_at(in_src, self.current);
    let etype = read_u32_at(in_etype, self.current);
    let out_index = self
      .in_out_index
      .and_then(|idx| {
        if self.current * 4 + 4 <= idx.len() {
          Some(read_u32_at(idx, self.current))
        } else {
          None
        }
      })
      .unwrap_or(0);

    self.current += 1;

    Some((src, etype, out_index))
  }

  fn size_hint(&self) -> (usize, Option<usize>) {
    let remaining = self.end.saturating_sub(self.current);
    (remaining, Some(remaining))
  }
}

impl<'a> ExactSizeIterator for InEdgeIter<'a> {}

// ============================================================================
// Extended SnapshotData methods for compaction
// ============================================================================

/// Out-edge info for compaction
pub struct OutEdgeInfo {
  pub dst: PhysNode,
  pub etype: ETypeId,
}

impl SnapshotData {
  /// Get label name by LabelID
  pub fn get_label_name(&self, label_id: LabelId) -> Option<&str> {
    let label_string_ids = self.section_slice(SectionId::LabelStringIds)?;
    let idx = label_id as usize;
    if idx * 4 + 4 > label_string_ids.len() {
      return None;
    }
    let string_id = read_u32_at(label_string_ids, idx);
    if string_id == 0 {
      return None;
    }
    // Return as owned String converted to &str via lifetime extension
    // This is a workaround - ideally we'd have a string cache
    self.get_string(string_id).map(|s| {
      // Leak the string to extend lifetime - only safe for short-lived operations
      // In production, this should use a proper string cache
      Box::leak(s.into_boxed_str()) as &str
    })
  }

  /// Get etype name by ETypeID
  pub fn get_etype_name(&self, etype_id: ETypeId) -> Option<&str> {
    let etype_string_ids = self.section_slice(SectionId::EtypeStringIds)?;
    let idx = etype_id as usize;
    if idx * 4 + 4 > etype_string_ids.len() {
      return None;
    }
    let string_id = read_u32_at(etype_string_ids, idx);
    if string_id == 0 {
      return None;
    }
    self
      .get_string(string_id)
      .map(|s| Box::leak(s.into_boxed_str()) as &str)
  }

  /// Get propkey name by PropKeyID
  pub fn get_propkey_name(&self, propkey_id: PropKeyId) -> Option<&str> {
    let propkey_string_ids = self.section_slice(SectionId::PropkeyStringIds)?;
    let idx = propkey_id as usize;
    if idx * 4 + 4 > propkey_string_ids.len() {
      return None;
    }
    let string_id = read_u32_at(propkey_string_ids, idx);
    if string_id == 0 {
      return None;
    }
    self
      .get_string(string_id)
      .map(|s| Box::leak(s.into_boxed_str()) as &str)
  }

  /// Get out-edges as a Vec for compaction purposes
  pub fn get_out_edges(&self, phys: PhysNode) -> Vec<OutEdgeInfo> {
    let mut edges = Vec::new();
    for (dst, etype) in self.iter_out_edges(phys) {
      edges.push(OutEdgeInfo { dst, etype });
    }
    edges
  }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_parse_snapshot_options_default() {
    let opts = ParseSnapshotOptions::default();
    assert!(!opts.skip_crc_validation);
  }
}
