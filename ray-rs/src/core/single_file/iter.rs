//! Node iteration and statistics for SingleFileDB
//!
//! Provides iterators over nodes and database statistics.

use crate::mvcc::visibility::{edge_exists as mvcc_edge_exists, node_exists as mvcc_node_exists};
use crate::types::*;
use std::collections::HashSet;

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
    let mut txid = 0;
    let mut tx_snapshot_ts = 0;
    let vc_guard = if let Some(mvcc) = db.mvcc.as_ref() {
      if let Some(tx) = tx_guard.as_ref() {
        txid = tx.txid;
        tx_snapshot_ts = tx.snapshot_ts;
      } else {
        tx_snapshot_ts = mvcc.tx_manager.lock().next_commit_ts();
      }
      Some(mvcc.version_chain.lock())
    } else {
      None
    };
    let delta = db.delta.read();
    let snapshot = db.snapshot.read();

    // 1. Collect nodes from snapshot (excluding deleted)
    if let Some(ref snap) = *snapshot {
      let num_nodes = snap.header.num_nodes as u32;
      for phys in 0..num_nodes {
        if let Some(node_id) = snap.node_id(phys) {
          // Skip if deleted in delta
          let node_visible = vc_guard
            .as_ref()
            .and_then(|vc| vc.node_version(node_id))
            .map(|version| mvcc_node_exists(Some(version), tx_snapshot_ts, txid));
          if pending.is_some_and(|p| p.is_node_deleted(node_id)) {
            continue;
          }
          if node_visible == Some(false) {
            continue;
          }
          if node_visible.is_none() && delta.is_node_deleted(node_id) {
            continue;
          }
          nodes.push(node_id);
        }
      }
    }

    // 2. Add nodes created in delta (excluding deleted)
    for &node_id in delta.created_nodes.keys() {
      if pending.is_some_and(|p| p.is_node_deleted(node_id)) {
        continue;
      }
      let node_visible = vc_guard
        .as_ref()
        .and_then(|vc| vc.node_version(node_id))
        .map(|version| mvcc_node_exists(Some(version), tx_snapshot_ts, txid));
      if node_visible == Some(false) {
        continue;
      }
      if node_visible.is_none() && delta.deleted_nodes.contains(&node_id) {
        continue;
      }
      nodes.push(node_id);
    }

    // 3. Add nodes created in pending (excluding deleted)
    if let Some(pending_delta) = pending {
      for &node_id in pending_delta.created_nodes.keys() {
        if !pending_delta.deleted_nodes.contains(&node_id) {
          nodes.push(node_id);
        }
      }
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

  /// Count total nodes in the database
  ///
  /// Optimized to avoid full iteration by using snapshot metadata
  /// and delta size adjustments.
  pub fn count_nodes(&self) -> usize {
    self.iter_nodes().len()
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
    let tx_guard = tx_handle.as_ref().map(|tx| tx.lock());
    let pending = tx_guard.as_ref().map(|tx| &tx.pending);
    let mut txid = 0;
    let mut tx_snapshot_ts = 0;
    let vc_guard = if let Some(mvcc) = self.mvcc.as_ref() {
      if let Some(tx) = tx_guard.as_ref() {
        txid = tx.txid;
        tx_snapshot_ts = tx.snapshot_ts;
      } else {
        tx_snapshot_ts = mvcc.tx_manager.lock().next_commit_ts();
      }
      Some(mvcc.version_chain.lock())
    } else {
      None
    };
    let delta = self.delta.read();
    let snapshot = self.snapshot.read();
    let mut edges = Vec::new();
    let mut read_srcs = (self.mvcc.is_some() && txid != 0).then(HashSet::<NodeId>::new);

    // From snapshot
    if let Some(ref snap) = *snapshot {
      let num_nodes = snap.header.num_nodes as u32;
      for phys in 0..num_nodes {
        if let Some(src) = snap.node_id(phys) {
          // Skip deleted nodes
          let src_visible = vc_guard
            .as_ref()
            .and_then(|vc| vc.node_version(src))
            .map(|version| mvcc_node_exists(Some(version), tx_snapshot_ts, txid));
          if src_visible == Some(false)
            || pending.is_some_and(|p| p.is_node_deleted(src))
            || (src_visible.is_none() && delta.is_node_deleted(src))
          {
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
                .and_then(|vc| vc.node_version(dst))
                .map(|version| mvcc_node_exists(Some(version), tx_snapshot_ts, txid));
              if dst_visible == Some(false)
                || pending.is_some_and(|p| p.is_node_deleted(dst))
                || (dst_visible.is_none() && delta.is_node_deleted(dst))
              {
                continue;
              }
              let edge_visible = vc_guard
                .as_ref()
                .and_then(|vc| vc.edge_version(src, etype, dst))
                .map(|version| mvcc_edge_exists(Some(version), tx_snapshot_ts, txid));
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
          .and_then(|vc| vc.node_version(src))
          .map(|version| mvcc_node_exists(Some(version), tx_snapshot_ts, txid));
        if src_visible == Some(false)
          || pending.is_some_and(|p| p.is_node_deleted(src))
          || (src_visible.is_none() && delta.is_node_deleted(src))
        {
          continue;
        }
        if let Some(ref mut srcs) = read_srcs {
          srcs.insert(src);
        }
        let dst_visible = vc_guard
          .as_ref()
          .and_then(|vc| vc.node_version(patch.other))
          .map(|version| mvcc_node_exists(Some(version), tx_snapshot_ts, txid));
        if dst_visible == Some(false)
          || pending.is_some_and(|p| p.is_node_deleted(patch.other))
          || (dst_visible.is_none() && delta.is_node_deleted(patch.other))
        {
          continue;
        }
        let edge_visible = vc_guard
          .as_ref()
          .and_then(|vc| vc.edge_version(src, patch.etype, patch.other))
          .map(|version| mvcc_edge_exists(Some(version), tx_snapshot_ts, txid));
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
            .and_then(|vc| vc.node_version(src))
            .map(|version| mvcc_node_exists(Some(version), tx_snapshot_ts, txid));
          if src_visible == Some(false)
            || pending_delta.is_node_deleted(src)
            || (src_visible.is_none() && delta.is_node_deleted(src))
          {
            continue;
          }
          if let Some(ref mut srcs) = read_srcs {
            srcs.insert(src);
          }
          let dst_visible = vc_guard
            .as_ref()
            .and_then(|vc| vc.node_version(patch.other))
            .map(|version| mvcc_node_exists(Some(version), tx_snapshot_ts, txid));
          if dst_visible == Some(false)
            || pending_delta.is_node_deleted(patch.other)
            || (dst_visible.is_none() && delta.is_node_deleted(patch.other))
          {
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

    if let (Some(mvcc), Some(srcs)) = (self.mvcc.as_ref(), read_srcs) {
      let mut tx_mgr = mvcc.tx_manager.lock();
      if let Some(filter_etype) = etype_filter {
        for src in srcs {
          tx_mgr.record_read(
            txid,
            TxKey::NeighborsOut {
              node_id: src,
              etype: Some(filter_etype),
            },
          );
        }
      } else {
        for src in srcs {
          tx_mgr.record_read(
            txid,
            TxKey::NeighborsOut {
              node_id: src,
              etype: None,
            },
          );
        }
      }
    }

    edges
  }

  /// Get database statistics
  pub fn stats(&self) -> DbStats {
    let delta = self.delta.read();
    let snapshot = self.snapshot.read();
    let header = self.header.read();

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
      snapshot_gen: header.active_snapshot_gen,
      snapshot_nodes,
      snapshot_edges,
      snapshot_max_node_id,
      delta_nodes_created: delta.created_nodes.len(),
      delta_nodes_deleted: delta.deleted_nodes.len(),
      delta_edges_added: delta.total_edges_added(),
      delta_edges_deleted: delta.total_edges_deleted(),
      wal_segment: 0, // Not applicable for single-file
      wal_bytes: self.wal_stats().used,
      recommend_compact: self.should_checkpoint(0.8),
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
