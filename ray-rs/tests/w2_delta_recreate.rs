//! Wave-2 lane delta-recreate: reproductions for R1.
//!
//! R1: deleting a node that lives in the snapshot and then recreating the same
//! id (`create_node_with_id`, the bindings' `upsert_node_by_id`, Kite
//! `upsert_by_id`) leaves the new node hidden behind the delete tombstone, and
//! WAL replay drops it.
//!
//! The contract encoded here is a real recreate: the id is live again, and only
//! what the recreating transaction wrote is visible. The old node's props,
//! labels, key and incident edges (out and in, and the props of an old edge the
//! new node adds again) stay masked: in session, after a checkpoint, after WAL
//! replay, and after reopen.
//!
//! Run: `cargo test --no-default-features --test w2_delta_recreate`

use kitedb::api::kite::{EdgeDef, Kite, KiteOptions, NodeDef};
use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions,
};
use kitedb::types::{ETypeId, LabelId, NodeId, PropKeyId, PropValue};
use std::collections::HashMap;
use std::path::Path;

const OLD_KEY: &str = "node:old";
const NEW_KEY: &str = "node:new";

fn options() -> SingleFileOpenOptions {
  // Deterministic: checkpoints happen only where a test asks for one.
  SingleFileOpenOptions::new().auto_checkpoint(false)
}

fn open(path: &Path) -> SingleFileDB {
  open_single_file(path, options()).expect("open")
}

/// Ids of the fixture: the old node `n` and its neighbours `a` and `b`.
#[derive(Clone, Copy, Debug)]
struct Fx {
  n: NodeId,
  a: NodeId,
  b: NodeId,
  t: ETypeId,
  old_label: LabelId,
  new_label: LabelId,
  /// Set only on the old node.
  old_only: PropKeyId,
  /// Set on both nodes, with different values.
  shared: PropKeyId,
  /// Set only on the recreated node.
  new_only: PropKeyId,
  /// Edge prop of the old edge n -[t]-> a.
  weight: PropKeyId,
}

/// What the recreating transaction writes.
#[derive(Clone, Copy, Debug)]
struct Spec {
  key: Option<&'static str>,
  /// Also add label `new_label` and edges n->a (an old edge's triple, without
  /// props) and a->n. Otherwise only props `new_only` and `shared` are set.
  labels_and_edges: bool,
}

const FULL_NEW_KEY: Spec = Spec {
  key: Some(NEW_KEY),
  labels_and_edges: true,
};

/// Builds the old node `n` (key OLD_KEY, label `old_label`, props `old_only`
/// and `shared`, edges n->a {weight: 5}, n->b and b->n) next to nodes `a` and
/// `b`, then checkpoints so all of it lives in the snapshot.
fn snapshot_fixture(db: &SingleFileDB) -> Fx {
  let fx = build_fixture(db, true);
  db.checkpoint().expect("fixture checkpoint");
  assert!(db.node_exists(fx.n), "fixture: n must be in the snapshot");
  assert!(
    db.edge_exists(fx.n, fx.t, fx.a),
    "fixture: n->a in snapshot"
  );
  fx
}

