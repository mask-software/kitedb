//! raydb-b4 `engine-concurrency` lane: public-API repros and guards.
//!
//! - F1: non-MVCC mode serializes write transactions (single writer), without
//!   deadlocking against checkpoints, group commit, bulk load, or maintenance.
//! - F2: an edge or vector added to a node that another transaction deletes
//!   concurrently is never committed dangling.
//! - F3: a zero-pause `background_checkpoint()` loop does not starve blocking
//!   `checkpoint()` and optimize.
//! - F5: `count_nodes` does not materialize every node id.
//! - F6: commits are not blocked for the length of a background checkpoint
//!   (the guard holds the checkpoint at a phase hook, so it is a unit test:
//!   `src/core/single_file/b4_commit_pipeline_checkpoint_tests.rs`).
//! - F7: readers outside a transaction see consistent state while vacuum and
//!   `resize_wal` relocate the snapshot.
//!
//! Tests named `*_guard` pass before and after the fixes; they pin down the
//! behavior the fixes must keep (no deadlock, unaffected modes). Tests that
//! need private access live in `src/b4_engine_concurrency_tests.rs`.

use kitedb::core::single_file::{
  close_single_file, open_single_file, ResizeWalOptions, SingleFileDB, SingleFileOpenOptions,
  SyncMode, VacuumOptions,
};
use kitedb::error::KiteError;
use kitedb::types::{ETypeId, NodeId, PropKeyId, PropValue};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const MIB: usize = 1024 * 1024;

/// How long a second writer gets to finish while the first one is still open.
/// It only bounds the wait when the second writer is (correctly) serialized
/// behind the first; an unserialized writer finishes in a few milliseconds.
const SECOND_WRITER_GRACE: Duration = Duration::from_millis(500);

/// Generous bound for scenarios that must not deadlock.
const DEADLOCK_TIMEOUT: Duration = Duration::from_secs(20);

fn options(mvcc: bool) -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .mvcc(mvcc)
    // GC runs often while the tests run (close does not wait for it).
    .mvcc_gc_interval_ms(10)
    .sync_mode(SyncMode::Normal)
    .auto_checkpoint(false)
    .wal_size(4 * MIB)
}

fn open(path: &Path, options: SingleFileOpenOptions) -> Arc<SingleFileDB> {
  Arc::new(open_single_file(path, options).expect("open"))
}

fn close(db: Arc<SingleFileDB>) {
  let db = Arc::try_unwrap(db)
    .ok()
    .expect("sole owner of the database");
  close_single_file(db).expect("close");
}

/// Run `scenario` on its own thread and fail if it does not finish within
/// `timeout` (the thread is left behind: it is deadlocked).
fn finishes_within<T: Send + 'static>(
  what: &str,
  timeout: Duration,
  scenario: impl FnOnce() -> T + Send + 'static,
) -> T {
  let (done_tx, done_rx) = mpsc::channel();
  let runner = thread::spawn(move || {
    let _ = done_tx.send(scenario());
  });
  match done_rx.recv_timeout(timeout) {
    Ok(value) => {
      runner.join().expect("scenario thread");
      value
    }
    Err(mpsc::RecvTimeoutError::Timeout) => {
      panic!("{what}: did not finish within {timeout:?} (deadlock or starvation)")
    }
    Err(mpsc::RecvTimeoutError::Disconnected) => {
      let panic = runner
        .join()
        .expect_err("scenario thread ended without a result");
      std::panic::resume_unwind(panic)
    }
  }
}

fn commit_node(db: &SingleFileDB, key: &str) -> NodeId {
  db.begin(false).expect("begin");
  let node = db.create_node(Some(key)).expect("create node");
  db.commit().expect("commit");
  node
}

/// A writer that begins, runs `stage`, reports, and commits once released.
/// Returns the release sender, the "staged" receiver, and the join handle
/// carrying the commit result.
fn staged_writer<S, T>(
  db: &Arc<SingleFileDB>,
  stage: S,
) -> (
  mpsc::Sender<()>,
  thread::JoinHandle<(T, kitedb::error::Result<()>)>,
)
where
  S: FnOnce(&SingleFileDB) -> T + Send + 'static,
  T: Send + 'static,
{
  let (staged_tx, staged_rx) = mpsc::channel();
  let (go_tx, go_rx) = mpsc::channel::<()>();
  let db = Arc::clone(db);
  let handle = thread::spawn(move || {
    db.begin(false).expect("first writer begin");
    let staged = stage(&db);
    staged_tx.send(()).expect("report staged");
    go_rx.recv().expect("wait for release");
    let committed = db.commit();
    (staged, committed)
  });
  staged_rx.recv().expect("first writer staged");
  (go_tx, handle)
}

/// Run `write` in a second writer thread while the first one is open, give it
/// `SECOND_WRITER_GRACE` to finish, then release the first writer. Returns
/// whether the second writer finished before the release, and both results.
fn race_second_writer<T>(
  release_first: mpsc::Sender<()>,
  first: thread::JoinHandle<(T, kitedb::error::Result<()>)>,
  db: &Arc<SingleFileDB>,
  write: impl FnOnce(&SingleFileDB) -> kitedb::error::Result<()> + Send + 'static,
) -> (
  bool,
  (T, kitedb::error::Result<()>),
  kitedb::error::Result<()>,
) {
  let (done_tx, done_rx) = mpsc::channel();
  let second = {
    let db = Arc::clone(db);
    thread::spawn(move || {
      let result = (|| {
        db.begin(false)?;
        if let Err(error) = write(&db) {
          let _ = db.rollback();
          return Err(error);
        }
        db.commit()
      })();
      let _ = done_tx.send(());
      result
    })
  };
  let second_finished_first = done_rx.recv_timeout(SECOND_WRITER_GRACE).is_ok();
  release_first.send(()).expect("release first writer");
  let first_result = first.join().expect("first writer thread");
  let second_result = second.join().expect("second writer thread");
  (second_finished_first, first_result, second_result)
}

// ============================================================================
// F1: single writer in non-MVCC mode
// ============================================================================

fn counter_db(path: &Path, mvcc: bool) -> (Arc<SingleFileDB>, NodeId, PropKeyId) {
  let db = open(path, options(mvcc));
  db.begin(false).expect("begin");
  let counter = db.define_propkey("counter").expect("propkey");
  let node = db.create_node(Some("counter")).expect("create");
  db.set_node_prop(node, counter, PropValue::I64(0))
    .expect("set");
  db.commit().expect("commit");
  (db, node, counter)
}

