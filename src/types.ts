/**
 * Embedded Graph DB Type Definitions
 * Based on spec v1.1 (Mode B)
 */

// ============================================================================
// Public (stable) IDs - never reused in v1
// ============================================================================

/** Monotonic node ID, never reused. u64 */
export type NodeID = bigint;

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
}

export type PropValue =
  | { tag: PropValueTag.NULL }
  | { tag: PropValueTag.BOOL; value: boolean }
  | { tag: PropValueTag.I64; value: bigint }
  | { tag: PropValueTag.F64; value: number }
  | { tag: PropValueTag.STRING; value: string };

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
  labels: Set<LabelID>;
  labelsDeleted: Set<LabelID>;
  props: Map<PropKeyID, PropValue | null>; // null = deleted
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
}

// ============================================================================
// Database options and state
// ============================================================================

export interface OpenOptions {
  readOnly?: boolean;
  createIfMissing?: boolean;
  lockFile?: boolean;
  cache?: CacheOptions;
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
  snapshotMaxNodeId: bigint;
  deltaNodesCreated: bigint;
  deltaNodesDeleted: bigint;
  deltaEdgesAdded: bigint;
  deltaEdgesDeleted: bigint;
  walSegment: bigint;
  walBytes: bigint;
  recommendCompact: boolean;
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

  // Internal state (opaque to users)
  _manifest: ManifestV1;
  _snapshot: SnapshotData | null;
  _delta: DeltaState;
  _walFd: number | null;
  _walOffset: number;
  _nextNodeId: bigint;
  _nextLabelId: number;
  _nextEtypeId: number;
  _nextPropkeyId: number;
  _nextTxId: bigint;
  _currentTx: TxState | null;
  _lockFd: unknown; // LockHandle from util/lock.ts or null
  _cache?: unknown; // CacheManager instance (opaque to users)
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
