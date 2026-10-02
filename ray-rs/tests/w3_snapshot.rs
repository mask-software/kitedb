//! Wave-3 snapshot and checker findings, reproduced through the public API.
//!
//! S5:  every compressed section is inflated at open before its declared size
//!      is compared with what the header counts allow (decompression bomb).
//! S8:  `check_snapshot` lacks invariants: key bucket membership, key hashes,
//!      KeyEntries vs NodeKeyString, schema ID ranges, UTF-8, in-edge order.
//! S9:  checker bugs: an early return without KeyEntries skips the string
//!      table checks; duplicate (src, etype, dst) edges are false mismatches.
//! S10: string-ID validators accept `id == num_strings` (one past the table).
//! S11: without KeyBuckets, key lookup binary-searches by hash over entries
//!      sorted by (bucket, hash) and misses keys.
//! S13: PhysToNodeId contents are not validated at load, and the writer
//!      turns duplicate node IDs into an orphaned physical node.
//!
//! `s5_writer_*`, `s10_vector_*` and `s14_*` pass today: they guard the S5
//! fix against rejecting writer output, show the S10 vector-index bound is
//! already enforced at load, and pin the v5 property the S14 design for
//! future sections relies on.
//!
//! Every reproduction decodes a writer-built snapshot into an `Image` (header
//! plus 29 sections), mutates it, and re-encodes it with a fresh footer CRC,
//! so the defect under test is the only thing wrong with the file.
//!
//! Memory guard (S5): this binary's global allocator can refuse, on one
//! thread, any single allocation above `GUARD_CAP`. The reader inflates with
//! `Vec::try_reserve`, so a refused allocation surfaces as a load error, and a
//! bomb never inflates more than a few MiB. S5 scenarios run in a child
//! process, so an infallible allocation that gets refused (abort) fails only
//! that test.
//!
//! `baseline_*` tests are `#[ignore]`d perf baselines for S4, S6, S7, S9 and
//! S12. Run them in release:
//!   CARGO_PROFILE_RELEASE_LTO=false CARGO_TARGET_DIR=target/w3bench \
//!     cargo test --release --no-default-features --test w3_snapshot \
//!     -- --ignored --nocapture --test-threads=1 baseline_snapshot_perf

use kitedb::check::check_snapshot;
use kitedb::constants::SECTION_ALIGNMENT;
use kitedb::core::snapshot::reader::SnapshotData;
use kitedb::core::snapshot::writer::{
  build_snapshot_to_memory, EdgeData, NodeData, SnapshotBuildInput,
};
use kitedb::types::{
  CheckResult, ETypeId, LabelId, NodeId, PhysNode, PropKeyId, PropValue, PropValueTag, SectionId,
  SnapshotFlags, KEY_INDEX_ENTRY_SIZE, PROP_VALUE_DISK_SIZE, SECTION_ENTRY_SIZE,
  SNAPSHOT_HEADER_SIZE,
};
use kitedb::util::binary::{
  align_up, read_u32, read_u32_at, read_u64, read_u64_at, write_u32, write_u64,
};
use kitedb::util::compression::{decompress_with_size, CompressionOptions, CompressionType};
use kitedb::util::crc::crc32;
use kitedb::util::hash::xxhash64_string;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::HashMap;
use std::hint::black_box;
use std::io::Write;
use std::process::Command;
use std::time::{Duration, Instant};
use tempfile::NamedTempFile;

// ============================================================================
// Allocation guard and accounting
// ============================================================================

/// Largest single allocation a guarded thread may make.
const GUARD_CAP: usize = 16 << 20;

struct GuardedAlloc;

thread_local! {
  static GUARDED: Cell<bool> = const { Cell::new(false) };
  static TRACKING: Cell<bool> = const { Cell::new(false) };
  static LARGEST_REFUSED: Cell<usize> = const { Cell::new(0) };
  static ALLOCATED: Cell<usize> = const { Cell::new(0) };
  static ALLOC_COUNT: Cell<usize> = const { Cell::new(0) };
  static LIVE: Cell<isize> = const { Cell::new(0) };
  static PEAK_LIVE: Cell<isize> = const { Cell::new(0) };
}

fn flag(key: &'static std::thread::LocalKey<Cell<bool>>) -> bool {
  key.try_with(Cell::get).unwrap_or(false)
}

/// Accounts for growing a block from `old` to `new` bytes. Returns false when
/// the guard refuses the allocation.
fn account(old: usize, new: usize) -> bool {
  if new > GUARD_CAP && new > old && flag(&GUARDED) {
    let _ = LARGEST_REFUSED.try_with(|largest| largest.set(largest.get().max(new)));
    return false;
  }
  if flag(&TRACKING) {
    let _ = ALLOCATED.try_with(|total| total.set(total.get().saturating_add(new)));
    let _ = ALLOC_COUNT.try_with(|count| count.set(count.get() + 1));
    let _ = LIVE.try_with(|live| {
      let value = live.get() + new as isize - old as isize;
      live.set(value);
      let _ = PEAK_LIVE.try_with(|peak| peak.set(peak.get().max(value)));
    });
  }
  true
}

fn account_free(size: usize) {
  if flag(&TRACKING) {
    let _ = LIVE.try_with(|live| live.set(live.get() - size as isize));
  }
}

unsafe impl GlobalAlloc for GuardedAlloc {
  unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
    if !account(0, layout.size()) {
      return std::ptr::null_mut();
    }
    System.alloc(layout)
  }

  unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
    if !account(0, layout.size()) {
      return std::ptr::null_mut();
    }
    System.alloc_zeroed(layout)
  }

  unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
    if !account(layout.size(), new_size) {
      return std::ptr::null_mut();
    }
    System.realloc(ptr, layout, new_size)
  }

  unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
    account_free(layout.size());
    System.dealloc(ptr, layout)
  }
}

#[global_allocator]
static GLOBAL: GuardedAlloc = GuardedAlloc;

#[derive(Debug, Clone, Copy, Default)]
struct AllocStats {
  /// Sum of all allocation and reallocation sizes.
  allocated: usize,
  /// Number of allocations and reallocations.
  count: usize,
  /// Peak of live bytes above the level at entry.
  peak_live: usize,
  /// Largest allocation the guard refused (0 = none).
  largest_refused: usize,
}

/// Runs `f` with this thread's allocations counted and, when `guard` is set,
/// single allocations above `GUARD_CAP` refused.
fn measure<R>(guard: bool, f: impl FnOnce() -> R) -> (R, AllocStats) {
  LARGEST_REFUSED.with(|cell| cell.set(0));
  ALLOCATED.with(|cell| cell.set(0));
  ALLOC_COUNT.with(|cell| cell.set(0));
  LIVE.with(|cell| cell.set(0));
  PEAK_LIVE.with(|cell| cell.set(0));
  TRACKING.with(|cell| cell.set(true));
  GUARDED.with(|cell| cell.set(guard));
  let result = f();
  GUARDED.with(|cell| cell.set(false));
  TRACKING.with(|cell| cell.set(false));
  let stats = AllocStats {
    allocated: ALLOCATED.with(Cell::get),
    count: ALLOC_COUNT.with(Cell::get),
    peak_live: PEAK_LIVE.with(Cell::get).max(0) as usize,
    largest_refused: LARGEST_REFUSED.with(Cell::get),
  };
  (result, stats)
}

// ============================================================================
// Child-process isolation
// ============================================================================

const CHILD_ENV: &str = "KITEDB_W3_SNAPSHOT_CHILD";

fn tail(text: &str) -> String {
  let lines: Vec<&str> = text.lines().collect();
  lines[lines.len().saturating_sub(40)..].join("\n")
}

