//! raydb-b4 `publish-cost` lane: what a commit group's publish records for
//! MVCC readers, and the snapshot isolation it must keep.
//!
//! Finding 1: a commit that created nodes while another transaction was open
//! recorded a version chain per created node (the node absent before it).
//! Every read checks that a node exists at the reader's snapshot before it
//! reads the node's key, props, labels or edges, so a reader whose snapshot
//! predates the commit needs nothing per created node but the commit itself.
//!
//! Found by the model test below: a reader whose snapshot predates a node
//! found it by its key once the node was deleted while the reader was open
//! (the key lookup did not check that its owner existed at the snapshot).
//!
//! The guards compare what readers see with a reference model: readers that
//! begin before, during and after commits of several writers (so commits form
//! groups), across creates, deletes, recreates by id, background checkpoints
//! and GC.
use crate::core::single_file::{open_single_file, SingleFileDB, SingleFileOpenOptions};
use crate::types::*;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use tempfile::{tempdir, TempDir};

fn open(dir: &TempDir) -> Arc<SingleFileDB> {
  let options = SingleFileOpenOptions::new()
    .mvcc(true)
    .mvcc_gc_interval_ms(60 * 60 * 1000)
    .auto_checkpoint(false);
  Arc::new(open_single_file(dir.path().join("publish-cost.kitedb"), options).expect("open"))
}

type Job = Box<dyn FnOnce(&SingleFileDB) + Send>;

/// A read transaction held open on its own thread; `read` runs a closure in it.
struct Reader {
  jobs: Option<mpsc::Sender<Job>>,
  handle: Option<std::thread::JoinHandle<()>>,
}

impl Reader {
  fn begin(db: &Arc<SingleFileDB>) -> Self {
    let (jobs_tx, jobs_rx) = mpsc::channel::<Job>();
    let (ready_tx, ready_rx) = mpsc::channel();
    let db = Arc::clone(db);
    let handle = std::thread::spawn(move || {
      db.begin(true).expect("begin reader");
      ready_tx.send(()).expect("reader ready");
      for job in jobs_rx {
        job(&db);
      }
      db.rollback().expect("end reader");
    });
    ready_rx.recv().expect("reader began");
    Self {
      jobs: Some(jobs_tx),
      handle: Some(handle),
    }
  }

  fn read<T: Send + 'static>(&self, read: impl FnOnce(&SingleFileDB) -> T + Send + 'static) -> T {
    let (result_tx, result_rx) = mpsc::channel();
    let job: Job = Box::new(move |db| {
      let _ = result_tx.send(read(db));
    });
    self
      .jobs
      .as_ref()
      .expect("reader open")
      .send(job)
      .expect("send read");
    result_rx.recv().expect("read result")
  }
}

impl Drop for Reader {
  fn drop(&mut self) {
    drop(self.jobs.take());
    if let Some(handle) = self.handle.take() {
      let _ = handle.join();
    }
  }
}

struct Schema {
  prop: PropKeyId,
  label: LabelId,
  etype: ETypeId,
  edge_props: Vec<PropKeyId>,
}

fn seed(db: &SingleFileDB) -> Schema {
  db.begin(false).expect("begin seed");
  let prop = db.define_propkey("p").expect("propkey");
  let label = db.define_label("L").expect("label");
  let etype = db.define_etype("T").expect("etype");
  let edge_props = (0..3)
    .map(|i| db.define_propkey(&format!("e{i}")).expect("edge propkey"))
    .collect();
  let seed = db.create_node(Some("seed")).expect("seed node");
  db.set_node_prop(seed, prop, PropValue::I64(-1))
    .expect("seed prop");
  db.commit().expect("commit seed");
  Schema {
    prop,
    label,
    etype,
    edge_props,
  }
}

