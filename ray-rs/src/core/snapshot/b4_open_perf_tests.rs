//! open-perf (raydb-b4): loading spreads sections over threads, and every
//! validator first runs a fast pass that only says whether the data is
//! valid. Both must accept and refuse exactly what the single-threaded load
//! with the precise validators does, with the same error.

use super::*;
use crate::core::snapshot::parallel::with_forced_threads;
use crate::core::snapshot::writer::{
  build_snapshot_to_memory, EdgeData, NodeData, SnapshotBuildInput,
};
use crate::util::binary::{align_up, write_u32, write_u64};
use crate::util::compression::{compress, CompressionOptions};
use std::io::Write;
use tempfile::NamedTempFile;

/// Deterministic xorshift64* generator.
struct Rng(u64);

impl Rng {
  fn next(&mut self) -> u64 {
    self.0 ^= self.0 >> 12;
    self.0 ^= self.0 << 25;
    self.0 ^= self.0 >> 27;
    self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
  }

  fn below(&mut self, bound: u64) -> u64 {
    self.next() % bound.max(1)
  }

  fn pick<T: Copy>(&mut self, items: &[T]) -> T {
    items[self.below(items.len() as u64) as usize]
  }
}

fn outcome(result: Result<()>) -> Option<String> {
  result.err().map(|error| error.to_string())
}

/// Asserts `check` gives the same result with and without the fast passes.
fn assert_fast_matches_precise(what: &str, check: impl Fn() -> Result<()>) {
  let fast = outcome(check());
  let precise = outcome(with_precise_checks_only(&check));
  assert_eq!(fast, precise, "{what}");
}

fn u32_bytes(values: &[u32]) -> Vec<u8> {
  values
    .iter()
    .flat_map(|value| value.to_le_bytes())
    .collect()
}

fn u64_bytes(values: &[u64]) -> Vec<u8> {
  values
    .iter()
    .flat_map(|value| value.to_le_bytes())
    .collect()
}

/// Ascending values with steps of 0-3, then one value replaced by a value
/// near `limit` or anything, so arrays are valid about half the time.
fn offsets(rng: &mut Rng, limit: u64) -> Vec<u64> {
  let len = rng.below(24) as usize;
  let mut value = 0;
  let mut values: Vec<u64> = (0..len)
    .map(|_| {
      value += rng.below(4);
      value
    })
    .collect();
  if !values.is_empty() && rng.below(2) == 0 {
    let index = rng.below(len as u64) as usize;
    let any = rng.next();
    values[index] = rng.pick(&[
      0,
      1,
      limit.saturating_sub(1),
      limit,
      limit + 1,
      value,
      value + 1,
      any,
    ]);
  }
  values
}

#[test]
fn validator_fast_passes_agree_with_precise_ones() {
  let mut rng = Rng(0x0BE7_F00D);
  for round in 0..4000 {
    let any = rng.below(1 << 20);
    let limit = rng.pick(&[0u64, 1, 2, 5, 40, 63, any]);
    let values = offsets(&mut rng, limit);
    let last = values.last().copied().unwrap_or(0);
    let end = rng.pick(&[limit, last, last.saturating_sub(1), last + 1]) as usize;
    let narrow: Vec<u32> = values.iter().map(|&value| value as u32).collect();
    let mut bytes32 = u32_bytes(&narrow);
    let mut bytes64 = u64_bytes(&values);
    if rng.below(8) == 0 {
      // A trailing partial element.
      bytes32.push(rng.next() as u8);
      bytes64.extend_from_slice(&[0, 1, 2]);
    }
    let what = |check: &str| format!("round {round}: {check} {values:?} limit {limit} end {end}");

    assert_fast_matches_precise(&what("u32 offsets"), || {
      SnapshotData::validate_u32_offsets(&bytes32, end, "S")
    });
    assert_fast_matches_precise(&what("u64 offsets"), || {
      SnapshotData::validate_u64_offsets(&bytes64, end, "S")
    });
    assert_fast_matches_precise(&what("values below"), || {
      SnapshotData::validate_u32_values_below(&bytes32, limit as usize, "S")
    });
    assert_fast_matches_precise(&what("string IDs"), || {
      SnapshotData::validate_string_id_array(&bytes32, limit as usize, "S")
    });

    // Property values: random tags (some unknown) and payloads around the
    // string and vector counts.
    let vector_count = rng.pick(&[None, Some(0), Some(1), Some(7)]);
    let mut vals = Vec::new();
    for _ in 0..rng.below(12) {
      let mut value = [0u8; PROP_VALUE_DISK_SIZE];
      value[0] = rng.below(8) as u8;
      let any = rng.next();
      let payload = rng.pick(&[0, 1, 6, 7, 8, limit, limit.saturating_sub(1), any]);
      value[8..].copy_from_slice(&payload.to_le_bytes());
      vals.extend_from_slice(&value);
    }
    assert_fast_matches_precise(&what("property values"), || {
      SnapshotData::validate_property_values(&vals, limit as usize, vector_count, "S")
    });
  }
}