/// Runs `scenario` in a child copy of this test binary. `test_name` must be
/// the name of the calling `#[test]` function.
fn run_in_child(test_name: &str, scenario: impl FnOnce()) {
  if std::env::var(CHILD_ENV).as_deref() == Ok(test_name) {
    scenario();
    return;
  }
  let output = Command::new(std::env::current_exe().expect("current test binary"))
    .args([test_name, "--exact", "--nocapture", "--test-threads=1"])
    .env(CHILD_ENV, test_name)
    .output()
    .expect("spawn child test process");
  assert!(
    output.status.success(),
    "{test_name} failed in its child process ({})\n--- stdout ---\n{}\n--- stderr ---\n{}",
    output.status,
    tail(&String::from_utf8_lossy(&output.stdout)),
    tail(&String::from_utf8_lossy(&output.stderr)),
  );
}

// ============================================================================
// Snapshot image: decode, mutate, re-encode
// ============================================================================

const FLAGS_OFFSET: usize = 12;
const NUM_NODES_OFFSET: usize = 32;
const MAX_NODE_ID_OFFSET: usize = 48;
const NUM_LABELS_OFFSET: usize = 56;
const NUM_ETYPES_OFFSET: usize = 64;
const NUM_PROPKEYS_OFFSET: usize = 72;
const NUM_STRINGS_OFFSET: usize = 80;

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
  fn build(input: SnapshotBuildInput) -> Self {
    Self::decode(&build_snapshot_to_memory(input).expect("build snapshot"))
  }

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

  /// Lays sections out back to back (64-byte aligned) and appends the CRC.
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

  fn header_u64(&self, offset: usize) -> u64 {
    read_u64(&self.header, offset)
  }

  fn set_header_u64(&mut self, offset: usize, value: u64) {
    write_u64(&mut self.header, offset, value);
  }

  fn flags(&self) -> SnapshotFlags {
    SnapshotFlags::from_bits_truncate(read_u32(&self.header, FLAGS_OFFSET))
  }

  fn set_flags(&mut self, flags: SnapshotFlags) {
    write_u32(&mut self.header, FLAGS_OFFSET, flags.bits());
  }

  fn raw(&self, id: SectionId) -> &[u8] {
    let section = &self.sections[id as usize];
    assert_eq!(section.compression, 0, "{id:?} must be uncompressed");
    &section.payload
  }

  fn set_raw(&mut self, id: SectionId, payload: Vec<u8>) {
    self.sections[id as usize] = Section {
      compression: 0,
      uncompressed_size: payload.len() as u64,
      payload,
    };
  }

  fn u32s(&self, id: SectionId) -> Vec<u32> {
    let bytes = self.raw(id);
    (0..bytes.len() / 4)
      .map(|i| read_u32_at(bytes, i))
      .collect()
  }

  fn set_u32s(&mut self, id: SectionId, values: &[u32]) {
    self.set_raw(id, values.iter().flat_map(|v| v.to_le_bytes()).collect());
  }

  fn u64s(&self, id: SectionId) -> Vec<u64> {
    let bytes = self.raw(id);
    (0..bytes.len() / 8)
      .map(|i| read_u64_at(bytes, i))
      .collect()
  }

  fn set_u64s(&mut self, id: SectionId, values: &[u64]) {
    self.set_raw(id, values.iter().flat_map(|v| v.to_le_bytes()).collect());
  }

  /// Replaces section `id` with a zstd frame that inflates to `declared`
  /// zero bytes, and declares that size.
  fn set_zero_bomb(&mut self, id: SectionId, declared: u64) {
    self.sections[id as usize] = Section {
      compression: CompressionType::Zstd as u32,
      uncompressed_size: declared,
      payload: zstd_zero_frame(declared),
    };
  }

  fn declared_uncompressed_total(&self) -> u64 {
    self.sections.iter().map(|s| s.uncompressed_size).sum()
  }

  fn load(&self) -> Result<Loaded, String> {
    load_bytes(&self.encode())
  }
}

/// A loaded snapshot and the temp file its mmap points into.
struct Loaded {
  _file: NamedTempFile,
  snapshot: SnapshotData,
}

fn write_temp(bytes: &[u8]) -> NamedTempFile {
  let mut file = NamedTempFile::new().expect("temp file");
  file.write_all(bytes).expect("write snapshot");
  file.flush().expect("flush snapshot");
  file
}

fn load_bytes(bytes: &[u8]) -> Result<Loaded, String> {
  let file = write_temp(bytes);
  match SnapshotData::load(file.path()) {
    Ok(snapshot) => Ok(Loaded {
      _file: file,
      snapshot,
    }),
    Err(error) => Err(error.to_string()),
  }
}

/// A zstd frame of `len` zero bytes made of RLE blocks: 4 bytes per 128 KiB,
/// so 1 GiB costs 32 KiB on disk and building it allocates almost nothing.
fn zstd_zero_frame(len: u64) -> Vec<u8> {
  const BLOCK: u64 = 128 * 1024;
  // Magic, then a frame header descriptor without content size, checksum or
  // dictionary, and a window descriptor of 2^(10 + 7) = 128 KiB.
  let mut frame = vec![0x28, 0xB5, 0x2F, 0xFD, 0x00, 7 << 3];
  let mut remaining = len;
  loop {
    let size = remaining.min(BLOCK);
    remaining -= size;
    let last = u32::from(remaining == 0);
    // Block header: last-block bit, block type 1 (RLE), regenerated size.
    let header = ((size as u32) << 3) | (1 << 1) | last;
    frame.extend_from_slice(&header.to_le_bytes()[..3]);
    frame.push(0);
    if remaining == 0 {
      return frame;
    }
  }
}

// ============================================================================
// Fixture graph
// ============================================================================

const KNOWS: ETypeId = 1;
const LIKES: ETypeId = 2;
const PERSON: LabelId = 1;
const NAME: PropKeyId = 1;
const AGE: PropKeyId = 2;
const SINCE: PropKeyId = 3;
const EMBEDDING: PropKeyId = 4;

fn edge(src: NodeId, etype: ETypeId, dst: NodeId) -> EdgeData {
  EdgeData {
    src,
    etype,
    dst,
    props: HashMap::new(),
  }
}

/// Four people (node IDs `ids`, physical order = ID order) with optional
/// keys, a label and properties, and seven edges:
///
/// out (etype, dst phys): p0 [(K,1) (K,2) (L,1)]  p1 [(K,2)]  p2 [(K,0)]  p3 [(K,0) (L,2)]
/// in  (etype, src phys): p0 [(K,2) (K,3)]  p1 [(K,0) (L,0)]  p2 [(K,0) (K,1) (L,3)]  p3 []
fn people(ids: [NodeId; 4], keyed: bool) -> SnapshotBuildInput {
  let nodes = ids
    .iter()
    .enumerate()
    .map(|(index, &node_id)| {
      let mut props = HashMap::from([
        (NAME, PropValue::String(format!("name-{index}"))),
        (AGE, PropValue::I64(20 + index as i64)),
      ]);
      if index == 0 {
        props.insert(EMBEDDING, PropValue::VectorF32(vec![0.25, 0.5, 0.75]));
      }
      NodeData {
        node_id,
        key: keyed.then(|| format!("user:{index}")),
        labels: vec![PERSON],
        props,
      }
    })
    .collect();
  let [a, b, c, d] = ids;
  let mut first = edge(a, KNOWS, b);
  first
    .props
    .insert(SINCE, PropValue::String("2020".to_string()));
  SnapshotBuildInput {
    generation: 1,
    nodes,
    edges: vec![
      first,
      edge(a, KNOWS, c),
      edge(a, LIKES, b),
      edge(b, KNOWS, c),
      edge(c, KNOWS, a),
      edge(d, KNOWS, a),
      edge(d, LIKES, c),
    ],
    labels: HashMap::from([(PERSON, "Person".to_string())]),
    etypes: HashMap::from([(KNOWS, "KNOWS".to_string()), (LIKES, "LIKES".to_string())]),
    propkeys: HashMap::from([
      (NAME, "name".to_string()),
      (AGE, "age".to_string()),
      (SINCE, "since".to_string()),
      (EMBEDDING, "embedding".to_string()),
    ]),
    vector_stores: None,
    compression: None,
  }
}

