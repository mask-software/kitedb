use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions,
};
use kitedb::types::PropValue;
use std::sync::{mpsc, Arc, Barrier};
use std::thread;

fn options() -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .auto_checkpoint(false)
    .background_checkpoint(false)
}

fn seed_nodes(db: &SingleFileDB) -> (u64, u64) {
  db.begin(false).expect("begin seed transaction");
  let source = db.create_node(Some("source")).expect("create source");
  let target = db.create_node(Some("target")).expect("create target");
  db.commit().expect("commit seed transaction");
  (source, target)
}

#[test]
fn rolled_back_propkey_is_local_then_redefinition_survives_reopen() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("propkey-rollback.kitedb");
  let db = open_single_file(&path, options()).expect("open");
  let (node, _) = seed_nodes(&db);

  db.begin(false).expect("begin rollback transaction");
  db.set_node_prop_by_name(node, "rollback_key", PropValue::I64(1))
    .expect("set rolled-back property");
  let rolled_back_id = db
    .propkey_id("rollback_key")
    .expect("own transaction sees staged property key");
  assert_eq!(
    db.propkey_name(rolled_back_id).as_deref(),
    Some("rollback_key")
  );
  db.rollback().expect("rollback");
  assert_eq!(db.propkey_id("rollback_key"), None);
  assert_eq!(db.propkey_name(rolled_back_id), None);

  db.begin(false).expect("begin committed transaction");
  db.set_node_prop_by_name(node, "rollback_key", PropValue::I64(2))
    .expect("set redefined property");
  let committed_id = db
    .propkey_id("rollback_key")
    .expect("redefined property key is visible to its transaction");
  db.set_node_prop_by_name(
    node,
    "different_key",
    PropValue::String("different".to_string()),
  )
  .expect("set different property");
  let different_id = db
    .propkey_id("different_key")
    .expect("different property key is visible to its transaction");
  db.commit().expect("commit redefinition transaction");
  db.checkpoint().expect("checkpoint sparse schema IDs");
  close_single_file(db).expect("close");

  let reopened = open_single_file(&path, options()).expect("reopen");
  assert_eq!(reopened.propkey_id("rollback_key"), Some(committed_id));
  assert_eq!(
    reopened.propkey_name(committed_id).as_deref(),
    Some("rollback_key")
  );
  assert_eq!(
    reopened.node_prop(node, committed_id),
    Some(PropValue::I64(2))
  );
  assert_eq!(reopened.propkey_id("different_key"), Some(different_id));
  assert_eq!(
    reopened.node_prop(node, different_id),
    Some(PropValue::String("different".to_string()))
  );
  close_single_file(reopened).expect("close reopened");
}

#[test]
fn rolled_back_label_and_etype_are_not_reused_as_phantoms() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("schema-rollback.kitedb");
  let db = open_single_file(&path, options()).expect("open");
  let (source, target) = seed_nodes(&db);

  db.begin(false).expect("begin rollback transaction");
  db.add_node_label_by_name(source, "rollback_label")
    .expect("add rolled-back label");
  db.add_edge_by_name(source, "rollback_etype", target)
    .expect("add rolled-back edge");
  let rolled_back_label_id = db
    .label_id("rollback_label")
    .expect("own transaction sees staged label");
  let rolled_back_etype_id = db
    .etype_id("rollback_etype")
    .expect("own transaction sees staged edge type");
  db.rollback().expect("rollback");
  assert_eq!(db.label_id("rollback_label"), None);
  assert_eq!(db.label_name(rolled_back_label_id), None);
  assert_eq!(db.etype_id("rollback_etype"), None);
  assert_eq!(db.etype_name(rolled_back_etype_id), None);

  db.begin(false).expect("begin committed transaction");
  db.add_node_label_by_name(source, "rollback_label")
    .expect("add redefined label");
  db.add_edge_by_name(source, "rollback_etype", target)
    .expect("add redefined edge");
  let committed_label_id = db
    .label_id("rollback_label")
    .expect("redefined label is visible");
  let committed_etype_id = db
    .etype_id("rollback_etype")
    .expect("redefined edge type is visible");
  db.commit().expect("commit redefinition transaction");
  close_single_file(db).expect("close");

  let reopened = open_single_file(&path, options()).expect("reopen");
  assert_eq!(
    reopened.label_id("rollback_label"),
    Some(committed_label_id)
  );
  assert_eq!(
    reopened.label_name(committed_label_id).as_deref(),
    Some("rollback_label")
  );
  assert!(reopened.node_has_label(source, committed_label_id));
  assert_eq!(
    reopened.etype_id("rollback_etype"),
    Some(committed_etype_id)
  );
  assert_eq!(
    reopened.etype_name(committed_etype_id).as_deref(),
    Some("rollback_etype")
  );
  assert!(reopened.edge_exists(source, committed_etype_id, target));
  close_single_file(reopened).expect("close reopened");
}

