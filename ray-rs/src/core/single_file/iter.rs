//! Node iteration, listings and statistics for SingleFileDB
//!
//! Full listings (`iter_nodes`, `list_edges`), listings that seek instead of
//! scanning (`nodes_after` / `edges_after` pages, nodes by label or key
//! prefix), counts, and database statistics. The seeking listings return what
//! the full listings would, from the same view; their cost follows what they
//! return plus the changes since the last checkpoint, not the graph.

use crate::core::delta::snapshot_has_edge;
use crate::types::*;
use std::collections::{BTreeSet, HashSet};
use std::ops::ControlFlow;

use super::read::{examined_edges, examined_nodes, merge_sorted, EdgeSide, NodeLayers, ReadView};
use super::SingleFileDB;

// ============================================================================
// Edge Types
// ============================================================================

/// Full edge with source, destination, and type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FullEdge {
  pub src: NodeId,
  pub etype: ETypeId,
  pub dst: NodeId,
}

// ============================================================================
// Node Iterator
// ============================================================================

/// Iterator over all nodes in the database
///
/// This iterator collects node IDs upfront to avoid holding locks during iteration.
/// For very large databases, page through them with `nodes_after()`.
pub struct NodeIterator {
  nodes: Vec<NodeId>,
  index: usize,
}

impl NodeIterator {
  pub(crate) fn new(db: &SingleFileDB) -> Self {
    let nodes = db.read_view(|view, _| view.listed_nodes());
    Self { nodes, index: 0 }
  }
}

