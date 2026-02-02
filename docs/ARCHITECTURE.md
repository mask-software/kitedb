# RayDB Architecture Overview

RayDB is a high-performance embedded graph database written in TypeScript for the Bun runtime.

## Table of Contents

- [Storage Model](#storage-model)
- [Directory Structure](#directory-structure)
- [Core Data Structures](#core-data-structures)
- [CSR Format](#csr-format)
- [Storage Formats](#storage-formats)
- [Delta/WAL System](#deltawal-system)
- [MVCC](#mvcc-multi-version-concurrency-control)
- [Key Index](#key-index)
- [Transaction Model](#transaction-model)
- [API Layers](#api-layers)
- [Why It's Fast](#why-its-fast)

---

## Storage Model

```
┌─────────────────────────────────────────────────────────────────┐
│                        GraphDB Handle                           │
├─────────────────────────────────────────────────────────────────┤
│  ┌──────────────┐    ┌──────────────┐    ┌──────────────────┐  │
│  │   Snapshot   │    │    Delta     │    │    WAL Buffer    │  │
│  │   (mmap'd)   │    │  (in-memory) │    │    (linear)      │  │
│  │              │    │              │    │                  │  │
│  │  CSR Format  │ +  │  Pending     │ →  │  Durability      │  │
│  │  Zero-copy   │    │  Changes     │    │  Crash Recovery  │  │
│  └──────────────┘    └──────────────┘    └──────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

### Key Insight: Snapshot + Delta Architecture

1. **Snapshot**: Immutable, memory-mapped CSR (Compressed Sparse Row) format
   - Zero-copy reads via `Bun.mmap()`
   - Edges stored in sorted adjacency lists for O(1) traversal start
   - Hash-bucketed key index for fast node lookups

2. **Delta**: In-memory overlay of uncommitted changes
   - Created/deleted nodes, added/deleted edges
   - Property modifications
   - Merged with snapshot on read

3. **WAL**: Write-ahead log for durability
   - Linear buffer in single-file format (checkpoint to reclaim space)
   - Dual-region design for background checkpointing

---

## Directory Structure

```
src/
├── index.ts              # Main entry point, exports all public APIs
├── types.ts              # Core type definitions (NodeID, Edge, GraphDB, etc.)
├── constants.ts          # Magic numbers, file extensions, thresholds
├── api/                  # High-level Drizzle-style API
│   ├── index.ts          # API exports
│   ├── kite.ts           # Main Kite class
│   ├── schema.ts         # Schema definition (defineNode, defineEdge, prop)
│   ├── builders.ts       # Query builders (insert, update, delete)
│   ├── traversal.ts      # Graph traversal builders
│   └── pathfinding.ts    # Dijkstra/A* pathfinding
├── core/                 # Low-level storage layer
│   ├── snapshot-reader.ts    # CSR snapshot reading with mmap
│   ├── snapshot-writer.ts    # CSR snapshot building
│   ├── delta.ts              # In-memory delta overlay
│   ├── wal.ts                # Multi-file WAL format
│   ├── wal-buffer.ts         # Single-file linear WAL buffer
│   ├── header.ts             # Single-file header management
│   ├── pager.ts              # Page-based I/O abstraction
│   ├── manifest.ts           # Multi-file manifest management
│   ├── compactor.ts          # Multi-file compaction
│   └── single-file-compactor.ts  # Single-file checkpoint/compaction
├── graph-db/             # Graph database operations
│   ├── graph-db/
│   │   ├── index.ts          # Re-exports all graph operations
│   │   ├── lifecycle.ts      # openGraphDB, closeGraphDB
│   │   ├── single-file.ts    # Single-file format support
│   │   ├── tx.ts             # Transaction handling
│   │   ├── nodes.ts          # Node CRUD operations
│   │   ├── edges.ts          # Edge CRUD operations
│   │   ├── definitions.ts    # Schema definitions
│   │   ├── checkpoint.ts     # Background checkpointing
│   │   └── wal-replay.ts     # WAL recovery
│   ├── key-index.ts      # Key lookup across snapshot + delta
│   └── iterators.ts      # Traversal iterators
├── mvcc/                 # Multi-Version Concurrency Control
│   ├── index.ts          # MvccManager coordinator
│   ├── tx-manager.ts     # Transaction state management
│   ├── version-chain.ts  # Version chain storage
│   ├── visibility.ts     # Snapshot isolation visibility rules
│   ├── conflict-detector.ts  # Write-write conflict detection
│   └── gc.ts             # Garbage collection of old versions
├── cache/                # Caching layer
│   ├── index.ts          # CacheManager
│   ├── property-cache.ts
│   ├── traversal-cache.ts
│   └── query-cache.ts
└── util/                 # Utilities
    ├── binary.ts         # Binary encoding/decoding helpers
    ├── compression.ts    # zstd/gzip/deflate compression
    ├── crc.ts            # CRC32C checksums
    ├── hash.ts           # xxHash64 for key hashing
    ├── lock.ts           # File locking
    ├── lru.ts            # LRU cache implementation
    └── heap.ts           # Min-heap for pathfinding
```

---

## Core Data Structures

### Node Representation

```typescript
type NodeID = number;      // Monotonic, never reused (up to 2^53-1)
type PhysNode = number;    // Physical index in snapshot arrays (0..num_nodes-1)

interface NodeDelta {
  key?: string;                             // Unique key for lookup
  labels?: Set<LabelID>;                    // Added labels
  labelsDeleted?: Set<LabelID>;             // Deleted labels
  props?: Map<PropKeyID, PropValue | null>; // Property changes (null = deleted)
}
```

### Edge Representation

```typescript
interface Edge {
  src: NodeID;
  etype: ETypeID;
  dst: NodeID;
}

interface EdgePatch {
  etype: ETypeID;
  other: NodeID;  // dst for out-edges, src for in-edges
}
```

### Property Values

```typescript
enum PropValueTag {
  NULL = 0,
  BOOL = 1,
  I64 = 2,
  F64 = 3,
  STRING = 4,
}

type PropValue =
  | { tag: NULL }
  | { tag: BOOL; value: boolean }
  | { tag: I64; value: bigint }
  | { tag: F64; value: number }
  | { tag: STRING; value: string };
```

---

## CSR Format

See [CSR.md](./CSR.md) for a detailed explanation.

The snapshot uses CSR (Compressed Sparse Row) format for efficient edge traversal. For `N` nodes and `E` edges:

### Out-Edges CSR

```
out_offsets[N+1]: u32[]    # Prefix-sum array, out_offsets[i] = start index
out_dst[E]: u32[]          # Destination physical node IDs
out_etype[E]: u32[]        # Edge type IDs
```

**Traversal**: To get out-edges of node `phys`:

```typescript
start = out_offsets[phys];
end = out_offsets[phys + 1];
for (i = start; i < end; i++) {
  edge = { dst: out_dst[i], etype: out_etype[i] };
}
```

### In-Edges CSR (Bidirectional)

```
in_offsets[N+1]: u32[]
in_src[E]: u32[]           # Source physical node IDs
in_etype[E]: u32[]
in_out_index[E]: u32[]     # Index back to corresponding out-edge
```

Edges within each node are **sorted by (etype, dst/src)** for binary search during edge existence checks.

---

## Storage Formats

### Multi-File Format (Directory) - Deprecated (Legacy)

The directory-based layout is legacy and deprecated for new deployments. The
single-file `.kitedb` format is the default and recommended path forward.

```
database/
├── manifest.gdm           # Database state (active snapshot, WAL segment)
├── lock.gdl               # Lock file
├── snapshots/
│   └── snap_0000000000000001.gds
├── wal/
│   └── wal_0000000000000001.gdw
└── trash/                 # Old snapshots pending cleanup
```

### Single-File Format (.kitedb) - Default

```
┌────────────────────────────────────────┐
│           Header (4KB)                 │  ← Atomic updates, checksummed
├────────────────────────────────────────┤
│                                        │
│        WAL Area (~64MB)                │  ← Linear buffer
│   ┌─────────────────────────────┐     │
│   │  Primary Region (75%)       │     │  ← Normal writes
│   ├─────────────────────────────┤     │
│   │  Secondary Region (25%)     │     │  ← Writes during checkpoint
│   └─────────────────────────────┘     │
│                                        │
├────────────────────────────────────────┤
│                                        │
│        Snapshot Area (grows)           │  ← CSR snapshot, zstd compressed
│                                        │
└────────────────────────────────────────┘
```

Header contents:
- Magic: `"RayDB format 1\0"`
- Page size, version, flags
- Snapshot area location/size
- WAL area location/size
- WAL head/tail pointers
- Snapshot generation
- Max node ID, next TX ID
- Checksums (header + footer)

---

## Delta/WAL System

### Delta State (In-Memory Overlay)

```typescript
interface DeltaState {
  // Node mutations
  createdNodes: Map<NodeID, NodeDelta>;
  deletedNodes: Set<NodeID>;
  modifiedNodes: Map<NodeID, NodeDelta>;

  // Edge patches (maintain both directions)
  outAdd: Map<NodeID, EdgePatch[]>;  // sorted by (etype, other)
  outDel: Map<NodeID, EdgePatch[]>;
  inAdd: Map<NodeID, EdgePatch[]>;
  inDel: Map<NodeID, EdgePatch[]>;

  // Edge properties
  edgeProps: Map<string, Map<PropKeyID, PropValue | null>>;

  // Schema definitions
  newLabels: Map<LabelID, string>;
  newEtypes: Map<ETypeID, string>;
  newPropkeys: Map<PropKeyID, string>;

  // Key index changes
  keyIndex: Map<string, NodeID>;
  keyIndexDeleted: Set<string>;
}
```

### WAL Record Format

Each record is framed with:

```
recLen (u32)        - Unpadded length
type (u8)           - Record type (BEGIN, COMMIT, CREATE_NODE, etc.)
flags (u8)          - Reserved
reserved (u16)      - Padding
txid (u64)          - Transaction ID
payloadLen (u32)    - Payload length
payload[...]        - Variable-length payload
crc32c (u32)        - Checksum of type..payload
padding             - Align to 8 bytes
```

Record types: `BEGIN`, `COMMIT`, `ROLLBACK`, `CREATE_NODE`, `DELETE_NODE`, `ADD_EDGE`, `DELETE_EDGE`, `SET_NODE_PROP`, `DEL_NODE_PROP`, etc.

### Linear WAL Buffer (Single-File)

- **Dual-region design** for background checkpointing:
  - Primary region (75%): Normal writes
  - Secondary region (25%): Writes during checkpoint
- Page-level write batching to reduce I/O amplification
- No wrap-around; checkpoints reset the WAL to reclaim space

---

## MVCC (Multi-Version Concurrency Control)

### Architecture

```
MvccManager
├── TxManager            # Transaction lifecycle (begin/commit/abort)
├── VersionChainManager  # Version chain storage
├── ConflictDetector     # Write-write conflict detection
└── GarbageCollector     # Prune old versions
```

### Version Chains

Each modified entity has a version chain:

```typescript
interface VersionedRecord<T> {
  data: T;
  txid: bigint;                    // Transaction that created this version
  commitTs: bigint;                // Commit timestamp
  prev: VersionedRecord<T> | null; // Previous version
  deleted: boolean;                // Tombstone marker
}
```

### Visibility Rules

```
Transaction T1 (startTs=100)     Transaction T2 (startTs=105)
        │                                │
        ▼                                ▼
   ┌─────────┐                    ┌─────────┐
   │ Read A  │ ← sees version     │ Read A  │ ← sees version
   │         │   committed ≤100   │         │   committed ≤105
   └─────────┘                    └─────────┘
        │                                │
        ▼                                ▼
   Version Chain for Node A:
   ┌───────────────┐    ┌───────────────┐    ┌───────────────┐
   │ v3: commitTs= │ ←  │ v2: commitTs= │ ←  │ v1: commitTs= │
   │     110       │    │     95        │    │     50        │
   └───────────────┘    └───────────────┘    └───────────────┘
                              ↑                     ↑
                         T2 sees this          T1 sees this
```

A version is visible to transaction `T` if:
1. Version's `commitTs <= T.startTs` (committed before T started)
2. Or version was created by T itself (read-your-own-writes)

### Conflict Detection

Uses **First-Committer-Wins (FCW)**: On commit, check if any key in the write-set was modified by another transaction after this transaction's start timestamp.

### Lazy Version Chain Creation

Version chains are only created when there are active readers who might need to see old versions. This optimization reduces allocations significantly in serial workloads.

---

## Key Index

### Hash-Based Lookup

```typescript
interface KeyIndexEntry {
  hash64: bigint;     // xxHash64 of key
  stringId: number;   // For collision resolution
  reserved: number;
  nodeId: bigint;
}
```

### Two-Level Lookup

1. **Delta first**: Check `delta.keyIndex` and `delta.keyIndexDeleted`
2. **Snapshot fallback**: Binary search in bucket (if HAS_KEY_BUCKETS flag) or full scan

### Bucket Organization

- Entries sorted by `(bucket, hash64, stringId, nodeId)`
- Bucket array provides O(1) lookup to contiguous entry range
- Load factor ~50% (2x entries as buckets)

---

## Transaction Model

### Transaction State

```typescript
interface TxState {
  txid: bigint;
  pendingCreatedNodes: Map<NodeID, NodeDelta>;
  pendingDeletedNodes: Set<NodeID>;
  pendingOutAdd/Del: Map<NodeID, EdgePatch[]>;
  pendingInAdd/Del: Map<NodeID, EdgePatch[]>;
  pendingNodeProps: Map<NodeID, Map<PropKeyID, PropValue | null>>;
  pendingEdgeProps: Map<string, Map<PropKeyID, PropValue | null>>;
  // ... schema definitions, key updates
}
```

### Commit Flow

1. **MVCC validation**: Check for write-write conflicts
2. **Build WAL records**: BEGIN, data records, COMMIT
3. **Write to WAL**: 
   - Multi-file: Append to WAL file
   - Single-file: Append to linear WAL buffer, update header
4. **Apply to delta**: Merge transaction state into global delta
5. **Create version chains**: If MVCC enabled and there are active readers
6. **Invalidate caches**: Clear affected nodes/edges from cache

### Non-MVCC Mode

Single transaction at a time (backward compatible).

### MVCC Mode

Concurrent transactions with snapshot isolation. Transaction sees data as of its start timestamp.

---

## API Layers

### Low-Level API (Direct Functions)

```typescript
// Lifecycle
openGraphDB(path, options) → GraphDB
closeGraphDB(db)

// Transactions
beginTx(db) → TxHandle
commit(handle)
rollback(handle)

// Node operations
createNode(handle, key?, labels?, props?) → NodeID
deleteNode(handle, nodeId)
getNodeByKey(db, key) → NodeID | null
nodeExists(db, nodeId) → boolean

// Edge operations
addEdge(handle, src, etype, dst)
deleteEdge(handle, src, etype, dst)
getNeighborsOut(db, nodeId) → {nodeId, etype}[]
getNeighborsIn(db, nodeId) → {nodeId, etype}[]

// Properties
setNodeProp(handle, nodeId, keyId, value)
getNodeProp(db, nodeId, keyId) → PropValue | null
```

### High-Level API (Drizzle-style)

```typescript
// Schema definition
const user = defineNode('user', {
  key: (id: string) => `user:${id}`,
  props: { name: string('name'), age: int('age') },
});

const follows = defineEdge('follows', {
  since: int('since'),
});

// Database instance
const db = await kite('./my-db', {
  nodes: [user],
  edges: [follows],
});

// CRUD with type inference
const alice = await db.insert(user).values({ key: '1', name: 'Alice', age: 30 }).returning();
const bob = await db.insert(user).values({ key: '2', name: 'Bob', age: 28 }).returning();
await db.link(alice, follows, bob, { since: 2024 });

// Traversal
const results = await db
  .from(alice)
  .out(follows)
  .toArray();

// Pathfinding
const path = await db
  .shortestPath(alice)
  .via(follows)
  .to(bob)
  .dijkstra();
```

---

## Why It's Fast

| Aspect | Design Choice | Benefit |
|--------|---------------|---------|
| **Storage** | Memory-mapped CSR | Zero-copy reads, OS handles caching |
| **Traversal** | Sorted adjacency lists | Cache-friendly sequential access |
| **Lookups** | Hash-bucketed key index | O(1) average key→node |
| **Embedded** | No network/IPC | Eliminates ~1ms per operation |
| **Writes** | WAL + async checkpoint | Durable without blocking reads |
| **Concurrency** | Lazy MVCC | No overhead in serial workloads |

### Benchmark Results (vs Memgraph)

At 100k nodes / 1M edges scale:

| Operation | RayDB Speedup |
|-----------|---------------|
| Key Lookups | ~624x faster |
| Traversals | ~52x faster |
| Edge Checks | ~164x faster |
| Multi-Hop | ~252x faster |
| Writes | ~1.5x faster |
| **Overall** | **~118x faster** |

The performance advantage comes primarily from:
1. **No network overhead** - embedded vs client-server
2. **Zero-copy mmap** - OS page cache vs application-level serialization
3. **CSR format** - optimal memory layout for graph traversal
