//! open-perf (raydb-b4): open got 4-5x slower when 9462079 made it inflate
//! and check every snapshot section up front. Speeding that up (sections
//! inflated and checked on several threads, faster checks) must not change
//! what open refuses, or how.
//!
//! These tests load a compressed snapshot big enough to take the
//! multi-threaded path, corrupt one section (or several) at a time, and pin
//! the exact error a load reports. They pass before the change too: the
//! messages are the sequential loader's, and when several checks fail, the
//! one reported is the first in the order the sequential loader runs them
//! (CRC, section sizes, inflation in section order, then the content checks),
//! however the work is split across threads.
//!
//! The perf side is measured by `scripts/open-close-non-vector-gate.sh` and
//! `examples/query_core_bench.rs --sections open`.

use kitedb::constants::SECTION_ALIGNMENT;
use kitedb::core::snapshot::reader::SnapshotData;
use kitedb::core::snapshot::writer::{
  build_snapshot_to_memory, EdgeData, NodeData, SnapshotBuildInput,
};
use kitedb::types::{
  ETypeId, LabelId, NodeId, PropKeyId, PropValue, SectionId, SnapshotFlags, KEY_INDEX_ENTRY_SIZE,
  PROP_VALUE_DISK_SIZE, SECTION_ENTRY_SIZE, SNAPSHOT_HEADER_SIZE,
};
use kitedb::util::binary::{align_up, read_u32, read_u64, write_u32, write_u64};
use kitedb::util::compression::{decompress_with_size, CompressionOptions, CompressionType};
use kitedb::util::crc::crc32;
use std::collections::HashMap;
use std::io::Write;
use std::sync::OnceLock;
use tempfile::NamedTempFile;

// ============================================================================
// Fixture: a compressed snapshot of a few MiB
// ============================================================================

const NODES: u64 = 12_000;
const DEGREE: u64 = 3;
const PERSON: LabelId = 1;
const KNOWS: ETypeId = 1;
const LIKES: ETypeId = 2;
const NAME: PropKeyId = 1;
const AGE: PropKeyId = 2;
const SINCE: PropKeyId = 3;
const EMBEDDING: PropKeyId = 4;
/// Every `VECTOR_EVERY`-th node has an embedding.
const VECTOR_EVERY: u64 = 10;
/// Every `EDGE_PROP_EVERY`-th edge has a property.
const EDGE_PROP_EVERY: u64 = 4;

/// Offsets of `flags` and `num_strings` in the snapshot header.
const FLAGS_OFFSET: usize = 12;
const NUM_STRINGS_OFFSET: usize = 80;

/// Node ID of the `index`-th node (1-based): `index` itself, or spread out
/// far enough that the writer picks the sparse NodeIdToPhys layout.
fn node_id(index: u64, sparse: bool) -> NodeId {
  if sparse {
    index * SPARSE_STRIDE
  } else {
    index
  }
}

const SPARSE_STRIDE: u64 = 1_000_003;

fn input(compression: bool, sparse: bool) -> SnapshotBuildInput {
  let nodes = (1..=NODES)
    .map(|index| {
      let mut props = HashMap::from([
        (NAME, PropValue::String(format!("name-{index}"))),
        (AGE, PropValue::I64(index as i64)),
      ]);
      if index % VECTOR_EVERY == 0 {
        let x = index as f32;
        props.insert(EMBEDDING, PropValue::VectorF32(vec![x, x + 0.5, x + 0.25]));
      }
      NodeData {
        node_id: node_id(index, sparse),
        key: Some(format!("user:{index:08}")),
        labels: vec![PERSON],
        props,
      }
    })
    .collect();
  let mut edges = Vec::new();
  let mut count = 0u64;
  for src in 1..=NODES {
    for k in 0..DEGREE {
      let dst = (src.wrapping_mul(2_654_435_761) + k * 40_503) % NODES + 1;
      let mut props = HashMap::new();
      if count.is_multiple_of(EDGE_PROP_EVERY) {
        props.insert(SINCE, PropValue::String(format!("since-{count}")));
      }
      count += 1;
      edges.push(EdgeData {
        src: node_id(src, sparse),
        etype: if k == 1 { LIKES } else { KNOWS },
        dst: node_id(dst, sparse),
        props,
      });
    }
  }
  SnapshotBuildInput {
    generation: 1,
    nodes,
    edges,
    labels: HashMap::from([(PERSON, "Person".to_string())]),
    etypes: HashMap::from([(KNOWS, "KNOWS".to_string()), (LIKES, "LIKES".to_string())]),
    propkeys: HashMap::from([
      (NAME, "name".to_string()),
      (AGE, "age".to_string()),
      (SINCE, "since".to_string()),
      (EMBEDDING, "embedding".to_string()),
    ]),
    vector_stores: None,
    compression: compression.then(|| CompressionOptions {
      enabled: true,
      ..Default::default()
    }),
  }
}

