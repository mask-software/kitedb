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
//! Each scenario runs twice: with MVCC off, and (`*_mvcc`) with MVCC on and a
//! witness: a read transaction on another thread that begins before the
//! scenario's first write and stays open across its commits, so they record
//! version chains. Reads outside it must see the recreated node exactly as
//! without MVCC, and the witness must keep seeing the old node until the first
//! reopen.
//!
//! Run: `cargo test --no-default-features --test w2_delta_recreate`

use kitedb::api::kite::{EdgeDef, Kite, KiteOptions, NodeDef};
use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions,
};
use kitedb::types::{ETypeId, LabelId, NodeId, PropKeyId, PropValue};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

const OLD_KEY: &str = "node:old";
const NEW_KEY: &str = "node:new";

/// MVCC off, or on with a witness (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
  Plain,
  Mvcc,
}

/// Runs `$scenario` as test `$plain` with MVCC off and as `$mvcc` with MVCC on.
macro_rules! in_both_modes {
  ($scenario:ident: $plain:ident, $mvcc:ident) => {
    #[test]
    fn $plain() {
      $scenario(Mode::Plain);
    }

    #[test]
    fn $mvcc() {
      $scenario(Mode::Mvcc);
    }
  };
}

/// MVCC GC runs every few ms and keeps nothing for retention, so it prunes all
/// history no open transaction needs while a scenario runs.
const GC_INTERVAL_MS: u64 = 5;

fn options(mode: Mode) -> SingleFileOpenOptions {
  // Deterministic: checkpoints happen only where a test asks for one.
  SingleFileOpenOptions::new()
    .auto_checkpoint(false)
    .mvcc(mode == Mode::Mvcc)
    .mvcc_gc_interval_ms(GC_INTERVAL_MS)
    .mvcc_retention_ms(0)
}

fn open(path: &Path, mode: Mode) -> Arc<SingleFileDB> {
  Arc::new(open_single_file(path, options(mode)).expect("open"))
}

