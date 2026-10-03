//! Read operations for SingleFileDB
//!
//! Handles all query operations: get properties, get edges, key lookups,
//! label checks, and neighbor traversal.
//!
//! # Lock order
//!
//! `delta` -> `snapshot` -> `mvcc.tx_manager` -> `mvcc.version_chain` -> `mvcc.gc`
//!
//! Every path that holds more than one of these takes them in this order: a commit's publish
//! holds `delta` (upgradable, then written) -> `snapshot.read()` -> `version_chain`, and
//! releases the last two before it waits to write the delta; checkpoint installs hold
//! `delta.write()` -> `snapshot.write()` (then the vector stores) to replace both in one
//! step (`install_loaded_snapshot`), and GC takes `tx_manager`, `version_chain` and `gc` one
//! at a time (`mvcc/manager.rs`). Readers take `version_chain` (shared) last, and only when
//! it can answer for them (`mvcc_history`); they take `tx_manager` never: a transaction's
//! reads go to its own state (`SingleFileTxState::record_read`). `delta` and `snapshot` are
//! task-fair RwLocks: a queued writer blocks new readers, so even a read guard must never be
//! requested while a later lock is held. The calling thread's own tx state mutex is private
//! to that thread and sits outside this order, but it is not reentrant: never lock it twice.
//!
//! # MVCC
//!
//! The committed delta and snapshot hold the latest committed state. The version chains only
//! hold history (see `mvcc::version_chain`): a read consults them first, and their `*_at`
//! lookups answer only for a reader whose snapshot predates a change, `None` otherwise, so a
//! reader newer than every recorded change skips them (`mvcc_history`). Reads
//! hold `delta` across those lookups. A commit records its versions while reads go on, then
//! merges into the delta under `delta.write()`: until the merge, which waits for the read, the
//! delta and the commit's versions both hold the state before it, and a reader that sees the
//! commit cannot begin before the merge. Enumerations add what only the chains still hold:
//! nodes, edges and keys deleted since the reader's snapshot.

use std::collections::HashMap;
use std::ops::{Bound, ControlFlow};
use std::sync::Arc;

use parking_lot::{Mutex, RwLockReadGuard};

use crate::core::snapshot::reader::SnapshotData;
use crate::mvcc::VersionChainManager;
use crate::types::*;

use super::{SingleFileDB, SingleFileTxState};

// ============================================================================
// Test instrumentation
// ============================================================================

#[cfg(test)]
thread_local! {
  /// Node entries (snapshot nodes; delta, transaction and version-chain node
  /// entries) that listings visited on this thread.
  pub(crate) static NODES_EXAMINED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
  /// Edge entries (snapshot edges; delta, transaction and version-chain edge
  /// entries) that reads visited on this thread.
  pub(crate) static EDGES_EXAMINED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
  /// Point reads of one node's key or labels on this thread.
  pub(crate) static NODE_LOOKUPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Count `n` node entries visited (test instrumentation).
#[inline]
pub(super) fn examined_nodes(n: usize) {
  #[cfg(test)]
  NODES_EXAMINED.with(|count| count.set(count.get() + n));
  let _ = n;
}

/// Count `n` edge entries visited (test instrumentation).
#[inline]
pub(super) fn examined_edges(n: usize) {
  #[cfg(test)]
  EDGES_EXAMINED.with(|count| count.set(count.get() + n));
  let _ = n;
}

/// Count one point read of a node's key or labels (test instrumentation).
#[inline]
fn node_lookup() {
  #[cfg(test)]
  NODE_LOOKUPS.with(|count| count.set(count.get() + 1));
}

/// Which layers' state of a node a reader sees: its transaction's pending
/// delta over the committed delta over the snapshot. A layer's delete masks
/// the node's copies below it (props, labels, key, edges); a recreated node
/// holds a fresh copy in the layer that recreated it. `mvcc` is whether the
/// node existed at the reader's MVCC snapshot, if it changed since (`None`
/// when the delta and snapshot decide, see the module docs).
#[derive(Clone, Copy)]
pub(super) struct NodeLayers<'a> {
  pub(super) pending: Option<&'a DeltaState>,
  pub(super) delta: &'a DeltaState,
}

impl NodeLayers<'_> {
  /// The snapshot's copy of the node: its props, labels, key and edges. A
  /// delete in the committed delta masks it for every reader, also for one
  /// that saw it before the delete: the version chains record what the
  /// delete removed, and a reader that saw a recreated node never saw the
  /// old copy.
  pub(super) fn sees_snapshot(&self, node_id: NodeId, mvcc: Option<bool>) -> bool {
    !self.pending_masks(node_id) && mvcc != Some(false) && !self.delta.is_node_deleted(node_id)
  }

  /// The committed delta's state of the node: its copy (if created there)
  /// and edge patches.
  pub(super) fn sees_delta(&self, node_id: NodeId, mvcc: Option<bool>) -> bool {
    !self.pending_masks(node_id)
      && match mvcc {
        Some(visible) => visible,
        None => !self.delta.is_node_removed(node_id),
      }
  }

  /// The transaction's own state of the node: its edge patches.
  pub(super) fn sees_pending(&self, node_id: NodeId, mvcc: Option<bool>) -> bool {
    self.pending.is_some_and(|p| p.is_node_created(node_id)) || self.sees_delta(node_id, mvcc)
  }

  /// Whether the transaction deleted (or recreated) the node, masking its
  /// committed copy.
  pub(super) fn pending_masks(&self, node_id: NodeId) -> bool {
    self.pending.is_some_and(|p| p.is_node_deleted(node_id))
  }

  /// Whether the node exists for the reader: created by its transaction, or
  /// committed and not deleted by it. A node's delta state alone is not
  /// existence: props or labels written to a missing node (by older versions
  /// or racing commits) must not make it appear.
  pub(super) fn node_exists(
    &self,
    snapshot: Option<&SnapshotData>,
    node_id: NodeId,
    mvcc: Option<bool>,
  ) -> bool {
    if self.pending.is_some_and(|p| p.is_node_created(node_id)) {
      return true;
    }
    !self.pending_masks(node_id)
      && match mvcc {
        Some(visible) => visible,
        None => self.delta.node_exists_over(snapshot, node_id),
      }
  }
}

/// One read's view of the database, under its guards: the calling thread's
/// transaction over the committed delta over the snapshot, and the version
/// history when the reader needs it (see the module docs). Built by
/// [`SingleFileDB::read_view`]; bulk reads use it to answer for many nodes or
/// edges under one set of guards.
pub(super) struct ReadView<'a> {
  pub(super) layers: NodeLayers<'a>,
  pub(super) snapshot: Option<&'a SnapshotData>,
  /// The version chains, when they can answer for this reader
  pub(super) history: Option<&'a VersionChainManager>,
  pub(super) txid: TxId,
  pub(super) snapshot_ts: Timestamp,
  /// Whether the reads go to a write transaction's MVCC conflict check
  pub(super) tracks_reads: bool,
}

/// Which edges of a node a walk reads: its out-edges, keyed `(etype, dst)`, or
/// its in-edges, keyed `(etype, src)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EdgeSide {
  Out,
  In,
}

/// The keys `(etype, other endpoint)` an edge walk visits: those of type `etype`
/// (every type for `None`) that come after `after`.
#[derive(Clone, Copy)]
struct EdgeKeys {
  etype: Option<ETypeId>,
  after: Option<(ETypeId, NodeId)>,
}

impl EdgeKeys {
  fn contains(&self, key: (ETypeId, NodeId)) -> bool {
    self.etype.is_none_or(|etype| key.0 == etype) && self.after.is_none_or(|after| key > after)
  }

  /// Where the keys start, as `(etype, smallest endpoint)` with the endpoint
  /// mapped by `first_after` to what follows `after`'s endpoint; `None` if the
  /// range is empty.
  fn start<T: Default>(&self, first_after: impl FnOnce(NodeId) -> T) -> Option<(ETypeId, T)> {
    match (self.etype, self.after) {
      (None, None) => Some((0, T::default())),
      (None, Some((etype, other))) => Some((etype, first_after(other))),
      (Some(etype), None) => Some((etype, T::default())),
      (Some(etype), Some((after_etype, other))) => match after_etype.cmp(&etype) {
        std::cmp::Ordering::Less => Some((etype, T::default())),
        std::cmp::Ordering::Equal => Some((etype, first_after(other))),
        std::cmp::Ordering::Greater => None,
      },
    }
  }

  /// The keys as a range of a delta's edge patches; `None` if it is empty.
  fn patch_bounds(&self) -> Option<(Bound<EdgePatch>, Bound<EdgePatch>)> {
    let patch = |(etype, other)| EdgePatch { etype, other };
    let lower = match (self.etype, self.after) {
      (Some(etype), Some(after)) if after.0 > etype => return None,
      (Some(etype), Some(after)) if after.0 == etype => Bound::Excluded(patch(after)),
      (Some(etype), _) => Bound::Included(patch((etype, 0))),
      (None, Some(after)) => Bound::Excluded(patch(after)),
      (None, None) => Bound::Unbounded,
    };
    let upper = self.etype.map_or(Bound::Unbounded, |etype| {
      Bound::Included(patch((etype, NodeId::MAX)))
    });
    Some((lower, upper))
  }
}

