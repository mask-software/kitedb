//! raydb-b4 `mvcc` lane, Phase 2 (finding 4: what MVCC costs).
//!
//! - Reads outside a transaction, and inside one whose snapshot no newer history
//!   affects, need neither the transaction manager nor the version chains.
//! - Read-only transactions never validate, so they record no reads; write
//!   transactions still conflict on what they read.
//! - A commit records history only for state an older snapshot could see: not
//!   for the props, labels, keys and edges of nodes it creates.
//! - GC does not hold the transaction manager while it prunes chains, keeps
//!   history only for open transactions by default, and its wall clock map
//!   stays small.
//! - Reading a node's props in an old snapshot does not scan every chain.

use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::{tempdir, TempDir};

use crate::core::single_file::{open_single_file, SingleFileDB, SingleFileOpenOptions, SyncMode};
use crate::error::KiteError;
use crate::mvcc::version_chain::KEYS_EXAMINED;
use crate::mvcc::{GcConfig, MvccManager, TxManager};
use crate::types::{ETypeId, LabelId, NodeId, PropKeyId, PropValue, TxKey};

/// How long a read may take while another thread holds the MVCC locks.
const UNBLOCKED: Duration = Duration::from_secs(2);

struct Graph {
  a: NodeId,
  b: NodeId,
  prop: PropKeyId,
  edge_prop: PropKeyId,
  label: LabelId,
  etype: ETypeId,
}

fn open_mvcc() -> (TempDir, Arc<SingleFileDB>) {
  let dir = tempdir().expect("tempdir");
  let options = SingleFileOpenOptions::new()
    .mvcc(true)
    .auto_checkpoint(false)
    .background_checkpoint(false)
    .sync_mode(SyncMode::Off);
  let db = open_single_file(dir.path().join("p2.kitedb"), options).expect("open");
  (dir, Arc::new(db))
}

fn seed(db: &SingleFileDB) -> Graph {
  db.begin(false).expect("begin seed");
  let a = db.create_node(Some("a")).expect("a");
  let b = db.create_node(Some("b")).expect("b");
  let prop = db.define_propkey("p").expect("propkey");
  let edge_prop = db.define_propkey("w").expect("edge propkey");
  let label = db.define_label("L").expect("label");
  let etype = db.define_etype("T").expect("etype");
  db.set_node_prop(a, prop, PropValue::I64(1)).expect("prop");
  db.add_node_label(a, label).expect("label");
  db.add_edge(a, etype, b).expect("edge");
  db.set_edge_prop(a, etype, b, edge_prop, PropValue::I64(7))
    .expect("edge prop");
  db.commit().expect("commit seed");
  Graph {
    a,
    b,
    prop,
    edge_prop,
    label,
    etype,
  }
}

/// Every read API, once.
fn read_everything(db: &SingleFileDB, g: &Graph) -> usize {
  let mut seen = 0;
  seen += db.node_prop(g.a, g.prop).is_some() as usize;
  seen += db.node_props(g.a).map_or(0, |props| props.len());
  seen += db.out_edges(g.a).len();
  seen += db.in_edges(g.b).len();
  seen += db.edge_prop(g.a, g.etype, g.b, g.edge_prop).is_some() as usize;
  seen += db
    .edge_props(g.a, g.etype, g.b)
    .map_or(0, |props| props.len());
  seen += db.node_labels(g.a).len();
  seen += db.node_has_label(g.a, g.label) as usize;
  seen += db.node_by_key("a").is_some() as usize;
  seen += db.node_key(g.a).is_some() as usize;
  seen += db.node_exists(g.a) as usize;
  seen += db.edge_exists(g.a, g.etype, g.b) as usize;
  seen += db.list_nodes().len();
  seen += db.list_edges(None).len();
  seen
}

/// Holds the transaction manager and the version chains exclusively on its own
/// thread until dropped.
struct MvccLocksHeld {
  release: Option<mpsc::Sender<()>>,
  handle: Option<thread::JoinHandle<()>>,
}

impl MvccLocksHeld {
  fn hold(db: &Arc<SingleFileDB>) -> Self {
    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let db = Arc::clone(db);
    let handle = thread::spawn(move || {
      let mvcc = db.mvcc.as_ref().expect("mvcc");
      let _tx_manager = mvcc.tx_manager.lock();
      let _version_chain = mvcc.version_chain.write();
      held_tx.send(()).expect("held");
      let _ = release_rx.recv();
    });
    held_rx.recv().expect("locks held");
    Self {
      release: Some(release_tx),
      handle: Some(handle),
    }
  }
}

