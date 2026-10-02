//! Recording commits in the MVCC version chains.
//!
//! The chains hold history (see `mvcc::version_chain`): for the transactions still open when
//! a commit lands, the state of every key it changes from before the commit. That includes
//! what a change implies: deleting a node also removes its key, props, labels and incident
//! edges, and deleting an edge also removes its props. The state before the commit is the
//! committed delta over the snapshot, which the commit has not been merged into yet.
//!
//! A node the commit brings into existence where no open snapshot can see that id (it is not
//! committed, and has no history of an earlier node) is "fresh": it gets no chain, only the
//! commit, as part of a run of consecutive ids (`VersionChainManager::record_node_creations`).
//! Every read of a node's props, labels and key, and of an edge, checks that the node (each
//! endpoint) exists at the reader's snapshot first, so its props, labels, key and edges need
//! no history either.
//!
//! Recording takes two steps. `HistoryPlan::of` works out, without any lock a commit holds,
//! which nodes are fresh and so what is left to record: nothing else for a commit that only
//! creates nodes and their edges. `record_commit` then records the plan in the publish.

use std::collections::HashMap;
use std::sync::Arc;

use crate::core::snapshot::reader::SnapshotData;
use crate::mvcc::VersionChainManager;
use crate::types::*;

type Edge = (NodeId, ETypeId, NodeId);

/// What recording a commit's history takes, worked out before its publish (see the module
/// docs).
#[derive(Debug, Default)]
pub(super) struct HistoryPlan {
  /// The fresh nodes it creates, as runs `[start, end)` of consecutive ids.
  fresh_runs: Vec<(NodeId, NodeId)>,
  /// The other nodes it creates (recreates of an id with history, or of one it deletes).
  created: Vec<NodeId>,
  /// The edges it adds that have no fresh endpoint.
  added_edges: Vec<Edge>,
  /// The edges whose props it changes that have no fresh endpoint.
  changed_edges: Vec<Edge>,
}

impl HistoryPlan {
  /// The plan for the changes `pending` makes to the committed state `delta` over
  /// `snapshot`, with history `vc`.
  ///
  /// It holds until the commit's publish, also when worked out before the commit's group is
  /// written: until then only commits that write one of `pending`'s nodes could make one of
  /// them committed or give it history (they create or delete it), and those conflict with it
  /// (MVCC aborts one of the two; bulk loads run alone). GC may drop a chain meanwhile, which
  /// only makes the publish record a node in full that it could have recorded as fresh.
  pub(super) fn of(
    vc: &VersionChainManager,
    delta: &DeltaState,
    snapshot: Option<&SnapshotData>,
    pending: &DeltaState,
  ) -> Self {
    let committed = Committed { delta, snapshot };
    let mut fresh = Vec::new();
    let mut created = Vec::new();
    for &node_id in pending.created_nodes.keys() {
      if !committed.exists(node_id) && !vc.has_node_history(node_id) {
        fresh.push(node_id);
      } else {
        created.push(node_id);
      }
    }
    fresh.sort_unstable();
    let mut fresh_runs: Vec<(NodeId, NodeId)> = Vec::new();
    for &node_id in &fresh {
      match fresh_runs.last_mut() {
        Some((_, end)) if *end == node_id => *end += 1,
        _ => fresh_runs.push((node_id, node_id + 1)),
      }
    }
    let is_fresh = |node_id: NodeId| fresh.binary_search(&node_id).is_ok();
    let edge_is_fresh = |&(src, _, dst): &Edge| is_fresh(src) || is_fresh(dst);
    let added_edges = pending
      .out_add
      .iter()
      .flat_map(|(&src, patches)| {
        patches
          .iter()
          .map(move |patch| (src, patch.etype, patch.other))
      })
      .filter(|edge| !edge_is_fresh(edge))
      .collect();
    let changed_edges = pending
      .edge_props
      .keys()
      .copied()
      .filter(|edge| !edge_is_fresh(edge))
      .collect();
    Self {
      fresh_runs,
      created,
      added_edges,
      changed_edges,
    }
  }
}

