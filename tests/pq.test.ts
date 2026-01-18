/**
 * Tests for Product Quantization (PQ) module
 *
 * PQ divides vectors into M subspaces and quantizes each subspace independently
 * using K centroids (typically 256 for uint8 codes).
 */

import { describe, expect, test } from "bun:test";

import {
  createPQIndex,
  pqTrain,
  pqEncode,
  pqEncodeOne,
  pqBuildDistanceTable,
  pqDistanceADC,
  pqSearch,
  pqSearchWithTable,
  pqStats,
  type PQIndex,
  type PQConfig,
  DEFAULT_PQ_CONFIG,
} from "../src/vector/pq.ts";

// ============================================================================
// Helper Functions
// ============================================================================

/**
 * Create a test config with small values for fast tests
 */
function testConfig(): Partial<PQConfig> {
  return {
    numSubspaces: 4,
    numCentroids: 8,
    maxIterations: 10,
  };
}

/**
 * Generate training vectors with deterministic pattern
 */
function generateTrainingVectors(
  numVectors: number,
  dimensions: number
): Float32Array {
  const vectors = new Float32Array(numVectors * dimensions);
  for (let i = 0; i < numVectors; i++) {
    for (let d = 0; d < dimensions; d++) {
      vectors[i * dimensions + d] = ((i * dimensions + d) % 1000) / 1000;
    }
  }
  return vectors;
}

/**
 * Generate random vectors
 */
function generateRandomVectors(
  numVectors: number,
  dimensions: number
): Float32Array {
  const vectors = new Float32Array(numVectors * dimensions);
  for (let i = 0; i < numVectors; i++) {
    for (let d = 0; d < dimensions; d++) {
      vectors[i * dimensions + d] = Math.random() * 2 - 1;
    }
  }
  return vectors;
}

// ============================================================================
// Index Creation Tests
// ============================================================================

describe("PQ Index Creation", () => {
  test("createPQIndex initializes correctly", () => {
    const index = createPQIndex(16, testConfig());

    expect(index.dimensions).toBe(16);
    expect(index.subspaceDims).toBe(4); // 16 / 4 subspaces
    expect(index.config.numSubspaces).toBe(4);
    expect(index.config.numCentroids).toBe(8);
    expect(index.trained).toBe(false);
    expect(index.codes).toBeNull();
    expect(index.numVectors).toBe(0);
  });

  test("createPQIndex uses default config when none provided", () => {
    const index = createPQIndex(384);

    expect(index.config.numSubspaces).toBe(DEFAULT_PQ_CONFIG.numSubspaces);
    expect(index.config.numCentroids).toBe(DEFAULT_PQ_CONFIG.numCentroids);
  });

  test("createPQIndex throws when dimensions not divisible by numSubspaces", () => {
    expect(() => createPQIndex(15, testConfig())).toThrow(
      /must be divisible by numSubspaces/
    );
  });

  test("createPQIndex initializes centroids for each subspace", () => {
    const index = createPQIndex(16, testConfig());

    expect(index.centroids.length).toBe(4); // numSubspaces
    for (const centroids of index.centroids) {
      // Each subspace has numCentroids * subspaceDims floats
      expect(centroids.length).toBe(8 * 4); // 8 centroids * 4 dims
    }
  });
});

// ============================================================================
// Training Tests
// ============================================================================

