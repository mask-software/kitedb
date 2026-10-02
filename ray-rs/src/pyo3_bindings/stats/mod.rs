//! Statistics and metrics types for Python bindings
//!
//! This module contains all statistics and metrics types:
//! - Database stats (nodes, edges, WAL info)
//! - Check results
//! - Cache stats (deprecated stubs: the cache layer was removed)
//! - Comprehensive metrics (data, MVCC, memory, replication)
//! - Health check results

pub mod database;
pub mod metrics;

// Re-export all stats types for convenience
pub use database::{CacheStats, CheckResult, DbStats};
pub use metrics::{
  CacheLayerMetrics, CacheMetrics, DataMetrics, DatabaseMetrics, HealthCheckEntry,
  HealthCheckResult, MemoryMetrics, MvccMetrics, MvccStats, PrimaryReplicationMetrics,
  ReplicaReplicationMetrics, ReplicationMetrics,
};