fn close(db: Arc<SingleFileDB>) {
  let db = Arc::into_inner(db).expect("no other reference to the database");
  close_single_file(db).expect("close");
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
/// each `Check`. The failure context names the steps so far. A witness must
/// keep its view at each `Check` until the first `Reopen` ends it; while it is
/// open, `Checkpoint` runs a background checkpoint (a blocking one waits for
/// every open transaction).
fn run_steps(
  path: &Path,
  mode: Mode,
  db: Arc<SingleFileDB>,
  witness: Option<Witness>,
  fx: &Fx,
  spec: Spec,
  steps: &[Step],
) {
  let (mut db, mut witness) = (db, witness);
  let mut ctx = String::from("commit");
  for step in steps {
    match step {
      Step::Check => {
        assert_recreated(&db, fx, spec, &ctx);
        if let Some(witness) = &witness {
          witness.assert_unchanged(&ctx);
        }
      }
      Step::Checkpoint => {
        let result = if witness.is_some() {
          db.background_checkpoint()
        } else {
          db.checkpoint()
        };
        result.unwrap_or_else(|e| panic!("{ctx}: checkpoint failed: {e:?}"));
        ctx.push_str(" > checkpoint");
      }
      Step::Reopen => {
        if let Some(witness) = witness.take() {
          witness.finish(&ctx);
        }
        close(db);
        db = Arc::new(
          open_single_file(path, options(mode))
            .unwrap_or_else(|e| panic!("{ctx}: reopen failed: {e:?}")),
        );
        ctx.push_str(" > reopen");
      }
    }
  }
  if let Some(witness) = witness {
    witness.finish(&ctx);
  }
  close(db);
}

// ============================================================================
// MVCC witness
// ============================================================================

/// A node's edges as `(etype, other end)`.
type Adjacent = Vec<(ETypeId, NodeId)>;

/// What a reader sees of the fixture.
#[derive(Debug, PartialEq)]
struct View {
  n_exists: bool,
  n_key: Option<String>,
  by_old_key: Option<NodeId>,
  by_new_key: Option<NodeId>,
  n_props: Option<HashMap<PropKeyId, PropValue>>,
  /// `node_prop` of `old_only`, `shared` and `new_only`.
  n_prop: [Option<PropValue>; 3],
  n_labels: Vec<LabelId>,
  /// `node_has_label` of `old_label` and `new_label`.
  n_has_label: [bool; 2],
  /// `(out_edges, in_edges)` of `n`, `a` and `b`.
  adjacency: [(Adjacent, Adjacent); 3],
  /// `edge_exists` of n->a, a->n, n->b and b->n.
  edge_exists: [bool; 4],
  n_a_weight: Option<PropValue>,
  n_a_props: Option<HashMap<PropKeyId, PropValue>>,
  nodes: Vec<NodeId>,
  edges: Vec<(NodeId, ETypeId, NodeId)>,
}

fn view(db: &SingleFileDB, fx: &Fx) -> View {
  let Fx { n, a, b, t, .. } = *fx;
  View {
    n_exists: db.node_exists(n),
    n_key: db.node_key(n),
    by_old_key: db.node_by_key(OLD_KEY),
    by_new_key: db.node_by_key(NEW_KEY),
    n_props: db.node_props(n),
    n_prop: [fx.old_only, fx.shared, fx.new_only].map(|key_id| db.node_prop(n, key_id)),
    n_labels: sorted(db.node_labels(n)),
    n_has_label: [fx.old_label, fx.new_label].map(|label_id| db.node_has_label(n, label_id)),
    adjacency: [n, a, b].map(|node| (sorted(db.out_edges(node)), sorted(db.in_edges(node)))),
    edge_exists: [(n, a), (a, n), (n, b), (b, n)].map(|(src, dst)| db.edge_exists(src, t, dst)),
    n_a_weight: db.edge_prop(n, t, a, fx.weight),
    n_a_props: db.edge_props(n, t, a),
    nodes: db.list_nodes(),
    edges: sorted(
      db.list_edges(None)
        .into_iter()
        .map(|e| (e.src, e.etype, e.dst))
        .collect(),
    ),
  }
}

/// A read transaction on another thread, open from `begin` to `finish`.
struct Witness {
  ask: mpsc::Sender<()>,
  answers: mpsc::Receiver<View>,
  /// What it saw when it began: what reads outside it saw then.
  seen: View,
  thread: thread::JoinHandle<()>,
}

impl Witness {
  fn begin(db: &Arc<SingleFileDB>, fx: &Fx) -> Self {
    let (ask, asks) = mpsc::channel::<()>();
    let (answer, answers) = mpsc::channel();
    let (reader, fx_copy) = (Arc::clone(db), *fx);
    let thread = thread::spawn(move || {
      reader.begin(true).expect("begin witness");
      // Answer every ask until `finish` hangs up.
      while answer.send(view(&reader, &fx_copy)).is_ok() && asks.recv().is_ok() {}
      reader.commit().expect("end witness");
    });
    let seen = answers.recv().expect("witness began");
    assert_eq!(
      seen,
      view(db, fx),
      "a witness that just began must see the committed state"
    );
    Self {
      ask,
      answers,
      seen,
      thread,
    }
  }

  /// The witness still sees what it saw when it began.
  fn assert_unchanged(&self, ctx: &str) {
    self.ask.send(()).expect("ask witness");
    let now = self.answers.recv().expect("witness view");
    assert_eq!(now, self.seen, "{ctx}: the witness's snapshot changed");
  }

  fn finish(self, ctx: &str) {
    self.assert_unchanged(ctx);
    drop(self.ask);
    self.thread.join().expect("witness thread");
  }
}

/// With MVCC, a witness of the old node `n`, begun before it is deleted.
fn witness(db: &Arc<SingleFileDB>, fx: &Fx, mode: Mode) -> Option<Witness> {
  (mode == Mode::Mvcc).then(|| {
    let witness = Witness::begin(db, fx);
    assert_eq!(
      witness.seen.n_key.as_deref(),
      Some(OLD_KEY),
      "the witness must see the old node"
    );
    witness
  })
}

// ============================================================================
// Recreate in a later transaction (create_node_with_id)
// ============================================================================

fn recreate_snapshot_node_visible_after_commit(mode: Mode) {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("visible.kitedb");
  let db = open(&path, mode);
  let fx = snapshot_fixture(&db);
  let witness = witness(&db, &fx, mode);
  delete_then_recreate(&db, &fx, FULL_NEW_KEY);
  run_steps(&path, mode, db, witness, &fx, FULL_NEW_KEY, &[Check]);
}
in_both_modes!(recreate_snapshot_node_visible_after_commit:
  r1_recreate_snapshot_node_visible_after_commit,
  r1_recreate_snapshot_node_visible_after_commit_mvcc);

/// The checkpoint path on its own: no read before the checkpoint.
fn recreate_snapshot_node_survives_checkpoint_and_reopen(mode: Mode) {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("checkpoint.kitedb");
  let db = open(&path, mode);
  let fx = snapshot_fixture(&db);
  let witness = witness(&db, &fx, mode);
  delete_then_recreate(&db, &fx, FULL_NEW_KEY);
  run_steps(
    &path,
    mode,
    db,
    witness,
    &fx,
    FULL_NEW_KEY,
    &[Checkpoint, Check, Reopen, Check],
  );
}
in_both_modes!(recreate_snapshot_node_survives_checkpoint_and_reopen:
  r1_recreate_snapshot_node_survives_checkpoint_and_reopen,
  r1_recreate_snapshot_node_survives_checkpoint_and_reopen_mvcc);

/// WAL replay without a checkpoint: reopen right after the commit.
fn recreate_snapshot_node_survives_wal_replay(mode: Mode) {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("replay.kitedb");
  let db = open(&path, mode);
  let fx = snapshot_fixture(&db);
  let witness = witness(&db, &fx, mode);
  delete_then_recreate(&db, &fx, FULL_NEW_KEY);
  run_steps(
    &path,
    mode,
    db,
    witness,
    &fx,
    FULL_NEW_KEY,
    &[Reopen, Check, Checkpoint, Check, Reopen, Check],
  );
}
in_both_modes!(recreate_snapshot_node_survives_wal_replay:
  r1_recreate_snapshot_node_survives_wal_replay,
  r1_recreate_snapshot_node_survives_wal_replay_mvcc);

/// The recreated node takes the old node's key again.
fn recreated_node_reuses_old_key(mode: Mode) {
  let spec = Spec {
    key: Some(OLD_KEY),
    labels_and_edges: true,
  };
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("key_reuse.kitedb");
  let db = open(&path, mode);
  let fx = snapshot_fixture(&db);
  let witness = witness(&db, &fx, mode);
  delete_then_recreate(&db, &fx, spec);
  run_steps(
    &path,
    mode,
    db,
    witness,
    &fx,
    spec,
    &[Check, Reopen, Check, Checkpoint, Check, Reopen, Check],
  );
}
in_both_modes!(recreated_node_reuses_old_key:
  r1_recreated_node_reuses_old_key,
  r1_recreated_node_reuses_old_key_mvcc);

/// Guard (passes before the fix): the same scenario when the old node never
/// reached the snapshot. The old edge n->a carries no prop here: a delta edge's
/// props outlive an unlink and reattach on relink with or without a node
/// delete, which is a separate, general issue and not R1.
fn guard_recreate_delta_node(mode: Mode) {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("delta_only.kitedb");
  let db = open(&path, mode);
  let fx = build_fixture(&db, false);
  let witness = witness(&db, &fx, mode);
  delete_then_recreate(&db, &fx, FULL_NEW_KEY);
  run_steps(
    &path,
    mode,
    db,
    witness,
    &fx,
    FULL_NEW_KEY,
    &[Check, Reopen, Check, Checkpoint, Check, Reopen, Check],
  );
}
in_both_modes!(guard_recreate_delta_node:
  r1_guard_recreate_delta_node,
  r1_guard_recreate_delta_node_mvcc);

// ============================================================================
// Delete and recreate within one transaction
// ============================================================================

fn delete_and_recreate_in_one_tx(mode: Mode) {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("same_tx.kitedb");
  let db = open(&path, mode);
  let fx = snapshot_fixture(&db);
  let witness = witness(&db, &fx, mode);

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
    mode,
    db,
    witness,
    &fx,
    FULL_NEW_KEY,
    &[Check, Reopen, Check, Checkpoint, Check, Reopen, Check],
  );
}
in_both_modes!(delete_and_recreate_in_one_tx:
  r1_delete_and_recreate_in_one_tx,
  r1_delete_and_recreate_in_one_tx_mvcc);

