//! Node operations for Python bindings

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use crate::core::single_file::SingleFileDB as RustSingleFileDB;
use crate::types::{NodeId, PropKeyId, PropValue};

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
  db.node_by_key(key).map(|id| id as i64)
}

/// Get node key on single-file database
pub fn get_node_key_single(db: &RustSingleFileDB, node_id: NodeId) -> Option<String> {
  db.node_key(node_id)
}

/// List nodes on single-file database
pub fn list_nodes_single(db: &RustSingleFileDB) -> Vec<i64> {
  db.list_nodes().into_iter().map(|id| id as i64).collect()
}

/// Count nodes on single-file database
pub fn count_nodes_single(db: &RustSingleFileDB) -> i64 {
  db.count_nodes() as i64
}

/// Upsert node on single-file database
pub fn upsert_node_single(
  db: &RustSingleFileDB,
  key: &str,
  props: &[(PropKeyId, Option<PropValue>)],
) -> PyResult<i64> {
  let node_id = match db.node_by_key(key) {
    Some(id) => id,
    None => db
      .create_node(Some(key))
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to create node: {e}")))?,
  };

  for (prop_key_id, value_opt) in props {
    match value_opt {
      Some(value) => db
        .set_node_prop(node_id, *prop_key_id, value.clone())
        .map_err(|e| PyRuntimeError::new_err(format!("Failed to set prop: {e}")))?,
      None => db
        .delete_node_prop(node_id, *prop_key_id)
        .map_err(|e| PyRuntimeError::new_err(format!("Failed to delete prop: {e}")))?,
    }
  }

  Ok(node_id as i64)
}

/// Upsert node by ID on single-file database
pub fn upsert_node_by_id_single(
  db: &RustSingleFileDB,
  node_id: NodeId,
  props: &[(PropKeyId, Option<PropValue>)],
) -> PyResult<i64> {
  if !db.node_exists(node_id) {
    db.create_node_with_id(node_id, None)
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to create node: {e}")))?;
  }

  for (prop_key_id, value_opt) in props {
    match value_opt {
      Some(value) => db
        .set_node_prop(node_id, *prop_key_id, value.clone())
        .map_err(|e| PyRuntimeError::new_err(format!("Failed to set prop: {e}")))?,
      None => db
        .delete_node_prop(node_id, *prop_key_id)
        .map_err(|e| PyRuntimeError::new_err(format!("Failed to delete prop: {e}")))?,
    }
  }

  Ok(node_id as i64)
}

/// List nodes with key prefix on single-file database
pub fn list_nodes_with_prefix_single(db: &RustSingleFileDB, prefix: &str) -> Vec<i64> {
  db.list_nodes()
    .into_iter()
    .filter(|&id| {
      if let Some(key) = db.node_key(id) {
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
      if let Some(key) = db.node_key(id) {
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
