/**
 * Product Quantization (PQ) for vector compression and fast distance computation
 *
 * PQ divides vectors into M subspaces and quantizes each subspace independently
 * using K centroids (typically 256 for uint8 codes).
 *
 * Benefits:
 * - Memory: 384D float32 (1536 bytes) -> 48 bytes (32x compression with M=48)
 * - Speed: Pre-compute distance tables for O(M) distance lookups instead of O(D)
 *
 * Algorithm:
 * 1. Training: Run k-means on each subspace to find subspace centroids
 * 2. Encoding: Assign each subvector to nearest centroid, store code
 * 3. Search: Build distance table (query to all centroids), sum table lookups
 */

import { squaredEuclidean } from "./distance.ts";

// ============================================================================
// Types
// ============================================================================

export interface PQConfig {
  /** Number of subspaces (M). dimensions must be divisible by M */
  numSubspaces: number;
  /** Number of centroids per subspace (K). Typically 256 for uint8 codes */
  numCentroids: number;
  /** K-means iterations for training */
  maxIterations: number;
}

export interface PQIndex {
  config: PQConfig;
  /** Original vector dimensions */
  dimensions: number;
  /** Dimensions per subspace (D/M) */
  subspaceDims: number;
  /** Centroids for each subspace: M arrays of K*subspaceDims floats */
  centroids: Float32Array[];
  /** Encoded vectors: each vector is M uint8 codes */
  codes: Uint8Array | null;
  /** Number of encoded vectors */
  numVectors: number;
  /** Whether the index has been trained */
  trained: boolean;
}

export const DEFAULT_PQ_CONFIG: PQConfig = {
  numSubspaces: 48,      // Good for 384D (8 dims per subspace)
  numCentroids: 256,     // uint8 codes
  maxIterations: 20,
};

// ============================================================================
// Index Creation
// ============================================================================

/**
 * Create a new PQ index
 */
export function createPQIndex(
  dimensions: number,
  config?: Partial<PQConfig>
): PQIndex {
  const fullConfig: PQConfig = { ...DEFAULT_PQ_CONFIG, ...config };
  const { numSubspaces, numCentroids } = fullConfig;

  if (dimensions % numSubspaces !== 0) {
    throw new Error(
      `Dimensions (${dimensions}) must be divisible by numSubspaces (${numSubspaces})`
    );
  }

  const subspaceDims = dimensions / numSubspaces;

  // Initialize empty centroids for each subspace
  const centroids: Float32Array[] = [];
  for (let m = 0; m < numSubspaces; m++) {
    centroids.push(new Float32Array(numCentroids * subspaceDims));
  }

  return {
    config: fullConfig,
    dimensions,
    subspaceDims,
    centroids,
    codes: null,
    numVectors: 0,
    trained: false,
  };
}

// ============================================================================
// Training
// ============================================================================

/**
 * Train the PQ index on a set of vectors
 *
 * @param index - The PQ index
 * @param vectors - Training vectors (contiguous float32 array)
 * @param numVectors - Number of training vectors
 */
export function pqTrain(
  index: PQIndex,
  vectors: Float32Array,
  numVectors: number
): void {
  if (index.trained) {
    throw new Error("Index already trained");
  }

  const { numSubspaces, numCentroids, maxIterations } = index.config;
  const { dimensions, subspaceDims } = index;

  if (numVectors < numCentroids) {
    throw new Error(
      `Need at least ${numCentroids} training vectors, got ${numVectors}`
    );
  }

  // Train each subspace independently
  for (let m = 0; m < numSubspaces; m++) {
    // Extract subvectors for this subspace
    const subvectors = new Float32Array(numVectors * subspaceDims);
    const subOffset = m * subspaceDims;

    for (let i = 0; i < numVectors; i++) {
      const vecOffset = i * dimensions + subOffset;
      for (let d = 0; d < subspaceDims; d++) {
        subvectors[i * subspaceDims + d] = vectors[vecOffset + d];
      }
    }

    // Run k-means on subvectors
    trainSubspace(
      index.centroids[m],
      subvectors,
      numVectors,
      subspaceDims,
      numCentroids,
      maxIterations
    );
  }

  index.trained = true;
}

