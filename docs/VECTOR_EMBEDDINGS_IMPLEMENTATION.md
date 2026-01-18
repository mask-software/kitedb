# Vector Embeddings Implementation Plan

## Overview

This document provides a complete implementation plan for adding native vector embeddings support to RayDB, optimized for RAG/LLM integration on codebases.

### Key Design Goals

1. **Zero impact on existing graph operations** - Vector subsystem is isolated
2. **Lance-style columnar storage** - Inspired by LanceDB's architecture for efficient vector operations
3. **768-dimension Float32 vectors** - Standard for code embedding models
4. **Auto-normalization** - Enables fast dot-product similarity
5. **Incremental updates** - Append-only fragments with deletion tombstones
6. **Hybrid pre-filtering** - Fast path for indexed filters, fallback for predicates
7. **Secondary indexes** - B-tree indexes on scalar properties for fast filtering
8. **Multi-vector search** - Search with multiple query vectors
9. **Batch insert API** - Efficient bulk loading for initial indexing

---

## Why Lance-Style Columnar Format?

The original plan used HNSW with row-oriented storage (`Map<number, Float32Array>`). We're switching to a **Lance-inspired columnar architecture** for these benefits:

### Advantages over Row-Oriented Storage

| Aspect | Row-Oriented (Original) | Columnar (Lance-style) |
|--------|------------------------|------------------------|
| **Memory Layout** | Scattered allocations | Contiguous Float32Array |
| **Cache Performance** | Poor - random access | Excellent - sequential access |
| **SIMD Potential** | Limited | Full - process 4-8 dims at once |
| **Compression** | Per-vector overhead | Block compression possible |
| **Disk I/O** | Many small reads | Large sequential reads |
| **Memory Mapping** | Difficult | Natural fit |
| **Batch Operations** | O(n) allocations | Single allocation |

### Lance Format Concepts We Adopt

1. **Fragments** - Immutable chunks of vectors (append-only)
2. **Row Groups** - Vectors grouped for efficient batch processing (e.g., 1024 vectors)
3. **Columnar Layout** - All vectors in a fragment stored contiguously
4. **Deletion Tombstones** - Track deleted vectors without rewriting data
5. **Manifest** - Metadata tracking all fragments and their state

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              GraphDB Handle                                  │
├─────────────────────────────────────────────────────────────────────────────┤
│  ┌──────────────────────────────────┐  ┌──────────────────────────────────┐ │
│  │      Existing Graph Layer        │  │         Vector Layer             │ │
│  │  ┌──────────┐  ┌──────────────┐  │  │  ┌────────────────────────────┐  │ │
│  │  │ Topology │  │  Properties  │  │  │  │      Vector Index          │  │ │
│  │  │  (CSR)   │  │  (scalars)   │  │  │  │  (IVF-Flat or IVF-PQ)      │  │ │
│  │  └──────────┘  └──────────────┘  │  │  └────────────────────────────┘  │ │
│  │  ┌──────────────────────────────┐│  │  ┌────────────────────────────┐  │ │
│  │  │     Secondary Indexes        ││  │  │   Columnar Vector Store    │  │ │
│  │  │  (B-tree on indexed props)   ││  │  │  ┌──────────────────────┐  │  │ │
│  │  └──────────────────────────────┘│  │  │  │ Fragment 0 (sealed)  │  │  │ │
│  └──────────────────────────────────┘  │  │  │  - RowGroup 0..N     │  │  │ │
│                                        │  │  │  - Deletion bitmap   │  │  │ │
│                                        │  │  ├──────────────────────┤  │  │ │
│                                        │  │  │ Fragment 1 (sealed)  │  │  │ │
│                                        │  │  ├──────────────────────┤  │  │ │
│                                        │  │  │ Fragment 2 (active)  │  │  │ │
│                                        │  │  │  - Append buffer     │  │  │ │
│                                        │  │  └──────────────────────┘  │  │ │
│                                        │  └────────────────────────────┘  │ │
│                                        └──────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Core Concepts

#### Fragments
- **Immutable** once sealed (append-only design)
- Contains one or more **row groups** of vectors
- Has a **deletion bitmap** for soft deletes
- Can be **compacted** to remove deleted vectors

#### Row Groups
- Fixed-size batches of vectors (default: 1024)
- Stored contiguously in a single `Float32Array`
- Layout: `[v0_d0, v0_d1, ..., v0_d767, v1_d0, v1_d1, ..., v1023_d767]`
- Enables efficient SIMD distance calculations

#### Active Fragment
- The one fragment that accepts new inserts
- Sealed when it reaches target size (e.g., 100K vectors)
- New fragment created after sealing

#### Manifest
- Tracks all fragments and their metadata
- Points to current active fragment
- Stores global vector count, deletion count

---

## File Structure

### New Files to Create

```
src/vector/
├── index.ts                 # Module exports
├── types.ts                 # Vector-specific type definitions
├── distance.ts              # Distance functions (SIMD-optimized)
├── normalize.ts             # Vector normalization utilities
├── columnar-store.ts        # Lance-style columnar vector storage
├── fragment.ts              # Fragment management
├── row-group.ts             # Row group operations
├── manifest.ts              # Manifest tracking
├── ivf-index.ts             # IVF-Flat index implementation
├── ivf-pq.ts                # IVF-PQ index (optional, for large scale)
├── ivf-serialize.ts         # Binary serialization for IVF index
├── secondary-index.ts       # B-tree secondary indexes for fast filtering
├── compaction.ts            # Fragment compaction
└── batch.ts                 # Batch insert operations

src/api/
├── vector-search.ts         # VectorSearchBuilder fluent API (NEW)
└── ... (existing files to modify)
```

### Files to Modify

```
src/types.ts                 # Add VECTOR_F32 tag, new sections, WAL records
src/api/schema.ts            # Add prop.vector() builder
src/api/ray.ts               # Add .similar() method
src/core/delta.ts            # Add vector index to delta state
src/core/wal.ts              # Add vector serialization
src/core/wal-buffer.ts       # Handle vector WAL records
src/core/snapshot-writer.ts  # Write vector sections
src/core/snapshot-reader.ts  # Read vector sections
src/ray/graph-db/nodes.ts    # Handle vector property CRUD
src/ray/graph-db/lifecycle.ts # Initialize vector index
src/ray/graph-db/tx.ts       # Vector transaction handling
src/index.ts                 # Export vector types
```

---

## Phase 1: Type System Foundation

### 1.1 Core Types (`src/types.ts`)

Add to `PropValueTag` enum:
```typescript
export enum PropValueTag {
  NULL = 0,
  BOOL = 1,
  I64 = 2,
  F64 = 3,
  STRING = 4,
  VECTOR_F32 = 5,  // NEW: Normalized float32 vector
}
```

Add to `PropValue` union:
```typescript
export type PropValue =
  | { tag: PropValueTag.NULL }
  | { tag: PropValueTag.BOOL; value: boolean }
  | { tag: PropValueTag.I64; value: bigint }
  | { tag: PropValueTag.F64; value: number }
  | { tag: PropValueTag.STRING; value: string }
  | { tag: PropValueTag.VECTOR_F32; value: Float32Array };  // NEW
```

