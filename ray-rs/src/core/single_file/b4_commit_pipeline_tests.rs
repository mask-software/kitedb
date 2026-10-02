//! raydb-b4 `commit-pipeline` lane: concurrent MVCC commits (finding 1).
//! Included from transaction.rs for private access.
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use tempfile::tempdir;

use crate::core::single_file::{open_single_file, SingleFileOpenOptions, SyncMode};
use crate::types::{PropValue, TxKey};

/// How long writes get while the test holds a lock they must not need. They
/// take milliseconds; only writes that wait for the lock run out of it.
const DEADLINE: Duration = Duration::from_secs(20);

fn mvcc_options() -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .mvcc(true)
    .mvcc_gc_interval_ms(10)
    .sync_mode(SyncMode::Normal)
    .auto_checkpoint(false)
}

/// With MVCC, every write notes what it wrote (and read) for the
/// transaction's conflict check at commit. The notes stay with the
/// transaction until then: a write must not take the transaction manager's
/// lock, which every begin and commit takes. (Each write took it, and a batch
/// write held it while inserting every key it wrote, so concurrent writers
/// stalled each other's commits, and commits stalled behind batch writes.)
#[test]
fn mvcc_writes_take_no_transaction_manager_lock() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("writes-no-mvcc-lock.kitedb");
  let db = Arc::new(open_single_file(&path, mvcc_options()).expect("open"));
  db.begin(false).expect("begin");
  let etype = db.define_etype("knows").expect("etype");
  let prop = db.define_propkey("weight").expect("propkey");
  let label = db.define_label("Person").expect("label");
  let base = db.create_node(Some("base")).expect("base");
  let other = db.create_node(Some("other")).expect("other");
  db.add_edge(base, etype, other).expect("edge");
  db.commit().expect("commit");

  let (begun_tx, begun_rx) = mpsc::channel();
  let (go_tx, go_rx) = mpsc::channel::<()>();
  let (written_tx, written_rx) = mpsc::channel();
  let (commit_tx, commit_rx) = mpsc::channel::<()>();
  let writer = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || {
      db.begin(false).expect("begin");
      begun_tx.send(()).expect("begun");
      go_rx.recv().expect("go");
      let nodes = db
        .create_nodes_batch(&[Some("a"), Some("b"), None])
        .expect("create batch");
      let single = db.create_node(Some("c")).expect("create");
      db.add_edges_batch(&[(nodes[0], etype, nodes[1])])
        .expect("add edges");
      db.add_edges_with_props_batch(vec![(
        nodes[1],
        etype,
        nodes[2],
        vec![(prop, PropValue::I64(1))],
      )])
      .expect("add edges with props");
      db.add_edge_with_props(nodes[2], etype, single, vec![(prop, PropValue::I64(2))])
        .expect("add edge with props");
      db.add_edge(single, etype, nodes[0]).expect("add edge");
      db.set_node_prop(base, prop, PropValue::I64(3))
        .expect("set node prop");
      db.delete_node_prop(nodes[0], prop)
        .expect("delete node prop");
      db.add_node_label(base, label).expect("add label");
      db.remove_node_label(nodes[1], label).expect("remove label");
      db.set_edge_prop(base, etype, other, prop, PropValue::I64(4))
        .expect("set edge prop");
      db.set_edge_props(base, etype, other, vec![(prop, PropValue::I64(5))])
        .expect("set edge props");
      db.delete_edge_prop(nodes[0], etype, nodes[1], prop)
        .expect("delete edge prop");
      db.delete_edge(single, etype, nodes[0])
        .expect("delete edge");
      db.delete_node(nodes[2]).expect("delete node");
      let _ = written_tx.send(());
      commit_rx.recv().expect("commit");
      db.commit()
    })
  };
  begun_rx.recv().expect("writer began");

  let mvcc = db.mvcc.as_ref().expect("mvcc");
  let held = mvcc.tx_manager.lock();
  go_tx.send(()).expect("go");
  let finished = written_rx.recv_timeout(DEADLINE).is_ok();
  drop(held);
  if !finished {
    // Let the writer finish, so the failure ends instead of hanging.
    let _ = written_rx.recv();
  }
  commit_tx.send(()).expect("commit");
  writer.join().expect("writer").expect("writer commit");

  assert!(
    finished,
    "writes did not finish within {DEADLINE:?} while the transaction manager's lock was held"
  );
  assert!(db.node_by_key("a").is_some() && db.node_by_key("c").is_some());
  assert_eq!(
    db.node_prop(base, prop),
    Some(PropValue::I64(3)),
    "the writes committed"
  );
}

/// The keys a write transaction notes still reach its conflict check: a
/// concurrent commit of what it wrote, or of what a write read, conflicts.
#[test]
fn mvcc_conflicts_on_noted_writes_and_reads_guard() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("noted-keys-conflict.kitedb");
  let db = Arc::new(open_single_file(&path, mvcc_options()).expect("open"));
  db.begin(false).expect("begin");
  let prop = db.define_propkey("count").expect("propkey");
  let node = db.create_node(Some("counter")).expect("node");
  let doomed = db.create_node(Some("doomed")).expect("doomed");
  db.commit().expect("commit");

  // Write-write: both set the same prop.
  // Read-write: a prop write reads its node, which the other deletes.
  for (first_key, second_target) in [
    (
      TxKey::NodeProp {
        node_id: node,
        key_id: prop,
      },
      node,
    ),
    (TxKey::Node(doomed), doomed),
  ] {
    let (staged_tx, staged_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let second = {
      let db = Arc::clone(&db);
      std::thread::spawn(move || {
        db.begin(false).expect("begin");
        db.set_node_prop(second_target, prop, PropValue::I64(2))
          .expect("set");
        staged_tx.send(()).expect("staged");
        go_rx.recv().expect("go");
        db.commit()
      })
    };
    staged_rx.recv().expect("second staged");
    db.begin(false).expect("begin");
    match &first_key {
      TxKey::NodeProp { node_id, key_id } => db
        .set_node_prop(*node_id, *key_id, PropValue::I64(1))
        .expect("set"),
      TxKey::Node(node_id) => db.delete_node(*node_id).expect("delete"),
      _ => unreachable!(),
    }
    db.commit().expect("first commit");
    go_tx.send(()).expect("go");
    let result = second.join().expect("second");
    assert!(
      matches!(result, Err(crate::error::KiteError::Conflict { .. })),
      "the second commit must conflict on {first_key}: {result:?}"
    );
  }
}