/**
 * K-means training for a single subspace
 */
function trainSubspace(
  centroids: Float32Array,
  subvectors: Float32Array,
  numVectors: number,
  subspaceDims: number,
  numCentroids: number,
  maxIterations: number
): void {
  // Initialize centroids with k-means++ 
  initializeCentroidsKMeansPP(
    centroids,
    subvectors,
    numVectors,
    subspaceDims,
    numCentroids
  );

  const assignments = new Uint16Array(numVectors);
  const clusterSums = new Float32Array(numCentroids * subspaceDims);
  const clusterCounts = new Uint32Array(numCentroids);

  for (let iter = 0; iter < maxIterations; iter++) {
    // Assign vectors to nearest centroids
    for (let i = 0; i < numVectors; i++) {
      const vecOffset = i * subspaceDims;
      let bestCentroid = 0;
      let bestDist = Infinity;

      for (let c = 0; c < numCentroids; c++) {
        const centOffset = c * subspaceDims;
        let dist = 0;
        for (let d = 0; d < subspaceDims; d++) {
          const diff = subvectors[vecOffset + d] - centroids[centOffset + d];
          dist += diff * diff;
        }
        if (dist < bestDist) {
          bestDist = dist;
          bestCentroid = c;
        }
      }
      assignments[i] = bestCentroid;
    }

    // Update centroids
    clusterSums.fill(0);
    clusterCounts.fill(0);

    for (let i = 0; i < numVectors; i++) {
      const cluster = assignments[i];
      const vecOffset = i * subspaceDims;
      const sumOffset = cluster * subspaceDims;

      for (let d = 0; d < subspaceDims; d++) {
        clusterSums[sumOffset + d] += subvectors[vecOffset + d];
      }
      clusterCounts[cluster]++;
    }

    for (let c = 0; c < numCentroids; c++) {
      const count = clusterCounts[c];
      if (count === 0) continue;

      const offset = c * subspaceDims;
      for (let d = 0; d < subspaceDims; d++) {
        centroids[offset + d] = clusterSums[offset + d] / count;
      }
    }
  }
}

/**
 * K-means++ initialization
 */
function initializeCentroidsKMeansPP(
  centroids: Float32Array,
  vectors: Float32Array,
  numVectors: number,
  dims: number,
  k: number
): void {
  // First centroid: random vector
  const firstIdx = Math.floor(Math.random() * numVectors);
  for (let d = 0; d < dims; d++) {
    centroids[d] = vectors[firstIdx * dims + d];
  }

  const minDists = new Float32Array(numVectors).fill(Infinity);

  for (let c = 1; c < k; c++) {
    // Update min distances
    const prevCentOffset = (c - 1) * dims;
    let totalDist = 0;

    for (let i = 0; i < numVectors; i++) {
      const vecOffset = i * dims;
      let dist = 0;
      for (let d = 0; d < dims; d++) {
        const diff = vectors[vecOffset + d] - centroids[prevCentOffset + d];
        dist += diff * diff;
      }
      minDists[i] = Math.min(minDists[i], dist);
      totalDist += minDists[i];
    }

    // Weighted random selection
    let r = Math.random() * totalDist;
    let selectedIdx = 0;
    for (let i = 0; i < numVectors; i++) {
      r -= minDists[i];
      if (r <= 0) {
        selectedIdx = i;
        break;
      }
    }

    // Copy selected vector to centroid
    const centOffset = c * dims;
    for (let d = 0; d < dims; d++) {
      centroids[centOffset + d] = vectors[selectedIdx * dims + d];
    }
  }
}

// ============================================================================
// Encoding
// ============================================================================

/**
 * Encode vectors into PQ codes
 *
 * @param index - The trained PQ index
 * @param vectors - Vectors to encode (contiguous float32 array)
 * @param numVectors - Number of vectors
 */