fn build(sparse: bool) -> Image {
  let image = Image::decode(&build_snapshot_to_memory(input(true, sparse)).expect("build"));
  let compressed = image
    .sections
    .iter()
    .filter(|section| section.compression != 0)
    .count();
  // Enough inflation to spread over threads, in many sections.
  assert!(
    image.declared_inflated_bytes() > 2 << 20 && compressed >= 15,
    "fixture too small: {} inflated bytes in {compressed} compressed sections",
    image.declared_inflated_bytes()
  );
  let flags = SnapshotFlags::from_bits_truncate(read_u32(&image.header, FLAGS_OFFSET));
  assert_eq!(flags.contains(SnapshotFlags::SPARSE_NODE_ID_MAP), sparse);
  image
}

/// The compressed fixture with a dense NodeIdToPhys, built once.
fn base() -> &'static Image {
  static BASE: OnceLock<Image> = OnceLock::new();
  BASE.get_or_init(|| build(false))
}

/// The compressed fixture with a sparse NodeIdToPhys, built once.
fn sparse_base() -> &'static Image {
  static BASE: OnceLock<Image> = OnceLock::new();
  BASE.get_or_init(|| build(true))
}

// ============================================================================
// Snapshot image: decode, mutate, re-encode
// ============================================================================

#[derive(Clone)]
struct Section {
  compression: u32,
  uncompressed_size: u64,
  payload: Vec<u8>,
}

/// A snapshot as header bytes plus one payload per section.
#[derive(Clone)]
struct Image {
  header: Vec<u8>,
  sections: Vec<Section>,
}

impl Image {
  fn decode(bytes: &[u8]) -> Self {
    let sections = (0..SectionId::COUNT)
      .map(|id| {
        let entry = SNAPSHOT_HEADER_SIZE + id * SECTION_ENTRY_SIZE;
        let offset = read_u64(bytes, entry) as usize;
        let length = read_u64(bytes, entry + 8) as usize;
        Section {
          compression: read_u32(bytes, entry + 16),
          uncompressed_size: read_u64(bytes, entry + 20),
          payload: bytes[offset..offset + length].to_vec(),
        }
      })
      .collect();
    Self {
      header: bytes[..SNAPSHOT_HEADER_SIZE].to_vec(),
      sections,
    }
  }

  /// Lays sections out back to back (aligned) and appends the CRC.
  fn encode(&self) -> Vec<u8> {
    let table_end = SNAPSHOT_HEADER_SIZE + SectionId::COUNT * SECTION_ENTRY_SIZE;
    let mut cursor = align_up(table_end, SECTION_ALIGNMENT);
    let mut offsets = Vec::with_capacity(self.sections.len());
    for section in &self.sections {
      if section.payload.is_empty() {
        offsets.push(0);
      } else {
        offsets.push(cursor);
        cursor = align_up(cursor + section.payload.len(), SECTION_ALIGNMENT);
      }
    }
    let mut bytes = vec![0u8; cursor + 4];
    bytes[..SNAPSHOT_HEADER_SIZE].copy_from_slice(&self.header);
    for (id, (section, &offset)) in self.sections.iter().zip(&offsets).enumerate() {
      let entry = SNAPSHOT_HEADER_SIZE + id * SECTION_ENTRY_SIZE;
      write_u64(&mut bytes, entry, offset as u64);
      write_u64(&mut bytes, entry + 8, section.payload.len() as u64);
      write_u32(&mut bytes, entry + 16, section.compression);
      write_u64(&mut bytes, entry + 20, section.uncompressed_size);
      bytes[offset..offset + section.payload.len()].copy_from_slice(&section.payload);
    }
    let crc = crc32(&bytes[..cursor]);
    write_u32(&mut bytes, cursor, crc);
    bytes
  }

