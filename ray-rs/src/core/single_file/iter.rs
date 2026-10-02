//! Node iteration and statistics for SingleFileDB
//!
//! Provides iterators over nodes and database statistics.

use crate::types::*;
use std::collections::HashSet;

use super::read::NodeLayers;
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
/// For very large databases, consider using `list_nodes()` with chunking.
pub struct NodeIterator {
  nodes: Vec<NodeId>,
  index: usize,
}

impl NodeIterator {
  pub(crate) fn new(db: &SingleFileDB) -> Self {
    let mut nodes = Vec::new();
    let tx_handle = db.current_tx_handle();
    let tx_guard = tx_handle.as_ref().map(|tx| tx.lock());
    let pending = tx_guard.as_ref().map(|tx| &tx.pending);
    // Lock order: see read.rs.
    let (txid, tx_snapshot_ts) = db.mvcc_read_ts(tx_guard.as_deref());
    let delta = db.delta.read();
    let snapshot = db.snapshot.read();
    let vc_guard = db.mvcc_history(tx_snapshot_ts);
    let layers = NodeLayers {
      pending,
      delta: &delta,
    };

    // 1. Collect nodes from snapshot (excluding deleted ones; a recreated
    // node is listed with the delta's nodes)
    if let Some(ref snap) = *snapshot {
      let num_nodes = snap.header.num_nodes as u32;
      for phys in 0..num_nodes {
        if let Some(node_id) = snap.node_id(phys) {
          // Skip if deleted in delta
          let node_visible = vc_guard
            .as_ref()
            .and_then(|vc| vc.node_exists_at(node_id, tx_snapshot_ts, txid));
          if !layers.sees_snapshot(node_id, node_visible) {
            continue;
          }
          nodes.push(node_id);
        }
      }
    }

    // 2. Add nodes created in delta (excluding deleted)
    for &node_id in delta.created_nodes.keys() {
      let node_visible = vc_guard
        .as_ref()
        .and_then(|vc| vc.node_exists_at(node_id, tx_snapshot_ts, txid));
      if !layers.sees_delta(node_id, node_visible) {
        continue;
      }
      nodes.push(node_id);
    }

    // 3. Add nodes created (or recreated) in pending
    if let Some(pending_delta) = pending {
      nodes.extend(pending_delta.created_nodes.keys().copied());
    }

    // 4. Add nodes deleted since the reader's snapshot: only their version chains hold them
    if let Some(vc) = vc_guard.as_ref() {
      nodes.extend(
        vc.nodes_at(tx_snapshot_ts, txid)
          .filter(|&node_id| !layers.pending_masks(node_id)),
      );
    }

    // Sort for consistent ordering
    nodes.sort_unstable();
    nodes.dedup();

    Self { nodes, index: 0 }
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
    // once, unless listed from the snapshot as well.
    let maybe_added: HashSet<NodeId> = delta
      .created_nodes
      .keys()
      .chain(pending_created)
      .chain(&chained)
      .copied()
      .collect();
    for node_id in maybe_added {
      let mvcc = mvcc_visible(node_id);
      if from_elsewhere(node_id, mvcc) && !from_snapshot(node_id, mvcc) {
        count += 1;
      }
    }
    count
  }

  /// Count total edges in the database
  ///
  /// Note: This may be slow for large graphs as it needs to iterate.
  pub fn count_edges(&self) -> usize {
    self.list_edges(None).len()
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
    if let Some(ref snap) = *snapshot {
      let num_nodes = snap.header.num_nodes as u32;
      for phys in 0..num_nodes {
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

    let (snapshot_nodes, snapshot_edges, snapshot_max_node_id) = if let Some(ref snap) = *snapshot {
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
