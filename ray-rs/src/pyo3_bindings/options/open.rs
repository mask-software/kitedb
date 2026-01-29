//! Database open options for Python bindings

use crate::core::single_file::{
  SingleFileOpenOptions as RustOpenOptions, SyncMode as RustSyncMode,
};
use crate::graph::db::OpenOptions as GraphOpenOptions;
use crate::types::{CacheOptions, PropertyCacheConfig, QueryCacheConfig, TraversalCacheConfig};
use pyo3::prelude::*;

/// Synchronization mode for WAL writes
///
/// Controls the durability vs performance trade-off for commits.
/// - "full": Fsync on every commit (durable to OS, slowest)
/// - "normal": Fsync only on checkpoint (~1000x faster, safe from app crash)
/// - "off": No fsync (fastest, data may be lost on any crash)
#[pyclass(name = "SyncMode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncMode {
  pub(crate) mode: RustSyncMode,
}

#[pymethods]
impl SyncMode {
  /// Full durability: fsync on every commit
  #[staticmethod]
  fn full() -> Self {
    Self {
      mode: RustSyncMode::Full,
    }
  }

  /// Normal: fsync on checkpoint only (~1000x faster)
  /// Safe from application crashes, but not OS crashes.
  #[staticmethod]
  fn normal() -> Self {
    Self {
      mode: RustSyncMode::Normal,
    }
  }

  /// No fsync (fastest, for testing only)
  #[staticmethod]
  fn off() -> Self {
    Self {
      mode: RustSyncMode::Off,
    }
  }

  fn __repr__(&self) -> String {
    match self.mode {
      RustSyncMode::Full => "SyncMode.full()".to_string(),
      RustSyncMode::Normal => "SyncMode.normal()".to_string(),
      RustSyncMode::Off => "SyncMode.off()".to_string(),
    }
  }
}

/// Options for opening a database
#[pyclass(name = "OpenOptions")]
#[derive(Debug, Clone, Default)]
pub struct OpenOptions {
  /// Open in read-only mode
  #[pyo3(get, set)]
  pub read_only: Option<bool>,
  /// Create database if it doesn't exist
  #[pyo3(get, set)]
  pub create_if_missing: Option<bool>,
  /// Acquire file lock (multi-file only)
  #[pyo3(get, set)]
  pub lock_file: Option<bool>,
  /// Require locking support (multi-file only)
  #[pyo3(get, set)]
  pub require_locking: Option<bool>,
  /// Enable MVCC (multi-file only)
  #[pyo3(get, set)]
  pub mvcc: Option<bool>,
  /// MVCC GC interval in ms (multi-file only)
  #[pyo3(get, set)]
  pub mvcc_gc_interval_ms: Option<i64>,
  /// MVCC retention in ms (multi-file only)
  #[pyo3(get, set)]
  pub mvcc_retention_ms: Option<i64>,
  /// MVCC max version chain depth (multi-file only)
  #[pyo3(get, set)]
  pub mvcc_max_chain_depth: Option<u32>,
  /// Page size in bytes (default 4096)
  #[pyo3(get, set)]
  pub page_size: Option<u32>,
  /// WAL size in bytes (default 1MB)
  #[pyo3(get, set)]
  pub wal_size: Option<u32>,
  /// Enable auto-checkpoint when WAL usage exceeds threshold
  #[pyo3(get, set)]
  pub auto_checkpoint: Option<bool>,
  /// WAL usage threshold (0.0-1.0) to trigger auto-checkpoint
  #[pyo3(get, set)]
  pub checkpoint_threshold: Option<f64>,
  /// Use background (non-blocking) checkpoint
  #[pyo3(get, set)]
  pub background_checkpoint: Option<bool>,
  /// Cache parsed snapshot in memory (single-file only)
  #[pyo3(get, set)]
  pub cache_snapshot: Option<bool>,
  /// Enable caching
  #[pyo3(get, set)]
  pub cache_enabled: Option<bool>,
  /// Max node properties in cache
  #[pyo3(get, set)]
  pub cache_max_node_props: Option<i64>,
  /// Max edge properties in cache
  #[pyo3(get, set)]
  pub cache_max_edge_props: Option<i64>,
  /// Max traversal cache entries
  #[pyo3(get, set)]
  pub cache_max_traversal_entries: Option<i64>,
  /// Max query cache entries
  #[pyo3(get, set)]
  pub cache_max_query_entries: Option<i64>,
  /// Query cache TTL in milliseconds
  #[pyo3(get, set)]
  pub cache_query_ttl_ms: Option<i64>,
  /// Sync mode: "full", "normal", or "off"
  pub sync_mode: Option<SyncMode>,
}

