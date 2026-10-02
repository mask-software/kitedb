//! A database's WAL size is fixed when the file is created. Reopening without
//! an explicit `wal_size` must use the file's size; an explicit size that
//! doesn't match is still rejected.
//!
//! Regression: an unset `wal_size` meant "4 MB", so any file with a different
//! WAL (a custom size, a profile, or one shrunk by a default vacuum) failed to
//! reopen with default options ("WAL size mismatch").

use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileOpenOptions, VacuumOptions,
};

#[test]
fn reopen_with_default_options_uses_the_files_wal_size() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("custom-wal.kitedb");
  let db = open_single_file(&path, SingleFileOpenOptions::new().wal_size(64 * 1024))
    .expect("create with a 64 KB WAL");
  db.begin(false).expect("begin");
  db.create_node(Some("node")).expect("create");
  db.commit().expect("commit");
  close_single_file(db).expect("close");

  let reopened =
    open_single_file(&path, SingleFileOpenOptions::new()).expect("reopen with default options");
  assert!(reopened.node_by_key("node").is_some());
  reopened.begin(false).expect("begin");
  reopened
    .create_node(Some("after-reopen"))
    .expect("write after reopen");
  reopened.commit().expect("commit after reopen");
  close_single_file(reopened).expect("close reopened");
}

#[test]
fn reopen_after_default_vacuum_with_default_options_succeeds() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("vacuumed.kitedb");
  let db = open_single_file(&path, SingleFileOpenOptions::new()).expect("create");
  db.begin(false).expect("begin");
  db.create_node(Some("node")).expect("create");
  db.commit().expect("commit");
  db.checkpoint().expect("checkpoint");
  // The default vacuum shrinks the (now empty) WAL.
  db.vacuum_single_file(Some(VacuumOptions::default()))
    .expect("vacuum");
  close_single_file(db).expect("close");

  let reopened =
    open_single_file(&path, SingleFileOpenOptions::new()).expect("reopen after vacuum");
  assert!(reopened.node_by_key("node").is_some());
  close_single_file(reopened).expect("close reopened");
}

#[test]
fn explicit_mismatched_wal_size_is_still_rejected() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("strict.kitedb");
  let db =
    open_single_file(&path, SingleFileOpenOptions::new().wal_size(64 * 1024)).expect("create");
  close_single_file(db).expect("close");

  let result = open_single_file(
    &path,
    SingleFileOpenOptions::new().wal_size(8 * 1024 * 1024),
  );
  assert!(
    result.is_err(),
    "an explicit WAL size that doesn't match the file must be rejected"
  );
}
