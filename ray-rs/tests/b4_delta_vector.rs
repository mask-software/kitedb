//! raydb-b4 `delta-vector` lane: delta-overlay and node-vector findings,
//! written against the public `SingleFileDB` API so they survive the wave-2
//! rewrite of delta.rs, read.rs and vector.rs.
//!
//! - F1: edge props survive an unlink and come back on relink.
//! - F2: a deleted delta-created node (or a never-created id) shows orphan
//!   props, labels, edges or vectors.
//! - F3: key lookups after deletes (guards for the removal of the
//!   never-populated `DeltaState::key_index_deleted`).
//! - F5: `set_node_vector` accepts a node that does not exist.
//! - F6: `node_vector` is raw inside the writing transaction, normalized after
//!   commit.
//! - F7: vector reads racing checkpoint installs (guard).
//! - F4, F8: perf baselines, `#[ignore]`d. Run them with
//!   `cargo test --no-default-features --test b4_delta_vector -- --ignored --nocapture bench_`.
//!
//! Each scenario collects every violation, at every stage (live, WAL replay,
//! checkpoint, checkpoint + reopen), before asserting, so one failure shows
//! the whole picture.

use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions,
};
use kitedb::error::{KiteError, Result};
use kitedb::types::{NodeId, PropKeyId, PropValue};
use kitedb::vector::ivf::serialize::{deserialize_manifest, serialize_manifest};
use kitedb::vector::store::{create_vector_store, vector_store_insert};
use kitedb::vector::types::VectorStoreConfig;
use std::collections::HashMap;
use std::fmt::{Debug, Display};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use tempfile::TempDir;

// ============================================================================
// Harness
// ============================================================================

#[derive(Default)]
struct Violations(Vec<String>);

impl Violations {
  fn expect_eq<T: PartialEq + Debug>(&mut self, stage: &str, what: &str, got: T, want: T) {
    if got != want {
      self
        .0
        .push(format!("[{stage}] {what}: got {got:?}, want {want:?}"));
    }
  }

  fn expect(&mut self, stage: &str, ok: bool, what: impl Display) {
    if !ok {
      self.0.push(format!("[{stage}] {what}"));
    }
  }

  #[track_caller]
  fn assert_none(&self, scenario: &str) {
    assert!(
      self.0.is_empty(),
      "{scenario}: {} violation(s)\n  {}",
      self.0.len(),
      self.0.join("\n  ")
    );
  }
}

/// A database that can be closed and reopened in place.
struct Fixture {
  _dir: TempDir,
  path: PathBuf,
  options: SingleFileOpenOptions,
  db: Option<Arc<SingleFileDB>>,
}

impl Fixture {
  fn new(name: &str, mvcc: bool) -> Self {
    Self::with_options(
      name,
      SingleFileOpenOptions::new()
        .auto_checkpoint(false)
        .mvcc(mvcc)
        // Close waits for the GC thread's sleep; keep reopens fast.
        .mvcc_gc_interval_ms(20),
    )
  }

  fn with_options(name: &str, options: SingleFileOpenOptions) -> Self {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(format!("{name}.kitedb"));
    let db = open_single_file(&path, options.clone()).expect("open");
    Self {
      _dir: dir,
      path,
      options,
      db: Some(Arc::new(db)),
    }
  }

  fn db(&self) -> &SingleFileDB {
    self.db.as_ref().expect("open database")
  }

  fn shared(&self) -> Arc<SingleFileDB> {
    Arc::clone(self.db.as_ref().expect("open database"))
  }

  fn reopen(&mut self) {
    let db = self.db.take().expect("open database");
    let db = Arc::try_unwrap(db).unwrap_or_else(|_| panic!("database still shared at reopen"));
    close_single_file(db).expect("close");
    let db = open_single_file(&self.path, self.options.clone()).expect("reopen");
    self.db = Some(Arc::new(db));
  }

  /// Run `check` live, after a reopen (WAL replay), after a checkpoint, and
  /// after a reopen from that checkpoint.
  fn check_stages(
    &mut self,
    violations: &mut Violations,
    check: impl Fn(&SingleFileDB, &str, &mut Violations),
  ) {
    check(self.db(), "live", violations);
    self.reopen();
    check(self.db(), "reopen (WAL replay)", violations);
    if let Err(err) = self.db().checkpoint() {
      violations.expect("checkpoint", false, format!("checkpoint failed: {err}"));
      return;
    }
    check(self.db(), "checkpoint", violations);
    self.reopen();
    check(self.db(), "checkpoint + reopen", violations);
  }
}

/// Run `f` in a committed write transaction.
fn tx<R>(db: &SingleFileDB, f: impl FnOnce(&SingleFileDB) -> R) -> R {
  db.begin(false).expect("begin");
  let result = f(db);
  db.commit().expect("commit");
  result
}

/// A write to a missing node may be refused with `NodeNotFound`, or be
/// accepted and stay invisible; any other result is a violation.
fn expect_ok_or_node_not_found(
  violations: &mut Violations,
  what: &str,
  node: NodeId,
  result: Result<()>,
) {
  match result {
    Ok(()) | Err(KiteError::NodeNotFound(_)) => {}
    Err(err) => violations.expect(
      "write",
      false,
      format!("{what} on missing node {node}: unexpected error {err}"),
    ),
  }
}

fn unit(v: &[f32]) -> Vec<f32> {
  let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
  v.iter().map(|x| x / norm).collect()
}

fn approx_eq(a: &[f32], b: &[f32]) -> bool {
  a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-6)
}

fn env_usize(name: &str, default: usize) -> usize {
  std::env::var(name)
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(default)
}

// ============================================================================
// F1: edge props survive an unlink and come back on relink
// ============================================================================

