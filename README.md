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

## File Format

The database stores files in the following structure:

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
bun run bench/benchmark-mvcc.ts

# Type check
bun run tsc --noEmit
```

## License

MIT