  fn declared_inflated_bytes(&self) -> u64 {
    self
      .sections
      .iter()
      .filter(|section| section.compression != 0)
      .map(|section| section.uncompressed_size)
      .sum()
  }

  fn header_u64(&self, offset: usize) -> u64 {
    read_u64(&self.header, offset)
  }

  /// Section `id`'s bytes, inflated if compressed.
  fn bytes(&self, id: SectionId) -> Vec<u8> {
    let section = &self.sections[id as usize];
    match CompressionType::from_u32(section.compression).expect("compression type") {
      CompressionType::None => section.payload.clone(),
      compression => decompress_with_size(
        &section.payload,
        compression,
        section.uncompressed_size as usize,
      )
      .expect("inflate fixture section"),
    }
  }

  /// Stores `bytes` as section `id`, uncompressed.
  fn set_bytes(&mut self, id: SectionId, bytes: Vec<u8>) {
    self.sections[id as usize] = Section {
      compression: 0,
      uncompressed_size: bytes.len() as u64,
      payload: bytes,
    };
  }

  fn u32s(&self, id: SectionId) -> Vec<u32> {
    let bytes = self.bytes(id);
    (0..bytes.len() / 4)
      .map(|i| read_u32(&bytes, i * 4))
      .collect()
  }

  fn set_u32s(&mut self, id: SectionId, values: &[u32]) {
    self.set_bytes(id, values.iter().flat_map(|v| v.to_le_bytes()).collect());
  }

  fn u64s(&self, id: SectionId) -> Vec<u64> {
    let bytes = self.bytes(id);
    (0..bytes.len() / 8)
      .map(|i| read_u64(&bytes, i * 8))
      .collect()
  }

  fn set_u64s(&mut self, id: SectionId, values: &[u64]) {
    self.set_bytes(id, values.iter().flat_map(|v| v.to_le_bytes()).collect());
  }

  /// Applies `f` to u32 array section `id`.
  fn edit_u32s(&mut self, id: SectionId, f: impl FnOnce(&mut Vec<u32>)) {
    let mut values = self.u32s(id);
    f(&mut values);
    self.set_u32s(id, &values);
  }

  /// Applies `f` to section `id`'s bytes.
  fn edit_bytes(&mut self, id: SectionId, f: impl FnOnce(&mut Vec<u8>)) {
    let mut bytes = self.bytes(id);
    f(&mut bytes);
    self.set_bytes(id, bytes);
  }

  /// Cuts compressed section `id`'s frame short, keeping its declared size,
  /// so inflating it fails.
  fn truncate_frame(&mut self, id: SectionId) {
    let section = &mut self.sections[id as usize];
    assert_ne!(section.compression, 0, "{id:?} must be compressed");
    let keep = section.payload.len() / 2;
    section.payload.truncate(keep);
  }

  /// Load error, or None if the snapshot loads.
  fn load_error(&self) -> Option<String> {
    load_error(&self.encode())
  }
}

fn load_error(bytes: &[u8]) -> Option<String> {
  let mut file = NamedTempFile::new().expect("temp file");
  file.write_all(bytes).expect("write snapshot");
  file.flush().expect("flush snapshot");
  SnapshotData::load(file.path())
    .err()
    .map(|error| error.to_string())
}

fn num_strings() -> u64 {
  base().header_u64(NUM_STRINGS_OFFSET)
}

/// Index of the first property value with `tag` in a PropVals section.
fn first_with_tag(vals: &[u8], tag: u8) -> usize {
  (0..vals.len() / PROP_VALUE_DISK_SIZE)
    .find(|&index| vals[index * PROP_VALUE_DISK_SIZE] == tag)
    .expect("a value with the tag")
}

// ============================================================================
// Corruptions
// ============================================================================

