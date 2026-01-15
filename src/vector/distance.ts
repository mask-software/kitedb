/**
 * Distance and similarity functions for vector search
 *
 * Optimized for columnar storage with SIMD-friendly patterns.
 * For normalized vectors, cosine similarity = dot product.
 */

/**
 * Dot product of two vectors
 * For normalized vectors: dot(a,b) = cos(θ) = cosine similarity
 */
export function dotProduct(a: Float32Array, b: Float32Array): number {
  let sum = 0;
  const len = a.length;

  // Unroll loop for better performance (helps JIT generate SIMD)
  const remainder = len % 8;
  const mainLen = len - remainder;

  for (let i = 0; i < mainLen; i += 8) {
    sum +=
      a[i] * b[i] +
      a[i + 1] * b[i + 1] +
      a[i + 2] * b[i + 2] +
      a[i + 3] * b[i + 3] +
      a[i + 4] * b[i + 4] +
      a[i + 5] * b[i + 5] +
      a[i + 6] * b[i + 6] +
      a[i + 7] * b[i + 7];
  }

  for (let i = mainLen; i < len; i++) {
    sum += a[i] * b[i];
  }

  return sum;
}

/**
 * Cosine distance (1 - cosine similarity)
 * For normalized vectors: 1 - dot(a,b)
 */
export function cosineDistance(a: Float32Array, b: Float32Array): number {
  return 1 - dotProduct(a, b);
}

/**
 * Cosine similarity
 * For normalized vectors: dot(a,b)
 */
export function cosineSimilarity(a: Float32Array, b: Float32Array): number {
  return dotProduct(a, b);
}

/**
 * Squared Euclidean distance (faster than euclidean, preserves ordering)
 */
export function squaredEuclidean(a: Float32Array, b: Float32Array): number {
  let sum = 0;
  const len = a.length;

  // Unroll for better performance
  const remainder = len % 4;
  const mainLen = len - remainder;

  for (let i = 0; i < mainLen; i += 4) {
    const d0 = a[i] - b[i];
    const d1 = a[i + 1] - b[i + 1];
    const d2 = a[i + 2] - b[i + 2];
    const d3 = a[i + 3] - b[i + 3];
    sum += d0 * d0 + d1 * d1 + d2 * d2 + d3 * d3;
  }

  for (let i = mainLen; i < len; i++) {
    const diff = a[i] - b[i];
    sum += diff * diff;
  }

  return sum;
}

/**
 * Euclidean distance
 */
export function euclideanDistance(a: Float32Array, b: Float32Array): number {
  return Math.sqrt(squaredEuclidean(a, b));
}

/**
 * Compute dot product between query and a vector at a specific index in row group data
 */
export function dotProductAt(
  query: Float32Array,
  rowGroupData: Float32Array,
  dimensions: number,
  index: number
): number {
  const offset = index * dimensions;
  let sum = 0;

  // Unrolled for SIMD
  const remainder = dimensions % 8;
  const mainLen = dimensions - remainder;

  for (let d = 0; d < mainLen; d += 8) {
    sum +=
      query[d] * rowGroupData[offset + d] +
      query[d + 1] * rowGroupData[offset + d + 1] +
      query[d + 2] * rowGroupData[offset + d + 2] +
      query[d + 3] * rowGroupData[offset + d + 3] +
      query[d + 4] * rowGroupData[offset + d + 4] +
      query[d + 5] * rowGroupData[offset + d + 5] +
      query[d + 6] * rowGroupData[offset + d + 6] +
      query[d + 7] * rowGroupData[offset + d + 7];
  }

  for (let d = mainLen; d < dimensions; d++) {
    sum += query[d] * rowGroupData[offset + d];
  }

  return sum;
}

/**
 * Compute squared Euclidean distance between query and a vector at a specific index
 */
export function squaredEuclideanAt(
  query: Float32Array,
  rowGroupData: Float32Array,
  dimensions: number,
  index: number
): number {
  const offset = index * dimensions;
  let sum = 0;

  for (let d = 0; d < dimensions; d++) {
    const diff = query[d] - rowGroupData[offset + d];
    sum += diff * diff;
  }

  return sum;
}

