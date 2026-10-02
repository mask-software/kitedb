//! Audit repros for lane mvcc-write-view: write-path existence checks must not
//! trust MVCC version chains.
//!
//! `apply_mvcc_commit` appends versions only while another transaction is
//! active, and checkpoints clear the delta without touching the chains. So a
//! change committed while a reader is open lands in the chain, and a later
//! change committed with no other transaction open does not: the chain head
//! keeps the older state. Reads still trust that head (the read-side fix is
//! deferred to wave 2). Write-side checks must instead use the committed state
//! (delta + snapshot) plus the transaction's own pending changes.
//!
//! `stats()` and a reopen without MVCC show the committed state without any
//! version chain.

use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions,
};
use kitedb::error::{KiteError, Result};
use kitedb::types::{ETypeId, NodeId, PropKeyId, PropValue};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

fn options(mvcc: bool) -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .mvcc(mvcc)
    // GC keeps running, but retention keeps every chain built by a test.
    .mvcc_gc_interval_ms(20)
    .mvcc_retention_ms(60 * 60 * 1000)
    .auto_checkpoint(false)
}

struct Fixture {
  _dir: tempfile::TempDir,
  path: PathBuf,
  db: SingleFileDB,
  a: NodeId,
  b: NodeId,
  t: ETypeId,
  weight: PropKeyId,
}

/// Nodes `a` and `b`, edge type `t` and prop key `weight`, committed with no
/// other transaction open (no version chains).
fn fixture() -> Fixture {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("write-view.kitedb");
  let db = open_single_file(&path, options(true)).expect("open");
  let (a, b, t, weight) = write_tx(&db, |db| {
    Ok((
      db.create_node(Some("a"))?,
      db.create_node(Some("b"))?,
      db.define_etype("T")?,
      db.define_propkey("weight")?,
    ))
  })
  .expect("seed");
  Fixture {
    _dir: dir,
    path,
    db,
    a,
    b,
    t,
    weight,
  }
}

fn write_tx<T>(db: &SingleFileDB, ops: impl FnOnce(&SingleFileDB) -> Result<T>) -> Result<T> {
  db.begin(false)?;
  match ops(db) {
    Ok(value) => {
      db.commit()?;
      Ok(value)
    }
    Err(error) => {
      db.rollback()?;
      Err(error)
    }
  }
}

/// Runs `write` while another thread holds a read transaction open, so the
/// commit inside `write` appends MVCC versions.
fn with_reader_open<T>(db: &SingleFileDB, write: impl FnOnce() -> T) -> T {
  let (ready_tx, ready_rx) = mpsc::channel();
  let (release_tx, release_rx) = mpsc::channel::<()>();
  thread::scope(|scope| {
    let reader = scope.spawn(move || {
      db.begin(true).expect("begin reader");
      ready_tx.send(()).expect("reader ready");
      let _ = release_rx.recv();
      db.commit().expect("end reader");
    });
    ready_rx.recv().expect("reader started");
    let value = write();
    drop(release_tx);
    reader.join().expect("reader thread");
    value
  })
}

/// Edge chain head says "exists"; the edge was deleted by a later commit.
fn make_stale_existing_edge(db: &SingleFileDB, src: NodeId, t: ETypeId, dst: NodeId) {
  with_reader_open(db, || write_tx(db, |db| db.add_edge(src, t, dst))).expect("add edge");
  write_tx(db, |db| db.delete_edge(src, t, dst)).expect("delete edge");
}

/// Edge chain head says "deleted"; the edge was re-added by a later commit.
fn make_stale_deleted_edge(db: &SingleFileDB, src: NodeId, t: ETypeId, dst: NodeId) {
  write_tx(db, |db| db.add_edge(src, t, dst)).expect("add edge");
  with_reader_open(db, || write_tx(db, |db| db.delete_edge(src, t, dst))).expect("delete edge");
  write_tx(db, |db| db.add_edge(src, t, dst)).expect("re-add edge");
}

/// Node chain head says "exists"; the node was deleted by a later commit.
fn make_stale_live_node(db: &SingleFileDB) -> NodeId {
  let node = with_reader_open(db, || write_tx(db, |db| db.create_node(None))).expect("create");
  write_tx(db, |db| db.delete_node(node)).expect("delete node");
  node
}

/// Node chain head says "deleted"; the node was re-created (same id and key)
/// by a later commit.
fn make_stale_deleted_node(db: &SingleFileDB, node: NodeId, key: &str) {
  with_reader_open(db, || write_tx(db, |db| db.delete_node(node))).expect("delete node");
  write_tx(db, |db| db.create_node_with_id(node, Some(key))).expect("re-create node");
}

