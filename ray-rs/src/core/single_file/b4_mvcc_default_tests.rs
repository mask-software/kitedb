//! raydb-b4 `mvcc-default` lane: MVCC as the default, and bulk loads under
//! MVCC. Included from transaction.rs for private access.
//!
//! A bulk load under MVCC is exclusive among writers: it waits for the open
//! write transactions, and write transactions that begin while it is open or
//! waiting wait for it. Readers never wait for it. Reads outside a
//! transaction see it whole once it commits; a read transaction that began
//! before its commit never sees it (snapshot isolation).
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use tempfile::tempdir;

use crate::api::kite::KiteOptions;
use crate::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions, SyncMode,
};
use crate::types::{ETypeId, LabelId, NodeId, PropKeyId, PropValue};

/// How long a step that must not block gets; it takes milliseconds.
const DEADLINE: Duration = Duration::from_secs(20);

fn mvcc_options() -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .mvcc(true)
    .mvcc_gc_interval_ms(10)
    .sync_mode(SyncMode::Normal)
    .auto_checkpoint(false)
}

fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
  let deadline = Instant::now() + DEADLINE;
  while !condition() {
    assert!(Instant::now() < deadline, "timed out waiting for {what}");
    std::thread::sleep(Duration::from_millis(1));
  }
}

/// Run `f` on its own thread; fail if it does not finish within `DEADLINE`
/// (it blocked on something it must not wait for).
fn within<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
  let (done_tx, done_rx) = mpsc::channel();
  std::thread::spawn(move || {
    let _ = done_tx.send(f());
  });
  done_rx
    .recv_timeout(DEADLINE)
    .unwrap_or_else(|_| panic!("{what} did not finish: it blocked"))
}

fn commit_node(db: &SingleFileDB, key: &str) -> NodeId {
  db.begin(false).expect("begin");
  let node = db.create_node(Some(key)).expect("create");
  db.commit().expect("commit");
  node
}

/// MVCC is the default everywhere a default lives, and a file written without
/// MVCC opens under it: MVCC is runtime state, the file format is the same.
#[test]
fn mvcc_is_the_default_and_opens_files_written_without_it() {
  assert!(
    SingleFileOpenOptions::new().mvcc,
    "SingleFileOpenOptions::new()"
  );
  assert!(
    SingleFileOpenOptions::default().mvcc,
    "SingleFileOpenOptions::default()"
  );
  assert!(KiteOptions::new().mvcc, "KiteOptions::new()");
  for profile in [
    KiteOptions::recommended_safe(),
    KiteOptions::recommended_balanced(),
    KiteOptions::recommended_reopen_heavy(),
  ] {
    assert!(profile.mvcc, "recommended profiles use MVCC");
  }

  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("written-without-mvcc.kitedb");
  let plain = SingleFileOpenOptions::new()
    .mvcc(false)
    .sync_mode(SyncMode::Normal);
  let db = open_single_file(&path, plain.clone()).expect("open without mvcc");
  assert!(!db.mvcc_enabled());
  db.begin(false).expect("begin");
  let etype = db.define_etype("links").expect("etype");
  let weight = db.define_propkey("weight").expect("propkey");
  let a = db.create_node(Some("a")).expect("a");
  let b = db.create_node(Some("b")).expect("b");
  db.add_edge(a, etype, b).expect("edge");
  db.set_node_prop(a, weight, PropValue::I64(1))
    .expect("prop");
  db.commit().expect("commit");
  db.checkpoint().expect("checkpoint");
  // And some state only in the WAL.
  let c = commit_node(&db, "c");
  close_single_file(db).expect("close");

  let db = open_single_file(&path, SingleFileOpenOptions::new()).expect("open with the default");
  assert!(db.mvcc_enabled(), "the default opens with MVCC");
  assert_eq!(db.node_by_key("a"), Some(a));
  assert_eq!(db.node_by_key("c"), Some(c));
  assert!(db.edge_exists(a, etype, b));
  assert_eq!(db.node_prop(a, weight), Some(PropValue::I64(1)));
  db.begin(false).expect("begin");
  db.set_node_prop(b, weight, PropValue::I64(2))
    .expect("prop");
  db.commit().expect("commit");
  close_single_file(db).expect("close");

  let db = open_single_file(&path, plain).expect("reopen without mvcc");
  assert_eq!(db.node_prop(b, weight), Some(PropValue::I64(2)));
  assert_eq!(db.count_nodes(), 3);
}

