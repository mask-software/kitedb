/**
 * CRC32C wrapper using Bun's native hash implementation
 */

/**
 * Compute CRC32C hash of data
 * Uses Bun.hash.crc32 which implements CRC32C (Castagnoli)
 */
export function crc32c(data: Uint8Array): number {
  // Bun.hash.crc32 returns a number (u32)
  return Bun.hash.crc32(data);
}

/**
 * Compute CRC32C hash of multiple data segments
 */
export function crc32cMulti(...segments: Uint8Array[]): number {
  // Concatenate segments and compute hash
  // For small segments this is fine; for large data consider streaming
  const totalLength = segments.reduce((sum, seg) => sum + seg.length, 0);
  const combined = new Uint8Array(totalLength);
  let offset = 0;
  for (const seg of segments) {
    combined.set(seg, offset);
    offset += seg.length;
  }
  return Bun.hash.crc32(combined);
}

/**
 * Verify CRC32C matches expected value
 */
export function verifyCrc32c(data: Uint8Array, expected: number): boolean {
  return crc32c(data) === expected;
}