/// Closes `db` and reads the file again without MVCC, so without any chain.
fn reopen_without_mvcc<T>(
  path: &Path,
  db: SingleFileDB,
  read: impl FnOnce(&SingleFileDB) -> T,
) -> T {
  close_single_file(db).expect("close");
  let db = open_single_file(path, options(false)).expect("reopen without mvcc");
  let value = read(&db);
  close_single_file(db).expect("close reopened");
  value
}

// ============================================================================
// Failing on 14ed498: `upsert_edge_with_props` and `create_node_with_id`
// decide through `edge_exists` / `node_exists`, which trust the chain head.
// ============================================================================

#[test]
fn upsert_edge_writes_edge_deleted_behind_stale_chain() {
  let Fixture {
    _dir,
    path,
    db,
    a,
    b,
    t,
    weight,
  } = fixture();
  make_stale_existing_edge(&db, a, t, b);
  let stale_read = db.edge_exists(a, t, b);

  let created = write_tx(&db, |db| {
    db.upsert_edge_with_props(a, t, b, [(weight, Some(PropValue::I64(7)))])
  })
  .expect("upsert edge");

  assert_eq!(
    db.stats().delta_edges_added,
    1,
    "upsert_edge_with_props lost the add: the committed state has no edge \
     (edge_exists before the upsert: {stale_read}, from a stale chain)"
  );
  assert!(
    created,
    "the edge did not exist, so upsert must report created"
  );
  // No checkpoint ran: the reopened state is the WAL replay, so the edge is
  // there only if the upsert logged an AddEdge record.
  let (exists, prop) = reopen_without_mvcc(&path, db, |db| {
    (db.edge_exists(a, t, b), db.edge_prop(a, t, b, weight))
  });
  assert!(exists, "no AddEdge in the WAL: edge missing after reopen");
  assert_eq!(prop, Some(PropValue::I64(7)));
}

#[test]
fn upsert_edge_to_deleted_node_fails_behind_stale_chain() {
  let Fixture {
    _dir,
    path,
    db,
    a,
    b,
    t,
    weight,
  } = fixture();
  // The edge chain says a->b exists; deleting b then drops the edge.
  with_reader_open(&db, || write_tx(&db, |db| db.add_edge(a, t, b))).expect("add edge");
  write_tx(&db, |db| db.delete_node(b)).expect("delete b");
  let stale_read = db.edge_exists(a, t, b);

  let result = write_tx(&db, |db| {
    db.upsert_edge_with_props(a, t, b, [(weight, Some(PropValue::I64(7)))])
  });

  assert!(
    matches!(result, Err(KiteError::NodeNotFound(node)) if node == b),
    "upsert to deleted node {b} must fail with NodeNotFound, got {result:?} \
     (edge_exists before the upsert: {stale_read}, from a stale chain)"
  );
  db.checkpoint().expect("checkpoint after rejected upsert");
  let (exists, prop) = reopen_without_mvcc(&path, db, |db| {
    (db.edge_exists(a, t, b), db.edge_prop(a, t, b, weight))
  });
  assert!(!exists);
  assert_eq!(prop, None);
}

#[test]
fn upsert_edge_reports_existing_edge_behind_stale_deleted_chain() {
  let Fixture {
    _dir,
    path,
    db,
    a,
    b,
    t,
    weight,
  } = fixture();
  make_stale_deleted_edge(&db, a, t, b);
  let stale_read = db.edge_exists(a, t, b);

  let created = write_tx(&db, |db| {
    db.upsert_edge_with_props(a, t, b, [(weight, Some(PropValue::I64(7)))])
  })
  .expect("upsert edge");

  assert!(
    !created,
    "the edge already existed, so upsert must not report created \
     (edge_exists before the upsert: {stale_read}, from a stale chain)"
  );
  assert_eq!(db.stats().delta_edges_added, 1);
  let (exists, prop) = reopen_without_mvcc(&path, db, |db| {
    (db.edge_exists(a, t, b), db.edge_prop(a, t, b, weight))
  });
  assert!(exists);
  assert_eq!(prop, Some(PropValue::I64(7)));
}

