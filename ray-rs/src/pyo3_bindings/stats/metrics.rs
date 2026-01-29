//! Metrics types for Python bindings

use crate::metrics as core_metrics;
use pyo3::prelude::*;

/// Cache layer metrics (single layer - property, traversal, or query)
#[pyclass(name = "CacheLayerMetrics")]
#[derive(Debug, Clone)]
pub struct CacheLayerMetrics {
  #[pyo3(get)]
  pub hits: i64,
  #[pyo3(get)]
  pub misses: i64,
  #[pyo3(get)]
  pub hit_rate: f64,
  #[pyo3(get)]
  pub size: i64,
  #[pyo3(get)]
  pub max_size: i64,
  #[pyo3(get)]
  pub utilization_percent: f64,
}

#[pymethods]
impl CacheLayerMetrics {
  fn __repr__(&self) -> String {
    format!(
      "CacheLayerMetrics(hit_rate={:.2}%, size={}/{})",
      self.hit_rate * 100.0,
      self.size,
      self.max_size
    )
  }
}

impl From<core_metrics::CacheLayerMetrics> for CacheLayerMetrics {
  fn from(metrics: core_metrics::CacheLayerMetrics) -> Self {
    CacheLayerMetrics {
      hits: metrics.hits,
      misses: metrics.misses,
      hit_rate: metrics.hit_rate,
      size: metrics.size,
      max_size: metrics.max_size,
      utilization_percent: metrics.utilization_percent,
    }
  }
}

/// Cache metrics (all cache layers)
#[pyclass(name = "CacheMetrics")]
#[derive(Debug, Clone)]
pub struct CacheMetrics {
  #[pyo3(get)]
  pub enabled: bool,
  #[pyo3(get)]
  pub property_cache: CacheLayerMetrics,
  #[pyo3(get)]
  pub traversal_cache: CacheLayerMetrics,
  #[pyo3(get)]
  pub query_cache: CacheLayerMetrics,
}

#[pymethods]
impl CacheMetrics {
  fn __repr__(&self) -> String {
    format!(
      "CacheMetrics(enabled={}, property={:.1}%, traversal={:.1}%, query={:.1}%)",
      self.enabled,
      self.property_cache.hit_rate * 100.0,
      self.traversal_cache.hit_rate * 100.0,
      self.query_cache.hit_rate * 100.0
    )
  }
}

impl From<core_metrics::CacheMetrics> for CacheMetrics {
  fn from(metrics: core_metrics::CacheMetrics) -> Self {
    CacheMetrics {
      enabled: metrics.enabled,
      property_cache: metrics.property_cache.into(),
      traversal_cache: metrics.traversal_cache.into(),
      query_cache: metrics.query_cache.into(),
    }
  }
}

/// Data metrics (node/edge counts)
#[pyclass(name = "DataMetrics")]
#[derive(Debug, Clone)]
pub struct DataMetrics {
  #[pyo3(get)]
  pub node_count: i64,
  #[pyo3(get)]
  pub edge_count: i64,
  #[pyo3(get)]
  pub delta_nodes_created: i64,
  #[pyo3(get)]
  pub delta_nodes_deleted: i64,
  #[pyo3(get)]
  pub delta_edges_added: i64,
  #[pyo3(get)]
  pub delta_edges_deleted: i64,
  #[pyo3(get)]
  pub snapshot_generation: i64,
  #[pyo3(get)]
  pub max_node_id: i64,
  #[pyo3(get)]
  pub schema_labels: i64,
  #[pyo3(get)]
  pub schema_etypes: i64,
  #[pyo3(get)]
  pub schema_prop_keys: i64,
}

#[pymethods]
impl DataMetrics {
  fn __repr__(&self) -> String {
    format!(
      "DataMetrics(nodes={}, edges={}, gen={})",
      self.node_count, self.edge_count, self.snapshot_generation
    )
  }
}

impl From<core_metrics::DataMetrics> for DataMetrics {
  fn from(metrics: core_metrics::DataMetrics) -> Self {
    DataMetrics {
      node_count: metrics.node_count,
      edge_count: metrics.edge_count,
      delta_nodes_created: metrics.delta_nodes_created,
      delta_nodes_deleted: metrics.delta_nodes_deleted,
      delta_edges_added: metrics.delta_edges_added,
      delta_edges_deleted: metrics.delta_edges_deleted,
      snapshot_generation: metrics.snapshot_generation,
      max_node_id: metrics.max_node_id,
      schema_labels: metrics.schema_labels,
      schema_etypes: metrics.schema_etypes,
      schema_prop_keys: metrics.schema_prop_keys,
    }
  }
}

