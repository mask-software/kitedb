use kitedb::core::single_file::{close_single_file, open_single_file, SingleFileOpenOptions};
use kitedb::error::KiteError;
use std::fs;

fn writable_options() -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .auto_checkpoint(false)
    .background_checkpoint(false)
}

#[test]
fn second_writable_open_fails_until_first_closes() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("locked.kitedb");

  let first = open_single_file(&path, writable_options()).expect("first open");
  let second_error = match open_single_file(&path, writable_options()) {
    Ok(second) => {
      close_single_file(second).expect("close unexpected second handle");
      panic!("second writable open unexpectedly succeeded")
    }
    Err(error) => error,
  };
  assert!(
    matches!(second_error, KiteError::LockFailed(_)),
    "expected a lock error, got {second_error}"
  );

  close_single_file(first).expect("close first");
  let reopened = open_single_file(&path, writable_options()).expect("reopen after close");
  close_single_file(reopened).expect("close reopened");
}

#[test]
fn writable_open_fails_while_read_only_handle_is_open() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("shared.kitedb");

  let db = open_single_file(&path, writable_options()).expect("create");
  close_single_file(db).expect("close initial");

  let read_only = open_single_file(
    &path,
    writable_options().read_only(true).create_if_missing(false),
  )
  .expect("read-only open");

  let writer_error = match open_single_file(&path, writable_options()) {
    Ok(writer) => {
      close_single_file(writer).expect("close unexpected writer");
      panic!("writable open unexpectedly succeeded while read-only handle was open")
    }
    Err(error) => error,
  };
  assert!(
    matches!(writer_error, KiteError::LockFailed(_)),
    "expected a lock error, got {writer_error}"
  );

  close_single_file(read_only).expect("close read-only");
  let writer = open_single_file(&path, writable_options()).expect("writer after read-only close");
  close_single_file(writer).expect("close writer");
}

#[cfg(unix)]
#[test]
fn read_only_open_reads_intact_database_without_writing() {
  use std::os::unix::fs::PermissionsExt;

  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("read-only.kitedb");

  let db = open_single_file(&path, writable_options()).expect("create");
  db.begin(false).expect("begin write transaction");
  let node_id = db.create_node(Some("observer-key")).expect("create node");
  let prop_key_id = db.define_propkey("value").expect("define propkey");
  db.set_node_prop(node_id, prop_key_id, kitedb::types::PropValue::I64(42))
    .expect("set property");
  db.commit().expect("commit");
  close_single_file(db).expect("close initial");

  fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).expect("make file read-only");
  let result = open_single_file(
    &path,
    writable_options().read_only(true).create_if_missing(false),
  );
  fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("restore permissions");

  let read_only = result.expect("read-only open on a read-only file");
  assert!(read_only.node_exists(node_id));
  assert_eq!(
    read_only.node_prop(node_id, prop_key_id),
    Some(kitedb::types::PropValue::I64(42))
  );
  assert!(matches!(read_only.begin(false), Err(KiteError::ReadOnly)));
  assert!(matches!(read_only.commit(), Err(KiteError::ReadOnly)));
  close_single_file(read_only).expect("close read-only");
}