fn read_counter(db: &SingleFileDB, node: NodeId, key: PropKeyId) -> i64 {
  match db.node_prop(node, key) {
    Some(PropValue::I64(value)) => value,
    other => panic!("counter holds {other:?}"),
  }
}

/// Two read-modify-write transactions on one property must not lose an
/// increment. Docs (ARCHITECTURE.md "Non-MVCC Mode: single transaction at a
/// time"; guides/concurrency "Exclusive writer") promise serialized writers,
/// but today a second thread opens a concurrent write transaction with no
/// conflict detection, and the first commit's increment silently vanishes.
#[test]
fn f1_non_mvcc_read_modify_write_transactions_lose_no_update() {
  let dir = tempfile::tempdir().expect("tempdir");
  let (db, node, counter) = counter_db(&dir.path().join("f1-lost-update.kitedb"), false);

  let increment = move |db: &SingleFileDB| {
    let seen = read_counter(db, node, counter);
    db.set_node_prop(node, counter, PropValue::I64(seen + 1))
  };
  // The first writer reads and increments, then stays open while a second
  // writer does the same.
  let (release_first, first) = staged_writer(&db, move |db: &SingleFileDB| {
    increment(db).expect("first increment")
  });
  let (second_finished_first, (_, first_commit), second_commit) =
    race_second_writer(release_first, first, &db, increment);
  first_commit.expect("first writer commit");
  second_commit.expect("second writer commit");

  let value = read_counter(&db, node, counter);
  assert_eq!(
    value, 2,
    "two committed increments left the counter at {value}: one update was lost (the second \
     writer finished while the first was open: {second_finished_first})"
  );
}

/// Read-only transactions never wait for an open writer, in either mode.
#[test]
fn f1_read_only_transactions_do_not_wait_for_an_open_writer_guard() {
  for mvcc in [false, true] {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = open(&dir.path().join("f1-readers.kitedb"), options(mvcc));
    let existing = commit_node(&db, "existing");

    let (release_writer, writer) = staged_writer(&db, |db: &SingleFileDB| {
      db.create_node(Some("pending")).expect("create");
    });
    let reader_db = Arc::clone(&db);
    finishes_within(
      &format!("read-only transaction beside an open writer (mvcc: {mvcc})"),
      Duration::from_secs(5),
      move || {
        reader_db.begin(true).expect("read-only begin");
        assert!(reader_db.node_exists(existing));
        assert!(reader_db.node_by_key("pending").is_none());
        reader_db.commit().expect("read-only commit");
        // And plain reads outside any transaction.
        assert!(reader_db.node_exists(existing));
      },
    );
    release_writer.send(()).expect("release");
    writer.join().expect("writer").1.expect("writer commit");
    assert!(db.node_by_key("pending").is_some());
  }
}

/// MVCC mode keeps concurrent writers: a second writer commits while the
/// first is still open.
#[test]
fn f1_mvcc_writers_stay_concurrent_guard() {
  let dir = tempfile::tempdir().expect("tempdir");
  let db = open(&dir.path().join("f1-mvcc.kitedb"), options(true));
  let (release_first, first) = staged_writer(&db, |db: &SingleFileDB| {
    db.create_node(Some("first")).expect("create first");
  });
  let (second_finished_first, (_, first_commit), second_commit) =
    race_second_writer(release_first, first, &db, |db| {
      db.create_node(Some("second")).map(|_| ())
    });
  first_commit.expect("first commit");
  second_commit.expect("second commit");
  assert!(
    second_finished_first,
    "MVCC mode must not serialize writers: the second writer waited for the first"
  );
  assert!(db.node_by_key("first").is_some());
  assert!(db.node_by_key("second").is_some());
}

/// A thread that ends with a write transaction open must not wedge the
/// database: other writers and checkpoints proceed, and its writes are
/// discarded. Today the transaction stays registered forever, so every
/// blocking checkpoint waits for it; a writer lock leaked the same way would
/// block every later writer.
#[test]
fn f1_thread_exiting_with_an_open_write_transaction_releases_it() {
  let dir = tempfile::tempdir().expect("tempdir");
  let db = open(&dir.path().join("f1-thread-exit.kitedb"), options(false));
  {
    let db = Arc::clone(&db);
    thread::spawn(move || {
      db.begin(false).expect("begin");
      db.create_node(Some("abandoned")).expect("create");
      // The thread ends here without commit or rollback.
    })
    .join()
    .expect("abandoning thread");
  }

  let after = Arc::clone(&db);
  finishes_within(
    "writer and checkpoint after a thread exited inside a write transaction",
    Duration::from_secs(5),
    move || {
      commit_node(&after, "after");
      after.checkpoint().expect("checkpoint");
    },
  );
  assert!(db.node_by_key("after").is_some());
  assert!(
    db.node_by_key("abandoned").is_none(),
    "the abandoned transaction's node became visible"
  );
}

/// A blocking checkpoint and a second writer both queue behind an open
/// writer; once it commits, both finish.
#[test]
fn f1_checkpoint_and_queued_writer_behind_an_open_writer_guard() {
  for background in [false, true] {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = open(&dir.path().join("f1-gate.kitedb"), options(false));
    commit_node(&db, "seed");

    let (release_first, first) = staged_writer(&db, |db: &SingleFileDB| {
      db.create_node(Some("first")).expect("create first");
    });
    let checkpointer = {
      let db = Arc::clone(&db);
      thread::spawn(move || {
        if background {
          db.background_checkpoint()
        } else {
          db.checkpoint()
        }
      })
    };
    let queued = {
      let db = Arc::clone(&db);
      thread::spawn(move || commit_node(&db, "queued"))
    };
    // Let the checkpoint and the queued writer park; the assertion below does
    // not depend on how far they got.
    thread::sleep(Duration::from_millis(100));
    release_first.send(()).expect("release first");

    finishes_within(
      &format!("checkpoint (background: {background}) and queued writer"),
      DEADLOCK_TIMEOUT,
      move || {
        first.join().expect("first").1.expect("first commit");
        match checkpointer.join().expect("checkpointer") {
          Ok(()) | Err(KiteError::CheckpointDeclined(_)) => {}
          Err(error) => panic!("checkpoint failed: {error}"),
        }
        queued.join().expect("queued writer");
      },
    );
    for key in ["seed", "first", "queued"] {
      assert!(db.node_by_key(key).is_some(), "{key} missing");
    }
  }
}

