//! Audit repros for the MVCC read path (lane mvcc-reads, findings M1, M2, M3, M5).
//!
//! The deadlock tests run the racing operations on worker threads and fail with
//! "deadlock" when the workers do not finish in time. Hung threads are leaked.

use kitedb::api::kite::{Kite, KiteOptions, NodeDef, PropDef};
use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions, SyncMode,
};
use kitedb::metrics::collect_metrics_single_file;
use kitedb::types::{ETypeId, LabelId, NodeId, PropKeyId, PropValue};
use std::collections::HashMap;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

const RACE_BUDGET: Duration = Duration::from_secs(3);
const DEADLOCK_TIMEOUT: Duration = Duration::from_secs(15);
const FANOUT: usize = 64;
const READER_THREADS: usize = 3;
const MAX_WRITER_COMMITS: u64 = 50_000;

struct Graph {
  hub: NodeId,
  etype: ETypeId,
  label: LabelId,
  prop: PropKeyId,
}

fn base_options() -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .mvcc(true)
    .auto_checkpoint(false)
    .background_checkpoint(false)
    .sync_mode(SyncMode::Off)
    .wal_size(64 * 1024 * 1024)
}

fn seed(db: &SingleFileDB) -> Graph {
  db.begin(false).expect("begin seed tx");
  let hub = db.create_node(Some("hub")).expect("create hub");
  let etype = db.define_etype("LINK").expect("define etype");
  let label = db.define_label("Tag").expect("define label");
  let prop = db.define_propkey("counter").expect("define propkey");
  db.add_node_label(hub, label).expect("label hub");
  db.set_node_prop(hub, prop, PropValue::I64(0))
    .expect("set hub prop");
  for i in 0..FANOUT {
    let leaf = db
      .create_node(Some(&format!("leaf-{i}")))
      .expect("create leaf");
    db.add_edge(hub, etype, leaf).expect("hub -> leaf");
    db.add_edge(leaf, etype, hub).expect("leaf -> hub");
  }
  db.commit().expect("commit seed tx");
  Graph {
    hub,
    etype,
    label,
    prop,
  }
}

fn wait_all<T>(rx: &mpsc::Receiver<T>, count: usize, what: &str) -> Vec<T> {
  let deadline = Instant::now() + DEADLOCK_TIMEOUT;
  let mut results = Vec::with_capacity(count);
  while results.len() < count {
    let remaining = deadline.saturating_duration_since(Instant::now());
    match rx.recv_timeout(remaining) {
      Ok(value) => results.push(value),
      Err(mpsc::RecvTimeoutError::Timeout) => panic!(
        "deadlock: {what}: only {}/{count} workers finished within {DEADLOCK_TIMEOUT:?}",
        results.len()
      ),
      Err(mpsc::RecvTimeoutError::Disconnected) => {
        panic!("{what}: a worker panicked before finishing")
      }
    }
  }
  results
}

fn close_shared(db: Arc<SingleFileDB>) {
  let db = match Arc::try_unwrap(db) {
    Ok(db) => db,
    Err(_) => panic!("database still shared after workers finished"),
  };
  close_single_file(db).expect("close db");
}

// ============================================================================
// M1: background GC (tx_manager -> version_chain) vs readers inside a tx
// (version_chain -> tx_manager for record_read)
// ============================================================================

fn race_gc_with_tx_reader(what: &str, read: fn(&SingleFileDB, &Graph)) {
  let dir = tempfile::tempdir().expect("tempdir");
  let db = Arc::new(
    open_single_file(
      dir.path().join("m1.kitedb"),
      base_options().mvcc_gc_interval_ms(1),
    )
    .expect("open db"),
  );
  let graph = Arc::new(seed(&db));

  let (done_tx, done_rx) = mpsc::channel();
  let reader_db = Arc::clone(&db);
  let reader_graph = Arc::clone(&graph);
  let handle = thread::spawn(move || {
    reader_db.begin(true).expect("begin read tx");
    let start = Instant::now();
    let mut calls = 0u64;
    while start.elapsed() < RACE_BUDGET {
      read(&reader_db, &reader_graph);
      calls += 1;
    }
    reader_db.commit().expect("commit read tx");
    drop(reader_db);
    let _ = done_tx.send(calls);
  });

  let calls = wait_all(&done_rx, 1, what)[0];
  handle.join().expect("reader thread");
  let gc_runs = collect_metrics_single_file(&db)
    .mvcc
    .expect("mvcc metrics")
    .gc_runs;
  assert!(
    gc_runs >= 10,
    "{what}: GC ran only {gc_runs} times during the race; the race was not exercised"
  );
  assert!(calls > 0, "{what}: reader made no calls");
  close_shared(db);
}

