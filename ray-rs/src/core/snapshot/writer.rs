//! CSR Snapshot Writer
//!
//! Builds CSR snapshots from nodes and edges for checkpointing.
//! Ported from src/core/snapshot-writer.ts
//!
//! Memory: strings are stored once (in the StringBytes section data, keyed
//! by borrowed input strings), the CSR arrays are filled in place using the
//! per-node counts as cursors, and each section goes into the output buffer
//! as soon as it is encoded, so the writer holds roughly one snapshot of
//! section data plus its input.

use crate::constants::*;
use crate::core::snapshot::node_map::{self, NodeIdMapLayout};
use crate::core::snapshot::sections::inflation_budget;
use crate::error::{KiteError, Result};
use crate::types::*;
use crate::util::binary::*;
use crate::util::compression::{try_compress, CompressionOptions, CompressionType};
use crate::util::crc::crc32;
use crate::util::hash::xxhash64_string;
use crate::vector::ivf::serialize::serialize_manifest;
use crate::vector::types::VectorManifest;
use std::collections::HashMap;

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

/// Edges as columns: edge `i` is `(src[i], etype[i], dst[i])`, with
/// properties `props(i)`, sorted by key. A checkpoint of millions of edges
/// holds a few allocations instead of a property map per edge (`EdgeData`):
/// at one property an edge, about 60 bytes an edge instead of about 300.
#[derive(Debug, Default)]
pub(crate) struct EdgeColumns {
  src: Vec<NodeId>,
  etype: Vec<ETypeId>,
  dst: Vec<NodeId>,
  /// Where each edge's properties start in `props`, and where the last
  /// one's end: one more entry than edges.
  prop_starts: Vec<usize>,
  props: Vec<(PropKeyId, PropValue)>,
}

impl EdgeColumns {
  /// Room for `edges` edges with `props` properties in all.
  pub(crate) fn with_capacity(edges: usize, props: usize) -> Self {
    let mut prop_starts = Vec::with_capacity(edges + 1);
    prop_starts.push(0);
    Self {
      src: Vec::with_capacity(edges),
      etype: Vec::with_capacity(edges),
      dst: Vec::with_capacity(edges),
      prop_starts,
      props: Vec::with_capacity(props),
    }
  }

  pub(crate) fn len(&self) -> usize {
    self.src.len()
  }

  /// Append an edge whose properties `push_props` appends to the vector it
  /// is given (each key once, in any order).
  pub(crate) fn push_with(
    &mut self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    push_props: impl FnOnce(&mut Vec<(PropKeyId, PropValue)>),
  ) {
    let start = self.props.len();
    push_props(&mut self.props);
    let pushed = &mut self.props[start..];
    if !pushed.is_sorted_by_key(|(key, _)| *key) {
      pushed.sort_unstable_by_key(|(key, _)| *key);
    }
    self.src.push(src);
    self.etype.push(etype);
    self.dst.push(dst);
    self.prop_starts.push(self.props.len());
  }

  /// Edge `index`'s properties, sorted by key.
  pub(crate) fn props(&self, index: usize) -> &[(PropKeyId, PropValue)] {
    &self.props[self.prop_starts[index]..self.prop_starts[index + 1]]
  }

  /// Whether any edge has a property.
  fn has_props(&self) -> bool {
    !self.props.is_empty()
  }

  /// Keep the edges from `from` on for which `keep(src, dst)` holds, in
  /// order; returns how many were dropped.
  pub(crate) fn retain_from(
    &mut self,
    from: usize,
    mut keep: impl FnMut(NodeId, NodeId) -> bool,
  ) -> usize {
    let len = self.len();
    let kept: Vec<usize> = (from..len)
      .filter(|&index| keep(self.src[index], self.dst[index]))
      .collect();
    if kept.len() == len - from {
      return 0;
    }
    let tail_props: Vec<Vec<(PropKeyId, PropValue)>> = kept
      .iter()
      .map(|&index| self.props(index).to_vec())
      .collect();
    for (to, &index) in (from..).zip(&kept) {
      self.src[to] = self.src[index];
      self.etype[to] = self.etype[index];
      self.dst[to] = self.dst[index];
    }
    let kept_len = from + kept.len();
    self.src.truncate(kept_len);
    self.etype.truncate(kept_len);
    self.dst.truncate(kept_len);
    self.props.truncate(self.prop_starts[from]);
    self.prop_starts.truncate(from + 1);
    for props in tail_props {
      self.props.extend(props);
      self.prop_starts.push(self.props.len());
    }
    len - kept_len
  }

  fn from_edge_data(edges: Vec<EdgeData>) -> Self {
    let props = edges.iter().map(|edge| edge.props.len()).sum();
    let mut columns = Self::with_capacity(edges.len(), props);
    for edge in edges {
      columns.push_with(edge.src, edge.etype, edge.dst, |props| {
        props.extend(edge.props)
      });
    }
    columns
  }
}

/// `SnapshotBuildInput` with its edges as columns.
pub(crate) struct ColumnarBuildInput {
  pub(crate) generation: u64,
  pub(crate) nodes: Vec<NodeData>,
  pub(crate) edges: EdgeColumns,
  pub(crate) labels: HashMap<LabelId, String>,
  pub(crate) etypes: HashMap<ETypeId, String>,
  pub(crate) propkeys: HashMap<PropKeyId, String>,
  pub(crate) vector_stores: Option<HashMap<PropKeyId, VectorManifest>>,
  pub(crate) compression: Option<CompressionOptions>,
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
  pub vector_stores: Option<HashMap<PropKeyId, VectorManifest>>,
  pub compression: Option<CompressionOptions>,
}

/// Narrow a count or offset to the u32 the snapshot format stores. Fails the
/// build, before anything is written, instead of truncating.
fn checked_u32(value: usize, what: &str) -> Result<u32> {
  u32::try_from(value).map_err(|_| {
    KiteError::InvalidSnapshot(format!(
      "{what} {value} exceeds the snapshot format limit of {}",
      u32::MAX
    ))
  })
}

fn push_u32(data: &mut Vec<u8>, value: u32) {
  data.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(data: &mut Vec<u8>, value: u64) {
  data.extend_from_slice(&value.to_le_bytes());
}

fn encode_u32_slice(values: &[u32]) -> Vec<u8> {
  let mut data = Vec::with_capacity(values.len() * 4);
  for &value in values {
    push_u32(&mut data, value);
  }
  data
}

fn encode_u64_slice(values: &[u64]) -> Vec<u8> {
  let mut data = Vec::with_capacity(values.len() * 8);
  for &value in values {
    push_u64(&mut data, value);
  }
  data
}

// ============================================================================
// String table for interning
// ============================================================================

/// Interned strings. The bytes and offsets are the StringBytes and
/// StringOffsets section data; the ID map borrows the input strings, so each
/// string is stored once.
struct StringTable<'a> {
  bytes: Vec<u8>,
  /// String `id` is `bytes[offsets[id]..offsets[id + 1]]`.
  offsets: Vec<u64>,
  ids: hashbrown::HashMap<&'a str, StringId>,
}

