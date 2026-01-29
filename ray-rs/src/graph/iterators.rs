//! Traversal iterators
//!
//! Provides iterators for traversing nodes and edges in the graph.
//! These iterators merge snapshot and delta state.

use crate::types::*;

use super::db::GraphDB;

// ============================================================================
// Node Iterator
// ============================================================================

/// Iterator over all nodes in the database
pub struct NodeIter<'a> {
  db: &'a GraphDB,
  /// Current snapshot physical index
  snapshot_phys: u64,
  /// Iterator over delta created nodes
  delta_iter: Option<std::collections::hash_map::Keys<'a, NodeId, NodeDelta>>,
  /// Tracks yielded snapshot node IDs to avoid duplicates
  yielded_from_snapshot: std::collections::HashSet<NodeId>,
  /// Phase of iteration
  phase: NodeIterPhase,
}

enum NodeIterPhase {
  Snapshot,
  DeltaCreated,
  Done,
}

impl<'a> NodeIter<'a> {
  pub fn new(db: &'a GraphDB) -> Self {
    Self {
      db,
      snapshot_phys: 0,
      delta_iter: None,
      yielded_from_snapshot: std::collections::HashSet::new(),
      phase: NodeIterPhase::Snapshot,
    }
  }
}

impl<'a> Iterator for NodeIter<'a> {
  type Item = NodeId;

  fn next(&mut self) -> Option<Self::Item> {
    let delta = self.db.delta.read();

    loop {
      match self.phase {
        NodeIterPhase::Snapshot => {
          if let Some(ref snapshot) = self.db.snapshot {
            while self.snapshot_phys < snapshot.header.num_nodes {
              let phys = self.snapshot_phys as u32;
              self.snapshot_phys += 1;

              if let Some(node_id) = snapshot.get_node_id(phys) {
                // Skip if deleted in delta
                if delta.deleted_nodes.contains(&node_id) {
                  continue;
                }

                self.yielded_from_snapshot.insert(node_id);
                return Some(node_id);
              }
            }
          }

          // Move to delta phase
          self.phase = NodeIterPhase::DeltaCreated;
        }

        NodeIterPhase::DeltaCreated => {
          // Iterate over created nodes
          if self.delta_iter.is_none() {
            // We need to drop the read guard before re-acquiring
            drop(delta);
            let delta = self.db.delta.read();

            // Return created nodes one at a time
            for &node_id in delta.created_nodes.keys() {
              if !self.yielded_from_snapshot.contains(&node_id)
                && !delta.deleted_nodes.contains(&node_id)
              {
                // We found one - but we can't hold the iterator
                // So just return it and continue next time
                self.yielded_from_snapshot.insert(node_id);
                return Some(node_id);
              }
            }

            self.phase = NodeIterPhase::Done;
            return None;
          }

          self.phase = NodeIterPhase::Done;
          return None;
        }

        NodeIterPhase::Done => {
          return None;
        }
      }
    }
  }
}

// ============================================================================
// Edge Iterator (Outgoing)
// ============================================================================

/// Iterator over outgoing edges from a source node
pub struct OutEdgeIter<'a> {
  db: &'a GraphDB,
  src: NodeId,
  /// Current index into snapshot edges
  snapshot_idx: usize,
  /// Total snapshot edges for this node
  snapshot_count: usize,
  /// Source physical node in snapshot
  src_phys: Option<u32>,
  /// Iterator over delta added edges
  delta_add_iter: Option<std::collections::btree_set::Iter<'a, EdgePatch>>,
  /// Phase
  phase: OutEdgeIterPhase,
}

#[derive(Debug, Clone, Copy)]
enum OutEdgeIterPhase {
  Snapshot,
  DeltaAdded,
  Done,
}

/// An edge with its edge type and destination
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edge {
  pub etype: ETypeId,
  pub dst: NodeId,
}

impl<'a> OutEdgeIter<'a> {
  pub fn new(db: &'a GraphDB, src: NodeId) -> Self {
    let src_phys = db.snapshot.as_ref().and_then(|s| s.get_phys_node(src));
    let snapshot_count = src_phys
      .and_then(|p| db.snapshot.as_ref()?.get_out_degree(p))
      .unwrap_or(0);

    Self {
      db,
      src,
      snapshot_idx: 0,
      snapshot_count,
      src_phys,
      delta_add_iter: None,
      phase: OutEdgeIterPhase::Snapshot,
    }
  }
}

impl<'a> Iterator for OutEdgeIter<'a> {
  type Item = Edge;

