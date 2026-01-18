/**
 * Tests for the vector embeddings module
 */

import { describe, expect, test } from "bun:test";

import {
  // Types and constants
  DEFAULT_VECTOR_CONFIG,
  DEFAULT_IVF_CONFIG,
  // Validation
  validateVector,
  hasNaN,
  hasInfinity,
  isZeroVector,
  // Normalization
  l2Norm,
  normalizeInPlace,
  normalize,
  isNormalized,
  normalizeRowGroup,
  normalizeVectorAt,
  // Distance functions
  dotProduct,
  cosineDistance,
  cosineSimilarity,
  squaredEuclidean,
  euclideanDistance,
  dotProductAt,
  batchCosineDistance,
  batchDotProductDistance,
  distanceToSimilarity,
  findKNearest,
  MinHeap,
  MaxHeap,
  // Row group operations
  createRowGroup,
  rowGroupAppend,
  rowGroupGet,
  rowGroupGetCopy,
  rowGroupIsFull,
  rowGroupTrim,
  // Fragment operations
  createFragment,
  fragmentAppend,
  fragmentDelete,
  fragmentIsDeleted,
  fragmentSeal,
  fragmentShouldSeal,
  fragmentGetVector,
  fragmentLiveCount,
  // Columnar store
  createVectorStore,
  vectorStoreInsert,
  vectorStoreDelete,
  vectorStoreGet,
  vectorStoreHas,
  vectorStoreIterator,
  vectorStoreBatchInsert,
  vectorStoreStats,
  vectorStoreClear,
  vectorStoreClone,
  // IVF index
  createIvfIndex,
  ivfAddTrainingVectors,
  ivfTrain,
  ivfInsert,
  ivfSearch,
  ivfSearchMulti,
  ivfBuildFromStore,
  ivfStats,
  // Compaction
  findFragmentsToCompact,
  compactFragments,
  applyCompaction,
  getCompactionStats,
  clearDeletedFragments,
  // Serialization
  serializeIvf,
  deserializeIvf,
  serializeManifest,
  deserializeManifest,
} from "../src/vector/index.ts";

// ============================================================================
// Helper Functions
// ============================================================================

/**
 * Create a random vector
 */
function randomVector(dimensions: number): Float32Array {
  const v = new Float32Array(dimensions);
  for (let i = 0; i < dimensions; i++) {
    v[i] = Math.random() * 2 - 1; // Range [-1, 1]
  }
  return v;
}

/**
 * Create a random normalized vector
 */
function randomNormalizedVector(dimensions: number): Float32Array {
  const v = randomVector(dimensions);
  normalizeInPlace(v);
  return v;
}

/**
 * Create a vector with specific pattern for testing
 */
function patternVector(dimensions: number, pattern: number): Float32Array {
  const v = new Float32Array(dimensions);
  for (let i = 0; i < dimensions; i++) {
    v[i] = Math.sin(i * pattern) + Math.cos(i * pattern * 0.5);
  }
  return normalize(v);
}

// ============================================================================
// Normalization Tests
// ============================================================================

describe("Vector Normalization", () => {
  test("l2Norm computes correct norm", () => {
    const v = new Float32Array([3, 4]);
    expect(l2Norm(v)).toBeCloseTo(5, 5);

    const v2 = new Float32Array([1, 0, 0]);
    expect(l2Norm(v2)).toBeCloseTo(1, 5);

    const v3 = new Float32Array([1, 1, 1, 1]);
    expect(l2Norm(v3)).toBeCloseTo(2, 5);
  });

  test("normalizeInPlace normalizes vector", () => {
    const v = new Float32Array([3, 4]);
    const norm = normalizeInPlace(v);

    expect(norm).toBeCloseTo(5, 5);
    expect(l2Norm(v)).toBeCloseTo(1, 5);
    expect(v[0]).toBeCloseTo(0.6, 5);
    expect(v[1]).toBeCloseTo(0.8, 5);
  });

  test("normalize returns new array", () => {
    const v = new Float32Array([3, 4]);
    const normalized = normalize(v);

    // Original unchanged
    expect(v[0]).toBe(3);
    expect(v[1]).toBe(4);

    // New array is normalized
    expect(l2Norm(normalized)).toBeCloseTo(1, 5);
  });

  test("isNormalized checks normalization", () => {
    const normalized = normalize(new Float32Array([3, 4]));
    expect(isNormalized(normalized)).toBe(true);

    const notNormalized = new Float32Array([3, 4]);
    expect(isNormalized(notNormalized)).toBe(false);
  });

  test("normalizeRowGroup normalizes multiple vectors", () => {
    const dimensions = 4;
    const count = 3;
    const data = new Float32Array(dimensions * count);

    // Set up 3 vectors
    data.set([1, 2, 3, 4], 0);
    data.set([5, 6, 7, 8], 4);
    data.set([9, 10, 11, 12], 8);

    normalizeRowGroup(data, dimensions, count);

    // Check each vector is normalized
    for (let i = 0; i < count; i++) {
      let sum = 0;
      for (let d = 0; d < dimensions; d++) {
        sum += data[i * dimensions + d] ** 2;
      }
      expect(Math.sqrt(sum)).toBeCloseTo(1, 5);
    }
  });

  test("normalizeVectorAt normalizes single vector in row group", () => {
    const dimensions = 4;
    const data = new Float32Array([1, 2, 3, 4, 5, 6, 7, 8]); // 2 vectors

    normalizeVectorAt(data, dimensions, 1);

    // First vector unchanged
    expect(data[0]).toBe(1);

    // Second vector normalized
    let sum = 0;
    for (let d = 0; d < dimensions; d++) {
      sum += data[4 + d] ** 2;
    }
    expect(Math.sqrt(sum)).toBeCloseTo(1, 5);
  });
});

// ============================================================================
// Vector Validation Tests
// ============================================================================