impl<'a> StringTable<'a> {
  fn new() -> Self {
    // StringID 0 is reserved/empty
    let mut ids = hashbrown::HashMap::new();
    ids.insert("", 0);
    Self {
      bytes: Vec::new(),
      offsets: vec![0, 0],
      ids,
    }
  }

  fn intern(&mut self, s: &'a str) -> Result<StringId> {
    if let Some(&id) = self.ids.get(s) {
      return Ok(id);
    }
    let id = checked_u32(self.len(), "string count")?;
    self.bytes.extend_from_slice(s.as_bytes());
    self.offsets.push(self.bytes.len() as u64);
    self.ids.insert(s, id);
    Ok(id)
  }

  /// ID of an interned string (0 if it was never interned).
  fn id(&self, s: &str) -> StringId {
    self.ids.get(s).copied().unwrap_or(0)
  }

  fn len(&self) -> usize {
    self.offsets.len() - 1
  }
}

// ============================================================================
// Node index
// ============================================================================

/// NodeID -> physical node, through the NodeIdToPhys section data itself.
struct NodeIndex {
  layout: NodeIdMapLayout,
  map: Vec<u8>,
}

impl NodeIndex {
  /// `phys_to_node_id` lists node IDs in physical order, ascending.
  fn build(phys_to_node_id: &[NodeId], max_node_id: NodeId) -> Result<Self> {
    if let Some(pair) = phys_to_node_id.windows(2).find(|pair| pair[0] == pair[1]) {
      return Err(KiteError::InvalidSnapshot(format!(
        "duplicate node ID {} in snapshot input",
        pair[0]
      )));
    }
    let (layout, map) = node_map::encode(phys_to_node_id, max_node_id)?;
    Ok(Self { layout, map })
  }

  #[inline]
  fn phys(&self, node_id: NodeId) -> Option<PhysNode> {
    match self.layout {
      NodeIdMapLayout::Dense => node_map::dense_lookup(&self.map, node_id),
      NodeIdMapLayout::Sparse => node_map::sparse_lookup(&self.map, node_id),
    }
  }

  /// Physical node of an endpoint of edge `(src, dst)`, or an error naming
  /// the edge.
  #[inline]
  fn endpoint(&self, node_id: NodeId, (src, dst): (NodeId, NodeId)) -> Result<PhysNode> {
    self.phys(node_id).ok_or_else(|| {
      KiteError::InvalidSnapshot(format!(
        "Edge references missing node(s): src={src}, dst={dst}"
      ))
    })
  }
}

// ============================================================================
// CSR building
// ============================================================================

struct CSRData {
  offsets: Vec<u32>,
  /// Destination (out-edges) or source (in-edges) per edge.
  dst: Vec<u32>,
  etype: Vec<u32>,
  /// For in-edges: index back to out-edge
  out_index: Option<Vec<u32>>,
  /// For out-edges: the input index of each edge, until its properties
  /// are written (`edge_prop_sections`)
  input_index: Option<Vec<u32>>,
}

/// Prefix sums of per-node counts; `counts` must fit u32 in total.
fn prefix_offsets(counts: &[u32]) -> Vec<u32> {
  let mut offsets = Vec::with_capacity(counts.len() + 1);
  let mut total = 0u32;
  offsets.push(0);
  for &count in counts {
    total += count;
    offsets.push(total);
  }
  offsets
}

/// Callers bound `edges.len()` by u32::MAX (see `build_snapshot_to_memory`),
/// so per-node counts and prefix sums fit u32. Edges are placed in input
/// order using the offsets as cursors, then each node's range is sorted by
/// (etype, dst, input index) unless it already is by (etype, dst).
fn build_out_edges_csr(
  num_nodes: usize,
  edges: &EdgeColumns,
  index: &NodeIndex,
) -> Result<CSRData> {
  // Consecutive edges usually share a source; resolve it once.
  let mut last_src: Option<(NodeId, PhysNode)> = None;
  let mut src_phys = |edge: usize| -> Result<PhysNode> {
    let src = edges.src[edge];
    match last_src {
      Some((node_id, phys)) if node_id == src => Ok(phys),
      _ => {
        let phys = index.endpoint(src, (src, edges.dst[edge]))?;
        last_src = Some((src, phys));
        Ok(phys)
      }
    }
  };

  let mut counts = vec![0u32; num_nodes];
  for edge in 0..edges.len() {
    counts[src_phys(edge)? as usize] += 1;
  }
  let offsets = prefix_offsets(&counts);
  // Reuse the counts as per-node write cursors.
  let mut cursors = counts;
  cursors.copy_from_slice(&offsets[..num_nodes]);

  let mut dst = vec![0u32; edges.len()];
  let mut etype = vec![0u32; edges.len()];
  let mut input_index = vec![0u32; edges.len()];
  for edge in 0..edges.len() {
    let src = src_phys(edge)? as usize;
    let pos = cursors[src] as usize;
    cursors[src] += 1;
    dst[pos] = index.endpoint(edges.dst[edge], (edges.src[edge], edges.dst[edge]))?;
    etype[pos] = edges.etype[edge];
    input_index[pos] = edge as u32;
  }
  drop(cursors);

  let mut scratch: Vec<(ETypeId, PhysNode, u32)> = Vec::new();
  for node in 0..num_nodes {
    let range = offsets[node] as usize..offsets[node + 1] as usize;
    let sorted = range
      .clone()
      .skip(1)
      .all(|i| (etype[i - 1], dst[i - 1]) <= (etype[i], dst[i]));
    if sorted {
      continue;
    }
    scratch.clear();
    scratch.extend(range.clone().map(|i| (etype[i], dst[i], input_index[i])));
    // Input indices are distinct, so the order is deterministic.
    scratch.sort_unstable();
    for (i, &(edge_etype, edge_dst, edge)) in range.zip(&scratch) {
      etype[i] = edge_etype;
      dst[i] = edge_dst;
      input_index[i] = edge;
    }
  }

  Ok(CSRData {
    offsets,
    dst,
    etype,
    out_index: None,
    input_index: Some(input_index),
  })
}

