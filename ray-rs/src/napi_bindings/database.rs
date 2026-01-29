//! NAPI bindings for SingleFileDB
//!
//! Provides Node.js/Bun access to the single-file database format.

use napi::bindgen_prelude::*;
use napi_derive::napi;
use parking_lot::Mutex;
use std::path::PathBuf;

use super::traversal::{
  JsPathConfig, JsPathResult, JsTraversalDirection, JsTraversalResult, JsTraversalStep,
  JsTraverseOptions,
};
use crate::api::pathfinding::{bfs, dijkstra, yen_k_shortest, PathConfig};
use crate::api::traversal::{
  TraversalBuilder as RustTraversalBuilder, TraversalDirection, TraverseOptions,
};
use crate::backup as core_backup;
use crate::core::single_file::{
  close_single_file, is_single_file_path, open_single_file, SingleFileDB as RustSingleFileDB,
  SingleFileOpenOptions as RustOpenOptions, SyncMode as RustSyncMode,
  SingleFileOptimizeOptions as RustSingleFileOptimizeOptions, VacuumOptions as RustVacuumOptions,
};
use crate::export as ray_export;
use crate::graph::db::{
  close_graph_db, open_graph_db as open_multi_file, GraphDB as RustGraphDB,
  OpenOptions as GraphOpenOptions,
};
use crate::graph::definitions::define_label as graph_define_label;
use crate::graph::edges::{
  add_edge as graph_add_edge, del_edge_prop as graph_del_edge_prop,
  delete_edge as graph_delete_edge, edge_exists_db, get_edge_prop_db, get_edge_props_db,
  set_edge_prop as graph_set_edge_prop,
};
use crate::graph::iterators::{
  count_edges as graph_count_edges, count_nodes as graph_count_nodes,
  list_edges as graph_list_edges, list_in_edges, list_nodes as graph_list_nodes, list_out_edges,
  ListEdgesOptions,
};
use crate::graph::key_index::get_node_key as graph_get_node_key;
use crate::graph::nodes::{
  add_node_label as graph_add_node_label, create_node as graph_create_node,
  del_node_prop as graph_del_node_prop, delete_node as graph_delete_node, get_node_by_key_db,
  get_node_labels_db, get_node_prop_db, get_node_props_db, node_exists_db, node_has_label_db,
  remove_node_label as graph_remove_node_label, set_node_prop as graph_set_node_prop, NodeOpts,
};
use crate::graph::tx::{
  begin_read_tx as graph_begin_read_tx, begin_tx as graph_begin_tx, commit as graph_commit,
  rollback as graph_rollback, TxHandle as GraphTxHandle,
};
use crate::graph::vectors::{
  delete_node_vector as graph_delete_node_vector, get_node_vector_db as graph_get_node_vector_db,
  has_node_vector_db as graph_has_node_vector_db, set_node_vector as graph_set_node_vector,
};
use crate::metrics as core_metrics;
use crate::streaming;
use crate::types::{
  CheckResult as RustCheckResult, ETypeId, Edge, NodeId, PropKeyId, PropValue,
  TxState as GraphTxState,
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
  /// Acquire file lock (multi-file only)
  pub lock_file: Option<bool>,
  /// Require locking support (multi-file only)
  pub require_locking: Option<bool>,
  /// Enable MVCC (multi-file only)
  pub mvcc: Option<bool>,
  /// MVCC GC interval in ms (multi-file only)
  pub mvcc_gc_interval_ms: Option<i64>,
  /// MVCC retention in ms (multi-file only)
  pub mvcc_retention_ms: Option<i64>,
  /// MVCC max version chain depth (multi-file only)
  pub mvcc_max_chain_depth: Option<u32>,
  /// Page size in bytes (default 4096)
  pub page_size: Option<u32>,
  /// WAL size in bytes (default 1MB)
  pub wal_size: Option<u32>,
  /// Enable auto-checkpoint when WAL usage exceeds threshold
  pub auto_checkpoint: Option<bool>,
  /// WAL usage threshold (0.0-1.0) to trigger auto-checkpoint
  pub checkpoint_threshold: Option<f64>,
  /// Use background (non-blocking) checkpoint
  pub background_checkpoint: Option<bool>,
  /// Enable caching
  pub cache_enabled: Option<bool>,
  /// Max node properties in cache
  pub cache_max_node_props: Option<i64>,
  /// Max edge properties in cache
  pub cache_max_edge_props: Option<i64>,
  /// Max traversal cache entries
  pub cache_max_traversal_entries: Option<i64>,
  /// Max query cache entries
  pub cache_max_query_entries: Option<i64>,
  /// Query cache TTL in milliseconds
  pub cache_query_ttl_ms: Option<i64>,
  /// Sync mode: "Full", "Normal", or "Off" (default: "Full")
  pub sync_mode: Option<JsSyncMode>,
}