describe("Vector Validation", () => {
  test("validateVector accepts valid vectors", () => {
    const valid = new Float32Array([1, 2, 3, 4]);
    const result = validateVector(valid);
    expect(result.valid).toBe(true);
    expect(result.error).toBeUndefined();
  });

  test("validateVector rejects NaN values", () => {
    const withNaN = new Float32Array([1, NaN, 3, 4]);
    const result = validateVector(withNaN);
    expect(result.valid).toBe(false);
    expect(result.error).toBe("contains_nan");
    expect(result.message).toContain("NaN");
    expect(result.message).toContain("index 1");
  });

  test("validateVector rejects Infinity values", () => {
    const withInf = new Float32Array([1, 2, Infinity, 4]);
    const result = validateVector(withInf);
    expect(result.valid).toBe(false);
    expect(result.error).toBe("contains_infinity");
    expect(result.message).toContain("Infinity");
  });

  test("validateVector rejects negative Infinity", () => {
    const withNegInf = new Float32Array([1, -Infinity, 3, 4]);
    const result = validateVector(withNegInf);
    expect(result.valid).toBe(false);
    expect(result.error).toBe("contains_infinity");
  });

  test("validateVector rejects zero vectors", () => {
    const zero = new Float32Array([0, 0, 0, 0]);
    const result = validateVector(zero);
    expect(result.valid).toBe(false);
    expect(result.error).toBe("zero_vector");
    expect(result.message).toContain("zero vector");
  });

  test("validateVector accepts vectors with some zeros", () => {
    const sparse = new Float32Array([0, 1, 0, 0]);
    const result = validateVector(sparse);
    expect(result.valid).toBe(true);
  });

  test("hasNaN detects NaN values", () => {
    expect(hasNaN(new Float32Array([1, 2, 3]))).toBe(false);
    expect(hasNaN(new Float32Array([1, NaN, 3]))).toBe(true);
    expect(hasNaN(new Float32Array([NaN]))).toBe(true);
  });

  test("hasInfinity detects Infinity values", () => {
    expect(hasInfinity(new Float32Array([1, 2, 3]))).toBe(false);
    expect(hasInfinity(new Float32Array([1, Infinity, 3]))).toBe(true);
    expect(hasInfinity(new Float32Array([1, -Infinity, 3]))).toBe(true);
  });

  test("isZeroVector detects all-zero vectors", () => {
    expect(isZeroVector(new Float32Array([0, 0, 0]))).toBe(true);
    expect(isZeroVector(new Float32Array([0, 1, 0]))).toBe(false);
    expect(isZeroVector(new Float32Array([0.0001, 0, 0]))).toBe(false);
  });

  test("vectorStoreInsert rejects invalid vectors", () => {
    const store = createVectorStore(4);
    
    // NaN
    expect(() => {
      vectorStoreInsert(store, 1, new Float32Array([1, NaN, 3, 4]));
    }).toThrow(/Invalid vector.*NaN/);

    // Infinity
    expect(() => {
      vectorStoreInsert(store, 2, new Float32Array([1, 2, Infinity, 4]));
    }).toThrow(/Invalid vector.*Infinity/);

    // Zero vector
    expect(() => {
      vectorStoreInsert(store, 3, new Float32Array([0, 0, 0, 0]));
    }).toThrow(/Invalid vector.*zero vector/);
  });

  test("vectorStoreInsert with skipValidation bypasses checks", () => {
    const store = createVectorStore(4);
    
    // This would normally throw, but skipValidation allows it
    // (useful for performance when you trust the data source)
    expect(() => {
      vectorStoreInsert(store, 1, new Float32Array([0, 0, 0, 0]), true);
    }).not.toThrow();
  });
});

// ============================================================================
// Distance Function Tests
// ============================================================================

describe("Distance Functions", () => {
  test("dotProduct computes correct value", () => {
    const a = new Float32Array([1, 2, 3]);
    const b = new Float32Array([4, 5, 6]);
    expect(dotProduct(a, b)).toBe(32); // 1*4 + 2*5 + 3*6 = 32
  });

  test("cosineDistance for normalized vectors", () => {
    const a = normalize(new Float32Array([1, 0]));
    const b = normalize(new Float32Array([0, 1]));

    // Orthogonal vectors: cosine similarity = 0, distance = 1
    expect(cosineDistance(a, b)).toBeCloseTo(1, 5);

    // Same direction: distance = 0
    expect(cosineDistance(a, a)).toBeCloseTo(0, 5);

    // Opposite direction: distance = 2
    const c = normalize(new Float32Array([-1, 0]));
    expect(cosineDistance(a, c)).toBeCloseTo(2, 5);
  });

  test("cosineSimilarity is 1 - distance", () => {
    const a = normalize(new Float32Array([1, 2, 3]));
    const b = normalize(new Float32Array([4, 5, 6]));

    const dist = cosineDistance(a, b);
    const sim = cosineSimilarity(a, b);

    expect(dist + sim).toBeCloseTo(1, 5);
  });

  test("squaredEuclidean computes correct value", () => {
    const a = new Float32Array([1, 2, 3]);
    const b = new Float32Array([4, 6, 8]);

    // (4-1)^2 + (6-2)^2 + (8-3)^2 = 9 + 16 + 25 = 50
    expect(squaredEuclidean(a, b)).toBe(50);
  });

  test("euclideanDistance is sqrt of squared", () => {
    const a = new Float32Array([0, 0]);
    const b = new Float32Array([3, 4]);

    expect(euclideanDistance(a, b)).toBeCloseTo(5, 5);
  });

  test("dotProductAt computes distance in row group", () => {
    const query = normalize(new Float32Array([1, 0, 0, 0]));
    const dimensions = 4;

    // Row group with 2 vectors
    const rowGroupData = new Float32Array(8);
    rowGroupData.set(normalize(new Float32Array([1, 0, 0, 0])), 0);
    rowGroupData.set(normalize(new Float32Array([0, 1, 0, 0])), 4);

    expect(dotProductAt(query, rowGroupData, dimensions, 0)).toBeCloseTo(1, 5);
    expect(dotProductAt(query, rowGroupData, dimensions, 1)).toBeCloseTo(0, 5);
  });

  test("batchCosineDistance computes multiple distances", () => {
    const dimensions = 4;
    const query = normalize(new Float32Array([1, 0, 0, 0]));

    const rowGroupData = new Float32Array(12); // 3 vectors
    rowGroupData.set(normalize(new Float32Array([1, 0, 0, 0])), 0);
    rowGroupData.set(normalize(new Float32Array([0, 1, 0, 0])), 4);
    rowGroupData.set(normalize(new Float32Array([-1, 0, 0, 0])), 8);

    const distances = batchCosineDistance(query, rowGroupData, dimensions, 0, 3);

    expect(distances[0]).toBeCloseTo(0, 5); // Same direction
    expect(distances[1]).toBeCloseTo(1, 5); // Orthogonal
    expect(distances[2]).toBeCloseTo(2, 5); // Opposite
  });

  test("batchDotProductDistance computes negated dot products", () => {
    const dimensions = 4;
    const query = normalize(new Float32Array([1, 0, 0, 0]));

    const rowGroupData = new Float32Array(12); // 3 vectors
    rowGroupData.set(normalize(new Float32Array([1, 0, 0, 0])), 0);
    rowGroupData.set(normalize(new Float32Array([0, 1, 0, 0])), 4);
    rowGroupData.set(normalize(new Float32Array([-1, 0, 0, 0])), 8);

    const distances = batchDotProductDistance(query, rowGroupData, dimensions, 0, 3);

    // Dot product distance is -dot, so:
    // Same direction: dot=1, distance=-1
    // Orthogonal: dot=0, distance=0
    // Opposite: dot=-1, distance=1
    expect(distances[0]).toBeCloseTo(-1, 5); // Same direction (highest similarity = lowest distance)
    expect(distances[1]).toBeCloseTo(0, 5);  // Orthogonal
    expect(distances[2]).toBeCloseTo(1, 5);  // Opposite (lowest similarity = highest distance)
  });

  test("distanceToSimilarity converts correctly", () => {
    // Cosine: similarity = 1 - distance
    expect(distanceToSimilarity(0, "cosine")).toBeCloseTo(1, 5);
    expect(distanceToSimilarity(1, "cosine")).toBeCloseTo(0, 5);

    // Euclidean: similarity = 1 / (1 + sqrt(distance))
    expect(distanceToSimilarity(0, "euclidean")).toBeCloseTo(1, 5);
    expect(distanceToSimilarity(4, "euclidean")).toBeCloseTo(1 / 3, 5);

    // Dot product: distance is -dot, so similarity = -distance = dot
    expect(distanceToSimilarity(-1, "dot")).toBeCloseTo(1, 5);  // distance=-1 means dot=1
    expect(distanceToSimilarity(0, "dot")).toBeCloseTo(0, 5);   // distance=0 means dot=0
    expect(distanceToSimilarity(1, "dot")).toBeCloseTo(-1, 5);  // distance=1 means dot=-1
  });

  test("findKNearest returns sorted results", () => {
    const distances = new Float32Array([0.5, 0.1, 0.9, 0.3]);
    const nearest = findKNearest(distances, 2);

    expect(nearest.length).toBe(2);
    expect(nearest[0].index).toBe(1);
    expect(nearest[0].distance).toBeCloseTo(0.1, 5);
    expect(nearest[1].index).toBe(3);
    expect(nearest[1].distance).toBeCloseTo(0.3, 5);
  });
});

