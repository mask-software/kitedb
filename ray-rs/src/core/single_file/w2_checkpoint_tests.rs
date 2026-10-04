//! Wave-2 checkpoint reproductions (K1-K5). Each test fails until its finding
//! is fixed, except the guards marked as such, which pass on this base and
//! keep the behavior from regressing.
//!
//! Included from checkpoint.rs, so its test hooks are in scope. The public-API
//! repros live in `tests/w2_checkpoint.rs`.
use super::*;
use crate::constants::MAGIC_SNAPSHOT;
use crate::core::single_file::{open_single_file, SingleFileOpenOptions, SyncMode};
use crate::replication::primary::default_replication_sidecar_path;
use crate::replication::types::ReplicationRole;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};
use tempfile::tempdir;

fn options() -> SingleFileOpenOptions {
  SingleFileOpenOptions::new().auto_checkpoint(false)
}

fn commit_nodes(db: &SingleFileDB, prefix: &str, count: usize) {
  db.begin(false).expect("begin");
  for index in 0..count {
    db.create_node(Some(&format!("{prefix}-{index}")))
      .expect("create node");
  }
  db.commit().expect("commit");
}

fn missing_nodes(db: &SingleFileDB, prefix: &str, count: usize) -> Vec<String> {
  (0..count)
    .map(|index| format!("{prefix}-{index}"))
    .filter(|key| db.node_by_key(key).is_none())
    .collect()
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
  let deadline = Instant::now() + Duration::from_secs(20);
  while !cond() {
    assert!(Instant::now() < deadline, "timed out waiting for {what}");
    std::thread::sleep(Duration::from_millis(1));
  }
}

fn run_checkpoint(db: &SingleFileDB, background: bool) -> Result<()> {
  if background {
    db.background_checkpoint()
  } else {
    db.checkpoint()
  }
}

fn kind(background: bool) -> &'static str {
  if background {
    "background"
  } else {
    "blocking"
  }
}

// ============================================================================
// K1: the new snapshot must be validated before the header flip
// ============================================================================

fn read_u32_at(path: &std::path::Path, offset: u64) -> u32 {
  let mut file = std::fs::File::open(path).expect("open db file");
  file.seek(SeekFrom::Start(offset)).expect("seek");
  let mut bytes = [0u8; 4];
  file.read_exact(&mut bytes).expect("read");
  u32::from_le_bytes(bytes)
}

fn flip_byte(path: &std::path::Path, offset: u64) {
  let mut file = std::fs::OpenOptions::new()
    .read(true)
    .write(true)
    .open(path)
    .expect("open db file");
  file.seek(SeekFrom::Start(offset)).expect("seek");
  let mut byte = [0u8; 1];
  file.read_exact(&mut byte).expect("read");
  file.seek(SeekFrom::Start(offset)).expect("seek");
  file.write_all(&[byte[0] ^ 0xFF]).expect("write");
}

