//! Schema operations for Python bindings

use crate::pyo3_bindings::errors;
use pyo3::prelude::*;

use crate::core::single_file::SingleFileDB as RustSingleFileDB;

/// Trait for schema operations
pub trait SchemaOps {
  /// Get or create a label ID
  fn ensure_label_impl(&self, name: &str) -> PyResult<u32>;
  /// Get label ID by name
  fn label_id_impl(&self, name: &str) -> Option<u32>;
  /// Get label name by ID
  fn label_name_impl(&self, id: u32) -> Option<String>;
  /// Get or create an edge type ID
  fn ensure_etype_impl(&self, name: &str) -> PyResult<u32>;
  /// Get edge type ID by name
  fn etype_id_impl(&self, name: &str) -> Option<u32>;
  /// Get edge type name by ID
  fn etype_name_impl(&self, id: u32) -> Option<String>;
  /// Get or create a property key ID
  fn ensure_propkey_impl(&self, name: &str) -> PyResult<u32>;
  /// Get property key ID by name
  fn propkey_id_impl(&self, name: &str) -> Option<u32>;
  /// Get property key name by ID
  fn propkey_name_impl(&self, id: u32) -> Option<String>;
}

// ============================================================================
// Single-file database operations
// ============================================================================

pub fn ensure_label_single(db: &RustSingleFileDB, name: &str) -> PyResult<u32> {
  db.ensure_label(name)
    .map_err(|e| errors::wrap(e, "Failed to ensure label"))
}

pub fn label_id_single(db: &RustSingleFileDB, name: &str) -> Option<u32> {
  db.label_id(name)
}

pub fn label_name_single(db: &RustSingleFileDB, id: u32) -> Option<String> {
  db.label_name(id)
}

pub fn ensure_etype_single(db: &RustSingleFileDB, name: &str) -> PyResult<u32> {
  db.ensure_etype(name)
    .map_err(|e| errors::wrap(e, "Failed to ensure edge type"))
}

pub fn etype_id_single(db: &RustSingleFileDB, name: &str) -> Option<u32> {
  db.etype_id(name)
}

pub fn etype_name_single(db: &RustSingleFileDB, id: u32) -> Option<String> {
  db.etype_name(id)
}

pub fn ensure_propkey_single(db: &RustSingleFileDB, name: &str) -> PyResult<u32> {
  db.ensure_propkey(name)
    .map_err(|e| errors::wrap(e, "Failed to ensure property key"))
}

pub fn propkey_id_single(db: &RustSingleFileDB, name: &str) -> Option<u32> {
  db.propkey_id(name)
}

pub fn propkey_name_single(db: &RustSingleFileDB, id: u32) -> Option<String> {
  db.propkey_name(id)
}