/// MVCC metrics
#[pyclass(name = "MvccMetrics")]
#[derive(Debug, Clone)]
pub struct MvccMetrics {
  #[pyo3(get)]
  pub enabled: bool,
  #[pyo3(get)]
  pub active_transactions: i64,
  #[pyo3(get)]
  pub versions_pruned: i64,
  #[pyo3(get)]
  pub gc_runs: i64,
  #[pyo3(get)]
  pub min_active_timestamp: i64,
  #[pyo3(get)]
  pub committed_writes_size: i64,
  #[pyo3(get)]
  pub committed_writes_pruned: i64,
}

#[pymethods]
impl MvccMetrics {
  fn __repr__(&self) -> String {
    format!(
      "MvccMetrics(enabled={}, active_tx={}, gc_runs={})",
      self.enabled, self.active_transactions, self.gc_runs
    )
  }
}

impl From<core_metrics::MvccMetrics> for MvccMetrics {
  fn from(metrics: core_metrics::MvccMetrics) -> Self {
    MvccMetrics {
      enabled: metrics.enabled,
      active_transactions: metrics.active_transactions,
      versions_pruned: metrics.versions_pruned,
      gc_runs: metrics.gc_runs,
      min_active_timestamp: metrics.min_active_timestamp,
      committed_writes_size: metrics.committed_writes_size,
      committed_writes_pruned: metrics.committed_writes_pruned,
    }
  }
}

/// MVCC stats (from stats())
#[pyclass(name = "MvccStats")]
#[derive(Debug, Clone)]
pub struct MvccStats {
  #[pyo3(get)]
  pub active_transactions: i64,
  #[pyo3(get)]
  pub min_active_ts: i64,
  #[pyo3(get)]
  pub versions_pruned: i64,
  #[pyo3(get)]
  pub gc_runs: i64,
  #[pyo3(get)]
  pub last_gc_time: i64,
  #[pyo3(get)]
  pub committed_writes_size: i64,
  #[pyo3(get)]
  pub committed_writes_pruned: i64,
}

#[pymethods]
impl MvccStats {
  fn __repr__(&self) -> String {
    format!(
      "MvccStats(active_tx={}, gc_runs={}, versions_pruned={})",
      self.active_transactions, self.gc_runs, self.versions_pruned
    )
  }
}

/// Memory metrics
#[pyclass(name = "MemoryMetrics")]
#[derive(Debug, Clone)]
pub struct MemoryMetrics {
  #[pyo3(get)]
  pub delta_estimate_bytes: i64,
  #[pyo3(get)]
  pub cache_estimate_bytes: i64,
  #[pyo3(get)]
  pub snapshot_bytes: i64,
  #[pyo3(get)]
  pub total_estimate_bytes: i64,
}

