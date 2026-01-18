/**
 * Vector Search API for Ray
 * 
 * Provides high-level API for vector similarity search integrated with the Ray API.
 */

import type { NodeID } from "../types.ts";
import type { NodeDef } from "./schema.ts";
import type { NodeRef } from "./builders.ts";
import type { VectorSearchResult, VectorManifest, IvfIndex, IvfConfig } from "../vector/types.ts";
import {
  createVectorStore,
  vectorStoreInsert,
  vectorStoreDelete,
  vectorStoreGet,
  vectorStoreStats,
  vectorStoreClear,
} from "../vector/columnar-store.ts";
import {
  createIvfIndex,
  ivfAddTrainingVectors,
  ivfTrain,
  ivfInsert,
  ivfDelete,
  ivfSearch,
} from "../vector/ivf-index.ts";
import { normalize, validateVector } from "../vector/normalize.ts";
import { getDistanceFunction, distanceToSimilarity } from "../vector/distance.ts";

// ============================================================================
// LRU Cache
// ============================================================================

const DEFAULT_CACHE_MAX_SIZE = 10_000;

/**
 * Simple LRU cache with a maximum size limit
 */
class LRUCache<K, V> {
  private readonly _map = new Map<K, V>();
  private readonly _maxSize: number;

  constructor(maxSize: number = DEFAULT_CACHE_MAX_SIZE) {
    this._maxSize = maxSize;
  }

  get(key: K): V | undefined {
    const value = this._map.get(key);
    if (value !== undefined) {
      // Move to end (most recently used)
      this._map.delete(key);
      this._map.set(key, value);
    }
    return value;
  }

  set(key: K, value: V): void {
    // If key exists, delete it first so it goes to end
    if (this._map.has(key)) {
      this._map.delete(key);
    } else if (this._map.size >= this._maxSize) {
      // Evict oldest entry (first in map)
      const firstKey = this._map.keys().next().value;
      if (firstKey !== undefined) {
        this._map.delete(firstKey);
      }
    }
    this._map.set(key, value);
  }

  has(key: K): boolean {
    return this._map.has(key);
  }

  delete(key: K): boolean {
    return this._map.delete(key);
  }

  clear(): void {
    this._map.clear();
  }

  get size(): number {
    return this._map.size;
  }
}

// ============================================================================
// Types
// ============================================================================

export interface VectorIndexOptions {
  /** Vector dimensions (required) */
  dimensions: number;
  /** Distance metric (default: 'cosine') */
  metric?: 'cosine' | 'euclidean' | 'dot';
  /** Vectors per row group (default: 1024) */
  rowGroupSize?: number;
  /** Vectors per fragment before sealing (default: 100_000) */
  fragmentTargetSize?: number;
  /** Whether to auto-normalize vectors (default: true for cosine) */
  normalize?: boolean;
  /** IVF index configuration */
  ivf?: Partial<IvfConfig>;
  /** Minimum training vectors before index training (default: 1000) */
  trainingThreshold?: number;
  /** Maximum node refs to cache for search results (default: 10_000) */
  cacheMaxSize?: number;
}

export interface SimilarOptions {
  /** Number of results to return */
  k: number;
  /** Minimum similarity threshold (0-1 for cosine) */
  threshold?: number;
  /** Number of clusters to probe for IVF (default: 10) */
  nProbe?: number;
  /** Filter function applied to results */
  filter?: (nodeId: NodeID) => boolean;
}

export interface VectorSearchHit<N extends NodeDef = NodeDef> {
  /** Node reference */
  node: NodeRef<N>;
  /** Distance to query vector (lower is better) */
  distance: number;
  /** Similarity score (0-1 for cosine, higher is better) */
  similarity: number;
}

// ============================================================================
// VectorIndex Class
// ============================================================================

/**
 * VectorIndex - manages vector embeddings for a set of nodes
 * 
 * @example
 * ```ts
 * // Create a vector index for 768-dimensional embeddings
 * const embeddings = new VectorIndex({ dimensions: 768 });
 * 
 * // Add vectors for nodes
 * await embeddings.set(userRef, userEmbedding);
 * 
 * // Find similar nodes
 * const similar = await embeddings.search(queryVector, { k: 10 });
 * for (const hit of similar) {
 *   console.log(hit.node.$key, hit.similarity);
 * }
 * ```
 */
export class VectorIndex {
  private readonly _manifest: VectorManifest;
  private _index: IvfIndex | null = null;
  private readonly _nodeRefCache: LRUCache<NodeID, NodeRef>;
  private readonly _trainingThreshold: number;
  private readonly _ivfConfig: Partial<IvfConfig>;
  private _needsTraining: boolean = true;
  private _isBuilding: boolean = false;

