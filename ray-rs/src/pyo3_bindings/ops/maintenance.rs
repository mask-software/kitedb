//! Maintenance operations for Python bindings

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use crate::core::single_file::{
  SingleFileDB as RustSingleFileDB, SingleFileOptimizeOptions as RustSingleFileOptimizeOptions,
  VacuumOptions as RustVacuumOptions,
};
use crate::graph::db::GraphDB as RustGraphDB;
use crate::types::CheckResult as RustCheckResult;

use crate::pyo3_bindings::stats::{CheckResult, DbStats, MvccStats};

/// Trait for maintenance operations
pub trait MaintenanceOps {
  /// Perform a checkpoint
  fn checkpoint_impl(&self) -> PyResult<()>;
  /// Perform a background checkpoint
  fn background_checkpoint_impl(&self) -> PyResult<()>;
  /// Check if checkpoint is recommended
  fn should_checkpoint_impl(&self, threshold: f64) -> bool;
  /// Optimize the database
  fn optimize_impl(&self) -> PyResult<()>;
  /// Vacuum the database
  fn vacuum_impl(&self, shrink_wal: bool, min_wal_size: Option<u64>) -> PyResult<()>;
  /// Get database statistics
  fn stats_impl(&self) -> DbStats;
  /// Check database integrity
  fn check_impl(&self) -> CheckResult;
}

// ============================================================================
// Single-file database operations
// ============================================================================

pub fn checkpoint_single(db: &RustSingleFileDB) -> PyResult<()> {
  db.checkpoint()
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to checkpoint: {e}")))
}

pub fn background_checkpoint_single(db: &RustSingleFileDB) -> PyResult<()> {
  db.background_checkpoint()
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to background checkpoint: {e}")))
}

pub fn should_checkpoint_single(db: &RustSingleFileDB, threshold: f64) -> bool {
  db.should_checkpoint(threshold)
}

pub fn optimize_single(
  db: &mut RustSingleFileDB,
  options: Option<RustSingleFileOptimizeOptions>,
) -> PyResult<()> {
  db.optimize_single_file(options)
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to optimize: {e}")))
}

pub fn vacuum_single(
  db: &mut RustSingleFileDB,
  options: Option<RustVacuumOptions>,
) -> PyResult<()> {
  db.vacuum_single_file(options)
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to vacuum: {e}")))
}

pub fn stats_single(db: &RustSingleFileDB) -> DbStats {
  let s = db.stats();
  DbStats {
    snapshot_gen: s.snapshot_gen as i64,
    snapshot_nodes: s.snapshot_nodes as i64,
    snapshot_edges: s.snapshot_edges as i64,
    snapshot_max_node_id: s.snapshot_max_node_id as i64,
    delta_nodes_created: s.delta_nodes_created as i64,
    delta_nodes_deleted: s.delta_nodes_deleted as i64,
    delta_edges_added: s.delta_edges_added as i64,
    delta_edges_deleted: s.delta_edges_deleted as i64,
    wal_segment: s.wal_segment as i64,
    wal_bytes: s.wal_bytes as i64,
    recommend_compact: s.recommend_compact,
    mvcc_stats: s.mvcc_stats.map(|stats| MvccStats {
      active_transactions: stats.active_transactions as i64,
      min_active_ts: stats.min_active_ts as i64,
      versions_pruned: stats.versions_pruned as i64,
      gc_runs: stats.gc_runs as i64,
      last_gc_time: stats.last_gc_time as i64,
      committed_writes_size: stats.committed_writes_size as i64,
      committed_writes_pruned: stats.committed_writes_pruned as i64,
    }),
  }
}

pub fn check_single(db: &RustSingleFileDB) -> CheckResult {
  db.check().into()
}

// ============================================================================
// Graph database operations
// ============================================================================

pub fn optimize_graph(db: &mut RustGraphDB) -> PyResult<()> {
  db.optimize()
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to optimize: {e}")))
}

pub fn check_graph(db: &RustGraphDB) -> RustCheckResult {
  if let Some(ref snapshot) = db.snapshot {
    return crate::check::check_snapshot(snapshot);
  }

  RustCheckResult {
    valid: true,
    errors: Vec::new(),
    warnings: vec!["No snapshot to check".to_string()],
  }
}
