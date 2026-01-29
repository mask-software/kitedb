//! Magic numbers and constants for RayDB
//!
//! Ported from src/constants.ts

use crate::types::NodeId;

// ============================================================================
// Magic bytes (little-endian u32)
// ============================================================================

/// Manifest magic: "GDBM"
pub const MAGIC_MANIFEST: u32 = 0x4D424447;
/// Snapshot magic: "GDS1"
pub const MAGIC_SNAPSHOT: u32 = 0x31534447;
/// WAL magic: "GDW1"
pub const MAGIC_WAL: u32 = 0x31574447;

// ============================================================================
// Current versions
// ============================================================================

pub const VERSION_MANIFEST: u32 = 1;
pub const VERSION_SNAPSHOT: u32 = 3;
pub const VERSION_WAL: u32 = 1;

// ============================================================================
// Minimum reader versions
// ============================================================================

pub const MIN_READER_MANIFEST: u32 = 1;
pub const MIN_READER_SNAPSHOT: u32 = 3;
pub const MIN_READER_WAL: u32 = 1;

// ============================================================================
// Alignment requirements
// ============================================================================

/// 64-byte alignment for mmap friendliness
pub const SECTION_ALIGNMENT: usize = 64;
/// 8-byte alignment for WAL records
pub const WAL_RECORD_ALIGNMENT: usize = 8;

// ============================================================================
// File extensions
// ============================================================================

pub const EXT_MANIFEST: &str = ".gdm";
pub const EXT_SNAPSHOT: &str = ".gds";
pub const EXT_WAL: &str = ".gdw";
pub const EXT_LOCK: &str = ".gdl";

// ============================================================================
// File name patterns
// ============================================================================

pub const MANIFEST_FILENAME: &str = "manifest.gdm";
pub const LOCK_FILENAME: &str = "lock.gdl";
pub const SNAPSHOTS_DIR: &str = "snapshots";
pub const WAL_DIR: &str = "wal";
pub const TRASH_DIR: &str = "trash";

// ============================================================================
// Single-file format constants
// ============================================================================

/// Magic bytes for single-file format: "RayDB format 1\0" (16 bytes)
pub const MAGIC_RAYDB: [u8; 16] = [
  0x52, 0x61, 0x79, 0x44, 0x42, 0x20, 0x66, 0x6f, // "RayDB fo"
  0x72, 0x6d, 0x61, 0x74, 0x20, 0x31, 0x00, 0x00, // "rmat 1\0\0"
];

/// Single-file format version
pub const VERSION_SINGLE_FILE: u32 = 1;
pub const MIN_READER_SINGLE_FILE: u32 = 1;

/// Single-file extension
pub const EXT_RAYDB: &str = ".raydb";

/// Default page size (4KB - matches OS page size and SSD blocks)
pub const DEFAULT_PAGE_SIZE: usize = 4096;

/// Minimum page size (4KB)
pub const MIN_PAGE_SIZE: usize = 4096;

/// Maximum page size (64KB)
pub const MAX_PAGE_SIZE: usize = 65536;

/// OS page size for mmap alignment validation
pub const OS_PAGE_SIZE: usize = 4096;

/// Database header size (first page)
pub const DB_HEADER_SIZE: usize = 4096;

/// Database header reserved area size - reduced for V2 fields
pub const DB_HEADER_RESERVED_SIZE: usize = 14;

/// Default WAL buffer size (1MB - grows dynamically as needed)
pub const WAL_DEFAULT_SIZE: usize = 1024 * 1024;

/// Minimum WAL to snapshot ratio (10%)
pub const WAL_MIN_SNAPSHOT_RATIO: f64 = 0.1;

/// SQLite-style lock byte offset (2^30 = 1GB)
pub const LOCK_BYTE_OFFSET: u64 = 0x40000000;

/// Lock byte range size
pub const LOCK_BYTE_RANGE: usize = 512;

// ============================================================================
// Database header flags
// ============================================================================