/// A bulk load commits under MVCC, durably.
#[test]
fn bulk_load_commits_under_mvcc() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("bulk-mvcc.kitedb");
  let db = open_single_file(&path, mvcc_options()).expect("open");
  db.begin_bulk().expect("bulk load under MVCC");
  let etype = db.define_etype("links").expect("etype");
  let weight = db.define_propkey("weight").expect("propkey");
  let label = db.define_label("Item").expect("label");
  let keys: Vec<String> = (0..100).map(|i| format!("n{i}")).collect();
  let key_refs: Vec<Option<&str>> = keys.iter().map(|key| Some(key.as_str())).collect();
  let nodes = db.create_nodes_batch(&key_refs).expect("nodes");
  for (i, &node) in nodes.iter().enumerate() {
    db.set_node_prop(node, weight, PropValue::I64(i as i64))
      .expect("prop");
    db.add_node_label(node, label).expect("label");
  }
  let edges = nodes
    .windows(2)
    .map(|pair| (pair[0], etype, pair[1], vec![(weight, PropValue::I64(7))]))
    .collect();
  db.add_edges_with_props_batch(edges).expect("edges");
  // The bulk load sees its own writes.
  assert_eq!(db.node_by_key("n5"), Some(nodes[5]));
  db.commit().expect("commit");

  let check = |db: &SingleFileDB, context: &str| {
    assert_eq!(db.count_nodes(), 100, "{context}");
    assert_eq!(db.count_edges(), 99, "{context}");
    assert_eq!(db.node_by_key("n99"), Some(nodes[99]), "{context}");
    assert_eq!(
      db.node_prop(nodes[42], weight),
      Some(PropValue::I64(42)),
      "{context}"
    );
    assert!(db.node_has_label(nodes[3], label), "{context}");
    assert_eq!(
      db.edge_prop(nodes[0], etype, nodes[1], weight),
      Some(PropValue::I64(7)),
      "{context}"
    );
  };
  check(&db, "live");
  close_single_file(db).expect("close");
  check(
    &open_single_file(&path, mvcc_options()).expect("reopen"),
    "reopened",
  );
}

/// A bulk load records nothing for MVCC conflict checks: no other write
/// transaction is open beside it to conflict with.
#[test]
fn bulk_load_records_no_conflict_keys() {
  let dir = tempdir().expect("tempdir");
  let db = open_single_file(dir.path().join("bulk-keys.kitedb"), mvcc_options()).expect("open");
  let existing = commit_node(&db, "existing");
  db.begin(false).expect("begin");
  let weight = db.define_propkey("weight").expect("propkey");
  let etype = db.define_etype("links").expect("etype");
  db.commit().expect("commit");

  db.begin_bulk().expect("bulk load under MVCC");
  let nodes = db
    .create_nodes_batch(&[Some("a"), Some("b")])
    .expect("nodes");
  db.set_node_prop(existing, weight, PropValue::I64(1))
    .expect("prop");
  db.add_edges_batch(&[(nodes[0], etype, nodes[1]), (existing, etype, nodes[0])])
    .expect("edges");
  // Reads and no-op writes inside it note nothing either.
  let _ = db.node_prop(existing, weight);
  let _ = db.out_edges(existing);
  db.add_edges_batch(&[(nodes[0], etype, nodes[1])])
    .expect("existing edge");
  {
    let handle = db.current_tx_handle().expect("bulk transaction");
    let tx = handle.lock();
    assert!(tx.mvcc_reads.is_empty(), "reads noted: {:?}", tx.mvcc_reads);
    assert!(
      tx.mvcc_writes.is_empty(),
      "writes noted: {:?}",
      tx.mvcc_writes
    );
  }
  db.commit().expect("commit");
}