Add new snapshot sections:
```typescript
export enum SectionId {
  // ... existing 0-22
  VECTOR_MANIFEST = 23,       // Vector store manifest
  VECTOR_FRAGMENT = 24,       // Vector fragment data
  VECTOR_INDEX = 25,          // Serialized IVF index
  VECTOR_NODE_MAP = 26,       // NodeID -> VectorID mapping
  SECONDARY_INDEX_META = 27,  // Secondary index metadata
  SECONDARY_INDEX_DATA = 28,  // Secondary index B-tree data
  _COUNT = 29,
}
```

Add new WAL record types:
```typescript
export enum WalRecordType {
  // ... existing
  SET_NODE_VECTOR = 60,     // Set vector on node
  DEL_NODE_VECTOR = 61,     // Delete vector from node
  BATCH_VECTORS = 62,       // Batch vector insert
  SEAL_FRAGMENT = 63,       // Seal active fragment
  COMPACT_FRAGMENTS = 64,   // Compact fragments
}
```

### 1.2 Vector Types (`src/vector/types.ts`)

```typescript
/**
 * Vector-specific type definitions for RayDB embeddings support
 * 
 * Inspired by LanceDB's columnar architecture
 */

import type { NodeID, PropKeyID } from '../types.ts';

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
  metric: 'cosine' | 'euclidean' | 'dot';
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
export const DEFAULT_VECTOR_CONFIG: Omit<VectorStoreConfig, 'dimensions'> = {
  metric: 'cosine',
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
  state: 'active' | 'sealed';
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

// ============================================================================
// Secondary Index Types (unchanged)
// ============================================================================

/**
 * Supported secondary index types
 */
export type SecondaryIndexType = 'btree' | 'hash';

/**
 * Secondary index configuration
 */
export interface SecondaryIndexConfig {
  /** Property key ID to index */
  propKeyId: PropKeyID;
  /** Index type */
  type: SecondaryIndexType;
  /** Property value type */
  valueType: 'string' | 'int' | 'float';
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
// Filter Types (unchanged)
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
// Search Builder Types (unchanged)
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
  aggregation: 'min' | 'max' | 'avg' | 'sum';
}

/**
 * Batch insert options
 */
export interface BatchInsertOptions {
  /** What to do on key conflict */
  onConflict: 'skip' | 'replace' | 'error';
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
```

### 1.3 Schema API Extensions (`src/api/schema.ts`)

```typescript
// (Same as original - no changes needed)
export type PropType = "string" | "int" | "float" | "bool" | "vector";

export interface VectorPropOptions {
  normalize?: boolean;
}

export interface VectorPropDef<Dims extends number = number> {
  readonly _tag: "prop";
  readonly name: string;
  readonly type: "vector";
  readonly dimensions: Dims;
  readonly normalize: boolean;
  readonly optional: boolean;
}

export const prop = {
  // ... existing string, int, float, bool
  
  vector: <D extends number>(
    name: string,
    dimensions: D,
    options?: VectorPropOptions
  ): VectorPropBuilder<D> => createVectorPropBuilder(name, dimensions, options),
};
```

---

## Phase 2: Distance Functions & Normalization

### 2.1 Normalization (`src/vector/normalize.ts`)

```typescript
/**
 * Vector normalization utilities
 * 
 * Normalizing vectors enables fast cosine similarity via dot product.
 */

/**
 * Compute L2 norm of a vector
 */
export function l2Norm(v: Float32Array): number {
  let sum = 0;
  for (let i = 0; i < v.length; i++) {
    sum += v[i] * v[i];
  }
  return Math.sqrt(sum);
}

/**
 * Normalize a vector in-place
 * @returns The norm before normalization
 */
export function normalizeInPlace(v: Float32Array): number {
  const norm = l2Norm(v);
  if (norm > 0) {
    const invNorm = 1 / norm;
    for (let i = 0; i < v.length; i++) {
      v[i] *= invNorm;
    }
  }
  return norm;
}

/**
 * Normalize a vector, returning a new array
 */
export function normalize(v: Float32Array): Float32Array {
  const result = new Float32Array(v);
  normalizeInPlace(result);
  return result;
}

/**
 * Check if a vector is normalized (L2 norm ≈ 1)
 */
export function isNormalized(v: Float32Array, tolerance = 1e-6): boolean {
  const norm = l2Norm(v);
  return Math.abs(norm - 1) < tolerance;
}

/**
 * Normalize multiple vectors in a contiguous array (row group)
 * More efficient than normalizing individually due to cache locality
 */
export function normalizeRowGroup(
  data: Float32Array,
  dimensions: number,
  count: number
): void {
  for (let i = 0; i < count; i++) {
    const offset = i * dimensions;
    let sum = 0;
    
    // Compute norm
    for (let d = 0; d < dimensions; d++) {
      const val = data[offset + d];
      sum += val * val;
    }
    
    // Normalize
    if (sum > 0) {
      const invNorm = 1 / Math.sqrt(sum);
      for (let d = 0; d < dimensions; d++) {
        data[offset + d] *= invNorm;
      }
    }
  }
}
```

### 2.2 Distance Functions (`src/vector/distance.ts`)