  constructor(options: VectorIndexOptions) {
    const {
      dimensions,
      metric = 'cosine',
      rowGroupSize = 1024,
      fragmentTargetSize = 100_000,
      normalize: shouldNormalize = metric === 'cosine',
      ivf = {},
      trainingThreshold = 1000,
      cacheMaxSize = DEFAULT_CACHE_MAX_SIZE,
    } = options;

    this._manifest = createVectorStore(dimensions, {
      metric,
      rowGroupSize,
      fragmentTargetSize,
      normalize: shouldNormalize,
    });

    this._ivfConfig = ivf;
    this._trainingThreshold = trainingThreshold;
    this._nodeRefCache = new LRUCache(cacheMaxSize);
  }

  /**
   * Set/update a vector for a node
   * 
   * @throws Error if called while buildIndex() is in progress
   */
  set(nodeRef: NodeRef, vector: Float32Array): void {
    if (this._isBuilding) {
      throw new Error("Cannot modify vectors while index is being built");
    }

    if (vector.length !== this._manifest.config.dimensions) {
      throw new Error(
        `Vector dimension mismatch: expected ${this._manifest.config.dimensions}, got ${vector.length}`
      );
    }

    const nodeId = nodeRef.$id;
    
    // Check if we need to delete from index first
    const existingVectorId = this._manifest.nodeIdToVectorId.get(nodeId);
    if (existingVectorId !== undefined && this._index?.trained) {
      const existingVector = vectorStoreGet(this._manifest, nodeId);
      if (existingVector) {
        ivfDelete(this._index, existingVectorId, existingVector, this._manifest.config.dimensions);
      }
    }

    // Insert into store
    const vectorId = vectorStoreInsert(this._manifest, nodeId, vector);

    // Cache the node ref for retrieval
    this._nodeRefCache.set(nodeId, nodeRef);

    // Add to index if trained, otherwise mark for training
    if (this._index?.trained) {
      const storedVector = vectorStoreGet(this._manifest, nodeId);
      if (storedVector) {
        ivfInsert(this._index, vectorId, storedVector, this._manifest.config.dimensions);
      }
    } else {
      this._needsTraining = true;
    }
  }

  /**
   * Get the vector for a node (if any)
   */
  get(nodeRef: NodeRef): Float32Array | null {
    return vectorStoreGet(this._manifest, nodeRef.$id);
  }

  /**
   * Delete the vector for a node
   * 
   * @throws Error if called while buildIndex() is in progress
   */
  delete(nodeRef: NodeRef): boolean {
    if (this._isBuilding) {
      throw new Error("Cannot modify vectors while index is being built");
    }

    const nodeId = nodeRef.$id;
    
    // Remove from index if trained
    if (this._index?.trained) {
      const vectorId = this._manifest.nodeIdToVectorId.get(nodeId);
      const vector = vectorStoreGet(this._manifest, nodeId);
      if (vectorId !== undefined && vector) {
        ivfDelete(this._index, vectorId, vector, this._manifest.config.dimensions);
      }
    }

    // Remove from cache
    this._nodeRefCache.delete(nodeId);

    // Remove from store
    return vectorStoreDelete(this._manifest, nodeId);
  }

  /**
   * Check if a node has a vector
   */
  has(nodeRef: NodeRef): boolean {
    return this._manifest.nodeIdToVectorId.has(nodeRef.$id);
  }

  /**
   * Build/rebuild the IVF index for faster search
   * 
   * Call this after bulk loading vectors, or periodically as vectors are updated.
   * Uses k-means clustering for approximate nearest neighbor search.
   * 
   * Note: Modifications (set/delete) are blocked while building is in progress.
   */
  buildIndex(): void {
    if (this._isBuilding) {
      throw new Error("Index build already in progress");
    }

    this._isBuilding = true;
    try {
      const dimensions = this._manifest.config.dimensions;
      const stats = vectorStoreStats(this._manifest);
      const liveVectors = stats.liveVectors;

      if (liveVectors < this._trainingThreshold) {
        // Not enough vectors for index - will use brute force search
        this._index = null;
        this._needsTraining = false;
        return;
      }

      // Determine number of clusters (sqrt rule, min 16, max 1024)
      const nClusters = Math.min(
        1024,
        Math.max(16, Math.floor(Math.sqrt(liveVectors)))
      );

      // Create new index with the same metric as the store
      this._index = createIvfIndex(dimensions, {
        ...this._ivfConfig,
        nClusters,
        metric: this._manifest.config.metric,
      });

      // Collect training vectors
      const trainingData = new Float32Array(liveVectors * dimensions);
      const vectorIds: number[] = [];
      let idx = 0;

      for (const [nodeId, vectorId] of this._manifest.nodeIdToVectorId) {
        const vector = vectorStoreGet(this._manifest, nodeId);
        if (vector) {
          trainingData.set(vector, idx * dimensions);
          vectorIds.push(vectorId);
          idx++;
        }
      }

      // Train the index
      ivfAddTrainingVectors(this._index, trainingData, dimensions, idx);
      ivfTrain(this._index, dimensions);

      // Insert all vectors into the trained index
      idx = 0;
      for (const [nodeId, vectorId] of this._manifest.nodeIdToVectorId) {
        const vector = vectorStoreGet(this._manifest, nodeId);
        if (vector) {
          ivfInsert(this._index, vectorId, vector, dimensions);
          idx++;
        }
      }

      this._needsTraining = false;
    } finally {
      this._isBuilding = false;
    }
  }

