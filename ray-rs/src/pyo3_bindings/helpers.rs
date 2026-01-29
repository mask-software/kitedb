//! Internal helper functions for Python bindings

use crate::api::traversal::TraversalDirection;
use crate::core::single_file::SingleFileDB as RustSingleFileDB;
use crate::graph::db::GraphDB as RustGraphDB;
use crate::graph::iterators::{
  count_edges as graph_count_edges, count_nodes as graph_count_nodes, list_in_edges, list_out_edges,
};
use crate::graph::key_index::get_node_key as graph_get_node_key;
use crate::types::{CheckResult as RustCheckResult, ETypeId, Edge, NodeId};

use super::stats::{DbStats, MvccStats};

/// Get node key from graph database
pub fn get_graph_node_key(db: &RustGraphDB, node_id: NodeId) -> Option<String> {
  let delta = db.delta.read();
  graph_get_node_key(db.snapshot.as_ref(), &delta, node_id)
}

/// Get neighbors from single-file database for traversal
pub fn get_neighbors_from_single_file(
  db: &RustSingleFileDB,
  node_id: NodeId,
  direction: TraversalDirection,
  etype: Option<ETypeId>,
) -> Vec<Edge> {
  let mut edges = Vec::new();
  match direction {
    TraversalDirection::Out => {
      for (e, dst) in db.get_out_edges(node_id) {
        if etype.is_none() || etype == Some(e) {
          edges.push(Edge {
            src: node_id,
            etype: e,
            dst,
          });
        }
      }
    }
    TraversalDirection::In => {
      for (e, src) in db.get_in_edges(node_id) {
        if etype.is_none() || etype == Some(e) {
          edges.push(Edge {
            src,
            etype: e,
            dst: node_id,
          });
        }
      }
    }
    TraversalDirection::Both => {
      edges.extend(get_neighbors_from_single_file(
        db,
        node_id,
        TraversalDirection::Out,
        etype,
      ));
      edges.extend(get_neighbors_from_single_file(
        db,
        node_id,
        TraversalDirection::In,
        etype,
      ));
    }
  }
  edges
}

/// Get neighbors from graph database for traversal
pub fn get_neighbors_from_graph_db(
  db: &RustGraphDB,
  node_id: NodeId,
  direction: TraversalDirection,
  etype: Option<ETypeId>,
) -> Vec<Edge> {
  let mut edges = Vec::new();
  match direction {
    TraversalDirection::Out => {
      for edge in list_out_edges(db, node_id) {
        if etype.is_none() || etype == Some(edge.etype) {
          edges.push(Edge {
            src: node_id,
            etype: edge.etype,
            dst: edge.dst,
          });
        }
      }
    }
    TraversalDirection::In => {
      for edge in list_in_edges(db, node_id) {
        if etype.is_none() || etype == Some(edge.etype) {
          edges.push(Edge {
            src: edge.dst,
            etype: edge.etype,
            dst: node_id,
          });
        }
      }
    }
    TraversalDirection::Both => {
      edges.extend(get_neighbors_from_graph_db(
        db,
        node_id,
        TraversalDirection::Out,
        etype,
      ));
      edges.extend(get_neighbors_from_graph_db(
        db,
        node_id,
        TraversalDirection::In,
        etype,
      ));
    }
  }
  edges
}

/// Compute statistics for graph database
pub fn graph_stats(db: &RustGraphDB) -> DbStats {
  let node_count = graph_count_nodes(db);
  let edge_count = graph_count_edges(db, None);

  let delta = db.delta.read();
  let delta_nodes_created = delta.created_nodes.len();
  let delta_nodes_deleted = delta.deleted_nodes.len();
  let delta_edges_added = delta.total_edges_added();
  let delta_edges_deleted = delta.total_edges_deleted();
  drop(delta);

  let (snapshot_gen, snapshot_nodes, snapshot_edges, snapshot_max_node_id) =
    if let Some(ref snapshot) = db.snapshot {
      (
        snapshot.header.generation,
        snapshot.header.num_nodes,
        snapshot.header.num_edges,
        snapshot.header.max_node_id,
      )
    } else {
      (0, 0, 0, 0)
    };

  let wal_segment = db.manifest.as_ref().map(|m| m.active_wal_seg).unwrap_or(0);

  let mvcc_stats = db.mvcc.as_ref().map(|mvcc| {
    let tx_mgr = mvcc.tx_manager.lock();
    let gc = mvcc.gc.lock();
    let gc_stats = gc.get_stats();
    let committed_stats = tx_mgr.get_committed_writes_stats();
    MvccStats {
      active_transactions: tx_mgr.get_active_count() as i64,
      min_active_ts: tx_mgr.min_active_ts() as i64,
      versions_pruned: gc_stats.versions_pruned as i64,
      gc_runs: gc_stats.gc_runs as i64,
      last_gc_time: gc_stats.last_gc_time as i64,
      committed_writes_size: committed_stats.size as i64,
      committed_writes_pruned: committed_stats.pruned as i64,
    }
  });

  let total_changes =
    delta_nodes_created + delta_nodes_deleted + delta_edges_added + delta_edges_deleted;
  let recommend_compact = total_changes > 10_000;

  DbStats {
    snapshot_gen: snapshot_gen as i64,
    snapshot_nodes: snapshot_nodes.max(node_count) as i64,
    snapshot_edges: snapshot_edges.max(edge_count) as i64,
    snapshot_max_node_id: snapshot_max_node_id as i64,
    delta_nodes_created: delta_nodes_created as i64,
    delta_nodes_deleted: delta_nodes_deleted as i64,
    delta_edges_added: delta_edges_added as i64,
    delta_edges_deleted: delta_edges_deleted as i64,
    wal_segment: wal_segment as i64,
    wal_bytes: db.wal_bytes() as i64,
    recommend_compact,
    mvcc_stats,
  }
}

/// Check graph database integrity
pub fn graph_check(db: &RustGraphDB) -> RustCheckResult {
  if let Some(ref snapshot) = db.snapshot {
    return crate::check::check_snapshot(snapshot);
  }

  RustCheckResult {
    valid: true,
    errors: Vec::new(),
    warnings: vec!["No snapshot to check".to_string()],
  }
}

#[cfg(test)]
mod tests {
  // Note: Most helper tests require database instances which are
  // better tested through integration tests
}