/// Same u32 bounds as `build_out_edges_csr`. Out-edges are visited in index
/// order, so each node's in-edges arrive sorted by source; a range needs
/// sorting by (etype, src, out index) only when its edge types are mixed.
fn build_in_edges_csr(num_nodes: usize, out_csr: &CSRData) -> CSRData {
  let num_edges = out_csr.dst.len();

  let mut counts = vec![0u32; num_nodes];
  for &dst in &out_csr.dst {
    counts[dst as usize] += 1;
  }
  let offsets = prefix_offsets(&counts);
  let mut cursors = counts;
  cursors.copy_from_slice(&offsets[..num_nodes]);

  let mut src_arr = vec![0u32; num_edges];
  let mut etype_arr = vec![0u32; num_edges];
  let mut out_index = vec![0u32; num_edges];
  for src_phys in 0..num_nodes {
    let start = out_csr.offsets[src_phys] as usize;
    let end = out_csr.offsets[src_phys + 1] as usize;
    for out_idx in start..end {
      let dst = out_csr.dst[out_idx] as usize;
      let pos = cursors[dst] as usize;
      cursors[dst] += 1;
      src_arr[pos] = src_phys as PhysNode;
      etype_arr[pos] = out_csr.etype[out_idx];
      out_index[pos] = out_idx as u32;
    }
  }
  drop(cursors);

  let mut scratch: Vec<(ETypeId, PhysNode, u32)> = Vec::new();
  for node in 0..num_nodes {
    let range = offsets[node] as usize..offsets[node + 1] as usize;
    if range
      .clone()
      .skip(1)
      .all(|i| etype_arr[i - 1] <= etype_arr[i])
    {
      continue;
    }
    scratch.clear();
    scratch.extend(
      range
        .clone()
        .map(|i| (etype_arr[i], src_arr[i], out_index[i])),
    );
    scratch.sort_unstable();
    for (i, &(etype, src, out_idx)) in range.zip(&scratch) {
      etype_arr[i] = etype;
      src_arr[i] = src;
      out_index[i] = out_idx;
    }
  }

  CSRData {
    offsets,
    dst: src_arr, // For in-edges, "dst" is actually source
    etype: etype_arr,
    out_index: Some(out_index),
    input_index: None,
  }
}

// ============================================================================
// Key index building
// ============================================================================

/// KeyEntries sorted by (bucket, hash64, string_id, node_id), and the
/// KeyBuckets offsets, as section data. `node_key_strings` is per node.
fn build_key_index(nodes: &[NodeData], node_key_strings: &[StringId]) -> (Vec<u8>, Vec<u8>) {
  let mut entries: Vec<(u64, StringId, NodeId)> = nodes
    .iter()
    .zip(node_key_strings)
    .filter_map(|(node, &string_id)| {
      let key = node.key.as_deref()?;
      Some((xxhash64_string(key), string_id, node.node_id))
    })
    .collect();

  // Use 2x entries for reasonable load factor, minimum 16 buckets
  let num_buckets = std::cmp::max(16, entries.len() * 2);
  let num_buckets_u64 = num_buckets as u64;
  entries.sort_unstable_by_key(|&(hash, string_id, node_id)| {
    (hash % num_buckets_u64, hash, string_id, node_id)
  });

  let mut counts = vec![0u32; num_buckets];
  let mut entry_data = Vec::with_capacity(entries.len() * KEY_INDEX_ENTRY_SIZE);
  for &(hash, string_id, node_id) in &entries {
    counts[(hash % num_buckets_u64) as usize] += 1;
    push_u64(&mut entry_data, hash);
    push_u32(&mut entry_data, string_id);
    push_u32(&mut entry_data, 0);
    push_u64(&mut entry_data, node_id);
  }
  (entry_data, encode_u32_slice(&prefix_offsets(&counts)))
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
  string_table: &StringTable<'_>,
  vectors: &mut VectorTable,
) -> (u8, u64) {
  match value {
    PropValue::Null => (PropValueTag::Null as u8, 0),
    PropValue::Bool(b) => (PropValueTag::Bool as u8, if *b { 1 } else { 0 }),
    PropValue::I64(v) => (PropValueTag::I64 as u8, *v as u64),
    PropValue::F64(v) => (PropValueTag::F64 as u8, v.to_bits()),
    PropValue::String(s) => (PropValueTag::String as u8, string_table.id(s) as u64),
    PropValue::VectorF32(vec) => (PropValueTag::VectorF32 as u8, vectors.push(vec)),
  }
}

/// Offsets, keys and values sections for one property list per item.
struct PropSections {
  offsets: Vec<u8>,
  keys: Vec<u8>,
  vals: Vec<u8>,
  count: usize,
}

impl PropSections {
  fn with_items(items: usize) -> Self {
    Self {
      offsets: Vec::with_capacity((items + 1) * 4),
      keys: Vec::new(),
      vals: Vec::new(),
      count: 0,
    }
  }

  /// Appends the next item's properties, sorted by key; `scratch` is a
  /// buffer for the sort.
  fn push_item<'p>(
    &mut self,
    props: Option<&'p HashMap<PropKeyId, PropValue>>,
    scratch: &mut Vec<(PropKeyId, &'p PropValue)>,
    string_table: &StringTable<'_>,
    vectors: &mut VectorTable,
    what: &str,
  ) -> Result<()> {
    scratch.clear();
    if let Some(props) = props {
      scratch.extend(props.iter().map(|(&key, value)| (key, value)));
      scratch.sort_unstable_by_key(|&(key, _)| key);
    }
    self.push_sorted(scratch.iter().copied(), string_table, vectors, what)
  }

  /// Appends the next item's properties, `props`, sorted by key.
  fn push_sorted<'p>(
    &mut self,
    props: impl Iterator<Item = (PropKeyId, &'p PropValue)>,
    string_table: &StringTable<'_>,
    vectors: &mut VectorTable,
    what: &str,
  ) -> Result<()> {
    push_u32(&mut self.offsets, checked_u32(self.count, what)?);
    for (key_id, value) in props {
      let (tag, payload) = encode_prop_value(value, string_table, vectors);
      push_u32(&mut self.keys, key_id);
      self.vals.push(tag);
      self.vals.extend_from_slice(&[0; 7]);
      push_u64(&mut self.vals, payload);
      self.count += 1;
    }
    Ok(())
  }

  /// Writes the final offset and the three sections.
  fn write(mut self, out: &mut SectionWriter<'_>, ids: [SectionId; 3], what: &str) -> Result<()> {
    push_u32(&mut self.offsets, checked_u32(self.count, what)?);
    out.add(ids[0], self.offsets);
    out.add(ids[1], self.keys);
    out.add(ids[2], self.vals);
    Ok(())
  }
}

fn vector_store_sections(
  vector_stores: Option<&HashMap<PropKeyId, VectorManifest>>,
) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
  let Some(vector_stores) = vector_stores.filter(|stores| !stores.is_empty()) else {
    return Ok(None);
  };

  let mut ordered: Vec<(PropKeyId, &VectorManifest)> =
    vector_stores.iter().map(|(&k, v)| (k, v)).collect();
  ordered.sort_by_key(|(prop_key_id, _)| *prop_key_id);

  let mut index_data = Vec::with_capacity(4 + ordered.len() * 20);
  push_u32(
    &mut index_data,
    checked_u32(ordered.len(), "vector store count")?,
  );
  let mut blob_data = Vec::new();
  for (prop_key_id, manifest) in ordered {
    let encoded = serialize_manifest(manifest);
    push_u32(&mut index_data, prop_key_id);
    push_u64(&mut index_data, blob_data.len() as u64);
    push_u64(&mut index_data, encoded.len() as u64);
    blob_data.extend_from_slice(&encoded);
  }
  Ok(Some((index_data, blob_data)))
}

// ============================================================================
// Section output
// ============================================================================

#[derive(Clone, Copy, Default)]
struct TableEntry {
  offset: u64,
  length: u64,
  compression: u32,
  uncompressed_size: u64,
}

