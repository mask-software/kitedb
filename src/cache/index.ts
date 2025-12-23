/**
 * Cache Manager
 *
 * Coordinates property cache, traversal cache, query cache, and key lookup cache.
 * Provides unified invalidation and statistics APIs.
 */

import type {
  CacheOptions,
  CacheStats,
  ETypeID,
  NodeID,
  PropKeyID,
  PropValue,
} from "../types.ts";
import type { Edge } from "../types.ts";
import { PropertyCache } from "./property-cache.ts";
import { QueryCache } from "./query-cache.ts";
import { TraversalCache } from "./traversal-cache.ts";
import { LRUCache } from "../util/lru.ts";

const DEFAULT_PROPERTY_CACHE_CONFIG = {
  maxNodeProps: 10000,
  maxEdgeProps: 10000,
};

const DEFAULT_TRAVERSAL_CACHE_CONFIG = {
  maxEntries: 5000,
  maxNeighborsPerEntry: 100,
};

const DEFAULT_QUERY_CACHE_CONFIG = {
  maxEntries: 1000,
};

const DEFAULT_KEY_CACHE_SIZE = 10000;

/**
 * Cache manager coordinating all caches
 */
export class CacheManager {
  private readonly propertyCache: PropertyCache;
  private readonly traversalCache: TraversalCache;
  private readonly queryCache: QueryCache;
  
  // Key lookup cache: string key -> NodeID (or null for negative caches)
  private readonly keyCache: LRUCache<string, NodeID | null>;
  
  private readonly enabled: boolean;

  constructor(options: CacheOptions = {}) {
    this.enabled = options.enabled !== false; // Default to enabled

    if (this.enabled) {
      const propConfig = {
        ...DEFAULT_PROPERTY_CACHE_CONFIG,
        ...options.propertyCache,
      };
      const travConfig = {
        ...DEFAULT_TRAVERSAL_CACHE_CONFIG,
        ...options.traversalCache,
      };
      const queryConfig = {
        ...DEFAULT_QUERY_CACHE_CONFIG,
        ...options.queryCache,
      };

      this.propertyCache = new PropertyCache(propConfig);
      this.traversalCache = new TraversalCache(travConfig);
      this.queryCache = new QueryCache(queryConfig);
      this.keyCache = new LRUCache(DEFAULT_KEY_CACHE_SIZE);
    } else {
      // Create disabled caches (no-ops)
      this.propertyCache = new PropertyCache(DEFAULT_PROPERTY_CACHE_CONFIG);
      this.traversalCache = new TraversalCache(DEFAULT_TRAVERSAL_CACHE_CONFIG);
      this.queryCache = new QueryCache(DEFAULT_QUERY_CACHE_CONFIG);
      this.keyCache = new LRUCache(DEFAULT_KEY_CACHE_SIZE);
    }
  }

  // ============================================================================
  // Property Cache API
  // ============================================================================

  /**
   * Get a node property from cache
   */
  getNodeProp(
    nodeId: NodeID,
    propKeyId: PropKeyID,
  ): PropValue | null | undefined {
    if (!this.enabled) return undefined;
    return this.propertyCache.getNodeProp(nodeId, propKeyId);
  }

  /**
   * Set a node property in cache
   */
  setNodeProp(
    nodeId: NodeID,
    propKeyId: PropKeyID,
    value: PropValue | null,
  ): void {
    if (!this.enabled) return;
    this.propertyCache.setNodeProp(nodeId, propKeyId, value);
  }

  /**
   * Get an edge property from cache
   */
  getEdgeProp(
    src: NodeID,
    etype: ETypeID,
    dst: NodeID,
    propKeyId: PropKeyID,
  ): PropValue | null | undefined {
    if (!this.enabled) return undefined;
    return this.propertyCache.getEdgeProp(src, etype, dst, propKeyId);
  }

  /**
   * Set an edge property in cache
   */
  setEdgeProp(
    src: NodeID,
    etype: ETypeID,
    dst: NodeID,
    propKeyId: PropKeyID,
    value: PropValue | null,
  ): void {
    if (!this.enabled) return;
    this.propertyCache.setEdgeProp(src, etype, dst, propKeyId, value);
  }

  // ============================================================================
  // Traversal Cache API
  // ============================================================================

