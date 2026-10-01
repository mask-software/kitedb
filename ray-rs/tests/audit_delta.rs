//! Reproductions for delta-lane audit findings D1-D5.
//!
//! Each test encodes the intended contract and fails against the buggy code.

use kitedb::api::kite::{Kite, KiteOptions, NodeDef};
use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions,
};
use kitedb::types::{ETypeId, NodeId, PropKeyId, PropValue};
use std::collections::{HashMap, HashSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;

fn options() -> SingleFileOpenOptions {
  // Deterministic: no background or auto checkpoints.
  SingleFileOpenOptions::new().auto_checkpoint(false)
}

fn open(path: &Path) -> SingleFileDB {
  open_single_file(path, options()).expect("open")
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
  if let Some(s) = payload.downcast_ref::<&str>() {
    (*s).to_string()
  } else if let Some(s) = payload.downcast_ref::<String>() {
    s.clone()
  } else {
    "<non-string panic>".to_string()
  }
}

/// Creates nodes `a` and `b` with edge a -[T]-> b and checkpoints so the edge
/// lives in the snapshot, not in the delta.
fn snapshot_edge(db: &SingleFileDB) -> (NodeId, ETypeId, NodeId) {
  db.begin(false).expect("begin");
  let a = db.create_node(Some("a")).expect("create a");
  let b = db.create_node(Some("b")).expect("create b");
  let t = db.define_etype("T").expect("define etype");
  db.add_edge(a, t, b).expect("add edge");
  db.commit().expect("commit");
  db.checkpoint().expect("checkpoint");
  assert!(db.edge_exists(a, t, b), "edge must be in snapshot");
  (a, t, b)
}

// ============================================================================
// D1a: re-adding a snapshot edge then deleting it leaves it visible
// ============================================================================

#[test]
fn audit_d1a_unlink_after_relink_of_snapshot_edge() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("d1a.kitedb");

  let db = open(&path);
  let (a, t, b) = snapshot_edge(&db);

  db.begin(false).expect("begin relink");
  db.add_edge(a, t, b).expect("relink existing edge");
  db.commit().expect("commit relink");

  db.begin(false).expect("begin unlink");
  db.delete_edge(a, t, b).expect("unlink");
  db.commit().expect("commit unlink");

  assert!(
    !db.edge_exists(a, t, b),
    "edge {a}-[{t}]->{b} still visible after unlink (re-added snapshot edge)"
  );
  assert_eq!(db.count_edges(), 0, "count_edges after unlink");

  close_single_file(db).expect("close");
  let db = open(&path);
  assert!(
    !db.edge_exists(a, t, b),
    "edge {a}-[{t}]->{b} resurrected after reopen (WAL replay)"
  );
  assert_eq!(db.count_edges(), 0, "count_edges after reopen");
  close_single_file(db).expect("close reopened");
}

#[test]
fn audit_d1a_unlink_after_relink_same_tx() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("d1a_same_tx.kitedb");

  let db = open(&path);
  let (a, t, b) = snapshot_edge(&db);

  db.begin(false).expect("begin");
  db.add_edge(a, t, b).expect("relink existing edge");
  db.delete_edge(a, t, b).expect("unlink");
  assert!(
    !db.edge_exists(a, t, b),
    "edge visible inside tx after relink+unlink"
  );
  db.commit().expect("commit");

  assert!(
    !db.edge_exists(a, t, b),
    "edge {a}-[{t}]->{b} still visible after relink+unlink in one tx"
  );

  close_single_file(db).expect("close");
  let db = open(&path);
  assert!(!db.edge_exists(a, t, b), "edge resurrected after reopen");
  close_single_file(db).expect("close reopened");
}

#[test]
fn audit_d1a_relink_snapshot_edge_not_double_counted() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("d1a_count.kitedb");

  let db = open(&path);
  let (a, t, b) = snapshot_edge(&db);

  db.begin(false).expect("begin relink");
  db.add_edge(a, t, b)
    .expect("relink existing edge (idempotent)");
  db.commit().expect("commit relink");

  assert_eq!(db.count_edges(), 1, "count_edges after idempotent relink");
  assert_eq!(db.list_edges(None).len(), 1, "list_edges after relink");
  assert_eq!(db.out_degree(a), 1, "out_degree after relink");

  db.checkpoint().expect("checkpoint after relink");
  assert_eq!(db.count_edges(), 1, "count_edges after checkpoint");
  assert_eq!(db.out_degree(a), 1, "out_degree after checkpoint");

  close_single_file(db).expect("close");
  let db = open(&path);
  assert_eq!(db.count_edges(), 1, "count_edges after reopen");
  close_single_file(db).expect("close reopened");
}