/// Commit `count` keyed nodes with a prop and a label each, and a ring of
/// edges between them with props. Returns their ids.
fn commit_ring(db: &SingleFileDB, schema: &Schema, prefix: &str, count: usize) -> Vec<NodeId> {
  db.begin(false).expect("begin ring");
  let keys: Vec<String> = (0..count).map(|i| format!("{prefix}{i}")).collect();
  let key_refs: Vec<Option<&str>> = keys.iter().map(|key| Some(key.as_str())).collect();
  let ids = db.create_nodes_batch(&key_refs).expect("create nodes");
  for (i, &id) in ids.iter().enumerate() {
    db.set_node_prop(id, schema.prop, PropValue::I64(i as i64))
      .expect("node prop");
    db.add_node_label(id, schema.label).expect("label");
  }
  let edges = ids
    .iter()
    .enumerate()
    .map(|(i, &src)| {
      let dst = ids[(i + 1) % ids.len()];
      let props = schema
        .edge_props
        .iter()
        .map(|&key| (key, PropValue::I64(i as i64)))
        .collect();
      (src, schema.etype, dst, props)
    })
    .collect();
  db.add_edges_with_props_batch(edges).expect("edges");
  db.commit().expect("commit ring");
  ids
}

/// The aggregate state a reader sees.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Totals {
  count_nodes: usize,
  listed_nodes: usize,
  count_edges: usize,
  listed_edges: usize,
  labelled: usize,
}

fn totals(db: &SingleFileDB, label: LabelId) -> Totals {
  Totals {
    count_nodes: db.count_nodes(),
    listed_nodes: db.list_nodes().len(),
    count_edges: db.count_edges(),
    listed_edges: db.list_edges(None).len(),
    labelled: db.nodes_with_label(label).len(),
  }
}

/// Whatever of `ids` (keys `prefix{i}`) a reader sees.
fn seen(db: &SingleFileDB, ids: &[NodeId], prefix: &str) -> Vec<String> {
  let mut seen = Vec::new();
  for (i, &id) in ids.iter().enumerate() {
    let key = format!("{prefix}{i}");
    if db.node_exists(id) {
      seen.push(format!("{id} exists"));
    }
    if db.node_by_key(&key).is_some() {
      seen.push(format!("{key} resolves"));
    }
    if db.node_key(id).is_some() {
      seen.push(format!("{id} has a key"));
    }
    if db.node_props(id).is_some_and(|props| !props.is_empty()) {
      seen.push(format!("{id} has props"));
    }
    if !db.node_labels(id).is_empty() {
      seen.push(format!("{id} has labels"));
    }
    if !db.out_edges(id).is_empty() || !db.in_edges(id).is_empty() {
      seen.push(format!("{id} has edges"));
    }
  }
  seen
}

/// Finding 1: a commit creating 200 nodes (keys, props, labels, edges with
/// props) while a reader is open recorded 200 node version chains. The reader
/// must still see none of them.
#[test]
fn created_nodes_record_no_version_chains() {
  let dir = tempdir().expect("tempdir");
  let db = open(&dir);
  let schema = seed(&db);
  let label = schema.label;
  let reader = Reader::begin(&db);
  let before = reader.read(move |db| totals(db, label));

  let ids = Arc::new(commit_ring(&db, &schema, "fresh-", 200));

  let counts = db
    .mvcc
    .as_ref()
    .expect("mvcc")
    .version_chain
    .read()
    .counts();
  let recorded = [
    ("node", counts.node_versions),
    ("key owner", counts.key_owner_versions),
    ("node prop", counts.node_prop_versions),
    ("node label", counts.node_label_versions),
    ("edge", counts.edge_versions),
    ("edge prop", counts.edge_prop_versions),
  ];
  assert!(
    recorded.iter().all(|&(_, chains)| chains == 0),
    "a commit of 200 new nodes recorded version chains: {recorded:?}"
  );

  // The reader sees none of the new nodes, and a new reader sees all of them.
  let (after, hidden) = {
    let ids = Arc::clone(&ids);
    reader.read(move |db| (totals(db, label), seen(db, &ids, "fresh-")))
  };
  assert_eq!(after, before, "the reader's totals changed");
  assert!(hidden.is_empty(), "the reader sees new nodes: {hidden:?}");
  drop(reader);
  let late = Reader::begin(&db);
  let (late_totals, late_seen) = {
    let ids = Arc::clone(&ids);
    late.read(move |db| (totals(db, label), seen(db, &ids, "fresh-")))
  };
  assert_eq!(late_totals.count_nodes, before.count_nodes + 200);
  assert_eq!(late_totals.count_edges, before.count_edges + 200);
  assert_eq!(late_totals.labelled, before.labelled + 200);
  assert_eq!(
    late_seen.len(),
    200 * 6,
    "a later reader sees every new node whole"
  );
}