const DENSE_IDS: [NodeId; 4] = [1, 2, 3, 4];
const SPARSE_IDS: [NodeId; 4] = [1, 1 << 40, 1 << 41, u64::MAX - 1];

fn keyed_image() -> Image {
  Image::build(people(DENSE_IDS, true))
}

/// Out- and in-edge CSR arrays of an uncompressed image.
struct Csr {
  out_offsets: Vec<u32>,
  out_dst: Vec<u32>,
  out_etype: Vec<u32>,
  in_offsets: Vec<u32>,
  in_src: Vec<u32>,
  in_etype: Vec<u32>,
  in_out_index: Vec<u32>,
}

impl Csr {
  fn read(image: &Image) -> Self {
    Self {
      out_offsets: image.u32s(SectionId::OutOffsets),
      out_dst: image.u32s(SectionId::OutDst),
      out_etype: image.u32s(SectionId::OutEtype),
      in_offsets: image.u32s(SectionId::InOffsets),
      in_src: image.u32s(SectionId::InSrc),
      in_etype: image.u32s(SectionId::InEtype),
      in_out_index: image.u32s(SectionId::InOutIndex),
    }
  }

  fn write(&self, image: &mut Image) {
    image.set_u32s(SectionId::OutOffsets, &self.out_offsets);
    image.set_u32s(SectionId::OutDst, &self.out_dst);
    image.set_u32s(SectionId::OutEtype, &self.out_etype);
    image.set_u32s(SectionId::InOffsets, &self.in_offsets);
    image.set_u32s(SectionId::InSrc, &self.in_src);
    image.set_u32s(SectionId::InEtype, &self.in_etype);
    image.set_u32s(SectionId::InOutIndex, &self.in_out_index);
  }

  /// Recomputes the in-edge arrays from the out-edge arrays, sorted by
  /// (etype, src) like the writer, so they stay reciprocal.
  fn rebuild_in_edges(&mut self) {
    let num_nodes = self.out_offsets.len() - 1;
    let mut per_dst: Vec<Vec<(u32, u32, u32)>> = vec![Vec::new(); num_nodes];
    for src in 0..num_nodes {
      for index in self.out_offsets[src]..self.out_offsets[src + 1] {
        let position = index as usize;
        per_dst[self.out_dst[position] as usize].push((
          self.out_etype[position],
          src as u32,
          index,
        ));
      }
    }
    self.in_offsets = vec![0];
    self.in_src.clear();
    self.in_etype.clear();
    self.in_out_index.clear();
    for mut edges in per_dst {
      edges.sort_unstable();
      for (etype, src, index) in edges {
        self.in_src.push(src);
        self.in_etype.push(etype);
        self.in_out_index.push(index);
      }
      self.in_offsets.push(self.in_src.len() as u32);
    }
  }

  fn swap_out_edges(&mut self, a: usize, b: usize) {
    self.out_dst.swap(a, b);
    self.out_etype.swap(a, b);
  }

  fn swap_in_edges(&mut self, a: usize, b: usize) {
    self.in_src.swap(a, b);
    self.in_etype.swap(a, b);
    self.in_out_index.swap(a, b);
  }
}

/// One KeyEntries record.
#[derive(Clone, Copy, Debug)]
struct KeyRecord {
  hash: u64,
  string_id: u32,
  node_id: NodeId,
}

fn key_records(image: &Image) -> Vec<KeyRecord> {
  let bytes = image.raw(SectionId::KeyEntries);
  (0..bytes.len() / KEY_INDEX_ENTRY_SIZE)
    .map(|index| {
      let offset = index * KEY_INDEX_ENTRY_SIZE;
      KeyRecord {
        hash: read_u64(bytes, offset),
        string_id: read_u32(bytes, offset + 8),
        node_id: read_u64(bytes, offset + 16),
      }
    })
    .collect()
}

fn set_key_records(image: &mut Image, records: &[KeyRecord]) {
  let mut bytes = vec![0u8; records.len() * KEY_INDEX_ENTRY_SIZE];
  for (index, record) in records.iter().enumerate() {
    let offset = index * KEY_INDEX_ENTRY_SIZE;
    write_u64(&mut bytes, offset, record.hash);
    write_u32(&mut bytes, offset + 8, record.string_id);
    write_u64(&mut bytes, offset + 16, record.node_id);
  }
  image.set_raw(SectionId::KeyEntries, bytes);
}

/// Index of the first `PROP_VALUE_DISK_SIZE` record in `vals` with `tag`.
fn first_prop_with_tag(vals: &[u8], tag: PropValueTag) -> usize {
  (0..vals.len() / PROP_VALUE_DISK_SIZE)
    .find(|&index| vals[index * PROP_VALUE_DISK_SIZE] == tag as u8)
    .unwrap_or_else(|| panic!("no {tag:?} property value"))
}

// ============================================================================
// Checker helpers
// ============================================================================

fn check(image: &Image) -> Result<CheckResult, String> {
  image.load().map(|loaded| check_snapshot(&loaded.snapshot))
}

/// Ok when the corruption is detected: by the reader at load, or by
/// `check_snapshot` on the loaded snapshot.
fn detected(image: &Image) -> Result<(), String> {
  match check(image) {
    Err(_) => Ok(()),
    Ok(report) if !report.valid => Ok(()),
    Ok(report) => Err(format!(
      "load accepted it and check_snapshot reported valid (warnings: {:?})",
      report.warnings
    )),
  }
}

fn assert_detected(what: &str, image: &Image) {
  if let Err(message) = detected(image) {
    panic!("{what}: {message}");
  }
}

// ============================================================================
// Helper self-tests (pass today; they keep the fixtures honest)
// ============================================================================

#[test]
fn helper_zero_frame_inflates_to_zeros() {
  for len in [1u64, 131_071, 131_072, 300_000] {
    let frame = zstd_zero_frame(len);
    let inflated = decompress_with_size(&frame, CompressionType::Zstd, len as usize)
      .unwrap_or_else(|error| panic!("zero frame of {len} bytes: {error}"));
    assert!(inflated.iter().all(|&byte| byte == 0), "len {len}");
  }
}

/// Unmodified fixtures load and pass the checker after a decode/encode round
/// trip, so every failure below is caused by its mutation alone.
#[test]
fn helper_image_round_trip_loads_checker_clean() {
  for (ids, keyed) in [(DENSE_IDS, true), (DENSE_IDS, false), (SPARSE_IDS, true)] {
    let image = Image::build(people(ids, keyed));
    let report = check(&image).expect("re-encoded snapshot loads");
    assert!(report.valid, "{ids:?} keyed={keyed}: {:?}", report.errors);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
  }

  let compressed = Image::build(SnapshotBuildInput {
    compression: Some(CompressionOptions {
      enabled: true,
      min_size: 1,
      ..Default::default()
    }),
    ..people(DENSE_IDS, true)
  });
  let report = check(&compressed).expect("compressed snapshot loads");
  assert!(report.valid, "compressed: {:?}", report.errors);
}