// ============================================================================
// Whole loads: small snapshots, every corruption, every way to split them
// ============================================================================

/// A snapshot as header bytes plus `(compression, uncompressed_size,
/// payload)` per section.
#[derive(Clone)]
struct Image {
  header: Vec<u8>,
  sections: Vec<(u32, u64, Vec<u8>)>,
}

impl Image {
  fn decode(bytes: &[u8]) -> Self {
    let sections = (0..SectionId::COUNT)
      .map(|id| {
        let entry = SNAPSHOT_HEADER_SIZE + id * SECTION_ENTRY_SIZE;
        let offset = read_u64(bytes, entry) as usize;
        let length = read_u64(bytes, entry + 8) as usize;
        (
          read_u32(bytes, entry + 16),
          read_u64(bytes, entry + 20),
          bytes[offset..offset + length].to_vec(),
        )
      })
      .collect();
    Self {
      header: bytes[..SNAPSHOT_HEADER_SIZE].to_vec(),
      sections,
    }
  }

  fn encode(&self) -> Vec<u8> {
    let table_end = SNAPSHOT_HEADER_SIZE + SectionId::COUNT * SECTION_ENTRY_SIZE;
    let mut cursor = align_up(table_end, SECTION_ALIGNMENT);
    let mut offsets = Vec::new();
    for (_, _, payload) in &self.sections {
      offsets.push(if payload.is_empty() { 0 } else { cursor });
      if !payload.is_empty() {
        cursor = align_up(cursor + payload.len(), SECTION_ALIGNMENT);
      }
    }
    let mut bytes = vec![0u8; cursor + 4];
    bytes[..SNAPSHOT_HEADER_SIZE].copy_from_slice(&self.header);
    for (id, ((compression, size, payload), &offset)) in
      self.sections.iter().zip(&offsets).enumerate()
    {
      let entry = SNAPSHOT_HEADER_SIZE + id * SECTION_ENTRY_SIZE;
      write_u64(&mut bytes, entry, offset as u64);
      write_u64(&mut bytes, entry + 8, payload.len() as u64);
      write_u32(&mut bytes, entry + 16, *compression);
      write_u64(&mut bytes, entry + 20, *size);
      bytes[offset..offset + payload.len()].copy_from_slice(payload);
    }
    let crc = crc32(&bytes[..cursor]);
    write_u32(&mut bytes, cursor, crc);
    bytes
  }

  /// Section `index`'s bytes, inflated if compressed; None if its frame
  /// was cut short.
  fn inflated(&self, index: usize) -> Option<Vec<u8>> {
    let (compression, size, payload) = &self.sections[index];
    match CompressionType::from_u32(*compression).expect("compression type") {
      CompressionType::None => Some(payload.clone()),
      compression => decompress_with_size(payload, compression, *size as usize).ok(),
    }
  }

  /// Stores section `index`, compressed with zstd or raw.
  fn store(&mut self, index: usize, bytes: Vec<u8>, compressed: bool) {
    self.sections[index] = if compressed {
      let payload = compress(&bytes, CompressionType::Zstd, 3).expect("compress");
      (CompressionType::Zstd as u32, bytes.len() as u64, payload)
    } else {
      (0, bytes.len() as u64, bytes)
    };
  }
}

