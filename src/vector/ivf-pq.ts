/**
 * IVF-PQ: Combined Inverted File Index with Product Quantization
 *
 * This combines IVF (for coarse clustering) with PQ (for fast distance computation).
 * It's the standard approach used by FAISS and other high-performance vector DBs.
 *
 * Architecture:
 * 1. IVF partitions vectors into clusters using coarse centroids
 * 2. PQ compresses residuals (vector - centroid) for each cluster
 * 3. Search: find nearest clusters, then use ADC on PQ codes
 *
 * This provides:
 * - Fast coarse search (IVF centroid comparison)
 * - Fast fine search (PQ table lookups instead of full distance)
 * - Memory efficiency (PQ codes instead of full vectors)
 */

import type { VectorManifest, VectorSearchResult, IvfConfig } from "./types.ts";
import { DEFAULT_IVF_CONFIG } from "./types.ts";
import { getDistanceFunction, distanceToSimilarity, MaxHeap } from "./distance.ts";
import { normalize } from "./normalize.ts";
import {
  createPQIndex,
  pqTrain,
  pqEncode,
  pqBuildDistanceTable,
  pqDistanceADC,
  type PQIndex,
  type PQConfig,
  DEFAULT_PQ_CONFIG,
} from "./pq.ts";
import type { NodeID } from "../types.ts";

// ============================================================================
// Types
// ============================================================================

export interface IvfPqConfig extends IvfConfig {
  /** PQ configuration */
  pq: PQConfig;
  /** Whether to use residual encoding (recommended for better accuracy) */
  useResiduals: boolean;
}

export interface IvfPqIndex {
  config: IvfPqConfig;
  /** IVF centroids: nClusters * dimensions */
  centroids: Float32Array;
  /** Inverted lists: cluster -> array of vector IDs */
  invertedLists: Map<number, number[]>;
  /** PQ codes for each vector: vectorId -> Uint8Array of codes */
  pqCodes: Map<number, Uint8Array>;
  /** PQ index (shared codebook trained on residuals) */
  pqIndex: PQIndex;
  /** Pre-computed squared distances between IVF centroids for faster search */
  centroidDistances: Float32Array | null;
  /** Whether the index is trained */
  trained: boolean;
  /** Training data (cleared after training) */
  trainingVectors?: Float32Array;
  trainingCount?: number;
}

export const DEFAULT_IVF_PQ_CONFIG: IvfPqConfig = {
  ...DEFAULT_IVF_CONFIG,
  pq: DEFAULT_PQ_CONFIG,
  useResiduals: true,
};

// ============================================================================
// Index Creation
// ============================================================================

/**
 * Create a new IVF-PQ index
 */
export function createIvfPqIndex(
  dimensions: number,
  config?: Partial<IvfPqConfig>
): IvfPqIndex {
  const fullConfig: IvfPqConfig = {
    ...DEFAULT_IVF_PQ_CONFIG,
    ...config,
    pq: { ...DEFAULT_PQ_CONFIG, ...config?.pq },
  };

  return {
    config: fullConfig,
    centroids: new Float32Array(fullConfig.nClusters * dimensions),
    invertedLists: new Map(),
    pqCodes: new Map(),
    pqIndex: createPQIndex(dimensions, fullConfig.pq),
    centroidDistances: null,
    trained: false,
  };
}

// ============================================================================
// Training
// ============================================================================

/**
 * Add vectors for training
 */
export function ivfPqAddTrainingVectors(
  index: IvfPqIndex,
  vectors: Float32Array,
  dimensions: number,
  count: number
): void {
  if (index.trained) {
    throw new Error("Index already trained");
  }

  const currentCapacity = (index.trainingVectors?.length ?? 0) / dimensions;
  const neededCapacity = (index.trainingCount ?? 0) + count;

  if (neededCapacity > currentCapacity) {
    const newCapacity = Math.max(neededCapacity, currentCapacity * 2, 1000);
    const newBuffer = new Float32Array(newCapacity * dimensions);
    if (index.trainingVectors) {
      newBuffer.set(index.trainingVectors);
    }
    index.trainingVectors = newBuffer;
  }

  const offset = (index.trainingCount ?? 0) * dimensions;
  index.trainingVectors!.set(vectors.subarray(0, count * dimensions), offset);
  index.trainingCount = (index.trainingCount ?? 0) + count;
}

/**
 * Train the IVF-PQ index
 */