  fn next(&mut self) -> Option<Self::Item> {
    let delta = self.db.delta.read();

    loop {
      match self.phase {
        OutEdgeIterPhase::Snapshot => {
          // Iterate snapshot edges
          if let (Some(snapshot), Some(phys)) = (&self.db.snapshot, self.src_phys) {
            while self.snapshot_idx < self.snapshot_count {
              let idx = self.snapshot_idx;
              self.snapshot_idx += 1;

              // Get edge from snapshot
              let mut iter = snapshot.iter_out_edges(phys);
              // Skip to current index
              for _ in 0..idx {
                iter.next();
              }

              if let Some((etype, dst_phys)) = iter.next() {
                // Convert dst_phys to node_id
                if let Some(dst) = snapshot.get_node_id(dst_phys) {
                  // Skip if deleted in delta
                  if delta.is_edge_deleted(self.src, etype, dst) {
                    continue;
                  }

                  return Some(Edge { etype, dst });
                }
              }
            }
          }

          self.phase = OutEdgeIterPhase::DeltaAdded;
        }

        OutEdgeIterPhase::DeltaAdded => {
          // Iterate delta added edges
          if let Some(add_set) = delta.out_add.get(&self.src) {
            if let Some(patch) = add_set.iter().next() {
              // Return each added edge
              return Some(Edge {
                etype: patch.etype,
                dst: patch.other,
              });
            }
          }

          self.phase = OutEdgeIterPhase::Done;
          return None;
        }

        OutEdgeIterPhase::Done => {
          return None;
        }
      }
    }
  }
}

// ============================================================================
// Simplified list_nodes function
// ============================================================================

/// List all node IDs in the database
pub fn list_nodes(db: &GraphDB) -> Vec<NodeId> {
  let delta = db.delta.read();
  let mut nodes = Vec::new();
  let mut seen = std::collections::HashSet::new();

  // From snapshot
  if let Some(ref snapshot) = db.snapshot {
    for phys in 0..snapshot.header.num_nodes {
      if let Some(node_id) = snapshot.get_node_id(phys as u32) {
        if !delta.deleted_nodes.contains(&node_id) {
          nodes.push(node_id);
          seen.insert(node_id);
        }
      }
    }
  }

  // From delta created
  for &node_id in delta.created_nodes.keys() {
    if !seen.contains(&node_id) && !delta.deleted_nodes.contains(&node_id) {
      nodes.push(node_id);
    }
  }

  nodes
}

/// List outgoing edges from a source node
pub fn list_out_edges(db: &GraphDB, src: NodeId) -> Vec<Edge> {
  let delta = db.delta.read();
  let mut edges = Vec::new();

  // From snapshot
  if let Some(ref snapshot) = db.snapshot {
    if let Some(phys) = snapshot.get_phys_node(src) {
      for (dst_phys, etype) in snapshot.iter_out_edges(phys) {
        if let Some(dst) = snapshot.get_node_id(dst_phys) {
          if !delta.is_edge_deleted(src, etype, dst) {
            edges.push(Edge { etype, dst });
          }
        }
      }
    }
  }

  // From delta added
  if let Some(add_set) = delta.out_add.get(&src) {
    for patch in add_set {
      edges.push(Edge {
        etype: patch.etype,
        dst: patch.other,
      });
    }
  }

  edges
}

/// Count all nodes in the database
pub fn count_nodes(db: &GraphDB) -> u64 {
  let delta = db.delta.read();

  // Start with snapshot count
  let mut count = db
    .snapshot
    .as_ref()
    .map(|s| s.header.num_nodes)
    .unwrap_or(0);

  // Subtract deleted snapshot nodes
  for &node_id in &delta.deleted_nodes {
    if let Some(ref snapshot) = db.snapshot {
      if snapshot.has_node(node_id) {
        count = count.saturating_sub(1);
      }
    }
  }

  // Add created nodes (that weren't deleted)
  for &node_id in delta.created_nodes.keys() {
    if !delta.deleted_nodes.contains(&node_id) {
      count += 1;
    }
  }

  count
}

/// Count all edges in the database, optionally filtered by edge type
pub fn count_edges(db: &GraphDB, etype_filter: Option<ETypeId>) -> u64 {
  let delta = db.delta.read();

  // Start with snapshot count (approximate - doesn't filter by type)
  let mut count = db
    .snapshot
    .as_ref()
    .map(|s| s.header.num_edges)
    .unwrap_or(0);

  // Subtract deleted edges
  for del_set in delta.out_del.values() {
    for patch in del_set {
      if etype_filter.is_none() || etype_filter == Some(patch.etype) {
        count = count.saturating_sub(1);
      }
    }
  }

  // Add created edges
  for add_set in delta.out_add.values() {
    for patch in add_set {
      if etype_filter.is_none() || etype_filter == Some(patch.etype) {
        count += 1;
      }
    }
  }

  count
}