export function pqEncode(
  index: PQIndex,
  vectors: Float32Array,
  numVectors: number
): void {
  if (!index.trained) {
    throw new Error("Index must be trained before encoding");
  }

  const { numSubspaces, numCentroids } = index.config;
  const { dimensions, subspaceDims } = index;

  // Allocate codes array
  index.codes = new Uint8Array(numVectors * numSubspaces);
  index.numVectors = numVectors;

  // Encode each vector
  for (let i = 0; i < numVectors; i++) {
    const vecOffset = i * dimensions;
    const codeOffset = i * numSubspaces;

    for (let m = 0; m < numSubspaces; m++) {
      const subOffset = m * subspaceDims;
      const centroids = index.centroids[m];

      // Find nearest centroid for this subspace
      let bestCentroid = 0;
      let bestDist = Infinity;

      for (let c = 0; c < numCentroids; c++) {
        const centOffset = c * subspaceDims;
        let dist = 0;

        for (let d = 0; d < subspaceDims; d++) {
          const diff =
            vectors[vecOffset + subOffset + d] - centroids[centOffset + d];
          dist += diff * diff;
        }

        if (dist < bestDist) {
          bestDist = dist;
          bestCentroid = c;
        }
      }

      index.codes[codeOffset + m] = bestCentroid;
    }
  }
}

/**
 * Encode a single vector and return the codes
 */