impl ReadView<'_> {
  /// Every node the reader sees, sorted by ID: what `iter_nodes` lists.
  pub(super) fn listed_nodes(&self) -> Vec<NodeId> {
    let layers = self.layers;
    let mut nodes = Vec::new();

    // 1. Nodes from the snapshot (excluding deleted ones; a recreated node is
    // listed with the delta's nodes)
    if let Some(snap) = self.snapshot {
      let num_nodes = snap.header.num_nodes as u32;
      for phys in 0..num_nodes {
        examined_nodes(1);
        if let Some(node_id) = snap.node_id(phys) {
          if layers.sees_snapshot(node_id, self.node_mvcc(node_id)) {
            nodes.push(node_id);
          }
        }
      }
    }

    // 2. Nodes created in the delta, and those the transaction created or
    // recreated, and those deleted since the reader's snapshot (only their
    // version chains hold them)
    self.for_each_unsorted_node(|node_id, listed| {
      if listed(node_id) {
        nodes.push(node_id);
      }
    });

    nodes.sort_unstable();
    nodes.dedup();
    nodes
  }

  /// The nodes `listed_nodes` may list from the delta, the transaction and the
  /// version chains, in no particular order (some more than once), each with
  /// the check whether it lists it. They follow the changes since the last
  /// checkpoint, not the graph.
  fn for_each_unsorted_node(&self, mut f: impl FnMut(NodeId, &dyn Fn(NodeId) -> bool)) {
    let layers = self.layers;
    examined_nodes(layers.delta.created_nodes.len());
    let from_delta = |node_id| layers.sees_delta(node_id, self.node_mvcc(node_id));
    for &node_id in layers.delta.created_nodes.keys() {
      f(node_id, &from_delta);
    }
    if let Some(pending) = layers.pending {
      examined_nodes(pending.created_nodes.len());
      for &node_id in pending.created_nodes.keys() {
        f(node_id, &|_| true);
      }
    }
    if let Some(vc) = self.history {
      let from_history = |node_id| !layers.pending_masks(node_id);
      for node_id in vc.nodes_at(self.snapshot_ts, self.txid) {
        examined_nodes(1);
        f(node_id, &from_history);
      }
    }
  }

  /// The first `limit` nodes of `listed_nodes` after `after`.
  fn nodes_after(&self, after: Option<NodeId>, limit: usize) -> Vec<NodeId> {
    let layers = self.layers;
    // The first ID the page may hold (none past the largest)
    let Some(from) = after.map_or(Some(0), |after| after.checked_add(1)) else {
      return Vec::new();
    };

    // The nodes the delta and the transaction created (or recreated), in ID
    // order from the cursor on, each with the check `listed_nodes` makes
    let mut from_delta = layers
      .delta
      .created_nodes
      .keys_from(from)
      .inspect(|_| examined_nodes(1))
      .filter(|&node_id| layers.sees_delta(node_id, self.node_mvcc(node_id)));
    let mut from_pending = layers
      .pending
      .into_iter()
      .flat_map(|pending| pending.created_nodes.keys_from(from))
      .inspect(|_| examined_nodes(1));

    // The nodes deleted since the reader's snapshot, which only their version
    // chains hold, in no order: keep the first `limit` after the cursor.
    let mut from_history = Smallest::new(limit);
    if let Some(vc) = self.history {
      for node_id in vc.nodes_at(self.snapshot_ts, self.txid) {
        examined_nodes(1);
        if node_id >= from && from_history.wants(node_id) && !layers.pending_masks(node_id) {
          from_history.insert(node_id);
        }
      }
    }

    // The snapshot's nodes, in ID order from the first after the cursor
    let mut from_snapshot = self
      .snapshot
      .into_iter()
      .flat_map(|snap| {
        let start = after.map_or(0, |after| snap.phys_after(after));
        (start..snap.header.num_nodes as u32).filter_map(move |phys| {
          examined_nodes(1);
          snap.node_id(phys)
        })
      })
      .filter(|&node_id| layers.sees_snapshot(node_id, self.node_mvcc(node_id)));

    let mut nodes = Vec::with_capacity(limit.min(1024));
    let sources: [&mut dyn Iterator<Item = NodeId>; 4] = [
      &mut from_snapshot,
      &mut from_delta,
      &mut from_pending,
      &mut from_history.into_iter(),
    ];
    let _ = merge_sorted(sources, |node_id| {
      nodes.push(node_id);
      if nodes.len() >= limit {
        ControlFlow::Break(())
      } else {
        ControlFlow::Continue(())
      }
    });
    nodes
  }

  /// The first `limit` edges of the sorted `list_edges(None)` after `after`,
  /// noting in `reads` the sources whose out-edges it read.
  fn edges_after(
    &self,
    after: Option<(NodeId, ETypeId, NodeId)>,
    limit: usize,
    reads: &mut Vec<TxKey>,
  ) -> Vec<FullEdge> {
    let layers = self.layers;
    let first_src = after.map_or(0, |(src, _, _)| src);

    // Sources with out-edges the delta or the transaction added, in ID order
    // from the cursor's source on
    let mut from_delta = layers
      .delta
      .out_add
      .keys_from(first_src)
      .inspect(|_| examined_nodes(1));
    let mut from_pending = layers
      .pending
      .into_iter()
      .flat_map(|pending| pending.out_add.keys_from(first_src))
      .inspect(|_| examined_nodes(1));
    // Sources of edges only version chains hold, in ID order
    let mut from_history = HistorySources {
      view: self,
      from: Some(first_src),
      batch: limit.saturating_add(1),
      sorted: BTreeSet::new().into_iter(),
      exhausted: self.history.is_none(),
    };

    // The snapshot's nodes, in ID order from the cursor's source
    let mut from_snapshot = self.snapshot.into_iter().flat_map(|snap| {
      let start = first_src
        .checked_sub(1)
        .map_or(0, |before| snap.phys_after(before));
      (start..snap.header.num_nodes as u32).filter_map(move |phys| {
        examined_nodes(1);
        snap.node_id(phys)
      })
    });

    let mut edges = Vec::with_capacity(limit.min(1024));
    let sources: [&mut dyn Iterator<Item = NodeId>; 4] = [
      &mut from_snapshot,
      &mut from_delta,
      &mut from_pending,
      &mut from_history,
    ];
    let _ = merge_sorted(sources, |src| {
      let start = after
        .filter(|&(after_src, _, _)| after_src == src)
        .map(|(_, etype, dst)| (etype, dst));
      let visible = self.for_each_edge(EdgeSide::Out, src, None, start, |etype, dst| {
        edges.push(FullEdge { src, etype, dst });
        if edges.len() >= limit {
          ControlFlow::Break(())
        } else {
          ControlFlow::Continue(())
        }
      });
      if visible.is_some() && self.tracks_reads {
        reads.push(TxKey::NeighborsOut {
          node_id: src,
          etype: None,
        });
      }
      if edges.len() >= limit {
        ControlFlow::Break(())
      } else {
        ControlFlow::Continue(())
      }
    });
    edges
  }

  /// `list_edges(None).len()`, from the snapshot's edge count and the changes
  /// since: the snapshot edges of deleted nodes and tombstoned edges, and the
  /// delta's and transaction's added edges. `None` when that does not answer:
  /// with version history (which has no index of what it hides), or a
  /// snapshot without in-edges.
  fn count_edges(&self) -> Option<usize> {
    if self.history.is_some() {
      return None;
    }
    let layers = self.layers;
    let delta = layers.delta;
    let pending = layers.pending;
    let snapshot = self.snapshot;
    if snapshot.is_some_and(|snap| !snap.header.flags.contains(SnapshotFlags::HAS_IN_EDGES)) {
      return None;
    }
    let pending_deleted =
      |src, etype, dst| pending.is_some_and(|p| p.is_edge_deleted(src, etype, dst));
    let layers_iter = || std::iter::once(delta).chain(pending);

    // Every snapshot edge is listed unless a layer hides it: an endpoint's
    // delete, or a tombstone.
    let mut count = snapshot.map_or(0, |snap| snap.header.num_edges as usize);
    let mut maybe_hidden = HashSet::new();
    if let Some(snap) = snapshot {
      for &node_id in layers_iter().flat_map(|layer| layer.deleted_nodes.iter()) {
        let Some(phys) = snap.phys_node(node_id) else {
          continue;
        };
        for (dst_phys, etype) in snap.iter_out_edges(phys) {
          examined_edges(1);
          if let Some(dst) = snap.node_id(dst_phys) {
            maybe_hidden.insert((node_id, etype, dst));
          }
        }
        for (src_phys, etype, _) in snap.iter_in_edges(phys) {
          examined_edges(1);
          if let Some(src) = snap.node_id(src_phys) {
            maybe_hidden.insert((src, etype, node_id));
          }
        }
      }
      for (&src, patches) in layers_iter().flat_map(|layer| layer.out_del.iter()) {
        examined_edges(patches.len());
        maybe_hidden.extend(patches.iter().map(|patch| (src, patch.etype, patch.other)));
      }
    }
    for (src, etype, dst) in maybe_hidden {
      let listed = layers.sees_snapshot(src, None)
        && layers.sees_snapshot(dst, None)
        && !pending_deleted(src, etype, dst)
        && !delta.is_edge_deleted(src, etype, dst);
      if !listed && snapshot_has_edge(snapshot, src, etype, dst) {
        count -= 1;
      }
    }

    // Plus the edges `list_edges` lists from the delta: every edge it added
    // (it counts them), but those an endpoint's delete or recreate hides (in
    // the delta or the transaction: the endpoint's own patches name them) and
    // those the transaction deleted.
    count += delta.out_add.patch_count();
    let hides = |node_id| !layers.sees_delta(node_id, None);
    let hidden: HashSet<NodeId> = layers_iter()
      .flat_map(|layer| layer.deleted_nodes.iter())
      .copied()
      .filter(|&node_id| hides(node_id))
      .collect();
    for &node_id in &hidden {
      if let Some(patches) = delta.out_add.get(&node_id) {
        examined_edges(patches.len());
        count -= patches.len();
      }
      // Edges into it from a source counted above are not counted again.
      if let Some(patches) = delta.in_add.get(&node_id) {
        examined_edges(patches.len());
        count -= patches.iter().filter(|patch| !hides(patch.other)).count();
      }
    }
    for (&src, tombstones) in pending.into_iter().flat_map(|p| p.out_del.iter()) {
      examined_edges(tombstones.len());
      if hides(src) {
        continue;
      }
      count -= tombstones
        .iter()
        .filter(|patch| !hides(patch.other) && delta.is_edge_added(src, patch.etype, patch.other))
        .count();
    }
    // And the transaction's own added edges.
    for (&src, patches) in pending.into_iter().flat_map(|p| p.out_add.iter()) {
      examined_edges(patches.len());
      if !layers.sees_pending(src, None) {
        continue;
      }
      count += patches
        .iter()
        .filter(|patch| layers.sees_pending(patch.other, None))
        .count();
    }
    Some(count)
  }

  /// Whether no layer above the snapshot names `node_id`: then the snapshot's
  /// copy (its key and labels) is the node, and the reader sees it.
  fn snapshot_copy_untouched(&self, node_id: NodeId) -> bool {
    let untouched = |layer: &DeltaState| {
      !layer.deleted_nodes.contains(&node_id)
        && !layer.created_nodes.contains_key(&node_id)
        && !layer.modified_nodes.contains_key(&node_id)
    };
    self.history.is_none()
      && untouched(self.layers.delta)
      && self.layers.pending.is_none_or(untouched)
  }

  /// Visit each node the reader sees with label `label_id`, once, in no
  /// particular order.
  fn for_each_labeled(&self, label_id: LabelId, mut f: impl FnMut(NodeId)) {
    if self.history.is_some() {
      // Version history may hold a label change of any node.
      for node_id in self.listed_nodes() {
        if self.node_has_label(node_id, label_id) {
          f(node_id);
        }
      }
      return;
    }
    // Otherwise a node has the label through its snapshot copy, or through the
    // delta or the transaction adding it.
    let in_snapshot_labels = |node_id| {
      self.snapshot.is_some_and(|snap| {
        snap
          .phys_node(node_id)
          .is_some_and(|phys| snap.node_has_label(phys, label_id))
      })
    };
    if let Some(snap) = self.snapshot {
      for phys in 0..snap.header.num_nodes as u32 {
        examined_nodes(1);
        if !snap.node_has_label(phys, label_id) {
          continue;
        }
        if let Some(node_id) = snap.node_id(phys) {
          if self.snapshot_copy_untouched(node_id) || self.node_has_label(node_id, label_id) {
            f(node_id);
          }
        }
      }
    }
    // The nodes a layer's created or modified copy adds the label to: its
    // label index names them (and maybe some more).
    let names = |node: Option<&NodeDelta>| {
      node
        .and_then(|node| node.labels.as_ref())
        .is_some_and(|labels| labels.contains(&label_id))
    };
    let mut added = Vec::new();
    for layer in std::iter::once(self.layers.delta).chain(self.layers.pending) {
      for &node_id in layer.labeled_nodes.get(&label_id).into_iter().flatten() {
        examined_nodes(1);
        if (names(layer.created_nodes.get(&node_id)) || names(layer.modified_nodes.get(&node_id)))
          && !in_snapshot_labels(node_id)
        {
          added.push(node_id);
        }
      }
    }
    // Each once (an index may name a node twice, and both layers may name it).
    added.sort_unstable();
    added.dedup();
    for node_id in added {
      if self.node_has_label(node_id, label_id) {
        f(node_id);
      }
    }
  }

  /// The nodes the reader sees whose key starts with `prefix`, with their
  /// keys, sorted by ID.
  fn nodes_with_key_prefix(&self, prefix: &str) -> Vec<(NodeId, String)> {
    let mut nodes = Vec::new();
    // `known`: the node's key, when its snapshot copy is untouched.
    let mut take = |node_id, known: Option<&str>| {
      let key = match known {
        Some(key) => Some(key),
        None => self
          .node_key(node_id)
          .filter(|key| key.starts_with(prefix) && self.node_exists(node_id)),
      };
      if let Some(key) = key {
        nodes.push((node_id, key.to_owned()));
      }
    };
    if self.history.is_some() {
      // Version history may hold the key of any node deleted since.
      for node_id in self.listed_nodes() {
        take(node_id, None);
      }
      return nodes;
    }
    // Otherwise a node's key is its snapshot copy's, or the one it was created
    // (or recreated) with in the delta or the transaction.
    let snapshot_key_matches = |node_id| {
      self.snapshot.is_some_and(|snap| {
        snap
          .phys_node(node_id)
          .and_then(|phys| snap.node_key_str(phys))
          .is_some_and(|key| key.starts_with(prefix))
      })
    };
    if let Some(snap) = self.snapshot {
      for phys in 0..snap.header.num_nodes as u32 {
        examined_nodes(1);
        let Some(key) = snap
          .node_key_str(phys)
          .filter(|key| key.starts_with(prefix))
        else {
          continue;
        };
        let Some(node_id) = snap.node_id(phys) else {
          continue;
        };
        let known = self.snapshot_copy_untouched(node_id).then_some(key);
        take(node_id, known);
      }
    }
    let mut created = Vec::new();
    for layer in std::iter::once(self.layers.delta).chain(self.layers.pending) {
      examined_nodes(layer.created_nodes.len());
      for (&node_id, node) in &layer.created_nodes {
        if node
          .key
          .as_deref()
          .is_some_and(|key| key.starts_with(prefix))
          && !snapshot_key_matches(node_id)
        {
          created.push(node_id);
        }
      }
    }
    // Each once (both layers may hold a node).
    created.sort_unstable();
    created.dedup();
    for node_id in created {
      take(node_id, None);
    }
    nodes.sort_unstable_by_key(|&(node_id, _)| node_id);
    nodes
  }
}

