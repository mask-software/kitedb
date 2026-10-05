# Kite API Documentation

> Note: The directory-based GraphDB (multi-file) engine has been removed. KiteDB is single-file (`.kitedb`) only; GraphDB references below are historical.

This document provides a high-level overview of Kite's architecture and API layers.

## Architecture Overview

Kite is organized into several key layers:

```
┌─────────────────────────────────────────┐
│  Application Code                       │
└──────────────────┬──────────────────────┘
                   │
┌──────────────────▼──────────────────────┐
│  High-Level API (src/api/)              │
│  - Type-safe schema definitions         │
│  - Fluent query builders                │
│  - Graph traversal & pathfinding        │
└──────────────────┬──────────────────────┘
                   │
┌──────────────────▼──────────────────────┐
│  Core Database (graph-db/)              │
│  - Low-level CRUD operations            │
│  - Transaction management               │
│  - Node/edge IDs                        │
└──────────────────┬──────────────────────┘
                   │
┌──────────────────▼──────────────────────┐
│  MVCC Layer (src/mvcc/)                 │
│  - Version chains                       │
│  - Transaction isolation                │
│  - Conflict detection                   │
│  - Garbage collection                   │
└──────────────────┬──────────────────────┘
                   │
┌──────────────────▼──────────────────────┐
│  Storage Layer (src/core/)              │
│  - WAL (Write-Ahead Log)                │
│  - Snapshots (CSR format)               │
│  - Compaction                           │
└──────────────────┬──────────────────────┘
                   │
┌──────────────────▼──────────────────────┐
│  Utilities (src/util/)                  │
│  - Binary encoding                      │
│  - Compression                          │
│  - CRC checksums                        │
│  - File locking                         │
└─────────────────────────────────────────┘
```

## Fast Writes (Single-File)

Recommended profile for high write throughput:

- `sync_mode = Normal`
- Commit from several threads: commits that arrive while others are written are written
  together, in any sync mode, with one WAL write, one header write and (in `Full` mode) one
  fsync for the group. This is always on; `group_commit_enabled` and
  `group_commit_window_ms` have no effect, and no commit waits for others to join
- The default 4 MB WAL is enough for heavy ingest: when it fills, its records spill into a WAL
  segment (a copy and three syncs), and automatic checkpoints run on a thread of the database's
  own, without holding up commits. The WAL size sets more than how often it spills: the floor of
  the checkpoint trigger (three eighths of the WAL), of the segment limit (16 WALs) and of the
  segment extent (two WALs). A larger `wal_size` means fewer spills, and more log in memory
  before a small database checkpoints
- Checkpoints start once the log (WAL segments and WAL) reaches `checkpoint_log_ratio` (default
  0.5) of the snapshot's size, at least three eighths of the WAL (where earlier releases
  checkpointed) and at most `checkpoint_log_budget` (default 128 MiB). The in-memory delta takes
  about ten times the log's size, so the budget bounds that memory (about 1.3 GB at the default)
  while checkpoints keep up; lower it to cap memory and reopen replay time. Writers that outrun
  checkpoints grow the log up to `wal_segment_limit` (default: twice the trigger, at least 16
  WALs, at most four times the budget) and wait for a checkpoint only there
- `checkpoint_threshold` is deprecated and has no effect

Durability note: `Normal` mode does not `fsync` on every commit. An OS crash can
lose recent commits, but application crashes are recovered via WAL replay.

## API Layers

### 1. High-Level API (`src/api/`)

**For application developers** - Recommended for most use cases.

Features:

- Type-safe schema definitions (`node`, `edge`)
- Fluent query builders (insert, update, delete)
- Graph traversal with filtering
- **Pathfinding** (Dijkstra, A\*)
- Automatic type inference
- Transaction support
- Property type validation

**Modules:**

- `kite.ts` - Main database context
- `schema.ts` - Schema builders
- `builders.ts` - Query builders
- `traversal.ts` - Graph traversal
- `pathfinding.ts` - Shortest path algorithms
- `index.ts` - Public exports

**Example:**

```typescript
const user = node("user", {
  key: (id: string) => `user:${id}`,
  props: { name: string("name") },
});

const db = await kite("./db", { nodes: [user], edges: [] });
const alice = await db
  .insert(user)
  .values({ key: "alice", name: "Alice" })
  .returning();
```

### 2. Low-Level API (`graph-db/`)

**For advanced users and framework builders** - Direct database access.

Provides:

