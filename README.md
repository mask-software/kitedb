# Kite - Embedded Graph Database

A high-performance embedded graph database written in Rust, with bindings for Node.js/Bun (TypeScript) and Python:

- **Fast reads** via mmap CSR (Compressed Sparse Row) snapshots
- **Reliable writes** via WAL (Write-Ahead Log) + in-memory delta overlay
- **Stable node IDs** that never change or get reused
- **Periodic compaction** to merge snapshots with deltas
- **MVCC** (on by default) for snapshot-isolated transactions and concurrent writers
- **Pathfinding** with Dijkstra and A* algorithms

## Features

- Zero-copy mmap reading of snapshot files
- ACID transactions with commit/rollback
- **MVCC (Multi-Version Concurrency Control)** for snapshot isolation, on by default
  since 0.3.0 (opening with `mvcc: false` is deprecated)
- Efficient CSR format for graph traversal
- Binary search for edge existence checks
- Key-based node lookup with hash index
- Node and edge properties
- In/out edge traversal
- **Graph pathfinding** (shortest path, weighted paths)
- Snapshot integrity checking

## Installation

```bash
bun add @kitedb/core
```

Or for development:

```bash
git clone https://github.com/mask-software/kitedb.git
cd kitedb
bun install
```

## Browser (WASM) prototype

There is no browser build on npm: `@kitedb/core` loads a native addon and runs on Node.js and Bun.
The repository has an unpublished WASI build of the core (`cd ray-rs && bun run build:wasm`, smoke test
`bun run test:wasm`, and a demo in `ray-rs/examples/browser` that persists to OPFS or IndexedDB). See
[ray-rs/README.md](ray-rs/README.md#browserwasi-builds) for what it leaves out.

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
- **Header (pages 0 and 1)**: two checksummed copies of the magic, format version, page size,
  snapshot/WAL locations, WAL salts and the WAL segment table; open uses the newest valid copy.
  A header naming WAL segments is format version 3 (a build that reads version 2 but not
  segments refuses it); otherwise it is version 2. No released version opens these files:
  v0.2.18 and earlier read neither the two header copies nor salted WAL records
- **WAL Area**: Linear buffer for write-ahead log records. When it fills, its records spill into a
  WAL segment and it starts over
- **WAL Segments**: extents of pages holding spilled log records, up to a limit
  (`walSegmentLimit`); a checkpoint covers and frees them, and a clean close leaves none
- **Snapshot Area**: CSR snapshot data (mmap-friendly)

Automatic checkpoints run on a thread of the database's own once the log (WAL segments and
WAL) reaches the checkpoint trigger: half the snapshot's size (`checkpointLogRatio`), at least
three eighths of the WAL (where earlier releases checkpointed), at most 128 MiB
(`checkpointLogBudget`). The in-memory delta takes about ten times the log's size, so the budget
bounds its memory while checkpoints keep up; writers that outrun them grow the log up to
`walSegmentLimit`, where they wait for a checkpoint.

### Snapshot Section

- Magic: `GDS1`
- CSR (Compressed Sparse Row) format for edges
- In-edges and out-edges stored separately
- String table for interned strings
- Key index for fast lookups
- CRC-32 integrity checking

### WAL Records

- 8-byte aligned records
- CRC-32 (IEEE) per record, XORed with the WAL region's salt, so a leftover record from an earlier
  WAL cycle fails its check like a torn one; WAL segments hold records unsalted, up to the length
  the header names
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
cd ray-rs
cargo run --release --example single_file_raw_bench --no-default-features -- \
  --nodes 10000 --edges 50000 --iterations 10000
node --import @oxc-node/core/register benchmark/bench-fluent-vs-lowlevel.ts
cargo run --release --example vector_bench --no-default-features -- \
  --vectors 10000 --dimensions 768 --iterations 1000 --k 10 --n-probe 10
python3 python/benchmarks/benchmark_single_file_raw.py \
  --nodes 10000 --edges 50000 --iterations 10000

# Type check
bun run tsc --noEmit
```

## License

MIT