/// The `limit` smallest keys inserted, sorted: what a page needs of keys that
/// arrive in hash order.
struct Smallest<K> {
  keys: BTreeSet<K>,
  limit: usize,
  /// The largest key kept, once `limit` are kept
  max: Option<K>,
}

impl<K: Ord + Copy> Smallest<K> {
  fn new(limit: usize) -> Self {
    Self {
      keys: BTreeSet::new(),
      limit,
      max: None,
    }
  }

  /// Whether inserting `key` would keep it: a comparison, once full.
  fn wants(&self, key: K) -> bool {
    self.limit > 0 && self.max.is_none_or(|max| key < max)
  }

  fn insert(&mut self, key: K) {
    if !self.wants(key) || !self.keys.insert(key) {
      return;
    }
    if self.keys.len() > self.limit {
      self.keys.pop_last();
    }
    if self.keys.len() == self.limit {
      self.max = self.keys.last().copied();
    }
  }

  fn len(&self) -> usize {
    self.keys.len()
  }

  fn into_iter(self) -> std::collections::btree_set::IntoIter<K> {
    self.keys.into_iter()
  }
}

/// The sources of edges that only version chains hold (deleted since the
/// reader's snapshot), from `from` on in ID order. They come in hash order, so
/// each `batch` of them takes a pass over all of them: one pass per page unless
/// many of them have no edges left.
struct HistorySources<'v> {
  view: &'v ReadView<'v>,
  /// The smallest source not yielded yet (`None`: past the largest ID)
  from: Option<NodeId>,
  batch: usize,
  sorted: std::collections::btree_set::IntoIter<NodeId>,
  /// Whether the last pass found every remaining source
  exhausted: bool,
}