export function ivfPqTrain(
  index: IvfPqIndex,
  dimensions: number,
  maxIterations: number = 25
): void {
  if (index.trained) return;
  if (!index.trainingVectors || index.trainingCount === 0) {
    throw new Error("No training vectors provided");
  }

  const { nClusters, metric, useResiduals } = index.config;
  const n = index.trainingCount!;
  const vectors = index.trainingVectors!;

  if (n < nClusters) {
    throw new Error(`Not enough training vectors: ${n} < ${nClusters} clusters`);
  }

  const distanceFn = getDistanceFunction(metric);

  // Step 1: Train IVF centroids with k-means
  initializeCentroidsKMeansPlusPlus(
    index.centroids,
    vectors,
    dimensions,
    n,
    nClusters,
    distanceFn
  );

  const assignments = new Uint32Array(n);

  for (let iter = 0; iter < maxIterations; iter++) {
    // Assign vectors to nearest centroids
    for (let i = 0; i < n; i++) {
      const vecOffset = i * dimensions;
      const vec = vectors.subarray(vecOffset, vecOffset + dimensions);

      let bestCluster = 0;
      let bestDist = Infinity;

      for (let c = 0; c < nClusters; c++) {
        const centOffset = c * dimensions;
        const centroid = index.centroids.subarray(centOffset, centOffset + dimensions);
        const dist = distanceFn(vec, centroid);

        if (dist < bestDist) {
          bestDist = dist;
          bestCluster = c;
        }
      }
      assignments[i] = bestCluster;
    }

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
      if (count === 0) continue;

      const offset = c * dimensions;
      for (let d = 0; d < dimensions; d++) {
        index.centroids[offset + d] = clusterSums[offset + d] / count;
      }
    }
  }

  // Step 2: Compute residuals and train PQ
  let pqTrainingData: Float32Array;

  if (useResiduals) {
    // Compute residuals: vector - assigned_centroid
    pqTrainingData = new Float32Array(n * dimensions);
    for (let i = 0; i < n; i++) {
      const cluster = assignments[i];
      const vecOffset = i * dimensions;
      const centOffset = cluster * dimensions;

      for (let d = 0; d < dimensions; d++) {
        pqTrainingData[vecOffset + d] =
          vectors[vecOffset + d] - index.centroids[centOffset + d];
      }
    }
  } else {
    pqTrainingData = vectors;
  }

  // Train PQ on residuals
  pqTrain(index.pqIndex, pqTrainingData, n);

  // Step 3: Pre-compute centroid distances for faster search
  index.centroidDistances = new Float32Array(nClusters * nClusters);
  for (let i = 0; i < nClusters; i++) {
    const ci = index.centroids.subarray(i * dimensions, (i + 1) * dimensions);
    for (let j = i; j < nClusters; j++) {
      const cj = index.centroids.subarray(j * dimensions, (j + 1) * dimensions);
      const dist = distanceFn(ci, cj);
      index.centroidDistances[i * nClusters + j] = dist;
      index.centroidDistances[j * nClusters + i] = dist;
    }
  }

  // Initialize inverted lists
  for (let c = 0; c < nClusters; c++) {
    index.invertedLists.set(c, []);
  }

  index.trained = true;
  index.trainingVectors = undefined;
  index.trainingCount = undefined;
}

function initializeCentroidsKMeansPlusPlus(
  centroids: Float32Array,
  vectors: Float32Array,
  dimensions: number,
  n: number,
  k: number,
  distanceFn: (a: Float32Array, b: Float32Array) => number
): void {
  const firstIdx = Math.floor(Math.random() * n);
  const firstOffset = firstIdx * dimensions;
  for (let d = 0; d < dimensions; d++) {
    centroids[d] = vectors[firstOffset + d];
  }

  const minDists = new Float32Array(n).fill(Infinity);

  for (let c = 1; c < k; c++) {
    const prevCentOffset = (c - 1) * dimensions;
    const prevCentroid = centroids.subarray(prevCentOffset, prevCentOffset + dimensions);

    let totalDist = 0;
    for (let i = 0; i < n; i++) {
      const vecOffset = i * dimensions;
      const vec = vectors.subarray(vecOffset, vecOffset + dimensions);
      const dist = distanceFn(vec, prevCentroid);
      const absDist = Math.abs(dist);
      minDists[i] = Math.min(minDists[i], absDist * absDist);
      totalDist += minDists[i];
    }

    let r = Math.random() * totalDist;
    let selectedIdx = 0;

    for (let i = 0; i < n; i++) {
      r -= minDists[i];
      if (r <= 0) {
        selectedIdx = i;
        break;
      }
    }

    const selectedOffset = selectedIdx * dimensions;
    const centOffset = c * dimensions;
    for (let d = 0; d < dimensions; d++) {
      centroids[centOffset + d] = vectors[selectedOffset + d];
    }
  }
}