- `GraphDB` - Raw database handle
- Node/edge CRUD with numeric IDs
- Transaction primitives (`beginTx`, `commit`, `rollback`)
- Property access (get/set)
- Edge queries and traversal
- **Node/edge listing and counting**
- Database maintenance
- **MVCC transaction support**

**Key types:**

- `NodeID` - Numeric node identifier (number)
- `ETypeID` - Edge type identifier
- `PropKeyID` - Property key identifier
- `TxHandle` - Transaction handle

**Example:**

```typescript
const db = await openGraphDB("./db");
const tx = beginTx(db);

const alice = createNode(tx, { key: "user:alice" });
const bob = createNode(tx, { key: "user:bob" });

const knows = defineEtype(tx, "knows");
addEdge(tx, alice, knows, bob);

await commit(tx);

// List and count nodes/edges
for (const nodeId of listNodes(db)) {
  console.log("Node:", nodeId);
}

for (const edge of listEdges(db, { etype: knows })) {
  console.log(`${edge.src} knows ${edge.dst}`);
}

console.log("Total nodes:", countNodes(db));
console.log("Total edges:", countEdges(db));
```

### 3. MVCC Layer (`src/mvcc/`)

**Internal** - Provides Multi-Version Concurrency Control.

Components:

- `tx-manager.ts` - Transaction lifecycle and ID assignment
- `version-chain.ts` - Version history for nodes/edges/properties
- `visibility.ts` - Snapshot isolation visibility rules
- `conflict-detector.ts` - Read-write and write-write conflict detection
- `gc.ts` - Garbage collection of old versions
- `index.ts` - MvccManager coordinator

**Key concepts:**

- **Snapshot Isolation** - Each transaction sees a consistent snapshot
- **Version Chains** - Historical versions linked in a chain
- **Conflict Detection** - Prevents lost updates on concurrent modifications
- **Garbage Collection** - Automatically prunes old versions

### 4. Core Storage (`src/core/`)

**Internal** - Handles persistence and optimization.

Components:

- `wal.ts` - Write-Ahead Log for durability
- `snapshot-reader.ts` / `snapshot-writer.ts` - CSR snapshot format
- `compactor.ts` - Merges deltas into new snapshots
- `delta.ts` - In-memory delta overlay
- `manifest.ts` - Database metadata

## File Structure

```
src/
├── api/                    # High-level API
│   ├── README.md          # API documentation
│   ├── kite.ts            # Main database context
│   ├── schema.ts          # Schema definitions
│   ├── builders.ts        # Query builders
│   ├── traversal.ts       # Graph traversal
│   ├── pathfinding.ts     # Shortest path algorithms
│   └── index.ts           # Exports
│
├── graph-db/              # Low-level database
│   ├── nodes.ts           # Node operations
│   ├── edges.ts           # Edge operations
│   ├── tx.ts              # Transaction management
│   ├── lifecycle.ts       # DB open/close
│   └── index.ts           # Exports
│
├── mvcc/                  # MVCC layer
│   ├── tx-manager.ts      # Transaction management
│   ├── version-chain.ts   # Version history
│   ├── visibility.ts      # Snapshot isolation
│   ├── conflict-detector.ts # Conflict detection
│   ├── gc.ts              # Garbage collection
│   └── index.ts           # Exports
│
├── core/                  # Storage layer
│   ├── wal.ts             # Write-ahead log
│   ├── snapshot-reader.ts # Snapshot reading
│   ├── snapshot-writer.ts # Snapshot writing
│   ├── compactor.ts       # Compaction
│   ├── delta.ts           # Delta overlay
│   ├── manifest.ts        # Metadata
│   └── index.ts           # Exports
│
├── util/                  # Utilities
│   ├── compression.ts     # Compression
│   ├── binary.ts          # Binary encoding
│   ├── crc.ts             # Checksums
│   ├── hash.ts            # Hashing
│   ├── lock.ts            # File locks
│   └── index.ts           # Exports
│
├── check/                 # Verification
│   └── checker.ts         # Integrity checking
│
├── index.ts               # Main entry point
└── types.ts               # Type definitions
```

## Choosing the Right API

### Use High-Level API (`src/api/`)

✅ You're building an application
✅ You want type safety and ergonomics
✅ You want automatic property type handling
✅ You want traversal with filtering
✅ You want comfortable error handling

```typescript
import { kite, node, edge, prop } from "./src/api";
```

### Use Low-Level API (`graph-db/`)

✅ You're building a framework or tool
✅ You need maximum control
✅ You want to work with numeric IDs directly
✅ You're implementing custom traversal logic
✅ You need escape-hatch access to raw operations
✅ You need MVCC transaction control