impl HistorySources<'_> {
  fn next_batch(&mut self) {
    let (Some(from), Some(vc)) = (self.from, self.view.history) else {
      self.exhausted = true;
      return;
    };
    let mut batch = Smallest::new(self.batch);
    for (src, _, _) in vc.edges_at(self.view.snapshot_ts, self.view.txid) {
      examined_nodes(1);
      if src >= from && batch.wants(src) {
        batch.insert(src);
      }
    }
    self.exhausted = batch.len() < self.batch;
    self.sorted = batch.into_iter();
  }
}

impl Iterator for HistorySources<'_> {
  type Item = NodeId;

  fn next(&mut self) -> Option<NodeId> {
    loop {
      if let Some(src) = self.sorted.next() {
        self.from = src.checked_add(1);
        return Some(src);
      }
      if self.exhausted {
        return None;
      }
      self.next_batch();
    }
  }
}

impl Iterator for NodeIterator {
  type Item = NodeId;

  fn next(&mut self) -> Option<Self::Item> {
    if self.index < self.nodes.len() {
      let node_id = self.nodes[self.index];
      self.index += 1;
      Some(node_id)
    } else {
      None
    }
  }

  fn size_hint(&self) -> (usize, Option<usize>) {
    let remaining = self.nodes.len() - self.index;
    (remaining, Some(remaining))
  }
}