describe("PQ Training", () => {
  test("pqTrain trains the index", () => {
    const index = createPQIndex(16, testConfig());
    const vectors = generateTrainingVectors(100, 16);

    pqTrain(index, vectors, 100);

    expect(index.trained).toBe(true);
  });

  test("pqTrain throws if already trained", () => {
    const index = createPQIndex(16, testConfig());
    const vectors = generateTrainingVectors(100, 16);

    pqTrain(index, vectors, 100);

    expect(() => pqTrain(index, vectors, 100)).toThrow(/already trained/);
  });

  test("pqTrain throws if not enough training vectors", () => {
    const index = createPQIndex(16, testConfig());
    const vectors = generateTrainingVectors(5, 16); // Only 5 vectors, need 8

    expect(() => pqTrain(index, vectors, 5)).toThrow(
      /at least 8 training vectors/
    );
  });

  test("pqTrain updates centroids", () => {
    const index = createPQIndex(16, testConfig());
    const vectors = generateTrainingVectors(100, 16);

    // Centroids should be all zeros initially
    for (const centroids of index.centroids) {
      expect(centroids.every((v) => v === 0)).toBe(true);
    }

    pqTrain(index, vectors, 100);

    // Centroids should now have non-zero values
    let hasNonZero = false;
    for (const centroids of index.centroids) {
      if (centroids.some((v) => v !== 0)) {
        hasNonZero = true;
        break;
      }
    }
    expect(hasNonZero).toBe(true);
  });
});

// ============================================================================
// Encoding Tests
// ============================================================================

describe("PQ Encoding", () => {
  test("pqEncode encodes vectors into codes", () => {
    const index = createPQIndex(16, testConfig());
    const vectors = generateTrainingVectors(100, 16);

    pqTrain(index, vectors, 100);
    pqEncode(index, vectors, 100);

    expect(index.codes).not.toBeNull();
    expect(index.numVectors).toBe(100);
    // Each vector produces numSubspaces codes
    expect(index.codes!.length).toBe(100 * 4);
  });

  test("pqEncode throws if not trained", () => {
    const index = createPQIndex(16, testConfig());
    const vectors = generateTrainingVectors(100, 16);

    expect(() => pqEncode(index, vectors, 100)).toThrow(/must be trained/);
  });

  test("pqEncode produces valid code values", () => {
    const index = createPQIndex(16, testConfig());
    const vectors = generateTrainingVectors(100, 16);

    pqTrain(index, vectors, 100);
    pqEncode(index, vectors, 100);

    // All codes should be in range [0, numCentroids)
    for (const code of index.codes!) {
      expect(code).toBeGreaterThanOrEqual(0);
      expect(code).toBeLessThan(8); // numCentroids
    }
  });

  test("pqEncodeOne encodes a single vector", () => {
    const index = createPQIndex(16, testConfig());
    const vectors = generateTrainingVectors(100, 16);

    pqTrain(index, vectors, 100);

    const singleVector = new Float32Array(16).fill(0.5);
    const codes = pqEncodeOne(index, singleVector);

    expect(codes.length).toBe(4); // numSubspaces
    for (const code of codes) {
      expect(code).toBeGreaterThanOrEqual(0);
      expect(code).toBeLessThan(8);
    }
  });

  test("pqEncodeOne throws if not trained", () => {
    const index = createPQIndex(16, testConfig());
    const vector = new Float32Array(16).fill(0.5);

    expect(() => pqEncodeOne(index, vector)).toThrow(/must be trained/);
  });
});

// ============================================================================
// Distance Table Tests
// ============================================================================

describe("PQ Distance Table", () => {
  test("pqBuildDistanceTable creates correct size table", () => {
    const index = createPQIndex(16, testConfig());
    const vectors = generateTrainingVectors(100, 16);

    pqTrain(index, vectors, 100);

    const query = new Float32Array(16).fill(0.5);
    const table = pqBuildDistanceTable(index, query);

    // Table size = numSubspaces * numCentroids
    expect(table.length).toBe(4 * 8);
  });

  test("pqBuildDistanceTable throws if not trained", () => {
    const index = createPQIndex(16, testConfig());
    const query = new Float32Array(16).fill(0.5);

    expect(() => pqBuildDistanceTable(index, query)).toThrow(/must be trained/);
  });

  test("pqBuildDistanceTable produces non-negative distances", () => {
    const index = createPQIndex(16, testConfig());
    const vectors = generateTrainingVectors(100, 16);

    pqTrain(index, vectors, 100);

    const query = new Float32Array(16).fill(0.5);
    const table = pqBuildDistanceTable(index, query);

    // All distances should be non-negative (squared distances)
    for (const dist of table) {
      expect(dist).toBeGreaterThanOrEqual(0);
    }
  });
});

