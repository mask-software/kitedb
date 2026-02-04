//! Graph traversal operations for Python bindings

use pyo3::prelude::*;
use std::collections::HashSet;

use crate::api::pathfinding::{bfs, dijkstra, PathConfig};
use crate::api::traversal::{
  TraversalBuilder as RustTraversalBuilder, TraversalDirection, TraverseOptions,
};
use crate::core::single_file::SingleFileDB as RustSingleFileDB;
use crate::types::{ETypeId, Edge, NodeId};

use crate::pyo3_bindings::helpers::get_neighbors_from_single_file;
use crate::pyo3_bindings::traversal::{PyPathResult, PyTraversalResult as TraversalResult};

/// Trait for graph traversal operations
pub trait GraphTraversalOps {
  /// Traverse outgoing edges from a node
  fn traverse_out_impl(&self, node_id: i64, etype: Option<u32>) -> PyResult<Vec<i64>>;
  /// Traverse incoming edges to a node
  fn traverse_in_impl(&self, node_id: i64, etype: Option<u32>) -> PyResult<Vec<i64>>;
  /// Variable-depth traversal from a node
  fn traverse_impl(
    &self,
    node_id: i64,
    max_depth: u32,
    etype: Option<u32>,
    min_depth: Option<u32>,
    direction: Option<String>,
    unique: Option<bool>,
  ) -> PyResult<Vec<TraversalResult>>;
  /// Find shortest path using BFS
  fn find_path_bfs_impl(
    &self,
    source: i64,
    target: i64,
    etype: Option<u32>,
    max_depth: Option<u32>,
    direction: Option<String>,
  ) -> PyResult<PyPathResult>;
  /// Find shortest path using Dijkstra
  fn find_path_dijkstra_impl(
    &self,
    source: i64,
    target: i64,
    etype: Option<u32>,
    max_depth: Option<u32>,
    direction: Option<String>,
  ) -> PyResult<PyPathResult>;
}

// ============================================================================
// Single-file database operations
// ============================================================================

pub fn traverse_out_single(
  db: &RustSingleFileDB,
  node_id: NodeId,
  etype: Option<ETypeId>,
) -> Vec<i64> {
  db.out_edges(node_id)
    .into_iter()
    .filter(|(e, _)| etype.is_none() || etype == Some(*e))
    .map(|(_, dst)| dst as i64)
    .collect()
}

pub fn traverse_out_with_keys_single(
  db: &RustSingleFileDB,
  node_id: NodeId,
  etype: Option<ETypeId>,
) -> Vec<(i64, Option<String>)> {
  db.out_edges(node_id)
    .into_iter()
    .filter(|(e, _)| etype.is_none() || etype == Some(*e))
    .map(|(_, dst)| {
      let key = db.node_key(dst);
      (dst as i64, key)
    })
    .collect()
}

pub fn traverse_out_count_single(
  db: &RustSingleFileDB,
  node_id: NodeId,
  etype: Option<ETypeId>,
) -> i64 {
  if let Some(et) = etype {
    db.out_edges(node_id)
      .into_iter()
      .filter(|(e, _)| *e == et)
      .count() as i64
  } else {
    db.out_degree(node_id) as i64
  }
}

pub fn traverse_in_single(
  db: &RustSingleFileDB,
  node_id: NodeId,
  etype: Option<ETypeId>,
) -> Vec<i64> {
  db.in_edges(node_id)
    .into_iter()
    .filter(|(e, _)| etype.is_none() || etype == Some(*e))
    .map(|(_, src)| src as i64)
    .collect()
}

pub fn traverse_in_with_keys_single(
  db: &RustSingleFileDB,
  node_id: NodeId,
  etype: Option<ETypeId>,
) -> Vec<(i64, Option<String>)> {
  db.in_edges(node_id)
    .into_iter()
    .filter(|(e, _)| etype.is_none() || etype == Some(*e))
    .map(|(_, src)| {
      let key = db.node_key(src);
      (src as i64, key)
    })
    .collect()
}