  /**
   * Get cached neighbors for a node
   */
  getTraversal(
    nodeId: NodeID,
    etype: ETypeID | undefined,
    direction: "out" | "in",
  ): { neighbors: Edge[]; truncated: boolean } | undefined {
    if (!this.enabled) return undefined;
    return this.traversalCache.get(nodeId, etype, direction);
  }

  /**
   * Set cached neighbors for a node
   */
  setTraversal(
    nodeId: NodeID,
    etype: ETypeID | undefined,
    direction: "out" | "in",
    neighbors: Edge[],
  ): void {
    if (!this.enabled) return;
    this.traversalCache.set(nodeId, etype, direction, neighbors);
  }

  // ============================================================================
  // Query Cache API
  // ============================================================================

  /**
   * Get a cached query result
   */
  getQuery<T>(queryKey: string): T | undefined {
    if (!this.enabled) return undefined;
    return this.queryCache.get<T>(queryKey);
  }

  /**
   * Set a query result in cache
   */
  setQuery<T>(queryKey: string, value: T): void {
    if (!this.enabled) return;
    this.queryCache.set(queryKey, value);
  }

  /**
   * Generate a cache key from query parameters
   */
  generateQueryKey(params: Record<string, unknown> | string): string {
    return QueryCache.generateKey(params);
  }
  
  // ============================================================================
  // Key Lookup Cache API
  // ============================================================================
  
  /**
   * Get a node ID from cache by key
   * Returns undefined if not cached, null if key was looked up but not found
   */
  getNodeByKey(key: string): NodeID | null | undefined {
    if (!this.enabled) return undefined;
    return this.keyCache.get(key);
  }
  
  /**
   * Set a node ID in cache by key
   * Pass null to cache a "not found" result
   */
  setNodeByKey(key: string, nodeId: NodeID | null): void {
    if (!this.enabled) return;
    this.keyCache.set(key, nodeId);
  }
  
  /**
   * Invalidate a cached key lookup
   */
  invalidateKey(key: string): void {
    if (!this.enabled) return;
    this.keyCache.delete(key);
  }

  // ============================================================================
  // Invalidation API
  // ============================================================================

  /**
   * Invalidate all caches for a node
   */
  invalidateNode(nodeId: NodeID): void {
    if (!this.enabled) return;
    this.propertyCache.invalidateNode(nodeId);
    this.traversalCache.invalidateNode(nodeId);
    // Query cache is not invalidated by node (queries are content-addressed)
    // Key cache is not invalidated here - handled by invalidateKey()
  }

  /**
   * Invalidate caches for a specific edge
   */
  invalidateEdge(src: NodeID, etype: ETypeID, dst: NodeID): void {
    if (!this.enabled) return;
    this.propertyCache.invalidateEdge(src, etype, dst);
    this.traversalCache.invalidateEdge(src, etype, dst);
    // Query cache is not invalidated by edge (queries are content-addressed)
  }

  /**
   * Clear all caches
   */
  clear(): void {
    if (!this.enabled) return;
    this.propertyCache.clear();
    this.traversalCache.clear();
    this.queryCache.clear();
    this.keyCache.clear();
  }

  /**
   * Clear only query cache (useful for manual invalidation)
   */
  clearQueryCache(): void {
    if (!this.enabled) return;
    this.queryCache.clear();
  }
  
  /**
   * Clear only key cache (useful after checkpoint)
   */
  clearKeyCache(): void {
    if (!this.enabled) return;
    this.keyCache.clear();
  }

  // ============================================================================
  // Statistics API
  // ============================================================================

  /**
   * Check if cache is enabled
   */
  isEnabled(): boolean {
    return this.enabled;
  }

  /**
   * Get statistics for all caches
   */
  getStats(): CacheStats {
    const propStats = this.propertyCache.getStats();
    const travStats = this.traversalCache.getStats();
    const queryStats = this.queryCache.getStats();

    return {
      propertyCache: {
        hits: propStats.hits,
        misses: propStats.misses,
        size: propStats.size,
        maxSize: propStats.maxSize,
      },
      traversalCache: {
        hits: travStats.hits,
        misses: travStats.misses,
        size: travStats.size,
        maxSize: travStats.maxSize,
      },
      queryCache: {
        hits: queryStats.hits,
        misses: queryStats.misses,
        size: queryStats.size,
        maxSize: queryStats.maxSize,
      },
    };
  }
}

