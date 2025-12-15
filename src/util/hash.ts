/**
 * Hash utilities for key index
 * Uses Bun's native xxHash64 for fast hashing
 */

/**
 * Compute xxHash64 of data, returns as bigint
 */
export function xxhash64(data: Uint8Array): bigint {
  // Bun.hash returns a number or bigint depending on the algorithm
  // For xxhash64, we get a bigint
  return BigInt(Bun.hash(data));
}

/**
 * Compute xxHash64 of a string (UTF-8 encoded)
 */
export function xxhash64String(str: string): bigint {
  const encoder = new TextEncoder();
  return xxhash64(encoder.encode(str));
}