  /**
   * Search for similar vectors
   * 
   * Returns the k most similar nodes to the query vector.
   * Uses IVF index if available, otherwise falls back to brute force.
   */
  search<N extends NodeDef = NodeDef>(
    query: Float32Array,
    options: SimilarOptions
  ): VectorSearchHit<N>[] {
    const { k, threshold, nProbe, filter } = options;
    const dimensions = this._manifest.config.dimensions;

    if (query.length !== dimensions) {
      throw new Error(
        `Query dimension mismatch: expected ${dimensions}, got ${query.length}`
      );
    }

    // Validate query vector
    const validation = validateVector(query);
    if (!validation.valid) {
      throw new Error(`Invalid query vector: ${validation.message}`);
    }

    // Auto-build index if needed and we have enough vectors
    if (this._needsTraining) {
      this.buildIndex();
    }

    let results: VectorSearchResult[];

    if (this._index?.trained) {
      // Use IVF index for approximate search
      results = ivfSearch(this._index, this._manifest, query, k * 2, {
        nProbe,
        filter,
      });
    } else {
      // Brute force search
      results = this.bruteForceSearch(query, k * 2, filter);
    }

    // Apply threshold and limit
    const hits: VectorSearchHit<N>[] = [];
    for (const result of results) {
      if (threshold !== undefined && result.similarity < threshold) {
        continue;
      }

      const nodeRef = this._nodeRefCache.get(result.nodeId);
      if (!nodeRef) continue;

      hits.push({
        node: nodeRef as NodeRef<N>,
        distance: result.distance,
        similarity: result.similarity,
      });

      if (hits.length >= k) break;
    }

    return hits;
  }

  /**
   * Brute force search (fallback when index not available)
   */
  private bruteForceSearch(
    query: Float32Array,
    k: number,
    filter?: (nodeId: NodeID) => boolean
  ): VectorSearchResult[] {
    const metric = this._manifest.config.metric;

    // Normalize query for cosine similarity
    const queryForSearch = metric === 'cosine' ? normalize(query) : query;
    
    // Get the appropriate distance function
    const distanceFn = getDistanceFunction(metric);

    const candidates: VectorSearchResult[] = [];

    for (const [nodeId, vectorId] of this._manifest.nodeIdToVectorId) {
      if (filter) {
        try {
          if (!filter(nodeId)) continue;
        } catch {
          // Filter threw an error - skip this result
          continue;
        }
      }

      const vector = vectorStoreGet(this._manifest, nodeId);
      if (!vector) continue;

      // Compute distance using the appropriate function
      const distance = distanceFn(queryForSearch, vector);

      candidates.push({
        vectorId,
        nodeId,
        distance,
        similarity: distanceToSimilarity(distance, metric),
      });
    }

    // Sort by distance and return top k
    candidates.sort((a, b) => a.distance - b.distance);
    return candidates.slice(0, k);
  }

  /**
   * Get index statistics
   */
  stats(): {
    totalVectors: number;
    liveVectors: number;
    dimensions: number;
    metric: string;
    indexTrained: boolean;
    indexClusters: number | null;
  } {
    const storeStats = vectorStoreStats(this._manifest);
    return {
      totalVectors: storeStats.totalVectors,
      liveVectors: storeStats.liveVectors,
      dimensions: this._manifest.config.dimensions,
      metric: this._manifest.config.metric,
      indexTrained: this._index?.trained ?? false,
      indexClusters: this._index?.config.nClusters ?? null,
    };
  }

  /**
   * Clear all vectors and reset the index
   */
  clear(): void {
    vectorStoreClear(this._manifest);
    this._nodeRefCache.clear();
    this._index = null;
    this._needsTraining = true;
  }
}

// ============================================================================
// Factory Functions
// ============================================================================

/**
 * Create a new vector index
 * 
 * @example
 * ```ts
 * import { createVectorIndex } from '@ray-db/ray';
 * 
 * // Create index for 768-dimensional embeddings (e.g., from OpenAI)
 * const index = createVectorIndex({ dimensions: 768 });
 * 
 * // Or with custom configuration
 * const index = createVectorIndex({
 *   dimensions: 1536,
 *   metric: 'cosine',
 *   trainingThreshold: 500,
 *   ivf: { nProbe: 20 },
 * });
 * ```
 */
export function createVectorIndex(options: VectorIndexOptions): VectorIndex {
  return new VectorIndex(options);
}
