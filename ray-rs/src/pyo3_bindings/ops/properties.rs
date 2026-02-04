//! Property operations for Python bindings

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use crate::core::single_file::SingleFileDB as RustSingleFileDB;
use crate::types::{ETypeId, NodeId, PropKeyId, PropValue as CorePropValue};

use crate::pyo3_bindings::types::{NodeProp, PropValue};

/// Trait for property operations
pub trait PropertyOps {
  /// Set a node property
  fn set_node_prop_impl(&self, node_id: i64, key_id: u32, value: PropValue) -> PyResult<()>;

  /// Get a node property
  fn get_node_prop_impl(&self, node_id: i64, key_id: u32) -> PyResult<Option<PropValue>>;

  /// Delete a node property
  fn delete_node_prop_impl(&self, node_id: i64, key_id: u32) -> PyResult<()>;

  /// Get all properties for a node
  fn get_node_props_impl(&self, node_id: i64) -> PyResult<Option<Vec<NodeProp>>>;

  /// Set an edge property
  fn set_edge_prop_impl(
    &self,
    src: i64,
    etype: u32,
    dst: i64,
    key_id: u32,
    value: PropValue,
  ) -> PyResult<()>;

  /// Get an edge property
  fn get_edge_prop_impl(
    &self,
    src: i64,
    etype: u32,
    dst: i64,
    key_id: u32,
  ) -> PyResult<Option<PropValue>>;

  /// Delete an edge property
  fn delete_edge_prop_impl(&self, src: i64, etype: u32, dst: i64, key_id: u32) -> PyResult<()>;

  /// Get all properties for an edge
  fn get_edge_props_impl(&self, src: i64, etype: u32, dst: i64) -> PyResult<Option<Vec<NodeProp>>>;
}

// ============================================================================
// Single-file database node property operations
// ============================================================================

/// Set node property on single-file database
pub fn set_node_prop_single(
  db: &RustSingleFileDB,
  node_id: NodeId,
  key_id: PropKeyId,
  value: CorePropValue,
) -> PyResult<()> {
  db.set_node_prop(node_id, key_id, value)
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to set property: {e}")))
}

/// Set node property by name on single-file database
pub fn set_node_prop_by_name_single(
  db: &RustSingleFileDB,
  node_id: NodeId,
  key_name: &str,
  value: CorePropValue,
) -> PyResult<()> {
  db.set_node_prop_by_name(node_id, key_name, value)
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to set property: {e}")))
}

/// Get node property on single-file database
pub fn get_node_prop_single(
  db: &RustSingleFileDB,
  node_id: NodeId,
  key_id: PropKeyId,
) -> Option<PropValue> {
  db.node_prop(node_id, key_id).map(|v| v.into())
}

/// Delete node property on single-file database
pub fn delete_node_prop_single(
  db: &RustSingleFileDB,
  node_id: NodeId,
  key_id: PropKeyId,
) -> PyResult<()> {
  db.delete_node_prop(node_id, key_id)
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to delete property: {e}")))
}

/// Get all node properties on single-file database
pub fn get_node_props_single(db: &RustSingleFileDB, node_id: NodeId) -> Option<Vec<NodeProp>> {
  db.node_props(node_id).map(|props| {
    props
      .into_iter()
      .map(|(k, v)| NodeProp {
        key_id: k,
        value: v.into(),
      })
      .collect()
  })
}

// ============================================================================
// Single-file database edge property operations
// ============================================================================

/// Set edge property on single-file database
pub fn set_edge_prop_single(
  db: &RustSingleFileDB,
  src: NodeId,
  etype: ETypeId,
  dst: NodeId,
  key_id: PropKeyId,
  value: CorePropValue,
) -> PyResult<()> {
  db.set_edge_prop(src, etype, dst, key_id, value)
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to set edge property: {e}")))
}

/// Set edge property by name on single-file database
pub fn set_edge_prop_by_name_single(
  db: &RustSingleFileDB,
  src: NodeId,
  etype: ETypeId,
  dst: NodeId,
  key_name: &str,
  value: CorePropValue,
) -> PyResult<()> {
  db.set_edge_prop_by_name(src, etype, dst, key_name, value)
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to set edge property: {e}")))
}

/// Get edge property on single-file database
pub fn get_edge_prop_single(
  db: &RustSingleFileDB,
  src: NodeId,
  etype: ETypeId,
  dst: NodeId,
  key_id: PropKeyId,
) -> Option<PropValue> {
  db.edge_prop(src, etype, dst, key_id).map(|v| v.into())
}

/// Delete edge property on single-file database
pub fn delete_edge_prop_single(
  db: &RustSingleFileDB,
  src: NodeId,
  etype: ETypeId,
  dst: NodeId,
  key_id: PropKeyId,
) -> PyResult<()> {
  db.delete_edge_prop(src, etype, dst, key_id)
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to delete edge property: {e}")))
}

/// Get all edge properties on single-file database
pub fn get_edge_props_single(
  db: &RustSingleFileDB,
  src: NodeId,
  etype: ETypeId,
  dst: NodeId,
) -> Option<Vec<NodeProp>> {
  db.edge_props(src, etype, dst).map(|props| {
    props
      .into_iter()
      .map(|(k, v)| NodeProp {
        key_id: k,
        value: v.into(),
      })
      .collect()
  })
}

// ============================================================================
// Direct type property operations (bypass PropValue wrapper for performance)
// ============================================================================

/// Get string property directly
pub fn get_node_prop_string_single(
  db: &RustSingleFileDB,
  node_id: NodeId,
  key_id: PropKeyId,
) -> Option<String> {
  db.node_prop(node_id, key_id).and_then(|v| match v {
    CorePropValue::String(s) => Some(s),
    _ => None,
  })
}

/// Get int property directly
pub fn get_node_prop_int_single(
  db: &RustSingleFileDB,
  node_id: NodeId,
  key_id: PropKeyId,
) -> Option<i64> {
  db.node_prop(node_id, key_id).and_then(|v| match v {
    CorePropValue::I64(i) => Some(i),
    _ => None,
  })
}

/// Get float property directly
pub fn get_node_prop_float_single(
  db: &RustSingleFileDB,
  node_id: NodeId,
  key_id: PropKeyId,
) -> Option<f64> {
  db.node_prop(node_id, key_id).and_then(|v| match v {
    CorePropValue::F64(f) => Some(f),
    _ => None,
  })
}

/// Get bool property directly
pub fn get_node_prop_bool_single(
  db: &RustSingleFileDB,
  node_id: NodeId,
  key_id: PropKeyId,
) -> Option<bool> {
  db.node_prop(node_id, key_id).and_then(|v| match v {
    CorePropValue::Bool(b) => Some(b),
    _ => None,
  })
}

#[cfg(test)]
mod tests {
  // Property operation tests require database instances
  // Better tested through integration tests
}
