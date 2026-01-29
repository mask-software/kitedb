//! Core type definitions for RayDB
//!
//! Based on spec v1.1 (Mode B) - ported from src/types.ts

#[cfg(feature = "napi")]
use napi_derive::napi;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};

// ============================================================================
// Public (stable) IDs - never reused in v1
// ============================================================================

/// Monotonic node ID, never reused. Safe integer (up to 2^53-1 for JS compat)
pub type NodeId = u64;

/// Label ID (u32)
pub type LabelId = u32;

/// Edge type ID (u32)
pub type ETypeId = u32;

/// Property key ID (u32)
pub type PropKeyId = u32;

/// Transaction ID (u64)
pub type TxId = u64;

/// Timestamp for MVCC (u64)
pub type Timestamp = u64;

// ============================================================================
// Snapshot internal IDs
// ============================================================================

/// Physical node index in snapshot arrays (0..num_nodes-1). u32
pub type PhysNode = u32;

/// String ID in string table. u32, 0 = none
pub type StringId = u32;

// ============================================================================
// Manifest (manifest.gdm)
// ============================================================================

/// Manifest header structure
#[derive(Debug, Clone)]
pub struct ManifestV1 {
  pub magic: u32,              // "GDBM" = 0x4D424447
  pub version: u32,            // = 1
  pub min_reader_version: u32, // = 1
  pub reserved: u32,
  pub active_snapshot_gen: u64,
  pub prev_snapshot_gen: u64, // 0 if none
  pub active_wal_seg: u64,
  pub reserved2: [u64; 5],
  pub crc32c: u32,
}

/// Manifest size in bytes
pub const MANIFEST_SIZE: usize = 4 + 4 + 4 + 4 + 8 + 8 + 8 + 8 * 5 + 4; // 76 bytes

// ============================================================================
// Snapshot Header (snapshot.gds)
// ============================================================================

bitflags::bitflags! {
    /// Snapshot flags
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct SnapshotFlags: u32 {
        const HAS_IN_EDGES = 1 << 0;
        const HAS_PROPERTIES = 1 << 1;
        const HAS_KEY_BUCKETS = 1 << 2;
        const HAS_EDGE_BLOOM = 1 << 3; // future
        const HAS_NODE_LABELS = 1 << 4;
        const HAS_VECTORS = 1 << 5;
    }
}

/// Snapshot header structure
#[derive(Debug, Clone)]
pub struct SnapshotHeaderV1 {
  pub magic: u32,              // "GDS1" = 0x31534447
  pub version: u32,            // = 1
  pub min_reader_version: u32, // = 1
  pub flags: SnapshotFlags,
  pub generation: u64,
  pub created_unix_ns: u64,
  pub num_nodes: u64,
  pub num_edges: u64,
  pub max_node_id: u64,
  pub num_labels: u64,
  pub num_etypes: u64,
  pub num_propkeys: u64,
  pub num_strings: u64,
}

/// Section entry in snapshot header
#[derive(Debug, Clone, Copy, Default)]
pub struct SectionEntry {
  pub offset: u64,            // byte offset in file
  pub length: u64,            // size on disk (compressed size if compressed)
  pub compression: u32,       // 0 = none, 1 = zstd, 2 = gzip, 3 = deflate
  pub uncompressed_size: u32, // original size before compression (0 if uncompressed)
}

/// Section identifiers
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SectionId {
  PhysToNodeId = 0,
  NodeIdToPhys = 1,
  OutOffsets = 2,
  OutDst = 3,
  OutEtype = 4,
  InOffsets = 5,
  InSrc = 6,
  InEtype = 7,
  InOutIndex = 8,
  StringOffsets = 9,
  StringBytes = 10,
  LabelStringIds = 11,
  EtypeStringIds = 12,
  PropkeyStringIds = 13,
  NodeKeyString = 14,
  KeyEntries = 15,
  KeyBuckets = 16,
  NodePropOffsets = 17,
  NodePropKeys = 18,
  NodePropVals = 19,
  EdgePropOffsets = 20,
  EdgePropKeys = 21,
  EdgePropVals = 22,
  NodeLabelOffsets = 23,
  NodeLabelIds = 24,
  VectorOffsets = 25,
  VectorData = 26,
}

impl SectionId {
  pub const COUNT_V1: usize = 23;
  pub const COUNT_V2: usize = 25;
  pub const COUNT: usize = 27;