#[derive(Clone, Copy)]
enum Relink {
  /// Unlink in one transaction, relink in the next.
  LaterTx,
  /// Unlink and relink in one transaction.
  SameTx,
  /// Unlink, then relink with `add_edge_with_props(x = 1)`.
  LaterTxWithProps,
}

/// `a -[T]-> b` with `w = 5`, optionally checkpointed into the snapshot, is
/// unlinked and relinked. The relinked edge is a new edge: `w` is gone.
fn f1_relink(name: &str, mvcc: bool, in_snapshot: bool, relink: Relink) {
  let mut fx = Fixture::new(name, mvcc);
  let mut v = Violations::default();
  let (a, t, b, w, x) = tx(fx.db(), |db| {
    let a = db.create_node(Some("a")).expect("a");
    let b = db.create_node(Some("b")).expect("b");
    let t = db.define_etype("T").expect("etype");
    let w = db.define_propkey("w").expect("propkey w");
    let x = db.define_propkey("x").expect("propkey x");
    db.add_edge(a, t, b).expect("link");
    db.set_edge_prop(a, t, b, w, PropValue::I64(5))
      .expect("set w");
    (a, t, b, w, x)
  });
  if in_snapshot {
    fx.db().checkpoint().expect("checkpoint");
  }

  let mut want: HashMap<PropKeyId, PropValue> = HashMap::new();
  match relink {
    Relink::LaterTx => {
      tx(fx.db(), |db| db.delete_edge(a, t, b).expect("unlink"));
      tx(fx.db(), |db| db.add_edge(a, t, b).expect("relink"));
    }
    Relink::LaterTxWithProps => {
      tx(fx.db(), |db| db.delete_edge(a, t, b).expect("unlink"));
      tx(fx.db(), |db| {
        db.add_edge_with_props(a, t, b, vec![(x, PropValue::I64(1))])
          .expect("relink with props")
      });
      want.insert(x, PropValue::I64(1));
    }
    Relink::SameTx => {
      let db = fx.db();
      db.begin(false).expect("begin");
      db.delete_edge(a, t, b).expect("unlink");
      db.add_edge(a, t, b).expect("relink");
      v.expect_eq(
        "inside the relinking tx",
        "edge_prop(w)",
        db.edge_prop(a, t, b, w),
        None,
      );
      v.expect_eq(
        "inside the relinking tx",
        "edge_props",
        db.edge_props(a, t, b),
        Some(HashMap::new()),
      );
      db.commit().expect("commit");
    }
  }

  fx.check_stages(&mut v, |db, stage, v| {
    v.expect_eq(stage, "edge_exists", db.edge_exists(a, t, b), true);
    v.expect_eq(stage, "edge_prop(w)", db.edge_prop(a, t, b, w), None);
    v.expect_eq(
      stage,
      "edge_props",
      db.edge_props(a, t, b),
      Some(want.clone()),
    );
  });
  v.assert_none("the relinked edge must not carry the unlinked edge's props");
}

#[test]
fn f1_unlink_then_relink_of_delta_edge_drops_its_props() {
  f1_relink("f1-delta", false, false, Relink::LaterTx);
}

#[test]
fn f1_unlink_then_relink_of_snapshot_edge_drops_its_props() {
  f1_relink("f1-snapshot", false, true, Relink::LaterTx);
}

#[test]
fn f1_unlink_and_relink_in_one_tx_drops_props_of_delta_edge() {
  f1_relink("f1-delta-same-tx", false, false, Relink::SameTx);
}

#[test]
fn f1_unlink_and_relink_in_one_tx_drops_props_of_snapshot_edge() {
  f1_relink("f1-snapshot-same-tx", false, true, Relink::SameTx);
}

#[test]
fn f1_relink_with_new_props_keeps_only_the_new_props() {
  f1_relink("f1-delta-new-props", false, false, Relink::LaterTxWithProps);
  f1_relink(
    "f1-snapshot-new-props",
    false,
    true,
    Relink::LaterTxWithProps,
  );
}

#[test]
fn f1_mvcc_unlink_then_relink_of_delta_edge_drops_its_props() {
  f1_relink("f1-mvcc-delta", true, false, Relink::LaterTx);
}

#[test]
fn f1_mvcc_unlink_then_relink_of_snapshot_edge_drops_its_props() {
  f1_relink("f1-mvcc-snapshot", true, true, Relink::LaterTx);
}

/// A delta-created node `b` with edges carrying props is deleted and its id
/// recreated with `create_node_with_id`. Relinking the same triples must not
/// bring back the old edges' props, and the node starts fresh.
#[test]
fn f1_recreated_delta_node_does_not_inherit_old_edge_props() {
  let mut fx = Fixture::new("f1-recreated-delta-node", false);
  let mut v = Violations::default();
  let (a, b, t, w, name) = tx(fx.db(), |db| {
    let a = db.create_node(Some("a")).expect("a");
    let b = db.create_node(Some("b")).expect("b");
    let t = db.define_etype("T").expect("etype");
    let w = db.define_propkey("w").expect("propkey w");
    let name = db.define_propkey("name").expect("propkey name");
    db.set_node_prop(b, name, PropValue::String("old".into()))
      .expect("node prop");
    db.add_edge(a, t, b).expect("a->b");
    db.add_edge(b, t, a).expect("b->a");
    db.set_edge_prop(a, t, b, w, PropValue::I64(5))
      .expect("w on a->b");
    db.set_edge_prop(b, t, a, w, PropValue::I64(6))
      .expect("w on b->a");
    (a, b, t, w, name)
  });
  tx(fx.db(), |db| db.delete_node(b).expect("delete b"));
  tx(fx.db(), |db| {
    db.create_node_with_id(b, Some("b-new"))
      .expect("recreate b")
  });
  tx(fx.db(), |db| {
    db.add_edge(a, t, b).expect("relink a->b");
    db.add_edge(b, t, a).expect("relink b->a");
  });

  fx.check_stages(&mut v, |db, stage, v| {
    v.expect_eq(stage, "node_exists(b)", db.node_exists(b), true);
    v.expect_eq(
      stage,
      "node_key(b)",
      db.node_key(b),
      Some("b-new".to_string()),
    );
    v.expect_eq(stage, "node_prop(b, name)", db.node_prop(b, name), None);
    v.expect_eq(stage, "edge_prop(a->b, w)", db.edge_prop(a, t, b, w), None);
    v.expect_eq(stage, "edge_prop(b->a, w)", db.edge_prop(b, t, a, w), None);
    v.expect_eq(
      stage,
      "edge_props(a->b)",
      db.edge_props(a, t, b),
      Some(HashMap::new()),
    );
  });
  v.assert_none("a recreated node must not inherit the deleted node's edge props");
}