/// Merge sorted sources into one sorted walk, each key once, until `f` breaks.
pub(super) fn merge_sorted<K: Ord + Copy, const N: usize>(
  mut sources: [&mut dyn Iterator<Item = K>; N],
  mut f: impl FnMut(K) -> ControlFlow<()>,
) -> ControlFlow<()> {
  let mut heads: [Option<K>; N] = std::array::from_fn(|i| sources[i].next());
  let mut last = None;
  loop {
    let mut live = heads.iter().enumerate().filter(|(_, head)| head.is_some());
    let (Some((first, _)), second) = (live.next(), live.next()) else {
      return ControlFlow::Continue(());
    };
    if second.is_none() {
      // One source left: walk it without comparing.
      let head = heads[first].take();
      for key in head.into_iter().chain(&mut *sources[first]) {
        if last != Some(key) {
          last = Some(key);
          f(key)?;
        }
      }
      return ControlFlow::Continue(());
    }
    let min = heads.iter().flatten().min().copied();
    for (head, source) in heads.iter_mut().zip(sources.iter_mut()) {
      if *head == min {
        *head = source.next();
      }
    }
    if last != min {
      last = min;
      if let Some(key) = min {
        f(key)?;
      }
    }
  }
}

impl<'a> ReadView<'a> {
  /// Whether `node_id` existed at the reader's snapshot, if it changed since (see the module
  /// docs); `None` when the delta and snapshot decide.
  #[inline]
  pub(super) fn node_mvcc(&self, node_id: NodeId) -> Option<bool> {
    node_exists_in_history(self.history?, node_id, self.snapshot_ts, self.txid)
  }

  #[inline]
  fn edge_mvcc(&self, src: NodeId, etype: ETypeId, dst: NodeId) -> Option<bool> {
    edge_exists_in_history(self.history?, src, etype, dst, self.snapshot_ts, self.txid)
  }

  /// Whether the reader sees `node_id`: whether `iter_nodes` lists it.
  pub(super) fn node_exists(&self, node_id: NodeId) -> bool {
    self
      .layers
      .node_exists(self.snapshot, node_id, self.node_mvcc(node_id))
  }

  /// [`SingleFileDB::node_key`].
  pub(super) fn node_key(&self, node_id: NodeId) -> Option<&'a str> {
    let pending = self.layers.pending;
    let delta = self.layers.delta;
    if pending.is_some_and(|p| p.is_node_removed(node_id)) {
      return None;
    }
    if let Some(node) = pending.and_then(|p| p.created_nodes.get(&node_id)) {
      return node.key.as_deref();
    }
    // The node at the reader's snapshot, with its key, if it changed since
    if let Some(node) = self
      .history
      .and_then(|vc| vc.node_at(node_id, self.snapshot_ts, self.txid))
    {
      return node.and_then(|node| node.delta.key.as_deref());
    }
    if delta.is_node_removed(node_id) {
      return None;
    }
    if let Some(node) = delta.created_nodes.get(&node_id) {
      return node.key.as_deref();
    }
    let snapshot = self.snapshot?;
    snapshot.node_key_str(snapshot.phys_node(node_id)?)
  }

  /// Whether [`SingleFileDB::node_labels`] lists `label_id`.
  pub(super) fn node_has_label(&self, node_id: NodeId, label_id: LabelId) -> bool {
    let layers = self.layers;
    let pending = layers.pending;
    if pending.is_some_and(|p| p.is_node_removed(node_id)) {
      return false;
    }
    let node_mvcc = self.node_mvcc(node_id);
    if !layers.node_exists(self.snapshot, node_id, node_mvcc) {
      return false;
    }
    let mut has = match self.snapshot {
      Some(snapshot) if layers.sees_snapshot(node_id, node_mvcc) => snapshot
        .phys_node(node_id)
        .is_some_and(|phys| snapshot.node_has_label(phys, label_id)),
      _ => false,
    };
    let names = |labels: Option<&std::collections::HashSet<LabelId>>| {
      labels.is_some_and(|labels| labels.contains(&label_id))
    };
    // The committed delta and the version history, unless the transaction recreated the node
    if !layers.pending_masks(node_id) {
      if names(layers.delta.added_labels(node_id)) {
        has = true;
      }
      if names(layers.delta.removed_labels(node_id)) {
        has = false;
      }
      if let Some(history) = self
        .history
        .and_then(|vc| vc.node_label_at(node_id, label_id, self.snapshot_ts, self.txid))
      {
        has = history;
      }
    }
    if let Some(pending) = pending {
      if names(pending.added_labels(node_id)) {
        has = true;
      }
      if names(pending.removed_labels(node_id)) {
        has = false;
      }
    }
    has
  }

  /// Visit the edges of `node_id` on `side` of type `etype` (every type for
  /// `None`) after `after`, in key order, until `f` breaks: the edges
  /// `out_edges` / `in_edges` list, in their order, without listing the rest.
  /// Each source (snapshot, delta, transaction, version history) is read from
  /// the first key in range on, so the walk costs a binary search plus what it
  /// visits. `None` if the reader does not see the node (nothing was read).
  pub(super) fn for_each_edge(
    &self,
    side: EdgeSide,
    node_id: NodeId,
    etype: Option<ETypeId>,
    after: Option<(ETypeId, NodeId)>,
    mut f: impl FnMut(ETypeId, NodeId) -> ControlFlow<()>,
  ) -> Option<ControlFlow<()>> {
    let layers = self.layers;
    let pending = layers.pending;
    let delta = layers.delta;
    if pending.is_some_and(|p| p.is_node_removed(node_id)) {
      return None;
    }
    let node_mvcc = self.node_mvcc(node_id);
    if !layers.sees_pending(node_id, node_mvcc) {
      return None;
    }
    let keys = EdgeKeys { etype, after };
    let edge = |etype: ETypeId, other: NodeId| match side {
      EdgeSide::Out => (node_id, etype, other),
      EdgeSide::In => (other, etype, node_id),
    };
    let pending_deleted =
      |(src, etype, dst)| pending.is_some_and(|p| p.is_edge_deleted(src, etype, dst));

    // The snapshot's copy of the node's edges, from the first key in range
    let snapshot_edges = self
      .snapshot
      .filter(|_| layers.sees_snapshot(node_id, node_mvcc))
      .and_then(|snapshot| {
        let phys = snapshot.phys_node(node_id)?;
        let start = keys.start(|other| snapshot.phys_after(other))?;
        let mut edges = match side {
          EdgeSide::Out => snapshot.out_edge_keys(phys),
          EdgeSide::In => snapshot.in_edge_keys(phys),
        };
        edges.seek(start);
        if let Some(etype) = keys.etype {
          edges.end_after_etype(etype);
        }
        Some(edges)
      });
    let has_snapshot = snapshot_edges.is_some();
    let mut from_snapshot = snapshot_edges
      .unwrap_or_default()
      .inspect(|_| examined_edges(1))
      .filter_map(|(etype, other)| {
        if !layers.sees_snapshot(other, self.node_mvcc(other)) {
          return None;
        }
        let (src, etype, dst) = edge(etype, other);
        let edge_mvcc = self.edge_mvcc(src, etype, dst);
        let hidden = edge_mvcc == Some(false)
          || pending_deleted((src, etype, dst))
          || (edge_mvcc.is_none() && delta.is_edge_deleted(src, etype, dst));
        (!hidden).then_some((etype, other))
      });

    // Edge patches of the committed delta and of the transaction, from the first key in range
    let added = |layer: Option<&'a DeltaState>| {
      let added = layer.and_then(|layer| match side {
        EdgeSide::Out => layer.out_add.get(&node_id),
        EdgeSide::In => layer.in_add.get(&node_id),
      });
      added.zip(keys.patch_bounds())
    };
    let patches = |added: Option<(&'a std::collections::BTreeSet<EdgePatch>, _)>| {
      added
        .map(|(patches, bounds)| patches.range(bounds))
        .unwrap_or_default()
        .inspect(|_| examined_edges(1))
        .map(|patch| (patch.etype, patch.other))
    };
    let delta_added = added(layers.sees_delta(node_id, node_mvcc).then_some(delta));
    let pending_added = added(pending);
    let has_delta = delta_added.is_some();
    let has_pending = pending_added.is_some();
    let mut from_delta = patches(delta_added).filter(|&(etype, other)| {
      let (src, etype, dst) = edge(etype, other);
      layers.sees_delta(other, self.node_mvcc(other))
        && self.edge_mvcc(src, etype, dst) != Some(false)
        && !pending_deleted((src, etype, dst))
    });
    let mut from_pending = patches(pending_added)
      .filter(|&(_, other)| layers.sees_pending(other, self.node_mvcc(other)));

    // Edges deleted since the reader's snapshot remain only in their version chains
    let mut from_history: Vec<(ETypeId, NodeId)> = self
      .history
      .filter(|_| layers.sees_delta(node_id, node_mvcc))
      .map(|vc| {
        vc.node_edges_at(node_id, self.snapshot_ts, self.txid)
          .inspect(|_| examined_edges(1))
          .filter_map(|(src, etype, dst)| {
            let other = match side {
              EdgeSide::Out => (src == node_id).then_some(dst),
              EdgeSide::In => (dst == node_id).then_some(src),
            }?;
            let visible = keys.contains((etype, other))
              && !pending_deleted((src, etype, dst))
              && layers.sees_delta(other, vc.node_exists_at(other, self.snapshot_ts, self.txid));
            visible.then_some((etype, other))
          })
          .collect()
      })
      .unwrap_or_default();
    from_history.sort_unstable();

    // Most nodes have their edges in one source (the snapshot after a checkpoint, the delta
    // before one): walk it directly, and merge only when several hold edges.
    let emit = |(etype, other)| f(etype, other);
    let has_history = !from_history.is_empty();
    Some(if !has_delta && !has_pending && !has_history {
      walk_sorted(from_snapshot, emit)
    } else if !has_snapshot && !has_pending && !has_history {
      // A delta's patches are a set: each key once already.
      from_delta.try_for_each(emit)
    } else {
      merge_sorted(
        [
          &mut from_snapshot,
          &mut from_delta,
          &mut from_pending,
          &mut from_history.into_iter(),
        ],
        emit,
      )
    })
  }

  /// Room for the edges of `node_id` on `side`: its degree in the snapshot plus the edges the
  /// delta and the transaction add.
  pub(super) fn edge_capacity(&self, side: EdgeSide, node_id: NodeId) -> usize {
    let snapshot = self.snapshot.and_then(|snap| {
      let phys = snap.phys_node(node_id)?;
      match side {
        EdgeSide::Out => snap.out_degree(phys),
        EdgeSide::In => snap.in_degree(phys),
      }
    });
    let added = |layer: &DeltaState| {
      let added = match side {
        EdgeSide::Out => &layer.out_add,
        EdgeSide::In => &layer.in_add,
      };
      added.get(&node_id).map_or(0, |patches| patches.len())
    };
    snapshot
      .unwrap_or(0)
      .saturating_add(added(self.layers.delta))
      .saturating_add(self.layers.pending.map_or(0, added))
  }
}

