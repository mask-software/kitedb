/**
 * Metrics and Observability for RayDB
 * 
 * Provides runtime metrics for monitoring database performance:
 * - Operation counts and latencies
 * - Cache hit/miss rates
 * - Transaction statistics
 * - Memory usage estimates
 */

import type { GraphDB } from "../types.ts";
import { getCacheStats } from "../ray/graph-db/cache-api.ts";
import { stats } from "../ray/graph-db/stats.ts";
import { getSnapshot } from "../ray/graph-db/snapshot-helper.ts";

// ============================================================================
// Types
// ============================================================================

export interface DatabaseMetrics {
  /** Database identity */
  path: string;
  isSingleFile: boolean;
  readOnly: boolean;
  
  /** Data statistics */
  data: DataMetrics;
  
  /** Cache statistics */
  cache: CacheMetrics;
  
  /** MVCC statistics (if enabled) */
  mvcc: MvccMetrics | null;
  
  /** Memory estimates */
  memory: MemoryMetrics;
  
  /** Timestamp when metrics were collected */
  collectedAt: Date;
}

export interface DataMetrics {
  /** Total nodes (snapshot + delta) */
  nodeCount: number;
  /** Total edges (snapshot + delta) */
  edgeCount: number;
  /** Nodes created in delta (uncommitted to snapshot) */
  deltaNodesCreated: number;
  /** Nodes deleted in delta */
  deltaNodesDeleted: number;
  /** Edges added in delta */
  deltaEdgesAdded: number;
  /** Edges deleted in delta */
  deltaEdgesDeleted: number;
  /** Current snapshot generation */
  snapshotGeneration: bigint;
  /** Maximum node ID ever allocated */
  maxNodeId: number;
  /** Schema counts */
  schemaLabels: number;
  schemaEtypes: number;
  schemaPropKeys: number;
}

export interface CacheMetrics {
  enabled: boolean;
  propertyCache: CacheLayerMetrics;
  traversalCache: CacheLayerMetrics;
  queryCache: CacheLayerMetrics;
}

export interface CacheLayerMetrics {
  hits: number;
  misses: number;
  hitRate: number;
  size: number;
  maxSize: number;
  utilizationPercent: number;
}

export interface MvccMetrics {
  enabled: boolean;
  activeTransactions: number;
  versionsPruned: number;
  gcRuns: number;
  minActiveTimestamp: bigint;
}

export interface MemoryMetrics {
  /** Estimated delta memory usage in bytes */
  deltaEstimateBytes: number;
  /** Estimated cache memory usage in bytes */
  cacheEstimateBytes: number;
  /** Snapshot file size in bytes (if loaded) */
  snapshotBytes: number;
  /** Total estimated memory */
  totalEstimateBytes: number;
}

// ============================================================================
// Metric Collection
// ============================================================================

/**
 * Collect comprehensive metrics from a database
 * 
 * @example
 * ```ts
 * const metrics = collectMetrics(db);
 * console.log(`Nodes: ${metrics.data.nodeCount}`);
 * console.log(`Cache hit rate: ${(metrics.cache.propertyCache.hitRate * 100).toFixed(1)}%`);
 * ```
 */