// ============================================================================
// S5: decompression bomb
// ============================================================================

const ONE_GIB: u64 = 1 << 30;

/// Loads `image` with single allocations above `GUARD_CAP` refused. Ok when
/// the reader rejects it without having tried to inflate the bomb.
fn rejected_before_inflating(image: &Image) -> Result<(), String> {
  let bytes = image.encode();
  let file = write_temp(&bytes);
  let (result, stats) = measure(true, || SnapshotData::load(file.path()).map(|_| ()));
  let declared = image.declared_uncompressed_total();
  let mut problems = Vec::new();
  if stats.largest_refused > 0 {
    problems.push(format!(
      "tried a {} byte allocation (refused by the test guard) while inflating",
      stats.largest_refused
    ));
  }
  if stats.allocated > GUARD_CAP {
    problems.push(format!("allocated {} bytes in total", stats.allocated));
  }
  if result.is_ok() {
    problems.push("loaded successfully".to_string());
  }
  if problems.is_empty() {
    Ok(())
  } else {
    Err(format!(
      "{}-byte file declaring {declared} uncompressed bytes: {} (load result: {:?})",
      bytes.len(),
      problems.join("; "),
      result.map_err(|error| error.to_string())
    ))
  }
}

/// Each section's declared size contradicts what the header counts (or the
/// sections they depend on) allow. A fixed reader rejects the file from the
/// section table alone; today it inflates first and compares afterwards.
#[test]
fn s5_declared_section_sizes_checked_before_inflating() {
  run_in_child("s5_declared_section_sizes_checked_before_inflating", || {
    let base = keyed_image();
    let mut failures = Vec::new();
    for id in [
      SectionId::PhysToNodeId,
      SectionId::OutEtype,
      SectionId::LabelStringIds,
      SectionId::NodeKeyString,
      SectionId::StringBytes,
      SectionId::KeyEntries,
      SectionId::NodePropVals,
      SectionId::EdgePropKeys,
      SectionId::NodeLabelIds,
    ] {
      let mut image = base.clone();
      image.set_zero_bomb(id, ONE_GIB);
      if let Err(message) = rejected_before_inflating(&image) {
        failures.push(format!("{id:?}: {message}"));
      }
    }
    assert!(
      failures.is_empty(),
      "S5: bombs inflated before their size was checked:\n{}",
      failures.join("\n")
    );
  });
}

/// Every declared size matches the header counts (2^26 nodes, no edges), so
/// only a cap on total inflation relative to file size can stop it: a ~57 KiB
/// file declares ~1.75 GiB of zeros.
#[test]
fn s5_total_inflation_capped_relative_to_file_size() {
  run_in_child("s5_total_inflation_capped_relative_to_file_size", || {
    const NODES: u64 = 1 << 26;
    let mut image = Image::build(SnapshotBuildInput {
      generation: 1,
      nodes: Vec::new(),
      edges: Vec::new(),
      labels: HashMap::new(),
      etypes: HashMap::new(),
      propkeys: HashMap::new(),
      vector_stores: None,
      compression: None,
    });
    image.set_header_u64(NUM_NODES_OFFSET, NODES);
    image.set_flags(image.flags() | SnapshotFlags::SPARSE_NODE_ID_MAP);
    image.set_raw(SectionId::NodeIdToPhys, Vec::new());
    image.set_zero_bomb(SectionId::PhysToNodeId, 8 * NODES);
    for id in [
      SectionId::OutOffsets,
      SectionId::InOffsets,
      SectionId::NodeKeyString,
      SectionId::NodePropOffsets,
      SectionId::NodeLabelOffsets,
    ] {
      let len = if id == SectionId::NodeKeyString {
        4 * NODES
      } else {
        4 * (NODES + 1)
      };
      image.set_zero_bomb(id, len);
    }
    if let Err(message) = rejected_before_inflating(&image) {
      panic!("S5: header-consistent bomb inflated: {message}");
    }
  });
}

/// A legitimate but extremely compressible section (one 65 MiB run of a
/// single byte, ~32000:1 under zstd) must still round-trip: the writer may
/// not emit what the S5 inflation cap refuses.
#[test]
fn s5_writer_output_stays_within_inflation_budget() {
  const LEN: usize = 65 << 20;
  let mut input = people(DENSE_IDS, false);
  input.nodes[0]
    .props
    .insert(NAME, PropValue::String("a".repeat(LEN)));
  input.compression = Some(CompressionOptions {
    enabled: true,
    ..Default::default()
  });
  let bytes = build_snapshot_to_memory(input).expect("build snapshot");
  let loaded = load_bytes(&bytes).expect("writer output loads");
  let phys = loaded.snapshot.phys_node(DENSE_IDS[0]).expect("node");
  match loaded.snapshot.node_prop(phys, NAME) {
    Some(PropValue::String(value)) => assert_eq!(value.len(), LEN),
    other => panic!("name of node 0: {other:?}"),
  }
}

// ============================================================================
// S8: checker invariants
// ============================================================================

/// Present today (check_out_edge_sorting); guards the (etype, dst) order that
/// has_edge's binary search relies on.
#[test]
fn s8_checker_reports_unsorted_out_edges() {
  let mut image = keyed_image();
  let mut csr = Csr::read(&image);
  // p0: [(K,1) (K,2) (L,1)] -> [(K,2) (K,1) (L,1)], in-edges kept reciprocal.
  csr.swap_out_edges(0, 1);
  csr.rebuild_in_edges();
  csr.write(&mut image);
  assert_detected("out-edges of p0 not sorted by (etype, dst)", &image);
}

/// Present today (check_edge_reciprocity).
#[test]
fn s8_checker_reports_broken_in_out_reciprocity() {
  let mut image = keyed_image();
  let mut csr = Csr::read(&image);
  // p0's first in-edge claims to come from p1 instead of p2.
  csr.in_src[0] = 1;
  csr.write(&mut image);
  assert_detected("in-edge without a matching out-edge", &image);
}

/// Present today (check_mapping_bijection); S13 asks load to reject it too.
#[test]
fn s8_checker_reports_broken_node_mapping() {
  let mut image = keyed_image();
  let mut phys_to_node = image.u64s(SectionId::PhysToNodeId);
  phys_to_node.swap(0, 1);
  image.set_u64s(SectionId::PhysToNodeId, &phys_to_node);
  assert_detected("PhysToNodeId disagrees with NodeIdToPhys", &image);
}

/// Present today (check_key_index_ordering).
#[test]
fn s8_checker_reports_unsorted_key_index() {
  let mut image = keyed_image();
  let mut records = key_records(&image);
  let last = records.len() - 1;
  records.swap(0, last);
  set_key_records(&mut image, &records);
  assert_detected("KeyEntries out of (bucket, hash) order", &image);
}

/// KeyBuckets offsets must place every entry in the bucket its hash selects;
/// lookup_by_key reads only that range. Today only the (bucket, hash) order of
/// the entries is checked.
#[test]
fn s8_checker_reports_key_bucket_offsets_not_matching_entries() {
  let mut image = keyed_image();
  let records = key_records(&image);
  let mut buckets = image.u32s(SectionId::KeyBuckets);
  let num_buckets = (buckets.len() - 1) as u64;
  let (index, record) = records
    .iter()
    .enumerate()
    .find(|(_, record)| ((record.hash % num_buckets) as usize) < buckets.len() - 2)
    .expect("an entry outside the last bucket");
  let bucket = (record.hash % num_buckets) as usize;
  // Empty its bucket: the entry now sits in bucket + 1's range.
  buckets[bucket + 1] = buckets[bucket];
  image.set_u32s(SectionId::KeyBuckets, &buckets);

  let loaded = image.load().expect("corrupt buckets load today");
  let key = format!("user:{}", record.node_id - 1);
  assert_eq!(
    loaded.snapshot.lookup_by_key(&key),
    None,
    "precondition: entry {index} ({key}) is unreachable"
  );
  assert_detected(&format!("entry {index} ({key}) outside its bucket"), &image);
}