/// Writers, a bulk loader, group commit, and auto-checkpoints (blocking and
/// background) on a small WAL all make progress together, with MVCC (the
/// writers run together, the bulk loader alone) and without it (deprecated:
/// every writer runs alone).
#[test]
fn f1_writers_bulk_load_group_commit_and_auto_checkpoints_guard() {
  for (mvcc, background) in [(false, false), (false, true), (true, false), (true, true)] {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("f1-mixed.kitedb");
    let opts = options(mvcc)
      .sync_mode(SyncMode::Normal)
      .group_commit_enabled(true)
      .group_commit_window_ms(1)
      .wal_size(256 * 1024)
      .auto_checkpoint(true)
      .checkpoint_threshold(0.5)
      .background_checkpoint(background);
    let db = open(&path, opts.clone());
    let scenario_db = Arc::clone(&db);
    finishes_within(
      &format!("mixed writers (mvcc: {mvcc}, background checkpoints: {background})"),
      DEADLOCK_TIMEOUT,
      move || {
        let mut handles = Vec::new();
        for writer in 0..3 {
          let db = Arc::clone(&scenario_db);
          handles.push(thread::spawn(move || {
            for index in 0..150 {
              commit_node(&db, &format!("w{writer}-{index}"));
            }
          }));
        }
        let db = Arc::clone(&scenario_db);
        handles.push(thread::spawn(move || {
          for batch in 0..15 {
            db.begin_bulk().expect("bulk begin");
            let keys: Vec<String> = (0..20).map(|index| format!("b{batch}-{index}")).collect();
            let keys: Vec<Option<&str>> = keys.iter().map(|key| Some(key.as_str())).collect();
            db.create_nodes_batch(&keys).expect("bulk create");
            db.commit().expect("bulk commit");
          }
        }));
        for handle in handles {
          handle.join().expect("writer thread");
        }
      },
    );
    for key in ["w0-0", "w2-149", "b0-0", "b14-19"] {
      assert!(db.node_by_key(key).is_some(), "{key} missing");
    }
    close(db);
    let reopened = open_single_file(&path, opts).expect("reopen");
    assert!(reopened.node_by_key("w1-75").is_some());
  }
}

/// Maintenance (checkpoint, optimize, vacuum, resize_wal) loops while writers
/// commit; nothing deadlocks and nothing is lost.
#[test]
fn f1_maintenance_beside_writers_guard() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("f1-maintenance.kitedb");
  let db = open(&path, options(false).wal_size(MIB));
  let scenario_db = Arc::clone(&db);
  let commits = finishes_within("maintenance beside writers", DEADLOCK_TIMEOUT, move || {
    let stop = Arc::new(AtomicBool::new(false));
    let writers: Vec<_> = (0..2)
      .map(|writer| {
        let db = Arc::clone(&scenario_db);
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
          let mut index = 0usize;
          while !stop.load(Ordering::Relaxed) || index < 20 {
            commit_node(&db, &format!("w{writer}-{index}"));
            index += 1;
          }
          index
        })
      })
      .collect();
    for round in 0..12 {
      match round % 4 {
        0 => scenario_db.checkpoint().expect("checkpoint"),
        1 => scenario_db.optimize_single_file(None).expect("optimize"),
        2 => scenario_db
          .vacuum_single_file(Some(VacuumOptions {
            shrink_wal: false,
            min_wal_size: None,
          }))
          .expect("vacuum"),
        _ => scenario_db
          .resize_wal(
            if round % 8 == 3 { 2 * MIB } else { MIB },
            Some(ResizeWalOptions {
              allow_shrink: true,
              checkpoint: true,
            }),
          )
          .expect("resize"),
      }
    }
    stop.store(true, Ordering::Relaxed);
    writers
      .into_iter()
      .map(|writer| writer.join().expect("writer"))
      .collect::<Vec<_>>()
  });
  for (writer, count) in commits.into_iter().enumerate() {
    for index in [0, count / 2, count - 1] {
      let key = format!("w{writer}-{index}");
      assert!(db.node_by_key(&key).is_some(), "{key} missing");
    }
  }
  assert!(db.check().valid);
}

// ============================================================================
// F2: dangling edges and vectors from a concurrent delete
// ============================================================================

struct EdgeRace {
  db: Arc<SingleFileDB>,
  a: NodeId,
  x: NodeId,
  knows: ETypeId,
}

/// `a` and `x`, either checkpointed into the snapshot or only in the delta.
fn edge_race_db(path: &Path, mvcc: bool, in_snapshot: bool) -> EdgeRace {
  let db = open(path, options(mvcc));
  db.begin(false).expect("begin");
  let knows = db.define_etype("knows").expect("etype");
  let a = db.create_node(Some("a")).expect("a");
  let x = db.create_node(Some("x")).expect("x");
  db.commit().expect("commit");
  if in_snapshot {
    db.checkpoint().expect("checkpoint");
  }
  EdgeRace { db, a, x, knows }
}

fn delete_node(x: NodeId) -> impl FnOnce(&SingleFileDB) -> kitedb::error::Result<()> {
  move |db: &SingleFileDB| db.delete_node(x)
}

/// No committed edge may point at a node that does not exist, live, through
/// a checkpoint, and after reopen.
fn assert_no_dangling_edge(race: EdgeRace, path: &Path, context: &str) {
  let EdgeRace { db, a, x, knows } = race;
  let dangling = |db: &SingleFileDB| {
    let edge = db.edge_exists(a, knows, x) || db.out_edges(a).contains(&(knows, x));
    edge && !db.node_exists(x)
  };
  assert!(
    !dangling(&db),
    "{context}: committed edge a -> x points at deleted node x (out_edges(a) = {:?})",
    db.out_edges(a)
  );
  let check = db.check();
  assert!(check.valid, "{context}: check() failed: {:?}", check.errors);
  db.checkpoint()
    .unwrap_or_else(|error| panic!("{context}: checkpoint failed: {error}"));
  assert!(!dangling(&db), "{context}: dangling edge after checkpoint");
  close(db);
  let reopened = open_single_file(path, options(false)).expect("reopen");
  assert!(
    !dangling(&reopened),
    "{context}: dangling edge after reopen"
  );
}