/// Snapshot edge `n -[T]-> a` whose `w` was set after the checkpoint (so the
/// delta holds it). `n` is deleted and its id recreated, then the triple is
/// relinked. The delete masks the snapshot copy's props (raydb-79's R1), but
/// the delta-held `w = 7` must go as well.
///
/// On a0d26a2 this also fails for R1 itself (the recreated node is
/// invisible, so the relink is refused); after the wave-2 R1 fix only the
/// delta-held prop leak remains.
#[test]
fn f1_recreated_snapshot_node_does_not_inherit_delta_held_edge_props() {
  let mut fx = Fixture::new("f1-recreated-snapshot-node", false);
  let mut v = Violations::default();
  let (n, a, t, w) = tx(fx.db(), |db| {
    let n = db.create_node(Some("n")).expect("n");
    let a = db.create_node(Some("a")).expect("a");
    let t = db.define_etype("T").expect("etype");
    let w = db.define_propkey("w").expect("propkey");
    db.add_edge(n, t, a).expect("n->a");
    (n, a, t, w)
  });
  fx.db().checkpoint().expect("checkpoint");
  tx(fx.db(), |db| {
    db.set_edge_prop(n, t, a, w, PropValue::I64(7))
      .expect("w held by the delta")
  });
  tx(fx.db(), |db| db.delete_node(n).expect("delete n"));
  {
    let db = fx.db();
    db.begin(false).expect("begin");
    if let Err(err) = db.create_node_with_id(n, Some("n-new")) {
      v.expect("write", false, format!("recreate n: {err}"));
    }
    db.commit().expect("commit");
    db.begin(false).expect("begin");
    if let Err(err) = db.add_edge(n, t, a) {
      v.expect("write", false, format!("relink n->a: {err}"));
    }
    db.commit().expect("commit");
  }

  fx.check_stages(&mut v, |db, stage, v| {
    v.expect_eq(stage, "edge_exists(n->a)", db.edge_exists(n, t, a), true);
    v.expect_eq(stage, "edge_prop(n->a, w)", db.edge_prop(n, t, a, w), None);
  });
  v.assert_none("a recreated node must not inherit delta-held props of its old edges");
}

// ============================================================================
// F2: orphan state on a deleted delta-created node
// ============================================================================

/// Every read of a node that does not exist reports nothing.
fn check_node_absent(
  db: &SingleFileDB,
  stage: &str,
  v: &mut Violations,
  node: NodeId,
  name: PropKeyId,
  label: kitedb::types::LabelId,
) {
  v.expect_eq(stage, "node_exists", db.node_exists(node), false);
  v.expect_eq(stage, "node_prop(name)", db.node_prop(node, name), None);
  v.expect_eq(stage, "node_props", db.node_props(node), None);
  v.expect_eq(stage, "node_labels", db.node_labels(node), Vec::new());
  v.expect_eq(
    stage,
    "node_has_label",
    db.node_has_label(node, label),
    false,
  );
  v.expect_eq(stage, "node_key", db.node_key(node), None);
  v.expect(
    stage,
    !db.list_nodes().contains(&node),
    format!("list_nodes contains missing node {node}"),
  );
}

/// Write a prop and a label to `target`, which does not exist, through the
/// low-level API (it does not check node existence), and check no read
/// reports them.
fn f2_writes_to_missing_node(name_: &str, mvcc: bool, deleted_delta_node: bool) {
  let mut fx = Fixture::new(name_, mvcc);
  let mut v = Violations::default();
  let (target, name, label) = tx(fx.db(), |db| {
    db.create_node(Some("alice")).expect("alice");
    let name = db.define_propkey("name").expect("propkey");
    let label = db.define_label("Person").expect("label");
    let target = if deleted_delta_node {
      let carol = db.create_node(Some("carol")).expect("carol");
      db.set_node_prop(carol, name, PropValue::String("C0".into()))
        .expect("prop");
      db.add_node_label(carol, label).expect("label");
      carol
    } else {
      999_999
    };
    (target, name, label)
  });
  if deleted_delta_node {
    tx(fx.db(), |db| db.delete_node(target).expect("delete carol"));
  }
  let (prop_write, label_write) = tx(fx.db(), |db| {
    (
      db.set_node_prop(target, name, PropValue::String("Carol".into())),
      db.add_node_label(target, label),
    )
  });
  expect_ok_or_node_not_found(&mut v, "set_node_prop", target, prop_write);
  expect_ok_or_node_not_found(&mut v, "add_node_label", target, label_write);

  fx.check_stages(&mut v, |db, stage, v| {
    check_node_absent(db, stage, v, target, name, label);
    v.expect_eq(
      stage,
      "count_nodes == list_nodes().len()",
      db.count_nodes(),
      db.list_nodes().len(),
    );
  });
  v.assert_none("a node that does not exist must have no props or labels");
}