/// Every KeyEntries hash must equal xxhash64 of its key string.
#[test]
fn s8_checker_reports_key_hash_not_matching_string() {
  let mut image = keyed_image();
  let mut records = key_records(&image);
  let buckets = image.u32s(SectionId::KeyBuckets);
  let num_buckets = (buckets.len() - 1) as u64;
  // An entry alone in its bucket, so a same-bucket hash keeps the order.
  let index = (0..records.len())
    .find(|&index| {
      let bucket = (records[index].hash % num_buckets) as usize;
      buckets[bucket + 1] - buckets[bucket] == 1
    })
    .expect("an entry alone in its bucket");
  let hash = records[index].hash;
  records[index].hash = if hash >= num_buckets {
    hash - num_buckets
  } else {
    hash + num_buckets
  };
  set_key_records(&mut image, &records);
  assert_detected(
    &format!("entry {index} hash differs from xxhash64 of its key"),
    &image,
  );
}

/// KeyEntries and NodeKeyString must describe the same (key, node) pairs.
#[test]
fn s8_checker_reports_key_entries_disagreeing_with_node_keys() {
  let mut image = keyed_image();
  let mut records = key_records(&image);
  let (first, second) = (records[0].node_id, records[1].node_id);
  records[0].node_id = second;
  records[1].node_id = first;
  set_key_records(&mut image, &records);

  let loaded = image.load().expect("swapped key entries load today");
  let key = format!("user:{}", first - 1);
  assert_eq!(
    loaded.snapshot.lookup_by_key(&key),
    Some(second),
    "precondition: {key} now resolves to the wrong node"
  );
  assert_detected("KeyEntries node IDs swapped vs NodeKeyString", &image);
}

/// Ok when the snapshot loads and `check_snapshot` warns about `section`.
fn warned(image: &Image, section: &str) -> Result<(), String> {
  match check(image) {
    Err(error) => Err(format!("load rejected it: {error}")),
    Ok(report) if report.warnings.iter().any(|w| w.contains(section)) => Ok(()),
    Ok(report) => Err(format!(
      "no warning names {section} (valid={}, errors: {:?}, warnings: {:?})",
      report.valid, report.errors, report.warnings
    )),
  }
}

/// Labels, edge types and property keys stored per node/edge outside the
/// header's schema bounds (num_labels, num_etypes, num_propkeys) are reported,
/// as warnings: the low-level API accepts IDs that were never defined, so a
/// valid database can hold them.
#[test]
fn s8_checker_reports_schema_ids_out_of_range() {
  let base = keyed_image();
  let num_labels = base.header_u64(NUM_LABELS_OFFSET) as u32;
  let num_etypes = base.header_u64(NUM_ETYPES_OFFSET) as u32;
  let num_propkeys = base.header_u64(NUM_PROPKEYS_OFFSET) as u32;
  let mut failures = Vec::new();

  let mut image = base.clone();
  let mut labels = image.u32s(SectionId::NodeLabelIds);
  labels[0] = num_labels + 1;
  image.set_u32s(SectionId::NodeLabelIds, &labels);
  if let Err(message) = warned(&image, "NodeLabelIds") {
    failures.push(format!("NodeLabelIds[0] = {}: {message}", num_labels + 1));
  }

  // p3's last out-edge (L,2) becomes etype num_etypes + 1; still sorted.
  let mut image = base.clone();
  let mut csr = Csr::read(&image);
  let last = csr.out_etype.len() - 1;
  csr.out_etype[last] = num_etypes + 1;
  csr.rebuild_in_edges();
  csr.write(&mut image);
  if let Err(message) = warned(&image, "OutEtype") {
    failures.push(format!("OutEtype[{last}] = {}: {message}", num_etypes + 1));
  }

  // p0's last property key (embedding) becomes num_propkeys + 1; still sorted.
  let mut image = base.clone();
  let offsets = image.u32s(SectionId::NodePropOffsets);
  let mut keys = image.u32s(SectionId::NodePropKeys);
  let last = offsets[1] as usize - 1;
  keys[last] = num_propkeys + 1;
  image.set_u32s(SectionId::NodePropKeys, &keys);
  if let Err(message) = warned(&image, "NodePropKeys") {
    failures.push(format!(
      "NodePropKeys[{last}] = {}: {message}",
      num_propkeys + 1
    ));
  }

  let mut image = base.clone();
  let mut keys = image.u32s(SectionId::EdgePropKeys);
  keys[0] = num_propkeys + 1;
  image.set_u32s(SectionId::EdgePropKeys, &keys);
  if let Err(message) = warned(&image, "EdgePropKeys") {
    failures.push(format!("EdgePropKeys[0] = {}: {message}", num_propkeys + 1));
  }

  assert!(
    failures.is_empty(),
    "S8: schema IDs out of range went unreported:\n{}",
    failures.join("\n")
  );
}

/// Overwrites the first byte of the label name "Person" with 0xFF.
fn corrupt_label_name_utf8(image: &mut Image) {
  let string_id = image.u32s(SectionId::LabelStringIds)[PERSON as usize] as usize;
  let start = image.u64s(SectionId::StringOffsets)[string_id] as usize;
  let mut bytes = image.raw(SectionId::StringBytes).to_vec();
  assert_eq!(&bytes[start..start + 6], b"Person");
  bytes[start] = 0xFF;
  image.set_raw(SectionId::StringBytes, bytes);
}

/// Strings must be valid UTF-8; today a bad label name loads, and
/// `label_name` silently returns None.
#[test]
fn s8_checker_reports_invalid_utf8_strings() {
  let mut image = keyed_image();
  corrupt_label_name_utf8(&mut image);
  let loaded = image.load().expect("invalid UTF-8 loads today");
  assert_eq!(
    loaded.snapshot.label_name(PERSON),
    None,
    "precondition: the label name no longer decodes"
  );
  assert_detected("label name with invalid UTF-8", &image);
}

/// In-edges are sorted by (etype, src) per destination, like the writer emits
/// them. Today their order is not checked.
#[test]
fn s8_checker_reports_unsorted_in_edges() {
  let mut image = keyed_image();
  let mut csr = Csr::read(&image);
  // p2: [(K,0) (K,1) (L,3)] -> [(K,1) (K,0) (L,3)], still reciprocal.
  let start = csr.in_offsets[2] as usize;
  csr.swap_in_edges(start, start + 1);
  csr.write(&mut image);
  assert_detected("in-edges of p2 not sorted by (etype, src)", &image);
}

// ============================================================================
// S9: checker bugs
// ============================================================================

/// Without a KeyEntries section, check_snapshot returns before its string
/// table pass. A keyless snapshot with a bad string must still be reported.
#[test]
fn s9_checker_checks_strings_without_key_index() {
  let mut image = Image::build(people(DENSE_IDS, false));
  assert!(
    image.raw(SectionId::KeyEntries).is_empty(),
    "keyless fixture"
  );
  corrupt_label_name_utf8(&mut image);
  assert_detected("keyless snapshot, label name with invalid UTF-8", &image);
}