impl From<OpenOptions> for RustOpenOptions {
  fn from(opts: OpenOptions) -> Self {
    use crate::types::{CacheOptions, PropertyCacheConfig, QueryCacheConfig, TraversalCacheConfig};

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
    if let Some(mode) = opts.sync_mode {
      rust_opts = rust_opts.sync_mode(mode.into());
    }

    rust_opts
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

impl From<VacuumOptions> for RustVacuumOptions {
  fn from(opts: VacuumOptions) -> Self {
    let min_wal_size = opts
      .min_wal_size
      .and_then(|v| if v >= 0 { Some(v as u64) } else { None });
    Self {
      shrink_wal: opts.shrink_wal.unwrap_or(true),
      min_wal_size,
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

impl From<CompressionOptions> for CoreCompressionOptions {
  fn from(opts: CompressionOptions) -> Self {
    let mut out = CoreCompressionOptions::default();
    if let Some(enabled) = opts.enabled {
      out.enabled = enabled;
    }
    if let Some(t) = opts.r#type {
      out.compression_type = t.into();
    }
    if let Some(min_size) = opts.min_size {
      out.min_size = min_size as usize;
    }
    if let Some(level) = opts.level {
      out.level = level;
    }
    out
  }
}

/// Options for optimizing a single-file database
#[napi(object)]
#[derive(Debug, Default)]
pub struct SingleFileOptimizeOptions {
  /// Compression options for the new snapshot
  pub compression: Option<CompressionOptions>,
}

impl From<SingleFileOptimizeOptions> for RustSingleFileOptimizeOptions {
  fn from(opts: SingleFileOptimizeOptions) -> Self {
    RustSingleFileOptimizeOptions {
      compression: opts.compression.map(Into::into),
    }
  }
}

impl OpenOptions {
  fn to_graph_options(&self) -> GraphOpenOptions {
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
  pub batch_size: Option<i64>,
}

impl ImportOptions {
  fn into_rust(self) -> ray_export::ImportOptions {
    let mut opts = ray_export::ImportOptions::default();
    if let Some(v) = self.skip_existing {
      opts.skip_existing = v;
    }
    if let Some(v) = self.batch_size {
      if v > 0 {
        opts.batch_size = v as usize;
      }
    }
    opts
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
  /// Number of items per batch (default: 1000)
  pub batch_size: Option<i64>,
}

impl StreamOptions {
  fn into_rust(self) -> Result<crate::streaming::StreamOptions> {
    let batch_size = self.batch_size.unwrap_or(0);
    if batch_size < 0 {
      return Err(Error::from_reason("batchSize must be non-negative"));
    }
    Ok(crate::streaming::StreamOptions {
      batch_size: batch_size as usize,
    })
  }
}

/// Options for cursor-based pagination
#[napi(object)]
#[derive(Debug, Default)]
pub struct PaginationOptions {
  /// Number of items per page (default: 100)
  pub limit: Option<i64>,
  /// Cursor from previous page
  pub cursor: Option<String>,
}

impl PaginationOptions {
  fn into_rust(self) -> Result<crate::streaming::PaginationOptions> {
    let limit = self.limit.unwrap_or(0);
    if limit < 0 {
      return Err(Error::from_reason("limit must be non-negative"));
    }
    Ok(crate::streaming::PaginationOptions {
      limit: limit as usize,
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
  pub src: i64,
  pub etype: u32,
  pub dst: i64,
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
// Database NAPI Wrapper (single-file + multi-file)
// ============================================================================

#[allow(clippy::large_enum_variant)]
enum DatabaseInner {
  SingleFile(RustSingleFileDB),
  Graph(RustGraphDB),
}

/// Graph database handle (single-file or multi-file)
#[napi]
pub struct Database {
  inner: Option<DatabaseInner>,
  graph_tx: Mutex<Option<GraphTxState>>, // Only used for multi-file GraphDB
}

#[napi]
impl Database {
  /// Open a database file
  #[napi(factory)]
  pub fn open(path: String, options: Option<OpenOptions>) -> Result<Database> {
    let options = options.unwrap_or_default();
    let path_buf = PathBuf::from(&path);

    if path_buf.exists() && path_buf.is_dir() {
      let graph_opts = options.to_graph_options();
      let db = open_multi_file(&path_buf, graph_opts)
        .map_err(|e| Error::from_reason(format!("Failed to open database: {e}")))?;
      return Ok(Database {
        inner: Some(DatabaseInner::Graph(db)),
        graph_tx: Mutex::new(None),
      });
    }

    let mut db_path = path_buf;
    if !is_single_file_path(&db_path) {
      db_path = PathBuf::from(format!("{path}.raydb"));
    }

    let opts: RustOpenOptions = options.into();
    let db = open_single_file(&db_path, opts)
      .map_err(|e| Error::from_reason(format!("Failed to open database: {e}")))?;
    Ok(Database {
      inner: Some(DatabaseInner::SingleFile(db)),
      graph_tx: Mutex::new(None),
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
        DatabaseInner::Graph(db) => {
          close_graph_db(db)
            .map_err(|e| Error::from_reason(format!("Failed to close database: {e}")))?;
        }
      }
    }
    self.graph_tx.lock().take();
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
      Some(DatabaseInner::Graph(db)) => Ok(db.path.to_string_lossy().to_string()),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Check if database is read-only
  #[napi(getter)]
  pub fn read_only(&self) -> Result<bool> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.read_only),
      Some(DatabaseInner::Graph(db)) => Ok(db.read_only),
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
      Some(DatabaseInner::Graph(db)) => {
        let mut guard = self.graph_tx.lock();
        if guard.is_some() {
          return Err(Error::from_reason("Transaction already active"));
        }

        let handle = if read_only {
          graph_begin_read_tx(db)
            .map_err(|e| Error::from_reason(format!("Failed to begin transaction: {e}")))?
        } else {
          graph_begin_tx(db)
            .map_err(|e| Error::from_reason(format!("Failed to begin transaction: {e}")))?
        };
        let txid = handle.tx.txid as i64;
        *guard = Some(handle.tx);
        Ok(txid)
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
      Some(DatabaseInner::Graph(db)) => {
        let mut guard = self.graph_tx.lock();
        let tx_state = guard
          .take()
          .ok_or_else(|| Error::from_reason("No active transaction"))?;
        let mut handle = GraphTxHandle::new(db, tx_state);
        graph_commit(&mut handle)
          .map_err(|e| Error::from_reason(format!("Failed to commit: {e}")))?;
        Ok(())
      }
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
      Some(DatabaseInner::Graph(db)) => {
        let mut guard = self.graph_tx.lock();
        let tx_state = guard
          .take()
          .ok_or_else(|| Error::from_reason("No active transaction"))?;
        let mut handle = GraphTxHandle::new(db, tx_state);
        graph_rollback(&mut handle)
          .map_err(|e| Error::from_reason(format!("Failed to rollback: {e}")))?;
        Ok(())
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Check if there's an active transaction
  #[napi]
  pub fn has_transaction(&self) -> Result<bool> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.has_transaction()),
      Some(DatabaseInner::Graph(_)) => Ok(self.graph_tx.lock().is_some()),
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
      Some(DatabaseInner::Graph(_)) => self.with_graph_tx(|handle| {
        let mut opts = NodeOpts::new();
        if let Some(key) = key {
          opts = opts.with_key(key);
        }
        let node_id = graph_create_node(handle, opts)
          .map_err(|e| Error::from_reason(format!("Failed to create node: {e}")))?;
        Ok(node_id as i64)
      }),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Delete a node
  #[napi]
  pub fn delete_node(&self, node_id: i64) -> Result<()> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .delete_node(node_id as NodeId)
        .map_err(|e| Error::from_reason(format!("Failed to delete node: {e}"))),
      Some(DatabaseInner::Graph(_)) => self.with_graph_tx(|handle| {
        graph_delete_node(handle, node_id as NodeId)
          .map_err(|e| Error::from_reason(format!("Failed to delete node: {e}")))?;
        Ok(())
      }),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Check if a node exists
  #[napi]
  pub fn node_exists(&self, node_id: i64) -> Result<bool> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.node_exists(node_id as NodeId)),
      Some(DatabaseInner::Graph(db)) => Ok(node_exists_db(db, node_id as NodeId)),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get node by key
  #[napi]
  pub fn get_node_by_key(&self, key: String) -> Result<Option<i64>> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.get_node_by_key(&key).map(|id| id as i64)),
      Some(DatabaseInner::Graph(db)) => Ok(get_node_by_key_db(db, &key).map(|id| id as i64)),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get the key for a node
  #[napi]
  pub fn get_node_key(&self, node_id: i64) -> Result<Option<String>> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.get_node_key(node_id as NodeId)),
      Some(DatabaseInner::Graph(db)) => {
        let delta = db.delta.read();
        Ok(graph_get_node_key(
          db.snapshot.as_ref(),
          &delta,
          node_id as NodeId,
        ))
      }
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
      Some(DatabaseInner::Graph(db)) => Ok(
        graph_list_nodes(db)
          .into_iter()
          .map(|id| id as i64)
          .collect(),
      ),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Count all nodes
  #[napi]
  pub fn count_nodes(&self) -> Result<i64> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.count_nodes() as i64),
      Some(DatabaseInner::Graph(db)) => Ok(graph_count_nodes(db) as i64),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  // ========================================================================
  // Edge Operations
  // ========================================================================

  /// Add an edge
  #[napi]
  pub fn add_edge(&self, src: i64, etype: u32, dst: i64) -> Result<()> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .add_edge(src as NodeId, etype as ETypeId, dst as NodeId)
        .map_err(|e| Error::from_reason(format!("Failed to add edge: {e}"))),
      Some(DatabaseInner::Graph(_)) => self.with_graph_tx(|handle| {
        graph_add_edge(handle, src as NodeId, etype as ETypeId, dst as NodeId)
          .map_err(|e| Error::from_reason(format!("Failed to add edge: {e}")))?;
        Ok(())
      }),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Add an edge by type name
  #[napi]
  pub fn add_edge_by_name(&self, src: i64, etype_name: String, dst: i64) -> Result<()> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .add_edge_by_name(src as NodeId, &etype_name, dst as NodeId)
        .map_err(|e| Error::from_reason(format!("Failed to add edge: {e}"))),
      Some(DatabaseInner::Graph(db)) => {
        let etype = db
          .get_etype_id(&etype_name)
          .ok_or_else(|| Error::from_reason(format!("Unknown edge type: {etype_name}")))?;
        self.with_graph_tx(|handle| {
          graph_add_edge(handle, src as NodeId, etype, dst as NodeId)
            .map_err(|e| Error::from_reason(format!("Failed to add edge: {e}")))?;
          Ok(())
        })
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Delete an edge
  #[napi]
  pub fn delete_edge(&self, src: i64, etype: u32, dst: i64) -> Result<()> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .delete_edge(src as NodeId, etype as ETypeId, dst as NodeId)
        .map_err(|e| Error::from_reason(format!("Failed to delete edge: {e}"))),
      Some(DatabaseInner::Graph(_)) => self.with_graph_tx(|handle| {
        graph_delete_edge(handle, src as NodeId, etype as ETypeId, dst as NodeId)
          .map_err(|e| Error::from_reason(format!("Failed to delete edge: {e}")))?;
        Ok(())
      }),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Check if an edge exists
  #[napi]
  pub fn edge_exists(&self, src: i64, etype: u32, dst: i64) -> Result<bool> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        Ok(db.edge_exists(src as NodeId, etype as ETypeId, dst as NodeId))
      }
      Some(DatabaseInner::Graph(db)) => Ok(edge_exists_db(
        db,
        src as NodeId,
        etype as ETypeId,
        dst as NodeId,
      )),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get outgoing edges for a node
  #[napi]
  pub fn get_out_edges(&self, node_id: i64) -> Result<Vec<JsEdge>> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(
        db.get_out_edges(node_id as NodeId)
          .into_iter()
          .map(|(etype, dst)| JsEdge {
            etype,
            node_id: dst as i64,
          })
          .collect(),
      ),
      Some(DatabaseInner::Graph(db)) => Ok(
        list_out_edges(db, node_id as NodeId)
          .into_iter()
          .map(|edge| JsEdge {
            etype: edge.etype,
            node_id: edge.dst as i64,
          })
          .collect(),
      ),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get incoming edges for a node
  #[napi]
  pub fn get_in_edges(&self, node_id: i64) -> Result<Vec<JsEdge>> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(
        db.get_in_edges(node_id as NodeId)
          .into_iter()
          .map(|(etype, src)| JsEdge {
            etype,
            node_id: src as i64,
          })
          .collect(),
      ),
      Some(DatabaseInner::Graph(db)) => Ok(
        list_in_edges(db, node_id as NodeId)
          .into_iter()
          .map(|edge| JsEdge {
            etype: edge.etype,
            node_id: edge.dst as i64,
          })
          .collect(),
      ),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get out-degree for a node
  #[napi]
  pub fn get_out_degree(&self, node_id: i64) -> Result<i64> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.get_out_degree(node_id as NodeId) as i64),
      Some(DatabaseInner::Graph(db)) => Ok(list_out_edges(db, node_id as NodeId).len() as i64),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get in-degree for a node
  #[napi]
  pub fn get_in_degree(&self, node_id: i64) -> Result<i64> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.get_in_degree(node_id as NodeId) as i64),
      Some(DatabaseInner::Graph(db)) => Ok(list_in_edges(db, node_id as NodeId).len() as i64),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Count all edges
  #[napi]
  pub fn count_edges(&self) -> Result<i64> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.count_edges() as i64),
      Some(DatabaseInner::Graph(db)) => Ok(graph_count_edges(db, None) as i64),
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
            src: e.src as i64,
            etype: e.etype,
            dst: e.dst as i64,
          })
          .collect(),
      ),
      Some(DatabaseInner::Graph(db)) => {
        let options = ListEdgesOptions { etype };
        Ok(
          graph_list_edges(db, options)
            .into_iter()
            .map(|e| JsFullEdge {
              src: e.src as i64,
              etype: e.etype,
              dst: e.dst as i64,
            })
            .collect(),
        )
      }
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
          .get_etype_id(&etype_name)
          .ok_or_else(|| Error::from_reason(format!("Unknown edge type: {etype_name}")))?;
        Ok(
          db.list_edges(Some(etype))
            .into_iter()
            .map(|e| JsFullEdge {
              src: e.src as i64,
              etype: e.etype,
              dst: e.dst as i64,
            })
            .collect(),
        )
      }
      Some(DatabaseInner::Graph(db)) => {
        let etype = db
          .get_etype_id(&etype_name)
          .ok_or_else(|| Error::from_reason(format!("Unknown edge type: {etype_name}")))?;
        let options = ListEdgesOptions { etype: Some(etype) };
        Ok(
          graph_list_edges(db, options)
            .into_iter()
            .map(|e| JsFullEdge {
              src: e.src as i64,
              etype: e.etype,
              dst: e.dst as i64,
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
      Some(DatabaseInner::Graph(db)) => Ok(graph_count_edges(db, Some(etype)) as i64),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Count edges by type name
  #[napi]
  pub fn count_edges_by_name(&self, etype_name: String) -> Result<i64> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        let etype = db
          .get_etype_id(&etype_name)
          .ok_or_else(|| Error::from_reason(format!("Unknown edge type: {etype_name}")))?;
        Ok(db.count_edges_by_type(etype) as i64)
      }
      Some(DatabaseInner::Graph(db)) => {
        let etype = db
          .get_etype_id(&etype_name)
          .ok_or_else(|| Error::from_reason(format!("Unknown edge type: {etype_name}")))?;
        Ok(graph_count_edges(db, Some(etype)) as i64)
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
      Some(DatabaseInner::Graph(db)) => Ok(
        streaming::stream_nodes_graph(db, options)
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
                  let key = db.get_node_key(node_id as NodeId);
                  let props = db.get_node_props(node_id as NodeId).unwrap_or_default();
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
      Some(DatabaseInner::Graph(db)) => {
        let batches = streaming::stream_nodes_graph(db, options);
        Ok(
          batches
            .into_iter()
            .map(|batch| {
              batch
                .into_iter()
                .map(|node_id| {
                  let key = {
                    let delta = db.delta.read();
                    graph_get_node_key(db.snapshot.as_ref(), &delta, node_id)
                  };
                  let props = get_node_props_db(db, node_id).unwrap_or_default();
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
                src: edge.src as i64,
                etype: edge.etype,
                dst: edge.dst as i64,
              })
              .collect()
          })
          .collect(),
      ),
      Some(DatabaseInner::Graph(db)) => Ok(
        streaming::stream_edges_graph(db, options)
          .into_iter()
          .map(|batch| {
            batch
              .into_iter()
              .map(|edge| JsFullEdge {
                src: edge.src as i64,
                etype: edge.etype,
                dst: edge.dst as i64,
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
                    .get_edge_props(edge.src, edge.etype, edge.dst)
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
      Some(DatabaseInner::Graph(db)) => {
        let batches = streaming::stream_edges_graph(db, options);
        Ok(
          batches
            .into_iter()
            .map(|batch| {
              batch
                .into_iter()
                .map(|edge| {
                  let props =
                    get_edge_props_db(db, edge.src, edge.etype, edge.dst).unwrap_or_default();
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
  #[napi]
  pub fn get_nodes_page(&self, options: Option<PaginationOptions>) -> Result<NodePage> {
    let options = options.unwrap_or_default().into_rust()?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        let page = streaming::get_nodes_page_single(db, options);
        Ok(NodePage {
          items: page.items.into_iter().map(|id| id as i64).collect(),
          next_cursor: page.next_cursor,
          has_more: page.has_more,
          total: Some(db.count_nodes() as i64),
        })
      }
      Some(DatabaseInner::Graph(db)) => {
        let page = streaming::get_nodes_page_graph(db, options);
        Ok(NodePage {
          items: page.items.into_iter().map(|id| id as i64).collect(),
          next_cursor: page.next_cursor,
          has_more: page.has_more,
          total: Some(graph_count_nodes(db) as i64),
        })
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get a page of edges
  #[napi]
  pub fn get_edges_page(&self, options: Option<PaginationOptions>) -> Result<EdgePage> {
    let options = options.unwrap_or_default().into_rust()?;
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        let page = streaming::get_edges_page_single(db, options);
        Ok(EdgePage {
          items: page
            .items
            .into_iter()
            .map(|edge| JsFullEdge {
              src: edge.src as i64,
              etype: edge.etype,
              dst: edge.dst as i64,
            })
            .collect(),
          next_cursor: page.next_cursor,
          has_more: page.has_more,
          total: Some(db.count_edges() as i64),
        })
      }
      Some(DatabaseInner::Graph(db)) => {
        let page = streaming::get_edges_page_graph(db, options);
        Ok(EdgePage {
          items: page
            .items
            .into_iter()
            .map(|edge| JsFullEdge {
              src: edge.src as i64,
              etype: edge.etype,
              dst: edge.dst as i64,
            })
            .collect(),
          next_cursor: page.next_cursor,
          has_more: page.has_more,
          total: Some(graph_count_edges(db, None) as i64),
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
  pub fn set_node_prop(&self, node_id: i64, key_id: u32, value: JsPropValue) -> Result<()> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .set_node_prop(node_id as NodeId, key_id as PropKeyId, value.into())
        .map_err(|e| Error::from_reason(format!("Failed to set property: {e}"))),
      Some(DatabaseInner::Graph(_)) => self.with_graph_tx(|handle| {
        graph_set_node_prop(handle, node_id as NodeId, key_id as PropKeyId, value.into())
          .map_err(|e| Error::from_reason(format!("Failed to set property: {e}")))?;
        Ok(())
      }),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Set a node property by key name
  #[napi]
  pub fn set_node_prop_by_name(
    &self,
    node_id: i64,
    key_name: String,
    value: JsPropValue,
  ) -> Result<()> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .set_node_prop_by_name(node_id as NodeId, &key_name, value.into())
        .map_err(|e| Error::from_reason(format!("Failed to set property: {e}"))),
      Some(DatabaseInner::Graph(db)) => {
        let key_id = db
          .get_propkey_id(&key_name)
          .ok_or_else(|| Error::from_reason(format!("Unknown property key: {key_name}")))?;
        self.with_graph_tx(|handle| {
          graph_set_node_prop(handle, node_id as NodeId, key_id, value.into())
            .map_err(|e| Error::from_reason(format!("Failed to set property: {e}")))?;
          Ok(())
        })
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Delete a node property
  #[napi]
  pub fn delete_node_prop(&self, node_id: i64, key_id: u32) -> Result<()> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .delete_node_prop(node_id as NodeId, key_id as PropKeyId)
        .map_err(|e| Error::from_reason(format!("Failed to delete property: {e}"))),
      Some(DatabaseInner::Graph(_)) => self.with_graph_tx(|handle| {
        graph_del_node_prop(handle, node_id as NodeId, key_id as PropKeyId)
          .map_err(|e| Error::from_reason(format!("Failed to delete property: {e}")))?;
        Ok(())
      }),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get a specific node property
  #[napi]
  pub fn get_node_prop(&self, node_id: i64, key_id: u32) -> Result<Option<JsPropValue>> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(
        db.get_node_prop(node_id as NodeId, key_id as PropKeyId)
          .map(|v| v.into()),
      ),
      Some(DatabaseInner::Graph(db)) => {
        Ok(get_node_prop_db(db, node_id as NodeId, key_id as PropKeyId).map(|v| v.into()))
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get all properties for a node (returns array of {key_id, value} pairs)
  #[napi]
  pub fn get_node_props(&self, node_id: i64) -> Result<Option<Vec<JsNodeProp>>> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        Ok(db.get_node_props(node_id as NodeId).map(|props| {
          props
            .into_iter()
            .map(|(k, v)| JsNodeProp {
              key_id: k,
              value: v.into(),
            })
            .collect()
        }))
      }
      Some(DatabaseInner::Graph(db)) => Ok(get_node_props_db(db, node_id as NodeId).map(|props| {
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
    src: i64,
    etype: u32,
    dst: i64,
    key_id: u32,
    value: JsPropValue,
  ) -> Result<()> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .set_edge_prop(
          src as NodeId,
          etype as ETypeId,
          dst as NodeId,
          key_id as PropKeyId,
          value.into(),
        )
        .map_err(|e| Error::from_reason(format!("Failed to set edge property: {e}"))),
      Some(DatabaseInner::Graph(_)) => self.with_graph_tx(|handle| {
        graph_set_edge_prop(
          handle,
          src as NodeId,
          etype as ETypeId,
          dst as NodeId,
          key_id as PropKeyId,
          value.into(),
        )
        .map_err(|e| Error::from_reason(format!("Failed to set edge property: {e}")))?;
        Ok(())
      }),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Set an edge property by key name
  #[napi]
  pub fn set_edge_prop_by_name(
    &self,
    src: i64,
    etype: u32,
    dst: i64,
    key_name: String,
    value: JsPropValue,
  ) -> Result<()> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .set_edge_prop_by_name(
          src as NodeId,
          etype as ETypeId,
          dst as NodeId,
          &key_name,
          value.into(),
        )
        .map_err(|e| Error::from_reason(format!("Failed to set edge property: {e}"))),
      Some(DatabaseInner::Graph(db)) => {
        let key_id = db
          .get_propkey_id(&key_name)
          .ok_or_else(|| Error::from_reason(format!("Unknown property key: {key_name}")))?;
        self.with_graph_tx(|handle| {
          graph_set_edge_prop(
            handle,
            src as NodeId,
            etype as ETypeId,
            dst as NodeId,
            key_id,
            value.into(),
          )
          .map_err(|e| Error::from_reason(format!("Failed to set edge property: {e}")))?;
          Ok(())
        })
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Delete an edge property
  #[napi]
  pub fn delete_edge_prop(&self, src: i64, etype: u32, dst: i64, key_id: u32) -> Result<()> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .delete_edge_prop(
          src as NodeId,
          etype as ETypeId,
          dst as NodeId,
          key_id as PropKeyId,
        )
        .map_err(|e| Error::from_reason(format!("Failed to delete edge property: {e}"))),
      Some(DatabaseInner::Graph(_)) => self.with_graph_tx(|handle| {
        graph_del_edge_prop(
          handle,
          src as NodeId,
          etype as ETypeId,
          dst as NodeId,
          key_id as PropKeyId,
        )
        .map_err(|e| Error::from_reason(format!("Failed to delete edge property: {e}")))?;
        Ok(())
      }),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get a specific edge property
  #[napi]
  pub fn get_edge_prop(
    &self,
    src: i64,
    etype: u32,
    dst: i64,
    key_id: u32,
  ) -> Result<Option<JsPropValue>> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(
        db.get_edge_prop(
          src as NodeId,
          etype as ETypeId,
          dst as NodeId,
          key_id as PropKeyId,
        )
        .map(|v| v.into()),
      ),
      Some(DatabaseInner::Graph(db)) => Ok(
        get_edge_prop_db(
          db,
          src as NodeId,
          etype as ETypeId,
          dst as NodeId,
          key_id as PropKeyId,
        )
        .map(|v| v.into()),
      ),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get all properties for an edge (returns array of {key_id, value} pairs)
  #[napi]
  pub fn get_edge_props(&self, src: i64, etype: u32, dst: i64) -> Result<Option<Vec<JsNodeProp>>> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(
        db.get_edge_props(src as NodeId, etype as ETypeId, dst as NodeId)
          .map(|props| {
            props
              .into_iter()
              .map(|(k, v)| JsNodeProp {
                key_id: k,
                value: v.into(),
              })
              .collect()
          }),
      ),
      Some(DatabaseInner::Graph(db)) => Ok(
        get_edge_props_db(db, src as NodeId, etype as ETypeId, dst as NodeId).map(|props| {
          props
            .into_iter()
            .map(|(k, v)| JsNodeProp {
              key_id: k,
              value: v.into(),
            })
            .collect()
        }),
      ),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  // ========================================================================
  // Vector Operations
  // ========================================================================

  /// Set a vector embedding for a node
  #[napi]
  pub fn set_node_vector(&self, node_id: i64, prop_key_id: u32, vector: Vec<f64>) -> Result<()> {
    let vector_f32: Vec<f32> = vector.iter().map(|&v| v as f32).collect();
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .set_node_vector(node_id as NodeId, prop_key_id as PropKeyId, &vector_f32)
        .map_err(|e| Error::from_reason(format!("Failed to set vector: {e}"))),
      Some(DatabaseInner::Graph(_)) => self.with_graph_tx(|handle| {
        graph_set_node_vector(
          handle,
          node_id as NodeId,
          prop_key_id as PropKeyId,
          &vector_f32,
        )
        .map_err(|e| Error::from_reason(format!("Failed to set vector: {e}")))?;
        Ok(())
      }),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get a vector embedding for a node
  #[napi]
  pub fn get_node_vector(&self, node_id: i64, prop_key_id: u32) -> Result<Option<Vec<f64>>> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(
        db.get_node_vector(node_id as NodeId, prop_key_id as PropKeyId)
          .map(|v| v.iter().map(|&f| f as f64).collect()),
      ),
      Some(DatabaseInner::Graph(db)) => {
        let pending = {
          let guard = self.graph_tx.lock();
          guard.as_ref().and_then(|tx| {
            let key = (node_id as NodeId, prop_key_id as PropKeyId);
            if tx.pending_vector_deletes.contains(&key) {
              return Some(None);
            }
            tx.pending_vector_sets.get(&key).cloned().map(Some)
          })
        };

        if let Some(pending_vec) = pending {
          return Ok(pending_vec.map(|v| v.iter().map(|&f| f as f64).collect()));
        }

        Ok(
          graph_get_node_vector_db(db, node_id as NodeId, prop_key_id as PropKeyId)
            .map(|v| v.iter().map(|&f| f as f64).collect()),
        )
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Delete a vector embedding for a node
  #[napi]
  pub fn delete_node_vector(&self, node_id: i64, prop_key_id: u32) -> Result<()> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .delete_node_vector(node_id as NodeId, prop_key_id as PropKeyId)
        .map_err(|e| Error::from_reason(format!("Failed to delete vector: {e}"))),
      Some(DatabaseInner::Graph(_)) => self.with_graph_tx(|handle| {
        graph_delete_node_vector(handle, node_id as NodeId, prop_key_id as PropKeyId)
          .map_err(|e| Error::from_reason(format!("Failed to delete vector: {e}")))?;
        Ok(())
      }),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Check if a node has a vector embedding
  #[napi]
  pub fn has_node_vector(&self, node_id: i64, prop_key_id: u32) -> Result<bool> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        Ok(db.has_node_vector(node_id as NodeId, prop_key_id as PropKeyId))
      }
      Some(DatabaseInner::Graph(db)) => {
        let pending = {
          let guard = self.graph_tx.lock();
          guard.as_ref().and_then(|tx| {
            let key = (node_id as NodeId, prop_key_id as PropKeyId);
            if tx.pending_vector_deletes.contains(&key) {
              return Some(false);
            }
            tx.pending_vector_sets.get(&key).map(|_| true)
          })
        };

        if let Some(pending_has) = pending {
          return Ok(pending_has);
        }

        Ok(graph_has_node_vector_db(
          db,
          node_id as NodeId,
          prop_key_id as PropKeyId,
        ))
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  // ========================================================================
  // Schema Operations
  // ========================================================================

  /// Get or create a label ID
  #[napi]
  pub fn get_or_create_label(&self, name: String) -> Result<u32> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.get_or_create_label(&name)),
      Some(DatabaseInner::Graph(db)) => Ok(db.get_or_create_label(&name)),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get label ID by name
  #[napi]
  pub fn get_label_id(&self, name: String) -> Result<Option<u32>> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.get_label_id(&name)),
      Some(DatabaseInner::Graph(db)) => Ok(db.get_label_id(&name)),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get label name by ID
  #[napi]
  pub fn get_label_name(&self, id: u32) -> Result<Option<String>> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.get_label_name(id)),
      Some(DatabaseInner::Graph(db)) => Ok(db.get_label_name(id)),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get or create an edge type ID
  #[napi]
  pub fn get_or_create_etype(&self, name: String) -> Result<u32> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.get_or_create_etype(&name)),
      Some(DatabaseInner::Graph(db)) => Ok(db.get_or_create_etype(&name)),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get edge type ID by name
  #[napi]
  pub fn get_etype_id(&self, name: String) -> Result<Option<u32>> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.get_etype_id(&name)),
      Some(DatabaseInner::Graph(db)) => Ok(db.get_etype_id(&name)),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get edge type name by ID
  #[napi]
  pub fn get_etype_name(&self, id: u32) -> Result<Option<String>> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.get_etype_name(id)),
      Some(DatabaseInner::Graph(db)) => Ok(db.get_etype_name(id)),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get or create a property key ID
  #[napi]
  pub fn get_or_create_propkey(&self, name: String) -> Result<u32> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.get_or_create_propkey(&name)),
      Some(DatabaseInner::Graph(db)) => Ok(db.get_or_create_propkey(&name)),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get property key ID by name
  #[napi]
  pub fn get_propkey_id(&self, name: String) -> Result<Option<u32>> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.get_propkey_id(&name)),
      Some(DatabaseInner::Graph(db)) => Ok(db.get_propkey_id(&name)),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get property key name by ID
  #[napi]
  pub fn get_propkey_name(&self, id: u32) -> Result<Option<String>> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.get_propkey_name(id)),
      Some(DatabaseInner::Graph(db)) => Ok(db.get_propkey_name(id)),
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
      Some(DatabaseInner::Graph(_)) => self.with_graph_tx(|handle| {
        let label_id = graph_define_label(handle, &name)
          .map_err(|e| Error::from_reason(format!("Failed to define label: {e}")))?;
        Ok(label_id)
      }),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Add a label to a node
  #[napi]
  pub fn add_node_label(&self, node_id: i64, label_id: u32) -> Result<()> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .add_node_label(node_id as NodeId, label_id)
        .map_err(|e| Error::from_reason(format!("Failed to add label: {e}"))),
      Some(DatabaseInner::Graph(_)) => self.with_graph_tx(|handle| {
        graph_add_node_label(handle, node_id as NodeId, label_id)
          .map_err(|e| Error::from_reason(format!("Failed to add label: {e}")))?;
        Ok(())
      }),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Add a label to a node by name
  #[napi]
  pub fn add_node_label_by_name(&self, node_id: i64, label_name: String) -> Result<()> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .add_node_label_by_name(node_id as NodeId, &label_name)
        .map_err(|e| Error::from_reason(format!("Failed to add label: {e}"))),
      Some(DatabaseInner::Graph(db)) => {
        let label_id = db
          .get_label_id(&label_name)
          .ok_or_else(|| Error::from_reason(format!("Unknown label: {label_name}")))?;
        self.with_graph_tx(|handle| {
          graph_add_node_label(handle, node_id as NodeId, label_id)
            .map_err(|e| Error::from_reason(format!("Failed to add label: {e}")))?;
          Ok(())
        })
      }
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Remove a label from a node
  #[napi]
  pub fn remove_node_label(&self, node_id: i64, label_id: u32) -> Result<()> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .remove_node_label(node_id as NodeId, label_id)
        .map_err(|e| Error::from_reason(format!("Failed to remove label: {e}"))),
      Some(DatabaseInner::Graph(_)) => self.with_graph_tx(|handle| {
        graph_remove_node_label(handle, node_id as NodeId, label_id)
          .map_err(|e| Error::from_reason(format!("Failed to remove label: {e}")))?;
        Ok(())
      }),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Check if a node has a label
  #[napi]
  pub fn node_has_label(&self, node_id: i64, label_id: u32) -> Result<bool> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.node_has_label(node_id as NodeId, label_id)),
      Some(DatabaseInner::Graph(db)) => Ok(node_has_label_db(db, node_id as NodeId, label_id)),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Get all labels for a node
  #[napi]
  pub fn get_node_labels(&self, node_id: i64) -> Result<Vec<u32>> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.get_node_labels(node_id as NodeId)),
      Some(DatabaseInner::Graph(db)) => Ok(get_node_labels_db(db, node_id as NodeId)),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  // ========================================================================
  // Graph Traversal (DB-backed)
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
    start_nodes: Vec<i64>,
    direction: JsTraversalDirection,
    edge_type: Option<u32>,
  ) -> Result<Vec<JsTraversalResult>> {
    let start: Vec<NodeId> = start_nodes.iter().map(|&id| id as NodeId).collect();
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
            .execute(|node_id, dir, etype| get_neighbors_from_single_file(db, node_id, dir, etype))
            .map(JsTraversalResult::from)
            .collect(),
        )
      }
      Some(DatabaseInner::Graph(db)) => {
        let builder = match direction {
          JsTraversalDirection::Out => RustTraversalBuilder::new(start).out(etype),
          JsTraversalDirection::In => RustTraversalBuilder::new(start).r#in(etype),
          JsTraversalDirection::Both => RustTraversalBuilder::new(start).both(etype),
        };

        Ok(
          builder
            .execute(|node_id, dir, etype| get_neighbors_from_graph_db(db, node_id, dir, etype))
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
    start_nodes: Vec<i64>,
    steps: Vec<JsTraversalStep>,
    limit: Option<u32>,
  ) -> Result<Vec<JsTraversalResult>> {
    let start: Vec<NodeId> = start_nodes.iter().map(|&id| id as NodeId).collect();
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
          builder = builder.take(n as usize);
        }

        Ok(
          builder
            .execute(|node_id, dir, etype| get_neighbors_from_single_file(db, node_id, dir, etype))
            .map(JsTraversalResult::from)
            .collect(),
        )
      }
      Some(DatabaseInner::Graph(db)) => {
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
          builder = builder.take(n as usize);
        }

        Ok(
          builder
            .execute(|node_id, dir, etype| get_neighbors_from_graph_db(db, node_id, dir, etype))
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
    start_nodes: Vec<i64>,
    edge_type: Option<u32>,
    options: JsTraverseOptions,
  ) -> Result<Vec<JsTraversalResult>> {
    let start: Vec<NodeId> = start_nodes.iter().map(|&id| id as NodeId).collect();
    let opts: TraverseOptions = options.into();

    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(
        RustTraversalBuilder::new(start)
          .traverse(edge_type, opts)
          .execute(|node_id, dir, etype| get_neighbors_from_single_file(db, node_id, dir, etype))
          .map(JsTraversalResult::from)
          .collect(),
      ),
      Some(DatabaseInner::Graph(db)) => Ok(
        RustTraversalBuilder::new(start)
          .traverse(edge_type, opts)
          .execute(|node_id, dir, etype| get_neighbors_from_graph_db(db, node_id, dir, etype))
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
  pub fn traverse_count(&self, start_nodes: Vec<i64>, steps: Vec<JsTraversalStep>) -> Result<u32> {
    let start: Vec<NodeId> = start_nodes.iter().map(|&id| id as NodeId).collect();
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
          builder
            .count(|node_id, dir, etype| get_neighbors_from_single_file(db, node_id, dir, etype))
            as u32,
        )
      }
      Some(DatabaseInner::Graph(db)) => {
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
          builder.count(|node_id, dir, etype| get_neighbors_from_graph_db(db, node_id, dir, etype))
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
    start_nodes: Vec<i64>,
    steps: Vec<JsTraversalStep>,
    limit: Option<u32>,
  ) -> Result<Vec<i64>> {
    let start: Vec<NodeId> = start_nodes.iter().map(|&id| id as NodeId).collect();
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
          builder = builder.take(n as usize);
        }

        Ok(
          builder
            .collect_node_ids(|node_id, dir, etype| {
              get_neighbors_from_single_file(db, node_id, dir, etype)
            })
            .into_iter()
            .map(|id| id as i64)
            .collect(),
        )
      }
      Some(DatabaseInner::Graph(db)) => {
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
          builder = builder.take(n as usize);
        }

        Ok(
          builder
            .collect_node_ids(|node_id, dir, etype| {
              get_neighbors_from_graph_db(db, node_id, dir, etype)
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
        let rust_config: PathConfig = config.into();
        Ok(
          dijkstra(
            rust_config,
            |node_id, dir, etype| get_neighbors_from_single_file(db, node_id, dir, etype),
            |src, etype, dst| get_edge_weight_from_single_file(db, src, etype, dst, weight_key),
          )
          .into(),
        )
      }
      Some(DatabaseInner::Graph(db)) => {
        let weight_key = resolve_weight_key_graph(db, &config)?;
        let rust_config: PathConfig = config.into();
        Ok(
          dijkstra(
            rust_config,
            |node_id, dir, etype| get_neighbors_from_graph_db(db, node_id, dir, etype),
            |src, etype, dst| get_edge_weight_from_graph_db(db, src, etype, dst, weight_key),
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
    let rust_config: PathConfig = config.into();
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(
        bfs(rust_config, |node_id, dir, etype| {
          get_neighbors_from_single_file(db, node_id, dir, etype)
        })
        .into(),
      ),
      Some(DatabaseInner::Graph(db)) => Ok(
        bfs(rust_config, |node_id, dir, etype| {
          get_neighbors_from_graph_db(db, node_id, dir, etype)
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
        let rust_config: PathConfig = config.into();
        Ok(
          yen_k_shortest(
            rust_config,
            k as usize,
            |node_id, dir, etype| get_neighbors_from_single_file(db, node_id, dir, etype),
            |src, etype, dst| get_edge_weight_from_single_file(db, src, etype, dst, weight_key),
          )
          .into_iter()
          .map(JsPathResult::from)
          .collect(),
        )
      }
      Some(DatabaseInner::Graph(db)) => {
        let weight_key = resolve_weight_key_graph(db, &config)?;
        let rust_config: PathConfig = config.into();
        Ok(
          yen_k_shortest(
            rust_config,
            k as usize,
            |node_id, dir, etype| get_neighbors_from_graph_db(db, node_id, dir, etype),
            |src, etype, dst| get_edge_weight_from_graph_db(db, src, etype, dst, weight_key),
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
    source: i64,
    target: i64,
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
    source: i64,
    target: i64,
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
    source: i64,
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
      Some(DatabaseInner::Graph(_)) => Err(Error::from_reason(
        "checkpoint() only supports single-file databases",
      )),
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
      Some(DatabaseInner::Graph(_)) => Err(Error::from_reason(
        "backgroundCheckpoint() only supports single-file databases",
      )),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Check if checkpoint is recommended
  #[napi]
  pub fn should_checkpoint(&self, threshold: Option<f64>) -> Result<bool> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db.should_checkpoint(threshold.unwrap_or(0.8))),
      Some(DatabaseInner::Graph(_)) => Ok(false),
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
      Some(DatabaseInner::Graph(db)) => db
        .optimize()
        .map_err(|e| Error::from_reason(format!("Failed to optimize: {e}"))),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Optimize (compact) a single-file database with options
  #[napi(js_name = "optimizeSingleFile")]
  pub fn optimize_single_file(
    &mut self,
    options: Option<SingleFileOptimizeOptions>,
  ) -> Result<()> {
    match self.inner.as_mut() {
      Some(DatabaseInner::SingleFile(db)) => db
        .optimize_single_file(options.map(Into::into))
        .map_err(|e| Error::from_reason(format!("Failed to optimize single-file: {e}"))),
      Some(DatabaseInner::Graph(_)) => Err(Error::from_reason(
        "optimizeSingleFile() only supports single-file databases",
      )),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Vacuum a single-file database to reclaim free space
  #[napi]
  pub fn vacuum(&mut self, options: Option<VacuumOptions>) -> Result<()> {
    match self.inner.as_mut() {
      Some(DatabaseInner::SingleFile(db)) => db
        .vacuum_single_file(options.map(Into::into))
        .map_err(|e| Error::from_reason(format!("Failed to vacuum: {e}"))),
      Some(DatabaseInner::Graph(_)) => Err(Error::from_reason(
        "vacuum() only supports single-file databases",
      )),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Vacuum a single-file database to reclaim free space
  #[napi(js_name = "vacuumSingleFile")]
  pub fn vacuum_single_file(&mut self, options: Option<VacuumOptions>) -> Result<()> {
    self.vacuum(options)
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
      Some(DatabaseInner::Graph(db)) => Ok(graph_stats(db)),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Check database integrity
  #[napi]
  pub fn check(&self) -> Result<CheckResult> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(CheckResult::from(db.check())),
      Some(DatabaseInner::Graph(db)) => Ok(CheckResult::from(graph_check(db))),
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
      Some(DatabaseInner::Graph(db)) => ray_export::export_to_object_graph(db, opts)
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
      Some(DatabaseInner::Graph(db)) => ray_export::export_to_object_graph(db, rust_opts.clone())
        .map_err(|e| Error::from_reason(e.to_string()))?,
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
      Some(DatabaseInner::Graph(db)) => ray_export::export_to_object_graph(db, rust_opts)
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
    let rust_opts = opts.into_rust();
    let parsed: ray_export::ExportedDatabase =
      serde_json::from_value(data).map_err(|e| Error::from_reason(e.to_string()))?;

    let result = match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        ray_export::import_from_object_single(db, &parsed, rust_opts)
          .map_err(|e| Error::from_reason(e.to_string()))?
      }
      Some(DatabaseInner::Graph(db)) => {
        ray_export::import_from_object_graph(db, &parsed, rust_opts)
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
    let rust_opts = opts.into_rust();
    let parsed =
      ray_export::import_from_json(path).map_err(|e| Error::from_reason(e.to_string()))?;

    let result = match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        ray_export::import_from_object_single(db, &parsed, rust_opts)
          .map_err(|e| Error::from_reason(e.to_string()))?
      }
      Some(DatabaseInner::Graph(db)) => {
        ray_export::import_from_object_graph(db, &parsed, rust_opts)
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
      Some(DatabaseInner::Graph(_)) => Ok(false),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Invalidate all caches for a node
  #[napi]
  pub fn cache_invalidate_node(&self, node_id: i64) -> Result<()> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        db.cache_invalidate_node(node_id as NodeId);
        Ok(())
      }
      Some(DatabaseInner::Graph(_)) => Ok(()),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  /// Invalidate caches for a specific edge
  #[napi]
  pub fn cache_invalidate_edge(&self, src: i64, etype: u32, dst: i64) -> Result<()> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        db.cache_invalidate_edge(src as NodeId, etype as ETypeId, dst as NodeId);
        Ok(())
      }
      Some(DatabaseInner::Graph(_)) => Ok(()),
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
      Some(DatabaseInner::Graph(_)) => Ok(()),
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
      Some(DatabaseInner::Graph(_)) => Ok(()),
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
      Some(DatabaseInner::Graph(_)) => Ok(()),
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
      Some(DatabaseInner::Graph(_)) => Ok(()),
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
      Some(DatabaseInner::Graph(_)) => Ok(()),
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
      Some(DatabaseInner::Graph(_)) => Ok(()),
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
      Some(DatabaseInner::Graph(_)) => Ok(None),
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
      Some(DatabaseInner::Graph(_)) => Ok(()),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  // ========================================================================
  // Internal Helpers
  // ========================================================================

  fn get_db(&self) -> Result<&RustSingleFileDB> {
    match self.inner.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => Ok(db),
      Some(DatabaseInner::Graph(_)) => Err(Error::from_reason("Database is multi-file")),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  fn get_graph_db(&self) -> Result<&RustGraphDB> {
    match self.inner.as_ref() {
      Some(DatabaseInner::Graph(db)) => Ok(db),
      Some(DatabaseInner::SingleFile(_)) => Err(Error::from_reason("Database is single-file")),
      None => Err(Error::from_reason("Database is closed")),
    }
  }

  fn with_graph_tx<F, R>(&self, f: F) -> Result<R>
  where
    F: FnOnce(&mut GraphTxHandle) -> Result<R>,
  {
    let db = self.get_graph_db()?;
    let mut guard = self.graph_tx.lock();
    let tx_state = guard
      .take()
      .ok_or_else(|| Error::from_reason("No active transaction"))?;
    let mut handle = GraphTxHandle::new(db, tx_state);
    let result = f(&mut handle);
    *guard = Some(handle.tx);
    result
  }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Get neighbors from database for traversal
fn get_neighbors_from_single_file(
  db: &RustSingleFileDB,
  node_id: NodeId,
  direction: TraversalDirection,
  etype: Option<ETypeId>,
) -> Vec<Edge> {
  let mut edges = Vec::new();
  match direction {
    TraversalDirection::Out => {
      for (e, dst) in db.get_out_edges(node_id) {
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
      for (e, src) in db.get_in_edges(node_id) {
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
      edges.extend(get_neighbors_from_single_file(
        db,
        node_id,
        TraversalDirection::Out,
        etype,
      ));
      edges.extend(get_neighbors_from_single_file(
        db,
        node_id,
        TraversalDirection::In,
        etype,
      ));
    }
  }
  edges
}

fn get_neighbors_from_graph_db(
  db: &RustGraphDB,
  node_id: NodeId,
  direction: TraversalDirection,
  etype: Option<ETypeId>,
) -> Vec<Edge> {
  let mut edges = Vec::new();
  match direction {
    TraversalDirection::Out => {
      for edge in list_out_edges(db, node_id) {
        if etype.is_none() || etype == Some(edge.etype) {
          edges.push(Edge {
            src: node_id,
            etype: edge.etype,
            dst: edge.dst,
          });
        }
      }
    }
    TraversalDirection::In => {
      for edge in list_in_edges(db, node_id) {
        if etype.is_none() || etype == Some(edge.etype) {
          edges.push(Edge {
            src: edge.dst,
            etype: edge.etype,
            dst: node_id,
          });
        }
      }
    }
    TraversalDirection::Both => {
      edges.extend(get_neighbors_from_graph_db(
        db,
        node_id,
        TraversalDirection::Out,
        etype,
      ));
      edges.extend(get_neighbors_from_graph_db(
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
      .get_propkey_id(key_name)
      .ok_or_else(|| Error::from_reason(format!("Unknown property key: {key_name}")))?;
    return Ok(Some(key_id));
  }

  Ok(None)
}

fn resolve_weight_key_graph(db: &RustGraphDB, config: &JsPathConfig) -> Result<Option<PropKeyId>> {
  if let Some(key_id) = config.weight_key_id {
    return Ok(Some(key_id as PropKeyId));
  }

  if let Some(ref key_name) = config.weight_key_name {
    let key_id = db
      .get_propkey_id(key_name)
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

fn get_edge_weight_from_single_file(
  db: &RustSingleFileDB,
  src: NodeId,
  etype: ETypeId,
  dst: NodeId,
  weight_key: Option<PropKeyId>,
) -> f64 {
  match weight_key {
    Some(key_id) => prop_value_to_weight(db.get_edge_prop(src, etype, dst, key_id)),
    None => 1.0,
  }
}

fn get_edge_weight_from_graph_db(
  db: &RustGraphDB,
  src: NodeId,
  etype: ETypeId,
  dst: NodeId,
  weight_key: Option<PropKeyId>,
) -> f64 {
  match weight_key {
    Some(key_id) => prop_value_to_weight(get_edge_prop_db(db, src, etype, dst, key_id)),
    None => 1.0,
  }
}

fn graph_stats(db: &RustGraphDB) -> DbStats {
  let node_count = graph_count_nodes(db);
  let edge_count = graph_count_edges(db, None);

  let delta = db.delta.read();
  let delta_nodes_created = delta.created_nodes.len();
  let delta_nodes_deleted = delta.deleted_nodes.len();
  let delta_edges_added = delta.total_edges_added();
  let delta_edges_deleted = delta.total_edges_deleted();
  drop(delta);

  let (snapshot_gen, snapshot_nodes, snapshot_edges, snapshot_max_node_id) =
    if let Some(ref snapshot) = db.snapshot {
      (
        snapshot.header.generation,
        snapshot.header.num_nodes,
        snapshot.header.num_edges,
        snapshot.header.max_node_id,
      )
    } else {
      (0, 0, 0, 0)
    };

  let wal_segment = db
    .manifest
    .as_ref()
    .map(|m| m.active_wal_seg)
    .unwrap_or(0);

  let mvcc_stats = db.mvcc.as_ref().map(|mvcc| {
    let tx_mgr = mvcc.tx_manager.lock();
    let gc = mvcc.gc.lock();
    let gc_stats = gc.get_stats();
    let committed_stats = tx_mgr.get_committed_writes_stats();
    MvccStats {
      active_transactions: tx_mgr.get_active_count() as i64,
      min_active_ts: tx_mgr.min_active_ts() as i64,
      versions_pruned: gc_stats.versions_pruned as i64,
      gc_runs: gc_stats.gc_runs as i64,
      last_gc_time: gc_stats.last_gc_time as i64,
      committed_writes_size: committed_stats.size as i64,
      committed_writes_pruned: committed_stats.pruned as i64,
    }
  });

  let total_changes =
    delta_nodes_created + delta_nodes_deleted + delta_edges_added + delta_edges_deleted;
  let recommend_compact = total_changes > 10_000;

  DbStats {
    snapshot_gen: snapshot_gen as i64,
    snapshot_nodes: snapshot_nodes.max(node_count) as i64,
    snapshot_edges: snapshot_edges.max(edge_count) as i64,
    snapshot_max_node_id: snapshot_max_node_id as i64,
    delta_nodes_created: delta_nodes_created as i64,
    delta_nodes_deleted: delta_nodes_deleted as i64,
    delta_edges_added: delta_edges_added as i64,
    delta_edges_deleted: delta_edges_deleted as i64,
    wal_segment: wal_segment as i64,
    wal_bytes: db.wal_bytes() as i64,
    recommend_compact,
    mvcc_stats,
  }
}

fn graph_check(db: &RustGraphDB) -> RustCheckResult {
  if let Some(ref snapshot) = db.snapshot {
    return crate::check::check_snapshot(snapshot);
  }

  RustCheckResult {
    valid: true,
    errors: Vec::new(),
    warnings: vec!["No snapshot to check".to_string()],
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

// ============================================================================
// Metrics / Health
// ============================================================================

#[napi]
pub fn collect_metrics(db: &Database) -> Result<DatabaseMetrics> {
  match db.inner.as_ref() {
    Some(DatabaseInner::SingleFile(db)) => Ok(core_metrics::collect_metrics_single_file(db).into()),
    Some(DatabaseInner::Graph(db)) => Ok(core_metrics::collect_metrics_graph(db).into()),
    None => Err(Error::from_reason("Database is closed")),
  }
}

#[napi]
pub fn health_check(db: &Database) -> Result<HealthCheckResult> {
  match db.inner.as_ref() {
    Some(DatabaseInner::SingleFile(db)) => Ok(core_metrics::health_check_single_file(db).into()),
    Some(DatabaseInner::Graph(db)) => Ok(core_metrics::health_check_graph(db).into()),
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
  /// Backup type ("single-file" or "multi-file")
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
    Some(DatabaseInner::SingleFile(db)) => core_backup::create_backup_single_file(
      db,
      &backup_path,
      core_options,
    )
    .map(BackupResult::from)
    .map_err(|e| Error::from_reason(format!("Failed to create backup: {e}"))),
    Some(DatabaseInner::Graph(db)) => {
      core_backup::create_backup_graph(db, &backup_path, core_options)
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
pub fn get_backup_info(backup_path: String) -> Result<BackupResult> {
  core_backup::get_backup_info(backup_path)
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