/// Record in `vc` the changes `pending` makes, committed by `txid` at `commit_ts`, to the
/// committed state `delta` over `snapshot`, as `plan` (`HistoryPlan::of` for them) lays out.
pub(super) fn record_commit(
  vc: &mut VersionChainManager,
  delta: &DeltaState,
  snapshot: Option<&SnapshotData>,
  pending: &DeltaState,
  plan: &HistoryPlan,
  txid: TxId,
  commit_ts: Timestamp,
) {
  CommitRecorder {
    vc,
    committed: Committed { delta, snapshot },
    txid,
    commit_ts,
  }
  .record(pending, plan);
}

struct CommitRecorder<'a> {
  vc: &'a mut VersionChainManager,
  committed: Committed<'a>,
  txid: TxId,
  commit_ts: Timestamp,
}

impl CommitRecorder<'_> {
  fn record(&mut self, pending: &DeltaState, plan: &HistoryPlan) {
    let (txid, commit_ts) = (self.txid, self.commit_ts);
    self
      .vc
      .record_node_creations(&plan.fresh_runs, txid, commit_ts);

    // Removals first. A later change of the same key by this commit (a node deleted and
    // re-created, an edge re-added to a re-created node) replaces what they record.
    for &node_id in &pending.deleted_nodes {
      self.remove_node(node_id);
    }
    for (&src, patches) in &pending.out_del {
      for patch in patches {
        self.remove_edge(src, patch.etype, patch.other);
      }
    }

    let created = plan
      .created
      .iter()
      .filter_map(|node_id| pending.created_nodes.get_key_value(node_id));
    for (&node_id, node_delta) in created.clone() {
      if let Some(key) = node_delta.key.as_deref() {
        let owner = self.committed.key_owner(key);
        self
          .vc
          .record_key_owner(key, owner, Some(node_id), txid, commit_ts);
      }
      let created = NodeVersionData {
        node_id,
        delta: node_delta.for_version(),
      };
      let before = self.committed.node(node_id);
      self
        .vc
        .record_node(node_id, before, Some(created), txid, commit_ts);
    }
    for &(src, etype, dst) in &plan.added_edges {
      self.add_edge(src, etype, dst);
    }

    for (&node_id, node_delta) in created.chain(&pending.modified_nodes) {
      for (&key_id, after) in node_delta.props.iter().flatten() {
        let before = self.committed.node_prop(node_id, key_id);
        self
          .vc
          .record_node_prop(node_id, key_id, before, after.clone(), txid, commit_ts);
      }
      for &label_id in node_delta.labels.iter().flatten() {
        let before = self.committed.has_label(node_id, label_id);
        self
          .vc
          .record_node_label(node_id, label_id, before, true, txid, commit_ts);
      }
      for &label_id in node_delta.labels_deleted.iter().flatten() {
        let before = self.committed.has_label(node_id, label_id);
        self
          .vc
          .record_node_label(node_id, label_id, before, false, txid, commit_ts);
      }
    }
    let changed_edges = plan
      .changed_edges
      .iter()
      .filter_map(|edge| Some((*edge, pending.edge_props.get(edge)?)));
    for ((src, etype, dst), props) in changed_edges {
      for (&key_id, after) in props {
        let before = self.committed.edge_prop(src, etype, dst, key_id);
        self.vc.record_edge_prop(
          src,
          etype,
          dst,
          key_id,
          before,
          after.clone(),
          txid,
          commit_ts,
        );
      }
    }
  }

  /// Record the removal of node `node_id` with its key, props, labels and edges.
  fn remove_node(&mut self, node_id: NodeId) {
    let (txid, commit_ts) = (self.txid, self.commit_ts);
    let Some(node) = self.committed.node(node_id) else {
      return;
    };
    if let Some(key) = node.delta.key.as_deref() {
      self
        .vc
        .record_key_owner(key, Some(node_id), None, txid, commit_ts);
    }
    for (key_id, value) in self.committed.node_props(node_id) {
      self
        .vc
        .record_node_prop(node_id, key_id, Some(value), None, txid, commit_ts);
    }
    for label_id in self.committed.node_labels(node_id) {
      self
        .vc
        .record_node_label(node_id, label_id, true, false, txid, commit_ts);
    }
    for (src, etype, dst) in self.committed.incident_edges(node_id) {
      self.remove_edge(src, etype, dst);
    }
    self
      .vc
      .record_node(node_id, Some(node), None, txid, commit_ts);
  }

  /// Record the addition of an edge. An edge re-added before a checkpoint gets back the
  /// props the delta and snapshot still hold for it.
  fn add_edge(&mut self, src: NodeId, etype: ETypeId, dst: NodeId) {
    let (txid, commit_ts) = (self.txid, self.commit_ts);
    if self.committed.edge_exists(src, etype, dst) {
      return;
    }
    for (key_id, value) in self.committed.stored_edge_props(src, etype, dst) {
      self
        .vc
        .record_edge_prop(src, etype, dst, key_id, None, Some(value), txid, commit_ts);
    }
    self
      .vc
      .record_edge(src, etype, dst, false, true, txid, commit_ts);
  }

  /// Record the removal of an edge with its props.
  fn remove_edge(&mut self, src: NodeId, etype: ETypeId, dst: NodeId) {
    let (txid, commit_ts) = (self.txid, self.commit_ts);
    if !self.committed.edge_exists(src, etype, dst) {
      return;
    }
    for (key_id, value) in self.committed.edge_props(src, etype, dst) {
      self
        .vc
        .record_edge_prop(src, etype, dst, key_id, Some(value), None, txid, commit_ts);
    }
    self
      .vc
      .record_edge(src, etype, dst, true, false, txid, commit_ts);
  }
}

