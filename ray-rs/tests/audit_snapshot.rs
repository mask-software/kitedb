//! Snapshot audit findings S1-S3, reproduced through the public API.
//!
//! S1: NodeIdToPhys is a dense array of `4 * (max_node_id + 1)` bytes. One node
//!     with a large user-chosen ID makes checkpoint, and every later open,
//!     allocate memory proportional to that ID, or overflow outright.
//! S2: Section sizes are stored as u32. The S1 section reaches 4 GiB at
//!     max_node_id = 2^30 - 1, its size is truncated, and the reader rejects the
//!     snapshot after checkpoint has already installed it.
//! S3: `phys_node` computes `node_id * 4 + 4` unchecked, so IDs >= 2^62 panic
//!     (overflow checks) or alias low node IDs (release).
//!
//! Memory guard: this binary installs a global allocator that refuses any
//! single allocation above `ALLOC_CAP` by exiting the process with
//! `ALLOC_CAP_EXIT_CODE`. Scenarios that can hit the cap run in a child
//! process; the parent turns the exit into an assertion failure. A dense-array
//! regression therefore fails fast instead of exhausting the machine.

use kitedb::check::check_snapshot;
use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions,
};
use kitedb::core::snapshot::reader::SnapshotData;
use kitedb::types::{DbHeaderV1, ETypeId, NodeId, PropKeyId, PropValue};
use kitedb::util::binary::read_u32;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::fmt::Debug;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::Command;

// ============================================================================
// Allocation guard
// ============================================================================

const ALLOC_CAP: usize = 1 << 30;
const ALLOC_CAP_EXIT_CODE: i32 = 86;

struct CappedAlloc;

thread_local! {
  static TRACK_LARGEST: Cell<bool> = const { Cell::new(false) };
  static LARGEST_ALLOC: Cell<usize> = const { Cell::new(0) };
}

fn write_stderr(bytes: &[u8]) {
  // SAFETY: plain write(2) of a valid buffer; never allocates.
  unsafe {
    libc::write(2, bytes.as_ptr().cast(), bytes.len());
  }
}

fn refuse_allocation(size: usize) -> ! {
  let mut digits = [0u8; 20];
  let mut start = digits.len();
  let mut value = size;
  loop {
    start -= 1;
    digits[start] = b'0' + (value % 10) as u8;
    value /= 10;
    if value == 0 {
      break;
    }
  }
  write_stderr(b"\naudit_snapshot: refused a single allocation of ");
  write_stderr(&digits[start..]);
  write_stderr(b" bytes (cap 1 GiB)\n");
  // SAFETY: terminates the process without unwinding or allocating.
  unsafe { libc::_exit(ALLOC_CAP_EXIT_CODE) }
}

fn record_allocation(size: usize) {
  if size > ALLOC_CAP {
    refuse_allocation(size);
  }
  let _ = TRACK_LARGEST.try_with(|track| {
    if track.get() {
      let _ = LARGEST_ALLOC.try_with(|largest| largest.set(largest.get().max(size)));
    }
  });
}

unsafe impl GlobalAlloc for CappedAlloc {
  unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
    record_allocation(layout.size());
    System.alloc(layout)
  }

  unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
    record_allocation(layout.size());
    System.alloc_zeroed(layout)
  }

  unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
    record_allocation(new_size);
    System.realloc(ptr, layout, new_size)
  }

  unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
    System.dealloc(ptr, layout)
  }
}

#[global_allocator]
static GLOBAL: CappedAlloc = CappedAlloc;

/// Largest single allocation made by the current thread while `f` runs.
fn largest_allocation_during<R>(f: impl FnOnce() -> R) -> (R, usize) {
  LARGEST_ALLOC.with(|largest| largest.set(0));
  TRACK_LARGEST.with(|track| track.set(true));
  let result = f();
  TRACK_LARGEST.with(|track| track.set(false));
  (result, LARGEST_ALLOC.with(|largest| largest.get()))
}

// ============================================================================
// Child-process isolation
// ============================================================================

const CHILD_ENV: &str = "KITEDB_AUDIT_SNAPSHOT_CHILD";

fn tail(text: &str) -> String {
  let lines: Vec<&str> = text.lines().collect();
  lines[lines.len().saturating_sub(40)..].join("\n")
}

