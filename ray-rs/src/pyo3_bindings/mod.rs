//! Python bindings for RayDB using PyO3
//!
//! Exposes SingleFileDB and related types to Python.
//!
//! ## Module Structure
//!
//! - `database` - Core Database class and main operations
//! - `types` - Basic types (PropValue, Edge, FullEdge, NodeProp, etc.)
//! - `options` - Configuration options (OpenOptions, BackupOptions, etc.)
//! - `stats` - Statistics and metrics types
//! - `ops` - Operation implementations organized by domain
//! - `helpers` - Internal helper functions
//! - `traversal` - Traversal result types
//! - `vector` - Vector index types

#[cfg(feature = "python")]
pub mod database;
#[cfg(feature = "python")]
pub mod helpers;
#[cfg(feature = "python")]
pub mod ops;
#[cfg(feature = "python")]
pub mod options;
#[cfg(feature = "python")]
pub mod stats;
#[cfg(feature = "python")]
pub mod traversal;
#[cfg(feature = "python")]
pub mod types;
#[cfg(feature = "python")]
pub mod vector;

#[cfg(feature = "python")]
pub use database::*;
#[cfg(feature = "python")]
pub use traversal::*;
#[cfg(feature = "python")]
pub use vector::*;

#[cfg(feature = "python")]
use pyo3::prelude::*;

/// RayDB Python module
#[cfg(feature = "python")]
#[pymodule]
#[pyo3(name = "_raydb")]
pub fn raydb(m: &Bound<'_, PyModule>) -> PyResult<()> {
  // Database class
  m.add_class::<database::PyDatabase>()?;

  // Options classes
  m.add_class::<options::OpenOptions>()?;
  m.add_class::<options::SyncMode>()?;
  m.add_class::<options::CompressionOptions>()?;
  m.add_class::<options::SingleFileOptimizeOptions>()?;
  m.add_class::<options::VacuumOptions>()?;
  m.add_class::<options::ExportOptions>()?;
  m.add_class::<options::ImportOptions>()?;
  m.add_class::<options::StreamOptions>()?;
  m.add_class::<options::PaginationOptions>()?;
  m.add_class::<options::BackupOptions>()?;
  m.add_class::<options::RestoreOptions>()?;
  m.add_class::<options::OfflineBackupOptions>()?;

  // Stats classes
  m.add_class::<stats::DbStats>()?;
  m.add_class::<stats::CheckResult>()?;
  m.add_class::<stats::CacheStats>()?;
  m.add_class::<stats::CacheLayerMetrics>()?;
  m.add_class::<stats::CacheMetrics>()?;
  m.add_class::<stats::DataMetrics>()?;
  m.add_class::<stats::MvccMetrics>()?;
  m.add_class::<stats::MvccStats>()?;
  m.add_class::<stats::MemoryMetrics>()?;
  m.add_class::<stats::DatabaseMetrics>()?;
  m.add_class::<stats::HealthCheckEntry>()?;
  m.add_class::<stats::HealthCheckResult>()?;

  // Types classes
  m.add_class::<types::PropValue>()?;
  m.add_class::<types::Edge>()?;
  m.add_class::<types::FullEdge>()?;
  m.add_class::<types::NodeProp>()?;
  m.add_class::<types::NodeWithProps>()?;
  m.add_class::<types::EdgeWithProps>()?;
  m.add_class::<types::NodePage>()?;
  m.add_class::<types::EdgePage>()?;

  // Result types from options (ExportResult, ImportResult, BackupResult)
  m.add_class::<options::ExportResult>()?;
  m.add_class::<options::ImportResult>()?;
  m.add_class::<options::BackupResult>()?;

  // Traversal result classes
  m.add_class::<traversal::PyTraversalResult>()?;
  m.add_class::<traversal::PyPathResult>()?;
  m.add_class::<traversal::PyPathEdge>()?;

  // Vector search classes
  m.add_class::<vector::PyIvfIndex>()?;
  m.add_class::<vector::PyIvfPqIndex>()?;
  m.add_class::<vector::PyIvfConfig>()?;
  m.add_class::<vector::PyPqConfig>()?;
  m.add_class::<vector::PySearchOptions>()?;
  m.add_class::<vector::PySearchResult>()?;
  m.add_class::<vector::PyIvfStats>()?;

  // Standalone functions
  m.add_function(wrap_pyfunction!(database::open_database, m)?)?;
  m.add_function(wrap_pyfunction!(database::collect_metrics, m)?)?;
  m.add_function(wrap_pyfunction!(database::health_check, m)?)?;
  m.add_function(wrap_pyfunction!(database::create_backup, m)?)?;
  m.add_function(wrap_pyfunction!(database::restore_backup, m)?)?;
  m.add_function(wrap_pyfunction!(database::get_backup_info, m)?)?;
  m.add_function(wrap_pyfunction!(database::create_offline_backup, m)?)?;
  m.add_function(wrap_pyfunction!(version, m)?)?;
  m.add_function(wrap_pyfunction!(vector::brute_force_search, m)?)?;

  Ok(())
}

/// Get RayDB version
#[cfg(feature = "python")]
#[pyfunction]
pub fn version() -> String {
  env!("CARGO_PKG_VERSION").to_string()
}