// ============================================================================
// Heap Tests
// ============================================================================

describe("Heap Data Structures", () => {
  test("MinHeap maintains min order", () => {
    const heap = new MinHeap();

    heap.push(1, 0.5);
    heap.push(2, 0.1);
    heap.push(3, 0.9);

    expect(heap.peek()?.id).toBe(2);
    expect(heap.peek()?.distance).toBeCloseTo(0.1, 5);

    const first = heap.pop();
    expect(first?.id).toBe(2);

    const second = heap.pop();
    expect(second?.id).toBe(1);

    const third = heap.pop();
    expect(third?.id).toBe(3);
  });

  test("MaxHeap maintains max order", () => {
    const heap = new MaxHeap();

    heap.push(1, 0.5);
    heap.push(2, 0.1);
    heap.push(3, 0.9);

    expect(heap.peek()?.id).toBe(3);
    expect(heap.peek()?.distance).toBeCloseTo(0.9, 5);

    const first = heap.pop();
    expect(first?.id).toBe(3);

    const second = heap.pop();
    expect(second?.id).toBe(1);

    const third = heap.pop();
    expect(third?.id).toBe(2);
  });
});

// ============================================================================
// Row Group Tests
// ============================================================================

describe("Row Group Operations", () => {
  const dimensions = 8;
  const capacity = 100;

  test("createRowGroup initializes correctly", () => {
    const rg = createRowGroup(0, dimensions, capacity);

    expect(rg.id).toBe(0);
    expect(rg.count).toBe(0);
    expect(rg.data.length).toBe(dimensions * capacity);
  });

  test("rowGroupAppend adds vectors", () => {
    const rg = createRowGroup(0, dimensions, capacity);
    const v1 = randomVector(dimensions);
    const v2 = randomVector(dimensions);

    const idx1 = rowGroupAppend(rg, v1, dimensions, false);
    const idx2 = rowGroupAppend(rg, v2, dimensions, false);

    expect(idx1).toBe(0);
    expect(idx2).toBe(1);
    expect(rg.count).toBe(2);
  });

  test("rowGroupAppend normalizes when requested", () => {
    const rg = createRowGroup(0, dimensions, capacity);
    const v = randomVector(dimensions);

    rowGroupAppend(rg, v, dimensions, true);

    const stored = rowGroupGet(rg, 0, dimensions);
    expect(isNormalized(stored)).toBe(true);
  });

  test("rowGroupGet returns correct vector", () => {
    const rg = createRowGroup(0, dimensions, capacity);
    const v = new Float32Array([1, 2, 3, 4, 5, 6, 7, 8]);

    rowGroupAppend(rg, v, dimensions, false);

    const retrieved = rowGroupGet(rg, 0, dimensions);
    expect(retrieved.length).toBe(dimensions);
    expect(retrieved[0]).toBe(1);
    expect(retrieved[7]).toBe(8);
  });

  test("rowGroupGetCopy returns independent copy", () => {
    const rg = createRowGroup(0, dimensions, capacity);
    const v = new Float32Array([1, 2, 3, 4, 5, 6, 7, 8]);

    rowGroupAppend(rg, v, dimensions, false);

    const copy = rowGroupGetCopy(rg, 0, dimensions);
    copy[0] = 999;

    // Original should be unchanged
    const original = rowGroupGet(rg, 0, dimensions);
    expect(original[0]).toBe(1);
  });

  test("rowGroupIsFull detects full row group", () => {
    const smallCapacity = 2;
    const rg = createRowGroup(0, dimensions, smallCapacity);

    expect(rowGroupIsFull(rg, dimensions, smallCapacity)).toBe(false);

    rowGroupAppend(rg, randomVector(dimensions), dimensions, false);
    expect(rowGroupIsFull(rg, dimensions, smallCapacity)).toBe(false);

    rowGroupAppend(rg, randomVector(dimensions), dimensions, false);
    expect(rowGroupIsFull(rg, dimensions, smallCapacity)).toBe(true);
  });

  test("rowGroupTrim reduces size", () => {
    const rg = createRowGroup(0, dimensions, capacity);
    rowGroupAppend(rg, randomVector(dimensions), dimensions, false);
    rowGroupAppend(rg, randomVector(dimensions), dimensions, false);

    const trimmed = rowGroupTrim(rg, dimensions);

    expect(trimmed.data.length).toBe(2 * dimensions);
    expect(trimmed.count).toBe(2);
  });
});

// ============================================================================
// Fragment Tests
// ============================================================================