/**
 * Compute distances from query to multiple vectors in a row group
 *
 * This is the hot path - optimized for columnar access pattern.
 * Returns array of distances for vectors [startIdx, startIdx + count)
 */
export function batchCosineDistance(
  query: Float32Array,
  rowGroupData: Float32Array,
  dimensions: number,
  startIdx: number,
  count: number
): Float32Array {
  const distances = new Float32Array(count);

  for (let i = 0; i < count; i++) {
    const offset = (startIdx + i) * dimensions;
    let sum = 0;

    // Unrolled for SIMD
    const remainder = dimensions % 8;
    const mainLen = dimensions - remainder;

    for (let d = 0; d < mainLen; d += 8) {
      sum +=
        query[d] * rowGroupData[offset + d] +
        query[d + 1] * rowGroupData[offset + d + 1] +
        query[d + 2] * rowGroupData[offset + d + 2] +
        query[d + 3] * rowGroupData[offset + d + 3] +
        query[d + 4] * rowGroupData[offset + d + 4] +
        query[d + 5] * rowGroupData[offset + d + 5] +
        query[d + 6] * rowGroupData[offset + d + 6] +
        query[d + 7] * rowGroupData[offset + d + 7];
    }

    for (let d = mainLen; d < dimensions; d++) {
      sum += query[d] * rowGroupData[offset + d];
    }

    // For cosine distance: 1 - dot_product (since vectors are normalized)
    distances[i] = 1 - sum;
  }

  return distances;
}

/**
 * Batch squared Euclidean distance
 */
export function batchSquaredEuclidean(
  query: Float32Array,
  rowGroupData: Float32Array,
  dimensions: number,
  startIdx: number,
  count: number
): Float32Array {
  const distances = new Float32Array(count);

  for (let i = 0; i < count; i++) {
    const offset = (startIdx + i) * dimensions;
    let sum = 0;

    for (let d = 0; d < dimensions; d++) {
      const diff = query[d] - rowGroupData[offset + d];
      sum += diff * diff;
    }

    distances[i] = sum;
  }

  return distances;
}

/**
 * Get distance function by metric name
 */
export function getDistanceFunction(
  metric: "cosine" | "euclidean" | "dot"
): (a: Float32Array, b: Float32Array) => number {
  switch (metric) {
    case "cosine":
      return cosineDistance;
    case "euclidean":
      return squaredEuclidean; // Use squared for efficiency
    case "dot":
      // For dot product, we want higher = better, so negate for distance
      return (a, b) => -dotProduct(a, b);
  }
}

/**
 * Get batch distance function by metric name
 */
export function getBatchDistanceFunction(
  metric: "cosine" | "euclidean" | "dot"
): (
  query: Float32Array,
  rowGroupData: Float32Array,
  dimensions: number,
  startIdx: number,
  count: number
) => Float32Array {
  switch (metric) {
    case "cosine":
    case "dot":
      return batchCosineDistance;
    case "euclidean":
      return batchSquaredEuclidean;
  }
}

/**
 * Convert distance to similarity (0-1 scale)
 */
export function distanceToSimilarity(
  distance: number,
  metric: "cosine" | "euclidean" | "dot"
): number {
  switch (metric) {
    case "cosine":
      return 1 - distance;
    case "euclidean":
      return 1 / (1 + Math.sqrt(distance));
    case "dot":
      return 1 - distance; // distance was negated dot product
  }
}

/**
 * Find k nearest neighbors from distances array
 * Returns indices sorted by distance (ascending)
 */
