/**
 * Compression utilities for snapshot sections
 *
 * Supports multiple compression algorithms with automatic detection.
 * Uses Bun's built-in compression functions for zero-dependency operation.
 */

// ============================================================================
// Compression Types
// ============================================================================

/**
 * Compression algorithm identifier
 * Stored in section entry's compression field (u32)
 */
export enum CompressionType {
  /** No compression */
  NONE = 0,
  /** Zstandard compression (default) */
  ZSTD = 1,
  /** Gzip compression */
  GZIP = 2,
  /** Raw deflate compression */
  DEFLATE = 3,
}

/**
 * Compression options for snapshot building
 */
export interface CompressionOptions {
  /** Enable compression (default: false for backwards compatibility) */
  enabled: boolean;
  /** Compression algorithm to use (default: ZSTD) */
  type?: CompressionType;
  /** Minimum section size to compress (default: 64 bytes) */
  minSize?: number;
  /** Zstd compression level 1-22 (default: 3) */
  level?: number;
}

/**
 * Default compression options
 */
export const DEFAULT_COMPRESSION_OPTIONS: CompressionOptions = {
  enabled: false,
  type: CompressionType.ZSTD,
  minSize: 64,
  level: 3,
};

// ============================================================================
// Compression Functions
// ============================================================================

/**
 * Compress data using the specified algorithm
 *
 * @param data - Raw data to compress
 * @param type - Compression algorithm to use
 * @param level - Compression level (for zstd: 1-22)
 * @returns Compressed data
 */
export function compress(
  data: Uint8Array,
  type: CompressionType = CompressionType.ZSTD,
  level = 3,
): Uint8Array {
  switch (type) {
    case CompressionType.NONE:
      return data;

    case CompressionType.ZSTD:
      return new Uint8Array(
        Bun.zstdCompressSync(data, { level }) as unknown as ArrayBuffer,
      );

    case CompressionType.GZIP:
      return new Uint8Array(
        Bun.gzipSync(data as unknown as ArrayBuffer, {
          level: Math.min(level, 9) as 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9,
        }) as unknown as ArrayBuffer,
      );

    case CompressionType.DEFLATE:
      return new Uint8Array(
        Bun.deflateSync(data as unknown as ArrayBuffer, {
          level: Math.min(level, 9) as 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9,
        }) as unknown as ArrayBuffer,
      );

    default:
      throw new Error(`Unknown compression type: ${type}`);
  }
}

/**
 * Decompress data using the specified algorithm
 *
 * @param data - Compressed data
 * @param type - Compression algorithm used
 * @returns Decompressed data
 */
export function decompress(
  data: Uint8Array,
  type: CompressionType,
): Uint8Array {
  switch (type) {
    case CompressionType.NONE:
      return data;

    case CompressionType.ZSTD:
      return new Uint8Array(
        Bun.zstdDecompressSync(data) as unknown as ArrayBuffer,
      );

    case CompressionType.GZIP:
      return new Uint8Array(
        Bun.gunzipSync(
          data as unknown as ArrayBuffer,
        ) as unknown as ArrayBuffer,
      );

    case CompressionType.DEFLATE:
      return new Uint8Array(
        Bun.inflateSync(
          data as unknown as ArrayBuffer,
        ) as unknown as ArrayBuffer,
      );

    default:
      throw new Error(`Unknown compression type: ${type}`);
  }
}

/**
 * Check if a compression type is valid
 */
export function isValidCompressionType(type: number): type is CompressionType {
  return (
    type === CompressionType.NONE ||
    type === CompressionType.ZSTD ||
    type === CompressionType.GZIP ||
    type === CompressionType.DEFLATE
  );
}

/**
 * Get the name of a compression type for display/debugging
 */
export function compressionTypeName(type: CompressionType): string {
  switch (type) {
    case CompressionType.NONE:
      return "none";
    case CompressionType.ZSTD:
      return "zstd";
    case CompressionType.GZIP:
      return "gzip";
    case CompressionType.DEFLATE:
      return "deflate";
    default:
      return `unknown(${type})`;
  }
}

/**
 * Determine if compression is beneficial for the given data
 *
 * Only compresses if:
 * 1. Data size >= minSize
 * 2. Compressed size < original size
 *
 * @param data - Data to potentially compress
 * @param options - Compression options
 * @returns Object with compressed data and compression type used (NONE if not compressed)
 */
export function maybeCompress(
  data: Uint8Array,
  options: CompressionOptions,
): { data: Uint8Array; type: CompressionType } {
  if (!options.enabled) {
    return { data, type: CompressionType.NONE };
  }

  const minSize = options.minSize ?? 64;
  if (data.length < minSize) {
    return { data, type: CompressionType.NONE };
  }

  const type = options.type ?? CompressionType.ZSTD;
  const level = options.level ?? 3;

  try {
    const compressed = compress(data, type, level);

    // Only use compression if it actually reduces size
    if (compressed.length < data.length) {
      return { data: compressed, type };
    }
  } catch {
    // If compression fails, fall back to no compression
  }

  return { data, type: CompressionType.NONE };
}