/// The snapshot buffer: header and section table space, then each section's
/// data, 64-byte aligned, appended as it is added.
struct SectionWriter<'o> {
  buffer: Vec<u8>,
  table: [TableEntry; SectionId::COUNT],
  /// Declared uncompressed bytes of the compressed sections so far.
  inflated: usize,
  compression: &'o CompressionOptions,
}

impl<'o> SectionWriter<'o> {
  fn new(compression: &'o CompressionOptions) -> Self {
    let data_start = align_up(
      SNAPSHOT_HEADER_SIZE + SectionId::COUNT * SECTION_ENTRY_SIZE,
      SECTION_ALIGNMENT,
    );
    Self {
      buffer: vec![0; data_start],
      table: [TableEntry::default(); SectionId::COUNT],
      inflated: 0,
      compression,
    }
  }

  /// Appends section `id`, compressed when that is smaller and keeps every
  /// compressed section within what readers inflate (`inflation_budget` of
  /// the snapshot size, which only grows from here). Empty sections stay
  /// absent from the table.
  fn add(&mut self, id: SectionId, data: Vec<u8>) {
    if data.is_empty() {
      return;
    }
    let uncompressed = data.len();
    // Vector stores are read in place, so they stay uncompressed.
    let compressible = !matches!(id, SectionId::VectorStoreIndex | SectionId::VectorStoreData);
    let compressed = compressible
      .then(|| try_compress(&data, self.compression))
      .flatten()
      .filter(|compressed| {
        // The snapshot if this were its last section; it only grows.
        let snapshot_len = align_up(self.buffer.len() + compressed.len(), SECTION_ALIGNMENT) + 4;
        self.inflated.saturating_add(uncompressed) <= inflation_budget(snapshot_len)
      });
    let (bytes, compression) = match compressed {
      Some(compressed) => {
        self.inflated += uncompressed;
        drop(data);
        (compressed, self.compression.compression_type)
      }
      None => (data, CompressionType::None),
    };

    let offset = self.buffer.len();
    self.buffer.extend_from_slice(&bytes);
    self
      .buffer
      .resize(align_up(self.buffer.len(), SECTION_ALIGNMENT), 0);
    self.table[id as usize] = TableEntry {
      offset: offset as u64,
      length: bytes.len() as u64,
      compression: compression as u32,
      uncompressed_size: uncompressed as u64,
    };
  }

  /// Writes the header and section table, and appends the footer CRC.
  fn finish(mut self, header: &SnapshotHeaderV1) -> Vec<u8> {
    let fields = [
      header.generation,
      header.created_unix_ns,
      header.num_nodes,
      header.num_edges,
      header.max_node_id,
      header.num_labels,
      header.num_etypes,
      header.num_propkeys,
      header.num_strings,
    ];
    let buffer = &mut self.buffer;
    write_u32(buffer, 0, header.magic);
    write_u32(buffer, 4, header.version);
    write_u32(buffer, 8, header.min_reader_version);
    write_u32(buffer, 12, header.flags.bits());
    for (index, value) in fields.into_iter().enumerate() {
      write_u64(buffer, 16 + index * 8, value);
    }

    for (index, entry) in self.table.iter().enumerate() {
      let offset = SNAPSHOT_HEADER_SIZE + index * SECTION_ENTRY_SIZE;
      write_u64(buffer, offset, entry.offset);
      write_u64(buffer, offset + 8, entry.length);
      write_u32(buffer, offset + 16, entry.compression);
      write_u64(buffer, offset + 20, entry.uncompressed_size);
      // +28: reserved, left zero
    }

    let footer_crc = crc32(buffer);
    push_u32(buffer, footer_crc);
    self.buffer
  }
}

// ============================================================================
// Main snapshot building
// ============================================================================

/// Schema IDs are array-indexed in the snapshot, so the header/table bound is
/// the largest committed ID, not the number of map entries. Rollbacks may
/// leave holes in the ID space.
fn schema_id_bound<Id>(ids: &HashMap<Id, String>) -> usize
where
  Id: Copy + Ord + Into<u64>,
{
  ids
    .keys()
    .copied()
    .max()
    .map(|id| id.into() as usize)
    .unwrap_or(0)
}

/// StringIds of the names `1..=bound` (0 where an ID has no name), with
/// index 0 reserved.
fn intern_name_table<'a, Id>(
  names: &'a HashMap<Id, String>,
  string_table: &mut StringTable<'a>,
) -> Result<Vec<StringId>>
where
  Id: Copy + Ord + Into<u64> + TryFrom<usize> + std::hash::Hash,
{
  let bound = schema_id_bound(names);
  let mut ids: Vec<StringId> = Vec::with_capacity(bound + 1);
  ids.push(0);
  for i in 1..=bound {
    let name = Id::try_from(i).ok().and_then(|id| names.get(&id));
    ids.push(match name {
      Some(name) => string_table.intern(name)?,
      None => 0,
    });
  }
  Ok(ids)
}

/// Interns every string the snapshot stores, in a fixed order: schema names,
/// node keys, then string property values (nodes, then edges, each by key).
/// Returns the label, etype and propkey name tables and each node's key.
fn intern_strings<'a>(
  nodes: &'a [NodeData],
  edges: &'a EdgeColumns,
  schema: [&'a HashMap<u32, String>; 3],
  string_table: &mut StringTable<'a>,
) -> Result<([Vec<StringId>; 3], Vec<StringId>)> {
  let [labels, etypes, propkeys] = schema;
  let name_tables = [
    intern_name_table(labels, string_table)?,
    intern_name_table(etypes, string_table)?,
    intern_name_table(propkeys, string_table)?,
  ];
  let node_key_strings = nodes
    .iter()
    .map(|node| match node.key.as_deref() {
      Some(key) => string_table.intern(key),
      None => Ok(0),
    })
    .collect::<Result<Vec<_>>>()?;

  let mut strings: Vec<(PropKeyId, &str)> = Vec::new();
  for node in nodes {
    strings.clear();
    strings.extend(node.props.iter().filter_map(|(key, value)| match value {
      PropValue::String(s) => Some((*key, s.as_str())),
      _ => None,
    }));
    strings.sort_unstable_by_key(|(key, _)| *key);
    for &(_, s) in &strings {
      string_table.intern(s)?;
    }
  }
  // Each edge's properties are sorted by key already.
  for (_, value) in &edges.props {
    if let PropValue::String(s) = value {
      string_table.intern(s)?;
    }
  }
  Ok((name_tables, node_key_strings))
}

/// Node label offsets and sorted, deduplicated label IDs, as section data.
fn node_label_sections(nodes: &[NodeData]) -> Result<(Vec<u8>, Vec<u8>)> {
  let mut offsets = Vec::with_capacity((nodes.len() + 1) * 4);
  let mut label_ids = Vec::new();
  let mut count = 0usize;
  push_u32(&mut offsets, 0);
  let mut labels = Vec::new();
  for node in nodes {
    labels.clear();
    labels.extend_from_slice(&node.labels);
    labels.sort_unstable();
    labels.dedup();
    for &label in &labels {
      push_u32(&mut label_ids, label);
    }
    count += labels.len();
    push_u32(&mut offsets, checked_u32(count, "node label count")?);
  }
  Ok((offsets, label_ids))
}