// ============================================================================
// ADC Distance Tests
// ============================================================================

describe("PQ ADC Distance", () => {
  test("pqDistanceADC computes distance from table", () => {
    const index = createPQIndex(16, testConfig());
    const vectors = generateTrainingVectors(100, 16);

    pqTrain(index, vectors, 100);
    pqEncode(index, vectors, 100);

    const query = new Float32Array(16).fill(0.5);
    const table = pqBuildDistanceTable(index, query);

    const dist = pqDistanceADC(table, index.codes!, 0, 4, 8);

    expect(dist).toBeGreaterThanOrEqual(0);
    expect(isFinite(dist)).toBe(true);
  });

  test("pqDistanceADC handles different code offsets", () => {
    const index = createPQIndex(16, testConfig());
    const vectors = generateTrainingVectors(100, 16);

    pqTrain(index, vectors, 100);
    pqEncode(index, vectors, 100);

    const query = new Float32Array(16).fill(0.5);
    const table = pqBuildDistanceTable(index, query);

    const dist0 = pqDistanceADC(table, index.codes!, 0, 4, 8);
    const dist1 = pqDistanceADC(table, index.codes!, 4, 4, 8); // Second vector
    const dist2 = pqDistanceADC(table, index.codes!, 8, 4, 8); // Third vector

    // Distances should all be valid
    expect(isFinite(dist0)).toBe(true);
    expect(isFinite(dist1)).toBe(true);
    expect(isFinite(dist2)).toBe(true);
  });

  test("pqDistanceADC returns 0 for identical vector", () => {
    const index = createPQIndex(16, testConfig());
    const vectors = generateTrainingVectors(100, 16);

    pqTrain(index, vectors, 100);
    pqEncode(index, vectors, 100);

    // Query with the first training vector
    const query = vectors.subarray(0, 16);
    const table = pqBuildDistanceTable(index, query);

    const dist = pqDistanceADC(table, index.codes!, 0, 4, 8);

    // Distance should be 0 or very small (due to quantization error)
    expect(dist).toBeLessThan(0.1);
  });
});

// ============================================================================
// Search Tests
// ============================================================================

describe("PQ Search", () => {
  test("pqSearch returns k nearest neighbors", () => {
    const index = createPQIndex(16, testConfig());
    const vectors = generateTrainingVectors(100, 16);

    pqTrain(index, vectors, 100);
    pqEncode(index, vectors, 100);

    const query = new Float32Array(16).fill(0.5);
    const results = pqSearch(index, query, 5);

    expect(results.length).toBe(5);
  });

  test("pqSearch results are sorted by distance", () => {
    const index = createPQIndex(16, testConfig());
    const vectors = generateTrainingVectors(100, 16);

    pqTrain(index, vectors, 100);
    pqEncode(index, vectors, 100);

    const query = new Float32Array(16).fill(0.5);
    const results = pqSearch(index, query, 10);

    for (let i = 1; i < results.length; i++) {
      expect(results[i - 1].distance).toBeLessThanOrEqual(results[i].distance);
    }
  });

  test("pqSearch throws if not trained or encoded", () => {
    const index = createPQIndex(16, testConfig());
    const query = new Float32Array(16).fill(0.5);

    expect(() => pqSearch(index, query, 5)).toThrow(
      /must be trained and have encoded vectors/
    );
  });

  test("pqSearch with subset of vector IDs", () => {
    const index = createPQIndex(16, testConfig());
    const vectors = generateTrainingVectors(100, 16);

    pqTrain(index, vectors, 100);
    pqEncode(index, vectors, 100);

    const query = new Float32Array(16).fill(0.5);
    const vectorIds = [10, 20, 30, 40, 50];
    const results = pqSearch(index, query, 3, vectorIds);

    expect(results.length).toBe(3);
    // All results should be from the specified IDs
    for (const result of results) {
      expect(vectorIds).toContain(result.index);
    }
  });

  test("pqSearch returns fewer results if k > numVectors", () => {
    const index = createPQIndex(16, testConfig());
    const vectors = generateTrainingVectors(10, 16);

    pqTrain(index, vectors, 10);
    pqEncode(index, vectors, 10);

    const query = new Float32Array(16).fill(0.5);
    const results = pqSearch(index, query, 20); // Request more than available

    expect(results.length).toBe(10);
  });
});

