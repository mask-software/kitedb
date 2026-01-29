//! Node label operations for Python bindings

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use crate::core::single_file::SingleFileDB as RustSingleFileDB;
use crate::graph::db::GraphDB as RustGraphDB;
use crate::graph::definitions::define_label as graph_define_label;
use crate::graph::nodes::{
  add_node_label as graph_add_node_label, get_node_labels_db, node_has_label_db,
  remove_node_label as graph_remove_node_label,
};
use crate::graph::tx::TxHandle as GraphTxHandle;
use crate::types::NodeId;

/// Trait for label operations
pub trait LabelOps {
  /// Define a new label
  fn define_label_impl(&self, name: &str) -> PyResult<u32>;
  /// Add a label to a node
  fn add_node_label_impl(&self, node_id: i64, label_id: u32) -> PyResult<()>;
  /// Remove a label from a node
  fn remove_node_label_impl(&self, node_id: i64, label_id: u32) -> PyResult<()>;
  /// Check if a node has a label
  fn node_has_label_impl(&self, node_id: i64, label_id: u32) -> PyResult<bool>;
  /// Get all labels for a node
  fn get_node_labels_impl(&self, node_id: i64) -> PyResult<Vec<u32>>;
}

// ============================================================================
// Single-file database operations
// ============================================================================

pub fn define_label_single(db: &RustSingleFileDB, name: &str) -> PyResult<u32> {
  db.define_label(name)
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to define label: {e}")))
}

pub fn add_node_label_single(
  db: &RustSingleFileDB,
  node_id: NodeId,
  label_id: u32,
) -> PyResult<()> {
  db.add_node_label(node_id, label_id)
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to add label: {e}")))
}

pub fn add_node_label_by_name_single(
  db: &RustSingleFileDB,
  node_id: NodeId,
  label_name: &str,
) -> PyResult<()> {
  db.add_node_label_by_name(node_id, label_name)
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to add label: {e}")))
}

pub fn remove_node_label_single(
  db: &RustSingleFileDB,
  node_id: NodeId,
  label_id: u32,
) -> PyResult<()> {
  db.remove_node_label(node_id, label_id)
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to remove label: {e}")))
}

pub fn node_has_label_single(db: &RustSingleFileDB, node_id: NodeId, label_id: u32) -> bool {
  db.node_has_label(node_id, label_id)
}

pub fn get_node_labels_single(db: &RustSingleFileDB, node_id: NodeId) -> Vec<u32> {
  db.get_node_labels(node_id)
}

// ============================================================================
// Graph database operations
// ============================================================================

pub fn define_label_graph(handle: &mut GraphTxHandle, name: &str) -> PyResult<u32> {
  graph_define_label(handle, name)
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to define label: {e}")))
}

pub fn add_node_label_graph(
  handle: &mut GraphTxHandle,
  node_id: NodeId,
  label_id: u32,
) -> PyResult<()> {
  graph_add_node_label(handle, node_id, label_id)
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to add label: {e}")))?;
  Ok(())
}

pub fn remove_node_label_graph(
  handle: &mut GraphTxHandle,
  node_id: NodeId,
  label_id: u32,
) -> PyResult<()> {
  graph_remove_node_label(handle, node_id, label_id)
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to remove label: {e}")))?;
  Ok(())
}

pub fn node_has_label_graph(db: &RustGraphDB, node_id: NodeId, label_id: u32) -> bool {
  node_has_label_db(db, node_id, label_id)
}

pub fn get_node_labels_graph(db: &RustGraphDB, node_id: NodeId) -> Vec<u32> {
  get_node_labels_db(db, node_id)
}
