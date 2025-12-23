/**
 * Property Cache
 *
 * Caches node and edge property lookups to avoid repeated delta/snapshot reads.
 * 
 * Optimization: Uses targeted invalidation instead of clearing entire cache.
 * Maintains a reverse index from node/edge to their cache keys for O(1) invalidation.
 */

import type { ETypeID, NodeID, PropKeyID, PropValue } from "../types.ts";
import { LRUCache } from "../util/lru.ts";

// Cache keys
type NodePropKey = string; // Format: `n:${NodeID}:${PropKeyID}`
type EdgePropKey = string; // Format: `e:${NodeID}:${ETypeID}:${NodeID}:${PropKeyID}`

interface PropertyCacheConfig {
  maxNodeProps: number;
  maxEdgeProps: number;
}

/**
 * Property cache for node and edge properties
 * 
 * Uses targeted invalidation via reverse index mapping:
 * - nodeKeyIndex: Maps NodeID -> Set<NodePropKey> for O(1) node invalidation
 * - edgeKeyIndex: Maps "src:etype:dst" -> Set<EdgePropKey> for O(1) edge invalidation
 */
export class PropertyCache {
  private readonly nodeCache: LRUCache<NodePropKey, PropValue | null>;
  private readonly edgeCache: LRUCache<EdgePropKey, PropValue | null>;
  
  // Reverse index for targeted invalidation
  private readonly nodeKeyIndex: Map<NodeID, Set<NodePropKey>> = new Map();
  private readonly edgeKeyIndex: Map<string, Set<EdgePropKey>> = new Map();
  
  private hits = 0;
  private misses = 0;

  constructor(config: PropertyCacheConfig) {
    this.nodeCache = new LRUCache(config.maxNodeProps);
    this.edgeCache = new LRUCache(config.maxEdgeProps);
  }

  /**
   * Get a node property from cache
   */
  getNodeProp(nodeId: NodeID, propKeyId: PropKeyID): PropValue | null | undefined {
    const key = this.nodePropKey(nodeId, propKeyId);
    const value = this.nodeCache.get(key);
    if (value !== undefined) {
      this.hits++;
      return value;
    }
    this.misses++;
    return undefined;
  }

  /**
   * Set a node property in cache
   */
  setNodeProp(
    nodeId: NodeID,
    propKeyId: PropKeyID,
    value: PropValue | null,
  ): void {
    const key = this.nodePropKey(nodeId, propKeyId);
    this.nodeCache.set(key, value);
    
    // Track which keys belong to this node for targeted invalidation
    let keys = this.nodeKeyIndex.get(nodeId);
    if (!keys) {
      keys = new Set();
      this.nodeKeyIndex.set(nodeId, keys);
    }
    keys.add(key);
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
    const key = this.edgePropKey(src, etype, dst, propKeyId);
    const value = this.edgeCache.get(key);
    if (value !== undefined) {
      this.hits++;
      return value;
    }
    this.misses++;
    return undefined;
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
    const key = this.edgePropKey(src, etype, dst, propKeyId);
    this.edgeCache.set(key, value);
    
    // Track which keys belong to this edge for targeted invalidation
    const edgeIndexKey = this.edgeIndexKey(src, etype, dst);
    let keys = this.edgeKeyIndex.get(edgeIndexKey);
    if (!keys) {
      keys = new Set();
      this.edgeKeyIndex.set(edgeIndexKey, keys);
    }
    keys.add(key);
  }

  /**
   * Invalidate all properties for a node (targeted O(k) where k = props for node)
   */
  invalidateNode(nodeId: NodeID): void {
    const keys = this.nodeKeyIndex.get(nodeId);
    if (keys) {
      for (const key of keys) {
        this.nodeCache.delete(key);
      }
      this.nodeKeyIndex.delete(nodeId);
    }
  }

  /**
   * Invalidate a specific edge property (targeted O(k) where k = props for edge)
   */
  invalidateEdge(src: NodeID, etype: ETypeID, dst: NodeID): void {
    const edgeIndexKey = this.edgeIndexKey(src, etype, dst);
    const keys = this.edgeKeyIndex.get(edgeIndexKey);
    if (keys) {
      for (const key of keys) {
        this.edgeCache.delete(key);
      }
      this.edgeKeyIndex.delete(edgeIndexKey);
    }
  }

  /**
   * Clear all cached properties
   */
  clear(): void {
    this.nodeCache.clear();
    this.edgeCache.clear();
    this.nodeKeyIndex.clear();
    this.edgeKeyIndex.clear();
    this.hits = 0;
    this.misses = 0;
  }

  /**
   * Get cache statistics
   */
  getStats() {
    const total = this.hits + this.misses;
    return {
      hits: this.hits,
      misses: this.misses,
      hitRate: total > 0 ? this.hits / total : 0,
      size: this.nodeCache.size + this.edgeCache.size,
      maxSize: this.nodeCache.max + this.edgeCache.max,
    };
  }

  /**
   * Generate cache key for node property
   */
  private nodePropKey(nodeId: NodeID, propKeyId: PropKeyID): NodePropKey {
    return `n:${nodeId}:${propKeyId}`;
  }

  /**
   * Generate cache key for edge property
   */
  private edgePropKey(
    src: NodeID,
    etype: ETypeID,
    dst: NodeID,
    propKeyId: PropKeyID,
  ): EdgePropKey {
    return `e:${src}:${etype}:${dst}:${propKeyId}`;
  }
  
  /**
   * Generate index key for edge (without propKeyId)
   */
  private edgeIndexKey(src: NodeID, etype: ETypeID, dst: NodeID): string {
    return `${src}:${etype}:${dst}`;
  }
}
