//! CSR Snapshot Writer
//!
//! Builds CSR snapshots from nodes and edges for checkpointing.
//! Ported from src/core/snapshot-writer.ts

use crate::constants::*;
use crate::error::Result;
use crate::types::*;
use crate::util::binary::*;
use crate::util::compression::{maybe_compress, CompressionOptions, CompressionType};
use crate::util::crc::crc32c;
use crate::util::hash::xxhash64_string;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

// ============================================================================
// Builder input types
// ============================================================================

/// Node data for snapshot building
#[derive(Debug, Clone)]
pub struct NodeData {
  pub node_id: NodeId,
  pub key: Option<String>,
  pub labels: Vec<LabelId>,
  pub props: HashMap<PropKeyId, PropValue>,
}

/// Edge data for snapshot building
#[derive(Debug, Clone)]
pub struct EdgeData {
  pub src: NodeId,
  pub etype: ETypeId,
  pub dst: NodeId,
  pub props: HashMap<PropKeyId, PropValue>,
}

/// Input for building a snapshot
#[derive(Debug)]
pub struct SnapshotBuildInput {
  pub generation: u64,
  pub nodes: Vec<NodeData>,
  pub edges: Vec<EdgeData>,
  pub labels: HashMap<LabelId, String>,
  pub etypes: HashMap<ETypeId, String>,
  pub propkeys: HashMap<PropKeyId, String>,
  pub compression: Option<CompressionOptions>,
}

// ============================================================================
// String table for interning
// ============================================================================

struct StringTable {
  strings: Vec<String>,
  string_to_id: HashMap<String, StringId>,
}

impl StringTable {
  fn new() -> Self {
    let mut table = Self {
      strings: vec![String::new()], // StringID 0 is reserved/empty
      string_to_id: HashMap::new(),
    };
    table.string_to_id.insert(String::new(), 0);
    table
  }

  fn intern(&mut self, s: &str) -> StringId {
    if let Some(&id) = self.string_to_id.get(s) {
      return id;
    }
    let id = self.strings.len() as StringId;
    self.strings.push(s.to_string());
    self.string_to_id.insert(s.to_string(), id);
    id
  }

  fn len(&self) -> usize {
    self.strings.len()
  }
}

// ============================================================================
// CSR building
// ============================================================================

struct CSRData {
  offsets: Vec<u32>,
  dst: Vec<u32>,
  etype: Vec<u32>,
  /// For in-edges: index back to out-edge
  out_index: Option<Vec<u32>>,
}

fn build_out_edges_csr(
  nodes: &[NodeData],
  edges: &[EdgeData],
  node_id_to_phys: &HashMap<NodeId, PhysNode>,
) -> CSRData {
  let num_nodes = nodes.len();
  let num_edges = edges.len();

  // Count edges per node
  let mut counts = vec![0u32; num_nodes];
  for edge in edges {
    if let Some(&src_phys) = node_id_to_phys.get(&edge.src) {
      counts[src_phys as usize] += 1;
    }
  }

  // Build offsets (prefix sum)
  let mut offsets = vec![0u32; num_nodes + 1];
  for i in 0..num_nodes {
    offsets[i + 1] = offsets[i] + counts[i];
  }

  // Fill edge arrays (sort by etype, dst within each node)
  let mut dst_arr = vec![0u32; num_edges];
  let mut etype_arr = vec![0u32; num_edges];

  // Group edges by source node
  let mut edges_by_node: HashMap<PhysNode, Vec<(ETypeId, PhysNode)>> = HashMap::new();
  for edge in edges {
    if let (Some(&src_phys), Some(&dst_phys)) = (
      node_id_to_phys.get(&edge.src),
      node_id_to_phys.get(&edge.dst),
    ) {
      edges_by_node
        .entry(src_phys)
        .or_default()
        .push((edge.etype, dst_phys));
    }
  }

  // Sort and write edges for each node
  for (src_phys, mut node_edges) in edges_by_node {
    // Sort by (etype, dst)
    node_edges.sort_by(|a, b| {
      if a.0 != b.0 {
        a.0.cmp(&b.0)
      } else {
        a.1.cmp(&b.1)
      }
    });

    let mut pos = offsets[src_phys as usize] as usize;
    for (etype, dst_phys) in node_edges {
      dst_arr[pos] = dst_phys;
      etype_arr[pos] = etype;
      pos += 1;
    }
  }

  CSRData {
    offsets,
    dst: dst_arr,
    etype: etype_arr,
    out_index: None,
  }
}

fn build_in_edges_csr(nodes: &[NodeData], out_csr: &CSRData) -> CSRData {
  let num_nodes = nodes.len();
  let num_edges = out_csr.dst.len();

  // Count in-edges per node
  let mut counts = vec![0u32; num_nodes];
  for &dst in &out_csr.dst {
    counts[dst as usize] += 1;
  }

  // Build offsets (prefix sum)
  let mut offsets = vec![0u32; num_nodes + 1];
  for i in 0..num_nodes {
    offsets[i + 1] = offsets[i] + counts[i];
  }

  // Fill in-edge arrays
  let mut src_arr = vec![0u32; num_edges];
  let mut etype_arr = vec![0u32; num_edges];
  let mut out_index = vec![0u32; num_edges];

  // Collect in-edges with their out-edge indices
  let mut in_edges_by_node: HashMap<PhysNode, Vec<(PhysNode, ETypeId, u32)>> = HashMap::new();

  for src_phys in 0..num_nodes {
    let start = out_csr.offsets[src_phys] as usize;
    let end = out_csr.offsets[src_phys + 1] as usize;

    for out_idx in start..end {
      let dst_phys = out_csr.dst[out_idx];
      let edge_etype = out_csr.etype[out_idx];

      in_edges_by_node.entry(dst_phys).or_default().push((
        src_phys as PhysNode,
        edge_etype,
        out_idx as u32,
      ));
    }
  }

  // Sort and write in-edges for each node
  for (dst_phys, mut node_in_edges) in in_edges_by_node {
    // Sort by (etype, src)
    node_in_edges.sort_by(|a, b| {
      if a.1 != b.1 {
        a.1.cmp(&b.1)
      } else {
        a.0.cmp(&b.0)
      }
    });

    let mut pos = offsets[dst_phys as usize] as usize;
    for (src_phys, etype, out_idx) in node_in_edges {
      src_arr[pos] = src_phys;
      etype_arr[pos] = etype;
      out_index[pos] = out_idx;
      pos += 1;
    }
  }

  CSRData {
    offsets,
    dst: src_arr, // For in-edges, "dst" is actually source
    etype: etype_arr,
    out_index: Some(out_index),
  }
}

// ============================================================================
// Key index building
// ============================================================================

struct KeyEntry {
  hash64: u64,
  string_id: StringId,
  node_id: NodeId,
}

struct KeyIndexData {
  entries: Vec<KeyEntry>,
  buckets: Vec<u32>,
}