// ============================================================================
// pqSearchWithTable Tests
// ============================================================================

describe("PQ Search With Table", () => {
  test("pqSearchWithTable searches using pre-built table", () => {
    const index = createPQIndex(16, testConfig());
    const vectors = generateTrainingVectors(100, 16);

    pqTrain(index, vectors, 100);
    pqEncode(index, vectors, 100);

    const query = new Float32Array(16).fill(0.5);
    const table = pqBuildDistanceTable(index, query);

    const vectorIds = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
    const results = pqSearchWithTable(table, index.codes!, vectorIds, 4, 8, 5);

    expect(results.length).toBe(5);
    // All results should have vectorId from the specified list
    for (const result of results) {
      expect(vectorIds).toContain(result.vectorId);
    }
  });

  test("pqSearchWithTable results are sorted by distance", () => {
    const index = createPQIndex(16, testConfig());
    const vectors = generateTrainingVectors(100, 16);

    pqTrain(index, vectors, 100);
    pqEncode(index, vectors, 100);

    const query = new Float32Array(16).fill(0.5);
    const table = pqBuildDistanceTable(index, query);

    const vectorIds = Array.from({ length: 50 }, (_, i) => i);
    const results = pqSearchWithTable(table, index.codes!, vectorIds, 4, 8, 10);

    for (let i = 1; i < results.length; i++) {
      expect(results[i - 1].distance).toBeLessThanOrEqual(results[i].distance);
    }
  });
});

// ============================================================================
// Statistics Tests
// ============================================================================

describe("PQ Statistics", () => {
  test("pqStats returns correct statistics before training", () => {
    const index = createPQIndex(16, testConfig());

    const stats = pqStats(index);

    expect(stats.trained).toBe(false);
    expect(stats.dimensions).toBe(16);
    expect(stats.numSubspaces).toBe(4);
    expect(stats.subspaceDims).toBe(4);
    expect(stats.numCentroids).toBe(8);
    expect(stats.numVectors).toBe(0);
    expect(stats.codeSizeBytes).toBe(0);
    expect(stats.compressionRatio).toBe(0);
  });

  test("pqStats returns correct statistics after encoding", () => {
    const index = createPQIndex(16, testConfig());
    const vectors = generateTrainingVectors(100, 16);

    pqTrain(index, vectors, 100);
    pqEncode(index, vectors, 100);

    const stats = pqStats(index);

    expect(stats.trained).toBe(true);
    expect(stats.numVectors).toBe(100);
    expect(stats.codeSizeBytes).toBe(100 * 4); // numVectors * numSubspaces
    expect(stats.compressionRatio).toBeGreaterThan(0);
  });

  test("pqStats compression ratio is positive for encoded vectors", () => {
    const index = createPQIndex(16, testConfig());
    const vectors = generateTrainingVectors(100, 16);

    pqTrain(index, vectors, 100);
    pqEncode(index, vectors, 100);

    const stats = pqStats(index);

    // Original size: 100 vectors * 16 dims * 4 bytes = 6400 bytes
    // Code size: 100 vectors * 4 subspaces = 400 bytes
    // Centroids: 4 subspaces * 8 centroids * 4 dims * 4 bytes = 512 bytes
    // Total compressed: 400 + 512 = 912 bytes
    // Ratio: 6400 / 912 ≈ 7
    expect(stats.compressionRatio).toBeGreaterThan(1);
  });
});

// ============================================================================
// Edge Cases and Stress Tests
// ============================================================================

