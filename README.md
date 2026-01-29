# Ray - Embedded Graph Database

A high-performance embedded graph database for Bun/TypeScript with:

- **Fast reads** via mmap CSR (Compressed Sparse Row) snapshots
- **Reliable writes** via WAL (Write-Ahead Log) + in-memory delta overlay
- **Stable node IDs** that never change or get reused
- **Periodic compaction** to merge snapshots with deltas
- **MVCC** for concurrent transaction isolation
- **Pathfinding** with Dijkstra and A* algorithms
- **Caching** for frequently accessed nodes, edges, and properties

## Features

- Zero-copy mmap reading of snapshot files
- ACID transactions with commit/rollback
- **MVCC (Multi-Version Concurrency Control)** for snapshot isolation
- Efficient CSR format for graph traversal
- Binary search for edge existence checks
- Key-based node lookup with hash index
- Node and edge properties
- In/out edge traversal
- **Graph pathfinding** (shortest path, weighted paths)
- **Query result caching** with automatic invalidation
- Snapshot integrity checking

## Installation

```bash
bun add @ray-db/ray
```

Or for development:

```bash
git clone <repo>
cd ray
bun install
```

## Quick Start

```typescript
import {
  openGraphDB,
  closeGraphDB,
  beginTx,
  commit,
  createNode,
  addEdge,
  getNeighborsOut,
  defineEtype,
  getNodeByKey,
  optimize,
  listNodes,
  listEdges,
  countNodes,
  countEdges,
} from './src/index.ts';

// Open or create a database
const db = await openGraphDB('./my-graph');

// Start a transaction
const tx = beginTx(db);

// Define an edge type
const knows = defineEtype(tx, 'knows');

// Create nodes
const alice = createNode(tx, { key: 'user:alice' });
const bob = createNode(tx, { key: 'user:bob' });

// Add an edge
addEdge(tx, alice, knows, bob);

// Commit the transaction
await commit(tx);

// Query the graph
const friends = [...getNeighborsOut(db, alice, knows)];
console.log('Alice knows:', friends);

// Look up by key
const aliceNode = getNodeByKey(db, 'user:alice');

// Compact when needed (merges delta into snapshot)
await optimize(db);

// Close the database
await closeGraphDB(db);
```

## API Reference

### Database Lifecycle

```typescript
// Open a database
const db = await openGraphDB(path, {
  readOnly?: boolean,        // Open in read-only mode
  createIfMissing?: boolean, // Create if doesn't exist (default: true)
  lockFile?: boolean,        // Use file locking (default: true)
  legacyMultiFile?: boolean, // Allow legacy directory format (default: false)
  mvcc?: boolean,            // Enable MVCC for concurrent transactions
  cache?: boolean,           // Enable caching (default: false)
});

// Close the database
await closeGraphDB(db);
```

### Transactions

```typescript
// Begin a transaction
const tx = beginTx(db);

// Commit changes
await commit(tx);

// Or rollback
rollback(tx);

// With MVCC enabled, concurrent transactions are supported:
const tx1 = beginTx(db);  // Transaction 1
const tx2 = beginTx(db);  // Transaction 2 (concurrent)
```

### Node Operations

```typescript
// Create a node
const nodeId = createNode(tx, {
  key?: string,           // Optional unique key
  labels?: LabelID[],     // Optional labels
  props?: Map<PropKeyID, PropValue>, // Optional properties
});

// Delete a node
deleteNode(tx, nodeId);

// Check if node exists
nodeExists(db, nodeId);

// Look up by key
getNodeByKey(db, 'user:alice');

// List all nodes (lazy generator)
for (const nodeId of listNodes(db)) {
  console.log(nodeId);
}

// Count total nodes (O(1) optimized)
const totalNodes = countNodes(db);
```

### Edge Operations

