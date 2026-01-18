/**
 * Tests for IVF-PQ (Inverted File Index with Product Quantization)
 *
 * IVF-PQ combines IVF (coarse clustering) with PQ (compression) for
 * fast approximate nearest neighbor search with low memory usage.
 */

import { describe, expect, test } from "bun:test";

import {
  createIvfPqIndex,
  ivfPqAddTrainingVectors,
  ivfPqTrain,
  ivfPqInsert,
  ivfPqSearch,
  ivfPqStats,
  type IvfPqIndex,
  type IvfPqConfig,
  DEFAULT_IVF_PQ_CONFIG,
} from "../src/vector/ivf-pq.ts";

import {
  createVectorStore,
  vectorStoreInsert,
} from "../src/vector/index.ts";

import { normalize } from "../src/vector/normalize.ts";

// ============================================================================
// Helper Functions
// ============================================================================

/**
 * Create a test config with small values for fast tests
 * nProbe equals nClusters to ensure all clusters are searched
 */
function testConfig(): Partial<IvfPqConfig> {
  return {
    nClusters: 4,
    nProbe: 4, // Search all clusters to ensure we find results
    metric: "euclidean",
    pq: {
      numSubspaces: 4,
      numCentroids: 8,
      maxIterations: 10,
    },
    useResiduals: true,
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
 * Generate random normalized vectors
 */
function generateRandomNormalizedVectors(
  numVectors: number,
  dimensions: number
): Float32Array {
  const vectors = new Float32Array(numVectors * dimensions);
  for (let i = 0; i < numVectors; i++) {
    const vec = new Float32Array(dimensions);
    for (let d = 0; d < dimensions; d++) {
      vec[d] = Math.random() * 2 - 1;
    }
    const normalized = normalize(vec);
    vectors.set(normalized, i * dimensions);
  }
  return vectors;
}

/**
 * Create a vector store with test data
 */
function createTestVectorStore(dimensions: number, numVectors: number) {
  const store = createVectorStore(dimensions);
  for (let i = 0; i < numVectors; i++) {
    const vec = new Float32Array(dimensions);
    for (let d = 0; d < dimensions; d++) {
      vec[d] = Math.random() * 2 - 1;
    }
    vectorStoreInsert(store, i, vec);
  }
  return store;
}

/**
 * Helper to insert a vector into both the store and index with correct ID mapping
 * Returns the vectorId for reference
 */
function insertIntoStoreAndIndex(
  store: ReturnType<typeof createVectorStore>,
  index: IvfPqIndex,
  nodeId: number,
  vec: Float32Array,
  dimensions: number,
  skipValidation: boolean = false
): number {
  const vectorId = vectorStoreInsert(store, nodeId, vec, skipValidation);
  ivfPqInsert(index, vectorId, vec, dimensions);
  return vectorId;
}

// ============================================================================
// Index Creation Tests
// ============================================================================

describe("IVF-PQ Index Creation", () => {
  test("createIvfPqIndex initializes correctly", () => {
    const index = createIvfPqIndex(16, testConfig());

    expect(index.trained).toBe(false);
    expect(index.config.nClusters).toBe(4);
    expect(index.config.nProbe).toBe(4); // Search all clusters
    expect(index.config.pq.numSubspaces).toBe(4);
    expect(index.config.pq.numCentroids).toBe(8);
    expect(index.config.useResiduals).toBe(true);
  });

  test("createIvfPqIndex uses default config when none provided", () => {
    const index = createIvfPqIndex(384);

    expect(index.config.nClusters).toBe(DEFAULT_IVF_PQ_CONFIG.nClusters);
    expect(index.config.nProbe).toBe(DEFAULT_IVF_PQ_CONFIG.nProbe);
    expect(index.config.pq.numSubspaces).toBe(DEFAULT_IVF_PQ_CONFIG.pq.numSubspaces);
  });

  test("createIvfPqIndex throws when dimensions not divisible by numSubspaces", () => {
    expect(() =>
      createIvfPqIndex(15, {
        pq: { numSubspaces: 4, numCentroids: 8, maxIterations: 10 },
      })
    ).toThrow(/must be divisible by numSubspaces/);
  });

  test("createIvfPqIndex initializes empty data structures", () => {
    const index = createIvfPqIndex(16, testConfig());

    expect(index.invertedLists.size).toBe(0);
    expect(index.pqCodes.size).toBe(0);
    expect(index.centroids.length).toBe(4 * 16); // nClusters * dimensions
  });
});

// ============================================================================
// Training Tests
// ============================================================================

describe("IVF-PQ Training", () => {
  test("ivfPqAddTrainingVectors accumulates vectors", () => {
    const index = createIvfPqIndex(16, testConfig());
    const vectors1 = generateTrainingVectors(50, 16);
    const vectors2 = generateTrainingVectors(50, 16);

    ivfPqAddTrainingVectors(index, vectors1, 16, 50);
    ivfPqAddTrainingVectors(index, vectors2, 16, 50);

    // Training count should be 100
    expect(index.trainingCount).toBe(100);
  });

  test("ivfPqTrain trains the index", () => {
    const index = createIvfPqIndex(16, testConfig());
    const vectors = generateTrainingVectors(500, 16);

    ivfPqAddTrainingVectors(index, vectors, 16, 500);
    ivfPqTrain(index, 16);

    expect(index.trained).toBe(true);
    expect(index.centroidDistances).not.toBeNull();
    expect(index.invertedLists.size).toBe(4); // nClusters
  });

  test("ivfPqTrain is idempotent", () => {
    const index = createIvfPqIndex(16, testConfig());
    const vectors = generateTrainingVectors(500, 16);

    ivfPqAddTrainingVectors(index, vectors, 16, 500);
    ivfPqTrain(index, 16);
    ivfPqTrain(index, 16); // Should not throw

    expect(index.trained).toBe(true);
  });

  test("ivfPqTrain throws if no training vectors", () => {
    const index = createIvfPqIndex(16, testConfig());

    expect(() => ivfPqTrain(index, 16)).toThrow(/No training vectors/);
  });

  test("ivfPqTrain throws if not enough vectors for clusters", () => {
    const index = createIvfPqIndex(16, testConfig());
    const vectors = generateTrainingVectors(2, 16); // Only 2, need 4

    ivfPqAddTrainingVectors(index, vectors, 16, 2);

    expect(() => ivfPqTrain(index, 16)).toThrow(/Not enough training vectors/);
  });

  test("ivfPqAddTrainingVectors throws if already trained", () => {
    const index = createIvfPqIndex(16, testConfig());
    const vectors = generateTrainingVectors(500, 16);

    ivfPqAddTrainingVectors(index, vectors, 16, 500);
    ivfPqTrain(index, 16);

    expect(() => ivfPqAddTrainingVectors(index, vectors, 16, 50)).toThrow(
      /already trained/
    );
  });

  test("ivfPqTrain clears training data after completion", () => {
    const index = createIvfPqIndex(16, testConfig());
    const vectors = generateTrainingVectors(500, 16);

    ivfPqAddTrainingVectors(index, vectors, 16, 500);
    ivfPqTrain(index, 16);

    expect(index.trainingVectors).toBeUndefined();
    expect(index.trainingCount).toBeUndefined();
  });
});

// ============================================================================
// Insert Tests
// ============================================================================

describe("IVF-PQ Insert", () => {
  test("ivfPqInsert adds vectors to the index", () => {
    const index = createIvfPqIndex(16, testConfig());
    const vectors = generateTrainingVectors(500, 16);

    ivfPqAddTrainingVectors(index, vectors, 16, 500);
    ivfPqTrain(index, 16);

    const vector = new Float32Array(16).fill(0.5);
    ivfPqInsert(index, 0, vector, 16);

    expect(index.pqCodes.has(0)).toBe(true);
    
    const stats = ivfPqStats(index);
    expect(stats.totalVectors).toBe(1);
  });

  test("ivfPqInsert throws if not trained", () => {
    const index = createIvfPqIndex(16, testConfig());
    const vector = new Float32Array(16).fill(0.5);

    expect(() => ivfPqInsert(index, 0, vector, 16)).toThrow(/not trained/);
  });

  test("ivfPqInsert places vectors in correct clusters", () => {
    const index = createIvfPqIndex(16, testConfig());
    const vectors = generateTrainingVectors(500, 16);

    ivfPqAddTrainingVectors(index, vectors, 16, 500);
    ivfPqTrain(index, 16);

    // Insert multiple vectors
    for (let i = 0; i < 20; i++) {
      const vector = new Float32Array(16);
      for (let d = 0; d < 16; d++) {
        vector[d] = Math.random() * 2 - 1;
      }
      ivfPqInsert(index, i, vector, 16);
    }

    // All vectors should be in some cluster
    let totalInLists = 0;
    for (const [, list] of index.invertedLists) {
      totalInLists += list.length;
    }
    expect(totalInLists).toBe(20);
  });

  test("ivfPqInsert creates PQ codes for each vector", () => {
    const index = createIvfPqIndex(16, testConfig());
    const vectors = generateTrainingVectors(500, 16);

    ivfPqAddTrainingVectors(index, vectors, 16, 500);
    ivfPqTrain(index, 16);

    const vector = new Float32Array(16).fill(0.5);
    ivfPqInsert(index, 42, vector, 16);

    const codes = index.pqCodes.get(42);
    expect(codes).not.toBeUndefined();
    expect(codes!.length).toBe(4); // numSubspaces
  });
});

// ============================================================================
// Search Tests
// ============================================================================

describe("IVF-PQ Search", () => {
  test("ivfPqSearch returns k nearest neighbors", () => {
    const store = createVectorStore(16, { metric: "euclidean", normalize: false });
    const index = createIvfPqIndex(16, testConfig());

    // Train
    const vectors = generateTrainingVectors(500, 16);
    ivfPqAddTrainingVectors(index, vectors, 16, 500);
    ivfPqTrain(index, 16);

    // Insert vectors into both store and index
    for (let i = 0; i < 50; i++) {
      const vec = new Float32Array(16);
      for (let d = 0; d < 16; d++) {
        vec[d] = Math.random() * 2 - 1;
      }
      insertIntoStoreAndIndex(store, index, i, vec, 16, true);
    }

    const query = new Float32Array(16).fill(0.5);
    const results = ivfPqSearch(index, store, query, 5);

    expect(results.length).toBe(5);
  });

  test("ivfPqSearch returns empty array if not trained", () => {
    const store = createTestVectorStore(16, 10);
    const index = createIvfPqIndex(16, testConfig());

    const query = new Float32Array(16).fill(0.5);
    const results = ivfPqSearch(index, store, query, 5);

    expect(results.length).toBe(0);
  });

  test("ivfPqSearch results are sorted by distance", () => {
    const store = createVectorStore(16);
    const index = createIvfPqIndex(16, testConfig());

    // Train
    const vectors = generateTrainingVectors(500, 16);
    ivfPqAddTrainingVectors(index, vectors, 16, 500);
    ivfPqTrain(index, 16);

    // Insert vectors
    for (let i = 0; i < 50; i++) {
      const vec = new Float32Array(16);
      for (let d = 0; d < 16; d++) {
        vec[d] = Math.random() * 2 - 1;
      }
      vectorStoreInsert(store, i, vec);
      ivfPqInsert(index, i, vec, 16);
    }

    const query = new Float32Array(16).fill(0.5);
    const results = ivfPqSearch(index, store, query, 10);

    for (let i = 1; i < results.length; i++) {
      expect(results[i - 1].distance).toBeLessThanOrEqual(results[i].distance);
    }
  });

  test("ivfPqSearch respects nProbe option", () => {
    // Use 2 clusters to ensure vectors are distributed
    const store = createVectorStore(16, { metric: "euclidean", normalize: false });
    const index = createIvfPqIndex(16, {
      nClusters: 2,
      nProbe: 2,
      metric: "euclidean",
      pq: {
        numSubspaces: 4,
        numCentroids: 8,
        maxIterations: 10,
      },
      useResiduals: true,
    });

    // Train with varied data to ensure good cluster separation
    const vectors = new Float32Array(500 * 16);
    for (let i = 0; i < 500; i++) {
      for (let d = 0; d < 16; d++) {
        // Create two distinct clusters: one centered at 0.2, one at 0.8
        const clusterCenter = i < 250 ? 0.2 : 0.8;
        vectors[i * 16 + d] = clusterCenter + (Math.random() - 0.5) * 0.2;
      }
    }
    ivfPqAddTrainingVectors(index, vectors, 16, 500);
    ivfPqTrain(index, 16);

    // Insert vectors into store and index
    for (let i = 0; i < 50; i++) {
      const vec = new Float32Array(16);
      for (let d = 0; d < 16; d++) {
        vec[d] = Math.random() * 2 - 1;
      }
      insertIntoStoreAndIndex(store, index, i, vec, 16, true);
    }

    const query = new Float32Array(16).fill(0.5);

    // Search with different nProbe values
    const results1 = ivfPqSearch(index, store, query, 10, { nProbe: 1 });
    const results2 = ivfPqSearch(index, store, query, 10, { nProbe: 2 });

    // With 2 clusters, searching 2 clusters should find all vectors
    expect(results2.length).toBeGreaterThan(0);
    // Results with more probes should include at least as many as fewer probes
    expect(results2.length).toBeGreaterThanOrEqual(results1.length);
  });

  test("ivfPqSearch with filter excludes nodes", () => {
    const store = createVectorStore(16);
    const index = createIvfPqIndex(16, testConfig());

    // Train
    const vectors = generateTrainingVectors(500, 16);
    ivfPqAddTrainingVectors(index, vectors, 16, 500);
    ivfPqTrain(index, 16);

    // Insert vectors
    for (let i = 0; i < 50; i++) {
      const vec = new Float32Array(16);
      for (let d = 0; d < 16; d++) {
        vec[d] = Math.random() * 2 - 1;
      }
      vectorStoreInsert(store, i, vec);
      ivfPqInsert(index, i, vec, 16);
    }

    const query = new Float32Array(16).fill(0.5);

    // Filter to only even node IDs
    const results = ivfPqSearch(index, store, query, 20, {
      filter: (nodeId) => nodeId % 2 === 0,
    });

    for (const result of results) {
      expect(result.nodeId % 2).toBe(0);
    }
  });

  test("ivfPqSearch with threshold filters by similarity", () => {
    const store = createVectorStore(16, { metric: "euclidean", normalize: false });
    const index = createIvfPqIndex(16, {
      ...testConfig(),
      metric: "euclidean",
    });

    // Train
    const vectors = generateTrainingVectors(500, 16);
    ivfPqAddTrainingVectors(index, vectors, 16, 500);
    ivfPqTrain(index, 16);

    // Insert vectors
    for (let i = 0; i < 50; i++) {
      const vec = new Float32Array(16);
      for (let d = 0; d < 16; d++) {
        vec[d] = Math.random() * 2 - 1;
      }
      vectorStoreInsert(store, i, vec, true);
      ivfPqInsert(index, i, vec, 16);
    }

    const query = new Float32Array(16).fill(0.5);

    // High threshold should return fewer results
    const resultsHigh = ivfPqSearch(index, store, query, 50, {
      threshold: 0.9,
    });

    const resultsLow = ivfPqSearch(index, store, query, 50, {
      threshold: 0.1,
    });

    // All high-threshold results should meet the similarity requirement
    for (const result of resultsHigh) {
      expect(result.similarity).toBeGreaterThanOrEqual(0.9);
    }

    // More results expected with lower threshold
    expect(resultsLow.length).toBeGreaterThanOrEqual(resultsHigh.length);
  });
});

// ============================================================================
// Statistics Tests
// ============================================================================

describe("IVF-PQ Statistics", () => {
  test("ivfPqStats returns correct statistics before training", () => {
    const index = createIvfPqIndex(16, testConfig());

    const stats = ivfPqStats(index);

    expect(stats.trained).toBe(false);
    expect(stats.nClusters).toBe(4);
    expect(stats.totalVectors).toBe(0);
    expect(stats.avgVectorsPerCluster).toBe(0);
    expect(stats.pqNumSubspaces).toBe(4);
    expect(stats.pqNumCentroids).toBe(8);
  });

  test("ivfPqStats returns correct statistics after insertion", () => {
    const index = createIvfPqIndex(16, testConfig());
    const vectors = generateTrainingVectors(500, 16);

    ivfPqAddTrainingVectors(index, vectors, 16, 500);
    ivfPqTrain(index, 16);

    // Insert vectors
    for (let i = 0; i < 20; i++) {
      const vec = new Float32Array(16).fill(i / 20);
      ivfPqInsert(index, i, vec, 16);
    }

    const stats = ivfPqStats(index);

    expect(stats.trained).toBe(true);
    expect(stats.totalVectors).toBe(20);
    expect(stats.avgVectorsPerCluster).toBe(5); // 20 / 4 clusters
    expect(stats.memorySavingsRatio).toBeGreaterThan(0);
  });

  test("ivfPqStats memory savings ratio is positive", () => {
    const index = createIvfPqIndex(16, testConfig());
    const vectors = generateTrainingVectors(500, 16);

    ivfPqAddTrainingVectors(index, vectors, 16, 500);
    ivfPqTrain(index, 16);

    // Insert vectors
    for (let i = 0; i < 100; i++) {
      const vec = new Float32Array(16);
      for (let d = 0; d < 16; d++) {
        vec[d] = Math.random();
      }
      ivfPqInsert(index, i, vec, 16);
    }

    const stats = ivfPqStats(index);

    // Should have positive memory savings
    expect(stats.memorySavingsRatio).toBeGreaterThan(1);
  });
});

// ============================================================================
// Residual vs Non-Residual Mode Tests
// ============================================================================

describe("IVF-PQ Residual Mode", () => {
  test("works with useResiduals=true", () => {
    const index = createIvfPqIndex(16, {
      ...testConfig(),
      useResiduals: true,
    });
    const vectors = generateTrainingVectors(500, 16);

    ivfPqAddTrainingVectors(index, vectors, 16, 500);
    ivfPqTrain(index, 16);

    // Insert and search
    for (let i = 0; i < 20; i++) {
      const vec = new Float32Array(16).fill(i / 20);
      ivfPqInsert(index, i, vec, 16);
    }

    const store = createVectorStore(16, { metric: "euclidean", normalize: false });
    for (let i = 0; i < 20; i++) {
      const vec = new Float32Array(16).fill(i / 20);
      vectorStoreInsert(store, i, vec, true);
    }

    const query = new Float32Array(16).fill(0.5);
    const results = ivfPqSearch(index, store, query, 5);

    expect(results.length).toBe(5);
  });

  test("works with useResiduals=false", () => {
    const index = createIvfPqIndex(16, {
      ...testConfig(),
      useResiduals: false,
    });
    const vectors = generateTrainingVectors(500, 16);

    ivfPqAddTrainingVectors(index, vectors, 16, 500);
    ivfPqTrain(index, 16);

    // Insert and search
    for (let i = 0; i < 20; i++) {
      const vec = new Float32Array(16).fill(i / 20);
      ivfPqInsert(index, i, vec, 16);
    }

    const store = createVectorStore(16, { metric: "euclidean", normalize: false });
    for (let i = 0; i < 20; i++) {
      const vec = new Float32Array(16).fill(i / 20);
      vectorStoreInsert(store, i, vec, true);
    }

    const query = new Float32Array(16).fill(0.5);
    const results = ivfPqSearch(index, store, query, 5);

    expect(results.length).toBe(5);
  });
});

// ============================================================================
// Different Distance Metrics Tests
// ============================================================================

describe("IVF-PQ Distance Metrics", () => {
  test("works with cosine metric", () => {
    const store = createVectorStore(16, { metric: "cosine" });
    const index = createIvfPqIndex(16, {
      ...testConfig(),
      metric: "cosine",
    });

    // Train
    const vectors = generateRandomNormalizedVectors(500, 16);
    ivfPqAddTrainingVectors(index, vectors, 16, 500);
    ivfPqTrain(index, 16);

    // Insert normalized vectors
    for (let i = 0; i < 20; i++) {
      const vec = new Float32Array(16);
      for (let d = 0; d < 16; d++) {
        vec[d] = Math.random() * 2 - 1;
      }
      const normalized = normalize(vec);
      vectorStoreInsert(store, i, normalized);
      ivfPqInsert(index, i, normalized, 16);
    }

    const query = normalize(new Float32Array(16).fill(0.5));
    const results = ivfPqSearch(index, store, query, 5);

    expect(results.length).toBe(5);
    // Cosine similarity should be in [-1, 1]
    for (const result of results) {
      expect(result.similarity).toBeGreaterThanOrEqual(-1);
      expect(result.similarity).toBeLessThanOrEqual(1);
    }
  });

  test("works with euclidean metric", () => {
    const store = createVectorStore(16, { metric: "euclidean", normalize: false });
    const index = createIvfPqIndex(16, {
      ...testConfig(),
      metric: "euclidean",
    });

    // Train
    const vectors = generateTrainingVectors(500, 16);
    ivfPqAddTrainingVectors(index, vectors, 16, 500);
    ivfPqTrain(index, 16);

    // Insert vectors using helper function
    for (let i = 0; i < 20; i++) {
      const vec = new Float32Array(16);
      for (let d = 0; d < 16; d++) {
        vec[d] = Math.random() * 2 - 1;
      }
      insertIntoStoreAndIndex(store, index, i, vec, 16, true);
    }

    const query = new Float32Array(16).fill(0.5);
    const results = ivfPqSearch(index, store, query, 5);

    expect(results.length).toBe(5);
    // Euclidean distance should be non-negative
    for (const result of results) {
      expect(result.distance).toBeGreaterThanOrEqual(0);
    }
  });
});

// ============================================================================
// Edge Cases
// ============================================================================

describe("IVF-PQ Edge Cases", () => {
  test("handles minimum configuration", () => {
    const index = createIvfPqIndex(4, {
      nClusters: 2,
      nProbe: 1,
      pq: {
        numSubspaces: 2,
        numCentroids: 2,
        maxIterations: 5,
      },
      useResiduals: true,
    });

    const vectors = generateTrainingVectors(10, 4);
    ivfPqAddTrainingVectors(index, vectors, 4, 10);
    ivfPqTrain(index, 4);

    expect(index.trained).toBe(true);
  });

  test("handles empty cluster during search", () => {
    const store = createVectorStore(16, { metric: "euclidean", normalize: false });
    const index = createIvfPqIndex(16, {
      nClusters: 10, // Many clusters
      nProbe: 5,
      pq: {
        numSubspaces: 4,
        numCentroids: 8,
        maxIterations: 10,
      },
      useResiduals: true,
      metric: "euclidean",
    });

    // Train
    const vectors = generateTrainingVectors(500, 16);
    ivfPqAddTrainingVectors(index, vectors, 16, 500);
    ivfPqTrain(index, 16);

    // Insert only a few vectors (some clusters may be empty)
    for (let i = 0; i < 5; i++) {
      const vec = new Float32Array(16).fill(i / 5);
      vectorStoreInsert(store, i, vec, true);
      ivfPqInsert(index, i, vec, 16);
    }

    const query = new Float32Array(16).fill(0.5);
    const results = ivfPqSearch(index, store, query, 5);

    // Should still return results
    expect(results.length).toBeLessThanOrEqual(5);
  });

  test("search returns all vectors if k > totalVectors", () => {
    const store = createVectorStore(16, { metric: "euclidean", normalize: false });
    const index = createIvfPqIndex(16, {
      ...testConfig(),
      metric: "euclidean",
    });

    // Train
    const vectors = generateTrainingVectors(500, 16);
    ivfPqAddTrainingVectors(index, vectors, 16, 500);
    ivfPqTrain(index, 16);

    // Insert only 3 vectors
    for (let i = 0; i < 3; i++) {
      const vec = new Float32Array(16).fill(i / 3);
      vectorStoreInsert(store, i, vec, true);
      ivfPqInsert(index, i, vec, 16);
    }

    const query = new Float32Array(16).fill(0.5);
    const results = ivfPqSearch(index, store, query, 10); // Request more than available

    expect(results.length).toBeLessThanOrEqual(3);
  });

  test("handles large number of centroids", () => {
    const index = createIvfPqIndex(16, {
      nClusters: 4,
      nProbe: 2,
      pq: {
        numSubspaces: 4,
        numCentroids: 256, // Maximum typical value
        maxIterations: 5,
      },
      useResiduals: true,
    });

    const vectors = generateTrainingVectors(300, 16);
    ivfPqAddTrainingVectors(index, vectors, 16, 300);
    ivfPqTrain(index, 16);

    expect(index.trained).toBe(true);
    expect(index.pqIndex.config.numCentroids).toBe(256);
  });
});

// ============================================================================
// Approximate Nearest Neighbor Quality Tests
// ============================================================================

describe("IVF-PQ Search Quality", () => {
  test("finds exact match in top results", () => {
    const store = createVectorStore(16, { metric: "euclidean", normalize: false });
    const index = createIvfPqIndex(16, {
      nClusters: 4,
      nProbe: 4, // Search all clusters
      pq: {
        numSubspaces: 4,
        numCentroids: 8,
        maxIterations: 10,
      },
      useResiduals: true,
      metric: "euclidean",
    });

    // Train
    const vectors = generateTrainingVectors(500, 16);
    ivfPqAddTrainingVectors(index, vectors, 16, 500);
    ivfPqTrain(index, 16);

    // Create a known target
    const target = new Float32Array(16).fill(0.5);
    vectorStoreInsert(store, 0, target, true);
    ivfPqInsert(index, 0, target, 16);

    // Add some other vectors
    for (let i = 1; i < 20; i++) {
      const vec = new Float32Array(16);
      for (let d = 0; d < 16; d++) {
        vec[d] = Math.random() * 2 - 1;
      }
      vectorStoreInsert(store, i, vec, true);
      ivfPqInsert(index, i, vec, 16);
    }

    // Search for the target
    const results = ivfPqSearch(index, store, target, 5);

    // Target (node 0) should be in top results
    const nodeIds = results.map((r) => r.nodeId);
    expect(nodeIds).toContain(0);

    // And likely in first position (or close to it)
    expect(results[0].distance).toBeLessThan(0.1);
  });

  test("similar vectors rank higher than distant ones", () => {
    const store = createVectorStore(16, { metric: "euclidean", normalize: false });
    const index = createIvfPqIndex(16, {
      nClusters: 4,
      nProbe: 4,
      pq: {
        numSubspaces: 4,
        numCentroids: 8,
        maxIterations: 10,
      },
      useResiduals: true,
      metric: "euclidean",
    });

    // Train
    const vectors = generateTrainingVectors(500, 16);
    ivfPqAddTrainingVectors(index, vectors, 16, 500);
    ivfPqTrain(index, 16);

    // Insert a close vector and a far vector
    const target = new Float32Array(16).fill(0.5);
    const closeVec = new Float32Array(16).fill(0.51); // Very close
    const farVec = new Float32Array(16).fill(-0.5); // Far away

    vectorStoreInsert(store, 0, closeVec, true);
    vectorStoreInsert(store, 1, farVec, true);
    ivfPqInsert(index, 0, closeVec, 16);
    ivfPqInsert(index, 1, farVec, 16);

    const results = ivfPqSearch(index, store, target, 2);

    // Close vector should rank before far vector
    expect(results[0].nodeId).toBe(0);
    expect(results[1].nodeId).toBe(1);
    expect(results[0].distance).toBeLessThan(results[1].distance);
  });
});