// ============================================================================
// D1b: edges to missing nodes are accepted and poison checkpoints
// ============================================================================

#[test]
fn audit_d1b_add_edge_to_missing_node_rejected() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("d1b_reject.kitedb");
  let missing: NodeId = 999_999;

  let db = open(&path);
  db.begin(false).expect("begin");
  let a = db.create_node(Some("a")).expect("create a");
  let gone = db.create_node(Some("gone")).expect("create gone");
  let t = db.define_etype("T").expect("define etype");
  db.commit().expect("commit");

  db.begin(false).expect("begin delete");
  db.delete_node(gone).expect("delete gone");
  db.commit().expect("commit delete");

  db.begin(false).expect("begin edges");
  assert!(
    db.add_edge(a, t, missing).is_err(),
    "add_edge to nonexistent dst {missing} must fail"
  );
  assert!(
    db.add_edge(missing, t, a).is_err(),
    "add_edge from nonexistent src {missing} must fail"
  );
  assert!(
    db.add_edge(a, t, gone).is_err(),
    "add_edge to deleted node {gone} must fail"
  );
  // Endpoints visible only to this tx are valid.
  let fresh = db.create_node(Some("fresh")).expect("create fresh");
  db.add_edge(a, t, fresh)
    .expect("add_edge to node created in same tx");
  db.commit().expect("commit edges");

  assert!(db.edge_exists(a, t, fresh));
  assert!(!db.edge_exists(a, t, missing));
  close_single_file(db).expect("close");
}

#[test]
fn audit_d1b_dangling_edge_does_not_break_checkpoint() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("d1b_checkpoint.kitedb");
  let missing: NodeId = 999_999;

  let db = open(&path);
  db.begin(false).expect("begin");
  let a = db.create_node(Some("a")).expect("create a");
  let t = db.define_etype("T").expect("define etype");
  db.commit().expect("commit");

  db.begin(false).expect("begin dangling");
  // Rejecting is the intended behavior; if accepted, it must not poison the DB.
  let _ = db.add_edge(a, t, missing);
  db.commit().expect("commit dangling attempt");

  db.checkpoint()
    .expect("checkpoint after add_edge to missing node must succeed");

  db.begin(false).expect("begin later write");
  db.create_node(Some("later")).expect("create later");
  db.commit().expect("commit later write");
  close_single_file(db).expect("close");

  let db = open(&path);
  db.checkpoint()
    .expect("checkpoint after reopen (WAL replay) must succeed");
  assert!(db.node_by_key("later").is_some(), "later write lost");
  close_single_file(db).expect("close reopened");
}

#[test]
fn audit_d1b_other_edge_paths_reject_missing_node() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("d1b_paths.kitedb");
  let missing: NodeId = 999_999;

  let db = open(&path);
  db.begin(false).expect("begin");
  let a = db.create_node(Some("a")).expect("create a");
  let b = db.create_node(Some("b")).expect("create b");
  let t = db.define_etype("T").expect("define etype");
  let w = db.define_propkey("weight").expect("define propkey");
  db.commit().expect("commit");

  db.begin(false).expect("begin edges");
  let batch = db.add_edges_batch(&[(a, t, b), (a, t, missing)]);
  assert!(batch.is_err(), "add_edges_batch with missing dst must fail");
  let with_props = db.add_edge_with_props(a, t, missing, vec![(w, PropValue::I64(1))]);
  assert!(
    with_props.is_err(),
    "add_edge_with_props with missing dst must fail"
  );
  let props_batch =
    db.add_edges_with_props_batch(vec![(missing, t, b, vec![(w, PropValue::I64(1))])]);
  assert!(
    props_batch.is_err(),
    "add_edges_with_props_batch with missing src must fail"
  );
  let upsert = db.upsert_edge_with_props(a, t, missing, vec![(w, Some(PropValue::I64(1)))]);
  assert!(
    upsert.is_err(),
    "upsert_edge_with_props with missing dst must fail"
  );
  db.commit().expect("commit");

  db.checkpoint()
    .expect("checkpoint after rejected edge writes must succeed");
  assert!(!db.edge_exists(a, t, missing));
  close_single_file(db).expect("close");
}