```typescript
import { openGraphDB, createNode, addEdge } from "./src/graph-db";
```

### Use Raw Database via Escape Hatch

```typescript
const raw: GraphDB = db.$raw;
// Now you can use low-level APIs directly
```

## Common Patterns

### Define a Schema

```typescript
const user = node("user", {
  key: (id: string) => `user:${id}`,
  props: {
    name: string("name"),
    email: string("email"),
    created: int("created"),
  },
});

const knows = edge("knows", {
  since: int("since"),
  confidence: optional(float("confidence")),
});
```

### CRUD Operations

```typescript
// Create
const alice = await db
  .insert(user)
  .values({
    key: "alice",
    name: "Alice",
    email: "alice@example.com",
    created: Date.now(),
  })
  .returning();

// Read
const retrieved = await db.get(user, "alice");

// Update
await db.update(user, "alice").setAll({ name: "Alice Updated" }).execute();

// Delete
const success = db.delete(user, "alice");
```

### Relationships

```typescript
const alice = await db.get(user, "alice");
const bob = await db.get(user, "bob");

// Link
await db.link(alice, knows, bob, { since: 2020 });

// Query
const friends = await db.from(alice).out(knows).nodes().toArray();

// Unlink
await db.unlink(alice, knows, bob);
```

### Transactions

```typescript
await db.transaction(async (ctx) => {
  const alice = await ctx
    .insert(user)
    .values({ key: "alice", name: "Alice", email: "..." })
    .returning();

  const bob = await ctx
    .insert(user)
    .values({ key: "bob", name: "Bob", email: "..." })
    .returning();

  await ctx.link(alice, knows, bob);
});
// All committed or all rolled back
```

## Type Inference

The API uses TypeScript's advanced type system for full inference:

```typescript
const user = node("user", {
  key: (id: string) => `user:${id}`,
  props: {
    name: string("name"),
    age: optional(int("age")),
  },
});

// Inferred insert type
type InsertUser = InferNodeInsert<typeof user>;
// { key: string; name: string; age?: number; }

// Inferred return type
type User = InferNode<typeof user>;
// { id: number; key: string; name: string; age?: number; }

// Inferred edge props
const knows = edge("knows", { since: int("since") });
type KnowsProps = InferEdgeProps<typeof knows>;
// { since: number; }
```

## Property Types

Property builders are available as top-level exports or under `prop` (e.g. `string()` / `prop.string()`).

| Type            | TypeScript | Storage | Notes            |
| --------------- | ---------- | ------- | ---------------- |
| `string()` | `string`   | UTF-8   | Interned strings |
| `int()`    | `number`   | i64     | 64-bit signed    |
| `float()`  | `number`   | f64     | IEEE 754         |
| `bool()`   | `boolean`  | bool    | True/false       |

Optional properties can be omitted or set to `undefined`.

## Performance Characteristics

- **Node creation**: O(1)
- **Edge creation**: O(log n) with CSR compaction
- **Key lookup**: O(1) average with hash index
- **Edge existence**: O(log n) binary search on CSR
- **Traversal**: O(k) where k = number of edges
- **Snapshot read**: Zero-copy mmap
- **MVCC overhead**: single-thread reads within about 2-7% of non-MVCC mode; a single small writer about 7-8% slower
- **Pathfinding**: O((V + E) log V) for Dijkstra/A\*
- **Node count**: O(1) using snapshot metadata + delta adjustments
- **Edge count**: O(1) when unfiltered, O(n+m) when filtered by type
- **Node listing**: O(n) lazy generator, memory efficient
- **Edge listing**: O(n+m) lazy generator, memory efficient

## MVCC Details

MVCC is on by default since 0.3.0. Opening with `mvcc: false` (Rust `.mvcc(false)`,
Python `OpenOptions(mvcc=False)`) is deprecated and will be removed in a later release;
without MVCC, write transactions run one at a time and transactions read the latest
committed state. The file format is the same in both modes.

### Snapshot Isolation

Each transaction sees a consistent snapshot of the database from its start time, plus its
own writes. Reads outside a transaction see the latest committed state:

```typescript
const db = await openGraphDB("./db"); // MVCC is on by default

const tx1 = beginTx(db); // Snapshot at time T1
const tx2 = beginTx(db); // Snapshot at time T1 (same)

// tx2 modifies data
setNodeProp(tx2, node, prop, newValue);
await commit(tx2); // Commits at time T2

// tx1 still sees data from T1 (before tx2's changes)
const value = getNodeProp(db, node, prop); // Old value
```