#[test]
fn uncommitted_schema_is_invisible_to_another_transaction_until_commit() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("schema-visibility.kitedb");
  let db = Arc::new(open_single_file(&path, options()).expect("open"));
  let (defined_tx, defined_rx) = mpsc::channel();
  let (checked_tx, checked_rx) = mpsc::channel();
  let (committed_tx, committed_rx) = mpsc::channel();

  let writer_db = Arc::clone(&db);
  let writer = thread::spawn(move || {
    writer_db.begin(false).expect("writer begin");
    let id = writer_db.define_label("foo").expect("define foo");
    defined_tx.send(id).expect("send defined");
    checked_rx.recv().expect("wait for observer");
    writer_db.commit().expect("writer commit");
    committed_tx.send(id).expect("send committed");
  });

  let observer_db = Arc::clone(&db);
  let observer = thread::spawn(move || {
    let id = defined_rx.recv().expect("receive defined");
    observer_db
      .begin(true)
      .expect("observer begin read transaction");
    assert_eq!(observer_db.label_id("foo"), None);
    checked_tx.send(()).expect("send checked");
    let committed_id = committed_rx.recv().expect("receive committed");
    assert_eq!(committed_id, id);
    assert_eq!(observer_db.label_id("foo"), Some(id));
    assert_eq!(observer_db.label_name(id).as_deref(), Some("foo"));
    observer_db.commit().expect("observer finish");
  });

  writer.join().expect("writer thread");
  observer.join().expect("observer thread");
  let db = match Arc::try_unwrap(db) {
    Ok(db) => db,
    Err(_) => panic!("database still has active thread references"),
  };
  close_single_file(db).expect("close");
}

#[test]
fn concurrent_same_name_definitions_share_id_and_preserve_both_writes() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("schema-reservation.kitedb");
  let db = Arc::new(open_single_file(&path, options()).expect("open"));
  let (node_a, node_b) = seed_nodes(&db);
  let barrier = Arc::new(Barrier::new(3));
  let (ids_tx, ids_rx) = mpsc::channel();

  let first_db = Arc::clone(&db);
  let first_barrier = Arc::clone(&barrier);
  let first_ids = ids_tx.clone();
  let first = thread::spawn(move || {
    first_db.begin(false).expect("first begin");
    let id = first_db.define_propkey("bar").expect("first define");
    first_db
      .set_node_prop(node_a, id, PropValue::I64(10))
      .expect("first write");
    first_ids.send(id).expect("send first id");
    first_barrier.wait();
    first_db.commit().expect("first commit");
  });

  let second_db = Arc::clone(&db);
  let second_barrier = Arc::clone(&barrier);
  let second = thread::spawn(move || {
    second_db.begin(false).expect("second begin");
    let id = second_db.define_propkey("bar").expect("second define");
    second_db
      .set_node_prop(node_b, id, PropValue::I64(20))
      .expect("second write");
    ids_tx.send(id).expect("send second id");
    second_barrier.wait();
    second_db.commit().expect("second commit");
  });

  let first_id = ids_rx.recv().expect("receive first id");
  let second_id = ids_rx.recv().expect("receive second id");
  assert_eq!(first_id, second_id);
  assert_eq!(db.propkey_id("bar"), None);
  barrier.wait();
  first.join().expect("first thread");
  second.join().expect("second thread");
  assert_eq!(db.propkey_id("bar"), Some(first_id));
  assert_eq!(db.node_prop(node_a, first_id), Some(PropValue::I64(10)));
  assert_eq!(db.node_prop(node_b, first_id), Some(PropValue::I64(20)));
  let db = match Arc::try_unwrap(db) {
    Ok(db) => db,
    Err(_) => panic!("database still has active thread references"),
  };
  close_single_file(db).expect("close");

  let reopened = open_single_file(&path, options()).expect("reopen");
  assert_eq!(reopened.propkey_id("bar"), Some(first_id));
  assert_eq!(
    reopened.node_prop(node_a, first_id),
    Some(PropValue::I64(10))
  );
  assert_eq!(
    reopened.node_prop(node_b, first_id),
    Some(PropValue::I64(20))
  );
  close_single_file(reopened).expect("close reopened");
}

#[test]
fn rolled_back_reservation_can_be_claimed_by_a_later_transaction() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("schema-reservation-reuse.kitedb");
  let db = open_single_file(&path, options()).expect("open");

  db.begin(false).expect("begin rollback transaction");
  let rolled_back_id = db.define_etype("baz").expect("define baz");
  assert_eq!(db.etype_id("baz"), Some(rolled_back_id));
  db.rollback().expect("rollback");
  assert_eq!(db.etype_id("baz"), None);

  db.begin(false).expect("begin later transaction");
  let committed_id = db.define_etype("baz").expect("redefine baz");
  db.commit().expect("commit later transaction");
  close_single_file(db).expect("close");

  let reopened = open_single_file(&path, options()).expect("reopen");
  assert_eq!(reopened.etype_id("baz"), Some(committed_id));
  assert_eq!(reopened.etype_name(committed_id).as_deref(), Some("baz"));
  close_single_file(reopened).expect("close reopened");
}