/// A reader whose snapshot predates a node must not find it by its key once
/// the node is deleted while the reader is open. Regression: the delete
/// recorded the key's owner before it (the node) with no start, and the key
/// lookup returned it without checking that the node existed at the reader's
/// snapshot (every other read checks that first).
#[test]
fn key_lookup_hides_a_node_created_and_deleted_after_the_snapshot() {
  let dir = tempdir().expect("tempdir");
  let db = open(&dir);
  seed(&db);
  let reader = Reader::begin(&db);
  db.begin(false).expect("begin create");
  let node = db.create_node(Some("brief")).expect("create");
  db.commit().expect("commit create");
  assert_eq!(
    reader.read(|db| db.node_by_key("brief")),
    None,
    "after the create"
  );
  db.begin(false).expect("begin delete");
  db.delete_node(node).expect("delete");
  db.commit().expect("commit delete");
  assert_eq!(
    reader.read(move |db| (db.node_by_key("brief"), db.node_exists(node))),
    (None, false),
    "after the delete"
  );
}

/// A reader keeps not seeing nodes created after its snapshot once a
/// background checkpoint moves them into the snapshot, and across GC runs;
/// once it ends, GC drops what was kept for it.
#[test]
fn created_nodes_stay_hidden_across_checkpoint_and_gc() {
  let dir = tempdir().expect("tempdir");
  let db = open(&dir);
  let schema = seed(&db);
  let label = schema.label;
  let reader = Reader::begin(&db);
  let before = reader.read(move |db| totals(db, label));
  let ids = Arc::new(commit_ring(&db, &schema, "moved-", 50));

  db.background_checkpoint().expect("background checkpoint");
  let mvcc = db.mvcc.as_ref().expect("mvcc");
  mvcc.run_gc();
  for round in 0..2 {
    let ids = Arc::clone(&ids);
    let (after, hidden) = reader.read(move |db| (totals(db, label), seen(db, &ids, "moved-")));
    assert_eq!(after, before, "round {round}: the reader's totals changed");
    assert!(
      hidden.is_empty(),
      "round {round}: the reader sees new nodes: {hidden:?}"
    );
    db.background_checkpoint().expect("background checkpoint");
  }

  drop(reader);
  mvcc.run_gc();
  let counts = mvcc.version_chain.read().counts();
  assert_eq!(counts.node_versions, 0, "GC kept history no reader needs");
  assert!(
    mvcc.history_ts() == 0 || mvcc.version_chain.read().newest_commit_ts() == 0,
    "GC kept history no reader needs"
  );
  let late = Reader::begin(&db);
  let late_seen = {
    let ids = Arc::clone(&ids);
    late.read(move |db| seen(db, &ids, "moved-"))
  };
  assert_eq!(late_seen.len(), 50 * 6);
}