```typescript
// Add an edge
addEdge(tx, srcId, etypeId, dstId);

// Delete an edge
deleteEdge(tx, srcId, etypeId, dstId);

// Check if edge exists
edgeExists(db, srcId, etypeId, dstId);

// Traverse out-neighbors
for (const edge of getNeighborsOut(db, nodeId)) {
  console.log(edge.src, edge.etype, edge.dst);
}

// Traverse in-neighbors
for (const edge of getNeighborsIn(db, nodeId)) {
  console.log(edge.src, edge.etype, edge.dst);
}

// Filter by edge type
for (const edge of getNeighborsOut(db, nodeId, knowsEtype)) {
  // Only edges of type 'knows'
}

// List all edges (lazy generator)
for (const edge of listEdges(db)) {
  console.log(`${edge.src} -> ${edge.dst}`);
}

// List edges of specific type
for (const edge of listEdges(db, { etype: knowsEtype })) {
  console.log(`${edge.src} knows ${edge.dst}`);
}

// Count total edges (O(1) optimized when unfiltered)
const totalEdges = countEdges(db);
const knowsCount = countEdges(db, { etype: knowsEtype });
```

### Schema Definitions

```typescript
// Define edge types
const knows = defineEtype(tx, 'knows');
const follows = defineEtype(tx, 'follows');

// Define labels
const person = defineLabel(tx, 'Person');

// Define property keys
const name = definePropkey(tx, 'name');
```

### Properties

```typescript
import { PropValueTag } from './src/index.ts';

// Set node property
setNodeProp(tx, nodeId, nameProp, {
  tag: PropValueTag.STRING,
  value: 'Alice'
});

// Delete node property
delNodeProp(tx, nodeId, nameProp);

// Set edge property
setEdgeProp(tx, src, etype, dst, weightProp, {
  tag: PropValueTag.F64,
  value: 0.5
});
```

### Maintenance

```typescript
// Get database stats
const s = stats(db);
console.log('Nodes:', s.snapshotNodes);
console.log('Edges:', s.snapshotEdges);
console.log('Recommend compact:', s.recommendCompact);

// Check database integrity
const result = check(db);
if (!result.valid) {
  console.error('Errors:', result.errors);
}

// Compact (merge delta into new snapshot)
await optimize(db);
```

### MVCC (Multi-Version Concurrency Control)

MVCC enables concurrent read and write transactions with snapshot isolation:

```typescript
// Open database with MVCC enabled
const db = await openGraphDB('./my-graph', { mvcc: true });

// Start concurrent transactions
const reader = beginTx(db);
const writer = beginTx(db);

// Reader sees consistent snapshot from its start time
const node = getNodeByKey(db, 'user:alice');

// Writer can modify data
setNodeProp(writer, nodeId, nameProp, { tag: PropValueTag.STRING, value: 'Updated' });
await commit(writer);

// Reader still sees old data (snapshot isolation)
// ...

// Conflict detection prevents lost updates
const tx1 = beginTx(db);
const tx2 = beginTx(db);
setNodeProp(tx1, nodeId, prop, value1);
setNodeProp(tx2, nodeId, prop, value2);
await commit(tx1);  // Succeeds
await commit(tx2);  // Throws ConflictError
```

### Pathfinding

Find shortest paths between nodes:

```typescript
import { ray, defineNode, defineEdge, prop } from './src/api';

const db = await ray('./my-graph', { nodes: [...], edges: [...] });

// Shortest path (unweighted)
const path = await db
  .from(startNode)
  .shortestPath(endNode)
  .via(edgeType)
  .maxDepth(10)
  .execute();

// Weighted shortest path (Dijkstra)
const weightedPath = await db
  .from(startNode)
  .shortestPath(endNode)
  .via(edgeType)
  .weight({ prop: distanceProp })
  .execute();

// A* pathfinding with heuristic
const astarPath = await db
  .from(startNode)
  .shortestPath(endNode)
  .via(edgeType)
  .weight({ prop: distanceProp })
  .heuristic((node) => estimateDistance(node, endNode))
  .execute();
```

### Caching

Enable caching for read-heavy workloads:

```typescript
import { 
  invalidateNodeCache, 
  invalidateEdgeCache, 
  clearCache, 
  getCacheStats 
} from './src/index.ts';

// Open with caching enabled
const db = await openGraphDB('./my-graph', { cache: true });

// Cache is automatically populated on reads and invalidated on writes

// Manual cache management
invalidateNodeCache(db, nodeId);
invalidateEdgeCache(db, srcId, etypeId, dstId);
clearCache(db);

// Get cache statistics
const cacheStats = getCacheStats(db);
console.log('Cache hits:', cacheStats.hits);
console.log('Cache misses:', cacheStats.misses);
```

## Fluent API

The fluent API provides a type-safe, ergonomic interface for graph operations with schema definitions:

```typescript
import { ray, defineNode, defineEdge, prop, optional } from '@ray-db/ray';

// Define schema
const user = defineNode('user', {
  key: (id: string) => `user:${id}`,
  props: {
    name: prop.string('name'),
    email: prop.string('email'),
    age: optional(prop.int('age')),
  },
});

const knows = defineEdge('knows', {
  since: prop.int('since'),
});

// Initialize database with schema
const db = await ray('./my-graph', {
  nodes: [user],
  edges: [knows],
});

// Insert nodes
const alice = await db
  .insert(user)
  .values({ key: 'alice', name: 'Alice', email: 'alice@example.com', age: 30n })
  .returning();

const bob = await db
  .insert(user)
  .values({ key: 'bob', name: 'Bob', email: 'bob@example.com' })
  .returning();

// Create edges
await db.link(alice).to(bob).via(knows).props({ since: 2024n }).execute();

// Query nodes
const foundUser = await db.get(user, 'alice');

// Lightweight reference lookup (no property loading)
const userRef = await db.getRef(user, 'alice');

// Traverse the graph
const friends = await db.from(alice).out(knows).toArray();
const friendCount = await db.from(alice).out(knows).count();

// Multi-hop traversal
const friendsOfFriends = await db
  .from(alice)
  .out(knows)
  .out(knows)
  .toArray();

// Traverse with depth range
const network = await db
  .from(alice)
  .traverse(knows, { direction: 'out', minDepth: 1, maxDepth: 3 })
  .toArray();

// Selective property loading
const namesOnly = await db
  .from(alice)
  .out(knows)
  .select(['name'])
  .toArray();

// Raw edge iteration (zero-copy)
for (const edge of db.from(alice).out(knows).rawEdges()) {
  console.log(edge.src, '->', edge.dst);
}

// Close database
await db.close();
```

### Listing and Counting

```typescript
// List all nodes of a type (lazy async generator)
for await (const u of db.all(user)) {
  console.log(u.name, u.email);
}

// Collect to array
const allUsers: typeof user[] = [];
for await (const u of db.all(user)) {
  allUsers.push(u);
}

// Count all nodes in database (O(1) - fast)
const totalNodes = await db.count();

// Count nodes of specific type (requires iteration)
const userCount = await db.count(user);

// List all edges (lazy async generator)
for await (const e of db.allEdges()) {
  console.log(`${e.src.$key} -> ${e.dst.$key}`);
}

// List edges of specific type with properties
for await (const e of db.allEdges(knows)) {
  console.log(`${e.src.$key} knows ${e.dst.$key} since ${e.props.since}`);
}

// Count all edges (O(1) - fast)
const totalEdges = await db.countEdges();

// Count edges of specific type
const knowsCount = await db.countEdges(knows);
```

### Performance Characteristics

The fluent API is optimized for minimal overhead compared to raw graph operations:

| Operation | Raw | Fluent | Overhead |
|-----------|-----|--------|----------|
| Insert (single) | 62µs | 65µs | **1.05x** |
| Key lookup (`getRef`) | 125ns | 250ns | **2.00x** |
| Key lookup (`get`) | 125ns | 1.5µs | 12x |
| 1-hop traversal `.count()` | 1.1µs | 2.0µs | **1.85x** |

**Performance tips:**

