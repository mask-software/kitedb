//! Write-path semantics over a snapshot: idempotent edge adds, key reuse, and
//! WAL replay of records that name edges the snapshot already holds.

use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions,
};
use kitedb::types::{ETypeId, NodeId, PropValue};
use std::path::Path;

fn open(path: &Path) -> SingleFileDB {
  open_single_file(path, SingleFileOpenOptions::new().auto_checkpoint(false)).expect("open")
}

fn snapshot_edge(db: &SingleFileDB) -> (NodeId, ETypeId, NodeId) {
  db.begin(false).expect("begin");
  let a = db.create_node(Some("a")).expect("a");
  let b = db.create_node(Some("b")).expect("b");
  let t = db.define_etype("T").expect("etype");
  db.add_edge(a, t, b).expect("edge");
  db.commit().expect("commit");
  db.checkpoint().expect("checkpoint");
  (a, t, b)
}

#[test]
fn props_on_snapshot_edge_replay_without_duplicating_it() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("props.kitedb");
  let db = open(&path);
  let (a, t, b) = snapshot_edge(&db);
  let w = {
    db.begin(false).expect("begin");
    let w = db.define_propkey("weight").expect("propkey");
    // Both log an add record for an edge the snapshot already holds.
    db.add_edge_with_props(a, t, b, vec![(w, PropValue::I64(1))])
      .expect("props");
    db.add_edges_with_props_batch(vec![(a, t, b, vec![(w, PropValue::I64(2))])])
      .expect("props batch");
    db.commit().expect("commit");
    w
  };
  assert_eq!(db.count_edges(), 1);
  close_single_file(db).expect("close");

  let db = open(&path);
  assert_eq!(db.count_edges(), 1, "replay duplicated a snapshot edge");
  assert_eq!(db.out_degree(a), 1);
  assert_eq!(db.edge_prop(a, t, b, w), Some(PropValue::I64(2)));
  db.begin(false).expect("begin");
  db.delete_edge(a, t, b).expect("delete");
  db.commit().expect("commit");
  close_single_file(db).expect("close");

  let db = open(&path);
  assert!(
    !db.edge_exists(a, t, b),
    "deleted snapshot edge resurrected"
  );
  db.checkpoint().expect("checkpoint");
  assert_eq!(db.count_edges(), 0);
  close_single_file(db).expect("close");
}

#[test]
fn readding_deleted_snapshot_edge_restores_it_once() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("readd.kitedb");
  let db = open(&path);
  let (a, t, b) = snapshot_edge(&db);
  db.begin(false).expect("begin");
  db.delete_edge(a, t, b).expect("delete");
  db.commit().expect("commit");
  db.begin(false).expect("begin");
  db.add_edges_batch(&[(a, t, b), (a, t, b)]).expect("re-add");
  db.commit().expect("commit");
  assert_eq!(db.count_edges(), 1);
  close_single_file(db).expect("close");

  let db = open(&path);
  assert_eq!(db.count_edges(), 1);
  db.checkpoint().expect("checkpoint");
  assert_eq!(db.count_edges(), 1);
  close_single_file(db).expect("close");
}

#[test]
fn key_of_deleted_snapshot_node_can_be_reused() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("keys.kitedb");
  let db = open(&path);
  db.begin(false).expect("begin");
  let first = db.create_node(Some("user:alice")).expect("first");
  db.commit().expect("commit");
  db.checkpoint().expect("checkpoint");

  db.begin(false).expect("begin");
  db.delete_node(first).expect("delete");
  let second = db
    .create_node(Some("user:alice"))
    .expect("reuse key in same tx");
  assert!(db.create_node(Some("user:alice")).is_err());
  db.commit().expect("commit");
  assert_eq!(db.node_by_key("user:alice"), Some(second));

  db.checkpoint().expect("checkpoint");
  close_single_file(db).expect("close");
  let db = open(&path);
  assert_eq!(db.node_by_key("user:alice"), Some(second));
  assert_eq!(db.count_nodes(), 1);
  close_single_file(db).expect("close");
}

#[test]
fn bulk_load_skips_existing_edges_across_checkpoint() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("bulk.kitedb");
  let db = open(&path);
  db.begin_bulk().expect("begin bulk");
  let ids = db
    .create_nodes_batch(&[Some("a"), Some("b")])
    .expect("nodes");
  let t = db.define_etype("T").expect("etype");
  db.add_edges_batch(&[(ids[0], t, ids[1])]).expect("edge");
  db.commit().expect("commit");
  db.checkpoint().expect("checkpoint");

  db.begin_bulk().expect("begin bulk");
  db.add_edges_batch(&[(ids[0], t, ids[1]), (ids[1], t, ids[0])])
    .expect("edges");
  db.commit().expect("commit");
  db.checkpoint().expect("checkpoint");
  assert_eq!(db.count_edges(), 2);
  close_single_file(db).expect("close");
}