describe("Fragment Operations", () => {
  const config = {
    dimensions: 8,
    metric: "cosine" as const,
    rowGroupSize: 10,
    fragmentTargetSize: 100,
    normalize: true,
  };

  test("createFragment initializes correctly", () => {
    const fragment = createFragment(0, config);

    expect(fragment.id).toBe(0);
    expect(fragment.state).toBe("active");
    expect(fragment.rowGroups.length).toBe(1);
    expect(fragment.totalVectors).toBe(0);
    expect(fragment.deletedCount).toBe(0);
  });

  test("fragmentAppend adds vectors", () => {
    const fragment = createFragment(0, config);
    const v1 = randomVector(config.dimensions);
    const v2 = randomVector(config.dimensions);

    const idx1 = fragmentAppend(fragment, v1, config);
    const idx2 = fragmentAppend(fragment, v2, config);

    expect(idx1).toBe(0);
    expect(idx2).toBe(1);
    expect(fragment.totalVectors).toBe(2);
  });

  test("fragmentAppend creates new row groups when needed", () => {
    const fragment = createFragment(0, config);

    // Add more vectors than row group size
    for (let i = 0; i < 15; i++) {
      fragmentAppend(fragment, randomVector(config.dimensions), config);
    }

    expect(fragment.rowGroups.length).toBe(2);
    expect(fragment.totalVectors).toBe(15);
  });

  test("fragmentDelete marks vectors as deleted", () => {
    const fragment = createFragment(0, config);
    fragmentAppend(fragment, randomVector(config.dimensions), config);
    fragmentAppend(fragment, randomVector(config.dimensions), config);

    expect(fragmentDelete(fragment, 0)).toBe(true);
    expect(fragment.deletedCount).toBe(1);
    expect(fragmentIsDeleted(fragment, 0)).toBe(true);
    expect(fragmentIsDeleted(fragment, 1)).toBe(false);

    // Can't delete twice
    expect(fragmentDelete(fragment, 0)).toBe(false);
    expect(fragment.deletedCount).toBe(1);
  });

  test("fragmentSeal marks fragment as sealed", () => {
    const fragment = createFragment(0, config);
    fragmentAppend(fragment, randomVector(config.dimensions), config);

    fragmentSeal(fragment);

    expect(fragment.state).toBe("sealed");
  });

  test("fragmentAppend throws on sealed fragment", () => {
    const fragment = createFragment(0, config);
    fragmentSeal(fragment);

    expect(() =>
      fragmentAppend(fragment, randomVector(config.dimensions), config)
    ).toThrow();
  });

  test("fragmentShouldSeal returns true when at target size", () => {
    const smallConfig = { ...config, fragmentTargetSize: 5 };
    const fragment = createFragment(0, smallConfig);

    for (let i = 0; i < 4; i++) {
      fragmentAppend(fragment, randomVector(smallConfig.dimensions), smallConfig);
    }
    expect(fragmentShouldSeal(fragment, smallConfig)).toBe(false);

    fragmentAppend(fragment, randomVector(smallConfig.dimensions), smallConfig);
    expect(fragmentShouldSeal(fragment, smallConfig)).toBe(true);
  });

  test("fragmentGetVector returns null for deleted vectors", () => {
    const fragment = createFragment(0, config);
    fragmentAppend(fragment, randomVector(config.dimensions), config);

    expect(fragmentGetVector(fragment, 0, config.dimensions)).not.toBeNull();

    fragmentDelete(fragment, 0);
    expect(fragmentGetVector(fragment, 0, config.dimensions)).toBeNull();
  });

  test("fragmentGetVector works after seal with single row group", () => {
    // This tests the edge case where a single row group is trimmed on seal
    const fragment = createFragment(0, config);
    const v1 = randomNormalizedVector(config.dimensions);
    const v2 = randomNormalizedVector(config.dimensions);
    
    fragmentAppend(fragment, v1, config);
    fragmentAppend(fragment, v2, config);
    
    // Seal the fragment - this trims the single row group
    fragmentSeal(fragment);
    
    // Verify we can still retrieve vectors after sealing
    const retrieved1 = fragmentGetVector(fragment, 0, config.dimensions);
    const retrieved2 = fragmentGetVector(fragment, 1, config.dimensions);
    
    expect(retrieved1).not.toBeNull();
    expect(retrieved2).not.toBeNull();
    
    // Verify the data is correct
    for (let i = 0; i < config.dimensions; i++) {
      expect(retrieved1![i]).toBeCloseTo(v1[i], 5);
      expect(retrieved2![i]).toBeCloseTo(v2[i], 5);
    }
  });

  test("fragmentGetVector works with multiple row groups after seal", () => {
    const fragment = createFragment(0, config);
    const vectors: Float32Array[] = [];
    
    // Add more vectors than one row group can hold
    for (let i = 0; i < 15; i++) {
      const v = randomNormalizedVector(config.dimensions);
      vectors.push(v);
      fragmentAppend(fragment, v, config);
    }
    
    // Should have 2 row groups now (rowGroupSize is 10)
    expect(fragment.rowGroups.length).toBe(2);
    
    // Seal the fragment
    fragmentSeal(fragment);
    
    // Verify we can still retrieve all vectors
    for (let i = 0; i < 15; i++) {
      const retrieved = fragmentGetVector(fragment, i, config.dimensions);
      expect(retrieved).not.toBeNull();
      
      for (let d = 0; d < config.dimensions; d++) {
        expect(retrieved![d]).toBeCloseTo(vectors[i][d], 5);
      }
    }
  });

  test("fragmentLiveCount returns correct count", () => {
    const fragment = createFragment(0, config);

    for (let i = 0; i < 5; i++) {
      fragmentAppend(fragment, randomVector(config.dimensions), config);
    }

    expect(fragmentLiveCount(fragment)).toBe(5);

    fragmentDelete(fragment, 1);
    fragmentDelete(fragment, 3);

    expect(fragmentLiveCount(fragment)).toBe(3);
  });
});

// ============================================================================
// Columnar Store Tests
// ============================================================================

describe("Columnar Vector Store", () => {
  const dimensions = 16;

  test("createVectorStore initializes correctly", () => {
    const store = createVectorStore(dimensions);

    expect(store.config.dimensions).toBe(dimensions);
    expect(store.fragments.length).toBe(1);
    expect(store.totalVectors).toBe(0);
    expect(store.nextVectorId).toBe(0);
  });

  test("vectorStoreInsert adds vectors", () => {
    const store = createVectorStore(dimensions);

    const id1 = vectorStoreInsert(store, 100, randomVector(dimensions));
    const id2 = vectorStoreInsert(store, 101, randomVector(dimensions));

    expect(id1).toBe(0);
    expect(id2).toBe(1);
    expect(store.totalVectors).toBe(2);
    expect(vectorStoreHas(store, 100)).toBe(true);
    expect(vectorStoreHas(store, 101)).toBe(true);
  });

  test("vectorStoreInsert replaces existing vector", () => {
    const store = createVectorStore(dimensions);
    const v1 = randomVector(dimensions);
    const v2 = randomVector(dimensions);

    vectorStoreInsert(store, 100, v1);
    vectorStoreInsert(store, 100, v2);

    // Should have only 1 vector (replacement)
    expect(store.nodeIdToVectorId.size).toBe(1);
  });

  test("vectorStoreDelete removes vectors", () => {
    const store = createVectorStore(dimensions);
    vectorStoreInsert(store, 100, randomVector(dimensions));

    expect(vectorStoreDelete(store, 100)).toBe(true);
    expect(vectorStoreHas(store, 100)).toBe(false);
    expect(store.totalDeleted).toBe(1);
  });

  test("vectorStoreGet retrieves vectors", () => {
    const store = createVectorStore(dimensions);
    const v = randomNormalizedVector(dimensions);
    vectorStoreInsert(store, 100, v);

    const retrieved = vectorStoreGet(store, 100);
    expect(retrieved).not.toBeNull();

    // Check values match (normalized)
    for (let i = 0; i < dimensions; i++) {
      expect(retrieved![i]).toBeCloseTo(v[i], 3);
    }
  });

  test("vectorStoreIterator yields all live vectors", () => {
    const store = createVectorStore(dimensions);

    vectorStoreInsert(store, 100, randomVector(dimensions));
    vectorStoreInsert(store, 101, randomVector(dimensions));
    vectorStoreInsert(store, 102, randomVector(dimensions));
    vectorStoreDelete(store, 101);

    const results = [...vectorStoreIterator(store)];
    expect(results.length).toBe(2);

    const nodeIds = results.map((r) => r[1]);
    expect(nodeIds).toContain(100);
    expect(nodeIds).toContain(102);
    expect(nodeIds).not.toContain(101);
  });

  test("vectorStoreBatchInsert adds multiple vectors", () => {
    const store = createVectorStore(dimensions);

    const entries = Array.from({ length: 50 }, (_, i) => ({
      nodeId: i,
      vector: randomVector(dimensions),
    }));

    const ids = vectorStoreBatchInsert(store, entries);

    expect(ids.length).toBe(50);
    expect(store.totalVectors).toBe(50);
  });

  test("vectorStoreStats returns correct statistics", () => {
    const store = createVectorStore(dimensions);

    for (let i = 0; i < 10; i++) {
      vectorStoreInsert(store, i, randomVector(dimensions));
    }
    vectorStoreDelete(store, 5);

    const stats = vectorStoreStats(store);

    expect(stats.totalVectors).toBe(10);
    expect(stats.totalDeleted).toBe(1);
    expect(stats.liveVectors).toBe(9);
    expect(stats.dimensions).toBe(dimensions);
  });

  test("vectorStoreClear removes all data", () => {
    const store = createVectorStore(dimensions);

    for (let i = 0; i < 10; i++) {
      vectorStoreInsert(store, i, randomVector(dimensions));
    }

    vectorStoreClear(store);

    expect(store.totalVectors).toBe(0);
    expect(store.nodeIdToVectorId.size).toBe(0);
  });

  test("vectorStoreClone creates independent copy", () => {
    const store = createVectorStore(dimensions);
    vectorStoreInsert(store, 100, randomVector(dimensions));

    const clone = vectorStoreClone(store);
    vectorStoreDelete(clone, 100);

    // Original should be unchanged
    expect(vectorStoreHas(store, 100)).toBe(true);
    expect(vectorStoreHas(clone, 100)).toBe(false);
  });
});

