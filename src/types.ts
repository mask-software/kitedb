/**
 * Embedded Graph DB Type Definitions
 * Based on spec v1.1 (Mode B)
 */

// ============================================================================
// Public (stable) IDs - never reused in v1
// ============================================================================

/** Monotonic node ID, never reused. Safe integer (up to 2^53-1) */
export type NodeID = number;

/** Label ID. u32 */
export type LabelID = number;

/** Edge type ID. u32 */
export type ETypeID = number;

/** Property key ID. u32 */
export type PropKeyID = number;

// ============================================================================
// Snapshot internal IDs
// ============================================================================

/** Physical node index in snapshot arrays (0..num_nodes-1). u32 */
export type PhysNode = number;

/** String ID in string table. u32, 0 = none */
export type StringID = number;

// ============================================================================
// Manifest (manifest.gdm)
// ============================================================================

export interface ManifestV1 {
  magic: number; // u32 = "GDBM" = 0x4D424447
  version: number; // u32 = 1
  minReaderVersion: number; // u32 = 1
  reserved: number; // u32
  activeSnapshotGen: bigint; // u64
  prevSnapshotGen: bigint; // u64, 0 if none
  activeWalSeg: bigint; // u64
  reserved2: bigint[]; // u64[5]
  crc32c: number; // u32
}

export const MANIFEST_SIZE = 4 + 4 + 4 + 4 + 8 + 8 + 8 + 8 * 5 + 4; // 76 bytes

// ============================================================================
// Snapshot Header (snapshot.gds)
// ============================================================================

export enum SnapshotFlags {
  HAS_IN_EDGES = 1 << 0,
  HAS_PROPERTIES = 1 << 1,
  HAS_KEY_BUCKETS = 1 << 2,
  HAS_EDGE_BLOOM = 1 << 3, // future
}

export interface SnapshotHeaderV1 {
  magic: number; // u32 = "GDS1" = 0x31534447
  version: number; // u32 = 1
  minReaderVersion: number; // u32 = 1
  flags: number; // u32
  generation: bigint; // u64
  createdUnixNs: bigint; // u64
  numNodes: bigint; // u64
  numEdges: bigint; // u64
  maxNodeId: bigint; // u64
  numLabels: bigint; // u64
  numEtypes: bigint; // u64
  numPropkeys: bigint; // u64
  numStrings: bigint; // u64
}

export interface SectionEntry {
  offset: bigint; // u64 - byte offset in file
  length: bigint; // u64 - size on disk (compressed size if compressed)
  compression: number; // u32, 0 = none, 1 = zstd, 2 = gzip, 3 = deflate
  uncompressedSize: number; // u32 - original size before compression (0 if uncompressed)
}

export enum SectionId {
  PHYS_TO_NODEID = 0,
  NODEID_TO_PHYS = 1,
  OUT_OFFSETS = 2,
  OUT_DST = 3,
  OUT_ETYPE = 4,
  IN_OFFSETS = 5,
  IN_SRC = 6,
  IN_ETYPE = 7,
  IN_OUT_INDEX = 8,
  STRING_OFFSETS = 9,
  STRING_BYTES = 10,
  LABEL_STRING_IDS = 11,
  ETYPE_STRING_IDS = 12,
  PROPKEY_STRING_IDS = 13,
  NODE_KEY_STRING = 14,
  KEY_ENTRIES = 15,
  KEY_BUCKETS = 16,
  NODE_PROP_OFFSETS = 17,
  NODE_PROP_KEYS = 18,
  NODE_PROP_VALS = 19,
  EDGE_PROP_OFFSETS = 20,
  EDGE_PROP_KEYS = 21,
  EDGE_PROP_VALS = 22,
  _COUNT = 23,
}

export const SECTION_ENTRY_SIZE = 8 + 8 + 4 + 4; // 24 bytes

// Header fixed size: magic(4) + version(4) + minReader(4) + flags(4) + gen(8) + created(8)
//   + numNodes(8) + numEdges(8) + maxNodeId(8) + numLabels(8) + numEtypes(8) + numPropkeys(8) + numStrings(8)
export const SNAPSHOT_HEADER_SIZE = 4 + 4 + 4 + 4 + 8 + 8 + 8 * 7; // 88 bytes
export const SNAPSHOT_SECTION_TABLE_OFFSET = SNAPSHOT_HEADER_SIZE;
export const SNAPSHOT_SECTION_TABLE_SIZE =
  SectionId._COUNT * SECTION_ENTRY_SIZE;