/// Commits creating fewer nodes, edges and edge props than this leave their history plan to
/// their publish (see `SingleFileDB::plan_history`).
const EARLY_PLAN_MIN_CHANGES: usize = 16;

impl super::SingleFileDB {
  /// The history plan of a commit of `pending` (see `HistoryPlan::of`), worked out by its
  /// committer before it queues, without any lock a commit group holds: when another
  /// transaction is open (so the commit records history) and the commit creates enough for
  /// the plan to matter. `None` leaves the plan to the publish.
  pub(super) fn plan_history(&self, pending: &DeltaState) -> Option<HistoryPlan> {
    let mvcc = self.mvcc.as_ref()?;
    let changes = pending.created_nodes.len() + pending.out_add.len() + pending.edge_props.len();
    if changes < EARLY_PLAN_MIN_CHANGES
      || self
        .active_transactions
        .load(std::sync::atomic::Ordering::Acquire)
        <= 1
    {
      return None;
    }
    // Lock order: see read.rs.
    let delta = self.delta.read();
    let snapshot = self.snapshot.read();
    let vc = mvcc.version_chain.read();
    Some(HistoryPlan::of(&vc, &delta, snapshot.as_ref(), pending))
  }
}

/// The committed state: the delta over the snapshot, as reads see it.
///
/// A node the delta created is the delta's own copy, and its state is the delta's alone,
/// also when it re-creates a deleted id: the delete masks the snapshot copy (its props,
/// labels, key and edges) and the re-created node starts fresh.
struct Committed<'a> {
  delta: &'a DeltaState,
  snapshot: Option<&'a SnapshotData>,
}