/// A bulk load waits for the write transactions open when it begins, and
/// then sees their commits.
#[test]
fn bulk_load_waits_for_open_write_transactions() {
  let dir = tempdir().expect("tempdir");
  let db =
    Arc::new(open_single_file(dir.path().join("bulk-waits.kitedb"), mvcc_options()).expect("open"));

  let (open_tx, open_rx) = mpsc::channel();
  let (go_tx, go_rx) = mpsc::channel::<()>();
  let writer = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || {
      db.begin(false).expect("writer begin");
      db.create_node(Some("writer")).expect("writer node");
      open_tx.send(()).expect("open");
      go_rx.recv().expect("go");
      db.commit()
    })
  };
  open_rx.recv().expect("writer open");

  let bulk_begun = Arc::new(AtomicBool::new(false));
  let bulk = {
    let db = Arc::clone(&db);
    let bulk_begun = Arc::clone(&bulk_begun);
    std::thread::spawn(move || -> crate::error::Result<bool> {
      db.begin_bulk()?;
      bulk_begun.store(true, Ordering::SeqCst);
      let sees_writer = db.node_by_key("writer").is_some();
      db.create_nodes_batch(&[Some("bulk")])?;
      db.commit()?;
      Ok(sees_writer)
    })
  };
  wait_until("the bulk load to wait for the writer", || {
    db.tx_shared.writer.waiting() == 1 || bulk.is_finished()
  });
  assert!(
    !bulk_begun.load(Ordering::SeqCst),
    "the bulk load began beside an open write transaction"
  );
  go_tx.send(()).expect("release writer");
  writer.join().expect("writer").expect("writer commit");
  let sees_writer = bulk.join().expect("bulk").expect("bulk load");
  assert!(
    sees_writer,
    "the bulk load began before the writer committed"
  );
  assert!(db.node_by_key("writer").is_some());
  assert!(db.node_by_key("bulk").is_some());
}

/// Write transactions that begin while a bulk load is open wait for it, and
/// then see it.
#[test]
fn write_transactions_wait_for_an_open_bulk_load() {
  let dir = tempdir().expect("tempdir");
  let db = Arc::new(
    open_single_file(dir.path().join("writer-waits.kitedb"), mvcc_options()).expect("open"),
  );
  db.begin_bulk().expect("bulk load under MVCC");
  db.create_nodes_batch(&[Some("bulk")]).expect("bulk node");

  let writer_begun = Arc::new(AtomicBool::new(false));
  let writer = {
    let db = Arc::clone(&db);
    let writer_begun = Arc::clone(&writer_begun);
    std::thread::spawn(move || -> crate::error::Result<bool> {
      db.begin(false)?;
      writer_begun.store(true, Ordering::SeqCst);
      let sees_bulk = db.node_by_key("bulk").is_some();
      db.create_node(Some("writer"))?;
      db.commit()?;
      Ok(sees_bulk)
    })
  };
  wait_until("the writer to wait for the bulk load", || {
    db.tx_shared.writer.waiting() == 1 || writer.is_finished()
  });
  assert!(
    !writer_begun.load(Ordering::SeqCst),
    "a write transaction began beside an open bulk load"
  );
  db.commit().expect("bulk commit");
  let sees_bulk = writer.join().expect("writer").expect("writer");
  assert!(sees_bulk, "the writer began before the bulk load committed");
  assert!(db.node_by_key("writer").is_some());
}

