//! Node operations for Python bindings

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use crate::core::single_file::SingleFileDB as RustSingleFileDB;
use crate::graph::db::GraphDB as RustGraphDB;
use crate::graph::iterators::{count_nodes as graph_count_nodes, list_nodes as graph_list_nodes};
use crate::graph::key_index::get_node_key as graph_get_node_key;
use crate::graph::nodes::{
  create_node as graph_create_node, delete_node as graph_delete_node, get_node_by_key_db,
  node_exists_db, NodeOpts,
};
use crate::graph::tx::TxHandle as GraphTxHandle;
use crate::types::NodeId;

/// Trait for node operations
pub trait NodeOps {
  /// Create a new node
  fn create_node_impl(&self, key: Option<String>) -> PyResult<i64>;

  /// Delete a node
  fn delete_node_impl(&self, node_id: i64) -> PyResult<()>;

  /// Check if a node exists
  fn node_exists_impl(&self, node_id: i64) -> PyResult<bool>;

  /// Get node by key
  fn get_node_by_key_impl(&self, key: &str) -> PyResult<Option<i64>>;

  /// Get the key for a node
  fn get_node_key_impl(&self, node_id: i64) -> PyResult<Option<String>>;

  /// List all node IDs
  fn list_nodes_impl(&self) -> PyResult<Vec<i64>>;

  /// Count all nodes
  fn count_nodes_impl(&self) -> PyResult<i64>;
}

// ============================================================================
// Single-file database operations
// ============================================================================

/// Create node on single-file database
pub fn create_node_single(db: &RustSingleFileDB, key: Option<&str>) -> PyResult<i64> {
  let node_id = db
    .create_node(key)
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to create node: {e}")))?;
  Ok(node_id as i64)
}

/// Delete node on single-file database
pub fn delete_node_single(db: &RustSingleFileDB, node_id: NodeId) -> PyResult<()> {
  db.delete_node(node_id)
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to delete node: {e}")))
}

/// Check node exists on single-file database
pub fn node_exists_single(db: &RustSingleFileDB, node_id: NodeId) -> bool {
  db.node_exists(node_id)
}

/// Get node by key on single-file database
pub fn get_node_by_key_single(db: &RustSingleFileDB, key: &str) -> Option<i64> {
  db.get_node_by_key(key).map(|id| id as i64)
}

/// Get node key on single-file database
pub fn get_node_key_single(db: &RustSingleFileDB, node_id: NodeId) -> Option<String> {
  db.get_node_key(node_id)
}

/// List nodes on single-file database
pub fn list_nodes_single(db: &RustSingleFileDB) -> Vec<i64> {
  db.list_nodes().into_iter().map(|id| id as i64).collect()
}

/// Count nodes on single-file database
pub fn count_nodes_single(db: &RustSingleFileDB) -> i64 {
  db.count_nodes() as i64
}

// ============================================================================
// Graph database operations
// ============================================================================

/// Create node on graph database (requires transaction handle)
pub fn create_node_graph(handle: &mut GraphTxHandle, key: Option<String>) -> PyResult<i64> {
  let mut opts = NodeOpts::new();
  if let Some(k) = key {
    opts = opts.with_key(k);
  }
  let node_id = graph_create_node(handle, opts)
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to create node: {e}")))?;
  Ok(node_id as i64)
}

/// Delete node on graph database (requires transaction handle)
pub fn delete_node_graph(handle: &mut GraphTxHandle, node_id: NodeId) -> PyResult<()> {
  graph_delete_node(handle, node_id)
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to delete node: {e}")))?;
  Ok(())
}

/// Check node exists on graph database
pub fn node_exists_graph(db: &RustGraphDB, node_id: NodeId) -> bool {
  node_exists_db(db, node_id)
}

/// Get node by key on graph database
pub fn get_node_by_key_graph(db: &RustGraphDB, key: &str) -> Option<i64> {
  get_node_by_key_db(db, key).map(|id| id as i64)
}

/// Get node key on graph database
pub fn get_node_key_graph(db: &RustGraphDB, node_id: NodeId) -> Option<String> {
  let delta = db.delta.read();
  graph_get_node_key(db.snapshot.as_ref(), &delta, node_id)
}

/// List nodes on graph database
pub fn list_nodes_graph(db: &RustGraphDB) -> Vec<i64> {
  graph_list_nodes(db)
    .into_iter()
    .map(|id| id as i64)
    .collect()
}

/// Count nodes on graph database
pub fn count_nodes_graph(db: &RustGraphDB) -> i64 {
  graph_count_nodes(db) as i64
}

/// List nodes with key prefix on single-file database
pub fn list_nodes_with_prefix_single(db: &RustSingleFileDB, prefix: &str) -> Vec<i64> {
  db.list_nodes()
    .into_iter()
    .filter(|&id| {
      if let Some(key) = db.get_node_key(id) {
        key.starts_with(prefix)
      } else {
        false
      }
    })
    .map(|id| id as i64)
    .collect()
}

/// List nodes with key prefix on graph database
pub fn list_nodes_with_prefix_graph(db: &RustGraphDB, prefix: &str) -> Vec<i64> {
  graph_list_nodes(db)
    .into_iter()
    .filter(|&id| {
      if let Some(key) = get_node_key_graph(db, id) {
        key.starts_with(prefix)
      } else {
        false
      }
    })
    .map(|id| id as i64)
    .collect()
}

/// Count nodes with key prefix on single-file database
pub fn count_nodes_with_prefix_single(db: &RustSingleFileDB, prefix: &str) -> i64 {
  db.list_nodes()
    .into_iter()
    .filter(|&id| {
      if let Some(key) = db.get_node_key(id) {
        key.starts_with(prefix)
      } else {
        false
      }
    })
    .count() as i64
}

/// Count nodes with key prefix on graph database
pub fn count_nodes_with_prefix_graph(db: &RustGraphDB, prefix: &str) -> i64 {
  graph_list_nodes(db)
    .into_iter()
    .filter(|&id| {
      if let Some(key) = get_node_key_graph(db, id) {
        key.starts_with(prefix)
      } else {
        false
      }
    })
    .count() as i64
}

#[cfg(test)]
mod tests {
  // Node operation tests require database instances
  // Better tested through integration tests
}
