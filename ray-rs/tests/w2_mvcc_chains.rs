//! Wave-2 repros for lane mvcc-chains: MVCC version chains (V1-V3).
//!
//! V1: `apply_mvcc_commit` appends versions only while another transaction is
//! active, and reads trust an existing chain head over the delta and the
//! snapshot. A change committed while a reader is open creates a chain; a later
//! change committed with no other transaction open is not appended, so the
//! chain head keeps the older state. Checkpoints clear the delta and replace the
//! snapshot without touching the chains. The stale head feeds plain reads, the
//! Kite API, replica apply and `check()`.
//!
//! V2: deleting a node or an edge appends a tombstone with no baseline version,
//! so a reader whose snapshot predates the delete no longer sees what it saw.
//!
//! V3: conflict keys. Prop writes record only the prop key, so they do not
//! conflict with a concurrent delete of their node or edge, while label writes
//! record `Node(N)` and conflict with readers that only checked existence.
//!
//! GC keeps running (20 ms interval), but a one-hour retention keeps every chain
//! a test builds, so GC never hides a stale head. "Committed state" below is
//! the database reopened without MVCC (WAL replay or snapshot, no chains).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use kitedb::api::kite::{EdgeDef, Kite, KiteOptions, NodeDef, PropDef};
use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions, SyncMode,
};
use kitedb::error::{KiteError, Result};
use kitedb::replication::types::ReplicationRole;
use kitedb::types::{ETypeId, LabelId, NodeId, PropKeyId, PropValue};

const GC_INTERVAL_MS: u64 = 20;
const RETENTION_MS: u64 = 60 * 60 * 1000;

fn options(mvcc: bool) -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .mvcc(mvcc)
    .mvcc_gc_interval_ms(GC_INTERVAL_MS)
    .mvcc_retention_ms(RETENTION_MS)
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
  other: PropKeyId,
  label: LabelId,
}