// ============================================================================
// D2: duplicate keys are silently accepted
// ============================================================================

#[test]
fn audit_d2_duplicate_key_same_tx_rejected() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("d2_same_tx.kitedb");

  let db = open(&path);
  db.begin(false).expect("begin");
  let first = db.create_node(Some("user:alice")).expect("first create");
  let dup = db.create_node(Some("user:alice"));
  assert!(
    dup.is_err(),
    "duplicate key in same tx must fail, got {dup:?}"
  );
  db.commit().expect("commit");

  assert_eq!(db.node_by_key("user:alice"), Some(first));
  assert_eq!(db.count_nodes(), 1, "only one node may own the key");
  close_single_file(db).expect("close");
}

#[test]
fn audit_d2_duplicate_key_after_commit_rejected() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("d2_commit.kitedb");

  let db = open(&path);
  db.begin(false).expect("begin");
  let first = db.create_node(Some("user:alice")).expect("first create");
  db.commit().expect("commit");

  db.begin(false).expect("begin dup");
  let dup = db.create_node(Some("user:alice"));
  assert!(
    dup.is_err(),
    "duplicate of committed (delta) key must fail, got {dup:?}"
  );
  let dup_with_id = db.create_node_with_id(10_000, Some("user:alice"));
  assert!(
    dup_with_id.is_err(),
    "create_node_with_id with duplicate key must fail, got {dup_with_id:?}"
  );
  db.commit().expect("commit after rejected dup");

  assert_eq!(db.node_by_key("user:alice"), Some(first));
  assert_eq!(db.count_nodes(), 1);

  // Key becomes free again once its owner is deleted.
  db.begin(false).expect("begin delete");
  db.delete_node(first).expect("delete first");
  db.commit().expect("commit delete");
  db.begin(false).expect("begin recreate");
  let second = db
    .create_node(Some("user:alice"))
    .expect("recreate key after delete");
  db.commit().expect("commit recreate");
  assert_eq!(db.node_by_key("user:alice"), Some(second));
  close_single_file(db).expect("close");
}

#[test]
fn audit_d2_duplicate_key_in_snapshot_rejected() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("d2_snapshot.kitedb");

  let db = open(&path);
  db.begin(false).expect("begin");
  let first = db.create_node(Some("user:alice")).expect("first create");
  db.commit().expect("commit");
  db.checkpoint().expect("checkpoint");

  db.begin(false).expect("begin dup");
  let dup = db.create_node(Some("user:alice"));
  assert!(
    dup.is_err(),
    "duplicate of snapshot key must fail, got {dup:?}"
  );
  db.commit().expect("commit");

  assert_eq!(db.node_by_key("user:alice"), Some(first));
  assert_eq!(db.count_nodes(), 1);
  close_single_file(db).expect("close");
}

#[test]
fn audit_d2_duplicate_key_in_batch_rejected() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("d2_batch.kitedb");

  let db = open(&path);
  db.begin(false).expect("begin");
  let first = db.create_node(Some("user:alice")).expect("first create");
  db.commit().expect("commit");

  db.begin(false).expect("begin batch");
  let dup = db.create_nodes_batch(&[Some("user:bob"), Some("user:alice")]);
  assert!(
    dup.is_err(),
    "batch containing an existing key must fail, got {dup:?}"
  );
  db.rollback().expect("rollback batch");

  db.begin(false).expect("begin batch 2");
  let dup_in_batch = db.create_nodes_batch(&[Some("user:carol"), Some("user:carol")]);
  assert!(
    dup_in_batch.is_err(),
    "batch repeating a key must fail, got {dup_in_batch:?}"
  );
  db.rollback().expect("rollback batch 2");

  assert_eq!(db.node_by_key("user:alice"), Some(first));
  assert_eq!(db.count_nodes(), 1);
  close_single_file(db).expect("close");
}