/// The snapshot a checkpoint wrote and synced is corrupted on disk (bit rot,
/// a torn write, a stray writer) before it is installed. The checkpoint must
/// refuse it: the header keeps naming the old snapshot and WAL, so the
/// database stays readable and reopens with every commit.
fn corrupted_new_snapshot_is_refused(background: bool) {
  let _serial = checkpoint_test_serial();
  let dir = tempdir().expect("tempdir");
  let db_path = dir
    .path()
    .join(format!("k1-corrupt-{}.kitedb", kind(background)));
  let db = Arc::new(open_single_file(&db_path, options()).expect("open"));
  commit_nodes(&db, "k1", 64);
  let header = db.header.read().clone();
  let page_size = header.page_size as u64;

  let durable = Arc::new(Barrier::new(2));
  let reload = Arc::new(Barrier::new(2));
  set_checkpoint_test_barrier(&db, CheckpointPhase::SnapshotDurable, Arc::clone(&durable));
  set_checkpoint_test_barrier(&db, CheckpointPhase::SnapshotReload, Arc::clone(&reload));
  let checkpoint_db = Arc::clone(&db);
  let checkpoint = std::thread::spawn(move || run_checkpoint(&checkpoint_db, background));

  // Written and synced; no header names it yet.
  durable.wait();
  let snapshot_offset = checkpoint_test_snapshot_page(&db).expect("a snapshot written") * page_size;
  assert_eq!(
    read_u32_at(&db_path, snapshot_offset),
    MAGIC_SNAPSHOT,
    "test setup: the new snapshot must start at its first page"
  );
  // num_edges: covered by the footer CRC.
  flip_byte(&db_path, snapshot_offset + 40);
  reload.wait();
  let result = checkpoint.join().expect("checkpoint thread");

  assert!(
    result.is_err(),
    "{} checkpoint installed a snapshot whose bytes fail validation",
    kind(background)
  );
  assert_eq!(
    db.header.read().active_snapshot_gen,
    header.active_snapshot_gen,
    "the header must keep naming the old snapshot"
  );
  assert_eq!(missing_nodes(&db, "k1", 64), Vec::<String>::new());
  commit_nodes(&db, "k1-after", 1);
  drop(db);

  let reopened = open_single_file(&db_path, options()).expect("reopen after refused install");
  assert_eq!(missing_nodes(&reopened, "k1", 64), Vec::<String>::new());
  assert_eq!(
    missing_nodes(&reopened, "k1-after", 1),
    Vec::<String>::new()
  );
  reopened.checkpoint().expect("next checkpoint");
  assert_eq!(missing_nodes(&reopened, "k1", 64), Vec::<String>::new());
}

/// Guard: passes on 39fefea (`load_unnamed_snapshot` parses the new
/// snapshot, footer CRC included, before `install_snapshot`).
#[test]
fn k1_corrupted_new_snapshot_is_refused_by_blocking_checkpoint() {
  corrupted_new_snapshot_is_refused(false);
}

/// Guard: passes on 39fefea (`build_and_write_snapshot` loads the snapshot
/// before `complete_background_checkpoint` installs it).
#[test]
fn k1_corrupted_new_snapshot_is_refused_by_background_checkpoint() {
  corrupted_new_snapshot_is_refused(true);
}

/// A writer bug (here: in-memory store state the reader rejects) produces a
/// snapshot whose footer CRC and section table are fine but whose vector
/// store does not decode. `load_snapshot` keeps vector stores lazy, so the
/// checkpoint installs it; from then on the vector reads as missing, writes
/// to that key fail, and every later checkpoint fails in
/// `materialize_all_vector_stores`, so the WAL can only fill up. The install
/// must be refused.
fn undecodable_vector_store_is_not_installed(background: bool) {
  let _serial = checkpoint_test_serial();
  let dir = tempdir().expect("tempdir");
  let db_path = dir
    .path()
    .join(format!("k1-vector-store-{}.kitedb", kind(background)));
  let db = open_single_file(&db_path, options()).expect("open");
  db.begin(false).expect("begin");
  let node = db.create_node(Some("k1-vec")).expect("create node");
  let embedding = db.define_propkey("embedding").expect("propkey");
  db.set_node_vector(node, embedding, &[0.25, 0.5, 0.75])
    .expect("set vector");
  db.commit().expect("commit");
  let expected = db.node_vector(node, embedding).map(|v| v.to_vec());
  assert!(expected.is_some(), "test setup: vector must be readable");

  // The store serializes faithfully into a manifest the reader rejects
  // ("row_group_size must be nonzero"); the snapshot CRC covers it.
  db.vector_stores
    .write()
    .get_mut(&embedding)
    .expect("vector store")
    .config
    .row_group_size = 0;
  let installed = run_checkpoint(&db, background);
  drop(db);

  let reopened = open_single_file(&db_path, options()).expect("reopen");
  let vector = reopened.node_vector(node, embedding).map(|v| v.to_vec());
  let next_checkpoint = reopened.checkpoint();
  assert!(
    installed.is_err() && vector == expected && next_checkpoint.is_ok(),
    "{} checkpoint installed a snapshot whose vector store does not decode \
     (checkpoint: {installed:?}); after reopen the vector reads {vector:?} (expected \
     {expected:?}) and the next checkpoint returns {next_checkpoint:?}",
    kind(background)
  );
}