/// Without reads inside the tx: the commit must not bring the old node back.
fn delete_and_recreate_in_one_tx_does_not_resurrect_old_node(mode: Mode) {
  let spec = Spec {
    key: Some(NEW_KEY),
    labels_and_edges: false,
  };
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("same_tx_merge.kitedb");
  let db = open(&path, mode);
  let fx = snapshot_fixture(&db);
  let witness = witness(&db, &fx, mode);

  db.begin(false).expect("begin");
  db.delete_node(fx.n).expect("delete n");
  write_recreated(&db, &fx, spec);
  db.commit().expect("commit");

  run_steps(
    &path,
    mode,
    db,
    witness,
    &fx,
    spec,
    &[Check, Reopen, Check, Checkpoint, Check, Reopen, Check],
  );
}
in_both_modes!(delete_and_recreate_in_one_tx_does_not_resurrect_old_node:
  r1_delete_and_recreate_in_one_tx_does_not_resurrect_old_node,
  r1_delete_and_recreate_in_one_tx_does_not_resurrect_old_node_mvcc);

/// The old node's state can also live in the delta: edges added after the
/// checkpoint (one a self-loop), a label, a prop, and an edge prop on a
/// snapshot edge. The recreate drops all of it, whether the delete commits on
/// its own, or that state, the delete and the recreate share one transaction.
fn recreate_masks_old_state_held_in_the_delta(mode: Mode) {
  let spec = Spec {
    key: Some(NEW_KEY),
    labels_and_edges: false,
  };
  for same_tx in [false, true] {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("delta_held.kitedb");
    let db = open(&path, mode);
    let fx = snapshot_fixture(&db);
    let witness = witness(&db, &fx, mode);
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
      mode,
      db,
      witness,
      &fx,
      spec,
      &[Check, Reopen, Check, Checkpoint, Check, Reopen, Check],
    );
  }
}
in_both_modes!(recreate_masks_old_state_held_in_the_delta:
  r1_recreate_masks_old_state_held_in_the_delta,
  r1_recreate_masks_old_state_held_in_the_delta_mvcc);

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

