/**
 * Vector-specific type definitions for RayDB embeddings support
 *
 * Inspired by LanceDB's columnar architecture for efficient vector operations.
 */

import type { NodeID, PropKeyID } from "../types.ts";

// ============================================================================
// Configuration
// ============================================================================

/**
 * Vector store configuration
 */
export interface VectorStoreConfig {
  /** Vector dimensions (e.g., 768) */
  dimensions: number;
  /** Distance metric */
  metric: "cosine" | "euclidean" | "dot";
  /** Vectors per row group (default: 1024) */
  rowGroupSize: number;
  /** Vectors per fragment before sealing (default: 100_000) */
  fragmentTargetSize: number;
  /** Whether to auto-normalize vectors (default: true for cosine) */
  normalize: boolean;
}

/**
 * Default configuration optimized for code embeddings
 */
export const DEFAULT_VECTOR_CONFIG: Omit<VectorStoreConfig, "dimensions"> = {
  metric: "cosine",
  rowGroupSize: 1024,
  fragmentTargetSize: 100_000,
  normalize: true,
};

/**
 * IVF index configuration
 */
export interface IvfConfig {
  /** Number of clusters/centroids (default: sqrt(n) or 256) */
  nClusters: number;
  /** Number of clusters to probe during search (default: 10) */
  nProbe: number;
  /** Distance metric for clustering and search (default: cosine) */
  metric: "cosine" | "euclidean" | "dot";
  /** Use Product Quantization for compression (default: false) */
  usePQ: boolean;
  /** PQ subvector count (default: dimensions / 8) */
  pqSubvectors?: number;
  /** PQ bits per subvector (default: 8) */
  pqBits?: number;
}

/**
 * Default IVF configuration
 */
export const DEFAULT_IVF_CONFIG: IvfConfig = {
  nClusters: 256,
  nProbe: 10,
  metric: "cosine",
  usePQ: false,
};

// ============================================================================
// Columnar Storage Structures
// ============================================================================

/**
 * A row group - a batch of vectors stored contiguously
 *
 * Memory layout (for dimensions=768, rowGroupSize=1024):
 * Float32Array of length 768 * 1024 = 786,432
 *
 * Vector i, dimension d is at index: i * dimensions + d
 */
export interface RowGroup {
  /** Row group ID within fragment */
  id: number;
  /** Number of vectors in this row group (may be < rowGroupSize if last group) */
  count: number;
  /** Contiguous vector data: [v0_d0, v0_d1, ..., v0_dN, v1_d0, ...] */
  data: Float32Array;
}

/**
 * A fragment - an immutable collection of row groups
 */
export interface Fragment {
  /** Fragment ID */
  id: number;
  /** Fragment state */
  state: "active" | "sealed";
  /** Row groups in this fragment */
  rowGroups: RowGroup[];
  /** Total vectors in fragment (including deleted) */
  totalVectors: number;
  /** Deletion bitmap (bit i = 1 means vector i is deleted) */
  deletionBitmap: Uint32Array;
  /** Number of deleted vectors */
  deletedCount: number;
  /** Byte offset in snapshot file (for memory mapping) */
  fileOffset?: number;
  /** Byte length in snapshot file */
  fileLength?: number;
}

/**
 * Manifest - tracks all fragments and global state
 */
export interface VectorManifest {
  /** Configuration */
  config: VectorStoreConfig;
  /** All fragments */
  fragments: Fragment[];
  /** ID of the active fragment (accepting inserts) */
  activeFragmentId: number;
  /** Total vectors across all fragments */
  totalVectors: number;
  /** Total deleted vectors */
  totalDeleted: number;
  /** Next vector ID to assign */
  nextVectorId: number;
  /** NodeID -> global vector ID mapping */
  nodeIdToVectorId: Map<NodeID, number>;
  /** Global vector ID -> NodeID mapping (reverse lookup) */
  vectorIdToNodeId: Map<number, NodeID>;
  /** Global vector ID -> (fragmentId, localIndex) mapping */
  vectorIdToLocation: Map<number, { fragmentId: number; localIndex: number }>;
}

// ============================================================================
// IVF Index Structures
// ============================================================================

/**
 * IVF (Inverted File) index for approximate nearest neighbor search
 *
 * Unlike HNSW which builds a navigable graph, IVF:
 * 1. Clusters vectors into K centroids using k-means
 * 2. Assigns each vector to its nearest centroid
 * 3. At search time, probes top-N nearest centroids
 *
 * Advantages over HNSW:
 * - Better for disk-based storage (locality of access)
 * - Easier to update (just add to cluster)
 * - Works well with columnar storage
 * - More predictable memory usage
 */
export interface IvfIndex {
  /** Index configuration */
  config: IvfConfig;
  /** Cluster centroids: Float32Array of length nClusters * dimensions */
  centroids: Float32Array;
  /** Inverted lists: cluster ID -> array of vector IDs */
  invertedLists: Map<number, number[]>;
  /** Whether index has been trained */
  trained: boolean;
  /** Vectors used for training (cleared after training) */
  trainingVectors?: Float32Array;
  /** Number of training vectors collected */
  trainingCount?: number;
}

/**
 * Search result
 */
export interface VectorSearchResult {
  /** Global vector ID */
  vectorId: number;
  /** Graph node ID */
  nodeId: NodeID;
  /** Distance to query (lower is better) */
  distance: number;
  /** Similarity score (higher is better, 0-1 for cosine) */
  similarity: number;
}

// ============================================================================
// Secondary Index Types
// ============================================================================

/**
 * Supported secondary index types
 */
export type SecondaryIndexType = "btree" | "hash";

/**
 * Secondary index configuration
 */
