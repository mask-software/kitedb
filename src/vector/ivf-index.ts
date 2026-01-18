/**
 * IVF (Inverted File) index for approximate nearest neighbor search
 *
 * Algorithm:
 * 1. Training: Run k-means to find cluster centroids
 * 2. Insert: Assign each vector to nearest centroid
 * 3. Search: Find nearest centroids, then search their vectors
 *
 * This is more disk-friendly than HNSW and works well with columnar storage.
 */

import type {
  IvfIndex,
  IvfConfig,
  VectorManifest,
  VectorSearchResult,
} from "./types.ts";
import { DEFAULT_IVF_CONFIG } from "./types.ts";
import {
  dotProduct,
  cosineDistance,
  squaredEuclidean,
  distanceToSimilarity,
  getDistanceFunction,
  getBatchDistanceFunction,
  MaxHeap,
} from "./distance.ts";
import { normalize, l2Norm } from "./normalize.ts";
import {
  vectorStoreGetById,
  vectorStoreGetLocation,
  vectorStoreGetNodeId,
} from "./columnar-store.ts";
import { fragmentIsDeleted } from "./fragment.ts";
import type { NodeID } from "../types.ts";

// ============================================================================
// Index Creation
// ============================================================================

/**
 * Create a new IVF index
 *
 * @param dimensions - Number of dimensions per vector
 * @param config - Optional configuration overrides
 */
export function createIvfIndex(
  dimensions: number,
  config?: Partial<IvfConfig>
): IvfIndex {
  const fullConfig: IvfConfig = {
    ...DEFAULT_IVF_CONFIG,
    ...config,
  };

  return {
    config: fullConfig,
    centroids: new Float32Array(fullConfig.nClusters * dimensions),
    invertedLists: new Map(),
    trained: false,
    trainingVectors: new Float32Array(0),
    trainingCount: 0,
  };
}

// ============================================================================
// Training (K-Means)
// ============================================================================

/**
 * Add vectors for training
 *
 * @param index - The IVF index
 * @param vectors - Contiguous vector data
 * @param dimensions - Number of dimensions per vector
 * @param count - Number of vectors to add
 */
export function ivfAddTrainingVectors(
  index: IvfIndex,
  vectors: Float32Array,
  dimensions: number,
  count: number
): void {
  if (index.trained) {
    throw new Error("Index already trained");
  }

  // Expand training buffer if needed
  const currentCapacity =
    (index.trainingVectors?.length ?? 0) / dimensions;
  const neededCapacity = (index.trainingCount ?? 0) + count;

  if (neededCapacity > currentCapacity) {
    const newCapacity = Math.max(neededCapacity, currentCapacity * 2, 1000);
    const newBuffer = new Float32Array(newCapacity * dimensions);
    if (index.trainingVectors) {
      newBuffer.set(index.trainingVectors);
    }
    index.trainingVectors = newBuffer;
  }

  // Copy vectors
  const offset = (index.trainingCount ?? 0) * dimensions;
  index.trainingVectors!.set(vectors.subarray(0, count * dimensions), offset);
  index.trainingCount = (index.trainingCount ?? 0) + count;
}

/**
 * Train the index using k-means clustering
 *
 * @param index - The IVF index
 * @param dimensions - Number of dimensions per vector
 * @param maxIterations - Maximum k-means iterations (default: 25)
 * @param tolerance - Convergence tolerance (default: 1e-4)
 */