#[test]
fn audit_m1_gc_vs_out_edges_in_tx() {
  race_gc_with_tx_reader("GC vs out_edges in tx", |db, g| {
    std::hint::black_box(db.out_edges(g.hub));
  });
}

#[test]
fn audit_m1_gc_vs_in_edges_in_tx() {
  race_gc_with_tx_reader("GC vs in_edges in tx", |db, g| {
    std::hint::black_box(db.in_edges(g.hub));
  });
}

#[test]
fn audit_m1_gc_vs_node_has_label_in_tx() {
  race_gc_with_tx_reader("GC vs node_has_label in tx", |db, g| {
    std::hint::black_box(db.node_has_label(g.hub, g.label));
  });
}

#[test]
fn audit_m1_gc_vs_node_labels_in_tx() {
  race_gc_with_tx_reader("GC vs node_labels in tx", |db, g| {
    std::hint::black_box(db.node_labels(g.hub));
  });
}

#[test]
fn audit_m1_gc_vs_list_edges_in_tx() {
  race_gc_with_tx_reader("GC vs list_edges in tx", |db, g| {
    std::hint::black_box(db.list_edges(Some(g.etype)));
  });
}

// ============================================================================
// M2: commit (delta.write -> version_chain) vs readers
// (version_chain -> delta.read)
// ============================================================================

fn race_commit_with_reader(what: &str, read: fn(&SingleFileDB, &Graph)) {
  let dir = tempfile::tempdir().expect("tempdir");
  // Readers run outside a transaction, so they never take tx_manager while
  // holding version_chain: the GC thread cannot be part of this cycle.
  let db = Arc::new(
    open_single_file(
      dir.path().join("m2.kitedb"),
      base_options().mvcc_gc_interval_ms(1000),
    )
    .expect("open db"),
  );
  let graph = Arc::new(seed(&db));

  // An open read tx makes every commit publish MVCC versions, so commit
  // locks version_chain while holding delta.write().
  let (pin_ready_tx, pin_ready_rx) = mpsc::channel();
  let (pin_release_tx, pin_release_rx) = mpsc::channel::<()>();
  let pin_db = Arc::clone(&db);
  let pin = thread::spawn(move || {
    pin_db.begin(true).expect("begin pinning read tx");
    pin_ready_tx.send(()).expect("pin ready");
    let _ = pin_release_rx.recv();
    pin_db.commit().expect("commit pinning read tx");
  });
  pin_ready_rx
    .recv_timeout(DEADLOCK_TIMEOUT)
    .expect("pinning reader started");

  let (done_tx, done_rx) = mpsc::channel();
  let mut workers = Vec::new();

  let writer_db = Arc::clone(&db);
  let writer_graph = Arc::clone(&graph);
  let writer_done = done_tx.clone();
  workers.push(thread::spawn(move || {
    let start = Instant::now();
    let mut commits = 0u64;
    while start.elapsed() < RACE_BUDGET && commits < MAX_WRITER_COMMITS {
      writer_db.begin(false).expect("begin write tx");
      writer_db
        .set_node_prop(
          writer_graph.hub,
          writer_graph.prop,
          PropValue::I64(commits as i64 + 1),
        )
        .expect("set prop");
      writer_db.commit().expect("commit write tx");
      commits += 1;
    }
    drop(writer_db);
    let _ = writer_done.send(("writer", commits));
  }));

  for _ in 0..READER_THREADS {
    let reader_db = Arc::clone(&db);
    let reader_graph = Arc::clone(&graph);
    let reader_done = done_tx.clone();
    workers.push(thread::spawn(move || {
      let start = Instant::now();
      let mut calls = 0u64;
      while start.elapsed() < RACE_BUDGET {
        read(&reader_db, &reader_graph);
        calls += 1;
      }
      drop(reader_db);
      let _ = reader_done.send(("reader", calls));
    }));
  }
  drop(done_tx);

  let results = wait_all(&done_rx, READER_THREADS + 1, what);
  for worker in workers {
    worker.join().expect("worker thread");
  }
  pin_release_tx.send(()).expect("release pin");
  pin.join().expect("pinning reader");

  let commits: u64 = results
    .iter()
    .filter(|(role, _)| *role == "writer")
    .map(|(_, n)| *n)
    .sum();
  assert!(
    commits >= 100,
    "{what}: writer committed only {commits} times; the race was not exercised"
  );
  close_shared(db);
}