fn build_key_index(nodes: &[NodeData], node_key_strings: &[StringId]) -> KeyIndexData {
  let mut raw_entries: Vec<KeyEntry> = Vec::new();

  for (i, node) in nodes.iter().enumerate() {
    if let Some(ref key) = node.key {
      let string_id = node_key_strings[i];
      raw_entries.push(KeyEntry {
        hash64: xxhash64_string(key),
        string_id,
        node_id: node.node_id,
      });
    }
  }

  // Build bucket array for O(1) bucket lookup
  // Use 2x entries for reasonable load factor, minimum 16 buckets
  let num_buckets = std::cmp::max(16, raw_entries.len() * 2);
  let mut buckets = vec![0u32; num_buckets + 1];

  if raw_entries.is_empty() {
    return KeyIndexData {
      entries: raw_entries,
      buckets,
    };
  }

  // Sort entries by (bucket, hash64, string_id, node_id)
  let num_buckets_u64 = num_buckets as u64;
  raw_entries.sort_by(|a, b| {
    let a_bucket = (a.hash64 % num_buckets_u64) as usize;
    let b_bucket = (b.hash64 % num_buckets_u64) as usize;
    if a_bucket != b_bucket {
      return a_bucket.cmp(&b_bucket);
    }
    if a.hash64 != b.hash64 {
      return a.hash64.cmp(&b.hash64);
    }
    if a.string_id != b.string_id {
      return a.string_id.cmp(&b.string_id);
    }
    a.node_id.cmp(&b.node_id)
  });

  // Count entries per bucket
  let mut counts = vec![0u32; num_buckets];
  for entry in &raw_entries {
    let bucket = (entry.hash64 % num_buckets_u64) as usize;
    counts[bucket] += 1;
  }

  // Build offsets (prefix sum)
  for i in 0..num_buckets {
    buckets[i + 1] = buckets[i] + counts[i];
  }

  KeyIndexData {
    entries: raw_entries,
    buckets,
  }
}

// ============================================================================
// Property encoding
// ============================================================================

struct VectorTable {
  offsets: Vec<u64>,
  data: Vec<u8>,
}

impl VectorTable {
  fn new() -> Self {
    Self {
      offsets: vec![0],
      data: Vec::new(),
    }
  }

  fn push(&mut self, vec: &[f32]) -> u64 {
    for v in vec {
      self.data.extend_from_slice(&v.to_le_bytes());
    }
    let offset = self.data.len() as u64;
    self.offsets.push(offset);
    (self.offsets.len() - 2) as u64
  }

  fn is_empty(&self) -> bool {
    self.offsets.len() <= 1
  }
}

fn encode_prop_value(
  value: &PropValue,
  string_table: &StringTable,
  vectors: &mut VectorTable,
) -> (u8, u64) {
  match value {
    PropValue::Null => (PropValueTag::Null as u8, 0),
    PropValue::Bool(b) => (PropValueTag::Bool as u8, if *b { 1 } else { 0 }),
    PropValue::I64(v) => (PropValueTag::I64 as u8, *v as u64),
    PropValue::F64(v) => (PropValueTag::F64 as u8, v.to_bits()),
    PropValue::String(s) => {
      let string_id = string_table.string_to_id.get(s).copied().unwrap_or(0);
      (PropValueTag::String as u8, string_id as u64)
    }
    PropValue::VectorF32(vec) => (PropValueTag::VectorF32 as u8, vectors.push(vec)),
  }
}

// ============================================================================
// Section data tracking
// ============================================================================

struct SectionData {
  id: SectionId,
  data: Vec<u8>,
  compression: CompressionType,
  uncompressed_size: u32,
}

// ============================================================================
// Main snapshot building
// ============================================================================

