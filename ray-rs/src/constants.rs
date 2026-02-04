//! Magic numbers and constants for KiteDB
//!
//! Ported from src/constants.ts

use crate::types::NodeId;

// ============================================================================
// Magic bytes (little-endian u32)
// ============================================================================

/// Snapshot magic: "GDS1"
pub const MAGIC_SNAPSHOT: u32 = 0x31534447;

// ============================================================================
// Current versions
// ============================================================================

pub const VERSION_SNAPSHOT: u32 = 3;

// ============================================================================
// Minimum reader versions
// ============================================================================

pub const MIN_READER_SNAPSHOT: u32 = 3;

// ============================================================================
// Alignment requirements
// ============================================================================

/// 64-byte alignment for mmap friendliness
pub const SECTION_ALIGNMENT: usize = 64;
/// 8-byte alignment for WAL records
pub const WAL_RECORD_ALIGNMENT: usize = 8;

// ============================================================================
// Single-file format constants
// ============================================================================

/// Magic bytes for single-file format: "KiteDB format 1\0" (16 bytes)
pub const MAGIC_KITEDB: [u8; 16] = [
  0x4b, 0x69, 0x74, 0x65, 0x44, 0x42, 0x20, 0x66, // "KiteDB f"
  0x6f, 0x72, 0x6d, 0x61, 0x74, 0x20, 0x31, 0x00, // "ormat 1\0"
];

/// Single-file format version
pub const VERSION_SINGLE_FILE: u32 = 1;
pub const MIN_READER_SINGLE_FILE: u32 = 1;

/// Single-file extension
pub const EXT_KITEDB: &str = ".kitedb";

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
pub const WAL_DEFAULT_SIZE: usize = 4 * 1024 * 1024;

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