/// `VersionChainManager::node_exists_at`, out of line: reads check it per node and per edge
/// endpoint, mostly with no history to consult, and inline the check that there is.
#[inline(never)]
fn node_exists_in_history(
  history: &VersionChainManager,
  node_id: NodeId,
  snapshot_ts: Timestamp,
  txid: TxId,
) -> Option<bool> {
  history.node_exists_at(node_id, snapshot_ts, txid)
}

/// `VersionChainManager::edge_exists_at`, out of line (see `node_exists_in_history`).
#[inline(never)]
fn edge_exists_in_history(
  history: &VersionChainManager,
  src: NodeId,
  etype: ETypeId,
  dst: NodeId,
  snapshot_ts: Timestamp,
  txid: TxId,
) -> Option<bool> {
  history.edge_exists_at(src, etype, dst, snapshot_ts, txid)
}

/// Walk a sorted source, each key once, until `f` breaks.
fn walk_sorted<K: PartialEq + Copy>(
  mut keys: impl Iterator<Item = K>,
  mut f: impl FnMut(K) -> ControlFlow<()>,
) -> ControlFlow<()> {
  let Some(first) = keys.next() else {
    return ControlFlow::Continue(());
  };
  f(first)?;
  let mut last = first;
  keys.try_for_each(|key| {
    if key == last {
      return ControlFlow::Continue(());
    }
    last = key;
    f(key)
  })
}

impl SingleFileDB {
  /// MVCC visibility context `(txid, snapshot_ts)`: the transaction's snapshot inside a
  /// transaction, every commit (`Timestamp::MAX`) outside one, and `(0, 0)` with MVCC
  /// disabled. A read outside a transaction holds `delta.read()`, so every commit it can
  /// see is merged and the delta and snapshot alone answer it.
  pub(super) fn mvcc_read_ts(&self, tx: Option<&SingleFileTxState>) -> (TxId, Timestamp) {
    match (self.mvcc.as_ref(), tx) {
      (None, _) => (0, 0),
      (Some(_), Some(tx)) => (tx.txid, tx.snapshot_ts),
      (Some(_), None) => (0, Timestamp::MAX),
    }
  }