export const SNAPSHOT_DATA_START =
  SNAPSHOT_HEADER_SIZE + SNAPSHOT_SECTION_TABLE_SIZE;

// ============================================================================
// Key Index Entry
// ============================================================================

export interface KeyIndexEntry {
  hash64: bigint; // u64 - xxHash64 of key bytes
  stringId: number; // u32 - for collision resolution
  reserved: number; // u32
  nodeId: bigint; // u64
}

export const KEY_INDEX_ENTRY_SIZE = 8 + 4 + 4 + 8; // 24 bytes

// ============================================================================
// WAL Header and Records
// ============================================================================

export interface WalHeaderV1 {
  magic: number; // u32 = "GDW1" = 0x31574447
  version: number; // u32 = 1
  minReaderVersion: number; // u32 = 1
  reserved: number; // u32
  segmentId: bigint; // u64
  createdUnixNs: bigint; // u64
  reserved2: bigint[]; // u64[8]
}

export const WAL_HEADER_SIZE = 4 + 4 + 4 + 4 + 8 + 8 + 8 * 8; // 96 bytes

export enum WalRecordType {
  BEGIN = 1,
  COMMIT = 2,
  ROLLBACK = 3,
  CREATE_NODE = 10,
  DELETE_NODE = 11,
  ADD_EDGE = 20,
  DELETE_EDGE = 21,
  DEFINE_LABEL = 30,
  ADD_NODE_LABEL = 31,
  REMOVE_NODE_LABEL = 32,
  DEFINE_ETYPE = 40,
  DEFINE_PROPKEY = 50,
  SET_NODE_PROP = 51,
  DEL_NODE_PROP = 52,
  SET_EDGE_PROP = 53,
  DEL_EDGE_PROP = 54,
  // Vector embeddings operations
  SET_NODE_VECTOR = 60,
  DEL_NODE_VECTOR = 61,
  BATCH_VECTORS = 62,
  SEAL_FRAGMENT = 63,
  COMPACT_FRAGMENTS = 64,
}

export interface WalRecordHeader {
  recLen: number; // u32 - unpadded length
  type: number; // u8
  flags: number; // u8
  reserved: number; // u16
  txid: bigint; // u64
  payloadLen: number; // u32
}

export const WAL_RECORD_HEADER_SIZE = 4 + 1 + 1 + 2 + 8 + 4; // 20 bytes

// ============================================================================
// Property Values
// ============================================================================

export enum PropValueTag {
  NULL = 0,
  BOOL = 1,
  I64 = 2,
  F64 = 3,
  STRING = 4,
  VECTOR_F32 = 5, // Normalized float32 vector for embeddings
}

export type PropValue =
  | { tag: PropValueTag.NULL }
  | { tag: PropValueTag.BOOL; value: boolean }
  | { tag: PropValueTag.I64; value: bigint }
  | { tag: PropValueTag.F64; value: number }
  | { tag: PropValueTag.STRING; value: string }
  | { tag: PropValueTag.VECTOR_F32; value: Float32Array };

/** Fixed-width disk encoding for properties (16 bytes) */
export interface PropValueDisk {
  tag: number; // u8
  pad: Uint8Array; // 7 bytes
  payload: bigint; // u64 (bool as 0/1, i64/f64 via bitcast, string as StringID)
}

export const PROP_VALUE_DISK_SIZE = 16;

// ============================================================================
// Edge representation
// ============================================================================

export interface Edge {
  src: NodeID;
  etype: ETypeID;
  dst: NodeID;
}

export interface EdgeKey {
  etype: ETypeID;
  other: NodeID; // dst for out-edges, src for in-edges
}

// ============================================================================
// Delta (in-memory overlay)
// ============================================================================

export interface NodeDelta {
  key?: string;
  labels?: Set<LabelID>;           // Lazy: only allocated when labels are added
  labelsDeleted?: Set<LabelID>;    // Lazy: only allocated when labels are deleted
  props?: Map<PropKeyID, PropValue | null>; // Lazy: only allocated when props are set/deleted
}

export interface EdgePatch {
  etype: ETypeID;
  other: NodeID;
}