pub const DB_FLAG_WAL_MODE: u32 = 1 << 0;
pub const DB_FLAG_COMPRESSION: u32 = 1 << 1;
pub const DB_FLAG_ENCRYPTED: u32 = 1 << 2;

// ============================================================================
// Thresholds for compact recommendation
// ============================================================================

/// 10% of snapshot edges
pub const COMPACT_EDGE_RATIO: f64 = 0.1;
/// 10% of snapshot nodes
pub const COMPACT_NODE_RATIO: f64 = 0.1;
/// 64MB
pub const COMPACT_WAL_SIZE: usize = 64 * 1024 * 1024;

// ============================================================================
// Delta set upgrade threshold
// ============================================================================

/// Upgrade from Vec to Set after this many elements
pub const DELTA_SET_UPGRADE_THRESHOLD: usize = 64;

// ============================================================================
// Compression settings
// ============================================================================

/// Default minimum section size for compression (bytes)
pub const COMPRESSION_MIN_SIZE: usize = 64;

// ============================================================================
// Initial IDs (start from 1, 0 is reserved/null)
// ============================================================================

pub const INITIAL_NODE_ID: NodeId = 1;
pub const INITIAL_LABEL_ID: u32 = 1;
pub const INITIAL_ETYPE_ID: u32 = 1;
pub const INITIAL_PROPKEY_ID: u32 = 1;
pub const INITIAL_TX_ID: u64 = 1;

// ============================================================================
// Snapshot generation starts at 1 (0 means no snapshot)
// ============================================================================

pub const INITIAL_SNAPSHOT_GEN: u64 = 0;
pub const INITIAL_WAL_SEG: u64 = 1;

// ============================================================================
// Filename formatting utilities
// ============================================================================

/// Format snapshot filename from generation
#[inline]
pub fn snapshot_filename(gen: u64) -> String {
  format!("snap_{gen:016}{EXT_SNAPSHOT}")
}

/// Format WAL filename from segment ID
#[inline]
pub fn wal_filename(seg: u64) -> String {
  format!("wal_{seg:016}{EXT_WAL}")
}

/// Parse generation from snapshot filename
pub fn parse_snapshot_gen(filename: &str) -> Option<u64> {
  let prefix = "snap_";
  let suffix = EXT_SNAPSHOT;

  if !filename.starts_with(prefix) || !filename.ends_with(suffix) {
    return None;
  }

  let num_str = &filename[prefix.len()..filename.len() - suffix.len()];
  if num_str.len() != 16 {
    return None;
  }

  num_str.parse().ok()
}

/// Parse segment ID from WAL filename
pub fn parse_wal_seg(filename: &str) -> Option<u64> {
  let prefix = "wal_";
  let suffix = EXT_WAL;

  if !filename.starts_with(prefix) || !filename.ends_with(suffix) {
    return None;
  }

  let num_str = &filename[prefix.len()..filename.len() - suffix.len()];
  if num_str.len() != 16 {
    return None;
  }

  num_str.parse().ok()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_snapshot_filename() {
    assert_eq!(snapshot_filename(1), "snap_0000000000000001.gds");
    assert_eq!(snapshot_filename(12345), "snap_0000000000012345.gds");
  }

  #[test]
  fn test_wal_filename() {
    assert_eq!(wal_filename(1), "wal_0000000000000001.gdw");
    assert_eq!(wal_filename(99999), "wal_0000000000099999.gdw");
  }

  #[test]
  fn test_parse_snapshot_gen() {
    assert_eq!(parse_snapshot_gen("snap_0000000000000001.gds"), Some(1));
    assert_eq!(parse_snapshot_gen("snap_0000000000012345.gds"), Some(12345));
    assert_eq!(parse_snapshot_gen("invalid.gds"), None);
    assert_eq!(parse_snapshot_gen("snap_123.gds"), None);
  }

  #[test]
  fn test_parse_wal_seg() {
    assert_eq!(parse_wal_seg("wal_0000000000000001.gdw"), Some(1));
    assert_eq!(parse_wal_seg("wal_0000000000099999.gdw"), Some(99999));
    assert_eq!(parse_wal_seg("invalid.gdw"), None);
  }
}