/// Build a snapshot and write it to disk
pub fn build_snapshot(db_path: &Path, input: SnapshotBuildInput) -> Result<String> {
  let SnapshotBuildInput {
    generation,
    mut nodes,
    edges,
    labels,
    etypes,
    propkeys,
    compression,
  } = input;

  // Sort nodes by NodeID for deterministic ordering
  nodes.sort_by_key(|n| n.node_id);

  // Build mappings
  let phys_to_node_id: Vec<NodeId> = nodes.iter().map(|n| n.node_id).collect();
  let mut node_id_to_phys: HashMap<NodeId, PhysNode> = HashMap::new();
  let mut max_node_id: NodeId = 0;

  for (i, node) in nodes.iter().enumerate() {
    node_id_to_phys.insert(node.node_id, i as PhysNode);
    if node.node_id > max_node_id {
      max_node_id = node.node_id;
    }
  }

  // Build string table
  let mut string_table = StringTable::new();

  // Intern label names
  let mut label_string_ids: Vec<StringId> = vec![0]; // LabelID 0 reserved
  let num_labels = labels.len();
  for i in 1..=num_labels {
    let name = labels.get(&(i as LabelId));
    label_string_ids.push(if let Some(n) = name {
      string_table.intern(n)
    } else {
      0
    });
  }

  // Intern etype names
  let mut etype_string_ids: Vec<StringId> = vec![0]; // ETypeID 0 reserved
  let num_etypes = etypes.len();
  for i in 1..=num_etypes {
    let name = etypes.get(&(i as ETypeId));
    etype_string_ids.push(if let Some(n) = name {
      string_table.intern(n)
    } else {
      0
    });
  }

  // Intern propkey names
  let mut propkey_string_ids: Vec<StringId> = vec![0]; // PropKeyID 0 reserved
  let num_propkeys = propkeys.len();
  for i in 1..=num_propkeys {
    let name = propkeys.get(&(i as PropKeyId));
    propkey_string_ids.push(if let Some(n) = name {
      string_table.intern(n)
    } else {
      0
    });
  }

  // Intern node keys
  let node_key_strings: Vec<StringId> = nodes
    .iter()
    .map(|node| {
      node
        .key
        .as_ref()
        .map(|k| string_table.intern(k))
        .unwrap_or(0)
    })
    .collect();

  // Build CSR
  let out_csr = build_out_edges_csr(&nodes, &edges, &node_id_to_phys);
  let in_csr = build_in_edges_csr(&nodes, &out_csr);

  // Build key index with buckets
  let key_index = build_key_index(&nodes, &node_key_strings);

  // Intern string property values before building property arrays (deterministic order)
  for node in &nodes {
    let mut sorted_props: Vec<_> = node.props.iter().collect();
    sorted_props.sort_by_key(|(k, _)| *k);
    for (_, value) in sorted_props {
      if let PropValue::String(s) = value {
        string_table.intern(s);
      }
    }
  }
  for edge in &edges {
    let mut sorted_props: Vec<_> = edge.props.iter().collect();
    sorted_props.sort_by_key(|(k, _)| *k);
    for (_, value) in sorted_props {
      if let PropValue::String(s) = value {
        string_table.intern(s);
      }
    }
  }

  // Check if we have properties
  let has_properties =
    nodes.iter().any(|n| !n.props.is_empty()) || edges.iter().any(|e| !e.props.is_empty());

  // Build node label offsets/ids (always included for v2)
  let mut node_label_offsets: Vec<u32> = Vec::with_capacity(nodes.len() + 1);
  let mut node_label_ids: Vec<u32> = Vec::new();
  node_label_offsets.push(0);
  for node in &nodes {
    let mut labels = node.labels.clone();
    labels.sort_unstable();
    labels.dedup();
    node_label_ids.extend(labels.iter().copied());
    node_label_offsets.push(node_label_ids.len() as u32);
  }

  // Get compression options
  let compression_opts = compression.unwrap_or_default();

  // Build all sections
  let mut section_data: Vec<SectionData> = Vec::new();
  let num_nodes = nodes.len();
  let num_edges = edges.len();
  let num_strings = string_table.len();

  // Helper to add section with optional compression
  let mut add_section = |id: SectionId, data: Vec<u8>| {
    let (compressed, compression_type) = maybe_compress(&data, &compression_opts);
    section_data.push(SectionData {
      id,
      uncompressed_size: data.len() as u32,
      data: compressed,
      compression: compression_type,
    });
  };

  // phys_to_nodeid: u64[num_nodes]
  {
    let mut data = vec![0u8; num_nodes * 8];
    for (i, &node_id) in phys_to_node_id.iter().enumerate() {
      write_u64(&mut data, i * 8, node_id);
    }
    add_section(SectionId::PhysToNodeId, data);
  }

  // nodeid_to_phys: i32[max_node_id + 1]
  {
    let size = (max_node_id + 1) as usize;
    let mut data = vec![0u8; size * 4];
    // Initialize all to -1
    for i in 0..size {
      write_i32(&mut data, i * 4, -1);
    }
    // Set valid mappings
    for (&node_id, &phys) in &node_id_to_phys {
      write_i32(&mut data, (node_id as usize) * 4, phys as i32);
    }
    add_section(SectionId::NodeIdToPhys, data);
  }

  // out_offsets: u32[num_nodes + 1]
  {
    let mut data = vec![0u8; (num_nodes + 1) * 4];
    for (i, &offset) in out_csr.offsets.iter().enumerate() {
      write_u32(&mut data, i * 4, offset);
    }
    add_section(SectionId::OutOffsets, data);
  }

  // out_dst: u32[num_edges]
  {
    let mut data = vec![0u8; num_edges * 4];
    for (i, &dst) in out_csr.dst.iter().enumerate() {
      write_u32(&mut data, i * 4, dst);
    }
    add_section(SectionId::OutDst, data);
  }

  // out_etype: u32[num_edges]
  {
    let mut data = vec![0u8; num_edges * 4];
    for (i, &etype) in out_csr.etype.iter().enumerate() {
      write_u32(&mut data, i * 4, etype);
    }
    add_section(SectionId::OutEtype, data);
  }

  // in_offsets: u32[num_nodes + 1]
  {
    let mut data = vec![0u8; (num_nodes + 1) * 4];
    for (i, &offset) in in_csr.offsets.iter().enumerate() {
      write_u32(&mut data, i * 4, offset);
    }
    add_section(SectionId::InOffsets, data);
  }

  // in_src: u32[num_edges]
  {
    let mut data = vec![0u8; num_edges * 4];
    for (i, &src) in in_csr.dst.iter().enumerate() {
      write_u32(&mut data, i * 4, src);
    }
    add_section(SectionId::InSrc, data);
  }

  // in_etype: u32[num_edges]
  {
    let mut data = vec![0u8; num_edges * 4];
    for (i, &etype) in in_csr.etype.iter().enumerate() {
      write_u32(&mut data, i * 4, etype);
    }
    add_section(SectionId::InEtype, data);
  }

  // in_out_index: u32[num_edges]
  {
    let mut data = vec![0u8; num_edges * 4];
    if let Some(ref out_index) = in_csr.out_index {
      for (i, &idx) in out_index.iter().enumerate() {
        write_u32(&mut data, i * 4, idx);
      }
    }
    add_section(SectionId::InOutIndex, data);
  }

  // string_offsets: u32[num_strings + 1] and string_bytes: u8[...]
  {
    let encoded_strings: Vec<Vec<u8>> = string_table
      .strings
      .iter()
      .map(|s| s.as_bytes().to_vec())
      .collect();
    let total_bytes: usize = encoded_strings.iter().map(|s| s.len()).sum();

    let mut offsets_data = vec![0u8; (num_strings + 1) * 4];
    let mut bytes_data = vec![0u8; total_bytes];

    let mut byte_offset = 0usize;
    for (i, encoded) in encoded_strings.iter().enumerate() {
      write_u32(&mut offsets_data, i * 4, byte_offset as u32);
      bytes_data[byte_offset..byte_offset + encoded.len()].copy_from_slice(encoded);
      byte_offset += encoded.len();
    }
    write_u32(&mut offsets_data, num_strings * 4, byte_offset as u32);

    add_section(SectionId::StringOffsets, offsets_data);
    add_section(SectionId::StringBytes, bytes_data);
  }

  // label_string_ids: u32[num_labels + 1]
  {
    let mut data = vec![0u8; label_string_ids.len() * 4];
    for (i, &string_id) in label_string_ids.iter().enumerate() {
      write_u32(&mut data, i * 4, string_id);
    }
    add_section(SectionId::LabelStringIds, data);
  }

  // etype_string_ids: u32[num_etypes + 1]
  {
    let mut data = vec![0u8; etype_string_ids.len() * 4];
    for (i, &string_id) in etype_string_ids.iter().enumerate() {
      write_u32(&mut data, i * 4, string_id);
    }
    add_section(SectionId::EtypeStringIds, data);
  }

  // propkey_string_ids: u32[num_propkeys + 1]
  {
    let mut data = vec![0u8; propkey_string_ids.len() * 4];
    for (i, &string_id) in propkey_string_ids.iter().enumerate() {
      write_u32(&mut data, i * 4, string_id);
    }
    add_section(SectionId::PropkeyStringIds, data);
  }

  // node_key_string: u32[num_nodes]
  {
    let mut data = vec![0u8; num_nodes * 4];
    for (i, &string_id) in node_key_strings.iter().enumerate() {
      write_u32(&mut data, i * 4, string_id);
    }
    add_section(SectionId::NodeKeyString, data);
  }

  // node_label_offsets: u32[num_nodes + 1]
  {
    let mut data = vec![0u8; node_label_offsets.len() * 4];
    for (i, &offset) in node_label_offsets.iter().enumerate() {
      write_u32(&mut data, i * 4, offset);
    }
    add_section(SectionId::NodeLabelOffsets, data);
  }

  // node_label_ids: u32[total_labels]
  {
    let mut data = vec![0u8; node_label_ids.len() * 4];
    for (i, &label_id) in node_label_ids.iter().enumerate() {
      write_u32(&mut data, i * 4, label_id);
    }
    add_section(SectionId::NodeLabelIds, data);
  }

  // key_entries
  {
    let mut data = vec![0u8; key_index.entries.len() * KEY_INDEX_ENTRY_SIZE];
    for (i, entry) in key_index.entries.iter().enumerate() {
      let offset = i * KEY_INDEX_ENTRY_SIZE;
      write_u64(&mut data, offset, entry.hash64);
      write_u32(&mut data, offset + 8, entry.string_id);
      write_u32(&mut data, offset + 12, 0); // reserved
      write_u64(&mut data, offset + 16, entry.node_id);
    }
    add_section(SectionId::KeyEntries, data);
  }

  // key_buckets: u32[num_buckets + 1]
  {
    let mut data = vec![0u8; key_index.buckets.len() * 4];
    for (i, &bucket) in key_index.buckets.iter().enumerate() {
      write_u32(&mut data, i * 4, bucket);
    }
    add_section(SectionId::KeyBuckets, data);
  }

  // Node property sections
  let mut vector_table = VectorTable::new();
  {
    let mut node_prop_offsets = vec![0u32; num_nodes + 1];
    let mut node_prop_keys: Vec<u32> = Vec::new();
    let mut node_prop_vals: Vec<(u8, u64)> = Vec::new();

    for (i, node) in nodes.iter().enumerate() {
      node_prop_offsets[i] = node_prop_keys.len() as u32;

      // Sort props by key ID for deterministic output
      let mut sorted_props: Vec<_> = node.props.iter().collect();
      sorted_props.sort_by_key(|(k, _)| *k);

      for (&key_id, value) in sorted_props {
        node_prop_keys.push(key_id);
        node_prop_vals.push(encode_prop_value(value, &string_table, &mut vector_table));
      }
    }
    node_prop_offsets[num_nodes] = node_prop_keys.len() as u32;

    // Write offsets
    let mut offsets_data = vec![0u8; (num_nodes + 1) * 4];
    for (i, &offset) in node_prop_offsets.iter().enumerate() {
      write_u32(&mut offsets_data, i * 4, offset);
    }
    add_section(SectionId::NodePropOffsets, offsets_data);

    // Write keys
    let mut keys_data = vec![0u8; node_prop_keys.len() * 4];
    for (i, &key) in node_prop_keys.iter().enumerate() {
      write_u32(&mut keys_data, i * 4, key);
    }
    add_section(SectionId::NodePropKeys, keys_data);

    // Write values (16 bytes each: 1 byte tag, 7 bytes pad, 8 bytes payload)
    let mut vals_data = vec![0u8; node_prop_vals.len() * PROP_VALUE_DISK_SIZE];
    for (i, (tag, payload)) in node_prop_vals.iter().enumerate() {
      let offset = i * PROP_VALUE_DISK_SIZE;
      vals_data[offset] = *tag;
      // 7 bytes of padding (already 0)
      write_u64(&mut vals_data, offset + 8, *payload);
    }
    add_section(SectionId::NodePropVals, vals_data);
  }

  // Edge property sections
  {
    // Build a mapping from edge key to edge props
    let mut edge_prop_map: HashMap<(PhysNode, ETypeId, PhysNode), &HashMap<PropKeyId, PropValue>> =
      HashMap::new();
    for edge in &edges {
      if !edge.props.is_empty() {
        if let (Some(&src_phys), Some(&dst_phys)) = (
          node_id_to_phys.get(&edge.src),
          node_id_to_phys.get(&edge.dst),
        ) {
          edge_prop_map.insert((src_phys, edge.etype, dst_phys), &edge.props);
        }
      }
    }

    let mut edge_prop_offsets = vec![0u32; num_edges + 1];
    let mut edge_prop_keys: Vec<u32> = Vec::new();
    let mut edge_prop_vals: Vec<(u8, u64)> = Vec::new();

    // Iterate edges in CSR order
    let mut edge_idx = 0usize;
    for src_phys in 0..num_nodes {
      let start = out_csr.offsets[src_phys] as usize;
      let end = out_csr.offsets[src_phys + 1] as usize;

      for i in start..end {
        edge_prop_offsets[edge_idx] = edge_prop_keys.len() as u32;
        let dst_phys = out_csr.dst[i];
        let etype = out_csr.etype[i];

        if let Some(props) = edge_prop_map.get(&(src_phys as PhysNode, etype, dst_phys)) {
          let mut sorted_props: Vec<_> = props.iter().collect();
          sorted_props.sort_by_key(|(k, _)| *k);

          for (&key_id, value) in sorted_props {
            edge_prop_keys.push(key_id);
            edge_prop_vals.push(encode_prop_value(value, &string_table, &mut vector_table));
          }
        }
        edge_idx += 1;
      }
    }
    edge_prop_offsets[num_edges] = edge_prop_keys.len() as u32;

    // Write offsets
    let mut offsets_data = vec![0u8; (num_edges + 1) * 4];
    for (i, &offset) in edge_prop_offsets.iter().enumerate() {
      write_u32(&mut offsets_data, i * 4, offset);
    }
    add_section(SectionId::EdgePropOffsets, offsets_data);

    // Write keys
    let mut keys_data = vec![0u8; edge_prop_keys.len() * 4];
    for (i, &key) in edge_prop_keys.iter().enumerate() {
      write_u32(&mut keys_data, i * 4, key);
    }
    add_section(SectionId::EdgePropKeys, keys_data);

    // Write values
    let mut vals_data = vec![0u8; edge_prop_vals.len() * PROP_VALUE_DISK_SIZE];
    for (i, (tag, payload)) in edge_prop_vals.iter().enumerate() {
      let offset = i * PROP_VALUE_DISK_SIZE;
      vals_data[offset] = *tag;
      write_u64(&mut vals_data, offset + 8, *payload);
    }
    add_section(SectionId::EdgePropVals, vals_data);
  }

  let has_vectors = !vector_table.is_empty();

  // Vector sections (shared for node/edge vector props)
  if has_vectors {
    let mut offsets_data = vec![0u8; vector_table.offsets.len() * 8];
    for (i, &offset) in vector_table.offsets.iter().enumerate() {
      write_u64(&mut offsets_data, i * 8, offset);
    }
    add_section(SectionId::VectorOffsets, offsets_data);
    add_section(SectionId::VectorData, vector_table.data);
  }

  // Calculate total size with alignment
  let header_size = SNAPSHOT_HEADER_SIZE;
  let section_table_size = SectionId::COUNT * SECTION_ENTRY_SIZE;
  let mut data_offset = align_up(header_size + section_table_size, SECTION_ALIGNMENT);

  // Track section offsets
  let mut section_offsets: HashMap<SectionId, (u64, u64, CompressionType, u32)> = HashMap::new();

  for section in &section_data {
    section_offsets.insert(
      section.id,
      (
        data_offset as u64,
        section.data.len() as u64,
        section.compression,
        section.uncompressed_size,
      ),
    );
    data_offset = align_up(data_offset + section.data.len(), SECTION_ALIGNMENT);
  }

  // Build final buffer
  let total_size = data_offset + 4; // +4 for footer CRC
  let mut buffer = vec![0u8; total_size];

  // Write header
  let mut offset = 0;
  write_u32(&mut buffer, offset, MAGIC_SNAPSHOT);
  offset += 4;
  write_u32(&mut buffer, offset, VERSION_SNAPSHOT);
  offset += 4;
  write_u32(&mut buffer, offset, MIN_READER_SNAPSHOT);
  offset += 4;

  // Flags
  let mut flags = SnapshotFlags::HAS_IN_EDGES | SnapshotFlags::HAS_NODE_LABELS;
  if has_properties {
    flags |= SnapshotFlags::HAS_PROPERTIES;
  }
  if key_index.buckets.len() > 1 {
    flags |= SnapshotFlags::HAS_KEY_BUCKETS;
  }
  if has_vectors {
    flags |= SnapshotFlags::HAS_VECTORS;
  }
  write_u32(&mut buffer, offset, flags.bits());
  offset += 4;

  write_u64(&mut buffer, offset, generation);
  offset += 8;

  // created_unix_ns - current timestamp in nanoseconds
  let created_unix_ns = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.as_nanos() as u64)
    .unwrap_or(0);
  write_u64(&mut buffer, offset, created_unix_ns);
  offset += 8;

  write_u64(&mut buffer, offset, num_nodes as u64);
  offset += 8;
  write_u64(&mut buffer, offset, num_edges as u64);
  offset += 8;
  write_u64(&mut buffer, offset, max_node_id);
  offset += 8;
  write_u64(&mut buffer, offset, num_labels as u64);
  offset += 8;
  write_u64(&mut buffer, offset, num_etypes as u64);
  offset += 8;
  write_u64(&mut buffer, offset, num_propkeys as u64);
  offset += 8;
  write_u64(&mut buffer, offset, num_strings as u64);

  // Write section table
  offset = header_size;
  for id_num in 0..SectionId::COUNT {
    let id = SectionId::from_u32(id_num as u32).unwrap();
    let (sec_offset, sec_length, compression, uncompressed_size) = section_offsets
      .get(&id)
      .copied()
      .unwrap_or((0, 0, CompressionType::None, 0));

    write_u64(&mut buffer, offset, sec_offset);
    offset += 8;
    write_u64(&mut buffer, offset, sec_length);
    offset += 8;
    write_u32(&mut buffer, offset, compression as u32);
    offset += 4;
    write_u32(&mut buffer, offset, uncompressed_size);
    offset += 4;
  }

  // Write section data
  for section in &section_data {
    let (sec_offset, _, _, _) = section_offsets[&section.id];
    buffer[sec_offset as usize..sec_offset as usize + section.data.len()]
      .copy_from_slice(&section.data);
  }

  // Write footer CRC (over entire file except CRC itself)
  let footer_crc = crc32c(&buffer[..total_size - 4]);
  write_u32(&mut buffer, total_size - 4, footer_crc);

  // Write to file
  let snapshots_dir = db_path.join(SNAPSHOTS_DIR);
  fs::create_dir_all(&snapshots_dir)?;

  let filename = snapshot_filename(generation);
  let filepath = snapshots_dir.join(&filename);

  let mut file = File::create(&filepath)?;
  file.write_all(&buffer)?;
  file.sync_all()?;

  Ok(filepath.to_string_lossy().to_string())
}

