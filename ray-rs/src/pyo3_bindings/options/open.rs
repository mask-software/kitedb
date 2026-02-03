//! Database open options for Python bindings

use super::maintenance::CompressionOptions;
use crate::core::single_file::{
  SingleFileOpenOptions as RustOpenOptions, SnapshotParseMode as RustSnapshotParseMode,
  SyncMode as RustSyncMode,
};
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

/// Snapshot parse behavior for single-file databases
///
/// - "strict": Fail open if snapshot parsing fails
/// - "salvage": Ignore snapshot parse errors and recover from WAL only
#[pyclass(name = "SnapshotParseMode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotParseMode {
  pub(crate) mode: RustSnapshotParseMode,
}

#[pymethods]
impl SnapshotParseMode {
  /// Strict: snapshot parse errors are fatal
  #[staticmethod]
  fn strict() -> Self {
    Self {
      mode: RustSnapshotParseMode::Strict,
    }
  }

  /// Salvage: ignore snapshot parse errors and recover from WAL only
  #[staticmethod]
  fn salvage() -> Self {
    Self {
      mode: RustSnapshotParseMode::Salvage,
    }
  }

  fn __repr__(&self) -> String {
    match self.mode {
      RustSnapshotParseMode::Strict => "SnapshotParseMode.strict()".to_string(),
      RustSnapshotParseMode::Salvage => "SnapshotParseMode.salvage()".to_string(),
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
  /// Enable MVCC (snapshot isolation + conflict detection)
  #[pyo3(get, set)]
  pub mvcc: Option<bool>,
  /// MVCC GC interval in ms
  #[pyo3(get, set)]
  pub mvcc_gc_interval_ms: Option<i64>,
  /// MVCC retention in ms
  #[pyo3(get, set)]
  pub mvcc_retention_ms: Option<i64>,
  /// MVCC max version chain depth
  #[pyo3(get, set)]
  pub mvcc_max_chain_depth: Option<i64>,
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
  /// Compression options for checkpoint snapshots (single-file only)
  #[pyo3(get, set)]
  pub checkpoint_compression: Option<CompressionOptions>,
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
  /// Snapshot parse mode: "strict" or "salvage" (single-file only)
  #[pyo3(get, set)]
  pub snapshot_parse_mode: Option<SnapshotParseMode>,
}

#[pymethods]
impl OpenOptions {
  #[new]
  #[pyo3(signature = (
        read_only=None,
        create_if_missing=None,
        mvcc=None,
        mvcc_gc_interval_ms=None,
        mvcc_retention_ms=None,
        mvcc_max_chain_depth=None,
        page_size=None,
        wal_size=None,
        auto_checkpoint=None,
        checkpoint_threshold=None,
        background_checkpoint=None,
        checkpoint_compression=None,
        cache_snapshot=None,
        cache_enabled=None,
        cache_max_node_props=None,
        cache_max_edge_props=None,
        cache_max_traversal_entries=None,
        cache_max_query_entries=None,
        cache_query_ttl_ms=None,
        sync_mode=None,
        snapshot_parse_mode=None
    ))]
  #[allow(clippy::too_many_arguments)]
  fn new(
    read_only: Option<bool>,
    create_if_missing: Option<bool>,
    mvcc: Option<bool>,
    mvcc_gc_interval_ms: Option<i64>,
    mvcc_retention_ms: Option<i64>,
    mvcc_max_chain_depth: Option<i64>,
    page_size: Option<u32>,
    wal_size: Option<u32>,
    auto_checkpoint: Option<bool>,
    checkpoint_threshold: Option<f64>,
    background_checkpoint: Option<bool>,
    checkpoint_compression: Option<CompressionOptions>,
    cache_snapshot: Option<bool>,
    cache_enabled: Option<bool>,
    cache_max_node_props: Option<i64>,
    cache_max_edge_props: Option<i64>,
    cache_max_traversal_entries: Option<i64>,
    cache_max_query_entries: Option<i64>,
    cache_query_ttl_ms: Option<i64>,
    sync_mode: Option<SyncMode>,
    snapshot_parse_mode: Option<SnapshotParseMode>,
  ) -> Self {
    Self {
      read_only,
      create_if_missing,
      mvcc,
      mvcc_gc_interval_ms,
      mvcc_retention_ms,
      mvcc_max_chain_depth,
      page_size,
      wal_size,
      auto_checkpoint,
      checkpoint_threshold,
      background_checkpoint,
      checkpoint_compression,
      cache_snapshot,
      cache_enabled,
      cache_max_node_props,
      cache_max_edge_props,
      cache_max_traversal_entries,
      cache_max_query_entries,
      cache_query_ttl_ms,
      sync_mode,
      snapshot_parse_mode,
    }
  }

  fn __repr__(&self) -> String {
    format!(
      "OpenOptions(read_only={:?}, create_if_missing={:?}, cache_enabled={:?})",
      self.read_only, self.create_if_missing, self.cache_enabled
    )
  }
}

