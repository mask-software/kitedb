//! Vector operations for Python bindings

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use crate::core::single_file::SingleFileDB as RustSingleFileDB;
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
  db.node_vector(node_id, prop_key_id)
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