  pub fn from_u32(v: u32) -> Option<Self> {
    match v {
      0 => Some(Self::PhysToNodeId),
      1 => Some(Self::NodeIdToPhys),
      2 => Some(Self::OutOffsets),
      3 => Some(Self::OutDst),
      4 => Some(Self::OutEtype),
      5 => Some(Self::InOffsets),
      6 => Some(Self::InSrc),
      7 => Some(Self::InEtype),
      8 => Some(Self::InOutIndex),
      9 => Some(Self::StringOffsets),
      10 => Some(Self::StringBytes),
      11 => Some(Self::LabelStringIds),
      12 => Some(Self::EtypeStringIds),
      13 => Some(Self::PropkeyStringIds),
      14 => Some(Self::NodeKeyString),
      15 => Some(Self::KeyEntries),
      16 => Some(Self::KeyBuckets),
      17 => Some(Self::NodePropOffsets),
      18 => Some(Self::NodePropKeys),
      19 => Some(Self::NodePropVals),
      20 => Some(Self::EdgePropOffsets),
      21 => Some(Self::EdgePropKeys),
      22 => Some(Self::EdgePropVals),
      23 => Some(Self::NodeLabelOffsets),
      24 => Some(Self::NodeLabelIds),
      25 => Some(Self::VectorOffsets),
      26 => Some(Self::VectorData),
      _ => None,
    }
  }
}

/// Section entry size in bytes
pub const SECTION_ENTRY_SIZE: usize = 8 + 8 + 4 + 4; // 24 bytes

/// Header fixed size
pub const SNAPSHOT_HEADER_SIZE: usize = 4 + 4 + 4 + 4 + 8 + 8 + 8 * 7; // 88 bytes
pub const SNAPSHOT_SECTION_TABLE_OFFSET: usize = SNAPSHOT_HEADER_SIZE;
pub const SNAPSHOT_SECTION_TABLE_SIZE: usize = SectionId::COUNT * SECTION_ENTRY_SIZE;
pub const SNAPSHOT_DATA_START: usize = SNAPSHOT_HEADER_SIZE + SNAPSHOT_SECTION_TABLE_SIZE;

// ============================================================================
// Key Index Entry
// ============================================================================

/// Key index entry for hash-based key lookup
#[derive(Debug, Clone, Copy)]
pub struct KeyIndexEntry {
  pub hash64: u64,    // xxHash64 of key bytes
  pub string_id: u32, // for collision resolution
  pub reserved: u32,
  pub node_id: u64,
}

pub const KEY_INDEX_ENTRY_SIZE: usize = 8 + 4 + 4 + 8; // 24 bytes

// ============================================================================
// WAL Header and Records
// ============================================================================

/// WAL header structure
#[derive(Debug, Clone)]
pub struct WalHeaderV1 {
  pub magic: u32,              // "GDW1" = 0x31574447
  pub version: u32,            // = 1
  pub min_reader_version: u32, // = 1
  pub reserved: u32,
  pub segment_id: u64,
  pub created_unix_ns: u64,
  pub reserved2: [u64; 8],
}

pub const WAL_HEADER_SIZE: usize = 4 + 4 + 4 + 4 + 8 + 8 + 8 * 8; // 96 bytes

/// WAL record types
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalRecordType {
  Begin = 1,
  Commit = 2,
  Rollback = 3,
  CreateNode = 10,
  DeleteNode = 11,
  AddEdge = 20,
  DeleteEdge = 21,
  DefineLabel = 30,
  AddNodeLabel = 31,
  RemoveNodeLabel = 32,
  DefineEtype = 40,
  DefinePropkey = 50,
  SetNodeProp = 51,
  DelNodeProp = 52,
  SetEdgeProp = 53,
  DelEdgeProp = 54,
  // Vector embeddings operations
  SetNodeVector = 60,
  DelNodeVector = 61,
  BatchVectors = 62,
  SealFragment = 63,
  CompactFragments = 64,
}

impl WalRecordType {
  pub fn from_u8(v: u8) -> Option<Self> {
    match v {
      1 => Some(Self::Begin),
      2 => Some(Self::Commit),
      3 => Some(Self::Rollback),
      10 => Some(Self::CreateNode),
      11 => Some(Self::DeleteNode),
      20 => Some(Self::AddEdge),
      21 => Some(Self::DeleteEdge),
      30 => Some(Self::DefineLabel),
      31 => Some(Self::AddNodeLabel),
      32 => Some(Self::RemoveNodeLabel),
      40 => Some(Self::DefineEtype),
      50 => Some(Self::DefinePropkey),
      51 => Some(Self::SetNodeProp),
      52 => Some(Self::DelNodeProp),
      53 => Some(Self::SetEdgeProp),
      54 => Some(Self::DelEdgeProp),
      60 => Some(Self::SetNodeVector),
      61 => Some(Self::DelNodeVector),
      62 => Some(Self::BatchVectors),
      63 => Some(Self::SealFragment),
      64 => Some(Self::CompactFragments),
      _ => None,
    }
  }
}