/// A bulk load waiting for an open writer goes before write transactions that
/// begin after it: a steady stream of writers cannot starve it.
#[test]
fn waiting_bulk_load_goes_before_later_writers() {
  let dir = tempdir().expect("tempdir");
  let db =
    Arc::new(open_single_file(dir.path().join("bulk-first.kitedb"), mvcc_options()).expect("open"));
  let order = Arc::new(StdMutex::new(Vec::new()));

  let (open_tx, open_rx) = mpsc::channel();
  let (go_tx, go_rx) = mpsc::channel::<()>();
  let first = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || {
      db.begin(false).expect("first begin");
      open_tx.send(()).expect("open");
      go_rx.recv().expect("go");
      db.commit()
    })
  };
  open_rx.recv().expect("first open");

  let spawn_writer = |name: &'static str, bulk: bool| {
    let db = Arc::clone(&db);
    let order = Arc::clone(&order);
    std::thread::spawn(move || -> crate::error::Result<()> {
      if bulk {
        db.begin_bulk()?;
      } else {
        db.begin(false)?;
      }
      order.lock().expect("order").push(name);
      db.create_nodes_batch(&[Some(name)])?;
      db.commit()
    })
  };
  let bulk = spawn_writer("bulk", true);
  wait_until("the bulk load to wait", || {
    db.tx_shared.writer.waiting() == 1 || bulk.is_finished()
  });
  let later = spawn_writer("later", false);
  wait_until("the later writer to wait", || {
    db.tx_shared.writer.waiting() == 2 || later.is_finished()
  });
  go_tx.send(()).expect("release first");
  first.join().expect("first").expect("first commit");
  bulk.join().expect("bulk").expect("bulk load");
  later.join().expect("later").expect("later writer");
  assert_eq!(*order.lock().expect("order"), vec!["bulk", "later"]);
}

struct Graph {
  a: NodeId,
  b: NodeId,
  etype: ETypeId,
  weight: PropKeyId,
  label: LabelId,
}

/// What a reader that began before the bulk load must keep seeing.
fn assert_before_bulk(db: &SingleFileDB, graph: &Graph, context: &str) {
  assert_eq!(db.count_nodes(), 2, "{context}: count_nodes");
  assert_eq!(db.list_nodes().len(), 2, "{context}: list_nodes");
  assert_eq!(db.count_edges(), 1, "{context}: count_edges");
  assert_eq!(db.node_by_key("n0"), None, "{context}: new key");
  assert_eq!(db.node_by_key("b"), Some(graph.b), "{context}: deleted key");
  assert_eq!(
    db.node_prop(graph.a, graph.weight),
    Some(PropValue::I64(1)),
    "{context}: changed prop"
  );
  assert!(
    !db.node_has_label(graph.a, graph.label),
    "{context}: added label"
  );
  assert_eq!(
    db.out_edges(graph.a),
    vec![(graph.etype, graph.b)],
    "{context}: out_edges"
  );
  assert!(db.node_exists(graph.b), "{context}: deleted node");
}

/// What a reader that began after the bulk load commits sees.
fn assert_after_bulk(db: &SingleFileDB, graph: &Graph, context: &str) {
  assert_eq!(db.count_nodes(), 101, "{context}: count_nodes");
  assert_eq!(db.list_nodes().len(), 101, "{context}: list_nodes");
  assert_eq!(db.count_edges(), 100, "{context}: count_edges");
  assert!(db.node_by_key("n0").is_some(), "{context}: new key");
  assert_eq!(db.node_by_key("b"), None, "{context}: deleted key");
  assert_eq!(
    db.node_prop(graph.a, graph.weight),
    Some(PropValue::I64(2)),
    "{context}: changed prop"
  );
  assert!(
    db.node_has_label(graph.a, graph.label),
    "{context}: added label"
  );
  assert!(!db.node_exists(graph.b), "{context}: deleted node");
}