/// Edge property sections in out-edge order. Duplicate (src, etype, dst)
/// edges share the properties of the last one in the input.
fn edge_prop_sections(
  edges: &EdgeColumns,
  out_csr: &CSRData,
  string_table: &StringTable<'_>,
  vectors: &mut VectorTable,
) -> Result<PropSections> {
  let input_index = out_csr
    .input_index
    .as_deref()
    .expect("out-edge CSR keeps its input indices");
  let num_nodes = out_csr.offsets.len() - 1;
  let mut sections = PropSections::with_items(out_csr.dst.len());
  for src in 0..num_nodes {
    let range = out_csr.offsets[src] as usize..out_csr.offsets[src + 1] as usize;
    let mut i = range.start;
    while i < range.end {
      // The run of duplicates of edge `i` (sorted by input index).
      let mut run_end = i + 1;
      while run_end < range.end
        && out_csr.etype[run_end] == out_csr.etype[i]
        && out_csr.dst[run_end] == out_csr.dst[i]
      {
        run_end += 1;
      }
      let last = input_index[i..run_end]
        .iter()
        .copied()
        .max()
        .expect("non-empty run") as usize;
      for _ in i..run_end {
        sections.push_sorted(
          edges.props(last).iter().map(|(key, value)| (*key, value)),
          string_table,
          vectors,
          "edge property count",
        )?;
      }
      i = run_end;
    }
  }
  Ok(sections)
}

/// Build a snapshot to memory (useful for single-file format embedding)
pub fn build_snapshot_to_memory(input: SnapshotBuildInput) -> Result<Vec<u8>> {
  let SnapshotBuildInput {
    generation,
    nodes,
    edges,
    labels,
    etypes,
    propkeys,
    vector_stores,
    compression,
  } = input;
  build_columnar_snapshot_to_memory(ColumnarBuildInput {
    generation,
    nodes,
    edges: EdgeColumns::from_edge_data(edges),
    labels,
    etypes,
    propkeys,
    vector_stores,
    compression,
  })
}