export interface DeltaState {
  // Node state
  createdNodes: Map<NodeID, NodeDelta>;
  deletedNodes: Set<NodeID>;
  modifiedNodes: Map<NodeID, NodeDelta>; // existing nodes with modified labels/props

  // Edge patches (both directions maintained)
  outAdd: Map<NodeID, EdgePatch[]>;
  outDel: Map<NodeID, EdgePatch[]>;
  inAdd: Map<NodeID, EdgePatch[]>;
  inDel: Map<NodeID, EdgePatch[]>;

  // Edge properties (keyed by serialized edge key)
  edgeProps: Map<string, Map<PropKeyID, PropValue | null>>;

  // New definitions
  newLabels: Map<LabelID, string>;
  newEtypes: Map<ETypeID, string>;
  newPropkeys: Map<PropKeyID, string>;

  // Key index delta
  keyIndex: Map<string, NodeID>;
  keyIndexDeleted: Set<string>;

  // Reverse index for efficient edge cleanup on node deletion
  // Maps destination node -> set of source nodes with edges to it
  // Only populated lazily when edges are added
  incomingEdgeSources?: Map<NodeID, Set<NodeID>>;

  // Cached edge Sets for O(1) edge existence checks (lazily populated)
  // Only populated when patch arrays exceed EDGE_SET_THRESHOLD
  outAddSets?: Map<NodeID, Set<bigint>>;
  outDelSets?: Map<NodeID, Set<bigint>>;
}

// ============================================================================
// Database options and state
// ============================================================================

export interface OpenOptions {
  readOnly?: boolean;
  createIfMissing?: boolean;
  /** 
   * Enable file locking for cross-process safety. Default: true
   * Set to false to disable locking entirely (useful for single-process scenarios)
   */
  lockFile?: boolean;
  /**
   * Require proper OS-level file locking (via fs-ext). Default: false
   * When true, opening the database will fail if fs-ext is not installed.
   * This provides strict guarantees against multi-process data corruption.
   * Install fs-ext with: `bun add fs-ext`
   */
  requireLocking?: boolean;
  cache?: CacheOptions;
  mvcc?: boolean; // Enable MVCC mode (default: false for backward compatibility)
  mvccGcInterval?: number; // GC interval in ms (default: 5000)
  mvccRetentionMs?: number; // Minimum version retention time in ms (default: 60000)
  mvccMaxChainDepth?: number; // Maximum version chain depth before truncation (default: 10)
  
  // Single-file options
  autoCheckpoint?: boolean;      // Default: true - auto-checkpoint when WAL fills
  checkpointThreshold?: number;  // Default: 0.8 - trigger checkpoint at 80% WAL usage
  cacheSnapshot?: boolean;       // Default: true - cache parsed snapshot in memory
  
  // Single-file creation options
  pageSize?: number;             // Default: 4096 - page size for new databases
  walSize?: number;              // Default: 64MB - WAL area size
}

// ============================================================================
// Cache Configuration
// ============================================================================

export interface CacheOptions {
  enabled?: boolean;
  propertyCache?: PropertyCacheConfig;
  traversalCache?: TraversalCacheConfig;
  queryCache?: QueryCacheConfig;
}

export interface PropertyCacheConfig {
  maxNodeProps?: number;  // Default: 10000
  maxEdgeProps?: number;  // Default: 10000
}

export interface TraversalCacheConfig {
  maxEntries?: number;    // Default: 5000
  maxNeighborsPerEntry?: number;  // Cap stored neighbors, Default: 100
}

export interface QueryCacheConfig {
  maxEntries?: number;    // Default: 1000
  ttlMs?: number;         // Optional TTL for query results
}

export interface CacheStats {
  propertyCache: {
    hits: number;
    misses: number;
    size: number;
    maxSize: number;
  };
  traversalCache: {
    hits: number;
    misses: number;
    size: number;
    maxSize: number;
  };
  queryCache: {
    hits: number;
    misses: number;
    size: number;
    maxSize: number;
  };
}

export interface DbStats {
  snapshotGen: bigint;
  snapshotNodes: bigint;
  snapshotEdges: bigint;
  snapshotMaxNodeId: number;
  deltaNodesCreated: number;
  deltaNodesDeleted: number;
  deltaEdgesAdded: number;
  deltaEdgesDeleted: number;
  walSegment: bigint;
  walBytes: bigint;
  recommendCompact: boolean;
  mvccStats?: MvccStats;
}