// ============================================================================
// IVF Index Tests
// ============================================================================

describe("IVF Index", () => {
  const dimensions = 32;

  test("createIvfIndex initializes correctly", () => {
    const index = createIvfIndex(dimensions);

    expect(index.trained).toBe(false);
    expect(index.config.nClusters).toBe(DEFAULT_IVF_CONFIG.nClusters);
    expect(index.centroids.length).toBe(DEFAULT_IVF_CONFIG.nClusters * dimensions);
  });

  test("ivfTrain trains the index", () => {
    const index = createIvfIndex(dimensions, { nClusters: 8 });

    // Add training vectors
    const trainingData = new Float32Array(100 * dimensions);
    for (let i = 0; i < 100; i++) {
      const v = randomNormalizedVector(dimensions);
      trainingData.set(v, i * dimensions);
    }

    ivfAddTrainingVectors(index, trainingData, dimensions, 100);
    ivfTrain(index, dimensions);

    expect(index.trained).toBe(true);
    expect(index.invertedLists.size).toBe(8);
  });

  test("ivfTrain requires enough training vectors", () => {
    const index = createIvfIndex(dimensions, { nClusters: 100 });

    const trainingData = new Float32Array(10 * dimensions);
    ivfAddTrainingVectors(index, trainingData, dimensions, 10);

    expect(() => ivfTrain(index, dimensions)).toThrow();
  });

  test("ivfInsert adds vectors to index", () => {
    const index = createIvfIndex(dimensions, { nClusters: 4 });

    // Train first
    const trainingData = new Float32Array(50 * dimensions);
    for (let i = 0; i < 50; i++) {
      trainingData.set(randomNormalizedVector(dimensions), i * dimensions);
    }
    ivfAddTrainingVectors(index, trainingData, dimensions, 50);
    ivfTrain(index, dimensions);

    // Insert vectors
    const v = randomNormalizedVector(dimensions);
    ivfInsert(index, 0, v, dimensions);

    const stats = ivfStats(index);
    expect(stats.totalVectors).toBe(1);
  });

  test("ivfSearch finds nearest neighbors", () => {
    const store = createVectorStore(dimensions);
    // Use only 2 clusters since we have few vectors
    const index = createIvfIndex(dimensions, { nClusters: 2, nProbe: 2 });

    // Create vectors with known patterns
    const baseVector = patternVector(dimensions, 1.0);
    const similarVector = patternVector(dimensions, 1.1);
    const differentVector = patternVector(dimensions, 5.0);

    vectorStoreInsert(store, 1, baseVector);
    vectorStoreInsert(store, 2, similarVector);
    vectorStoreInsert(store, 3, differentVector);

    // Build index
    ivfBuildFromStore(index, store);

    // Search for similar to base
    const results = ivfSearch(index, store, baseVector, 2);

    expect(results.length).toBe(2);
    // Base vector should be most similar to itself
    expect(results[0].nodeId).toBe(1);
    // Similar vector should be second
    expect(results[1].nodeId).toBe(2);
  });

  test("ivfSearch respects filter", () => {
    const store = createVectorStore(dimensions);
    const index = createIvfIndex(dimensions, { nClusters: 4, nProbe: 4 });

    // Add vectors
    for (let i = 0; i < 20; i++) {
      vectorStoreInsert(store, i, randomNormalizedVector(dimensions));
    }

    ivfBuildFromStore(index, store);

    const query = randomNormalizedVector(dimensions);

    // Filter to only even node IDs
    const results = ivfSearch(index, store, query, 10, {
      filter: (nodeId) => nodeId % 2 === 0,
    });

    for (const result of results) {
      expect(result.nodeId % 2).toBe(0);
    }
  });

  test("ivfSearch respects threshold", () => {
    const store = createVectorStore(dimensions);
    const index = createIvfIndex(dimensions, { nClusters: 4, nProbe: 4 });

    // Add vectors
    for (let i = 0; i < 20; i++) {
      vectorStoreInsert(store, i, randomNormalizedVector(dimensions));
    }

    ivfBuildFromStore(index, store);

    const query = randomNormalizedVector(dimensions);

    // High threshold should return fewer results
    const resultsHigh = ivfSearch(index, store, query, 20, {
      threshold: 0.9,
    });

    const resultsLow = ivfSearch(index, store, query, 20, {
      threshold: 0.1,
    });

    // All results should meet the threshold
    for (const result of resultsHigh) {
      expect(result.similarity).toBeGreaterThanOrEqual(0.9);
    }
  });

  test("ivfStats returns correct statistics", () => {
    const index = createIvfIndex(dimensions, { nClusters: 4 });

    const trainingData = new Float32Array(50 * dimensions);
    for (let i = 0; i < 50; i++) {
      trainingData.set(randomNormalizedVector(dimensions), i * dimensions);
    }
    ivfAddTrainingVectors(index, trainingData, dimensions, 50);
    ivfTrain(index, dimensions);

    // Add some vectors
    for (let i = 0; i < 20; i++) {
      ivfInsert(index, i, randomNormalizedVector(dimensions), dimensions);
    }

    const stats = ivfStats(index);

    expect(stats.trained).toBe(true);
    expect(stats.nClusters).toBe(4);
    expect(stats.totalVectors).toBe(20);
    expect(stats.avgVectorsPerCluster).toBe(5);
  });

  test("IVF with euclidean metric", () => {
    const store = createVectorStore(dimensions, { metric: "euclidean", normalize: false });
    const index = createIvfIndex(dimensions, { nClusters: 2, nProbe: 2, metric: "euclidean" });

    // Create vectors with known distances
    // v1 = [1,0,0,...], v2 = [2,0,0,...], v3 = [10,0,0,...]
    const v1 = new Float32Array(dimensions).fill(0); v1[0] = 1;
    const v2 = new Float32Array(dimensions).fill(0); v2[0] = 2;
    const v3 = new Float32Array(dimensions).fill(0); v3[0] = 10;

    vectorStoreInsert(store, 1, v1, true); // skip validation since not normalized
    vectorStoreInsert(store, 2, v2, true);
    vectorStoreInsert(store, 3, v3, true);

    ivfBuildFromStore(index, store);

    // Query with v1 - should find v2 as closest (distance 1), then v3 (distance 9)
    const query = new Float32Array(dimensions).fill(0); query[0] = 1;
    const results = ivfSearch(index, store, query, 3);

    expect(results.length).toBe(3);
    // Euclidean distance: v1 is identical (dist=0), v2 is close (squared dist=1), v3 is far (squared dist=81)
    expect(results[0].nodeId).toBe(1); // Itself
    expect(results[0].distance).toBeCloseTo(0, 5);
    expect(results[1].nodeId).toBe(2);
    expect(results[1].distance).toBeCloseTo(1, 5); // squared euclidean
    expect(results[2].nodeId).toBe(3);
    expect(results[2].distance).toBeCloseTo(81, 5);
  });

  test("IVF with dot product metric", () => {
    const store = createVectorStore(dimensions, { metric: "dot", normalize: false });
    const index = createIvfIndex(dimensions, { nClusters: 2, nProbe: 2, metric: "dot" });

    // Create vectors where dot product ordering is different from cosine
    const v1 = new Float32Array(dimensions).fill(0); v1[0] = 1;
    const v2 = new Float32Array(dimensions).fill(0); v2[0] = 5;  // Higher magnitude
    const v3 = new Float32Array(dimensions).fill(0); v3[0] = -1; // Opposite direction

    vectorStoreInsert(store, 1, v1, true);
    vectorStoreInsert(store, 2, v2, true);
    vectorStoreInsert(store, 3, v3, true);

    ivfBuildFromStore(index, store);

    // Query with [1,0,0,...] - dot products: v1=1, v2=5, v3=-1
    // Distance is -dot, so: v1=-1, v2=-5, v3=1
    // Smallest distance (best match) should be v2 with distance -5
    const query = new Float32Array(dimensions).fill(0); query[0] = 1;
    const results = ivfSearch(index, store, query, 3);

    expect(results.length).toBe(3);
    // Dot product: v2 has highest dot product (5), so lowest distance (-5)
    expect(results[0].nodeId).toBe(2);
    expect(results[0].distance).toBeCloseTo(-5, 5);
    expect(results[0].similarity).toBeCloseTo(5, 5); // similarity = -distance = dot
    
    expect(results[1].nodeId).toBe(1);
    expect(results[1].distance).toBeCloseTo(-1, 5);
    
    expect(results[2].nodeId).toBe(3);
    expect(results[2].distance).toBeCloseTo(1, 5);
    expect(results[2].similarity).toBeCloseTo(-1, 5);
  });

  test("ivfSearchMulti throws on empty queries", () => {
    const store = createVectorStore(dimensions);
    const index = createIvfIndex(dimensions, { nClusters: 4 });

    // Train index
    const trainingData = new Float32Array(50 * dimensions);
    for (let i = 0; i < 50; i++) {
      trainingData.set(randomNormalizedVector(dimensions), i * dimensions);
    }
    ivfAddTrainingVectors(index, trainingData, dimensions, 50);
    ivfTrain(index, dimensions);

    expect(() => ivfSearchMulti(index, store, [], 10, "min")).toThrow(
      "ivfSearchMulti requires at least one query vector"
    );
  });

  test("ivfSearchMulti with min aggregation", () => {
    const store = createVectorStore(dimensions);
    // Use 2 clusters since we have few vectors
    const index = createIvfIndex(dimensions, { nClusters: 2, nProbe: 2 });

    // Create vectors in truly different directions
    // v1: [1,0,0,...] normalized
    // v2: [0,1,0,...] normalized (orthogonal to v1)
    // v3: [-1,0,0,...] normalized (opposite to v1)
    const v1 = new Float32Array(dimensions); v1[0] = 1;
    const v2 = new Float32Array(dimensions); v2[1] = 1;
    const v3 = new Float32Array(dimensions); v3[0] = -1;

    vectorStoreInsert(store, 1, v1);
    vectorStoreInsert(store, 2, v2);
    vectorStoreInsert(store, 3, v3);

    ivfBuildFromStore(index, store);

    // Query with v1 and v3 - min aggregation should favor results close to either
    const q1 = new Float32Array(dimensions); q1[0] = 1;  // Same as v1
    const q2 = new Float32Array(dimensions); q2[0] = -1; // Same as v3
    const queries = [q1, q2];

    const results = ivfSearchMulti(index, store, queries, 3, "min");

    expect(results.length).toBe(3);
    // v1 matches q1 perfectly (dist=0), v3 matches q2 perfectly (dist=0)
    // v2 is orthogonal to both (dist=1 for cosine)
    // With min aggregation: v1 min=0, v3 min=0, v2 min=1
    const topTwo = results.slice(0, 2).map(r => r.nodeId);
    expect(topTwo).toContain(1);
    expect(topTwo).toContain(3);
  });

  test("ivfSearchMulti with max aggregation", () => {
    const store = createVectorStore(dimensions);
    const index = createIvfIndex(dimensions, { nClusters: 2, nProbe: 2 });

    // Create vectors: v1 and v3 are opposite, v2 is orthogonal
    const v1 = new Float32Array(dimensions); v1[0] = 1;
    const v2 = new Float32Array(dimensions); v2[1] = 1;
    const v3 = new Float32Array(dimensions); v3[0] = -1;

    vectorStoreInsert(store, 1, v1);
    vectorStoreInsert(store, 2, v2);
    vectorStoreInsert(store, 3, v3);

    ivfBuildFromStore(index, store);

    // Query with v1 and v3 directions
    const q1 = new Float32Array(dimensions); q1[0] = 1;
    const q2 = new Float32Array(dimensions); q2[0] = -1;
    const queries = [q1, q2];

    const results = ivfSearchMulti(index, store, queries, 3, "max");

    expect(results.length).toBe(3);
    // With max aggregation:
    // v1: dist to q1=0, dist to q2=2 -> max=2
    // v2: dist to q1=1, dist to q2=1 -> max=1 (orthogonal to both)
    // v3: dist to q1=2, dist to q2=0 -> max=2
    // v2 should win with lowest max distance
    expect(results[0].nodeId).toBe(2);
  });

  test("ivfSearchMulti with avg aggregation", () => {
    const store = createVectorStore(dimensions);
    const index = createIvfIndex(dimensions, { nClusters: 2, nProbe: 2 });

    // Same setup: v1 and v3 opposite, v2 orthogonal
    const v1 = new Float32Array(dimensions); v1[0] = 1;
    const v2 = new Float32Array(dimensions); v2[1] = 1;
    const v3 = new Float32Array(dimensions); v3[0] = -1;

    vectorStoreInsert(store, 1, v1);
    vectorStoreInsert(store, 2, v2);
    vectorStoreInsert(store, 3, v3);

    ivfBuildFromStore(index, store);

    const q1 = new Float32Array(dimensions); q1[0] = 1;
    const q2 = new Float32Array(dimensions); q2[0] = -1;
    const queries = [q1, q2];

    const results = ivfSearchMulti(index, store, queries, 3, "avg");

    expect(results.length).toBe(3);
    // With avg aggregation:
    // v1: (0 + 2) / 2 = 1
    // v2: (1 + 1) / 2 = 1
    // v3: (2 + 0) / 2 = 1
    // All have same average - just check we get 3 results
    expect(results.map(r => r.nodeId).sort()).toEqual([1, 2, 3]);
  });

  test("ivfSearchMulti with sum aggregation", () => {
    const store = createVectorStore(dimensions);
    const index = createIvfIndex(dimensions, { nClusters: 2, nProbe: 2 });

    // Same setup
    const v1 = new Float32Array(dimensions); v1[0] = 1;
    const v2 = new Float32Array(dimensions); v2[1] = 1;
    const v3 = new Float32Array(dimensions); v3[0] = -1;

    vectorStoreInsert(store, 1, v1);
    vectorStoreInsert(store, 2, v2);
    vectorStoreInsert(store, 3, v3);

    ivfBuildFromStore(index, store);

    const q1 = new Float32Array(dimensions); q1[0] = 1;
    const q2 = new Float32Array(dimensions); q2[0] = -1;
    const queries = [q1, q2];

    const results = ivfSearchMulti(index, store, queries, 3, "sum");

    expect(results.length).toBe(3);
    // Sum is just avg * count, so same ordering as avg
    expect(results.map(r => r.nodeId).sort()).toEqual([1, 2, 3]);
  });
});