/// Nodes `a` (key "a") and `b` (key "b"), edge type `t`, prop keys `weight`
/// and `other`, label `label`: committed with no other transaction open, so
/// no version chains exist yet.
fn fixture() -> Fixture {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("mvcc-chains.kitedb");
  let db = open_single_file(&path, options(true)).expect("open");
  let (a, b, t, weight, other, label) = write_tx(&db, |db| {
    Ok((
      db.create_node(Some("a"))?,
      db.create_node(Some("b"))?,
      db.define_etype("T")?,
      db.define_propkey("weight")?,
      db.define_propkey("other")?,
      db.define_label("L")?,
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
    other,
    label,
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

/// A read transaction on another thread reads with `read`, then `write` runs
/// on this thread, then the same transaction reads again. Returns both reads.
fn reader_before_and_after<T: Send>(
  db: &SingleFileDB,
  read: impl Fn(&SingleFileDB) -> T + Sync,
  write: impl FnOnce(),
) -> (T, T) {
  let (ready_tx, ready_rx) = mpsc::channel();
  let (go_tx, go_rx) = mpsc::channel::<()>();
  let read = &read;
  thread::scope(|scope| {
    let reader = scope.spawn(move || {
      db.begin(true).expect("begin reader");
      let before = read(db);
      ready_tx.send(()).expect("reader ready");
      let _ = go_rx.recv();
      let after = read(db);
      db.commit().expect("end reader");
      (before, after)
    });
    ready_rx.recv().expect("reader started");
    write();
    drop(go_tx);
    reader.join().expect("reader thread")
  })
}

/// The same read outside any transaction and in a read transaction begun now.
fn read_outside_and_in_tx<T>(db: &SingleFileDB, read: impl Fn(&SingleFileDB) -> T) -> (T, T) {
  let outside = read(db);
  db.begin(true).expect("begin read tx");
  let inside = read(db);
  db.commit().expect("end read tx");
  (outside, inside)
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

fn maybe_checkpoint(db: &SingleFileDB, checkpoint: bool) {
  if checkpoint {
    db.checkpoint().expect("checkpoint");
  }
}

fn i64v(value: i64) -> Option<PropValue> {
  Some(PropValue::I64(value))
}

// ============================================================================
// V1: a stale chain head shadows a later solo commit (and the checkpoint that
// follows it). Each scenario runs without and with a checkpoint between the
// solo commit and the reads.
// ============================================================================

/// Chain for (a, weight): set to 1 while a reader is open, then to 2 alone.
fn stale_node_prop(checkpoint: bool) {
  let Fixture {
    _dir,
    path,
    db,
    a,
    weight,
    ..
  } = fixture();
  with_reader_open(&db, || {
    write_tx(&db, |db| db.set_node_prop(a, weight, PropValue::I64(1)))
  })
  .expect("set weight 1 with a reader open");
  write_tx(&db, |db| db.set_node_prop(a, weight, PropValue::I64(2))).expect("set weight 2 alone");
  maybe_checkpoint(&db, checkpoint);

  let (outside, inside) = read_outside_and_in_tx(&db, |db| db.node_prop(a, weight));
  let all = db
    .node_props(a)
    .and_then(|props| props.get(&weight).cloned());
  let committed = reopen_without_mvcc(&path, db, |db| db.node_prop(a, weight));

  assert_eq!(committed, i64v(2), "committed state");
  assert_eq!(
    (outside, inside, all),
    (i64v(2), i64v(2), i64v(2)),
    "(node_prop outside a tx, node_prop in a new tx, node_props) must return the last \
     committed value 2, not 1 from the chain head written while a reader was open \
     (checkpoint in between: {checkpoint})"
  );
}

#[test]
fn v1_node_prop_stale_after_solo_commit() {
  stale_node_prop(false);
}

#[test]
fn v1_node_prop_stale_after_solo_commit_and_checkpoint() {
  stale_node_prop(true);
}

/// Chain for (a -t-> b, weight): set to 1 while a reader is open, then to 2
/// alone.
fn stale_edge_prop(checkpoint: bool) {
  let Fixture {
    _dir,
    path,
    db,
    a,
    b,
    t,
    weight,
    ..
  } = fixture();
  write_tx(&db, |db| db.add_edge(a, t, b)).expect("add edge");
  with_reader_open(&db, || {
    write_tx(&db, |db| {
      db.set_edge_prop(a, t, b, weight, PropValue::I64(1))
    })
  })
  .expect("set weight 1 with a reader open");
  write_tx(&db, |db| {
    db.set_edge_prop(a, t, b, weight, PropValue::I64(2))
  })
  .expect("set weight 2 alone");
  maybe_checkpoint(&db, checkpoint);

  let (outside, inside) = read_outside_and_in_tx(&db, |db| db.edge_prop(a, t, b, weight));
  let all = db
    .edge_props(a, t, b)
    .and_then(|props| props.get(&weight).cloned());
  let committed = reopen_without_mvcc(&path, db, |db| db.edge_prop(a, t, b, weight));

  assert_eq!(committed, i64v(2), "committed state");
  assert_eq!(
    (outside, inside, all),
    (i64v(2), i64v(2), i64v(2)),
    "(edge_prop outside a tx, edge_prop in a new tx, edge_props) must return the last \
     committed value 2, not 1 from a stale chain head (checkpoint in between: {checkpoint})"
  );
}

#[test]
fn v1_edge_prop_stale_after_solo_commit() {
  stale_edge_prop(false);
}

#[test]
fn v1_edge_prop_stale_after_solo_commit_and_checkpoint() {
  stale_edge_prop(true);
}

/// Node chain says "exists": created while a reader is open, deleted alone.
fn stale_live_node(checkpoint: bool) {
  let Fixture { _dir, path, db, .. } = fixture();
  let node = with_reader_open(&db, || write_tx(&db, |db| db.create_node(None)))
    .expect("create with a reader open");
  write_tx(&db, |db| db.delete_node(node)).expect("delete alone");
  maybe_checkpoint(&db, checkpoint);

  let (outside, inside) = read_outside_and_in_tx(&db, |db| db.node_exists(node));
  let committed = reopen_without_mvcc(&path, db, |db| db.node_exists(node));

  assert!(!committed, "committed state: node {node} is deleted");
  assert_eq!(
    (outside, inside),
    (false, false),
    "(node_exists outside a tx, in a new tx) for deleted node {node} must be false; the \
     chain head written while a reader was open says it exists (checkpoint in between: \
     {checkpoint})"
  );
}

#[test]
fn v1_node_exists_stale_after_solo_delete() {
  stale_live_node(false);
}

#[test]
fn v1_node_exists_stale_after_solo_delete_and_checkpoint() {
  stale_live_node(true);
}

/// Node chain says "deleted": deleted while a reader is open, re-created (same
/// id and key) alone.
fn stale_deleted_node(checkpoint: bool) {
  let Fixture {
    _dir, path, db, a, ..
  } = fixture();
  with_reader_open(&db, || write_tx(&db, |db| db.delete_node(a)))
    .expect("delete with a reader open");
  write_tx(&db, |db| db.create_node_with_id(a, Some("a"))).expect("re-create alone");
  maybe_checkpoint(&db, checkpoint);

  let (outside, inside) = read_outside_and_in_tx(&db, |db| db.node_exists(a));
  let listed = db.list_nodes().contains(&a);
  let by_key = db.node_by_key("a");
  let committed = reopen_without_mvcc(&path, db, |db| (db.node_exists(a), db.node_by_key("a")));

  assert_eq!(committed, (true, Some(a)), "committed state");
  assert_eq!(
    (outside, inside, listed, by_key),
    (true, true, true, Some(a)),
    "(node_exists outside a tx, in a new tx, list_nodes contains, node_by_key) for live \
     node {a} must all find it; the chain head written while a reader was open says it is \
     deleted (checkpoint in between: {checkpoint})"
  );
}

#[test]
fn v1_node_deleted_stale_after_solo_recreate() {
  stale_deleted_node(false);
}

#[test]
fn v1_node_deleted_stale_after_solo_recreate_and_checkpoint() {
  stale_deleted_node(true);
}

/// Edge chain says "exists": added while a reader is open, deleted alone.
fn stale_live_edge(checkpoint: bool) {
  let Fixture {
    _dir,
    path,
    db,
    a,
    b,
    t,
    ..
  } = fixture();
  with_reader_open(&db, || write_tx(&db, |db| db.add_edge(a, t, b)))
    .expect("add with a reader open");
  write_tx(&db, |db| db.delete_edge(a, t, b)).expect("delete alone");
  maybe_checkpoint(&db, checkpoint);

  let (outside, inside) = read_outside_and_in_tx(&db, |db| db.edge_exists(a, t, b));
  let committed = reopen_without_mvcc(&path, db, |db| db.edge_exists(a, t, b));

  assert!(!committed, "committed state: the edge is deleted");
  assert_eq!(
    (outside, inside),
    (false, false),
    "(edge_exists outside a tx, in a new tx) for the deleted edge must be false; the chain \
     head written while a reader was open says it exists (checkpoint in between: \
     {checkpoint})"
  );
}

#[test]
fn v1_edge_exists_stale_after_solo_delete() {
  stale_live_edge(false);
}

#[test]
fn v1_edge_exists_stale_after_solo_delete_and_checkpoint() {
  stale_live_edge(true);
}

/// Edge chain says "deleted": deleted while a reader is open, re-added alone.
fn stale_deleted_edge(checkpoint: bool) {
  let Fixture {
    _dir,
    path,
    db,
    a,
    b,
    t,
    ..
  } = fixture();
  write_tx(&db, |db| db.add_edge(a, t, b)).expect("add");
  with_reader_open(&db, || write_tx(&db, |db| db.delete_edge(a, t, b)))
    .expect("delete with a reader open");
  write_tx(&db, |db| db.add_edge(a, t, b)).expect("re-add alone");
  maybe_checkpoint(&db, checkpoint);

  let (outside, inside) = read_outside_and_in_tx(&db, |db| db.edge_exists(a, t, b));
  let out = db.out_edges(a).contains(&(t, b));
  let listed = db
    .list_edges(None)
    .iter()
    .any(|edge| (edge.src, edge.etype, edge.dst) == (a, t, b));
  let committed = reopen_without_mvcc(&path, db, |db| {
    (db.edge_exists(a, t, b), db.out_edges(a).contains(&(t, b)))
  });

  assert_eq!(committed, (true, true), "committed state");
  assert_eq!(
    (outside, inside, out, listed),
    (true, true, true, true),
    "(edge_exists outside a tx, in a new tx, out_edges contains, list_edges contains) for \
     the re-added edge must all find it; the chain head written while a reader was open \
     says it is deleted (checkpoint in between: {checkpoint})"
  );
}

#[test]
fn v1_edge_deleted_stale_after_solo_readd() {
  stale_deleted_edge(false);
}

#[test]
fn v1_edge_deleted_stale_after_solo_readd_and_checkpoint() {
  stale_deleted_edge(true);
}

/// Label chain says "has label": added while a reader is open, removed alone.
fn stale_label(checkpoint: bool) {
  let Fixture {
    _dir,
    path,
    db,
    a,
    label,
    ..
  } = fixture();
  with_reader_open(&db, || write_tx(&db, |db| db.add_node_label(a, label)))
    .expect("add label with a reader open");
  write_tx(&db, |db| db.remove_node_label(a, label)).expect("remove label alone");
  maybe_checkpoint(&db, checkpoint);

  let (outside, inside) = read_outside_and_in_tx(&db, |db| db.node_has_label(a, label));
  let listed = db.node_labels(a).contains(&label);
  let committed = reopen_without_mvcc(&path, db, |db| db.node_has_label(a, label));

  assert!(!committed, "committed state: the label was removed");
  assert_eq!(
    (outside, inside, listed),
    (false, false, false),
    "(node_has_label outside a tx, in a new tx, node_labels contains) for the removed \
     label must be false; the chain head written while a reader was open says it is set \
     (checkpoint in between: {checkpoint})"
  );
}

#[test]
fn v1_label_stale_after_solo_remove() {
  stale_label(false);
}

#[test]
fn v1_label_stale_after_solo_remove_and_checkpoint() {
  stale_label(true);
}

/// Chains a solo commit invalidates without touching their keys: `delete_node`
/// lists only the node in its pending delta, not its prop and label keys. The
/// prop and label chains of `a` (written while a reader was open) survive a
/// solo delete and re-create of `a`, which has no props or labels.
fn stale_prop_and_label_after_recreate(checkpoint: bool) {
  let Fixture {
    _dir,
    path,
    db,
    a,
    weight,
    label,
    ..
  } = fixture();
  with_reader_open(&db, || {
    write_tx(&db, |db| {
      db.set_node_prop(a, weight, PropValue::I64(1))?;
      db.add_node_label(a, label)
    })
  })
  .expect("set prop and label with a reader open");
  write_tx(&db, |db| db.delete_node(a)).expect("delete alone");
  write_tx(&db, |db| db.create_node_with_id(a, Some("a"))).expect("re-create alone");
  maybe_checkpoint(&db, checkpoint);

  let (outside, inside) = read_outside_and_in_tx(&db, |db| {
    (db.node_prop(a, weight), db.node_has_label(a, label))
  });
  let committed = reopen_without_mvcc(&path, db, |db| {
    (
      db.node_exists(a),
      db.node_prop(a, weight),
      db.node_has_label(a, label),
    )
  });

  assert_eq!(committed, (true, None, false), "committed state");
  assert_eq!(
    (outside, inside),
    ((None, false), (None, false)),
    "re-created node {a} has no prop or label; (node_prop, node_has_label) outside a tx \
     and in a new tx come from chains of the deleted node (checkpoint in between: \
     {checkpoint})"
  );
}

#[test]
fn v1_prop_and_label_chains_survive_solo_delete_and_recreate() {
  stale_prop_and_label_after_recreate(false);
}

#[test]
fn v1_prop_and_label_chains_survive_solo_delete_and_recreate_and_checkpoint() {
  stale_prop_and_label_after_recreate(true);
}

/// Same for incident edges: deleting `b` drops `a -t-> b` without listing the
/// edge in the pending delta, so its "exists" chain survives a solo delete and
/// re-create of `b`.
fn stale_edge_after_endpoint_recreate(checkpoint: bool) {
  let Fixture {
    _dir,
    path,
    db,
    a,
    b,
    t,
    ..
  } = fixture();
  with_reader_open(&db, || write_tx(&db, |db| db.add_edge(a, t, b)))
    .expect("add edge with a reader open");
  write_tx(&db, |db| db.delete_node(b)).expect("delete b alone");
  write_tx(&db, |db| db.create_node_with_id(b, Some("b"))).expect("re-create b alone");
  maybe_checkpoint(&db, checkpoint);

  let (outside, inside) = read_outside_and_in_tx(&db, |db| db.edge_exists(a, t, b));
  let committed = reopen_without_mvcc(&path, db, |db| (db.node_exists(b), db.edge_exists(a, t, b)));

  assert_eq!(committed, (true, false), "committed state");
  assert_eq!(
    (outside, inside),
    (false, false),
    "deleting {b} dropped a -t-> b and re-created {b} has no edges; edge_exists outside a tx \
     and in a new tx comes from the edge's chain (checkpoint in between: {checkpoint})"
  );
}

#[test]
fn v1_edge_chain_survives_solo_endpoint_delete_and_recreate() {
  stale_edge_after_endpoint_recreate(false);
}

#[test]
fn v1_edge_chain_survives_solo_endpoint_delete_and_recreate_and_checkpoint() {
  stale_edge_after_endpoint_recreate(true);
}

/// The independent review's symptom: after commit n, a live read returns
/// n - 1. Commits alternate between "a reader is open" and "alone", with
/// blocking and background checkpoints in between.
#[test]
fn v1_live_read_returns_latest_seq_across_checkpoints() {
  let Fixture {
    _dir,
    db,
    a,
    weight,
    ..
  } = fixture();
  let mut stale = Vec::new();
  for seq in 1..=8i64 {
    let set = |db: &SingleFileDB| db.set_node_prop(a, weight, PropValue::I64(seq));
    let committed = if seq % 3 == 1 {
      with_reader_open(&db, || write_tx(&db, set))
    } else {
      write_tx(&db, set)
    };
    committed.expect("commit seq");
    match seq % 4 {
      2 => db.checkpoint().expect("checkpoint"),
      0 => db.background_checkpoint().expect("background checkpoint"),
      _ => {}
    }
    let read = db.node_prop(a, weight);
    if read != i64v(seq) {
      stale.push((seq, read));
    }
  }
  assert!(
    stale.is_empty(),
    "after commit n a live read must return n; stale reads (n, read): {stale:?}"
  );
}

/// Guard (passes on 39fefea): a reader open across a background checkpoint
/// keeps its snapshot, even though the installed snapshot holds newer state.
/// A fix that drops or re-anchors chains at install must keep this.
#[test]
fn v1_guard_reader_keeps_snapshot_across_background_checkpoint() {
  let Fixture {
    _dir,
    db,
    a,
    b,
    t,
    weight,
    label,
    ..
  } = fixture();
  write_tx(&db, |db| db.set_node_prop(a, weight, PropValue::I64(0))).expect("seed weight");

  let mut created = None;
  let (before, after) = reader_before_and_after(
    &db,
    |db| {
      (
        db.node_prop(a, weight),
        db.edge_exists(a, t, b),
        db.out_edges(a),
        db.node_has_label(a, label),
        db.count_nodes(),
      )
    },
    || {
      let node = write_tx(&db, |db| {
        db.set_node_prop(a, weight, PropValue::I64(1))?;
        db.add_edge(a, t, b)?;
        db.add_node_label(a, label)?;
        db.create_node(None)
      })
      .expect("write with the reader open");
      created = Some(node);
      db.background_checkpoint().expect("background checkpoint");
    },
  );

  assert_eq!(
    after, before,
    "a reader's snapshot must not change when a background checkpoint installs newer state"
  );
  let node = created.expect("created node");
  assert_eq!(db.node_prop(a, weight), i64v(1));
  assert!(db.edge_exists(a, t, b));
  assert!(db.node_has_label(a, label));
  assert!(db.node_exists(node));
  close_single_file(db).expect("close");
}

// ============================================================================
// V1 consumers: Kite API
// ============================================================================

fn kite_options(mvcc: bool) -> KiteOptions {
  KiteOptions::new()
    .node(NodeDef::new("User", "user:").prop(PropDef::string("name")))
    .edge(EdgeDef::new("KNOWS").prop(PropDef::int("since")))
    .mvcc(mvcc)
    .mvcc_gc_interval_ms(GC_INTERVAL_MS)
    .mvcc_retention_ms(RETENTION_MS)
}

fn name_props(name: &str) -> HashMap<String, PropValue> {
  HashMap::from([("name".to_string(), PropValue::String(name.into()))])
}

fn name(value: &str) -> Option<PropValue> {
  Some(PropValue::String(value.into()))
}

struct KiteFixture {
  _dir: tempfile::TempDir,
  path: PathBuf,
  kite: Kite,
  alice: NodeId,
  bob: NodeId,
  knows: ETypeId,
}

/// Users alice and bob, created with no other transaction open.
fn kite_fixture() -> KiteFixture {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("kite-chains.kitedb");
  let mut kite = Kite::open(&path, kite_options(true)).expect("open kite");
  let alice = kite
    .create_node("User", "alice", name_props("Alice"))
    .expect("create alice")
    .id();
  let bob = kite
    .create_node("User", "bob", name_props("Bob"))
    .expect("create bob")
    .id();
  let knows = kite.raw().etype_id("KNOWS").expect("KNOWS etype");
  KiteFixture {
    _dir: dir,
    path,
    kite,
    alice,
    bob,
    knows,
  }
}

fn reopen_kite_without_mvcc<T>(path: &Path, kite: Kite, read: impl FnOnce(&Kite) -> T) -> T {
  kite.close().expect("close kite");
  let kite = Kite::open(path, kite_options(false)).expect("reopen kite without mvcc");
  let value = read(&kite);
  kite.close().expect("close reopened kite");
  value
}

/// Leaves `node`'s chain saying "deleted" while the node is live: deleted with
/// a reader open, re-created (same id and key) alone.
fn make_stale_deleted_kite_node(kite: &Kite, node: NodeId, key: &str) {
  let db = kite.raw();
  with_reader_open(db, || write_tx(db, |db| db.delete_node(node)))
    .expect("delete with a reader open");
  write_tx(db, |db| db.create_node_with_id(node, Some(key))).expect("re-create alone");
}

/// A node whose chain says "exists" while it is deleted: created with a reader
/// open, deleted alone.
fn make_stale_live_kite_node(kite: &Kite, key: &str) -> NodeId {
  let db = kite.raw();
  let node = with_reader_open(db, || write_tx(db, |db| db.create_node(Some(key))))
    .expect("create with a reader open");
  write_tx(db, |db| db.delete_node(node)).expect("delete alone");
  node
}

/// Leaves the edge's chain saying "deleted" while it is live: deleted with a
/// reader open, re-added alone.
fn make_stale_deleted_kite_edge(kite: &Kite, src: NodeId, etype: ETypeId, dst: NodeId) {
  let db = kite.raw();
  with_reader_open(db, || write_tx(db, |db| db.delete_edge(src, etype, dst)))
    .expect("delete edge with a reader open");
  write_tx(db, |db| db.add_edge(src, etype, dst)).expect("re-add edge alone");
}

fn kite_delete_node_behind_stale_deleted_chain(checkpoint: bool) {
  let KiteFixture {
    _dir,
    path,
    mut kite,
    alice,
    ..
  } = kite_fixture();
  make_stale_deleted_kite_node(&kite, alice, "user:alice");
  maybe_checkpoint(kite.raw(), checkpoint);

  let deleted = kite.delete_node(alice);
  let exists = reopen_kite_without_mvcc(&path, kite, |kite| kite.exists(alice));

  assert!(
    matches!(deleted, Ok(true)),
    "alice ({alice}) is live, so Kite::delete_node must delete it and return true, got \
     {deleted:?} (checkpoint before: {checkpoint})"
  );
  assert!(!exists, "alice still exists after Kite::delete_node");
}

#[test]
fn v1_kite_delete_node_is_noop_behind_stale_deleted_chain() {
  kite_delete_node_behind_stale_deleted_chain(false);
}

#[test]
fn v1_kite_delete_node_is_noop_behind_stale_deleted_chain_after_checkpoint() {
  kite_delete_node_behind_stale_deleted_chain(true);
}

#[test]
fn v1_kite_unlink_is_noop_behind_stale_deleted_edge_chain() {
  let KiteFixture {
    _dir,
    path,
    mut kite,
    alice,
    bob,
    knows,
  } = kite_fixture();
  kite.link(alice, "KNOWS", bob).expect("link");
  make_stale_deleted_kite_edge(&kite, alice, knows, bob);

  let removed = kite.unlink(alice, "KNOWS", bob);
  let exists = reopen_kite_without_mvcc(&path, kite, |kite| {
    kite.has_edge(alice, "KNOWS", bob).expect("has_edge")
  });

  assert!(
    matches!(removed, Ok(true)),
    "alice -KNOWS-> bob is live, so Kite::unlink must remove it and return true, got \
     {removed:?}"
  );
  assert!(!exists, "the edge still exists after Kite::unlink");
}

#[test]
fn v1_kite_upsert_by_key_fails_behind_stale_deleted_chain() {
  let KiteFixture {
    _dir,
    path,
    mut kite,
    alice,
    ..
  } = kite_fixture();
  make_stale_deleted_kite_node(&kite, alice, "user:alice");

  let result = kite
    .upsert("User")
    .and_then(|upsert| upsert.values("alice", name_props("Alice 2")))
    .and_then(|upsert| upsert.returning())
    .map(|node| node.id());
  let (owner, stored) = reopen_kite_without_mvcc(&path, kite, |kite| {
    (
      kite.raw().node_by_key("user:alice"),
      kite.prop(alice, "name"),
    )
  });

  assert!(
    matches!(result, Ok(id) if id == alice),
    "user:alice is held by live node {alice}, so the upsert must update it, got {result:?}"
  );
  assert_eq!(owner, Some(alice));
  assert_eq!(stored, name("Alice 2"));
}

#[test]
fn v1_kite_upsert_by_id_updates_deleted_node_behind_stale_live_chain() {
  let KiteFixture {
    _dir,
    path,
    mut kite,
    ..
  } = kite_fixture();
  let carol = make_stale_live_kite_node(&kite, "user:carol");

  let result = kite.upsert_by_id("User", carol).and_then(|upsert| {
    upsert
      .set("name", PropValue::String("Carol".into()))
      .execute()
  });
  let (exists, stored) = reopen_kite_without_mvcc(&path, kite, |kite| {
    (kite.exists(carol), kite.prop(carol, "name"))
  });

  assert!(result.is_ok(), "upsert_by_id failed: {result:?}");
  assert_eq!(
    (exists, stored),
    (true, name("Carol")),
    "node {carol} was deleted, so upsert_by_id must create it with name Carol; instead it \
     wrote the prop to the deleted node"
  );
}

#[test]
fn v1_kite_update_by_id_rejects_live_node_behind_stale_deleted_chain() {
  let KiteFixture {
    _dir,
    path,
    mut kite,
    alice,
    ..
  } = kite_fixture();
  make_stale_deleted_kite_node(&kite, alice, "user:alice");

  let result = kite.update_by_id(alice).and_then(|update| {
    update
      .set("name", PropValue::String("Alice 2".into()))
      .execute()
  });
  let stored = reopen_kite_without_mvcc(&path, kite, |kite| kite.prop(alice, "name"));

  assert!(
    result.is_ok(),
    "alice ({alice}) is live, so update_by_id must succeed, got {result:?}"
  );
  assert_eq!(stored, name("Alice 2"));
}

#[test]
fn v1_kite_set_prop_writes_to_deleted_node_behind_stale_live_chain() {
  let KiteFixture {
    _dir,
    path,
    mut kite,
    ..
  } = kite_fixture();
  let carol = make_stale_live_kite_node(&kite, "user:carol");

  let result = kite.set_prop(carol, "name", PropValue::String("Carol".into()));
  let exists = reopen_kite_without_mvcc(&path, kite, |kite| kite.exists(carol));

  assert!(!exists, "committed state: node {carol} is deleted");
  assert!(
    matches!(result, Err(KiteError::NodeNotFound(node)) if node == carol),
    "node {carol} is deleted, so set_prop must fail with NodeNotFound, got {result:?}"
  );
}

#[test]
fn v1_kite_set_edge_prop_rejects_live_edge_behind_stale_deleted_chain() {
  let KiteFixture {
    _dir,
    path,
    mut kite,
    alice,
    bob,
    knows,
  } = kite_fixture();
  kite.link(alice, "KNOWS", bob).expect("link");
  make_stale_deleted_kite_edge(&kite, alice, knows, bob);

  let result = kite.set_edge_prop(alice, "KNOWS", bob, "since", PropValue::I64(2020));
  let stored = reopen_kite_without_mvcc(&path, kite, |kite| {
    kite
      .edge_prop(alice, "KNOWS", bob, "since")
      .expect("edge_prop")
  });

  assert!(
    result.is_ok(),
    "alice -KNOWS-> bob is live, so set_edge_prop must succeed, got {result:?}"
  );
  assert_eq!(stored, i64v(2020));
}

// ============================================================================
// V1 consumers: check() and replica apply
// ============================================================================

/// `check()` cannot report a false *error* from a stale chain: `list_edges`,
/// `node_exists` and `edge_exists` all read the same chain-first view, so they
/// agree with each other. It does report a false warning, and skips every
/// integrity check, when stale chains hide every live node.
#[test]
fn v1_check_sees_no_nodes_behind_stale_deleted_chain() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("check.kitedb");
  let db = open_single_file(&path, options(true)).expect("open");
  let node = write_tx(&db, |db| db.create_node(Some("only"))).expect("create");
  with_reader_open(&db, || write_tx(&db, |db| db.delete_node(node)))
    .expect("delete with a reader open");
  write_tx(&db, |db| db.create_node_with_id(node, Some("only"))).expect("re-create alone");

  let result = db.check();
  let committed_nodes = reopen_without_mvcc(&path, db, |db| db.count_nodes());

  assert_eq!(committed_nodes, 1, "committed state");
  assert!(
    !result
      .warnings
      .iter()
      .any(|warning| warning.contains("No nodes")),
    "the database holds live node {node}, but check() saw none: {result:?}"
  );
}

fn open_primary(path: &Path) -> SingleFileDB {
  open_single_file(
    path,
    SingleFileOpenOptions::new()
      .sync_mode(SyncMode::Full)
      .auto_checkpoint(false)
      .replication_role(ReplicationRole::Primary),
  )
  .expect("open primary")
}

fn replica_options(primary_path: &Path, mvcc: bool) -> SingleFileOpenOptions {
  options(mvcc)
    .replication_role(ReplicationRole::Replica)
    .replication_source_db_path(primary_path)
}

fn catch_up_all(replica: &SingleFileDB) -> usize {
  let mut total = 0usize;
  loop {
    let applied = replica.replica_catch_up_once(64).expect("catch up");
    if applied == 0 {
      return total;
    }
    total += applied;
  }
}

/// Replica apply skips an AddEdge when `edge_exists` says the edge is there:
/// a stale "exists" chain on an MVCC replica drops the re-add.
#[test]
fn v1_replica_skips_readd_behind_stale_chain() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("chains-primary.kitedb");
  let replica_path = dir.path().join("chains-replica.kitedb");
  let primary = open_primary(&primary_path);
  let replica =
    open_single_file(&replica_path, replica_options(&primary_path, true)).expect("open replica");
  replica
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap empty primary");

  let (a, b, t) = write_tx(&primary, |db| {
    Ok((
      db.create_node(Some("a"))?,
      db.create_node(Some("b"))?,
      db.define_etype("T")?,
    ))
  })
  .expect("seed primary");
  catch_up_all(&replica);

  // The add reaches the replica while a reader is open there: chain "exists".
  write_tx(&primary, |db| db.add_edge(a, t, b)).expect("primary add");
  with_reader_open(&replica, || catch_up_all(&replica));
  // Delete, then re-add, each applied alone.
  write_tx(&primary, |db| db.delete_edge(a, t, b)).expect("primary delete");
  catch_up_all(&replica);
  write_tx(&primary, |db| db.add_edge(a, t, b)).expect("primary re-add");
  catch_up_all(&replica);

  assert!(primary.edge_exists(a, t, b), "primary has the edge");
  let replica_edges_added = replica.stats().delta_edges_added;
  close_single_file(replica).expect("close replica");
  let replica = open_single_file(&replica_path, replica_options(&primary_path, false))
    .expect("reopen replica without mvcc");
  let replica_has_edge = replica.edge_exists(a, t, b);
  close_single_file(replica).expect("close reopened replica");
  close_single_file(primary).expect("close primary");

  assert!(
    replica_has_edge,
    "the replica dropped the primary's re-add of {a} -[{t}]-> {b} (replica delta edges \
     added before reopen: {replica_edges_added})"
  );
}

// ============================================================================
// V2: a delete hides the node or edge from readers whose snapshot predates it
//
// Point reads (existence, props, labels, edge_exists/edge_prop) consult the
// chains first, so baseline versions written at delete time can fix them.
// Enumerations and key lookups (out_edges, in_edges, list_nodes, list_edges,
// node_key, node_by_key) walk only the snapshot and the delta. When the
// deleted node or edge lives in the snapshot they still find it; when it lived
// only in the delta, the delete removed it from there, so those reads also
// need the read path to consult the chains. The delta-only tests are split so
// that scope shows.
// ============================================================================

/// Point reads of node `a` and its edge `a -t-> b`.
#[derive(Debug, PartialEq)]
struct NodePointView {
  exists: bool,
  weight: Option<PropValue>,
  props: Option<usize>,
  labels: Vec<LabelId>,
  edge_out: bool,
  edge_out_weight: Option<PropValue>,
}

/// Enumerations and key lookups that should include node `a`.
#[derive(Debug, PartialEq)]
struct NodeListingView {
  key: Option<String>,
  by_key: Option<NodeId>,
  out_edges: Vec<(ETypeId, NodeId)>,
  in_edges: Vec<(ETypeId, NodeId)>,
  b_in_edges: Vec<(ETypeId, NodeId)>,
  listed: bool,
}

type NodeView = (NodePointView, NodeListingView);

/// Node `a` (weight 5, label, edges a -t-> b with weight 7 and b -t-> a) as
/// a reader sees it before and after another transaction deletes `a`.
fn node_views_across_delete(checkpoint: bool) -> (NodeView, NodeView) {
  let Fixture {
    _dir,
    db,
    a,
    b,
    t,
    weight,
    label,
    ..
  } = fixture();
  write_tx(&db, |db| {
    db.set_node_prop(a, weight, PropValue::I64(5))?;
    db.add_node_label(a, label)?;
    db.add_edge(a, t, b)?;
    db.set_edge_prop(a, t, b, weight, PropValue::I64(7))?;
    db.add_edge(b, t, a)
  })
  .expect("seed a");
  maybe_checkpoint(&db, checkpoint);

  let (before, after) = reader_before_and_after(
    &db,
    |db| {
      (
        NodePointView {
          exists: db.node_exists(a),
          weight: db.node_prop(a, weight),
          props: db.node_props(a).map(|props| props.len()),
          labels: db.node_labels(a),
          edge_out: db.edge_exists(a, t, b),
          edge_out_weight: db.edge_prop(a, t, b, weight),
        },
        NodeListingView {
          key: db.node_key(a),
          by_key: db.node_by_key("a"),
          out_edges: db.out_edges(a),
          in_edges: db.in_edges(a),
          b_in_edges: db.in_edges(b),
          listed: db.list_nodes().contains(&a),
        },
      )
    },
    || write_tx(&db, |db| db.delete_node(a)).expect("delete a"),
  );

  assert!(before.0.exists, "the reader saw a before the delete");
  assert!(!db.node_exists(a), "a is deleted for new reads");
  close_single_file(db).expect("close");
  (before, after)
}

#[test]
fn v2_reader_keeps_node_deleted_after_it_began() {
  let ((before, _), (after, _)) = node_views_across_delete(false);
  assert_eq!(
    after, before,
    "reader R saw node a (created after the last checkpoint), its props, label and edge; \
     W deleted a and committed; R's point reads must not change"
  );
}

/// Needs read-path changes (read.rs, iter.rs) beyond chain baselines: see the
/// section comment.
#[test]
fn v2_reader_keeps_edges_and_key_of_node_deleted_after_it_began() {
  let ((_, before), (_, after)) = node_views_across_delete(false);
  assert_eq!(
    after, before,
    "reader R saw node a (created after the last checkpoint), its key and its edges in \
     out_edges/in_edges/list_nodes; W deleted a and committed; R's view must not change"
  );
}

#[test]
fn v2_reader_keeps_snapshot_node_deleted_after_it_began() {
  let (before, after) = node_views_across_delete(true);
  assert_eq!(
    after, before,
    "reader R saw node a (in the snapshot), its props, label, key and edges; W deleted a \
     and committed; R's view must not change"
  );
}

/// Point reads of the edge `a -t-> b`.
#[derive(Debug, PartialEq)]
struct EdgePointView {
  exists: bool,
  weight: Option<PropValue>,
  props: Option<usize>,
}

/// Enumerations that should include the edge `a -t-> b`.
#[derive(Debug, PartialEq)]
struct EdgeListingView {
  out_edges: Vec<(ETypeId, NodeId)>,
  in_edges: Vec<(ETypeId, NodeId)>,
  listed: bool,
}

type EdgeView = (EdgePointView, EdgeListingView);

/// The edge `a -t-> b` (weight 7) as a reader sees it before and after
/// another transaction deletes it.
fn edge_views_across_delete(checkpoint: bool) -> (EdgeView, EdgeView) {
  let Fixture {
    _dir,
    db,
    a,
    b,
    t,
    weight,
    ..
  } = fixture();
  write_tx(&db, |db| {
    db.add_edge(a, t, b)?;
    db.set_edge_prop(a, t, b, weight, PropValue::I64(7))
  })
  .expect("seed edge");
  maybe_checkpoint(&db, checkpoint);

  let (before, after) = reader_before_and_after(
    &db,
    |db| {
      (
        EdgePointView {
          exists: db.edge_exists(a, t, b),
          weight: db.edge_prop(a, t, b, weight),
          props: db.edge_props(a, t, b).map(|props| props.len()),
        },
        EdgeListingView {
          out_edges: db.out_edges(a),
          in_edges: db.in_edges(b),
          listed: db
            .list_edges(None)
            .iter()
            .any(|edge| (edge.src, edge.etype, edge.dst) == (a, t, b)),
        },
      )
    },
    || write_tx(&db, |db| db.delete_edge(a, t, b)).expect("delete edge"),
  );

  assert!(before.0.exists, "the reader saw the edge before the delete");
  assert!(
    !db.edge_exists(a, t, b),
    "the edge is deleted for new reads"
  );
  close_single_file(db).expect("close");
  (before, after)
}

#[test]
fn v2_reader_keeps_edge_deleted_after_it_began() {
  let ((before, _), (after, _)) = edge_views_across_delete(false);
  assert_eq!(
    after, before,
    "reader R saw edge a -t-> b (added after the last checkpoint) and its prop; W deleted \
     the edge and committed; R's point reads must not change"
  );
}

/// Needs read-path changes (read.rs, iter.rs) beyond chain baselines: see the
/// section comment.
#[test]
fn v2_reader_keeps_listing_of_edge_deleted_after_it_began() {
  let ((_, before), (_, after)) = edge_views_across_delete(false);
  assert_eq!(
    after, before,
    "reader R saw edge a -t-> b (added after the last checkpoint) in out_edges/in_edges/\
     list_edges; W deleted the edge and committed; R's view must not change"
  );
}

#[test]
fn v2_reader_keeps_snapshot_edge_deleted_after_it_began() {
  let (before, after) = edge_views_across_delete(true);
  assert_eq!(
    after, before,
    "reader R saw edge a -t-> b (in the snapshot) and its prop; W deleted the edge and \
     committed; R's view must not change"
  );
}

// ============================================================================
// V3: conflict keys
// ============================================================================

/// Commits `ops` on another thread while this thread's transaction is open.
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

fn is_conflict<T>(result: &Result<T>) -> bool {
  matches!(result, Err(KiteError::Conflict { .. }))
}

#[test]
fn v3_set_node_prop_conflicts_with_concurrent_delete_node() {
  let Fixture {
    _dir,
    path,
    db,
    a,
    weight,
    ..
  } = fixture();
  db.begin(false).expect("begin");
  db.set_node_prop(a, weight, PropValue::I64(9))
    .expect("set prop");
  commit_on_other_thread(&db, |db| db.delete_node(a));

  let result = db.commit();
  let exists = reopen_without_mvcc(&path, db, |db| db.node_exists(a));

  assert!(
    is_conflict(&result),
    "the prop write targets node {a}, deleted by a transaction that committed after this \
     one began; both must not commit, got {result:?} (node exists afterwards: {exists}, \
     so the committed prop write was lost)"
  );
}

#[test]
fn v3_set_edge_prop_conflicts_with_concurrent_delete_edge() {
  let Fixture {
    _dir,
    path,
    db,
    a,
    b,
    t,
    weight,
    ..
  } = fixture();
  write_tx(&db, |db| db.add_edge(a, t, b)).expect("add edge");
  db.begin(false).expect("begin");
  db.set_edge_prop(a, t, b, weight, PropValue::I64(9))
    .expect("set edge prop");
  commit_on_other_thread(&db, |db| db.delete_edge(a, t, b));

  let result = db.commit();
  let exists = reopen_without_mvcc(&path, db, |db| db.edge_exists(a, t, b));

  assert!(
    is_conflict(&result),
    "the edge prop write targets a -t-> b, deleted by a transaction that committed after \
     this one began; both must not commit, got {result:?} (edge exists afterwards: {exists})"
  );
}

#[test]
fn v3_set_edge_prop_conflicts_with_concurrent_endpoint_delete() {
  let Fixture {
    _dir,
    path,
    db,
    a,
    b,
    t,
    weight,
    ..
  } = fixture();
  write_tx(&db, |db| db.add_edge(a, t, b)).expect("add edge");
  db.begin(false).expect("begin");
  db.set_edge_prop(a, t, b, weight, PropValue::I64(9))
    .expect("set edge prop");
  commit_on_other_thread(&db, |db| db.delete_node(b));

  let result = db.commit();
  let exists = reopen_without_mvcc(&path, db, |db| db.edge_exists(a, t, b));

  assert!(
    is_conflict(&result),
    "the edge prop write targets a -t-> b, whose endpoint {b} was deleted by a transaction \
     that committed after this one began; both must not commit, got {result:?} (edge \
     exists afterwards: {exists})"
  );
}

#[test]
fn v3_add_node_label_does_not_conflict_with_node_exists_reader() {
  let Fixture {
    _dir,
    db,
    a,
    b,
    weight,
    label,
    ..
  } = fixture();
  db.begin(false).expect("begin");
  assert!(db.node_exists(a), "a exists");
  db.set_node_prop(b, weight, PropValue::I64(1))
    .expect("unrelated write");
  commit_on_other_thread(&db, |db| db.add_node_label(a, label));

  let result = db.commit();

  assert!(
    result.is_ok(),
    "this transaction only checked that {a} exists; a concurrent add_node_label does not \
     change that, so its commit must succeed, got {result:?}"
  );
  close_single_file(db).expect("close");
}

#[test]
fn v3_remove_node_label_does_not_conflict_with_node_exists_reader() {
  let Fixture {
    _dir,
    db,
    a,
    b,
    weight,
    label,
    ..
  } = fixture();
  write_tx(&db, |db| db.add_node_label(a, label)).expect("add label");
  db.begin(false).expect("begin");
  assert!(db.node_exists(a), "a exists");
  db.set_node_prop(b, weight, PropValue::I64(1))
    .expect("unrelated write");
  commit_on_other_thread(&db, |db| db.remove_node_label(a, label));

  let result = db.commit();

  assert!(
    result.is_ok(),
    "this transaction only checked that {a} exists; a concurrent remove_node_label does \
     not change that, so its commit must succeed, got {result:?}"
  );
  close_single_file(db).expect("close");
}

/// Guard (passes on 39fefea, through the `Node(N)` write that label writes
/// record): a label write and a concurrent delete of its node must not both
/// commit. Dropping that write without a replacement would break this.
#[test]
fn v3_guard_add_node_label_conflicts_with_concurrent_delete_node() {
  let Fixture {
    _dir, db, a, label, ..
  } = fixture();
  db.begin(false).expect("begin");
  db.add_node_label(a, label).expect("add label");
  commit_on_other_thread(&db, |db| db.delete_node(a));

  let result = db.commit();

  assert!(
    is_conflict(&result),
    "the label write targets node {a}, deleted concurrently; got {result:?}"
  );
  close_single_file(db).expect("close");
}

/// Guard (passes on 39fefea): prop writes to different keys of one node or
/// edge do not conflict. Recording `Node(N)` / `Edge` as a *write* for every
/// prop write would make them conflict.
#[test]
fn v3_guard_prop_writes_to_distinct_keys_do_not_conflict() {
  let Fixture {
    _dir,
    db,
    a,
    b,
    t,
    weight,
    other,
    ..
  } = fixture();
  write_tx(&db, |db| db.add_edge(a, t, b)).expect("add edge");
  db.begin(false).expect("begin");
  db.set_node_prop(a, weight, PropValue::I64(1))
    .expect("set node prop");
  db.set_edge_prop(a, t, b, weight, PropValue::I64(1))
    .expect("set edge prop");
  commit_on_other_thread(&db, |db| {
    db.set_node_prop(a, other, PropValue::I64(2))?;
    db.set_edge_prop(a, t, b, other, PropValue::I64(2))
  });

  let result = db.commit();

  assert!(
    result.is_ok(),
    "writes to different prop keys must not conflict, got {result:?}"
  );
  close_single_file(db).expect("close");
}