/// `old_edge_weight`: give the old edge n->a its `weight` prop.
fn build_fixture(db: &SingleFileDB, old_edge_weight: bool) -> Fx {
  db.begin(false).expect("begin fixture");
  let t = db.define_etype("T").expect("etype");
  let old_label = db.define_label("Old").expect("label");
  let new_label = db.define_label("New").expect("label");
  let old_only = db.define_propkey("old_only").expect("propkey");
  let shared = db.define_propkey("shared").expect("propkey");
  let new_only = db.define_propkey("new_only").expect("propkey");
  let weight = db.define_propkey("weight").expect("propkey");
  let a = db.create_node(Some("node:a")).expect("create a");
  let b = db.create_node(Some("node:b")).expect("create b");
  let n = db.create_node(Some(OLD_KEY)).expect("create n");
  db.set_node_prop(n, old_only, PropValue::I64(1))
    .expect("prop");
  db.set_node_prop(n, shared, PropValue::String("old".into()))
    .expect("prop");
  db.add_node_label(n, old_label).expect("label");
  db.add_edge(n, t, a).expect("edge n->a");
  if old_edge_weight {
    db.set_edge_prop(n, t, a, weight, PropValue::I64(5))
      .expect("edge prop");
  }
  db.add_edge(n, t, b).expect("edge n->b");
  db.add_edge(b, t, n).expect("edge b->n");
  db.commit().expect("commit fixture");
  Fx {
    n,
    a,
    b,
    t,
    old_label,
    new_label,
    old_only,
    shared,
    new_only,
    weight,
  }
}

/// The recreating writes, inside the caller's write transaction.
fn write_recreated(db: &SingleFileDB, fx: &Fx, spec: Spec) {
  let id = db
    .create_node_with_id(fx.n, spec.key)
    .expect("create_node_with_id of a deleted id");
  assert_eq!(id, fx.n);
  write_new_state(db, fx, spec);
}

fn write_new_state(db: &SingleFileDB, fx: &Fx, spec: Spec) {
  db.set_node_prop(fx.n, fx.new_only, PropValue::I64(2))
    .expect("set new_only on the recreated node");
  db.set_node_prop(fx.n, fx.shared, PropValue::String("new".into()))
    .expect("set shared on the recreated node");
  if spec.labels_and_edges {
    db.add_node_label(fx.n, fx.new_label)
      .expect("add label to the recreated node");
    db.add_edge(fx.n, fx.t, fx.a)
      .expect("add_edge n->a from the recreated node");
    db.add_edge(fx.a, fx.t, fx.n)
      .expect("add_edge a->n to the recreated node");
  }
}

/// Deletes `n` in one transaction and recreates it in the next.
fn delete_then_recreate(db: &SingleFileDB, fx: &Fx, spec: Spec) {
  db.begin(false).expect("begin delete");
  db.delete_node(fx.n).expect("delete n");
  db.commit().expect("commit delete");
  assert!(!db.node_exists(fx.n), "delete must hide n");

  db.begin(false).expect("begin recreate");
  write_recreated(db, fx, spec);
  db.commit().expect("commit recreate");
}

fn sorted<T: Ord>(mut v: Vec<T>) -> Vec<T> {
  v.sort_unstable();
  v
}