fn out_dst_past_node_count(image: &mut Image) {
  image.edit_u32s(SectionId::OutDst, |dst| dst[5] = NODES as u32);
}

fn out_offsets_not_monotonic(image: &mut Image) {
  image.edit_u32s(SectionId::OutOffsets, |offsets| {
    offsets[10] = offsets[11] + 1
  });
}

fn in_src_past_node_count(image: &mut Image) {
  image.edit_u32s(SectionId::InSrc, |src| src[7] = NODES as u32 + 3);
}

fn in_out_index_past_edge_count(image: &mut Image) {
  image.edit_u32s(SectionId::InOutIndex, |index| {
    index[9] = (NODES * DEGREE) as u32
  });
}

fn string_offsets_past_string_bytes(image: &mut Image) {
  let len = image.bytes(SectionId::StringBytes).len() as u64;
  let mut offsets = image.u64s(SectionId::StringOffsets);
  *offsets.last_mut().expect("offsets") = len + 1;
  image.set_u64s(SectionId::StringOffsets, &offsets);
}

fn node_key_string_past_table(image: &mut Image) {
  image.edit_u32s(SectionId::NodeKeyString, |ids| {
    ids[42] = num_strings() as u32
  });
}

fn label_string_id_past_table(image: &mut Image) {
  image.edit_u32s(SectionId::LabelStringIds, |ids| {
    ids[1] = num_strings() as u32
  });
}

fn key_entry_node_missing(image: &mut Image) {
  image.edit_bytes(SectionId::KeyEntries, |entries| {
    write_u64(entries, 100 * KEY_INDEX_ENTRY_SIZE + 16, NODES + 1000);
  });
}

fn key_entry_string_past_table(image: &mut Image) {
  image.edit_bytes(SectionId::KeyEntries, |entries| {
    write_u32(
      entries,
      200 * KEY_INDEX_ENTRY_SIZE + 8,
      num_strings() as u32,
    );
  });
}

fn key_buckets_short_of_entries(image: &mut Image) {
  image.edit_u32s(SectionId::KeyBuckets, |offsets| {
    *offsets.last_mut().expect("offsets") -= 1;
  });
}

fn node_prop_tag_invalid(image: &mut Image) {
  image.edit_bytes(SectionId::NodePropVals, |vals| {
    vals[33 * PROP_VALUE_DISK_SIZE] = 0x7F;
  });
}

fn node_prop_offsets_past_values(image: &mut Image) {
  let count = image.bytes(SectionId::NodePropVals).len() / PROP_VALUE_DISK_SIZE;
  image.edit_u32s(SectionId::NodePropOffsets, |offsets| {
    *offsets.last_mut().expect("offsets") = count as u32 + 1;
  });
}

fn edge_prop_string_past_table(image: &mut Image) {
  image.edit_bytes(SectionId::EdgePropVals, |vals| {
    let index = first_with_tag(vals, 4);
    write_u64(vals, index * PROP_VALUE_DISK_SIZE + 8, num_strings());
  });
}

fn edge_prop_offsets_not_monotonic(image: &mut Image) {
  image.edit_u32s(SectionId::EdgePropOffsets, |offsets| {
    offsets[20] = offsets[21] + 2;
  });
}

fn node_label_offsets_past_ids(image: &mut Image) {
  let count = image.bytes(SectionId::NodeLabelIds).len() / 4;
  image.edit_u32s(SectionId::NodeLabelOffsets, |offsets| {
    *offsets.last_mut().expect("offsets") = count as u32 + 5;
  });
}

fn vector_offsets_past_data(image: &mut Image) {
  let len = image.bytes(SectionId::VectorData).len() as u64;
  let mut offsets = image.u64s(SectionId::VectorOffsets);
  *offsets.last_mut().expect("offsets") = len + 4;
  image.set_u64s(SectionId::VectorOffsets, &offsets);
}

fn phys_to_node_swapped(image: &mut Image) {
  let mut ids = image.u64s(SectionId::PhysToNodeId);
  ids.swap(500, 501);
  image.set_u64s(SectionId::PhysToNodeId, &ids);
}

