/**
 * RayDB - A fast, lightweight, embedded graph database for Node.js
 *
 * @example
 * ```typescript
 * import { ray, defineNode, defineEdge, prop, optional } from '@ray-db/core'
 *
 * // Define schema
 * const User = defineNode('user', {
 *   key: (id: string) => `user:${id}`,
 *   props: {
 *     name: prop.string('name'),
 *     email: prop.string('email'),
 *   },
 * })
 *
 * const knows = defineEdge('knows', {
 *   since: prop.int('since'),
 * })
 *
 * // Open database
 * const db = await ray('./my.raydb', {
 *   nodes: [User],
 *   edges: [knows],
 * })
 *
 * // Insert nodes
 * const alice = db.insert('user').values('alice', { name: 'Alice' }).returning()
 *
 * // Close when done
 * db.close()
 * ```
 *
 * @packageDocumentation
 */

// =============================================================================
// Schema Builders (clean API)
// =============================================================================

export { node, edge, prop, optional, withDefault, defineNode, defineEdge } from './schema'
export type { PropType, PropSpec, KeySpec, NodeSpec, NodeConfig, EdgeSpec } from './schema'

// =============================================================================
// Native Bindings
// =============================================================================

// Import native bindings
import { ray as nativeRay, raySync as nativeRaySync, Ray as NativeRay } from '../index'

import type { JsRayOptions, JsNodeSpec, JsEdgeSpec, JsPropSpec, JsPropValue } from '../index'

import type { NodeSpec, EdgeSpec, PropSpec } from './schema'

// =============================================================================
// Clean Type Aliases (no Js prefix)
// =============================================================================

// Re-export Ray class
export { Ray } from '../index'

// Re-export other classes with clean names
export {
  Database,
  VectorIndex,
  RayInsertBuilder,
  RayInsertExecutorSingle,
  RayInsertExecutorMany,
  RayUpdateBuilder,
  RayUpdateEdgeBuilder,
  RayTraversal,
  RayPath,
} from '../index'

// Re-export enums with clean names
export {
  JsTraversalDirection as TraversalDirection,
  JsDistanceMetric as DistanceMetric,
  JsAggregation as Aggregation,
  JsSyncMode as SyncMode,
  JsCompressionType as CompressionType,
  PropType as PropValueType,
} from '../index'

// Re-export utility functions
export {
  openDatabase,
  createBackup,
  restoreBackup,
  getBackupInfo,
  createOfflineBackup,
  collectMetrics,
  healthCheck,
  createVectorIndex,
  bruteForceSearch,
  pathConfig,
  traversalStep,
  version,
} from '../index'

// Re-export common types with clean names
export type {
  // Database
  DbStats,
  CheckResult,
  OpenOptions,
  // Export/Import
  ExportOptions,
  ExportResult,
  ImportOptions,
  ImportResult,
  // Backup
  BackupOptions,
  BackupResult,
  RestoreOptions,
  OfflineBackupOptions,
  // Streaming
  StreamOptions,
  PaginationOptions,
  NodePage,
  EdgePage,
  NodeWithProps,
  EdgeWithProps,
  // Metrics
  DatabaseMetrics,
  DataMetrics,
  CacheMetrics,
  CacheLayerMetrics,
  MemoryMetrics,
  MvccMetrics,
  MvccStats,
  HealthCheckResult,
  HealthCheckEntry,
  // Traversal
  JsTraverseOptions as TraverseOptions,
  JsTraversalStep as TraversalStep,
  JsTraversalResult as TraversalResult,
  // Pathfinding
  JsPathConfig as PathConfig,
  JsPathResult as PathResult,
  JsPathEdge as PathEdge,
  // Vectors
  VectorIndexOptions,
  VectorIndexStats,
  VectorSearchHit,
  SimilarOptions,
  JsIvfConfig as IvfConfig,
  JsIvfStats as IvfStats,
  JsPqConfig as PqConfig,
  JsSearchOptions as SearchOptions,
  JsSearchResult as SearchResult,
  JsBruteForceResult as BruteForceResult,
  // Compression
  CompressionOptions,
  SingleFileOptimizeOptions,
  VacuumOptions,
  // Cache
  JsCacheStats as CacheStats,
  // Low-level (for advanced use)
  JsEdge as Edge,
  JsFullEdge as FullEdge,
  JsNodeProp as NodeProp,
  JsPropValue as PropValue,
  JsEdgeInput as EdgeInput,
} from '../index'

// =============================================================================
// Ray Options (clean API)
// =============================================================================

/** Options for opening a Ray database */
export interface RayOptions {
  /** Node type definitions */
  nodes: NodeSpec[]
  /** Edge type definitions */
  edges: EdgeSpec[]
  /** Open in read-only mode (default: false) */
  readOnly?: boolean
  /** Create database if it doesn't exist (default: true) */
  createIfMissing?: boolean
  /** Acquire file lock (default: true) */
  lockFile?: boolean
}

// =============================================================================
// Type Conversion Helpers
// =============================================================================

function propSpecToNative(spec: PropSpec): JsPropSpec {
  return {
    type: spec.type,
    optional: spec.optional,
    default: spec.default as JsPropValue | undefined,
  }
}

function nodeSpecToNative(spec: NodeSpec): JsNodeSpec {
  let props: Record<string, JsPropSpec> | undefined

  if (spec.props) {
    props = {}
    for (const [k, v] of Object.entries(spec.props)) {
      props[k] = propSpecToNative(v)
    }
  }

  return {
    name: spec.name,
    key: spec.key,
    props,
  }
}

function edgeSpecToNative(spec: EdgeSpec): JsEdgeSpec {
  let props: Record<string, JsPropSpec> | undefined

  if (spec.props) {
    props = {}
    for (const [k, v] of Object.entries(spec.props)) {
      props[k] = propSpecToNative(v)
    }
  }

  return {
    name: spec.name,
    props,
  }
}

function optionsToNative(options: RayOptions): JsRayOptions {
  return {
    nodes: options.nodes.map(nodeSpecToNative),
    edges: options.edges.map(edgeSpecToNative),
    readOnly: options.readOnly,
    createIfMissing: options.createIfMissing,
    lockFile: options.lockFile,
  }
}

// =============================================================================
// Main Entry Points
// =============================================================================

/**
 * Open a Ray database asynchronously.
 *
 * This is the recommended way to open a database as it doesn't block
 * the Node.js event loop during file I/O.
 *
 * @param path - Path to the database file
 * @param options - Database options including schema
 * @returns Promise resolving to a Ray database instance
 *
 * @example
 * ```typescript
 * const db = await ray('./my.raydb', {
 *   nodes: [User, Post],
 *   edges: [follows, authored],
 * })
 * ```
 */
export async function ray(path: string, options: RayOptions): Promise<NativeRay> {
  const nativeOptions = optionsToNative(options)
  // Cast through unknown because NAPI-RS generates Promise<unknown> for async tasks
  return (await nativeRay(path, nativeOptions)) as NativeRay
}

/**
 * Open a Ray database synchronously.
 *
 * Use this when you need synchronous initialization (e.g., at module load time).
 * For most cases, prefer the async `ray()` function.
 *
 * @param path - Path to the database file
 * @param options - Database options including schema
 * @returns A Ray database instance
 *
 * @example
 * ```typescript
 * const db = raySync('./my.raydb', {
 *   nodes: [User],
 *   edges: [knows],
 * })
 * ```
 */
export function raySync(path: string, options: RayOptions): NativeRay {
  const nativeOptions = optionsToNative(options)
  return nativeRaySync(path, nativeOptions)
}
