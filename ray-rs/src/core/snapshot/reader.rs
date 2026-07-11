//! CSR Snapshot Reader - mmap-based snapshot reading
//!
//! Ported from src/core/snapshot-reader.ts

use crate::constants::*;
use crate::core::snapshot::sections::{parse_section_table, section_count_for_version};
use crate::error::{KiteError, Result};
use crate::types::*;
use crate::util::binary::*;
use crate::util::compression::{decompress_with_size, CompressionType};
use crate::util::crc::{crc32c, crc32c_chunked, Crc32cHasher};
use crate::util::hash::xxhash64_string;
use crate::util::mmap::{map_file, Mmap};
use parking_lot::RwLock;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

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
  decompressed_cache: RwLock<HashMap<SectionId, Arc<[u8]>>>,
  /// Cache for string table entries (indexed by StringId)
  string_cache: Vec<OnceLock<Arc<str>>>,
}

/// Borrowed or shared section bytes.
#[derive(Clone)]
pub enum SectionBytes<'a> {
  Borrowed(&'a [u8]),
  Shared(Arc<[u8]>),
}

impl SectionBytes<'_> {}

impl AsRef<[u8]> for SectionBytes<'_> {
  fn as_ref(&self) -> &[u8] {
    match self {
      SectionBytes::Borrowed(bytes) => bytes,
      SectionBytes::Shared(bytes) => bytes.as_ref(),
    }
  }
}

/// Options for parsing a snapshot
#[derive(Debug, Clone, Default)]
pub struct ParseSnapshotOptions {
  /// Skip CRC validation (for performance when reading cached/trusted data)
  pub skip_crc_validation: bool,
  /// Optional CRC chunk size for throughput experiments.
  /// `None` or `Some(0)` uses the default whole-buffer CRC path.
  pub crc_chunk_size: Option<usize>,
  /// Optional sink to capture CRC section attribution (bytes + time).
  pub crc_profile_sink: Option<Arc<Mutex<Option<SnapshotCrcProfile>>>>,
}

/// Per-segment CRC timing attribution.
///
/// `section_id == None` means bytes outside section payloads (header/table/alignment gaps).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotCrcSectionProfile {
  pub section_id: Option<SectionId>,
  pub bytes: usize,
  pub crc_ns: u64,
}

/// Snapshot CRC profile captured while validating footer checksum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotCrcProfile {
  pub total_bytes: usize,
  pub total_ns: u64,
  pub chunk_size: usize,
  pub sections: Vec<SnapshotCrcSectionProfile>,
}

#[derive(Debug, Clone, Copy)]
struct CrcSegment {
  section_id: Option<SectionId>,
  start: usize,
  end: usize,
}

fn normalized_crc_chunk_size(requested: Option<usize>, total_len: usize) -> usize {
  let Some(size) = requested else {
    return total_len.max(1);
  };
  if size == 0 {
    total_len.max(1)
  } else {
    size
  }
}

fn section_segments(
  sections: &[SectionEntry],
  base_offset: usize,
  crc_len: usize,
) -> Vec<CrcSegment> {
  let mut payload_ranges: Vec<(usize, usize, Option<SectionId>)> = Vec::new();

  for (idx, section) in sections.iter().enumerate() {
    if section.length == 0 {
      continue;
    }

    let global_start = section.offset as usize;
    let Some(local_start) = global_start.checked_sub(base_offset) else {
      continue;
    };
    let local_end = local_start.saturating_add(section.length as usize);
    if local_start >= crc_len {
      continue;
    }

    payload_ranges.push((
      local_start,
      local_end.min(crc_len),
      SectionId::from_u32(idx as u32),
    ));
  }

  payload_ranges.sort_by_key(|(start, _, _)| *start);
  let mut segments = Vec::with_capacity(payload_ranges.len().saturating_mul(2).saturating_add(1));
  let mut cursor = 0usize;

  for (start, end, section_id) in payload_ranges {
    if start > cursor {
      segments.push(CrcSegment {
        section_id: None,
        start: cursor,
        end: start,
      });
    }
    if end > start {
      segments.push(CrcSegment {
        section_id,
        start,
        end,
      });
      cursor = end;
    }
  }

  if cursor < crc_len {
    segments.push(CrcSegment {
      section_id: None,
      start: cursor,
      end: crc_len,
    });
  }

  segments
}

fn compute_crc_with_options(
  data: &[u8],
  options: &ParseSnapshotOptions,
  sections: &[SectionEntry],
  base_offset: usize,
) -> (u32, Option<SnapshotCrcProfile>) {
  let chunk_size = normalized_crc_chunk_size(options.crc_chunk_size, data.len());
  if options.crc_profile_sink.is_none() {
    if chunk_size >= data.len().max(1) {
      return (crc32c(data), None);
    }
    return (crc32c_chunked(data, chunk_size), None);
  }

  let segments = section_segments(sections, base_offset, data.len());
  let mut hasher = Crc32cHasher::new();
  let mut profile_sections = Vec::with_capacity(segments.len());
  let mut total_ns: u64 = 0;

  for segment in segments {
    let bytes = &data[segment.start..segment.end];
    if bytes.is_empty() {
      continue;
    }

    let segment_start = std::time::Instant::now();
    if chunk_size >= bytes.len() {
      hasher.update(bytes);
    } else {
      for chunk in bytes.chunks(chunk_size) {
        hasher.update(chunk);
      }
    }
    let crc_ns = segment_start.elapsed().as_nanos() as u64;
    total_ns = total_ns.saturating_add(crc_ns);
    profile_sections.push(SnapshotCrcSectionProfile {
      section_id: segment.section_id,
      bytes: bytes.len(),
      crc_ns,
    });
  }

  (
    hasher.finalize(),
    Some(SnapshotCrcProfile {
      total_bytes: data.len(),
      total_ns,
      chunk_size,
      sections: profile_sections,
    }),
  )
}

