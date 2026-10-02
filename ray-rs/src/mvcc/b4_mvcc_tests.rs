//! Regression tests for the raydb-b4 `mvcc` lane: committed-write pruning cost (finding 1),
//! committed transactions keeping their read/write sets (finding 2), and close blocking on
//! the GC interval (finding 3).

use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tempfile::tempdir;

use crate::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions, SyncMode,
};
use crate::error::KiteError;
use crate::mvcc::gc::start_background_gc;
use crate::mvcc::tx_manager::MAX_COMMITTED_WRITES;
use crate::mvcc::{
  ConflictDetector, GarbageCollector, GcConfig, MvccManager, TxManager, VersionChainManager,
};
use crate::types::{MvccTxStatus, NodeId, PropKeyId, PropValue, TxKey};

fn node_key(i: usize) -> TxKey {
  TxKey::Node(i as NodeId)
}

fn prop_key(i: usize) -> TxKey {
  TxKey::NodeProp {
    node_id: i as NodeId,
    key_id: 1,
  }
}

/// Committed transactions still held by the manager, and the read/write-set
/// entries they keep alive.
fn retained_committed(tx_mgr: &TxManager) -> (usize, usize) {
  tx_mgr
    .all_txs()
    .filter(|(_, tx)| tx.status != MvccTxStatus::Active)
    .fold((0, 0), |(txs, keys), (_, tx)| {
      (txs + 1, keys + tx.read_set.len() + tx.write_set.len())
    })
}

fn mvcc_options() -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .mvcc(true)
    .auto_checkpoint(false)
    .background_checkpoint(false)
    .sync_mode(SyncMode::Off)
}

/// Holds a read-only transaction open on its own thread until dropped.
struct LongLivedReader {
  release: Option<mpsc::Sender<()>>,
  handle: Option<thread::JoinHandle<()>>,
}

impl LongLivedReader {
  fn open(db: &Arc<SingleFileDB>) -> Self {
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let db = Arc::clone(db);
    let handle = thread::spawn(move || {
      db.begin(true).expect("begin long-lived reader");
      ready_tx.send(()).expect("signal reader ready");
      let _ = release_rx.recv();
      db.commit().expect("finish long-lived reader");
    });
    ready_rx.recv().expect("reader began");
    Self {
      release: Some(release_tx),
      handle: Some(handle),
    }
  }
}

impl Drop for LongLivedReader {
  fn drop(&mut self) {
    drop(self.release.take());
    if let Some(handle) = self.handle.take() {
      let _ = handle.join();
    }
  }
}

fn seed_nodes(db: &SingleFileDB, count: usize) -> (Vec<NodeId>, PropKeyId) {
  db.begin(false).expect("begin seed");
  let prop = db.define_propkey("counter").expect("define propkey");
  let nodes = (0..count)
    .map(|i| {
      let node = db.create_node(Some(&format!("n{i}"))).expect("create node");
      db.set_node_prop(node, prop, PropValue::I64(0))
        .expect("seed prop");
      node
    })
    .collect();
  db.commit().expect("commit seed");
  (nodes, prop)
}

// ============================================================================
// Finding 1: committed-write pruning with nothing prunable
// ============================================================================

/// With a long-lived reader nothing in `committed_writes` is older than the
/// oldest snapshot, so a prune can remove nothing. Each commit over the limit
/// must not pay for the whole map again.
#[test]
fn prune_does_not_rescan_committed_writes_when_nothing_is_prunable() {
  const COMMITS: usize = 200;
  let mut tx_mgr = TxManager::new();
  let (_reader, _) = tx_mgr.begin_tx();

  let (bulk, _) = tx_mgr.begin_tx();
  for i in 0..=MAX_COMMITTED_WRITES {
    tx_mgr.record_write(bulk, node_key(i));
  }
  tx_mgr.commit_tx(bulk).expect("commit bulk");

  let work_before = tx_mgr.prune_work;
  let started = Instant::now();
  for i in 0..COMMITS {
    let (txid, _) = tx_mgr.begin_tx();
    tx_mgr.record_write(txid, node_key(MAX_COMMITTED_WRITES + 1 + i));
    tx_mgr.commit_tx(txid).expect("commit");
  }
  let elapsed = started.elapsed();
  let work = tx_mgr.prune_work - work_before;

  assert_eq!(
    tx_mgr.committed_writes_stats().size,
    MAX_COMMITTED_WRITES + 1 + COMMITS,
    "the reader's snapshot needs every entry, so none may be pruned"
  );
  assert!(
    work <= 4 * COMMITS as u64,
    "{COMMITS} commits with nothing prunable visited {work} committed-write entries \
     ({} per commit, {:?} per commit)",
    work / COMMITS as u64,
    elapsed / COMMITS as u32
  );
}