#[test]
fn k1_blocking_checkpoint_refuses_snapshot_with_undecodable_vector_store() {
  undecodable_vector_store_is_not_installed(false);
}

#[test]
fn k1_background_checkpoint_refuses_snapshot_with_undecodable_vector_store() {
  undecodable_vector_store_is_not_installed(true);
}

// ============================================================================
// K2: a dangling delta edge must not fail every checkpoint
// ============================================================================

/// An edge in the committed delta whose endpoint exists nowhere (left by
/// older versions or by racing non-MVCC writers; see
/// `tests/w2_checkpoint.rs`). The snapshot writer rejects it, so every
/// checkpoint fails and the WAL fills. The checkpoint must drop the edge and
/// its props instead.
fn dangling_delta_edges_are_dropped(background: bool) {
  let _serial = checkpoint_test_serial();
  let dir = tempdir().expect("tempdir");
  let db_path = dir
    .path()
    .join(format!("k2-dangling-{}.kitedb", kind(background)));
  let db = open_single_file(&db_path, options()).expect("open");
  db.begin(false).expect("begin");
  let a = db.create_node(Some("k2-a")).expect("node a");
  let b = db.create_node(Some("k2-b")).expect("node b");
  let knows = db.define_etype("knows").expect("etype");
  let weight = db.define_propkey("weight").expect("propkey");
  db.add_edge(a, knows, b).expect("edge a->b");
  db.commit().expect("commit");

  let missing: NodeId = 1_000_000;
  {
    use crate::core::wal::record::{build_add_edge_payload, build_set_edge_prop_payload};
    commit_raw(
      &db,
      &[
        (
          WalRecordType::AddEdge,
          build_add_edge_payload(a, knows, missing),
        ),
        (
          WalRecordType::SetEdgeProp,
          build_set_edge_prop_payload(a, knows, missing, weight, &PropValue::I64(1)),
        ),
        (
          WalRecordType::AddEdge,
          build_add_edge_payload(missing, knows, b),
        ),
      ],
      |delta| {
        delta.add_edge(a, knows, missing);
        delta.set_edge_prop(a, knows, missing, weight, PropValue::I64(1));
        delta.add_edge(missing, knows, b);
      },
    );
  }

  let result = run_checkpoint(&db, background);
  assert!(
    result.is_ok(),
    "{} checkpoint failed on a dangling delta edge (every later checkpoint fails the \
     same way): {result:?}",
    kind(background)
  );
  assert_eq!(db.out_edges(a), vec![(knows, b)]);
  drop(db);

  let reopened = open_single_file(&db_path, options()).expect("reopen");
  assert_eq!(reopened.out_edges(a), vec![(knows, b)]);
  assert_eq!(reopened.in_edges(b), vec![(knows, a)]);
  assert!(!reopened.node_exists(missing));
  reopened.checkpoint().expect("next checkpoint");
}

#[test]
fn k2_blocking_checkpoint_drops_dangling_delta_edges() {
  dangling_delta_edges_are_dropped(false);
}

#[test]
fn k2_background_checkpoint_drops_dangling_delta_edges() {
  dangling_delta_edges_are_dropped(true);
}

// ============================================================================
// K3: vectors of nodes that do not exist must not reach a snapshot
// ============================================================================

/// Commit a transaction of `records` (type and payload) straight to the WAL,
/// as an older version or a racing writer could have, and apply `apply` to
/// the committed delta as its commit did: the log and the delta agree, so a
/// checkpoint replaying the log sees it as well as one copying the delta.
fn commit_raw(
  db: &SingleFileDB,
  records: &[(WalRecordType, Vec<u8>)],
  apply: impl FnOnce(&mut DeltaState),
) {
  use crate::core::wal::record::{build_begin_payload, build_commit_payload, WalRecord};
  let txid = db.next_tx_id.fetch_add(1, Ordering::SeqCst);
  let mut bytes = WalRecord::new(WalRecordType::Begin, txid, build_begin_payload()).build();
  for (record_type, payload) in records {
    bytes.extend(WalRecord::new(*record_type, txid, payload.clone()).build());
  }
  bytes.extend(WalRecord::new(WalRecordType::Commit, txid, build_commit_payload()).build());
  {
    let mut pager = db.pager.lock();
    let mut wal = db.wal_buffer.lock();
    let mut header = db.header.write();
    wal.write_record_bytes_batch(&bytes).expect("append");
    wal.flush(&mut pager).expect("flush");
    wal.store_in_header(&mut header);
    header.next_tx_id = db.next_tx_id.load(Ordering::SeqCst);
    db.persist_header(&mut pager, &mut header, true)
      .expect("header");
  }
  apply(&mut db.delta.write());
}