/// A keyed graph of `nodes` people with labels, string, integer and vector
/// properties, and three edges each, some with properties.
fn graph(nodes: u64, sparse: bool, compressed: bool) -> SnapshotBuildInput {
  let id = |index: u64| if sparse { index * 1_000_003 } else { index };
  let nodes_data = (1..=nodes)
    .map(|index| {
      let mut props = HashMap::from([
        (1, PropValue::String(format!("name-{index}"))),
        (2, PropValue::I64(index as i64)),
      ]);
      if index % 5 == 0 {
        props.insert(3, PropValue::VectorF32(vec![index as f32, 0.5]));
      }
      NodeData {
        node_id: id(index),
        key: (index % 7 != 0).then(|| format!("key-{index}")),
        labels: vec![1 + (index % 2) as LabelId],
        props,
      }
    })
    .collect();
  let mut edges = Vec::new();
  for src in 1..=nodes {
    for k in 0..3 {
      let dst = (src * 31 + k * 17) % nodes + 1;
      let mut props = HashMap::new();
      if (src + k) % 3 == 0 {
        props.insert(4, PropValue::String(format!("since-{src}")));
      }
      edges.push(EdgeData {
        src: id(src),
        etype: 1 + k as ETypeId % 2,
        dst: id(dst),
        props,
      });
    }
  }
  SnapshotBuildInput {
    generation: 1,
    nodes: nodes_data,
    edges,
    labels: HashMap::from([(1, "A".to_string()), (2, "B".to_string())]),
    etypes: HashMap::from([(1, "KNOWS".to_string()), (2, "LIKES".to_string())]),
    propkeys: HashMap::from([
      (1, "name".to_string()),
      (2, "age".to_string()),
      (3, "embedding".to_string()),
      (4, "since".to_string()),
    ]),
    vector_stores: None,
    compression: compressed.then(|| CompressionOptions {
      enabled: true,
      min_size: 16,
      ..Default::default()
    }),
  }
}

fn write_temp(bytes: &[u8]) -> NamedTempFile {
  let mut file = NamedTempFile::new().expect("temp file");
  file.write_all(bytes).expect("write snapshot");
  file.flush().expect("flush snapshot");
  file
}

/// Load outcome on one thread with the precise validators only, on one
/// thread, and split over 2, 3 and 8 threads. All must agree.
fn assert_loads_agree(what: &str, bytes: &[u8]) -> Option<String> {
  let file = write_temp(bytes);
  let load = || outcome(SnapshotData::load(file.path()).map(|_| ()));
  let reference = with_forced_threads(1, || with_precise_checks_only(load));
  assert_eq!(with_forced_threads(1, load), reference, "{what}: 1 thread");
  for threads in [2, 3, 8] {
    assert_eq!(
      with_forced_threads(threads, load),
      reference,
      "{what}: {threads} threads"
    );
  }
  reference
}

#[test]
fn forced_parallel_load_reads_like_sequential() {
  for (sparse, compressed) in [(false, true), (true, true), (false, false), (true, false)] {
    let bytes = build_snapshot_to_memory(graph(200, sparse, compressed)).expect("build");
    let file = write_temp(&bytes);
    let sequential = with_forced_threads(1, || SnapshotData::load(file.path())).expect("loads");
    for threads in [2, 3, 8] {
      let parallel = with_forced_threads(threads, || SnapshotData::load(file.path()))
        .expect("loads on several threads");
      for phys in 0..200 {
        let node_id = sequential.node_id(phys);
        assert_eq!(parallel.node_id(phys), node_id);
        assert_eq!(parallel.phys_node(node_id.expect("node")), Some(phys));
        assert_eq!(parallel.node_key(phys), sequential.node_key(phys));
        assert_eq!(parallel.node_props(phys), sequential.node_props(phys));
        assert_eq!(parallel.node_labels(phys), sequential.node_labels(phys));
        assert_eq!(
          parallel.iter_in_edges(phys).collect::<Vec<_>>(),
          sequential.iter_in_edges(phys).collect::<Vec<_>>()
        );
        if let Some(key) = sequential.node_key(phys) {
          assert_eq!(parallel.lookup_by_key(&key), node_id);
        }
      }
      for edge in 0..600 {
        assert_eq!(parallel.edge_props(edge), sequential.edge_props(edge));
      }
    }
  }
}