impl ExactSizeIterator for NodeIterator {}

// ============================================================================
// SingleFileDB Implementation - Iteration and Stats
// ============================================================================

impl SingleFileDB {
  /// Iterate all nodes in the database
  ///
  /// Yields node IDs by merging snapshot nodes with delta changes.
  /// Nodes deleted in delta are skipped, nodes created in delta are included.
  pub fn iter_nodes(&self) -> NodeIterator {
    NodeIterator::new(self)
  }

  /// Collect all node IDs into a Vec
  ///
  /// For large databases, prefer `iter_nodes()` to avoid memory allocation.
  pub fn list_nodes(&self) -> Vec<NodeId> {
    self.iter_nodes().collect()
  }

  /// The first `limit` nodes of [`Self::list_nodes`] with an ID greater than
  /// `after` (from the first node for `None`), sorted by ID.
  ///
  /// Seeks into the snapshot instead of listing it: a page costs a binary
  /// search plus the nodes it returns, plus one pass over the nodes created
  /// since the last checkpoint. Pass the last node of one page as `after` to
  /// get the next; a node deleted in between does not move the cursor.
  pub fn nodes_after(&self, after: Option<NodeId>, limit: usize) -> Vec<NodeId> {
    if limit == 0 {
      return Vec::new();
    }
    self.read_view(|view, _| view.nodes_after(after, limit))
  }

  /// The first `limit` edges of [`Self::list_edges`]`(None)` that come after
  /// `after` in `(src, etype, dst)` order, in that order.
  ///
  /// Seeks into the snapshot instead of listing it: a page costs a binary
  /// search plus the sources and edges it walks, plus one pass over the
  /// sources with edges added since the last checkpoint. Pass the last edge of
  /// one page as `after` to get the next; an edge deleted in between does not
  /// move the cursor.
  pub fn edges_after(
    &self,
    after: Option<(NodeId, ETypeId, NodeId)>,
    limit: usize,
  ) -> Vec<FullEdge> {
    if limit == 0 {
      return Vec::new();
    }
    self.read_view(|view, reads| view.edges_after(after, limit, reads))
  }

  /// The nodes with label `label_id`, sorted by ID: those of
  /// [`Self::list_nodes`] whose [`Self::node_labels`] include it.
  ///
  /// Reads the snapshot's label lists in one pass and the label changes since
  /// the last checkpoint, instead of each node's labels.
  pub fn nodes_with_label(&self, label_id: LabelId) -> Vec<NodeId> {
    let mut nodes = self.read_view(|view, _| {
      let mut nodes = Vec::new();
      view.for_each_labeled(label_id, |node_id| nodes.push(node_id));
      nodes
    });
    nodes.sort_unstable();
    nodes
  }

  /// The number of nodes [`Self::nodes_with_label`] lists, without listing them.
  pub fn count_nodes_with_label(&self, label_id: LabelId) -> usize {
    self.read_view(|view, _| {
      let mut count = 0;
      view.for_each_labeled(label_id, |_| count += 1);
      count
    })
  }

  /// The nodes whose key starts with `prefix`, with their keys, sorted by ID:
  /// those of [`Self::list_nodes`] whose [`Self::node_key`] starts with it.
  ///
  /// Reads the snapshot's keys in one pass and the nodes created since the
  /// last checkpoint, instead of looking up each node's key.
  pub fn nodes_with_key_prefix(&self, prefix: &str) -> Vec<(NodeId, String)> {
    self.read_view(|view, _| view.nodes_with_key_prefix(prefix))
  }