/// A node deleted the way older versions did (no vector deletes), so its
/// vector stays in the store. The checkpoint copies the store unfiltered, so
/// the new snapshot (and every one after it) holds a vector for a node it
/// does not contain. Checkpoint must drop it.
fn vectors_of_missing_nodes_are_dropped(background: bool) {
  let _serial = checkpoint_test_serial();
  let dir = tempdir().expect("tempdir");
  let db_path = dir
    .path()
    .join(format!("k3-vectors-{}.kitedb", kind(background)));
  let db = open_single_file(&db_path, options()).expect("open");
  db.begin(false).expect("begin");
  let keep = db.create_node(Some("k3-keep")).expect("node keep");
  let gone = db.create_node(Some("k3-gone")).expect("node gone");
  let embedding = db.define_propkey("embedding").expect("propkey");
  db.set_node_vector(keep, embedding, &[1.0, 0.0, 0.0])
    .expect("vector keep");
  db.set_node_vector(gone, embedding, &[0.0, 1.0, 0.0])
    .expect("vector gone");
  db.commit().expect("commit");
  db.checkpoint().expect("first checkpoint");

  // An older version's delete: the node goes, its vector stays.
  commit_raw(
    &db,
    &[(
      WalRecordType::DeleteNode,
      crate::core::wal::record::build_delete_node_payload(gone),
    )],
    |delta| delta.delete_node(gone),
  );
  run_checkpoint(&db, background).expect("checkpoint");
  db.materialize_all_vector_stores()
    .expect("materialize stores");
  let in_snapshot = db
    .vector_stores
    .read()
    .get(&embedding)
    .is_some_and(|store| crate::vector::store::vector_store_has(store, gone));
  drop(db);

  let reopened = open_single_file(&db_path, options()).expect("reopen");
  assert!(!reopened.node_exists(gone), "test setup: node must be gone");
  assert!(
    reopened.has_node_vector(keep, embedding),
    "a live node's vector must survive"
  );
  assert!(
    !in_snapshot && !reopened.has_node_vector(gone, embedding),
    "{} checkpoint wrote the vector of deleted node {gone} into the snapshot \
     (in new snapshot: {in_snapshot}; after reopen has_node_vector: {})",
    kind(background),
    reopened.has_node_vector(gone, embedding)
  );
}

#[test]
fn k3_blocking_checkpoint_drops_vectors_of_missing_nodes() {
  vectors_of_missing_nodes_are_dropped(false);
}

#[test]
fn k3_background_checkpoint_drops_vectors_of_missing_nodes() {
  vectors_of_missing_nodes_are_dropped(true);
}

// ============================================================================
// K4: the replication sidecar must hold every commit a checkpoint folds in
// ============================================================================

fn copy_dir(src: &std::path::Path, dst: &std::path::Path) {
  std::fs::create_dir_all(dst).expect("create sidecar copy");
  for entry in std::fs::read_dir(src).expect("read sidecar dir") {
    let entry = entry.expect("sidecar entry");
    if entry.file_type().expect("file type").is_file() {
      std::fs::copy(entry.path(), dst.join(entry.file_name())).expect("copy sidecar file");
    }
  }
}

/// Copy the database file and its sidecar as a crash would leave them now.
fn crash_copy(db_path: &std::path::Path, tag: &str) -> std::path::PathBuf {
  let copy = db_path.with_extension(format!("{tag}.crash.kitedb"));
  std::fs::copy(db_path, &copy).expect("copy db file");
  copy_dir(
    &default_replication_sidecar_path(db_path),
    &default_replication_sidecar_path(&copy),
  );
  copy
}