fn node_id_map_out_of_order(image: &mut Image) {
  // Swap the physical nodes of two IDs in both maps: they stay inverse
  // bijections, only the order breaks.
  phys_to_node_swapped(image);
  image.edit_bytes(SectionId::NodeIdToPhys, |map| {
    let (a, b) = (501usize, 502usize); // node IDs of phys 500 and 501
    write_u32(map, 4 * a, 501);
    write_u32(map, 4 * b, 500);
  });
}

type Corruption = fn(&mut Image);

/// Each corruption alone and the error a load reports for it.
const SINGLE: &[(&str, Corruption, &str)] = &[
  (
    "OutDst",
    out_dst_past_node_count,
    "Invalid snapshot: OutDst section: value 12000 at index 5 is outside 0..12000",
  ),
  (
    "OutOffsets",
    out_offsets_not_monotonic,
    "Invalid snapshot: OutOffsets section: offset 11 is not monotonic: 33 < 34",
  ),
  (
    "InSrc",
    in_src_past_node_count,
    "Invalid snapshot: InSrc section: value 12003 at index 7 is outside 0..12000",
  ),
  (
    "InOutIndex",
    in_out_index_past_edge_count,
    "Invalid snapshot: InOutIndex section: value 36000 at index 9 is outside 0..36000",
  ),
  (
    "StringOffsets",
    string_offsets_past_string_bytes,
    "Invalid snapshot: StringOffsets section: offset 33008 (361154) exceeds end 361153",
  ),
  (
    "NodeKeyString",
    node_key_string_past_table,
    "Invalid snapshot: NodeKeyString section: string ID 33008 at index 42 is outside 0..33008",
  ),
  (
    "LabelStringIds",
    label_string_id_past_table,
    "Invalid snapshot: LabelStringIds section: string ID 33008 at index 1 is outside 0..33008",
  ),
  (
    "KeyEntries node",
    key_entry_node_missing,
    "Invalid snapshot: KeyEntries section: node ID 13000 at entry 100 is not present",
  ),
  (
    "KeyEntries string",
    key_entry_string_past_table,
    "Invalid snapshot: KeyEntries section: string ID 33008 at entry 200 is outside 0..33008",
  ),
  (
    "KeyBuckets",
    key_buckets_short_of_entries,
    "Invalid snapshot: KeyBuckets section: bucket offsets do not cover all key entries",
  ),
  (
    "NodePropVals",
    node_prop_tag_invalid,
    "Invalid snapshot: NodePropVals section: invalid property tag 127 at entry 33",
  ),
  (
    "NodePropOffsets",
    node_prop_offsets_past_values,
    "Invalid snapshot: NodePropOffsets section: offset 12000 (25201) exceeds end 25200",
  ),
  (
    "EdgePropVals",
    edge_prop_string_past_table,
    "Invalid snapshot: EdgePropVals section: string ID 33008 at entry 0 is outside 0..33008",
  ),
  (
    "EdgePropOffsets",
    edge_prop_offsets_not_monotonic,
    "Invalid snapshot: EdgePropOffsets section: offset 21 is not monotonic: 6 < 8",
  ),
  (
    "NodeLabelOffsets",
    node_label_offsets_past_ids,
    "Invalid snapshot: NodeLabelOffsets section: offset 12000 (12005) exceeds end 12000",
  ),
  (
    "VectorOffsets",
    vector_offsets_past_data,
    "Invalid snapshot: VectorOffsets section: offset 1200 (14404) exceeds end 14400",
  ),
  (
    "PhysToNodeId",
    phys_to_node_swapped,
    "Invalid snapshot: PhysToNodeId section: physical node 500 has node ID 502, but NodeIdToPhys maps node ID 501 to it",
  ),
  (
    "node order",
    node_id_map_out_of_order,
    "Invalid snapshot: PhysToNodeId section: node ID 501 is at physical node 501, out of order: physical nodes must be in ascending node ID order (expected physical node 500)",
  ),
];

fn corrupted(corruptions: &[Corruption]) -> Image {
  let mut image = base().clone();
  for corruption in corruptions {
    corruption(&mut image);
  }
  image
}

// ============================================================================
// Tests
// ============================================================================

