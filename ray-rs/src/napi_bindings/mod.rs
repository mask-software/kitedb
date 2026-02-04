//! NAPI bindings for KiteDB
//!
//! Exposes SingleFileDB and related types to Node.js/Bun.

pub mod database;
pub mod kite;
pub mod traversal;
pub mod vector;

pub use database::{
  backup_info, collect_metrics, create_backup, create_offline_backup, health_check, open_database,
  restore_backup, BackupOptions, BackupResult, CacheLayerMetrics, CacheMetrics, CheckResult,
  CompressionOptions, DataMetrics, Database, DatabaseMetrics, DbStats, EdgePage, EdgeWithProps,
  HealthCheckEntry, HealthCheckResult, JsCompressionType, JsEdge, JsFullEdge, JsNodeProp,
  JsPropValue, MemoryMetrics, MvccMetrics, MvccStats, NodePage, NodeWithProps,
  OfflineBackupOptions, OpenOptions, PaginationOptions, PropType, RestoreOptions,
  SingleFileOptimizeOptions, StreamOptions, VacuumOptions,
};

pub use kite::{
  kite, kite_sync, JsEdgeSpec, JsKeySpec, JsKiteOptions, JsNodeSpec, JsPathEdge, JsPathResult,
  JsPropSpec, Kite, KiteInsertBuilder, KiteInsertExecutorMany, KiteInsertExecutorSingle, KitePath,
  KiteTraversal, KiteUpdateBuilder, KiteUpdateEdgeBuilder, KiteUpsertBuilder,
  KiteUpsertByIdBuilder, KiteUpsertEdgeBuilder, KiteUpsertExecutorMany, KiteUpsertExecutorSingle,
};

pub use traversal::{
  path_config, traversal_step, JsEdgeInput, JsGraphAccessor, JsPathConfig, JsTraversalDirection,
  JsTraversalResult, JsTraversalStep, JsTraverseOptions,
};

pub use vector::{
  brute_force_search, create_vector_index, JsAggregation, JsBruteForceResult, JsDistanceMetric,
  JsIvfConfig, JsIvfIndex, JsIvfPqIndex, JsIvfStats, JsPqConfig, JsSearchOptions, JsSearchResult,
  SimilarOptions, VectorIndex, VectorIndexOptions, VectorIndexStats, VectorSearchHit,
};