// ============================================================================
// Compaction Tests
// ============================================================================

describe("Compaction", () => {
  const dimensions = 16;

  test("findFragmentsToCompact identifies candidates", () => {
    const store = createVectorStore(dimensions, {
      fragmentTargetSize: 10,
      rowGroupSize: 5,
    });

    // Create and fill a fragment
    for (let i = 0; i < 10; i++) {
      vectorStoreInsert(store, i, randomVector(dimensions));
    }

    // Force seal the first fragment and create a new active one
    store.fragments[0].state = "sealed";
    store.fragments.push(createFragment(1, store.config));
    store.activeFragmentId = 1;

    // Delete 40% from the sealed fragment (above threshold)
    vectorStoreDelete(store, 0);
    vectorStoreDelete(store, 1);
    vectorStoreDelete(store, 2);
    vectorStoreDelete(store, 3);

    // Use custom strategy with lower minVectorsToCompact
    const candidates = findFragmentsToCompact(store, {
      minDeletionRatio: 0.3,
      maxFragmentsPerCompaction: 4,
      minVectorsToCompact: 1, // Low threshold for test
    });
    expect(candidates).toContain(0);
  });

  test("compactFragments creates new fragment", () => {
    const store = createVectorStore(dimensions, {
      fragmentTargetSize: 10,
      rowGroupSize: 5,
    });

    // Add vectors
    for (let i = 0; i < 10; i++) {
      vectorStoreInsert(store, i, randomVector(dimensions));
    }

    // Seal and delete some
    store.fragments[0].state = "sealed";
    vectorStoreDelete(store, 0);
    vectorStoreDelete(store, 5);

    const { newFragment, updatedLocations } = compactFragments(store, [0]);

    expect(newFragment.totalVectors).toBe(8);
    expect(newFragment.deletedCount).toBe(0);
    expect(updatedLocations.size).toBe(8);
  });

  test("applyCompaction updates manifest", () => {
    const store = createVectorStore(dimensions, {
      fragmentTargetSize: 10,
      rowGroupSize: 5,
    });

    // Add vectors
    for (let i = 0; i < 10; i++) {
      vectorStoreInsert(store, i, randomVector(dimensions));
    }

    store.fragments[0].state = "sealed";
    vectorStoreDelete(store, 0);
    vectorStoreDelete(store, 5);

    const { newFragment, updatedLocations } = compactFragments(store, [0]);
    applyCompaction(store, [0], newFragment, updatedLocations);

    expect(store.fragments.length).toBe(2); // Original + new
    expect(store.totalDeleted).toBe(0); // Deleted vectors removed

    // Original fragment should be cleared
    expect(store.fragments[0].totalVectors).toBe(0);
  });

  test("getCompactionStats returns correct info", () => {
    const store = createVectorStore(dimensions, {
      fragmentTargetSize: 10,
      rowGroupSize: 5,
    });

    for (let i = 0; i < 10; i++) {
      vectorStoreInsert(store, i, randomVector(dimensions));
    }

    store.fragments[0].state = "sealed";
    vectorStoreDelete(store, 0);
    vectorStoreDelete(store, 1);
    vectorStoreDelete(store, 2);
    vectorStoreDelete(store, 3);

    const stats = getCompactionStats(store);

    expect(stats.fragmentsNeedingCompaction).toBe(1);
    expect(stats.totalDeletedVectors).toBe(4);
    expect(stats.averageDeletionRatio).toBeCloseTo(0.4, 5);
  });

  test("clearDeletedFragments removes fully-deleted fragments", () => {
    const store = createVectorStore(dimensions, {
      fragmentTargetSize: 10,
      rowGroupSize: 5,
    });

    // Add vectors
    for (let i = 0; i < 10; i++) {
      vectorStoreInsert(store, i, randomVector(dimensions));
    }

    // Seal the fragment
    store.fragments[0].state = "sealed";
    
    // Create a new active fragment
    store.fragments.push(createFragment(1, store.config));
    store.activeFragmentId = 1;

    // Delete ALL vectors from the first fragment
    for (let i = 0; i < 10; i++) {
      vectorStoreDelete(store, i);
    }

    // First fragment should have 100% deletion
    expect(store.fragments[0].deletedCount).toBe(10);
    expect(store.fragments[0].totalVectors).toBe(10);

    // Clear deleted fragments
    const cleared = clearDeletedFragments(store);

    expect(cleared).toBe(1);
    expect(store.fragments[0].totalVectors).toBe(0);
    expect(store.fragments[0].deletedCount).toBe(0);
    expect(store.fragments[0].rowGroups.length).toBe(0);
    expect(store.totalDeleted).toBe(0);
  });
});