#[test]
fn f2_writes_to_deleted_delta_node_leave_no_orphan_state() {
  f2_writes_to_missing_node("f2-deleted-delta-node", false, true);
}

#[test]
fn f2_mvcc_writes_to_deleted_delta_node_leave_no_orphan_state() {
  f2_writes_to_missing_node("f2-mvcc-deleted-delta-node", true, true);
}

#[test]
fn f2_writes_to_never_created_node_leave_no_orphan_state() {
  f2_writes_to_missing_node("f2-never-created", false, false);
}

/// The same for an edge: a prop written (through the low-level API, which
/// does not check edge existence) to an edge that does not exist must not
/// surface when the edge is linked later.
#[test]
fn f2_write_to_missing_edge_does_not_surface_when_it_is_linked() {
  let mut fx = Fixture::new("f2-missing-edge", false);
  let mut v = Violations::default();
  let (a, b, t, w) = tx(fx.db(), |db| {
    let a = db.create_node(Some("a")).expect("a");
    let b = db.create_node(Some("b")).expect("b");
    let t = db.define_etype("T").expect("etype");
    let w = db.define_propkey("w").expect("propkey");
    (a, b, t, w)
  });
  let write = tx(fx.db(), |db| {
    db.set_edge_prop(a, t, b, w, PropValue::I64(9))
  });
  match write {
    Ok(()) | Err(KiteError::EdgeNotFound { .. }) => {}
    Err(err) => v.expect(
      "write",
      false,
      format!("set_edge_prop on a missing edge: unexpected error {err}"),
    ),
  }
  v.expect_eq(
    "before the link",
    "edge_props",
    fx.db().edge_props(a, t, b),
    None,
  );
  tx(fx.db(), |db| db.add_edge(a, t, b).expect("link"));

  fx.check_stages(&mut v, |db, stage, v| {
    v.expect_eq(stage, "edge_prop(w)", db.edge_prop(a, t, b, w), None);
    v.expect_eq(
      stage,
      "edge_props",
      db.edge_props(a, t, b),
      Some(HashMap::new()),
    );
  });
  v.assert_none("a prop written to a missing edge must not surface on a later link");
}

/// One transaction links `a -> x` (with a prop), then deletes `x`. Inside the
/// transaction the edge is gone. The commit merges the delete and then the
/// link, so the committed delta holds an add patch to a deleted node. When
/// `x` was created by an earlier commit (still in the delta, so its delete
/// leaves no tombstone) the edge outlives its endpoint. When `x` is in the
/// snapshot, the tombstone hides the patch from every read except
/// `edge_exists` on a0d26a2 (wave 2's `edge_exists_over` checks endpoints).
#[test]
fn f2_edge_linked_then_endpoint_deleted_in_one_tx_does_not_dangle() {
  let mut v = Violations::default();
  for (case, in_snapshot) in [("delta x", false), ("snapshot x", true)] {
    let mut fx = Fixture::new(&format!("f2-link-delete-{in_snapshot}"), false);
    let (a, x, t, w) = tx(fx.db(), |db| {
      let a = db.create_node(Some("a")).expect("a");
      let x = db.create_node(Some("x")).expect("x");
      let t = db.define_etype("T").expect("etype");
      let w = db.define_propkey("w").expect("propkey");
      (a, x, t, w)
    });
    if in_snapshot {
      fx.db().checkpoint().expect("checkpoint");
    }
    {
      let db = fx.db();
      db.begin(false).expect("begin");
      db.add_edge(a, t, x).expect("link");
      db.set_edge_prop(a, t, x, w, PropValue::I64(3))
        .expect("prop");
      db.delete_node(x).expect("delete x");
      v.expect_eq(
        &format!("{case}: inside the tx"),
        "edge_exists(a->x)",
        db.edge_exists(a, t, x),
        false,
      );
      db.commit().expect("commit");
    }
    fx.check_stages(&mut v, |db, stage, v| {
      let stage = &format!("{case}: {stage}");
      v.expect_eq(stage, "node_exists(x)", db.node_exists(x), false);
      v.expect_eq(stage, "edge_exists(a->x)", db.edge_exists(a, t, x), false);
      v.expect_eq(stage, "edge_prop(a->x, w)", db.edge_prop(a, t, x, w), None);
      v.expect_eq(stage, "out_edges(a)", db.out_edges(a), Vec::new());
      v.expect_eq(stage, "count_edges", db.count_edges(), 0);
    });
  }
  v.assert_none("an edge must not outlive its endpoint deleted in the same tx");
}