impl Committed<'_> {
  /// Whether the node exists: the delta's own copy, or a snapshot copy the delta kept.
  fn exists(&self, node_id: NodeId) -> bool {
    self.delta.is_node_created(node_id) || self.snapshot_node(node_id).is_some()
  }

  /// Node `node_id` with its key, if it exists.
  fn node(&self, node_id: NodeId) -> Option<NodeVersionData> {
    if !self.exists(node_id) {
      return None;
    }
    let key = match self.delta.created_nodes.get(&node_id) {
      Some(node_delta) => node_delta.key.clone(),
      None => self
        .snapshot_node(node_id)
        .and_then(|(snap, phys)| snap.node_key(phys)),
    };
    Some(NodeVersionData {
      node_id,
      delta: NodeDelta {
        key,
        ..NodeDelta::default()
      },
    })
  }

  /// The live node holding `key`.
  fn key_owner(&self, key: &str) -> Option<NodeId> {
    if let Some(&node_id) = self.delta.key_index.get(key) {
      if self.exists(node_id) {
        return Some(node_id);
      }
    }
    self
      .snapshot?
      .lookup_by_key(key)
      .filter(|&node_id| self.snapshot_node(node_id).is_some())
  }

  /// The snapshot's copy of node `node_id`, unless the delta deleted it or holds its own.
  fn snapshot_node(&self, node_id: NodeId) -> Option<(&SnapshotData, PhysNode)> {
    if self.delta.is_node_created(node_id) || self.delta.is_node_deleted(node_id) {
      return None;
    }
    let snap = self.snapshot?;
    Some((snap, snap.phys_node(node_id)?))
  }

  fn node_prop(&self, node_id: NodeId, key_id: PropKeyId) -> Option<PropValueRef> {
    if !self.exists(node_id) {
      return None;
    }
    let delta_props = self
      .delta
      .node_delta(node_id)
      .and_then(|node_delta| node_delta.props.as_ref());
    if let Some(value) = delta_props.and_then(|props| props.get(&key_id)) {
      return value.clone();
    }
    let (snap, phys) = self.snapshot_node(node_id)?;
    snap.node_prop(phys, key_id).map(Arc::new)
  }

  fn node_props(&self, node_id: NodeId) -> HashMap<PropKeyId, PropValueRef> {
    let mut props = HashMap::new();
    if !self.exists(node_id) {
      return props;
    }
    if let Some((snap, phys)) = self.snapshot_node(node_id) {
      for (key_id, value) in snap.node_props(phys).unwrap_or_default() {
        props.insert(key_id, Arc::new(value));
      }
    }
    let delta_props = self
      .delta
      .node_delta(node_id)
      .and_then(|node_delta| node_delta.props.as_ref());
    for (&key_id, value) in delta_props.into_iter().flatten() {
      match value {
        Some(value) => props.insert(key_id, value.clone()),
        None => props.remove(&key_id),
      };
    }
    props
  }

  fn has_label(&self, node_id: NodeId, label_id: LabelId) -> bool {
    if !self.exists(node_id) || self.delta.is_label_removed(node_id, label_id) {
      return false;
    }
    if self.delta.is_label_added(node_id, label_id) {
      return true;
    }
    self
      .snapshot_node(node_id)
      .and_then(|(snap, phys)| snap.node_labels(phys))
      .is_some_and(|labels| labels.contains(&label_id))
  }

  fn node_labels(&self, node_id: NodeId) -> Vec<LabelId> {
    if !self.exists(node_id) {
      return Vec::new();
    }
    let mut labels = self
      .snapshot_node(node_id)
      .and_then(|(snap, phys)| snap.node_labels(phys))
      .unwrap_or_default();
    if let Some(removed) = self.delta.removed_labels(node_id) {
      labels.retain(|label_id| !removed.contains(label_id));
    }
    for &label_id in self.delta.added_labels(node_id).into_iter().flatten() {
      if !labels.contains(&label_id) {
        labels.push(label_id);
      }
    }
    labels
  }

  /// Whether the edge exists: both endpoints do, and the delta added it or the snapshot
  /// holds it between copies the delta kept.
  fn edge_exists(&self, src: NodeId, etype: ETypeId, dst: NodeId) -> bool {
    if !self.exists(src) || !self.exists(dst) {
      return false;
    }
    self.delta.is_edge_added(src, etype, dst)
      || (!self.delta.is_edge_deleted(src, etype, dst)
        && self.snapshot_edge_index(src, etype, dst).is_some())
  }

  fn snapshot_edge_index(
    &self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
  ) -> Option<(&SnapshotData, usize)> {
    let (snap, src_phys) = self.snapshot_node(src)?;
    let (_, dst_phys) = self.snapshot_node(dst)?;
    Some((snap, snap.find_edge_index(src_phys, etype, dst_phys)?))
  }

  fn edge_prop(
    &self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    key_id: PropKeyId,
  ) -> Option<PropValueRef> {
    if !self.edge_exists(src, etype, dst) {
      return None;
    }
    let delta_props = self.delta.edge_props_delta(src, etype, dst);
    if let Some(value) = delta_props.and_then(|props| props.get(&key_id)) {
      return value.clone();
    }
    let (snap, edge_idx) = self.snapshot_edge_index(src, etype, dst)?;
    snap.edge_props(edge_idx)?.remove(&key_id).map(Arc::new)
  }

  fn edge_props(
    &self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
  ) -> HashMap<PropKeyId, PropValueRef> {
    if !self.edge_exists(src, etype, dst) {
      return HashMap::new();
    }
    self.stored_edge_props(src, etype, dst)
  }

  /// The props the delta and snapshot hold for an edge, whether or not it exists.
  fn stored_edge_props(
    &self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
  ) -> HashMap<PropKeyId, PropValueRef> {
    let mut props = HashMap::new();
    if let Some((snap, edge_idx)) = self.snapshot_edge_index(src, etype, dst) {
      for (key_id, value) in snap.edge_props(edge_idx).unwrap_or_default() {
        props.insert(key_id, Arc::new(value));
      }
    }
    let delta_props = self.delta.edge_props_delta(src, etype, dst);
    for (&key_id, value) in delta_props.into_iter().flatten() {
      match value {
        Some(value) => props.insert(key_id, value.clone()),
        None => props.remove(&key_id),
      };
    }
    props
  }

  /// The existing edges `(src, etype, dst)` with endpoint `node_id`.
  fn incident_edges(&self, node_id: NodeId) -> Vec<(NodeId, ETypeId, NodeId)> {
    let mut edges = Vec::new();
    if let Some((snap, phys)) = self.snapshot_node(node_id) {
      for (dst_phys, etype) in snap.iter_out_edges(phys) {
        edges.extend(snap.node_id(dst_phys).map(|dst| (node_id, etype, dst)));
      }
      for (src_phys, etype, _) in snap.iter_in_edges(phys) {
        edges.extend(snap.node_id(src_phys).map(|src| (src, etype, node_id)));
      }
    }
    for patch in self.delta.out_add.get(&node_id).into_iter().flatten() {
      edges.push((node_id, patch.etype, patch.other));
    }
    for patch in self.delta.in_add.get(&node_id).into_iter().flatten() {
      edges.push((patch.other, patch.etype, node_id));
    }
    edges.sort_unstable();
    edges.dedup();
    edges.retain(|&(src, etype, dst)| self.edge_exists(src, etype, dst));
    edges
  }
}