#[pymethods]
impl OpenOptions {
  #[new]
  #[pyo3(signature = (
        read_only=None,
        create_if_missing=None,
        lock_file=None,
        require_locking=None,
        mvcc=None,
        mvcc_gc_interval_ms=None,
        mvcc_retention_ms=None,
        mvcc_max_chain_depth=None,
        page_size=None,
        wal_size=None,
        auto_checkpoint=None,
        checkpoint_threshold=None,
        background_checkpoint=None,
        cache_snapshot=None,
        cache_enabled=None,
        cache_max_node_props=None,
        cache_max_edge_props=None,
        cache_max_traversal_entries=None,
        cache_max_query_entries=None,
        cache_query_ttl_ms=None,
        sync_mode=None
    ))]
  #[allow(clippy::too_many_arguments)]
  fn new(
    read_only: Option<bool>,
    create_if_missing: Option<bool>,
    lock_file: Option<bool>,
    require_locking: Option<bool>,
    mvcc: Option<bool>,
    mvcc_gc_interval_ms: Option<i64>,
    mvcc_retention_ms: Option<i64>,
    mvcc_max_chain_depth: Option<u32>,
    page_size: Option<u32>,
    wal_size: Option<u32>,
    auto_checkpoint: Option<bool>,
    checkpoint_threshold: Option<f64>,
    background_checkpoint: Option<bool>,
    cache_snapshot: Option<bool>,
    cache_enabled: Option<bool>,
    cache_max_node_props: Option<i64>,
    cache_max_edge_props: Option<i64>,
    cache_max_traversal_entries: Option<i64>,
    cache_max_query_entries: Option<i64>,
    cache_query_ttl_ms: Option<i64>,
    sync_mode: Option<SyncMode>,
  ) -> Self {
    Self {
      read_only,
      create_if_missing,
      lock_file,
      require_locking,
      mvcc,
      mvcc_gc_interval_ms,
      mvcc_retention_ms,
      mvcc_max_chain_depth,
      page_size,
      wal_size,
      auto_checkpoint,
      checkpoint_threshold,
      background_checkpoint,
      cache_snapshot,
      cache_enabled,
      cache_max_node_props,
      cache_max_edge_props,
      cache_max_traversal_entries,
      cache_max_query_entries,
      cache_query_ttl_ms,
      sync_mode,
    }
  }

  fn __repr__(&self) -> String {
    format!(
      "OpenOptions(read_only={:?}, create_if_missing={:?}, cache_enabled={:?})",
      self.read_only, self.create_if_missing, self.cache_enabled
    )
  }
}

impl From<OpenOptions> for RustOpenOptions {
  fn from(opts: OpenOptions) -> Self {
    let mut rust_opts = RustOpenOptions::new();
    if let Some(v) = opts.read_only {
      rust_opts = rust_opts.read_only(v);
    }
    if let Some(v) = opts.create_if_missing {
      rust_opts = rust_opts.create_if_missing(v);
    }
    if let Some(v) = opts.page_size {
      rust_opts = rust_opts.page_size(v as usize);
    }
    if let Some(v) = opts.wal_size {
      rust_opts = rust_opts.wal_size(v as usize);
    }
    if let Some(v) = opts.auto_checkpoint {
      rust_opts = rust_opts.auto_checkpoint(v);
    }
    if let Some(v) = opts.checkpoint_threshold {
      rust_opts = rust_opts.checkpoint_threshold(v);
    }
    if let Some(v) = opts.background_checkpoint {
      rust_opts = rust_opts.background_checkpoint(v);
    }

    // Cache options
    if opts.cache_enabled == Some(true) {
      let property_cache = Some(PropertyCacheConfig {
        max_node_props: opts.cache_max_node_props.unwrap_or(10000) as usize,
        max_edge_props: opts.cache_max_edge_props.unwrap_or(10000) as usize,
      });

      let traversal_cache = Some(TraversalCacheConfig {
        max_entries: opts.cache_max_traversal_entries.unwrap_or(5000) as usize,
        max_neighbors_per_entry: 100,
      });

      let query_cache = Some(QueryCacheConfig {
        max_entries: opts.cache_max_query_entries.unwrap_or(1000) as usize,
        ttl_ms: opts.cache_query_ttl_ms.map(|v| v as u64),
      });

      rust_opts = rust_opts.cache(Some(CacheOptions {
        enabled: true,
        property_cache,
        traversal_cache,
        query_cache,
      }));
    }

    // Sync mode
    if let Some(sync) = opts.sync_mode {
      rust_opts = rust_opts.sync_mode(sync.mode);
    }

    rust_opts
  }
}

impl OpenOptions {
  /// Convert to GraphOpenOptions for multi-file databases
  pub fn to_graph_options(&self) -> GraphOpenOptions {
    let mut opts = GraphOpenOptions::new();

    if let Some(v) = self.read_only {
      opts.read_only = v;
    }
    if let Some(v) = self.create_if_missing {
      opts.create_if_missing = v;
    }
    if let Some(v) = self.lock_file {
      opts.lock_file = v;
    }
    if let Some(v) = self.mvcc {
      opts.mvcc = v;
    }
    if let Some(v) = self.mvcc_gc_interval_ms {
      opts.mvcc_gc_interval_ms = Some(v as u64);
    }
    if let Some(v) = self.mvcc_retention_ms {
      opts.mvcc_retention_ms = Some(v as u64);
    }
    if let Some(v) = self.mvcc_max_chain_depth {
      opts.mvcc_max_chain_depth = Some(v as usize);
    }

    opts
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_sync_mode_full() {
    let mode = SyncMode::full();
    assert_eq!(mode.mode, RustSyncMode::Full);
  }

  #[test]
  fn test_sync_mode_normal() {
    let mode = SyncMode::normal();
    assert_eq!(mode.mode, RustSyncMode::Normal);
  }

  #[test]
  fn test_sync_mode_off() {
    let mode = SyncMode::off();
    assert_eq!(mode.mode, RustSyncMode::Off);
  }

  #[test]
  fn test_open_options_default() {
    let opts = OpenOptions::default();
    assert!(opts.read_only.is_none());
    assert!(opts.create_if_missing.is_none());
  }

  #[test]
  fn test_open_options_to_rust() {
    let opts = OpenOptions {
      read_only: Some(true),
      create_if_missing: Some(false),
      page_size: Some(8192),
      ..Default::default()
    };
    let rust_opts: RustOpenOptions = opts.into();
    assert!(rust_opts.read_only);
    assert!(!rust_opts.create_if_missing);
  }
}