export interface MvccStats {
  activeTransactions: number;
  minActiveTs: bigint;
  versionsPruned: bigint;
  gcRuns: number;
  lastGcTime: bigint;
}

export interface CheckResult {
  valid: boolean;
  errors: string[];
  warnings: string[];
}

// ============================================================================
// Transaction
// ============================================================================

export interface TxState {
  txid: bigint;
  // Pending operations (not yet written to WAL)
  pendingCreatedNodes: Map<NodeID, NodeDelta>;
  pendingDeletedNodes: Set<NodeID>;
  pendingOutAdd: Map<NodeID, EdgePatch[]>;
  pendingOutDel: Map<NodeID, EdgePatch[]>;
  pendingInAdd: Map<NodeID, EdgePatch[]>;
  pendingInDel: Map<NodeID, EdgePatch[]>;
  pendingNodeProps: Map<NodeID, Map<PropKeyID, PropValue | null>>;
  pendingEdgeProps: Map<string, Map<PropKeyID, PropValue | null>>;
  pendingNewLabels: Map<LabelID, string>;
  pendingNewEtypes: Map<ETypeID, string>;
  pendingNewPropkeys: Map<PropKeyID, string>;
  pendingKeyUpdates: Map<string, NodeID>;
  pendingKeyDeletes: Set<string>;
  // Vector embeddings pending operations
  // Key format: "nodeId:propKeyId"
  pendingVectorSets: Map<string, { nodeId: NodeID; propKeyId: PropKeyID; vector: Float32Array }>;
  pendingVectorDeletes: Set<string>; // Set of "nodeId:propKeyId" keys
}

// ============================================================================
// MVCC Types
// ============================================================================

export interface MvccTransaction {
  txid: bigint;              // Unique transaction ID
  startTs: bigint;           // Snapshot timestamp (for visibility)
  commitTs: bigint | null;   // Set on commit, null while active
  status: 'active' | 'committed' | 'aborted';
  readSet: Set<string>;      // Keys read (for conflict detection)
  writeSet: Set<string>;     // Keys written
}

export interface VersionedRecord<T> {
  data: T;
  txid: bigint;              // Transaction that created this version
  commitTs: bigint;          // Commit timestamp
  prev: VersionedRecord<T> | null;  // Previous version (undo chain)
  deleted: boolean;          // Tombstone marker
}

export interface NodeVersionData {
  nodeId: NodeID;
  delta: NodeDelta;
}

export interface EdgeVersionData {
  src: NodeID;
  etype: ETypeID;
  dst: NodeID;
  added: boolean;  // true if added, false if deleted
}

export interface VersionChainStore {
  nodeVersions: Map<NodeID, VersionedRecord<NodeVersionData>>;
  edgeVersions: Map<bigint, VersionedRecord<EdgeVersionData>>; // key: numeric composite (src << 40 | etype << 20 | dst)
  nodePropVersions: Map<bigint, VersionedRecord<PropValue | null>>; // key: numeric composite (nodeId << 24 | propKeyId)
  edgePropVersions: Map<bigint, VersionedRecord<PropValue | null>>; // key: numeric composite (src << 44 | etype << 24 | dst >> 16 | propKeyId) - see nodePropKey/edgePropKey
}

export class ConflictError extends Error {
  constructor(
    message: string,
    public readonly txid: bigint,
    public readonly conflictingKeys: string[],
  ) {
    super(message);
    this.name = 'ConflictError';
  }
}

// ============================================================================
// Internal snapshot data (mmap'd)
// ============================================================================

export interface SnapshotData {
  buffer: Uint8Array;
  view: DataView;
  header: SnapshotHeaderV1;
  sections: SectionEntry[];

  // Cached section views
  physToNodeId: DataView;
  nodeIdToPhys: DataView;
  outOffsets: DataView;
  outDst: DataView;
  outEtype: DataView;
  inOffsets: DataView | null;
  inSrc: DataView | null;
  inEtype: DataView | null;
  inOutIndex: DataView | null;
  stringOffsets: DataView;
  stringBytes: Uint8Array;
  labelStringIds: DataView;
  etypeStringIds: DataView;
  propkeyStringIds: DataView | null;
  nodeKeyString: DataView;
  keyEntries: DataView;
  keyBuckets: DataView | null;
  nodePropOffsets: DataView | null;
  nodePropKeys: DataView | null;
  nodePropVals: DataView | null;
  edgePropOffsets: DataView | null;
  edgePropKeys: DataView | null;
  edgePropVals: DataView | null;
}

