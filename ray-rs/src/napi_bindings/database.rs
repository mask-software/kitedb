//! NAPI bindings for SingleFileDB
//!
//! Provides Node.js/Bun access to the single-file database format.

use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::path::PathBuf;
use std::str::FromStr;

use super::traversal::{
  JsPathConfig, JsPathResult, JsTraversalDirection, JsTraversalResult, JsTraversalStep,
  JsTraverseOptions,
};
use super::validation;
use crate::api::kite::KiteRuntimeProfile as RustKiteRuntimeProfile;
use crate::api::pathfinding::{bfs, dijkstra, yen_k_shortest};
use crate::api::traversal::{TraversalBuilder as RustTraversalBuilder, TraversalDirection};
use crate::backup as core_backup;
use crate::core::single_file::{
  close_single_file, close_single_file_with_options, is_single_file_path, open_single_file,
  single_file_extension, ResizeWalOptions as RustResizeWalOptions,
  SingleFileCloseOptions as RustSingleFileCloseOptions, SingleFileDB as RustSingleFileDB,
  SingleFileOpenOptions as RustOpenOptions,
  SingleFileOptimizeOptions as RustSingleFileOptimizeOptions,
  SnapshotParseMode as RustSnapshotParseMode, SyncMode as RustSyncMode,
  VacuumOptions as RustVacuumOptions,
};
use crate::export as ray_export;
use crate::metrics as core_metrics;
use crate::replication::primary::{
  PrimaryReplicationStatus, PrimaryRetentionOutcome, ReplicaLagStatus,
};
use crate::replication::replica::ReplicaReplicationStatus;
use crate::replication::types::{CommitToken, ReplicationRole as RustReplicationRole};
use crate::streaming;
use crate::types::{
  CheckResult as RustCheckResult, ETypeId, Edge, EdgeWithProps as CoreEdgeWithProps, NodeId,
  PropKeyId, PropValue,
};
use crate::util::compression::{CompressionOptions as CoreCompressionOptions, CompressionType};
use serde_json;

// ============================================================================
// Sync Mode
// ============================================================================

/// Synchronization mode for WAL writes
///
/// Controls the durability vs performance trade-off for commits.
/// - Full: Fsync on every commit (durable to OS, slowest)
/// - Normal: Fsync only on checkpoint (~1000x faster, safe from app crash)
/// - Off: No fsync (fastest, data may be lost on any crash)
#[napi(string_enum)]
#[derive(Debug)]
pub enum JsSyncMode {
  /// Fsync on every commit (durable to OS, slowest)
  Full,
  /// Fsync on checkpoint only (balanced)
  Normal,
  /// No fsync (fastest, least safe)
  Off,
}

impl From<JsSyncMode> for RustSyncMode {
  fn from(mode: JsSyncMode) -> Self {
    match mode {
      JsSyncMode::Full => RustSyncMode::Full,
      JsSyncMode::Normal => RustSyncMode::Normal,
      JsSyncMode::Off => RustSyncMode::Off,
    }
  }
}

impl From<&JsSyncMode> for RustSyncMode {
  fn from(mode: &JsSyncMode) -> Self {
    match mode {
      JsSyncMode::Full => RustSyncMode::Full,
      JsSyncMode::Normal => RustSyncMode::Normal,
      JsSyncMode::Off => RustSyncMode::Off,
    }
  }
}

/// Snapshot parse behavior for single-file databases
#[napi(string_enum)]
#[derive(Debug)]
pub enum JsSnapshotParseMode {
  /// Treat snapshot parse errors as fatal
  Strict,
  /// Ignore snapshot parse errors and recover from WAL only
  Salvage,
}

impl From<JsSnapshotParseMode> for RustSnapshotParseMode {
  fn from(mode: JsSnapshotParseMode) -> Self {
    match mode {
      JsSnapshotParseMode::Strict => RustSnapshotParseMode::Strict,
      JsSnapshotParseMode::Salvage => RustSnapshotParseMode::Salvage,
    }
  }
}

/// Replication role for single-file open options
#[napi(string_enum)]
#[derive(Debug)]
pub enum JsReplicationRole {
  Disabled,
  Primary,
  Replica,
}

impl From<JsReplicationRole> for RustReplicationRole {
  fn from(role: JsReplicationRole) -> Self {
    match role {
      JsReplicationRole::Disabled => RustReplicationRole::Disabled,
      JsReplicationRole::Primary => RustReplicationRole::Primary,
      JsReplicationRole::Replica => RustReplicationRole::Replica,
    }
  }
}

impl From<&JsReplicationRole> for RustReplicationRole {
  fn from(role: &JsReplicationRole) -> Self {
    match role {
      JsReplicationRole::Disabled => RustReplicationRole::Disabled,
      JsReplicationRole::Primary => RustReplicationRole::Primary,
      JsReplicationRole::Replica => RustReplicationRole::Replica,
    }
  }
}

// ============================================================================
// Open Options
// ============================================================================

/// Options for opening a database
#[napi(object)]
#[derive(Debug, Default)]
pub struct OpenOptions {
  /// Open in read-only mode
  pub read_only: Option<bool>,
  /// Create database if it doesn't exist
  pub create_if_missing: Option<bool>,
  /// Enable MVCC (snapshot isolation + conflict detection)
  pub mvcc: Option<bool>,
  /// MVCC GC interval in ms
  pub mvcc_gc_interval_ms: Option<i64>,
  /// MVCC retention in ms (0 means retain no historical window)
  pub mvcc_retention_ms: Option<i64>,
  /// MVCC max version chain depth (must be positive)
  pub mvcc_max_chain_depth: Option<i64>,
  /// Page size in bytes (must be a supported positive power of two)
  pub page_size: Option<u32>,
  /// WAL size in bytes (at least 16 pages), fixed when the file is created.
  /// Unset: a new file gets a 4MB WAL and an existing file keeps its own.
  /// Set: a new file gets this size; an existing file with a different WAL
  /// size fails to open.
  pub wal_size: Option<u32>,
  /// Enable auto-checkpoint when WAL usage exceeds threshold
  pub auto_checkpoint: Option<bool>,
  /// WAL usage threshold (0.0-1.0) to trigger auto-checkpoint
  pub checkpoint_threshold: Option<f64>,
  /// Use background (non-blocking) checkpoint
  pub background_checkpoint: Option<bool>,
  /// Compression options for checkpoint snapshots (single-file only)
  pub checkpoint_compression: Option<CompressionOptions>,
  /// Enable caching
  pub cache_enabled: Option<bool>,
  /// Max node properties in cache (0 disables the node-property cache)
  pub cache_max_node_props: Option<i64>,
  /// Max edge properties in cache (0 disables the edge-property cache)
  pub cache_max_edge_props: Option<i64>,
  /// Max traversal cache entries (0 disables the traversal cache)
  pub cache_max_traversal_entries: Option<i64>,
  /// Max query cache entries (0 disables the query cache)
  pub cache_max_query_entries: Option<i64>,
  /// Query cache TTL in milliseconds (0 expires entries immediately)
  pub cache_query_ttl_ms: Option<i64>,
  /// Sync mode: "Full", "Normal", or "Off" (default: "Full")
  pub sync_mode: Option<JsSyncMode>,
  /// Enable group commit (coalesce WAL flushes across commits)
  pub group_commit_enabled: Option<bool>,
  /// Group commit window in milliseconds (0 adds no coalescing delay)
  pub group_commit_window_ms: Option<i64>,
  /// Snapshot parse mode: "Strict" or "Salvage" (single-file only)
  pub snapshot_parse_mode: Option<JsSnapshotParseMode>,
  /// Replication role: "Disabled", "Primary", or "Replica"
  pub replication_role: Option<JsReplicationRole>,
  /// Replication sidecar path override
  pub replication_sidecar_path: Option<String>,
  /// Source primary db path (replica role only)
  pub replication_source_db_path: Option<String>,
  /// Source primary sidecar path (replica role only)
  pub replication_source_sidecar_path: Option<String>,
  /// Segment rotation threshold in bytes (primary role only)
  pub replication_segment_max_bytes: Option<i64>,
  /// Minimum retained entries window (0 imposes no entry-count floor)
  pub replication_retention_min_entries: Option<i64>,
  /// Minimum retained segment age in milliseconds (0 imposes no age floor)
  pub replication_retention_min_ms: Option<i64>,
  /// TEST-ONLY: skip database file locking to simulate multi-node topologies
  /// (e.g. split-brain fencing tests) in a single process. Honored only when
  /// the KITEDB_DANGER_ALLOW_MULTI_NODE_SIMULATION environment variable is
  /// set; rejected otherwise. Never use this outside tests: it removes the
  /// corruption protection that prevents two writers on one database file.
  pub danger_bypass_file_lock_for_multi_node_simulation: Option<bool>,
}

impl OpenOptions {
  fn into_rust(self) -> Result<RustOpenOptions> {
    use crate::types::{CacheOptions, PropertyCacheConfig, QueryCacheConfig, TraversalCacheConfig};

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
        "mvccGcIntervalMs",
        v,
        validation::MAX_DURATION_MS as u64,
      )?);
    }
    if let Some(v) = self.mvcc_retention_ms {
      rust_opts = rust_opts.mvcc_retention_ms(validation::non_negative_u64(
        "mvccRetentionMs",
        v,
        validation::MAX_DURATION_MS as u64,
      )?);
    }
    if let Some(v) = self.mvcc_max_chain_depth {
      rust_opts = rust_opts.mvcc_max_chain_depth(validation::positive_usize(
        "mvccMaxChainDepth",
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
      rust_opts = rust_opts.checkpoint_threshold(validation::ratio("checkpointThreshold", v)?);
    }
    if let Some(v) = self.background_checkpoint {
      rust_opts = rust_opts.background_checkpoint(v);
    }
    if let Some(compression) = self.checkpoint_compression {
      rust_opts = rust_opts.checkpoint_compression(Some(compression.into_rust()?));
    }

    let max_node_props = self
      .cache_max_node_props
      .map(|v| {
        validation::non_negative_usize("cacheMaxNodeProps", v, validation::MAX_CACHE_ENTRIES)
      })
      .transpose()?
      .unwrap_or(10_000);
    let max_edge_props = self
      .cache_max_edge_props
      .map(|v| {
        validation::non_negative_usize("cacheMaxEdgeProps", v, validation::MAX_CACHE_ENTRIES)
      })
      .transpose()?
      .unwrap_or(10_000);
    let max_traversal_entries = self
      .cache_max_traversal_entries
      .map(|v| {
        validation::non_negative_usize("cacheMaxTraversalEntries", v, validation::MAX_CACHE_ENTRIES)
      })
      .transpose()?
      .unwrap_or(5_000);
    let max_query_entries = self
      .cache_max_query_entries
      .map(|v| {
        validation::non_negative_usize("cacheMaxQueryEntries", v, validation::MAX_CACHE_ENTRIES)
      })
      .transpose()?
      .unwrap_or(1_000);
    let query_ttl_ms = self
      .cache_query_ttl_ms
      .map(|v| {
        validation::non_negative_u64("cacheQueryTtlMs", v, validation::MAX_DURATION_MS as u64)
      })
      .transpose()?;

    // Cache options
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
    if let Some(mode) = self.sync_mode {
      rust_opts = rust_opts.sync_mode(mode.into());
    }
    if let Some(enabled) = self.group_commit_enabled {
      rust_opts = rust_opts.group_commit_enabled(enabled);
    }
    if let Some(window_ms) = self.group_commit_window_ms {
      rust_opts = rust_opts.group_commit_window_ms(validation::non_negative_u64(
        "groupCommitWindowMs",
        window_ms,
        validation::MAX_DURATION_MS as u64,
      )?);
    }

    // Snapshot parse mode
    if let Some(mode) = self.snapshot_parse_mode {
      rust_opts = rust_opts.snapshot_parse_mode(mode.into());
    }
    if let Some(role) = self.replication_role {
      rust_opts = rust_opts.replication_role(role.into());
    }
    if self.danger_bypass_file_lock_for_multi_node_simulation == Some(true) {
      if std::env::var_os("KITEDB_DANGER_ALLOW_MULTI_NODE_SIMULATION").is_none() {
        return Err(Error::new(
          Status::InvalidArg,
          "dangerBypassFileLockForMultiNodeSimulation is test-only and requires the \
           KITEDB_DANGER_ALLOW_MULTI_NODE_SIMULATION environment variable"
            .to_string(),
        ));
      }
      rust_opts = rust_opts.danger_bypass_file_lock_for_multi_node_simulation(true);
    }
    if let Some(path) = self.replication_sidecar_path {
      rust_opts = rust_opts.replication_sidecar_path(path);
    }
    if let Some(path) = self.replication_source_db_path {
      rust_opts = rust_opts.replication_source_db_path(path);
    }
    if let Some(path) = self.replication_source_sidecar_path {
      rust_opts = rust_opts.replication_source_sidecar_path(path);
    }
    if let Some(value) = self.replication_segment_max_bytes {
      rust_opts = rust_opts.replication_segment_max_bytes(validation::positive_u64(
        "replicationSegmentMaxBytes",
        value,
        validation::MAX_BYTES as u64,
      )?);
    }
    if let Some(value) = self.replication_retention_min_entries {
      rust_opts = rust_opts.replication_retention_min_entries(validation::non_negative_u64(
        "replicationRetentionMinEntries",
        value,
        validation::MAX_COUNT as u64,
      )?);
    }
    if let Some(value) = self.replication_retention_min_ms {
      rust_opts = rust_opts.replication_retention_min_ms(validation::non_negative_u64(
        "replicationRetentionMinMs",
        value,
        validation::MAX_DURATION_MS as u64,
      )?);
    }

    Ok(rust_opts)
  }
}

#[cfg(test)]
mod open_option_validation_tests {
  use super::*;