impl TryFrom<OpenOptions> for RustOpenOptions {
  type Error = PyErr;

  fn try_from(opts: OpenOptions) -> Result<Self, Self::Error> {
    opts.to_single_file_options()
  }
}

impl OpenOptions {
  /// Convert to single-file open options with validation
  pub fn to_single_file_options(&self) -> PyResult<RustOpenOptions> {
    let mut rust_opts = RustOpenOptions::new();
    if let Some(v) = self.read_only {
      rust_opts = rust_opts.read_only(v);
    }
    if let Some(v) = self.create_if_missing {
      rust_opts = rust_opts.create_if_missing(v);
    }
    if let Some(v) = self.mvcc {
      rust_opts = rust_opts.mvcc(v);
    }
    if let Some(v) = self.mvcc_gc_interval_ms {
      rust_opts = rust_opts.mvcc_gc_interval_ms(v as u64);
    }
    if let Some(v) = self.mvcc_retention_ms {
      rust_opts = rust_opts.mvcc_retention_ms(v as u64);
    }
    if let Some(v) = self.mvcc_max_chain_depth {
      rust_opts = rust_opts.mvcc_max_chain_depth(v as usize);
    }
    if let Some(v) = self.page_size {
      rust_opts = rust_opts.page_size(v as usize);
    }
    if let Some(v) = self.wal_size {
      rust_opts = rust_opts.wal_size(v as usize);
    }
    if let Some(v) = self.auto_checkpoint {
      rust_opts = rust_opts.auto_checkpoint(v);
    }
    if let Some(v) = self.checkpoint_threshold {
      rust_opts = rust_opts.checkpoint_threshold(v);
    }
    if let Some(v) = self.background_checkpoint {
      rust_opts = rust_opts.background_checkpoint(v);
    }
    if let Some(ref compression) = self.checkpoint_compression {
      rust_opts = rust_opts.checkpoint_compression(Some(compression.to_core()?));
    }

    // Cache options
    if self.cache_enabled == Some(true) {
      let property_cache = Some(PropertyCacheConfig {
        max_node_props: self.cache_max_node_props.unwrap_or(10000) as usize,
        max_edge_props: self.cache_max_edge_props.unwrap_or(10000) as usize,
      });

      let traversal_cache = Some(TraversalCacheConfig {
        max_entries: self.cache_max_traversal_entries.unwrap_or(5000) as usize,
        max_neighbors_per_entry: 100,
      });

      let query_cache = Some(QueryCacheConfig {
        max_entries: self.cache_max_query_entries.unwrap_or(1000) as usize,
        ttl_ms: self.cache_query_ttl_ms.map(|v| v as u64),
      });

      rust_opts = rust_opts.cache(Some(CacheOptions {
        enabled: true,
        property_cache,
        traversal_cache,
        query_cache,
      }));
    }

    // Sync mode
    if let Some(sync) = self.sync_mode {
      rust_opts = rust_opts.sync_mode(sync.mode);
    }
    if let Some(mode) = self.snapshot_parse_mode {
      rust_opts = rust_opts.snapshot_parse_mode(mode.mode);
    }

    Ok(rust_opts)
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
    let rust_opts: RustOpenOptions = opts.try_into().unwrap();
    assert!(rust_opts.read_only);
    assert!(!rust_opts.create_if_missing);
  }
}