/// Non-MVCC: T1 adds a -> x (its existence check passes), the main thread
/// deletes x and commits, then T1 commits. Nothing re-checks x at commit, so
/// the edge is committed pointing at a node that exists nowhere. Single-writer
/// serialization (F1) keeps the delete out until T1 commits.
#[test]
fn f2_non_mvcc_edge_to_a_concurrently_deleted_node_is_never_committed() {
  for in_snapshot in [false, true] {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("f2-edge.kitedb");
    let race = edge_race_db(&path, false, in_snapshot);
    let (a, x, knows) = (race.a, race.x, race.knows);
    let (release, first) = staged_writer(&race.db, move |db: &SingleFileDB| {
      db.add_edge(a, knows, x).expect("add edge (x exists now)");
    });
    let (_, (_, first_commit), second_commit) =
      race_second_writer(release, first, &race.db, delete_node(x));
    second_commit.expect("delete commit");
    // The edge's commit may be refused; what must never happen is that it
    // commits dangling.
    let _ = first_commit;
    assert_no_dangling_edge(
      race,
      &path,
      &format!("non-MVCC, x in snapshot: {in_snapshot}"),
    );
  }
}

/// MVCC: the delete of x and a concurrent edge add to x conflict (the edge
/// add writes NeighborsIn(x), as does the delete), so the later commit is
/// refused and no edge dangles. Already the case; kept as a guard.
#[test]
fn f2_mvcc_edge_to_a_concurrently_deleted_node_conflicts_guard() {
  for in_snapshot in [false, true] {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("f2-edge-mvcc.kitedb");
    let race = edge_race_db(&path, true, in_snapshot);
    let (a, x, knows) = (race.a, race.x, race.knows);
    let (release, first) = staged_writer(&race.db, move |db: &SingleFileDB| {
      db.add_edge(a, knows, x).expect("add edge (x exists now)");
    });
    let (second_finished_first, (_, first_commit), second_commit) =
      race_second_writer(release, first, &race.db, delete_node(x));
    assert!(second_finished_first, "MVCC serialized the writers");
    second_commit.expect("delete commit");
    match &first_commit {
      Err(KiteError::Conflict { keys, .. }) => {
        println!("f2 MVCC edge add vs delete (x in snapshot: {in_snapshot}) conflicts on {keys:?}")
      }
      other => panic!(
        "MVCC, x in snapshot: {in_snapshot}: the edge add to the deleted node committed: {other:?}"
      ),
    }
    assert_no_dangling_edge(race, &path, &format!("MVCC, x in snapshot: {in_snapshot}"));
  }
}

struct VectorRace {
  db: Arc<SingleFileDB>,
  x: NodeId,
  embedding: PropKeyId,
}

/// `x` (in the delta: a deleted delta node leaves no tombstone, so a stray
/// vector stays readable) and a vector property in use.
fn vector_race_db(path: &Path, mvcc: bool) -> VectorRace {
  let db = open(path, options(mvcc));
  db.begin(false).expect("begin");
  let embedding = db.define_propkey("embedding").expect("propkey");
  let other = db.create_node(Some("other")).expect("other");
  db.set_node_vector(other, embedding, &[1.0, 0.0, 0.0])
    .expect("vector");
  let x = db.create_node(Some("x")).expect("x");
  db.commit().expect("commit");
  VectorRace { db, x, embedding }
}

fn assert_no_vector_of_deleted_node(race: VectorRace, path: &Path, context: &str) {
  let VectorRace { db, x, embedding } = race;
  if db.node_exists(x) {
    return;
  }
  assert!(
    !db.has_node_vector(x, embedding),
    "{context}: deleted node x has a committed vector {:?}",
    db.node_vector(x, embedding)
  );
  db.checkpoint()
    .unwrap_or_else(|error| panic!("{context}: checkpoint failed: {error}"));
  assert!(
    !db.has_node_vector(x, embedding),
    "{context}: deleted node x has a vector after checkpoint"
  );
  close(db);
  let reopened = open_single_file(path, options(false)).expect("reopen");
  assert!(
    !reopened.has_node_vector(x, embedding),
    "{context}: deleted node x has a vector after reopen"
  );
}

/// Non-MVCC: T1 sets a vector on x, the main thread deletes x (its vector
/// cleanup cannot see T1's pending vector) and commits, then T1 commits the
/// vector of a node that no longer exists.
#[test]
fn f2_non_mvcc_vector_on_a_concurrently_deleted_node_is_never_committed() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("f2-vector.kitedb");
  let race = vector_race_db(&path, false);
  let (x, embedding) = (race.x, race.embedding);
  let (release, first) = staged_writer(&race.db, move |db: &SingleFileDB| {
    db.set_node_vector(x, embedding, &[0.0, 1.0, 0.0])
      .expect("set vector (x exists now)");
  });
  let (_, (_, first_commit), second_commit) =
    race_second_writer(release, first, &race.db, delete_node(x));
  second_commit.expect("delete commit");
  let _ = first_commit;
  assert_no_vector_of_deleted_node(race, &path, "non-MVCC");
}

/// MVCC: `set_node_vector` records no conflict key, so it commits beside a
/// concurrent delete of its node. One of the two must be refused.
#[test]
fn f2_mvcc_vector_on_a_concurrently_deleted_node_conflicts() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("f2-vector-mvcc.kitedb");
  let race = vector_race_db(&path, true);
  let (x, embedding) = (race.x, race.embedding);
  let (release, first) = staged_writer(&race.db, move |db: &SingleFileDB| {
    db.set_node_vector(x, embedding, &[0.0, 1.0, 0.0])
      .expect("set vector (x exists now)");
  });
  let (second_finished_first, (_, first_commit), second_commit) =
    race_second_writer(release, first, &race.db, delete_node(x));
  assert!(second_finished_first, "MVCC serialized the writers");
  second_commit.expect("delete commit");
  assert!(
    matches!(first_commit, Err(KiteError::Conflict { .. })),
    "MVCC: the vector set on the deleted node committed: {first_commit:?}"
  );
  assert_no_vector_of_deleted_node(race, &path, "MVCC");
}