/// Build a snapshot to memory (useful for single-file format embedding)
pub fn build_snapshot_to_memory(input: SnapshotBuildInput) -> Result<Vec<u8>> {
  // This is a simplified version that returns the buffer instead of writing to disk
  // We'll reuse most of the logic but skip the file I/O

  let SnapshotBuildInput {
    generation,
    mut nodes,
    edges,
    labels,
    etypes,
    propkeys,
    compression,
  } = input;

  // Sort nodes by NodeID for deterministic ordering
  nodes.sort_by_key(|n| n.node_id);

  // Build mappings
  let phys_to_node_id: Vec<NodeId> = nodes.iter().map(|n| n.node_id).collect();
  let mut node_id_to_phys: HashMap<NodeId, PhysNode> = HashMap::new();
  let mut max_node_id: NodeId = 0;

  for (i, node) in nodes.iter().enumerate() {
    node_id_to_phys.insert(node.node_id, i as PhysNode);
    if node.node_id > max_node_id {
      max_node_id = node.node_id;
    }
  }

  // Build string table
  let mut string_table = StringTable::new();

  // Intern label names
  let mut label_string_ids: Vec<StringId> = vec![0];
  let num_labels = labels.len();
  for i in 1..=num_labels {
    let name = labels.get(&(i as LabelId));
    label_string_ids.push(if let Some(n) = name {
      string_table.intern(n)
    } else {
      0
    });
  }

  // Intern etype names
  let mut etype_string_ids: Vec<StringId> = vec![0];
  let num_etypes = etypes.len();
  for i in 1..=num_etypes {
    let name = etypes.get(&(i as ETypeId));
    etype_string_ids.push(if let Some(n) = name {
      string_table.intern(n)
    } else {
      0
    });
  }

  // Intern propkey names
  let mut propkey_string_ids: Vec<StringId> = vec![0];
  let num_propkeys = propkeys.len();
  for i in 1..=num_propkeys {
    let name = propkeys.get(&(i as PropKeyId));
    propkey_string_ids.push(if let Some(n) = name {
      string_table.intern(n)
    } else {
      0
    });
  }

  // Intern node keys
  let node_key_strings: Vec<StringId> = nodes
    .iter()
    .map(|node| {
      node
        .key
        .as_ref()
        .map(|k| string_table.intern(k))
        .unwrap_or(0)
    })
    .collect();

  // Build CSR
  let out_csr = build_out_edges_csr(&nodes, &edges, &node_id_to_phys);
  let in_csr = build_in_edges_csr(&nodes, &out_csr);

  // Build key index
  let key_index = build_key_index(&nodes, &node_key_strings);

  // Intern string property values (deterministic order)
  for node in &nodes {
    let mut sorted_props: Vec<_> = node.props.iter().collect();
    sorted_props.sort_by_key(|(k, _)| *k);
    for (_, value) in sorted_props {
      if let PropValue::String(s) = value {
        string_table.intern(s);
      }
    }
  }
  for edge in &edges {
    let mut sorted_props: Vec<_> = edge.props.iter().collect();
    sorted_props.sort_by_key(|(k, _)| *k);
    for (_, value) in sorted_props {
      if let PropValue::String(s) = value {
        string_table.intern(s);
      }
    }
  }

  let has_properties =
    nodes.iter().any(|n| !n.props.is_empty()) || edges.iter().any(|e| !e.props.is_empty());

  // Build node label offsets/ids (always included for v2)
  let mut node_label_offsets: Vec<u32> = Vec::with_capacity(nodes.len() + 1);
  let mut node_label_ids: Vec<u32> = Vec::new();
  node_label_offsets.push(0);
  for node in &nodes {
    let mut labels = node.labels.clone();
    labels.sort_unstable();
    labels.dedup();
    node_label_ids.extend(labels.iter().copied());
    node_label_offsets.push(node_label_ids.len() as u32);
  }

  let compression_opts = compression.unwrap_or_default();
  let mut section_data: Vec<SectionData> = Vec::new();
  let num_nodes = nodes.len();
  let num_edges = edges.len();
  let num_strings = string_table.len();

  let mut add_section = |id: SectionId, data: Vec<u8>| {
    let (compressed, compression_type) = maybe_compress(&data, &compression_opts);
    section_data.push(SectionData {
      id,
      uncompressed_size: data.len() as u32,
      data: compressed,
      compression: compression_type,
    });
  };

  // Build all sections (same as above - abbreviated for space)
  // phys_to_nodeid
  {
    let mut data = vec![0u8; num_nodes * 8];
    for (i, &node_id) in phys_to_node_id.iter().enumerate() {
      write_u64(&mut data, i * 8, node_id);
    }
    add_section(SectionId::PhysToNodeId, data);
  }

  // nodeid_to_phys
  {
    let size = (max_node_id + 1) as usize;
    let mut data = vec![0u8; size * 4];
    for i in 0..size {
      write_i32(&mut data, i * 4, -1);
    }
    for (&node_id, &phys) in &node_id_to_phys {
      write_i32(&mut data, (node_id as usize) * 4, phys as i32);
    }
    add_section(SectionId::NodeIdToPhys, data);
  }

  // out_offsets
  {
    let mut data = vec![0u8; (num_nodes + 1) * 4];
    for (i, &offset) in out_csr.offsets.iter().enumerate() {
      write_u32(&mut data, i * 4, offset);
    }
    add_section(SectionId::OutOffsets, data);
  }

  // out_dst
  {
    let mut data = vec![0u8; num_edges * 4];
    for (i, &dst) in out_csr.dst.iter().enumerate() {
      write_u32(&mut data, i * 4, dst);
    }
    add_section(SectionId::OutDst, data);
  }

  // out_etype
  {
    let mut data = vec![0u8; num_edges * 4];
    for (i, &etype) in out_csr.etype.iter().enumerate() {
      write_u32(&mut data, i * 4, etype);
    }
    add_section(SectionId::OutEtype, data);
  }

  // in_offsets
  {
    let mut data = vec![0u8; (num_nodes + 1) * 4];
    for (i, &offset) in in_csr.offsets.iter().enumerate() {
      write_u32(&mut data, i * 4, offset);
    }
    add_section(SectionId::InOffsets, data);
  }

  // in_src
  {
    let mut data = vec![0u8; num_edges * 4];
    for (i, &src) in in_csr.dst.iter().enumerate() {
      write_u32(&mut data, i * 4, src);
    }
    add_section(SectionId::InSrc, data);
  }

  // in_etype
  {
    let mut data = vec![0u8; num_edges * 4];
    for (i, &etype) in in_csr.etype.iter().enumerate() {
      write_u32(&mut data, i * 4, etype);
    }
    add_section(SectionId::InEtype, data);
  }

  // in_out_index
  {
    let mut data = vec![0u8; num_edges * 4];
    if let Some(ref out_index) = in_csr.out_index {
      for (i, &idx) in out_index.iter().enumerate() {
        write_u32(&mut data, i * 4, idx);
      }
    }
    add_section(SectionId::InOutIndex, data);
  }

  // string_offsets and string_bytes
  {
    let encoded_strings: Vec<Vec<u8>> = string_table
      .strings
      .iter()
      .map(|s| s.as_bytes().to_vec())
      .collect();
    let total_bytes: usize = encoded_strings.iter().map(|s| s.len()).sum();

    let mut offsets_data = vec![0u8; (num_strings + 1) * 4];
    let mut bytes_data = vec![0u8; total_bytes];

    let mut byte_offset = 0usize;
    for (i, encoded) in encoded_strings.iter().enumerate() {
      write_u32(&mut offsets_data, i * 4, byte_offset as u32);
      bytes_data[byte_offset..byte_offset + encoded.len()].copy_from_slice(encoded);
      byte_offset += encoded.len();
    }
    write_u32(&mut offsets_data, num_strings * 4, byte_offset as u32);

    add_section(SectionId::StringOffsets, offsets_data);
    add_section(SectionId::StringBytes, bytes_data);
  }

  // label_string_ids
  {
    let mut data = vec![0u8; label_string_ids.len() * 4];
    for (i, &string_id) in label_string_ids.iter().enumerate() {
      write_u32(&mut data, i * 4, string_id);
    }
    add_section(SectionId::LabelStringIds, data);
  }

  // etype_string_ids
  {
    let mut data = vec![0u8; etype_string_ids.len() * 4];
    for (i, &string_id) in etype_string_ids.iter().enumerate() {
      write_u32(&mut data, i * 4, string_id);
    }
    add_section(SectionId::EtypeStringIds, data);
  }

  // propkey_string_ids
  {
    let mut data = vec![0u8; propkey_string_ids.len() * 4];
    for (i, &string_id) in propkey_string_ids.iter().enumerate() {
      write_u32(&mut data, i * 4, string_id);
    }
    add_section(SectionId::PropkeyStringIds, data);
  }

  // node_key_string
  {
    let mut data = vec![0u8; num_nodes * 4];
    for (i, &string_id) in node_key_strings.iter().enumerate() {
      write_u32(&mut data, i * 4, string_id);
    }
    add_section(SectionId::NodeKeyString, data);
  }

  // node_label_offsets
  {
    let mut data = vec![0u8; node_label_offsets.len() * 4];
    for (i, &offset) in node_label_offsets.iter().enumerate() {
      write_u32(&mut data, i * 4, offset);
    }
    add_section(SectionId::NodeLabelOffsets, data);
  }

  // node_label_ids
  {
    let mut data = vec![0u8; node_label_ids.len() * 4];
    for (i, &label_id) in node_label_ids.iter().enumerate() {
      write_u32(&mut data, i * 4, label_id);
    }
    add_section(SectionId::NodeLabelIds, data);
  }

  // key_entries
  {
    let mut data = vec![0u8; key_index.entries.len() * KEY_INDEX_ENTRY_SIZE];
    for (i, entry) in key_index.entries.iter().enumerate() {
      let offset = i * KEY_INDEX_ENTRY_SIZE;
      write_u64(&mut data, offset, entry.hash64);
      write_u32(&mut data, offset + 8, entry.string_id);
      write_u32(&mut data, offset + 12, 0);
      write_u64(&mut data, offset + 16, entry.node_id);
    }
    add_section(SectionId::KeyEntries, data);
  }

  // key_buckets
  {
    let mut data = vec![0u8; key_index.buckets.len() * 4];
    for (i, &bucket) in key_index.buckets.iter().enumerate() {
      write_u32(&mut data, i * 4, bucket);
    }
    add_section(SectionId::KeyBuckets, data);
  }

  // Node properties
  let mut vector_table = VectorTable::new();
  {
    let mut node_prop_offsets = vec![0u32; num_nodes + 1];
    let mut node_prop_keys: Vec<u32> = Vec::new();
    let mut node_prop_vals: Vec<(u8, u64)> = Vec::new();

    for (i, node) in nodes.iter().enumerate() {
      node_prop_offsets[i] = node_prop_keys.len() as u32;
      let mut sorted_props: Vec<_> = node.props.iter().collect();
      sorted_props.sort_by_key(|(k, _)| *k);
      for (&key_id, value) in sorted_props {
        node_prop_keys.push(key_id);
        node_prop_vals.push(encode_prop_value(value, &string_table, &mut vector_table));
      }
    }
    node_prop_offsets[num_nodes] = node_prop_keys.len() as u32;

    let mut offsets_data = vec![0u8; (num_nodes + 1) * 4];
    for (i, &offset) in node_prop_offsets.iter().enumerate() {
      write_u32(&mut offsets_data, i * 4, offset);
    }
    add_section(SectionId::NodePropOffsets, offsets_data);

    let mut keys_data = vec![0u8; node_prop_keys.len() * 4];
    for (i, &key) in node_prop_keys.iter().enumerate() {
      write_u32(&mut keys_data, i * 4, key);
    }
    add_section(SectionId::NodePropKeys, keys_data);

    let mut vals_data = vec![0u8; node_prop_vals.len() * PROP_VALUE_DISK_SIZE];
    for (i, (tag, payload)) in node_prop_vals.iter().enumerate() {
      let offset = i * PROP_VALUE_DISK_SIZE;
      vals_data[offset] = *tag;
      write_u64(&mut vals_data, offset + 8, *payload);
    }
    add_section(SectionId::NodePropVals, vals_data);
  }

  // Edge properties
  {
    let mut edge_prop_map: HashMap<(PhysNode, ETypeId, PhysNode), &HashMap<PropKeyId, PropValue>> =
      HashMap::new();
    for edge in &edges {
      if !edge.props.is_empty() {
        if let (Some(&src_phys), Some(&dst_phys)) = (
          node_id_to_phys.get(&edge.src),
          node_id_to_phys.get(&edge.dst),
        ) {
          edge_prop_map.insert((src_phys, edge.etype, dst_phys), &edge.props);
        }
      }
    }

    let mut edge_prop_offsets = vec![0u32; num_edges + 1];
    let mut edge_prop_keys: Vec<u32> = Vec::new();
    let mut edge_prop_vals: Vec<(u8, u64)> = Vec::new();

    let mut edge_idx = 0usize;
    for src_phys in 0..num_nodes {
      let start = out_csr.offsets[src_phys] as usize;
      let end = out_csr.offsets[src_phys + 1] as usize;

      for i in start..end {
        edge_prop_offsets[edge_idx] = edge_prop_keys.len() as u32;
        let dst_phys = out_csr.dst[i];
        let etype = out_csr.etype[i];

        if let Some(props) = edge_prop_map.get(&(src_phys as PhysNode, etype, dst_phys)) {
          let mut sorted_props: Vec<_> = props.iter().collect();
          sorted_props.sort_by_key(|(k, _)| *k);
          for (&key_id, value) in sorted_props {
            edge_prop_keys.push(key_id);
            edge_prop_vals.push(encode_prop_value(value, &string_table, &mut vector_table));
          }
        }
        edge_idx += 1;
      }
    }
    edge_prop_offsets[num_edges] = edge_prop_keys.len() as u32;

    let mut offsets_data = vec![0u8; (num_edges + 1) * 4];
    for (i, &offset) in edge_prop_offsets.iter().enumerate() {
      write_u32(&mut offsets_data, i * 4, offset);
    }
    add_section(SectionId::EdgePropOffsets, offsets_data);

    let mut keys_data = vec![0u8; edge_prop_keys.len() * 4];
    for (i, &key) in edge_prop_keys.iter().enumerate() {
      write_u32(&mut keys_data, i * 4, key);
    }
    add_section(SectionId::EdgePropKeys, keys_data);

    let mut vals_data = vec![0u8; edge_prop_vals.len() * PROP_VALUE_DISK_SIZE];
    for (i, (tag, payload)) in edge_prop_vals.iter().enumerate() {
      let offset = i * PROP_VALUE_DISK_SIZE;
      vals_data[offset] = *tag;
      write_u64(&mut vals_data, offset + 8, *payload);
    }
    add_section(SectionId::EdgePropVals, vals_data);
  }

  let has_vectors = !vector_table.is_empty();

  // Vector sections (shared for node/edge vector props)
  if has_vectors {
    let mut offsets_data = vec![0u8; vector_table.offsets.len() * 8];
    for (i, &offset) in vector_table.offsets.iter().enumerate() {
      write_u64(&mut offsets_data, i * 8, offset);
    }
    add_section(SectionId::VectorOffsets, offsets_data);
    add_section(SectionId::VectorData, vector_table.data);
  }

  // Calculate total size and offsets
  let header_size = SNAPSHOT_HEADER_SIZE;
  let section_table_size = SectionId::COUNT * SECTION_ENTRY_SIZE;
  let mut data_offset = align_up(header_size + section_table_size, SECTION_ALIGNMENT);

  let mut section_offsets: HashMap<SectionId, (u64, u64, CompressionType, u32)> = HashMap::new();
  for section in &section_data {
    section_offsets.insert(
      section.id,
      (
        data_offset as u64,
        section.data.len() as u64,
        section.compression,
        section.uncompressed_size,
      ),
    );
    data_offset = align_up(data_offset + section.data.len(), SECTION_ALIGNMENT);
  }

  // Build final buffer
  let total_size = data_offset + 4;
  let mut buffer = vec![0u8; total_size];

  // Write header
  let mut offset = 0;
  write_u32(&mut buffer, offset, MAGIC_SNAPSHOT);
  offset += 4;
  write_u32(&mut buffer, offset, VERSION_SNAPSHOT);
  offset += 4;
  write_u32(&mut buffer, offset, MIN_READER_SNAPSHOT);
  offset += 4;

  let mut flags = SnapshotFlags::HAS_IN_EDGES | SnapshotFlags::HAS_NODE_LABELS;
  if has_properties {
    flags |= SnapshotFlags::HAS_PROPERTIES;
  }
  if key_index.buckets.len() > 1 {
    flags |= SnapshotFlags::HAS_KEY_BUCKETS;
  }
  if has_vectors {
    flags |= SnapshotFlags::HAS_VECTORS;
  }
  write_u32(&mut buffer, offset, flags.bits());
  offset += 4;

  write_u64(&mut buffer, offset, generation);
  offset += 8;

  let created_unix_ns = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.as_nanos() as u64)
    .unwrap_or(0);
  write_u64(&mut buffer, offset, created_unix_ns);
  offset += 8;

  write_u64(&mut buffer, offset, num_nodes as u64);
  offset += 8;
  write_u64(&mut buffer, offset, num_edges as u64);
  offset += 8;
  write_u64(&mut buffer, offset, max_node_id);
  offset += 8;
  write_u64(&mut buffer, offset, num_labels as u64);
  offset += 8;
  write_u64(&mut buffer, offset, num_etypes as u64);
  offset += 8;
  write_u64(&mut buffer, offset, num_propkeys as u64);
  offset += 8;
  write_u64(&mut buffer, offset, num_strings as u64);

  // Write section table
  offset = header_size;
  for id_num in 0..SectionId::COUNT {
    let id = SectionId::from_u32(id_num as u32).unwrap();
    let (sec_offset, sec_length, compression, uncompressed_size) = section_offsets
      .get(&id)
      .copied()
      .unwrap_or((0, 0, CompressionType::None, 0));

    write_u64(&mut buffer, offset, sec_offset);
    offset += 8;
    write_u64(&mut buffer, offset, sec_length);
    offset += 8;
    write_u32(&mut buffer, offset, compression as u32);
    offset += 4;
    write_u32(&mut buffer, offset, uncompressed_size);
    offset += 4;
  }

  // Write section data
  for section in &section_data {
    let (sec_offset, _, _, _) = section_offsets[&section.id];
    buffer[sec_offset as usize..sec_offset as usize + section.data.len()]
      .copy_from_slice(&section.data);
  }

  // Write footer CRC
  let footer_crc = crc32c(&buffer[..total_size - 4]);
  write_u32(&mut buffer, total_size - 4, footer_crc);

  Ok(buffer)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;
  use crate::core::snapshot::reader::SnapshotData;
  use crate::util::crc::crc32c;
  use tempfile::tempdir;

  fn create_test_input() -> SnapshotBuildInput {
    let nodes = vec![
      NodeData {
        node_id: 1,
        key: Some("user:alice".to_string()),
        labels: vec![1],
        props: {
          let mut props = HashMap::new();
          props.insert(1, PropValue::String("Alice".to_string()));
          props.insert(2, PropValue::I64(30));
          props.insert(4, PropValue::VectorF32(vec![0.1, 0.2, 0.3]));
          props
        },
      },
      NodeData {
        node_id: 2,
        key: Some("user:bob".to_string()),
        labels: vec![1],
        props: {
          let mut props = HashMap::new();
          props.insert(1, PropValue::String("Bob".to_string()));
          props.insert(2, PropValue::I64(25));
          props
        },
      },
      NodeData {
        node_id: 3,
        key: None,
        labels: vec![2],
        props: HashMap::new(),
      },
    ];

    let edges = vec![
      EdgeData {
        src: 1,
        etype: 1,
        dst: 2,
        props: {
          let mut props = HashMap::new();
          props.insert(3, PropValue::F64(0.9));
          props
        },
      },
      EdgeData {
        src: 2,
        etype: 1,
        dst: 1,
        props: HashMap::new(),
      },
      EdgeData {
        src: 1,
        etype: 2,
        dst: 3,
        props: HashMap::new(),
      },
    ];

    let mut labels = HashMap::new();
    labels.insert(1, "Person".to_string());
    labels.insert(2, "Document".to_string());

    let mut etypes = HashMap::new();
    etypes.insert(1, "KNOWS".to_string());
    etypes.insert(2, "CREATED".to_string());

    let mut propkeys = HashMap::new();
    propkeys.insert(1, "name".to_string());
    propkeys.insert(2, "age".to_string());
    propkeys.insert(3, "weight".to_string());
    propkeys.insert(4, "embedding".to_string());

    SnapshotBuildInput {
      generation: 1,
      nodes,
      edges,
      labels,
      etypes,
      propkeys,
      compression: None,
    }
  }

  #[test]
  fn test_build_snapshot_to_memory() {
    let input = create_test_input();
    let buffer = build_snapshot_to_memory(input).unwrap();

    // Verify the buffer is non-empty and starts with correct magic
    assert!(buffer.len() > SNAPSHOT_HEADER_SIZE);
    assert_eq!(read_u32(&buffer, 0), MAGIC_SNAPSHOT);
    assert_eq!(read_u32(&buffer, 4), VERSION_SNAPSHOT);
    assert_eq!(read_u32(&buffer, 8), MIN_READER_SNAPSHOT);

    // Verify header fields
    let generation = read_u64(&buffer, 16);
    assert_eq!(generation, 1);

    let num_nodes = read_u64(&buffer, 32);
    assert_eq!(num_nodes, 3);

    let num_edges = read_u64(&buffer, 40);
    assert_eq!(num_edges, 3);

    let max_node_id = read_u64(&buffer, 48);
    assert_eq!(max_node_id, 3);

    // Verify CRC at the end
    let crc_offset = buffer.len() - 4;
    let stored_crc = read_u32(&buffer, crc_offset);
    let computed_crc = crc32c(&buffer[..crc_offset]);
    assert_eq!(stored_crc, computed_crc);
  }

  #[test]
  fn test_build_empty_snapshot() {
    let input = SnapshotBuildInput {
      generation: 1,
      nodes: vec![],
      edges: vec![],
      labels: HashMap::new(),
      etypes: HashMap::new(),
      propkeys: HashMap::new(),
      compression: None,
    };

    let buffer = build_snapshot_to_memory(input).unwrap();

    // Verify header
    assert_eq!(read_u32(&buffer, 0), MAGIC_SNAPSHOT);

    // Verify counts
    let num_nodes = read_u64(&buffer, 32);
    let num_edges = read_u64(&buffer, 40);
    assert_eq!(num_nodes, 0);
    assert_eq!(num_edges, 0);
  }

  #[test]
  fn test_string_table() {
    let mut table = StringTable::new();

    // First string (empty) is pre-populated
    assert_eq!(table.len(), 1);

    // Intern new strings
    let id1 = table.intern("hello");
    assert_eq!(id1, 1);

    let id2 = table.intern("world");
    assert_eq!(id2, 2);

    // Interning again returns same ID
    let id1_again = table.intern("hello");
    assert_eq!(id1_again, 1);

    assert_eq!(table.len(), 3);
  }

  #[test]
  fn test_csr_building() {
    let nodes = vec![
      NodeData {
        node_id: 1,
        key: None,
        labels: vec![],
        props: HashMap::new(),
      },
      NodeData {
        node_id: 2,
        key: None,
        labels: vec![],
        props: HashMap::new(),
      },
      NodeData {
        node_id: 3,
        key: None,
        labels: vec![],
        props: HashMap::new(),
      },
    ];

    let edges = vec![
      EdgeData {
        src: 1,
        etype: 1,
        dst: 2,
        props: HashMap::new(),
      },
      EdgeData {
        src: 1,
        etype: 1,
        dst: 3,
        props: HashMap::new(),
      },
      EdgeData {
        src: 2,
        etype: 2,
        dst: 1,
        props: HashMap::new(),
      },
    ];

    let mut node_id_to_phys = HashMap::new();
    node_id_to_phys.insert(1, 0);
    node_id_to_phys.insert(2, 1);
    node_id_to_phys.insert(3, 2);

    let out_csr = build_out_edges_csr(&nodes, &edges, &node_id_to_phys);

    // Check offsets - node 0 has 2 edges, node 1 has 1 edge, node 2 has 0 edges
    assert_eq!(out_csr.offsets, vec![0, 2, 3, 3]);
    assert_eq!(out_csr.dst.len(), 3);
    assert_eq!(out_csr.etype.len(), 3);

    // Build in-edges and verify
    let in_csr = build_in_edges_csr(&nodes, &out_csr);

    // Check in-edge offsets - node 0 has 1 in-edge, node 1 has 1, node 2 has 1
    assert_eq!(in_csr.offsets, vec![0, 1, 2, 3]);
  }

  #[test]
  fn test_snapshot_roundtrip() {
    // Create test data
    let input = create_test_input();

    // Write to temp directory
    let temp_dir = tempdir().unwrap();
    let filepath = build_snapshot(temp_dir.path(), input).unwrap();

    // Read it back
    let snapshot = SnapshotData::load(&filepath).unwrap();

    // Verify header
    assert_eq!(snapshot.header.generation, 1);
    assert_eq!(snapshot.header.num_nodes, 3);
    assert_eq!(snapshot.header.num_edges, 3);
    assert_eq!(snapshot.header.max_node_id, 3);
    assert_eq!(snapshot.header.num_labels, 2);
    assert_eq!(snapshot.header.num_etypes, 2);
    assert_eq!(snapshot.header.num_propkeys, 4);

    // Verify node existence
    assert!(snapshot.has_node(1));
    assert!(snapshot.has_node(2));
    assert!(snapshot.has_node(3));
    assert!(!snapshot.has_node(4));

    // Verify physical mappings
    let phys0 = snapshot.get_phys_node(1).unwrap();
    let phys1 = snapshot.get_phys_node(2).unwrap();
    let phys2 = snapshot.get_phys_node(3).unwrap();
    assert_eq!(snapshot.get_node_id(phys0), Some(1));
    assert_eq!(snapshot.get_node_id(phys1), Some(2));
    assert_eq!(snapshot.get_node_id(phys2), Some(3));

    // Verify key lookup
    assert_eq!(snapshot.lookup_by_key("user:alice"), Some(1));
    assert_eq!(snapshot.lookup_by_key("user:bob"), Some(2));
    assert_eq!(snapshot.lookup_by_key("user:nonexistent"), None);

    // Verify node keys
    assert_eq!(snapshot.get_node_key(phys0), Some("user:alice".to_string()));
    assert_eq!(snapshot.get_node_key(phys1), Some("user:bob".to_string()));
    assert_eq!(snapshot.get_node_key(phys2), None); // node 3 has no key

    // Verify edge existence (binary search)
    assert!(snapshot.has_edge(phys0, 1, phys1)); // 1 -[KNOWS]-> 2
    assert!(snapshot.has_edge(phys1, 1, phys0)); // 2 -[KNOWS]-> 1
    assert!(snapshot.has_edge(phys0, 2, phys2)); // 1 -[CREATED]-> 3
    assert!(!snapshot.has_edge(phys0, 1, phys2)); // No 1 -[KNOWS]-> 3

    // Verify out-degree
    assert_eq!(snapshot.get_out_degree(phys0), Some(2)); // node 1 has 2 outgoing
    assert_eq!(snapshot.get_out_degree(phys1), Some(1)); // node 2 has 1 outgoing
    assert_eq!(snapshot.get_out_degree(phys2), Some(0)); // node 3 has 0 outgoing

    // Verify in-degree
    assert_eq!(snapshot.get_in_degree(phys0), Some(1)); // node 1 has 1 incoming
    assert_eq!(snapshot.get_in_degree(phys1), Some(1)); // node 2 has 1 incoming
    assert_eq!(snapshot.get_in_degree(phys2), Some(1)); // node 3 has 1 incoming

    // Verify out-edge iteration
    let out_edges: Vec<_> = snapshot.iter_out_edges(phys0).collect();
    assert_eq!(out_edges.len(), 2);

    // Verify in-edge iteration
    let in_edges: Vec<_> = snapshot.iter_in_edges(phys1).collect();
    assert_eq!(in_edges.len(), 1);
    assert_eq!(in_edges[0].0, phys0); // source is phys0 (node 1)

    // Verify node properties
    let props = snapshot.get_node_props(phys0).unwrap();
    assert!(props.contains_key(&1)); // name
    assert!(props.contains_key(&2)); // age
    assert_eq!(props.get(&2), Some(&PropValue::I64(30)));

    // Verify specific property lookup
    assert_eq!(snapshot.get_node_prop(phys0, 2), Some(PropValue::I64(30)));
    assert_eq!(snapshot.get_node_prop(phys1, 2), Some(PropValue::I64(25)));

    // Verify string property values
    if let Some(PropValue::String(name)) = snapshot.get_node_prop(phys0, 1) {
      assert_eq!(name, "Alice");
    } else {
      panic!("Expected string property");
    }

    // Verify vector property values
    if let Some(PropValue::VectorF32(vec)) = snapshot.get_node_prop(phys0, 4) {
      assert_eq!(vec, vec![0.1, 0.2, 0.3]);
    } else {
      panic!("Expected vector property");
    }

    // Verify edge properties
    let edge_idx = snapshot.find_edge_index(phys0, 1, phys1).unwrap();
    let edge_props = snapshot.get_edge_props(edge_idx).unwrap();
    assert_eq!(edge_props.get(&3), Some(&PropValue::F64(0.9)));

    // Verify string table
    assert_eq!(snapshot.get_string(0), Some(String::new())); // empty string at 0
  }
}