```typescript
/**
 * Distance and similarity functions for vector search
 * 
 * Optimized for columnar storage with SIMD-friendly patterns.
 * For normalized vectors, cosine similarity = dot product.
 */

/**
 * Dot product of two vectors
 * For normalized vectors: dot(a,b) = cos(θ) = cosine similarity
 */
export function dotProduct(a: Float32Array, b: Float32Array): number {
  let sum = 0;
  const len = a.length;
  
  // Unroll loop for better performance (helps JIT generate SIMD)
  const remainder = len % 8;
  const mainLen = len - remainder;
  
  for (let i = 0; i < mainLen; i += 8) {
    sum += a[i] * b[i] 
         + a[i + 1] * b[i + 1] 
         + a[i + 2] * b[i + 2] 
         + a[i + 3] * b[i + 3]
         + a[i + 4] * b[i + 4]
         + a[i + 5] * b[i + 5]
         + a[i + 6] * b[i + 6]
         + a[i + 7] * b[i + 7];
  }
  
  for (let i = mainLen; i < len; i++) {
    sum += a[i] * b[i];
  }
  
  return sum;
}

/**
 * Compute distances from query to multiple vectors in a row group
 * 
 * This is the hot path - optimized for columnar access pattern.
 * Returns array of distances for vectors [startIdx, startIdx + count)
 */
export function batchDotProduct(
  query: Float32Array,
  rowGroupData: Float32Array,
  dimensions: number,
  startIdx: number,
  count: number
): Float32Array {
  const distances = new Float32Array(count);
  
  for (let i = 0; i < count; i++) {
    const offset = (startIdx + i) * dimensions;
    let sum = 0;
    
    // Unrolled for SIMD
    const remainder = dimensions % 8;
    const mainLen = dimensions - remainder;
    
    for (let d = 0; d < mainLen; d += 8) {
      sum += query[d] * rowGroupData[offset + d]
           + query[d + 1] * rowGroupData[offset + d + 1]
           + query[d + 2] * rowGroupData[offset + d + 2]
           + query[d + 3] * rowGroupData[offset + d + 3]
           + query[d + 4] * rowGroupData[offset + d + 4]
           + query[d + 5] * rowGroupData[offset + d + 5]
           + query[d + 6] * rowGroupData[offset + d + 6]
           + query[d + 7] * rowGroupData[offset + d + 7];
    }
    
    for (let d = mainLen; d < dimensions; d++) {
      sum += query[d] * rowGroupData[offset + d];
    }
    
    // For cosine distance: 1 - dot_product (since vectors are normalized)
    distances[i] = 1 - sum;
  }
  
  return distances;
}

/**
 * Squared Euclidean distance (faster than euclidean, preserves ordering)
 */
export function squaredEuclidean(a: Float32Array, b: Float32Array): number {
  let sum = 0;
  for (let i = 0; i < a.length; i++) {
    const diff = a[i] - b[i];
    sum += diff * diff;
  }
  return sum;
}

/**
 * Batch squared Euclidean distance
 */
export function batchSquaredEuclidean(
  query: Float32Array,
  rowGroupData: Float32Array,
  dimensions: number,
  startIdx: number,
  count: number
): Float32Array {
  const distances = new Float32Array(count);
  
  for (let i = 0; i < count; i++) {
    const offset = (startIdx + i) * dimensions;
    let sum = 0;
    
    for (let d = 0; d < dimensions; d++) {
      const diff = query[d] - rowGroupData[offset + d];
      sum += diff * diff;
    }
    
    distances[i] = sum;
  }
  
  return distances;
}

/**
 * Cosine distance (1 - cosine similarity)
 */
export function cosineDistance(a: Float32Array, b: Float32Array): number {
  return 1 - dotProduct(a, b);
}

/**
 * Get batch distance function by metric name
 */
export function getBatchDistanceFunction(
  metric: 'cosine' | 'euclidean' | 'dot'
): (
  query: Float32Array,
  rowGroupData: Float32Array,
  dimensions: number,
  startIdx: number,
  count: number
) => Float32Array {
  switch (metric) {
    case 'cosine':
    case 'dot':
      return batchDotProduct;
    case 'euclidean':
      return batchSquaredEuclidean;
  }
}

/**
 * Convert distance to similarity (0-1 scale)
 */
export function distanceToSimilarity(
  distance: number,
  metric: 'cosine' | 'euclidean' | 'dot'
): number {
  switch (metric) {
    case 'cosine':
      return 1 - distance;
    case 'euclidean':
      return 1 / (1 + Math.sqrt(distance));
    case 'dot':
      return 1 - distance;
  }
}
```

---

## Phase 3: Columnar Storage Implementation

### 3.1 Row Group Operations (`src/vector/row-group.ts`)

```typescript
/**
 * Row group operations for columnar vector storage
 */

import type { RowGroup } from './types.ts';
import { normalizeRowGroup } from './normalize.ts';

/**
 * Create a new row group
 */
export function createRowGroup(
  id: number,
  dimensions: number,
  capacity: number
): RowGroup {
  return {
    id,
    count: 0,
    data: new Float32Array(capacity * dimensions),
  };
}

/**
 * Append a vector to a row group
 * @returns The local index within the row group
 */
export function rowGroupAppend(
  rowGroup: RowGroup,
  vector: Float32Array,
  dimensions: number,
  shouldNormalize: boolean
): number {
  const localIdx = rowGroup.count;
  const offset = localIdx * dimensions;
  
  // Copy vector data
  rowGroup.data.set(vector, offset);
  
  // Normalize if needed
  if (shouldNormalize) {
    let sum = 0;
    for (let d = 0; d < dimensions; d++) {
      const val = rowGroup.data[offset + d];
      sum += val * val;
    }
    if (sum > 0) {
      const invNorm = 1 / Math.sqrt(sum);
      for (let d = 0; d < dimensions; d++) {
        rowGroup.data[offset + d] *= invNorm;
      }
    }
  }
  
  rowGroup.count++;
  return localIdx;
}

/**
 * Get a vector from a row group (returns a view, not a copy)
 */
export function rowGroupGet(
  rowGroup: RowGroup,
  localIdx: number,
  dimensions: number
): Float32Array {
  const offset = localIdx * dimensions;
  return rowGroup.data.subarray(offset, offset + dimensions);
}

/**
 * Check if row group is full
 */
export function rowGroupIsFull(
  rowGroup: RowGroup,
  dimensions: number,
  capacity: number
): boolean {
  return rowGroup.count >= capacity;
}
```

### 3.2 Fragment Operations (`src/vector/fragment.ts`)

```typescript
/**
 * Fragment operations for columnar vector storage
 */

import type { Fragment, RowGroup, VectorStoreConfig } from './types.ts';
import { createRowGroup, rowGroupAppend, rowGroupIsFull } from './row-group.ts';

/**
 * Create a new fragment
 */
export function createFragment(
  id: number,
  config: VectorStoreConfig
): Fragment {
  return {
    id,
    state: 'active',
    rowGroups: [createRowGroup(0, config.dimensions, config.rowGroupSize)],
    totalVectors: 0,
    deletionBitmap: new Uint32Array(Math.ceil(config.fragmentTargetSize / 32)),
    deletedCount: 0,
  };
}

/**
 * Append a vector to a fragment
 * @returns The local index within the fragment
 */
export function fragmentAppend(
  fragment: Fragment,
  vector: Float32Array,
  config: VectorStoreConfig
): number {
  if (fragment.state === 'sealed') {
    throw new Error('Cannot append to sealed fragment');
  }
  
  // Get current row group or create new one
  let rowGroup = fragment.rowGroups[fragment.rowGroups.length - 1];
  
  if (rowGroupIsFull(rowGroup, config.dimensions, config.rowGroupSize)) {
    // Create new row group
    rowGroup = createRowGroup(
      fragment.rowGroups.length,
      config.dimensions,
      config.rowGroupSize
    );
    fragment.rowGroups.push(rowGroup);
  }
  
  const localIdx = fragment.totalVectors;
  rowGroupAppend(rowGroup, vector, config.dimensions, config.normalize);
  fragment.totalVectors++;
  
  return localIdx;
}

/**
 * Mark a vector as deleted in a fragment
 */
export function fragmentDelete(
  fragment: Fragment,
  localIdx: number
): boolean {
  const wordIdx = localIdx >>> 5; // localIdx / 32
  const bitIdx = localIdx & 31;   // localIdx % 32
  const mask = 1 << bitIdx;
  
  if (fragment.deletionBitmap[wordIdx] & mask) {
    // Already deleted
    return false;
  }
  
  fragment.deletionBitmap[wordIdx] |= mask;
  fragment.deletedCount++;
  return true;
}

/**
 * Check if a vector is deleted
 */
export function fragmentIsDeleted(
  fragment: Fragment,
  localIdx: number
): boolean {
  const wordIdx = localIdx >>> 5;
  const bitIdx = localIdx & 31;
  return (fragment.deletionBitmap[wordIdx] & (1 << bitIdx)) !== 0;
}

/**
 * Seal a fragment (make it immutable)
 */
export function fragmentSeal(fragment: Fragment): void {
  fragment.state = 'sealed';
  
  // Trim the last row group's data array if not full
  const lastRowGroup = fragment.rowGroups[fragment.rowGroups.length - 1];
  if (lastRowGroup.count < lastRowGroup.data.length) {
    // Create trimmed copy
    const config = { dimensions: lastRowGroup.data.length / lastRowGroup.count };
    lastRowGroup.data = lastRowGroup.data.slice(0, lastRowGroup.count * config.dimensions);
  }
}

/**
 * Check if fragment should be sealed
 */
export function fragmentShouldSeal(
  fragment: Fragment,
  config: VectorStoreConfig
): boolean {
  return fragment.totalVectors >= config.fragmentTargetSize;
}

/**
 * Get a vector from a fragment
 */
export function fragmentGetVector(
  fragment: Fragment,
  localIdx: number,
  dimensions: number
): Float32Array | null {
  if (fragmentIsDeleted(fragment, localIdx)) {
    return null;
  }
  
  const rowGroupIdx = Math.floor(localIdx / dimensions);
  const rowGroup = fragment.rowGroups[rowGroupIdx];
  if (!rowGroup) return null;
  
  const localRowIdx = localIdx % dimensions;
  const offset = localRowIdx * dimensions;
  return rowGroup.data.subarray(offset, offset + dimensions);
}
```