  /// The version chains, shared, when they can answer for a reader at `snapshot_ts`: only
  /// when a commit at or after it recorded history (`MvccManager::history_ts`). Call it
  /// holding `delta.read()`. A commit may record history meanwhile, but merges only once the
  /// read is done, so the delta answers for a reader that skips the chains (see the module
  /// docs).
  pub(super) fn mvcc_history(
    &self,
    snapshot_ts: Timestamp,
  ) -> Option<RwLockReadGuard<'_, VersionChainManager>> {
    let mvcc = self.mvcc.as_ref()?;
    (snapshot_ts <= mvcc.history_ts()).then(|| mvcc.version_chain.read())
  }

  /// Note reads of the calling thread's transaction `tx` for its MVCC conflict check.
  pub(super) fn record_reads(
    &self,
    tx: Option<&mut SingleFileTxState>,
    keys: impl IntoIterator<Item = TxKey>,
  ) {
    if let (Some(_), Some(tx)) = (self.mvcc.as_ref(), tx) {
      for key in keys {
        tx.record_read(key);
      }
    }
  }

  /// `record_reads` for a transaction whose state is not locked yet.
  pub(super) fn record_handle_reads(
    &self,
    tx: Option<&Arc<Mutex<SingleFileTxState>>>,
    keys: impl IntoIterator<Item = TxKey>,
  ) {
    if let (Some(_), Some(tx)) = (self.mvcc.as_ref(), tx) {
      let mut tx = tx.lock();
      for key in keys {
        tx.record_read(key);
      }
    }
  }

  /// Run `read` on the calling thread's [`ReadView`], under the read guards (taken in lock
  /// order, see the module docs). The keys `read` notes in its second argument go to the
  /// transaction's MVCC conflict check; it notes them only when `tracks_reads` is set.
  pub(super) fn read_view<R>(&self, read: impl FnOnce(&ReadView<'_>, &mut Vec<TxKey>) -> R) -> R {
    let tx_handle = self.current_tx_handle();
    let mut tx_guard = tx_handle.as_ref().map(|tx| tx.lock());
    let (txid, snapshot_ts) = self.mvcc_read_ts(tx_guard.as_deref());
    let tracks_reads = self.mvcc.is_some() && tx_guard.as_ref().is_some_and(|tx| tx.tracks_reads());
    let mut reads = Vec::new();
    let result = {
      let delta = self.delta.read();
      let snapshot = self.snapshot.read();
      let history = self.mvcc_history(snapshot_ts);
      let view = ReadView {
        layers: NodeLayers {
          pending: tx_guard.as_ref().map(|tx| &tx.pending),
          delta: &delta,
        },
        snapshot: snapshot.as_ref(),
        history: history.as_deref(),
        txid,
        snapshot_ts,
        tracks_reads,
      };
      read(&view, &mut reads)
    };
    self.record_reads(tx_guard.as_deref_mut(), reads);
    result
  }

  /// Whether a node existed at the reader's snapshot, if it changed since (see the module
  /// docs); `None` when the delta and snapshot decide. Holds `version_chain` only for the
  /// lookup.
  fn mvcc_node_visible(
    &self,
    node_id: NodeId,
    tx_snapshot_ts: Timestamp,
    txid: TxId,
  ) -> Option<bool> {
    self
      .mvcc_history(tx_snapshot_ts)?
      .node_exists_at(node_id, tx_snapshot_ts, txid)
  }

  // ========================================================================
  // Node Property Reads
  // ========================================================================

  /// Get all properties for a node
  ///
  /// Returns None if the node doesn't exist or is deleted.
  /// Merges properties from snapshot with delta modifications.
  pub fn node_props(&self, node_id: NodeId) -> Option<HashMap<PropKeyId, PropValue>> {
    let tx_handle = self.current_tx_handle();
    let mut tx_guard = tx_handle.as_ref().map(|tx| tx.lock());
    let pending = tx_guard.as_ref().map(|tx| &tx.pending);

    if pending.is_some_and(|p| p.is_node_removed(node_id)) {
      return None;
    }

    let (txid, tx_snapshot_ts) = self.mvcc_read_ts(tx_guard.as_deref());
    let delta = self.delta.read();
    let layers = NodeLayers {
      pending,
      delta: &delta,
    };

    let mut props = HashMap::new();
    let snapshot = self.snapshot.read();
    let vc_guard = self.mvcc_history(tx_snapshot_ts);
    let mvcc_node_visible = vc_guard
      .as_ref()
      .and_then(|vc| vc.node_exists_at(node_id, tx_snapshot_ts, txid));

    // Get properties from snapshot first
    if let Some(ref snap) = *snapshot {
      if let Some(phys) = snap
        .phys_node(node_id)
        .filter(|_| layers.sees_snapshot(node_id, mvcc_node_visible))
      {
        if let Some(snapshot_props) = snap.node_props(phys) {
          props = snapshot_props;
        }
      }
    }

    // Apply committed delta modifications
    if let Some(node_delta) = delta
      .node_delta(node_id)
      .filter(|_| !layers.pending_masks(node_id))
    {
      if let Some(ref delta_props) = node_delta.props {
        props.reserve(delta_props.len());
        for (&key_id, value) in delta_props {
          match value {
            Some(v) => {
              props.insert(key_id, v.as_ref().clone());
            }
            None => {
              props.remove(&key_id);
            }
          }
        }
      }
    }

    // What changed since the reader's snapshot, unless the transaction recreated the node
    if let Some(vc) = vc_guard.as_ref().filter(|_| !layers.pending_masks(node_id)) {
      for key_id in vc.node_prop_keys(node_id) {
        if let Some(value) = vc.node_prop_at(node_id, key_id, tx_snapshot_ts, txid) {
          match value {
            Some(v) => {
              props.insert(key_id, v.as_ref().clone());
            }
            None => {
              props.remove(&key_id);
            }
          }
        }
      }
    }
    drop(vc_guard);

    // Apply pending modifications (overlay)
    if let Some(pending_delta) = pending {
      if let Some(node_delta) = pending_delta.node_delta(node_id) {
        if let Some(ref delta_props) = node_delta.props {
          props.reserve(delta_props.len());
          for (&key_id, value) in delta_props {
            match value {
              Some(v) => {
                props.insert(key_id, v.as_ref().clone());
              }
              None => {
                props.remove(&key_id);
              }
            }
          }
        }
      }
    }

    if !layers.node_exists(snapshot.as_ref(), node_id, mvcc_node_visible) {
      return None;
    }

    self.record_reads(
      tx_guard.as_deref_mut(),
      props
        .keys()
        .map(|&key_id| TxKey::NodeProp { node_id, key_id }),
    );

    Some(props)
  }

  /// Get a specific property for a node
  ///
  /// Returns None if the node doesn't exist, is deleted, or doesn't have the property.
  pub fn node_prop(&self, node_id: NodeId, key_id: PropKeyId) -> Option<PropValue> {
    let tx_handle = self.current_tx_handle();
    let mut tx_guard = tx_handle.as_ref().map(|tx| tx.lock());
    if let Some(tx) = tx_guard.as_deref() {
      // The transaction's own copy, looked up once.
      let created = tx.pending.created_nodes.get(&node_id);
      if created.is_none() && tx.pending.is_node_deleted(node_id) {
        return None;
      }
      let node_delta = created.or_else(|| tx.pending.modified_nodes.get(&node_id));
      if let Some(value) = node_delta
        .and_then(|node_delta| node_delta.props.as_ref())
        .and_then(|props| props.get(&key_id))
      {
        return value.as_deref().cloned();
      }
      if created.is_some() {
        return None;
      }
    }
    let (txid, tx_snapshot_ts) = self.mvcc_read_ts(tx_guard.as_deref());
    self.record_reads(
      tx_guard.as_deref_mut(),
      [TxKey::NodeProp { node_id, key_id }],
    );
    drop(tx_guard);

    let delta = self.delta.read();

    let mut mvcc_node_visible = None;
    if let Some(vc) = self.mvcc_history(tx_snapshot_ts) {
      mvcc_node_visible = vc.node_exists_at(node_id, tx_snapshot_ts, txid);
      if mvcc_node_visible != Some(false) {
        if let Some(value) = vc.node_prop_at(node_id, key_id, tx_snapshot_ts, txid) {
          return value.as_deref().cloned();
        }
      }
    }

    // Check if node exists (at the reader's MVCC snapshot, if that differs). Its delta
    // state alone does not count, see `NodeLayers::node_exists`. The delta's copy of the
    // node is looked up once, for this and the props below.
    let snapshot = self.snapshot.read();
    let created = delta.created_nodes.get(&node_id);
    let exists = match mvcc_node_visible {
      Some(visible) => visible,
      None => created.is_some() || delta.snapshot_node_over(snapshot.as_ref(), node_id),
    };
    if !exists {
      return None;
    }

    // Check delta first (for modifications); `None` means explicitly deleted.
    let node_delta = created.or_else(|| delta.modified_nodes.get(&node_id));
    if let Some(value) = node_delta
      .and_then(|node_delta| node_delta.props.as_ref())
      .and_then(|props| props.get(&key_id))
    {
      return value.as_deref().cloned();
    }

    // A node created (or recreated) in the delta has no snapshot props, and a delete masks
    // them (see `NodeLayers::sees_snapshot`).
    if created.is_some() || delta.is_node_deleted(node_id) {
      return None;
    }

    // Fall back to snapshot
    if let Some(ref snap) = *snapshot {
      if let Some(phys) = snap.phys_node(node_id) {
        return snap.node_prop(phys, key_id);
      }
    }

    None
  }

  // ========================================================================
  // Edge Property Reads
  // ========================================================================

  /// Get all properties for an edge
  ///
  /// Returns None if the edge doesn't exist.
  /// Merges properties from snapshot with delta modifications.
  pub fn edge_props(
    &self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
  ) -> Option<HashMap<PropKeyId, PropValue>> {
    let tx_handle = self.current_tx_handle();
    let mut tx_guard = tx_handle.as_ref().map(|tx| tx.lock());
    let pending = tx_guard.as_ref().map(|tx| &tx.pending);

    if pending.is_some_and(|p| p.is_node_removed(src) || p.is_node_removed(dst)) {
      return None;
    }
    if pending.is_some_and(|p| p.is_edge_deleted(src, etype, dst)) {
      return None;
    }

    let (txid, tx_snapshot_ts) = self.mvcc_read_ts(tx_guard.as_deref());
    let delta = self.delta.read();
    let layers = NodeLayers {
      pending,
      delta: &delta,
    };
    // An endpoint the transaction deleted or recreated masks the committed
    // edge: only the transaction's own edge and props remain.
    let committed_masked = layers.pending_masks(src) || layers.pending_masks(dst);

    let mut mvcc_edge_visible = None;

    let mut props = HashMap::new();
    let snapshot = self.snapshot.read();
    let vc_guard = self.mvcc_history(tx_snapshot_ts);
    let node_visible = |node_id| {
      vc_guard
        .as_ref()
        .and_then(|vc| vc.node_exists_at(node_id, tx_snapshot_ts, txid))
    };
    let mvcc_src_visible = node_visible(src);
    let mvcc_dst_visible = node_visible(dst);

    // First, determine if edge exists
    let edge_added_in_delta = delta.is_edge_added(src, etype, dst);
    let edge_added_in_pending = pending.is_some_and(|p| p.is_edge_added(src, etype, dst));
    let mut edge_exists_in_snapshot = false;

    // Check snapshot for edge existence and get base properties
    if let Some(ref snap) = *snapshot {
      if let (Some(src_phys), Some(dst_phys)) = (snap.phys_node(src), snap.phys_node(dst)) {
        if layers.sees_snapshot(src, mvcc_src_visible)
          && layers.sees_snapshot(dst, mvcc_dst_visible)
        {
          if let Some(edge_idx) = snap.find_edge_index(src_phys, etype, dst_phys) {
            edge_exists_in_snapshot = true;
            // Get properties from snapshot
            if let Some(snapshot_props) = snap.edge_props(edge_idx) {
              props = snapshot_props;
            }
          }
        }
      }
    }

    // Apply committed delta modifications, then what changed since the reader's snapshot
    if !committed_masked {
      if let Some(delta_props) = delta.edge_props_delta(src, etype, dst) {
        props.reserve(delta_props.len());
        for (&key_id, value) in delta_props {
          match value {
            Some(v) => {
              props.insert(key_id, v.as_ref().clone());
            }
            None => {
              props.remove(&key_id);
            }
          }
        }
      }
    }
    if let Some(vc) = vc_guard.as_ref().filter(|_| !committed_masked) {
      mvcc_edge_visible = vc.edge_exists_at(src, etype, dst, tx_snapshot_ts, txid);
      for key_id in vc.edge_prop_keys(src, etype, dst) {
        if let Some(value) = vc.edge_prop_at(src, etype, dst, key_id, tx_snapshot_ts, txid) {
          match value {
            Some(v) => {
              props.insert(key_id, v.as_ref().clone());
            }
            None => {
              props.remove(&key_id);
            }
          }
        }
      }
    }
    drop(vc_guard);

    if committed_masked {
      if !edge_added_in_pending {
        return None;
      }
    } else {
      if mvcc_src_visible == Some(false) || mvcc_dst_visible == Some(false) {
        return None;
      }
      if mvcc_src_visible.is_none() && delta.is_node_removed(src) {
        return None;
      }
      if mvcc_dst_visible.is_none() && delta.is_node_removed(dst) {
        return None;
      }
      if mvcc_edge_visible == Some(false) {
        return None;
      }
      if mvcc_edge_visible.is_none() && delta.is_edge_deleted(src, etype, dst) {
        return None;
      }

      // Edge must exist either in delta or snapshot (unless MVCC says visible)
      if mvcc_edge_visible != Some(true)
        && !edge_added_in_delta
        && !edge_added_in_pending
        && !edge_exists_in_snapshot
      {
        return None;
      }
    }

    // Apply pending modifications
    if let Some(pending_delta) = pending {
      if let Some(delta_props) = pending_delta.edge_props_delta(src, etype, dst) {
        props.reserve(delta_props.len());
        for (&key_id, value) in delta_props {
          match value {
            Some(v) => {
              props.insert(key_id, v.as_ref().clone());
            }
            None => {
              props.remove(&key_id);
            }
          }
        }
      }
    }

    self.record_reads(
      tx_guard.as_deref_mut(),
      props.keys().map(|&key_id| TxKey::EdgeProp {
        src,
        etype,
        dst,
        key_id,
      }),
    );

    Some(props)
  }

  /// Get a specific property for an edge
  ///
  /// Returns None if the edge doesn't exist or doesn't have the property.
  pub fn edge_prop(
    &self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    key_id: PropKeyId,
  ) -> Option<PropValue> {
    let tx_handle = self.current_tx_handle();
    let mut tx_guard = tx_handle.as_ref().map(|tx| tx.lock());
    if let Some(tx) = tx_guard.as_deref() {
      if tx.pending.is_node_removed(src) || tx.pending.is_node_removed(dst) {
        return None;
      }
      if tx.pending.is_edge_deleted(src, etype, dst) {
        return None;
      }
      if let Some(delta_props) = tx.pending.edge_props_delta(src, etype, dst) {
        if let Some(value) = delta_props.get(&key_id) {
          return value.as_deref().cloned();
        }
      }
      // An endpoint the transaction recreated masks the committed edge.
      if tx.pending.is_node_deleted(src) || tx.pending.is_node_deleted(dst) {
        return None;
      }
    }
    let (txid, tx_snapshot_ts) = self.mvcc_read_ts(tx_guard.as_deref());
    self.record_reads(
      tx_guard.as_deref_mut(),
      [TxKey::EdgeProp {
        src,
        etype,
        dst,
        key_id,
      }],
    );
    drop(tx_guard);

    let delta = self.delta.read();

    let mut mvcc_src_visible = None;
    let mut mvcc_dst_visible = None;
    let mut mvcc_edge_visible = None;
    if let Some(vc) = self.mvcc_history(tx_snapshot_ts) {
      mvcc_src_visible = vc.node_exists_at(src, tx_snapshot_ts, txid);
      mvcc_dst_visible = vc.node_exists_at(dst, tx_snapshot_ts, txid);
      mvcc_edge_visible = vc.edge_exists_at(src, etype, dst, tx_snapshot_ts, txid);
      let gone = [mvcc_src_visible, mvcc_dst_visible, mvcc_edge_visible].contains(&Some(false));
      if !gone {
        if let Some(value) = vc.edge_prop_at(src, etype, dst, key_id, tx_snapshot_ts, txid) {
          return value.as_deref().cloned();
        }
      }
    }

    if mvcc_src_visible == Some(false) || mvcc_dst_visible == Some(false) {
      return None;
    }

    // Check if either node is deleted
    if mvcc_src_visible.is_none() && delta.is_node_removed(src) {
      return None;
    }
    if mvcc_dst_visible.is_none() && delta.is_node_removed(dst) {
      return None;
    }

    // Check if edge is deleted in delta
    if mvcc_edge_visible == Some(false) {
      return None;
    }
    if mvcc_edge_visible.is_none() && delta.is_edge_deleted(src, etype, dst) {
      return None;
    }

    // First, determine if edge exists at all
    let edge_added_in_delta = delta.is_edge_added(src, etype, dst);
    let edge_added_in_pending = tx_handle
      .as_ref()
      .map(|handle| handle.lock().pending.is_edge_added(src, etype, dst))
      .unwrap_or(false);
    let snapshot = self.snapshot.read();
    // The transaction's masks returned above.
    let layers = NodeLayers {
      pending: None,
      delta: &delta,
    };
    let snapshot_edge = snapshot
      .as_ref()
      .filter(|_| {
        layers.sees_snapshot(src, mvcc_src_visible) && layers.sees_snapshot(dst, mvcc_dst_visible)
      })
      .and_then(|snap| {
        let (src_phys, dst_phys) = (snap.phys_node(src)?, snap.phys_node(dst)?);
        Some((snap, snap.find_edge_index(src_phys, etype, dst_phys)?))
      });
    let edge_exists_in_snapshot = snapshot_edge.is_some();

    // Edge must exist either in delta or snapshot
    if mvcc_edge_visible != Some(true)
      && !edge_added_in_delta
      && !edge_added_in_pending
      && !edge_exists_in_snapshot
    {
      return None;
    }

    // Check delta first (for modifications)
    if let Some(delta_props) = delta.edge_props_delta(src, etype, dst) {
      if let Some(value) = delta_props.get(&key_id) {
        // Some(None) means explicitly deleted
        return value.as_deref().cloned();
      }
    }

    // Fall back to snapshot
    let (snap, edge_idx) = snapshot_edge?;
    snap.edge_props(edge_idx)?.remove(&key_id)
  }

  // ========================================================================
  // Edge Traversal
  // ========================================================================

  /// Get outgoing edges for a node
  ///
  /// Returns edges as (edge_type_id, destination_node_id) pairs.
  /// Merges edges from snapshot with delta additions/deletions.
  /// Filters out edges to deleted nodes.
  pub fn out_edges(&self, node_id: NodeId) -> Vec<(ETypeId, NodeId)> {
    let tx_handle = self.current_tx_handle();
    let mut tx_guard = tx_handle.as_ref().map(|tx| tx.lock());
    let pending = tx_guard.as_ref().map(|tx| &tx.pending);

    // If node is deleted, no edges
    if pending.is_some_and(|p| p.is_node_removed(node_id)) {
      return Vec::new();
    }

    let (txid, tx_snapshot_ts) = self.mvcc_read_ts(tx_guard.as_deref());
    let delta = self.delta.read();
    let snapshot = self.snapshot.read();
    let vc_guard = self.mvcc_history(tx_snapshot_ts);
    let layers = NodeLayers {
      pending,
      delta: &delta,
    };

    // If node is deleted in committed state, no edges
    let node_visible = vc_guard
      .as_ref()
      .and_then(|vc| vc.node_exists_at(node_id, tx_snapshot_ts, txid));
    if !layers.sees_pending(node_id, node_visible) {
      return Vec::new();
    }

    let mut capacity = 0usize;
    if let Some(ref snap) = *snapshot {
      if let Some(phys) = snap.phys_node(node_id) {
        capacity = capacity.saturating_add(snap.out_degree(phys).unwrap_or(0));
      }
    }
    if let Some(added_edges) = delta.out_add.get(&node_id) {
      capacity = capacity.saturating_add(added_edges.len());
    }
    if let Some(added_edges) = pending.and_then(|p| p.out_add.get(&node_id)) {
      capacity = capacity.saturating_add(added_edges.len());
    }
    let mut edges = Vec::with_capacity(capacity);

    // Get edges from snapshot
    if let Some(ref snap) = *snapshot {
      if let Some(phys) = snap
        .phys_node(node_id)
        .filter(|_| layers.sees_snapshot(node_id, node_visible))
      {
        for (dst_phys, etype) in snap.iter_out_edges(phys) {
          examined_edges(1);
          // Convert physical dst to NodeId
          if let Some(dst_node_id) = snap.node_id(dst_phys) {
            // Skip edges to deleted nodes
            let dst_visible = vc_guard
              .as_ref()
              .and_then(|vc| vc.node_exists_at(dst_node_id, tx_snapshot_ts, txid));
            if !layers.sees_snapshot(dst_node_id, dst_visible) {
              continue;
            }
            // Skip edges deleted in delta
            let edge_visible = vc_guard
              .as_ref()
              .and_then(|vc| vc.edge_exists_at(node_id, etype, dst_node_id, tx_snapshot_ts, txid));
            if edge_visible == Some(false)
              || pending.is_some_and(|p| p.is_edge_deleted(node_id, etype, dst_node_id))
              || (edge_visible.is_none() && delta.is_edge_deleted(node_id, etype, dst_node_id))
            {
              continue;
            }
            edges.push((etype, dst_node_id));
          }
        }
      }
    }

    // Add edges from delta
    if let Some(added_edges) = delta
      .out_add
      .get(&node_id)
      .filter(|_| layers.sees_delta(node_id, node_visible))
    {
      examined_edges(added_edges.len());
      for edge_patch in added_edges {
        // Skip edges to deleted nodes
        let dst_visible = vc_guard
          .as_ref()
          .and_then(|vc| vc.node_exists_at(edge_patch.other, tx_snapshot_ts, txid));
        if !layers.sees_delta(edge_patch.other, dst_visible) {
          continue;
        }
        let edge_visible = vc_guard.as_ref().and_then(|vc| {
          vc.edge_exists_at(
            node_id,
            edge_patch.etype,
            edge_patch.other,
            tx_snapshot_ts,
            txid,
          )
        });
        if edge_visible == Some(false)
          || pending.is_some_and(|p| p.is_edge_deleted(node_id, edge_patch.etype, edge_patch.other))
        {
          continue;
        }
        edges.push((edge_patch.etype, edge_patch.other));
      }
    }

    if let Some(added_edges) = pending.and_then(|p| p.out_add.get(&node_id)) {
      examined_edges(added_edges.len());
      for edge_patch in added_edges {
        let dst_visible = vc_guard
          .as_ref()
          .and_then(|vc| vc.node_exists_at(edge_patch.other, tx_snapshot_ts, txid));
        if !layers.sees_pending(edge_patch.other, dst_visible) {
          continue;
        }
        edges.push((edge_patch.etype, edge_patch.other));
      }
    }

    // Edges deleted since the reader's snapshot remain only in their version chains
    if let Some(vc) = vc_guard
      .as_ref()
      .filter(|_| layers.sees_delta(node_id, node_visible))
    {
      for (src, etype, dst) in vc.node_edges_at(node_id, tx_snapshot_ts, txid) {
        examined_edges(1);
        if src != node_id || pending.is_some_and(|p| p.is_edge_deleted(src, etype, dst)) {
          continue;
        }
        if !layers.sees_delta(dst, vc.node_exists_at(dst, tx_snapshot_ts, txid)) {
          continue;
        }
        edges.push((etype, dst));
      }
    }

    // Sort by (etype, dst) for consistent ordering
    edges.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    edges.dedup();

    drop(vc_guard);
    self.record_reads(
      tx_guard.as_deref_mut(),
      [TxKey::NeighborsOut {
        node_id,
        etype: None,
      }],
    );

    edges
  }

  /// Get incoming edges for a node
  ///
  /// Returns edges as (edge_type_id, source_node_id) pairs.
  /// Merges edges from snapshot with delta additions/deletions.
  /// Filters out edges from deleted nodes.
  pub fn in_edges(&self, node_id: NodeId) -> Vec<(ETypeId, NodeId)> {
    let tx_handle = self.current_tx_handle();
    let mut tx_guard = tx_handle.as_ref().map(|tx| tx.lock());
    let pending = tx_guard.as_ref().map(|tx| &tx.pending);

    // If node is deleted, no edges
    if pending.is_some_and(|p| p.is_node_removed(node_id)) {
      return Vec::new();
    }

    let (txid, tx_snapshot_ts) = self.mvcc_read_ts(tx_guard.as_deref());
    let delta = self.delta.read();
    let snapshot = self.snapshot.read();
    let vc_guard = self.mvcc_history(tx_snapshot_ts);
    let layers = NodeLayers {
      pending,
      delta: &delta,
    };

    // If node is deleted, no edges
    let node_visible = vc_guard
      .as_ref()
      .and_then(|vc| vc.node_exists_at(node_id, tx_snapshot_ts, txid));
    if !layers.sees_pending(node_id, node_visible) {
      return Vec::new();
    }

    let mut capacity = 0usize;
    if let Some(ref snap) = *snapshot {
      if let Some(phys) = snap.phys_node(node_id) {
        capacity = capacity.saturating_add(snap.in_degree(phys).unwrap_or(0));
      }
    }
    if let Some(added_edges) = delta.in_add.get(&node_id) {
      capacity = capacity.saturating_add(added_edges.len());
    }
    if let Some(added_edges) = pending.and_then(|p| p.in_add.get(&node_id)) {
      capacity = capacity.saturating_add(added_edges.len());
    }
    let mut edges = Vec::with_capacity(capacity);

    // Get edges from snapshot
    if let Some(ref snap) = *snapshot {
      if let Some(phys) = snap
        .phys_node(node_id)
        .filter(|_| layers.sees_snapshot(node_id, node_visible))
      {
        for (src_phys, etype, _out_index) in snap.iter_in_edges(phys) {
          examined_edges(1);
          // Convert physical src to NodeId
          if let Some(src_node_id) = snap.node_id(src_phys) {
            // Skip edges from deleted nodes
            let src_visible = vc_guard
              .as_ref()
              .and_then(|vc| vc.node_exists_at(src_node_id, tx_snapshot_ts, txid));
            if !layers.sees_snapshot(src_node_id, src_visible) {
              continue;
            }
            // Skip edges deleted in delta
            let edge_visible = vc_guard
              .as_ref()
              .and_then(|vc| vc.edge_exists_at(src_node_id, etype, node_id, tx_snapshot_ts, txid));
            if edge_visible == Some(false)
              || pending.is_some_and(|p| p.is_edge_deleted(src_node_id, etype, node_id))
              || (edge_visible.is_none() && delta.is_edge_deleted(src_node_id, etype, node_id))
            {
              continue;
            }
            edges.push((etype, src_node_id));
          }
        }
      }
    }

    // Add edges from delta (in_add stores patches where other=src)
    if let Some(added_edges) = delta
      .in_add
      .get(&node_id)
      .filter(|_| layers.sees_delta(node_id, node_visible))
    {
      examined_edges(added_edges.len());
      for edge_patch in added_edges {
        // Skip edges from deleted nodes
        let src_visible = vc_guard
          .as_ref()
          .and_then(|vc| vc.node_exists_at(edge_patch.other, tx_snapshot_ts, txid));
        if !layers.sees_delta(edge_patch.other, src_visible) {
          continue;
        }
        let edge_visible = vc_guard.as_ref().and_then(|vc| {
          vc.edge_exists_at(
            edge_patch.other,
            edge_patch.etype,
            node_id,
            tx_snapshot_ts,
            txid,
          )
        });
        if edge_visible == Some(false)
          || pending.is_some_and(|p| p.is_edge_deleted(edge_patch.other, edge_patch.etype, node_id))
        {
          continue;
        }
        edges.push((edge_patch.etype, edge_patch.other));
      }
    }

    if let Some(added_edges) = pending.and_then(|p| p.in_add.get(&node_id)) {
      examined_edges(added_edges.len());
      for edge_patch in added_edges {
        let src_visible = vc_guard
          .as_ref()
          .and_then(|vc| vc.node_exists_at(edge_patch.other, tx_snapshot_ts, txid));
        if !layers.sees_pending(edge_patch.other, src_visible) {
          continue;
        }
        edges.push((edge_patch.etype, edge_patch.other));
      }
    }

    // Edges deleted since the reader's snapshot remain only in their version chains
    if let Some(vc) = vc_guard
      .as_ref()
      .filter(|_| layers.sees_delta(node_id, node_visible))
    {
      for (src, etype, dst) in vc.node_edges_at(node_id, tx_snapshot_ts, txid) {
        examined_edges(1);
        if dst != node_id || pending.is_some_and(|p| p.is_edge_deleted(src, etype, dst)) {
          continue;
        }
        if !layers.sees_delta(src, vc.node_exists_at(src, tx_snapshot_ts, txid)) {
          continue;
        }
        edges.push((etype, src));
      }
    }

    // Sort by (etype, src) for consistent ordering
    edges.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    edges.dedup();

    drop(vc_guard);
    self.record_reads(
      tx_guard.as_deref_mut(),
      [TxKey::NeighborsIn {
        node_id,
        etype: None,
      }],
    );

    edges
  }

  /// Get out-degree (number of outgoing edges) for a node
  pub fn out_degree(&self, node_id: NodeId) -> usize {
    self.out_edges(node_id).len()
  }

  /// Get in-degree (number of incoming edges) for a node
  pub fn in_degree(&self, node_id: NodeId) -> usize {
    self.in_edges(node_id).len()
  }

  /// Up to `limit` of `node_id`'s out-edges of type `etype` (every type for
  /// `None`) that come after `after` in `(etype, dst)` order, as `(etype, dst)`:
  /// a slice of [`Self::out_edges`] (filtered by `etype`) that costs a binary
  /// search plus the edges returned, however many edges the node has. Pass the
  /// last edge of one slice as `after` to get the next (`usize::MAX` gets them
  /// all).
  pub fn out_edges_after(
    &self,
    node_id: NodeId,
    etype: Option<ETypeId>,
    after: Option<(ETypeId, NodeId)>,
    limit: usize,
  ) -> Vec<(ETypeId, NodeId)> {
    self.node_edges_after(EdgeSide::Out, node_id, etype, after, limit)
  }

  /// [`Self::out_edges_after`] for in-edges: up to `limit` of `node_id`'s
  /// in-edges of type `etype` after `after` in `(etype, src)` order, as
  /// `(etype, src)`.
  pub fn in_edges_after(
    &self,
    node_id: NodeId,
    etype: Option<ETypeId>,
    after: Option<(ETypeId, NodeId)>,
    limit: usize,
  ) -> Vec<(ETypeId, NodeId)> {
    self.node_edges_after(EdgeSide::In, node_id, etype, after, limit)
  }

  fn node_edges_after(
    &self,
    side: EdgeSide,
    node_id: NodeId,
    etype: Option<ETypeId>,
    after: Option<(ETypeId, NodeId)>,
    limit: usize,
  ) -> Vec<(ETypeId, NodeId)> {
    if etype.is_none() && after.is_none() && limit == usize::MAX {
      // Every edge: the full listing reads each source front to back.
      return match side {
        EdgeSide::Out => self.out_edges(node_id),
        EdgeSide::In => self.in_edges(node_id),
      };
    }
    self.read_view(|view, reads| {
      let mut edges = Vec::with_capacity(limit.min(view.edge_capacity(side, node_id)));
      let visible = limit > 0
        && view
          .for_each_edge(side, node_id, etype, after, |etype, other| {
            edges.push((etype, other));
            if edges.len() >= limit {
              ControlFlow::Break(())
            } else {
              ControlFlow::Continue(())
            }
          })
          .is_some();
      if view.tracks_reads {
        let key = |etype| match side {
          EdgeSide::Out => TxKey::NeighborsOut { node_id, etype },
          EdgeSide::In => TxKey::NeighborsIn { node_id, etype },
        };
        if visible {
          reads.push(key(None));
        }
        if etype.is_some() {
          reads.push(key(etype));
        }
      }
      edges
    })
  }

  /// Get neighbors via outgoing edges of a specific type
  ///
  /// Returns destination node IDs for edges of the given type.
  pub fn out_neighbors(&self, node_id: NodeId, etype: ETypeId) -> Vec<NodeId> {
    self
      .out_edges_after(node_id, Some(etype), None, usize::MAX)
      .into_iter()
      .map(|(_, dst)| dst)
      .collect()
  }

  /// Get neighbors via incoming edges of a specific type
  ///
  /// Returns source node IDs for edges of the given type.
  pub fn in_neighbors(&self, node_id: NodeId, etype: ETypeId) -> Vec<NodeId> {
    self
      .in_edges_after(node_id, Some(etype), None, usize::MAX)
      .into_iter()
      .map(|(_, src)| src)
      .collect()
  }

  /// Check if there are any outgoing edges of a specific type
  pub fn has_out_edges(&self, node_id: NodeId, etype: ETypeId) -> bool {
    !self
      .out_edges_after(node_id, Some(etype), None, 1)
      .is_empty()
  }

  /// Check if there are any incoming edges of a specific type
  pub fn has_in_edges(&self, node_id: NodeId, etype: ETypeId) -> bool {
    !self
      .in_edges_after(node_id, Some(etype), None, 1)
      .is_empty()
  }

  // ========================================================================
  // Node Label Reads
  // ========================================================================

  /// Check if a node has a specific label
  pub fn node_has_label(&self, node_id: NodeId, label_id: LabelId) -> bool {
    let tx_handle = self.current_tx_handle();
    let mut tx_guard = tx_handle.as_ref().map(|tx| tx.lock());
    let pending = tx_guard.as_ref().map(|tx| &tx.pending);

    if pending.is_some_and(|p| p.is_node_removed(node_id)) {
      return false;
    }
    if pending.is_some_and(|p| p.is_label_removed(node_id, label_id)) {
      return false;
    }
    if pending.is_some_and(|p| p.is_label_added(node_id, label_id)) {
      return true;
    }
    // A node the transaction created (or recreated) has only the labels it added.
    if pending.is_some_and(|p| p.is_node_created(node_id)) {
      return false;
    }

    let (txid, tx_snapshot_ts) = self.mvcc_read_ts(tx_guard.as_deref());
    let has_label = self.committed_node_has_label(node_id, label_id, tx_snapshot_ts, txid);

    self.record_reads(
      tx_guard.as_deref_mut(),
      [
        TxKey::NodeLabels(node_id),
        TxKey::NodeLabel { node_id, label_id },
      ],
    );
    has_label
  }

  /// Label membership in committed state (MVCC versions, delta, snapshot), ignoring the
  /// current transaction's pending changes.
  fn committed_node_has_label(
    &self,
    node_id: NodeId,
    label_id: LabelId,
    tx_snapshot_ts: Timestamp,
    txid: TxId,
  ) -> bool {
    let delta = self.delta.read();

    let mut node_visible = None;
    if let Some(vc) = self.mvcc_history(tx_snapshot_ts) {
      node_visible = vc.node_exists_at(node_id, tx_snapshot_ts, txid);
      if node_visible == Some(false) {
        return false;
      }
      if let Some(has_label) = vc.node_label_at(node_id, label_id, tx_snapshot_ts, txid) {
        return has_label;
      }
    }

    // Check if node exists; its delta state alone does not count (see
    // `NodeLayers::node_exists`)
    let snapshot = self.snapshot.read();
    if node_visible.is_none() && !delta.node_exists_over(snapshot.as_ref(), node_id) {
      return false;
    }

    // Check if label was removed in delta
    if delta.is_label_removed(node_id, label_id) {
      return false;
    }

    // Check if label was added in delta
    if delta.is_label_added(node_id, label_id) {
      return true;
    }

    // A node created (or recreated) in the delta has no snapshot labels, and a delete masks
    // them (see `NodeLayers::sees_snapshot`).
    if delta.is_node_created(node_id) || delta.is_node_deleted(node_id) {
      return false;
    }

    // Check snapshot for label (if present)
    if let Some(ref snapshot) = *snapshot {
      if let Some(phys) = snapshot.phys_node(node_id) {
        if let Some(labels) = snapshot.node_labels(phys) {
          return labels.contains(&label_id);
        }
      }
    }

    false
  }

  /// Get all labels for a node
  pub fn node_labels(&self, node_id: NodeId) -> Vec<LabelId> {
    node_lookup();
    let tx_handle = self.current_tx_handle();
    let mut tx_guard = tx_handle.as_ref().map(|tx| tx.lock());
    let pending = tx_guard.as_ref().map(|tx| &tx.pending);

    if pending.is_some_and(|p| p.is_node_removed(node_id)) {
      return Vec::new();
    }

    let (txid, tx_snapshot_ts) = self.mvcc_read_ts(tx_guard.as_deref());
    let delta = self.delta.read();
    let snapshot = self.snapshot.read();
    let vc_guard = self.mvcc_history(tx_snapshot_ts);
    let layers = NodeLayers {
      pending,
      delta: &delta,
    };

    // Check if node is deleted
    let node_visible = vc_guard
      .as_ref()
      .and_then(|vc| vc.node_exists_at(node_id, tx_snapshot_ts, txid));
    if !layers.node_exists(snapshot.as_ref(), node_id, node_visible) {
      return Vec::new();
    }

    let mut labels = std::collections::HashSet::new();

    // Load labels from snapshot first (if present)
    if let Some(ref snapshot) = *snapshot {
      if let Some(phys) = snapshot
        .phys_node(node_id)
        .filter(|_| layers.sees_snapshot(node_id, node_visible))
      {
        if let Some(snapshot_labels) = snapshot.node_labels(phys) {
          labels.extend(snapshot_labels);
        }
      }
    }

    // The committed delta and MVCC labels, unless the transaction recreated
    // the node.
    let committed = !layers.pending_masks(node_id);

    // Add labels from committed delta
    if let Some(added) = delta.added_labels(node_id).filter(|_| committed) {
      labels.extend(added.iter().copied());
    }

    // Remove labels deleted in committed delta
    if let Some(removed) = delta.removed_labels(node_id).filter(|_| committed) {
      for &label_id in removed {
        labels.remove(&label_id);
      }
    }

    if let Some(vc) = vc_guard.as_ref().filter(|_| committed) {
      for label_id in vc.node_label_keys(node_id) {
        match vc.node_label_at(node_id, label_id, tx_snapshot_ts, txid) {
          Some(true) => {
            labels.insert(label_id);
          }
          Some(false) => {
            labels.remove(&label_id);
          }
          None => {}
        }
      }
    }
    drop(vc_guard);

    // Apply pending label changes
    if let Some(pending_delta) = pending {
      if let Some(added) = pending_delta.added_labels(node_id) {
        labels.extend(added.iter().copied());
      }
      if let Some(removed) = pending_delta.removed_labels(node_id) {
        for &label_id in removed {
          labels.remove(&label_id);
        }
      }
    }

    let mut result: Vec<_> = labels.into_iter().collect();
    result.sort_unstable();
    self.record_reads(
      tx_guard.as_deref_mut(),
      std::iter::once(TxKey::NodeLabels(node_id)).chain(
        result
          .iter()
          .map(|&label_id| TxKey::NodeLabel { node_id, label_id }),
      ),
    );
    result
  }

  // ========================================================================
  // Key Lookups
  // ========================================================================

  /// Look up a node by its key
  ///
  /// Returns the NodeId if found, None otherwise.
  /// Checks delta key index first, then falls back to snapshot.
  pub fn node_by_key(&self, key: &str) -> Option<NodeId> {
    let tx_handle = self.current_tx_handle();
    let mut tx_guard = tx_handle.as_ref().map(|tx| tx.lock());
    let (txid, tx_snapshot_ts) = self.mvcc_read_ts(tx_guard.as_deref());
    self.record_reads(tx_guard.as_deref_mut(), [TxKey::Key(key.into())]);
    let pending = tx_guard.as_ref().map(|tx| &tx.pending);

    let delta = self.delta.read();

    // Check pending key index first
    if let Some(&node_id) = pending.and_then(|p| p.key_index.get(key)) {
      if pending.is_some_and(|p| p.is_node_removed(node_id)) {
        return None;
      }
      // A node the transaction created (or recreated) is its own.
      if !pending.is_some_and(|p| p.is_node_created(node_id))
        && self.mvcc_node_visible(node_id, tx_snapshot_ts, txid) == Some(false)
      {
        return None;
      }
      return Some(node_id);
    }

    // The key's owner at the reader's snapshot, if it changed since. A node
    // created after the snapshot gets no key history of its own (see
    // `mvcc_history`): a later change of the key records the node as its
    // owner from before, so the owner must exist at the snapshot too.
    if let Some(vc) = self.mvcc_history(tx_snapshot_ts) {
      let owner = vc.key_owner_at(key, tx_snapshot_ts, txid).map(|owner| {
        owner.filter(|&node_id| vc.node_exists_at(node_id, tx_snapshot_ts, txid) != Some(false))
      });
      drop(vc);
      if let Some(owner) = owner {
        return owner.filter(|&node_id| !pending.is_some_and(|p| p.is_node_deleted(node_id)));
      }
    }

    // Check committed delta key index
    if let Some(&node_id) = delta.key_index.get(key) {
      // Verify node isn't deleted
      if pending.is_some_and(|p| p.is_node_deleted(node_id)) {
        return None;
      }
      let node_visible = self.mvcc_node_visible(node_id, tx_snapshot_ts, txid);
      if node_visible == Some(false) {
        return None;
      }
      if node_visible == Some(true) || !delta.is_node_removed(node_id) {
        return Some(node_id);
      }
    }

    // Fall back to snapshot: a deleted or recreated node's snapshot key is
    // masked with the rest of its snapshot copy.
    let snapshot = self.snapshot.read();
    let layers = NodeLayers {
      pending,
      delta: &delta,
    };
    snapshot
      .as_ref()
      .and_then(|snap| snap.lookup_by_key(key))
      .filter(|&node_id| {
        let node_visible = self.mvcc_node_visible(node_id, tx_snapshot_ts, txid);
        layers.sees_snapshot(node_id, node_visible)
      })
  }

  /// Get the key for a node
  ///
  /// Returns the key string if the node has one, None otherwise.
  pub fn node_key(&self, node_id: NodeId) -> Option<String> {
    node_lookup();
    self.read_view(|view, _| view.node_key(node_id).map(str::to_owned))
  }
}

