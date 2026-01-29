//! Edge operations for Python bindings

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use crate::core::single_file::SingleFileDB as RustSingleFileDB;
use crate::graph::db::GraphDB as RustGraphDB;
use crate::graph::edges::{
  add_edge as graph_add_edge, delete_edge as graph_delete_edge, edge_exists_db,
};
use crate::graph::iterators::{
  count_edges as graph_count_edges, list_edges as graph_list_edges, list_in_edges, list_out_edges,
  ListEdgesOptions,
};
use crate::graph::tx::TxHandle as GraphTxHandle;
use crate::types::{ETypeId, NodeId};

use crate::pyo3_bindings::types::{Edge, FullEdge};

/// Trait for edge operations
pub trait EdgeOps {
  /// Add an edge
  fn add_edge_impl(&self, src: i64, etype: u32, dst: i64) -> PyResult<()>;

  /// Delete an edge
  fn delete_edge_impl(&self, src: i64, etype: u32, dst: i64) -> PyResult<()>;

  /// Check if an edge exists
  fn edge_exists_impl(&self, src: i64, etype: u32, dst: i64) -> PyResult<bool>;

  /// Get outgoing edges for a node
  fn get_out_edges_impl(&self, node_id: i64) -> PyResult<Vec<Edge>>;

  /// Get incoming edges for a node
  fn get_in_edges_impl(&self, node_id: i64) -> PyResult<Vec<Edge>>;

  /// Count all edges
  fn count_edges_impl(&self) -> PyResult<i64>;

  /// List all edges
  fn list_edges_impl(&self, etype: Option<u32>) -> PyResult<Vec<FullEdge>>;
}

// ============================================================================
// Single-file database operations
// ============================================================================

/// Add edge on single-file database
pub fn add_edge_single(
  db: &RustSingleFileDB,
  src: NodeId,
  etype: ETypeId,
  dst: NodeId,
) -> PyResult<()> {
  db.add_edge(src, etype, dst)
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to add edge: {e}")))
}

/// Add edge by type name on single-file database
pub fn add_edge_by_name_single(
  db: &RustSingleFileDB,
  src: NodeId,
  etype_name: &str,
  dst: NodeId,
) -> PyResult<()> {
  db.add_edge_by_name(src, etype_name, dst)
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to add edge: {e}")))
}

/// Delete edge on single-file database
pub fn delete_edge_single(
  db: &RustSingleFileDB,
  src: NodeId,
  etype: ETypeId,
  dst: NodeId,
) -> PyResult<()> {
  db.delete_edge(src, etype, dst)
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to delete edge: {e}")))
}

/// Check edge exists on single-file database
pub fn edge_exists_single(db: &RustSingleFileDB, src: NodeId, etype: ETypeId, dst: NodeId) -> bool {
  db.edge_exists(src, etype, dst)
}

/// Get out edges on single-file database
pub fn get_out_edges_single(db: &RustSingleFileDB, node_id: NodeId) -> Vec<Edge> {
  db.get_out_edges(node_id)
    .into_iter()
    .map(|(etype, dst)| Edge {
      etype,
      node_id: dst as i64,
    })
    .collect()
}

/// Get in edges on single-file database
pub fn get_in_edges_single(db: &RustSingleFileDB, node_id: NodeId) -> Vec<Edge> {
  db.get_in_edges(node_id)
    .into_iter()
    .map(|(etype, src)| Edge {
      etype,
      node_id: src as i64,
    })
    .collect()
}

/// Get out degree on single-file database
pub fn get_out_degree_single(db: &RustSingleFileDB, node_id: NodeId) -> i64 {
  db.get_out_degree(node_id) as i64
}

/// Get in degree on single-file database
pub fn get_in_degree_single(db: &RustSingleFileDB, node_id: NodeId) -> i64 {
  db.get_in_degree(node_id) as i64
}

/// Count edges on single-file database
pub fn count_edges_single(db: &RustSingleFileDB) -> i64 {
  db.count_edges() as i64
}

/// Count edges by type on single-file database
pub fn count_edges_by_type_single(db: &RustSingleFileDB, etype: ETypeId) -> i64 {
  db.count_edges_by_type(etype) as i64
}

/// List edges on single-file database
pub fn list_edges_single(db: &RustSingleFileDB, etype: Option<ETypeId>) -> Vec<FullEdge> {
  db.list_edges(etype)
    .into_iter()
    .map(|e| FullEdge {
      src: e.src as i64,
      etype: e.etype,
      dst: e.dst as i64,
    })
    .collect()
}

// ============================================================================
// Graph database operations
// ============================================================================

/// Add edge on graph database (requires transaction handle)
pub fn add_edge_graph(
  handle: &mut GraphTxHandle,
  src: NodeId,
  etype: ETypeId,
  dst: NodeId,
) -> PyResult<()> {
  graph_add_edge(handle, src, etype, dst)
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to add edge: {e}")))?;
  Ok(())
}

/// Delete edge on graph database (requires transaction handle)
pub fn delete_edge_graph(
  handle: &mut GraphTxHandle,
  src: NodeId,
  etype: ETypeId,
  dst: NodeId,
) -> PyResult<()> {
  graph_delete_edge(handle, src, etype, dst)
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to delete edge: {e}")))?;
  Ok(())
}

/// Check edge exists on graph database
pub fn edge_exists_graph(db: &RustGraphDB, src: NodeId, etype: ETypeId, dst: NodeId) -> bool {
  edge_exists_db(db, src, etype, dst)
}

/// Get out edges on graph database
pub fn get_out_edges_graph(db: &RustGraphDB, node_id: NodeId) -> Vec<Edge> {
  list_out_edges(db, node_id)
    .into_iter()
    .map(|edge| Edge {
      etype: edge.etype,
      node_id: edge.dst as i64,
    })
    .collect()
}

/// Get in edges on graph database
pub fn get_in_edges_graph(db: &RustGraphDB, node_id: NodeId) -> Vec<Edge> {
  list_in_edges(db, node_id)
    .into_iter()
    .map(|edge| Edge {
      etype: edge.etype,
      node_id: edge.dst as i64,
    })
    .collect()
}

/// Get out degree on graph database
pub fn get_out_degree_graph(db: &RustGraphDB, node_id: NodeId) -> i64 {
  list_out_edges(db, node_id).len() as i64
}

/// Get in degree on graph database
pub fn get_in_degree_graph(db: &RustGraphDB, node_id: NodeId) -> i64 {
  list_in_edges(db, node_id).len() as i64
}

/// Count edges on graph database
pub fn count_edges_graph(db: &RustGraphDB, etype: Option<ETypeId>) -> i64 {
  graph_count_edges(db, etype) as i64
}

/// List edges on graph database
pub fn list_edges_graph(db: &RustGraphDB, etype: Option<ETypeId>) -> Vec<FullEdge> {
  let options = ListEdgesOptions { etype };
  graph_list_edges(db, options)
    .into_iter()
    .map(|e| FullEdge {
      src: e.src as i64,
      etype: e.etype,
      dst: e.dst as i64,
    })
    .collect()
}

#[cfg(test)]
mod tests {
  // Edge operation tests require database instances
  // Better tested through integration tests
}