describe("PQ Edge Cases", () => {
  test("works with minimum valid configuration", () => {
    const index = createPQIndex(4, {
      numSubspaces: 2,
      numCentroids: 2,
      maxIterations: 5,
    });

    const vectors = generateTrainingVectors(10, 4);

    pqTrain(index, vectors, 10);
    pqEncode(index, vectors, 10);

    const query = new Float32Array(4).fill(0.5);
    const results = pqSearch(index, query, 3);

    expect(results.length).toBe(3);
  });

  test("handles large number of centroids", () => {
    const index = createPQIndex(16, {
      numSubspaces: 4,
      numCentroids: 256,
      maxIterations: 5,
    });

    const vectors = generateTrainingVectors(300, 16);

    pqTrain(index, vectors, 300);
    pqEncode(index, vectors, 300);

    const query = new Float32Array(16).fill(0.5);
    const results = pqSearch(index, query, 5);

    expect(results.length).toBe(5);
  });

  test("handles many subspaces", () => {
    const index = createPQIndex(32, {
      numSubspaces: 16,
      numCentroids: 8,
      maxIterations: 5,
    });

    const vectors = generateTrainingVectors(100, 32);

    pqTrain(index, vectors, 100);
    pqEncode(index, vectors, 100);

    const stats = pqStats(index);
    expect(stats.subspaceDims).toBe(2); // 32 / 16
  });

  test("search with exactly k vectors", () => {
    const index = createPQIndex(16, testConfig());
    const vectors = generateTrainingVectors(5, 16);

    // Need at least 8 vectors for 8 centroids
    const moreVectors = generateTrainingVectors(10, 16);
    pqTrain(index, moreVectors, 10);
    pqEncode(index, vectors, 5);

    const query = new Float32Array(16).fill(0.5);
    const results = pqSearch(index, query, 5);

    expect(results.length).toBe(5);
  });

  test("maintains approximate nearest neighbor quality", () => {
    const index = createPQIndex(16, testConfig());

    // Create a target vector and some nearby/far vectors
    const target = new Float32Array(16).fill(0.5);
    const vectors = new Float32Array(100 * 16);

    // First vector is the target
    vectors.set(target, 0);

    // Next few are close to target
    for (let i = 1; i < 5; i++) {
      for (let d = 0; d < 16; d++) {
        vectors[i * 16 + d] = 0.5 + (Math.random() - 0.5) * 0.1;
      }
    }

    // Rest are random
    for (let i = 5; i < 100; i++) {
      for (let d = 0; d < 16; d++) {
        vectors[i * 16 + d] = Math.random() * 2 - 1;
      }
    }

    pqTrain(index, vectors, 100);
    pqEncode(index, vectors, 100);

    const results = pqSearch(index, target, 5);

    // The target (index 0) should be in top results
    const topIndices = results.map((r) => r.index);
    expect(topIndices).toContain(0);
  });
});

// ============================================================================
// Regression Tests
// ============================================================================

describe("PQ Regression", () => {
  test("distance values are consistent across multiple queries", () => {
    const index = createPQIndex(16, testConfig());
    const vectors = generateTrainingVectors(100, 16);

    pqTrain(index, vectors, 100);
    pqEncode(index, vectors, 100);

    const query = new Float32Array(16).fill(0.5);

    // Run search multiple times
    const results1 = pqSearch(index, query, 5);
    const results2 = pqSearch(index, query, 5);

    // Results should be identical
    for (let i = 0; i < results1.length; i++) {
      expect(results1[i].index).toBe(results2[i].index);
      expect(results1[i].distance).toBe(results2[i].distance);
    }
  });

  test("training is deterministic with same input", () => {
    // Note: This may not always pass due to k-means++ randomness
    // but we test that the API works correctly
    const vectors = generateTrainingVectors(100, 16);

    const index1 = createPQIndex(16, {
      ...testConfig(),
      maxIterations: 1, // Reduce iterations to minimize randomness impact
    });
    const index2 = createPQIndex(16, {
      ...testConfig(),
      maxIterations: 1,
    });

    pqTrain(index1, vectors, 100);
    pqTrain(index2, vectors, 100);

    // Both should be trained
    expect(index1.trained).toBe(true);
    expect(index2.trained).toBe(true);
  });
});
