//! Wave-2 checkpoint reproductions (K2, K3, K5) through the public API.
//!
//! K2 and K3 fail until fixed: two non-MVCC write transactions race, one
//! deleting a node while the other, already past its existence check, adds an
//! edge to it or sets its vector. When both commit, the committed state holds
//! a dangling edge or a vector of a missing node. (A commit-time existence
//! check now refuses the racing commit, and serialized writers make the
//! delete run after it; either way nothing may be left behind.) K5 is a guard
//! that passes on 39fefea. The tests that need
//! private access or checkpoint phase hooks (K1, K4, and the legacy-state
//! variants of K2 and K3) live in `src/core/single_file/w2_checkpoint_tests.rs`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use kitedb::core::single_file::{open_single_file, SingleFileDB, SingleFileOpenOptions};
use kitedb::KiteError;

fn options() -> SingleFileOpenOptions {
  SingleFileOpenOptions::new().auto_checkpoint(false)
}

/// Run `racing` in a write transaction on another thread, commit a
/// transaction that runs `deleting` while it is open, then commit the racing
/// one. The racing commit may be refused by the commit-time existence check.
/// If writers are serialized at begin, the deleting transaction waits for the
/// racing one and runs after it.
fn race_against_delete(
  db: &Arc<SingleFileDB>,
  racing: impl FnOnce(&SingleFileDB) + Send + 'static,
  deleting: impl FnOnce(&SingleFileDB) + Send + 'static,
) {
  let (ready_tx, ready_rx) = mpsc::channel();
  let (go_tx, go_rx) = mpsc::channel::<()>();
  let worker_db = Arc::clone(db);
  let worker = std::thread::spawn(move || {
    worker_db.begin(false).expect("begin racing transaction");
    racing(&worker_db);
    ready_tx.send(()).expect("signal ready");
    go_rx.recv().expect("wait for the delete");
    worker_db.commit()
  });
  ready_rx.recv().expect("racing transaction ready");
  let (deleted_tx, deleted_rx) = mpsc::channel();
  let deleter_db = Arc::clone(db);
  let deleter = std::thread::spawn(move || {
    deleter_db.begin(false).expect("begin deleting transaction");
    deleting(&deleter_db);
    deleter_db.commit().expect("commit delete");
    let _ = deleted_tx.send(());
  });
  // A begin serialized behind the racing transaction cannot finish first.
  let _ = deleted_rx.recv_timeout(Duration::from_secs(2));
  go_tx.send(()).expect("release racing transaction");
  let racing_commit = worker.join().expect("racing thread");
  deleter.join().expect("deleting thread");
  match racing_commit {
    Ok(()) | Err(KiteError::NodeNotFound(_)) | Err(KiteError::EdgeNotFound { .. }) => {}
    Err(error) => panic!("the racing commit failed unexpectedly: {error}"),
  }
}

fn reopen(db: Arc<SingleFileDB>) -> SingleFileDB {
  let path = db.path().to_path_buf();
  drop(Arc::try_unwrap(db).unwrap_or_else(|_| panic!("sole database handle")));
  open_single_file(&path, options()).expect("reopen")
}

// ============================================================================
// K2: an edge to a node deleted concurrently must not fail every checkpoint
// ============================================================================

#[test]
fn k2_edge_racing_a_delete_of_its_endpoint_does_not_fail_checkpoints() {
  let dir = tempfile::tempdir().expect("tempdir");
  let db = Arc::new(open_single_file(dir.path().join("k2-race.kitedb"), options()).expect("open"));
  db.begin(false).expect("begin");
  let a = db.create_node(Some("k2-a")).expect("node a");
  let x = db.create_node(Some("k2-x")).expect("node x");
  let knows = db.define_etype("knows").expect("etype");
  db.commit().expect("commit");

  race_against_delete(
    &db,
    move |db| db.add_edge(a, knows, x).expect("add edge to a live node"),
    move |db| db.delete_node(x).expect("delete x"),
  );
  assert!(!db.node_exists(x), "test setup: x must be deleted");

  let first = db.checkpoint();
  let second = db.checkpoint();
  assert!(
    first.is_ok() && second.is_ok(),
    "checkpoint fails on the committed edge {a} -> {x} whose destination is gone, and keeps \
     failing (the WAL can only fill up): first {first:?}, second {second:?}"
  );
  assert!(!db.edge_exists(a, knows, x));

  let reopened = reopen(db);
  assert!(reopened.node_exists(a));
  assert!(!reopened.node_exists(x));
  assert!(
    reopened.out_edges(a).is_empty(),
    "{:?}",
    reopened.out_edges(a)
  );
  reopened.checkpoint().expect("checkpoint after reopen");
}

// ============================================================================
// K3: a vector of a node deleted concurrently must not reach a snapshot
// ============================================================================