pub fn traverse_in_count_single(
  db: &RustSingleFileDB,
  node_id: NodeId,
  etype: Option<ETypeId>,
) -> i64 {
  if let Some(et) = etype {
    db.in_edges(node_id)
      .into_iter()
      .filter(|(e, _)| *e == et)
      .count() as i64
  } else {
    db.in_degree(node_id) as i64
  }
}

pub fn traverse_multi_single(
  db: &RustSingleFileDB,
  start_ids: Vec<i64>,
  steps: Vec<(String, Option<u32>)>,
) -> Vec<(i64, Option<String>)> {
  let mut current_ids: Vec<NodeId> = start_ids.iter().map(|&id| id as NodeId).collect();

  for (direction, etype) in steps {
    let mut next_ids: Vec<NodeId> = Vec::new();
    let mut visited: HashSet<NodeId> = HashSet::new();

    for node_id in &current_ids {
      let neighbors: Vec<NodeId> = match direction.as_str() {
        "out" => db
          .out_edges(*node_id)
          .into_iter()
          .filter(|(e, _)| etype.is_none() || etype == Some(*e))
          .map(|(_, dst)| dst)
          .collect(),
        "in" => db
          .in_edges(*node_id)
          .into_iter()
          .filter(|(e, _)| etype.is_none() || etype == Some(*e))
          .map(|(_, src)| src)
          .collect(),
        _ => {
          let mut out: Vec<NodeId> = db
            .out_edges(*node_id)
            .into_iter()
            .filter(|(e, _)| etype.is_none() || etype == Some(*e))
            .map(|(_, dst)| dst)
            .collect();
          let in_edges: Vec<NodeId> = db
            .in_edges(*node_id)
            .into_iter()
            .filter(|(e, _)| etype.is_none() || etype == Some(*e))
            .map(|(_, src)| src)
            .collect();
          out.extend(in_edges);
          out
        }
      };

      for neighbor_id in neighbors {
        if !visited.contains(&neighbor_id) {
          visited.insert(neighbor_id);
          next_ids.push(neighbor_id);
        }
      }
    }

    current_ids = next_ids;
  }

  current_ids
    .into_iter()
    .map(|id| {
      let key = db.node_key(id);
      (id as i64, key)
    })
    .collect()
}

pub fn traverse_multi_count_single(
  db: &RustSingleFileDB,
  start_ids: Vec<i64>,
  steps: Vec<(String, Option<u32>)>,
) -> i64 {
  let mut current_ids: Vec<NodeId> = start_ids.iter().map(|&id| id as NodeId).collect();

  for (direction, etype) in steps {
    let mut next_ids: Vec<NodeId> = Vec::new();
    let mut visited: HashSet<NodeId> = HashSet::new();

    for node_id in &current_ids {
      let neighbors: Vec<NodeId> = match direction.as_str() {
        "out" => db
          .out_edges(*node_id)
          .into_iter()
          .filter(|(e, _)| etype.is_none() || etype == Some(*e))
          .map(|(_, dst)| dst)
          .collect(),
        "in" => db
          .in_edges(*node_id)
          .into_iter()
          .filter(|(e, _)| etype.is_none() || etype == Some(*e))
          .map(|(_, src)| src)
          .collect(),
        _ => {
          let mut out: Vec<NodeId> = db
            .out_edges(*node_id)
            .into_iter()
            .filter(|(e, _)| etype.is_none() || etype == Some(*e))
            .map(|(_, dst)| dst)
            .collect();
          let in_edges: Vec<NodeId> = db
            .in_edges(*node_id)
            .into_iter()
            .filter(|(e, _)| etype.is_none() || etype == Some(*e))
            .map(|(_, src)| src)
            .collect();
          out.extend(in_edges);
          out
        }
      };

      for neighbor_id in neighbors {
        if !visited.contains(&neighbor_id) {
          visited.insert(neighbor_id);
          next_ids.push(neighbor_id);
        }
      }
    }

    current_ids = next_ids;
  }

  current_ids.len() as i64
}