/// A Normal-sync primary buffers sidecar frames in memory. A checkpoint
/// then folds their commits into the snapshot and empties the WAL; a crash
/// before the frames are published (the 100 ms publisher, stopped here to
/// make that window deterministic) leaves commits that no replica can ever
/// receive from the log. Open only detects it and fences the sidecar for a
/// reseed. The checkpoint must publish the frames before the WAL holding
/// their commits is reset.
fn sidecar_holds_commits_folded_by_checkpoint(background: bool) {
  let _serial = checkpoint_test_serial();
  const COMMITS: usize = 5;
  let replication_options = || {
    options()
      .sync_mode(SyncMode::Normal)
      .replication_role(ReplicationRole::Primary)
  };
  let dir = tempdir().expect("tempdir");
  let db_path = dir
    .path()
    .join(format!("k4-sidecar-{}.kitedb", kind(background)));
  let mut db = open_single_file(&db_path, replication_options()).expect("open primary");
  db.primary_replication
    .as_mut()
    .expect("primary replication")
    .stop_publisher_for_testing();

  let mut head = None;
  for index in 0..COMMITS {
    db.begin(false).expect("begin");
    db.create_node(Some(&format!("k4-{index}")))
      .expect("create node");
    head = db.commit_with_token().expect("commit");
  }
  let head = head.expect("primary commit token");
  run_checkpoint(&db, background).expect("checkpoint");
  let crashed = crash_copy(&db_path, "k4");
  drop(db);

  let reopened = open_single_file(&crashed, replication_options()).expect("reopen crash copy");
  assert_eq!(
    missing_nodes(&reopened, "k4", COMMITS),
    Vec::<String>::new(),
    "test setup: local commits must survive the crash"
  );
  let status = reopened
    .primary_replication_status()
    .expect("primary status");
  assert!(
    !status.sidecar_needs_repair && status.head_log_index >= head.log_index,
    "{} checkpoint emptied the WAL while {COMMITS} commits were only buffered for the \
     sidecar: after a crash the sidecar has head_log_index={} (commits reached {}), \
     sidecar_needs_repair={} ({:?})",
    kind(background),
    status.head_log_index,
    head.log_index,
    status.sidecar_needs_repair,
    status.last_replication_error
  );
}

#[test]
fn k4_blocking_checkpoint_publishes_buffered_sidecar_frames_first() {
  sidecar_holds_commits_folded_by_checkpoint(false);
}

#[test]
fn k4_background_checkpoint_publishes_buffered_sidecar_frames_first() {
  sidecar_holds_commits_folded_by_checkpoint(true);
}

/// Restores a directory's permissions when dropped, also on panic.
#[cfg(unix)]
struct RestoreMode<'a>(&'a std::path::Path);

#[cfg(unix)]
impl Drop for RestoreMode<'_> {
  fn drop(&mut self) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(self.0, std::fs::Permissions::from_mode(0o755));
  }
}