/// Run `scenario` in a child copy of this test binary. `test_name` must be the
/// name of the calling `#[test]` function.
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
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);

  if output.status.code() == Some(ALLOC_CAP_EXIT_CODE) {
    panic!(
      "{test_name}: memory scales with the node ID; the child requested a single allocation \
       above {ALLOC_CAP} bytes\n--- child stderr ---\n{}",
      tail(&stderr)
    );
  }
  assert!(
    output.status.success(),
    "{test_name}: child failed ({})\n--- child stdout ---\n{}\n--- child stderr ---\n{}",
    output.status,
    tail(&stdout),
    tail(&stderr)
  );
  assert!(
    stdout.contains("1 passed"),
    "{test_name}: child did not run the scenario\n{stdout}"
  );
}

// ============================================================================
// Shared graph helpers
// ============================================================================

const WAL_SIZE: usize = 64 * 1024;
/// Header pages + a 64 KiB WAL + a snapshot of three nodes fit easily.
const MAX_SMALL_DB_FILE_BYTES: u64 = 1024 * 1024;
/// Generous bound for any single allocation while checkpointing or opening a
/// three-node graph with a 64 KiB WAL.
const MAX_SMALL_GRAPH_ALLOC: usize = 1024 * 1024;

fn options() -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .wal_size(WAL_SIZE)
    .auto_checkpoint(false)
}

struct Schema {
  name: PropKeyId,
  knows: ETypeId,
}

/// Nodes 1 and 2 plus `high`, with keys, a property and edges through `high`.
fn populate(db: &SingleFileDB, high: NodeId) -> Schema {
  db.begin(false).expect("begin");
  let name = db.define_propkey("name").expect("define propkey");
  let knows = db.define_etype("KNOWS").expect("define etype");
  for low in [1u64, 2] {
    db.create_node_with_id(low, Some(&format!("low:{low}")))
      .expect("create low node");
  }
  db.create_node_with_id(high, Some("high"))
    .unwrap_or_else(|error| panic!("create_node_with_id({high}) failed: {error}"));
  db.set_node_prop(high, name, PropValue::String(format!("node {high}")))
    .expect("set prop");
  db.add_edge(1, knows, high).expect("add edge 1 -> high");
  db.add_edge(high, knows, 2).expect("add edge high -> 2");
  db.commit().expect("commit");
  Schema { name, knows }
}

fn assert_graph_intact(db: &SingleFileDB, high: NodeId, schema: &Schema, stage: &str) {
  assert!(db.node_exists(high), "{stage}: node {high} is missing");
  assert!(db.node_exists(1), "{stage}: node 1 is missing");
  assert!(db.node_exists(2), "{stage}: node 2 is missing");
  assert!(!db.node_exists(3), "{stage}: node 3 should not exist");
  assert!(
    !db.node_exists(high - 1),
    "{stage}: node {} should not exist",
    high - 1
  );
  if let Some(next) = high.checked_add(1) {
    assert!(
      !db.node_exists(next),
      "{stage}: node {next} should not exist"
    );
  }
  assert_eq!(db.node_by_key("high"), Some(high), "{stage}: key lookup");
  assert_eq!(
    db.node_key(high).as_deref(),
    Some("high"),
    "{stage}: node key"
  );
  assert_eq!(
    db.node_prop(high, schema.name),
    Some(PropValue::String(format!("node {high}"))),
    "{stage}: node prop"
  );
  assert_eq!(
    db.out_edges(1),
    vec![(schema.knows, high)],
    "{stage}: out(1)"
  );
  assert_eq!(
    db.out_edges(high),
    vec![(schema.knows, 2)],
    "{stage}: out(high)"
  );
  assert_eq!(db.in_edges(2), vec![(schema.knows, high)], "{stage}: in(2)");
  assert_eq!(db.count_nodes(), 3, "{stage}: node count");
}

/// Create, checkpoint, close and reopen a graph containing node `high`.
fn huge_node_id_round_trip(high: NodeId) {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("huge-id.kitedb");

  let db = open_single_file(&path, options()).expect("open");
  let schema = populate(&db, high);
  assert_graph_intact(&db, high, &schema, "before checkpoint");
  db.checkpoint()
    .unwrap_or_else(|error| panic!("checkpoint with node {high} failed: {error}"));
  assert_graph_intact(&db, high, &schema, "after checkpoint");
  close_single_file(db).expect("close");

  let file_size = std::fs::metadata(&path).expect("db metadata").len();
  assert!(
    file_size < MAX_SMALL_DB_FILE_BYTES,
    "database with 3 nodes (max id {high}) is {file_size} bytes"
  );

  let db = open_single_file(&path, options())
    .unwrap_or_else(|error| panic!("reopen after checkpoint with node {high} failed: {error}"));
  assert_graph_intact(&db, high, &schema, "after reopen");
  close_single_file(db).expect("close reopened");
}

