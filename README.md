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
  Every header this version writes carries the magic `KiteDB format 2\0`. Releases v0.2.3 to
  v0.2.18 check a header only by its magic (`KiteDB format 1\0`) and checksum, read only the
  first header page and check no format version, so they refuse these files (invalid magic
  number), read-only and writable, instead of misreading them; earlier ones, whose magic was
  `RayDB format 1`, refuse them too. This version opens files v0.2.3 to v0.2.18 wrote (one header
  page; the first writable open migrates them) and files earlier unreleased builds wrote in the
  old magic (a writable open rewrites both header copies in the new one as its last step, so an
  open that fails leaves them in the old one; a crash between those two writes can leave page 0
  in the old magic, and v0.2.18 then opens the file as it was before that open, until the next
  writable open finishes). It refuses files of v0.1.4 to v0.2.2 (`RayDB format 1`), as releases
  have since v0.2.3, and an old-magic header that names WAL segments (only unreleased builds
  wrote one). A header copy in the new magic has a checksum over every byte of it but its two
  checksums (the fixed fields have their own), so a copy torn by a crash, at any 512-byte
  sector, fails it. A header naming WAL segments is format version 3 (minimum reader 3);
  otherwise it is version 2
- **WAL Area**: Linear buffer for write-ahead log records. When it fills, its records spill into a
  WAL segment and it starts over
- **WAL Segments**: extents of pages holding spilled log records, up to a limit
  (`walSegmentLimit`); a checkpoint covers and frees them, and a clean close checkpoints them away
  unless a transaction is open or that checkpoint fails
- **Snapshot Area**: CSR snapshot data (mmap-friendly)

Automatic checkpoints run on a thread of the database's own once the log (WAL segments and
WAL) reaches the checkpoint trigger: half the snapshot's size (`checkpointLogRatio`), at least
three eighths of the WAL (where earlier releases checkpointed), at most 128 MiB
(`checkpointLogBudget`). The in-memory delta takes about ten times the log's size, so the budget
bounds its memory while checkpoints keep up; writers that outrun them grow the log up to
`walSegmentLimit`, paced on the way (while a checkpoint runs past the trigger, each commit waits
up to 100 ms once done, so the room left lasts the run), and wait for a checkpoint only there.
Once per run a commit may also wait for the install, which holds the commit lock for a time that
grows with the delta (about 0.5-1 s at 1M nodes and 10M edges).

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