/// Non-MVCC race: a transaction writes a prop, a label, an in-edge and a
/// vector to `carol` (all pass their checks), another transaction deletes
/// carol and commits first, then the first one commits. Nothing re-checks
/// carol at commit, so the committed state holds writes to a missing node.
/// Either refusing the late commit or dropping those writes is fine; showing
/// them is not. If writers are serialized (the delete's begin waits), the
/// writes commit first and the delete must remove all of them.
#[test]
fn f2_racing_writes_to_node_deleted_meanwhile_leave_no_orphan_state() {
  let mut fx = Fixture::new("f2-race", false);
  let mut v = Violations::default();
  let (alice, carol, t, name, label, emb) = tx(fx.db(), |db| {
    let alice = db.create_node(Some("alice")).expect("alice");
    let carol = db.create_node(Some("carol")).expect("carol");
    let t = db.define_etype("KNOWS").expect("etype");
    let name = db.define_propkey("name").expect("propkey");
    let label = db.define_label("Person").expect("label");
    let emb = db.define_propkey("embedding").expect("propkey");
    (alice, carol, t, name, label, emb)
  });

  let (staged_tx, staged_rx) = mpsc::channel::<std::result::Result<(), String>>();
  let (go_tx, go_rx) = mpsc::channel::<()>();
  let writer_db = fx.shared();
  let writer = std::thread::spawn(move || -> Result<()> {
    if let Err(err) = writer_db.begin(false) {
      staged_tx
        .send(Err(format!("{err}")))
        .expect("signal refused");
      return Err(err);
    }
    let stage_writes = || -> Result<()> {
      writer_db.set_node_prop(carol, name, PropValue::String("Carol".into()))?;
      writer_db.add_node_label(carol, label)?;
      writer_db.add_edge(alice, t, carol)?;
      writer_db.set_node_vector(carol, emb, &[1.0, 0.0, 0.0])?;
      Ok(())
    };
    let staged = stage_writes();
    staged_tx
      .send(staged.as_ref().map(|_| ()).map_err(|e| format!("{e}")))
      .expect("signal staged");
    staged?;
    go_rx.recv().expect("wait for the delete");
    writer_db.commit()
  });
  match staged_rx.recv().expect("staged") {
    Ok(()) => {}
    Err(err) => {
      // A second concurrent writer may be refused outright (single-writer
      // mode); then this race cannot happen.
      eprintln!("concurrent writer refused, race impossible: {err}");
      let _ = writer.join();
      return;
    }
  }
  // The delete runs on its own thread. With writers serialized at begin, its
  // begin waits for the writer's transaction: the writes commit first and
  // the delete then removes them, which must leave the same state.
  let (deleted_tx, deleted_rx) = mpsc::channel::<()>();
  let deleter_db = fx.shared();
  let deleter = std::thread::spawn(move || -> Result<()> {
    let result = deleter_db.begin(false).and_then(|_| {
      deleter_db.delete_node(carol)?;
      deleter_db.commit()
    });
    let _ = deleted_tx.send(());
    result
  });
  let delete_first = deleted_rx.recv_timeout(Duration::from_secs(2)).is_ok();
  go_tx.send(()).expect("release writer");
  let late_commit = writer.join().expect("writer thread");
  let delete = deleter.join().expect("deleter thread");
  eprintln!(
    "delete committed first: {delete_first}; late commit: {late_commit:?}; delete: {delete:?}"
  );
  if delete.is_err() {
    // A refused concurrent begin: delete after the writer instead.
    tx(fx.db(), |db| db.delete_node(carol).expect("delete carol"));
  }

  fx.check_stages(&mut v, |db, stage, v| {
    check_node_absent(db, stage, v, carol, name, label);
    v.expect_eq(
      stage,
      "edge_exists(alice->carol)",
      db.edge_exists(alice, t, carol),
      false,
    );
    v.expect_eq(stage, "out_edges(alice)", db.out_edges(alice), Vec::new());
    v.expect_eq(stage, "in_edges(carol)", db.in_edges(carol), Vec::new());
    v.expect_eq(stage, "count_edges", db.count_edges(), 0);
    v.expect_eq(
      stage,
      "node_vector(carol)",
      db.node_vector(carol, emb).map(|v| v.to_vec()),
      None,
    );
    v.expect_eq(
      stage,
      "has_node_vector(carol)",
      db.has_node_vector(carol, emb),
      false,
    );
  });
  v.assert_none("writes racing a node delete must not survive as orphan state");
}

// ============================================================================
// F3: key lookups after deletes (guards: `key_index_deleted` was never
// populated and is gone; keys are masked by `key_index` and node deletes)
// ============================================================================

fn f3_key_after_delete(name: &str, mvcc: bool, in_snapshot: bool, reuse_key: bool) {
  let mut fx = Fixture::new(name, mvcc);
  let mut v = Violations::default();
  let old = tx(fx.db(), |db| {
    db.create_node(Some("other")).expect("other");
    db.create_node(Some("user:k")).expect("keyed node")
  });
  if in_snapshot {
    fx.db().checkpoint().expect("checkpoint");
  }
  {
    let db = fx.db();
    db.begin(false).expect("begin");
    db.delete_node(old).expect("delete");
    v.expect_eq(
      "inside the deleting tx",
      "node_by_key",
      db.node_by_key("user:k"),
      None,
    );
    db.commit().expect("commit");
  }
  v.expect_eq(
    "after the delete",
    "node_by_key",
    fx.db().node_by_key("user:k"),
    None,
  );
  let want = if reuse_key {
    let new = tx(fx.db(), |db| {
      db.create_node(Some("user:k"))
        .expect("the deleted node's key is free")
    });
    Some(new)
  } else {
    None
  };
  fx.check_stages(&mut v, |db, stage, v| {
    v.expect_eq(stage, "node_by_key", db.node_by_key("user:k"), want);
    v.expect_eq(stage, "node_exists(old)", db.node_exists(old), false);
    v.expect_eq(stage, "node_key(old)", db.node_key(old), None);
    if let Some(new) = want {
      v.expect_eq(
        stage,
        "node_key(new)",
        db.node_key(new),
        Some("user:k".to_string()),
      );
    }
  });
  v.assert_none("key lookups after deleting a keyed node");
}

#[test]
fn f3_key_of_deleted_delta_node_does_not_resolve() {
  for mvcc in [false, true] {
    f3_key_after_delete("f3-delta", mvcc, false, false);
    f3_key_after_delete("f3-delta-reuse", mvcc, false, true);
  }
}

#[test]
fn f3_key_of_deleted_snapshot_node_does_not_resolve() {
  for mvcc in [false, true] {
    f3_key_after_delete("f3-snapshot", mvcc, true, false);
    f3_key_after_delete("f3-snapshot-reuse", mvcc, true, true);
  }
}

// ============================================================================
// F5: set_node_vector on a node that does not exist
// ============================================================================