// ============================================================================
// S1: snapshot cost must not scale with the largest node ID
// ============================================================================

#[test]
fn s1_node_id_3e9_survives_checkpoint_and_reopen() {
  run_in_child("s1_node_id_3e9_survives_checkpoint_and_reopen", || {
    huge_node_id_round_trip(3_000_000_000)
  });
}

#[test]
fn s1_node_id_2_pow_40_survives_checkpoint_and_reopen() {
  run_in_child("s1_node_id_2_pow_40_survives_checkpoint_and_reopen", || {
    huge_node_id_round_trip(1 << 40)
  });
}

#[test]
fn s1_node_id_u64_max_minus_one_survives_checkpoint_and_reopen() {
  run_in_child(
    "s1_node_id_u64_max_minus_one_survives_checkpoint_and_reopen",
    || huge_node_id_round_trip(u64::MAX - 1),
  );
}

/// Scaled-down S1: a 16 MiB dense array is cheap enough to build in-process,
/// but a three-node graph should never need anything close to it.
#[test]
fn s1_checkpoint_and_open_memory_do_not_scale_with_max_node_id() {
  let high: NodeId = 1 << 22;
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("scaled.kitedb");

  let db = open_single_file(&path, options()).expect("open");
  let schema = populate(&db, high);
  let (checkpoint, checkpoint_peak) = largest_allocation_during(|| db.checkpoint());
  checkpoint.expect("checkpoint");
  assert_graph_intact(&db, high, &schema, "after checkpoint");
  close_single_file(db).expect("close");

  let (reopened, open_peak) = largest_allocation_during(|| open_single_file(&path, options()));
  let db = reopened.expect("reopen");
  assert_graph_intact(&db, high, &schema, "after reopen");
  close_single_file(db).expect("close reopened");

  assert!(
    checkpoint_peak <= MAX_SMALL_GRAPH_ALLOC && open_peak <= MAX_SMALL_GRAPH_ALLOC,
    "3 nodes (max id {high}): largest single allocation was {checkpoint_peak} bytes during \
     checkpoint and {open_peak} bytes during open (limit {MAX_SMALL_GRAPH_ALLOC})"
  );
}

/// Scaled-down S1 for disk usage: without compression the dense array lands
/// on disk byte for byte.
#[test]
fn s1_snapshot_file_size_does_not_scale_with_max_node_id() {
  let high: NodeId = 1 << 22;
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("scaled-uncompressed.kitedb");
  let options = options().disable_checkpoint_compression();

  let db = open_single_file(&path, options.clone()).expect("open");
  let schema = populate(&db, high);
  db.checkpoint().expect("checkpoint");
  close_single_file(db).expect("close");

  let file_size = std::fs::metadata(&path).expect("db metadata").len();
  let db = open_single_file(&path, options).expect("reopen");
  assert_graph_intact(&db, high, &schema, "after reopen");
  close_single_file(db).expect("close reopened");

  assert!(
    file_size < MAX_SMALL_DB_FILE_BYTES,
    "uncompressed database with 3 nodes (max id {high}) is {file_size} bytes"
  );
}

// ============================================================================
// S2: no section may silently exceed its on-disk size field
// ============================================================================

/// max_node_id = 2^30 - 1 makes the dense NodeIdToPhys section exactly 4 GiB,
/// so its u32 `uncompressed_size` truncates to 0 and the installed snapshot is
/// rejected on reload. The truncation itself needs 4 GiB of memory to observe;
/// here the allocation guard stops the child first.
#[test]
fn s2_node_id_at_4gib_section_boundary_survives_checkpoint_and_reopen() {
  run_in_child(
    "s2_node_id_at_4gib_section_boundary_survives_checkpoint_and_reopen",
    || huge_node_id_round_trip((1 << 30) - 1),
  );
}

// ============================================================================
// S3: lookups by out-of-range node ID must miss, not panic or alias
// ============================================================================

const OUT_OF_RANGE_NODE_IDS: [NodeId; 6] = [
  1 << 62,
  (1 << 62) + 1,
  (1 << 62) + 2,
  (1 << 63) + 1,
  u64::MAX - 1,
  u64::MAX,
];

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
  if let Some(message) = payload.downcast_ref::<&str>() {
    (*message).to_string()
  } else if let Some(message) = payload.downcast_ref::<String>() {
    message.clone()
  } else {
    "non-string panic".to_string()
  }
}