/// The writer emits duplicate (src, etype, dst) edges as given, and the
/// checker's sort pass treats them as a warning. Its reciprocity pass matches
/// in-edges by (src, etype) only and reports the second copy as a mismatch.
#[test]
fn s9_checker_accepts_duplicate_edges() {
  let mut input = people(DENSE_IDS, true);
  input.edges.push(edge(1, KNOWS, 2));
  let image = Image::build(input);
  let report = check(&image).expect("snapshot with a duplicate edge loads");
  assert!(
    report.valid,
    "S9: duplicate edge reported as corruption: {:?} (warnings: {:?})",
    report.errors, report.warnings
  );
}

/// Found while fixing S9: with no nodes, PhysToNodeId is empty and therefore
/// absent, and check_snapshot reported "node/phys mapping sections missing"
/// for every empty snapshot.
#[test]
fn s9_checker_accepts_empty_snapshot() {
  let image = Image::build(SnapshotBuildInput {
    generation: 1,
    nodes: Vec::new(),
    edges: Vec::new(),
    labels: HashMap::new(),
    etypes: HashMap::new(),
    propkeys: HashMap::new(),
    vector_stores: None,
    compression: None,
  });
  let report = check(&image).expect("empty snapshot loads");
  assert!(
    report.valid,
    "S9: empty snapshot reported as corrupt: {:?}",
    report.errors
  );
}

// ============================================================================
// S10: string ID off-by-one
// ============================================================================

/// StringOffsets has num_strings + 1 entries, so valid string IDs are
/// 0..num_strings. Every validator compares with `> num_strings` and lets
/// `id == num_strings` through; reading it then returns None.
#[test]
fn s10_string_id_equal_to_num_strings_is_rejected_at_load() {
  let base = keyed_image();
  let num_strings = base.header_u64(NUM_STRINGS_OFFSET);
  let past_end = num_strings as u32;
  let mut accepted = Vec::new();
  let mut try_case = |name: &str, image: Image| {
    if image.load().is_ok() {
      accepted.push(name.to_string());
    }
  };

  for (name, id, index) in [
    ("LabelStringIds[1]", SectionId::LabelStringIds, 1),
    ("EtypeStringIds[1]", SectionId::EtypeStringIds, 1),
    ("PropkeyStringIds[1]", SectionId::PropkeyStringIds, 1),
    ("NodeKeyString[0]", SectionId::NodeKeyString, 0),
  ] {
    let mut image = base.clone();
    let mut ids = image.u32s(id);
    ids[index] = past_end;
    image.set_u32s(id, &ids);
    try_case(name, image);
  }

  let mut image = base.clone();
  let mut records = key_records(&image);
  records[0].string_id = past_end;
  set_key_records(&mut image, &records);
  try_case("KeyEntries[0].string_id", image);

  for (name, id) in [
    ("NodePropVals string payload", SectionId::NodePropVals),
    ("EdgePropVals string payload", SectionId::EdgePropVals),
  ] {
    let mut image = base.clone();
    let mut vals = image.raw(id).to_vec();
    let index = first_prop_with_tag(&vals, PropValueTag::String);
    write_u64(&mut vals, index * PROP_VALUE_DISK_SIZE + 8, num_strings);
    image.set_raw(id, vals);
    try_case(name, image);
  }

  assert!(
    accepted.is_empty(),
    "S10: string ID {past_end} (== num_strings) accepted in: {}",
    accepted.join(", ")
  );
}

/// The vector half of S10 (`(idx + 1) * 8 > len` let the last index through)
/// was fixed in the reader by wave 1; load also rejects a vector index equal
/// to the vector count before any accessor sees it.
#[test]
fn s10_vector_index_equal_to_vector_count_is_rejected_at_load() {
  let mut image = keyed_image();
  let vector_count = image.u64s(SectionId::VectorOffsets).len() as u64 - 1;
  let mut vals = image.raw(SectionId::NodePropVals).to_vec();
  let index = first_prop_with_tag(&vals, PropValueTag::VectorF32);
  write_u64(&mut vals, index * PROP_VALUE_DISK_SIZE + 8, vector_count);
  image.set_raw(SectionId::NodePropVals, vals);
  assert!(
    image.load().is_err(),
    "vector index {vector_count} (== vector count) accepted"
  );
}

// ============================================================================
// S11: key lookup without buckets
// ============================================================================

/// Drop KeyBuckets (and its flag) from a keyed snapshot. Entries stay sorted
/// by (bucket, hash), so the hash binary search misses keys. Either the
/// reader rejects the file or every key must still resolve.
#[test]
fn s11_keyed_snapshot_without_buckets_is_rejected_or_searchable() {
  const KEYS: u64 = 64;
  let nodes = (1..=KEYS)
    .map(|node_id| NodeData {
      node_id,
      key: Some(format!("key-{node_id}")),
      labels: Vec::new(),
      props: HashMap::new(),
    })
    .collect();
  let mut image = Image::build(SnapshotBuildInput {
    generation: 1,
    nodes,
    edges: Vec::new(),
    labels: HashMap::new(),
    etypes: HashMap::new(),
    propkeys: HashMap::new(),
    vector_stores: None,
    compression: None,
  });
  image.set_raw(SectionId::KeyBuckets, Vec::new());
  image.set_flags(image.flags() - SnapshotFlags::HAS_KEY_BUCKETS);

  let Ok(loaded) = image.load() else {
    return;
  };
  let missed: Vec<String> = (1..=KEYS)
    .map(|node_id| (node_id, format!("key-{node_id}")))
    .filter(|(node_id, key)| loaded.snapshot.lookup_by_key(key) != Some(*node_id))
    .map(|(_, key)| key)
    .collect();
  assert!(
    missed.is_empty(),
    "S11: bucketless keyed snapshot loaded, but {} of {KEYS} keys miss: {missed:?}",
    missed.len()
  );
}

// ============================================================================
// S13: PhysToNodeId contents
// ============================================================================

/// validate_structure checks PhysToNodeId's length only. Each case loads
/// today and makes node_id(phys) disagree with phys_node(node_id).
#[test]
fn s13_phys_to_node_id_contents_validated_at_load() {
  let mut accepted = Vec::new();
  for (layout, ids) in [("dense", DENSE_IDS), ("sparse", SPARSE_IDS)] {
    let base = Image::build(people(ids, true));
    let sparse = base.flags().contains(SnapshotFlags::SPARSE_NODE_ID_MAP);
    assert_eq!(sparse, layout == "sparse", "fixture layout");
    let max_node_id = base.header_u64(MAX_NODE_ID_OFFSET);
    let original = base.u64s(SectionId::PhysToNodeId);

    let mut duplicate = original.clone();
    duplicate[1] = duplicate[0];
    let mut swapped = original.clone();
    swapped.swap(0, 1);
    let mut past_max = original.clone();
    past_max[3] = max_node_id.wrapping_add(1);
    let mut unmapped = original.clone();
    unmapped[2] = 0;

    for (name, values) in [
      ("duplicate node ID", duplicate),
      ("swapped node IDs", swapped),
      ("node ID above max_node_id", past_max),
      ("node ID absent from NodeIdToPhys", unmapped),
    ] {
      let mut image = base.clone();
      image.set_u64s(SectionId::PhysToNodeId, &values);
      if image.load().is_ok() {
        accepted.push(format!("{layout}: {name} {values:?}"));
      }
    }
  }
  assert!(
    accepted.is_empty(),
    "S13: invalid PhysToNodeId accepted at load:\n{}",
    accepted.join("\n")
  );
}