  #[test]
  fn validates_open_numeric_ranges_and_zero_semantics() {
    assert!(OpenOptions {
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
    }
    .into_rust()
    .is_ok());

    for options in [
      OpenOptions {
        page_size: Some(0),
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
      assert!(options.into_rust().is_err());
    }

    assert!(OpenOptions {
      cache_enabled: Some(true),
      cache_max_node_props: Some(validation::MAX_CACHE_ENTRIES),
      ..Default::default()
    }
    .into_rust()
    .is_ok());
  }

  #[test]
  fn validates_maintenance_and_streaming_options() {
    assert!(CompressionOptions {
      r#type: Some(JsCompressionType::Zstd),
      min_size: Some(1),
      level: Some(1),
      ..Default::default()
    }
    .into_rust()
    .is_ok());
    assert!(CompressionOptions {
      r#type: Some(JsCompressionType::Zstd),
      level: Some(0),
      ..Default::default()
    }
    .into_rust()
    .is_err());
    assert!(ImportOptions {
      batch_size: Some(0),
      skip_existing: None,
    }
    .into_rust()
    .map(|opts| opts.batch_size == 1000)
    .unwrap_or(false));
    assert!(ImportOptions {
      batch_size: Some(-1),
      skip_existing: None,
    }
    .into_rust()
    .is_err());
    assert!(StreamOptions {
      batch_size: Some(0),
    }
    .into_rust()
    .map(|opts| opts.batch_size == 0)
    .unwrap_or(false));
    assert!(PaginationOptions {
      limit: Some(-1),
      cursor: None,
    }
    .into_rust()
    .is_err());
  }

  #[test]
  fn validates_otlp_numeric_options_and_zero_semantics() {
    assert!(build_core_otel_push_options(PushReplicationMetricsOtelOptions::default()).is_ok());

    let zero = PushReplicationMetricsOtelOptions {
      retry_backoff_ms: Some(0),
      retry_backoff_max_ms: Some(0),
      circuit_breaker_failure_threshold: Some(0),
      circuit_breaker_open_ms: Some(0),
      circuit_breaker_half_open_probes: Some(0),
      ..Default::default()
    };
    assert!(build_core_otel_push_options(zero).is_ok());

    let invalid_cases = [
      PushReplicationMetricsOtelOptions {
        timeout_ms: Some(0),
        ..Default::default()
      },
      PushReplicationMetricsOtelOptions {
        retry_max_attempts: Some(0),
        ..Default::default()
      },
      PushReplicationMetricsOtelOptions {
        retry_backoff_ms: Some(-1),
        ..Default::default()
      },
      PushReplicationMetricsOtelOptions {
        retry_jitter_ratio: Some(2.0),
        ..Default::default()
      },
      PushReplicationMetricsOtelOptions {
        circuit_breaker_failure_threshold: Some(-1),
        ..Default::default()
      },
      PushReplicationMetricsOtelOptions {
        circuit_breaker_state_patch_batch_max_keys: Some(0),
        ..Default::default()
      },
      PushReplicationMetricsOtelOptions {
        circuit_breaker_state_patch_retry_max_attempts: Some(validation::MAX_COUNT + 1),
        ..Default::default()
      },
    ];
    for invalid in invalid_cases {
      assert!(build_core_otel_push_options(invalid).is_err());
    }
  }

  #[test]
  fn unset_wal_size_reopens_a_file_with_its_own_wal_size() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("napi-wal-size.kitedb");
    let create = OpenOptions {
      wal_size: Some(64 * 1024),
      ..Default::default()
    }
    .into_rust()
    .expect("create options");
    assert_eq!(create.wal_size, Some(64 * 1024));
    close_single_file(open_single_file(&path, create).expect("create")).expect("close");

    let reopen = OpenOptions::default().into_rust().expect("default options");
    assert_eq!(
      reopen.wal_size, None,
      "an unset walSize must not become a default"
    );
    let db = open_single_file(&path, reopen).expect("reopen without walSize");
    close_single_file(db).expect("close reopened");
  }
}

fn js_sync_mode_from_rust(mode: RustSyncMode) -> JsSyncMode {
  match mode {
    RustSyncMode::Full => JsSyncMode::Full,
    RustSyncMode::Normal => JsSyncMode::Normal,
    RustSyncMode::Off => JsSyncMode::Off,
  }
}

fn js_replication_role_from_rust(role: RustReplicationRole) -> JsReplicationRole {
  match role {
    RustReplicationRole::Disabled => JsReplicationRole::Disabled,
    RustReplicationRole::Primary => JsReplicationRole::Primary,
    RustReplicationRole::Replica => JsReplicationRole::Replica,
  }
}