#[cfg(test)]
mod tests {
  use std::sync::mpsc;
  use std::thread;

  use crate::core::single_file::{
    close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions,
  };
  use crate::types::PropValue;

  fn open(dir: &tempfile::TempDir) -> SingleFileDB {
    let options = SingleFileOpenOptions::new()
      .mvcc(true)
      .mvcc_gc_interval_ms(20)
      .mvcc_retention_ms(60 * 60 * 1000)
      .auto_checkpoint(false);
    open_single_file(dir.path().join("history.kitedb"), options).expect("open")
  }

  fn write(db: &SingleFileDB, ops: impl FnOnce(&SingleFileDB)) {
    db.begin(false).expect("begin");
    ops(db);
    db.commit().expect("commit");
  }

  /// Runs `read` in a transaction on another thread before and after `write`.
  fn read_around<T: Send>(
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
        ready_tx.send(()).expect("ready");
        go_rx.recv().expect("go");
        let after = read(db);
        db.commit().expect("end reader");
        (before, after)
      });
      ready_rx.recv().expect("reader started");
      write();
      go_tx.send(()).expect("go");
      reader.join().expect("reader")
    })
  }

  fn i64v(value: i64) -> Option<PropValue> {
    Some(PropValue::I64(value))
  }

  /// A commit with no other transaction open records nothing. A reader that begins after it
  /// must still see it once a later commit records the key's history.
  #[test]
  fn reader_sees_unrecorded_commit_once_the_key_gets_history() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = open(&dir);
    let mut ids = None;
    write(&db, |db| {
      let node = db.create_node(Some("n")).expect("node");
      let prop = db.define_propkey("p").expect("propkey");
      db.set_node_prop(node, prop, PropValue::I64(0))
        .expect("prop");
      ids = Some((node, prop));
    });
    let (node, prop) = ids.expect("ids");
    let set = |value| {
      write(&db, |db| {
        db.set_node_prop(node, prop, PropValue::I64(value))
          .expect("set")
      })
    };

    // Recorded (a reader is open), then unrecorded (alone).
    read_around(&db, |_| (), || set(1));
    set(2);

    let (before, after) = read_around(&db, |db| db.node_prop(node, prop), || set(3));
    assert_eq!((before, after), (i64v(2), i64v(2)));
    assert_eq!(db.node_prop(node, prop), i64v(3));
    close_single_file(db).expect("close");
  }

  /// A reader keeps the old node when another transaction deletes it and re-creates the id
  /// in one commit.
  #[test]
  fn reader_keeps_node_deleted_and_recreated_in_one_commit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = open(&dir);
    let mut ids = None;
    write(&db, |db| {
      let node = db.create_node(Some("old")).expect("node");
      let other = db.create_node(None).expect("other");
      let prop = db.define_propkey("p").expect("propkey");
      let label = db.define_label("L").expect("label");
      let etype = db.define_etype("T").expect("etype");
      db.set_node_prop(node, prop, PropValue::I64(7))
        .expect("prop");
      db.add_node_label(node, label).expect("label");
      db.add_edge(node, etype, other).expect("edge");
      ids = Some((node, other, prop, label, etype));
    });
    let (node, other, prop, label, etype) = ids.expect("ids");

    let (before, after) = read_around(
      &db,
      |db| {
        (
          db.node_key(node),
          db.node_by_key("old"),
          db.node_prop(node, prop),
          db.node_labels(node),
          db.out_edges(node),
          db.edge_exists(node, etype, other),
        )
      },
      || {
        write(&db, |db| {
          db.delete_node(node).expect("delete");
          db.create_node_with_id(node, Some("new"))
            .expect("re-create");
        })
      },
    );

    assert_eq!(after, before);
    assert_eq!(
      before,
      (
        Some("old".to_string()),
        Some(node),
        i64v(7),
        vec![label],
        vec![(etype, other)],
        true
      )
    );
    close_single_file(db).expect("close");
  }

  /// An edge re-added before a checkpoint gets back the props the delta still holds; a
  /// reader that began after the re-add sees them when a later commit changes one.
  #[test]
  fn reader_sees_props_of_readded_edge() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = open(&dir);
    let mut ids = None;
    write(&db, |db| {
      let a = db.create_node(None).expect("a");
      let b = db.create_node(None).expect("b");
      let etype = db.define_etype("T").expect("etype");
      let prop = db.define_propkey("w").expect("propkey");
      db.add_edge(a, etype, b).expect("edge");
      db.set_edge_prop(a, etype, b, prop, PropValue::I64(5))
        .expect("prop");
      ids = Some((a, b, etype, prop));
    });
    let (a, b, etype, prop) = ids.expect("ids");

    read_around(
      &db,
      |_| (),
      || write(&db, |db| db.delete_edge(a, etype, b).expect("delete")),
    );
    write(&db, |db| db.add_edge(a, etype, b).expect("re-add"));
    let current = db.edge_prop(a, etype, b, prop);

    let (before, after) = read_around(
      &db,
      |db| db.edge_prop(a, etype, b, prop),
      || {
        write(&db, |db| {
          db.set_edge_prop(a, etype, b, prop, PropValue::I64(6))
            .expect("set")
        })
      },
    );
    assert_eq!((before, after), (current.clone(), current));
    assert_eq!(db.edge_prop(a, etype, b, prop), i64v(6));
    close_single_file(db).expect("close");
  }
}