fn probe<T: PartialEq + Debug>(
  failures: &mut Vec<String>,
  what: String,
  expected: T,
  f: impl FnOnce() -> T,
) {
  match catch_unwind(AssertUnwindSafe(f)) {
    Ok(actual) if actual == expected => {}
    Ok(actual) => failures.push(format!("{what} = {actual:?}, expected {expected:?}")),
    Err(payload) => failures.push(format!("{what} panicked: {}", panic_message(&*payload))),
  }
}

#[test]
fn s3_lookups_by_out_of_range_node_id_miss_without_panicking() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("lookups.kitedb");
  let db = open_single_file(&path, options()).expect("open");
  db.begin(false).expect("begin");
  let name = db.define_propkey("name").expect("define propkey");
  let knows = db.define_etype("KNOWS").expect("define etype");
  for id in [1u64, 2] {
    db.create_node_with_id(id, Some(&format!("n{id}")))
      .expect("create node");
    db.set_node_prop(id, name, PropValue::I64(id as i64))
      .expect("set prop");
  }
  db.add_edge(1, knows, 2).expect("add edge");
  db.commit().expect("commit");
  db.checkpoint().expect("checkpoint");

  let fixture = SnapshotData::load(fixture_path("audit_snapshot_v4.gds")).expect("load v4 fixture");

  let mut failures = Vec::new();
  for id in OUT_OF_RANGE_NODE_IDS {
    probe(
      &mut failures,
      format!("db.node_exists({id})"),
      false,
      || db.node_exists(id),
    );
    probe(&mut failures, format!("db.node_props({id})"), None, || {
      db.node_props(id)
    });
    probe(&mut failures, format!("db.node_key({id})"), None, || {
      db.node_key(id)
    });
    probe(&mut failures, format!("db.out_edges({id})"), vec![], || {
      db.out_edges(id)
    });
    probe(&mut failures, format!("db.in_edges({id})"), vec![], || {
      db.in_edges(id)
    });
    probe(
      &mut failures,
      format!("db.edge_exists({id}, KNOWS, 2)"),
      false,
      || db.edge_exists(id, knows, 2),
    );
    probe(
      &mut failures,
      format!("v4 fixture phys_node({id})"),
      None,
      || fixture.phys_node(id),
    );
  }
  close_single_file(db).expect("close");

  assert!(
    failures.is_empty(),
    "out-of-range node IDs must miss cleanly:\n{}",
    failures.join("\n")
  );
}

// ============================================================================
// Format migration guards (policy: bump the version, keep reading v4)
// ============================================================================

fn fixture_path(name: &str) -> PathBuf {
  PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    .join("tests/fixtures")
    .join(name)
}

/// Snapshot (version, min_reader_version) of the active header slot.
fn active_snapshot_versions(path: &Path) -> (u32, u32) {
  let bytes = std::fs::read(path).expect("read db file");
  let page_size = read_u32(&bytes, 16) as usize;
  let header = [0usize, 1]
    .iter()
    .filter_map(|&slot| DbHeaderV1::parse(&bytes[slot * page_size..(slot + 1) * page_size]).ok())
    .max_by_key(|header| header.change_counter)
    .expect("valid header slot");
  assert!(header.snapshot_page_count > 0, "database has no snapshot");
  let offset = header.snapshot_start_page as usize * page_size;
  (read_u32(&bytes, offset + 4), read_u32(&bytes, offset + 8))
}

