/**
 * Vector normalization utilities
 *
 * Normalizing vectors enables fast cosine similarity via dot product.
 * For normalized vectors: cos(a,b) = dot(a,b)
 */

// ============================================================================
// Validation
// ============================================================================

/**
 * Result of vector validation
 */
export interface VectorValidationResult {
  valid: boolean;
  error?: "zero_vector" | "contains_nan" | "contains_infinity";
  message?: string;
}

/**
 * Validate a vector for invalid values
 * 
 * Checks for:
 * - NaN values
 * - Infinity values  
 * - Zero vectors (all zeros)
 * 
 * @param v - The vector to validate
 * @returns Validation result with error details if invalid
 */
export function validateVector(v: Float32Array): VectorValidationResult {
  let hasNonZero = false;
  
  for (let i = 0; i < v.length; i++) {
    const val = v[i];
    
    if (Number.isNaN(val)) {
      return {
        valid: false,
        error: "contains_nan",
        message: `Vector contains NaN at index ${i}`,
      };
    }
    
    if (!Number.isFinite(val)) {
      return {
        valid: false,
        error: "contains_infinity",
        message: `Vector contains Infinity at index ${i}`,
      };
    }
    
    if (val !== 0) {
      hasNonZero = true;
    }
  }
  
  if (!hasNonZero) {
    return {
      valid: false,
      error: "zero_vector",
      message: "Vector is all zeros (zero vector)",
    };
  }
  
  return { valid: true };
}

/**
 * Check if a vector contains any NaN values
 */
export function hasNaN(v: Float32Array): boolean {
  for (let i = 0; i < v.length; i++) {
    if (Number.isNaN(v[i])) return true;
  }
  return false;
}

/**
 * Check if a vector contains any Infinity values
 */
export function hasInfinity(v: Float32Array): boolean {
  for (let i = 0; i < v.length; i++) {
    if (!Number.isFinite(v[i])) return true;
  }
  return false;
}

/**
 * Check if a vector is all zeros
 */
export function isZeroVector(v: Float32Array): boolean {
  for (let i = 0; i < v.length; i++) {
    if (v[i] !== 0) return false;
  }
  return true;
}

// ============================================================================
// Normalization
// ============================================================================

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