#[test]
fn f5_set_node_vector_on_missing_node_fails_with_node_not_found() {
  let mut fx = Fixture::new("f5-missing", false);
  let mut v = Violations::default();
  let (emb, snap_node) = tx(fx.db(), |db| {
    let snap_node = db.create_node(Some("snap")).expect("snapshot node");
    let emb = db.define_propkey("embedding").expect("propkey");
    db.set_node_vector(snap_node, emb, &[0.0, 1.0, 0.0])
      .expect("first vector creates the store");
    (emb, snap_node)
  });
  fx.db().checkpoint().expect("checkpoint");
  let delta_node = tx(fx.db(), |db| db.create_node(Some("delta")).expect("delta"));
  let survivor = tx(fx.db(), |db| {
    db.create_node(Some("survivor")).expect("survivor")
  });
  // Deleted in earlier, committed transactions.
  tx(fx.db(), |db| {
    db.delete_node(delta_node).expect("delete delta node");
    db.delete_node(snap_node).expect("delete snapshot node");
  });

  let never: NodeId = 1_000_000;
  let db = fx.db();
  db.begin(false).expect("begin");
  let same_tx_created = db.create_node(Some("ephemeral")).expect("create");
  db.delete_node(same_tx_created)
    .expect("delete in the same tx");
  db.delete_node(survivor)
    .expect("delete committed node in the same tx");
  let cases = [
    ("never created", never),
    ("delta node deleted in an earlier tx", delta_node),
    ("snapshot node deleted in an earlier tx", snap_node),
    ("created and deleted in this tx", same_tx_created),
    ("committed node deleted in this tx", survivor),
  ];
  for (case, node) in cases {
    let result = db.set_node_vector(node, emb, &[1.0, 2.0, 3.0]);
    v.expect(
      "write",
      matches!(result, Err(KiteError::NodeNotFound(n)) if n == node),
      format!(
        "set_node_vector on {case} (node {node}): got {result:?}, want Err(NodeNotFound({node}))"
      ),
    );
  }
  db.commit().expect("commit");

  fx.check_stages(&mut v, |db, stage, v| {
    for (case, node) in cases {
      v.expect_eq(
        stage,
        &format!("node_vector of {case} (node {node})"),
        db.node_vector(node, emb).map(|v| v.to_vec()),
        None,
      );
      v.expect_eq(
        stage,
        &format!("has_node_vector of {case} (node {node})"),
        db.has_node_vector(node, emb),
        false,
      );
    }
  });
  v.assert_none("set_node_vector must refuse a node that does not exist");
}

// ============================================================================
// F6: read-your-writes for node vectors
// ============================================================================

/// The store normalizes on insert (cosine), so the committed vector is the
/// unit vector. The writing transaction must read the same value.
#[test]
fn f6_node_vector_inside_the_tx_equals_the_committed_value() {
  let mut fx = Fixture::new("f6-normalize", false);
  let mut v = Violations::default();
  let raw_first = [1.0f32, 2.0, 3.0];
  let raw_second = [3.0f32, 0.0, 4.0];

  let db = fx.db();
  db.begin(false).expect("begin");
  let first = db.create_node(Some("first")).expect("node");
  let emb = db.define_propkey("embedding").expect("propkey");
  db.set_node_vector(first, emb, &raw_first)
    .expect("vector (new store)");
  let in_tx_first = db.node_vector(first, emb).expect("own write").to_vec();
  db.commit().expect("commit");
  let committed_first = db.node_vector(first, emb).expect("committed").to_vec();

  // The store exists now.
  db.begin(false).expect("begin");
  let second = db.create_node(Some("second")).expect("node");
  db.set_node_vector(second, emb, &raw_second)
    .expect("vector (existing store)");
  let in_tx_second = db.node_vector(second, emb).expect("own write").to_vec();
  db.commit().expect("commit");
  let committed_second = db.node_vector(second, emb).expect("committed").to_vec();

  for (what, in_tx, committed, raw) in [
    ("new store", &in_tx_first, &committed_first, &raw_first[..]),
    (
      "existing store",
      &in_tx_second,
      &committed_second,
      &raw_second[..],
    ),
  ] {
    v.expect(
      "commit",
      approx_eq(committed, &unit(raw)),
      format!("{what}: committed vector {committed:?} is not unit({raw:?})"),
    );
    // Exactly equal: the same normalization, not just a close one.
    v.expect(
      "inside the writing tx",
      in_tx == committed,
      format!("{what}: read {in_tx:?} inside the tx, {committed:?} after commit"),
    );
  }

  let expected = [(first, committed_first), (second, committed_second)];
  fx.check_stages(&mut v, |db, stage, v| {
    for (node, want) in &expected {
      let got = db.node_vector(*node, emb).map(|v| v.to_vec());
      v.expect(
        stage,
        got.as_deref().is_some_and(|got| approx_eq(got, want)),
        format!("node_vector({node}) = {got:?}, want {want:?}"),
      );
    }
  });
  v.assert_none("node_vector must read the same inside the tx and after commit");
}

// ============================================================================
// F7: vector reads racing checkpoint installs (guard)
// ============================================================================

