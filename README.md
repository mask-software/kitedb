# Kite - Embedded Graph Database

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
bun add @kitedb/core
```

Or for development:

```bash
git clone <repo>
cd raydb
bun install
```

## Browser (WASM) prototype

KiteDB can run in the browser via the WASI build of the core (`@kitedb/core`).
This uses an in-memory filesystem by default (ephemeral per page load).

Build the WASM bundle locally:

```bash
npm run build:wasm
```

Then import `@kitedb/core` in your browser bundler (it uses the `browser` entry).
Persistence in the browser requires wiring WASI to a persistent FS (e.g. OPFS/IndexedDB).
See the browser example in the Rust bindings package for a minimal demo (OPFS first, IndexedDB fallback).

## Quick Start

```typescript
import { Database } from '@kitedb/core';

// Open or create a single-file database
const db = Database.open('./my-graph.kitedb');

// Start a transaction
db.begin();

try {
  // Create nodes
  const alice = db.createNode('user:alice');
  const bob = db.createNode('user:bob');

  // Add an edge (create type by name)
  db.addEdgeByName(alice, 'KNOWS', bob);

  // Commit the transaction
  db.commit();
} catch (err) {
  db.rollback();
  throw err;
}

// Look up by key
const aliceNode = db.getNodeByKey('user:alice');
console.log('Alice node id:', aliceNode);

// Close the database
db.close();
```

## Documentation

See the full docs at [kitedb.vercel.com/docs](https://kitedb.vercel.com/docs).

## File Format

Kite uses a single-file format (`.kitedb`) for simpler deployment and backup.

A SQLite-style single-file database for simpler deployment and backup:

```typescript
import { Database } from '@kitedb/core';

// Open or create a single-file database
const db = Database.open('./my-graph.kitedb');

// Optional maintenance
db.optimizeSingleFile();
db.vacuumSingleFile();

// Close the database
db.close();
```

The `.kitedb` format contains:
- **Header (page 0)**: Magic, version, page size, snapshot/WAL locations
- **WAL Area**: Linear buffer for write-ahead log records (checkpoint to reclaim space)
- **Snapshot Area**: CSR snapshot data (mmap-friendly)

### Snapshot Section

- Magic: `GDS1`
- CSR (Compressed Sparse Row) format for edges
- In-edges and out-edges stored separately
- String table for interned strings
- Key index for fast lookups
- CRC32C integrity checking

### WAL Records

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
application must keep all data resident. You can `db.close()` to flush and
unmap the snapshot/WAL data, and later `Database.open(...)` on the same path to
read from the embedded on-disk snapshot again.

## License

MIT