export function ivfTrain(
  index: IvfIndex,
  dimensions: number,
  maxIterations: number = 25,
  tolerance: number = 1e-4
): void {
  if (index.trained) return;
  if (!index.trainingVectors || index.trainingCount === 0) {
    throw new Error("No training vectors provided");
  }

  const { nClusters, metric } = index.config;
  const n = index.trainingCount!;
  const vectors = index.trainingVectors!;

  // Need at least as many vectors as clusters
  if (n < nClusters) {
    throw new Error(
      `Not enough training vectors: ${n} < ${nClusters} clusters`
    );
  }

  // Get the appropriate distance function for this metric
  const distanceFn = getDistanceFunction(metric);

  // Initialize centroids using k-means++
  initializeCentroidsKMeansPlusPlus(
    index.centroids,
    vectors,
    dimensions,
    n,
    nClusters,
    distanceFn
  );

  // Run k-means iterations
  const assignments = new Uint32Array(n);
  let prevInertia = Infinity;

  for (let iter = 0; iter < maxIterations; iter++) {
    // Assign vectors to nearest centroids
    let inertia = 0;
    for (let i = 0; i < n; i++) {
      const vecOffset = i * dimensions;
      const vec = vectors.subarray(vecOffset, vecOffset + dimensions);

      let bestCluster = 0;
      let bestDist = Infinity;

      for (let c = 0; c < nClusters; c++) {
        const centOffset = c * dimensions;
        const centroid = index.centroids.subarray(
          centOffset,
          centOffset + dimensions
        );
        const dist = distanceFn(vec, centroid);

        if (dist < bestDist) {
          bestDist = dist;
          bestCluster = c;
        }
      }

      assignments[i] = bestCluster;
      inertia += bestDist;
    }

    // Check for convergence
    const inertiaChange = Math.abs(prevInertia - inertia) / Math.max(inertia, 1);
    if (inertiaChange < tolerance) {
      break;
    }
    prevInertia = inertia;

    // Update centroids
    const clusterSums = new Float32Array(nClusters * dimensions);
    const clusterCounts = new Uint32Array(nClusters);

    for (let i = 0; i < n; i++) {
      const cluster = assignments[i];
      const vecOffset = i * dimensions;
      const sumOffset = cluster * dimensions;

      for (let d = 0; d < dimensions; d++) {
        clusterSums[sumOffset + d] += vectors[vecOffset + d];
      }
      clusterCounts[cluster]++;
    }

    for (let c = 0; c < nClusters; c++) {
      const count = clusterCounts[c];
      if (count === 0) {
        // Reinitialize empty cluster with random vector
        const randIdx = Math.floor(Math.random() * n);
        const randOffset = randIdx * dimensions;
        const centOffset = c * dimensions;
        for (let d = 0; d < dimensions; d++) {
          index.centroids[centOffset + d] = vectors[randOffset + d];
        }
        continue;
      }

      const offset = c * dimensions;
      for (let d = 0; d < dimensions; d++) {
        index.centroids[offset + d] = clusterSums[offset + d] / count;
      }

      // Normalize centroid only for cosine metric
      if (metric === "cosine") {
        const centroid = index.centroids.subarray(offset, offset + dimensions);
        const norm = l2Norm(centroid);
        if (norm > 0) {
          for (let d = 0; d < dimensions; d++) {
            index.centroids[offset + d] /= norm;
          }
        }
      }
    }
  }

  // Initialize inverted lists
  for (let c = 0; c < nClusters; c++) {
    index.invertedLists.set(c, []);
  }

  index.trained = true;

  // Clear training data
  index.trainingVectors = undefined;
  index.trainingCount = undefined;
}

/**
 * K-means++ initialization for better centroid starting positions
 */
function initializeCentroidsKMeansPlusPlus(
  centroids: Float32Array,
  vectors: Float32Array,
  dimensions: number,
  n: number,
  k: number,
  distanceFn: (a: Float32Array, b: Float32Array) => number
): void {
  // First centroid: random vector
  const firstIdx = Math.floor(Math.random() * n);
  const firstOffset = firstIdx * dimensions;
  for (let d = 0; d < dimensions; d++) {
    centroids[d] = vectors[firstOffset + d];
  }

  // Remaining centroids: weighted by distance squared
  const minDists = new Float32Array(n).fill(Infinity);

  for (let c = 1; c < k; c++) {
    // Update min distances to nearest centroid
    const prevCentOffset = (c - 1) * dimensions;
    const prevCentroid = centroids.subarray(
      prevCentOffset,
      prevCentOffset + dimensions
    );

    let totalDist = 0;
    for (let i = 0; i < n; i++) {
      const vecOffset = i * dimensions;
      const vec = vectors.subarray(vecOffset, vecOffset + dimensions);
      const dist = distanceFn(vec, prevCentroid);
      // For k-means++ we need positive weights, so use abs(dist)^2
      const absDist = Math.abs(dist);
      minDists[i] = Math.min(minDists[i], absDist * absDist);
      totalDist += minDists[i];
    }

    // Weighted random selection
    let r = Math.random() * totalDist;
    let selectedIdx = 0;

    for (let i = 0; i < n; i++) {
      r -= minDists[i];
      if (r <= 0) {
        selectedIdx = i;
        break;
      }
    }

    // Copy selected vector to centroid
    const selectedOffset = selectedIdx * dimensions;
    const centOffset = c * dimensions;
    for (let d = 0; d < dimensions; d++) {
      centroids[centOffset + d] = vectors[selectedOffset + d];
    }
  }
}