// ============================================================================
// F3: blocking checkpoint / optimize starved by a background loop
// ============================================================================

/// With `background_checkpoint()` called in a zero-pause loop, blocking
/// `checkpoint()` and optimize must still get their turn. Today the
/// background run claims the checkpoint status before taking the gate, so
/// `exclusive_checkpoint_gate` sees a run in progress every time it gets the
/// gate, and waits again (wave 2 measured 1 blocking run in 1.5 s against
/// 274 background runs).
#[test]
fn f3_background_checkpoint_loop_does_not_starve_blocking_checkpoints() {
  const WINDOW: Duration = Duration::from_millis(1500);
  const REQUIRED: usize = 5;

  let dir = tempfile::tempdir().expect("tempdir");
  let db = open(&dir.path().join("f3-starvation.kitedb"), options(false));
  commit_node(&db, "seed");

  let stop = Arc::new(AtomicBool::new(false));
  let background_runs = Arc::new(AtomicUsize::new(0));
  let writer = {
    let (db, stop) = (Arc::clone(&db), Arc::clone(&stop));
    thread::spawn(move || {
      let mut index = 0usize;
      while !stop.load(Ordering::Relaxed) {
        commit_node(&db, &format!("w-{index}"));
        index += 1;
      }
      index
    })
  };
  let background = {
    let (db, stop, runs) = (
      Arc::clone(&db),
      Arc::clone(&stop),
      Arc::clone(&background_runs),
    );
    thread::spawn(move || {
      while !stop.load(Ordering::Relaxed) {
        match db.background_checkpoint() {
          Ok(()) => {
            runs.fetch_add(1, Ordering::Relaxed);
          }
          Err(KiteError::CheckpointDeclined(_)) => {}
          Err(error) => panic!("background checkpoint failed: {error}"),
        }
      }
    })
  };

  // Blocking checkpoints and optimizes, alternating, on their own thread so a
  // starved call cannot hold the test past the window.
  let blocking_runs = Arc::new(AtomicUsize::new(0));
  let optimize_runs = Arc::new(AtomicUsize::new(0));
  let blocking = {
    let (db, stop) = (Arc::clone(&db), Arc::clone(&stop));
    let (blocking_runs, optimize_runs) = (Arc::clone(&blocking_runs), Arc::clone(&optimize_runs));
    thread::spawn(move || {
      let mut round = 0usize;
      while !stop.load(Ordering::Relaxed) {
        if round.is_multiple_of(2) {
          db.checkpoint().expect("blocking checkpoint");
          blocking_runs.fetch_add(1, Ordering::Relaxed);
        } else {
          db.optimize_single_file(None).expect("optimize");
          optimize_runs.fetch_add(1, Ordering::Relaxed);
        }
        round += 1;
      }
    })
  };

  thread::sleep(WINDOW);
  let blocking_done = blocking_runs.load(Ordering::Relaxed);
  let optimize_done = optimize_runs.load(Ordering::Relaxed);
  let background_done = background_runs.load(Ordering::Relaxed);
  stop.store(true, Ordering::Relaxed);
  let commits = writer.join().expect("writer");
  background.join().expect("background loop");
  blocking.join().expect("blocking loop");

  println!(
    "f3: in {WINDOW:?}: {background_done} background checkpoints ran (not declined), {blocking_done} blocking checkpoints, \
     {optimize_done} optimizes, {commits} commits"
  );
  assert!(
    blocking_done >= REQUIRED && optimize_done >= REQUIRED,
    "in {WINDOW:?} a zero-pause background checkpoint loop ran {background_done} times while \
     blocking checkpoint ran {blocking_done} and optimize {optimize_done} times (need \
     {REQUIRED} each; {commits} commits)"
  );
  assert!(db.check().valid);
}

// ============================================================================
// F5: count_nodes
// ============================================================================

/// A graph of `count` nodes checkpointed into the snapshot.
fn snapshot_graph(path: &Path, count: usize, mvcc: bool) -> Arc<SingleFileDB> {
  let db = open(path, options(mvcc).wal_size(64 * MIB));
  let keys: Vec<String> = (0..count).map(|index| format!("n{index}")).collect();
  for chunk in keys.chunks(10_000) {
    db.begin(false).expect("begin");
    let chunk: Vec<Option<&str>> = chunk.iter().map(|key| Some(key.as_str())).collect();
    db.create_nodes_batch(&chunk).expect("create batch");
    db.commit().expect("commit");
  }
  db.checkpoint().expect("checkpoint");
  db
}

fn fastest(runs: usize, mut f: impl FnMut() -> usize) -> (Duration, usize) {
  let mut best = Duration::MAX;
  let mut value = 0;
  for _ in 0..runs {
    let started = Instant::now();
    value = std::hint::black_box(f());
    best = best.min(started.elapsed());
  }
  (best, value)
}

/// `count_nodes` is documented as "optimized via snapshot metadata and delta
/// size adjustments", but it materializes, sorts, and dedups every node id,
/// exactly what `list_nodes` does. Counting must be far cheaper than listing.
#[test]
fn f5_count_nodes_does_not_materialize_every_node() {
  const NODES: usize = 100_000;
  let dir = tempfile::tempdir().expect("tempdir");
  let db = snapshot_graph(&dir.path().join("f5-count.kitedb"), NODES, false);
  // Some delta on top: deletes of snapshot nodes, creates, a create+delete.
  db.begin(false).expect("begin");
  for index in 0..100 {
    let node = db.node_by_key(&format!("n{index}")).expect("node");
    db.delete_node(node).expect("delete");
  }
  for index in 0..50 {
    db.create_node(Some(&format!("d{index}"))).expect("create");
  }
  db.commit().expect("commit");
  let expected = NODES - 100 + 50;

  let (list_time, listed) = fastest(3, || db.list_nodes().len());
  let (count_time, counted) = fastest(3, || db.count_nodes());
  assert_eq!(listed, expected);
  assert_eq!(counted, expected);
  println!("f5 count_nodes on {NODES} snapshot nodes: count {count_time:?}, list {list_time:?}");
  assert!(
    count_time * 20 <= list_time,
    "count_nodes took {count_time:?}, list_nodes {list_time:?}: counting {NODES} nodes is no \
     cheaper than materializing them"
  );
}