/// `build_snapshot_to_memory`, from edges as columns. The edges are dropped
/// once their properties are written, before the in-edge CSR is built.
pub(crate) fn build_columnar_snapshot_to_memory(input: ColumnarBuildInput) -> Result<Vec<u8>> {
  let ColumnarBuildInput {
    generation,
    mut nodes,
    edges,
    labels,
    etypes,
    propkeys,
    vector_stores,
    compression,
  } = input;

  // Physical node and edge indices, CSR offsets and key-bucket offsets are
  // u32. Bounding both counts here keeps every such cast below lossless.
  checked_u32(nodes.len(), "node count")?;
  checked_u32(edges.len(), "edge count")?;

  // Sort nodes by NodeID: physical order is ID order.
  nodes.sort_unstable_by_key(|n| n.node_id);
  let num_nodes = nodes.len();
  let num_edges = edges.len();
  let phys_to_node_id: Vec<NodeId> = nodes.iter().map(|n| n.node_id).collect();
  let max_node_id = phys_to_node_id.last().copied().unwrap_or(0);
  let node_index = NodeIndex::build(&phys_to_node_id, max_node_id)?;

  let mut string_table = StringTable::new();
  let ([label_string_ids, etype_string_ids, propkey_string_ids], node_key_strings) =
    intern_strings(
      &nodes,
      &edges,
      [&labels, &etypes, &propkeys],
      &mut string_table,
    )?;
  let num_strings = string_table.len();
  let out_csr = build_out_edges_csr(num_nodes, &edges, &node_index)?;

  let compression = compression.unwrap_or_default();
  let mut out = SectionWriter::new(&compression);

  out.add(SectionId::PhysToNodeId, encode_u64_slice(&phys_to_node_id));
  drop(phys_to_node_id);

  out.add(
    SectionId::StringOffsets,
    encode_u64_slice(&std::mem::take(&mut string_table.offsets)),
  );
  out.add(
    SectionId::StringBytes,
    std::mem::take(&mut string_table.bytes),
  );
  out.add(
    SectionId::LabelStringIds,
    encode_u32_slice(&label_string_ids),
  );
  out.add(
    SectionId::EtypeStringIds,
    encode_u32_slice(&etype_string_ids),
  );
  out.add(
    SectionId::PropkeyStringIds,
    encode_u32_slice(&propkey_string_ids),
  );
  out.add(
    SectionId::NodeKeyString,
    encode_u32_slice(&node_key_strings),
  );

  let (label_offsets, label_ids) = node_label_sections(&nodes)?;
  out.add(SectionId::NodeLabelOffsets, label_offsets);
  out.add(SectionId::NodeLabelIds, label_ids);

  let (key_entries, key_buckets) = build_key_index(&nodes, &node_key_strings);
  drop(node_key_strings);
  out.add(SectionId::KeyEntries, key_entries);
  out.add(SectionId::KeyBuckets, key_buckets);

  // Node, then edge, properties (vector indices follow that order).
  let mut vector_table = VectorTable::new();
  let mut node_props = PropSections::with_items(num_nodes);
  let mut scratch = Vec::new();
  for node in &nodes {
    node_props.push_item(
      Some(&node.props),
      &mut scratch,
      &string_table,
      &mut vector_table,
      "node property count",
    )?;
  }
  drop(scratch);
  let has_properties = node_props.count > 0 || edges.has_props();
  node_props.write(
    &mut out,
    [
      SectionId::NodePropOffsets,
      SectionId::NodePropKeys,
      SectionId::NodePropVals,
    ],
    "node property count",
  )?;
  let mut out_csr = out_csr;
  edge_prop_sections(&edges, &out_csr, &string_table, &mut vector_table)?.write(
    &mut out,
    [
      SectionId::EdgePropOffsets,
      SectionId::EdgePropKeys,
      SectionId::EdgePropVals,
    ],
    "edge property count",
  )?;
  // The nodes' and edges' sections are written: only the CSR is left.
  drop(string_table);
  drop(nodes);
  drop(edges);
  out_csr.input_index = None;

  let has_vectors = !vector_table.is_empty();
  if has_vectors {
    out.add(
      SectionId::VectorOffsets,
      encode_u64_slice(&vector_table.offsets),
    );
    out.add(SectionId::VectorData, vector_table.data);
  }

  let in_csr = build_in_edges_csr(num_nodes, &out_csr);
  let CSRData {
    offsets,
    dst,
    etype,
    ..
  } = out_csr;
  out.add(SectionId::OutOffsets, encode_u32_slice(&offsets));
  out.add(SectionId::OutDst, encode_u32_slice(&dst));
  out.add(SectionId::OutEtype, encode_u32_slice(&etype));
  drop((offsets, dst, etype));
  out.add(SectionId::InOffsets, encode_u32_slice(&in_csr.offsets));
  out.add(SectionId::InSrc, encode_u32_slice(&in_csr.dst));
  out.add(SectionId::InEtype, encode_u32_slice(&in_csr.etype));
  out.add(
    SectionId::InOutIndex,
    encode_u32_slice(in_csr.out_index.as_deref().unwrap_or_default()),
  );
  drop(in_csr);

  let node_id_map = node_index.layout;
  out.add(SectionId::NodeIdToPhys, node_index.map);

  let has_vector_stores = match vector_store_sections(vector_stores.as_ref())? {
    Some((index_data, blob_data)) => {
      out.add(SectionId::VectorStoreIndex, index_data);
      out.add(SectionId::VectorStoreData, blob_data);
      true
    }
    None => false,
  };

  let mut flags =
    SnapshotFlags::HAS_IN_EDGES | SnapshotFlags::HAS_NODE_LABELS | SnapshotFlags::HAS_KEY_BUCKETS;
  if has_properties {
    flags |= SnapshotFlags::HAS_PROPERTIES;
  }
  if has_vectors {
    flags |= SnapshotFlags::HAS_VECTORS;
  }
  if has_vector_stores {
    flags |= SnapshotFlags::HAS_VECTOR_STORES;
  }
  if node_id_map == NodeIdMapLayout::Sparse {
    flags |= SnapshotFlags::SPARSE_NODE_ID_MAP;
  }

  let created_unix_ns = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.as_nanos() as u64)
    .unwrap_or(0);
  Ok(out.finish(&SnapshotHeaderV1 {
    magic: MAGIC_SNAPSHOT,
    version: VERSION_SNAPSHOT,
    min_reader_version: MIN_READER_SNAPSHOT,
    flags,
    generation,
    created_unix_ns,
    num_nodes: num_nodes as u64,
    num_edges: num_edges as u64,
    max_node_id,
    num_labels: schema_id_bound(&labels) as u64,
    num_etypes: schema_id_bound(&etypes) as u64,
    num_propkeys: schema_id_bound(&propkeys) as u64,
    num_strings: num_strings as u64,
  }))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;
  use crate::core::snapshot::reader::SnapshotData;
  use crate::util::compression::{CompressionOptions, CompressionType};
  use crate::util::crc::crc32;
  use crate::vector::store::{create_vector_store, vector_store_insert};
  use crate::vector::types::VectorStoreConfig;
  use std::io::Write;
  use tempfile::NamedTempFile;

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
      vector_stores: None,
      compression: None,
    }
  }

  #[test]
  fn test_build_snapshot_to_memory() {
    let input = create_test_input();
    let buffer = build_snapshot_to_memory(input).expect("expected value");

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
    let computed_crc = crc32(&buffer[..crc_offset]);
    assert_eq!(stored_crc, computed_crc);
  }

  #[test]
  fn test_snapshot_round_trip_includes_vector_properties() {
    let input = create_test_input();
    let buffer = build_snapshot_to_memory(input).expect("expected value");

    let mut tmp = NamedTempFile::new().expect("expected value");
    tmp.write_all(&buffer).expect("expected value");
    tmp.flush().expect("expected value");

    let snapshot =
      crate::core::snapshot::reader::SnapshotData::load(tmp.path()).expect("expected value");

    assert!(snapshot
      .header
      .flags
      .contains(SnapshotFlags::HAS_PROPERTIES));
    assert!(snapshot.header.flags.contains(SnapshotFlags::HAS_VECTORS));

    let phys = snapshot.phys_node(1).expect("expected value");
    let embedding = snapshot.node_prop(phys, 4).expect("expected value");
    match embedding {
      PropValue::VectorF32(v) => {
        assert_eq!(v.len(), 3);
        assert!((v[0] - 0.1).abs() < 1e-6);
        assert!((v[1] - 0.2).abs() < 1e-6);
        assert!((v[2] - 0.3).abs() < 1e-6);
      }
      other => panic!("expected VectorF32, got {other:?}"),
    }
  }

  #[test]
  fn test_vector_store_sections_forced_uncompressed() {
    let mut manifest = create_vector_store(VectorStoreConfig::new(64));
    for node_id in 1..=1024u64 {
      let mut vector = vec![0.0f32; 64];
      vector[(node_id as usize) % 64] = 1.0;
      vector_store_insert(&mut manifest, node_id, &vector).expect("expected value");
    }

    let mut stores = HashMap::new();
    stores.insert(7, manifest);

    let mut propkeys = HashMap::new();
    propkeys.insert(7, "embedding".to_string());

    let buffer = build_snapshot_to_memory(SnapshotBuildInput {
      generation: 1,
      nodes: vec![NodeData {
        node_id: 1,
        key: None,
        labels: vec![],
        props: HashMap::new(),
      }],
      edges: Vec::new(),
      labels: HashMap::new(),
      etypes: HashMap::new(),
      propkeys,
      vector_stores: Some(stores),
      compression: Some(CompressionOptions {
        enabled: true,
        compression_type: CompressionType::Zstd,
        min_size: 1,
        level: 3,
      }),
    })
    .expect("expected value");

    let mut tmp = NamedTempFile::new().expect("expected value");
    tmp.write_all(&buffer).expect("expected value");
    tmp.flush().expect("expected value");

    let snapshot = SnapshotData::load(tmp.path()).expect("expected value");
    assert!(snapshot
      .section_slice(SectionId::VectorStoreIndex)
      .is_some());
    assert!(snapshot.section_slice(SectionId::VectorStoreData).is_some());
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
      vector_stores: None,
      compression: None,
    };

    let buffer = build_snapshot_to_memory(input).expect("expected value");

    // Verify header
    assert_eq!(read_u32(&buffer, 0), MAGIC_SNAPSHOT);

    // Verify counts
    let num_nodes = read_u64(&buffer, 32);
    let num_edges = read_u64(&buffer, 40);
    assert_eq!(num_nodes, 0);
    assert_eq!(num_edges, 0);
  }

  #[test]
  fn test_build_snapshot_missing_nodes_returns_error() {
    let mut etypes = HashMap::new();
    etypes.insert(1, "REL".to_string());

    let input = SnapshotBuildInput {
      generation: 1,
      nodes: vec![],
      edges: vec![EdgeData {
        src: 1,
        etype: 1,
        dst: 2,
        props: HashMap::new(),
      }],
      labels: HashMap::new(),
      etypes,
      propkeys: HashMap::new(),
      vector_stores: None,
      compression: None,
    };

    assert!(build_snapshot_to_memory(input).is_err());
  }

  #[test]
  fn test_string_table() {
    let mut table = StringTable::new();

    // First string (empty) is pre-populated
    assert_eq!(table.len(), 1);

    // Intern new strings
    let id1 = table.intern("hello").expect("intern");
    assert_eq!(id1, 1);

    let id2 = table.intern("world").expect("intern");
    assert_eq!(id2, 2);

    // Interning again returns same ID
    let id1_again = table.intern("hello").expect("intern");
    assert_eq!(id1_again, 1);

    assert_eq!(table.len(), 3);
  }

  #[test]
  fn test_csr_building() {
    let edge = |src, etype, dst| EdgeData {
      src,
      etype,
      dst,
      props: HashMap::new(),
    };
    // Node 1's edges arrive out of (etype, dst) order; node 3 has in-edges of
    // two types, from sources in descending etype order.
    let edges = vec![
      edge(1, 2, 3),
      edge(1, 1, 3),
      edge(1, 1, 2),
      edge(2, 2, 1),
      edge(2, 1, 3),
      edge(1, 1, 2),
    ];
    let index = NodeIndex::build(&[1, 2, 3], 3).expect("node index");

    let out_csr =
      build_out_edges_csr(3, &EdgeColumns::from_edge_data(edges), &index).expect("out csr");
    assert_eq!(out_csr.offsets, vec![0, 4, 6, 6]);
    assert_eq!(out_csr.etype, vec![1, 1, 1, 2, 1, 2]);
    assert_eq!(out_csr.dst, vec![1, 1, 2, 2, 2, 0]);

    let in_csr = build_in_edges_csr(3, &out_csr);
    assert_eq!(in_csr.offsets, vec![0, 1, 3, 6]);
    // (etype, src, out index) per destination; duplicates stay apart.
    assert_eq!(in_csr.etype, vec![2, 1, 1, 1, 1, 2]);
    assert_eq!(in_csr.dst, vec![1, 0, 0, 0, 1, 0]);
    assert_eq!(in_csr.out_index, Some(vec![5, 0, 1, 2, 4, 3]));
  }

  /// Edge properties land with their edges in out-edge (CSR) order, edges
  /// given in any order; duplicate edges share the properties of the last one
  /// in the input.
  #[test]
  fn test_edge_props_follow_their_edges() {
    let edge = |src, etype, dst, since: Option<i64>| EdgeData {
      src,
      etype,
      dst,
      props: since
        .map(|since| HashMap::from([(1, PropValue::I64(since))]))
        .unwrap_or_default(),
    };
    let edges = vec![
      edge(2, 1, 1, Some(21)),
      edge(1, 2, 3, Some(123)),
      edge(1, 1, 3, None),
      edge(1, 1, 2, Some(112)),
      edge(1, 1, 2, Some(1120)),
      edge(3, 1, 1, Some(31)),
    ];
    let buffer = build_snapshot_to_memory(SnapshotBuildInput {
      generation: 1,
      nodes: bare_nodes(&[1, 2, 3]),
      edges,
      labels: HashMap::new(),
      etypes: HashMap::from([(1, "a".to_string()), (2, "b".to_string())]),
      propkeys: HashMap::from([(1, "since".to_string())]),
      vector_stores: None,
      compression: None,
    })
    .expect("build");
    let mut tmp = NamedTempFile::new().expect("temp file");
    std::io::Write::write_all(&mut tmp, &buffer).expect("write snapshot");
    let snapshot = SnapshotData::load(tmp.path()).expect("load snapshot");
    let props_of = |src: NodeId, etype, dst: NodeId| {
      let src = snapshot.phys_node(src).expect("src");
      let dst = snapshot.phys_node(dst).expect("dst");
      let index = snapshot.find_edge_index(src, etype, dst).expect("edge");
      snapshot
        .edge_props(index)
        .unwrap_or_default()
        .get(&1)
        .cloned()
    };
    assert_eq!(props_of(1, 1, 2), Some(PropValue::I64(1120)));
    assert_eq!(props_of(1, 1, 3), None);
    assert_eq!(props_of(1, 2, 3), Some(PropValue::I64(123)));
    assert_eq!(props_of(2, 1, 1), Some(PropValue::I64(21)));
    assert_eq!(props_of(3, 1, 1), Some(PropValue::I64(31)));
    // Both copies of the duplicate carry the last one's properties.
    let one = snapshot.phys_node(1).expect("node 1");
    let range = snapshot.out_edge_indices(one);
    let duplicate_props: Vec<_> = range
      .zip(snapshot.iter_out_edges(one))
      .filter(|(_, (dst, etype))| *etype == 1 && snapshot.node_id(*dst) == Some(2))
      .map(|(index, _)| {
        snapshot
          .edge_props(index)
          .unwrap_or_default()
          .get(&1)
          .cloned()
      })
      .collect();
    assert_eq!(
      duplicate_props,
      vec![Some(PropValue::I64(1120)), Some(PropValue::I64(1120))]
    );
  }

  #[test]
  fn test_csr_building_rejects_missing_endpoints() {
    let index = NodeIndex::build(&[1, 2], 2).expect("node index");
    let edges = vec![EdgeData {
      src: 1,
      etype: 1,
      dst: 3,
      props: HashMap::new(),
    }];
    assert!(build_out_edges_csr(2, &EdgeColumns::from_edge_data(edges), &index).is_err());
  }

  #[cfg(target_pointer_width = "64")]
  #[test]
  fn test_checked_u32_fails_instead_of_truncating() {
    assert_eq!(
      checked_u32(u32::MAX as usize, "count").expect("fits"),
      u32::MAX
    );
    let error = checked_u32(u32::MAX as usize + 1, "string count").expect_err("too large");
    assert!(
      error.to_string().contains("string count 4294967296"),
      "{error}"
    );
  }

  fn bare_nodes(ids: &[NodeId]) -> Vec<NodeData> {
    ids
      .iter()
      .map(|&node_id| NodeData {
        node_id,
        key: None,
        labels: vec![],
        props: HashMap::new(),
      })
      .collect()
  }

  #[test]
  fn test_node_id_map_is_dense_for_packed_ids_and_sparse_otherwise() {
    use crate::check::check_snapshot;
    use crate::core::snapshot::node_map::{NodeIdMapLayout, SPARSE_ENTRY_SIZE};

    let packed: Vec<NodeId> = (1..=1000).map(|i| i * 2).collect();
    let spread: Vec<NodeId> = (1..=1000).map(|i| i << 20).collect();
    for (ids, layout) in [
      (packed, NodeIdMapLayout::Dense),
      (spread, NodeIdMapLayout::Sparse),
    ] {
      let buffer = build_snapshot_to_memory(SnapshotBuildInput {
        generation: 1,
        nodes: bare_nodes(&ids),
        edges: Vec::new(),
        labels: HashMap::new(),
        etypes: HashMap::new(),
        propkeys: HashMap::new(),
        vector_stores: None,
        compression: None,
      })
      .expect("build snapshot");
      let mut tmp = NamedTempFile::new().expect("temp file");
      tmp.write_all(&buffer).expect("write snapshot");
      let snapshot = SnapshotData::load(tmp.path()).expect("load snapshot");

      assert_eq!(snapshot.node_id_map_layout(), layout);
      assert_eq!(
        snapshot
          .header
          .flags
          .contains(SnapshotFlags::SPARSE_NODE_ID_MAP),
        layout == NodeIdMapLayout::Sparse
      );
      let report = check_snapshot(&snapshot);
      assert!(report.valid, "{layout:?}: {:?}", report.errors);
      for (phys, &node_id) in ids.iter().enumerate() {
        assert_eq!(snapshot.phys_node(node_id), Some(phys as PhysNode));
        assert_eq!(snapshot.phys_node(node_id + 1), None);
      }
      if layout == NodeIdMapLayout::Sparse {
        let map = snapshot
          .section_slice(SectionId::NodeIdToPhys)
          .expect("uncompressed map");
        assert_eq!(map.len(), ids.len() * SPARSE_ENTRY_SIZE);
      }
    }
  }

  #[test]
  fn test_v5_section_table_has_u64_sizes_and_string_offsets() {
    let buffer = build_snapshot_to_memory(create_test_input()).expect("build snapshot");
    assert_eq!(read_u32(&buffer, 4), 5);
    assert_eq!(read_u32(&buffer, 8), 5);

    for id in 0..SectionId::COUNT {
      let entry = SNAPSHOT_HEADER_SIZE + id * SECTION_ENTRY_SIZE;
      let length = read_u64(&buffer, entry + 8);
      let compression = read_u32(&buffer, entry + 16);
      let uncompressed_size = read_u64(&buffer, entry + 20);
      let reserved = read_u32(&buffer, entry + 28);
      assert_eq!(compression, CompressionType::None as u32, "section {id}");
      assert_eq!(uncompressed_size, length, "section {id}");
      assert_eq!(reserved, 0, "section {id}");
    }

    let num_strings = read_u64(&buffer, 80);
    let string_offsets =
      SNAPSHOT_HEADER_SIZE + SectionId::StringOffsets as usize * SECTION_ENTRY_SIZE;
    assert_eq!(read_u64(&buffer, string_offsets + 8), (num_strings + 1) * 8);
  }
}