// ============================================================================
// Insert & Delete
// ============================================================================

/**
 * Insert a vector into the index
 *
 * @param index - The IVF index
 * @param vectorId - Global vector ID
 * @param vector - The vector data
 * @param dimensions - Number of dimensions
 */
export function ivfInsert(
  index: IvfIndex,
  vectorId: number,
  vector: Float32Array,
  dimensions: number
): void {
  if (!index.trained) {
    throw new Error("Index not trained");
  }

  // Find nearest centroid
  const cluster = findNearestCentroid(index, vector, dimensions);

  // Add to inverted list
  const list = index.invertedLists.get(cluster);
  if (list) {
    list.push(vectorId);
  } else {
    index.invertedLists.set(cluster, [vectorId]);
  }
}

/**
 * Delete a vector from the index
 *
 * @param index - The IVF index
 * @param vectorId - Global vector ID
 * @param vector - The vector data (needed to find which cluster)
 * @param dimensions - Number of dimensions
 * @returns true if deleted, false if not found
 */
export function ivfDelete(
  index: IvfIndex,
  vectorId: number,
  vector: Float32Array,
  dimensions: number
): boolean {
  if (!index.trained) return false;

  // Find which cluster it's in
  const cluster = findNearestCentroid(index, vector, dimensions);
  const list = index.invertedLists.get(cluster);

  if (!list) return false;

  const idx = list.indexOf(vectorId);
  if (idx === -1) return false;

  // Remove from list (swap with last for O(1))
  list[idx] = list[list.length - 1];
  list.pop();

  return true;
}

/**
 * Find nearest centroid for a vector
 */
function findNearestCentroid(
  index: IvfIndex,
  vector: Float32Array,
  dimensions: number
): number {
  const { nClusters, metric } = index.config;
  const distanceFn = getDistanceFunction(metric);
  let bestCluster = 0;
  let bestDist = Infinity;

  // Prepare query vector (normalize for cosine metric)
  const queryVec = metric === "cosine" ? normalize(vector) : vector;

  for (let c = 0; c < nClusters; c++) {
    const centOffset = c * dimensions;
    const centroid = index.centroids.subarray(
      centOffset,
      centOffset + dimensions
    );
    const dist = distanceFn(queryVec, centroid);

    if (dist < bestDist) {
      bestDist = dist;
      bestCluster = c;
    }
  }

  return bestCluster;
}

/**
 * Find the top nProbe nearest centroids
 */
function findNearestCentroids(
  index: IvfIndex,
  query: Float32Array,
  dimensions: number,
  nProbe: number
): number[] {
  const { nClusters, metric } = index.config;
  const distanceFn = getDistanceFunction(metric);
  const centroidDists: Array<{ cluster: number; distance: number }> = [];

  for (let c = 0; c < nClusters; c++) {
    const centOffset = c * dimensions;
    const centroid = index.centroids.subarray(
      centOffset,
      centOffset + dimensions
    );
    const dist = distanceFn(query, centroid);
    centroidDists.push({ cluster: c, distance: dist });
  }

  centroidDists.sort((a, b) => a.distance - b.distance);
  return centroidDists.slice(0, nProbe).map((c) => c.cluster);
}

// ============================================================================
// Search
// ============================================================================

/**
 * Search for k nearest neighbors
 *
 * @param index - The IVF index
 * @param manifest - The vector store manifest
 * @param query - Query vector
 * @param k - Number of results to return
 * @param options - Search options
 * @returns Array of search results sorted by distance
 */