/// A node created while readers are open, then deleted and recreated by id,
/// shows each reader the node its snapshot holds: none, the first, none, the
/// second.
#[test]
fn recreated_id_shows_each_reader_its_own_node() {
  let dir = tempdir().expect("tempdir");
  let db = open(&dir);
  let schema = seed(&db);
  let (prop, label, etype) = (schema.prop, schema.label, schema.etype);
  db.begin(false).expect("begin");
  let other = db.create_node(Some("other")).expect("other");
  db.commit().expect("commit");

  type NodeView = (
    bool,
    Option<String>,
    Option<NodeId>,
    Option<NodeId>,
    Option<PropValue>,
    Vec<LabelId>,
    Vec<(ETypeId, NodeId)>,
    Vec<(ETypeId, NodeId)>,
  );
  let look = move |id: NodeId| {
    move |db: &SingleFileDB| -> NodeView {
      (
        db.node_exists(id),
        db.node_key(id),
        db.node_by_key("first"),
        db.node_by_key("second"),
        db.node_prop(id, prop),
        db.node_labels(id),
        db.out_edges(id),
        db.in_edges(other),
      )
    }
  };

  let before_create = Reader::begin(&db);
  db.begin(false).expect("begin create");
  let id = db.create_node(Some("first")).expect("create");
  db.set_node_prop(id, prop, PropValue::I64(1)).expect("prop");
  db.add_node_label(id, label).expect("label");
  db.add_edge(id, etype, other).expect("edge");
  db.commit().expect("commit create");

  let after_create = Reader::begin(&db);
  db.begin(false).expect("begin delete");
  db.delete_node(id).expect("delete");
  db.commit().expect("commit delete");

  let after_delete = Reader::begin(&db);
  db.begin(false).expect("begin recreate");
  db.create_node_with_id(id, Some("second"))
    .expect("recreate");
  db.set_node_prop(id, prop, PropValue::I64(2)).expect("prop");
  db.commit().expect("commit recreate");
  let after_recreate = Reader::begin(&db);

  for _ in 0..2 {
    let absent: NodeView = (false, None, None, None, None, vec![], vec![], vec![]);
    assert_eq!(before_create.read(look(id)), absent, "before the create");
    assert_eq!(
      after_create.read(look(id)),
      (
        true,
        Some("first".to_string()),
        Some(id),
        None,
        Some(PropValue::I64(1)),
        vec![label],
        vec![(etype, other)],
        vec![(etype, id)],
      ),
      "after the create"
    );
    assert_eq!(after_delete.read(look(id)), absent, "after the delete");
    assert_eq!(
      after_recreate.read(look(id)),
      (
        true,
        Some("second".to_string()),
        None,
        Some(id),
        Some(PropValue::I64(2)),
        vec![],
        vec![],
        vec![],
      ),
      "after the recreate"
    );
    db.background_checkpoint().expect("background checkpoint");
    db.mvcc.as_ref().expect("mvcc").run_gc();
  }
}

// ============================================================================
// Randomized readers against a reference model
// ============================================================================

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct NodeModel {
  key: String,
  props: BTreeMap<PropKeyId, i64>,
  labels: BTreeSet<LabelId>,
}

type EdgeKey = (NodeId, ETypeId, NodeId);

/// One writer's nodes and edges (writers never touch each other's).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct PartModel {
  nodes: BTreeMap<NodeId, NodeModel>,
  edges: BTreeMap<EdgeKey, BTreeMap<PropKeyId, i64>>,
}

/// What a reader sees of a writer's part, read through every kind of lookup.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct PartView {
  nodes: BTreeMap<NodeId, NodeModel>,
  edges: BTreeMap<EdgeKey, BTreeMap<PropKeyId, i64>>,
  in_edges: BTreeSet<EdgeKey>,
  by_key: BTreeMap<String, NodeId>,
  listed: BTreeSet<NodeId>,
  labelled: BTreeMap<LabelId, BTreeSet<NodeId>>,
}

impl PartModel {
  fn view(&self) -> PartView {
    let mut labelled: BTreeMap<LabelId, BTreeSet<NodeId>> = BTreeMap::new();
    for (&id, node) in &self.nodes {
      for &label in &node.labels {
        labelled.entry(label).or_default().insert(id);
      }
    }
    PartView {
      nodes: self.nodes.clone(),
      edges: self.edges.clone(),
      in_edges: self.edges.keys().copied().collect(),
      by_key: self
        .nodes
        .iter()
        .map(|(&id, node)| (node.key.clone(), id))
        .collect(),
      listed: self.nodes.keys().copied().collect(),
      labelled,
    }
  }

