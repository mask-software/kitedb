//! Database open options for Python bindings

use super::maintenance::CompressionOptions;
use crate::api::kite::{
  KiteOptions as RustKiteOptions, KiteRuntimeProfile as RustKiteRuntimeProfile,
};
use crate::core::single_file::{
  SingleFileOpenOptions as RustOpenOptions, SnapshotParseMode as RustSnapshotParseMode,
  SyncMode as RustSyncMode,
};
use crate::pyo3_bindings::validation;
use crate::replication::types::ReplicationRole;
use crate::types::{CacheOptions, PropertyCacheConfig, QueryCacheConfig, TraversalCacheConfig};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use std::str::FromStr;

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
  /// MVCC retention in ms (0 means retain no historical window)
  #[pyo3(get, set)]
  pub mvcc_retention_ms: Option<i64>,
  /// MVCC max version chain depth (must be positive)
  #[pyo3(get, set)]
  pub mvcc_max_chain_depth: Option<i64>,
  /// Page size in bytes (must be a supported positive power of two)
  #[pyo3(get, set)]
  pub page_size: Option<i64>,
  /// WAL size in bytes (must be positive and at least 16 pages)
  #[pyo3(get, set)]
  pub wal_size: Option<i64>,
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
  /// Max node properties in cache (0 disables the node-property cache)
  #[pyo3(get, set)]
  pub cache_max_node_props: Option<i64>,
  /// Max edge properties in cache (0 disables the edge-property cache)
  #[pyo3(get, set)]
  pub cache_max_edge_props: Option<i64>,
  /// Max traversal cache entries (0 disables the traversal cache)
  #[pyo3(get, set)]
  pub cache_max_traversal_entries: Option<i64>,
  /// Max query cache entries (0 disables the query cache)
  #[pyo3(get, set)]
  pub cache_max_query_entries: Option<i64>,
  /// Query cache TTL in milliseconds (0 expires entries immediately)
  #[pyo3(get, set)]
  pub cache_query_ttl_ms: Option<i64>,
  /// Sync mode: "full", "normal", or "off"
  pub sync_mode: Option<SyncMode>,
  /// Enable group commit (coalesce WAL flushes across commits)
  #[pyo3(get, set)]
  pub group_commit_enabled: Option<bool>,
  /// Group commit window in milliseconds (0 adds no coalescing delay)
  #[pyo3(get, set)]
  pub group_commit_window_ms: Option<i64>,
  /// Snapshot parse mode: "strict" or "salvage" (single-file only)
  #[pyo3(get, set)]
  pub snapshot_parse_mode: Option<SnapshotParseMode>,
  /// Replication role: "disabled", "primary", or "replica"
  #[pyo3(get, set)]
  pub replication_role: Option<String>,
  /// Replication sidecar path override
  #[pyo3(get, set)]
  pub replication_sidecar_path: Option<String>,
  /// Source primary db path (replica role only)
  #[pyo3(get, set)]
  pub replication_source_db_path: Option<String>,
  /// Source primary sidecar path (replica role only)
  #[pyo3(get, set)]
  pub replication_source_sidecar_path: Option<String>,
  /// Segment rotation threshold in bytes (primary role only)
  #[pyo3(get, set)]
  pub replication_segment_max_bytes: Option<i64>,
  /// Minimum retained entries window (0 imposes no entry-count floor)
  #[pyo3(get, set)]
  pub replication_retention_min_entries: Option<i64>,
  /// Minimum retained segment age in milliseconds (0 imposes no age floor)
  #[pyo3(get, set)]
  pub replication_retention_min_ms: Option<i64>,
  /// TEST-ONLY: skip database file locking to simulate multi-node topologies
  /// (e.g. split-brain fencing tests) in a single process. Honored only when
  /// the KITEDB_DANGER_ALLOW_MULTI_NODE_SIMULATION environment variable is
  /// set; rejected otherwise. Never use this outside tests: it removes the
  /// corruption protection that prevents two writers on one database file.
  #[pyo3(get, set)]
  pub danger_bypass_file_lock_for_multi_node_simulation: Option<bool>,
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
        group_commit_enabled=None,
        group_commit_window_ms=None,
        snapshot_parse_mode=None,
        replication_role=None,
        replication_sidecar_path=None,
        replication_source_db_path=None,
        replication_source_sidecar_path=None,
        replication_segment_max_bytes=None,
        replication_retention_min_entries=None,
        replication_retention_min_ms=None,
        danger_bypass_file_lock_for_multi_node_simulation=None
    ))]
  #[allow(clippy::too_many_arguments)]
  fn new(
    read_only: Option<bool>,
    create_if_missing: Option<bool>,
    mvcc: Option<bool>,
    mvcc_gc_interval_ms: Option<i64>,
    mvcc_retention_ms: Option<i64>,
    mvcc_max_chain_depth: Option<i64>,
    page_size: Option<i64>,
    wal_size: Option<i64>,
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
    group_commit_enabled: Option<bool>,
    group_commit_window_ms: Option<i64>,
    snapshot_parse_mode: Option<SnapshotParseMode>,
    replication_role: Option<String>,
    replication_sidecar_path: Option<String>,
    replication_source_db_path: Option<String>,
    replication_source_sidecar_path: Option<String>,
    replication_segment_max_bytes: Option<i64>,
    replication_retention_min_entries: Option<i64>,
    replication_retention_min_ms: Option<i64>,
    danger_bypass_file_lock_for_multi_node_simulation: Option<bool>,
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
      group_commit_enabled,
      group_commit_window_ms,
      snapshot_parse_mode,
      replication_role,
      replication_sidecar_path,
      replication_source_db_path,
      replication_source_sidecar_path,
      replication_segment_max_bytes,
      replication_retention_min_entries,
      replication_retention_min_ms,
      danger_bypass_file_lock_for_multi_node_simulation,
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
    let page_size = self
      .page_size
      .map(validation::page_size)
      .transpose()?
      .unwrap_or(validation::MIN_PAGE_SIZE as usize);
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
      rust_opts = rust_opts.mvcc_gc_interval_ms(validation::positive_u64(
        "mvcc_gc_interval_ms",
        v,
        validation::MAX_DURATION_MS as u64,
      )?);
    }
    if let Some(v) = self.mvcc_retention_ms {
      rust_opts = rust_opts.mvcc_retention_ms(validation::non_negative_u64(
        "mvcc_retention_ms",
        v,
        validation::MAX_DURATION_MS as u64,
      )?);
    }
    if let Some(v) = self.mvcc_max_chain_depth {
      rust_opts = rust_opts.mvcc_max_chain_depth(validation::positive_usize(
        "mvcc_max_chain_depth",
        v,
        validation::MAX_DEPTH,
      )?);
    }
    if self.page_size.is_some() {
      rust_opts = rust_opts.page_size(page_size);
    }
    if let Some(v) = self.wal_size {
      rust_opts = rust_opts.wal_size(validation::wal_size(v, page_size)?);
    }
    if let Some(v) = self.auto_checkpoint {
      rust_opts = rust_opts.auto_checkpoint(v);
    }
    if let Some(v) = self.checkpoint_threshold {
      rust_opts = rust_opts.checkpoint_threshold(validation::ratio("checkpoint_threshold", v)?);
    }
    if let Some(v) = self.background_checkpoint {
      rust_opts = rust_opts.background_checkpoint(v);
    }
    if let Some(ref compression) = self.checkpoint_compression {
      rust_opts = rust_opts.checkpoint_compression(Some(compression.to_core()?));
    }

    let max_node_props = self
      .cache_max_node_props
      .map(|value| {
        validation::non_negative_usize("cache_max_node_props", value, validation::MAX_CACHE_ENTRIES)
      })
      .transpose()?
      .unwrap_or(10_000);
    let max_edge_props = self
      .cache_max_edge_props
      .map(|value| {
        validation::non_negative_usize("cache_max_edge_props", value, validation::MAX_CACHE_ENTRIES)
      })
      .transpose()?
      .unwrap_or(10_000);
    let max_traversal_entries = self
      .cache_max_traversal_entries
      .map(|value| {
        validation::non_negative_usize(
          "cache_max_traversal_entries",
          value,
          validation::MAX_CACHE_ENTRIES,
        )
      })
      .transpose()?
      .unwrap_or(5_000);
    let max_query_entries = self
      .cache_max_query_entries
      .map(|value| {
        validation::non_negative_usize(
          "cache_max_query_entries",
          value,
          validation::MAX_CACHE_ENTRIES,
        )
      })
      .transpose()?
      .unwrap_or(1_000);
    let query_ttl_ms = self
      .cache_query_ttl_ms
      .map(|value| {
        validation::non_negative_u64(
          "cache_query_ttl_ms",
          value,
          validation::MAX_DURATION_MS as u64,
        )
      })
      .transpose()?;

    // A zero cache capacity disables that cache. A zero TTL keeps the cache
    // enabled but makes entries immediately stale, matching core semantics.
    if self.cache_enabled == Some(true) {
      let property_cache = Some(PropertyCacheConfig {
        max_node_props,
        max_edge_props,
      });

      let traversal_cache = Some(TraversalCacheConfig {
        max_entries: max_traversal_entries,
        max_neighbors_per_entry: 100,
      });

      let query_cache = Some(QueryCacheConfig {
        max_entries: max_query_entries,
        ttl_ms: query_ttl_ms,
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
    if let Some(enabled) = self.group_commit_enabled {
      rust_opts = rust_opts.group_commit_enabled(enabled);
    }
    if let Some(window_ms) = self.group_commit_window_ms {
      rust_opts = rust_opts.group_commit_window_ms(validation::non_negative_u64(
        "group_commit_window_ms",
        window_ms,
        validation::MAX_DURATION_MS as u64,
      )?);
    }
    if let Some(mode) = self.snapshot_parse_mode {
      rust_opts = rust_opts.snapshot_parse_mode(mode.mode);
    }
    if let Some(ref role) = self.replication_role {
      let role = ReplicationRole::from_str(role).map_err(|error| {
        PyValueError::new_err(format!("Invalid replication_role '{role}': {error}"))
      })?;
      rust_opts = rust_opts.replication_role(role);
    }
    if self.danger_bypass_file_lock_for_multi_node_simulation == Some(true) {
      if std::env::var_os("KITEDB_DANGER_ALLOW_MULTI_NODE_SIMULATION").is_none() {
        return Err(PyValueError::new_err(
          "danger_bypass_file_lock_for_multi_node_simulation is test-only and requires the \
           KITEDB_DANGER_ALLOW_MULTI_NODE_SIMULATION environment variable",
        ));
      }
      rust_opts = rust_opts.danger_bypass_file_lock_for_multi_node_simulation(true);
    }
    if let Some(ref path) = self.replication_sidecar_path {
      rust_opts = rust_opts.replication_sidecar_path(path);
    }
    if let Some(ref path) = self.replication_source_db_path {
      rust_opts = rust_opts.replication_source_db_path(path);
    }
    if let Some(ref path) = self.replication_source_sidecar_path {
      rust_opts = rust_opts.replication_source_sidecar_path(path);
    }
    if let Some(value) = self.replication_segment_max_bytes {
      rust_opts = rust_opts.replication_segment_max_bytes(validation::positive_u64(
        "replication_segment_max_bytes",
        value,
        validation::MAX_BYTES as u64,
      )?);
    }
    if let Some(value) = self.replication_retention_min_entries {
      rust_opts = rust_opts.replication_retention_min_entries(validation::non_negative_u64(
        "replication_retention_min_entries",
        value,
        validation::MAX_COUNT as u64,
      )?);
    }
    if let Some(value) = self.replication_retention_min_ms {
      rust_opts = rust_opts.replication_retention_min_ms(validation::non_negative_u64(
        "replication_retention_min_ms",
        value,
        validation::MAX_DURATION_MS as u64,
      )?);
    }

    Ok(rust_opts)
  }

  /// Build binding open options from high-level Kite profile options.
  pub fn from_kite_options(opts: RustKiteOptions) -> Self {
    let replication_role = match opts.replication_role {
      ReplicationRole::Disabled => "disabled",
      ReplicationRole::Primary => "primary",
      ReplicationRole::Replica => "replica",
    }
    .to_string();

    Self {
      read_only: Some(opts.read_only),
      create_if_missing: Some(opts.create_if_missing),
      mvcc: Some(opts.mvcc),
      mvcc_gc_interval_ms: opts.mvcc_gc_interval_ms.and_then(|v| i64::try_from(v).ok()),
      mvcc_retention_ms: opts.mvcc_retention_ms.and_then(|v| i64::try_from(v).ok()),
      mvcc_max_chain_depth: opts
        .mvcc_max_chain_depth
        .and_then(|v| i64::try_from(v).ok()),
      page_size: None,
      wal_size: opts.wal_size.and_then(|v| i64::try_from(v).ok()),
      auto_checkpoint: None,
      checkpoint_threshold: opts.checkpoint_threshold,
      background_checkpoint: None,
      checkpoint_compression: None,
      cache_snapshot: None,
      cache_enabled: None,
      cache_max_node_props: None,
      cache_max_edge_props: None,
      cache_max_traversal_entries: None,
      cache_max_query_entries: None,
      cache_query_ttl_ms: None,
      sync_mode: Some(SyncMode {
        mode: opts.sync_mode,
      }),
      group_commit_enabled: Some(opts.group_commit_enabled),
      group_commit_window_ms: i64::try_from(opts.group_commit_window_ms).ok(),
      snapshot_parse_mode: None,
      replication_role: Some(replication_role),
      replication_sidecar_path: opts
        .replication_sidecar_path
        .map(|p| p.to_string_lossy().to_string()),
      replication_source_db_path: opts
        .replication_source_db_path
        .map(|p| p.to_string_lossy().to_string()),
      replication_source_sidecar_path: opts
        .replication_source_sidecar_path
        .map(|p| p.to_string_lossy().to_string()),
      replication_segment_max_bytes: opts
        .replication_segment_max_bytes
        .and_then(|v| i64::try_from(v).ok()),
      replication_retention_min_entries: opts
        .replication_retention_min_entries
        .and_then(|v| i64::try_from(v).ok()),
      replication_retention_min_ms: opts
        .replication_retention_min_ms
        .and_then(|v| i64::try_from(v).ok()),
      danger_bypass_file_lock_for_multi_node_simulation: None,
    }
  }
}

/// Runtime profile preset for open/close behavior.
#[pyclass(name = "RuntimeProfile")]
#[derive(Debug, Clone)]
pub struct RuntimeProfile {
  /// Open-time options for Database(path, options)
  #[pyo3(get, set)]
  pub open_options: OpenOptions,
  /// Optional close-time checkpoint threshold
  #[pyo3(get, set)]
  pub close_checkpoint_if_wal_usage_at_least: Option<f64>,
}

#[pymethods]
impl RuntimeProfile {
  fn __repr__(&self) -> String {
    format!(
      "RuntimeProfile(close_checkpoint_if_wal_usage_at_least={:?})",
      self.close_checkpoint_if_wal_usage_at_least
    )
  }
}

impl RuntimeProfile {
  pub fn from_kite_runtime_profile(profile: RustKiteRuntimeProfile) -> Self {
    Self {
      open_options: OpenOptions::from_kite_options(profile.options),
      close_checkpoint_if_wal_usage_at_least: profile.close_checkpoint_if_wal_usage_at_least,
    }
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
      group_commit_enabled: Some(true),
      group_commit_window_ms: Some(5),
      ..Default::default()
    };
    let rust_opts: RustOpenOptions = opts.try_into().expect("expected value");
    assert!(rust_opts.read_only);
    assert!(!rust_opts.create_if_missing);
    assert!(rust_opts.group_commit_enabled);
    assert_eq!(rust_opts.group_commit_window_ms, 5);
  }

  #[test]
  fn test_open_numeric_validation_and_zero_semantics() {
    let valid = OpenOptions {
      page_size: Some(8192),
      wal_size: Some(8192 * 16),
      mvcc_gc_interval_ms: Some(1),
      mvcc_retention_ms: Some(0),
      mvcc_max_chain_depth: Some(1),
      cache_enabled: Some(true),
      cache_max_node_props: Some(0),
      cache_max_edge_props: Some(1),
      cache_max_traversal_entries: Some(0),
      cache_max_query_entries: Some(0),
      cache_query_ttl_ms: Some(0),
      checkpoint_threshold: Some(0.0),
      group_commit_window_ms: Some(0),
      replication_segment_max_bytes: Some(1),
      replication_retention_min_entries: Some(0),
      replication_retention_min_ms: Some(0),
      ..Default::default()
    };
    assert!(valid.to_single_file_options().is_ok());

    for options in [
      OpenOptions {
        page_size: Some(0),
        ..Default::default()
      },
      OpenOptions {
        page_size: Some(-1),
        ..Default::default()
      },
      OpenOptions {
        page_size: Some(1_000_000),
        ..Default::default()
      },
      OpenOptions {
        wal_size: Some(0),
        ..Default::default()
      },
      OpenOptions {
        wal_size: Some(-1),
        ..Default::default()
      },
      OpenOptions {
        wal_size: Some(validation::MAX_BYTES + 1),
        ..Default::default()
      },
      OpenOptions {
        mvcc_gc_interval_ms: Some(0),
        ..Default::default()
      },
      OpenOptions {
        mvcc_gc_interval_ms: Some(-1),
        ..Default::default()
      },
      OpenOptions {
        mvcc_retention_ms: Some(-1),
        ..Default::default()
      },
      OpenOptions {
        mvcc_max_chain_depth: Some(0),
        ..Default::default()
      },
      OpenOptions {
        cache_max_node_props: Some(-1),
        ..Default::default()
      },
      OpenOptions {
        cache_max_query_entries: Some(validation::MAX_CACHE_ENTRIES + 1),
        ..Default::default()
      },
      OpenOptions {
        checkpoint_threshold: Some(2.0),
        ..Default::default()
      },
      OpenOptions {
        group_commit_window_ms: Some(-1),
        ..Default::default()
      },
      OpenOptions {
        replication_segment_max_bytes: Some(0),
        ..Default::default()
      },
      OpenOptions {
        replication_retention_min_ms: Some(validation::MAX_DURATION_MS + 1),
        ..Default::default()
      },
    ] {
      assert!(options.to_single_file_options().is_err());
    }
    assert!(OpenOptions {
      cache_enabled: Some(true),
      cache_max_node_props: Some(validation::MAX_CACHE_ENTRIES),
      ..Default::default()
    }
    .to_single_file_options()
    .is_ok());
  }
}