/// A read transaction that began before a bulk load commits never sees it,
/// whether it began before the bulk load or while it was open; one that
/// begins after sees it whole, as do reads outside a transaction. Readers
/// never wait for the bulk load, nor it for them.
#[test]
fn read_transactions_keep_their_snapshot_across_a_bulk_load() {
  let dir = tempdir().expect("tempdir");
  let db = Arc::new(
    open_single_file(dir.path().join("bulk-readers.kitedb"), mvcc_options()).expect("open"),
  );
  db.begin(false).expect("begin");
  let etype = db.define_etype("links").expect("etype");
  let weight = db.define_propkey("weight").expect("propkey");
  let label = db.define_label("Hub").expect("label");
  let a = db.create_node(Some("a")).expect("a");
  let b = db.create_node(Some("b")).expect("b");
  db.add_edge(a, etype, b).expect("edge");
  db.set_node_prop(a, weight, PropValue::I64(1))
    .expect("prop");
  db.commit().expect("commit");
  let graph = Arc::new(Graph {
    a,
    b,
    etype,
    weight,
    label,
  });

  // A reader opens, signals, waits for the go, checks its view, and ends.
  let spawn_reader = |context: &'static str| {
    let (open_tx, open_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let db = Arc::clone(&db);
    let graph = Arc::clone(&graph);
    let reader = std::thread::spawn(move || {
      db.begin(true).expect("reader begin");
      assert_before_bulk(&db, &graph, &format!("{context}, before the commit"));
      open_tx.send(()).expect("open");
      go_rx.recv().expect("go");
      assert_before_bulk(&db, &graph, &format!("{context}, after the commit"));
      db.rollback().expect("reader end");
    });
    open_rx
      .recv_timeout(DEADLINE)
      .expect("a read transaction waited for the bulk load");
    (go_tx, reader)
  };

  let (early_go, early) = spawn_reader("reader begun before the bulk load");
  let (bulk_open_tx, bulk_open_rx) = mpsc::channel();
  let (bulk_go_tx, bulk_go_rx) = mpsc::channel::<()>();
  let bulk = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || -> crate::error::Result<()> {
      db.begin_bulk()?;
      bulk_open_tx.send(()).expect("bulk open");
      bulk_go_rx.recv().expect("bulk go");
      let keys: Vec<String> = (0..100).map(|i| format!("n{i}")).collect();
      let key_refs: Vec<Option<&str>> = keys.iter().map(|key| Some(key.as_str())).collect();
      let nodes = db.create_nodes_batch(&key_refs)?;
      let mut edges = vec![(a, etype, nodes[0])];
      edges.extend(nodes.windows(2).map(|pair| (pair[0], etype, pair[1])));
      db.add_edges_batch(&edges)?;
      db.set_node_prop(a, weight, PropValue::I64(2))?;
      db.add_node_label(a, label)?;
      db.delete_node(b)?;
      db.commit()
    })
  };
  bulk_open_rx
    .recv_timeout(DEADLINE)
    .expect("the bulk load waited for a read transaction");
  let (during_go, during) = spawn_reader("reader begun during the bulk load");
  bulk_go_tx.send(()).expect("bulk go");
  bulk.join().expect("bulk").expect("bulk load");

  assert_after_bulk(&db, &graph, "outside a transaction");
  {
    let db = Arc::clone(&db);
    let graph = Arc::clone(&graph);
    within("a reader after the bulk load", move || {
      db.begin(true).expect("begin");
      assert_after_bulk(&db, &graph, "reader begun after the commit");
      db.rollback().expect("end");
    });
  }

  early_go.send(()).expect("go");
  during_go.send(()).expect("go");
  early.join().expect("early reader");
  during.join().expect("reader during the bulk load");
}