  fn remove_node(&mut self, id: NodeId) {
    self.nodes.remove(&id);
    self
      .edges
      .retain(|&(src, _, dst), _| src != id && dst != id);
  }
}

/// What readers see of every state a writer's part went through, in commit
/// order (the next commit's appended before it commits), and every id and key
/// it used.
#[derive(Default)]
struct PartHistory {
  states: Vec<Arc<PartView>>,
  ids: BTreeSet<NodeId>,
  keys: BTreeSet<String>,
}

fn int(value: Option<PropValue>) -> Option<i64> {
  match value {
    Some(PropValue::I64(value)) => Some(value),
    _ => None,
  }
}

fn int_props(
  props: Option<std::collections::HashMap<PropKeyId, PropValue>>,
) -> BTreeMap<PropKeyId, i64> {
  props
    .into_iter()
    .flatten()
    .filter_map(|(key, value)| int(Some(value)).map(|value| (key, value)))
    .collect()
}

/// What `db`'s current transaction sees of a part that used `ids` and `keys`.
fn read_part(
  db: &SingleFileDB,
  ids: &BTreeSet<NodeId>,
  keys: &BTreeSet<String>,
  labels: &[LabelId],
  listed: &BTreeSet<NodeId>,
) -> PartView {
  let mut view = PartView::default();
  for &id in ids {
    if !db.node_exists(id) {
      continue;
    }
    let node = NodeModel {
      key: db.node_key(id).unwrap_or_default(),
      props: int_props(db.node_props(id)),
      labels: db.node_labels(id).into_iter().collect(),
    };
    view.nodes.insert(id, node);
    for (etype, dst) in db.out_edges(id) {
      view
        .edges
        .insert((id, etype, dst), int_props(db.edge_props(id, etype, dst)));
    }
    for (etype, src) in db.in_edges(id) {
      view.in_edges.insert((src, etype, id));
    }
  }
  for key in keys {
    if let Some(id) = db.node_by_key(key) {
      view.by_key.insert(key.clone(), id);
    }
  }
  view.listed = listed.intersection(ids).copied().collect();
  for &label in labels {
    let with: BTreeSet<NodeId> = db
      .nodes_with_label(label)
      .into_iter()
      .filter(|id| ids.contains(id))
      .collect();
    if !with.is_empty() {
      view.labelled.insert(label, with);
    }
  }
  view
}

