/**
 * Embedded Graph Database
 *
 * A high-performance embedded graph database with:
 * - Fast reads via mmap CSR snapshots
 * - Reliable writes via WAL + delta overlay
 * - Stable node IDs
 * - Periodic compaction for maintenance
 */

// ============================================================================
// Core types
// ============================================================================

export type {
  NodeID,
  ETypeID,
  LabelID,
  PropKeyID,
  PropValue,
  Edge,
  GraphDB,
  TxHandle,
  OpenOptions,
  NodeOpts,
  DbStats,
  CheckResult,
  CacheOptions,
  CacheStats,
  PropertyCacheConfig,
  TraversalCacheConfig,
  QueryCacheConfig,
} from "./types.ts";

export { PropValueTag } from "./types.ts";

// ============================================================================
// Database lifecycle
// ============================================================================

export { openGraphDB, closeGraphDB } from "./ray/graph-db/index.ts";

// ============================================================================
// Locking utilities
// ============================================================================

/**
 * Check if proper OS-level file locking is available.
 * Uses native flock() via Bun FFI - no external dependencies required.
 * 
 * @example
 * ```ts
 * if (!isProperLockingAvailable()) {
 *   console.warn("File locking not available on this platform");
 * }
 * 
 * const db = await openGraphDB("./mydb", {
 *   requireLocking: true, // Throws if locking unavailable
 * });
 * ```
 */
export { isProperLockingAvailable } from "./util/lock.ts";

// ============================================================================
// Backup and Restore
// ============================================================================

export {
  createBackup,
  restoreBackup,
  getBackupInfo,
  createOfflineBackup,
  type BackupOptions,
  type RestoreOptions,
  type BackupResult,
} from "./backup/index.ts";

// ============================================================================
// Streaming and Pagination
// ============================================================================

export {
  streamNodes,
  streamNodesWithProps,
  streamEdges,
  streamEdgesWithProps,
  getNodesPage,
  getEdgesPage,
  collectStream,
  processStream,
  mapStream,
  filterStream,
  takeStream,
  skipStream,
  type StreamOptions,
  type PaginationOptions,
  type Page,
  type NodeWithProps,
  type EdgeWithProps,
} from "./streaming/index.ts";

// ============================================================================
// Export and Import
// ============================================================================

export {
  exportToJSON,
  exportToObject,
  exportToJSONL,
  importFromJSON,
  importFromObject,
  type ExportOptions,
  type ImportOptions,
  type ExportedDatabase,
  type ExportedNode,
  type ExportedEdge,
} from "./export/index.ts";

// ============================================================================
// Metrics and Observability
// ============================================================================

export {
  collectMetrics,
  formatMetrics,
  metricsToJSON,
  healthCheck,
  type DatabaseMetrics,
  type DataMetrics,
  type CacheMetrics,
  type MvccMetrics,
  type MemoryMetrics,
  type HealthCheckResult,
} from "./metrics/index.ts";

// ============================================================================
// Transactions
// ============================================================================

export { beginTx, commit, rollback } from "./ray/graph-db/index.ts";

// ============================================================================
// Node operations
// ============================================================================

export {
  createNode,
  deleteNode,
  getNodeByKey,
  nodeExists,
  listNodes,
  countNodes,
} from "./ray/graph-db/index.ts";

// ============================================================================
// Edge operations
// ============================================================================

export {
  addEdge,
  deleteEdge,
  getNeighborsOut,
  getNeighborsIn,
  edgeExists,
  listEdges,
  countEdges,
} from "./ray/graph-db/index.ts";

// ============================================================================
// Property operations
// ============================================================================

export {
  setNodeProp,
  delNodeProp,
  setEdgeProp,
  delEdgeProp,
  getNodeProp,
  getNodeProps,
  getEdgeProp,
  getEdgeProps,
} from "./ray/graph-db/index.ts";

// ============================================================================
// Schema definitions
// ============================================================================

export { defineLabel, defineEtype, definePropkey } from "./ray/graph-db/index.ts";

// ============================================================================
// Maintenance
// ============================================================================

export { stats, check } from "./ray/graph-db/index.ts";

// ============================================================================
// Vector operations (low-level)
// ============================================================================

export {
  setNodeVector,
  getNodeVector,
  delNodeVector,
  hasNodeVector,
  getVectorStore,
  getVectorStats,
} from "./ray/graph-db/index.ts";

// ============================================================================
// Cache API
// ============================================================================

export {
  invalidateNodeCache,
  invalidateEdgeCache,
  clearCache,
  getCacheStats,
} from "./ray/graph-db/index.ts";

export { optimize, type OptimizeOptions } from "./core/compactor.ts";

// ============================================================================
// Single-file database format (.raydb)
// These are maintenance utilities - database opening/closing is handled
// automatically by openGraphDB/closeGraphDB based on path detection
// ============================================================================

export {
  optimizeSingleFile,
  vacuumSingleFile,
  type SingleFileOptimizeOptions,
  type VacuumOptions,
} from "./core/single-file-compactor.ts";