/// A compressed fixture loads, and reads the same as an uncompressed build
/// of the same graph (every section lands where its accessors look).
fn assert_reads_like_uncompressed(image: &Image, sparse: bool) {
  let compressed = image.encode();
  let plain = build_snapshot_to_memory(input(false, sparse)).expect("build uncompressed");
  let mut files = Vec::new();
  for bytes in [&compressed, &plain] {
    let mut file = NamedTempFile::new().expect("temp file");
    file.write_all(bytes).expect("write");
    file.flush().expect("flush");
    files.push(file);
  }
  let a = SnapshotData::load(files[0].path()).expect("compressed loads");
  let b = SnapshotData::load(files[1].path()).expect("uncompressed loads");
  assert_eq!(a.num_nodes(), NODES);
  for phys in 0..NODES as u32 {
    let node_id = a.node_id(phys).expect("node id");
    assert_eq!(node_id, self::node_id(u64::from(phys) + 1, sparse));
    assert_eq!(Some(node_id), b.node_id(phys));
    assert_eq!(a.phys_node(node_id), Some(phys));
    let key = a.node_key(phys).expect("key");
    assert_eq!(a.lookup_by_key(&key), Some(node_id), "{key}");
    assert_eq!(b.node_key(phys).as_deref(), Some(key.as_str()));
    assert_eq!(a.node_props(phys), b.node_props(phys), "props of {node_id}");
    assert_eq!(a.node_labels(phys), b.node_labels(phys));
    let out: Vec<_> = a.iter_out_edges(phys).collect();
    assert_eq!(out, b.iter_out_edges(phys).collect::<Vec<_>>());
    let ins: Vec<_> = a.iter_in_edges(phys).collect();
    assert_eq!(ins, b.iter_in_edges(phys).collect::<Vec<_>>());
  }
  for edge in 0..(NODES * DEGREE) as usize {
    assert_eq!(a.edge_props(edge), b.edge_props(edge), "edge {edge}");
  }
}

#[test]
fn large_compressed_snapshot_loads_and_reads_like_uncompressed() {
  assert_reads_like_uncompressed(base(), false);
}

#[test]
fn large_sparse_snapshot_loads_and_reads_like_uncompressed() {
  assert_reads_like_uncompressed(sparse_base(), true);
}