  /// Count total nodes in the database: the length of `iter_nodes()`,
  /// without listing them.
  ///
  /// The snapshot's node count, adjusted only for the nodes the committed
  /// delta, the caller's transaction, or the MVCC version chains name, so
  /// the cost follows those (bounded by the WAL and the open transactions),
  /// not the graph.
  pub fn count_nodes(&self) -> usize {
    let tx_handle = self.current_tx_handle();
    let tx_guard = tx_handle.as_ref().map(|tx| tx.lock());
    let pending = tx_guard.as_ref().map(|tx| &tx.pending);
    // Lock order: see read.rs.
    let (txid, tx_snapshot_ts) = self.mvcc_read_ts(tx_guard.as_deref());
    let delta = self.delta.read();
    let snapshot = self.snapshot.read();
    let vc_guard = self.mvcc_history(tx_snapshot_ts);
    let layers = NodeLayers {
      pending,
      delta: &delta,
    };
    let snapshot = snapshot.as_ref();
    let mvcc_visible = |node_id: NodeId| {
      vc_guard
        .as_ref()
        .and_then(|vc| vc.node_exists_at(node_id, tx_snapshot_ts, txid))
    };
    // What `NodeIterator` lists from each source.
    let from_snapshot = |node_id: NodeId, mvcc: Option<bool>| {
      snapshot.is_some_and(|snap| snap.has_node(node_id)) && layers.sees_snapshot(node_id, mvcc)
    };
    let from_elsewhere = |node_id: NodeId, mvcc: Option<bool>| {
      (delta.is_node_created(node_id) && layers.sees_delta(node_id, mvcc))
        || pending.is_some_and(|p| p.is_node_created(node_id))
        || (mvcc == Some(true) && !layers.pending_masks(node_id))
    };
    let chained: Vec<NodeId> = vc_guard
      .as_ref()
      .map(|vc| vc.chained_node_ids().collect())
      .unwrap_or_default();
    let pending_deleted = pending.into_iter().flat_map(|p| p.deleted_nodes.iter());
    let pending_created = pending.into_iter().flat_map(|p| p.created_nodes.keys());

    // Every snapshot node is listed unless a layer naming it hides it.
    let mut count = snapshot.map_or(0, |snap| snap.header.num_nodes as usize);
    let maybe_hidden: HashSet<NodeId> = delta
      .deleted_nodes
      .iter()
      .chain(pending_deleted)
      .chain(&chained)
      .copied()
      .collect();
    for node_id in maybe_hidden {
      let in_snapshot = snapshot.is_some_and(|snap| snap.has_node(node_id));
      if in_snapshot && !from_snapshot(node_id, mvcc_visible(node_id)) {
        count -= 1;
      }
    }
    // Plus the nodes listed from the delta, the transaction or the chains,
    // once, unless listed from the snapshot as well: each source's nodes but
    // those an earlier source names (the delta's are distinct, and most).
    let listed_elsewhere = |node_id: NodeId| {
      let mvcc = mvcc_visible(node_id);
      from_elsewhere(node_id, mvcc) && !from_snapshot(node_id, mvcc)
    };
    // A node the delta created is listed from it, and not from the snapshot,
    // unless the snapshot holds it or the transaction or the chains name it:
    // only those are checked, the created nodes up to the snapshot's largest
    // ID (in order) and the named ones.
    let snapshot_max = snapshot
      .filter(|snap| snap.header.num_nodes > 0)
      .map(|snap| {
        snap
          .node_id((snap.header.num_nodes - 1) as PhysNode)
          .unwrap_or(NodeId::MAX)
      });
    let named = pending
      .into_iter()
      .flat_map(|p| p.deleted_nodes.iter().chain(p.created_nodes.keys()))
      .chain(&chained)
      .copied()
      .filter(|node_id| delta.created_nodes.contains_key(node_id));
    let checked: HashSet<NodeId> = snapshot_max
      .into_iter()
      .flat_map(|max| {
        delta
          .created_nodes
          .keys_from(0)
          .take_while(move |&node_id| node_id <= max)
      })
      .inspect(|_| examined_nodes(1))
      .chain(named)
      .collect();
    count += delta.created_nodes.len() - checked.len();
    count += checked
      .into_iter()
      .filter(|&node_id| listed_elsewhere(node_id))
      .count();
    let pending_only: HashSet<NodeId> = pending_created
      .copied()
      .filter(|node_id| !delta.created_nodes.contains_key(node_id))
      .collect();
    let chained_only: HashSet<NodeId> = chained
      .iter()
      .copied()
      .filter(|node_id| {
        !delta.created_nodes.contains_key(node_id) && !pending_only.contains(node_id)
      })
      .collect();
    count += pending_only
      .into_iter()
      .chain(chained_only)
      .filter(|&node_id| listed_elsewhere(node_id))
      .count();
    count
  }