/// Pruning still drops entries no snapshot can conflict with once the reader
/// is gone, and keeps the ones a live snapshot can.
#[test]
fn prune_drops_old_entries_once_the_reader_finishes() {
  let mut tx_mgr = TxManager::new();
  let detector = ConflictDetector::new();
  let (reader, _) = tx_mgr.begin_tx();

  let (bulk, _) = tx_mgr.begin_tx();
  for i in 0..=MAX_COMMITTED_WRITES {
    tx_mgr.record_write(bulk, node_key(i));
  }
  tx_mgr.commit_tx(bulk).expect("commit bulk");
  assert_eq!(
    tx_mgr.committed_writes_stats().size,
    MAX_COMMITTED_WRITES + 1
  );

  // The reader read a key the bulk commit wrote: it must still conflict.
  tx_mgr.record_read(reader, node_key(7));
  assert!(detector.validate_commit(&tx_mgr, reader).is_err());
  tx_mgr.abort_tx(reader);

  let (late, _) = tx_mgr.begin_tx();
  tx_mgr.record_write(late, node_key(MAX_COMMITTED_WRITES + 1));
  tx_mgr.commit_tx(late).expect("commit late");

  let stats = tx_mgr.committed_writes_stats();
  assert!(
    stats.size <= MAX_COMMITTED_WRITES / 2,
    "nothing is active, so the map must shrink to the prune target, got {}",
    stats.size
  );
  assert!(stats.pruned >= MAX_COMMITTED_WRITES / 2);
}

// ============================================================================
// Finding 2: committed transactions keep their read/write sets
// ============================================================================

/// Commits next to a long-lived reader must not leave their transaction
/// records (with full read/write sets) behind, and GC with the default
/// retention must not be what frees them.
#[test]
fn committed_txs_do_not_keep_read_write_sets_while_a_reader_is_open() {
  const COMMITS: usize = 1_000;
  const KEYS_PER_TX: usize = 10;
  let mut tx_mgr = TxManager::new();
  let mut version_chain = VersionChainManager::new();
  let mut gc = GarbageCollector::new();
  let (reader, _) = tx_mgr.begin_tx();

  for c in 0..COMMITS {
    let (txid, _) = tx_mgr.begin_tx();
    for k in 0..KEYS_PER_TX {
      tx_mgr.record_read(txid, node_key(c * KEYS_PER_TX + k));
      tx_mgr.record_write(txid, prop_key(c * KEYS_PER_TX + k));
    }
    tx_mgr.commit_tx(txid).expect("commit");
  }
  let _ = gc.run_gc(&mut tx_mgr, &mut version_chain);

  assert_eq!(
    retained_committed(&tx_mgr),
    (0, 0),
    "committed transactions (and their read+write-set keys) retained next to one open reader"
  );
  assert!(tx_mgr.is_active(reader));
  assert_eq!(tx_mgr.all_txs().count(), 1);
}

/// The same through the database: a long-lived reader on another thread while
/// this thread commits property writes.
#[test]
fn db_commits_next_to_a_long_lived_reader_do_not_retain_tx_records() {
  const COMMITS: usize = 300;
  let dir = tempdir().expect("tempdir");
  let db =
    Arc::new(open_single_file(dir.path().join("retain.kitedb"), mvcc_options()).expect("open"));
  let (nodes, prop) = seed_nodes(&db, 8);

  let reader = LongLivedReader::open(&db);
  for c in 0..COMMITS {
    db.begin(false).expect("begin");
    for &node in &nodes[..4] {
      db.set_node_prop(node, prop, PropValue::I64(c as i64))
        .expect("set prop");
    }
    db.commit().expect("commit");
  }

  let retained = {
    let mvcc = db.mvcc.as_ref().expect("mvcc enabled");
    let tx_mgr = mvcc.tx_manager.lock();
    retained_committed(&tx_mgr)
  };
  drop(reader);
  assert_eq!(
    retained,
    (0, 0),
    "{COMMITS} commits next to one open reader left (committed tx records, read+write-set keys)"
  );

  let db = Arc::try_unwrap(db).unwrap_or_else(|_| panic!("db still shared"));
  close_single_file(db).expect("close");
}

/// Guard for finding 2: write-write and read-write conflicts are still
/// detected once the first committer's transaction record is gone.
#[test]
fn conflicts_still_detected_after_the_committer_record_is_released() {
  let key = prop_key(42);
  let mut tx_mgr = TxManager::new();
  let detector = ConflictDetector::new();
  let (_reader, _) = tx_mgr.begin_tx();

  let (first, _) = tx_mgr.begin_tx();
  let (writer, _) = tx_mgr.begin_tx();
  let (reader2, _) = tx_mgr.begin_tx();
  tx_mgr.record_write(first, key.clone());
  tx_mgr.record_write(writer, key.clone());
  tx_mgr.record_read(reader2, key.clone());

  detector
    .validate_commit(&tx_mgr, first)
    .expect("first committer has no conflict");
  let first_ts = tx_mgr.commit_tx(first).expect("commit first");

  let err = detector
    .validate_commit(&tx_mgr, writer)
    .expect_err("write-write conflict must be detected");
  assert_eq!(err.conflicting_keys, vec![key.to_string()]);
  assert_eq!(tx_mgr.committed_write_ts(&key, 0), Some(first_ts));
  tx_mgr.abort_tx(writer);

  assert!(
    detector.validate_commit(&tx_mgr, reader2).is_err(),
    "read-write conflict must be detected"
  );
  tx_mgr.abort_tx(reader2);

  // A transaction that began after the commit sees it, so no conflict.
  let (after, _) = tx_mgr.begin_tx();
  tx_mgr.record_write(after, key.clone());
  assert!(detector.validate_commit(&tx_mgr, after).is_ok());
}