/// WAL record header (20 bytes)
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct WalRecordHeader {
  pub rec_len: u32,    // unpadded length
  pub record_type: u8, // WalRecordType
  pub flags: u8,
  pub reserved: u16,
  pub txid: u64,
  pub payload_len: u32,
}

pub const WAL_RECORD_HEADER_SIZE: usize = 4 + 1 + 1 + 2 + 8 + 4; // 20 bytes

// ============================================================================
// Property Values
// ============================================================================

/// Property value tag for binary encoding
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "napi", napi)]
pub enum PropValueTag {
  Null = 0,
  Bool = 1,
  I64 = 2,
  F64 = 3,
  String = 4,
  VectorF32 = 5, // Normalized float32 vector for embeddings
}

impl PropValueTag {
  pub fn from_u8(v: u8) -> Option<Self> {
    match v {
      0 => Some(Self::Null),
      1 => Some(Self::Bool),
      2 => Some(Self::I64),
      3 => Some(Self::F64),
      4 => Some(Self::String),
      5 => Some(Self::VectorF32),
      _ => None,
    }
  }
}

/// Property value (discriminated union)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PropValue {
  Null,
  Bool(bool),
  I64(i64),
  F64(f64),
  String(String),
  VectorF32(Vec<f32>),
}

impl PropValue {
  pub fn tag(&self) -> PropValueTag {
    match self {
      PropValue::Null => PropValueTag::Null,
      PropValue::Bool(_) => PropValueTag::Bool,
      PropValue::I64(_) => PropValueTag::I64,
      PropValue::F64(_) => PropValueTag::F64,
      PropValue::String(_) => PropValueTag::String,
      PropValue::VectorF32(_) => PropValueTag::VectorF32,
    }
  }
}

/// Fixed-width disk encoding for properties (16 bytes)
pub const PROP_VALUE_DISK_SIZE: usize = 16;

// ============================================================================
// Edge representation
// ============================================================================

/// Edge structure
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Edge {
  pub src: NodeId,
  pub etype: ETypeId,
  pub dst: NodeId,
}

/// Edge patch for delta overlay (sorted by etype, then other)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EdgePatch {
  pub etype: ETypeId,
  pub other: NodeId, // dst for out-edges, src for in-edges
}

// ============================================================================
// Delta (in-memory overlay)
// ============================================================================

/// Node delta - changes to a single node
#[derive(Debug, Default, Clone)]
pub struct NodeDelta {
  pub key: Option<String>,
  pub labels: Option<HashSet<LabelId>>,
  pub labels_deleted: Option<HashSet<LabelId>>,
  pub props: Option<HashMap<PropKeyId, Option<PropValue>>>, // None value = deleted
}

/// Delta state - all uncommitted changes
#[derive(Debug, Default, Clone)]
pub struct DeltaState {
  // Node state
  pub created_nodes: HashMap<NodeId, NodeDelta>,
  pub deleted_nodes: HashSet<NodeId>,
  pub modified_nodes: HashMap<NodeId, NodeDelta>, // existing nodes with modified labels/props

  // Edge patches (both directions maintained)
  pub out_add: HashMap<NodeId, BTreeSet<EdgePatch>>,
  pub out_del: HashMap<NodeId, BTreeSet<EdgePatch>>,
  pub in_add: HashMap<NodeId, BTreeSet<EdgePatch>>,
  pub in_del: HashMap<NodeId, BTreeSet<EdgePatch>>,

  // Edge properties (keyed by (src, etype, dst))
  pub edge_props: HashMap<(NodeId, ETypeId, NodeId), HashMap<PropKeyId, Option<PropValue>>>,

  // New definitions
  pub new_labels: HashMap<LabelId, String>,
  pub new_etypes: HashMap<ETypeId, String>,
  pub new_propkeys: HashMap<PropKeyId, String>,

  // Key index delta
  pub key_index: HashMap<String, NodeId>,
  pub key_index_deleted: HashSet<String>,

  // Reverse index for efficient edge cleanup on node deletion
  // Maps destination node -> set of source nodes with edges to it
  pub incoming_edge_sources: HashMap<NodeId, HashSet<NodeId>>,