#[test]
fn k3_vector_racing_a_delete_of_its_node_is_not_checkpointed() {
  let dir = tempfile::tempdir().expect("tempdir");
  let db = Arc::new(open_single_file(dir.path().join("k3-race.kitedb"), options()).expect("open"));
  db.begin(false).expect("begin");
  let keep = db.create_node(Some("k3-keep")).expect("node keep");
  let gone = db.create_node(Some("k3-gone")).expect("node gone");
  let embedding = db.define_propkey("embedding").expect("propkey");
  db.set_node_vector(keep, embedding, &[1.0, 0.0, 0.0])
    .expect("vector keep");
  db.commit().expect("commit");

  race_against_delete(
    &db,
    move |db| {
      db.set_node_vector(gone, embedding, &[0.0, 1.0, 0.0])
        .expect("set vector of a live node")
    },
    move |db| db.delete_node(gone).expect("delete node"),
  );
  assert!(!db.node_exists(gone), "test setup: node must be deleted");

  db.checkpoint().expect("checkpoint");
  let reopened = reopen(db);
  assert!(!reopened.node_exists(gone));
  assert!(reopened.has_node_vector(keep, embedding));
  assert!(
    !reopened.has_node_vector(gone, embedding),
    "the checkpoint wrote the vector of deleted node {gone} into the snapshot: after reopen \
     node_vector = {:?}",
    reopened.node_vector(gone, embedding)
  );
}

// ============================================================================
// K5 (guard): blocking checkpoints racing background checkpoints
// ============================================================================

/// Guard: passes on 39fefea. Writers commit while background checkpoints,
/// blocking checkpoints, and optimize run back to back on other threads;
/// every acknowledged commit must survive, live and after reopen.
#[test]
fn k5_blocking_checkpoints_racing_background_checkpoints_lose_no_commits() {
  const RUN_FOR: Duration = Duration::from_millis(1500);
  let dir = tempfile::tempdir().expect("tempdir");
  let db = Arc::new(open_single_file(dir.path().join("k5-race.kitedb"), options()).expect("open"));
  let stop = Arc::new(AtomicBool::new(false));
  let committed = Arc::new(Mutex::new(BTreeSet::new()));
  let failures = Arc::new(Mutex::new(Vec::<String>::new()));
  let completed = Arc::new(Mutex::new(BTreeMap::<&str, usize>::new()));

  let mut threads = Vec::new();
  for writer in 0..2 {
    let (db, stop, committed) = (Arc::clone(&db), Arc::clone(&stop), Arc::clone(&committed));
    threads.push(std::thread::spawn(move || {
      let mut index = 0u64;
      while !stop.load(Ordering::Relaxed) {
        index += 1;
        let key = format!("k5-w{writer}-{index}");
        if db.begin(false).is_err() {
          continue;
        }
        if db.create_node(Some(&key)).is_err() {
          let _ = db.rollback();
          continue;
        }
        if db.commit().is_ok() {
          committed.lock().expect("committed keys").insert(key);
        }
      }
    }));
  }
  type Step = fn(&SingleFileDB) -> kitedb::Result<()>;
  let steps: [(&str, Step, Duration); 3] = [
    (
      "background_checkpoint",
      |db| db.background_checkpoint(),
      Duration::from_millis(3),
    ),
    ("checkpoint", |db| db.checkpoint(), Duration::from_millis(1)),
    (
      "optimize",
      |db| db.optimize_single_file(None),
      Duration::from_millis(50),
    ),
  ];
  for (name, step, pause) in steps {
    let (db, stop, failures) = (Arc::clone(&db), Arc::clone(&stop), Arc::clone(&failures));
    let completed = Arc::clone(&completed);
    threads.push(std::thread::spawn(move || {
      while !stop.load(Ordering::Relaxed) {
        match step(&db) {
          Ok(()) => {
            *completed
              .lock()
              .expect("completed")
              .entry(name)
              .or_default() += 1
          }
          Err(KiteError::CheckpointDeclined(_)) => {}
          Err(error) => failures
            .lock()
            .expect("failures")
            .push(format!("{name}: {error}")),
        }
        std::thread::sleep(pause);
      }
    }));
  }

  let started = Instant::now();
  while started.elapsed() < RUN_FOR {
    std::thread::sleep(Duration::from_millis(10));
  }
  stop.store(true, Ordering::Relaxed);
  for thread in threads {
    thread.join().expect("worker thread");
  }

  let failures = failures.lock().expect("failures").clone();
  assert!(failures.is_empty(), "checkpoints failed: {failures:?}");
  let committed = committed.lock().expect("committed keys").clone();
  let completed = completed.lock().expect("completed").clone();
  eprintln!("{} commits; completed runs: {completed:?}", committed.len());
  assert!(
    committed.len() > 100 && completed.len() == 3,
    "test setup: too little ran ({} commits; completed runs: {completed:?})",
    committed.len()
  );
  let missing = |db: &SingleFileDB| -> Vec<String> {
    committed
      .iter()
      .filter(|key| db.node_by_key(key).is_none())
      .cloned()
      .collect()
  };
  assert_eq!(missing(&db), Vec::<String>::new(), "commits lost live");
  let check = db.check();
  assert!(check.valid, "check: {:?}", check.errors);

  let reopened = reopen(db);
  assert_eq!(
    missing(&reopened),
    Vec::<String>::new(),
    "commits lost after reopen"
  );
  let check = reopened.check();
  assert!(check.valid, "check after reopen: {:?}", check.errors);
}