/// Chain visibility after `record_commit`, on hand-built committed and pending deltas.
#[cfg(test)]
mod recorder_tests {
  use std::sync::Arc;

  use super::{record_commit, HistoryPlan};
  use crate::mvcc::VersionChainManager;
  use crate::types::*;

  const N: NodeId = 1;
  const M: NodeId = 2;
  const T: ETypeId = 1;
  const OLD_PROP: PropKeyId = 1;
  const NEW_PROP: PropKeyId = 2;
  const OLD_LABEL: LabelId = 1;
  const NEW_LABEL: LabelId = 2;
  const COMMIT_TS: Timestamp = 10;
  /// A reader whose snapshot predates the commit, and one that sees it.
  const OLD: Timestamp = COMMIT_TS;
  const NEW: Timestamp = COMMIT_TS + 1;
  const READER: TxId = 999;

  fn value(value: i64) -> Option<PropValueRef> {
    Some(Arc::new(PropValue::I64(value)))
  }

  /// Node N (key "old", OLD_PROP 1, OLD_LABEL, edge N -T-> M with OLD_PROP 2) and node M.
  fn committed() -> DeltaState {
    let mut delta = DeltaState::new();
    delta.create_node(N, Some("old"));
    delta.create_node(M, None);
    delta.set_node_prop(N, OLD_PROP, PropValue::I64(1));
    delta.add_node_label(N, OLD_LABEL);
    delta.add_edge(N, T, M);
    delta.set_edge_prop(N, T, M, OLD_PROP, PropValue::I64(2));
    delta
  }