pub fn traverse_single(
  db: &RustSingleFileDB,
  node_id: NodeId,
  max_depth: u32,
  etype: Option<u32>,
  min_depth: Option<u32>,
  direction: Option<String>,
  unique: Option<bool>,
) -> Vec<TraversalResult> {
  let dir = match direction.as_deref() {
    Some("in") => TraversalDirection::In,
    Some("both") => TraversalDirection::Both,
    _ => TraversalDirection::Out,
  };

  let opts = TraverseOptions {
    direction: dir,
    min_depth: min_depth.unwrap_or(1) as usize,
    max_depth: max_depth as usize,
    unique: unique.unwrap_or(true),
    where_edge: None,
    where_node: None,
  };

  let get_neighbors = |nid: NodeId, d: TraversalDirection, et: Option<ETypeId>| -> Vec<Edge> {
    get_neighbors_from_single_file(db, nid, d, et)
  };

  RustTraversalBuilder::new(vec![node_id])
    .traverse(etype, opts)
    .execute(get_neighbors)
    .map(|r| {
      let (edge_src, edge_dst, edge_type) = match r.edge {
        Some(e) => (Some(e.src as i64), Some(e.dst as i64), Some(e.etype)),
        None => (None, None, None),
      };
      TraversalResult {
        node_id: r.node_id as i64,
        depth: r.depth as u32,
        edge_src,
        edge_dst,
        edge_type,
      }
    })
    .collect()
}

pub fn find_path_bfs_single(
  db: &RustSingleFileDB,
  source: NodeId,
  target: NodeId,
  etype: Option<u32>,
  max_depth: Option<u32>,
  direction: Option<String>,
) -> PyPathResult {
  let dir = match direction.as_deref() {
    Some("in") => TraversalDirection::In,
    Some("both") => TraversalDirection::Both,
    _ => TraversalDirection::Out,
  };

  let mut targets = HashSet::new();
  targets.insert(target);

  let mut allowed_etypes = HashSet::new();
  if let Some(e) = etype {
    allowed_etypes.insert(e);
  }

  let config = PathConfig {
    source,
    targets,
    allowed_etypes,
    direction: dir,
    max_depth: max_depth.unwrap_or(100) as usize,
  };

  let get_neighbors = |nid: NodeId, d: TraversalDirection, et: Option<ETypeId>| -> Vec<Edge> {
    get_neighbors_from_single_file(db, nid, d, et)
  };

  bfs(config, get_neighbors).into()
}

pub fn find_path_dijkstra_single(
  db: &RustSingleFileDB,
  source: NodeId,
  target: NodeId,
  etype: Option<u32>,
  max_depth: Option<u32>,
  direction: Option<String>,
) -> PyPathResult {
  let dir = match direction.as_deref() {
    Some("in") => TraversalDirection::In,
    Some("both") => TraversalDirection::Both,
    _ => TraversalDirection::Out,
  };

  let mut targets = HashSet::new();
  targets.insert(target);

  let mut allowed_etypes = HashSet::new();
  if let Some(e) = etype {
    allowed_etypes.insert(e);
  }

  let config = PathConfig {
    source,
    targets,
    allowed_etypes,
    direction: dir,
    max_depth: max_depth.unwrap_or(100) as usize,
  };

  let get_neighbors = |nid: NodeId, d: TraversalDirection, et: Option<ETypeId>| -> Vec<Edge> {
    get_neighbors_from_single_file(db, nid, d, et)
  };

  let get_weight = |_src: NodeId, _etype: ETypeId, _dst: NodeId| -> f64 { 1.0 };

  dijkstra(config, get_neighbors, get_weight).into()
}