  /// Count total edges in the database: the length of `list_edges(None)`,
  /// without listing them.
  ///
  /// The snapshot's edge count, adjusted for the changes since the last
  /// checkpoint (the edges of deleted nodes, tombstones, added edges), so the
  /// cost follows those, not the graph. It lists the edges instead inside an
  /// MVCC write transaction (whose conflict check notes the sources it read)
  /// and for an MVCC reader that reads version history.
  pub fn count_edges(&self) -> usize {
    self
      .read_view(|view, _| {
        if view.tracks_reads {
          return None;
        }
        view.count_edges()
      })
      .unwrap_or_else(|| self.list_edges(None).len())
  }

  /// Count edges of a specific type
  pub fn count_edges_by_type(&self, etype: ETypeId) -> usize {
    self.list_edges(Some(etype)).len()
  }

  /// List all edges in the database
  ///
  /// Optionally filter by edge type.
  pub fn list_edges(&self, etype_filter: Option<ETypeId>) -> Vec<FullEdge> {
    let tx_handle = self.current_tx_handle();
    let mut tx_guard = tx_handle.as_ref().map(|tx| tx.lock());
    let pending = tx_guard.as_ref().map(|tx| &tx.pending);
    // Lock order: see read.rs.
    let (txid, tx_snapshot_ts) = self.mvcc_read_ts(tx_guard.as_deref());
    let delta = self.delta.read();
    let snapshot = self.snapshot.read();
    let vc_guard = self.mvcc_history(tx_snapshot_ts);
    let mut edges = Vec::new();
    // A write transaction's reads, for its MVCC conflict check
    let mut read_srcs = (self.mvcc.is_some() && tx_guard.as_ref().is_some_and(|tx| !tx.read_only))
      .then(HashSet::<NodeId>::new);
    let layers = NodeLayers {
      pending,
      delta: &delta,
    };

    // From snapshot
    if let Some(ref snap) = **snapshot {
      let num_nodes = snap.header.num_nodes as u32;
      for phys in 0..num_nodes {
        examined_nodes(1);
        if let Some(src) = snap.node_id(phys) {
          // Skip deleted nodes
          let src_visible = vc_guard
            .as_ref()
            .and_then(|vc| vc.node_exists_at(src, tx_snapshot_ts, txid));
          if !layers.sees_snapshot(src, src_visible) {
            continue;
          }
          if let Some(ref mut srcs) = read_srcs {
            srcs.insert(src);
          }

          for (dst_phys, etype) in snap.iter_out_edges(phys) {
            examined_edges(1);
            // Apply filter
            if let Some(filter_etype) = etype_filter {
              if etype != filter_etype {
                continue;
              }
            }

            if let Some(dst) = snap.node_id(dst_phys) {
              // Skip deleted edges
              let dst_visible = vc_guard
                .as_ref()
                .and_then(|vc| vc.node_exists_at(dst, tx_snapshot_ts, txid));
              if !layers.sees_snapshot(dst, dst_visible) {
                continue;
              }
              let edge_visible = vc_guard
                .as_ref()
                .and_then(|vc| vc.edge_exists_at(src, etype, dst, tx_snapshot_ts, txid));
              if edge_visible == Some(false)
                || pending.is_some_and(|p| p.is_edge_deleted(src, etype, dst))
                || (edge_visible.is_none() && delta.is_edge_deleted(src, etype, dst))
              {
                continue;
              }

              edges.push(FullEdge { src, etype, dst });
            }
          }
        }
      }
    }

    // Add delta edges
    for (&src, add_set) in &delta.out_add {
      examined_edges(add_set.len());
      for patch in add_set {
        // Apply filter
        if let Some(filter_etype) = etype_filter {
          if patch.etype != filter_etype {
            continue;
          }
        }

        let src_visible = vc_guard
          .as_ref()
          .and_then(|vc| vc.node_exists_at(src, tx_snapshot_ts, txid));
        if !layers.sees_delta(src, src_visible) {
          continue;
        }
        if let Some(ref mut srcs) = read_srcs {
          srcs.insert(src);
        }
        let dst_visible = vc_guard
          .as_ref()
          .and_then(|vc| vc.node_exists_at(patch.other, tx_snapshot_ts, txid));
        if !layers.sees_delta(patch.other, dst_visible) {
          continue;
        }
        let edge_visible = vc_guard
          .as_ref()
          .and_then(|vc| vc.edge_exists_at(src, patch.etype, patch.other, tx_snapshot_ts, txid));
        if edge_visible == Some(false) {
          continue;
        }
        if pending.is_some_and(|p| p.is_edge_deleted(src, patch.etype, patch.other)) {
          continue;
        }

        edges.push(FullEdge {
          src,
          etype: patch.etype,
          dst: patch.other,
        });
      }
    }

    if let Some(pending_delta) = pending {
      for (&src, add_set) in &pending_delta.out_add {
        examined_edges(add_set.len());
        for patch in add_set {
          if let Some(filter_etype) = etype_filter {
            if patch.etype != filter_etype {
              continue;
            }
          }

          let src_visible = vc_guard
            .as_ref()
            .and_then(|vc| vc.node_exists_at(src, tx_snapshot_ts, txid));
          if !layers.sees_pending(src, src_visible) {
            continue;
          }
          if let Some(ref mut srcs) = read_srcs {
            srcs.insert(src);
          }
          let dst_visible = vc_guard
            .as_ref()
            .and_then(|vc| vc.node_exists_at(patch.other, tx_snapshot_ts, txid));
          if !layers.sees_pending(patch.other, dst_visible) {
            continue;
          }

          edges.push(FullEdge {
            src,
            etype: patch.etype,
            dst: patch.other,
          });
        }
      }
    }

    // Edges deleted since the reader's snapshot: only their version chains hold them
    if let Some(vc) = vc_guard.as_ref() {
      let listed = edges.len();
      let node_visible =
        |node_id| layers.sees_delta(node_id, vc.node_exists_at(node_id, tx_snapshot_ts, txid));
      for (src, etype, dst) in vc.edges_at(tx_snapshot_ts, txid) {
        examined_edges(1);
        if etype_filter.is_some_and(|filter_etype| filter_etype != etype)
          || pending.is_some_and(|p| p.is_edge_deleted(src, etype, dst))
          || !node_visible(src)
          || !node_visible(dst)
        {
          continue;
        }
        if let Some(ref mut srcs) = read_srcs {
          srcs.insert(src);
        }
        edges.push(FullEdge { src, etype, dst });
      }
      // Drop the ones the delta or snapshot listed too
      if edges.len() > listed {
        let mut seen = HashSet::with_capacity(edges.len());
        edges.retain(|edge| seen.insert((edge.src, edge.etype, edge.dst)));
      }
    }

    drop(vc_guard);
    self.record_reads(
      tx_guard.as_deref_mut(),
      read_srcs
        .into_iter()
        .flatten()
        .map(|src| TxKey::NeighborsOut {
          node_id: src,
          etype: etype_filter,
        }),
    );

    edges
  }