// ============================================================================
// Serialization Tests
// ============================================================================

describe("Serialization", () => {
  const dimensions = 16;

  test("IVF index serialization roundtrip", () => {
    const index = createIvfIndex(dimensions, { nClusters: 4 });

    // Train
    const trainingData = new Float32Array(50 * dimensions);
    for (let i = 0; i < 50; i++) {
      trainingData.set(randomNormalizedVector(dimensions), i * dimensions);
    }
    ivfAddTrainingVectors(index, trainingData, dimensions, 50);
    ivfTrain(index, dimensions);

    // Add vectors
    for (let i = 0; i < 10; i++) {
      ivfInsert(index, i, randomNormalizedVector(dimensions), dimensions);
    }

    // Serialize
    const serialized = serializeIvf(index, dimensions);
    expect(serialized.length).toBeGreaterThan(0);

    // Deserialize
    const { index: restored, dimensions: restoredDims } =
      deserializeIvf(serialized);

    expect(restoredDims).toBe(dimensions);
    expect(restored.trained).toBe(index.trained);
    expect(restored.config.nClusters).toBe(index.config.nClusters);

    // Check centroids
    for (let i = 0; i < index.centroids.length; i++) {
      expect(restored.centroids[i]).toBeCloseTo(index.centroids[i], 5);
    }

    // Check inverted lists
    expect(restored.invertedLists.size).toBe(index.invertedLists.size);
  });

  test("Manifest serialization roundtrip", () => {
    const store = createVectorStore(dimensions);

    // Add vectors
    for (let i = 0; i < 20; i++) {
      vectorStoreInsert(store, i * 10, randomVector(dimensions));
    }

    // Delete some
    vectorStoreDelete(store, 50);
    vectorStoreDelete(store, 100);

    // Serialize
    const serialized = serializeManifest(store);
    expect(serialized.length).toBeGreaterThan(0);

    // Deserialize
    const restored = deserializeManifest(serialized);

    expect(restored.config.dimensions).toBe(store.config.dimensions);
    expect(restored.totalVectors).toBe(store.totalVectors);
    expect(restored.totalDeleted).toBe(store.totalDeleted);
    expect(restored.nextVectorId).toBe(store.nextVectorId);

    // Check node ID mappings
    expect(restored.nodeIdToVectorId.size).toBe(store.nodeIdToVectorId.size);

    for (const [nodeId, vectorId] of store.nodeIdToVectorId) {
      expect(restored.nodeIdToVectorId.get(nodeId)).toBe(vectorId);
    }

    // Check vector data
    for (const [vectorId, nodeId, vector] of vectorStoreIterator(store)) {
      const restoredVec = vectorStoreGet(restored, nodeId);
      expect(restoredVec).not.toBeNull();

      for (let i = 0; i < dimensions; i++) {
        expect(restoredVec![i]).toBeCloseTo(vector[i], 5);
      }
    }
  });

  test("Empty store serialization", () => {
    const store = createVectorStore(dimensions);

    const serialized = serializeManifest(store);
    const restored = deserializeManifest(serialized);

    expect(restored.totalVectors).toBe(0);
    expect(restored.nodeIdToVectorId.size).toBe(0);
  });

  test("Serialization with large node IDs (within safe integer range)", () => {
    const store = createVectorStore(dimensions);

    // Use large node IDs that are still within Number.MAX_SAFE_INTEGER
    const largeIds = [
      1_000_000_000,           // 1 billion
      1_000_000_000_000,       // 1 trillion
      Number.MAX_SAFE_INTEGER - 1, // Just under the limit
    ];

    for (let i = 0; i < largeIds.length; i++) {
      vectorStoreInsert(store, largeIds[i], randomVector(dimensions));
    }

    // Serialize and deserialize
    const serialized = serializeManifest(store);
    const restored = deserializeManifest(serialized);

    // Verify large IDs are preserved correctly
    for (const largeId of largeIds) {
      expect(restored.nodeIdToVectorId.has(largeId)).toBe(true);
      const restoredVec = vectorStoreGet(restored, largeId);
      expect(restoredVec).not.toBeNull();
    }
  });

  test("Serialization with node ID at MAX_SAFE_INTEGER", () => {
    const store = createVectorStore(dimensions);

    // Use exactly MAX_SAFE_INTEGER
    const maxSafeId = Number.MAX_SAFE_INTEGER;
    vectorStoreInsert(store, maxSafeId, randomVector(dimensions));

    // Serialize and deserialize
    const serialized = serializeManifest(store);
    const restored = deserializeManifest(serialized);

    // Verify the max safe ID is preserved
    expect(restored.nodeIdToVectorId.has(maxSafeId)).toBe(true);
    const restoredVec = vectorStoreGet(restored, maxSafeId);
    expect(restoredVec).not.toBeNull();
  });

  test("Deserialize rejects empty buffer", () => {
    expect(() => deserializeManifest(new Uint8Array(0))).toThrow();
  });

  test("Deserialize rejects truncated buffer", () => {
    const store = createVectorStore(dimensions);
    vectorStoreInsert(store, 1, randomVector(dimensions));
    
    const serialized = serializeManifest(store);
    
    // Truncate to half the size
    const truncated = serialized.slice(0, Math.floor(serialized.length / 2));
    
    expect(() => deserializeManifest(truncated)).toThrow();
  });

  test("Deserialize rejects invalid magic number", () => {
    const store = createVectorStore(dimensions);
    vectorStoreInsert(store, 1, randomVector(dimensions));
    
    const serialized = serializeManifest(store);
    
    // Corrupt the magic number (first 4 bytes)
    serialized[0] = 0xFF;
    serialized[1] = 0xFF;
    serialized[2] = 0xFF;
    serialized[3] = 0xFF;
    
    expect(() => deserializeManifest(serialized)).toThrow(/Invalid manifest magic/);
  });

  test("IVF deserialize rejects empty buffer", () => {
    expect(() => deserializeIvf(new Uint8Array(0))).toThrow();
  });

  test("IVF deserialize rejects invalid magic number", () => {
    const index = createIvfIndex(dimensions, { nClusters: 4 });
    
    // Train the index
    const trainingData = new Float32Array(50 * dimensions);
    for (let i = 0; i < 50; i++) {
      trainingData.set(randomNormalizedVector(dimensions), i * dimensions);
    }
    ivfAddTrainingVectors(index, trainingData, dimensions, 50);
    ivfTrain(index, dimensions);
    
    const serialized = serializeIvf(index, dimensions);
    
    // Corrupt the magic number
    serialized[0] = 0xFF;
    serialized[1] = 0xFF;
    serialized[2] = 0xFF;
    serialized[3] = 0xFF;
    
    expect(() => deserializeIvf(serialized)).toThrow(/Invalid IVF magic/);
  });
});

