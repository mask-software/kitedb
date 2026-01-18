//! Node iteration and statistics for SingleFileDB
//!
//! Provides iterators over nodes and database statistics.

use crate::types::*;

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
    let delta = db.delta.read();
    let snapshot = db.snapshot.read();

    // 1. Collect nodes from snapshot (excluding deleted)
    if let Some(ref snap) = *snapshot {
      let num_nodes = snap.header.num_nodes as u32;
      for phys in 0..num_nodes {
        if let Some(node_id) = snap.get_node_id(phys) {
          // Skip if deleted in delta
          if !delta.is_node_deleted(node_id) {
            nodes.push(node_id);
          }
        }
      }
    }

    // 2. Add nodes created in delta (excluding deleted)
    for &node_id in delta.created_nodes.keys() {
      if !delta.deleted_nodes.contains(&node_id) {
        nodes.push(node_id);
      }
    }

    // Sort for consistent ordering
    nodes.sort_unstable();

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
    let delta = self.delta.read();
    let snapshot = self.snapshot.read();

    // Start with snapshot count
    let mut count = if let Some(ref snap) = *snapshot {
      snap.header.num_nodes as usize
    } else {
      0
    };

    // Subtract snapshot nodes that were deleted in delta
    for &node_id in &delta.deleted_nodes {
      // Only subtract if it was a snapshot node (not a delta-created node)
      if !delta.created_nodes.contains_key(&node_id) {
        if let Some(ref snap) = *snapshot {
          if snap.get_phys_node(node_id).is_some() {
            count = count.saturating_sub(1);
          }
        }
      }
    }

    // Add delta created nodes (that weren't deleted)
    for &node_id in delta.created_nodes.keys() {
      if !delta.deleted_nodes.contains(&node_id) {
        count += 1;
      }
    }

    count
  }

  /// Count total edges in the database
  ///
  /// Note: This may be slow for large graphs as it needs to iterate.
  pub fn count_edges(&self) -> usize {
    let delta = self.delta.read();
    let snapshot = self.snapshot.read();

    // Start with snapshot edge count
    let mut count = if let Some(ref snap) = *snapshot {
      snap.header.num_edges as usize
    } else {
      0
    };

    // Subtract deleted edges
    count = count.saturating_sub(delta.total_edges_deleted());

    // Add new edges
    count += delta.total_edges_added();

    count
  }

  /// Count edges of a specific type
  pub fn count_edges_by_type(&self, etype: ETypeId) -> usize {
    self.list_edges(Some(etype)).len()
  }

  /// List all edges in the database
  ///
  /// Optionally filter by edge type.
  pub fn list_edges(&self, etype_filter: Option<ETypeId>) -> Vec<FullEdge> {
    let delta = self.delta.read();
    let snapshot = self.snapshot.read();
    let mut edges = Vec::new();

    // From snapshot
    if let Some(ref snap) = *snapshot {
      let num_nodes = snap.header.num_nodes as u32;
      for phys in 0..num_nodes {
        if let Some(src) = snap.get_node_id(phys) {
          // Skip deleted nodes
          if delta.is_node_deleted(src) {
            continue;
          }

          for (dst_phys, etype) in snap.iter_out_edges(phys) {
            // Apply filter
            if let Some(filter_etype) = etype_filter {
              if etype != filter_etype {
                continue;
              }
            }

            if let Some(dst) = snap.get_node_id(dst_phys) {
              // Skip deleted edges
              if delta.is_edge_deleted(src, etype, dst) {
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

        edges.push(FullEdge {
          src,
          etype: patch.etype,
          dst: patch.other,
        });
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
      mvcc_stats: None,
    }
  }

  /// Get WAL buffer statistics
  pub fn wal_stats(&self) -> crate::core::wal::buffer::WalBufferStats {
    self.wal_buffer.lock().stats()
  }
}