- Use `getRef()` instead of `get()` when you only need the node reference (not properties)
- Use `.count()` instead of `.toArray().length` for counting (optimized fast path)
- Use `.select(['prop1', 'prop2'])` to load only needed properties
- Use `.rawEdges()` for zero-copy edge iteration when you don't need node properties

### Running Benchmarks

```bash
# Full benchmark suite
bun run bench/benchmark-api-vs-raw.ts

# Custom parameters
bun run bench/benchmark-api-vs-raw.ts --nodes 10000 --edges 50000 --iterations 10000
```

## File Formats

Ray supports two storage formats. The directory-based format is legacy and will be
deprecated in favor of the single-file format.

### Single-File Format (`.raydb`) - Recommended

A SQLite-style single-file database for simpler deployment and backup:

```typescript
import {
  openSingleFileDB,
  closeSingleFileDB,
  optimizeSingleFile,
  vacuumSingleFile,
} from '@ray-db/ray';

// Open or create a single-file database
const db = await openSingleFileDB('./my-graph.raydb', {
  readOnly?: boolean,        // Open in read-only mode
  createIfMissing?: boolean, // Create if doesn't exist (default: true)
  lockFile?: boolean,        // Use file locking (default: true)
  pageSize?: number,         // Page size (default: 4096, must be power of 2)
  walSize?: number,          // WAL buffer size in bytes (default: 64MB)
  mvcc?: boolean,            // Enable MVCC
  cache?: CacheOptions,      // Enable caching
});

// All the same operations work (beginTx, createNode, etc.)

// Compact to merge delta into snapshot
await optimizeSingleFile(db);

// Vacuum to reclaim space
await vacuumSingleFile(db);

// Close the database
await closeSingleFileDB(db);
```

The single-file format contains:
- **Header (page 0)**: Magic, version, page size, snapshot/WAL locations
- **WAL Area**: Circular buffer for write-ahead log records
- **Snapshot Area**: CSR snapshot data (mmap-friendly)

### Multi-File Format (directory) - Deprecated (Legacy)

The original directory-based format (legacy). New deployments should use
the single-file `.raydb` format. A separate WAL file may be retained for
single-file performance in the future, but the directory format is no longer
recommended.

To open an existing legacy directory, pass `{ legacyMultiFile: true }` to
`openGraphDB`.

```
db/
  manifest.gdm           # Current snapshot and WAL info
  lock.gdl               # Optional file lock
  snapshots/
    snap_0000000000000001.gds
    snap_0000000000000002.gds
  wal/
    wal_0000000000000001.gdw
    wal_0000000000000002.gdw
```

### Snapshot Format (`.gds`)

- Magic: `GDS1`
- CSR (Compressed Sparse Row) format for edges
- In-edges and out-edges stored separately
- String table for interned strings
- Key index for fast lookups
- CRC32C integrity checking

### WAL Format (`.gdw`)

- Magic: `GDW1`
- 8-byte aligned records
- CRC32C per record
- Transaction boundaries (BEGIN/COMMIT/ROLLBACK)

## Development

```bash
# Run tests
bun test

# Run specific test file
bun test tests/snapshot.test.ts

# Run MVCC tests
bun test tests/mvcc.test.ts

# Run benchmarks
bun run bench/benchmark.ts
bun run bench/benchmark-mvcc-v2.ts

# Type check
bun run tsc --noEmit
```

### Benchmark memory usage

The MVCC v2 benchmark (`bench/benchmark-mvcc-v2.ts`) includes a `scale` scenario
that deliberately builds a ~1M node / 5M edge graph fully in memory to stress
snapshot layout and MVCC. On typical machines this will use several GB of RAM.
For local runs, you can reduce memory pressure by lowering `--scale-nodes`
(e.g. `--scale-nodes 200000`) or disabling the `scale` scenario via
`--scenarios warm,contested,version-chain`.

This benchmark measures the in-memory engine; it does **not** mean your
application must keep all data resident. You can `closeGraphDB(db)` to flush and
unmap the snapshot/WAL files, and later `openGraphDB` on the same path to read
from the embedded on-disk snapshot again.

## License

MIT
