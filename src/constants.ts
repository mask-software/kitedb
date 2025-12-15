/**
 * Magic numbers and constants for the embedded graph DB
 */

// Magic bytes as little-endian u32
export const MAGIC_MANIFEST = 0x4d424447; // "GDBM"
export const MAGIC_SNAPSHOT = 0x31534447; // "GDS1"
export const MAGIC_WAL = 0x31574447; // "GDW1"

// Current versions
export const VERSION_MANIFEST = 1;
export const VERSION_SNAPSHOT = 1;
export const VERSION_WAL = 1;

// Minimum reader versions
export const MIN_READER_MANIFEST = 1;
export const MIN_READER_SNAPSHOT = 1;
export const MIN_READER_WAL = 1;

// Alignment requirements
export const SECTION_ALIGNMENT = 64; // 64-byte alignment for mmap friendliness
export const WAL_RECORD_ALIGNMENT = 8; // 8-byte alignment for WAL records

// File extensions
export const EXT_MANIFEST = ".gdm";
export const EXT_SNAPSHOT = ".gds";
export const EXT_WAL = ".gdw";
export const EXT_LOCK = ".gdl";

// File name patterns
export const MANIFEST_FILENAME = "manifest.gdm";
export const LOCK_FILENAME = "lock.gdl";
export const SNAPSHOTS_DIR = "snapshots";
export const WAL_DIR = "wal";
export const TRASH_DIR = "trash";

// Thresholds for compact recommendation
export const COMPACT_EDGE_RATIO = 0.1; // 10% of snapshot edges
export const COMPACT_NODE_RATIO = 0.1; // 10% of snapshot nodes
export const COMPACT_WAL_SIZE = 64 * 1024 * 1024; // 64MB

// Delta set upgrade threshold
export const DELTA_SET_UPGRADE_THRESHOLD = 64;

// Default minimum section size for compression (bytes)
export const COMPRESSION_MIN_SIZE = 64;

// Initial IDs (start from 1, 0 is reserved/null)
export const INITIAL_NODE_ID = 1n;
export const INITIAL_LABEL_ID = 1;
export const INITIAL_ETYPE_ID = 1;
export const INITIAL_PROPKEY_ID = 1;
export const INITIAL_TX_ID = 1n;

// Snapshot generation starts at 1 (0 means no snapshot)
export const INITIAL_SNAPSHOT_GEN = 0n;
export const INITIAL_WAL_SEG = 1n;

/**
 * Format snapshot filename from generation
 */
export function snapshotFilename(gen: bigint): string {
  return `snap_${gen.toString().padStart(16, "0")}${EXT_SNAPSHOT}`;
}

/**
 * Format WAL filename from segment ID
 */
export function walFilename(seg: bigint): string {
  return `wal_${seg.toString().padStart(16, "0")}${EXT_WAL}`;
}

/**
 * Parse generation from snapshot filename
 */
export function parseSnapshotGen(filename: string): bigint | null {
  const match = filename.match(/^snap_(\d{16})\.gds$/);
  if (!match?.[1]) return null;
  return BigInt(match[1]);
}

/**
 * Parse segment ID from WAL filename
 */
export function parseWalSeg(filename: string): bigint | null {
  const match = filename.match(/^wal_(\d{16})\.gdw$/);
  if (!match?.[1]) return null;
  return BigInt(match[1]);
}