export interface SecondaryIndexConfig {
  /** Property key ID to index */
  propKeyId: PropKeyID;
  /** Index type */
  type: SecondaryIndexType;
  /** Property value type */
  valueType: "string" | "int" | "float";
}

/**
 * B-tree node for secondary index
 */
export interface BTreeNode<K, V> {
  /** Is this a leaf node? */
  isLeaf: boolean;
  /** Keys in this node */
  keys: K[];
  /** Values (for leaf) or child pointers (for internal) */
  values: V[];
  /** Child node pointers (for internal nodes) */
  children?: BTreeNode<K, V>[];
}

/**
 * Secondary index structure
 */
export interface SecondaryIndex {
  /** Index configuration */
  config: SecondaryIndexConfig;
  /** B-tree root */
  root: BTreeNode<string | number | bigint, Set<NodeID>> | null;
  /** Total entries */
  size: number;
}

// ============================================================================
// Filter Types
// ============================================================================

/**
 * Fast-path filter operators
 */
export interface FilterOperators {
  $eq?: string | number | bigint | boolean;
  $ne?: string | number | bigint | boolean;
  $gt?: number | bigint;
  $gte?: number | bigint;
  $lt?: number | bigint;
  $lte?: number | bigint;
  $in?: Array<string | number | bigint>;
  $nin?: Array<string | number | bigint>;
  $startsWith?: string;
  $endsWith?: string;
  $contains?: string;
}

/**
 * Fast filter specification (uses indexes)
 */
export type FastFilter = {
  [propName: string]: FilterOperators | string | number | bigint | boolean;
};

/**
 * Compiled filter for execution
 */
export interface CompiledFilter {
  /** Filter uses secondary index? */
  usesIndex: boolean;
  /** Property key ID */
  propKeyId: PropKeyID;
  /** Pre-computed node set (if using index) */
  nodeSet?: Set<NodeID>;
  /** Predicate function (if not using index) */
  predicate?: (value: unknown) => boolean;
}

// ============================================================================
// Search Builder Types
// ============================================================================

/**
 * Vector search options
 */
export interface VectorSearchOptions {
  /** Number of results to return */
  k: number;
  /** Minimum similarity threshold (0-1 for cosine) */
  threshold?: number;
  /** Number of clusters to probe (IVF) */
  nProbe?: number;
  /** Fast filters (use indexes) */
  fastFilters?: FastFilter[];
  /** Slow filter predicate */
  slowFilter?: (node: unknown) => boolean;
}

/**
 * Multi-vector search options
 */
export interface MultiVectorSearchOptions extends VectorSearchOptions {
  /** How to aggregate scores from multiple queries */
  aggregation: "min" | "max" | "avg" | "sum";
}

/**
 * Batch insert options
 */
export interface BatchInsertOptions {
  /** What to do on key conflict */
  onConflict: "skip" | "replace" | "error";
  /** Progress callback */
  onProgress?: (inserted: number, total: number) => void;
}

// ============================================================================
// Snapshot Sections
// ============================================================================

/**
 * Vector manifest header in snapshot
 */
export interface VectorManifestHeader {
  /** Number of fragments */
  numFragments: number;
  /** Total vectors */
  totalVectors: number;
  /** Total deleted */
  totalDeleted: number;
  /** Active fragment ID */
  activeFragmentId: number;
  /** Dimensions */
  dimensions: number;
  /** Metric (0=cosine, 1=euclidean, 2=dot) */
  metric: number;
  /** Row group size */
  rowGroupSize: number;
}

/**
 * Fragment header in snapshot
 */
export interface FragmentHeader {
  /** Fragment ID */
  id: number;
  /** State (0=active, 1=sealed) */
  state: number;
  /** Number of row groups */
  numRowGroups: number;
  /** Total vectors */
  totalVectors: number;
  /** Deleted count */
  deletedCount: number;
  /** Deletion bitmap byte length */
  deletionBitmapLength: number;
}

/**
 * IVF index header in snapshot
 */
export interface IvfIndexHeader {
  /** Number of clusters */
  nClusters: number;
  /** Dimensions */
  dimensions: number;
  /** Whether trained */
  trained: number;
  /** Use PQ */
  usePQ: number;
}

// ============================================================================
// WAL Record Payloads
// ============================================================================

/**
 * SET_NODE_VECTOR WAL payload
 */
export interface SetNodeVectorPayload {
  nodeId: NodeID;
  propKeyId: PropKeyID;
  dimensions: number;
  vector: Float32Array;
}

/**
 * DEL_NODE_VECTOR WAL payload
 */
export interface DelNodeVectorPayload {
  nodeId: NodeID;
  propKeyId: PropKeyID;
}

/**
 * BATCH_VECTORS WAL payload
 */
export interface BatchVectorsPayload {
  propKeyId: PropKeyID;
  dimensions: number;
  entries: Array<{
    nodeId: NodeID;
    vector: Float32Array;
  }>;
}

/**
 * SEAL_FRAGMENT WAL payload
 */
export interface SealFragmentPayload {
  fragmentId: number;
  newFragmentId: number;
}

/**
 * COMPACT_FRAGMENTS WAL payload
 */
export interface CompactFragmentsPayload {
  sourceFragmentIds: number[];
  targetFragmentId: number;
}

// ============================================================================
// Delta State Extensions
// ============================================================================

/**
 * Vector-related delta state
 */
export interface VectorDeltaState {
  /** Manifest with pending changes */
  manifest: VectorManifest;
  /** IVF index */
  index: IvfIndex | null;
  /** Pending inserts not yet in a row group */
  pendingInserts: Array<{ nodeId: NodeID; vector: Float32Array }>;
  /** Pending deletes */
  pendingDeletes: Set<NodeID>;
}