#[test]
fn audit_d2_kite_duplicate_create_rejected_upsert_still_works() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("d2_kite");
  let schema = || {
    KiteOptions::new()
      .disable_close_checkpoint()
      .node(NodeDef::new("User", "user:"))
  };

  let mut kite = Kite::open(&path, schema()).expect("open");
  let alice = kite
    .create_node("User", "alice", HashMap::new())
    .expect("create alice");
  let dup = kite.create_node("User", "alice", HashMap::new());
  assert!(
    dup.is_err(),
    "Kite create_node with existing key must fail, got id {:?}",
    dup.as_ref().map(|n| n.id())
  );

  assert_eq!(
    kite.count_nodes(),
    1,
    "duplicate create must not add a node"
  );
  let found = kite.get("User", "alice").expect("get").expect("alice");
  assert_eq!(found.id(), alice.id());

  // DB stays usable and upsert resolves to the existing node.
  kite
    .create_node("User", "bob", HashMap::new())
    .expect("create bob after rejected dup");
  let upserted = kite
    .upsert("User")
    .expect("upsert builder")
    .values("alice", HashMap::new())
    .expect("values")
    .returning()
    .expect("upsert alice");
  assert_eq!(upserted.id(), alice.id());
  assert_eq!(kite.count_nodes(), 2);
  kite.close().expect("close");
}

// ============================================================================
// D3: deleted nodes keep their vectors
// ============================================================================

fn create_node_with_vector(db: &SingleFileDB, key: &str) -> (NodeId, PropKeyId) {
  db.begin(false).expect("begin");
  let node = db.create_node(Some(key)).expect("create node");
  let pk = db.define_propkey("embedding").expect("define propkey");
  db.set_node_vector(node, pk, &[1.0, 0.5, 0.25, 0.125])
    .expect("set vector");
  db.commit().expect("commit");
  assert!(db.has_node_vector(node, pk), "vector must be set");
  (node, pk)
}

fn delete_node(db: &SingleFileDB, node: NodeId) {
  db.begin(false).expect("begin delete");
  db.delete_node(node).expect("delete node");
  db.commit().expect("commit delete");
}

fn assert_no_vector(db: &SingleFileDB, node: NodeId, pk: PropKeyId, stage: &str) {
  let vector = db.node_vector(node, pk);
  assert!(
    vector.is_none(),
    "{stage}: node_vector({node}) of deleted node = {vector:?}"
  );
  assert!(
    !db.has_node_vector(node, pk),
    "{stage}: has_node_vector({node}) true for deleted node"
  );
}

#[test]
fn audit_d3_deleted_node_vector_hidden() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("d3_session.kitedb");

  let db = open(&path);
  let (node, pk) = create_node_with_vector(&db, "v");
  delete_node(&db, node);
  assert!(!db.node_exists(node));
  assert_no_vector(&db, node, pk, "after delete");
  close_single_file(db).expect("close");
}

#[test]
fn audit_d3_deleted_node_vector_gone_after_checkpoint() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("d3_checkpoint.kitedb");

  let db = open(&path);
  // Node deleted while still in the delta.
  let (delta_node, pk) = create_node_with_vector(&db, "delta");
  delete_node(&db, delta_node);

  // Node deleted after it reached the snapshot.
  let (snap_node, _) = create_node_with_vector(&db, "snap");
  db.checkpoint().expect("checkpoint 1");
  delete_node(&db, snap_node);
  assert_no_vector(&db, snap_node, pk, "snapshot node, before checkpoint");

  db.checkpoint().expect("checkpoint 2");
  assert_no_vector(&db, delta_node, pk, "delta node, after checkpoint");
  assert_no_vector(&db, snap_node, pk, "snapshot node, after checkpoint");

  close_single_file(db).expect("close");
  let db = open(&path);
  assert_no_vector(&db, delta_node, pk, "delta node, after reopen");
  assert_no_vector(&db, snap_node, pk, "snapshot node, after reopen");
  close_single_file(db).expect("close reopened");
}

#[test]
fn audit_d3_deleted_node_vector_gone_after_reopen() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("d3_reopen.kitedb");

  let db = open(&path);
  let (node, pk) = create_node_with_vector(&db, "v");
  delete_node(&db, node);
  close_single_file(db).expect("close");

  // No checkpoint: everything comes back through WAL replay.
  let db = open(&path);
  assert!(!db.node_exists(node));
  assert_no_vector(&db, node, pk, "after WAL replay");
  close_single_file(db).expect("close reopened");
}

// ============================================================================
// D4: node IDs reused after reopen
// ============================================================================

#[test]
fn audit_d4_deleted_max_id_not_reused_after_reopen() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("d4.kitedb");

  let db = open(&path);
  db.begin(false).expect("begin");
  let ids: Vec<NodeId> = (0..3)
    .map(|i| db.create_node(Some(&format!("n{i}"))).expect("create"))
    .collect();
  db.commit().expect("commit");
  let deleted = *ids.iter().max().unwrap();
  delete_node(&db, deleted);
  db.checkpoint().expect("checkpoint");
  close_single_file(db).expect("close");

  let db = open(&path);
  db.begin(false).expect("begin");
  let fresh = db.create_node(Some("fresh")).expect("create fresh");
  db.commit().expect("commit");
  assert!(
    fresh > deleted,
    "reused node id: new node got {fresh}, deleted max id was {deleted}"
  );
  close_single_file(db).expect("close reopened");
}

