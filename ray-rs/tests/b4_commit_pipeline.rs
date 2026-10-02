//! raydb-b4 `commit-pipeline` lane, finding 2: Kite `batch()`,
//! `TxBuilder::execute()` and `transaction()` nested inside an open
//! transaction run under a savepoint. A failed nested call rolls back only its
//! own writes and returns its error; the outer transaction stays usable and
//! commits everything else.

use kitedb::api::kite::{BatchOp, Kite, KiteOptions, NodeDef, PropDef};
use kitedb::types::PropValue;
use kitedb::KiteError;
use std::collections::HashMap;
use std::path::Path;
use tempfile::TempDir;

type Props = HashMap<String, PropValue>;

fn schema() -> KiteOptions {
  KiteOptions::new().node(
    NodeDef::new("User", "user:")
      .prop(PropDef::string("name"))
      .prop(PropDef::int("visits")),
  )
}

fn open_at(path: &Path, options: KiteOptions) -> Kite {
  Kite::open(path, options).expect("open kite")
}

fn open(options: KiteOptions) -> (TempDir, Kite) {
  let dir = tempfile::tempdir().expect("tempdir");
  let kite = open_at(&dir.path().join("b4-commit-pipeline.kitedb"), options);
  (dir, kite)
}

fn exists(kite: &Kite, key: &str) -> bool {
  kite.get("User", key).expect("get").is_some()
}

fn user(kite: &mut Kite, key: &str) {
  kite
    .create_node("User", key, Props::new())
    .expect("create user");
}

/// Ops that create `key`, then fail (an unknown node type).
fn failing_ops(key: &str) -> Vec<BatchOp> {
  vec![
    BatchOp::CreateNode {
      node_type: "User".into(),
      key_suffix: key.into(),
      props: Props::new(),
    },
    BatchOp::CreateNode {
      node_type: "NoSuchType".into(),
      key_suffix: "never".into(),
      props: Props::new(),
    },
  ]
}

fn create_op(key: &str) -> BatchOp {
  BatchOp::CreateNode {
    node_type: "User".into(),
    key_suffix: key.into(),
    props: Props::new(),
  }
}

/// Inside an open transaction, failed nested `transaction()`, `batch()` and
/// `TxBuilder::execute()` calls each return their own error and leave
/// nothing of theirs; the writes made before and after them commit.
#[test]
fn nested_failure_keeps_the_outer_writes_and_commits_them() {
  let (_dir, mut kite) = open(schema());
  kite.raw().begin(false).expect("outer begin");
  user(&mut kite, "before");

  let result = kite.transaction(|ctx| -> kitedb::Result<()> {
    ctx.create_node("User", "in-transaction", Props::new())?;
    Err(KiteError::Internal("closure failed".into()))
  });
  assert!(
    matches!(&result, Err(KiteError::Internal(message)) if message == "closure failed"),
    "transaction() must return its own error: {result:?}"
  );

  let result = kite.batch(failing_ops("in-batch"));
  assert!(
    matches!(result, Err(KiteError::InvalidSchema(_))),
    "batch() must return its own error: {result:?}"
  );

  let mut builder = kite.tx();
  for op in failing_ops("in-builder") {
    builder = match op {
      BatchOp::CreateNode {
        node_type,
        key_suffix,
        props,
      } => builder.create_node(node_type, key_suffix, props),
      _ => unreachable!(),
    };
  }
  let result = builder.execute(&mut kite);
  assert!(
    matches!(result, Err(KiteError::InvalidSchema(_))),
    "TxBuilder::execute() must return its own error: {result:?}"
  );

  assert!(
    kite.raw().has_transaction(),
    "the outer transaction must stay open"
  );
  user(&mut kite, "after");
  kite.raw().commit().expect("outer commit");

  for key in ["before", "after"] {
    assert!(exists(&kite, key), "{key:?} must commit with the outer tx");
  }
  for key in ["in-transaction", "in-batch", "in-builder"] {
    assert!(!exists(&kite, key), "{key:?} was rolled back");
  }
}