  // Pending vector operations (keyed by (node_id, prop_key_id))
  // Some(vec) = set, None = delete
  pub pending_vectors: HashMap<(NodeId, PropKeyId), Option<Vec<f32>>>,
}

// ============================================================================
// Database options
// ============================================================================

/// Database open options
#[derive(Debug, Clone, Default)]
pub struct OpenOptions {
  pub read_only: bool,
  pub create_if_missing: bool,
  pub lock_file: bool,
  pub mvcc: bool,
  pub mvcc_gc_interval_ms: Option<u64>,    // Default: 5000
  pub mvcc_retention_ms: Option<u64>,      // Default: 60000
  pub mvcc_max_chain_depth: Option<usize>, // Default: 10

  // Single-file options
  pub auto_checkpoint: bool,     // Default: true
  pub checkpoint_threshold: f64, // Default: 0.8
  pub cache_snapshot: bool,      // Default: true

  // Single-file creation options
  pub page_size: Option<usize>, // Default: 4096
  pub wal_size: Option<usize>,  // Default: 64MB
}

// ============================================================================
// Cache Configuration
// ============================================================================

/// Cache options
#[derive(Debug, Clone, Default)]
pub struct CacheOptions {
  pub enabled: bool,
  pub property_cache: Option<PropertyCacheConfig>,
  pub traversal_cache: Option<TraversalCacheConfig>,
  pub query_cache: Option<QueryCacheConfig>,
}

#[derive(Debug, Clone)]
pub struct PropertyCacheConfig {
  pub max_node_props: usize, // Default: 10000
  pub max_edge_props: usize, // Default: 10000
}

impl Default for PropertyCacheConfig {
  fn default() -> Self {
    Self {
      max_node_props: 10000,
      max_edge_props: 10000,
    }
  }
}

#[derive(Debug, Clone)]
pub struct TraversalCacheConfig {
  pub max_entries: usize,             // Default: 5000
  pub max_neighbors_per_entry: usize, // Default: 100
}

impl Default for TraversalCacheConfig {
  fn default() -> Self {
    Self {
      max_entries: 5000,
      max_neighbors_per_entry: 100,
    }
  }
}

#[derive(Debug, Clone)]
pub struct QueryCacheConfig {
  pub max_entries: usize, // Default: 1000
  pub ttl_ms: Option<u64>,
}

impl Default for QueryCacheConfig {
  fn default() -> Self {
    Self {
      max_entries: 1000,
      ttl_ms: None,
    }
  }
}

/// Cache statistics
#[derive(Debug, Clone, Default)]
pub struct CacheStats {
  pub property_cache_hits: u64,
  pub property_cache_misses: u64,
  pub property_cache_size: usize,
  pub traversal_cache_hits: u64,
  pub traversal_cache_misses: u64,
  pub traversal_cache_size: usize,
  pub query_cache_hits: u64,
  pub query_cache_misses: u64,
  pub query_cache_size: usize,
}

// ============================================================================
// Database Statistics
// ============================================================================

/// Database statistics
#[derive(Debug, Clone)]
pub struct DbStats {
  pub snapshot_gen: u64,
  pub snapshot_nodes: u64,
  pub snapshot_edges: u64,
  pub snapshot_max_node_id: u64,
  pub delta_nodes_created: usize,
  pub delta_nodes_deleted: usize,
  pub delta_edges_added: usize,
  pub delta_edges_deleted: usize,
  pub wal_segment: u64,
  pub wal_bytes: u64,
  pub recommend_compact: bool,
  pub mvcc_stats: Option<MvccStats>,
}

#[derive(Debug, Clone)]
pub struct MvccStats {
  pub active_transactions: usize,
  pub min_active_ts: u64,
  pub versions_pruned: u64,
  pub gc_runs: u64,
  pub last_gc_time: u64,
  pub committed_writes_size: usize,
  pub committed_writes_pruned: usize,
}

/// Database check result
#[derive(Debug, Clone)]
pub struct CheckResult {
  pub valid: bool,
  pub errors: Vec<String>,
  pub warnings: Vec<String>,
}

// ============================================================================
// Transaction State
// ============================================================================