#[cfg(test)]
mod tests {
  use crate::core::single_file::open::{
    close_single_file, open_single_file, SingleFileOpenOptions,
  };
  use crate::error::KiteError;
  use std::sync::{mpsc, Arc};
  use std::thread;
  use tempfile::tempdir;

  #[test]
  fn test_mvcc_label_visibility_across_transactions() {
    let temp_dir = tempdir().expect("expected value");
    let db_path = temp_dir.path().join("test-db");
    let db = Arc::new(
      open_single_file(db_path, SingleFileOpenOptions::new().mvcc(true)).expect("expected value"),
    );

    db.begin(false).expect("expected value");
    let node_id = db.create_node(Some("n1")).expect("expected value");
    let label_id = db.define_label("Tag").expect("expected value");
    db.commit().expect("expected value");

    let (ready_tx, ready_rx) = mpsc::channel();
    let (cont_tx, cont_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let db_reader = Arc::clone(&db);
    let handle = thread::spawn(move || {
      db_reader.begin(true).expect("expected value");
      assert!(!db_reader.node_has_label(node_id, label_id));
      assert!(db_reader.node_labels(node_id).is_empty());
      ready_tx.send(()).expect("expected value");
      cont_rx.recv().expect("expected value");
      assert!(!db_reader.node_has_label(node_id, label_id));
      assert!(db_reader.node_labels(node_id).is_empty());
      db_reader.commit().expect("expected value");
      done_tx.send(()).expect("expected value");
    });

    ready_rx.recv().expect("expected value");
    db.begin(false).expect("expected value");
    db.add_node_label(node_id, label_id)
      .expect("expected value");
    db.commit().expect("expected value");
    cont_tx.send(()).expect("expected value");
    done_rx.recv().expect("expected value");
    handle.join().expect("expected value");

    db.begin(true).expect("expected value");
    assert!(db.node_has_label(node_id, label_id));
    let labels = db.node_labels(node_id);
    assert!(labels.contains(&label_id));
    db.commit().expect("expected value");

    let (ready_tx2, ready_rx2) = mpsc::channel();
    let (cont_tx2, cont_rx2) = mpsc::channel();
    let (done_tx2, done_rx2) = mpsc::channel();
    let db_reader2 = Arc::clone(&db);
    let handle2 = thread::spawn(move || {
      db_reader2.begin(true).expect("expected value");
      assert!(db_reader2.node_has_label(node_id, label_id));
      assert!(db_reader2.node_labels(node_id).contains(&label_id));
      ready_tx2.send(()).expect("expected value");
      cont_rx2.recv().expect("expected value");
      assert!(db_reader2.node_has_label(node_id, label_id));
      assert!(db_reader2.node_labels(node_id).contains(&label_id));
      db_reader2.commit().expect("expected value");
      done_tx2.send(()).expect("expected value");
    });

    ready_rx2.recv().expect("expected value");
    db.begin(false).expect("expected value");
    db.remove_node_label(node_id, label_id)
      .expect("expected value");
    db.commit().expect("expected value");
    cont_tx2.send(()).expect("expected value");
    done_rx2.recv().expect("expected value");
    handle2.join().expect("expected value");

    db.begin(true).expect("expected value");
    assert!(!db.node_has_label(node_id, label_id));
    assert!(!db.node_labels(node_id).contains(&label_id));
    db.commit().expect("expected value");

    let db = match Arc::try_unwrap(db) {
      Ok(db) => db,
      Err(_) => panic!("single owner"),
    };
    close_single_file(db).expect("expected value");
  }

  #[test]
  fn test_mvcc_neighbor_read_conflicts_with_edge_write() {
    let temp_dir = tempdir().expect("expected value");
    let db_path = temp_dir.path().join("test-db");
    let db = Arc::new(
      open_single_file(db_path, SingleFileOpenOptions::new().mvcc(true)).expect("expected value"),
    );

    db.begin(false).expect("expected value");
    let src = db.create_node(Some("src")).expect("expected value");
    let dst = db.create_node(Some("dst")).expect("expected value");
    db.commit().expect("expected value");

    let (ready_tx, ready_rx) = mpsc::channel();
    let (cont_tx, cont_rx) = mpsc::channel();
    let db_reader = Arc::clone(&db);
    let handle = thread::spawn(move || {
      db_reader.begin(false).expect("expected value");
      let edges = db_reader.out_edges(src);
      assert!(edges.is_empty());
      ready_tx.send(()).expect("expected value");
      cont_rx.recv().expect("expected value");
      let result = db_reader.commit();
      match result {
        Err(KiteError::Conflict { keys, .. }) => {
          assert!(keys
            .iter()
            .any(|key| key == &format!("neighbors_out:{src}:*")));
        }
        other => panic!("expected conflict, got {other:?}"),
      }
    });

    ready_rx.recv().expect("expected value");
    db.begin(false).expect("expected value");
    db.add_edge_by_name(src, "Rel", dst)
      .expect("expected value");
    db.commit().expect("expected value");
    cont_tx.send(()).expect("expected value");
    handle.join().expect("expected value");

    let db = match Arc::try_unwrap(db) {
      Ok(db) => db,
      Err(_) => panic!("single owner"),
    };
    close_single_file(db).expect("expected value");
  }
}