impl Drop for MvccLocksHeld {
  fn drop(&mut self) {
    drop(self.release.take());
    if let Some(handle) = self.handle.take() {
      let _ = handle.join();
    }
  }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum ReaderTx {
  None,
  ReadOnly,
  Write,
}

/// Whether `read_everything` finishes while another thread holds the MVCC
/// locks, in a transaction of kind `tx` begun before they were taken.
fn reads_finish_while_mvcc_locks_held(tx: ReaderTx) -> bool {
  let (_dir, db) = open_mvcc();
  let graph = Arc::new(seed(&db));
  let (begun_tx, begun_rx) = mpsc::channel::<()>();
  let (go_tx, go_rx) = mpsc::channel::<()>();
  let (done_tx, done_rx) = mpsc::channel::<usize>();
  let reader = {
    let db = Arc::clone(&db);
    let graph = Arc::clone(&graph);
    thread::spawn(move || {
      match tx {
        ReaderTx::None => {}
        ReaderTx::ReadOnly => {
          db.begin(true).expect("begin read-only");
        }
        ReaderTx::Write => {
          db.begin(false).expect("begin write");
        }
      }
      begun_tx.send(()).expect("begun");
      go_rx.recv().expect("go");
      done_tx.send(read_everything(&db, &graph)).expect("done");
      if tx != ReaderTx::None {
        db.rollback().expect("end reader");
      }
    })
  };
  begun_rx.recv().expect("reader began");
  let held = MvccLocksHeld::hold(&db);
  go_tx.send(()).expect("go");
  let finished = done_rx.recv_timeout(UNBLOCKED).is_ok();
  drop(held);
  reader.join().expect("reader thread");
  finished
}

#[test]
fn reads_outside_a_transaction_take_no_mvcc_lock() {
  assert!(
    reads_finish_while_mvcc_locks_held(ReaderTx::None),
    "reads outside a transaction waited for the MVCC locks"
  );
}

#[test]
fn read_only_transaction_reads_take_no_mvcc_lock_without_newer_history() {
  assert!(
    reads_finish_while_mvcc_locks_held(ReaderTx::ReadOnly),
    "reads in a read-only transaction waited for the MVCC locks"
  );
}

#[test]
fn write_transaction_reads_take_no_mvcc_lock_without_newer_history() {
  assert!(
    reads_finish_while_mvcc_locks_held(ReaderTx::Write),
    "reads in a write transaction waited for the MVCC locks"
  );
}

#[test]
fn read_only_transactions_record_no_reads() {
  let (_dir, db) = open_mvcc();
  let graph = seed(&db);
  let txid = db.begin(true).expect("begin");
  assert!(read_everything(&db, &graph) > 0);
  let recorded = {
    let mvcc = db.mvcc.as_ref().expect("mvcc");
    let tx_mgr = mvcc.tx_manager.lock();
    let tx = tx_mgr.tx(txid).expect("open transaction");
    tx.read_set.len()
  };
  db.rollback().expect("end");
  assert_eq!(recorded, 0, "a read-only transaction recorded reads");
}

/// Guard: reads in a write transaction still make it conflict with a
/// concurrent commit of what it read.
#[test]
fn write_transaction_reads_still_conflict() {
  let (_dir, db) = open_mvcc();
  let graph = seed(&db);

  db.begin(false).expect("begin reader-writer");
  assert_eq!(db.node_prop(graph.a, graph.prop), Some(PropValue::I64(1)));
  assert_eq!(db.out_edges(graph.a).len(), 1);

  let other = {
    let db = Arc::clone(&db);
    let (a, prop) = (graph.a, graph.prop);
    thread::spawn(move || {
      db.begin(false).expect("begin other");
      db.set_node_prop(a, prop, PropValue::I64(2)).expect("set");
      db.commit()
    })
  };
  other.join().expect("other thread").expect("other commit");

  db.set_node_prop(graph.b, graph.prop, PropValue::I64(3))
    .expect("write elsewhere");
  let result = db.commit();
  assert!(
    matches!(&result, Err(KiteError::Conflict { keys, .. }) if keys.iter().any(|k| k.starts_with("nodeprop:"))),
    "the read of a concurrently written prop must conflict, got {result:?}"
  );
}

/// What an old snapshot sees of the nodes a later commit creates.
#[derive(Debug, PartialEq)]
struct OldView {
  nodes: Vec<NodeId>,
  a_out: Vec<(ETypeId, NodeId)>,
  a_in: Vec<(ETypeId, NodeId)>,
  new_by_key: Option<NodeId>,
  new_exists: bool,
  new_props: Option<usize>,
  new_prop: Option<PropValue>,
  new_labels: Vec<LabelId>,
  edge_to_new: bool,
  edge_from_new: bool,
  edge_from_new_prop: Option<PropValue>,
}

/// Holds a read transaction on its own thread; `view` asks it what it sees.
struct OldReader {
  ask: Option<mpsc::Sender<NodeId>>,
  answers: mpsc::Receiver<OldView>,
  handle: Option<thread::JoinHandle<()>>,
}

impl OldReader {
  fn open(db: &Arc<SingleFileDB>, g: &Graph) -> Self {
    let (ready_tx, ready_rx) = mpsc::channel();
    let (ask_tx, ask_rx) = mpsc::channel::<NodeId>();
    let (answer_tx, answer_rx) = mpsc::channel();
    let db = Arc::clone(db);
    let (a, prop, edge_prop, etype) = (g.a, g.prop, g.edge_prop, g.etype);
    let handle = thread::spawn(move || {
      db.begin(true).expect("begin old reader");
      ready_tx.send(()).expect("ready");
      while let Ok(new) = ask_rx.recv() {
        let mut nodes = db.list_nodes();
        nodes.sort_unstable();
        let view = OldView {
          nodes,
          a_out: db.out_edges(a),
          a_in: db.in_edges(a),
          new_by_key: db.node_by_key("new-0"),
          new_exists: db.node_exists(new),
          new_props: db.node_props(new).map(|props| props.len()),
          new_prop: db.node_prop(new, prop),
          new_labels: db.node_labels(new),
          edge_to_new: db.edge_exists(a, etype, new),
          edge_from_new: db.edge_exists(new, etype, a),
          edge_from_new_prop: db.edge_prop(new, etype, a, edge_prop),
        };
        answer_tx.send(view).expect("answer");
      }
      db.rollback().expect("end old reader");
    });
    ready_rx.recv().expect("old reader began");
    Self {
      ask: Some(ask_tx),
      answers: answer_rx,
      handle: Some(handle),
    }
  }