/// `audit_snapshot_v4.gds` was written by the v4 writer (commit 000e319):
/// nodes 1 ("alice"), 2 ("bob"), 5 (no key); edges 1 -KNOWS-> 2 (weight 0.5)
/// and 2 -KNOWS-> 5; label Person on 1 and 2.
#[test]
fn legacy_v4_snapshot_fixture_still_loads() {
  let snapshot =
    SnapshotData::load(fixture_path("audit_snapshot_v4.gds")).expect("load v4 fixture");
  assert_eq!(snapshot.header.version, 4);
  let report = check_snapshot(&snapshot);
  assert!(report.valid, "check_snapshot: {:?}", report.errors);

  assert_eq!(snapshot.num_nodes(), 3);
  assert_eq!(snapshot.num_edges(), 2);
  assert_eq!(snapshot.max_node_id(), 5);
  for (node_id, phys) in [(1, Some(0)), (2, Some(1)), (5, Some(2))] {
    assert_eq!(snapshot.phys_node(node_id), phys, "phys_node({node_id})");
    assert_eq!(snapshot.node_id(phys.expect("present")), Some(node_id));
  }
  for missing in [0, 3, 4, 6, 1000] {
    assert_eq!(snapshot.phys_node(missing), None, "phys_node({missing})");
  }
  assert_eq!(snapshot.lookup_by_key("alice"), Some(1));
  assert_eq!(snapshot.lookup_by_key("bob"), Some(2));
  assert_eq!(
    snapshot.node_prop(0, 1),
    Some(PropValue::String("Alice".to_string()))
  );
  assert_eq!(snapshot.node_prop(0, 2), Some(PropValue::I64(30)));
  assert_eq!(snapshot.node_labels(1), Some(vec![1]));
  assert_eq!(snapshot.iter_out_edges(0).collect::<Vec<_>>(), vec![(1, 1)]);
  assert_eq!(snapshot.iter_out_edges(1).collect::<Vec<_>>(), vec![(2, 1)]);
  let edge = snapshot.find_edge_index(0, 1, 1).expect("edge 1 -> 2");
  assert_eq!(
    snapshot
      .edge_props(edge)
      .and_then(|props| props.get(&3).cloned()),
    Some(PropValue::F64(0.5))
  );
  assert_eq!(snapshot.etype_name(1), Some("KNOWS"));
  assert_eq!(snapshot.label_name(1), Some("Person"));
}

/// `audit_snapshot_v4.kitedb` was written at commit 000e319 (64 KiB WAL):
/// a v4 snapshot holding nodes 1 ("alice"), 2 ("bob"), 5 and edges
/// 1 -KNOWS-> 2 (weight 0.5), 2 -KNOWS-> 5, plus a committed WAL-only tail with
/// node 7 ("carol") and edge 7 -KNOWS-> 1.
#[test]
fn legacy_v4_database_opens_and_upgrades_on_next_checkpoint() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("legacy-v4.kitedb");
  std::fs::copy(fixture_path("audit_snapshot_v4.kitedb"), &path).expect("copy fixture");
  assert_eq!(active_snapshot_versions(&path).0, 4, "fixture is not v4");

  let assert_legacy_graph = |db: &SingleFileDB, stage: &str| {
    let name = db.propkey_id("name").expect("propkey name");
    let weight = db.propkey_id("weight").expect("propkey weight");
    let knows = db.etype_id("KNOWS").expect("etype KNOWS");
    let mut nodes = db.list_nodes();
    nodes.sort_unstable();
    assert_eq!(nodes, vec![1, 2, 5, 7], "{stage}: nodes");
    assert_eq!(db.node_by_key("alice"), Some(1), "{stage}: alice");
    assert_eq!(db.node_by_key("carol"), Some(7), "{stage}: carol");
    assert_eq!(
      db.node_prop(2, name),
      Some(PropValue::String("Bob".to_string())),
      "{stage}: bob name"
    );
    assert_eq!(
      db.node_prop(7, name),
      Some(PropValue::String("Carol".to_string())),
      "{stage}: carol name"
    );
    assert_eq!(db.out_edges(2), vec![(knows, 5)], "{stage}: out(2)");
    assert_eq!(db.out_edges(7), vec![(knows, 1)], "{stage}: out(7)");
    assert_eq!(
      db.edge_prop(1, knows, 2, weight),
      Some(PropValue::F64(0.5)),
      "{stage}: edge weight"
    );
  };

  let db = open_single_file(&path, options()).expect("open legacy v4 database");
  assert_legacy_graph(&db, "legacy open");
  db.checkpoint().expect("checkpoint legacy database");
  assert_legacy_graph(&db, "after checkpoint");
  close_single_file(db).expect("close");

  let (version, min_reader) = active_snapshot_versions(&path);
  let db = open_single_file(&path, options()).expect("reopen upgraded database");
  assert_legacy_graph(&db, "after upgrade reopen");
  close_single_file(db).expect("close reopened");

  // v4 cannot represent sparse node IDs or sections >= 4 GiB. The next
  // checkpoint must write the new format, and old readers must refuse it.
  assert!(
    version > 4,
    "checkpoint still writes snapshot version {version}"
  );
  assert!(
    min_reader > 4,
    "upgraded snapshot declares min_reader_version {min_reader}; v4 readers would misread it"
  );
}