export function ivfSearch(
  index: IvfIndex,
  manifest: VectorManifest,
  query: Float32Array,
  k: number,
  options?: {
    nProbe?: number;
    filter?: (nodeId: NodeID) => boolean;
    threshold?: number;
  }
): VectorSearchResult[] {
  if (!index.trained) return [];

  const dimensions = manifest.config.dimensions;
  const nProbe = options?.nProbe ?? index.config.nProbe;
  const metric = index.config.metric; // Use index's metric
  const { rowGroupSize } = manifest.config;

  // Normalize query for cosine metric, use raw for others
  const queryForSearch = metric === "cosine" ? normalize(query) : query;
  
  // Get the appropriate distance function for this metric
  const distanceFn = getDistanceFunction(metric);

  // Find top nProbe nearest centroids (uses index's metric internally)
  const probeClusters = findNearestCentroids(index, queryForSearch, dimensions, nProbe);

  // Build fragment lookup map for O(1) access (avoid .find() in hot loop)
  const fragmentMap = new Map(manifest.fragments.map(f => [f.id, f]));

  // Use max-heap to track top-k candidates
  const heap = new MaxHeap();
  
  // Pre-fetch threshold for inner loop
  const hasFilter = options?.filter !== undefined;
  const hasThreshold = options?.threshold !== undefined;
  const threshold = options?.threshold;

  // Search within selected clusters
  for (const cluster of probeClusters) {
    const vectorIds = index.invertedLists.get(cluster);
    if (!vectorIds || vectorIds.length === 0) continue;

    for (const vectorId of vectorIds) {
      // Get vector location - single map lookup
      const location = manifest.vectorIdToLocation.get(vectorId);
      if (!location) continue;

      // Get fragment with O(1) lookup
      const fragment = fragmentMap.get(location.fragmentId);
      if (!fragment) continue;

      // Check deletion bitmap inline (avoid function call overhead)
      const wordIdx = location.localIndex >>> 5;
      const bitIdx = location.localIndex & 31;
      if (fragment.deletionBitmap[wordIdx] & (1 << bitIdx)) continue;

      // Apply filter early if provided (before computing distance)
      if (hasFilter) {
        const nodeId = manifest.vectorIdToNodeId.get(vectorId);
        if (nodeId !== undefined) {
          try {
            if (!options!.filter!(nodeId)) continue;
          } catch {
            continue;
          }
        }
      }

      // Get vector data inline (avoid function call + redundant lookups)
      const rowGroupIdx = Math.floor(location.localIndex / rowGroupSize);
      const localRowIdx = location.localIndex % rowGroupSize;
      const rowGroup = fragment.rowGroups[rowGroupIdx];
      if (!rowGroup || localRowIdx >= rowGroup.count) continue;
      
      const offset = localRowIdx * dimensions;
      const vec = rowGroup.data.subarray(offset, offset + dimensions);

      // Compute distance using the appropriate function
      const dist = distanceFn(queryForSearch, vec);

      // Apply threshold filter
      if (hasThreshold) {
        const similarity = distanceToSimilarity(dist, metric);
        if (similarity < threshold!) continue;
      }

      // Add to heap - only if better than worst candidate
      if (heap.size < k) {
        heap.push(vectorId, dist);
      } else if (dist < heap.peek()!.distance) {
        heap.pop();
        heap.push(vectorId, dist);
      }
    }
  }

  // Convert to results
  const results = heap.toSortedArray();

  return results.map(({ id: vectorId, distance }) => {
    const nodeId = manifest.vectorIdToNodeId.get(vectorId) ?? 0;
    return {
      vectorId,
      nodeId,
      distance,
      similarity: distanceToSimilarity(distance, metric),
    };
  });
}

/**
 * Search with multiple query vectors
 *
 * @param index - The IVF index
 * @param manifest - The vector store manifest
 * @param queries - Array of query vectors
 * @param k - Number of results to return
 * @param aggregation - How to aggregate scores
 * @param options - Search options
 * @returns Array of search results sorted by aggregated distance
 * @throws Error if queries array is empty
 */