/// A recreated `n` is live and holds only what `spec` wrote.
fn assert_recreated(db: &SingleFileDB, fx: &Fx, spec: Spec, ctx: &str) {
  let Fx {
    n, a, b, t, weight, ..
  } = *fx;

  // Existence.
  assert!(
    db.node_exists(n),
    "{ctx}: node_exists({n}) of the recreated node is false"
  );
  assert!(
    db.list_nodes().contains(&n),
    "{ctx}: list_nodes misses the recreated node {n}"
  );
  assert_eq!(db.count_nodes(), 3, "{ctx}: count_nodes");

  // Key: the new one (if any) resolves to n, the old one does not leak.
  assert_eq!(
    db.node_key(n).as_deref(),
    spec.key,
    "{ctx}: node_key of the recreated node"
  );
  if let Some(key) = spec.key {
    assert_eq!(db.node_by_key(key), Some(n), "{ctx}: node_by_key({key:?})");
  }
  if spec.key != Some(OLD_KEY) {
    assert_eq!(
      db.node_by_key(OLD_KEY),
      None,
      "{ctx}: the old node's key still resolves"
    );
  }

  // Props: only the new ones.
  let expected_props = HashMap::from([
    (fx.new_only, PropValue::I64(2)),
    (fx.shared, PropValue::String("new".into())),
  ]);
  assert_eq!(
    db.node_prop(n, fx.old_only),
    None,
    "{ctx}: the old node's prop leaked into the recreated node"
  );
  assert_eq!(
    db.node_props(n),
    Some(expected_props),
    "{ctx}: node_props of the recreated node"
  );

  // Labels: only the new ones.
  assert!(
    !db.node_has_label(n, fx.old_label),
    "{ctx}: the old node's label leaked into the recreated node"
  );
  let expected_labels = if spec.labels_and_edges {
    vec![fx.new_label]
  } else {
    vec![]
  };
  assert_eq!(
    sorted(db.node_labels(n)),
    expected_labels,
    "{ctx}: node_labels of the recreated node"
  );
  assert_eq!(
    db.node_has_label(n, fx.new_label),
    spec.labels_and_edges,
    "{ctx}: node_has_label(new)"
  );

  // Edges: the old n->b and b->n are gone; only the new ones are visible.
  assert!(
    !db.edge_exists(n, t, b),
    "{ctx}: the old out-edge n->b leaked"
  );
  assert!(
    !db.edge_exists(b, t, n),
    "{ctx}: the old in-edge b->n leaked"
  );
  assert!(
    db.out_edges(b).is_empty(),
    "{ctx}: out_edges(b): {:?}",
    db.out_edges(b)
  );
  assert!(
    db.in_edges(b).is_empty(),
    "{ctx}: in_edges(b): {:?}",
    db.in_edges(b)
  );
  let (expected_out, expected_edges) = if spec.labels_and_edges {
    (vec![(t, a)], sorted(vec![(n, t, a), (a, t, n)]))
  } else {
    (vec![], vec![])
  };
  assert_eq!(
    sorted(db.out_edges(n)),
    expected_out,
    "{ctx}: out_edges of the recreated node"
  );
  assert_eq!(
    sorted(db.in_edges(n)),
    expected_out,
    "{ctx}: in_edges of the recreated node"
  );
  assert_eq!(
    db.edge_exists(n, t, a),
    spec.labels_and_edges,
    "{ctx}: edge_exists(n->a)"
  );
  assert_eq!(
    db.edge_exists(a, t, n),
    spec.labels_and_edges,
    "{ctx}: edge_exists(a->n)"
  );
  assert_eq!(
    sorted(db.out_edges(a)),
    if spec.labels_and_edges {
      vec![(t, n)]
    } else {
      vec![]
    },
    "{ctx}: out_edges(a)"
  );
  assert_eq!(
    sorted(db.in_edges(a)),
    if spec.labels_and_edges {
      vec![(t, n)]
    } else {
      vec![]
    },
    "{ctx}: in_edges(a): the old n->a must not leak"
  );
  let all_edges = sorted(
    db.list_edges(None)
      .into_iter()
      .map(|e| (e.src, e.etype, e.dst))
      .collect(),
  );
  assert_eq!(all_edges, expected_edges, "{ctx}: list_edges");
  assert_eq!(db.count_edges(), expected_edges.len(), "{ctx}: count_edges");

  // The re-added n->a is a new edge: the old edge's props stay masked.
  assert_eq!(
    db.edge_prop(n, t, a, weight),
    None,
    "{ctx}: the re-added edge n->a inherited the old edge's weight"
  );
  assert!(
    db.edge_props(n, t, a).unwrap_or_default().is_empty(),
    "{ctx}: edge_props(n->a) = {:?}",
    db.edge_props(n, t, a)
  );
}

#[derive(Clone, Copy, Debug)]
enum Step {
  Check,
  Checkpoint,
  Reopen,
}
use Step::{Check, Checkpoint, Reopen};