export { WalBufferFullError } from "./types.ts";

// Deprecated exports for backwards compatibility
// Use openGraphDB/closeGraphDB instead - they auto-detect format
/** @deprecated Use openGraphDB instead - it auto-detects format */
export { openSingleFileDB, closeSingleFileDB, isSingleFilePath } from "./ray/graph-db/single-file.ts";
/** @deprecated SingleFileDB is now just an alias for GraphDB */
export type { SingleFileDB, SingleFileOpenOptions } from "./types.ts";

// ============================================================================
// Utilities for advanced use
// ============================================================================

export { checkSnapshot } from "./check/checker.ts";

// ============================================================================
// Compression
// ============================================================================

export {
  CompressionType,
  type CompressionOptions,
  DEFAULT_COMPRESSION_OPTIONS,
} from "./util/compression.ts";

// ============================================================================
// High-Level API (Drizzle-style)
// ============================================================================

export {
  // Main entry
  ray,
  Ray,
  type RayOptions,
  type TransactionContext,
  // Schema builders
  defineNode,
  defineEdge,
  prop,
  optional,
  type NodeDef,
  type EdgeDef,
  type PropDef,
  type PropBuilder,
  type OptionalPropDef,
  type PropsSchema,
  type EdgePropsSchema,
  type InferNode,
  type InferNodeInsert,
  type InferEdge,
  type InferEdgeProps,
  type RaySchema,
  // Query builders
  type InsertBuilder,
  type InsertExecutor,
  type UpdateBuilder,
  type UpdateExecutor,
  type UpdateByRefBuilder,
  type UpdateByRefExecutor,
  type DeleteBuilder,
  type DeleteExecutor,
  type LinkExecutor,
  type UpdateEdgeBuilder,
  type UpdateEdgeExecutor,
  type NodeRef,
  type WhereCondition,
  // Traversal
  type TraversalBuilder,
  type TraverseOptions,
  type TraversalDirection,
  type AsyncTraversalResult,
  type EdgeResult,
  type RawEdge,
  // Pathfinding
  type PathFindingBuilder,
  type PathExecutor,
  type PathResult,
  type WeightSpec,
  type Heuristic,
} from "./api/index.ts";

// ============================================================================
// Vector Embeddings (Lance-style columnar storage with IVF index)
// ============================================================================

export {
  // Types
  type VectorStoreConfig,
  type IvfConfig,
  type RowGroup,
  type Fragment,
  type VectorManifest,
  type IvfIndex,
  type VectorSearchResult,
  type VectorSearchOptions,
  type MultiVectorSearchOptions,
  type BatchInsertOptions,
  type VectorDeltaState,
  type CompactionStrategy,
  // Constants
  DEFAULT_VECTOR_CONFIG,
  DEFAULT_IVF_CONFIG,
  DEFAULT_COMPACTION_STRATEGY,
  // Normalization
  l2Norm,
  normalizeInPlace,
  normalize,
  isNormalized,
  normalizeRowGroup,
  // Distance functions
  dotProduct,
  cosineDistance,
  cosineSimilarity,
  squaredEuclidean,
  euclideanDistance,
  batchCosineDistance,
  batchSquaredEuclidean,
  getDistanceFunction,
  getBatchDistanceFunction,
  distanceToSimilarity,
  findKNearest,
  MinHeap,
  MaxHeap,
  // Row group operations
  createRowGroup,
  rowGroupAppend,
  rowGroupGet,
  rowGroupGetCopy,
  rowGroupIsFull,
  rowGroupTrim,
  // Fragment operations
  createFragment,
  fragmentAppend,
  fragmentDelete,
  fragmentIsDeleted,
  fragmentSeal,
  fragmentShouldSeal,
  fragmentGetVector,
  fragmentLiveCount,
  // Columnar store
  createVectorStore,
  vectorStoreInsert,
  vectorStoreDelete,
  vectorStoreGet,
  vectorStoreGetById,
  vectorStoreHas,
  vectorStoreIterator,
  vectorStoreBatchInsert,
  vectorStoreStats,
  vectorStoreFragmentStats,
  vectorStoreSealActive,
  vectorStoreGetAllVectors,
  vectorStoreClear,
  vectorStoreClone,
  // IVF index
  createIvfIndex,
  ivfAddTrainingVectors,
  ivfTrain,
  ivfInsert,
  ivfDelete,
  ivfSearch,
  ivfSearchMulti,
  ivfBuildFromStore,
  ivfStats,
  ivfClear,
  // Compaction
  findFragmentsToCompact,
  compactFragments,
  applyCompaction,
  runCompactionIfNeeded,
  getCompactionStats,
  forceFullCompaction,
  // Serialization
  serializeIvf,
  deserializeIvf,
  serializeManifest,
  deserializeManifest,
} from "./vector/index.ts";

// High-level vector search API
export {
  VectorIndex,
  createVectorIndex,
  type VectorIndexOptions,
  type SimilarOptions,
  type VectorSearchHit,
} from "./api/vector-search.ts";
