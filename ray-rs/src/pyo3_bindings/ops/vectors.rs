//! Vector operations for Python bindings

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use crate::core::single_file::SingleFileDB as RustSingleFileDB;
use crate::graph::db::GraphDB as RustGraphDB;
use crate::graph::tx::TxHandle as GraphTxHandle;
use crate::graph::vectors::{
  delete_node_vector as graph_delete_node_vector, get_node_vector_db as graph_get_node_vector_db,
  has_node_vector_db as graph_has_node_vector_db, set_node_vector as graph_set_node_vector,
};
use crate::types::{NodeId, PropKeyId};

/// Trait for vector operations
pub trait VectorOps {
  /// Set a vector embedding for a node
  fn set_node_vector_impl(&self, node_id: i64, prop_key_id: u32, vector: Vec<f64>) -> PyResult<()>;
  /// Get a vector embedding for a node
  fn get_node_vector_impl(&self, node_id: i64, prop_key_id: u32) -> PyResult<Option<Vec<f64>>>;
  /// Delete a vector embedding for a node
  fn delete_node_vector_impl(&self, node_id: i64, prop_key_id: u32) -> PyResult<()>;
  /// Check if a node has a vector embedding
  fn has_node_vector_impl(&self, node_id: i64, prop_key_id: u32) -> PyResult<bool>;
}

// ============================================================================
// Single-file database operations
// ============================================================================

pub fn set_node_vector_single(
  db: &RustSingleFileDB,
  node_id: NodeId,
  prop_key_id: PropKeyId,
  vector: &[f32],
) -> PyResult<()> {
  db.set_node_vector(node_id, prop_key_id, vector)
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to set vector: {e}")))
}

pub fn get_node_vector_single(
  db: &RustSingleFileDB,
  node_id: NodeId,
  prop_key_id: PropKeyId,
) -> Option<Vec<f64>> {
  db.get_node_vector(node_id, prop_key_id)
    .map(|v| v.iter().map(|&f| f as f64).collect())
}

pub fn delete_node_vector_single(
  db: &RustSingleFileDB,
  node_id: NodeId,
  prop_key_id: PropKeyId,
) -> PyResult<()> {
  db.delete_node_vector(node_id, prop_key_id)
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to delete vector: {e}")))
}

pub fn has_node_vector_single(
  db: &RustSingleFileDB,
  node_id: NodeId,
  prop_key_id: PropKeyId,
) -> bool {
  db.has_node_vector(node_id, prop_key_id)
}

// ============================================================================
// Graph database operations
// ============================================================================

pub fn set_node_vector_graph(
  handle: &mut GraphTxHandle,
  node_id: NodeId,
  prop_key_id: PropKeyId,
  vector: &[f32],
) -> PyResult<()> {
  graph_set_node_vector(handle, node_id, prop_key_id, vector)
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to set vector: {e}")))?;
  Ok(())
}

pub fn get_node_vector_graph(
  db: &RustGraphDB,
  node_id: NodeId,
  prop_key_id: PropKeyId,
) -> Option<Vec<f64>> {
  graph_get_node_vector_db(db, node_id, prop_key_id).map(|v| v.iter().map(|&f| f as f64).collect())
}

pub fn delete_node_vector_graph(
  handle: &mut GraphTxHandle,
  node_id: NodeId,
  prop_key_id: PropKeyId,
) -> PyResult<()> {
  graph_delete_node_vector(handle, node_id, prop_key_id)
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to delete vector: {e}")))?;
  Ok(())
}

pub fn has_node_vector_graph(db: &RustGraphDB, node_id: NodeId, prop_key_id: PropKeyId) -> bool {
  graph_has_node_vector_db(db, node_id, prop_key_id)
}