  fn record(delta: &DeltaState, pending: &DeltaState) -> VersionChainManager {
    let mut vc = VersionChainManager::new();
    let plan = HistoryPlan::of(&vc, delta, None, pending);
    record_commit(&mut vc, delta, None, pending, &plan, 5, COMMIT_TS);
    vc
  }

  fn key_at(vc: &VersionChainManager, ts: Timestamp) -> Option<Option<String>> {
    vc.node_at(N, ts, READER)
      .map(|node| node.and_then(|node| node.delta.key.clone()))
  }

  /// One transaction deletes N and creates the id again: N exists afterwards, as the new
  /// node only. Older readers keep the old node.
  #[test]
  fn delete_then_create_in_one_commit_ends_with_the_new_node() {
    let mut pending = DeltaState::new();
    pending.delete_node(N);
    pending.create_node(N, Some("new"));
    pending.set_node_prop(N, NEW_PROP, PropValue::I64(3));
    pending.add_node_label(N, NEW_LABEL);
    let vc = record(&committed(), &pending);

    assert_eq!(key_at(&vc, OLD), Some(Some("old".to_string())));
    assert_eq!(vc.node_prop_at(N, OLD_PROP, OLD, READER), Some(value(1)));
    assert_eq!(vc.node_prop_at(N, NEW_PROP, OLD, READER), Some(None));
    assert_eq!(vc.node_label_at(N, OLD_LABEL, OLD, READER), Some(true));
    assert_eq!(vc.node_label_at(N, NEW_LABEL, OLD, READER), Some(false));
    assert_eq!(vc.edge_exists_at(N, T, M, OLD, READER), Some(true));
    assert_eq!(
      vc.edge_prop_at(N, T, M, OLD_PROP, OLD, READER),
      Some(value(2))
    );
    assert_eq!(vc.key_owner_at("old", OLD, READER), Some(Some(N)));
    assert_eq!(vc.key_owner_at("new", OLD, READER), Some(None));

    // Newer readers read the delta, and every newest version is the new node's state:
    // none of the old node's props, labels, key or edges carry over.
    assert_eq!(key_at(&vc, NEW), None);
    let head = vc.node_version(N).expect("node chain");
    assert!(
      !head.deleted,
      "the create, not the delete, is the final state"
    );
    assert_eq!(head.data.delta.key.as_deref(), Some("new"));
    let prop = |key_id| vc.node_prop_version(N, key_id).map(|head| head.data);
    assert_eq!(prop(OLD_PROP), Some(None));
    assert_eq!(prop(NEW_PROP), Some(value(3)));
    let label = |label_id| vc.node_label_version(N, label_id).map(|head| head.data);
    assert_eq!(label(OLD_LABEL), Some(None));
    assert_eq!(label(NEW_LABEL), Some(Some(true)));
    assert!(!vc.edge_version(N, T, M).expect("edge chain").data.added);
    let edge_prop = vc
      .edge_prop_version(N, T, M, OLD_PROP)
      .map(|head| head.data);
    assert_eq!(edge_prop, Some(None));
  }