### Conflict Detection

MVCC uses optimistic concurrency control with conflict detection at commit:

```typescript
// Write-write conflict
const tx1 = beginTx(db);
const tx2 = beginTx(db);

setNodeProp(tx1, node, prop, "value1");
setNodeProp(tx2, node, prop, "value2");

await commit(tx1); // Succeeds
await commit(tx2); // Throws ConflictError

// Read-write conflict (if tx reads then another tx writes and commits)
const txReader = beginTx(db);
getNodeProp(db, node, prop); // Reads value

const txWriter = beginTx(db);
setNodeProp(txWriter, node, prop, "new");
await commit(txWriter); // Commits

setNodeProp(txReader, node, prop, "other");
await commit(txReader); // Throws ConflictError (read was invalidated)
```

### Performance Optimizations

MVCC includes several optimizations:

- **Fast path for single transactions**: Skips version chain creation when no concurrent readers
- **Cached MVCC flag**: O(1) check for MVCC mode
- **Inverted write index**: O(1) conflict detection
- **Background garbage collection**: Prunes old versions automatically

## Pathfinding

### Shortest Path (Unweighted)

```typescript
const path = await db
  .from(startNode)
  .shortestPath(endNode)
  .via(edgeType)
  .execute();

// Returns: { nodes: [...], edges: [...], distance: number }
```

### Weighted Shortest Path (Dijkstra)

```typescript
const path = await db
  .from(startNode)
  .shortestPath(endNode)
  .via(edgeType)
  .weight({ prop: distanceProp }) // Use edge property as weight
  .execute();
```

### A\* Pathfinding

```typescript
const path = await db
  .from(startNode)
  .shortestPath(endNode)
  .via(edgeType)
  .weight({ prop: distanceProp })
  .heuristic((node) => {
    // Estimate remaining distance (must be admissible)
    return estimateDistance(node, endNode);
  })
  .execute();
```

### Path Options

```typescript
const path = await db
  .from(startNode)
  .shortestPath(endNode)
  .via(edgeType)
  .maxDepth(10)           // Limit search depth
  .direction('out')       // 'out', 'in', or 'both'
  .filter((node) => ...)  // Filter nodes during traversal
  .execute();
```

## File Formats

KiteDB uses the single-file `.kitedb` format.

### Single-File Format (`.kitedb`)

```
mydb.kitedb
  Header (pages 0 and 1: two checksummed copies; open uses the newest valid one)
  WAL Area (linear buffer; spills into WAL segments when full)
  Snapshot Area (CSR)
  WAL Segments (extents named by the header's segment table; a checkpoint frees them)
```

Every header this version writes carries the magic `KiteDB format 2\0`. Releases up to v0.2.18
check a header only by its magic (`KiteDB format 1\0`) and the checksum of its first 176 bytes,
read only the first header page, and check no format version: they refuse files this version
writes with an invalid magic number error, read-only and writable, instead of misreading them.
The other way, this version opens files v0.2.18 and earlier wrote (one header page: the first
writable open migrates them to two) and files unreleased builds wrote in the old magic (a
writable open first rewrites both header slots in the new magic; a read-only open writes
nothing).

The format version is 2, or 3 while the header names WAL segments (minimum reader version 3), so
a build that reads version 2 but not segments refuses such a file (version mismatch) rather than
miss the commits in its segments. A checkpoint that covers every segment writes version 2 again,
as a clean close does unless a transaction is open or that checkpoint fails. Snapshots and WAL
segments take the first free range that holds them, else the end of the file.

### Snapshot Section

- Magic: `GDS1`
- CSR (Compressed Sparse Row) format for edges
- Separate in-edge and out-edge indexes
- String table for interned strings
- Key index for fast lookups
- CRC-32 (IEEE) integrity check

### WAL Records

- 8-byte aligned records
- CRC-32 (IEEE) per record, XORed with the WAL region's salt (stored in the header), so a
  leftover record from an earlier WAL cycle fails its check like a torn one
- WAL segments hold records unsalted, synced before a header names them; recovery reads each
  up to the byte length the header names, then the WAL, and replays the transactions that
  commit after the segments the snapshot covers
- Transaction boundaries (BEGIN/COMMIT/ROLLBACK)

## Getting Started

See `docs/api/README.md` for detailed API documentation and examples.

## References

- [Kite Main README](../README.md)
- [High-Level API Docs](./api/README.md)
- [TypeScript Docs](../tsconfig.json)
