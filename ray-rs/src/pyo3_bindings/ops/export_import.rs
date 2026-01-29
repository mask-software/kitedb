//! Export and import operations for Python bindings

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use crate::core::single_file::SingleFileDB as RustSingleFileDB;
use crate::export as ray_export;
use crate::graph::db::GraphDB as RustGraphDB;

use crate::pyo3_bindings::options::{ExportOptions, ExportResult, ImportOptions, ImportResult};

/// Trait for export/import operations
pub trait ExportImportOps {
  /// Export to JSON file
  fn export_to_json_impl(&self, path: String, options: ExportOptions) -> PyResult<ExportResult>;

  /// Export to JSONL file
  fn export_to_jsonl_impl(&self, path: String, options: ExportOptions) -> PyResult<ExportResult>;

  /// Import from JSON file
  fn import_from_json_impl(&self, path: String, options: ImportOptions) -> PyResult<ImportResult>;
}

// ============================================================================
// Single-file database operations
// ============================================================================

pub fn export_to_object_single(
  db: &RustSingleFileDB,
  options: ray_export::ExportOptions,
) -> PyResult<ray_export::ExportedDatabase> {
  ray_export::export_to_object_single(db, options)
    .map_err(|e| PyRuntimeError::new_err(e.to_string()))
}

pub fn export_to_json_single(
  db: &RustSingleFileDB,
  path: String,
  options: ExportOptions,
) -> PyResult<ExportResult> {
  let opts = options.to_rust();
  let data = ray_export::export_to_object_single(db, opts.clone())
    .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

  let result = ray_export::export_to_json(&data, path, opts.pretty)
    .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

  Ok(ExportResult {
    node_count: result.node_count as i64,
    edge_count: result.edge_count as i64,
  })
}

pub fn export_to_jsonl_single(
  db: &RustSingleFileDB,
  path: String,
  options: ExportOptions,
) -> PyResult<ExportResult> {
  let opts = options.to_rust();
  let data = ray_export::export_to_object_single(db, opts)
    .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

  let result =
    ray_export::export_to_jsonl(&data, path).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

  Ok(ExportResult {
    node_count: result.node_count as i64,
    edge_count: result.edge_count as i64,
  })
}

pub fn import_from_object_single(
  db: &RustSingleFileDB,
  data: &ray_export::ExportedDatabase,
  options: ImportOptions,
) -> PyResult<ImportResult> {
  let opts = options.to_rust();
  let result = ray_export::import_from_object_single(db, data, opts)
    .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

  Ok(ImportResult {
    node_count: result.node_count as i64,
    edge_count: result.edge_count as i64,
    skipped: result.skipped as i64,
  })
}

pub fn import_from_json_single(
  db: &RustSingleFileDB,
  path: String,
  options: ImportOptions,
) -> PyResult<ImportResult> {
  let opts = options.to_rust();
  let parsed =
    ray_export::import_from_json(path).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

  let result = ray_export::import_from_object_single(db, &parsed, opts)
    .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

  Ok(ImportResult {
    node_count: result.node_count as i64,
    edge_count: result.edge_count as i64,
    skipped: result.skipped as i64,
  })
}

// ============================================================================
// Graph database operations
// ============================================================================

pub fn export_to_object_graph(
  db: &RustGraphDB,
  options: ray_export::ExportOptions,
) -> PyResult<ray_export::ExportedDatabase> {
  ray_export::export_to_object_graph(db, options)
    .map_err(|e| PyRuntimeError::new_err(e.to_string()))
}

pub fn export_to_json_graph(
  db: &RustGraphDB,
  path: String,
  options: ExportOptions,
) -> PyResult<ExportResult> {
  let opts = options.to_rust();
  let data = ray_export::export_to_object_graph(db, opts.clone())
    .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

  let result = ray_export::export_to_json(&data, path, opts.pretty)
    .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

  Ok(ExportResult {
    node_count: result.node_count as i64,
    edge_count: result.edge_count as i64,
  })
}

pub fn export_to_jsonl_graph(
  db: &RustGraphDB,
  path: String,
  options: ExportOptions,
) -> PyResult<ExportResult> {
  let opts = options.to_rust();
  let data = ray_export::export_to_object_graph(db, opts)
    .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

  let result =
    ray_export::export_to_jsonl(&data, path).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

  Ok(ExportResult {
    node_count: result.node_count as i64,
    edge_count: result.edge_count as i64,
  })
}

pub fn import_from_object_graph(
  db: &RustGraphDB,
  data: &ray_export::ExportedDatabase,
  options: ImportOptions,
) -> PyResult<ImportResult> {
  let opts = options.to_rust();
  let result = ray_export::import_from_object_graph(db, data, opts)
    .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

  Ok(ImportResult {
    node_count: result.node_count as i64,
    edge_count: result.edge_count as i64,
    skipped: result.skipped as i64,
  })
}

pub fn import_from_json_graph(
  db: &RustGraphDB,
  path: String,
  options: ImportOptions,
) -> PyResult<ImportResult> {
  let opts = options.to_rust();
  let parsed =
    ray_export::import_from_json(path).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

  let result = ray_export::import_from_object_graph(db, &parsed, opts)
    .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

  Ok(ImportResult {
    node_count: result.node_count as i64,
    edge_count: result.edge_count as i64,
    skipped: result.skipped as i64,
  })
}
