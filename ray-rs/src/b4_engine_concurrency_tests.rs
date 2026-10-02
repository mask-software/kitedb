//! raydb-b4 `engine-concurrency` lane: repros that need private access.
//! Public-API repros and guards are in `tests/b4_engine_concurrency.rs`.

use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use tempfile::tempdir;

use crate::core::single_file::{open_single_file, SingleFileDB, SingleFileOpenOptions, SyncMode};
use crate::error::KiteError;
use crate::types::{NodeId, PropKeyId};
use crate::vector::store::vector_store_has;

fn options(mvcc: bool) -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .mvcc(mvcc)
    // GC runs often while the tests run (close does not wait for it).
    .mvcc_gc_interval_ms(10)
    .sync_mode(SyncMode::Normal)
    .auto_checkpoint(false)
}

// ============================================================================
// F4: MVCC begin and next_tx_id
// ============================================================================

/// MVCC begin takes its txid from the transaction manager, then plain-stores
/// `txid + 1` into `next_tx_id` after releasing that lock. Two concurrent
/// begins can store out of order, leaving `next_tx_id` at or below a txid
/// already handed out; a commit persists it in the header, and after reopen
/// that txid is issued again (two transactions' WAL records then share one
/// txid). The race window is a few instructions wide, so a stress test
/// rarely hits it (0 in 1500 rounds of 64 concurrent begins); this
/// deterministic form puts `next_tx_id` ahead of the manager first, the state
/// a later concurrent begin leaves, and checks that begin never lowers it.
#[test]
fn f4_mvcc_begin_never_moves_next_tx_id_backwards() {
  let dir = tempdir().expect("tempdir");
  let db = open_single_file(dir.path().join("f4.kitedb"), options(true)).expect("open");
  for _ in 0..5 {
    db.alloc_tx_id();
  }
  let ahead = db.next_tx_id.load(Ordering::SeqCst);
  let txid = db.begin(true).expect("begin");
  let after = db.next_tx_id.load(Ordering::SeqCst);
  db.commit().expect("commit");
  assert!(
    after >= ahead && after > txid,
    "MVCC begin (txid {txid}) moved next_tx_id backwards from {ahead} to {after}"
  );
}

// ============================================================================
// F5: reads and the global transaction map
// ============================================================================

/// Every read used to look up the calling thread's transaction in one
/// process-wide map (keyed by thread id) under its mutex, so all reads
/// serialized on that lock and waited whenever a thread inside begin, commit
/// or rollback held it. Transactions now live in thread-local entries: a read
/// outside a transaction takes no lock of the transaction bookkeeping. Here
/// another thread holds a write transaction open, and this test holds every
/// bookkeeping lock begin, commit and checkpoint take (the open set, the group
/// commit queue, the checkpoint state); reads still complete.
#[test]
fn f5_reads_outside_a_transaction_take_no_transaction_bookkeeping_lock() {
  let dir = tempdir().expect("tempdir");
  let db =
    Arc::new(open_single_file(dir.path().join("f5-map.kitedb"), options(false)).expect("open"));
  db.begin(false).expect("begin");
  let knows = db.define_etype("knows").expect("etype");
  let rank = db.define_propkey("rank").expect("propkey");
  let a = db.create_node(Some("a")).expect("a");
  let b = db.create_node(Some("b")).expect("b");
  db.add_edge(a, knows, b).expect("edge");
  db.set_node_prop(a, rank, crate::types::PropValue::I64(1))
    .expect("prop");
  db.commit().expect("commit");

  let (open_tx, open_rx) = mpsc::channel();
  let (done_tx, done_rx) = mpsc::channel::<()>();
  let writer = {
    let db = Arc::clone(&db);
    thread::spawn(move || {
      db.begin(false).expect("writer begin");
      db.create_node(Some("pending")).expect("pending");
      open_tx.send(()).expect("open");
      done_rx.recv().expect("done");
      db.rollback().expect("rollback");
    })
  };
  open_rx.recv().expect("writer open");

  let held = (
    db.open_write_txids.lock(),
    db.commit_queue.state.lock(),
    db.checkpoint_state.lock(),
  );
  let (read_tx, read_rx) = mpsc::channel();
  let reader = {
    let db = Arc::clone(&db);
    thread::spawn(move || {
      assert!(db.node_exists(a));
      assert_eq!(db.out_edges(a), vec![(knows, b)]);
      assert!(db.node_prop(a, rank).is_some());
      assert_eq!(db.node_by_key("b"), Some(b));
      assert_eq!(db.node_by_key("pending"), None);
      assert!(!db.has_transaction());
      let _ = read_tx.send(());
    })
  };
  let finished = read_rx.recv_timeout(Duration::from_secs(2)).is_ok();
  drop(held);
  reader.join().expect("reader");
  done_tx.send(()).expect("release writer");
  writer.join().expect("writer");
  assert!(
    finished,
    "reads outside any transaction waited for a transaction bookkeeping lock"
  );
}