fn upsert_node_by_id_recreates_deleted_snapshot_node(mode: Mode) {
  let spec = Spec {
    key: None,
    labels_and_edges: false,
  };
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("upsert.kitedb");
  let db = open(&path, mode);
  let fx = snapshot_fixture(&db);
  let witness = witness(&db, &fx, mode);

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
    mode,
    db,
    witness,
    &fx,
    spec,
    &[Check, Reopen, Check, Checkpoint, Check, Reopen, Check],
  );
}
in_both_modes!(upsert_node_by_id_recreates_deleted_snapshot_node:
  r1_upsert_node_by_id_recreates_deleted_snapshot_node,
  r1_upsert_node_by_id_recreates_deleted_snapshot_node_mvcc);

/// What a reader sees of the Kite fixture's `alice` and `bob`.
#[derive(Debug, PartialEq)]
struct KiteView {
  exists: bool,
  props: [Option<PropValue>; 2],
  key: Option<Option<String>>,
  by_key: Option<NodeId>,
  neighbors: [Vec<NodeId>; 2],
  has_edge: [bool; 2],
  counts: (u64, u64),
}

fn kite_view(kite: &Kite, alice: NodeId, bob: NodeId) -> KiteView {
  KiteView {
    exists: kite.exists(alice),
    props: ["name", "age"].map(|name| kite.prop(alice, name)),
    key: kite
      .node_by_id(alice)
      .expect("node_by_id")
      .map(|node| node.key().map(str::to_string)),
    by_key: kite
      .get("User", "alice")
      .expect("get")
      .map(|node| node.id()),
    neighbors: [
      kite.neighbors_out(alice, None).expect("out"),
      kite.neighbors_in(alice, None).expect("in"),
    ],
    has_edge: [(alice, bob), (bob, alice)]
      .map(|(src, dst)| kite.has_edge(src, "FOLLOWS", dst).expect("has_edge")),
    counts: (kite.count_nodes(), kite.count_edges()),
  }
}