/// Guard for finding 2 through the database: two writers of the same property
/// next to a long-lived reader; the second commit must fail with a conflict.
#[test]
fn db_write_write_conflict_detected_next_to_a_long_lived_reader() {
  let dir = tempdir().expect("tempdir");
  let db = Arc::new(open_single_file(dir.path().join("ww.kitedb"), mvcc_options()).expect("open"));
  let (nodes, prop) = seed_nodes(&db, 1);
  let node = nodes[0];
  let reader = LongLivedReader::open(&db);

  db.begin(false).expect("begin first");
  db.set_node_prop(node, prop, PropValue::I64(1))
    .expect("first write");

  let (ready_tx, ready_rx) = mpsc::channel();
  let (go_tx, go_rx) = mpsc::channel::<()>();
  let second = {
    let db = Arc::clone(&db);
    thread::spawn(move || {
      db.begin(false).expect("begin second");
      db.set_node_prop(node, prop, PropValue::I64(2))
        .expect("second write");
      ready_tx.send(()).expect("signal ready");
      go_rx.recv().expect("go");
      db.commit()
    })
  };
  ready_rx.recv().expect("second writer ready");
  db.commit().expect("first commit");
  go_tx.send(()).expect("release second");
  let result = second.join().expect("second writer thread");

  assert!(
    matches!(result, Err(KiteError::Conflict { .. })),
    "second writer must hit a write-write conflict, got {result:?}"
  );
  assert_eq!(db.node_prop(node, prop), Some(PropValue::I64(1)));
  drop(reader);
}

// ============================================================================
// Finding 3: close/drop waits for the GC interval
// ============================================================================

const PROMPT: Duration = Duration::from_secs(1);

#[test]
fn mvcc_manager_stop_does_not_wait_for_the_gc_interval() {
  let mvcc = MvccManager::new(1, 1, GcConfig::default());
  mvcc.start();
  let started = Instant::now();
  mvcc.stop();
  let elapsed = started.elapsed();
  assert!(elapsed < PROMPT, "stop took {elapsed:?}");
}

#[test]
fn close_with_mvcc_returns_promptly() {
  let dir = tempdir().expect("tempdir");
  let db = open_single_file(dir.path().join("close.kitedb"), mvcc_options()).expect("open");
  let started = Instant::now();
  close_single_file(db).expect("close");
  let elapsed = started.elapsed();
  assert!(elapsed < PROMPT, "close took {elapsed:?}");
}

#[test]
fn drop_with_mvcc_returns_promptly() {
  let dir = tempdir().expect("tempdir");
  let db = open_single_file(dir.path().join("drop.kitedb"), mvcc_options()).expect("open");
  let started = Instant::now();
  drop(db);
  let elapsed = started.elapsed();
  assert!(elapsed < PROMPT, "drop took {elapsed:?}");
}

#[test]
fn background_gc_handle_stop_does_not_wait_for_the_interval() {
  let handle = start_background_gc(
    Arc::new(Mutex::new(TxManager::new())),
    Arc::new(Mutex::new(VersionChainManager::new())),
    GcConfig::default(),
  );
  let started = Instant::now();
  handle.stop();
  let elapsed = started.elapsed();
  assert!(elapsed < PROMPT, "stop took {elapsed:?}");
}

/// Guard for finding 3: the GC threads still run on their interval.
#[test]
fn background_gc_still_runs_on_its_interval() {
  let config = GcConfig {
    interval_ms: 5,
    ..GcConfig::default()
  };
  let deadline = Instant::now() + Duration::from_secs(5);

  let mvcc = MvccManager::new(1, 1, config.clone());
  mvcc.start();
  while mvcc.gc.lock().stats().gc_runs < 3 {
    assert!(Instant::now() < deadline, "MvccManager GC stopped running");
    thread::sleep(Duration::from_millis(1));
  }
  mvcc.stop();
  let runs = mvcc.gc.lock().stats().gc_runs;
  thread::sleep(Duration::from_millis(20));
  assert_eq!(mvcc.gc.lock().stats().gc_runs, runs, "GC ran after stop");

  let handle = start_background_gc(
    Arc::new(Mutex::new(TxManager::new())),
    Arc::new(Mutex::new(VersionChainManager::new())),
    config,
  );
  while handle.gc_runs() < 3 {
    assert!(Instant::now() < deadline, "background GC stopped running");
    thread::sleep(Duration::from_millis(1));
  }
  handle.stop();
}