### 3.3 Columnar Store (`src/vector/columnar-store.ts`)

```typescript
/**
 * Lance-style columnar vector store
 * 
 * Manages fragments, handles inserts/deletes, coordinates with index.
 */

import type { 
  VectorManifest, 
  VectorStoreConfig, 
  Fragment,
  VectorSearchResult,
  NodeID 
} from './types.ts';
import { 
  createFragment, 
  fragmentAppend, 
  fragmentDelete, 
  fragmentSeal,
  fragmentShouldSeal,
  fragmentIsDeleted,
} from './fragment.ts';
import { DEFAULT_VECTOR_CONFIG } from './types.ts';

/**
 * Create a new vector store
 */
export function createVectorStore(
  dimensions: number,
  config?: Partial<Omit<VectorStoreConfig, 'dimensions'>>
): VectorManifest {
  const fullConfig: VectorStoreConfig = {
    ...DEFAULT_VECTOR_CONFIG,
    ...config,
    dimensions,
  };
  
  const initialFragment = createFragment(0, fullConfig);
  
  return {
    config: fullConfig,
    fragments: [initialFragment],
    activeFragmentId: 0,
    totalVectors: 0,
    totalDeleted: 0,
    nextVectorId: 0,
    nodeIdToVectorId: new Map(),
    vectorIdToLocation: new Map(),
  };
}

/**
 * Insert a vector into the store
 * @returns The global vector ID
 */
export function vectorStoreInsert(
  manifest: VectorManifest,
  nodeId: NodeID,
  vector: Float32Array
): number {
  // Check dimensions
  if (vector.length !== manifest.config.dimensions) {
    throw new Error(
      `Vector dimension mismatch: expected ${manifest.config.dimensions}, got ${vector.length}`
    );
  }
  
  // Check if node already has a vector
  const existingVectorId = manifest.nodeIdToVectorId.get(nodeId);
  if (existingVectorId !== undefined) {
    // Delete old vector first
    vectorStoreDelete(manifest, nodeId);
  }
  
  // Get active fragment
  let fragment = manifest.fragments.find(f => f.id === manifest.activeFragmentId);
  if (!fragment || fragment.state === 'sealed') {
    // Create new fragment
    const newId = manifest.fragments.length;
    fragment = createFragment(newId, manifest.config);
    manifest.fragments.push(fragment);
    manifest.activeFragmentId = newId;
  }
  
  // Check if fragment should be sealed
  if (fragmentShouldSeal(fragment, manifest.config)) {
    fragmentSeal(fragment);
    const newId = manifest.fragments.length;
    fragment = createFragment(newId, manifest.config);
    manifest.fragments.push(fragment);
    manifest.activeFragmentId = newId;
  }
  
  // Append to fragment
  const localIdx = fragmentAppend(fragment, vector, manifest.config);
  
  // Assign global vector ID
  const vectorId = manifest.nextVectorId++;
  
  // Update mappings
  manifest.nodeIdToVectorId.set(nodeId, vectorId);
  manifest.vectorIdToLocation.set(vectorId, {
    fragmentId: fragment.id,
    localIndex: localIdx,
  });
  
  manifest.totalVectors++;
  
  return vectorId;
}

/**
 * Delete a vector from the store
 */
export function vectorStoreDelete(
  manifest: VectorManifest,
  nodeId: NodeID
): boolean {
  const vectorId = manifest.nodeIdToVectorId.get(nodeId);
  if (vectorId === undefined) return false;
  
  const location = manifest.vectorIdToLocation.get(vectorId);
  if (!location) return false;
  
  const fragment = manifest.fragments.find(f => f.id === location.fragmentId);
  if (!fragment) return false;
  
  const deleted = fragmentDelete(fragment, location.localIndex);
  if (deleted) {
    manifest.nodeIdToVectorId.delete(nodeId);
    manifest.totalDeleted++;
  }
  
  return deleted;
}

/**
 * Get a vector by node ID
 */
export function vectorStoreGet(
  manifest: VectorManifest,
  nodeId: NodeID
): Float32Array | null {
  const vectorId = manifest.nodeIdToVectorId.get(nodeId);
  if (vectorId === undefined) return null;
  
  const location = manifest.vectorIdToLocation.get(vectorId);
  if (!location) return null;
  
  const fragment = manifest.fragments.find(f => f.id === location.fragmentId);
  if (!fragment) return null;
  
  if (fragmentIsDeleted(fragment, location.localIndex)) {
    return null;
  }
  
  // Calculate row group and local index
  const rowGroupSize = manifest.config.rowGroupSize;
  const rowGroupIdx = Math.floor(location.localIndex / rowGroupSize);
  const localRowIdx = location.localIndex % rowGroupSize;
  
  const rowGroup = fragment.rowGroups[rowGroupIdx];
  if (!rowGroup) return null;
  
  const dimensions = manifest.config.dimensions;
  const offset = localRowIdx * dimensions;
  return rowGroup.data.subarray(offset, offset + dimensions);
}

/**
 * Iterate over all non-deleted vectors
 * Yields (vectorId, nodeId, vector) tuples
 */
export function* vectorStoreIterator(
  manifest: VectorManifest
): Generator<[number, NodeID, Float32Array]> {
  const { dimensions, rowGroupSize } = manifest.config;
  
  for (const [nodeId, vectorId] of manifest.nodeIdToVectorId) {
    const location = manifest.vectorIdToLocation.get(vectorId);
    if (!location) continue;
    
    const fragment = manifest.fragments.find(f => f.id === location.fragmentId);
    if (!fragment) continue;
    
    if (fragmentIsDeleted(fragment, location.localIndex)) continue;
    
    const rowGroupIdx = Math.floor(location.localIndex / rowGroupSize);
    const localRowIdx = location.localIndex % rowGroupSize;
    const rowGroup = fragment.rowGroups[rowGroupIdx];
    if (!rowGroup) continue;
    
    const offset = localRowIdx * dimensions;
    const vector = rowGroup.data.subarray(offset, offset + dimensions);
    
    yield [vectorId, nodeId, vector];
  }
}

/**
 * Get store statistics
 */
export function vectorStoreStats(manifest: VectorManifest): {
  totalVectors: number;
  totalDeleted: number;
  liveVectors: number;
  fragmentCount: number;
  sealedFragments: number;
  activeFragmentVectors: number;
} {
  const activeFragment = manifest.fragments.find(
    f => f.id === manifest.activeFragmentId
  );
  
  return {
    totalVectors: manifest.totalVectors,
    totalDeleted: manifest.totalDeleted,
    liveVectors: manifest.totalVectors - manifest.totalDeleted,
    fragmentCount: manifest.fragments.length,
    sealedFragments: manifest.fragments.filter(f => f.state === 'sealed').length,
    activeFragmentVectors: activeFragment?.totalVectors ?? 0,
  };
}
```