/// Runs `write` while a read transaction on another thread stays open across
/// it (see the module docs), then checks the reader still sees what it saw.
/// The writes need `&mut Kite`, so both threads share it through a mutex,
/// each holding it only for a call.
fn with_kite_witness(
  kite: Kite,
  alice: NodeId,
  bob: NodeId,
  write: impl FnOnce(&mut Kite),
) -> Kite {
  let kite = Mutex::new(kite);
  let (ask, asks) = mpsc::channel::<()>();
  let (answer, answers) = mpsc::channel();
  thread::scope(|scope| {
    let kite = &kite;
    scope.spawn(move || {
      kite
        .lock()
        .expect("kite")
        .raw()
        .begin(true)
        .expect("begin witness");
      loop {
        let view = kite_view(&kite.lock().expect("kite"), alice, bob);
        if answer.send(view).is_err() || asks.recv().is_err() {
          break;
        }
      }
      kite
        .lock()
        .expect("kite")
        .raw()
        .commit()
        .expect("end witness");
    });
    let seen = answers.recv().expect("witness began");
    assert_eq!(
      seen.props[1],
      Some(PropValue::I64(30)),
      "the witness must see the old alice"
    );
    write(&mut kite.lock().expect("kite"));
    ask.send(()).expect("ask witness");
    let now = answers.recv().expect("witness view");
    assert_eq!(now, seen, "the witness's snapshot changed");
    drop(ask);
  });
  kite.into_inner().expect("kite")
}

fn kite_upsert_by_id_recreates_deleted_snapshot_node(mode: Mode) {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("kite");
  let schema = || {
    KiteOptions::new()
      .disable_close_checkpoint()
      .mvcc(mode == Mode::Mvcc)
      .mvcc_gc_interval_ms(GC_INTERVAL_MS)
      .mvcc_retention_ms(0)
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

  let recreate = |kite: &mut Kite| {
    assert!(kite.delete_node(alice).expect("delete alice"));
    assert!(!kite.exists(alice));
    kite
      .upsert_by_id("User", alice)
      .expect("upsert_by_id builder")
      .set("name", PropValue::String("Recreated".into()))
      .execute()
      .expect("upsert_by_id of a deleted snapshot node");
  };
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

  let write = |kite: &mut Kite| {
    recreate(kite);
    check(kite, "after upsert_by_id");
  };
  let kite = match mode {
    Mode::Plain => {
      write(&mut kite);
      kite
    }
    Mode::Mvcc => with_kite_witness(kite, alice, bob, write),
  };
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
in_both_modes!(kite_upsert_by_id_recreates_deleted_snapshot_node:
  r1_kite_upsert_by_id_recreates_deleted_snapshot_node,
  r1_kite_upsert_by_id_recreates_deleted_snapshot_node_mvcc);

// ============================================================================
// MVCC: a reader of the recreated node
// ============================================================================

/// A reader that began after the recreate keeps the recreated node when it is
/// deleted again, without the old node's state (the old copy is still in the
/// snapshot, masked by the first delete), and after a checkpoint drops it.
#[test]
fn r1_mvcc_reader_keeps_recreated_node_deleted_again() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("deleted_again.kitedb");
  let db = open(&path, Mode::Mvcc);
  let fx = snapshot_fixture(&db);
  delete_then_recreate(&db, &fx, FULL_NEW_KEY);
  assert_recreated(&db, &fx, FULL_NEW_KEY, "recreated");
  // It sees what `assert_recreated` checked (`Witness::begin` compares).
  let witness = Witness::begin(&db, &fx);

  db.begin(false).expect("begin second delete");
  db.delete_node(fx.n).expect("delete the recreated n");
  db.commit().expect("commit second delete");
  assert!(!db.node_exists(fx.n), "the second delete must hide n");
  witness.assert_unchanged("deleted again");

  db.background_checkpoint().expect("background checkpoint");
  assert!(
    !db.node_exists(fx.n),
    "deleted again > checkpoint: n is back"
  );
  witness.finish("deleted again > checkpoint");
  close(db);
}