// ============================================================================
// GraphDB Handle
// ============================================================================

export interface GraphDB {
  readonly path: string;
  readonly readOnly: boolean;
  readonly _isSingleFile: boolean;

  // Multi-file specific (null for single-file)
  _manifest: ManifestV1 | null;
  _snapshot: SnapshotData | null;
  _walFd: number | null;
  _walOffset: number;

  // Single-file specific (null for multi-file)
  _header: DbHeaderV1 | null;
  _pager: Pager | null;
  _snapshotMmap: Uint8Array | null;
  _snapshotCache: SnapshotData | null;  // Cached parsed snapshot for single-file
  _walWritePos: number;

  // Shared fields
  _delta: DeltaState;
  _nextNodeId: number;
  _nextLabelId: number;
  _nextEtypeId: number;
  _nextPropkeyId: number;
  _nextTxId: bigint;
  _currentTx: TxState | null;
  _lockFd: unknown; // LockHandle from util/lock.ts or null
  _cache?: unknown; // CacheManager instance (opaque to users)
  _mvcc?: unknown; // MVCC manager instance (opaque to users)
  _mvccEnabled?: boolean; // Cached MVCC enabled flag for fast checks

  // Vector embeddings storage (keyed by propKeyId for the vector property)
  _vectorStores?: Map<PropKeyID, unknown>; // Map<PropKeyID, VectorManifest>
  _vectorIndexes?: Map<PropKeyID, unknown>; // Map<PropKeyID, IvfIndex>

  // Single-file options
  _autoCheckpoint?: boolean;
  _checkpointThreshold?: number;
  _cacheSnapshot?: boolean;
  
  // Background checkpoint state
  _checkpointState?: CheckpointState;
  
  // Checkpoint merge lock - prevents concurrent commits during WAL merge
  // When true, commits must wait for merge to complete
  _checkpointMergeLock?: boolean;
}

export interface TxHandle {
  _db: GraphDB;
  _tx: TxState;
}

// ============================================================================
// Node creation options
// ============================================================================

export interface NodeOpts {
  key?: string;
  labels?: LabelID[];
  props?: Map<PropKeyID, PropValue>;
}

// ============================================================================
// Single-File Database Format Types
// ============================================================================

/**
 * Database header for single-file format (4KB page)
 * Based on spec from docs/SINGLE_FILE_MIGRATION.md
 * 
 * Layout (V2 with dual-buffer WAL support):
 * Offset  Size    Description
 * ──────────────────────────────────────────────────
 * 0       16      Magic: "RayDB format 1\0"
 * 16      4       Page size (power of 2, 4KB-64KB)
 * 20      4       Format version (1 or 2)
 * 24      4       Min reader version (1)
 * 28      4       Flags (WAL mode, compression, etc.)
 * 32      8       Change counter (incremented on commit)
 * 40      8       Database size in pages
 * 48      8       Snapshot start page
 * 56      8       Snapshot page count
 * 64      8       WAL start page
 * 72      8       WAL page count  
 * 80      8       WAL head offset (circular buffer head)
 * 88      8       WAL tail offset (circular buffer tail)
 * 96      8       Active snapshot generation
 * 104     8       Previous snapshot generation
 * 112     8       Max node ID
 * 120     8       Next TX ID
 * 128     8       Last commit timestamp
 * 136     8       Schema cookie (for cache invalidation)
 * --- V2 fields for dual-buffer WAL ---
 * 144     8       WAL primary head (primary region write position)
 * 152     8       WAL secondary head (secondary region write position)
 * 160     1       Active WAL region (0=primary, 1=secondary)
 * 161     1       Checkpoint in progress flag (for crash recovery)
 * 162     14      Reserved for expansion
 * 176     4       Header checksum (CRC32C)
 * 180     ...     Padding to page boundary
 * 4092    4       Footer checksum of first 4088 bytes
 */