impl SnapshotData {
  fn from_parsed_parts(
    mmap: Arc<Mmap>,
    header: SnapshotHeaderV1,
    sections: Vec<SectionEntry>,
  ) -> Result<Self> {
    let num_strings = header.num_strings;
    let mut snapshot = Self {
      mmap,
      header,
      sections,
      decompressed_cache: RwLock::new(HashMap::new()),
      string_cache: Vec::new(),
    };

    snapshot.validate_structure()?;
    snapshot.string_cache = Self::init_string_cache(num_strings)?;
    Ok(snapshot)
  }

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
      return Err(KiteError::InvalidSnapshot(format!(
        "Snapshot too small: {} bytes",
        buffer.len()
      )));
    }

    // Parse header
    let magic = read_u32(buffer, 0);
    if magic != MAGIC_SNAPSHOT {
      return Err(KiteError::InvalidMagic {
        expected: MAGIC_SNAPSHOT,
        got: magic,
      });
    }

    let version = read_u32(buffer, 4);
    let min_reader_version = read_u32(buffer, 8);

    if MIN_READER_SNAPSHOT < min_reader_version {
      return Err(KiteError::VersionMismatch {
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
    let parsed = parse_section_table(buffer, section_count, 0)?;
    let sections = parsed.sections;
    let aligned_end = parsed
      .max_section_end
      .checked_add(SECTION_ALIGNMENT - 1)
      .map(|value| value & !(SECTION_ALIGNMENT - 1))
      .ok_or_else(|| KiteError::InvalidSnapshot("Snapshot size alignment overflow".to_string()))?;
    let actual_snapshot_size = aligned_end
      .checked_add(4)
      .ok_or_else(|| KiteError::InvalidSnapshot("Snapshot CRC offset overflow".to_string()))?;

    if actual_snapshot_size > buffer.len() {
      return Err(KiteError::InvalidSnapshot(format!(
        "Snapshot truncated: expected {actual_snapshot_size} bytes, found {}",
        buffer.len()
      )));
    }

    // Verify footer CRC
    if !options.skip_crc_validation {
      let footer_crc = read_u32(buffer, actual_snapshot_size - 4);
      let (computed_crc, crc_profile) =
        compute_crc_with_options(&buffer[..actual_snapshot_size - 4], options, &sections, 0);
      if let Some(sink) = options.crc_profile_sink.as_ref() {
        if let Ok(mut guard) = sink.lock() {
          *guard = crc_profile;
        }
      }
      if footer_crc != computed_crc {
        return Err(KiteError::CrcMismatch {
          stored: footer_crc,
          computed: computed_crc,
        });
      }
    }

    Self::from_parsed_parts(mmap, header, sections)
  }

  /// Parse snapshot from mmap buffer at a specific byte offset
  /// Used for single-file format where snapshot is embedded after header+WAL
  pub fn parse_at_offset(
    mmap: Arc<Mmap>,
    offset: usize,
    options: &ParseSnapshotOptions,
  ) -> Result<Self> {
    if offset > mmap.len() {
      return Err(KiteError::InvalidSnapshot(format!(
        "Snapshot offset {offset} exceeds mmap length {}",
        mmap.len()
      )));
    }
    let buffer = &mmap[offset..];

    if buffer.len() < SNAPSHOT_HEADER_SIZE {
      return Err(KiteError::InvalidSnapshot(format!(
        "Snapshot too small: {} bytes",
        buffer.len()
      )));
    }

    // Parse header
    let magic = read_u32(buffer, 0);
    if magic != MAGIC_SNAPSHOT {
      return Err(KiteError::InvalidMagic {
        expected: MAGIC_SNAPSHOT,
        got: magic,
      });
    }

    let version = read_u32(buffer, 4);
    let min_reader_version = read_u32(buffer, 8);

    if MIN_READER_SNAPSHOT < min_reader_version {
      return Err(KiteError::VersionMismatch {
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
    let parsed = parse_section_table(buffer, section_count, offset)?;
    let sections = parsed.sections;
    let aligned_end = parsed
      .max_section_end
      .checked_add(SECTION_ALIGNMENT - 1)
      .map(|value| value & !(SECTION_ALIGNMENT - 1))
      .ok_or_else(|| KiteError::InvalidSnapshot("Snapshot size alignment overflow".to_string()))?;
    let actual_snapshot_size = aligned_end
      .checked_add(4)
      .ok_or_else(|| KiteError::InvalidSnapshot("Snapshot CRC offset overflow".to_string()))?;

    if actual_snapshot_size > buffer.len() {
      return Err(KiteError::InvalidSnapshot(format!(
        "Snapshot truncated: expected {actual_snapshot_size} bytes, found {}",
        buffer.len()
      )));
    }

    // Verify footer CRC (optional)
    if !options.skip_crc_validation {
      let footer_crc = read_u32(buffer, actual_snapshot_size - 4);
      let (computed_crc, crc_profile) = compute_crc_with_options(
        &buffer[..actual_snapshot_size - 4],
        options,
        &sections,
        offset,
      );
      if let Some(sink) = options.crc_profile_sink.as_ref() {
        if let Ok(mut guard) = sink.lock() {
          *guard = crc_profile;
        }
      }
      if footer_crc != computed_crc {
        return Err(KiteError::CrcMismatch {
          stored: footer_crc,
          computed: computed_crc,
        });
      }
    }

    Self::from_parsed_parts(mmap, header, sections)
  }

  fn section_name(id: SectionId) -> &'static str {
    match id {
      SectionId::PhysToNodeId => "PhysToNodeId",
      SectionId::NodeIdToPhys => "NodeIdToPhys",
      SectionId::OutOffsets => "OutOffsets",
      SectionId::OutDst => "OutDst",
      SectionId::OutEtype => "OutEtype",
      SectionId::InOffsets => "InOffsets",
      SectionId::InSrc => "InSrc",
      SectionId::InEtype => "InEtype",
      SectionId::InOutIndex => "InOutIndex",
      SectionId::StringOffsets => "StringOffsets",
      SectionId::StringBytes => "StringBytes",
      SectionId::LabelStringIds => "LabelStringIds",
      SectionId::EtypeStringIds => "EtypeStringIds",
      SectionId::PropkeyStringIds => "PropkeyStringIds",
      SectionId::NodeKeyString => "NodeKeyString",
      SectionId::KeyEntries => "KeyEntries",
      SectionId::KeyBuckets => "KeyBuckets",
      SectionId::NodePropOffsets => "NodePropOffsets",
      SectionId::NodePropKeys => "NodePropKeys",
      SectionId::NodePropVals => "NodePropVals",
      SectionId::EdgePropOffsets => "EdgePropOffsets",
      SectionId::EdgePropKeys => "EdgePropKeys",
      SectionId::EdgePropVals => "EdgePropVals",
      SectionId::NodeLabelOffsets => "NodeLabelOffsets",
      SectionId::NodeLabelIds => "NodeLabelIds",
      SectionId::VectorOffsets => "VectorOffsets",
      SectionId::VectorData => "VectorData",
      SectionId::VectorStoreIndex => "VectorStoreIndex",
      SectionId::VectorStoreData => "VectorStoreData",
    }
  }

  fn invalid_section(section: &str, message: impl Into<String>) -> KiteError {
    KiteError::InvalidSnapshot(format!("{section} section: {}", message.into()))
  }

  fn checked_count(value: u64, section: &str) -> Result<usize> {
    usize::try_from(value)
      .map_err(|_| Self::invalid_section(section, "count does not fit in usize"))
  }

  fn checked_count_plus_one(value: u64, section: &str) -> Result<usize> {
    Self::checked_count(value, section)?
      .checked_add(1)
      .ok_or_else(|| Self::invalid_section(section, "count plus one overflows"))
  }

  fn checked_bytes(count: usize, element_size: usize, section: &str) -> Result<usize> {
    count
      .checked_mul(element_size)
      .ok_or_else(|| Self::invalid_section(section, "array size overflows"))
  }

  fn section_data_for_validation(
    &self,
    id: SectionId,
    required: bool,
  ) -> Result<Option<SectionBytes<'_>>> {
    let Some(section) = self.sections.get(id as usize) else {
      if required {
        return Err(Self::invalid_section(
          Self::section_name(id),
          "section is missing",
        ));
      }
      return Ok(None);
    };

    if section.length == 0 {
      if required {
        return Err(Self::invalid_section(
          Self::section_name(id),
          "section is empty",
        ));
      }
      return Ok(None);
    }

    self
      .section_data_shared(id)
      .map(Some)
      .ok_or_else(|| Self::invalid_section(Self::section_name(id), "cannot decode section"))
  }

  fn validate_exact_section(
    &self,
    id: SectionId,
    expected_len: usize,
    required: bool,
  ) -> Result<Option<SectionBytes<'_>>> {
    let data = self.section_data_for_validation(id, required)?;
    let actual_len = data.as_ref().map(|bytes| bytes.as_ref().len()).unwrap_or(0);
    if actual_len != expected_len {
      return Err(Self::invalid_section(
        Self::section_name(id),
        format!("expected {expected_len} bytes, found {actual_len}"),
      ));
    }
    Ok(data)
  }

  fn validate_u32_offsets(data: &[u8], end_limit: usize, section: &str) -> Result<()> {
    if data.len() % 4 != 0 {
      return Err(Self::invalid_section(
        section,
        "offset array is not a multiple of 4 bytes",
      ));
    }

    let mut previous = 0usize;
    for index in 0..data.len() / 4 {
      let value = read_u32_at(data, index) as usize;
      if value < previous {
        return Err(Self::invalid_section(
          section,
          format!("offset {index} is not monotonic: {value} < {previous}"),
        ));
      }
      if value > end_limit {
        return Err(Self::invalid_section(
          section,
          format!("offset {index} ({value}) exceeds end {end_limit}"),
        ));
      }
      previous = value;
    }
    Ok(())
  }

  fn validate_u64_offsets(data: &[u8], end_limit: usize, section: &str) -> Result<()> {
    if data.len() % 8 != 0 {
      return Err(Self::invalid_section(
        section,
        "offset array is not a multiple of 8 bytes",
      ));
    }

    let mut previous = 0usize;
    for index in 0..data.len() / 8 {
      let value = usize::try_from(read_u64_at(data, index)).map_err(|_| {
        Self::invalid_section(section, format!("offset {index} does not fit in usize"))
      })?;
      if value < previous {
        return Err(Self::invalid_section(
          section,
          format!("offset {index} is not monotonic: {value} < {previous}"),
        ));
      }
      if value > end_limit {
        return Err(Self::invalid_section(
          section,
          format!("offset {index} ({value}) exceeds end {end_limit}"),
        ));
      }
      previous = value;
    }
    Ok(())
  }

  fn validate_string_id_array(data: &[u8], num_strings: usize, section: &str) -> Result<()> {
    if data.len() % 4 != 0 {
      return Err(Self::invalid_section(
        section,
        "string ID array is not a multiple of 4 bytes",
      ));
    }
    for index in 0..data.len() / 4 {
      let string_id = read_u32_at(data, index) as usize;
      if string_id > num_strings {
        return Err(Self::invalid_section(
          section,
          format!("string ID {string_id} at index {index} exceeds {num_strings}"),
        ));
      }
    }
    Ok(())
  }

  fn validate_u32_values_below(data: &[u8], limit: usize, section: &str) -> Result<()> {
    if data.len() % 4 != 0 {
      return Err(Self::invalid_section(
        section,
        "value array is not a multiple of 4 bytes",
      ));
    }
    for index in 0..data.len() / 4 {
      let value = read_u32_at(data, index) as usize;
      if value >= limit {
        return Err(Self::invalid_section(
          section,
          format!("value {value} at index {index} is outside 0..{limit}"),
        ));
      }
    }
    Ok(())
  }

  fn validate_property_values(
    data: &[u8],
    num_strings: usize,
    vector_count: Option<usize>,
    section: &str,
  ) -> Result<()> {
    if data.len() % PROP_VALUE_DISK_SIZE != 0 {
      return Err(Self::invalid_section(
        section,
        "value array has a partial entry",
      ));
    }
    for index in 0..data.len() / PROP_VALUE_DISK_SIZE {
      let offset = index * PROP_VALUE_DISK_SIZE;
      let tag = data[offset];
      let payload = read_u64(data, offset + 8);
      match PropValueTag::from_u8(tag) {
        Some(PropValueTag::String) => {
          if payload > num_strings as u64 {
            return Err(Self::invalid_section(
              section,
              format!("string ID {payload} at entry {index} exceeds {num_strings}"),
            ));
          }
        }
        Some(PropValueTag::VectorF32) => {
          let Some(vector_count) = vector_count else {
            return Err(Self::invalid_section(
              section,
              format!("vector value at entry {index} has no vector section"),
            ));
          };
          let vector_index = usize::try_from(payload).map_err(|_| {
            Self::invalid_section(section, format!("vector index at entry {index} overflows"))
          })?;
          if vector_index >= vector_count {
            return Err(Self::invalid_section(
              section,
              format!("vector index {vector_index} at entry {index} exceeds {vector_count}"),
            ));
          }
        }
        Some(_) => {}
        None => {
          return Err(Self::invalid_section(
            section,
            format!("invalid property tag {tag} at entry {index}"),
          ));
        }
      }
    }
    Ok(())
  }

  fn validate_structure(&self) -> Result<()> {
    for (index, section) in self.sections.iter().enumerate() {
      if section.length == 0 || section.compression == 0 {
        continue;
      }
      let id = SectionId::from_u32(index as u32)
        .ok_or_else(|| KiteError::InvalidSnapshot(format!("unknown section {index}")))?;
      let data = self.section_data_shared(id).ok_or_else(|| {
        Self::invalid_section(Self::section_name(id), "cannot decompress section")
      })?;
      if data.as_ref().len() != section.uncompressed_size as usize {
        return Err(Self::invalid_section(
          Self::section_name(id),
          format!(
            "decompressed size mismatch: expected {}, found {}",
            section.uncompressed_size,
            data.as_ref().len()
          ),
        ));
      }
    }

    let num_nodes = Self::checked_count(self.header.num_nodes, "node counts")?;
    let num_edges = Self::checked_count(self.header.num_edges, "edge counts")?;
    let max_node_id = Self::checked_count(self.header.max_node_id, "NodeIdToPhys")?;
    let num_strings = Self::checked_count(self.header.num_strings, "StringOffsets")?;

    let phys_to_node_len = Self::checked_bytes(num_nodes, 8, "PhysToNodeId")?;
    self.validate_exact_section(
      SectionId::PhysToNodeId,
      phys_to_node_len,
      phys_to_node_len != 0,
    )?;

    let node_id_to_phys_len = Self::checked_bytes(
      max_node_id
        .checked_add(1)
        .ok_or_else(|| Self::invalid_section("NodeIdToPhys", "max node ID overflows"))?,
      4,
      "NodeIdToPhys",
    )?;
    let node_id_to_phys = self
      .validate_exact_section(SectionId::NodeIdToPhys, node_id_to_phys_len, true)?
      .ok_or_else(|| Self::invalid_section("NodeIdToPhys", "section is missing"))?;
    for index in 0..node_id_to_phys.as_ref().len() / 4 {
      let phys = read_i32_at(node_id_to_phys.as_ref(), index);
      if phys < -1 || (phys >= 0 && (phys as usize) >= num_nodes) {
        return Err(Self::invalid_section(
          "NodeIdToPhys",
          format!("physical node {phys} at node ID {index} is outside the node count"),
        ));
      }
    }

    let node_offsets_len = Self::checked_bytes(
      num_nodes
        .checked_add(1)
        .ok_or_else(|| Self::invalid_section("OutOffsets", "node count overflows"))?,
      4,
      "OutOffsets",
    )?;
    let out_offsets = self
      .validate_exact_section(SectionId::OutOffsets, node_offsets_len, true)?
      .ok_or_else(|| Self::invalid_section("OutOffsets", "section is missing"))?;
    Self::validate_u32_offsets(out_offsets.as_ref(), num_edges, "OutOffsets")?;

    let edge_array_len = Self::checked_bytes(num_edges, 4, "OutDst")?;
    let out_dst = self
      .validate_exact_section(SectionId::OutDst, edge_array_len, edge_array_len != 0)?
      .unwrap_or(SectionBytes::Borrowed(&[]));
    Self::validate_u32_values_below(out_dst.as_ref(), num_nodes, "OutDst")?;
    self.validate_exact_section(SectionId::OutEtype, edge_array_len, edge_array_len != 0)?;

    if self.header.flags.contains(SnapshotFlags::HAS_IN_EDGES) {
      let in_offsets = self
        .validate_exact_section(SectionId::InOffsets, node_offsets_len, true)?
        .ok_or_else(|| Self::invalid_section("InOffsets", "section is missing"))?;
      Self::validate_u32_offsets(in_offsets.as_ref(), num_edges, "InOffsets")?;
      let in_src = self
        .validate_exact_section(SectionId::InSrc, edge_array_len, edge_array_len != 0)?
        .unwrap_or(SectionBytes::Borrowed(&[]));
      Self::validate_u32_values_below(in_src.as_ref(), num_nodes, "InSrc")?;
      self.validate_exact_section(SectionId::InEtype, edge_array_len, edge_array_len != 0)?;
      let in_out_index = self
        .validate_exact_section(SectionId::InOutIndex, edge_array_len, edge_array_len != 0)?
        .unwrap_or(SectionBytes::Borrowed(&[]));
      Self::validate_u32_values_below(in_out_index.as_ref(), num_edges, "InOutIndex")?;
    }

    let string_offsets_len = Self::checked_bytes(
      Self::checked_count_plus_one(self.header.num_strings, "StringOffsets")?,
      4,
      "StringOffsets",
    )?;
    let string_offsets = self
      .validate_exact_section(SectionId::StringOffsets, string_offsets_len, true)?
      .ok_or_else(|| Self::invalid_section("StringOffsets", "section is missing"))?;
    let string_bytes = self.section_data_for_validation(SectionId::StringBytes, false)?;
    let string_bytes_len = string_bytes
      .as_ref()
      .map(|bytes| bytes.as_ref().len())
      .unwrap_or(0);
    Self::validate_u32_offsets(string_offsets.as_ref(), string_bytes_len, "StringOffsets")?;

    let label_ids_len = Self::checked_bytes(
      Self::checked_count_plus_one(self.header.num_labels, "LabelStringIds")?,
      4,
      "LabelStringIds",
    )?;
    let label_ids = self
      .validate_exact_section(SectionId::LabelStringIds, label_ids_len, true)?
      .ok_or_else(|| Self::invalid_section("LabelStringIds", "section is missing"))?;
    Self::validate_string_id_array(label_ids.as_ref(), num_strings, "LabelStringIds")?;

    let etype_ids_len = Self::checked_bytes(
      Self::checked_count_plus_one(self.header.num_etypes, "EtypeStringIds")?,
      4,
      "EtypeStringIds",
    )?;
    let etype_ids = self
      .validate_exact_section(SectionId::EtypeStringIds, etype_ids_len, true)?
      .ok_or_else(|| Self::invalid_section("EtypeStringIds", "section is missing"))?;
    Self::validate_string_id_array(etype_ids.as_ref(), num_strings, "EtypeStringIds")?;

    let propkey_ids_len = Self::checked_bytes(
      Self::checked_count_plus_one(self.header.num_propkeys, "PropkeyStringIds")?,
      4,
      "PropkeyStringIds",
    )?;
    let propkey_ids = self
      .validate_exact_section(SectionId::PropkeyStringIds, propkey_ids_len, true)?
      .ok_or_else(|| Self::invalid_section("PropkeyStringIds", "section is missing"))?;
    Self::validate_string_id_array(propkey_ids.as_ref(), num_strings, "PropkeyStringIds")?;

    let node_key_len = Self::checked_bytes(num_nodes, 4, "NodeKeyString")?;
    let node_keys = self
      .validate_exact_section(SectionId::NodeKeyString, node_key_len, node_key_len != 0)?
      .unwrap_or(SectionBytes::Borrowed(&[]));
    Self::validate_string_id_array(node_keys.as_ref(), num_strings, "NodeKeyString")?;

    let key_entries = self.section_data_for_validation(SectionId::KeyEntries, false)?;
    let key_entry_bytes = key_entries
      .as_ref()
      .map(|bytes| bytes.as_ref())
      .unwrap_or(&[]);
    if key_entry_bytes.len() % KEY_INDEX_ENTRY_SIZE != 0 {
      return Err(Self::invalid_section(
        "KeyEntries",
        "entry array has a partial entry",
      ));
    }
    let key_entry_count = key_entry_bytes.len() / KEY_INDEX_ENTRY_SIZE;
    if key_entry_count > num_nodes {
      return Err(Self::invalid_section(
        "KeyEntries",
        format!("entry count {key_entry_count} exceeds node count {num_nodes}"),
      ));
    }
    for index in 0..key_entry_count {
      let entry_offset = index * KEY_INDEX_ENTRY_SIZE;
      let string_id = read_u32(key_entry_bytes, entry_offset + 8) as usize;
      if string_id > num_strings {
        return Err(Self::invalid_section(
          "KeyEntries",
          format!("string ID {string_id} at entry {index} exceeds {num_strings}"),
        ));
      }
      let node_id = read_u64(key_entry_bytes, entry_offset + 16);
      let node_id_index = usize::try_from(node_id).map_err(|_| {
        Self::invalid_section("KeyEntries", format!("node ID at entry {index} overflows"))
      })?;
      if node_id_index > max_node_id || read_i32_at(node_id_to_phys.as_ref(), node_id_index) < 0 {
        return Err(Self::invalid_section(
          "KeyEntries",
          format!("node ID {node_id} at entry {index} is not present"),
        ));
      }
    }

    let key_buckets = self.section_data_for_validation(
      SectionId::KeyBuckets,
      self.header.flags.contains(SnapshotFlags::HAS_KEY_BUCKETS),
    )?;
    if let Some(key_buckets) = key_buckets {
      let key_buckets = key_buckets.as_ref();
      if key_buckets.len() < 8 || key_buckets.len() % 4 != 0 {
        return Err(Self::invalid_section(
          "KeyBuckets",
          "bucket array must contain at least two u32 offsets",
        ));
      }
      Self::validate_u32_offsets(key_buckets, key_entry_count, "KeyBuckets")?;
      if read_u32_at(key_buckets, 0) != 0
        || read_u32_at(key_buckets, key_buckets.len() / 4 - 1) as usize != key_entry_count
      {
        return Err(Self::invalid_section(
          "KeyBuckets",
          "bucket offsets do not cover all key entries",
        ));
      }
    }

    let vector_count = if self.header.flags.contains(SnapshotFlags::HAS_VECTORS) {
      let vector_offsets = self
        .section_data_for_validation(SectionId::VectorOffsets, true)?
        .ok_or_else(|| Self::invalid_section("VectorOffsets", "section is missing"))?;
      let vector_data = self
        .section_data_for_validation(SectionId::VectorData, true)?
        .ok_or_else(|| Self::invalid_section("VectorData", "section is missing"))?;
      if vector_offsets.as_ref().len() < 16 {
        return Err(Self::invalid_section(
          "VectorOffsets",
          "vector offset array must contain at least one vector",
        ));
      }
      if vector_data.as_ref().len() % 4 != 0 {
        return Err(Self::invalid_section(
          "VectorData",
          "vector data is not a multiple of 4 bytes",
        ));
      }
      Self::validate_u64_offsets(
        vector_offsets.as_ref(),
        vector_data.as_ref().len(),
        "VectorOffsets",
      )?;
      Some(vector_offsets.as_ref().len() / 8 - 1)
    } else {
      None
    };

    let node_prop_offsets = self
      .validate_exact_section(SectionId::NodePropOffsets, node_offsets_len, true)?
      .ok_or_else(|| Self::invalid_section("NodePropOffsets", "section is missing"))?;
    let node_prop_keys = self.section_data_for_validation(SectionId::NodePropKeys, false)?;
    let node_prop_vals = self.section_data_for_validation(SectionId::NodePropVals, false)?;
    let node_prop_keys = node_prop_keys
      .as_ref()
      .map(|bytes| bytes.as_ref())
      .unwrap_or(&[]);
    let node_prop_vals = node_prop_vals
      .as_ref()
      .map(|bytes| bytes.as_ref())
      .unwrap_or(&[]);
    if node_prop_keys.len() % 4 != 0 || node_prop_vals.len() % PROP_VALUE_DISK_SIZE != 0 {
      return Err(Self::invalid_section(
        "NodePropKeys",
        "property arrays contain partial entries",
      ));
    }
    let node_prop_count = node_prop_keys.len() / 4;
    if node_prop_vals.len() / PROP_VALUE_DISK_SIZE != node_prop_count {
      return Err(Self::invalid_section(
        "NodePropVals",
        "property key/value counts differ",
      ));
    }
    Self::validate_property_values(node_prop_vals, num_strings, vector_count, "NodePropVals")?;
    Self::validate_u32_offsets(
      node_prop_offsets.as_ref(),
      node_prop_count,
      "NodePropOffsets",
    )?;

    let edge_offsets_len = Self::checked_bytes(
      num_edges
        .checked_add(1)
        .ok_or_else(|| Self::invalid_section("EdgePropOffsets", "edge count overflows"))?,
      4,
      "EdgePropOffsets",
    )?;
    let edge_prop_offsets = self
      .validate_exact_section(SectionId::EdgePropOffsets, edge_offsets_len, true)?
      .ok_or_else(|| Self::invalid_section("EdgePropOffsets", "section is missing"))?;
    let edge_prop_keys = self.section_data_for_validation(SectionId::EdgePropKeys, false)?;
    let edge_prop_vals = self.section_data_for_validation(SectionId::EdgePropVals, false)?;
    let edge_prop_keys = edge_prop_keys
      .as_ref()
      .map(|bytes| bytes.as_ref())
      .unwrap_or(&[]);
    let edge_prop_vals = edge_prop_vals
      .as_ref()
      .map(|bytes| bytes.as_ref())
      .unwrap_or(&[]);
    if edge_prop_keys.len() % 4 != 0 || edge_prop_vals.len() % PROP_VALUE_DISK_SIZE != 0 {
      return Err(Self::invalid_section(
        "EdgePropKeys",
        "property arrays contain partial entries",
      ));
    }
    let edge_prop_count = edge_prop_keys.len() / 4;
    if edge_prop_vals.len() / PROP_VALUE_DISK_SIZE != edge_prop_count {
      return Err(Self::invalid_section(
        "EdgePropVals",
        "property key/value counts differ",
      ));
    }
    Self::validate_property_values(edge_prop_vals, num_strings, vector_count, "EdgePropVals")?;
    Self::validate_u32_offsets(
      edge_prop_offsets.as_ref(),
      edge_prop_count,
      "EdgePropOffsets",
    )?;

    if self.header.flags.contains(SnapshotFlags::HAS_NODE_LABELS) {
      let label_offsets = self
        .validate_exact_section(SectionId::NodeLabelOffsets, node_offsets_len, true)?
        .ok_or_else(|| Self::invalid_section("NodeLabelOffsets", "section is missing"))?;
      let label_ids = self.section_data_for_validation(SectionId::NodeLabelIds, false)?;
      let label_ids = label_ids
        .as_ref()
        .map(|bytes| bytes.as_ref())
        .unwrap_or(&[]);
      if label_ids.len() % 4 != 0 {
        return Err(Self::invalid_section(
          "NodeLabelIds",
          "label array is not a multiple of 4 bytes",
        ));
      }
      Self::validate_u32_offsets(
        label_offsets.as_ref(),
        label_ids.len() / 4,
        "NodeLabelOffsets",
      )?;
    }

    if self.header.flags.contains(SnapshotFlags::HAS_VECTOR_STORES) {
      let index = self
        .section_data_for_validation(SectionId::VectorStoreIndex, true)?
        .ok_or_else(|| Self::invalid_section("VectorStoreIndex", "section is missing"))?;
      let data = self
        .section_data_for_validation(SectionId::VectorStoreData, true)?
        .ok_or_else(|| Self::invalid_section("VectorStoreData", "section is missing"))?;
      let index = index.as_ref();
      if index.len() < 4 {
        return Err(Self::invalid_section(
          "VectorStoreIndex",
          "index is smaller than its count",
        ));
      }
      let count = read_u32(index, 0) as usize;
      let entries_len = Self::checked_bytes(count, 20, "VectorStoreIndex")?;
      let expected_len = entries_len
        .checked_add(4)
        .ok_or_else(|| Self::invalid_section("VectorStoreIndex", "index size overflows"))?;
      if index.len() != expected_len {
        return Err(Self::invalid_section(
          "VectorStoreIndex",
          format!(
            "count {count} requires {expected_len} bytes, found {}",
            index.len()
          ),
        ));
      }
      for entry in 0..count {
        let entry_offset = 4 + entry * 20;
        let payload_offset = usize::try_from(read_u64(index, entry_offset + 4)).map_err(|_| {
          Self::invalid_section(
            "VectorStoreIndex",
            format!("entry {entry} offset overflows"),
          )
        })?;
        let payload_len = usize::try_from(read_u64(index, entry_offset + 12)).map_err(|_| {
          Self::invalid_section(
            "VectorStoreIndex",
            format!("entry {entry} length overflows"),
          )
        })?;
        let payload_end = payload_offset.checked_add(payload_len).ok_or_else(|| {
          Self::invalid_section("VectorStoreIndex", format!("entry {entry} range overflows"))
        })?;
        if payload_end > data.as_ref().len() {
          return Err(Self::invalid_section(
            "VectorStoreIndex",
            format!("entry {entry} exceeds VectorStoreData"),
          ));
        }
      }
    }

    Ok(())
  }

  fn init_string_cache(num_strings: u64) -> Result<Vec<OnceLock<Arc<str>>>> {
    let base_len = usize::try_from(num_strings)
      .map_err(|_| KiteError::InvalidSnapshot("Snapshot string table too large".to_string()))?;
    let len = base_len
      .checked_add(1)
      .ok_or_else(|| KiteError::InvalidSnapshot("Snapshot string table too large".to_string()))?;
    Ok(std::iter::repeat_with(OnceLock::new).take(len).collect())
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
        return Some(cached.as_ref().to_vec());
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
    let decompressed = Arc::<[u8]>::from(
      decompress_with_size(raw_bytes, compression, section.uncompressed_size as usize).ok()?,
    );

    // Cache the result
    {
      let mut cache = self.decompressed_cache.write();
      cache.insert(id, Arc::clone(&decompressed));
    }

    Some(decompressed.as_ref().to_vec())
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

  /// Get section data as a slice, decompressing if needed.
  pub fn section_data(&self, id: SectionId) -> Option<Cow<'_, [u8]>> {
    let data = self.section_data_shared(id)?;
    match data {
      SectionBytes::Borrowed(bytes) => Some(Cow::Borrowed(bytes)),
      SectionBytes::Shared(bytes) => Some(Cow::Owned(bytes.as_ref().to_vec())),
    }
  }

  /// Get section data as a borrowed slice or shared buffer.
  pub fn section_data_shared(&self, id: SectionId) -> Option<SectionBytes<'_>> {
    if let Some(slice) = self.section_slice(id) {
      return Some(SectionBytes::Borrowed(slice));
    }

    let section = self.sections.get(id as usize)?;
    if section.length == 0 {
      return None;
    }

    // Check cache first
    {
      let cache = self.decompressed_cache.read();
      if let Some(cached) = cache.get(&id) {
        return Some(SectionBytes::Shared(Arc::clone(cached)));
      }
    }

    let raw_bytes = self.raw_section_bytes(id)?;
    let compression =
      CompressionType::from_u32(section.compression).unwrap_or(CompressionType::None);

    if compression == CompressionType::None {
      return Some(SectionBytes::Borrowed(raw_bytes));
    }

    // Decompress
    let decompressed = Arc::<[u8]>::from(
      decompress_with_size(raw_bytes, compression, section.uncompressed_size as usize).ok()?,
    );

    // Cache the result
    {
      let mut cache = self.decompressed_cache.write();
      cache.insert(id, Arc::clone(&decompressed));
    }

    Some(SectionBytes::Shared(decompressed))
  }

  // ========================================================================
  // Node accessors
  // ========================================================================

  /// Get NodeID for a physical node index
  #[inline]
  pub fn node_id(&self, phys: PhysNode) -> Option<NodeId> {
    let section = self.section_data_shared(SectionId::PhysToNodeId)?;
    let section = section.as_ref();
    if (phys as usize) * 8 + 8 > section.len() {
      return None;
    }
    Some(read_u64_at(section, phys as usize))
  }

  /// Get physical node index for a NodeID, or None if not present
  #[inline]
  pub fn phys_node(&self, node_id: NodeId) -> Option<PhysNode> {
    let section = self.section_data_shared(SectionId::NodeIdToPhys)?;
    let section = section.as_ref();
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
    self.phys_node(node_id).is_some()
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
  pub fn string(&self, string_id: StringId) -> Option<String> {
    if string_id == 0 {
      return Some(String::new());
    }

    let offsets = self.section_data_shared(SectionId::StringOffsets)?;
    let bytes = self.section_data_shared(SectionId::StringBytes)?;
    let offsets = offsets.as_ref();
    let bytes = bytes.as_ref();

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

  fn string_cached(&self, string_id: StringId) -> Option<&str> {
    if string_id == 0 {
      return Some("");
    }

    let idx = string_id as usize;
    let cell = self.string_cache.get(idx)?;
    if let Some(value) = cell.get() {
      return Some(value.as_ref());
    }

    let value = self.string(string_id)?;
    let arc: Arc<str> = Arc::from(value);
    let _ = cell.set(arc);
    cell.get().map(|value| value.as_ref())
  }

  // ========================================================================
  // Edge accessors
  // ========================================================================

  /// Get out-edge offset range for a physical node
  fn out_edge_range(&self, phys: PhysNode) -> Option<(usize, usize)> {
    let offsets = self.section_data_shared(SectionId::OutOffsets)?;
    let offsets = offsets.as_ref();
    let idx = phys as usize;
    if idx * 4 + 8 > offsets.len() {
      return None;
    }
    let start = read_u32_at(offsets, idx) as usize;
    let end = read_u32_at(offsets, idx + 1) as usize;
    Some((start, end))
  }

  /// Get out-degree for a physical node
  pub fn out_degree(&self, phys: PhysNode) -> Option<usize> {
    let (start, end) = self.out_edge_range(phys)?;
    Some(end - start)
  }

  /// Check if an edge exists in the snapshot (binary search)
  pub fn has_edge(&self, src_phys: PhysNode, etype: ETypeId, dst_phys: PhysNode) -> bool {
    let (start, end) = match self.out_edge_range(src_phys) {
      Some(range) => range,
      None => return false,
    };

    let out_etype = match self.section_data_shared(SectionId::OutEtype) {
      Some(s) => s,
      None => return false,
    };
    let out_dst = match self.section_data_shared(SectionId::OutDst) {
      Some(s) => s,
      None => return false,
    };
    let out_etype = out_etype.as_ref();
    let out_dst = out_dst.as_ref();

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
    let out_etype = self.section_data_shared(SectionId::OutEtype)?;
    let out_dst = self.section_data_shared(SectionId::OutDst)?;
    let out_etype = out_etype.as_ref();
    let out_dst = out_dst.as_ref();

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
    let offsets = self.section_data_shared(SectionId::InOffsets)?;
    let offsets = offsets.as_ref();
    let idx = phys as usize;
    if idx * 4 + 8 > offsets.len() {
      return None;
    }
    let start = read_u32_at(offsets, idx) as usize;
    let end = read_u32_at(offsets, idx + 1) as usize;
    Some((start, end))
  }

  /// Get in-degree for a physical node
  pub fn in_degree(&self, phys: PhysNode) -> Option<usize> {
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

    let key_entries = self.section_data_shared(SectionId::KeyEntries)?;
    let key_entries = key_entries.as_ref();
    let num_entries = key_entries.len() / KEY_INDEX_ENTRY_SIZE;
    if num_entries == 0 {
      return None;
    }

    let (lo, hi) = if let Some(buckets) = self.section_data_shared(SectionId::KeyBuckets) {
      let buckets = buckets.as_ref();
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
      if let Some(entry_key) = self.string(string_id) {
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
  pub fn node_key(&self, phys: PhysNode) -> Option<String> {
    let node_key_string = self.section_data_shared(SectionId::NodeKeyString)?;
    let node_key_string = node_key_string.as_ref();
    let idx = phys as usize;
    if idx * 4 + 4 > node_key_string.len() {
      return None;
    }
    let string_id = read_u32_at(node_key_string, idx);
    if string_id == 0 {
      return None;
    }
    self.string(string_id)
  }

  // ========================================================================
  // Label access
  // ========================================================================

  /// Get all labels for a node
  pub fn node_labels(&self, phys: PhysNode) -> Option<Vec<LabelId>> {
    if !self.header.flags.contains(SnapshotFlags::HAS_NODE_LABELS) {
      return None;
    }

    let offsets = self.section_data_shared(SectionId::NodeLabelOffsets)?;
    let labels = self.section_data_shared(SectionId::NodeLabelIds)?;
    let offsets = offsets.as_ref();
    let labels = labels.as_ref();

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
  pub fn node_props(&self, phys: PhysNode) -> Option<HashMap<PropKeyId, PropValue>> {
    if !self.header.flags.contains(SnapshotFlags::HAS_PROPERTIES) {
      return None;
    }

    let offsets = self.section_data_shared(SectionId::NodePropOffsets)?;
    let keys = self.section_data_shared(SectionId::NodePropKeys)?;
    let vals = self.section_data_shared(SectionId::NodePropVals)?;
    let offsets = offsets.as_ref();
    let keys = keys.as_ref();
    let vals = vals.as_ref();

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
  pub fn node_prop(&self, phys: PhysNode, prop_key_id: PropKeyId) -> Option<PropValue> {
    if !self.header.flags.contains(SnapshotFlags::HAS_PROPERTIES) {
      return None;
    }

    let offsets = self.section_data_shared(SectionId::NodePropOffsets)?;
    let keys = self.section_data_shared(SectionId::NodePropKeys)?;
    let vals = self.section_data_shared(SectionId::NodePropVals)?;
    let offsets = offsets.as_ref();
    let keys = keys.as_ref();
    let vals = vals.as_ref();

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
  pub fn edge_props(&self, edge_idx: usize) -> Option<HashMap<PropKeyId, PropValue>> {
    if !self.header.flags.contains(SnapshotFlags::HAS_PROPERTIES) {
      return None;
    }

    let offsets = self.section_data_shared(SectionId::EdgePropOffsets)?;
    let keys = self.section_data_shared(SectionId::EdgePropKeys)?;
    let vals = self.section_data_shared(SectionId::EdgePropVals)?;
    let offsets = offsets.as_ref();
    let keys = keys.as_ref();
    let vals = vals.as_ref();

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
        let s = self.string(payload as u32)?;
        Some(PropValue::String(s))
      }
      PropValueTag::VectorF32 => {
        if !self.header.flags.contains(SnapshotFlags::HAS_VECTORS) {
          return None;
        }

        let offsets = self.section_data_shared(SectionId::VectorOffsets)?;
        let data = self.section_data_shared(SectionId::VectorData)?;
        let offsets = offsets.as_ref();
        let data = data.as_ref();

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
  out_etype: Option<SectionBytes<'a>>,
  out_dst: Option<SectionBytes<'a>>,
  current: usize,
  end: usize,
}

impl<'a> OutEdgeIter<'a> {
  fn new(snapshot: &'a SnapshotData, phys: PhysNode) -> Self {
    let (current, end) = snapshot.out_edge_range(phys).unwrap_or((0, 0));
    Self {
      snapshot,
      out_etype: snapshot.section_data_shared(SectionId::OutEtype),
      out_dst: snapshot.section_data_shared(SectionId::OutDst),
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

    let out_etype = self.out_etype.as_ref()?;
    let out_dst = self.out_dst.as_ref()?;
    let out_etype = out_etype.as_ref();
    let out_dst = out_dst.as_ref();

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
  in_etype: Option<SectionBytes<'a>>,
  in_src: Option<SectionBytes<'a>>,
  in_out_index: Option<SectionBytes<'a>>,
  current: usize,
  end: usize,
}

impl<'a> InEdgeIter<'a> {
  fn new(snapshot: &'a SnapshotData, phys: PhysNode) -> Self {
    let (current, end) = snapshot.in_edge_range(phys).unwrap_or((0, 0));
    Self {
      snapshot,
      in_etype: snapshot.section_data_shared(SectionId::InEtype),
      in_src: snapshot.section_data_shared(SectionId::InSrc),
      in_out_index: snapshot.section_data_shared(SectionId::InOutIndex),
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

    let in_etype = self.in_etype.as_ref()?;
    let in_src = self.in_src.as_ref()?;
    let in_etype = in_etype.as_ref();
    let in_src = in_src.as_ref();

    if self.current * 4 + 4 > in_etype.len() || self.current * 4 + 4 > in_src.len() {
      return None;
    }

    let src = read_u32_at(in_src, self.current);
    let etype = read_u32_at(in_etype, self.current);
    let out_index = self
      .in_out_index
      .as_ref()
      .and_then(|idx| {
        let idx = idx.as_ref();
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
  pub fn label_name(&self, label_id: LabelId) -> Option<&str> {
    let label_string_ids = self.section_data_shared(SectionId::LabelStringIds)?;
    let label_string_ids = label_string_ids.as_ref();
    let idx = label_id as usize;
    if idx * 4 + 4 > label_string_ids.len() {
      return None;
    }
    let string_id = read_u32_at(label_string_ids, idx);
    if string_id == 0 {
      return None;
    }
    self.string_cached(string_id)
  }

  /// Get etype name by ETypeID
  pub fn etype_name(&self, etype_id: ETypeId) -> Option<&str> {
    let etype_string_ids = self.section_data_shared(SectionId::EtypeStringIds)?;
    let etype_string_ids = etype_string_ids.as_ref();
    let idx = etype_id as usize;
    if idx * 4 + 4 > etype_string_ids.len() {
      return None;
    }
    let string_id = read_u32_at(etype_string_ids, idx);
    if string_id == 0 {
      return None;
    }
    self.string_cached(string_id)
  }

  /// Get propkey name by PropKeyID
  pub fn propkey_name(&self, propkey_id: PropKeyId) -> Option<&str> {
    let propkey_string_ids = self.section_data_shared(SectionId::PropkeyStringIds)?;
    let propkey_string_ids = propkey_string_ids.as_ref();
    let idx = propkey_id as usize;
    if idx * 4 + 4 > propkey_string_ids.len() {
      return None;
    }
    let string_id = read_u32_at(propkey_string_ids, idx);
    if string_id == 0 {
      return None;
    }
    self.string_cached(string_id)
  }

  /// Get out-edges as a Vec for compaction purposes
  pub fn out_edges(&self, phys: PhysNode) -> Vec<OutEdgeInfo> {
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
  use crate::core::snapshot::writer::{build_snapshot_to_memory, NodeData, SnapshotBuildInput};
  use crate::types::PropValue;
  use crate::util::crc::crc32c;
  use crate::util::mmap::map_file;
  use std::collections::HashMap;
  use std::fs::{self, File};
  use std::sync::{Arc, Mutex};
  use tempfile::tempdir;

  fn corruption_test_snapshot() -> Vec<u8> {
    let mut node_props = HashMap::new();
    node_props.insert(1, PropValue::String("node-value".to_string()));

    build_snapshot_to_memory(SnapshotBuildInput {
      generation: 1,
      nodes: vec![
        NodeData {
          node_id: 1,
          key: Some("alpha".to_string()),
          labels: vec![1],
          props: node_props,
        },
        NodeData {
          node_id: 2,
          key: Some("beta".to_string()),
          labels: vec![1],
          props: HashMap::new(),
        },
      ],
      edges: vec![crate::core::snapshot::writer::EdgeData {
        src: 1,
        etype: 1,
        dst: 2,
        props: HashMap::new(),
      }],
      labels: HashMap::from([(1, "person".to_string())]),
      etypes: HashMap::from([(1, "knows".to_string())]),
      propkeys: HashMap::from([(1, "value".to_string())]),
      vector_stores: None,
      compression: None,
    })
    .expect("snapshot build")
  }

  fn section_payload_offset(bytes: &[u8], id: SectionId) -> usize {
    let table_offset = SNAPSHOT_HEADER_SIZE + id as usize * SECTION_ENTRY_SIZE;
    read_u64(bytes, table_offset) as usize
  }

  fn load_corrupt_snapshot(bytes: &[u8], skip_crc_validation: bool) -> Result<()> {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("corrupt-snapshot.gds");
    fs::write(&path, bytes).expect("write snapshot");
    SnapshotData::load_with_options(
      &path,
      &ParseSnapshotOptions {
        skip_crc_validation,
        ..ParseSnapshotOptions::default()
      },
    )
    .map(|_| ())
  }

  #[test]
  fn test_parse_snapshot_options_default() {
    let opts = ParseSnapshotOptions::default();
    assert!(!opts.skip_crc_validation);
    assert!(opts.crc_chunk_size.is_none());
    assert!(opts.crc_profile_sink.is_none());
  }

  #[test]
  fn test_parse_collects_crc_section_profile() {
    let mut props = HashMap::new();
    props.insert(1, PropValue::VectorF32(vec![0.1, 0.2, 0.3, 0.4]));

    let bytes = build_snapshot_to_memory(SnapshotBuildInput {
      generation: 1,
      nodes: vec![NodeData {
        node_id: 1,
        key: Some("n1".to_string()),
        labels: Vec::new(),
        props,
      }],
      edges: Vec::new(),
      labels: HashMap::new(),
      etypes: HashMap::new(),
      propkeys: HashMap::from([(1, "embedding".to_string())]),
      vector_stores: None,
      compression: None,
    })
    .expect("snapshot build");

    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("profile-snapshot.gds");
    fs::write(&path, &bytes).expect("write snapshot");

    let profile_sink = Arc::new(Mutex::new(None));
    let options = ParseSnapshotOptions {
      skip_crc_validation: false,
      crc_chunk_size: Some(128),
      crc_profile_sink: Some(Arc::clone(&profile_sink)),
    };

    let _snapshot = SnapshotData::load_with_options(&path, &options).expect("snapshot parse");

    let profile = profile_sink
      .lock()
      .expect("profile lock")
      .clone()
      .expect("profile");
    assert_eq!(profile.chunk_size, 128);
    assert!(profile.total_bytes > 0);
    assert!(profile.total_ns > 0);
    assert!(!profile.sections.is_empty());
    assert_eq!(
      profile
        .sections
        .iter()
        .map(|entry| entry.bytes)
        .sum::<usize>(),
      profile.total_bytes
    );
    assert!(profile
      .sections
      .iter()
      .any(|entry| entry.section_id == Some(SectionId::VectorData) && entry.bytes > 0));
  }

  #[test]
  fn test_parse_at_offset_rejects_offset_past_mmap_without_panicking() {
    let bytes = corruption_test_snapshot();
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("offset-snapshot.gds");
    fs::write(&path, &bytes).expect("write snapshot");
    let file = File::open(&path).expect("open snapshot");
    let mmap = map_file(&file).expect("map snapshot");

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      SnapshotData::parse_at_offset(
        Arc::new(mmap),
        bytes.len().saturating_add(1),
        &ParseSnapshotOptions {
          skip_crc_validation: true,
          ..ParseSnapshotOptions::default()
        },
      )
    }));

    assert!(result.is_ok(), "offset parsing panicked");
    assert!(matches!(
      result.expect("panic checked"),
      Err(KiteError::InvalidSnapshot(_))
    ));
  }

  #[test]
  #[allow(clippy::type_complexity)]
  fn test_snapshot_corruption_matrix_returns_errors_without_panicking() {
    let valid = corruption_test_snapshot();
    let section_table_end = SNAPSHOT_HEADER_SIZE + SectionId::COUNT * SECTION_ENTRY_SIZE;
    let mut truncation_boundaries = vec![SNAPSHOT_HEADER_SIZE, section_table_end];
    for id in [
      SectionId::PhysToNodeId,
      SectionId::OutOffsets,
      SectionId::StringOffsets,
      SectionId::KeyEntries,
      SectionId::KeyBuckets,
    ] {
      truncation_boundaries.push(section_payload_offset(&valid, id));
    }

    for end in truncation_boundaries {
      let truncated = &valid[..end.min(valid.len().saturating_sub(1))];
      let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        load_corrupt_snapshot(truncated, true)
      }));
      assert!(result.is_ok(), "truncation at {end} panicked");
      assert!(
        result.expect("panic checked").is_err(),
        "truncation at {end} accepted"
      );
    }

    let mutations: [(&str, Box<dyn Fn(&mut Vec<u8>)>); 4] = [
      (
        "string offsets descending",
        Box::new(|bytes| {
          let offset = section_payload_offset(bytes, SectionId::StringOffsets);
          write_u32(bytes, offset + 4, 1);
          write_u32(bytes, offset + 8, 0);
        }),
      ),
      (
        "out csr offsets descending",
        Box::new(|bytes| {
          let offset = section_payload_offset(bytes, SectionId::OutOffsets);
          write_u32(bytes, offset + 4, 2);
          write_u32(bytes, offset + 8, 1);
        }),
      ),
      (
        "key bucket outside entries",
        Box::new(|bytes| {
          let entry_table =
            SNAPSHOT_HEADER_SIZE + SectionId::KeyEntries as usize * SECTION_ENTRY_SIZE;
          let entry_count = read_u64(bytes, entry_table + 8) as usize / KEY_INDEX_ENTRY_SIZE;
          let offset = section_payload_offset(bytes, SectionId::KeyBuckets);
          write_u32(bytes, offset, entry_count as u32 + 1);
        }),
      ),
      (
        "header node count mismatch",
        Box::new(|bytes| write_u64(bytes, 32, 3)),
      ),
    ];

    for (name, mutate) in mutations {
      let mut corrupted = valid.clone();
      mutate(&mut corrupted);
      let footer_crc = crc32c(&corrupted[..corrupted.len() - 4]);
      let footer_offset = corrupted.len() - 4;
      write_u32(&mut corrupted, footer_offset, footer_crc);
      let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        load_corrupt_snapshot(&corrupted, false)
      }));
      assert!(result.is_ok(), "{name} panicked");
      assert!(result.expect("panic checked").is_err(), "{name} accepted");
    }

    for (name, field_offset, values) in [
      ("header node count", 32usize, [0u64, 1, u64::MAX].as_slice()),
      ("header edge count", 40usize, [0u64, u64::MAX].as_slice()),
      (
        "header max node ID",
        48usize,
        [0u64, 1, u64::MAX].as_slice(),
      ),
      (
        "header string count",
        80usize,
        [0u64, 1, u64::MAX].as_slice(),
      ),
    ] {
      for &value in values {
        let mut corrupted = valid.clone();
        write_u64(&mut corrupted, field_offset, value);
        let footer_crc = crc32c(&corrupted[..corrupted.len() - 4]);
        let footer_offset = corrupted.len() - 4;
        write_u32(&mut corrupted, footer_offset, footer_crc);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
          load_corrupt_snapshot(&corrupted, false)
        }));
        assert!(result.is_ok(), "{name}={value} panicked");
        assert!(
          result.expect("panic checked").is_err(),
          "{name}={value} accepted"
        );
      }
    }
  }
}
