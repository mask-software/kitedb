//! Schema operations for Python bindings

use crate::core::single_file::SingleFileDB as RustSingleFileDB;
use crate::graph::db::GraphDB as RustGraphDB;

/// Trait for schema operations
pub trait SchemaOps {
  /// Get or create a label ID
  fn get_or_create_label_impl(&self, name: &str) -> u32;
  /// Get label ID by name
  fn get_label_id_impl(&self, name: &str) -> Option<u32>;
  /// Get label name by ID
  fn get_label_name_impl(&self, id: u32) -> Option<String>;
  /// Get or create an edge type ID
  fn get_or_create_etype_impl(&self, name: &str) -> u32;
  /// Get edge type ID by name
  fn get_etype_id_impl(&self, name: &str) -> Option<u32>;
  /// Get edge type name by ID
  fn get_etype_name_impl(&self, id: u32) -> Option<String>;
  /// Get or create a property key ID
  fn get_or_create_propkey_impl(&self, name: &str) -> u32;
  /// Get property key ID by name
  fn get_propkey_id_impl(&self, name: &str) -> Option<u32>;
  /// Get property key name by ID
  fn get_propkey_name_impl(&self, id: u32) -> Option<String>;
}

// ============================================================================
// Single-file database operations
// ============================================================================

pub fn get_or_create_label_single(db: &RustSingleFileDB, name: &str) -> u32 {
  db.get_or_create_label(name)
}

pub fn get_label_id_single(db: &RustSingleFileDB, name: &str) -> Option<u32> {
  db.get_label_id(name)
}

pub fn get_label_name_single(db: &RustSingleFileDB, id: u32) -> Option<String> {
  db.get_label_name(id)
}

pub fn get_or_create_etype_single(db: &RustSingleFileDB, name: &str) -> u32 {
  db.get_or_create_etype(name)
}

pub fn get_etype_id_single(db: &RustSingleFileDB, name: &str) -> Option<u32> {
  db.get_etype_id(name)
}

pub fn get_etype_name_single(db: &RustSingleFileDB, id: u32) -> Option<String> {
  db.get_etype_name(id)
}

pub fn get_or_create_propkey_single(db: &RustSingleFileDB, name: &str) -> u32 {
  db.get_or_create_propkey(name)
}

pub fn get_propkey_id_single(db: &RustSingleFileDB, name: &str) -> Option<u32> {
  db.get_propkey_id(name)
}

pub fn get_propkey_name_single(db: &RustSingleFileDB, id: u32) -> Option<String> {
  db.get_propkey_name(id)
}

// ============================================================================
// Graph database operations
// ============================================================================

pub fn get_or_create_label_graph(db: &RustGraphDB, name: &str) -> u32 {
  db.get_or_create_label(name)
}

pub fn get_label_id_graph(db: &RustGraphDB, name: &str) -> Option<u32> {
  db.get_label_id(name)
}

pub fn get_label_name_graph(db: &RustGraphDB, id: u32) -> Option<String> {
  db.get_label_name(id)
}

pub fn get_or_create_etype_graph(db: &RustGraphDB, name: &str) -> u32 {
  db.get_or_create_etype(name)
}

pub fn get_etype_id_graph(db: &RustGraphDB, name: &str) -> Option<u32> {
  db.get_etype_id(name)
}

pub fn get_etype_name_graph(db: &RustGraphDB, id: u32) -> Option<String> {
  db.get_etype_name(id)
}

pub fn get_or_create_propkey_graph(db: &RustGraphDB, name: &str) -> u32 {
  db.get_or_create_propkey(name)
}

pub fn get_propkey_id_graph(db: &RustGraphDB, name: &str) -> Option<u32> {
  db.get_propkey_id(name)
}

pub fn get_propkey_name_graph(db: &RustGraphDB, id: u32) -> Option<String> {
  db.get_propkey_name(id)
}