export function collectMetrics(db: GraphDB): DatabaseMetrics {
  const dbStats = stats(db);
  const cacheStats = getCacheStats(db);
  const snapshot = getSnapshot(db);
  const delta = db._delta;
  
  // Calculate cache hit rates
  const calcHitRate = (hits: number, misses: number) => {
    const total = hits + misses;
    return total > 0 ? hits / total : 0;
  };
  
  // Estimate delta memory (rough approximation)
  const estimateDeltaMemory = (): number => {
    let bytes = 0;
    
    // Created nodes: ~100 bytes per node base + props
    bytes += delta.createdNodes.size * 100;
    
    // Deleted nodes: ~8 bytes per entry
    bytes += delta.deletedNodes.size * 8;
    
    // Modified nodes
    bytes += delta.modifiedNodes.size * 100;
    
    // Edge patches: ~24 bytes per patch
    for (const patches of delta.outAdd.values()) {
      bytes += patches.length * 24;
    }
    for (const patches of delta.outDel.values()) {
      bytes += patches.length * 24;
    }
    for (const patches of delta.inAdd.values()) {
      bytes += patches.length * 24;
    }
    for (const patches of delta.inDel.values()) {
      bytes += patches.length * 24;
    }
    
    // Edge props
    bytes += delta.edgeProps.size * 50;
    
    // Key index
    bytes += delta.keyIndex.size * 40;
    
    return bytes;
  };
  
  // Estimate cache memory
  const estimateCacheMemory = (): number => {
    if (!cacheStats) return 0;
    
    // Rough estimate: 100 bytes per property cache entry
    // 200 bytes per traversal entry, 500 bytes per query entry
    return (
      cacheStats.propertyCache.size * 100 +
      cacheStats.traversalCache.size * 200 +
      cacheStats.queryCache.size * 500
    );
  };
  
  // Get snapshot size
  const snapshotBytes = snapshot ? Number(snapshot.header.numNodes) * 50 + Number(snapshot.header.numEdges) * 20 : 0;
  
  const deltaBytes = estimateDeltaMemory();
  const cacheBytes = estimateCacheMemory();
  
  // Build cache metrics
  const buildCacheLayerMetrics = (layer: { hits: number; misses: number; size: number; maxSize: number }): CacheLayerMetrics => ({
    hits: layer.hits,
    misses: layer.misses,
    hitRate: calcHitRate(layer.hits, layer.misses),
    size: layer.size,
    maxSize: layer.maxSize,
    utilizationPercent: layer.maxSize > 0 ? (layer.size / layer.maxSize) * 100 : 0,
  });
  
  // MVCC metrics
  let mvccMetrics: MvccMetrics | null = null;
  if (dbStats.mvccStats) {
    mvccMetrics = {
      enabled: true,
      activeTransactions: dbStats.mvccStats.activeTransactions,
      versionsPruned: Number(dbStats.mvccStats.versionsPruned),
      gcRuns: dbStats.mvccStats.gcRuns,
      minActiveTimestamp: dbStats.mvccStats.minActiveTs,
    };
  }
  
  return {
    path: db.path,
    isSingleFile: db._isSingleFile,
    readOnly: db.readOnly,
    
    data: {
      nodeCount: Number(dbStats.snapshotNodes) + dbStats.deltaNodesCreated - dbStats.deltaNodesDeleted,
      edgeCount: Number(dbStats.snapshotEdges) + dbStats.deltaEdgesAdded - dbStats.deltaEdgesDeleted,
      deltaNodesCreated: dbStats.deltaNodesCreated,
      deltaNodesDeleted: dbStats.deltaNodesDeleted,
      deltaEdgesAdded: dbStats.deltaEdgesAdded,
      deltaEdgesDeleted: dbStats.deltaEdgesDeleted,
      snapshotGeneration: dbStats.snapshotGen,
      maxNodeId: dbStats.snapshotMaxNodeId,
      schemaLabels: delta.newLabels.size,
      schemaEtypes: delta.newEtypes.size,
      schemaPropKeys: delta.newPropkeys.size,
    },
    
    cache: cacheStats ? {
      enabled: true,
      propertyCache: buildCacheLayerMetrics(cacheStats.propertyCache),
      traversalCache: buildCacheLayerMetrics(cacheStats.traversalCache),
      queryCache: buildCacheLayerMetrics(cacheStats.queryCache),
    } : {
      enabled: false,
      propertyCache: { hits: 0, misses: 0, hitRate: 0, size: 0, maxSize: 0, utilizationPercent: 0 },
      traversalCache: { hits: 0, misses: 0, hitRate: 0, size: 0, maxSize: 0, utilizationPercent: 0 },
      queryCache: { hits: 0, misses: 0, hitRate: 0, size: 0, maxSize: 0, utilizationPercent: 0 },
    },
    
    mvcc: mvccMetrics,
    
    memory: {
      deltaEstimateBytes: deltaBytes,
      cacheEstimateBytes: cacheBytes,
      snapshotBytes,
      totalEstimateBytes: deltaBytes + cacheBytes + snapshotBytes,
    },
    
    collectedAt: new Date(),
  };
}

/**
 * Format metrics as a human-readable string
 */
