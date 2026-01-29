//! Database statistics types for Python bindings

use super::metrics::MvccStats;
use crate::types::CheckResult as RustCheckResult;
use pyo3::prelude::*;

/// Database statistics
#[pyclass(name = "DbStats")]
#[derive(Debug, Clone)]
pub struct DbStats {
  #[pyo3(get)]
  pub snapshot_gen: i64,
  #[pyo3(get)]
  pub snapshot_nodes: i64,
  #[pyo3(get)]
  pub snapshot_edges: i64,
  #[pyo3(get)]
  pub snapshot_max_node_id: i64,
  #[pyo3(get)]
  pub delta_nodes_created: i64,
  #[pyo3(get)]
  pub delta_nodes_deleted: i64,
  #[pyo3(get)]
  pub delta_edges_added: i64,
  #[pyo3(get)]
  pub delta_edges_deleted: i64,
  #[pyo3(get)]
  pub wal_segment: i64,
  #[pyo3(get)]
  pub wal_bytes: i64,
  #[pyo3(get)]
  pub recommend_compact: bool,
  #[pyo3(get)]
  pub mvcc_stats: Option<MvccStats>,
}

#[pymethods]
impl DbStats {
  /// Get the current node count (snapshot + delta)
  fn node_count(&self) -> i64 {
    self.snapshot_nodes + self.delta_nodes_created - self.delta_nodes_deleted
  }

  /// Get the current edge count (snapshot + delta)
  fn edge_count(&self) -> i64 {
    self.snapshot_edges + self.delta_edges_added - self.delta_edges_deleted
  }

  fn __repr__(&self) -> String {
    format!(
      "DbStats(nodes={}, edges={}, wal_bytes={}, recommend_compact={})",
      self.node_count(),
      self.edge_count(),
      self.wal_bytes,
      self.recommend_compact
    )
  }
}

/// Database integrity check result
#[pyclass(name = "CheckResult")]
#[derive(Debug, Clone)]
pub struct CheckResult {
  #[pyo3(get)]
  pub valid: bool,
  #[pyo3(get)]
  pub errors: Vec<String>,
  #[pyo3(get)]
  pub warnings: Vec<String>,
}

#[pymethods]
impl CheckResult {
  /// Check if the database is valid (no errors)
  fn is_valid(&self) -> bool {
    self.valid
  }

  /// Check if there are any warnings
  fn has_warnings(&self) -> bool {
    !self.warnings.is_empty()
  }

  /// Get error count
  fn error_count(&self) -> usize {
    self.errors.len()
  }

  /// Get warning count
  fn warning_count(&self) -> usize {
    self.warnings.len()
  }

  fn __repr__(&self) -> String {
    format!(
      "CheckResult(valid={}, errors={}, warnings={})",
      self.valid,
      self.errors.len(),
      self.warnings.len()
    )
  }

  fn __bool__(&self) -> bool {
    self.valid
  }
}

impl From<RustCheckResult> for CheckResult {
  fn from(result: RustCheckResult) -> Self {
    CheckResult {
      valid: result.valid,
      errors: result.errors,
      warnings: result.warnings,
    }
  }
}

/// Cache statistics
#[pyclass(name = "CacheStats")]
#[derive(Debug, Clone)]
pub struct CacheStats {
  #[pyo3(get)]
  pub property_cache_hits: i64,
  #[pyo3(get)]
  pub property_cache_misses: i64,
  #[pyo3(get)]
  pub property_cache_size: i64,
  #[pyo3(get)]
  pub traversal_cache_hits: i64,
  #[pyo3(get)]
  pub traversal_cache_misses: i64,
  #[pyo3(get)]
  pub traversal_cache_size: i64,
  #[pyo3(get)]
  pub query_cache_hits: i64,
  #[pyo3(get)]
  pub query_cache_misses: i64,
  #[pyo3(get)]
  pub query_cache_size: i64,
}

#[pymethods]
impl CacheStats {
  /// Get property cache hit rate
  fn property_hit_rate(&self) -> f64 {
    let total = self.property_cache_hits + self.property_cache_misses;
    if total == 0 {
      0.0
    } else {
      self.property_cache_hits as f64 / total as f64
    }
  }

  /// Get traversal cache hit rate
  fn traversal_hit_rate(&self) -> f64 {
    let total = self.traversal_cache_hits + self.traversal_cache_misses;
    if total == 0 {
      0.0
    } else {
      self.traversal_cache_hits as f64 / total as f64
    }
  }

  /// Get query cache hit rate
  fn query_hit_rate(&self) -> f64 {
    let total = self.query_cache_hits + self.query_cache_misses;
    if total == 0 {
      0.0
    } else {
      self.query_cache_hits as f64 / total as f64
    }
  }

  fn __repr__(&self) -> String {
    format!(
      "CacheStats(property_hits={}, traversal_hits={}, query_hits={})",
      self.property_cache_hits, self.traversal_cache_hits, self.query_cache_hits
    )
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_db_stats_node_count() {
    let stats = DbStats {
      snapshot_gen: 1,
      snapshot_nodes: 100,
      snapshot_edges: 200,
      snapshot_max_node_id: 100,
      delta_nodes_created: 10,
      delta_nodes_deleted: 5,
      delta_edges_added: 20,
      delta_edges_deleted: 10,
      wal_segment: 0,
      wal_bytes: 1024,
      recommend_compact: false,
      mvcc_stats: None,
    };
    assert_eq!(stats.node_count(), 105);
    assert_eq!(stats.edge_count(), 210);
  }

  #[test]
  fn test_check_result_from_rust() {
    let rust_result = RustCheckResult {
      valid: true,
      errors: vec![],
      warnings: vec!["warning".to_string()],
    };
    let result: CheckResult = rust_result.into();
    assert!(result.is_valid());
    assert!(result.has_warnings());
    assert_eq!(result.error_count(), 0);
    assert_eq!(result.warning_count(), 1);
  }

  #[test]
  fn test_cache_stats_hit_rate() {
    let stats = CacheStats {
      property_cache_hits: 80,
      property_cache_misses: 20,
      property_cache_size: 100,
      traversal_cache_hits: 0,
      traversal_cache_misses: 0,
      traversal_cache_size: 0,
      query_cache_hits: 50,
      query_cache_misses: 50,
      query_cache_size: 100,
    };
    assert!((stats.property_hit_rate() - 0.8).abs() < 0.001);
    assert!((stats.traversal_hit_rate() - 0.0).abs() < 0.001);
    assert!((stats.query_hit_rate() - 0.5).abs() < 0.001);
  }
}