---

## Phase 4: IVF Index Implementation

### 4.1 IVF-Flat Index (`src/vector/ivf-index.ts`)

```typescript
/**
 * IVF (Inverted File) index for approximate nearest neighbor search
 * 
 * Algorithm:
 * 1. Training: Run k-means to find cluster centroids
 * 2. Insert: Assign each vector to nearest centroid
 * 3. Search: Find nearest centroids, then search their vectors
 * 
 * This is more disk-friendly than HNSW and works well with columnar storage.
 */

import type { 
  IvfIndex, 
  IvfConfig, 
  VectorManifest, 
  VectorSearchResult,
  NodeID 
} from './types.ts';
import { DEFAULT_IVF_CONFIG } from './types.ts';
import { dotProduct, cosineDistance, getBatchDistanceFunction, distanceToSimilarity } from './distance.ts';
import { normalize } from './normalize.ts';

// ============================================================================
// Index Creation
// ============================================================================

/**
 * Create a new IVF index
 */
export function createIvfIndex(
  dimensions: number,
  config?: Partial<IvfConfig>
): IvfIndex {
  const fullConfig: IvfConfig = {
    ...DEFAULT_IVF_CONFIG,
    ...config,
  };
  
  return {
    config: fullConfig,
    centroids: new Float32Array(fullConfig.nClusters * dimensions),
    invertedLists: new Map(),
    trained: false,
    trainingVectors: new Float32Array(0),
    trainingCount: 0,
  };
}

// ============================================================================
// Training (K-Means)
// ============================================================================

/**
 * Add vectors for training
 */
export function ivfAddTrainingVectors(
  index: IvfIndex,
  vectors: Float32Array,
  dimensions: number,
  count: number
): void {
  if (index.trained) {
    throw new Error('Index already trained');
  }
  
  // Expand training buffer if needed
  const currentCapacity = index.trainingVectors!.length / dimensions;
  const neededCapacity = (index.trainingCount ?? 0) + count;
  
  if (neededCapacity > currentCapacity) {
    const newCapacity = Math.max(neededCapacity, currentCapacity * 2, 1000);
    const newBuffer = new Float32Array(newCapacity * dimensions);
    newBuffer.set(index.trainingVectors!);
    index.trainingVectors = newBuffer;
  }
  
  // Copy vectors
  const offset = (index.trainingCount ?? 0) * dimensions;
  index.trainingVectors!.set(vectors.subarray(0, count * dimensions), offset);
  index.trainingCount = (index.trainingCount ?? 0) + count;
}

/**
 * Train the index using k-means clustering
 */
export function ivfTrain(
  index: IvfIndex,
  dimensions: number,
  maxIterations: number = 20
): void {
  if (index.trained) return;
  if (!index.trainingVectors || index.trainingCount === 0) {
    throw new Error('No training vectors provided');
  }
  
  const { nClusters } = index.config;
  const n = index.trainingCount!;
  const vectors = index.trainingVectors!;
  
  // Initialize centroids using k-means++
  initializeCentroidsKMeansPlusPlus(
    index.centroids,
    vectors,
    dimensions,
    n,
    nClusters
  );
  
  // Run k-means iterations
  const assignments = new Uint32Array(n);
  
  for (let iter = 0; iter < maxIterations; iter++) {
    // Assign vectors to nearest centroids
    let changed = 0;
    for (let i = 0; i < n; i++) {
      const vecOffset = i * dimensions;
      const vec = vectors.subarray(vecOffset, vecOffset + dimensions);
      
      let bestCluster = 0;
      let bestDist = Infinity;
      
      for (let c = 0; c < nClusters; c++) {
        const centOffset = c * dimensions;
        const centroid = index.centroids.subarray(centOffset, centOffset + dimensions);
        const dist = cosineDistance(vec, centroid);
        
        if (dist < bestDist) {
          bestDist = dist;
          bestCluster = c;
        }
      }
      
      if (assignments[i] !== bestCluster) {
        assignments[i] = bestCluster;
        changed++;
      }
    }
    
    // Check for convergence
    if (changed === 0) break;
    
    // Update centroids
    const clusterSums = new Float32Array(nClusters * dimensions);
    const clusterCounts = new Uint32Array(nClusters);
    
    for (let i = 0; i < n; i++) {
      const cluster = assignments[i];
      const vecOffset = i * dimensions;
      const sumOffset = cluster * dimensions;
      
      for (let d = 0; d < dimensions; d++) {
        clusterSums[sumOffset + d] += vectors[vecOffset + d];
      }
      clusterCounts[cluster]++;
    }
    
    for (let c = 0; c < nClusters; c++) {
      const count = clusterCounts[c];
      if (count === 0) continue;
      
      const offset = c * dimensions;
      for (let d = 0; d < dimensions; d++) {
        index.centroids[offset + d] = clusterSums[offset + d] / count;
      }
      
      // Normalize centroid
      const centroid = index.centroids.subarray(offset, offset + dimensions);
      const norm = Math.sqrt(dotProduct(centroid, centroid));
      if (norm > 0) {
        for (let d = 0; d < dimensions; d++) {
          index.centroids[offset + d] /= norm;
        }
      }
    }
  }
  
  // Initialize inverted lists
  for (let c = 0; c < nClusters; c++) {
    index.invertedLists.set(c, []);
  }
  
  index.trained = true;
  
  // Clear training data
  index.trainingVectors = undefined;
  index.trainingCount = undefined;
}

/**
 * K-means++ initialization
 */
function initializeCentroidsKMeansPlusPlus(
  centroids: Float32Array,
  vectors: Float32Array,
  dimensions: number,
  n: number,
  k: number
): void {
  // First centroid: random vector
  const firstIdx = Math.floor(Math.random() * n);
  centroids.set(
    vectors.subarray(firstIdx * dimensions, (firstIdx + 1) * dimensions),
    0
  );
  
  // Remaining centroids: weighted by distance squared
  const minDists = new Float32Array(n).fill(Infinity);
  
  for (let c = 1; c < k; c++) {
    // Update min distances
    const prevCentroid = centroids.subarray((c - 1) * dimensions, c * dimensions);
    
    let totalDist = 0;
    for (let i = 0; i < n; i++) {
      const vec = vectors.subarray(i * dimensions, (i + 1) * dimensions);
      const dist = cosineDistance(vec, prevCentroid);
      minDists[i] = Math.min(minDists[i], dist);
      totalDist += minDists[i];
    }
    
    // Weighted random selection
    let r = Math.random() * totalDist;
    let selectedIdx = 0;
    
    for (let i = 0; i < n; i++) {
      r -= minDists[i];
      if (r <= 0) {
        selectedIdx = i;
        break;
      }
    }
    
    centroids.set(
      vectors.subarray(selectedIdx * dimensions, (selectedIdx + 1) * dimensions),
      c * dimensions
    );
  }
}

// ============================================================================
// Insert & Delete
// ============================================================================

/**
 * Insert a vector into the index
 */
export function ivfInsert(
  index: IvfIndex,
  vectorId: number,
  vector: Float32Array,
  dimensions: number
): void {
  if (!index.trained) {
    throw new Error('Index not trained');
  }
  
  // Find nearest centroid
  const cluster = findNearestCentroid(index, vector, dimensions);
  
  // Add to inverted list
  const list = index.invertedLists.get(cluster);
  if (list) {
    list.push(vectorId);
  }
}

/**
 * Delete a vector from the index
 */
export function ivfDelete(
  index: IvfIndex,
  vectorId: number,
  vector: Float32Array,
  dimensions: number
): boolean {
  if (!index.trained) return false;
  
  // Find which cluster it's in
  const cluster = findNearestCentroid(index, vector, dimensions);
  const list = index.invertedLists.get(cluster);
  
  if (!list) return false;
  
  const idx = list.indexOf(vectorId);
  if (idx === -1) return false;
  
  // Remove from list (swap with last for O(1))
  list[idx] = list[list.length - 1];
  list.pop();
  
  return true;
}

/**
 * Find nearest centroid for a vector
 */
function findNearestCentroid(
  index: IvfIndex,
  vector: Float32Array,
  dimensions: number
): number {
  const { nClusters } = index.config;
  let bestCluster = 0;
  let bestDist = Infinity;
  
  for (let c = 0; c < nClusters; c++) {
    const centOffset = c * dimensions;
    const centroid = index.centroids.subarray(centOffset, centOffset + dimensions);
    const dist = cosineDistance(vector, centroid);
    
    if (dist < bestDist) {
      bestDist = dist;
      bestCluster = c;
    }
  }
  
  return bestCluster;
}

// ============================================================================
// Search
// ============================================================================

/**
 * Search for k nearest neighbors
 */
export function ivfSearch(
  index: IvfIndex,
  manifest: VectorManifest,
  query: Float32Array,
  k: number,
  options?: {
    nProbe?: number;
    filter?: (nodeId: NodeID) => boolean;
  }
): VectorSearchResult[] {
  if (!index.trained) return [];
  
  const dimensions = manifest.config.dimensions;
  const nProbe = options?.nProbe ?? index.config.nProbe;
  const metric = manifest.config.metric;
  
  // Normalize query
  const queryNorm = normalize(query);
  
  // Find top nProbe nearest centroids
  const centroidDists: Array<{ cluster: number; distance: number }> = [];
  
  for (let c = 0; c < index.config.nClusters; c++) {
    const centOffset = c * dimensions;
    const centroid = index.centroids.subarray(centOffset, centOffset + dimensions);
    const dist = cosineDistance(queryNorm, centroid);
    centroidDists.push({ cluster: c, distance: dist });
  }
  
  centroidDists.sort((a, b) => a.distance - b.distance);
  const probeClusters = centroidDists.slice(0, nProbe);
  
  // Search within selected clusters
  const candidates: Array<{ vectorId: number; distance: number }> = [];
  const batchDistFn = getBatchDistanceFunction(metric);
  
  for (const { cluster } of probeClusters) {
    const vectorIds = index.invertedLists.get(cluster);
    if (!vectorIds || vectorIds.length === 0) continue;
    
    for (const vectorId of vectorIds) {
      // Get vector location
      const location = manifest.vectorIdToLocation.get(vectorId);
      if (!location) continue;
      
      // Get fragment and check deletion
      const fragment = manifest.fragments.find(f => f.id === location.fragmentId);
      if (!fragment) continue;
      
      const { localIndex } = location;
      const wordIdx = localIndex >>> 5;
      const bitIdx = localIndex & 31;
      if (fragment.deletionBitmap[wordIdx] & (1 << bitIdx)) continue;
      
      // Get vector data
      const rowGroupSize = manifest.config.rowGroupSize;
      const rowGroupIdx = Math.floor(localIndex / rowGroupSize);
      const localRowIdx = localIndex % rowGroupSize;
      const rowGroup = fragment.rowGroups[rowGroupIdx];
      if (!rowGroup) continue;
      
      const offset = localRowIdx * dimensions;
      const vec = rowGroup.data.subarray(offset, offset + dimensions);
      
      // Apply filter
      if (options?.filter) {
        // Find nodeId for this vectorId
        let nodeId: NodeID | undefined;
        for (const [nid, vid] of manifest.nodeIdToVectorId) {
          if (vid === vectorId) {
            nodeId = nid;
            break;
          }
        }
        if (nodeId !== undefined && !options.filter(nodeId)) continue;
      }
      
      // Compute distance
      const dist = cosineDistance(queryNorm, vec);
      candidates.push({ vectorId, distance: dist });
    }
  }
  
  // Sort and take top k
  candidates.sort((a, b) => a.distance - b.distance);
  
  // Convert to results
  return candidates.slice(0, k).map(({ vectorId, distance }) => {
    // Find nodeId
    let nodeId: NodeID = 0;
    for (const [nid, vid] of manifest.nodeIdToVectorId) {
      if (vid === vectorId) {
        nodeId = nid;
        break;
      }
    }
    
    return {
      vectorId,
      nodeId,
      distance,
      similarity: distanceToSimilarity(distance, metric),
    };
  });
}

/**
 * Search with multiple query vectors
 */
export function ivfSearchMulti(
  index: IvfIndex,
  manifest: VectorManifest,
  queries: Float32Array[],
  k: number,
  aggregation: 'min' | 'max' | 'avg' | 'sum',
  options?: {
    nProbe?: number;
    filter?: (nodeId: NodeID) => boolean;
  }
): VectorSearchResult[] {
  // Get results for each query
  const allResults = queries.map(q => 
    ivfSearch(index, manifest, q, k * 2, options)
  );
  
  // Aggregate by nodeId
  const aggregated = new Map<NodeID, { distances: number[]; vectorId: number }>();
  
  for (const results of allResults) {
    for (const result of results) {
      const existing = aggregated.get(result.nodeId);
      if (existing) {
        existing.distances.push(result.distance);
      } else {
        aggregated.set(result.nodeId, {
          distances: [result.distance],
          vectorId: result.vectorId,
        });
      }
    }
  }
  
  // Compute aggregated score
  const scored: VectorSearchResult[] = [];
  const metric = manifest.config.metric;
  
  for (const [nodeId, { distances, vectorId }] of aggregated) {
    let distance: number;
    switch (aggregation) {
      case 'min':
        distance = Math.min(...distances);
        break;
      case 'max':
        distance = Math.max(...distances);
        break;
      case 'avg':
        distance = distances.reduce((a, b) => a + b, 0) / distances.length;
        break;
      case 'sum':
        distance = distances.reduce((a, b) => a + b, 0);
        break;
    }
    
    scored.push({
      vectorId,
      nodeId,
      distance,
      similarity: distanceToSimilarity(distance, metric),
    });
  }
  
  // Sort and return top k
  return scored
    .sort((a, b) => a.distance - b.distance)
    .slice(0, k);
}

/**
 * Get index statistics
 */
export function ivfStats(index: IvfIndex): {
  trained: boolean;
  nClusters: number;
  totalVectors: number;
  avgVectorsPerCluster: number;
  emptyClusterCount: number;
} {
  let total = 0;
  let empty = 0;
  
  for (const [, list] of index.invertedLists) {
    total += list.length;
    if (list.length === 0) empty++;
  }
  
  return {
    trained: index.trained,
    nClusters: index.config.nClusters,
    totalVectors: total,
    avgVectorsPerCluster: total / index.config.nClusters,
    emptyClusterCount: empty,
  };
}
```