export function findKNearest(
  distances: Float32Array,
  k: number,
  startIdx: number = 0
): Array<{ index: number; distance: number }> {
  // Create array of (index, distance) pairs
  const pairs: Array<{ index: number; distance: number }> = [];
  for (let i = 0; i < distances.length; i++) {
    pairs.push({ index: startIdx + i, distance: distances[i] });
  }

  // Partial sort to get k smallest (using simple selection for small k)
  if (k >= pairs.length) {
    pairs.sort((a, b) => a.distance - b.distance);
    return pairs;
  }

  // For small k, use partial selection sort
  for (let i = 0; i < k; i++) {
    let minIdx = i;
    for (let j = i + 1; j < pairs.length; j++) {
      if (pairs[j].distance < pairs[minIdx].distance) {
        minIdx = j;
      }
    }
    if (minIdx !== i) {
      [pairs[i], pairs[minIdx]] = [pairs[minIdx], pairs[i]];
    }
  }

  return pairs.slice(0, k);
}

/**
 * Min-heap for efficient top-k tracking during search
 */
export class MinHeap {
  private heap: Array<{ id: number; distance: number }> = [];

  get size(): number {
    return this.heap.length;
  }

  peek(): { id: number; distance: number } | undefined {
    return this.heap[0];
  }

  push(id: number, distance: number): void {
    this.heap.push({ id, distance });
    this.bubbleUp(this.heap.length - 1);
  }

  pop(): { id: number; distance: number } | undefined {
    if (this.heap.length === 0) return undefined;
    const result = this.heap[0];
    const last = this.heap.pop()!;
    if (this.heap.length > 0) {
      this.heap[0] = last;
      this.bubbleDown(0);
    }
    return result;
  }

  private bubbleUp(i: number): void {
    while (i > 0) {
      const parent = (i - 1) >> 1;
      if (this.heap[parent].distance <= this.heap[i].distance) break;
      [this.heap[parent], this.heap[i]] = [this.heap[i], this.heap[parent]];
      i = parent;
    }
  }

  private bubbleDown(i: number): void {
    const len = this.heap.length;
    while (true) {
      const left = 2 * i + 1;
      const right = 2 * i + 2;
      let smallest = i;

      if (
        left < len &&
        this.heap[left].distance < this.heap[smallest].distance
      ) {
        smallest = left;
      }
      if (
        right < len &&
        this.heap[right].distance < this.heap[smallest].distance
      ) {
        smallest = right;
      }

      if (smallest === i) break;
      [this.heap[smallest], this.heap[i]] = [this.heap[i], this.heap[smallest]];
      i = smallest;
    }
  }

  toSortedArray(): Array<{ id: number; distance: number }> {
    return [...this.heap].sort((a, b) => a.distance - b.distance);
  }
}

/**
 * Max-heap for tracking worst candidate in k-nearest search
 */
export class MaxHeap {
  private heap: Array<{ id: number; distance: number }> = [];

  get size(): number {
    return this.heap.length;
  }

  peek(): { id: number; distance: number } | undefined {
    return this.heap[0];
  }

  push(id: number, distance: number): void {
    this.heap.push({ id, distance });
    this.bubbleUp(this.heap.length - 1);
  }

  pop(): { id: number; distance: number } | undefined {
    if (this.heap.length === 0) return undefined;
    const result = this.heap[0];
    const last = this.heap.pop()!;
    if (this.heap.length > 0) {
      this.heap[0] = last;
      this.bubbleDown(0);
    }
    return result;
  }

  private bubbleUp(i: number): void {
    while (i > 0) {
      const parent = (i - 1) >> 1;
      if (this.heap[parent].distance >= this.heap[i].distance) break;
      [this.heap[parent], this.heap[i]] = [this.heap[i], this.heap[parent]];
      i = parent;
    }
  }

  private bubbleDown(i: number): void {
    const len = this.heap.length;
    while (true) {
      const left = 2 * i + 1;
      const right = 2 * i + 2;
      let largest = i;

      if (
        left < len &&
        this.heap[left].distance > this.heap[largest].distance
      ) {
        largest = left;
      }
      if (
        right < len &&
        this.heap[right].distance > this.heap[largest].distance
      ) {
        largest = right;
      }

      if (largest === i) break;
      [this.heap[largest], this.heap[i]] = [this.heap[i], this.heap[largest]];
      i = largest;
    }
  }

  toSortedArray(): Array<{ id: number; distance: number }> {
    return [...this.heap].sort((a, b) => a.distance - b.distance);
  }
}
