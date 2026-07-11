use kitedb::api::kite::{Kite, KiteOptions, NodeDef};
use kitedb::core::single_file::{close_single_file, open_single_file, SingleFileOpenOptions};
use kitedb::types::PropValue;
use std::collections::HashMap;

#[test]
fn dynamic_label_and_etype_survive_reopen_without_checkpoint() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("schema.kitedb");
  let options = SingleFileOpenOptions::new().auto_checkpoint(false);

  let db = open_single_file(&path, options.clone()).expect("open");
  db.begin(false).expect("begin");
  let source = db.create_node(Some("source")).expect("source");
  let target = db.create_node(Some("target")).expect("target");
  db.add_node_label_by_name(source, "DynamicLabel")
    .expect("label");
  db.add_edge_by_name(source, "DynamicEdge", target)
    .expect("edge");
  db.commit().expect("commit");

  let label_id = db.label_id("DynamicLabel").expect("label id");
  let etype_id = db.etype_id("DynamicEdge").expect("etype id");
  close_single_file(db).expect("close");

  let reopened = open_single_file(&path, options).expect("reopen");
  assert_eq!(reopened.label_id("DynamicLabel"), Some(label_id));
  assert_eq!(
    reopened.label_name(label_id).as_deref(),
    Some("DynamicLabel")
  );
  assert!(reopened.node_has_label(source, label_id));
  assert_eq!(reopened.etype_id("DynamicEdge"), Some(etype_id));
  assert_eq!(
    reopened.etype_name(etype_id).as_deref(),
    Some("DynamicEdge")
  );
  assert!(reopened.edge_exists(source, etype_id, target));
  close_single_file(reopened).expect("close reopened");
}

#[test]
fn dynamic_properties_do_not_reuse_ids_after_reopen() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("property-ids");
  let schema = || {
    KiteOptions::new()
      .disable_close_checkpoint()
      .node(NodeDef::new("User", "user:"))
  };

  let node_id = {
    let mut kite = Kite::open(&path, schema()).expect("open");
    let node = kite
      .create_node("User", "alice", HashMap::new())
      .expect("create node");
    kite
      .set_prop(node.id(), "first", PropValue::I64(1))
      .expect("set first");
    let node_id = node.id();
    kite.close().expect("close");
    node_id
  };

  {
    let mut kite = Kite::open(&path, schema()).expect("reopen");
    assert_eq!(kite.prop(node_id, "first"), Some(PropValue::I64(1)));
    kite
      .set_prop(node_id, "second", PropValue::String("two".to_string()))
      .expect("set second");
    assert_eq!(kite.prop(node_id, "first"), Some(PropValue::I64(1)));
    assert_eq!(
      kite.prop(node_id, "second"),
      Some(PropValue::String("two".to_string()))
    );
    kite.close().expect("close");
  }

  let kite = Kite::open(&path, schema()).expect("reopen again");
  assert_eq!(kite.prop(node_id, "first"), Some(PropValue::I64(1)));
  assert_eq!(
    kite.prop(node_id, "second"),
    Some(PropValue::String("two".to_string()))
  );
  kite.close().expect("close again");
}