---

## Phase 5: Compaction

### 5.1 Fragment Compaction (`src/vector/compaction.ts`)

```typescript
/**
 * Fragment compaction to remove deleted vectors
 * 
 * Compaction creates a new fragment containing only live vectors
 * from one or more source fragments.
 */

import type { Fragment, VectorManifest, VectorStoreConfig } from './types.ts';
import { createFragment, fragmentAppend, fragmentSeal, fragmentIsDeleted } from './fragment.ts';

/**
 * Compaction strategy
 */
export interface CompactionStrategy {
  /** Minimum deletion ratio to trigger compaction */
  minDeletionRatio: number;
  /** Maximum fragments to compact at once */
  maxFragmentsPerCompaction: number;
  /** Minimum total vectors across fragments to compact */
  minVectorsToCompact: number;
}

export const DEFAULT_COMPACTION_STRATEGY: CompactionStrategy = {
  minDeletionRatio: 0.3,    // 30% deleted
  maxFragmentsPerCompaction: 4,
  minVectorsToCompact: 10000,
};

/**
 * Find fragments that should be compacted
 */
export function findFragmentsToCompact(
  manifest: VectorManifest,
  strategy: CompactionStrategy = DEFAULT_COMPACTION_STRATEGY
): number[] {
  const candidates: Array<{ id: number; deletionRatio: number; liveVectors: number }> = [];
  
  for (const fragment of manifest.fragments) {
    // Skip active fragment
    if (fragment.state === 'active') continue;
    
    const deletionRatio = fragment.deletedCount / fragment.totalVectors;
    if (deletionRatio >= strategy.minDeletionRatio) {
      candidates.push({
        id: fragment.id,
        deletionRatio,
        liveVectors: fragment.totalVectors - fragment.deletedCount,
      });
    }
  }
  
  // Sort by deletion ratio (highest first)
  candidates.sort((a, b) => b.deletionRatio - a.deletionRatio);
  
  // Select fragments to compact
  const selected: number[] = [];
  let totalLiveVectors = 0;
  
  for (const candidate of candidates) {
    if (selected.length >= strategy.maxFragmentsPerCompaction) break;
    selected.push(candidate.id);
    totalLiveVectors += candidate.liveVectors;
  }
  
  // Only compact if we have enough vectors
  if (totalLiveVectors < strategy.minVectorsToCompact && selected.length < 2) {
    return [];
  }
  
  return selected;
}

/**
 * Compact fragments into a new fragment
 * Returns the new fragment and updated mappings
 */
export function compactFragments(
  manifest: VectorManifest,
  fragmentIds: number[]
): {
  newFragment: Fragment;
  updatedLocations: Map<number, { fragmentId: number; localIndex: number }>;
} {
  const config = manifest.config;
  const newFragmentId = manifest.fragments.length;
  const newFragment = createFragment(newFragmentId, config);
  const updatedLocations = new Map<number, { fragmentId: number; localIndex: number }>();
  
  const { dimensions, rowGroupSize } = config;
  
  // Process each source fragment
  for (const fragmentId of fragmentIds) {
    const fragment = manifest.fragments.find(f => f.id === fragmentId);
    if (!fragment) continue;
    
    // Iterate over all vectors in fragment
    for (let localIdx = 0; localIdx < fragment.totalVectors; localIdx++) {
      // Skip deleted vectors
      if (fragmentIsDeleted(fragment, localIdx)) continue;
      
      // Get vector data
      const rowGroupIdx = Math.floor(localIdx / rowGroupSize);
      const localRowIdx = localIdx % rowGroupSize;
      const rowGroup = fragment.rowGroups[rowGroupIdx];
      if (!rowGroup) continue;
      
      const offset = localRowIdx * dimensions;
      const vector = rowGroup.data.subarray(offset, offset + dimensions);
      
      // Find the vectorId for this location
      let vectorId: number | undefined;
      for (const [vid, loc] of manifest.vectorIdToLocation) {
        if (loc.fragmentId === fragmentId && loc.localIndex === localIdx) {
          vectorId = vid;
          break;
        }
      }
      
      if (vectorId === undefined) continue;
      
      // Append to new fragment (skip normalization since already normalized)
      const newLocalIdx = newFragment.totalVectors;
      
      // Get or create row group
      let targetRowGroup = newFragment.rowGroups[newFragment.rowGroups.length - 1];
      if (targetRowGroup.count >= rowGroupSize) {
        targetRowGroup = {
          id: newFragment.rowGroups.length,
          count: 0,
          data: new Float32Array(rowGroupSize * dimensions),
        };
        newFragment.rowGroups.push(targetRowGroup);
      }
      
      // Copy vector
      const targetOffset = targetRowGroup.count * dimensions;
      targetRowGroup.data.set(vector, targetOffset);
      targetRowGroup.count++;
      newFragment.totalVectors++;
      
      // Record updated location
      updatedLocations.set(vectorId, {
        fragmentId: newFragmentId,
        localIndex: newLocalIdx,
      });
    }
  }
  
  // Seal the new fragment
  fragmentSeal(newFragment);
  
  return { newFragment, updatedLocations };
}

/**
 * Apply compaction to manifest
 */
export function applyCompaction(
  manifest: VectorManifest,
  fragmentIds: number[],
  newFragment: Fragment,
  updatedLocations: Map<number, { fragmentId: number; localIndex: number }>
): void {
  // Add new fragment
  manifest.fragments.push(newFragment);
  
  // Update vector locations
  for (const [vectorId, location] of updatedLocations) {
    manifest.vectorIdToLocation.set(vectorId, location);
  }
  
  // Update deleted count
  let removedDeleted = 0;
  for (const fragmentId of fragmentIds) {
    const fragment = manifest.fragments.find(f => f.id === fragmentId);
    if (fragment) {
      removedDeleted += fragment.deletedCount;
    }
  }
  manifest.totalDeleted -= removedDeleted;
  
  // Mark old fragments for removal (keep them but clear data)
  for (const fragmentId of fragmentIds) {
    const idx = manifest.fragments.findIndex(f => f.id === fragmentId);
    if (idx !== -1) {
      // Clear fragment data but keep metadata for ID tracking
      manifest.fragments[idx].rowGroups = [];
      manifest.fragments[idx].deletionBitmap = new Uint32Array(0);
    }
  }
}
```