/// One random transaction of writer `w` on its part, applied to `model` as
/// well. Commits only after the new state is appended to `history`.
#[allow(clippy::too_many_arguments)]
fn random_transaction(
  db: &SingleFileDB,
  rng: &mut StdRng,
  w: usize,
  model: &mut PartModel,
  history: &Mutex<PartHistory>,
  next_key: &mut usize,
  used_edges: &mut BTreeSet<EdgeKey>,
  deleted: &mut Vec<NodeId>,
  props: &[PropKeyId],
  labels: &[LabelId],
  etypes: &[ETypeId],
) {
  db.begin(false).expect("begin writer");
  let new_key = |next_key: &mut usize| {
    *next_key += 1;
    format!("w{w}-k{next_key}")
  };
  for _ in 0..rng.gen_range(1..=4) {
    let live: Vec<NodeId> = model.nodes.keys().copied().collect();
    match rng.gen_range(0..10) {
      // Create a batch, with props, labels and edges.
      0..=2 => {
        let keys: Vec<String> = (0..rng.gen_range(1..=12))
          .map(|_| new_key(next_key))
          .collect();
        let key_refs: Vec<Option<&str>> = keys.iter().map(|key| Some(key.as_str())).collect();
        let ids = db.create_nodes_batch(&key_refs).expect("create batch");
        for (&id, key) in ids.iter().zip(&keys) {
          let mut node = NodeModel {
            key: key.clone(),
            ..NodeModel::default()
          };
          if rng.gen_bool(0.5) {
            let prop = props[rng.gen_range(0..props.len())];
            let value = rng.gen_range(0..1000);
            db.set_node_prop(id, prop, PropValue::I64(value))
              .expect("prop");
            node.props.insert(prop, value);
          }
          if rng.gen_bool(0.3) {
            let label = labels[rng.gen_range(0..labels.len())];
            db.add_node_label(id, label).expect("label");
            node.labels.insert(label);
          }
          model.nodes.insert(id, node);
        }
        let targets: Vec<NodeId> = model.nodes.keys().copied().collect();
        for &src in &ids {
          let dst = targets[rng.gen_range(0..targets.len())];
          let etype = etypes[rng.gen_range(0..etypes.len())];
          if src != dst && used_edges.insert((src, etype, dst)) {
            db.add_edge(src, etype, dst).expect("edge");
            let mut edge_props = BTreeMap::new();
            if rng.gen_bool(0.5) {
              let prop = props[rng.gen_range(0..props.len())];
              let value = rng.gen_range(0..1000);
              db.set_edge_prop(src, etype, dst, prop, PropValue::I64(value))
                .expect("edge prop");
              edge_props.insert(prop, value);
            }
            model.edges.insert((src, etype, dst), edge_props);
          }
        }
      }
      // Delete a node (its edges go with it).
      3 if !live.is_empty() => {
        let id = live[rng.gen_range(0..live.len())];
        db.delete_node(id).expect("delete");
        model.remove_node(id);
        deleted.push(id);
      }
      // Recreate a deleted id, possibly one deleted in this transaction.
      4 if !deleted.is_empty() => {
        let id = deleted.swap_remove(rng.gen_range(0..deleted.len()));
        let key = new_key(next_key);
        db.create_node_with_id(id, Some(&key)).expect("recreate");
        let mut node = NodeModel {
          key,
          ..NodeModel::default()
        };
        if rng.gen_bool(0.5) {
          let prop = props[rng.gen_range(0..props.len())];
          db.set_node_prop(id, prop, PropValue::I64(7)).expect("prop");
          node.props.insert(prop, 7);
        }
        model.nodes.insert(id, node);
      }
      // Set or delete a node prop.
      5 if !live.is_empty() => {
        let id = live[rng.gen_range(0..live.len())];
        let prop = props[rng.gen_range(0..props.len())];
        let node = model.nodes.get_mut(&id).expect("live node");
        if rng.gen_bool(0.7) {
          let value = rng.gen_range(0..1000);
          db.set_node_prop(id, prop, PropValue::I64(value))
            .expect("prop");
          node.props.insert(prop, value);
        } else {
          db.delete_node_prop(id, prop).expect("delete prop");
          node.props.remove(&prop);
        }
      }
      // Add or remove a label.
      6 if !live.is_empty() => {
        let id = live[rng.gen_range(0..live.len())];
        let label = labels[rng.gen_range(0..labels.len())];
        let node = model.nodes.get_mut(&id).expect("live node");
        if rng.gen_bool(0.6) {
          db.add_node_label(id, label).expect("label");
          node.labels.insert(label);
        } else {
          db.remove_node_label(id, label).expect("unlabel");
          node.labels.remove(&label);
        }
      }
      // Add an edge between live nodes.
      7 if !live.is_empty() => {
        let src = live[rng.gen_range(0..live.len())];
        let dst = live[rng.gen_range(0..live.len())];
        let etype = etypes[rng.gen_range(0..etypes.len())];
        if src != dst && used_edges.insert((src, etype, dst)) {
          db.add_edge(src, etype, dst).expect("edge");
          model.edges.insert((src, etype, dst), BTreeMap::new());
        }
      }
      // Delete an edge, or set one of its props.
      8 | 9 if !model.edges.is_empty() => {
        let edges: Vec<EdgeKey> = model.edges.keys().copied().collect();
        let (src, etype, dst) = edges[rng.gen_range(0..edges.len())];
        if rng.gen_bool(0.4) {
          db.delete_edge(src, etype, dst).expect("delete edge");
          model.edges.remove(&(src, etype, dst));
        } else {
          let prop = props[rng.gen_range(0..props.len())];
          let value = rng.gen_range(0..1000);
          db.set_edge_prop(src, etype, dst, prop, PropValue::I64(value))
            .expect("edge prop");
          model
            .edges
            .get_mut(&(src, etype, dst))
            .expect("live edge")
            .insert(prop, value);
        }
      }
      _ => {}
    }
  }
  {
    let mut history = history.lock().expect("history");
    history.ids.extend(model.nodes.keys().copied());
    history
      .keys
      .extend(model.nodes.values().map(|node| node.key.clone()));
    history.states.push(Arc::new(model.view()));
  }
  db.commit().expect("commit writer");
}