/// Runs `steps` after the recreating commit, checking the recreated node at
/// each `Check`. The failure context names the steps so far.
fn run_steps(path: &Path, db: SingleFileDB, fx: &Fx, spec: Spec, steps: &[Step]) {
  let mut db = db;
  let mut ctx = String::from("commit");
  for step in steps {
    match step {
      Step::Check => assert_recreated(&db, fx, spec, &ctx),
      Step::Checkpoint => {
        db.checkpoint()
          .unwrap_or_else(|e| panic!("{ctx}: checkpoint failed: {e:?}"));
        ctx.push_str(" > checkpoint");
      }
      Step::Reopen => {
        close_single_file(db).expect("close");
        db = open_single_file(path, options())
          .unwrap_or_else(|e| panic!("{ctx}: reopen failed: {e:?}"));
        ctx.push_str(" > reopen");
      }
    }
  }
  close_single_file(db).expect("close");
}

// ============================================================================
// Recreate in a later transaction (create_node_with_id)
// ============================================================================

#[test]
fn r1_recreate_snapshot_node_visible_after_commit() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("visible.kitedb");
  let db = open(&path);
  let fx = snapshot_fixture(&db);
  delete_then_recreate(&db, &fx, FULL_NEW_KEY);
  run_steps(&path, db, &fx, FULL_NEW_KEY, &[Check]);
}

/// The checkpoint path on its own: no read before the checkpoint.
#[test]
fn r1_recreate_snapshot_node_survives_checkpoint_and_reopen() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("checkpoint.kitedb");
  let db = open(&path);
  let fx = snapshot_fixture(&db);
  delete_then_recreate(&db, &fx, FULL_NEW_KEY);
  run_steps(
    &path,
    db,
    &fx,
    FULL_NEW_KEY,
    &[Checkpoint, Check, Reopen, Check],
  );
}

/// WAL replay without a checkpoint: reopen right after the commit.
#[test]
fn r1_recreate_snapshot_node_survives_wal_replay() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("replay.kitedb");
  let db = open(&path);
  let fx = snapshot_fixture(&db);
  delete_then_recreate(&db, &fx, FULL_NEW_KEY);
  run_steps(
    &path,
    db,
    &fx,
    FULL_NEW_KEY,
    &[Reopen, Check, Checkpoint, Check, Reopen, Check],
  );
}

/// The recreated node takes the old node's key again.
#[test]
fn r1_recreated_node_reuses_old_key() {
  let spec = Spec {
    key: Some(OLD_KEY),
    labels_and_edges: true,
  };
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("key_reuse.kitedb");
  let db = open(&path);
  let fx = snapshot_fixture(&db);
  delete_then_recreate(&db, &fx, spec);
  run_steps(
    &path,
    db,
    &fx,
    spec,
    &[Check, Reopen, Check, Checkpoint, Check, Reopen, Check],
  );
}

/// Guard (passes before the fix): the same scenario when the old node never
/// reached the snapshot. The old edge n->a carries no prop here: a delta edge's
/// props outlive an unlink and reattach on relink with or without a node
/// delete, which is a separate, general issue and not R1.
#[test]
fn r1_guard_recreate_delta_node() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("delta_only.kitedb");
  let db = open(&path);
  let fx = build_fixture(&db, false);
  delete_then_recreate(&db, &fx, FULL_NEW_KEY);
  run_steps(
    &path,
    db,
    &fx,
    FULL_NEW_KEY,
    &[Check, Reopen, Check, Checkpoint, Check, Reopen, Check],
  );
}

// ============================================================================
// Delete and recreate within one transaction
// ============================================================================

#[test]
fn r1_delete_and_recreate_in_one_tx() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("same_tx.kitedb");
  let db = open(&path);
  let fx = snapshot_fixture(&db);

  db.begin(false).expect("begin");
  db.delete_node(fx.n).expect("delete n");
  db.create_node_with_id(fx.n, Some(NEW_KEY))
    .expect("create_node_with_id(n) after deleting n in the same tx");
  assert!(
    db.node_exists(fx.n),
    "inside the tx: n is invisible to the transaction that recreated it"
  );
  write_new_state(&db, &fx, FULL_NEW_KEY);
  assert_recreated(&db, &fx, FULL_NEW_KEY, "inside the recreating tx");
  db.commit().expect("commit");

  run_steps(
    &path,
    db,
    &fx,
    FULL_NEW_KEY,
    &[Check, Reopen, Check, Checkpoint, Check, Reopen, Check],
  );
}

