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

/// v5: u64 section sizes, u64 string offsets and the sparse NodeIdToPhys
/// layout. v4 and older snapshots are still read; the next checkpoint
/// rewrites them as v5.
pub const VERSION_SNAPSHOT: u32 = 5;

// ============================================================================
// Minimum reader versions
// ============================================================================

pub const MIN_READER_SNAPSHOT: u32 = 5;

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

/// Magic bytes every header this build writes: "KiteDB format 2\0" (16
/// bytes). Released versions up to v0.2.18 check a header only by its magic
/// and the checksum of its first 176 bytes, read only the first header page,
/// and check no format version: a file in their magic, but in the format
/// since (two header slots, salted WAL records, WAL segments), they would
/// misread. They refuse this magic (`InvalidMagic`). The format version
/// fields tell what a file needs from here on.
pub const MAGIC_KITEDB: [u8; 16] = *b"KiteDB format 2\0";

/// The magic of v0.2.18 and earlier, and of files unreleased builds wrote
/// before `MAGIC_KITEDB` (dual-header files of format versions 1 and 2):
/// still read; a writable open rewrites both header slots in the current
/// magic before anything else.
pub const MAGIC_KITEDB_V1: [u8; 16] = *b"KiteDB format 1\0";

/// Single-file format version: the newest this build reads and writes.
///
/// v2: each WAL region has a salt in the header (`wal_primary_salt`,
/// `wal_secondary_salt`), XORed into the CRC of every record written there and
/// replaced whenever the region is emptied for reuse, so records an earlier
/// WAL cycle left behind no longer parse. v1 files (unsalted WAL, salts 0)
/// still open; their WAL replays as is, and the next WAL reset salts it and
/// upgrades the header to v2.
///
/// v3: the header page names WAL segments (`DbHeaderV1::wal_segments`),
/// extents holding WAL records the WAL spilled. A header is v3 only while it
/// names a segment, and v2 again once a checkpoint covers them all, so
/// releases that read v2 keep opening files without segments, and refuse
/// files with some (`VersionMismatch`) instead of missing their commits.
pub const VERSION_SINGLE_FILE: u32 = VERSION_WAL_SEGMENTS;
/// The format of a header that names no WAL segment (salted WAL).
pub const VERSION_SALTED_WAL: u32 = 2;
/// Readers before v2 cannot verify salted WAL records.
pub const MIN_READER_SALTED_WAL: u32 = 2;
/// The format of a header that names WAL segments.
pub const VERSION_WAL_SEGMENTS: u32 = 3;
/// Readers before v3 would miss the commits in WAL segments.
pub const MIN_READER_WAL_SEGMENTS: u32 = 3;
/// The reader version a header without WAL segments requires.
pub const MIN_READER_SINGLE_FILE: u32 = MIN_READER_SALTED_WAL;

/// The most WAL segments a header names (its page holds their table).
pub const MAX_WAL_SEGMENTS: usize = 64;
/// Where the WAL segment table starts in a header page.
pub const WAL_SEGMENT_TABLE_OFFSET: usize = 184;
/// Bytes of the WAL segment table before its entries.
pub const WAL_SEGMENT_TABLE_HEADER_SIZE: usize = 24;
/// Bytes of one WAL segment table entry.
pub const WAL_SEGMENT_ENTRY_SIZE: usize = 32;
/// The largest default size of a WAL segment extent (spills of the WAL fill
/// one until it is full or a checkpoint seals it): by default an extent is a
/// sixteenth of the segment limit, from two WALs up to this.
pub const WAL_SEGMENT_DEFAULT_SIZE: usize = 32 * 1024 * 1024;
/// The largest `wal_segment_size` (1 TiB).
pub const WAL_SEGMENT_MAX_SIZE: u64 = 1 << 40;
/// Default `checkpoint_log_ratio`: a checkpoint starts once the log the
/// snapshot does not cover (WAL segments and WAL) reaches this fraction of
/// the snapshot's size.
pub const CHECKPOINT_LOG_RATIO_DEFAULT: f64 = 0.5;
/// Default `checkpoint_log_budget`: the most log (WAL segments and WAL) an
/// automatic checkpoint waits for. The delta holding the log's commits takes
/// about ten times its bytes of memory. The WAL segment limit, where writers
/// wait for a checkpoint, is at most four times it.
pub const CHECKPOINT_LOG_BUDGET_DEFAULT: u64 = 128 * 1024 * 1024;

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

/// Database header reserved area size: bytes 162..164 and 172..176
pub const DB_HEADER_RESERVED_SIZE: usize = 6;

/// Default WAL size (4MB). The WAL is fixed-size: it does not grow on its own.
pub const WAL_DEFAULT_SIZE: usize = 4 * 1024 * 1024;

/// Minimum WAL to snapshot ratio (10%)
pub const WAL_MIN_SNAPSHOT_RATIO: f64 = 0.1;

// ============================================================================
// Database header flags
// ============================================================================

pub const DB_FLAG_WAL_MODE: u32 = 1 << 0;
pub const DB_FLAG_COMPRESSION: u32 = 1 << 1;
pub const DB_FLAG_ENCRYPTED: u32 = 1 << 2;

/// Header flags this build implements. Every single-file database uses a WAL,
/// so `DB_FLAG_WAL_MODE` changes nothing; any other flag makes open fail, since
/// this build would misread (or, writing, corrupt) such a file.
pub const SUPPORTED_DB_FLAGS: u32 = DB_FLAG_WAL_MODE;

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
/// Salt of a new database's primary WAL region. 0 marks an unsalted region
/// (a v1 WAL, or a secondary region no version 2 checkpoint used).
pub const INITIAL_WAL_SALT: u32 = 1;

// ============================================================================
// Snapshot generation starts at 1 (0 means no snapshot)
// ============================================================================

pub const INITIAL_SNAPSHOT_GEN: u64 = 0;