  /// Get database statistics
  pub fn stats(&self) -> DbStats {
    // Commit and checkpoints lock wal_buffer and then header.write(): read the
    // WAL before any other guard, and copy from the header instead of holding it.
    let wal_bytes = self.wal_stats().used;
    let recommend_compact = self.should_checkpoint(0.8);
    let snapshot_gen = self.header.read().active_snapshot_gen;
    let delta = self.delta.read();
    let snapshot = self.snapshot.read();

    let (snapshot_nodes, snapshot_edges, snapshot_max_node_id) = if let Some(ref snap) = **snapshot
    {
      (
        snap.header.num_nodes,
        snap.header.num_edges,
        snap.header.max_node_id,
      )
    } else {
      (0, 0, 0)
    };

    DbStats {
      snapshot_gen,
      snapshot_nodes,
      snapshot_edges,
      snapshot_max_node_id,
      delta_nodes_created: delta.created_nodes.len(),
      delta_nodes_deleted: delta.deleted_nodes.len(),
      delta_edges_added: delta.total_edges_added(),
      delta_edges_deleted: delta.total_edges_deleted(),
      wal_segment: 0, // Not applicable for single-file
      wal_bytes,
      recommend_compact,
      mvcc_stats: self.mvcc.as_ref().map(|mvcc| {
        let tx_mgr = mvcc.tx_manager.lock();
        let gc = mvcc.gc.lock();
        let gc_stats = gc.stats();
        let committed = tx_mgr.committed_writes_stats();
        MvccStats {
          active_transactions: tx_mgr.active_count(),
          min_active_ts: tx_mgr.min_active_ts(),
          versions_pruned: gc_stats.versions_pruned,
          gc_runs: gc_stats.gc_runs,
          last_gc_time: gc_stats.last_gc_time,
          committed_writes_size: committed.size,
          committed_writes_pruned: committed.pruned,
        }
      }),
    }
  }

  /// Get WAL buffer statistics
  pub fn wal_stats(&self) -> crate::core::wal::buffer::WalBufferStats {
    self.wal_buffer.lock().stats()
  }
}

#[cfg(test)]
#[path = "b4_query_core_tests.rs"]
mod b4_query_core_tests;

#[cfg(test)]
#[path = "b4_read_paths_tests.rs"]
mod b4_read_paths_tests;