/// The sparse NodeIdToPhys layout's node map and key checks.
#[test]
fn sparse_node_map_corruptions_refused_with_the_same_error() {
  let mut mismatches = Vec::new();
  let mut expect = |name: &str, image: Image, expected: &str| {
    let actual = image.load_error();
    if actual.as_deref() != Some(expected) {
      mismatches.push(format!("{name}: expected {expected:?}, got {actual:?}"));
    }
  };

  let mut image = sparse_base().clone();
  phys_to_node_swapped(&mut image);
  expect(
    "PhysToNodeId",
    image,
    "Invalid snapshot: PhysToNodeId section: physical node 500 has node ID 502001506, but NodeIdToPhys maps node ID 501001503 to it",
  );

  // Swap the physical nodes of the 501st and 502nd IDs in both maps.
  let mut image = sparse_base().clone();
  phys_to_node_swapped(&mut image);
  image.edit_bytes(SectionId::NodeIdToPhys, |map| {
    write_u32(map, 500 * 12 + 8, 501);
    write_u32(map, 501 * 12 + 8, 500);
  });
  expect(
    "node order",
    image,
    "Invalid snapshot: PhysToNodeId section: node ID 501001503 is at physical node 501, out of order: physical nodes must be in ascending node ID order (expected physical node 500)",
  );

  // An entry repeating the previous node ID.
  let mut image = sparse_base().clone();
  image.edit_bytes(SectionId::NodeIdToPhys, |map| {
    let previous = read_u64(map, 700 * 12);
    write_u64(map, 701 * 12, previous);
  });
  expect(
    "ascending",
    image,
    "Invalid snapshot: NodeIdToPhys section: node ID 701002103 at entry 701 is not strictly ascending",
  );

  // Two entries swapped in both maps: the maps agree, only the order breaks.
  let mut image = sparse_base().clone();
  phys_to_node_swapped(&mut image);
  image.edit_bytes(SectionId::NodeIdToPhys, |map| {
    let (a, b) = (read_u64(map, 500 * 12), read_u64(map, 501 * 12));
    write_u64(map, 500 * 12, b);
    write_u64(map, 501 * 12, a);
  });
  expect(
    "agreeing maps out of order",
    image,
    "Invalid snapshot: NodeIdToPhys section: node ID 501001503 at entry 501 is not strictly ascending",
  );

  let mut image = sparse_base().clone();
  key_entry_node_missing(&mut image);
  expect(
    "KeyEntries node",
    image,
    "Invalid snapshot: KeyEntries section: node ID 13000 at entry 100 is not present",
  );

  assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

/// Each corruption alone: refused at load with the sequential loader's error.
#[test]
fn each_corruption_refused_with_the_same_error() {
  let mut mismatches = Vec::new();
  for (name, corruption, expected) in SINGLE {
    let actual = corrupted(&[*corruption]).load_error();
    if actual.as_deref() != Some(*expected) {
      mismatches.push(format!("{name}: expected {expected:?}, got {actual:?}"));
    }
  }
  assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

/// A section that fails to inflate is reported by name, before any content
/// check, even one in an earlier section; the lowest failing section wins.
#[test]
fn inflation_failures_reported_first_in_section_order() {
  let mut image = corrupted(&[out_dst_past_node_count, node_prop_tag_invalid]);
  image.truncate_frame(SectionId::EdgePropOffsets);
  let error = image.load_error().expect("refused");
  assert!(
    error.starts_with("Invalid snapshot: EdgePropOffsets section: cannot decompress: "),
    "{error}"
  );

  image.truncate_frame(SectionId::KeyEntries);
  let error = image.load_error().expect("refused");
  assert!(
    error.starts_with("Invalid snapshot: KeyEntries section: cannot decompress: "),
    "{error}"
  );
}

/// Several content checks fail: the first in the sequential loader's order
/// is reported, which is not always section order.
#[test]
fn several_failures_report_the_first_in_load_order() {
  let cases: &[(&[Corruption], &str)] = &[
    // The node ID maps are checked before everything else.
    (
      &[out_dst_past_node_count, phys_to_node_swapped],
      "PhysToNodeId",
    ),
    (
      &[out_offsets_not_monotonic, out_dst_past_node_count],
      "OutOffsets",
    ),
    // KeyEntries before KeyBuckets.
    (
      &[key_buckets_short_of_entries, key_entry_node_missing],
      "KeyEntries node",
    ),
    // VectorOffsets (section 25) before NodePropVals (19).
    (
      &[node_prop_tag_invalid, vector_offsets_past_data],
      "VectorOffsets",
    ),
    // Property values before their offsets.
    (
      &[node_prop_offsets_past_values, node_prop_tag_invalid],
      "NodePropVals",
    ),
    (
      &[edge_prop_offsets_not_monotonic, edge_prop_string_past_table],
      "EdgePropVals",
    ),
    (
      &[node_label_offsets_past_ids, edge_prop_offsets_not_monotonic],
      "EdgePropOffsets",
    ),
    (
      &[node_label_offsets_past_ids, in_src_past_node_count],
      "InSrc",
    ),
  ];
  let mut mismatches = Vec::new();
  for (corruptions, winner) in cases {
    let expected = SINGLE
      .iter()
      .find(|(name, _, _)| name == winner)
      .map(|(_, _, error)| *error)
      .expect("winner listed in SINGLE");
    let actual = corrupted(corruptions).load_error();
    if actual.as_deref() != Some(expected) {
      mismatches.push(format!("{winner}: expected {expected:?}, got {actual:?}"));
    }
  }
  assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

/// A CRC mismatch is reported before anything in the sections.
#[test]
fn crc_mismatch_reported_before_section_errors() {
  let mut image = corrupted(&[out_dst_past_node_count]);
  image.truncate_frame(SectionId::KeyEntries);
  let mut bytes = image.encode();
  let at = bytes.len() / 2;
  bytes[at] ^= 0x5A;
  let error = load_error(&bytes).expect("refused");
  assert!(error.starts_with("CRC mismatch: "), "{error}");
}