  /// Delete, create and delete again in one transaction: N ends deleted.
  #[test]
  fn delete_create_delete_in_one_commit_ends_deleted() {
    let mut pending = DeltaState::new();
    pending.delete_node(N);
    pending.create_node(N, Some("new"));
    pending.delete_node(N);
    let vc = record(&committed(), &pending);

    assert_eq!(key_at(&vc, OLD), Some(Some("old".to_string())));
    assert!(vc.node_version(N).expect("node chain").deleted);
    assert_eq!(vc.key_owner_at("new", OLD, READER), None, "never held");
  }

  /// The committed state holds N re-created over its deleted id. A later commit records
  /// the new node's state as what it replaces, not the deleted node's.
  #[test]
  fn recreated_node_state_is_its_own() {
    let mut delta = DeltaState::new();
    delta.create_node(M, None);
    delta.deleted_nodes.insert(N);
    delta.create_node(N, Some("new"));
    delta.set_node_prop(N, NEW_PROP, PropValue::I64(3));
    delta.add_node_label(N, NEW_LABEL);
    delta.add_edge(N, T, M);

    let mut pending = DeltaState::new();
    pending.set_node_prop(N, NEW_PROP, PropValue::I64(4));
    pending.remove_node_label(N, NEW_LABEL);
    pending.delete_edge(N, T, M);
    let vc = record(&delta, &pending);

    assert_eq!(vc.node_prop_at(N, NEW_PROP, OLD, READER), Some(value(3)));
    assert_eq!(vc.node_label_at(N, NEW_LABEL, OLD, READER), Some(true));
    assert_eq!(vc.edge_exists_at(N, T, M, OLD, READER), Some(true));

    // Deleting it records the new node's key, not the deleted one's.
    let mut pending = DeltaState::new();
    pending.delete_node(N);
    let vc = record(&delta, &pending);
    assert_eq!(key_at(&vc, OLD), Some(Some("new".to_string())));
    assert_eq!(vc.key_owner_at("new", OLD, READER), Some(Some(N)));
    assert_eq!(vc.edge_exists_at(N, T, M, OLD, READER), Some(true));
  }
}

/// raydb-b4 `publish-cost` lane: history for created nodes, and readers
/// against a reference model.
#[cfg(test)]
#[path = "b4_publish_cost_tests.rs"]
mod b4_publish_cost_tests;