/// What `count_nodes` must keep returning: the number of nodes the caller
/// sees, with deletes, recreates, its own pending changes, and MVCC snapshots.
#[test]
fn f5_count_nodes_semantics_guard() {
  for mvcc in [false, true] {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = snapshot_graph(&dir.path().join("f5-semantics.kitedb"), 10, mvcc);
    let context = format!("mvcc: {mvcc}");
    assert_eq!(db.count_nodes(), 10, "{context}: snapshot only");

    // Delete snapshot nodes; create, delete, and recreate delta nodes; reuse
    // a deleted node's key.
    db.begin(false).expect("begin");
    let n0 = db.node_by_key("n0").expect("n0");
    db.delete_node(n0).expect("delete n0");
    let n1 = db.node_by_key("n1").expect("n1");
    db.delete_node(n1).expect("delete n1");
    let temp = db.create_node(Some("temp")).expect("temp");
    db.delete_node(temp).expect("delete temp");
    db.create_node(Some("n0")).expect("recreate n0 key");
    db.create_node(None).expect("anonymous");
    assert_eq!(
      db.count_nodes(),
      10,
      "{context}: inside the writing transaction"
    );
    db.commit().expect("commit");
    assert_eq!(db.count_nodes(), 10, "{context}: after commit");
    assert_eq!(
      db.count_nodes(),
      db.list_nodes().len(),
      "{context}: count vs list"
    );

    // A delta node deleted in a later transaction.
    db.begin(false).expect("begin");
    let anon = db.create_node(None).expect("create");
    db.commit().expect("commit");
    assert_eq!(db.count_nodes(), 11, "{context}: delta create");
    db.begin(false).expect("begin");
    db.delete_node(anon).expect("delete");
    db.commit().expect("commit");
    assert_eq!(db.count_nodes(), 10, "{context}: delta delete");

    if mvcc {
      // A reader that began before a commit keeps counting its snapshot,
      // through creates and deletes of snapshot nodes.
      let n2 = db.node_by_key("n2").expect("n2");
      let (began_tx, began_rx) = mpsc::channel();
      let (go_tx, go_rx) = mpsc::channel::<()>();
      let reader = {
        let db = Arc::clone(&db);
        thread::spawn(move || {
          db.begin(true).expect("reader begin");
          let before = db.count_nodes();
          began_tx.send(()).expect("began");
          go_rx.recv().expect("go");
          let after = db.count_nodes();
          let sees_n2 = db.node_exists(n2);
          db.commit().expect("reader commit");
          (before, after, sees_n2)
        })
      };
      began_rx.recv().expect("reader began");
      db.begin(false).expect("begin");
      db.create_node(Some("late")).expect("late");
      db.delete_node(n2).expect("delete n2");
      let n3 = db.node_by_key("n3").expect("n3");
      db.delete_node(n3).expect("delete n3");
      db.commit().expect("commit");
      go_tx.send(()).expect("go");
      let (before, after, sees_n2) = reader.join().expect("reader");
      assert!(
        sees_n2,
        "{context}: a reader lost a snapshot node deleted after it began"
      );
      assert_eq!((before, after), (10, 10), "{context}: reader snapshot");
      assert_eq!(db.count_nodes(), 9, "{context}: latest");
    }

    db.checkpoint().expect("checkpoint");
    assert_eq!(
      db.count_nodes(),
      db.list_nodes().len(),
      "{context}: after checkpoint"
    );
  }
}

/// `count_nodes` equals `list_nodes().len()` through random creates,
/// deletes, recreates by id, rollbacks, checkpoints, the caller's pending
/// changes, and (MVCC) readers whose snapshots predate later commits.
#[test]
fn f5_count_nodes_matches_list_nodes_under_random_operations_guard() {
  for mvcc in [false, true] {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = open(&dir.path().join("f5-random.kitedb"), options(mvcc));
    let mut state = 0x9E37_79B9_7F4A_7C15u64 ^ u64::from(mvcc);
    let mut random = move |bound: u64| {
      state ^= state << 13;
      state ^= state >> 7;
      state ^= state << 17;
      state % bound
    };
    let mut ids: Vec<NodeId> = Vec::new();
    let mut reader: Option<(mpsc::Sender<()>, thread::JoinHandle<()>)> = None;
    for step in 0..300 {
      db.begin(false).expect("begin");
      for _ in 0..=random(4) {
        match random(4) {
          0 | 1 => ids.push(db.create_node(None).expect("create")),
          2 if !ids.is_empty() => {
            let _ = db.delete_node(ids[random(ids.len() as u64) as usize]);
          }
          // Recreate by id; refused (ignored) while the node is live.
          _ if !ids.is_empty() => {
            let _ = db.create_node_with_id(ids[random(ids.len() as u64) as usize], None);
          }
          _ => {}
        }
        assert_eq!(
          db.count_nodes(),
          db.list_nodes().len(),
          "mvcc: {mvcc}, step {step}: inside a transaction"
        );
      }
      if random(5) == 0 {
        db.rollback().expect("rollback");
      } else {
        db.commit().expect("commit");
      }
      assert_eq!(
        db.count_nodes(),
        db.list_nodes().len(),
        "mvcc: {mvcc}, step {step}"
      );
      if step % 50 == 49 {
        // A blocking checkpoint waits for open transactions, the reader's too.
        if let Some((go, handle)) = reader.take() {
          go.send(()).expect("release reader");
          handle.join().expect("reader");
        }
        db.checkpoint().expect("checkpoint");
      }
      if mvcc && step % 30 == 0 {
        if let Some((go, handle)) = reader.take() {
          go.send(()).expect("release reader");
          handle.join().expect("reader");
        }
        let (began_tx, began_rx) = mpsc::channel();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let reader_db = Arc::clone(&db);
        let handle = thread::spawn(move || {
          reader_db.begin(true).expect("reader begin");
          let before = (reader_db.count_nodes(), reader_db.list_nodes().len());
          began_tx.send(()).expect("began");
          go_rx.recv().expect("go");
          let after = (reader_db.count_nodes(), reader_db.list_nodes().len());
          reader_db.commit().expect("reader commit");
          assert_eq!(before.0, before.1, "reader count vs list at its start");
          assert_eq!(after.0, after.1, "reader count vs list after later commits");
          assert_eq!(before.0, after.0, "reader snapshot changed");
        });
        began_rx.recv().expect("reader began");
        reader = Some((go_tx, handle));
      }
    }
    if let Some((go, handle)) = reader.take() {
      go.send(()).expect("release reader");
      handle.join().expect("reader");
    }
  }
}