/// Successful nested calls commit with the outer transaction, also around a
/// failed one, and roll back with it.
#[test]
fn nested_success_commits_with_the_outer() {
  let (_dir, mut kite) = open(schema());
  kite.raw().begin(false).expect("outer begin");
  kite.batch(vec![create_op("first")]).expect("nested batch");
  let failed = kite.batch(failing_ops("failed"));
  assert!(failed.is_err());
  kite
    .transaction(|ctx| ctx.create_node("User", "second", Props::new()).map(drop))
    .expect("nested transaction");
  kite
    .tx()
    .create_node("User", "third", Props::new())
    .execute(&mut kite)
    .expect("nested builder");
  kite.raw().commit().expect("outer commit");
  for key in ["first", "second", "third"] {
    assert!(exists(&kite, key), "{key:?} must commit with the outer tx");
  }
  assert!(!exists(&kite, "failed"));

  kite.raw().begin(false).expect("begin");
  kite.batch(vec![create_op("discarded")]).expect("nested");
  kite.raw().rollback().expect("outer rollback");
  assert!(!exists(&kite, "discarded"));
}

/// With MVCC, a nested call's rolled-back writes do not make the outer
/// transaction conflict with a concurrent commit of the same keys.
#[test]
fn rolled_back_nested_writes_do_not_conflict() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("b4-commit-pipeline-mvcc.kitedb");
  let mut kite = open_at(&path, schema().mvcc(true));
  user(&mut kite, "shared");
  let shared = kite
    .get("User", "shared")
    .expect("get")
    .expect("shared")
    .id();

  kite.raw().begin(false).expect("outer begin");
  user(&mut kite, "outer");
  let failed = kite.batch(vec![
    BatchOp::SetProp {
      node_id: shared,
      prop_name: "visits".into(),
      value: PropValue::I64(1),
    },
    BatchOp::CreateNode {
      node_type: "NoSuchType".into(),
      key_suffix: "never".into(),
      props: Props::new(),
    },
  ]);
  assert!(failed.is_err(), "the nested batch fails");

  // Another transaction writes the prop the nested batch wrote, and commits.
  let raw = kite.raw();
  std::thread::scope(|scope| {
    scope
      .spawn(|| {
        let visits = raw.propkey_id("visits").expect("visits");
        raw.begin(false)?;
        raw.set_node_prop(shared, visits, PropValue::I64(2))?;
        raw.commit()
      })
      .join()
      .expect("concurrent writer")
      .expect("concurrent commit");
  });

  kite
    .raw()
    .commit()
    .expect("the outer commit must not conflict with the rolled-back write");
  assert!(exists(&kite, "outer"));
}

/// The WAL a committed transaction leaves holds none of a rolled-back nested
/// call's records: reopening (which replays it) and opening a crash image
/// both see the outer writes only.
#[test]
fn wal_replay_after_commit_has_no_rolled_back_records() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("b4-commit-pipeline-wal.kitedb");
  let mut kite = open_at(&path, schema());
  kite.raw().begin(false).expect("outer begin");
  user(&mut kite, "outer");
  assert!(kite.batch(failing_ops("rolled-back")).is_err());
  kite.batch(vec![create_op("kept")]).expect("nested batch");
  kite.raw().commit().expect("outer commit");

  let image = path.with_extension("image.kitedb");
  std::fs::copy(&path, &image).expect("crash image");
  kite.close().expect("close");

  for opened in [&path, &image] {
    let reopened = open_at(opened, schema());
    assert!(exists(&reopened, "outer") && exists(&reopened, "kept"));
    assert!(
      !exists(&reopened, "rolled-back"),
      "{} replayed a rolled-back record",
      opened.display()
    );
    reopened.close().expect("close");
  }
}