export function pqEncodeOne(
  index: PQIndex,
  vector: Float32Array
): Uint8Array {
  if (!index.trained) {
    throw new Error("Index must be trained before encoding");
  }

  const { numSubspaces, numCentroids } = index.config;
  const { subspaceDims } = index;

  const codes = new Uint8Array(numSubspaces);

  for (let m = 0; m < numSubspaces; m++) {
    const subOffset = m * subspaceDims;
    const centroids = index.centroids[m];

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
// Distance Table (ADC - Asymmetric Distance Computation)
// ============================================================================

/**
 * Build distance table for a query vector
 *
 * The table contains squared distances from query subvectors to all centroids.
 * This allows O(M) distance computation instead of O(D).
 *
 * @param index - The trained PQ index
 * @param query - Query vector
 * @returns Distance table: M x K float32 array (flattened)
 */
export function pqBuildDistanceTable(
  index: PQIndex,
  query: Float32Array
): Float32Array {
  if (!index.trained) {
    throw new Error("Index must be trained");
  }

  const { numSubspaces, numCentroids } = index.config;
  const { subspaceDims } = index;

  const table = new Float32Array(numSubspaces * numCentroids);

  for (let m = 0; m < numSubspaces; m++) {
    const subOffset = m * subspaceDims;
    const tableOffset = m * numCentroids;
    const centroids = index.centroids[m];

    for (let c = 0; c < numCentroids; c++) {
      const centOffset = c * subspaceDims;
      let dist = 0;

      for (let d = 0; d < subspaceDims; d++) {
        const diff = query[subOffset + d] - centroids[centOffset + d];
        dist += diff * diff;
      }

      table[tableOffset + c] = dist;
    }
  }

  return table;
}

// ============================================================================
// Search with ADC
// ============================================================================

/**
 * Compute approximate squared distance using distance table (ADC)
 *
 * @param table - Pre-computed distance table
 * @param codes - PQ codes for the vector
 * @param numSubspaces - Number of subspaces
 * @param numCentroids - Number of centroids per subspace
 * @returns Approximate squared L2 distance
 */
export function pqDistanceADC(
  table: Float32Array,
  codes: Uint8Array,
  codeOffset: number,
  numSubspaces: number,
  numCentroids: number
): number {
  let dist = 0;

  // Unroll for performance (process 4 subspaces at a time)
  const remainder = numSubspaces % 4;
  const mainLen = numSubspaces - remainder;

  for (let m = 0; m < mainLen; m += 4) {
    dist +=
      table[m * numCentroids + codes[codeOffset + m]] +
      table[(m + 1) * numCentroids + codes[codeOffset + m + 1]] +
      table[(m + 2) * numCentroids + codes[codeOffset + m + 2]] +
      table[(m + 3) * numCentroids + codes[codeOffset + m + 3]];
  }

  for (let m = mainLen; m < numSubspaces; m++) {
    dist += table[m * numCentroids + codes[codeOffset + m]];
  }

  return dist;
}

/**
 * Search for k nearest neighbors using ADC
 *
 * @param index - The PQ index with encoded vectors
 * @param query - Query vector
 * @param k - Number of results
 * @param vectorIds - Optional array of vector IDs to search (for IVF integration)
 * @returns Array of {index, distance} sorted by distance
 */
export function pqSearch(
  index: PQIndex,
  query: Float32Array,
  k: number,
  vectorIds?: number[]
): Array<{ index: number; distance: number }> {
  if (!index.trained || !index.codes) {
    throw new Error("Index must be trained and have encoded vectors");
  }

  const { numSubspaces, numCentroids } = index.config;

  // Build distance table
  const table = pqBuildDistanceTable(index, query);

  // Search
  const results: Array<{ index: number; distance: number }> = [];
  const searchIndices = vectorIds ?? Array.from({ length: index.numVectors }, (_, i) => i);

  // Use a simple array for top-k (can optimize with heap for very large k)
  let maxDist = Infinity;

  for (const idx of searchIndices) {
    const codeOffset = idx * numSubspaces;
    const dist = pqDistanceADC(table, index.codes, codeOffset, numSubspaces, numCentroids);

    if (results.length < k) {
      results.push({ index: idx, distance: dist });
      if (results.length === k) {
        results.sort((a, b) => b.distance - a.distance);
        maxDist = results[0].distance;
      }
    } else if (dist < maxDist) {
      // Replace worst result
      results[0] = { index: idx, distance: dist };
      results.sort((a, b) => b.distance - a.distance);
      maxDist = results[0].distance;
    }
  }

  // Sort by distance ascending
  return results.sort((a, b) => a.distance - b.distance);
}

/**
 * Batch search multiple vector IDs using pre-built distance table
 * Optimized for IVF integration where we search within clusters
 *
 * @param table - Pre-computed distance table from pqBuildDistanceTable
 * @param codes - The full codes array from the PQ index
 * @param vectorIds - Vector IDs to search
 * @param numSubspaces - Number of subspaces
 * @param numCentroids - Number of centroids
 * @param k - Number of results
 * @returns Array of {vectorId, distance} sorted by distance
 */
export function pqSearchWithTable(
  table: Float32Array,
  codes: Uint8Array,
  vectorIds: number[],
  numSubspaces: number,
  numCentroids: number,
  k: number
): Array<{ vectorId: number; distance: number }> {
  // For small result sets, use simple array
  // For larger k, would use a heap
  const results: Array<{ vectorId: number; distance: number }> = [];
  let maxDist = Infinity;

  for (const vectorId of vectorIds) {
    const codeOffset = vectorId * numSubspaces;
    const dist = pqDistanceADC(table, codes, codeOffset, numSubspaces, numCentroids);

    if (results.length < k) {
      results.push({ vectorId, distance: dist });
      if (results.length === k) {
        results.sort((a, b) => b.distance - a.distance);
        maxDist = results[0].distance;
      }
    } else if (dist < maxDist) {
      results[0] = { vectorId, distance: dist };
      results.sort((a, b) => b.distance - a.distance);
      maxDist = results[0].distance;
    }
  }

  return results.sort((a, b) => a.distance - b.distance);
}

// ============================================================================
// Statistics
// ============================================================================

/**
 * Get PQ index statistics
 */
export function pqStats(index: PQIndex): {
  trained: boolean;
  dimensions: number;
  numSubspaces: number;
  subspaceDims: number;
  numCentroids: number;
  numVectors: number;
  codeSizeBytes: number;
  centroidsSizeBytes: number;
  compressionRatio: number;
} {
  const { numSubspaces, numCentroids } = index.config;
  const codeSizeBytes = index.numVectors * numSubspaces;
  const centroidsSizeBytes = numSubspaces * numCentroids * index.subspaceDims * 4;
  const originalSizeBytes = index.numVectors * index.dimensions * 4;
  const compressionRatio = originalSizeBytes > 0 
    ? originalSizeBytes / (codeSizeBytes + centroidsSizeBytes)
    : 0;

  return {
    trained: index.trained,
    dimensions: index.dimensions,
    numSubspaces,
    subspaceDims: index.subspaceDims,
    numCentroids,
    numVectors: index.numVectors,
    codeSizeBytes,
    centroidsSizeBytes,
    compressionRatio,
  };
}