---

## Phase 6: Serialization

### 6.1 IVF Serialization (`src/vector/ivf-serialize.ts`)

```typescript
/**
 * Binary serialization for IVF index
 */

import type { IvfIndex, IvfConfig } from './types.ts';

/**
 * Serialize IVF index to binary
 */
export function serializeIvf(index: IvfIndex, dimensions: number): Uint8Array {
  const { config, centroids, invertedLists, trained } = index;
  
  // Calculate size
  let size = 32; // Header
  size += centroids.length * 4; // Centroids
  size += 4; // Number of lists
  
  for (const [, list] of invertedLists) {
    size += 4 + list.length * 4; // List length + vector IDs
  }
  
  const buffer = new Uint8Array(size);
  const view = new DataView(buffer.buffer);
  let offset = 0;
  
  // Header
  view.setUint32(offset, config.nClusters, true); offset += 4;
  view.setUint32(offset, dimensions, true); offset += 4;
  view.setUint32(offset, config.nProbe, true); offset += 4;
  view.setUint8(offset, trained ? 1 : 0); offset += 1;
  view.setUint8(offset, config.usePQ ? 1 : 0); offset += 1;
  offset += 14; // Padding to 32 bytes
  
  // Centroids
  for (let i = 0; i < centroids.length; i++) {
    view.setFloat32(offset, centroids[i], true);
    offset += 4;
  }
  
  // Inverted lists
  view.setUint32(offset, invertedLists.size, true); offset += 4;
  
  for (const [cluster, list] of invertedLists) {
    view.setUint32(offset, cluster, true); offset += 4;
    view.setUint32(offset, list.length, true); offset += 4;
    for (const vectorId of list) {
      view.setUint32(offset, vectorId, true); offset += 4;
    }
  }
  
  return buffer;
}

/**
 * Deserialize IVF index from binary
 */
export function deserializeIvf(buffer: Uint8Array): { index: IvfIndex; dimensions: number } {
  const view = new DataView(buffer.buffer, buffer.byteOffset, buffer.byteLength);
  let offset = 0;
  
  // Header
  const nClusters = view.getUint32(offset, true); offset += 4;
  const dimensions = view.getUint32(offset, true); offset += 4;
  const nProbe = view.getUint32(offset, true); offset += 4;
  const trained = view.getUint8(offset) === 1; offset += 1;
  const usePQ = view.getUint8(offset) === 1; offset += 1;
  offset += 14; // Skip padding
  
  const config: IvfConfig = {
    nClusters,
    nProbe,
    usePQ,
  };
  
  // Centroids
  const centroids = new Float32Array(nClusters * dimensions);
  for (let i = 0; i < centroids.length; i++) {
    centroids[i] = view.getFloat32(offset, true);
    offset += 4;
  }
  
  // Inverted lists
  const numLists = view.getUint32(offset, true); offset += 4;
  const invertedLists = new Map<number, number[]>();
  
  for (let i = 0; i < numLists; i++) {
    const cluster = view.getUint32(offset, true); offset += 4;
    const listLength = view.getUint32(offset, true); offset += 4;
    const list: number[] = [];
    
    for (let j = 0; j < listLength; j++) {
      list.push(view.getUint32(offset, true));
      offset += 4;
    }
    
    invertedLists.set(cluster, list);
  }
  
  return {
    index: {
      config,
      centroids,
      invertedLists,
      trained,
    },
    dimensions,
  };
}
```