/// Transaction state (pending operations)
#[derive(Debug, Default)]
pub struct TxState {
  pub txid: TxId,
  pub read_only: bool,
  /// Snapshot timestamp for MVCC reads
  pub snapshot_ts: u64,
  pub pending_created_nodes: HashMap<NodeId, NodeDelta>,
  pub pending_deleted_nodes: HashSet<NodeId>,
  pub pending_out_add: HashMap<NodeId, BTreeSet<EdgePatch>>,
  pub pending_out_del: HashMap<NodeId, BTreeSet<EdgePatch>>,
  pub pending_in_add: HashMap<NodeId, BTreeSet<EdgePatch>>,
  pub pending_in_del: HashMap<NodeId, BTreeSet<EdgePatch>>,
  pub pending_node_props: HashMap<NodeId, HashMap<PropKeyId, Option<PropValue>>>,
  /// Pending node label additions (node_id -> labels)
  pub pending_node_labels_add: HashMap<NodeId, HashSet<LabelId>>,
  /// Pending node label removals (node_id -> labels)
  pub pending_node_labels_del: HashMap<NodeId, HashSet<LabelId>>,
  pub pending_edge_props: HashMap<(NodeId, ETypeId, NodeId), HashMap<PropKeyId, Option<PropValue>>>,
  pub pending_new_labels: HashMap<LabelId, String>,
  pub pending_new_etypes: HashMap<ETypeId, String>,
  pub pending_new_propkeys: HashMap<PropKeyId, String>,
  pub pending_key_updates: HashMap<String, NodeId>,
  pub pending_key_deletes: HashSet<String>,
  // Vector embeddings pending operations
  pub pending_vector_sets: HashMap<(NodeId, PropKeyId), Vec<f32>>,
  pub pending_vector_deletes: HashSet<(NodeId, PropKeyId)>,
}

impl TxState {
  pub fn new(txid: TxId, read_only: bool, snapshot_ts: u64) -> Self {
    Self {
      txid,
      read_only,
      snapshot_ts,
      ..Default::default()
    }
  }
}

// ============================================================================
// MVCC Types
// ============================================================================

/// MVCC transaction metadata
#[derive(Debug, Clone)]
pub struct MvccTransaction {
  pub txid: TxId,
  pub start_ts: Timestamp,
  pub commit_ts: Option<Timestamp>,
  pub status: MvccTxStatus,
  pub read_set: HashSet<String>,
  pub write_set: HashSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MvccTxStatus {
  Active,
  Committed,
  Aborted,
}

/// Versioned record for MVCC
#[derive(Debug)]
pub struct VersionedRecord<T> {
  pub data: T,
  pub txid: TxId,
  pub commit_ts: Timestamp,
  pub prev: Option<Box<VersionedRecord<T>>>,
  pub deleted: bool,
}

/// Node version data
#[derive(Debug, Clone)]
pub struct NodeVersionData {
  pub node_id: NodeId,
  pub delta: NodeDelta,
}

/// Edge version data
#[derive(Debug, Clone)]
pub struct EdgeVersionData {
  pub src: NodeId,
  pub etype: ETypeId,
  pub dst: NodeId,
  pub added: bool, // true if added, false if deleted
}

// ============================================================================
// Single-File Database Header
// ============================================================================

/// Database header for single-file format (4KB)
#[derive(Debug, Clone)]
pub struct DbHeaderV1 {
  pub magic: [u8; 16], // "RayDB format 1\0"
  pub page_size: u32,
  pub version: u32,
  pub min_reader_version: u32,
  pub flags: u32,
  pub change_counter: u64,
  pub db_size_pages: u64,
  pub snapshot_start_page: u64,
  pub snapshot_page_count: u64,
  pub wal_start_page: u64,
  pub wal_page_count: u64,
  pub wal_head: u64,
  pub wal_tail: u64,
  pub active_snapshot_gen: u64,
  pub prev_snapshot_gen: u64,
  pub max_node_id: u64,
  pub next_tx_id: u64,
  pub last_commit_ts: u64,
  pub schema_cookie: u64,
  // V2 dual-buffer WAL fields
  pub wal_primary_head: u64,
  pub wal_secondary_head: u64,
  pub active_wal_region: u8,      // 0=primary, 1=secondary
  pub checkpoint_in_progress: u8, // for crash recovery
}

/// Size of fixed header fields before reserved area (in bytes)
pub const DB_HEADER_FIXED_SIZE: usize = 176;

/// Size of reserved area in header (in bytes)
pub const DB_HEADER_RESERVED_SIZE: usize = 14;

/// Size of V2 fields
pub const DB_HEADER_V2_FIELDS_SIZE: usize = 8 + 8 + 1 + 1; // 18 bytes

// ============================================================================
// Node creation options
// ============================================================================

/// Options for creating a new node
#[derive(Debug, Default, Clone)]
pub struct NodeOpts {
  pub key: Option<String>,
  pub labels: Option<Vec<LabelId>>,
  pub props: Option<HashMap<PropKeyId, PropValue>>,
}