// ============================================================================
// D5: node ID allocator wraps / overflows
// ============================================================================

/// Either `create_node_with_id(explicit)` is rejected, or every later
/// allocation is an error or a fresh, strictly increasing id (no wrap, no dup).
fn assert_allocator_safe_after(explicit: NodeId) {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("d5.kitedb");
  let db = open(&path);

  let outcome = catch_unwind(AssertUnwindSafe(|| -> Result<(), String> {
    db.begin(false).map_err(|e| e.to_string())?;
    let first = db.create_node(None).map_err(|e| e.to_string())?;
    let mut seen: HashSet<NodeId> = HashSet::from([first]);
    let mut max_seen = first;

    match db.create_node_with_id(explicit, None) {
      Err(_) => {
        db.rollback().map_err(|e| e.to_string())?;
        return Ok(());
      }
      Ok(id) => {
        seen.insert(id);
        max_seen = max_seen.max(id);
      }
    }

    for _ in 0..3 {
      match db.create_node(None) {
        Err(_) => break,
        Ok(id) => {
          if seen.contains(&id) {
            return Err(format!(
              "create_node returned duplicate id {id} after create_node_with_id({explicit})"
            ));
          }
          if id <= max_seen {
            return Err(format!(
              "create_node returned wrapped id {id} (max so far {max_seen}) after create_node_with_id({explicit})"
            ));
          }
          seen.insert(id);
          max_seen = id;
        }
      }
    }
    db.rollback().map_err(|e| e.to_string())?;
    Ok(())
  }));

  match outcome {
    Ok(Ok(())) => {}
    Ok(Err(msg)) => panic!("{msg}"),
    Err(payload) => panic!(
      "panicked after create_node_with_id({explicit}): {}",
      panic_message(payload.as_ref())
    ),
  }
}

#[test]
fn audit_d5_allocator_no_wrap_after_u64_max() {
  assert_allocator_safe_after(u64::MAX);
}

#[test]
fn audit_d5_allocator_no_wrap_after_near_max() {
  assert_allocator_safe_after(u64::MAX - 1);
}

#[test]
fn audit_d5_ids_above_i64_max_never_issued() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("d5_i64.kitedb");
  let db = open(&path);
  let max_node_id = i64::MAX as u64;

  db.begin(false).expect("begin");
  let above = db.create_node_with_id(max_node_id + 1, None);
  assert!(
    above.is_err(),
    "create_node_with_id({}) above i64::MAX must fail, got {above:?}",
    max_node_id + 1
  );
  if db.create_node_with_id(max_node_id, None).is_ok() {
    let next = db.create_node(None);
    assert!(
      next.as_ref().map_or(true, |&id| id <= max_node_id),
      "create_node issued id above i64::MAX: {next:?}"
    );
  }
  db.rollback().expect("rollback");
  close_single_file(db).expect("close");
}

#[test]
fn audit_d5_reopen_after_max_id_does_not_panic() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("d5_reopen.kitedb");

  let db = open(&path);
  db.begin(false).expect("begin");
  let first = db.create_node(None).expect("create first");
  let _ = db.create_node_with_id(u64::MAX, None);
  db.commit().expect("commit");
  // Default close does not checkpoint (a checkpoint would size the
  // snapshot's dense id map by the max id).
  close_single_file(db).expect("close");

  let reopened = catch_unwind(AssertUnwindSafe(|| open_single_file(&path, options())));
  let db = match reopened {
    Ok(result) => result.expect("reopen after create_node_with_id(u64::MAX)"),
    Err(payload) => panic!(
      "reopen panicked during WAL replay: {}",
      panic_message(payload.as_ref())
    ),
  };
  assert!(db.node_exists(first));

  db.begin(false).expect("begin after reopen");
  if let Ok(id) = db.create_node(None) {
    assert!(
      id > first && id != u64::MAX,
      "create_node after reopen returned wrapped/duplicate id {id}"
    );
  }
  db.rollback().expect("rollback");
  close_single_file(db).expect("close reopened");
}