// ============================================================================
// Insert
// ============================================================================

/**
 * Insert a vector into the IVF-PQ index
 */
export function ivfPqInsert(
  index: IvfPqIndex,
  vectorId: number,
  vector: Float32Array,
  dimensions: number
): void {
  if (!index.trained) {
    throw new Error("Index not trained");
  }

  const { nClusters, metric, useResiduals } = index.config;
  const distanceFn = getDistanceFunction(metric);

  // Find nearest centroid
  let bestCluster = 0;
  let bestDist = Infinity;

  const queryVec = metric === "cosine" ? normalize(vector) : vector;

  for (let c = 0; c < nClusters; c++) {
    const centOffset = c * dimensions;
    const centroid = index.centroids.subarray(centOffset, centOffset + dimensions);
    const dist = distanceFn(queryVec, centroid);

    if (dist < bestDist) {
      bestDist = dist;
      bestCluster = c;
    }
  }

  // Compute residual or use raw vector
  let vectorToEncode: Float32Array;
  if (useResiduals) {
    vectorToEncode = new Float32Array(dimensions);
    const centOffset = bestCluster * dimensions;
    for (let d = 0; d < dimensions; d++) {
      vectorToEncode[d] = queryVec[d] - index.centroids[centOffset + d];
    }
  } else {
    vectorToEncode = queryVec;
  }

  // Encode with PQ
  const codes = encodeSingleVector(index.pqIndex, vectorToEncode);

  // Add to inverted list
  const list = index.invertedLists.get(bestCluster);
  if (list) {
    list.push(vectorId);
  } else {
    index.invertedLists.set(bestCluster, [vectorId]);
  }

  // Store PQ codes
  index.pqCodes.set(vectorId, codes);
}

function encodeSingleVector(pqIndex: PQIndex, vector: Float32Array): Uint8Array {
  const { numSubspaces, numCentroids } = pqIndex.config;
  const { subspaceDims } = pqIndex;

  const codes = new Uint8Array(numSubspaces);

  for (let m = 0; m < numSubspaces; m++) {
    const subOffset = m * subspaceDims;
    const centroids = pqIndex.centroids[m];

    let bestCentroid = 0;
    let bestDist = Infinity;

    for (let c = 0; c < numCentroids; c++) {
      const centOffset = c * subspaceDims;
      let dist = 0;

      for (let d = 0; d < subspaceDims; d++) {
        const diff = vector[subOffset + d] - centroids[centOffset + d];
        dist += diff * diff;
      }

      if (dist < bestDist) {
        bestDist = dist;
        bestCentroid = c;
      }
    }

    codes[m] = bestCentroid;
  }

  return codes;
}

// ============================================================================
// Search
// ============================================================================

/**
 * Search for k nearest neighbors using IVF-PQ
 */