/// Without reads inside the tx: the commit must not bring the old node back.
#[test]
fn r1_delete_and_recreate_in_one_tx_does_not_resurrect_old_node() {
  let spec = Spec {
    key: Some(NEW_KEY),
    labels_and_edges: false,
  };
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("same_tx_merge.kitedb");
  let db = open(&path);
  let fx = snapshot_fixture(&db);

  db.begin(false).expect("begin");
  db.delete_node(fx.n).expect("delete n");
  write_recreated(&db, &fx, spec);
  db.commit().expect("commit");

  run_steps(
    &path,
    db,
    &fx,
    spec,
    &[Check, Reopen, Check, Checkpoint, Check, Reopen, Check],
  );
}

/// The old node's state can also live in the delta: edges added after the
/// checkpoint (one a self-loop), a label, a prop, and an edge prop on a
/// snapshot edge. The recreate drops all of it, whether the delete commits on
/// its own, or that state, the delete and the recreate share one transaction.
#[test]
fn r1_recreate_masks_old_state_held_in_the_delta() {
  let spec = Spec {
    key: Some(NEW_KEY),
    labels_and_edges: false,
  };
  for same_tx in [false, true] {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("delta_held.kitedb");
    let db = open(&path);
    let fx = snapshot_fixture(&db);
    let next_tx = |what: &str| {
      if !same_tx {
        db.commit()
          .unwrap_or_else(|e| panic!("commit {what}: {e:?}"));
        db.begin(false).expect("begin");
      }
    };

    db.begin(false).expect("begin");
    db.add_edge(fx.a, fx.t, fx.n).expect("edge a->n");
    db.add_edge(fx.n, fx.t, fx.n).expect("self-loop n->n");
    db.add_node_label(fx.n, fx.new_label).expect("label");
    db.set_node_prop(fx.n, fx.old_only, PropValue::I64(9))
      .expect("prop");
    db.set_edge_prop(fx.n, fx.t, fx.b, fx.weight, PropValue::I64(3))
      .expect("edge prop");
    next_tx("old delta state");
    db.delete_node(fx.n).expect("delete n");
    next_tx("delete");
    write_recreated(&db, &fx, spec);
    db.commit().expect("commit recreate");
    assert!(db.check().valid, "same_tx={same_tx}: {:?}", db.check());

    run_steps(
      &path,
      db,
      &fx,
      spec,
      &[Check, Reopen, Check, Checkpoint, Check, Reopen, Check],
    );
  }
}

// ============================================================================
// Upsert by id
// ============================================================================

/// The core of the NAPI `upsertNodeById` and Python `upsert_node_by_id`
/// bindings (not compiled with `--no-default-features`): create the id if it
/// does not exist, then set props.
fn upsert_node_by_id(db: &SingleFileDB, node_id: NodeId, props: &[(PropKeyId, PropValue)]) {
  if !db.node_exists(node_id) {
    db.create_node_with_id(node_id, None)
      .expect("upsert_node_by_id: create");
  }
  for (key_id, value) in props {
    db.set_node_prop(node_id, *key_id, value.clone())
      .expect("upsert_node_by_id: set prop");
  }
}