/// Two nodes with one ID: the writer maps the ID to the second, so the first
/// physical node (and its key, labels and properties) is unreachable by ID
/// and reported by nothing. With S13 load validation such a snapshot would
/// not open again, so the writer must refuse it.
#[test]
fn s13_writer_rejects_duplicate_node_ids() {
  let mut input = people(DENSE_IDS, true);
  input.nodes[1].node_id = DENSE_IDS[0];
  input
    .edges
    .retain(|edge| edge.src != DENSE_IDS[1] && edge.dst != DENSE_IDS[1]);
  if let Ok(bytes) = build_snapshot_to_memory(input) {
    let loaded = load_bytes(&bytes).map(|loaded| {
      let snapshot = &loaded.snapshot;
      (snapshot.node_id(0), snapshot.phys_node(DENSE_IDS[0]))
    });
    panic!(
      "S13: snapshot with node ID {} twice was written; load gives \
       (node_id(0), phys_node(id)) = {loaded:?}",
      DENSE_IDS[0]
    );
  }
}

// ============================================================================
// S14: room for future sections
// ============================================================================

/// The footer CRC position comes from the known sections only, so a future
/// version can add sections without raising min_reader_version only if v5
/// readers accept, and CRC-cover, bytes they do not know about before the
/// last known section. Pins that property (see the S14 design).
#[test]
fn s14_unknown_bytes_before_known_sections_are_crc_covered_and_ignored() {
  let image = keyed_image();
  let bytes = image.encode();
  let table_end = SNAPSHOT_HEADER_SIZE + SectionId::COUNT * SECTION_ENTRY_SIZE;
  let data_start = align_up(table_end, SECTION_ALIGNMENT);
  let gap = 4 * SECTION_ALIGNMENT;

  // Shift every section by `gap` and fill the hole with unknown bytes.
  let mut shifted = Vec::with_capacity(bytes.len() + gap);
  shifted.extend_from_slice(&bytes[..data_start]);
  shifted.extend(std::iter::repeat_n(0xA5u8, gap));
  shifted.extend_from_slice(&bytes[data_start..bytes.len() - 4]);
  for id in 0..SectionId::COUNT {
    let entry = SNAPSHOT_HEADER_SIZE + id * SECTION_ENTRY_SIZE;
    let offset = read_u64(&shifted, entry);
    if offset != 0 {
      write_u64(&mut shifted, entry, offset + gap as u64);
    }
  }
  let crc = crc32(&shifted);
  shifted.extend_from_slice(&crc.to_le_bytes());

  let loaded = load_bytes(&shifted).expect("unknown bytes before the sections load");
  assert_eq!(loaded.snapshot.lookup_by_key("user:2"), Some(DENSE_IDS[2]));

  shifted[data_start] ^= 0xFF;
  let error = load_bytes(&shifted)
    .err()
    .expect("a flipped unknown byte fails the CRC");
  assert!(error.contains("CRC"), "{error}");
}

// ============================================================================
// Perf baselines (S4, S6, S7, S9, S12): #[ignore]d, run in release
// ============================================================================

fn generated_input(
  nodes: u64,
  degree: u64,
  compression: Option<CompressionOptions>,
) -> SnapshotBuildInput {
  let node_data = (1..=nodes)
    .map(|node_id| NodeData {
      node_id,
      key: Some(format!("user:{node_id:010}")),
      labels: vec![PERSON],
      props: HashMap::from([
        (NAME, PropValue::String(format!("name-{node_id}"))),
        (AGE, PropValue::I64(node_id as i64)),
      ]),
    })
    .collect();
  let mut edges = Vec::with_capacity((nodes * degree) as usize);
  for src in 1..=nodes {
    for k in 0..degree {
      let dst = (src.wrapping_mul(2_654_435_761) + k * 40_503 + k * k * 7) % nodes + 1;
      edges.push(edge(src, 1 + (k % 2) as ETypeId, dst));
    }
  }
  SnapshotBuildInput {
    generation: 1,
    nodes: node_data,
    edges,
    labels: HashMap::from([(PERSON, "Person".to_string())]),
    etypes: HashMap::from([(KNOWS, "KNOWS".to_string()), (LIKES, "LIKES".to_string())]),
    propkeys: HashMap::from([(NAME, "name".to_string()), (AGE, "age".to_string())]),
    vector_stores: None,
    compression,
  }
}

fn env_u64(name: &str, default: u64) -> u64 {
  std::env::var(name)
    .ok()
    .and_then(|value| value.parse().ok())
    .unwrap_or(default)
}

fn mib(bytes: usize) -> f64 {
  bytes as f64 / (1024.0 * 1024.0)
}

fn ns_per(elapsed: Duration, ops: u64) -> f64 {
  elapsed.as_nanos() as f64 / ops.max(1) as f64
}

/// Deterministic pseudo-random sequence (xorshift64*).
struct Rng(u64);

impl Rng {
  fn next_u64(&mut self) -> u64 {
    self.0 ^= self.0 >> 12;
    self.0 ^= self.0 << 25;
    self.0 ^= self.0 >> 27;
    self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
  }
}

/// Process max RSS in bytes (0 where getrusage is unavailable).
#[cfg(not(unix))]
fn max_rss_bytes() -> u64 {
  0
}

#[cfg(unix)]
fn max_rss_bytes() -> u64 {
  // SAFETY: getrusage fills a zeroed rusage struct.
  let usage = unsafe {
    let mut usage: libc::rusage = std::mem::zeroed();
    libc::getrusage(libc::RUSAGE_SELF, &mut usage);
    usage
  };
  let max_rss = usage.ru_maxrss as u64;
  if cfg!(target_os = "macos") {
    max_rss
  } else {
    max_rss * 1024
  }
}

const OPEN_CHILD_ENV: &str = "KITEDB_W3_BASELINE_OPEN";

/// Child half of the RSS baseline: opens the snapshot in `OPEN_CHILD_ENV`,
/// touches every node once, and prints the process max RSS.
#[test]
#[ignore = "perf baseline helper; spawned by baseline_snapshot_perf"]
fn baseline_open_rss_child() {
  let Ok(path) = std::env::var(OPEN_CHILD_ENV) else {
    return;
  };
  let before = max_rss_bytes();
  let snapshot = SnapshotData::load(&path).expect("load snapshot");
  let opened = max_rss_bytes();
  let mut sum = 0u64;
  for phys in 0..snapshot.num_nodes() as PhysNode {
    sum += snapshot.iter_out_edges(phys).count() as u64;
    sum += snapshot.node_key(phys).map_or(0, |key| key.len() as u64);
  }
  black_box(sum);
  let touched = max_rss_bytes();
  println!("RSS before={before} opened={opened} touched={touched}");
}

fn open_rss_in_child(path: &std::path::Path) -> String {
  let output = Command::new(std::env::current_exe().expect("current test binary"))
    .args([
      "baseline_open_rss_child",
      "--exact",
      "--ignored",
      "--nocapture",
      "--test-threads=1",
    ])
    .env(OPEN_CHILD_ENV, path)
    .output()
    .expect("spawn RSS child");
  let stdout = String::from_utf8_lossy(&output.stdout);
  // libtest prints "test <name> ... " before the child's own output.
  let line = stdout
    .lines()
    .find_map(|line| line.find("RSS before=").map(|start| &line[start..]))
    .unwrap_or("RSS child printed nothing");
  let values: HashMap<&str, f64> = line
    .trim_start_matches("RSS ")
    .split_whitespace()
    .filter_map(|pair| pair.split_once('='))
    .filter_map(|(name, value)| Some((name, value.parse::<f64>().ok()? / (1024.0 * 1024.0))))
    .collect();
  format!(
    "max RSS MiB: before open {:.1}, after open {:.1}, after touching all nodes {:.1}",
    values.get("before").copied().unwrap_or(f64::NAN),
    values.get("opened").copied().unwrap_or(f64::NAN),
    values.get("touched").copied().unwrap_or(f64::NAN),
  )
}