// ============================================================================
// Integration Tests
// ============================================================================

describe("Integration", () => {
  const dimensions = 64;

  test("Full workflow: insert, build index, search, delete, compact", () => {
    // Create store
    const store = createVectorStore(dimensions, {
      fragmentTargetSize: 50,
      rowGroupSize: 10,
    });

    // Insert vectors
    const vectors: Map<number, Float32Array> = new Map();
    for (let i = 0; i < 100; i++) {
      const v = randomNormalizedVector(dimensions);
      vectors.set(i, v);
      vectorStoreInsert(store, i, v);
    }

    expect(store.totalVectors).toBe(100);
    expect(store.fragments.length).toBeGreaterThan(1); // Should have sealed fragments

    // Build index
    const index = createIvfIndex(dimensions, { nClusters: 8, nProbe: 4 });
    ivfBuildFromStore(index, store);

    expect(index.trained).toBe(true);

    // Search
    const queryNodeId = 42;
    const queryVector = vectors.get(queryNodeId)!;
    const results = ivfSearch(index, store, queryVector, 5);

    expect(results.length).toBe(5);
    expect(results[0].nodeId).toBe(queryNodeId); // Should find itself first

    // Delete some vectors (every 3rd starting from 0)
    const deletedNodeIds = new Set<number>();
    for (let i = 0; i < 30; i++) {
      const nodeId = i * 3;
      if (vectorStoreDelete(store, nodeId)) {
        deletedNodeIds.add(nodeId);
      }
    }

    expect(store.totalDeleted).toBe(deletedNodeIds.size);

    // Check compaction stats - totalDeleted in store should match
    const compactionStats = getCompactionStats(store);
    // Note: compactionStats only counts deleted in sealed fragments
    expect(compactionStats.totalDeletedVectors).toBeGreaterThan(0);

    // Search again - deleted vectors should not appear
    const resultsAfterDelete = ivfSearch(index, store, queryVector, 10);

    for (const result of resultsAfterDelete) {
      expect(deletedNodeIds.has(result.nodeId)).toBe(false);
    }
  });

  test("Large scale test", () => {
    const largeDimensions = 128;
    const numVectors = 1000;

    const store = createVectorStore(largeDimensions, {
      fragmentTargetSize: 500,
      rowGroupSize: 100,
    });

    // Batch insert
    const entries = Array.from({ length: numVectors }, (_, i) => ({
      nodeId: i,
      vector: randomNormalizedVector(largeDimensions),
    }));

    vectorStoreBatchInsert(store, entries);

    expect(store.totalVectors).toBe(numVectors);

    // Build index
    const index = createIvfIndex(largeDimensions, {
      nClusters: 32,
      nProbe: 8,
    });
    ivfBuildFromStore(index, store);

    // Search should complete in reasonable time
    const query = randomNormalizedVector(largeDimensions);
    const startTime = performance.now();

    const results = ivfSearch(index, store, query, 10);

    const elapsed = performance.now() - startTime;
    expect(elapsed).toBeLessThan(1000); // Should be fast

    expect(results.length).toBe(10);

    // All results should have valid similarity scores
    for (const result of results) {
      expect(result.similarity).toBeGreaterThan(0);
      expect(result.similarity).toBeLessThanOrEqual(1);
    }
  });
});