/// Transactions are per thread and per database: one thread can hold
/// transactions on two databases at once, and each sees only its own.
#[test]
fn f5_one_thread_with_transactions_on_two_databases_guard() {
  let dir = tempfile::tempdir().expect("tempdir");
  let first = open(&dir.path().join("f5-first.kitedb"), options(false));
  let second = open(&dir.path().join("f5-second.kitedb"), options(true));

  first.begin(false).expect("begin first");
  let in_first = first.create_node(Some("in-first")).expect("create first");
  assert!(first.has_transaction());
  assert!(
    !second.has_transaction(),
    "a transaction on one database leaked into another"
  );
  assert!(second.node_by_key("in-first").is_none());

  second
    .begin(false)
    .expect("begin second while first is open");
  second
    .create_node(Some("in-second"))
    .expect("create second");
  assert!(first.node_exists(in_first), "first lost its pending node");
  assert!(first.node_by_key("in-second").is_none());
  second.commit().expect("commit second");
  assert!(
    first.has_transaction(),
    "committing one database ended the other's transaction"
  );
  assert!(matches!(
    first.begin(false),
    Err(KiteError::TransactionInProgress)
  ));
  first.commit().expect("commit first");
  assert!(!first.has_transaction() && !second.has_transaction());

  assert!(first.node_by_key("in-first").is_some());
  assert!(second.node_by_key("in-second").is_some());
  assert!(first.node_by_key("in-second").is_none());
}

/// Read throughput with 1 and 8 threads reading outside transactions (every
/// read currently locks the global `current_tx` map), and `count_nodes` cost.
/// Numbers only: `cargo test --test b4_engine_concurrency f5_bench -- --ignored --nocapture`.
#[test]
#[ignore = "benchmark"]
fn f5_bench_reads_and_count_nodes() {
  const NODES: usize = 100_000;
  let dir = tempfile::tempdir().expect("tempdir");
  let db = snapshot_graph(&dir.path().join("f5-bench.kitedb"), NODES, false);
  let ids = Arc::new(db.list_nodes());
  for threads in [1usize, 2, 4, 8] {
    let stop = Arc::new(AtomicBool::new(false));
    let workers: Vec<_> = (0..threads)
      .map(|worker| {
        let (db, ids, stop) = (Arc::clone(&db), Arc::clone(&ids), Arc::clone(&stop));
        thread::spawn(move || {
          let mut reads = 0usize;
          let mut index = worker * 7919;
          while !stop.load(Ordering::Relaxed) {
            index = (index + 104_729) % ids.len();
            std::hint::black_box(db.node_exists(ids[index]));
            reads += 1;
          }
          reads
        })
      })
      .collect();
    thread::sleep(Duration::from_millis(1000));
    stop.store(true, Ordering::Relaxed);
    let reads: usize = workers.into_iter().map(|w| w.join().expect("reader")).sum();
    println!("f5 bench: {threads} reader threads: {reads} node_exists/s");
  }
  let (count_time, counted) = fastest(5, || db.count_nodes());
  let (list_time, _) = fastest(5, || db.list_nodes().len());
  println!("f5 bench: count_nodes({counted}) {count_time:?}, list_nodes {list_time:?}");
}

// ============================================================================
// F6: commit latency during a background checkpoint
// ============================================================================

// The F6 guard (commits keep going while a background checkpoint collects and
// writes its snapshot) holds the checkpoint at a phase hook, so it lives in
// `src/core/single_file/b4_commit_pipeline_checkpoint_tests.rs`.

/// Commits as fast as one thread can during a background checkpoint: all of
/// them are post-cut commits its install replays. Numbers only:
/// `cargo test --test b4_engine_concurrency f6_bench -- --ignored --nocapture`.
#[test]
#[ignore = "benchmark"]
fn f6_bench_unpaced_commits_during_background_checkpoint() {
  for run in 0..3 {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = f6_graph(&dir.path().join("f6-bench.kitedb"));
    let (checkpoint_time, latencies) = commits_during_background_checkpoint(&db);
    let (median, max) = latency_summary(&latencies);
    println!(
      "f6 bench run {run}: background checkpoint {checkpoint_time:?}, {} unpaced commits \
       meanwhile, median {median:?}, max {max:?}",
      latencies.len()
    );
  }
}

const F6_NODES: usize = 100_000;

/// A ~100k-node snapshot with edges and props, so collecting it is real work,
/// and some commits in the WAL.
fn f6_graph(path: &Path) -> Arc<SingleFileDB> {
  let db = snapshot_graph(path, F6_NODES, false);
  db.begin(false).expect("begin");
  let knows = db.define_etype("knows").expect("etype");
  let weight = db.define_propkey("weight").expect("propkey");
  db.commit().expect("commit");
  let ids: Vec<NodeId> = db.list_nodes();
  for chunk in ids.chunks(10_000) {
    db.begin(false).expect("begin");
    let edges: Vec<(NodeId, ETypeId, NodeId)> =
      chunk.windows(2).map(|w| (w[0], knows, w[1])).collect();
    db.add_edges_batch(&edges).expect("edges");
    for &node in chunk.iter().step_by(10) {
      db.set_node_prop(node, weight, PropValue::I64(node as i64))
        .expect("prop");
    }
    db.commit().expect("commit");
  }
  db.checkpoint().expect("checkpoint");
  for index in 0..200 {
    commit_node(&db, &format!("pre-{index}"));
  }
  db
}

/// Run a background checkpoint while this thread commits; the checkpoint's
/// duration and each commit's latency.
fn commits_during_background_checkpoint(db: &Arc<SingleFileDB>) -> (Duration, Vec<Duration>) {
  let running = Arc::new(AtomicBool::new(true));
  let checkpointer = {
    let (db, running) = (Arc::clone(db), Arc::clone(&running));
    thread::spawn(move || {
      let started = Instant::now();
      let result = db.background_checkpoint();
      let elapsed = started.elapsed();
      running.store(false, Ordering::Release);
      (result, elapsed)
    })
  };
  let mut latencies = Vec::new();
  let mut index = 0usize;
  while running.load(Ordering::Acquire) {
    let started = Instant::now();
    commit_node(db, &format!("during-{index}"));
    latencies.push(started.elapsed());
    index += 1;
  }
  let (result, checkpoint_time) = checkpointer.join().expect("checkpointer");
  result.expect("background checkpoint");
  (checkpoint_time, latencies)
}