// ============================================================================
// F2: a vector committed for a concurrently deleted snapshot node
// ============================================================================

/// Whether the live store (not the delta-filtered reads) holds a vector for
/// `node_id`.
fn live_store_has(db: &SingleFileDB, prop_key_id: PropKeyId, node_id: NodeId) -> bool {
  db.materialize_all_vector_stores().expect("materialize");
  db.vector_stores
    .read()
    .get(&prop_key_id)
    .is_some_and(|store| vector_store_has(store, node_id))
}

/// T1 sets a vector on snapshot node x; the main thread deletes x (its vector
/// cleanup cannot see T1's pending vector) and commits; T1 commits. Reads
/// hide the vector behind x's tombstone, but it sits in the live store (ANN
/// search returns it) and the next checkpoint writes it into the snapshot.
/// Non-MVCC: serialization keeps the delete out until T1 commits, and the
/// delete then removes the vector. MVCC: the two must conflict.
#[test]
fn f2_vector_on_a_concurrently_deleted_snapshot_node_never_reaches_the_live_store() {
  for mvcc in [false, true] {
    let dir = tempdir().expect("tempdir");
    let db = Arc::new(
      open_single_file(dir.path().join("f2-vector-snapshot.kitedb"), options(mvcc)).expect("open"),
    );
    db.begin(false).expect("begin");
    let embedding = db.define_propkey("embedding").expect("propkey");
    let other = db.create_node(Some("other")).expect("other");
    db.set_node_vector(other, embedding, &[1.0, 0.0, 0.0])
      .expect("vector");
    let x = db.create_node(Some("x")).expect("x");
    db.commit().expect("commit");
    db.checkpoint().expect("checkpoint");

    let (staged_tx, staged_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let first = {
      let db = Arc::clone(&db);
      thread::spawn(move || {
        db.begin(false).expect("begin");
        db.set_node_vector(x, embedding, &[0.0, 1.0, 0.0])
          .expect("set vector (x exists now)");
        staged_tx.send(()).expect("staged");
        go_rx.recv().expect("go");
        db.commit()
      })
    };
    staged_rx.recv().expect("staged");
    let (deleted_tx, deleted_rx) = mpsc::channel();
    let second = {
      let db = Arc::clone(&db);
      thread::spawn(move || {
        db.begin(false).expect("begin");
        db.delete_node(x).expect("delete");
        let result = db.commit();
        let _ = deleted_tx.send(());
        result
      })
    };
    let _ = deleted_rx.recv_timeout(Duration::from_millis(500));
    go_tx.send(()).expect("go");
    let first_result = first.join().expect("first");
    second.join().expect("second").expect("delete commit");

    assert!(!db.node_exists(x), "mvcc: {mvcc}: x survived its delete");
    assert!(
      !live_store_has(&db, embedding, x),
      "mvcc: {mvcc}: the live vector store holds a vector for deleted node x (vector commit: \
       {first_result:?})"
    );
    if mvcc {
      assert!(
        matches!(first_result, Err(KiteError::Conflict { .. })),
        "MVCC: the vector set on the deleted node committed: {first_result:?}"
      );
    }
  }
}

// ============================================================================
// F1: no partial state from the writer lock on failure paths
// ============================================================================

/// A begin refused because the WAL is full must not leave a writer slot held:
/// the next begin on another thread succeeds once room is made. (Guard for the
/// writer lock's failure paths; passes today.)
#[test]
fn f1_refused_begin_leaves_no_writer_held_guard() {
  let dir = tempdir().expect("tempdir");
  let db = Arc::new(
    open_single_file(
      dir.path().join("f1-refused-begin.kitedb"),
      options(false).wal_size(64 * 1024),
    )
    .expect("open"),
  );
  // Fill the WAL until the begin, a write, or a commit is refused.
  for index in 0..10_000 {
    match db.begin(false) {
      Ok(_) => {}
      Err(KiteError::WalBufferFull) => break,
      Err(error) => panic!("begin #{index}: {error}"),
    }
    let written = db
      .create_node(Some(&format!("n{index}")))
      .and_then(|_| db.commit());
    match written {
      Ok(()) => {}
      Err(KiteError::WalBufferFull) => {
        let _ = db.rollback();
        break;
      }
      Err(error) => panic!("write #{index}: {error}"),
    }
  }
  db.checkpoint().expect("checkpoint");
  let other = Arc::clone(&db);
  let (done_tx, done_rx) = mpsc::channel();
  thread::spawn(move || {
    other.begin(false).expect("begin after refusal");
    other.create_node(Some("after")).expect("create");
    other.commit().expect("commit");
    let _ = done_tx.send(());
  });
  assert!(
    done_rx.recv_timeout(Duration::from_secs(5)).is_ok(),
    "a writer on another thread could not begin after a refused begin"
  );
  assert!(db.node_by_key("after").is_some());
}