#[test]
fn r1_upsert_node_by_id_recreates_deleted_snapshot_node() {
  let spec = Spec {
    key: None,
    labels_and_edges: false,
  };
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("upsert.kitedb");
  let db = open(&path);
  let fx = snapshot_fixture(&db);

  db.begin(false).expect("begin delete");
  db.delete_node(fx.n).expect("delete n");
  db.commit().expect("commit delete");

  let props = [
    (fx.new_only, PropValue::I64(2)),
    (fx.shared, PropValue::String("new".into())),
  ];
  db.begin(false).expect("begin upsert");
  upsert_node_by_id(&db, fx.n, &props);
  db.commit().expect("commit upsert");

  // A second upsert must update the recreated node, not create it again.
  db.begin(false).expect("begin second upsert");
  upsert_node_by_id(&db, fx.n, &props[..1]);
  db.commit().expect("commit second upsert");

  run_steps(
    &path,
    db,
    &fx,
    spec,
    &[Check, Reopen, Check, Checkpoint, Check, Reopen, Check],
  );
}

#[test]
fn r1_kite_upsert_by_id_recreates_deleted_snapshot_node() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("kite");
  let schema = || {
    KiteOptions::new()
      .disable_close_checkpoint()
      .node(NodeDef::new("User", "user:"))
      .edge(EdgeDef::new("FOLLOWS"))
  };

  let mut kite = Kite::open(&path, schema()).expect("open");
  let alice = kite
    .create_node(
      "User",
      "alice",
      HashMap::from([
        ("name".to_string(), PropValue::String("Alice".into())),
        ("age".to_string(), PropValue::I64(30)),
      ]),
    )
    .expect("create alice")
    .id();
  let bob = kite
    .create_node("User", "bob", HashMap::new())
    .expect("create bob")
    .id();
  kite.link(alice, "FOLLOWS", bob).expect("link alice->bob");
  kite.link(bob, "FOLLOWS", alice).expect("link bob->alice");
  kite.raw().checkpoint().expect("checkpoint");

  assert!(kite.delete_node(alice).expect("delete alice"));
  assert!(!kite.exists(alice));
  kite
    .upsert_by_id("User", alice)
    .expect("upsert_by_id builder")
    .set("name", PropValue::String("Recreated".into()))
    .execute()
    .expect("upsert_by_id of a deleted snapshot node");

  let check = |kite: &Kite, ctx: &str| {
    assert!(
      kite.exists(alice),
      "{ctx}: Kite exists() of the recreated node is false"
    );
    assert_eq!(
      kite.prop(alice, "name"),
      Some(PropValue::String("Recreated".into())),
      "{ctx}: name of the recreated node"
    );
    assert_eq!(
      kite.prop(alice, "age"),
      None,
      "{ctx}: the old node's age leaked"
    );
    let node = kite.node_by_id(alice).expect("node_by_id");
    assert_eq!(
      node.as_ref().map(|node| node.key().map(str::to_string)),
      Some(None),
      "{ctx}: node_by_id: the recreated node must exist without the old key"
    );
    assert_eq!(
      kite
        .get("User", "alice")
        .expect("get")
        .map(|node| node.id()),
      None,
      "{ctx}: the old key still resolves"
    );
    assert!(
      kite.neighbors_out(alice, None).expect("out").is_empty(),
      "{ctx}: old out-edge leaked"
    );
    assert!(
      kite.neighbors_in(alice, None).expect("in").is_empty(),
      "{ctx}: old in-edge leaked"
    );
    assert!(!kite.has_edge(alice, "FOLLOWS", bob).expect("has_edge"));
    assert!(!kite.has_edge(bob, "FOLLOWS", alice).expect("has_edge"));
    assert_eq!(kite.count_nodes(), 2, "{ctx}: count_nodes");
    assert_eq!(kite.count_edges(), 0, "{ctx}: count_edges");
  };

  check(&kite, "after upsert_by_id");
  kite.close().expect("close");
  let kite = Kite::open(&path, schema()).expect("reopen");
  check(&kite, "after WAL replay");
  kite.raw().checkpoint().expect("checkpoint");
  check(&kite, "after replay > checkpoint");
  kite.close().expect("close");
  let kite = Kite::open(&path, schema()).expect("reopen");
  check(&kite, "after replay > checkpoint > reopen");
  kite.close().expect("close");
}
