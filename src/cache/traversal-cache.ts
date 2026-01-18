/**
 * Traversal Cache
 *
 * Caches neighbor iteration results to avoid repeated graph traversals.
 * 
 * Optimization: Uses targeted invalidation instead of clearing entire cache.
 * Maintains a reverse index from node to their cache keys for O(1) invalidation.
 * 
 * Improvement: Now tracks both source and destination nodes for correct cache
 * invalidation when any node in a cached traversal result changes.
 */

import type { ETypeID, NodeID } from "../types.ts";
import type { Edge } from "../types.ts";
import { LRUCache } from "../util/lru.ts";

// Use bigint keys for faster comparison and hashing
// Pack: nodeId (53 bits) | etype (10 bits) | direction (1 bit)
// With etype=0x3FF meaning "all"
type TraversalKey = bigint;

interface TraversalCacheConfig {
  maxEntries: number;
  maxNeighborsPerEntry: number;
}

interface CachedNeighbors {
  neighbors: Edge[];
  truncated: boolean; // True if neighbors were truncated due to maxNeighborsPerEntry
}

/**
 * Traversal cache for neighbor lookups
 * 
 * Uses targeted invalidation via reverse index mapping:
 * - nodeKeyIndex: Maps NodeID -> Set<TraversalKey> for O(1) node invalidation
 * 
 * When a node changes, we invalidate:
 * - All outgoing traversals from that node
 * - All incoming traversals to that node
 * - All traversals that include this node in their results (as a destination)
 * 
 * When an edge changes (src -> dst), we invalidate:
 * - Outgoing traversals from src (affected by edge addition/removal)
 * - Incoming traversals to dst (affected by edge addition/removal)
 */
export class TraversalCache {
  private readonly cache: LRUCache<TraversalKey, CachedNeighbors>;
  private readonly maxNeighborsPerEntry: number;
  
  // Reverse index for targeted invalidation
  // Maps NodeID to all cache keys that reference this node (as source OR destination)
  private readonly nodeKeyIndex: Map<NodeID, Set<TraversalKey>> = new Map();
  
  private hits = 0;
  private misses = 0;

  constructor(config: TraversalCacheConfig) {
    this.cache = new LRUCache(config.maxEntries);
    this.maxNeighborsPerEntry = config.maxNeighborsPerEntry;
  }

  /**
   * Get cached neighbors for a node
   *
   * @param nodeId - Source node ID
   * @param etype - Edge type ID, or undefined for all types
   * @param direction - 'out' or 'in'
   * @returns Cached neighbors or undefined if not cached
   */
  get(
    nodeId: NodeID,
    etype: ETypeID | undefined,
    direction: "out" | "in",
  ): CachedNeighbors | undefined {
    const key = this.traversalKey(nodeId, etype, direction);
    const value = this.cache.get(key);
    if (value !== undefined) {
      this.hits++;
      return value;
    }
    this.misses++;
    return undefined;
  }

  /**
   * Set cached neighbors for a node
   *
   * @param nodeId - Source node ID
   * @param etype - Edge type ID, or undefined for all types
   * @param direction - 'out' or 'in'
   * @param neighbors - Array of neighbor edges
   */
  set(
    nodeId: NodeID,
    etype: ETypeID | undefined,
    direction: "out" | "in",
    neighbors: Edge[],
  ): void {
    const key = this.traversalKey(nodeId, etype, direction);
    
    // Truncate if exceeds max neighbors per entry
    let truncated = false;
    let cachedNeighbors = neighbors;
    if (neighbors.length > this.maxNeighborsPerEntry) {
      cachedNeighbors = neighbors.slice(0, this.maxNeighborsPerEntry);
      truncated = true;
    }

    this.cache.set(key, {
      neighbors: cachedNeighbors,
      truncated,
    });
    
    // Track source node for invalidation
    this.addToNodeIndex(nodeId, key);
    
    // Track destination nodes for invalidation
    // This ensures that when a destination node changes, this cache entry is invalidated
    for (const edge of cachedNeighbors) {
      const destId = direction === "out" ? edge.dst : edge.src;
      this.addToNodeIndex(destId, key);
    }
  }

  /**
   * Invalidate all cached traversals for a node (targeted O(k) where k = traversals for node)
   */
  invalidateNode(nodeId: NodeID): void {
    const keys = this.nodeKeyIndex.get(nodeId);
    if (keys) {
      for (const key of keys) {
        this.cache.delete(key);
      }
      this.nodeKeyIndex.delete(nodeId);
    }
  }

  /**
   * Invalidate traversals involving a specific edge (targeted invalidation)
   * 
   * When an edge (src, etype, dst) is added/removed:
   * - Outgoing traversals from src are affected
   * - Incoming traversals to dst are affected
   */
  invalidateEdge(src: NodeID, etype: ETypeID, dst: NodeID): void {
    // Invalidate outgoing traversals from src
    this.invalidateNodeTraversals(src, "out", etype);
    
    // Invalidate incoming traversals to dst
    this.invalidateNodeTraversals(dst, "in", etype);
  }
  
  /**
   * Invalidate specific traversals for a node
   */
  private invalidateNodeTraversals(nodeId: NodeID, direction: "out" | "in", etype: ETypeID): void {
    const keys = this.nodeKeyIndex.get(nodeId);
    if (!keys) return;
    
    const keysToDelete: TraversalKey[] = [];
    
    // Find keys that match this direction and etype (or 'all')
    const specificKey = this.traversalKey(nodeId, etype, direction);
    const allKey = this.traversalKey(nodeId, undefined, direction);
    
    if (keys.has(specificKey)) {
      keysToDelete.push(specificKey);
    }
    if (keys.has(allKey)) {
      keysToDelete.push(allKey);
    }
    
    // Delete matched keys
    for (const key of keysToDelete) {
      this.cache.delete(key);
      keys.delete(key);
    }
    
    // Clean up empty index entries
    if (keys.size === 0) {
      this.nodeKeyIndex.delete(nodeId);
    }
  }

  /**
   * Clear all cached traversals
   */
  clear(): void {
    this.cache.clear();
    this.nodeKeyIndex.clear();
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
      size: this.cache.size,
      maxSize: this.cache.max,
    };
  }

  /**
   * Generate cache key for traversal using bigint packing for faster comparison
   * 
   * Pack: nodeId (53 bits) | etype (10 bits) | direction (1 bit)
   * With etype=0x3FF meaning "all types"
   */
  private traversalKey(
    nodeId: NodeID,
    etype: ETypeID | undefined,
    direction: "out" | "in",
  ): TraversalKey {
    // Use 0x3FF (1023) to represent "all" edge types
    const etypeVal = etype === undefined ? 0x3FFn : BigInt(etype);
    const dirVal = direction === "out" ? 0n : 1n;
    // nodeId << 11 | etype << 1 | direction
    return (BigInt(nodeId) << 11n) | (etypeVal << 1n) | dirVal;
  }
  
  /**
   * Add a key to the node index
   */
  private addToNodeIndex(nodeId: NodeID, key: TraversalKey): void {
    let keys = this.nodeKeyIndex.get(nodeId);
    if (!keys) {
      keys = new Set();
      this.nodeKeyIndex.set(nodeId, keys);
    }
    keys.add(key);
  }
}