  fn view(&self, new: NodeId) -> OldView {
    self.ask.as_ref().expect("open").send(new).expect("ask");
    self.answers.recv().expect("view")
  }
}

impl Drop for OldReader {
  fn drop(&mut self) {
    drop(self.ask.take());
    if let Some(handle) = self.handle.take() {
      let _ = handle.join();
    }
  }
}

#[test]
fn commits_record_no_history_for_the_nodes_they_create() {
  const CREATED: usize = 50;
  let (_dir, db) = open_mvcc();
  let g = seed(&db);
  let old = OldReader::open(&db, &g);
  let before = old.view(NodeId::MAX);

  db.begin(false).expect("begin creator");
  let mut created = Vec::new();
  for i in 0..CREATED {
    let node = db.create_node(Some(&format!("new-{i}"))).expect("create");
    db.set_node_prop(node, g.prop, PropValue::I64(i as i64))
      .expect("prop");
    db.set_node_prop(node, g.edge_prop, PropValue::I64(-1))
      .expect("second prop");
    db.add_node_label(node, g.label).expect("label");
    db.add_edge(g.a, g.etype, node).expect("edge to new");
    db.add_edge(node, g.etype, g.a).expect("edge from new");
    db.set_edge_prop(node, g.etype, g.a, g.edge_prop, PropValue::I64(9))
      .expect("edge prop");
    created.push(node);
  }
  db.commit().expect("commit creator");

  let counts = db
    .mvcc
    .as_ref()
    .expect("mvcc")
    .version_chain
    .read()
    .counts();
  let after = old.view(created[0]);
  drop(old);

  assert_eq!(
    after,
    OldView {
      new_exists: false,
      ..before
    },
    "the old snapshot sees part of the nodes created after it"
  );
  assert_eq!(
    (
      counts.node_versions,
      counts.node_prop_versions,
      counts.node_label_versions,
      counts.key_owner_versions,
      counts.edge_versions,
      counts.edge_prop_versions,
    ),
    (CREATED, 0, 0, 0, 0, 0),
    "history recorded for {CREATED} created nodes (node, prop, label, key, edge, edge prop chains)"
  );
}

#[test]
fn gc_does_not_hold_the_tx_manager_while_pruning() {
  let mvcc = Arc::new(MvccManager::new(1, 1, GcConfig::default()));
  let (held_tx, held_rx) = mpsc::channel();
  let (release_tx, release_rx) = mpsc::channel::<()>();
  let holder = {
    let mvcc = Arc::clone(&mvcc);
    thread::spawn(move || {
      let _version_chain = mvcc.version_chain.write();
      held_tx.send(()).expect("held");
      let _ = release_rx.recv();
    })
  };
  held_rx.recv().expect("version chains held");

  // `start` runs one GC pass right away; it blocks on the version chains.
  let starter = {
    let mvcc = Arc::clone(&mvcc);
    thread::spawn(move || mvcc.start())
  };
  thread::sleep(Duration::from_millis(200));
  let deadline = Instant::now() + Duration::from_secs(1);
  let mut tx_manager_free = false;
  while Instant::now() < deadline {
    if mvcc
      .tx_manager
      .try_lock_for(Duration::from_millis(10))
      .is_some()
    {
      tx_manager_free = true;
      break;
    }
  }
  drop(release_tx);
  holder.join().expect("holder");
  starter.join().expect("starter");
  mvcc.stop();
  assert!(
    tx_manager_free,
    "a GC pass waiting for the version chains held the transaction manager"
  );
}

#[test]
fn history_is_dropped_once_no_transaction_needs_it() {
  let (_dir, db) = open_mvcc();
  let g = seed(&db);
  let old = OldReader::open(&db, &g);
  db.begin(false).expect("begin");
  db.set_node_prop(g.a, g.prop, PropValue::I64(5))
    .expect("set");
  db.commit().expect("commit");
  drop(old);

  let mvcc = db.mvcc.as_ref().expect("mvcc");
  let counts = {
    let mut tx_mgr = mvcc.tx_manager.lock();
    let mut vc = mvcc.version_chain.write();
    let mut gc = mvcc.gc.lock();
    let _ = gc.run_gc(&mut tx_mgr, &mut vc);
    vc.counts()
  };
  assert_eq!(
    counts.node_prop_versions, 0,
    "GC kept history no open transaction can read ({counts:?})"
  );
}

#[test]
fn wall_clock_map_holds_at_most_one_entry_per_millisecond() {
  const COMMITS: usize = 10_000;
  let mut tx_mgr = TxManager::new();
  let started = Instant::now();
  for i in 0..COMMITS {
    let (txid, _) = tx_mgr.begin_tx();
    tx_mgr.record_write(txid, TxKey::Node(i as NodeId));
    tx_mgr.commit_tx(txid).expect("commit");
  }
  let elapsed_ms = started.elapsed().as_millis() as usize;
  let entries = tx_mgr.wall_clock_len();
  assert!(
    entries <= elapsed_ms + 2,
    "{COMMITS} commits in {elapsed_ms} ms left {entries} wall clock entries"
  );
}

#[test]
fn old_snapshot_node_props_do_not_scan_every_chain() {
  const NODES: usize = 1_000;
  let (_dir, db) = open_mvcc();
  let g = seed(&db);
  db.begin(false).expect("begin nodes");
  let nodes: Vec<NodeId> = (0..NODES)
    .map(|i| {
      let node = db.create_node(None).expect("node");
      db.set_node_prop(node, g.prop, PropValue::I64(i as i64))
        .expect("prop");
      node
    })
    .collect();
  db.commit().expect("commit nodes");

  // An old snapshot, then history for every node's prop.
  let (ready_tx, ready_rx) = mpsc::channel();
  let (go_tx, go_rx) = mpsc::channel::<()>();
  let reader = {
    let db = Arc::clone(&db);
    let (node, prop) = (nodes[0], g.prop);
    thread::spawn(move || {
      db.begin(true).expect("begin old reader");
      ready_tx.send(()).expect("ready");
      go_rx.recv().expect("go");
      KEYS_EXAMINED.with(|count| count.set(0));
      let props = db.node_props(node).expect("node");
      let examined = KEYS_EXAMINED.with(|count| count.get());
      db.rollback().expect("end");
      (props.get(&prop).cloned(), examined)
    })
  };
  ready_rx.recv().expect("old reader began");
  db.begin(false).expect("begin rewrite");
  for &node in &nodes {
    db.set_node_prop(node, g.prop, PropValue::I64(-1))
      .expect("rewrite");
  }
  db.commit().expect("commit rewrite");
  go_tx.send(()).expect("go");
  let (value, examined) = reader.join().expect("reader thread");

  assert_eq!(value, Some(PropValue::I64(0)), "old snapshot value");
  assert!(
    examined <= 4,
    "node_props of one node examined {examined} chain keys ({NODES} nodes have history)"
  );
}