/// Audit S1/S2: node IDs are user-chosen, so nothing in the snapshot may be
/// sized by the largest ID, and no ID may make the writer panic.
#[cfg(test)]
mod audit_tests {
  use super::*;
  use crate::check::check_snapshot;
  use crate::core::snapshot::reader::SnapshotData;
  use std::io::Write;
  use std::panic::{catch_unwind, AssertUnwindSafe};
  use tempfile::NamedTempFile;

  const KNOWS: ETypeId = 1;
  const NAME: PropKeyId = 1;

  /// Node 1 and node `high`, linked both ways, with keys and a property.
  fn two_node_input(high: NodeId) -> SnapshotBuildInput {
    let node = |node_id: NodeId, key: &str| NodeData {
      node_id,
      key: Some(key.to_string()),
      labels: vec![1],
      props: HashMap::from([(NAME, PropValue::String(key.to_string()))]),
    };
    let edge = |src: NodeId, dst: NodeId| EdgeData {
      src,
      etype: KNOWS,
      dst,
      props: HashMap::new(),
    };
    SnapshotBuildInput {
      generation: 1,
      nodes: vec![node(1, "low"), node(high, "high")],
      edges: vec![edge(1, high), edge(high, 1)],
      labels: HashMap::from([(1, "Thing".to_string())]),
      etypes: HashMap::from([(KNOWS, "KNOWS".to_string())]),
      propkeys: HashMap::from([(NAME, "name".to_string())]),
      vector_stores: None,
      compression: None,
    }
  }

  fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
      (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
      message.clone()
    } else {
      "non-string panic".to_string()
    }
  }

  fn build_without_panic(high: NodeId) -> Result<Vec<u8>> {
    catch_unwind(AssertUnwindSafe(|| {
      build_snapshot_to_memory(two_node_input(high))
    }))
    .unwrap_or_else(|payload| {
      panic!(
        "writer panicked for node id {high}: {}",
        panic_message(&*payload)
      )
    })
  }

  fn assert_two_node_round_trip(buffer: &[u8], high: NodeId) {
    let mut file = NamedTempFile::new().expect("temp file");
    file.write_all(buffer).expect("write snapshot");
    file.flush().expect("flush snapshot");
    let snapshot = SnapshotData::load(file.path())
      .unwrap_or_else(|error| panic!("reader rejected snapshot with node id {high}: {error}"));

    let report = check_snapshot(&snapshot);
    assert!(report.valid, "check_snapshot: {:?}", report.errors);
    assert_eq!(snapshot.num_nodes(), 2);
    assert_eq!(snapshot.max_node_id(), high);
    assert_eq!(snapshot.phys_node(1), Some(0));
    assert_eq!(snapshot.phys_node(high), Some(1));
    assert_eq!(snapshot.node_id(1), Some(high));
    for missing in [0, 2, high - 1] {
      assert_eq!(snapshot.phys_node(missing), None, "phys_node({missing})");
    }
    if let Some(next) = high.checked_add(1) {
      assert_eq!(snapshot.phys_node(next), None, "phys_node({next})");
    }
    assert_eq!(snapshot.lookup_by_key("high"), Some(high));
    assert_eq!(
      snapshot.node_prop(1, NAME),
      Some(PropValue::String("high".to_string()))
    );
    assert_eq!(
      snapshot.iter_out_edges(0).collect::<Vec<_>>(),
      vec![(1, KNOWS)]
    );
    assert_eq!(
      snapshot.iter_out_edges(1).collect::<Vec<_>>(),
      vec![(0, KNOWS)]
    );
    assert_eq!(snapshot.iter_in_edges(0).count(), 1);
  }

  /// Scaled-down S1: max id 2^22 already costs a 16 MiB dense section for two
  /// nodes. IDs >= 2^30 cost >= 4 GiB (S2 truncation), 2^40 costs 4 TiB.
  #[test]
  fn s1_snapshot_size_does_not_scale_with_max_node_id() {
    let high: NodeId = 1 << 22;
    let buffer = build_without_panic(high).expect("build snapshot");
    assert!(
      buffer.len() < 64 * 1024,
      "snapshot of 2 nodes (max id {high}) is {} bytes",
      buffer.len()
    );
    assert_two_node_round_trip(&buffer, high);
  }

  /// `(max_node_id + 1) * 4` overflows before anything is allocated.
  #[test]
  fn s1_u64_max_minus_one_node_id_round_trips() {
    let high = u64::MAX - 1;
    let buffer = build_without_panic(high).expect("build snapshot");
    assert_two_node_round_trip(&buffer, high);
  }

  /// `max_node_id + 1` itself overflows. Rejecting the ID cleanly is fine;
  /// panicking mid-checkpoint is not.
  #[test]
  fn s1_u64_max_node_id_does_not_panic_the_writer() {
    let high = u64::MAX;
    if let Ok(buffer) = build_without_panic(high) {
      assert_two_node_round_trip(&buffer, high);
    }
  }

  /// S2 (format contract): v4 stores section sizes as u32 and NodeIdToPhys
  /// densely, so it cannot represent the graphs above. The writer must emit
  /// the new version, with a min reader version that v4 readers refuse.
  #[test]
  fn s2_writer_emits_snapshot_format_newer_than_v4() {
    let buffer = build_without_panic(2).expect("build snapshot");
    let version = read_u32(&buffer, 4);
    let min_reader = read_u32(&buffer, 8);
    assert!(version > 4, "writer emits snapshot version {version}");
    assert!(
      min_reader > 4,
      "writer emits min_reader_version {min_reader}; v4 readers would accept it"
    );
  }
}