export function formatMetrics(metrics: DatabaseMetrics): string {
  const lines: string[] = [];
  
  lines.push(`=== RayDB Metrics ===`);
  lines.push(`Path: ${metrics.path}`);
  lines.push(`Type: ${metrics.isSingleFile ? 'Single-file' : 'Multi-file'} (${metrics.readOnly ? 'read-only' : 'read-write'})`);
  lines.push(``);
  
  lines.push(`--- Data ---`);
  lines.push(`Nodes: ${metrics.data.nodeCount.toLocaleString()}`);
  lines.push(`Edges: ${metrics.data.edgeCount.toLocaleString()}`);
  lines.push(`Delta: +${metrics.data.deltaNodesCreated} -${metrics.data.deltaNodesDeleted} nodes, +${metrics.data.deltaEdgesAdded} -${metrics.data.deltaEdgesDeleted} edges`);
  lines.push(`Schema: ${metrics.data.schemaLabels} labels, ${metrics.data.schemaEtypes} edge types, ${metrics.data.schemaPropKeys} prop keys`);
  lines.push(``);
  
  lines.push(`--- Cache ---`);
  if (metrics.cache.enabled) {
    lines.push(`Property: ${(metrics.cache.propertyCache.hitRate * 100).toFixed(1)}% hit rate (${metrics.cache.propertyCache.size}/${metrics.cache.propertyCache.maxSize})`);
    lines.push(`Traversal: ${(metrics.cache.traversalCache.hitRate * 100).toFixed(1)}% hit rate (${metrics.cache.traversalCache.size}/${metrics.cache.traversalCache.maxSize})`);
    lines.push(`Query: ${(metrics.cache.queryCache.hitRate * 100).toFixed(1)}% hit rate (${metrics.cache.queryCache.size}/${metrics.cache.queryCache.maxSize})`);
  } else {
    lines.push(`Cache disabled`);
  }
  lines.push(``);
  
  if (metrics.mvcc) {
    lines.push(`--- MVCC ---`);
    lines.push(`Active transactions: ${metrics.mvcc.activeTransactions}`);
    lines.push(`Versions pruned: ${metrics.mvcc.versionsPruned}`);
    lines.push(`GC runs: ${metrics.mvcc.gcRuns}`);
    lines.push(``);
  }
  
  lines.push(`--- Memory (estimated) ---`);
  lines.push(`Delta: ${formatBytes(metrics.memory.deltaEstimateBytes)}`);
  lines.push(`Cache: ${formatBytes(metrics.memory.cacheEstimateBytes)}`);
  lines.push(`Snapshot: ${formatBytes(metrics.memory.snapshotBytes)}`);
  lines.push(`Total: ${formatBytes(metrics.memory.totalEstimateBytes)}`);
  
  return lines.join('\n');
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

/**
 * Export metrics as JSON (for monitoring systems)
 */
export function metricsToJSON(metrics: DatabaseMetrics): string {
  return JSON.stringify(metrics, (key, value) => {
    // Convert BigInt to string for JSON serialization
    if (typeof value === 'bigint') {
      return value.toString();
    }
    return value;
  }, 2);
}

/**
 * Create a simple health check result
 */
export interface HealthCheckResult {
  healthy: boolean;
  checks: {
    name: string;
    passed: boolean;
    message: string;
  }[];
}

/**
 * Perform a basic health check on the database
 */
export function healthCheck(db: GraphDB): HealthCheckResult {
  const checks: HealthCheckResult['checks'] = [];
  
  // Check 1: Database is open
  checks.push({
    name: 'database_open',
    passed: true, // If we can call this, it's open
    message: 'Database handle is valid',
  });
  
  // Check 2: Delta is not excessively large
  const delta = db._delta;
  const deltaSize = delta.createdNodes.size + delta.deletedNodes.size + 
                    delta.modifiedNodes.size + delta.outAdd.size + delta.inAdd.size;
  const deltaOk = deltaSize < 100000; // Warn if delta > 100k entries
  checks.push({
    name: 'delta_size',
    passed: deltaOk,
    message: deltaOk 
      ? `Delta size is reasonable (${deltaSize} entries)` 
      : `Delta is large (${deltaSize} entries) - consider checkpointing`,
  });
  
  // Check 3: Cache hit rate (if cache enabled)
  const cacheStats = getCacheStats(db);
  if (cacheStats) {
    const totalHits = cacheStats.propertyCache.hits + cacheStats.traversalCache.hits;
    const totalMisses = cacheStats.propertyCache.misses + cacheStats.traversalCache.misses;
    const hitRate = totalHits + totalMisses > 0 ? totalHits / (totalHits + totalMisses) : 1;
    const cacheOk = hitRate > 0.5 || (totalHits + totalMisses) < 100; // OK if hit rate > 50% or not enough data
    checks.push({
      name: 'cache_efficiency',
      passed: cacheOk,
      message: cacheOk
        ? `Cache hit rate: ${(hitRate * 100).toFixed(1)}%`
        : `Low cache hit rate: ${(hitRate * 100).toFixed(1)}% - consider adjusting cache size`,
    });
  }
  
  // Check 4: Not read-only (optional warning)
  if (db.readOnly) {
    checks.push({
      name: 'write_access',
      passed: true, // Not a failure, just informational
      message: 'Database is read-only',
    });
  }
  
  const allPassed = checks.every(c => c.passed);
  
  return {
    healthy: allPassed,
    checks,
  };
}