#[test]
fn audit_m2_commit_vs_out_edges() {
  race_commit_with_reader("commit vs out_edges", |db, g| {
    std::hint::black_box(db.out_edges(g.hub));
  });
}

#[test]
fn audit_m2_commit_vs_in_edges() {
  race_commit_with_reader("commit vs in_edges", |db, g| {
    std::hint::black_box(db.in_edges(g.hub));
  });
}

#[test]
fn audit_m2_commit_vs_node_has_label() {
  race_commit_with_reader("commit vs node_has_label", |db, g| {
    std::hint::black_box(db.node_has_label(g.hub, g.label));
  });
}

#[test]
fn audit_m2_commit_vs_node_labels() {
  race_commit_with_reader("commit vs node_labels", |db, g| {
    std::hint::black_box(db.node_labels(g.hub));
  });
}

#[test]
fn audit_m2_commit_vs_node_by_key() {
  race_commit_with_reader("commit vs node_by_key", |db, _g| {
    std::hint::black_box(db.node_by_key("hub"));
  });
}

#[test]
fn audit_m2_commit_vs_node_key() {
  race_commit_with_reader("commit vs node_key", |db, g| {
    std::hint::black_box(db.node_key(g.hub));
  });
}

#[test]
fn audit_m2_commit_vs_list_nodes() {
  race_commit_with_reader("commit vs list_nodes", |db, _g| {
    std::hint::black_box(db.list_nodes());
  });
}

#[test]
fn audit_m2_commit_vs_list_edges() {
  race_commit_with_reader("commit vs list_edges", |db, g| {
    std::hint::black_box(db.list_edges(Some(g.etype)));
  });
}

// ============================================================================
// M3: node_by_key relocks the (non-reentrant) tx mutex inside a transaction
// ============================================================================

fn run_with_timeout<T: Send + 'static>(
  what: &str,
  timeout: Duration,
  body: impl FnOnce() -> T + Send + 'static,
) -> T {
  let (tx, rx) = mpsc::channel();
  thread::spawn(move || {
    let _ = tx.send(body());
  });
  match rx.recv_timeout(timeout) {
    Ok(value) => value,
    Err(mpsc::RecvTimeoutError::Timeout) => {
      panic!("deadlock: {what} did not return within {timeout:?}")
    }
    Err(mpsc::RecvTimeoutError::Disconnected) => panic!("{what}: worker panicked"),
  }
}

#[test]
fn audit_m3_node_by_key_inside_tx() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("m3.kitedb");
  let (found, expected) = run_with_timeout(
    "node_by_key inside a write tx with MVCC",
    Duration::from_secs(10),
    move || {
      let db = open_single_file(&path, base_options()).expect("open db");
      let graph = seed(&db);
      db.begin(false).expect("begin tx");
      let found = db.node_by_key("hub");
      db.rollback().expect("rollback");
      close_single_file(db).expect("close db");
      (found, graph.hub)
    },
  );
  assert_eq!(found, Some(expected));
}