export function ivfSearchMulti(
  index: IvfIndex,
  manifest: VectorManifest,
  queries: Float32Array[],
  k: number,
  aggregation: "min" | "max" | "avg" | "sum",
  options?: {
    nProbe?: number;
    filter?: (nodeId: NodeID) => boolean;
    threshold?: number;
  }
): VectorSearchResult[] {
  // Validate queries array
  if (!queries || queries.length === 0) {
    throw new Error("ivfSearchMulti requires at least one query vector");
  }

  // Get results for each query (with higher k to ensure we have enough)
  const allResults = queries.map((q) =>
    ivfSearch(index, manifest, q, k * 2, options)
  );

  // Aggregate by nodeId
  const aggregated = new Map<
    NodeID,
    { distances: number[]; vectorId: number }
  >();

  for (const results of allResults) {
    for (const result of results) {
      const existing = aggregated.get(result.nodeId);
      if (existing) {
        existing.distances.push(result.distance);
      } else {
        aggregated.set(result.nodeId, {
          distances: [result.distance],
          vectorId: result.vectorId,
        });
      }
    }
  }

  // Compute aggregated score
  const scored: VectorSearchResult[] = [];
  const metric = manifest.config.metric;

  for (const [nodeId, { distances, vectorId }] of aggregated) {
    let distance: number;
    switch (aggregation) {
      case "min":
        distance = Math.min(...distances);
        break;
      case "max":
        distance = Math.max(...distances);
        break;
      case "avg":
        distance = distances.reduce((a, b) => a + b, 0) / distances.length;
        break;
      case "sum":
        distance = distances.reduce((a, b) => a + b, 0);
        break;
    }

    scored.push({
      vectorId,
      nodeId,
      distance,
      similarity: distanceToSimilarity(distance, metric),
    });
  }

  // Sort and return top k
  return scored.sort((a, b) => a.distance - b.distance).slice(0, k);
}

/**
 * Build index from all vectors in the store
 *
 * @param index - The IVF index
 * @param manifest - The vector store manifest
 */
export function ivfBuildFromStore(
  index: IvfIndex,
  manifest: VectorManifest
): void {
  const dimensions = manifest.config.dimensions;
  const { rowGroupSize } = manifest.config;

  // First, collect training vectors
  for (const fragment of manifest.fragments) {
    for (const rowGroup of fragment.rowGroups) {
      ivfAddTrainingVectors(
        index,
        rowGroup.data,
        dimensions,
        rowGroup.count
      );
    }
  }

  // Train the index
  ivfTrain(index, dimensions);

  // Then insert all vectors
  for (const [nodeId, vectorId] of manifest.nodeIdToVectorId) {
    const location = manifest.vectorIdToLocation.get(vectorId);
    if (!location) continue;

    const fragment = manifest.fragments.find(
      (f) => f.id === location.fragmentId
    );
    if (!fragment) continue;

    if (fragmentIsDeleted(fragment, location.localIndex)) continue;

    // Get vector
    const rowGroupIdx = Math.floor(location.localIndex / rowGroupSize);
    const localRowIdx = location.localIndex % rowGroupSize;
    const rowGroup = fragment.rowGroups[rowGroupIdx];
    if (!rowGroup) continue;

    const offset = localRowIdx * dimensions;
    const vector = rowGroup.data.subarray(offset, offset + dimensions);

    ivfInsert(index, vectorId, vector, dimensions);
  }
}

/**
 * Get index statistics
 */
export function ivfStats(index: IvfIndex): {
  trained: boolean;
  nClusters: number;
  totalVectors: number;
  avgVectorsPerCluster: number;
  emptyClusterCount: number;
  minClusterSize: number;
  maxClusterSize: number;
} {
  let total = 0;
  let empty = 0;
  let minSize = Infinity;
  let maxSize = 0;

  for (const [, list] of index.invertedLists) {
    total += list.length;
    if (list.length === 0) {
      empty++;
    }
    minSize = Math.min(minSize, list.length);
    maxSize = Math.max(maxSize, list.length);
  }

  if (index.invertedLists.size === 0) {
    minSize = 0;
  }

  return {
    trained: index.trained,
    nClusters: index.config.nClusters,
    totalVectors: total,
    avgVectorsPerCluster: total / Math.max(index.config.nClusters, 1),
    emptyClusterCount: empty,
    minClusterSize: minSize,
    maxClusterSize: maxSize,
  };
}

/**
 * Clear the index (but keep configuration)
 */
export function ivfClear(index: IvfIndex, dimensions: number): void {
  index.centroids = new Float32Array(index.config.nClusters * dimensions);
  index.invertedLists.clear();
  index.trained = false;
  index.trainingVectors = undefined;
  index.trainingCount = undefined;
}