/// The states of `history` a reader's `view` matches.
fn candidates(states: &[Arc<PartView>], view: &PartView) -> BTreeSet<usize> {
  states
    .iter()
    .enumerate()
    .filter(|(_, state)| state.listed == view.listed && ***state == *view)
    .map(|(k, _)| k)
    .collect()
}

/// Readers that begin at random points while three writers commit random
/// transactions (creates, deletes, recreates by id, props, labels, edges)
/// each see, for every writer, the state after one of its commits, and the
/// same one for their whole transaction, through every kind of read, while
/// background checkpoints and GC run.
#[test]
fn readers_match_a_reference_model_under_concurrent_commits() {
  const WRITERS: usize = 3;
  const READERS: usize = 3;
  const TRANSACTIONS: usize = 300;
  let dir = tempdir().expect("tempdir");
  let db = open(&dir);
  db.begin(false).expect("begin schema");
  let props: Vec<PropKeyId> = (0..3)
    .map(|i| db.define_propkey(&format!("p{i}")).expect("propkey"))
    .collect();
  let labels: Vec<LabelId> = (0..2)
    .map(|i| db.define_label(&format!("L{i}")).expect("label"))
    .collect();
  let etypes: Vec<ETypeId> = (0..2)
    .map(|i| db.define_etype(&format!("T{i}")).expect("etype"))
    .collect();
  db.commit().expect("commit schema");

  let histories: Arc<Vec<Mutex<PartHistory>>> = Arc::new(
    (0..WRITERS)
      .map(|_| {
        Mutex::new(PartHistory {
          states: vec![Arc::new(PartModel::default().view())],
          ..PartHistory::default()
        })
      })
      .collect(),
  );
  let writing = Arc::new(AtomicBool::new(true));
  let checks = Arc::new(AtomicUsize::new(0));
  let deadline = Instant::now() + Duration::from_secs(60);

  std::thread::scope(|scope| {
    let mut writers = Vec::new();
    for w in 0..WRITERS {
      let (db, histories) = (Arc::clone(&db), Arc::clone(&histories));
      let (props, labels, etypes) = (props.clone(), labels.clone(), etypes.clone());
      writers.push(scope.spawn(move || {
        let mut rng = StdRng::seed_from_u64(0x5eed_0000 + w as u64);
        let mut model = PartModel::default();
        let (mut next_key, mut used_edges, mut deleted) = (0, BTreeSet::new(), Vec::new());
        for _ in 0..TRANSACTIONS {
          // Room for readers to begin between commits, not only during them.
          if rng.gen_bool(0.8) {
            std::thread::sleep(Duration::from_micros(rng.gen_range(0..3000)));
          }
          random_transaction(
            &db,
            &mut rng,
            w,
            &mut model,
            &histories[w],
            &mut next_key,
            &mut used_edges,
            &mut deleted,
            &props,
            &labels,
            &etypes,
          );
        }
      }));
    }

    for r in 0..READERS {
      let (db, histories) = (Arc::clone(&db), Arc::clone(&histories));
      let (writing, checks, labels) = (Arc::clone(&writing), Arc::clone(&checks), labels.clone());
      scope.spawn(move || {
        let mut rng = StdRng::seed_from_u64(0xbead_0000 + r as u64);
        while writing.load(Ordering::Acquire) && Instant::now() < deadline {
          db.begin(true).expect("begin reader");
          let mut cands: Vec<Option<BTreeSet<usize>>> = vec![None; WRITERS];
          for read in 0..rng.gen_range(2..=4) {
            let listed: BTreeSet<NodeId> = db.list_nodes().into_iter().collect();
            let count_nodes = db.count_nodes();
            let (listed_edges, count_edges) = (db.list_edges(None).len(), db.count_edges());
            let (mut total, mut total_edges) = (0, 0);
            for (w, history) in histories.iter().enumerate() {
              let (ids, keys, states) = {
                let history = history.lock().expect("history");
                (
                  history.ids.clone(),
                  history.keys.clone(),
                  history.states.clone(),
                )
              };
              let view = read_part(&db, &ids, &keys, &labels, &listed);
              total += view.listed.len();
              total_edges += view.edges.len();
              let found = candidates(&states, &view);
              let kept: BTreeSet<usize> = match &cands[w] {
                Some(previous) => previous.intersection(&found).copied().collect(),
                None => found.clone(),
              };
              assert!(
                !kept.is_empty(),
                "reader {r}, read {read}: writer {w}'s part matches no single commit \
                 (this read matches {found:?}, the earlier ones {:?})\nview: {view:?}",
                cands[w]
              );
              cands[w] = Some(kept);
            }
            assert_eq!(
              listed.len(),
              total,
              "reader {r}: list_nodes lists nodes outside the writers' parts"
            );
            assert_eq!(
              count_nodes,
              listed.len(),
              "reader {r}: count_nodes != list_nodes"
            );
            assert_eq!(
              (listed_edges, count_edges),
              (total_edges, total_edges),
              "reader {r}: list_edges / count_edges disagree with the parts' edges"
            );
            checks.fetch_add(1, Ordering::Relaxed);
            if rng.gen_bool(0.5) {
              std::thread::sleep(Duration::from_micros(rng.gen_range(0..1500)));
            }
          }
          db.rollback().expect("end reader");
        }
      });
    }

    // Background checkpoints and GC.
    {
      let (db, writing) = (Arc::clone(&db), Arc::clone(&writing));
      scope.spawn(move || {
        let mut rounds = 0;
        while writing.load(Ordering::Acquire) && Instant::now() < deadline {
          std::thread::sleep(Duration::from_millis(15));
          rounds += 1;
          if rounds % 2 == 0 {
            match db.background_checkpoint() {
              Ok(()) | Err(crate::error::KiteError::CheckpointDeclined(_)) => {}
              Err(error) => panic!("background checkpoint: {error}"),
            }
          }
          db.mvcc.as_ref().expect("mvcc").run_gc();
        }
      });
    }

    for writer in writers {
      writer.join().expect("writer");
    }
    writing.store(false, Ordering::Release);
  });

  assert!(Instant::now() < deadline, "the test ran out of time");
  assert!(
    checks.load(Ordering::Relaxed) >= 100,
    "readers checked too few snapshots: {}",
    checks.load(Ordering::Relaxed)
  );
  // Every commit is visible once every reader is gone.
  for (w, history) in histories.iter().enumerate() {
    let history = history.lock().expect("history");
    let listed: BTreeSet<NodeId> = db.list_nodes().into_iter().collect();
    let view = read_part(&db, &history.ids, &history.keys, &labels, &listed);
    assert_eq!(
      Some(&view),
      history.states.last().map(|state| &**state),
      "writer {w}'s last commit"
    );
  }
}
