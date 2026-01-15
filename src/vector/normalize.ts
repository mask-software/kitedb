/**
 * Vector normalization utilities
 *
 * Normalizing vectors enables fast cosine similarity via dot product.
 * For normalized vectors: cos(a,b) = dot(a,b)
 */

/**
 * Compute L2 norm (Euclidean length) of a vector
 */
export function l2Norm(v: Float32Array): number {
  let sum = 0;
  const len = v.length;

  // Unroll loop for better performance
  const remainder = len % 4;
  const mainLen = len - remainder;

  for (let i = 0; i < mainLen; i += 4) {
    sum +=
      v[i] * v[i] +
      v[i + 1] * v[i + 1] +
      v[i + 2] * v[i + 2] +
      v[i + 3] * v[i + 3];
  }

  for (let i = mainLen; i < len; i++) {
    sum += v[i] * v[i];
  }

  return Math.sqrt(sum);
}

/**
 * Normalize a vector in-place
 * @returns The norm before normalization
 */
export function normalizeInPlace(v: Float32Array): number {
  const norm = l2Norm(v);
  if (norm > 0) {
    const invNorm = 1 / norm;
    const len = v.length;

    // Unroll for better performance
    const remainder = len % 4;
    const mainLen = len - remainder;

    for (let i = 0; i < mainLen; i += 4) {
      v[i] *= invNorm;
      v[i + 1] *= invNorm;
      v[i + 2] *= invNorm;
      v[i + 3] *= invNorm;
    }

    for (let i = mainLen; i < len; i++) {
      v[i] *= invNorm;
    }
  }
  return norm;
}

/**
 * Normalize a vector, returning a new array
 */
export function normalize(v: Float32Array): Float32Array {
  const result = new Float32Array(v);
  normalizeInPlace(result);
  return result;
}

/**
 * Check if a vector is normalized (L2 norm ≈ 1)
 */
export function isNormalized(v: Float32Array, tolerance = 1e-5): boolean {
  const norm = l2Norm(v);
  return Math.abs(norm - 1) < tolerance;
}

/**
 * Normalize multiple vectors in a contiguous array (row group)
 * More efficient than normalizing individually due to cache locality
 *
 * @param data - Contiguous vector data
 * @param dimensions - Number of dimensions per vector
 * @param count - Number of vectors to normalize
 */
export function normalizeRowGroup(
  data: Float32Array,
  dimensions: number,
  count: number
): void {
  for (let i = 0; i < count; i++) {
    const offset = i * dimensions;

    // Compute norm
    let sum = 0;
    for (let d = 0; d < dimensions; d++) {
      const val = data[offset + d];
      sum += val * val;
    }

    // Normalize if non-zero
    if (sum > 0) {
      const invNorm = 1 / Math.sqrt(sum);
      for (let d = 0; d < dimensions; d++) {
        data[offset + d] *= invNorm;
      }
    }
  }
}

/**
 * Normalize a single vector within a row group (by index)
 *
 * @param data - Contiguous vector data
 * @param dimensions - Number of dimensions per vector
 * @param index - Index of the vector to normalize
 * @returns The norm before normalization
 */
export function normalizeVectorAt(
  data: Float32Array,
  dimensions: number,
  index: number
): number {
  const offset = index * dimensions;

  // Compute norm
  let sum = 0;
  for (let d = 0; d < dimensions; d++) {
    const val = data[offset + d];
    sum += val * val;
  }

  const norm = Math.sqrt(sum);

  // Normalize if non-zero
  if (norm > 0) {
    const invNorm = 1 / norm;
    for (let d = 0; d < dimensions; d++) {
      data[offset + d] *= invNorm;
    }
  }

  return norm;
}

/**
 * Check if a vector at a specific index in a row group is normalized
 */
export function isNormalizedAt(
  data: Float32Array,
  dimensions: number,
  index: number,
  tolerance = 1e-5
): boolean {
  const offset = index * dimensions;

  let sum = 0;
  for (let d = 0; d < dimensions; d++) {
    const val = data[offset + d];
    sum += val * val;
  }

  return Math.abs(Math.sqrt(sum) - 1) < tolerance;
}