export function ivfPqSearch(
  index: IvfPqIndex,
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
  const { metric, useResiduals } = index.config;
  const { numSubspaces, numCentroids } = index.config.pq;

  // Normalize query for cosine metric
  const queryForSearch = metric === "cosine" ? normalize(query) : query;

  // Find top nProbe nearest centroids
  const probeClusters = findNearestCentroidsOptimized(
    index,
    queryForSearch,
    dimensions,
    nProbe
  );

  // Use max-heap to track top-k candidates
  const heap = new MaxHeap();
  const hasFilter = options?.filter !== undefined;
  const hasThreshold = options?.threshold !== undefined;
  const threshold = options?.threshold;

  // For non-residual mode, build the distance table ONCE
  // This is the key optimization - O(1) table builds instead of O(nProbe)
  let sharedDistTable: Float32Array | null = null;
  if (!useResiduals) {
    sharedDistTable = pqBuildDistanceTable(index.pqIndex, queryForSearch);
  }

  // Search within selected clusters
  for (const cluster of probeClusters) {
    const vectorIds = index.invertedLists.get(cluster);
    if (!vectorIds || vectorIds.length === 0) continue;

    // Get distance table (shared or per-cluster for residuals)
    let distTable: Float32Array;
    if (useResiduals) {
      // Query residual = query - centroid (requires per-cluster table)
      const queryResidual = new Float32Array(dimensions);
      const centOffset = cluster * dimensions;
      for (let d = 0; d < dimensions; d++) {
        queryResidual[d] = queryForSearch[d] - index.centroids[centOffset + d];
      }
      distTable = pqBuildDistanceTable(index.pqIndex, queryResidual);
    } else {
      distTable = sharedDistTable!;
    }

    // Search vectors in this cluster using PQ ADC
    for (const vectorId of vectorIds) {
      // Apply filter early if provided
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

      // Get PQ codes for this vector
      const codes = index.pqCodes.get(vectorId);
      if (!codes) continue;

      // Compute approximate distance using ADC
      const dist = pqDistanceADCInline(distTable, codes, numSubspaces, numCentroids);

      // Apply threshold filter
      if (hasThreshold) {
        const similarity = distanceToSimilarity(dist, metric);
        if (similarity < threshold!) continue;
      }

      // Add to heap
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
 * Find nearest centroids using pre-computed centroid distances for optimization
 */
function findNearestCentroidsOptimized(
  index: IvfPqIndex,
  query: Float32Array,
  dimensions: number,
  nProbe: number
): number[] {
  const { nClusters, metric } = index.config;
  const distanceFn = getDistanceFunction(metric);

  const centroidDists: Array<{ cluster: number; distance: number }> = [];

  for (let c = 0; c < nClusters; c++) {
    const centOffset = c * dimensions;
    const centroid = index.centroids.subarray(centOffset, centOffset + dimensions);
    const dist = distanceFn(query, centroid);
    centroidDists.push({ cluster: c, distance: dist });
  }

  centroidDists.sort((a, b) => a.distance - b.distance);
  return centroidDists.slice(0, nProbe).map((c) => c.cluster);
}

/**
 * Inline PQ ADC distance computation for maximum performance
 */
function pqDistanceADCInline(
  table: Float32Array,
  codes: Uint8Array,
  numSubspaces: number,
  numCentroids: number
): number {
  let dist = 0;

  // Unroll for performance
  const remainder = numSubspaces % 8;
  const mainLen = numSubspaces - remainder;

  for (let m = 0; m < mainLen; m += 8) {
    dist +=
      table[m * numCentroids + codes[m]] +
      table[(m + 1) * numCentroids + codes[m + 1]] +
      table[(m + 2) * numCentroids + codes[m + 2]] +
      table[(m + 3) * numCentroids + codes[m + 3]] +
      table[(m + 4) * numCentroids + codes[m + 4]] +
      table[(m + 5) * numCentroids + codes[m + 5]] +
      table[(m + 6) * numCentroids + codes[m + 6]] +
      table[(m + 7) * numCentroids + codes[m + 7]];
  }

  for (let m = mainLen; m < numSubspaces; m++) {
    dist += table[m * numCentroids + codes[m]];
  }

  return dist;
}

// ============================================================================
// Statistics
// ============================================================================

export function ivfPqStats(index: IvfPqIndex): {
  trained: boolean;
  nClusters: number;
  totalVectors: number;
  avgVectorsPerCluster: number;
  pqNumSubspaces: number;
  pqNumCentroids: number;
  memorySavingsRatio: number;
} {
  let total = 0;
  for (const [, list] of index.invertedLists) {
    total += list.length;
  }

  const { numSubspaces, numCentroids } = index.config.pq;
  const dimensions = index.pqIndex.dimensions;

  // Memory calculation
  const originalBytes = total * dimensions * 4; // float32
  const pqCodeBytes = total * numSubspaces; // uint8 codes
  const centroidBytes = numSubspaces * numCentroids * index.pqIndex.subspaceDims * 4;
  const ivfCentroidBytes = index.config.nClusters * dimensions * 4;

  const compressedBytes = pqCodeBytes + centroidBytes + ivfCentroidBytes;
  const memorySavingsRatio = originalBytes > 0 ? originalBytes / compressedBytes : 0;

  return {
    trained: index.trained,
    nClusters: index.config.nClusters,
    totalVectors: total,
    avgVectorsPerCluster: total / Math.max(index.config.nClusters, 1),
    pqNumSubspaces: numSubspaces,
    pqNumCentroids: numCentroids,
    memorySavingsRatio,
  };
}