/// A reader hammers `node_vector` / `has_node_vector` for vectors that live
/// in the snapshot while a writer commits new vectors and alternates blocking
/// and background checkpoints. Every install replaces the snapshot and the
/// vector-store state; a reader must never see a vector go missing.
#[test]
fn f7_vector_reads_never_miss_while_checkpoints_install() {
  const NODES: usize = 64;
  const DIMS: usize = 16;
  let fx = Fixture::new("f7-race", false);
  let db = fx.shared();
  let vector_of = |i: usize| -> Vec<f32> {
    (0..DIMS)
      .map(|d| ((i * 31 + d * 7) % 13 + 1) as f32)
      .collect()
  };
  let (nodes, emb) = tx(&db, |db| {
    let emb = db.define_propkey("embedding").expect("propkey");
    let nodes: Vec<NodeId> = (0..NODES)
      .map(|i| {
        let node = db.create_node(None).expect("node");
        db.set_node_vector(node, emb, &vector_of(i))
          .expect("vector");
        node
      })
      .collect();
    (nodes, emb)
  });
  db.checkpoint().expect("checkpoint");
  let expected: Vec<Vec<f32>> = (0..NODES).map(|i| unit(&vector_of(i))).collect();

  let stop = Arc::new(AtomicBool::new(false));
  let reads = Arc::new(AtomicUsize::new(0));
  let reader = {
    let (db, stop, reads, nodes, expected) = (
      Arc::clone(&db),
      Arc::clone(&stop),
      Arc::clone(&reads),
      nodes.clone(),
      expected.clone(),
    );
    std::thread::spawn(move || -> Vec<String> {
      let mut misses = Vec::new();
      while !stop.load(Ordering::Acquire) {
        for (node, want) in nodes.iter().zip(&expected) {
          match db.node_vector(*node, emb) {
            Some(got) if approx_eq(&got, want) => {}
            got => misses.push(format!("node_vector({node}) = {got:?}")),
          }
          if !db.has_node_vector(*node, emb) {
            misses.push(format!("has_node_vector({node}) = false"));
          }
          reads.fetch_add(2, Ordering::Relaxed);
        }
        if misses.len() > 20 {
          break;
        }
      }
      misses
    })
  };

  let started = Instant::now();
  let mut checkpoints = 0usize;
  let mut round = 0usize;
  while started.elapsed() < Duration::from_millis(1500) {
    tx(&db, |db| {
      let node = db.create_node(None).expect("node");
      db.set_node_vector(node, emb, &vector_of(NODES + round))
        .expect("vector");
    });
    let result = if round.is_multiple_of(2) {
      db.checkpoint()
    } else {
      db.background_checkpoint()
    };
    match result {
      Ok(()) => checkpoints += 1,
      Err(KiteError::CheckpointDeclined(_)) => {}
      Err(err) => panic!("checkpoint failed: {err}"),
    }
    round += 1;
  }
  stop.store(true, Ordering::Release);
  let misses = reader.join().expect("reader thread");
  eprintln!(
    "f7: {} reads across {checkpoints} checkpoint installs, {} misses",
    reads.load(Ordering::Relaxed),
    misses.len()
  );
  assert!(checkpoints > 0, "no checkpoint ran");
  assert!(
    misses.is_empty(),
    "vector reads missed during checkpoint installs ({checkpoints} installs):\n  {}",
    misses.join("\n  ")
  );
}

// ============================================================================
// Perf baselines (F4, F8)
// ============================================================================

/// Wall and process CPU time of one step. Other lanes load this machine, so
/// CPU time is the comparable number.
#[derive(Clone, Copy)]
struct Cost {
  wall: Duration,
  cpu: Duration,
}

impl Cost {
  fn of<R>(f: impl FnOnce() -> R) -> (R, Cost) {
    let (wall, cpu) = (Instant::now(), cpu_time());
    let result = f();
    let cost = Cost {
      wall: wall.elapsed(),
      cpu: cpu_time().saturating_sub(cpu),
    };
    (result, cost)
  }
}

impl Display for Cost {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "cpu {:>9.1} ms  (wall {:>9.1} ms)",
      self.cpu.as_secs_f64() * 1e3,
      self.wall.as_secs_f64() * 1e3
    )
  }
}

/// User + system CPU time of this process.
#[cfg(unix)]
fn cpu_time() -> Duration {
  let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
  // SAFETY: getrusage fills the struct it is given; RUSAGE_SELF is valid.
  let usage = unsafe {
    libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr());
    usage.assume_init()
  };
  let time = |t: libc::timeval| Duration::new(t.tv_sec as u64, t.tv_usec as u32 * 1000);
  time(usage.ru_utime) + time(usage.ru_stime)
}

#[cfg(not(unix))]
fn cpu_time() -> Duration {
  Duration::ZERO
}

/// F4: delete `B4_DELETES` delta-created nodes (each with out- and in-edges)
/// from a delta holding `B4_NODES` nodes and `B4_NODES * B4_OUT_DEGREE` edges.
/// The commit merges every delete into the committed delta
/// (`DeltaState::delete_node`), which scanned every `in_add` entry per delete.
#[test]
#[ignore = "perf baseline; run with --ignored --nocapture"]
fn bench_f4_delete_delta_nodes() {
  let nodes = env_usize("B4_NODES", 100_000);
  let out_degree = env_usize("B4_OUT_DEGREE", 10);
  let deletes = env_usize("B4_DELETES", 10_000).min(nodes);
  let edges = nodes * out_degree;
  let wal = (edges * 48 + nodes * 64 + deletes * 64 + (16 << 20)).next_power_of_two();
  let fx = Fixture::with_options(
    "bench-f4",
    SingleFileOpenOptions::new()
      .auto_checkpoint(false)
      .wal_size(wal),
  );
  let db = fx.db();

  let ((ids, t), build) = Cost::of(|| {
    let (ids, t) = tx(db, |db| {
      let t = db.define_etype("E").expect("etype");
      let keys: Vec<Option<&str>> = vec![None; nodes];
      (db.create_nodes_batch(&keys).expect("nodes"), t)
    });
    let mut batch = Vec::with_capacity(100_000);
    for (i, &src) in ids.iter().enumerate() {
      for k in 0..out_degree {
        batch.push((src, t, ids[(i * 7 + k * 13 + 1) % nodes]));
      }
      if batch.len() >= 100_000 || i + 1 == ids.len() {
        tx(db, |db| db.add_edges_batch(&batch).expect("edges"));
        batch.clear();
      }
    }
    (ids, t)
  });
  let edges_before = db.count_edges();

  let stride = (nodes / deletes.max(1)).max(1);
  let victims: Vec<NodeId> = ids.iter().step_by(stride).take(deletes).copied().collect();
  let ((), in_tx) = Cost::of(|| {
    db.begin(false).expect("begin");
    for &node in &victims {
      db.delete_node(node).expect("delete");
    }
  });
  let ((), commit) = Cost::of(|| db.commit().expect("commit"));
  let edges_after = db.count_edges();
  assert!(!db.edge_exists(victims[0], t, ids[1]));

  eprintln!(
    "bench_f4: nodes={nodes} edges={edges_before} deletes={} (edges after: {edges_after})\n  \
     build                   {build}\n  \
     delete_node calls       {in_tx}\n  \
     commit (delta merge)    {commit}\n  \
     per delete (cpu)        {:.2} us",
    victims.len(),
    commit.cpu.as_secs_f64() * 1e6 / victims.len().max(1) as f64,
  );
}

