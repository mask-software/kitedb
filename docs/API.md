# Kite API Documentation

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
│  Cache Layer (src/cache/)               │
│  - Property cache                       │
│  - Query cache                          │
│  - Traversal cache                      │
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

### 4. Cache Layer (`src/cache/`)

**Internal** - Provides caching for read-heavy workloads.

Components:

- `property-cache.ts` - Caches node and edge properties
- `query-cache.ts` - Caches query results
- `traversal-cache.ts` - Caches traversal results
- `index.ts` - CacheManager coordinator

### 5. Core Storage (`src/core/`)

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
├── cache/                 # Caching layer
│   ├── property-cache.ts  # Property caching
│   ├── query-cache.ts     # Query result caching
│   ├── traversal-cache.ts # Traversal caching
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
│   ├── lru.ts             # LRU cache
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
- **MVCC overhead**: ~0% for single transactions, minimal for concurrent
- **Pathfinding**: O((V + E) log V) for Dijkstra/A\*
- **Node count**: O(1) using snapshot metadata + delta adjustments
- **Edge count**: O(1) when unfiltered, O(n+m) when filtered by type
- **Node listing**: O(n) lazy generator, memory efficient
- **Edge listing**: O(n+m) lazy generator, memory efficient

## MVCC Details

### Snapshot Isolation

Each transaction sees a consistent snapshot of the database from its start time:

```typescript
const db = await openGraphDB("./db", { mvcc: true });

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

RayDB uses the single-file `.kitedb` format.

### Single-File Format (`.kitedb`)

```
mydb.kitedb
  Header (page 0)
  WAL Area (linear buffer; checkpoint to reclaim space)
  Snapshot Area (CSR)
```

### Snapshot Format (`.gds`)

- Magic: `GDS1`
- CSR (Compressed Sparse Row) format for edges
- Separate in-edge and out-edge indexes
- String table for interned strings
- Key index for fast lookups
- CRC32C integrity check

### WAL Format (`.gdw`)

- Magic: `GDW1`
- 8-byte aligned records
- CRC32C per record
- Transaction boundaries

## Getting Started

See `docs/api/README.md` for detailed API documentation and examples.

## References

- [Kite Main README](../README.md)
- [High-Level API Docs](./api/README.md)
- [TypeScript Docs](../tsconfig.json)