/// The publish before the install fails (here the sidecar directory turned
/// read-only, so its manifest cannot be written). The sidecar is fenced for
/// repair, as after a failed append, and the checkpoint still installs:
/// failing it would leave the WAL to fill. The fence is not silent: the
/// status reports it, and a crash copy reopens fenced too, since the marker
/// of buffered frames could not be removed.
#[cfg(unix)]
#[test]
fn k4_failed_sidecar_publish_fences_the_sidecar_and_the_checkpoint_proceeds() {
  use std::os::unix::fs::PermissionsExt;
  let _serial = checkpoint_test_serial();
  let replication_options = || {
    options()
      .sync_mode(SyncMode::Normal)
      .replication_role(ReplicationRole::Primary)
  };
  let dir = tempdir().expect("tempdir");
  let db_path = dir.path().join("k4-publish-fails.kitedb");
  let mut db = open_single_file(&db_path, replication_options()).expect("open primary");
  db.primary_replication
    .as_mut()
    .expect("primary replication")
    .stop_publisher_for_testing();
  commit_nodes(&db, "k4-fenced", 3);

  let sidecar = default_replication_sidecar_path(&db_path);
  let _restore = RestoreMode(&sidecar);
  std::fs::set_permissions(&sidecar, std::fs::Permissions::from_mode(0o555))
    .expect("make sidecar read-only");
  if std::fs::File::create(sidecar.join("probe")).is_ok() {
    eprintln!("skipped: permissions are not enforced here (running as root?)");
    return;
  }

  let checkpoint = db.checkpoint();
  let status = db.primary_replication_status().expect("primary status");
  assert!(
    checkpoint.is_ok() && status.sidecar_needs_repair,
    "a failed sidecar publish must fence the sidecar, not fail the checkpoint: checkpoint \
     {checkpoint:?}, sidecar_needs_repair={}",
    status.sidecar_needs_repair
  );
  assert_eq!(
    db.wal_stats().primary_head,
    0,
    "the install must reset the WAL"
  );
  assert_eq!(missing_nodes(&db, "k4-fenced", 3), Vec::<String>::new());
  let crashed = crash_copy(&db_path, "k4-fenced");
  drop(db);

  let reopened = open_single_file(&crashed, replication_options()).expect("reopen crash copy");
  assert_eq!(
    missing_nodes(&reopened, "k4-fenced", 3),
    Vec::<String>::new()
  );
  assert!(
    reopened
      .primary_replication_status()
      .expect("primary status")
      .sidecar_needs_repair,
    "the crash copy must reopen fenced"
  );
}

// ============================================================================
// K5: blocking checkpoint / optimize vs a running background checkpoint
// ============================================================================

/// Guard: passes on 39fefea (`exclusive_checkpoint_gate` waits for a running
/// background checkpoint). A blocking checkpoint or optimize between a
/// background cut and its install would empty the log holding the post-cut
/// commits, and the background install would then replace its snapshot.
#[test]
fn k5_blocking_checkpoint_and_optimize_wait_for_a_running_background_checkpoint() {
  let _serial = checkpoint_test_serial();
  let dir = tempdir().expect("tempdir");
  let db_path = dir.path().join("k5-exclusion.kitedb");
  let db = Arc::new(open_single_file(&db_path, options()).expect("open"));
  commit_nodes(&db, "k5-before-cut", 8);

  let parked = Arc::new(Barrier::new(2));
  set_checkpoint_test_barrier(&db, CheckpointPhase::SnapshotDurable, Arc::clone(&parked));
  let background_db = Arc::clone(&db);
  let background = std::thread::spawn(move || background_db.background_checkpoint());
  wait_until("background cut", || checkpoint_test_cuts(&db) > 0);
  commit_nodes(&db, "k5-after-cut", 8);

  let blocking_db = Arc::clone(&db);
  let blocking = std::thread::spawn(move || blocking_db.checkpoint());
  let optimize_db = Arc::clone(&db);
  let optimize = std::thread::spawn(move || optimize_db.optimize_single_file(None));
  std::thread::sleep(Duration::from_millis(200));
  assert!(
    !blocking.is_finished() && !optimize.is_finished(),
    "a blocking checkpoint or optimize ran inside a background checkpoint"
  );
  parked.wait();
  background
    .join()
    .expect("background thread")
    .expect("background checkpoint");
  blocking
    .join()
    .expect("blocking thread")
    .expect("blocking checkpoint");
  optimize.join().expect("optimize thread").expect("optimize");
  commit_nodes(&db, "k5-after-all", 8);

  for prefix in ["k5-before-cut", "k5-after-cut", "k5-after-all"] {
    assert_eq!(missing_nodes(&db, prefix, 8), Vec::<String>::new(), "live");
  }
  let check = db.check();
  assert!(
    check.valid,
    "check after racing checkpoints: {:?}",
    check.errors
  );
  drop(db);
  let reopened = open_single_file(&db_path, options()).expect("reopen");
  for prefix in ["k5-before-cut", "k5-after-cut", "k5-after-all"] {
    assert_eq!(
      missing_nodes(&reopened, prefix, 8),
      Vec::<String>::new(),
      "after reopen"
    );
  }
}