fn bench_reads(label: &str, snapshot: &SnapshotData, nodes: u64) {
  const OPS: u64 = 1_000_000;
  let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
  let queries: Vec<(PhysNode, ETypeId, PhysNode)> = (0..OPS)
    .map(|_| {
      (
        (rng.next_u64() % nodes) as PhysNode,
        1 + (rng.next_u64() % 2) as ETypeId,
        (rng.next_u64() % nodes) as PhysNode,
      )
    })
    .collect();

  let start = Instant::now();
  let mut hits = 0u64;
  for &(src, etype, dst) in &queries {
    hits += u64::from(snapshot.has_edge(src, etype, dst));
  }
  let has_edge_ns = ns_per(start.elapsed(), OPS);
  black_box(hits);

  let threads = 8u64;
  let start = Instant::now();
  std::thread::scope(|scope| {
    for thread in 0..threads {
      let queries = &queries;
      scope.spawn(move || {
        let mut hits = 0u64;
        for &(src, etype, dst) in queries.iter().skip(thread as usize) {
          hits += u64::from(snapshot.has_edge(src, etype, dst));
        }
        black_box(hits);
      });
    }
  });
  let parallel_elapsed = start.elapsed();
  let parallel_ops = queries.len() as u64 * threads;
  let parallel_mops = parallel_ops as f64 / parallel_elapsed.as_secs_f64() / 1e6;

  let start = Instant::now();
  let mut edges = 0u64;
  for phys in 0..nodes as PhysNode {
    for (dst, etype) in snapshot.iter_out_edges(phys) {
      edges += u64::from(dst) ^ u64::from(etype);
    }
  }
  let scan = start.elapsed();
  black_box(edges);
  let num_edges = snapshot.num_edges();

  let start = Instant::now();
  let mut ages = 0i64;
  for &(src, _, _) in &queries {
    if let Some(PropValue::I64(age)) = snapshot.node_prop(src, AGE) {
      ages += age;
    }
  }
  let node_prop_ns = ns_per(start.elapsed(), OPS);
  black_box(ages);

  let keys: Vec<String> = (0..100_000)
    .map(|_| format!("user:{:010}", 1 + rng.next_u64() % nodes))
    .collect();
  let (found, stats) = measure(false, || {
    let start = Instant::now();
    let found = keys
      .iter()
      .filter(|key| snapshot.lookup_by_key(key).is_some())
      .count();
    (found, start.elapsed())
  });
  let (found, lookup_elapsed) = found;
  assert_eq!(found, keys.len(), "all generated keys resolve");

  let missing: Vec<String> = (0..100_000).map(|i| format!("absent:{i}")).collect();
  let start = Instant::now();
  let absent = missing
    .iter()
    .filter(|key| snapshot.lookup_by_key(key).is_some())
    .count();
  let miss_ns = ns_per(start.elapsed(), missing.len() as u64);
  assert_eq!(absent, 0);

  println!(
    "[{label}] has_edge {has_edge_ns:.1} ns/op (1 thread); {parallel_mops:.1} M ops/s on \
     {threads} threads; iter_out_edges {:.2} ns/edge; node_prop {node_prop_ns:.1} ns/op; \
     lookup_by_key hit {:.1} ns/op, {:.2} allocs/op; miss {miss_ns:.1} ns/op",
    ns_per(scan, num_edges),
    ns_per(lookup_elapsed, keys.len() as u64),
    stats.count as f64 / keys.len() as f64,
  );
}

/// Records S4 (open cost, read latency, reader contention), S6 (string
/// cache), S7 (writer peak heap) and S12 (key lookup allocations) on a
/// generated graph. `W3_NODES` (default 200k) and `W3_DEGREE` (default 5).
#[test]
#[ignore = "perf baseline; run in release with --ignored --nocapture"]
fn baseline_snapshot_perf() {
  let nodes = env_u64("W3_NODES", 200_000);
  let degree = env_u64("W3_DEGREE", 5);
  println!(
    "graph: {nodes} nodes (key, label, 2 props each), {} edges, 2 etypes; release={}",
    nodes * degree,
    !cfg!(debug_assertions)
  );

  for (label, compression) in [
    ("raw", None),
    (
      "zstd",
      Some(CompressionOptions {
        enabled: true,
        ..Default::default()
      }),
    ),
  ] {
    let input = generated_input(nodes, degree, compression);
    let start = Instant::now();
    let (bytes, write_stats) = measure(false, || build_snapshot_to_memory(input));
    let write_elapsed = start.elapsed();
    let bytes = bytes.expect("build snapshot");
    println!(
      "[{label}] S7 writer: {:.0} ms, snapshot {:.1} MiB, peak live heap {:.1} MiB ({:.1}x \
       snapshot), {:.1} MiB allocated in {} allocations",
      write_elapsed.as_secs_f64() * 1e3,
      mib(bytes.len()),
      mib(write_stats.peak_live),
      write_stats.peak_live as f64 / bytes.len() as f64,
      mib(write_stats.allocated),
      write_stats.count,
    );

    let file = write_temp(&bytes);
    drop(bytes);
    let start = Instant::now();
    let (snapshot, open_stats) = measure(false, || SnapshotData::load(file.path()));
    let open_elapsed = start.elapsed();
    let snapshot = snapshot.expect("load snapshot");
    println!(
      "[{label}] S4/S6 open: {:.1} ms, heap {:.1} MiB live after open ({:.1} MiB allocated) \
       for {} strings",
      open_elapsed.as_secs_f64() * 1e3,
      mib(open_stats.peak_live),
      mib(open_stats.allocated),
      snapshot.header.num_strings,
    );
    println!("[{label}] S4 {}", open_rss_in_child(file.path()));

    bench_reads(label, &snapshot, nodes);

    let start = Instant::now();
    let report = check_snapshot(&snapshot);
    println!(
      "[{label}] check_snapshot: {:.0} ms ({} errors, {} warnings)",
      start.elapsed().as_secs_f64() * 1e3,
      report.errors.len(),
      report.warnings.len()
    );
  }

  // S9: reciprocity is O(E * indegree); a hub with high in-degree shows it.
  let hub_in = env_u64("W3_HUB_IN", 20_000);
  let mut hub = generated_input(hub_in + 1, 0, None);
  hub.edges = (2..=hub_in + 1).map(|src| edge(src, KNOWS, 1)).collect();
  let image = Image::build(hub);
  let loaded = image.load().expect("load hub snapshot");
  let start = Instant::now();
  let report = check_snapshot(&loaded.snapshot);
  println!(
    "[hub] S9 check_snapshot with one node of in-degree {hub_in}: {:.0} ms (valid={})",
    start.elapsed().as_secs_f64() * 1e3,
    report.valid
  );
}

/// Writer output satisfies the key-hash invariant S8 asks the checker to
/// enforce: every KeyEntries hash is xxhash64 of its key string.
#[test]
fn helper_key_hashes_are_xxhash64_of_the_key() {
  let image = keyed_image();
  let strings = image.u64s(SectionId::StringOffsets);
  let bytes = image.raw(SectionId::StringBytes);
  for record in key_records(&image) {
    let id = record.string_id as usize;
    let key = std::str::from_utf8(&bytes[strings[id] as usize..strings[id + 1] as usize])
      .expect("utf-8 key");
    assert_eq!(record.hash, xxhash64_string(key), "{key}");
  }
}