fn open_options_from_kite_profile_options(opts: crate::api::kite::KiteOptions) -> OpenOptions {
  OpenOptions {
    read_only: Some(opts.read_only),
    create_if_missing: Some(opts.create_if_missing),
    mvcc: Some(opts.mvcc),
    mvcc_gc_interval_ms: opts.mvcc_gc_interval_ms.and_then(|v| i64::try_from(v).ok()),
    mvcc_retention_ms: opts.mvcc_retention_ms.and_then(|v| i64::try_from(v).ok()),
    mvcc_max_chain_depth: opts
      .mvcc_max_chain_depth
      .and_then(|v| i64::try_from(v).ok()),
    page_size: None,
    wal_size: opts.wal_size.and_then(|v| u32::try_from(v).ok()),
    auto_checkpoint: None,
    checkpoint_threshold: opts.checkpoint_threshold,
    background_checkpoint: None,
    checkpoint_compression: None,
    cache_enabled: None,
    cache_max_node_props: None,
    cache_max_edge_props: None,
    cache_max_traversal_entries: None,
    cache_max_query_entries: None,
    cache_query_ttl_ms: None,
    sync_mode: Some(js_sync_mode_from_rust(opts.sync_mode)),
    group_commit_enabled: Some(opts.group_commit_enabled),
    group_commit_window_ms: i64::try_from(opts.group_commit_window_ms).ok(),
    snapshot_parse_mode: None,
    replication_role: Some(js_replication_role_from_rust(opts.replication_role)),
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

/// Runtime profile preset for open/close behavior.
#[napi(object)]
#[derive(Debug, Default)]
pub struct RuntimeProfile {
  /// Open-time options for `Database.open(path, options)`.
  pub open_options: OpenOptions,
  /// Optional close-time checkpoint trigger threshold.
  pub close_checkpoint_if_wal_usage_at_least: Option<f64>,
}

fn runtime_profile_from_rust(profile: RustKiteRuntimeProfile) -> RuntimeProfile {
  RuntimeProfile {
    open_options: open_options_from_kite_profile_options(profile.options),
    close_checkpoint_if_wal_usage_at_least: profile.close_checkpoint_if_wal_usage_at_least,
  }
}

// ============================================================================
// Single-File Maintenance Options
// ============================================================================

/// Options for vacuuming a single-file database
#[napi(object)]
#[derive(Debug, Default)]
pub struct VacuumOptions {
  /// Shrink WAL region if empty
  pub shrink_wal: Option<bool>,
  /// Minimum WAL size to keep (bytes)
  pub min_wal_size: Option<i64>,
}

impl VacuumOptions {
  fn into_rust(self) -> Result<RustVacuumOptions> {
    let min_wal_size = self
      .min_wal_size
      .map(|v| validation::non_negative_u64("minWalSize", v, validation::MAX_BYTES as u64))
      .transpose()?;
    Ok(RustVacuumOptions {
      shrink_wal: self.shrink_wal.unwrap_or(true),
      min_wal_size,
    })
  }
}

/// Options for resizing WAL
#[napi(object)]
#[derive(Debug, Default)]
pub struct ResizeWalOptions {
  /// Allow shrinking WAL size (default false)
  pub allow_shrink: Option<bool>,
  /// Perform checkpoint before resizing (default true)
  pub checkpoint: Option<bool>,
}

impl From<ResizeWalOptions> for RustResizeWalOptions {
  fn from(opts: ResizeWalOptions) -> Self {
    Self {
      allow_shrink: opts.allow_shrink.unwrap_or(false),
      checkpoint: opts.checkpoint.unwrap_or(true),
    }
  }
}

/// Compression type for snapshot building
#[napi(string_enum)]
#[derive(Debug)]
pub enum JsCompressionType {
  None,
  Zstd,
  Gzip,
  Deflate,
}

impl From<JsCompressionType> for CompressionType {
  fn from(value: JsCompressionType) -> Self {
    match value {
      JsCompressionType::None => CompressionType::None,
      JsCompressionType::Zstd => CompressionType::Zstd,
      JsCompressionType::Gzip => CompressionType::Gzip,
      JsCompressionType::Deflate => CompressionType::Deflate,
    }
  }
}

/// Compression options
#[napi(object)]
#[derive(Debug, Default)]
pub struct CompressionOptions {
  /// Enable compression (default false)
  pub enabled: Option<bool>,
  /// Compression algorithm
  pub r#type: Option<JsCompressionType>,
  /// Minimum section size to compress
  pub min_size: Option<u32>,
  /// Compression level
  pub level: Option<i32>,
}

impl CompressionOptions {
  fn into_rust(self) -> Result<CoreCompressionOptions> {
    let mut out = CoreCompressionOptions::default();
    if let Some(enabled) = self.enabled {
      out.enabled = enabled;
    }
    if let Some(t) = self.r#type {
      out.compression_type = t.into();
    }
    if let Some(min_size) = self.min_size {
      out.min_size = validation::compression_min_size(min_size)?;
    }
    if let Some(level) = self.level {
      let zstd = matches!(out.compression_type, CompressionType::Zstd);
      out.level = validation::compression_level("level", level, zstd)?;
    }
    Ok(out)
  }
}

/// Options for optimizing a single-file database
#[napi(object)]
#[derive(Debug, Default)]
pub struct SingleFileOptimizeOptions {
  /// Compression options for the new snapshot
  pub compression: Option<CompressionOptions>,
}

impl SingleFileOptimizeOptions {
  fn into_rust(self) -> Result<RustSingleFileOptimizeOptions> {
    Ok(RustSingleFileOptimizeOptions {
      compression: self
        .compression
        .map(CompressionOptions::into_rust)
        .transpose()?,
    })
  }
}

// ============================================================================
// Database Statistics
// ============================================================================

/// Database statistics
#[napi(object)]
pub struct DbStats {
  pub snapshot_gen: i64,
  pub snapshot_nodes: i64,
  pub snapshot_edges: i64,
  pub snapshot_max_node_id: i64,
  pub delta_nodes_created: i64,
  pub delta_nodes_deleted: i64,
  pub delta_edges_added: i64,
  pub delta_edges_deleted: i64,
  pub wal_segment: i64,
  pub wal_bytes: i64,
  pub recommend_compact: bool,
  pub mvcc_stats: Option<MvccStats>,
}

/// MVCC stats (from stats())
#[napi(object)]
pub struct MvccStats {
  pub active_transactions: i64,
  pub min_active_ts: i64,
  pub versions_pruned: i64,
  pub gc_runs: i64,
  pub last_gc_time: i64,
  pub committed_writes_size: i64,
  pub committed_writes_pruned: i64,
}

/// Per-replica lag entry on primary status
#[napi(object)]
pub struct JsReplicaLagStatus {
  pub replica_id: String,
  pub epoch: i64,
  pub applied_log_index: i64,
}

/// Primary replication runtime status
#[napi(object)]
pub struct JsPrimaryReplicationStatus {
  pub role: String,
  pub epoch: i64,
  pub head_log_index: i64,
  pub retained_floor: i64,
  pub replica_lags: Vec<JsReplicaLagStatus>,
  pub sidecar_path: String,
  pub last_token: Option<String>,
  pub last_replication_error: Option<String>,
  pub sidecar_needs_repair: bool,
  pub append_attempts: i64,
  pub append_failures: i64,
  pub append_successes: i64,
}

/// Replica replication runtime status
#[napi(object)]
pub struct JsReplicaReplicationStatus {
  pub role: String,
  pub source_db_path: Option<String>,
  pub source_sidecar_path: Option<String>,
  pub applied_epoch: i64,
  pub applied_log_index: i64,
  pub last_error: Option<String>,
  pub needs_reseed: bool,
}

/// Retention run outcome
#[napi(object)]
pub struct JsPrimaryRetentionOutcome {
  pub pruned_segments: i64,
  pub retained_floor: i64,
}

impl From<ReplicaLagStatus> for JsReplicaLagStatus {
  fn from(value: ReplicaLagStatus) -> Self {
    Self {
      replica_id: value.replica_id,
      epoch: value.epoch as i64,
      applied_log_index: value.applied_log_index as i64,
    }
  }
}

impl From<PrimaryReplicationStatus> for JsPrimaryReplicationStatus {
  fn from(value: PrimaryReplicationStatus) -> Self {
    Self {
      role: value.role.to_string(),
      epoch: value.epoch as i64,
      head_log_index: value.head_log_index as i64,
      retained_floor: value.retained_floor as i64,
      replica_lags: value.replica_lags.into_iter().map(Into::into).collect(),
      sidecar_path: value.sidecar_path.to_string_lossy().to_string(),
      last_token: value.last_token.map(|token| token.to_string()),
      last_replication_error: value.last_replication_error,
      sidecar_needs_repair: value.sidecar_needs_repair,
      append_attempts: value.append_attempts as i64,
      append_failures: value.append_failures as i64,
      append_successes: value.append_successes as i64,
    }
  }
}

impl From<ReplicaReplicationStatus> for JsReplicaReplicationStatus {
  fn from(value: ReplicaReplicationStatus) -> Self {
    Self {
      role: value.role.to_string(),
      source_db_path: value
        .source_db_path
        .map(|path| path.to_string_lossy().to_string()),
      source_sidecar_path: value
        .source_sidecar_path
        .map(|path| path.to_string_lossy().to_string()),
      applied_epoch: value.applied_epoch as i64,
      applied_log_index: value.applied_log_index as i64,
      last_error: value.last_error,
      needs_reseed: value.needs_reseed,
    }
  }
}

impl From<PrimaryRetentionOutcome> for JsPrimaryRetentionOutcome {
  fn from(value: PrimaryRetentionOutcome) -> Self {
    Self {
      pruned_segments: value.pruned_segments as i64,
      retained_floor: value.retained_floor as i64,
    }
  }
}

/// Options for export
#[napi(object)]
pub struct ExportOptions {
  pub include_nodes: Option<bool>,
  pub include_edges: Option<bool>,
  pub include_schema: Option<bool>,
  pub pretty: Option<bool>,
}

impl ExportOptions {
  fn into_rust(self) -> ray_export::ExportOptions {
    let mut opts = ray_export::ExportOptions::default();
    if let Some(v) = self.include_nodes {
      opts.include_nodes = v;
    }
    if let Some(v) = self.include_edges {
      opts.include_edges = v;
    }
    if let Some(v) = self.include_schema {
      opts.include_schema = v;
    }
    if let Some(v) = self.pretty {
      opts.pretty = v;
    }
    opts
  }
}

/// Options for import
#[napi(object)]
pub struct ImportOptions {
  pub skip_existing: Option<bool>,
  /// Batch size; 0 preserves the core default.
  pub batch_size: Option<i64>,
}

impl ImportOptions {
  fn into_rust(self) -> Result<ray_export::ImportOptions> {
    let mut opts = ray_export::ImportOptions::default();
    if let Some(v) = self.skip_existing {
      opts.skip_existing = v;
    }
    if let Some(v) = self.batch_size {
      let batch_size = validation::non_negative_usize("batchSize", v, validation::MAX_COUNT)?;
      if batch_size > 0 {
        opts.batch_size = batch_size;
      }
    }
    Ok(opts)
  }
}

/// Export result
#[napi(object)]
pub struct ExportResult {
  pub node_count: i64,
  pub edge_count: i64,
}

/// Import result
#[napi(object)]
pub struct ImportResult {
  pub node_count: i64,
  pub edge_count: i64,
  pub skipped: i64,
}

// =============================================================================
// Streaming / Pagination Options
// =============================================================================

/// Options for streaming node/edge batches
#[napi(object)]
#[derive(Debug, Default)]
pub struct StreamOptions {
  /// Number of items per batch; 0 preserves the core default.
  pub batch_size: Option<i64>,
}

impl StreamOptions {
  fn into_rust(self) -> Result<crate::streaming::StreamOptions> {
    let batch_size = self.batch_size.unwrap_or(0);
    Ok(crate::streaming::StreamOptions {
      batch_size: validation::non_negative_usize("batchSize", batch_size, validation::MAX_COUNT)?,
    })
  }
}

/// Options for cursor-based pagination
#[napi(object)]
#[derive(Debug, Default)]
pub struct PaginationOptions {
  /// Number of items per page; 0 preserves the core default.
  pub limit: Option<i64>,
  /// Cursor from previous page
  pub cursor: Option<String>,
}

impl PaginationOptions {
  fn into_rust(self) -> Result<crate::streaming::PaginationOptions> {
    let limit = self.limit.unwrap_or(0);
    Ok(crate::streaming::PaginationOptions {
      limit: validation::non_negative_usize("limit", limit, validation::MAX_COUNT)?,
      cursor: self.cursor,
    })
  }
}

/// Node entry with properties
#[napi(object)]
pub struct NodeWithProps {
  pub id: i64,
  pub key: Option<String>,
  pub props: Vec<JsNodeProp>,
}

/// Edge entry with properties
#[napi(object)]
pub struct EdgeWithProps {
  pub src: i64,
  pub etype: u32,
  pub dst: i64,
  pub props: Vec<JsNodeProp>,
}

/// Page of node IDs
#[napi(object)]
pub struct NodePage {
  pub items: Vec<i64>,
  pub next_cursor: Option<String>,
  pub has_more: bool,
  pub total: Option<i64>,
}

/// Page of edges
#[napi(object)]
pub struct EdgePage {
  pub items: Vec<JsFullEdge>,
  pub next_cursor: Option<String>,
  pub has_more: bool,
  pub total: Option<i64>,
}

/// Database check result
#[napi(object)]
pub struct CheckResult {
  pub valid: bool,
  pub errors: Vec<String>,
  pub warnings: Vec<String>,
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
#[napi(object)]
pub struct JsCacheStats {
  pub property_cache_hits: i64,
  pub property_cache_misses: i64,
  pub property_cache_size: i64,
  pub traversal_cache_hits: i64,
  pub traversal_cache_misses: i64,
  pub traversal_cache_size: i64,
  pub query_cache_hits: i64,
  pub query_cache_misses: i64,
  pub query_cache_size: i64,
}

/// Cache layer metrics
#[napi(object)]
pub struct CacheLayerMetrics {
  pub hits: i64,
  pub misses: i64,
  pub hit_rate: f64,
  pub size: i64,
  pub max_size: i64,
  pub utilization_percent: f64,
}

/// Cache metrics
#[napi(object)]
pub struct CacheMetrics {
  pub enabled: bool,
  pub property_cache: CacheLayerMetrics,
  pub traversal_cache: CacheLayerMetrics,
  pub query_cache: CacheLayerMetrics,
}

/// Data metrics
#[napi(object)]
pub struct DataMetrics {
  pub node_count: i64,
  pub edge_count: i64,
  pub delta_nodes_created: i64,
  pub delta_nodes_deleted: i64,
  pub delta_edges_added: i64,
  pub delta_edges_deleted: i64,
  pub snapshot_generation: i64,
  pub max_node_id: i64,
  pub schema_labels: i64,
  pub schema_etypes: i64,
  pub schema_prop_keys: i64,
}

/// MVCC metrics
#[napi(object)]
pub struct MvccMetrics {
  pub enabled: bool,
  pub active_transactions: i64,
  pub versions_pruned: i64,
  pub gc_runs: i64,
  pub min_active_timestamp: i64,
  pub committed_writes_size: i64,
  pub committed_writes_pruned: i64,
}

/// Primary replication metrics
#[napi(object)]
pub struct PrimaryReplicationMetrics {
  pub epoch: i64,
  pub head_log_index: i64,
  pub retained_floor: i64,
  pub replica_count: i64,
  pub stale_epoch_replica_count: i64,
  pub max_replica_lag: i64,
  pub min_replica_applied_log_index: Option<i64>,
  pub sidecar_path: String,
  pub last_token: Option<String>,
  pub last_replication_error: Option<String>,
  pub sidecar_needs_repair: bool,
  pub append_attempts: i64,
  pub append_failures: i64,
  pub append_successes: i64,
}

/// Replica replication metrics
#[napi(object)]
pub struct ReplicaReplicationMetrics {
  pub applied_epoch: i64,
  pub applied_log_index: i64,
  pub needs_reseed: bool,
  pub last_error: Option<String>,
}

/// Replication metrics
#[napi(object)]
pub struct ReplicationMetrics {
  pub enabled: bool,
  pub role: String,
  pub primary: Option<PrimaryReplicationMetrics>,
  pub replica: Option<ReplicaReplicationMetrics>,
}

/// Memory metrics
#[napi(object)]
pub struct MemoryMetrics {
  pub delta_estimate_bytes: i64,
  pub cache_estimate_bytes: i64,
  pub snapshot_bytes: i64,
  pub total_estimate_bytes: i64,
}

/// Database metrics
#[napi(object)]
pub struct DatabaseMetrics {
  pub path: String,
  pub is_single_file: bool,
  pub read_only: bool,
  pub data: DataMetrics,
  pub cache: CacheMetrics,
  pub mvcc: Option<MvccMetrics>,
  pub replication: ReplicationMetrics,
  pub memory: MemoryMetrics,
  /// Timestamp in milliseconds since epoch
  pub collected_at: i64,
}

/// Health check entry
#[napi(object)]
pub struct HealthCheckEntry {
  pub name: String,
  pub passed: bool,
  pub message: String,
}

/// Health check result
#[napi(object)]
pub struct HealthCheckResult {
  pub healthy: bool,
  pub checks: Vec<HealthCheckEntry>,
}

/// OTLP HTTP metrics push result.
#[napi(object)]
pub struct OtlpHttpExportResult {
  pub status_code: i64,
  pub response_body: String,
}

/// OTLP collector push options (host runtime).
#[napi(object)]
#[derive(Default, Clone)]
pub struct PushReplicationMetricsOtelOptions {
  pub timeout_ms: Option<i64>,
  pub bearer_token: Option<String>,
  pub retry_max_attempts: Option<i64>,
  pub retry_backoff_ms: Option<i64>,
  pub retry_backoff_max_ms: Option<i64>,
  pub retry_jitter_ratio: Option<f64>,
  pub adaptive_retry: Option<bool>,
  pub adaptive_retry_mode: Option<String>,
  pub adaptive_retry_ewma_alpha: Option<f64>,
  pub circuit_breaker_failure_threshold: Option<i64>,
  pub circuit_breaker_open_ms: Option<i64>,
  pub circuit_breaker_half_open_probes: Option<i64>,
  pub circuit_breaker_state_path: Option<String>,
  pub circuit_breaker_state_url: Option<String>,
  pub circuit_breaker_state_patch: Option<bool>,
  pub circuit_breaker_state_patch_batch: Option<bool>,
  pub circuit_breaker_state_patch_batch_max_keys: Option<i64>,
  pub circuit_breaker_state_patch_merge: Option<bool>,
  pub circuit_breaker_state_patch_merge_max_keys: Option<i64>,
  pub circuit_breaker_state_patch_retry_max_attempts: Option<i64>,
  pub circuit_breaker_state_cas: Option<bool>,
  pub circuit_breaker_state_lease_id: Option<String>,
  pub circuit_breaker_scope_key: Option<String>,
  pub compression_gzip: Option<bool>,
  pub https_only: Option<bool>,
  pub ca_cert_pem_path: Option<String>,
  pub client_cert_pem_path: Option<String>,
  pub client_key_pem_path: Option<String>,
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

impl From<core_metrics::PrimaryReplicationMetrics> for PrimaryReplicationMetrics {
  fn from(metrics: core_metrics::PrimaryReplicationMetrics) -> Self {
    PrimaryReplicationMetrics {
      epoch: metrics.epoch,
      head_log_index: metrics.head_log_index,
      retained_floor: metrics.retained_floor,
      replica_count: metrics.replica_count,
      stale_epoch_replica_count: metrics.stale_epoch_replica_count,
      max_replica_lag: metrics.max_replica_lag,
      min_replica_applied_log_index: metrics.min_replica_applied_log_index,
      sidecar_path: metrics.sidecar_path,
      last_token: metrics.last_token,
      last_replication_error: metrics.last_replication_error,
      sidecar_needs_repair: metrics.sidecar_needs_repair,
      append_attempts: metrics.append_attempts,
      append_failures: metrics.append_failures,
      append_successes: metrics.append_successes,
    }
  }
}

impl From<core_metrics::ReplicaReplicationMetrics> for ReplicaReplicationMetrics {
  fn from(metrics: core_metrics::ReplicaReplicationMetrics) -> Self {
    ReplicaReplicationMetrics {
      applied_epoch: metrics.applied_epoch,
      applied_log_index: metrics.applied_log_index,
      needs_reseed: metrics.needs_reseed,
      last_error: metrics.last_error,
    }
  }
}

impl From<core_metrics::ReplicationMetrics> for ReplicationMetrics {
  fn from(metrics: core_metrics::ReplicationMetrics) -> Self {
    ReplicationMetrics {
      enabled: metrics.enabled,
      role: metrics.role,
      primary: metrics.primary.map(Into::into),
      replica: metrics.replica.map(Into::into),
    }
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

impl From<core_metrics::DatabaseMetrics> for DatabaseMetrics {
  fn from(metrics: core_metrics::DatabaseMetrics) -> Self {
    DatabaseMetrics {
      path: metrics.path,
      is_single_file: metrics.is_single_file,
      read_only: metrics.read_only,
      data: metrics.data.into(),
      cache: metrics.cache.into(),
      mvcc: metrics.mvcc.map(Into::into),
      replication: metrics.replication.into(),
      memory: metrics.memory.into(),
      collected_at: metrics.collected_at_ms,
    }
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

impl From<core_metrics::HealthCheckResult> for HealthCheckResult {
  fn from(result: core_metrics::HealthCheckResult) -> Self {
    HealthCheckResult {
      healthy: result.healthy,
      checks: result.checks.into_iter().map(Into::into).collect(),
    }
  }
}

impl From<core_metrics::OtlpHttpExportResult> for OtlpHttpExportResult {
  fn from(result: core_metrics::OtlpHttpExportResult) -> Self {
    OtlpHttpExportResult {
      status_code: result.status_code,
      response_body: result.response_body,
    }
  }
}

// ============================================================================
// Property Value (JS-compatible)
// ============================================================================

/// Property value types
#[napi(string_enum)]
#[derive(Clone)]
pub enum PropType {
  Null,
  Bool,
  Int,
  Float,
  String,
  Vector,
}

/// Property value wrapper for JS
#[napi(object)]
#[derive(Clone)]
pub struct JsPropValue {
  pub prop_type: PropType,
  pub bool_value: Option<bool>,
  pub int_value: Option<i64>,
  pub float_value: Option<f64>,
  pub string_value: Option<String>,
  pub vector_value: Option<Vec<f64>>,
}

impl From<PropValue> for JsPropValue {
  fn from(value: PropValue) -> Self {
    match value {
      PropValue::Null => JsPropValue {
        prop_type: PropType::Null,
        bool_value: None,
        int_value: None,
        float_value: None,
        string_value: None,
        vector_value: None,
      },
      PropValue::Bool(v) => JsPropValue {
        prop_type: PropType::Bool,
        bool_value: Some(v),
        int_value: None,
        float_value: None,
        string_value: None,
        vector_value: None,
      },
      PropValue::I64(v) => JsPropValue {
        prop_type: PropType::Int,
        bool_value: None,
        int_value: Some(v),
        float_value: None,
        string_value: None,
        vector_value: None,
      },
      PropValue::F64(v) => JsPropValue {
        prop_type: PropType::Float,
        bool_value: None,
        int_value: None,
        float_value: Some(v),
        string_value: None,
        vector_value: None,
      },
      PropValue::String(v) => JsPropValue {
        prop_type: PropType::String,
        bool_value: None,
        int_value: None,
        float_value: None,
        string_value: Some(v),
        vector_value: None,
      },
      PropValue::VectorF32(v) => JsPropValue {
        prop_type: PropType::Vector,
        bool_value: None,
        int_value: None,
        float_value: None,
        string_value: None,
        vector_value: Some(v.iter().map(|&x| x as f64).collect()),
      },
    }
  }
}

impl From<JsPropValue> for PropValue {
  fn from(value: JsPropValue) -> Self {
    match value.prop_type {
      PropType::Null => PropValue::Null,
      PropType::Bool => PropValue::Bool(value.bool_value.unwrap_or(false)),
      PropType::Int => PropValue::I64(value.int_value.unwrap_or(0)),
      PropType::Float => PropValue::F64(value.float_value.unwrap_or(0.0)),
      PropType::String => PropValue::String(value.string_value.unwrap_or_default()),
      PropType::Vector => {
        let vector = value.vector_value.unwrap_or_default();
        PropValue::VectorF32(vector.iter().map(|&x| x as f32).collect())
      }
    }
  }
}

// ============================================================================
// Edge Result
// ============================================================================

/// Edge representation for JS (neighbor style)
#[napi(object)]
pub struct JsEdge {
  pub etype: u32,
  pub node_id: i64,
}

/// Full edge representation for JS (src, etype, dst)
#[napi(object)]
pub struct JsFullEdge {
  pub src: f64,
  pub etype: u32,
  pub dst: f64,
}

/// Edge input with properties for batch operations
#[napi(object)]
pub struct JsEdgeWithPropsInput {
  pub src: f64,
  pub etype: u32,
  pub dst: f64,
  pub props: Vec<JsNodeProp>,
}

// ============================================================================
// Node Property Result
// ============================================================================

/// Node property key-value pair for JS
#[napi(object)]
pub struct JsNodeProp {
  pub key_id: u32,
  pub value: JsPropValue,
}

// ============================================================================
// Database NAPI Wrapper (single-file)
// ============================================================================

#[allow(clippy::large_enum_variant)]
enum DatabaseInner {
  SingleFile(RustSingleFileDB),
}

/// Database handle for single-file storage
#[napi]
pub struct Database {
  inner: Option<DatabaseInner>,
}

#[napi]
impl Database {
  /// Open a database file
  #[napi(factory)]
  pub fn open(path: String, options: Option<OpenOptions>) -> Result<Database> {
    let options = options.unwrap_or_default();
    let path_buf = PathBuf::from(&path);

    if path_buf.exists() && path_buf.is_dir() {
      return Err(Error::from_reason(
        "Multi-file databases are no longer supported. Provide a single-file path.",
      ));
    }

    let mut db_path = path_buf;
    if db_path.extension().is_some() {
      if !is_single_file_path(&db_path) {
        let ext = db_path
          .extension()
          .map(|value| value.to_string_lossy())
          .unwrap_or_else(|| "".into());
        return Err(Error::from_reason(format!(
          "Invalid database extension '.{ext}'. Single-file databases must use {} (or pass a path without an extension).",
          single_file_extension()
        )));
      }
    } else {
      db_path = PathBuf::from(format!("{path}{}", single_file_extension()));
    }

    let opts = options.into_rust()?;
    let db = open_single_file(&db_path, opts)
      .map_err(|e| Error::from_reason(format!("Failed to open database: {e}")))?;
    Ok(Database {
      inner: Some(DatabaseInner::SingleFile(db)),
    })
  }

  /// Close the database
  #[napi]
  pub fn close(&mut self) -> Result<()> {
    if let Some(db) = self.inner.take() {
      match db {
        DatabaseInner::SingleFile(db) => {
          close_single_file(db)
            .map_err(|e| Error::from_reason(format!("Failed to close database: {e}")))?;
        }
      }
    }
    Ok(())
  }

  /// Close the database and run a blocking checkpoint if WAL usage is above threshold.
  #[napi]
  pub fn close_with_checkpoint_if_wal_over(&mut self, threshold: f64) -> Result<()> {
    let threshold = validation::ratio("threshold", threshold)?;
    if let Some(db) = self.inner.take() {
      match db {
        DatabaseInner::SingleFile(db) => close_single_file_with_options(
          db,
          RustSingleFileCloseOptions::new().checkpoint_if_wal_usage_at_least(threshold),
        )
        .map_err(|e| Error::from_reason(format!("Failed to close database: {e}")))?,
      }
    }
    Ok(())
  }

  /// Check if database is open
  #[napi(getter)]
  pub fn is_open(&self) -> bool {
    self.inner.is_some()
  }

  /// Get database path
  #[napi(getter)]
  pub fn path(&self) -> Result<String> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.path.to_string_lossy().to_string()),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Check if database is read-only
  #[napi(getter)]
  pub fn read_only(&self) -> Result<bool> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.read_only),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  // ========================================================================
  // Transaction Methods
  // ========================================================================

  /// Begin a transaction
  #[napi]
  pub fn begin(&self, read_only: Option<bool>) -> Result<i64> {
    let read_only = read_only.unwrap_or(false);
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        let txid = db
          .begin(read_only)
          .map_err(|e| Error::from_reason(format!("Failed to begin transaction: {e}")))?;
        Ok(txid as i64)
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Begin a bulk-load transaction (fast path, MVCC disabled)
  #[napi]
  pub fn begin_bulk(&self) -> Result<i64> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        let txid = db
          .begin_bulk()
          .map_err(|e| Error::from_reason(format!("Failed to begin bulk transaction: {e}")))?;
        Ok(txid as i64)
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Commit the current transaction
  #[napi]
  pub fn commit(&self) -> Result<()> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .commit()
        .map_err(|e| Error::from_reason(format!("Failed to commit: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Commit the current transaction and return replication token when primary replication is enabled.
  #[napi]
  pub fn commit_with_token(&self) -> Result<Option<String>> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .commit_with_token()
        .map(|token| token.map(|value| value.to_string()))
        .map_err(|e| Error::from_reason(format!("Failed to commit with token: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Rollback the current transaction
  #[napi]
  pub fn rollback(&self) -> Result<()> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .rollback()
        .map_err(|e| Error::from_reason(format!("Failed to rollback: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Check if there's an active transaction
  #[napi]
  pub fn has_transaction(&self) -> Result<bool> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.has_transaction()),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Wait until the DB has observed at least the provided commit token.
  #[napi]
  pub fn wait_for_token(&self, token: String, timeout_ms: i64) -> Result<bool> {
    let timeout_ms =
      validation::non_negative_u64("timeoutMs", timeout_ms, validation::MAX_DURATION_MS as u64)?;
    let token = CommitToken::from_str(&token)
      .map_err(|e| Error::from_reason(format!("Invalid commit token: {e}")))?;

    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .wait_for_token(token, timeout_ms)
        .map_err(|e| Error::from_reason(format!("Failed waiting for token: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  // ========================================================================
  // Replication Methods
  // ========================================================================

  /// Primary replication status when role=primary, else null.
  #[napi]
  pub fn primary_replication_status(&self) -> Result<Option<JsPrimaryReplicationStatus>> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.primary_replication_status().map(Into::into)),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Replica replication status when role=replica, else null.
  #[napi]
  pub fn replica_replication_status(&self) -> Result<Option<JsReplicaReplicationStatus>> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.replica_replication_status().map(Into::into)),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Promote this primary to the next replication epoch.
  #[napi]
  pub fn primary_promote_to_next_epoch(&self) -> Result<i64> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .primary_promote_to_next_epoch()
        .map(|epoch| epoch as i64)
        .map_err(|e| Error::from_reason(format!("Failed to promote primary: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Report replica applied cursor to primary for retention decisions.
  #[napi]
  pub fn primary_report_replica_progress(
    &self,
    replica_id: String,
    epoch: i64,
    applied_log_index: i64,
  ) -> Result<()> {
    let epoch = validation::non_negative_u64("epoch", epoch, i64::MAX as u64)?;
    let applied_log_index =
      validation::non_negative_u64("appliedLogIndex", applied_log_index, i64::MAX as u64)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .primary_report_replica_progress(&replica_id, epoch, applied_log_index)
        .map_err(|e| Error::from_reason(format!("Failed to report replica progress: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Execute replication retention on primary.
  #[napi]
  pub fn primary_run_retention(&self) -> Result<JsPrimaryRetentionOutcome> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .primary_run_retention()
        .map(Into::into)
        .map_err(|e| Error::from_reason(format!("Failed to run retention: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Export latest primary snapshot metadata and optional bytes as transport JSON.
  #[napi]
  pub fn export_replication_snapshot_transport_json(
    &self,
    include_data: Option<bool>,
  ) -> Result<String> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .primary_export_snapshot_transport_json(include_data.unwrap_or(false))
        .map_err(|e| Error::from_reason(format!("Failed to export replication snapshot: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Export primary replication log page (cursor + limits) as transport JSON.
  #[napi]
  pub fn export_replication_log_transport_json(
    &self,
    cursor: Option<String>,
    max_frames: Option<i64>,
    max_bytes: Option<i64>,
    include_payload: Option<bool>,
  ) -> Result<String> {
    let max_frames = max_frames.unwrap_or(128);
    let max_bytes = max_bytes.unwrap_or(1_048_576);
    let max_frames = validation::positive_usize("maxFrames", max_frames, validation::MAX_COUNT)?;
    let max_bytes = validation::positive_usize("maxBytes", max_bytes, validation::MAX_BYTES)?;

    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .primary_export_log_transport_json(
          cursor.as_deref(),
          max_frames,
          max_bytes,
          include_payload.unwrap_or(true),
        )
        .map_err(|e| Error::from_reason(format!("Failed to export replication log: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Bootstrap a replica from the primary snapshot.
  #[napi]
  pub fn replica_bootstrap_from_snapshot(&self) -> Result<()> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .replica_bootstrap_from_snapshot()
        .map_err(|e| Error::from_reason(format!("Failed to bootstrap replica: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Pull and apply up to maxFrames replication frames on replica.
  #[napi]
  pub fn replica_catch_up_once(&self, max_frames: i64) -> Result<i64> {
    let max_frames =
      validation::non_negative_usize("maxFrames", max_frames, validation::MAX_COUNT)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .replica_catch_up_once(max_frames)
        .map(|count| count as i64)
        .map_err(|e| Error::from_reason(format!("Failed replica catch-up: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Force a replica reseed from current primary snapshot.
  #[napi]
  pub fn replica_reseed_from_snapshot(&self) -> Result<()> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .replica_reseed_from_snapshot()
        .map_err(|e| Error::from_reason(format!("Failed to reseed replica: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  // ========================================================================
  // Node Operations
  // ========================================================================

  /// Create a new node
  #[napi]
  pub fn create_node(&self, key: Option<String>) -> Result<i64> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        let node_id = db
          .create_node(key.as_deref())
          .map_err(|e| Error::from_reason(format!("Failed to create node: {e}")))?;
        Ok(node_id as i64)
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Create multiple nodes in a single WAL record (fast path)
  #[napi]
  pub fn create_nodes_batch(&self, keys: Vec<Option<String>>) -> Result<Vec<i64>> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        let key_refs: Vec<Option<&str>> = keys.iter().map(|k| k.as_deref()).collect();
        let node_ids = db
          .create_nodes_batch(&key_refs)
          .map_err(|e| Error::from_reason(format!("Failed to create nodes: {e}")))?;
        Ok(node_ids.into_iter().map(|id| id as i64).collect())
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Upsert a node by key (create if missing, update props)
  #[napi]
  pub fn upsert_node(&self, key: String, props: Vec<JsNodeProp>) -> Result<i64> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        let node_id = match db.node_by_key(&key) {
          Some(id) => id,
          None => db
            .create_node(Some(&key))
            .map_err(|e| Error::from_reason(format!("Failed to create node: {e}")))?,
        };

        for prop in props {
          let key_id = prop.key_id as PropKeyId;
          if matches!(prop.value.prop_type, PropType::Null) {
            db.delete_node_prop(node_id, key_id)
              .map_err(|e| Error::from_reason(format!("Failed to delete property: {e}")))?;
          } else {
            db.set_node_prop(node_id, key_id, prop.value.into())
              .map_err(|e| Error::from_reason(format!("Failed to set property: {e}")))?;
          }
        }

        Ok(node_id as i64)
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Upsert a node by ID (create if missing, update props)
  #[napi]
  pub fn upsert_node_by_id(&self, node_id: f64, props: Vec<JsNodeProp>) -> Result<i64> {
    let node_id = validation::node_id("nodeId", node_id)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        if !db.node_exists(node_id) {
          db.create_node_with_id(node_id, None)
            .map_err(|e| Error::from_reason(format!("Failed to create node: {e}")))?;
        }

        for prop in props {
          let key_id = prop.key_id as PropKeyId;
          if matches!(prop.value.prop_type, PropType::Null) {
            db.delete_node_prop(node_id, key_id)
              .map_err(|e| Error::from_reason(format!("Failed to delete property: {e}")))?;
          } else {
            db.set_node_prop(node_id, key_id, prop.value.into())
              .map_err(|e| Error::from_reason(format!("Failed to set property: {e}")))?;
          }
        }

        Ok(node_id as i64)
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Delete a node
  #[napi]
  pub fn delete_node(&self, node_id: f64) -> Result<()> {
    let node_id = validation::node_id("nodeId", node_id)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .delete_node(node_id)
        .map_err(|e| Error::from_reason(format!("Failed to delete node: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Check if a node exists
  #[napi]
  pub fn node_exists(&self, node_id: f64) -> Result<bool> {
    let node_id = validation::node_id("nodeId", node_id)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.node_exists(node_id)),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get node by key
  #[napi(js_name = "get_node_by_key")]
  pub fn node_by_key(&self, key: String) -> Result<Option<i64>> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.node_by_key(&key).map(|id| id as i64)),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get the key for a node
  #[napi(js_name = "get_node_key")]
  pub fn node_key(&self, node_id: f64) -> Result<Option<String>> {
    let node_id = validation::node_id("nodeId", node_id)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.node_key(node_id)),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// List all node IDs
  #[napi]
  pub fn list_nodes(&self) -> Result<Vec<i64>> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        Ok(db.list_nodes().into_iter().map(|id| id as i64).collect())
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Count all nodes
  #[napi]
  pub fn count_nodes(&self) -> Result<i64> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.count_nodes() as i64),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  // ========================================================================
  // Edge Operations
  // ========================================================================

  /// Add an edge
  #[napi]
  pub fn add_edge(&self, src: f64, etype: u32, dst: f64) -> Result<()> {
    let src = validation::node_id("src", src)?;
    let dst = validation::node_id("dst", dst)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .add_edge(src, etype as ETypeId, dst)
        .map_err(|e| Error::from_reason(format!("Failed to add edge: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Add multiple edges in a single WAL record (fast path)
  #[napi]
  pub fn add_edges_batch(&self, edges: Vec<JsFullEdge>) -> Result<()> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        let core_edges = edges
          .into_iter()
          .map(|edge| {
            Ok((
              validation::node_id("src", edge.src)?,
              edge.etype as ETypeId,
              validation::node_id("dst", edge.dst)?,
            ))
          })
          .collect::<Result<Vec<(NodeId, ETypeId, NodeId)>>>()?;
        db.add_edges_batch(&core_edges)
          .map_err(|e| Error::from_reason(format!("Failed to add edges: {e}")))
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Add multiple edges with props in a single WAL record (fast path)
  #[napi]
  pub fn add_edges_with_props_batch(&self, edges: Vec<JsEdgeWithPropsInput>) -> Result<()> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        let core_edges = edges
          .into_iter()
          .map(|edge| {
            let props = edge
              .props
              .into_iter()
              .map(|prop| (prop.key_id as PropKeyId, prop.value.into()))
              .collect();
            Ok((
              validation::node_id("src", edge.src)?,
              edge.etype as ETypeId,
              validation::node_id("dst", edge.dst)?,
              props,
            ))
          })
          .collect::<Result<Vec<CoreEdgeWithProps>>>()?;
        db.add_edges_with_props_batch(core_edges)
          .map_err(|e| Error::from_reason(format!("Failed to add edges: {e}")))
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Add an edge by type name
  #[napi]
  pub fn add_edge_by_name(&self, src: f64, etype_name: String, dst: f64) -> Result<()> {
    let src = validation::node_id("src", src)?;
    let dst = validation::node_id("dst", dst)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .add_edge_by_name(src, &etype_name, dst)
        .map_err(|e| Error::from_reason(format!("Failed to add edge: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Upsert an edge (create if missing, update props)
  ///
  /// Returns true if the edge was created.
  #[napi]
  pub fn upsert_edge(
    &self,
    src: f64,
    etype: u32,
    dst: f64,
    props: Vec<JsNodeProp>,
  ) -> Result<bool> {
    let src = validation::node_id("src", src)?;
    let dst = validation::node_id("dst", dst)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        let updates: Vec<(PropKeyId, Option<PropValue>)> = props
          .into_iter()
          .map(|prop| {
            let value_opt = match prop.value.prop_type {
              PropType::Null => None,
              _ => Some(prop.value.into()),
            };
            (prop.key_id as PropKeyId, value_opt)
          })
          .collect();

        db.upsert_edge_with_props(src, etype as ETypeId, dst, updates)
          .map_err(|e| Error::from_reason(format!("Failed to upsert edge: {e}")))
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Delete an edge
  #[napi]
  pub fn delete_edge(&self, src: f64, etype: u32, dst: f64) -> Result<()> {
    let src = validation::node_id("src", src)?;
    let dst = validation::node_id("dst", dst)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .delete_edge(src, etype as ETypeId, dst)
        .map_err(|e| Error::from_reason(format!("Failed to delete edge: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Check if an edge exists
  #[napi]
  pub fn edge_exists(&self, src: f64, etype: u32, dst: f64) -> Result<bool> {
    let src = validation::node_id("src", src)?;
    let dst = validation::node_id("dst", dst)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.edge_exists(src, etype as ETypeId, dst)),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get outgoing edges for a node
  #[napi(js_name = "get_out_edges")]
  pub fn out_edges(&self, node_id: f64) -> Result<Vec<JsEdge>> {
    let node_id = validation::node_id("nodeId", node_id)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(
        db.out_edges(node_id)
          .into_iter()
          .map(|(etype, dst)| JsEdge {
            etype,
            node_id: dst as i64,
          })
          .collect(),
      ),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get incoming edges for a node
  #[napi(js_name = "get_in_edges")]
  pub fn in_edges(&self, node_id: f64) -> Result<Vec<JsEdge>> {
    let node_id = validation::node_id("nodeId", node_id)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(
        db.in_edges(node_id)
          .into_iter()
          .map(|(etype, src)| JsEdge {
            etype,
            node_id: src as i64,
          })
          .collect(),
      ),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get out-degree for a node
  #[napi(js_name = "get_out_degree")]
  pub fn out_degree(&self, node_id: f64) -> Result<i64> {
    let node_id = validation::node_id("nodeId", node_id)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.out_degree(node_id) as i64),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get in-degree for a node
  #[napi(js_name = "get_in_degree")]
  pub fn in_degree(&self, node_id: f64) -> Result<i64> {
    let node_id = validation::node_id("nodeId", node_id)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.in_degree(node_id) as i64),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Count all edges
  #[napi]
  pub fn count_edges(&self) -> Result<i64> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.count_edges() as i64),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// List all edges in the database
  ///
  /// Returns an array of {src, etype, dst} objects representing all edges.
  /// Optionally filter by edge type.
  #[napi]
  pub fn list_edges(&self, etype: Option<u32>) -> Result<Vec<JsFullEdge>> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(
        db.list_edges(etype)
          .into_iter()
          .map(|e| JsFullEdge {
            src: e.src as f64,
            etype: e.etype,
            dst: e.dst as f64,
          })
          .collect(),
      ),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// List edges by type name
  ///
  /// Returns an array of {src, etype, dst} objects for the given edge type.
  #[napi]
  pub fn list_edges_by_name(&self, etype_name: String) -> Result<Vec<JsFullEdge>> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        let etype = db
          .etype_id(&etype_name)
          .ok_or_else(|| Error::from_reason(format!("Unknown edge type: {etype_name}")))?;
        Ok(
          db.list_edges(Some(etype))
            .into_iter()
            .map(|e| JsFullEdge {
              src: e.src as f64,
              etype: e.etype,
              dst: e.dst as f64,
            })
            .collect(),
        )
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Count edges by type
  #[napi]
  pub fn count_edges_by_type(&self, etype: u32) -> Result<i64> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.count_edges_by_type(etype) as i64),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Count edges by type name
  #[napi]
  pub fn count_edges_by_name(&self, etype_name: String) -> Result<i64> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        let etype = db
          .etype_id(&etype_name)
          .ok_or_else(|| Error::from_reason(format!("Unknown edge type: {etype_name}")))?;
        Ok(db.count_edges_by_type(etype) as i64)
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  // ========================================================================
  // Streaming and Pagination
  // ========================================================================

  /// Stream nodes in batches
  #[napi]
  pub fn stream_nodes(&self, options: Option<StreamOptions>) -> Result<Vec<Vec<i64>>> {
    let options = options.unwrap_or_default().into_rust()?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(
        streaming::stream_nodes_single(db, options)
          .into_iter()
          .map(|batch| batch.into_iter().map(|id| id as i64).collect())
          .collect(),
      ),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Stream nodes with properties in batches
  #[napi]
  pub fn stream_nodes_with_props(
    &self,
    options: Option<StreamOptions>,
  ) -> Result<Vec<Vec<NodeWithProps>>> {
    let options = options.unwrap_or_default().into_rust()?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        let batches = streaming::stream_nodes_single(db, options);
        Ok(
          batches
            .into_iter()
            .map(|batch| {
              batch
                .into_iter()
                .map(|node_id| {
                  let key = db.node_key(node_id as NodeId);
                  let props = db.node_props(node_id as NodeId).unwrap_or_default();
                  let props = props
                    .into_iter()
                    .map(|(k, v)| JsNodeProp {
                      key_id: k,
                      value: v.into(),
                    })
                    .collect();
                  NodeWithProps {
                    id: node_id as i64,
                    key,
                    props,
                  }
                })
                .collect()
            })
            .collect(),
        )
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Stream edges in batches
  #[napi]
  pub fn stream_edges(&self, options: Option<StreamOptions>) -> Result<Vec<Vec<JsFullEdge>>> {
    let options = options.unwrap_or_default().into_rust()?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(
        streaming::stream_edges_single(db, options)
          .into_iter()
          .map(|batch| {
            batch
              .into_iter()
              .map(|edge| JsFullEdge {
                src: edge.src as f64,
                etype: edge.etype,
                dst: edge.dst as f64,
              })
              .collect()
          })
          .collect(),
      ),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Stream edges with properties in batches
  #[napi]
  pub fn stream_edges_with_props(
    &self,
    options: Option<StreamOptions>,
  ) -> Result<Vec<Vec<EdgeWithProps>>> {
    let options = options.unwrap_or_default().into_rust()?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        let batches = streaming::stream_edges_single(db, options);
        Ok(
          batches
            .into_iter()
            .map(|batch| {
              batch
                .into_iter()
                .map(|edge| {
                  let props = db
                    .edge_props(edge.src, edge.etype, edge.dst)
                    .unwrap_or_default();
                  let props = props
                    .into_iter()
                    .map(|(k, v)| JsNodeProp {
                      key_id: k,
                      value: v.into(),
                    })
                    .collect();
                  EdgeWithProps {
                    src: edge.src as i64,
                    etype: edge.etype,
                    dst: edge.dst as i64,
                    props,
                  }
                })
                .collect()
            })
            .collect(),
        )
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get a page of node IDs
  #[napi(js_name = "get_nodes_page")]
  pub fn nodes_page(&self, options: Option<PaginationOptions>) -> Result<NodePage> {
    let options = options.unwrap_or_default().into_rust()?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        let page = streaming::nodes_page_single(db, options);
        Ok(NodePage {
          items: page.items.into_iter().map(|id| id as i64).collect(),
          next_cursor: page.next_cursor,
          has_more: page.has_more,
          total: Some(db.count_nodes() as i64),
        })
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get a page of edges
  #[napi(js_name = "get_edges_page")]
  pub fn edges_page(&self, options: Option<PaginationOptions>) -> Result<EdgePage> {
    let options = options.unwrap_or_default().into_rust()?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        let page = streaming::edges_page_single(db, options);
        Ok(EdgePage {
          items: page
            .items
            .into_iter()
            .map(|edge| JsFullEdge {
              src: edge.src as f64,
              etype: edge.etype,
              dst: edge.dst as f64,
            })
            .collect(),
          next_cursor: page.next_cursor,
          has_more: page.has_more,
          total: Some(db.count_edges() as i64),
        })
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  // ========================================================================
  // Property Operations
  // ========================================================================

  /// Set a node property
  #[napi]
  pub fn set_node_prop(&self, node_id: f64, key_id: u32, value: JsPropValue) -> Result<()> {
    let node_id = validation::node_id("nodeId", node_id)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .set_node_prop(node_id, key_id as PropKeyId, value.into())
        .map_err(|e| Error::from_reason(format!("Failed to set property: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Set a node property by key name
  #[napi]
  pub fn set_node_prop_by_name(
    &self,
    node_id: f64,
    key_name: String,
    value: JsPropValue,
  ) -> Result<()> {
    let node_id = validation::node_id("nodeId", node_id)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .set_node_prop_by_name(node_id, &key_name, value.into())
        .map_err(|e| Error::from_reason(format!("Failed to set property: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Delete a node property
  #[napi]
  pub fn delete_node_prop(&self, node_id: f64, key_id: u32) -> Result<()> {
    let node_id = validation::node_id("nodeId", node_id)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .delete_node_prop(node_id, key_id as PropKeyId)
        .map_err(|e| Error::from_reason(format!("Failed to delete property: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get a specific node property
  #[napi(js_name = "get_node_prop")]
  pub fn node_prop(&self, node_id: f64, key_id: u32) -> Result<Option<JsPropValue>> {
    let node_id = validation::node_id("nodeId", node_id)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        Ok(db.node_prop(node_id, key_id as PropKeyId).map(|v| v.into()))
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get all properties for a node (returns array of {key_id, value} pairs)
  #[napi(js_name = "get_node_props")]
  pub fn node_props(&self, node_id: f64) -> Result<Option<Vec<JsNodeProp>>> {
    let node_id = validation::node_id("nodeId", node_id)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.node_props(node_id).map(|props| {
        props
          .into_iter()
          .map(|(k, v)| JsNodeProp {
            key_id: k,
            value: v.into(),
          })
          .collect()
      })),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  // ========================================================================
  // Edge Property Operations
  // ========================================================================

  /// Set an edge property
  #[napi]
  pub fn set_edge_prop(
    &self,
    src: f64,
    etype: u32,
    dst: f64,
    key_id: u32,
    value: JsPropValue,
  ) -> Result<()> {
    let src = validation::node_id("src", src)?;
    let dst = validation::node_id("dst", dst)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .set_edge_prop(
          src,
          etype as ETypeId,
          dst,
          key_id as PropKeyId,
          value.into(),
        )
        .map_err(|e| Error::from_reason(format!("Failed to set edge property: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Set an edge property by key name
  #[napi]
  pub fn set_edge_prop_by_name(
    &self,
    src: f64,
    etype: u32,
    dst: f64,
    key_name: String,
    value: JsPropValue,
  ) -> Result<()> {
    let src = validation::node_id("src", src)?;
    let dst = validation::node_id("dst", dst)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .set_edge_prop_by_name(src, etype as ETypeId, dst, &key_name, value.into())
        .map_err(|e| Error::from_reason(format!("Failed to set edge property: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Delete an edge property
  #[napi]
  pub fn delete_edge_prop(&self, src: f64, etype: u32, dst: f64, key_id: u32) -> Result<()> {
    let src = validation::node_id("src", src)?;
    let dst = validation::node_id("dst", dst)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .delete_edge_prop(src, etype as ETypeId, dst, key_id as PropKeyId)
        .map_err(|e| Error::from_reason(format!("Failed to delete edge property: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get a specific edge property
  #[napi(js_name = "get_edge_prop")]
  pub fn edge_prop(
    &self,
    src: f64,
    etype: u32,
    dst: f64,
    key_id: u32,
  ) -> Result<Option<JsPropValue>> {
    let src = validation::node_id("src", src)?;
    let dst = validation::node_id("dst", dst)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(
        db.edge_prop(src, etype as ETypeId, dst, key_id as PropKeyId)
          .map(|v| v.into()),
      ),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get all properties for an edge (returns array of {key_id, value} pairs)
  #[napi(js_name = "get_edge_props")]
  pub fn edge_props(&self, src: f64, etype: u32, dst: f64) -> Result<Option<Vec<JsNodeProp>>> {
    let src = validation::node_id("src", src)?;
    let dst = validation::node_id("dst", dst)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        Ok(db.edge_props(src, etype as ETypeId, dst).map(|props| {
          props
            .into_iter()
            .map(|(k, v)| JsNodeProp {
              key_id: k,
              value: v.into(),
            })
            .collect()
        }))
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  // ========================================================================
  // Vector Operations
  // ========================================================================

  /// Set a vector embedding for a node
  #[napi]
  pub fn set_node_vector(&self, node_id: f64, prop_key_id: u32, vector: Vec<f64>) -> Result<()> {
    let node_id = validation::node_id("nodeId", node_id)?;
    let vector_f32: Vec<f32> = vector.iter().map(|&v| v as f32).collect();
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .set_node_vector(node_id, prop_key_id as PropKeyId, &vector_f32)
        .map_err(|e| Error::from_reason(format!("Failed to set vector: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get a vector embedding for a node
  #[napi(js_name = "get_node_vector")]
  pub fn node_vector(&self, node_id: f64, prop_key_id: u32) -> Result<Option<Vec<f64>>> {
    let node_id = validation::node_id("nodeId", node_id)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(
        db.node_vector(node_id, prop_key_id as PropKeyId)
          .map(|v| v.iter().map(|&f| f as f64).collect()),
      ),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Delete a vector embedding for a node
  #[napi]
  pub fn delete_node_vector(&self, node_id: f64, prop_key_id: u32) -> Result<()> {
    let node_id = validation::node_id("nodeId", node_id)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .delete_node_vector(node_id, prop_key_id as PropKeyId)
        .map_err(|e| Error::from_reason(format!("Failed to delete vector: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Check if a node has a vector embedding
  #[napi]
  pub fn has_node_vector(&self, node_id: f64, prop_key_id: u32) -> Result<bool> {
    let node_id = validation::node_id("nodeId", node_id)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        Ok(db.has_node_vector(node_id, prop_key_id as PropKeyId))
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  // ========================================================================
  // Schema Operations
  // ========================================================================

  /// Get or create a label ID
  #[napi(js_name = "get_or_create_label")]
  pub fn ensure_label(&self, name: String) -> Result<u32> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .ensure_label(&name)
        .map_err(|e| Error::from_reason(format!("Failed to ensure label: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get label ID by name
  #[napi(js_name = "get_label_id")]
  pub fn label_id(&self, name: String) -> Result<Option<u32>> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.label_id(&name)),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get label name by ID
  #[napi(js_name = "get_label_name")]
  pub fn label_name(&self, id: u32) -> Result<Option<String>> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.label_name(id)),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get or create an edge type ID
  #[napi(js_name = "get_or_create_etype")]
  pub fn ensure_etype(&self, name: String) -> Result<u32> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .ensure_etype(&name)
        .map_err(|e| Error::from_reason(format!("Failed to ensure edge type: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get edge type ID by name
  #[napi(js_name = "get_etype_id")]
  pub fn etype_id(&self, name: String) -> Result<Option<u32>> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.etype_id(&name)),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get edge type name by ID
  #[napi(js_name = "get_etype_name")]
  pub fn etype_name(&self, id: u32) -> Result<Option<String>> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.etype_name(id)),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get or create a property key ID
  #[napi(js_name = "get_or_create_propkey")]
  pub fn ensure_propkey(&self, name: String) -> Result<u32> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .ensure_propkey(&name)
        .map_err(|e| Error::from_reason(format!("Failed to ensure property key: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get property key ID by name
  #[napi(js_name = "get_propkey_id")]
  pub fn propkey_id(&self, name: String) -> Result<Option<u32>> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.propkey_id(&name)),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get property key name by ID
  #[napi(js_name = "get_propkey_name")]
  pub fn propkey_name(&self, id: u32) -> Result<Option<String>> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.propkey_name(id)),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  // ========================================================================
  // Node Label Operations
  // ========================================================================

  /// Define a new label (requires transaction)
  #[napi]
  pub fn define_label(&self, name: String) -> Result<u32> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .define_label(&name)
        .map_err(|e| Error::from_reason(format!("Failed to define label: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Add a label to a node
  #[napi]
  pub fn add_node_label(&self, node_id: f64, label_id: u32) -> Result<()> {
    let node_id = validation::node_id("nodeId", node_id)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .add_node_label(node_id, label_id)
        .map_err(|e| Error::from_reason(format!("Failed to add label: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Add a label to a node by name
  #[napi]
  pub fn add_node_label_by_name(&self, node_id: f64, label_name: String) -> Result<()> {
    let node_id = validation::node_id("nodeId", node_id)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .add_node_label_by_name(node_id, &label_name)
        .map_err(|e| Error::from_reason(format!("Failed to add label: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Remove a label from a node
  #[napi]
  pub fn remove_node_label(&self, node_id: f64, label_id: u32) -> Result<()> {
    let node_id = validation::node_id("nodeId", node_id)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .remove_node_label(node_id, label_id)
        .map_err(|e| Error::from_reason(format!("Failed to remove label: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Check if a node has a label
  #[napi]
  pub fn node_has_label(&self, node_id: f64, label_id: u32) -> Result<bool> {
    let node_id = validation::node_id("nodeId", node_id)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.node_has_label(node_id, label_id)),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get all labels for a node
  #[napi(js_name = "get_node_labels")]
  pub fn node_labels(&self, node_id: f64) -> Result<Vec<u32>> {
    let node_id = validation::node_id("nodeId", node_id)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.node_labels(node_id)),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  // ========================================================================
  // Traversal (DB-backed)
  // ========================================================================

  /// Execute a single-hop traversal from start nodes
  ///
  /// @param startNodes - Array of starting node IDs
  /// @param direction - Traversal direction
  /// @param edgeType - Optional edge type filter
  /// @returns Array of traversal results
  #[napi]
  pub fn traverse_single(
    &self,
    start_nodes: Vec<f64>,
    direction: JsTraversalDirection,
    edge_type: Option<u32>,
  ) -> Result<Vec<JsTraversalResult>> {
    let start = validation::node_ids("startNodes", &start_nodes)?;
    let etype = edge_type;

    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        let builder = match direction {
          JsTraversalDirection::Out => RustTraversalBuilder::new(start).out(etype),
          JsTraversalDirection::In => RustTraversalBuilder::new(start).r#in(etype),
          JsTraversalDirection::Both => RustTraversalBuilder::new(start).both(etype),
        };

        Ok(
          builder
            .execute(|node_id, dir, etype| neighbors_from_single_file(db, node_id, dir, etype))
            .map(JsTraversalResult::from)
            .collect(),
        )
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Execute a multi-hop traversal
  ///
  /// @param startNodes - Array of starting node IDs
  /// @param steps - Array of traversal steps (direction, edgeType)
  /// @param limit - Maximum number of results
  /// @returns Array of traversal results
  #[napi]
  pub fn traverse(
    &self,
    start_nodes: Vec<f64>,
    steps: Vec<JsTraversalStep>,
    limit: Option<u32>,
  ) -> Result<Vec<JsTraversalResult>> {
    let start = validation::node_ids("startNodes", &start_nodes)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        let mut builder = RustTraversalBuilder::new(start);

        for step in steps {
          let etype = step.edge_type;
          builder = match step.direction {
            JsTraversalDirection::Out => builder.out(etype),
            JsTraversalDirection::In => builder.r#in(etype),
            JsTraversalDirection::Both => builder.both(etype),
          };
        }

        if let Some(n) = limit {
          let n = validation::non_negative_usize("limit", n as i64, validation::MAX_COUNT)?;
          builder = builder.take(n);
        }

        Ok(
          builder
            .execute(|node_id, dir, etype| neighbors_from_single_file(db, node_id, dir, etype))
            .map(JsTraversalResult::from)
            .collect(),
        )
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Execute a variable-depth traversal
  ///
  /// @param startNodes - Array of starting node IDs
  /// @param edgeType - Optional edge type filter
  /// @param options - Traversal options (maxDepth, minDepth, direction, unique)
  /// @returns Array of traversal results
  #[napi]
  pub fn traverse_depth(
    &self,
    start_nodes: Vec<f64>,
    edge_type: Option<u32>,
    options: JsTraverseOptions,
  ) -> Result<Vec<JsTraversalResult>> {
    let start = validation::node_ids("startNodes", &start_nodes)?;
    let opts = options.to_rust()?;

    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(
        RustTraversalBuilder::new(start)
          .traverse(edge_type, opts)
          .execute(|node_id, dir, etype| neighbors_from_single_file(db, node_id, dir, etype))
          .map(JsTraversalResult::from)
          .collect(),
      ),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Count traversal results without materializing them
  ///
  /// @param startNodes - Array of starting node IDs
  /// @param steps - Array of traversal steps
  /// @returns Number of results
  #[napi]
  pub fn traverse_count(&self, start_nodes: Vec<f64>, steps: Vec<JsTraversalStep>) -> Result<u32> {
    let start = validation::node_ids("startNodes", &start_nodes)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        let mut builder = RustTraversalBuilder::new(start);

        for step in steps {
          let etype = step.edge_type;
          builder = match step.direction {
            JsTraversalDirection::Out => builder.out(etype),
            JsTraversalDirection::In => builder.r#in(etype),
            JsTraversalDirection::Both => builder.both(etype),
          };
        }

        Ok(
          builder.count(|node_id, dir, etype| neighbors_from_single_file(db, node_id, dir, etype))
            as u32,
        )
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get just the node IDs from a traversal
  ///
  /// @param startNodes - Array of starting node IDs
  /// @param steps - Array of traversal steps
  /// @param limit - Maximum number of results
  /// @returns Array of node IDs
  #[napi]
  pub fn traverse_node_ids(
    &self,
    start_nodes: Vec<f64>,
    steps: Vec<JsTraversalStep>,
    limit: Option<u32>,
  ) -> Result<Vec<i64>> {
    let start = validation::node_ids("startNodes", &start_nodes)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        let mut builder = RustTraversalBuilder::new(start);

        for step in steps {
          let etype = step.edge_type;
          builder = match step.direction {
            JsTraversalDirection::Out => builder.out(etype),
            JsTraversalDirection::In => builder.r#in(etype),
            JsTraversalDirection::Both => builder.both(etype),
          };
        }

        if let Some(n) = limit {
          let n = validation::non_negative_usize("limit", n as i64, validation::MAX_COUNT)?;
          builder = builder.take(n);
        }

        Ok(
          builder
            .collect_node_ids(|node_id, dir, etype| {
              neighbors_from_single_file(db, node_id, dir, etype)
            })
            .into_iter()
            .map(|id| id as i64)
            .collect(),
        )
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  // ========================================================================
  // Pathfinding (DB-backed)
  // ========================================================================

  /// Find shortest path using Dijkstra's algorithm
  ///
  /// @param config - Pathfinding configuration
  /// @returns Path result with nodes, edges, and weight
  #[napi]
  pub fn dijkstra(&self, config: JsPathConfig) -> Result<JsPathResult> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        let weight_key = resolve_weight_key_single_file(db, &config)?;
        let rust_config = config.to_rust()?;
        Ok(
          dijkstra(
            rust_config,
            |node_id, dir, etype| neighbors_from_single_file(db, node_id, dir, etype),
            |src, etype, dst| edge_weight_from_single_file(db, src, etype, dst, weight_key),
          )
          .into(),
        )
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Find shortest path using BFS (unweighted)
  ///
  /// Faster than Dijkstra for unweighted graphs.
  ///
  /// @param config - Pathfinding configuration
  /// @returns Path result with nodes, edges, and weight
  #[napi]
  pub fn bfs(&self, config: JsPathConfig) -> Result<JsPathResult> {
    let rust_config = config.to_rust()?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(
        bfs(rust_config, |node_id, dir, etype| {
          neighbors_from_single_file(db, node_id, dir, etype)
        })
        .into(),
      ),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Find k shortest paths using Yen's algorithm
  ///
  /// @param config - Pathfinding configuration
  /// @param k - Maximum number of paths to find
  /// @returns Array of path results sorted by weight
  #[napi]
  pub fn k_shortest(&self, config: JsPathConfig, k: u32) -> Result<Vec<JsPathResult>> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        let weight_key = resolve_weight_key_single_file(db, &config)?;
        let rust_config = config.to_rust()?;
        let k = validation::non_negative_usize("k", k as i64, validation::MAX_COUNT)?;
        Ok(
          yen_k_shortest(
            rust_config,
            k,
            |node_id, dir, etype| neighbors_from_single_file(db, node_id, dir, etype),
            |src, etype, dst| edge_weight_from_single_file(db, src, etype, dst, weight_key),
          )
          .into_iter()
          .map(JsPathResult::from)
          .collect(),
        )
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Find shortest path between two nodes (convenience method)
  ///
  /// @param source - Source node ID
  /// @param target - Target node ID
  /// @param edgeType - Optional edge type filter
  /// @param maxDepth - Maximum search depth
  /// @returns Path result
  #[napi]
  pub fn shortest_path(
    &self,
    source: f64,
    target: f64,
    edge_type: Option<u32>,
    max_depth: Option<u32>,
  ) -> Result<JsPathResult> {
    let config = JsPathConfig {
      source,
      target: Some(target),
      targets: None,
      allowed_edge_types: edge_type.map(|e| vec![e]),
      weight_key_id: None,
      weight_key_name: None,
      direction: Some(JsTraversalDirection::Out),
      max_depth,
    };

    self.dijkstra(config)
  }

  /// Check if a path exists between two nodes
  ///
  /// @param source - Source node ID
  /// @param target - Target node ID
  /// @param edgeType - Optional edge type filter
  /// @param maxDepth - Maximum search depth
  /// @returns true if path exists
  #[napi]
  pub fn has_path(
    &self,
    source: f64,
    target: f64,
    edge_type: Option<u32>,
    max_depth: Option<u32>,
  ) -> Result<bool> {
    Ok(
      self
        .shortest_path(source, target, edge_type, max_depth)?
        .found,
    )
  }

  /// Get all nodes reachable from a source within a certain depth
  ///
  /// @param source - Source node ID
  /// @param maxDepth - Maximum depth to traverse
  /// @param edgeType - Optional edge type filter
  /// @returns Array of reachable node IDs
  #[napi]
  pub fn reachable_nodes(
    &self,
    source: f64,
    max_depth: u32,
    edge_type: Option<u32>,
  ) -> Result<Vec<i64>> {
    let opts = JsTraverseOptions {
      direction: Some(JsTraversalDirection::Out),
      min_depth: Some(1),
      max_depth,
      unique: Some(true),
    };

    Ok(
      self
        .traverse_depth(vec![source], edge_type, opts)?
        .into_iter()
        .map(|r| r.node_id)
        .collect(),
    )
  }

  // ========================================================================
  // Checkpoint / Maintenance
  // ========================================================================

  /// Perform a checkpoint (compact WAL into snapshot)
  #[napi]
  pub fn checkpoint(&self) -> Result<()> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .checkpoint()
        .map_err(|e| Error::from_reason(format!("Failed to checkpoint: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Perform a background (non-blocking) checkpoint
  #[napi]
  pub fn background_checkpoint(&self) -> Result<()> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .background_checkpoint()
        .map_err(|e| Error::from_reason(format!("Failed to background checkpoint: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Check if checkpoint is recommended
  #[napi]
  pub fn should_checkpoint(&self, threshold: Option<f64>) -> Result<bool> {
    let threshold = validation::ratio("threshold", threshold.unwrap_or(0.8))?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.should_checkpoint(threshold)),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Optimize (compact) the database
  ///
  /// For single-file databases, this compacts the WAL into a new snapshot
  /// (equivalent to optimizeSingleFile in the TypeScript API).
  #[napi]
  pub fn optimize(&mut self) -> Result<()> {
    match self.inner.as_mut() {
      Some(DatabaseInner::SingleFile(db)) => db
        .optimize_single_file(None)
        .map_err(|e| Error::from_reason(format!("Failed to optimize: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Optimize (compact) a single-file database with options
  #[napi(js_name = "optimizeSingleFile")]
  pub fn optimize_single_file(&mut self, options: Option<SingleFileOptimizeOptions>) -> Result<()> {
    let options = options
      .map(SingleFileOptimizeOptions::into_rust)
      .transpose()?;
    match self.inner.as_mut() {
      Some(DatabaseInner::SingleFile(db)) => db
        .optimize_single_file(options)
        .map_err(|e| Error::from_reason(format!("Failed to optimize single-file: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Vacuum a single-file database to reclaim free space
  #[napi]
  pub fn vacuum(&mut self, options: Option<VacuumOptions>) -> Result<()> {
    let options = options.map(VacuumOptions::into_rust).transpose()?;
    match self.inner.as_mut() {
      Some(DatabaseInner::SingleFile(db)) => db
        .vacuum_single_file(options)
        .map_err(|e| Error::from_reason(format!("Failed to vacuum: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Vacuum a single-file database to reclaim free space
  #[napi(js_name = "vacuumSingleFile")]
  pub fn vacuum_single_file(&mut self, options: Option<VacuumOptions>) -> Result<()> {
    self.vacuum(options)
  }

  /// Resize the WAL region (single-file only)
  #[napi(js_name = "resizeWal")]
  pub fn resize_wal(&mut self, size_bytes: i64, options: Option<ResizeWalOptions>) -> Result<()> {
    let size_bytes = validation::resize_wal_size("sizeBytes", size_bytes)?;

    match self.inner.as_mut() {
      Some(DatabaseInner::SingleFile(db)) => db
        .resize_wal(size_bytes, options.map(Into::into))
        .map_err(|e| Error::from_reason(format!("Failed to resize WAL: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get database statistics
  #[napi]
  pub fn stats(&self) -> Result<DbStats> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        let s = db.stats();
        Ok(DbStats {
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
        })
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Check database integrity
  #[napi]
  pub fn check(&self) -> Result<CheckResult> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(CheckResult::from(db.check())),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  // ========================================================================
  // Export / Import
  // ========================================================================

  /// Export database to a JSON object
  #[napi]
  pub fn export_to_object(&self, options: Option<ExportOptions>) -> Result<serde_json::Value> {
    let opts = options.unwrap_or(ExportOptions {
      include_nodes: None,
      include_edges: None,
      include_schema: None,
      pretty: None,
    });
    let opts = opts.into_rust();

    let data = match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => ray_export::export_to_object_single(db, opts)
        .map_err(|e| Error::from_reason(e.to_string()))?,
      None => return Err(Error::from_reason("Database is closed")),
    };

    serde_json::to_value(data).map_err(|e| Error::from_reason(e.to_string()))
  }

  /// Export database to a JSON file
  #[napi]
  pub fn export_to_json(
    &self,
    path: String,
    options: Option<ExportOptions>,
  ) -> Result<ExportResult> {
    let opts = options.unwrap_or(ExportOptions {
      include_nodes: None,
      include_edges: None,
      include_schema: None,
      pretty: None,
    });
    let rust_opts = opts.into_rust();

    let data = match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        ray_export::export_to_object_single(db, rust_opts.clone())
          .map_err(|e| Error::from_reason(e.to_string()))?
      }
      None => return Err(Error::from_reason("Database is closed")),
    };

    let result = ray_export::export_to_json(&data, path, rust_opts.pretty)
      .map_err(|e| Error::from_reason(e.to_string()))?;
    Ok(ExportResult {
      node_count: result.node_count as i64,
      edge_count: result.edge_count as i64,
    })
  }

  /// Export database to JSONL
  #[napi]
  pub fn export_to_jsonl(
    &self,
    path: String,
    options: Option<ExportOptions>,
  ) -> Result<ExportResult> {
    let opts = options.unwrap_or(ExportOptions {
      include_nodes: None,
      include_edges: None,
      include_schema: None,
      pretty: None,
    });
    let rust_opts = opts.into_rust();

    let data = match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => ray_export::export_to_object_single(db, rust_opts)
        .map_err(|e| Error::from_reason(e.to_string()))?,
      None => return Err(Error::from_reason("Database is closed")),
    };

    let result =
      ray_export::export_to_jsonl(&data, path).map_err(|e| Error::from_reason(e.to_string()))?;
    Ok(ExportResult {
      node_count: result.node_count as i64,
      edge_count: result.edge_count as i64,
    })
  }

  /// Import database from a JSON object
  #[napi]
  pub fn import_from_object(
    &self,
    data: serde_json::Value,
    options: Option<ImportOptions>,
  ) -> Result<ImportResult> {
    let opts = options.unwrap_or(ImportOptions {
      skip_existing: None,
      batch_size: None,
    });
    let rust_opts = opts.into_rust()?;
    let parsed: ray_export::ExportedDatabase =
      serde_json::from_value(data).map_err(|e| Error::from_reason(e.to_string()))?;

    let result = match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        ray_export::import_from_object_single(db, &parsed, rust_opts)
          .map_err(|e| Error::from_reason(e.to_string()))?
      }
      None => return Err(Error::from_reason("Database is closed")),
    };

    Ok(ImportResult {
      node_count: result.node_count as i64,
      edge_count: result.edge_count as i64,
      skipped: result.skipped as i64,
    })
  }

  /// Import database from a JSON file
  #[napi]
  pub fn import_from_json(
    &self,
    path: String,
    options: Option<ImportOptions>,
  ) -> Result<ImportResult> {
    let opts = options.unwrap_or(ImportOptions {
      skip_existing: None,
      batch_size: None,
    });
    let rust_opts = opts.into_rust()?;
    let parsed =
      ray_export::import_from_json(path).map_err(|e| Error::from_reason(e.to_string()))?;

    let result = match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        ray_export::import_from_object_single(db, &parsed, rust_opts)
          .map_err(|e| Error::from_reason(e.to_string()))?
      }
      None => return Err(Error::from_reason("Database is closed")),
    };

    Ok(ImportResult {
      node_count: result.node_count as i64,
      edge_count: result.edge_count as i64,
      skipped: result.skipped as i64,
    })
  }

  // ========================================================================
  // Cache Operations
  // ========================================================================

  /// Check if caching is enabled
  #[napi]
  pub fn cache_is_enabled(&self) -> Result<bool> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.cache_is_enabled()),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Invalidate all caches for a node
  #[napi]
  pub fn cache_invalidate_node(&self, node_id: f64) -> Result<()> {
    let node_id = validation::node_id("nodeId", node_id)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        db.cache_invalidate_node(node_id);
        Ok(())
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Invalidate caches for a specific edge
  #[napi]
  pub fn cache_invalidate_edge(&self, src: f64, etype: u32, dst: f64) -> Result<()> {
    let src = validation::node_id("src", src)?;
    let dst = validation::node_id("dst", dst)?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        db.cache_invalidate_edge(src, etype as ETypeId, dst);
        Ok(())
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Invalidate a cached key lookup
  #[napi]
  pub fn cache_invalidate_key(&self, key: String) -> Result<()> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        db.cache_invalidate_key(&key);
        Ok(())
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Clear all caches
  #[napi]
  pub fn cache_clear(&self) -> Result<()> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        db.cache_clear();
        Ok(())
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Clear only the query cache
  #[napi]
  pub fn cache_clear_query(&self) -> Result<()> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        db.cache_clear_query();
        Ok(())
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Clear only the key cache
  #[napi]
  pub fn cache_clear_key(&self) -> Result<()> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        db.cache_clear_key();
        Ok(())
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Clear only the property cache
  #[napi]
  pub fn cache_clear_property(&self) -> Result<()> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        db.cache_clear_property();
        Ok(())
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Clear only the traversal cache
  #[napi]
  pub fn cache_clear_traversal(&self) -> Result<()> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        db.cache_clear_traversal();
        Ok(())
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get cache statistics
  #[napi]
  pub fn cache_stats(&self) -> Result<Option<JsCacheStats>> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.cache_stats().map(|s| JsCacheStats {
        property_cache_hits: s.property_cache_hits as i64,
        property_cache_misses: s.property_cache_misses as i64,
        property_cache_size: s.property_cache_size as i64,
        traversal_cache_hits: s.traversal_cache_hits as i64,
        traversal_cache_misses: s.traversal_cache_misses as i64,
        traversal_cache_size: s.traversal_cache_size as i64,
        query_cache_hits: s.query_cache_hits as i64,
        query_cache_misses: s.query_cache_misses as i64,
        query_cache_size: s.query_cache_size as i64,
      })),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Reset cache statistics
  #[napi]
  pub fn cache_reset_stats(&self) -> Result<()> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        db.cache_reset_stats();
        Ok(())
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  // ========================================================================
  // Internal Helpers
  // ========================================================================

  fn db(&self) -> Result<&RustSingleFileDB> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db),
      None => Err(Error::from_reason("Database is closed")),
    }
  }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Get neighbors from database for traversal
fn neighbors_from_single_file(
  db: &RustSingleFileDB,
  node_id: NodeId,
  direction: TraversalDirection,
  etype: Option<ETypeId>,
) -> Vec<Edge> {
  let mut edges = Vec::new();
  match direction {
    TraversalDirection::Out => {
      for (e, dst) in db.out_edges(node_id) {
        if etype.is_none() || etype == Some(e) {
          edges.push(Edge {
            src: node_id,
            etype: e,
            dst,
          });
        }
      }
    }
    TraversalDirection::In => {
      for (e, src) in db.in_edges(node_id) {
        if etype.is_none() || etype == Some(e) {
          edges.push(Edge {
            src,
            etype: e,
            dst: node_id,
          });
        }
      }
    }
    TraversalDirection::Both => {
      edges.extend(neighbors_from_single_file(
        db,
        node_id,
        TraversalDirection::Out,
        etype,
      ));
      edges.extend(neighbors_from_single_file(
        db,
        node_id,
        TraversalDirection::In,
        etype,
      ));
    }
  }
  edges
}

fn resolve_weight_key_single_file(
  db: &RustSingleFileDB,
  config: &JsPathConfig,
) -> Result<Option<PropKeyId>> {
  if let Some(key_id) = config.weight_key_id {
    return Ok(Some(key_id as PropKeyId));
  }

  if let Some(ref key_name) = config.weight_key_name {
    let key_id = db
      .propkey_id(key_name)
      .ok_or_else(|| Error::from_reason(format!("Unknown property key: {key_name}")))?;
    return Ok(Some(key_id));
  }

  Ok(None)
}

fn prop_value_to_weight(value: Option<PropValue>) -> f64 {
  let weight = match value {
    Some(PropValue::Bool(v)) => {
      if v {
        1.0
      } else {
        0.0
      }
    }
    Some(PropValue::I64(v)) => v as f64,
    Some(PropValue::F64(v)) => v,
    Some(PropValue::String(v)) => v.parse::<f64>().unwrap_or(1.0),
    Some(PropValue::VectorF32(_)) => 1.0,
    Some(PropValue::Null) | None => 1.0,
  };

  if weight.is_finite() && weight > 0.0 {
    weight
  } else {
    1.0
  }
}

fn edge_weight_from_single_file(
  db: &RustSingleFileDB,
  src: NodeId,
  etype: ETypeId,
  dst: NodeId,
  weight_key: Option<PropKeyId>,
) -> f64 {
  match weight_key {
    Some(key_id) => prop_value_to_weight(db.edge_prop(src, etype, dst, key_id)),
    None => 1.0,
  }
}

// ============================================================================
// Convenience Functions
// ============================================================================

/// Open a database file (standalone function)
#[napi]
pub fn open_database(path: String, options: Option<OpenOptions>) -> Result<Database> {
  Database::open(path, options)
}

/// Recommended conservative profile (durability-first).
#[napi]
pub fn recommended_safe_profile() -> RuntimeProfile {
  runtime_profile_from_rust(RustKiteRuntimeProfile::safe())
}

/// Recommended balanced profile (good throughput + durability tradeoff).
#[napi]
pub fn recommended_balanced_profile() -> RuntimeProfile {
  runtime_profile_from_rust(RustKiteRuntimeProfile::balanced())
}

/// Recommended profile for reopen-heavy workloads.
#[napi]
pub fn recommended_reopen_heavy_profile() -> RuntimeProfile {
  runtime_profile_from_rust(RustKiteRuntimeProfile::reopen_heavy())
}

// ============================================================================
// Metrics / Health
// ============================================================================

#[napi]
pub fn collect_metrics(db: &Database) -> Result<DatabaseMetrics> {
  match db.inner.as_ref() {
    Some(DatabaseInner::SingleFile(db)) => Ok(core_metrics::collect_metrics_single_file(db).into()),
    None => Err(Error::from_reason("Database is closed")),
  }
}

#[napi]
pub fn collect_replication_metrics_prometheus(db: &Database) -> Result<String> {
  match db.inner.as_ref() {
    Some(DatabaseInner::SingleFile(db)) => {
      Ok(core_metrics::collect_replication_metrics_prometheus_single_file(db))
    }
    None => Err(Error::from_reason("Database is closed")),
  }
}

#[napi]
pub fn collect_replication_metrics_otel_json(db: &Database) -> Result<String> {
  match db.inner.as_ref() {
    Some(DatabaseInner::SingleFile(db)) => {
      Ok(core_metrics::collect_replication_metrics_otel_json_single_file(db))
    }
    None => Err(Error::from_reason("Database is closed")),
  }
}

#[napi]
pub fn collect_replication_metrics_otel_protobuf(db: &Database) -> Result<Buffer> {
  match db.inner.as_ref() {
    Some(DatabaseInner::SingleFile(db)) => {
      Ok(core_metrics::collect_replication_metrics_otel_protobuf_single_file(db).into())
    }
    None => Err(Error::from_reason("Database is closed")),
  }
}

#[napi]
pub fn collect_replication_snapshot_transport_json(
  db: &Database,
  include_data: Option<bool>,
) -> Result<String> {
  match db.inner.as_ref() {
    Some(DatabaseInner::SingleFile(db)) => db
      .primary_export_snapshot_transport_json(include_data.unwrap_or(false))
      .map_err(|e| Error::from_reason(format!("Failed to export replication snapshot: {e}"))),
    None => Err(Error::from_reason("Database is closed")),
  }
}

#[napi]
pub fn collect_replication_log_transport_json(
  db: &Database,
  cursor: Option<String>,
  max_frames: Option<i64>,
  max_bytes: Option<i64>,
  include_payload: Option<bool>,
) -> Result<String> {
  let max_frames = max_frames.unwrap_or(128);
  let max_bytes = max_bytes.unwrap_or(1_048_576);
  let max_frames = validation::positive_usize("maxFrames", max_frames, validation::MAX_COUNT)?;
  let max_bytes = validation::positive_usize("maxBytes", max_bytes, validation::MAX_BYTES)?;

  match db.inner.as_ref() {
    Some(DatabaseInner::SingleFile(db)) => db
      .primary_export_log_transport_json(
        cursor.as_deref(),
        max_frames,
        max_bytes,
        include_payload.unwrap_or(true),
      )
      .map_err(|e| Error::from_reason(format!("Failed to export replication log: {e}"))),
    None => Err(Error::from_reason("Database is closed")),
  }
}

#[napi]
pub fn push_replication_metrics_otel_json(
  db: &Database,
  endpoint: String,
  timeout_ms: i64,
  bearer_token: Option<String>,
) -> Result<OtlpHttpExportResult> {
  let timeout_ms =
    validation::positive_u64("timeoutMs", timeout_ms, validation::MAX_DURATION_MS as u64)?;

  match db.inner.as_ref() {
    Some(DatabaseInner::SingleFile(db)) => {
      core_metrics::push_replication_metrics_otel_json_single_file(
        db,
        &endpoint,
        timeout_ms,
        bearer_token.as_deref(),
      )
      .map(Into::into)
      .map_err(|e| Error::from_reason(format!("Failed to push replication metrics: {e}")))
    }
    None => Err(Error::from_reason("Database is closed")),
  }
}

fn build_core_otel_push_options(
  options: PushReplicationMetricsOtelOptions,
) -> Result<core_metrics::OtlpHttpPushOptions> {
  let timeout_ms = validation::positive_u64(
    "timeoutMs",
    options.timeout_ms.unwrap_or(5_000),
    validation::MAX_DURATION_MS as u64,
  )?;
  let retry_max_attempts = validation::positive_u32(
    "retryMaxAttempts",
    options.retry_max_attempts.unwrap_or(1),
    validation::MAX_COUNT,
  )?;
  let retry_backoff_ms = validation::non_negative_u64(
    "retryBackoffMs",
    options.retry_backoff_ms.unwrap_or(100),
    validation::MAX_DURATION_MS as u64,
  )?;
  let retry_backoff_max_ms = validation::non_negative_u64(
    "retryBackoffMaxMs",
    options.retry_backoff_max_ms.unwrap_or(2_000),
    validation::MAX_DURATION_MS as u64,
  )?;
  if retry_backoff_max_ms > 0 && retry_backoff_max_ms < retry_backoff_ms {
    return Err(validation::invalid_argument(
      "retryBackoffMaxMs must be >= retryBackoffMs when non-zero",
    ));
  }
  let retry_jitter_ratio = validation::ratio(
    "retryJitterRatio",
    options.retry_jitter_ratio.unwrap_or(0.0),
  )?;
  let adaptive_retry_mode = match options
    .adaptive_retry_mode
    .as_deref()
    .map(str::trim)
    .filter(|value| !value.is_empty())
  {
    None => core_metrics::OtlpAdaptiveRetryMode::Linear,
    Some(value) if value.eq_ignore_ascii_case("linear") => {
      core_metrics::OtlpAdaptiveRetryMode::Linear
    }
    Some(value) if value.eq_ignore_ascii_case("ewma") => core_metrics::OtlpAdaptiveRetryMode::Ewma,
    Some(_) => {
      return Err(Error::from_reason(
        "adaptiveRetryMode must be one of: linear, ewma",
      ));
    }
  };
  let adaptive_retry_ewma_alpha = validation::ratio(
    "adaptiveRetryEwmaAlpha",
    options.adaptive_retry_ewma_alpha.unwrap_or(0.3),
  )?;
  let circuit_breaker_failure_threshold = validation::non_negative_u32(
    "circuitBreakerFailureThreshold",
    options.circuit_breaker_failure_threshold.unwrap_or(0),
    validation::MAX_COUNT,
  )?;
  let circuit_breaker_open_ms = validation::non_negative_u64(
    "circuitBreakerOpenMs",
    options.circuit_breaker_open_ms.unwrap_or(0),
    validation::MAX_DURATION_MS as u64,
  )?;
  if circuit_breaker_failure_threshold > 0 && circuit_breaker_open_ms == 0 {
    return Err(validation::invalid_argument(
      "circuitBreakerOpenMs must be positive when circuitBreakerFailureThreshold is set",
    ));
  }
  let circuit_breaker_half_open_probes = validation::non_negative_u32(
    "circuitBreakerHalfOpenProbes",
    options.circuit_breaker_half_open_probes.unwrap_or(1),
    validation::MAX_COUNT,
  )?;
  if circuit_breaker_failure_threshold > 0 && circuit_breaker_half_open_probes == 0 {
    return Err(validation::invalid_argument(
      "circuitBreakerHalfOpenProbes must be positive when circuitBreakerFailureThreshold is set",
    ));
  }
  if let Some(path) = options.circuit_breaker_state_path.as_deref() {
    if path.trim().is_empty() {
      return Err(Error::from_reason(
        "circuitBreakerStatePath must not be empty when provided",
      ));
    }
  }
  if let Some(url) = options.circuit_breaker_state_url.as_deref() {
    let trimmed = url.trim();
    if trimmed.is_empty() {
      return Err(Error::from_reason(
        "circuitBreakerStateUrl must not be empty when provided",
      ));
    }
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
      return Err(Error::from_reason(
        "circuitBreakerStateUrl must use http:// or https://",
      ));
    }
    if options.https_only.unwrap_or(false) && trimmed.starts_with("http://") {
      return Err(Error::from_reason(
        "circuitBreakerStateUrl must use https when httpsOnly is enabled",
      ));
    }
  }
  if options.circuit_breaker_state_path.is_some() && options.circuit_breaker_state_url.is_some() {
    return Err(Error::from_reason(
      "circuitBreakerStatePath and circuitBreakerStateUrl are mutually exclusive",
    ));
  }
  if options.circuit_breaker_state_patch.unwrap_or(false)
    && options.circuit_breaker_state_url.is_none()
  {
    return Err(Error::from_reason(
      "circuitBreakerStatePatch requires circuitBreakerStateUrl",
    ));
  }
  if options.circuit_breaker_state_patch_batch.unwrap_or(false)
    && !options.circuit_breaker_state_patch.unwrap_or(false)
  {
    return Err(Error::from_reason(
      "circuitBreakerStatePatchBatch requires circuitBreakerStatePatch",
    ));
  }
  if options.circuit_breaker_state_patch_merge.unwrap_or(false)
    && !options.circuit_breaker_state_patch.unwrap_or(false)
  {
    return Err(Error::from_reason(
      "circuitBreakerStatePatchMerge requires circuitBreakerStatePatch",
    ));
  }
  let circuit_breaker_state_patch_batch_max_keys = validation::positive_u32(
    "circuitBreakerStatePatchBatchMaxKeys",
    options
      .circuit_breaker_state_patch_batch_max_keys
      .unwrap_or(8),
    validation::MAX_COUNT,
  )?;
  let circuit_breaker_state_patch_merge_max_keys = validation::positive_u32(
    "circuitBreakerStatePatchMergeMaxKeys",
    options
      .circuit_breaker_state_patch_merge_max_keys
      .unwrap_or(32),
    validation::MAX_COUNT,
  )?;
  let circuit_breaker_state_patch_retry_max_attempts = validation::positive_u32(
    "circuitBreakerStatePatchRetryMaxAttempts",
    options
      .circuit_breaker_state_patch_retry_max_attempts
      .unwrap_or(1),
    validation::MAX_COUNT,
  )?;
  if options.circuit_breaker_state_cas.unwrap_or(false)
    && options.circuit_breaker_state_url.is_none()
  {
    return Err(Error::from_reason(
      "circuitBreakerStateCas requires circuitBreakerStateUrl",
    ));
  }
  if let Some(lease_id) = options.circuit_breaker_state_lease_id.as_deref() {
    if lease_id.trim().is_empty() {
      return Err(Error::from_reason(
        "circuitBreakerStateLeaseId must not be empty when provided",
      ));
    }
    if options.circuit_breaker_state_url.is_none() {
      return Err(Error::from_reason(
        "circuitBreakerStateLeaseId requires circuitBreakerStateUrl",
      ));
    }
  }
  if let Some(scope_key) = options.circuit_breaker_scope_key.as_deref() {
    if scope_key.trim().is_empty() {
      return Err(Error::from_reason(
        "circuitBreakerScopeKey must not be empty when provided",
      ));
    }
  }

  Ok(core_metrics::OtlpHttpPushOptions {
    timeout_ms,
    bearer_token: options.bearer_token,
    retry_max_attempts,
    retry_backoff_ms,
    retry_backoff_max_ms,
    retry_jitter_ratio,
    adaptive_retry_mode,
    adaptive_retry_ewma_alpha,
    adaptive_retry: options.adaptive_retry.unwrap_or(false),
    circuit_breaker_failure_threshold,
    circuit_breaker_open_ms,
    circuit_breaker_half_open_probes,
    circuit_breaker_state_path: options.circuit_breaker_state_path,
    circuit_breaker_state_url: options.circuit_breaker_state_url,
    circuit_breaker_state_patch: options.circuit_breaker_state_patch.unwrap_or(false),
    circuit_breaker_state_patch_batch: options.circuit_breaker_state_patch_batch.unwrap_or(false),
    circuit_breaker_state_patch_batch_max_keys,
    circuit_breaker_state_patch_merge: options.circuit_breaker_state_patch_merge.unwrap_or(false),
    circuit_breaker_state_patch_merge_max_keys,
    circuit_breaker_state_patch_retry_max_attempts,
    circuit_breaker_state_cas: options.circuit_breaker_state_cas.unwrap_or(false),
    circuit_breaker_state_lease_id: options.circuit_breaker_state_lease_id,
    circuit_breaker_scope_key: options.circuit_breaker_scope_key,
    compression_gzip: options.compression_gzip.unwrap_or(false),
    tls: core_metrics::OtlpHttpTlsOptions {
      https_only: options.https_only.unwrap_or(false),
      ca_cert_pem_path: options.ca_cert_pem_path,
      client_cert_pem_path: options.client_cert_pem_path,
      client_key_pem_path: options.client_key_pem_path,
    },
  })
}

#[napi]
pub fn push_replication_metrics_otel_json_with_options(
  db: &Database,
  endpoint: String,
  options: Option<PushReplicationMetricsOtelOptions>,
) -> Result<OtlpHttpExportResult> {
  let core_options = build_core_otel_push_options(options.unwrap_or_default())?;

  match db.inner.as_ref() {
    Some(DatabaseInner::SingleFile(db)) => {
      core_metrics::push_replication_metrics_otel_json_single_file_with_options(
        db,
        &endpoint,
        &core_options,
      )
      .map(Into::into)
      .map_err(|e| Error::from_reason(format!("Failed to push replication metrics: {e}")))
    }
    None => Err(Error::from_reason("Database is closed")),
  }
}

#[napi]
pub fn push_replication_metrics_otel_protobuf(
  db: &Database,
  endpoint: String,
  timeout_ms: i64,
  bearer_token: Option<String>,
) -> Result<OtlpHttpExportResult> {
  let timeout_ms =
    validation::positive_u64("timeoutMs", timeout_ms, validation::MAX_DURATION_MS as u64)?;

  match db.inner.as_ref() {
    Some(DatabaseInner::SingleFile(db)) => {
      core_metrics::push_replication_metrics_otel_protobuf_single_file(
        db,
        &endpoint,
        timeout_ms,
        bearer_token.as_deref(),
      )
      .map(Into::into)
      .map_err(|e| Error::from_reason(format!("Failed to push replication metrics: {e}")))
    }
    None => Err(Error::from_reason("Database is closed")),
  }
}

#[napi]
pub fn push_replication_metrics_otel_protobuf_with_options(
  db: &Database,
  endpoint: String,
  options: Option<PushReplicationMetricsOtelOptions>,
) -> Result<OtlpHttpExportResult> {
  let core_options = build_core_otel_push_options(options.unwrap_or_default())?;

  match db.inner.as_ref() {
    Some(DatabaseInner::SingleFile(db)) => {
      core_metrics::push_replication_metrics_otel_protobuf_single_file_with_options(
        db,
        &endpoint,
        &core_options,
      )
      .map(Into::into)
      .map_err(|e| Error::from_reason(format!("Failed to push replication metrics: {e}")))
    }
    None => Err(Error::from_reason("Database is closed")),
  }
}

#[napi]
pub fn push_replication_metrics_otel_grpc(
  db: &Database,
  endpoint: String,
  timeout_ms: i64,
  bearer_token: Option<String>,
) -> Result<OtlpHttpExportResult> {
  let timeout_ms =
    validation::positive_u64("timeoutMs", timeout_ms, validation::MAX_DURATION_MS as u64)?;

  match db.inner.as_ref() {
    Some(DatabaseInner::SingleFile(db)) => {
      core_metrics::push_replication_metrics_otel_grpc_single_file(
        db,
        &endpoint,
        timeout_ms,
        bearer_token.as_deref(),
      )
      .map(Into::into)
      .map_err(|e| Error::from_reason(format!("Failed to push replication metrics: {e}")))
    }
    None => Err(Error::from_reason("Database is closed")),
  }
}

#[napi]
pub fn push_replication_metrics_otel_grpc_with_options(
  db: &Database,
  endpoint: String,
  options: Option<PushReplicationMetricsOtelOptions>,
) -> Result<OtlpHttpExportResult> {
  let core_options = build_core_otel_push_options(options.unwrap_or_default())?;

  match db.inner.as_ref() {
    Some(DatabaseInner::SingleFile(db)) => {
      core_metrics::push_replication_metrics_otel_grpc_single_file_with_options(
        db,
        &endpoint,
        &core_options,
      )
      .map(Into::into)
      .map_err(|e| Error::from_reason(format!("Failed to push replication metrics: {e}")))
    }
    None => Err(Error::from_reason("Database is closed")),
  }
}

#[napi]
pub fn health_check(db: &Database) -> Result<HealthCheckResult> {
  match db.inner.as_ref() {
    Some(DatabaseInner::SingleFile(db)) => Ok(core_metrics::health_check_single_file(db).into()),
    None => Err(Error::from_reason("Database is closed")),
  }
}

// ============================================================================
// Backup / Restore
// ============================================================================

/// Options for creating a backup
#[napi(object)]
#[derive(Default, Clone)]
pub struct BackupOptions {
  /// Force a checkpoint before backup (single-file only)
  pub checkpoint: Option<bool>,
  /// Overwrite existing backup if it exists
  pub overwrite: Option<bool>,
}

/// Options for restoring a backup
#[napi(object)]
#[derive(Default, Clone)]
pub struct RestoreOptions {
  /// Overwrite existing database if it exists
  pub overwrite: Option<bool>,
}

/// Options for offline backup
#[napi(object)]
#[derive(Default, Clone)]
pub struct OfflineBackupOptions {
  /// Overwrite existing backup if it exists
  pub overwrite: Option<bool>,
}

/// Backup result
#[napi(object)]
pub struct BackupResult {
  /// Backup path
  pub path: String,
  /// Size in bytes
  pub size: i64,
  /// Timestamp in milliseconds since epoch
  pub timestamp: i64,
  /// Backup type ("single-file")
  pub r#type: String,
}

impl From<BackupOptions> for core_backup::BackupOptions {
  fn from(options: BackupOptions) -> Self {
    Self {
      checkpoint: options.checkpoint.unwrap_or(true),
      overwrite: options.overwrite.unwrap_or(false),
    }
  }
}

impl From<RestoreOptions> for core_backup::RestoreOptions {
  fn from(options: RestoreOptions) -> Self {
    Self {
      overwrite: options.overwrite.unwrap_or(false),
    }
  }
}

impl From<OfflineBackupOptions> for core_backup::OfflineBackupOptions {
  fn from(options: OfflineBackupOptions) -> Self {
    Self {
      overwrite: options.overwrite.unwrap_or(false),
    }
  }
}

impl From<core_backup::BackupResult> for BackupResult {
  fn from(result: core_backup::BackupResult) -> Self {
    BackupResult {
      path: result.path,
      size: result.size as i64,
      timestamp: result.timestamp_ms as i64,
      r#type: result.kind,
    }
  }
}

/// Create a backup from an open database handle
#[napi]
pub fn create_backup(
  db: &Database,
  backup_path: String,
  options: Option<BackupOptions>,
) -> Result<BackupResult> {
  let options = options.unwrap_or_default();
  let core_options: core_backup::BackupOptions = options.clone().into();
  let backup_path = PathBuf::from(backup_path);

  match db.inner.as_ref() {
    Some(DatabaseInner::SingleFile(db)) => {
      core_backup::create_backup_single_file(db, &backup_path, core_options)
        .map(BackupResult::from)
        .map_err(|e| Error::from_reason(format!("Failed to create backup: {e}")))
    }
    None => Err(Error::from_reason("Database is closed")),
  }
}

/// Restore a backup into a target path
#[napi]
pub fn restore_backup(
  backup_path: String,
  restore_path: String,
  options: Option<RestoreOptions>,
) -> Result<String> {
  let options = options.unwrap_or_default();
  let core_options: core_backup::RestoreOptions = options.into();

  core_backup::restore_backup(backup_path, restore_path, core_options)
    .map(|p| p.to_string_lossy().to_string())
    .map_err(|e| Error::from_reason(format!("Failed to restore backup: {e}")))
}

/// Inspect a backup without restoring it
#[napi]
pub fn backup_info(backup_path: String) -> Result<BackupResult> {
  core_backup::backup_info(backup_path)
    .map(BackupResult::from)
    .map_err(|e| Error::from_reason(format!("Failed to inspect backup: {e}")))
}

/// Create a backup from a database path without opening it
#[napi]
pub fn create_offline_backup(
  db_path: String,
  backup_path: String,
  options: Option<OfflineBackupOptions>,
) -> Result<BackupResult> {
  let options = options.unwrap_or_default();
  let core_options: core_backup::OfflineBackupOptions = options.into();

  core_backup::create_offline_backup(db_path, backup_path, core_options)
    .map(BackupResult::from)
    .map_err(|e| Error::from_reason(format!("Failed to create offline backup: {e}")))
}