/// List incoming edges to a destination node
pub fn list_in_edges(db: &GraphDB, dst: NodeId) -> Vec<Edge> {
  let delta = db.delta.read();
  let mut edges = Vec::new();

  // From snapshot (if in-edges are available)
  if let Some(ref snapshot) = db.snapshot {
    if let Some(phys) = snapshot.get_phys_node(dst) {
      for (src_phys, etype, _out_index) in snapshot.iter_in_edges(phys) {
        if let Some(src) = snapshot.get_node_id(src_phys) {
          if !delta.is_edge_deleted(src, etype, dst) {
            edges.push(Edge { etype, dst: src }); // Note: dst field holds src here
          }
        }
      }
    }
  }

  // From delta added
  if let Some(add_set) = delta.in_add.get(&dst) {
    for patch in add_set {
      edges.push(Edge {
        etype: patch.etype,
        dst: patch.other, // This is actually the source
      });
    }
  }

  edges
}

/// Full edge with source, destination, and type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FullEdge {
  pub src: NodeId,
  pub etype: ETypeId,
  pub dst: NodeId,
}

/// Options for listing edges
#[derive(Debug, Clone, Default)]
pub struct ListEdgesOptions {
  /// Filter by edge type
  pub etype: Option<ETypeId>,
}

/// List all edges in the database
///
/// This iterates over all nodes and their outgoing edges.
/// Optionally filter by edge type.
pub fn list_edges(db: &GraphDB, options: ListEdgesOptions) -> Vec<FullEdge> {
  let delta = db.delta.read();
  let mut edges = Vec::new();
  let mut seen_nodes = std::collections::HashSet::new();

  // From snapshot
  if let Some(ref snapshot) = db.snapshot {
    for phys in 0..snapshot.header.num_nodes as u32 {
      if let Some(src) = snapshot.get_node_id(phys) {
        if delta.deleted_nodes.contains(&src) {
          continue;
        }
        seen_nodes.insert(src);

        for (dst_phys, etype) in snapshot.iter_out_edges(phys) {
          if let Some(dst) = snapshot.get_node_id(dst_phys) {
            // Apply etype filter
            if let Some(filter_etype) = options.etype {
              if etype != filter_etype {
                continue;
              }
            }

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

  // From delta created nodes and added edges
  for &src in delta.created_nodes.keys() {
    if delta.deleted_nodes.contains(&src) {
      continue;
    }
    seen_nodes.insert(src);
  }

  // Add delta edges
  for (&src, add_set) in &delta.out_add {
    for patch in add_set {
      // Apply etype filter
      if let Some(filter_etype) = options.etype {
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

#[cfg(test)]
mod tests {
  use super::*;
  use crate::graph::db::{close_graph_db, open_graph_db, OpenOptions};
  use crate::graph::edges::add_edge;
  use crate::graph::nodes::{create_node, NodeOpts};
  use crate::graph::tx::{begin_tx, commit};
  use tempfile::tempdir;

  #[test]
  fn test_list_nodes_empty() {
    let temp_dir = tempdir().unwrap();
    let db = open_graph_db(temp_dir.path(), OpenOptions::new()).unwrap();

    let nodes = list_nodes(&db);
    assert!(nodes.is_empty());

    close_graph_db(db).unwrap();
  }

  #[test]
  fn test_list_nodes_with_data() {
    let temp_dir = tempdir().unwrap();
    let db = open_graph_db(temp_dir.path(), OpenOptions::new()).unwrap();

    let mut tx = begin_tx(&db).unwrap();
    let n1 = create_node(&mut tx, NodeOpts::new()).unwrap();
    let n2 = create_node(&mut tx, NodeOpts::new()).unwrap();
    let n3 = create_node(&mut tx, NodeOpts::new()).unwrap();
    commit(&mut tx).unwrap();

    let nodes = list_nodes(&db);
    assert_eq!(nodes.len(), 3);
    assert!(nodes.contains(&n1));
    assert!(nodes.contains(&n2));
    assert!(nodes.contains(&n3));

    close_graph_db(db).unwrap();
  }

  #[test]
  fn test_list_out_edges() {
    let temp_dir = tempdir().unwrap();
    let db = open_graph_db(temp_dir.path(), OpenOptions::new()).unwrap();

    let mut tx = begin_tx(&db).unwrap();
    let alice = create_node(&mut tx, NodeOpts::new()).unwrap();
    let bob = create_node(&mut tx, NodeOpts::new()).unwrap();
    let charlie = create_node(&mut tx, NodeOpts::new()).unwrap();

    add_edge(&mut tx, alice, 1, bob).unwrap();
    add_edge(&mut tx, alice, 1, charlie).unwrap();
    commit(&mut tx).unwrap();

    let edges = list_out_edges(&db, alice);
    assert_eq!(edges.len(), 2);

    close_graph_db(db).unwrap();
  }
}