/// Reads outside a transaction see a bulk load whole or not at all: not
/// once its commit is durable but not yet merged.
#[test]
fn reads_outside_transactions_see_a_bulk_load_whole_or_not_at_all() {
  let dir = tempdir().expect("tempdir");
  let db =
    Arc::new(open_single_file(dir.path().join("bulk-whole.kitedb"), mvcc_options()).expect("open"));
  commit_node(&db, "before");
  db.begin_bulk().expect("bulk load under MVCC");
  let keys: Vec<String> = (0..1000).map(|i| format!("n{i}")).collect();
  let key_refs: Vec<Option<&str>> = keys.iter().map(|key| Some(key.as_str())).collect();
  db.create_nodes_batch(&key_refs).expect("nodes");
  let seen = Arc::new(StdMutex::new(None));
  {
    let db = Arc::clone(&db);
    let seen = Arc::clone(&seen);
    super::BEFORE_NEXT_COMMIT_MERGE.with(|hook| {
      *hook.borrow_mut() = Some(Box::new(move || {
        let view = std::thread::spawn(move || {
          (
            db.count_nodes(),
            db.node_by_key("n0").is_some(),
            db.node_by_key("n999").is_some(),
          )
        })
        .join()
        .expect("reader");
        *seen.lock().expect("seen") = Some(view);
      }));
    });
  }
  db.commit().expect("commit");
  assert_eq!(
    *seen.lock().expect("seen"),
    Some((1, false, false)),
    "a read saw part of the bulk load before it was merged"
  );
  assert_eq!(db.count_nodes(), 1001);
  assert!(db.node_by_key("n0").is_some() && db.node_by_key("n999").is_some());
}

/// A rolled-back bulk load leaves nothing, and writers go on.
#[test]
fn rolled_back_bulk_load_lets_writers_go_on() {
  let dir = tempdir().expect("tempdir");
  let db = Arc::new(
    open_single_file(dir.path().join("bulk-rollback.kitedb"), mvcc_options()).expect("open"),
  );
  db.begin_bulk().expect("bulk load under MVCC");
  db.create_nodes_batch(&[Some("rolled-back")]).expect("node");
  db.rollback().expect("rollback");
  assert!(db.node_by_key("rolled-back").is_none());
  let writer_db = Arc::clone(&db);
  within("a writer after the rolled-back bulk load", move || {
    commit_node(&writer_db, "after");
  });
  assert!(db.node_by_key("after").is_some());
}

/// A thread that ends inside its bulk load does not keep writers waiting.
#[test]
fn thread_ending_inside_a_bulk_load_lets_writers_go_on() {
  let dir = tempdir().expect("tempdir");
  let db = Arc::new(
    open_single_file(dir.path().join("bulk-abandoned.kitedb"), mvcc_options()).expect("open"),
  );
  {
    let db = Arc::clone(&db);
    std::thread::spawn(move || {
      db.begin_bulk().expect("bulk load under MVCC");
      db.create_nodes_batch(&[Some("abandoned")]).expect("node");
    })
    .join()
    .expect("bulk thread");
  }
  let writer_db = Arc::clone(&db);
  within("a writer after an abandoned bulk load", move || {
    commit_node(&writer_db, "after");
  });
  assert!(db.node_by_key("abandoned").is_none());
  assert!(db.node_by_key("after").is_some());
}

/// A read-only open commits nothing, so MVCC has nothing to collect: it
/// starts no GC thread (one thread per read-only handle, started and joined
/// on every open and close). Writable opens run it.
#[test]
fn read_only_open_starts_no_gc_thread() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("read-only-gc.kitedb");
  let db = open_single_file(&path, SingleFileOpenOptions::new()).expect("open");
  let mvcc = db.mvcc.as_ref().expect("MVCC is the default");
  assert!(mvcc.gc_thread_running(), "a writable open runs GC");
  commit_node(&db, "a");
  close_single_file(db).expect("close");

  let read_only =
    open_single_file(&path, SingleFileOpenOptions::new().read_only(true)).expect("open read-only");
  assert!(read_only.mvcc_enabled());
  assert!(
    !read_only
      .mvcc
      .as_ref()
      .expect("MVCC is the default")
      .gc_thread_running(),
    "a read-only open started a GC thread"
  );
  read_only.begin(true).expect("read transaction");
  assert!(read_only.node_by_key("a").is_some());
  read_only.rollback().expect("end");
  close_single_file(read_only).expect("close read-only");
}