fn latency_summary(latencies: &[Duration]) -> (Duration, Duration) {
  let mut sorted = latencies.to_vec();
  sorted.sort_unstable();
  let median = sorted.get(sorted.len() / 2).copied().unwrap_or_default();
  let max = sorted.last().copied().unwrap_or_default();
  (median, max)
}

// ============================================================================
// F7: readers during vacuum / resize_wal
// ============================================================================

/// Readers outside a transaction hammer node_exists / out_edges / in_edges /
/// node_prop / node_by_key / node_vector while vacuum and resize_wal relocate
/// the snapshot (and resize_wal's checkpoint replaces snapshot and delta) in a
/// loop, with MVCC off and on. Every read must see the same graph.
#[test]
fn f7_readers_outside_transactions_see_consistent_state_during_vacuum_and_resize() {
  for mvcc in [false, true] {
    readers_during_vacuum_and_resize(mvcc);
  }
}

fn readers_during_vacuum_and_resize(mvcc: bool) {
  const NODES: usize = 400;
  const ROUNDS: usize = 45;
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("f7-readers.kitedb");
  let db = open(&path, options(mvcc).wal_size(MIB));

  db.begin(false).expect("begin");
  let next = db.define_etype("next").expect("etype");
  let rank = db.define_propkey("rank").expect("propkey");
  let embedding = db.define_propkey("embedding").expect("propkey");
  let mut ids = Vec::with_capacity(NODES);
  for index in 0..NODES {
    let node = db.create_node(Some(&format!("n{index}"))).expect("create");
    db.set_node_prop(node, rank, PropValue::I64(index as i64))
      .expect("prop");
    db.set_node_vector(node, embedding, &[1.0, index as f32, 2.0])
      .expect("vector");
    ids.push(node);
  }
  for pair in ids.windows(2) {
    db.add_edge(pair[0], next, pair[1]).expect("edge");
  }
  db.commit().expect("commit");
  db.checkpoint().expect("checkpoint");
  // Leave part of the graph in the delta (rewritten props, same values) over
  // the snapshot, which the first resize then checkpoints.
  db.begin(false).expect("begin");
  for (index, &node) in ids.iter().enumerate().take(NODES / 4) {
    db.set_node_prop(node, rank, PropValue::I64(index as i64))
      .expect("prop");
  }
  db.commit().expect("commit");
  let vectors: Vec<_> = ids
    .iter()
    .map(|&node| db.node_vector(node, embedding).expect("vector"))
    .collect();
  let (ids, vectors) = (Arc::new(ids), Arc::new(vectors));

  let stop = Arc::new(AtomicBool::new(false));
  let readers: Vec<_> = (0..2)
    .map(|reader| {
      let (db, ids, vectors, stop) = (
        Arc::clone(&db),
        Arc::clone(&ids),
        Arc::clone(&vectors),
        Arc::clone(&stop),
      );
      thread::spawn(move || {
        let mut reads = 0usize;
        let mut errors = Vec::new();
        let mut index = reader * 7;
        while !stop.load(Ordering::Relaxed) && errors.len() < 5 {
          index = (index + 13) % NODES;
          let node = ids[index];
          if !db.node_exists(node) {
            errors.push(format!("node_exists(n{index}) = false"));
          }
          let expected_out: Vec<(ETypeId, NodeId)> = ids
            .get(index + 1)
            .map(|&dst| vec![(next, dst)])
            .unwrap_or_default();
          let out = db.out_edges(node);
          if out != expected_out {
            errors.push(format!(
              "out_edges(n{index}) = {out:?}, expected {expected_out:?}"
            ));
          }
          let expected_in: Vec<(ETypeId, NodeId)> = index
            .checked_sub(1)
            .map(|previous| vec![(next, ids[previous])])
            .unwrap_or_default();
          let incoming = db.in_edges(node);
          if incoming != expected_in {
            errors.push(format!(
              "in_edges(n{index}) = {incoming:?}, expected {expected_in:?}"
            ));
          }
          match db.node_prop(node, rank) {
            Some(PropValue::I64(value)) if value == index as i64 => {}
            other => errors.push(format!("node_prop(n{index}, rank) = {other:?}")),
          }
          if db.node_by_key(&format!("n{index}")) != Some(node) {
            errors.push(format!("node_by_key(n{index}) lost the node"));
          }
          let vector = db.node_vector(node, embedding);
          if vector.as_deref() != Some(&vectors[index][..]) {
            errors.push(format!("node_vector(n{index}) = {vector:?}"));
          }
          reads += 1;
        }
        (reads, errors)
      })
    })
    .collect();

  for round in 0..ROUNDS {
    let size = if round.is_multiple_of(2) {
      2 * MIB
    } else {
      MIB
    };
    match round % 3 {
      0 => db
        .resize_wal(
          size,
          Some(ResizeWalOptions {
            allow_shrink: true,
            checkpoint: true,
          }),
        )
        .expect("resize with checkpoint"),
      1 => db
        .resize_wal(
          size,
          Some(ResizeWalOptions {
            allow_shrink: true,
            checkpoint: false,
          }),
        )
        .expect("resize"),
      _ => db.vacuum_single_file(None).expect("vacuum"),
    }
  }
  stop.store(true, Ordering::Relaxed);
  let mut total_reads = 0;
  for reader in readers {
    let (reads, errors) = reader.join().expect("reader");
    total_reads += reads;
    assert!(
      errors.is_empty(),
      "mvcc: {mvcc}: a reader saw inconsistent state during vacuum/resize_wal: {errors:?}"
    );
  }
  println!(
    "f7 (mvcc: {mvcc}): {total_reads} consistent reads across {ROUNDS} vacuum/resize rounds"
  );
  assert!(total_reads > 0);
  close(db);
  // The rounds leave the WAL at another size; open with the file's.
  let reopened = open_single_file(&path, SingleFileOpenOptions::new()).expect("reopen");
  assert!(reopened.check().valid);
}