#[pymethods]
impl MemoryMetrics {
  /// Get memory in human-readable format
  fn human_readable(&self) -> String {
    let bytes = self.total_estimate_bytes;
    if bytes < 1024 {
      format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
      format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
      format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
      format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
  }

  fn __repr__(&self) -> String {
    format!(
      "MemoryMetrics(total={}, delta={}, cache={}, snapshot={})",
      self.human_readable(),
      self.delta_estimate_bytes,
      self.cache_estimate_bytes,
      self.snapshot_bytes
    )
  }
}

impl From<core_metrics::MemoryMetrics> for MemoryMetrics {
  fn from(metrics: core_metrics::MemoryMetrics) -> Self {
    MemoryMetrics {
      delta_estimate_bytes: metrics.delta_estimate_bytes,
      cache_estimate_bytes: metrics.cache_estimate_bytes,
      snapshot_bytes: metrics.snapshot_bytes,
      total_estimate_bytes: metrics.total_estimate_bytes,
    }
  }
}

/// Database metrics (complete snapshot)
#[pyclass(name = "DatabaseMetrics")]
#[derive(Debug, Clone)]
pub struct DatabaseMetrics {
  #[pyo3(get)]
  pub path: String,
  #[pyo3(get)]
  pub is_single_file: bool,
  #[pyo3(get)]
  pub read_only: bool,
  #[pyo3(get)]
  pub data: DataMetrics,
  #[pyo3(get)]
  pub cache: CacheMetrics,
  #[pyo3(get)]
  pub mvcc: Option<MvccMetrics>,
  #[pyo3(get)]
  pub memory: MemoryMetrics,
  #[pyo3(get)]
  pub collected_at: i64,
}

#[pymethods]
impl DatabaseMetrics {
  fn __repr__(&self) -> String {
    format!(
      "DatabaseMetrics(path='{}', nodes={}, edges={}, memory={})",
      self.path,
      self.data.node_count,
      self.data.edge_count,
      self.memory.human_readable()
    )
  }
}

impl From<core_metrics::DatabaseMetrics> for DatabaseMetrics {
  fn from(metrics: core_metrics::DatabaseMetrics) -> Self {
    DatabaseMetrics {
      path: metrics.path,
      is_single_file: metrics.is_single_file,
      read_only: metrics.read_only,
      data: metrics.data.into(),
      cache: metrics.cache.into(),
      mvcc: metrics.mvcc.map(Into::into),
      memory: metrics.memory.into(),
      collected_at: metrics.collected_at_ms,
    }
  }
}

/// Health check entry
#[pyclass(name = "HealthCheckEntry")]
#[derive(Debug, Clone)]
pub struct HealthCheckEntry {
  #[pyo3(get)]
  pub name: String,
  #[pyo3(get)]
  pub passed: bool,
  #[pyo3(get)]
  pub message: String,
}

#[pymethods]
impl HealthCheckEntry {
  fn __repr__(&self) -> String {
    let status = if self.passed { "PASS" } else { "FAIL" };
    format!(
      "HealthCheckEntry({}: {} - {})",
      self.name, status, self.message
    )
  }

  fn __bool__(&self) -> bool {
    self.passed
  }
}

impl From<core_metrics::HealthCheckEntry> for HealthCheckEntry {
  fn from(entry: core_metrics::HealthCheckEntry) -> Self {
    HealthCheckEntry {
      name: entry.name,
      passed: entry.passed,
      message: entry.message,
    }
  }
}

/// Health check result
#[pyclass(name = "HealthCheckResult")]
#[derive(Debug, Clone)]
pub struct HealthCheckResult {
  #[pyo3(get)]
  pub healthy: bool,
  #[pyo3(get)]
  pub checks: Vec<HealthCheckEntry>,
}

#[pymethods]
impl HealthCheckResult {
  /// Get count of passed checks
  fn passed_count(&self) -> usize {
    self.checks.iter().filter(|c| c.passed).count()
  }

  /// Get count of failed checks
  fn failed_count(&self) -> usize {
    self.checks.iter().filter(|c| !c.passed).count()
  }

  /// Get list of failed checks
  fn failed_checks(&self) -> Vec<HealthCheckEntry> {
    self.checks.iter().filter(|c| !c.passed).cloned().collect()
  }

  fn __repr__(&self) -> String {
    format!(
      "HealthCheckResult(healthy={}, passed={}/{})",
      self.healthy,
      self.passed_count(),
      self.checks.len()
    )
  }

  fn __bool__(&self) -> bool {
    self.healthy
  }
}

impl From<core_metrics::HealthCheckResult> for HealthCheckResult {
  fn from(result: core_metrics::HealthCheckResult) -> Self {
    HealthCheckResult {
      healthy: result.healthy,
      checks: result.checks.into_iter().map(Into::into).collect(),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_memory_metrics_human_readable() {
    let metrics = MemoryMetrics {
      delta_estimate_bytes: 1000,
      cache_estimate_bytes: 2000,
      snapshot_bytes: 3000,
      total_estimate_bytes: 1024 * 1024 * 50, // 50 MB
    };
    assert_eq!(metrics.human_readable(), "50.0 MB");
  }

  #[test]
  fn test_health_check_result_counts() {
    let result = HealthCheckResult {
      healthy: false,
      checks: vec![
        HealthCheckEntry {
          name: "check1".to_string(),
          passed: true,
          message: "ok".to_string(),
        },
        HealthCheckEntry {
          name: "check2".to_string(),
          passed: false,
          message: "failed".to_string(),
        },
      ],
    };
    assert_eq!(result.passed_count(), 1);
    assert_eq!(result.failed_count(), 1);
    assert_eq!(result.failed_checks().len(), 1);
  }
}