export interface DbHeaderV1 {
  /** Magic bytes (16 bytes) - "RayDB format 1\0" */
  magic: Uint8Array;
  /** Page size (power of 2, 4KB-64KB) */
  pageSize: number;
  /** Format version */
  version: number;
  /** Minimum reader version required */
  minReaderVersion: number;
  /** Flags (WAL mode, compression, etc.) */
  flags: number;
  /** Change counter (incremented on each commit) */
  changeCounter: bigint;
  /** Total database size in pages */
  dbSizePages: bigint;
  /** Snapshot area start page */
  snapshotStartPage: bigint;
  /** Snapshot area page count */
  snapshotPageCount: bigint;
  /** WAL area start page */
  walStartPage: bigint;
  /** WAL area page count */
  walPageCount: bigint;
  /** WAL circular buffer head offset (bytes from WAL start) */
  walHead: bigint;
  /** WAL circular buffer tail offset (bytes from WAL start) */
  walTail: bigint;
  /** Active snapshot generation */
  activeSnapshotGen: bigint;
  /** Previous snapshot generation (for rollback) */
  prevSnapshotGen: bigint;
  /** Maximum node ID allocated */
  maxNodeId: bigint;
  /** Next transaction ID */
  nextTxId: bigint;
  /** Last commit timestamp (unix ms) */
  lastCommitTs: bigint;
  /** Schema cookie for cache invalidation */
  schemaCookie: bigint;
  /** WAL primary region head (bytes from WAL start) - V2 */
  walPrimaryHead: bigint;
  /** WAL secondary region head (bytes from WAL start) - V2 */
  walSecondaryHead: bigint;
  /** Active WAL region (0=primary, 1=secondary) - V2 */
  activeWalRegion: 0 | 1;
  /** Checkpoint in progress flag (for crash recovery) - V2 */
  checkpointInProgress: 0 | 1;
  /** Reserved for future expansion */
  reserved: Uint8Array;
  /** Header checksum (CRC32C of bytes 0-175) */
  headerChecksum: number;
  /** Footer checksum (CRC32C of bytes 0-4087) */
  footerChecksum: number;
}

/** Size of fixed header fields before reserved area (in bytes) */
export const DB_HEADER_FIXED_SIZE = 176;

/** Size of reserved area in header (in bytes) - reduced for V2 fields */
export const DB_HEADER_RESERVED_SIZE = 14;

/** Size of V2 fields (walPrimaryHead + walSecondaryHead + activeWalRegion + checkpointInProgress) */
export const DB_HEADER_V2_FIELDS_SIZE = 8 + 8 + 1 + 1; // 18 bytes

/**
 * Checkpoint state for background checkpointing
 */
export type CheckpointState = 
  | { status: 'idle' }
  | { status: 'running'; promise: Promise<void> }
  | { status: 'completing' }
  | { status: 'merging' };

/**
 * WAL buffer full error - thrown when circular buffer is exhausted
 */
export class WalBufferFullError extends Error {
  constructor() {
    super('WAL buffer full: checkpoint required before continuing writes');
    this.name = 'WalBufferFullError';
  }
}

/**
 * Options for opening a single-file database
 * @deprecated Use OpenOptions directly - single-file options are now included
 */
export type SingleFileOpenOptions = OpenOptions;

/**
 * Pager interface for page-based I/O operations
 */
export interface Pager {
  /** File descriptor */
  readonly fd: number;
  /** Page size in bytes */
  readonly pageSize: number;
  /** Total file size in bytes */
  readonly fileSize: number;

  /** Read a single page by page number */
  readPage(pageNum: number): Uint8Array;
  
  /** Write a single page by page number */
  writePage(pageNum: number, data: Uint8Array): void;
  
  /** Memory-map a range of pages (for snapshot access) */
  mmapRange(startPage: number, pageCount: number): Uint8Array;
  
  /** Allocate new pages at end of file */
  allocatePages(count: number): number;
  
  /** Mark pages as free (for vacuum) */
  freePages(startPage: number, count: number): void;
  
  /** Sync file to disk */
  sync(): Promise<void>;
  
  /** Relocate an area to a new location (for growth) */
  relocateArea(srcPage: number, pageCount: number, dstPage: number): Promise<void>;
  
  /** Close the pager */
  close(): void;
}

/**
 * SingleFileDB is now an alias for GraphDB with _isSingleFile: true
 * @deprecated Use GraphDB directly
 */
export type SingleFileDB = GraphDB;