#[test]
fn create_node_with_id_reuses_deleted_id_behind_stale_chain() {
  let Fixture { _dir, path, db, .. } = fixture();
  let node = make_stale_live_node(&db);
  let stale_read = db.node_exists(node);

  let result = write_tx(&db, |db| db.create_node_with_id(node, Some("again")));

  assert!(
    matches!(result, Ok(id) if id == node),
    "node {node} was deleted, so its id is free (see the no-chain guard), got {result:?} \
     (node_exists before the create: {stale_read}, from a stale chain)"
  );
  let owner = reopen_without_mvcc(&path, db, |db| db.node_by_key("again"));
  assert_eq!(owner, Some(node));
}

#[test]
fn create_node_with_id_rejects_live_id_behind_stale_chain() {
  let Fixture {
    _dir,
    path,
    db,
    a,
    weight,
    ..
  } = fixture();
  make_stale_deleted_node(&db, a, "a");
  write_tx(&db, |db| db.set_node_prop(a, weight, PropValue::I64(2))).expect("set prop");
  let stale_read = db.node_exists(a);

  let result = write_tx(&db, |db| db.create_node_with_id(a, Some("dup")));

  assert!(
    result.is_err(),
    "node {a} exists, so create_node_with_id must fail, got {result:?} \
     (node_exists before the create: {stale_read}, from a stale chain)"
  );
  let (key, prop, dup) = reopen_without_mvcc(&path, db, |db| {
    (
      db.node_key(a),
      db.node_prop(a, weight),
      db.node_by_key("dup"),
    )
  });
  assert_eq!(key.as_deref(), Some("a"));
  assert_eq!(prop, Some(PropValue::I64(2)));
  assert_eq!(dup, None);
}

// The stale chain survives a checkpoint, which moves the committed state into
// the snapshot and clears the delta.

#[test]
fn upsert_edge_writes_edge_deleted_behind_stale_chain_after_checkpoint() {
  let Fixture {
    _dir,
    path,
    db,
    a,
    b,
    t,
    weight,
  } = fixture();
  make_stale_existing_edge(&db, a, t, b);
  db.checkpoint().expect("checkpoint");
  let stale_read = db.edge_exists(a, t, b);

  let created = write_tx(&db, |db| {
    db.upsert_edge_with_props(a, t, b, [(weight, Some(PropValue::I64(7)))])
  })
  .expect("upsert edge");

  assert_eq!(
    db.stats().delta_edges_added,
    1,
    "upsert_edge_with_props lost the add after a checkpoint \
     (edge_exists before the upsert: {stale_read}, from a stale chain)"
  );
  assert!(created);
  db.checkpoint().expect("checkpoint after upsert");
  assert_eq!(
    db.stats().snapshot_edges,
    1,
    "edge missing after checkpoint"
  );
  let exists = reopen_without_mvcc(&path, db, |db| db.edge_exists(a, t, b));
  assert!(exists, "edge missing after reopen");
}

#[test]
fn create_node_with_id_rejects_snapshot_node_behind_stale_chain_after_checkpoint() {
  let Fixture {
    _dir, path, db, a, ..
  } = fixture();
  make_stale_deleted_node(&db, a, "a");
  db.checkpoint().expect("checkpoint");
  let stale_read = db.node_exists(a);

  let result = write_tx(&db, |db| db.create_node_with_id(a, Some("dup")));

  assert!(
    result.is_err(),
    "node {a} is in the snapshot, so create_node_with_id must fail, got {result:?} \
     (node_exists before the create: {stale_read}, from a stale chain)"
  );
  db.checkpoint().expect("checkpoint after rejected create");
  assert_eq!(db.stats().snapshot_nodes, 2);
  let (nodes, key, dup) = reopen_without_mvcc(&path, db, |db| {
    (db.count_nodes(), db.node_key(a), db.node_by_key("dup"))
  });
  assert_eq!(nodes, 2);
  assert_eq!(key.as_deref(), Some("a"));
  assert_eq!(dup, None);
}

// ============================================================================
// Guards, passing on 14ed498: add_edge and the key check already validate
// through `TxView` (pending + delta + snapshot) and must keep doing so.
// ============================================================================

#[test]
fn add_edge_writes_edge_deleted_behind_stale_chain() {
  let Fixture {
    _dir,
    path,
    db,
    a,
    b,
    t,
    ..
  } = fixture();
  make_stale_existing_edge(&db, a, t, b);

  write_tx(&db, |db| db.add_edge(a, t, b)).expect("add edge");

  assert_eq!(db.stats().delta_edges_added, 1, "add_edge lost the add");
  db.checkpoint().expect("checkpoint");
  assert_eq!(
    db.stats().snapshot_edges,
    1,
    "edge missing after checkpoint"
  );
  let exists = reopen_without_mvcc(&path, db, |db| db.edge_exists(a, t, b));
  assert!(exists, "edge missing after reopen");
}