/// Two physical nodes swapped in both ID maps: the maps still agree, only
/// the order the seeks rely on breaks. Random corruptions rarely keep the
/// maps agreeing, so this is spelled out for both layouts.
#[test]
fn agreeing_node_id_maps_out_of_order_refused_alike() {
  for sparse in [false, true] {
    let mut image =
      Image::decode(&build_snapshot_to_memory(graph(64, sparse, false)).expect("build"));
    let (a, b) = (20usize, 21usize);
    let phys_to_node = &mut image.sections[SectionId::PhysToNodeId as usize].2;
    let (id_a, id_b) = (read_u64_at(phys_to_node, a), read_u64_at(phys_to_node, b));
    write_u64(phys_to_node, a * 8, id_b);
    write_u64(phys_to_node, b * 8, id_a);
    let map = &mut image.sections[SectionId::NodeIdToPhys as usize].2;
    if sparse {
      // (node_id, phys) entries: swap the IDs, keep phys = entry index.
      write_u64(map, a * node_map::SPARSE_ENTRY_SIZE, id_b);
      write_u64(map, b * node_map::SPARSE_ENTRY_SIZE, id_a);
    } else {
      write_u32(map, id_a as usize * node_map::DENSE_ENTRY_SIZE, b as u32);
      write_u32(map, id_b as usize * node_map::DENSE_ENTRY_SIZE, a as u32);
    }
    let error = assert_loads_agree(&format!("sparse {sparse}"), &image.encode());
    assert!(
      error
        .as_deref()
        .is_some_and(|error| error.contains("ascending")),
      "sparse {sparse}: {error:?}"
    );
  }
}

/// Random corruptions (one to three sections at a time, values set to
/// boundaries, bytes flipped, entries swapped, frames cut short): every way
/// to load refuses or accepts alike, with the same error. Covers the fast
/// passes of the node ID map and key entry checks, which only a whole load
/// reaches, and the first-failure order across threads.
#[test]
fn random_corruptions_refused_alike_by_every_load() {
  let mut rng = Rng(0x5EED_0F0B_E7E7);
  let mut refused = 0;
  let mut cases = 0;
  for (sparse, compressed) in [(false, true), (true, true), (false, false)] {
    let base =
      Image::decode(&build_snapshot_to_memory(graph(64, sparse, compressed)).expect("build"));
    let present: Vec<usize> = (0..SectionId::COUNT)
      .filter(|&index| !base.sections[index].2.is_empty())
      .collect();
    let num_nodes = 64u64;
    let num_strings = read_u64(&base.header, 80);
    for round in 0..300 {
      let mut image = base.clone();
      let mut touched = Vec::new();
      for _ in 0..1 + rng.below(3) {
        let index = rng.pick(&present);
        touched.push(index);
        if image.sections[index].0 != 0 && rng.below(10) == 0 {
          let payload = &mut image.sections[index].2;
          payload.truncate(payload.len() / 2);
          continue;
        }
        let Some(mut bytes) = image.inflated(index) else {
          continue;
        };
        match rng.below(4) {
          0 => {
            let at = rng.below(bytes.len() as u64) as usize;
            bytes[at] ^= 1 << rng.below(8);
          }
          1 if bytes.len() >= 4 => {
            let element = rng.below(bytes.len() as u64 / 4) as usize;
            let any = rng.next();
            let value = rng.pick(&[
              0,
              1,
              num_nodes - 1,
              num_nodes,
              num_nodes + 1,
              num_strings - 1,
              num_strings,
              u32::MAX as u64,
              any,
            ]);
            write_u32(&mut bytes, element * 4, value as u32);
          }
          2 if bytes.len() >= 8 => {
            let element = rng.below(bytes.len() as u64 / 8) as usize;
            let any = rng.next() % (num_nodes * 2_000_006);
            let value = rng.pick(&[0, 1, num_nodes, u64::MAX, any]);
            write_u64(&mut bytes, element * 8, value);
          }
          _ if bytes.len() >= 8 => {
            let (a, b) = (
              rng.below(bytes.len() as u64 / 4),
              rng.below(bytes.len() as u64 / 4),
            );
            let (a, b) = (a as usize * 4, b as usize * 4);
            let (x, y) = (read_u32(&bytes, a), read_u32(&bytes, b));
            write_u32(&mut bytes, a, y);
            write_u32(&mut bytes, b, x);
          }
          _ => bytes[0] ^= 0xFF,
        }
        image.store(index, bytes, compressed && rng.below(2) == 0);
      }
      cases += 1;
      let what =
        format!("sparse {sparse}, compressed {compressed}, round {round}, sections {touched:?}");
      if assert_loads_agree(&what, &image.encode()).is_some() {
        refused += 1;
      }
    }
  }
  // Most corruptions must reach a refusal, or the comparison proves little.
  assert!(
    refused * 2 > cases,
    "only {refused} of {cases} corruptions refused"
  );
}