/// F8: checkpoint cost and first-read cost with `B4_VECTORS` vectors of
/// `B4_DIMS` dimensions. Every checkpoint clones every manifest, and the
/// install leaves the stores to be decoded again by the next access.
#[test]
#[ignore = "perf baseline; run with --ignored --nocapture"]
fn bench_f8_vector_checkpoint() {
  let vectors = env_usize("B4_VECTORS", 100_000);
  let dims = env_usize("B4_DIMS", 128);
  let wal = (vectors * (dims * 4 + 96) * 2 + (16 << 20)).next_power_of_two();
  let mut fx = Fixture::with_options(
    "bench-f8",
    SingleFileOpenOptions::new()
      .auto_checkpoint(false)
      .wal_size(wal),
  );
  let vector_of = |i: usize| -> Vec<f32> {
    (0..dims)
      .map(|d| (((i * 2_654_435_761 + d * 40_503) % 1000) as f32 + 1.0) / 1000.0)
      .collect()
  };

  let ((emb, ids), load) = Cost::of(|| {
    let emb = tx(fx.db(), |db| {
      db.define_propkey("embedding").expect("propkey")
    });
    let mut ids = Vec::with_capacity(vectors);
    for chunk_start in (0..vectors).step_by(10_000) {
      let chunk = (vectors - chunk_start).min(10_000);
      tx(fx.db(), |db| {
        let keys: Vec<Option<&str>> = vec![None; chunk];
        for (j, node) in db
          .create_nodes_batch(&keys)
          .expect("nodes")
          .into_iter()
          .enumerate()
        {
          db.set_node_vector(node, emb, &vector_of(chunk_start + j))
            .expect("vector");
          ids.push(node);
        }
      });
    }
    (emb, ids)
  });

  let add_one = |db: &SingleFileDB, i: usize| {
    tx(db, |db| {
      let node = db.create_node(None).expect("node");
      db.set_node_vector(node, emb, &vector_of(vectors + i))
        .expect("vector");
    });
  };
  let read = |db: &SingleFileDB, node: NodeId| {
    assert!(db.node_vector(node, emb).is_some(), "vector {node} missing");
  };

  let db = fx.db();
  let ((), cp1) = Cost::of(|| db.checkpoint().expect("checkpoint 1"));
  let ((), first_read1) = Cost::of(|| read(db, ids[0]));
  let ((), second_read1) = Cost::of(|| read(db, ids[1]));
  add_one(db, 0);
  let ((), cp2) = Cost::of(|| db.checkpoint().expect("checkpoint 2"));
  // No vector access in between (a vector write would decode the store):
  // the next checkpoint has to decode the store first.
  tx(db, |db| db.create_node(None).expect("node"));
  let ((), cp3) = Cost::of(|| db.checkpoint().expect("checkpoint 3"));
  let ((), first_read3) = Cost::of(|| read(db, ids[2]));
  add_one(db, 2);
  let ((), bg) = Cost::of(|| db.background_checkpoint().expect("background checkpoint"));
  let ((), first_read_bg) = Cost::of(|| read(db, ids[3]));
  let ((), reopen) = Cost::of(|| fx.reopen());
  let db = fx.db();
  let ((), first_read_open) = Cost::of(|| read(db, ids[4]));

  eprintln!(
    "bench_f8: vectors={vectors} dims={dims}\n  \
     load                                       {load}\n  \
     checkpoint 1 (stores in memory)            {cp1}\n  \
     first node_vector after checkpoint 1       {first_read1}\n  \
     second node_vector                         {second_read1}\n  \
     checkpoint 2 (+1 vector, store decoded)    {cp2}\n  \
     checkpoint 3 (+1 node, store still lazy)   {cp3}\n  \
     first node_vector after checkpoint 3       {first_read3}\n  \
     background checkpoint (+1 vector)          {bg}\n  \
     first node_vector after background         {first_read_bg}\n  \
     reopen                                     {reopen}\n  \
     first node_vector after reopen             {first_read_open}",
  );

  // The pieces of that cost, on one store of the same shape: the deep clone
  // `collect_graph_data_from` makes, the serialization into the snapshot,
  // and the decode the next access (or a wave-2 install) makes.
  let mut store = create_vector_store(VectorStoreConfig::new(dims));
  for (i, &node) in ids.iter().enumerate() {
    vector_store_insert(&mut store, node, &vector_of(i)).expect("insert");
  }
  let (cloned, clone) = Cost::of(|| store.clone());
  let (bytes, serialize) = Cost::of(|| serialize_manifest(&cloned));
  let (decoded, decode) = Cost::of(|| deserialize_manifest(&bytes).expect("decode"));
  assert_eq!(decoded.config.dimensions, dims);
  eprintln!(
    "bench_f8 pieces (one store, {:.1} MB serialized)\n  \
     clone                                      {clone}\n  \
     serialize                                  {serialize}\n  \
     decode                                     {decode}",
    bytes.len() as f64 / 1e6,
  );
}