#[test]
fn add_edge_to_deleted_node_fails_behind_stale_chain() {
  let Fixture {
    _dir,
    path,
    db,
    a,
    t,
    ..
  } = fixture();
  let gone = make_stale_live_node(&db);

  let result = write_tx(&db, |db| db.add_edge(a, t, gone));

  assert!(
    matches!(result, Err(KiteError::NodeNotFound(node)) if node == gone),
    "add_edge to deleted node {gone} must fail with NodeNotFound, got {result:?}"
  );
  db.checkpoint().expect("checkpoint after rejected add");
  let edges = reopen_without_mvcc(&path, db, |db| db.count_edges());
  assert_eq!(edges, 0);
}

#[test]
fn add_edge_to_live_node_succeeds_behind_stale_deleted_chain() {
  let Fixture {
    _dir,
    path,
    db,
    a,
    b,
    t,
    ..
  } = fixture();
  make_stale_deleted_node(&db, b, "b");

  write_tx(&db, |db| db.add_edge(a, t, b)).expect("add edge to live node");

  db.checkpoint().expect("checkpoint");
  let exists = reopen_without_mvcc(&path, db, |db| db.edge_exists(a, t, b));
  assert!(exists);
}

#[test]
fn create_node_rejects_key_of_live_node_behind_stale_deleted_chain() {
  let Fixture {
    _dir, path, db, b, ..
  } = fixture();
  make_stale_deleted_node(&db, b, "b");

  let result = write_tx(&db, |db| db.create_node(Some("b")));

  assert!(
    matches!(result, Err(KiteError::DuplicateKey(ref key)) if key == "b"),
    "key b is held by live node {b}, got {result:?}"
  );
  let owner = reopen_without_mvcc(&path, db, |db| db.node_by_key("b"));
  assert_eq!(owner, Some(b));
}

#[test]
fn create_node_with_id_reuses_deleted_id_without_chain() {
  let Fixture { _dir, path, db, .. } = fixture();
  let node = write_tx(&db, |db| db.create_node(None)).expect("create");
  write_tx(&db, |db| db.delete_node(node)).expect("delete node");

  write_tx(&db, |db| db.create_node_with_id(node, Some("again"))).expect("reuse deleted id");

  let owner = reopen_without_mvcc(&path, db, |db| db.node_by_key("again"));
  assert_eq!(owner, Some(node));
}

// ============================================================================
// Conflict semantics: when a write-side check turns the write into a no-op or
// rejects it, the transaction still records a read of the checked key, so a
// commit to that key after the transaction started makes it conflict.
// ============================================================================

/// Commits `ops` from another thread while this thread's transaction is open.
fn commit_on_other_thread<T: Send>(
  db: &SingleFileDB,
  ops: impl FnOnce(&SingleFileDB) -> Result<T> + Send,
) -> T {
  thread::scope(|scope| {
    scope
      .spawn(|| write_tx(db, ops))
      .join()
      .expect("writer thread")
  })
  .expect("concurrent commit")
}

#[test]
fn upsert_edge_noop_conflicts_with_edge_committed_after_start() {
  let Fixture {
    _dir,
    db,
    a,
    b,
    t,
    weight,
    ..
  } = fixture();
  db.begin(false).expect("begin");
  commit_on_other_thread(&db, |db| db.add_edge(a, t, b));

  let created = db
    .upsert_edge_with_props(a, t, b, [(weight, Some(PropValue::I64(7)))])
    .expect("upsert edge");

  assert!(
    !created,
    "the edge is committed, so upsert must not add it again"
  );
  let result = db.commit();
  assert!(
    matches!(result, Err(KiteError::Conflict { .. })),
    "the no-op upsert depends on an edge committed after the tx started, got {result:?}"
  );
  close_single_file(db).expect("close");
}

#[test]
fn create_node_with_id_rejection_conflicts_with_node_committed_after_start() {
  let Fixture { _dir, db, .. } = fixture();
  let id: NodeId = 100;
  db.begin(false).expect("begin");
  commit_on_other_thread(&db, |db| db.create_node_with_id(id, None));

  let result = db.create_node_with_id(id, None);
  assert!(
    result.is_err(),
    "node {id} is committed, so create_node_with_id must fail, got {result:?}"
  );
  db.create_node(None).expect("unrelated create");
  let result = db.commit();
  assert!(
    matches!(result, Err(KiteError::Conflict { .. })),
    "the rejected create depends on node {id}, committed after the tx started, got {result:?}"
  );
  close_single_file(db).expect("close");
}