---

## Phase 7: Public API

### 7.1 Vector Search Builder (`src/api/vector-search.ts`)

```typescript
/**
 * Fluent API for vector search
 */

// (Same API as original - the underlying implementation changes,
// but the user-facing API remains the same)

// Example usage:
// const results = await db.similar(codeChunk, 'embedding', queryVector)
//   .filter({ language: 'typescript' })
//   .limit(10)
//   .threshold(0.8)
//   .exec();

// Multi-vector search:
// const results = await db.similarAny(codeChunk, 'embedding', [vec1, vec2])
//   .aggregation('max')
//   .limit(10)
//   .exec();
```

---

## Comparison: HNSW vs IVF-Flat (Lance-style)

| Aspect | HNSW (Original) | IVF-Flat (New) |
|--------|-----------------|----------------|
| **Index Structure** | Navigable small-world graph | Inverted file with centroids |
| **Memory Usage** | Higher (graph + vectors) | Lower (just centroids + lists) |
| **Insert Complexity** | O(log n) | O(k) for finding centroid |
| **Search Complexity** | O(log n) | O(nProbe * cluster_size) |
| **Disk Friendliness** | Poor (random access) | Good (sequential within cluster) |
| **Update Friendliness** | Complex (graph rewiring) | Simple (add to list) |
| **Columnar Compatibility** | Poor | Excellent |
| **Recall @ 10** | ~95%+ | ~90-95% (tunable via nProbe) |
| **Memory Mapping** | Difficult | Natural fit |

---

## Migration Path

If you have existing HNSW-based code:

1. The public API (`.similar()`, `.similarAny()`) remains unchanged
2. Only the internal implementation changes
3. Existing databases will need re-indexing (one-time migration)

---

## Future Enhancements

1. **IVF-PQ** - Product Quantization for 4-16x memory reduction
2. **Disk-based fragments** - Memory-map sealed fragments
3. **Incremental training** - Update centroids without full retrain
4. **GPU acceleration** - For batch distance computation
5. **Hybrid index** - HNSW on centroids + IVF for vectors