#[test]
fn audit_m3_node_by_key_inside_read_tx() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("m3-read.kitedb");
  let (found, expected) = run_with_timeout(
    "node_by_key inside a read-only tx with MVCC",
    Duration::from_secs(10),
    move || {
      let db = open_single_file(&path, base_options()).expect("open db");
      let graph = seed(&db);
      db.begin(true).expect("begin read tx");
      let found = db.node_by_key("hub");
      db.commit().expect("commit read tx");
      close_single_file(db).expect("close db");
      (found, graph.hub)
    },
  );
  assert_eq!(found, Some(expected));
}

#[test]
fn audit_m3_kite_update_by_key_with_mvcc() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("m3-kite.kitedb");
  let name = run_with_timeout(
    "Kite::update_by_key with MVCC",
    Duration::from_secs(10),
    move || {
      let options = KiteOptions::new()
        .node(NodeDef::new("User", "user:").prop(PropDef::string("name")))
        .mvcc(true);
      let mut kite = Kite::open(&path, options).expect("open kite");
      let mut props = HashMap::new();
      props.insert("name".to_string(), PropValue::String("Alice".into()));
      let alice = kite
        .create_node("User", "alice", props)
        .expect("create alice");
      kite
        .update_by_key("User", "alice")
        .expect("update_by_key")
        .set("name", PropValue::String("Alice Updated".into()))
        .execute()
        .expect("execute update");
      let name = kite.prop(alice.id(), "name");
      kite.close().expect("close kite");
      name
    },
  );
  assert_eq!(name, Some(PropValue::String("Alice Updated".into())));
}

// ============================================================================
// M5: SOA chain truncation frees the version the oldest reader needs
// ============================================================================

#[test]
fn audit_m5_reader_keeps_snapshot_value_after_deep_chain_truncation() {
  const MAX_CHAIN_DEPTH: usize = 10;
  const UPDATES: i64 = 12;

  let dir = tempfile::tempdir().expect("tempdir");
  let db = Arc::new(
    open_single_file(
      dir.path().join("m5.kitedb"),
      base_options()
        .mvcc_gc_interval_ms(20)
        .mvcc_max_chain_depth(MAX_CHAIN_DEPTH),
    )
    .expect("open db"),
  );
  let graph = seed(&db);
  let (hub, prop) = (graph.hub, graph.prop);

  let (ready_tx, ready_rx) = mpsc::channel();
  let (go_tx, go_rx) = mpsc::channel::<()>();
  let (result_tx, result_rx) = mpsc::channel();
  let reader_db = Arc::clone(&db);
  let reader = thread::spawn(move || {
    reader_db.begin(true).expect("begin read tx");
    let before = reader_db.node_prop(hub, prop);
    ready_tx.send(()).expect("reader ready");
    go_rx.recv().expect("go");
    let after = reader_db.node_prop(hub, prop);
    reader_db.commit().expect("commit read tx");
    drop(reader_db);
    let _ = result_tx.send((before, after));
  });
  ready_rx
    .recv_timeout(DEADLOCK_TIMEOUT)
    .expect("reader started");

  // More committed versions than max_chain_depth while the reader is open.
  for value in 1..=UPDATES {
    db.begin(false).expect("begin write tx");
    db.set_node_prop(hub, prop, PropValue::I64(value))
      .expect("set prop");
    db.commit().expect("commit write tx");
  }

  // Wait for two full GC cycles after the last commit (prune + truncate).
  let gc_runs = || {
    collect_metrics_single_file(&db)
      .mvcc
      .expect("mvcc metrics")
      .gc_runs
  };
  let target = gc_runs() + 2;
  let deadline = Instant::now() + DEADLOCK_TIMEOUT;
  while gc_runs() < target {
    assert!(Instant::now() < deadline, "GC did not run");
    thread::sleep(Duration::from_millis(5));
  }

  go_tx.send(()).expect("send go");
  let (before, after) = result_rx
    .recv_timeout(DEADLOCK_TIMEOUT)
    .expect("reader finished");
  reader.join().expect("reader thread");

  assert_eq!(before, Some(PropValue::I64(0)), "reader snapshot value");
  assert_eq!(
    after,
    Some(PropValue::I64(0)),
    "reader snapshot must not change after GC truncates a chain deeper than max_chain_depth"
  );
  close_shared(db);
}
