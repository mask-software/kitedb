# KiteDB Benchmarks

This document summarizes **measured** benchmark results. Raw outputs live in
`docs/benchmarks/results/` so we can trace every number back to an actual run.

> Latest numbers below were captured on **February 4, 2026**. Prior results
> from **February 3, 2026** are retained for comparison. If you need fresh
> numbers, rerun the commands in the next section and update this doc with the
> new output files.

## Test Environment

- Apple M4 (16GB)
- macOS 15.3 (Darwin 25.3.0)
- Rust 1.88.0
- Node 24.12.0
- Bun 1.3.5
- Python 3.12.8

## Running Benchmarks

### Rust (core, single-file raw)

```bash
cd ray-rs
cargo run --release --example single_file_raw_bench --no-default-features -- \
  --nodes 10000 --edges 50000 --iterations 10000
```

Optional knobs (Rust):
- `--edge-types N` (default: 3)
- `--edge-props N` (default: 10)
- `--sync-mode full|normal|off` (default: normal)
- `--group-commit-enabled`
- `--group-commit-window-ms N` (default: 2)

### Python bindings (single-file raw)

```bash
cd ray-rs/python/benchmarks
python3 benchmark_single_file_raw.py \
  --nodes 10000 --edges 50000 --iterations 10000
```

Optional knobs (Python):
- `--edge-types N` (default: 3)
- `--edge-props N` (default: 10)
- `--sync-mode full|normal|off` (default: normal)
- `--group-commit-enabled`
- `--group-commit-window-ms N` (default: 2)

### TypeScript API overhead (fluent vs low-level)

```bash
cd ray-rs
node --import @oxc-node/core/register benchmark/bench-fluent-vs-lowlevel.ts
```

### Vector index (Rust)

```bash
cd ray-rs
cargo run --release --example vector_bench --no-default-features -- \
  --vectors 10000 --dimensions 768 --iterations 1000 --k 10 --n-probe 10
```

### SQLite baseline (single-file raw)

```bash
cd docs/benchmarks
python3 sqlite_single_file_raw_bench.py \
  --nodes 10000 --edges 50000 --iterations 10000 --sync-mode normal
```

Notes (SQLite):
- WAL mode, `synchronous=normal`
- `temp_store=MEMORY`, `locking_mode=EXCLUSIVE`, `cache_size=256MB`
- WAL autocheckpoint disabled; `journal_size_limit` set to match WAL size
- Edge props stored in a separate table; edges use `INSERT OR IGNORE` and props use `INSERT OR REPLACE`

## Latest Results (2026-02-04)

Sync-mode sweep logs (nodes-only + edges-heavy datasets):

```
docs/benchmarks/results/2026-02-04-single-file-raw-rust-{nodes,edges}-{normal,full,off}-{gc,nogc}.txt
docs/benchmarks/results/2026-02-04-single-file-raw-python-{nodes,edges}-{normal,full,off}-{gc,nogc}.txt
docs/benchmarks/results/2026-02-04-bench-fluent-vs-lowlevel-{nodes,edges}-{normal,full,off}-{gc,nogc}.txt
```

Notes:
- Group commit only affects `SyncMode::Normal`; in `Full`/`Off` it is ignored.
- These runs disable auto-checkpoint (`--no-auto-checkpoint`) and use a 256MB WAL to expose raw commit costs.

### Edge Write Microbench (Rust, edges-heavy, sync=Normal, GC off)

Batch write p50/p95 (100 ops per batch):

| Operation | p50 | p95 |
|-----------|-----|-----|
| 100 nodes | 34.08us | 56.54us |
| 100 edges | 40.25us | 65.58us |
| 100 edges + props | 172.33us | 253.12us |

Raw log:

```
docs/benchmarks/results/2026-02-04-single-file-raw-rust-edges-normal-nogc.txt
```

### SQLite Baseline (single-file raw)

Batch write (100 nodes), sync=normal:

| Metric | Value |
|--------|-------|
| p50 | 120.67us |
| p95 | 2.98ms |

Raw logs:

```
docs/benchmarks/results/2026-02-04-sqlite-single-file-raw-{nodes,edges}-{normal,full,off}.txt
```

Large dataset sweep logs (100k nodes / 500k edges):

```
docs/benchmarks/results/2026-02-04-single-file-raw-rust-100k-500k-{normal,full,off}-{gc,nogc}.txt
docs/benchmarks/results/2026-02-04-single-file-raw-python-100k-500k-{normal,full,off}-{gc,nogc}.txt
docs/benchmarks/results/2026-02-04-bench-fluent-vs-lowlevel-100k-500k-{normal,full,off}-{gc,nogc}.txt
```

Notes (large dataset sweep):
- Config: 100k nodes, 500k edges, 3 edge types, 10 edge props, iterations=5k.
- WAL=1GB; auto-checkpoint disabled; Rust runs skip checkpoint.

### Sync Mode + Group Commit Sweep (Rust Core)

Config (nodes-only): 10k nodes, 0 edges, edge props=0, iterations=10k, WAL=256MB.

Batch write (100 nodes), nodes-only:

| Sync Mode | GC Off | GC On |
|-----------|--------|-------|
| Normal | 46.12us | 2.63ms |
| Full | 82.17us | 74.58us |
| Off | 39.38us | 51.58us |

Set vectors (batch 100), nodes-only:

| Sync Mode | GC Off | GC On |
|-----------|--------|-------|
| Normal | 102.71us | 2.79ms |
| Full | 160.33us | 166.54us |
| Off | 81.42us | 148.17us |

Config (edges-heavy): 10k nodes, 50k edges, 3 edge types, 10 edge props, iterations=10k, WAL=256MB.

Batch write (100 nodes), edges-heavy:

| Sync Mode | GC Off | GC On |
|-----------|--------|-------|
| Normal | 51.08us | 2.64ms |
| Full | 95.50us | 90.25us |
| Off | 43.29us | 36.42us |

Set vectors (batch 100), edges-heavy:

| Sync Mode | GC Off | GC On |
|-----------|--------|-------|
| Normal | 248.75us | 2.73ms |
| Full | 231.83us | 190.33us |
| Off | 75.33us | 80.54us |

### Sync Mode + Group Commit Sweep (Python Bindings)

Config (nodes-only): 10k nodes, 0 edges, edge props=0, iterations=10k, WAL=256MB.

Batch write (100 nodes), nodes-only:

| Sync Mode | GC Off | GC On |
|-----------|--------|-------|
| Normal | 66.96us | 2.63ms |
| Full | 85.62us | 90.79us |
| Off | 53.92us | 55.21us |

Set vectors (batch 100), nodes-only:

| Sync Mode | GC Off | GC On |
|-----------|--------|-------|
| Normal | 210.50us | 2.96ms |
| Full | 271.38us | 295.62us |
| Off | 209.83us | 208.46us |

Config (edges-heavy): 10k nodes, 50k edges, 3 edge types, 10 edge props, iterations=10k, WAL=256MB.

Batch write (100 nodes), edges-heavy:

| Sync Mode | GC Off | GC On |
|-----------|--------|-------|
| Normal | 57.79us | 2.62ms |
| Full | 95.38us | 81.96us |
| Off | 55.67us | 52.50us |

Set vectors (batch 100), edges-heavy:

| Sync Mode | GC Off | GC On |
|-----------|--------|-------|
| Normal | 207.38us | 2.79ms |
| Full | 262.38us | 264.25us |
| Off | 179.50us | 182.92us |

### Sync Mode + Group Commit Sweep (TypeScript Fluent vs Low-Level)

Config (nodes-only): 1k nodes, 0 edges, edge props=0, iterations=1k.

Insert p50 (low-level), nodes-only:

| Sync Mode | GC Off | GC On |
|-----------|--------|-------|
| Normal | 7.79us | 7.71us |
| Full | 28.54us | 28.63us |
| Off | 3.50us | 3.58us |

Config (edges-heavy): 1k nodes, 5k edges, 3 edge types, 10 edge props, iterations=1k.

Insert p50 (low-level), edges-heavy:

| Sync Mode | GC Off | GC On |
|-----------|--------|-------|
| Normal | 7.71us | 7.75us |
| Full | 28.50us | 28.04us |
| Off | 3.63us | 3.67us |

### Large Dataset Sweep (100k nodes / 500k edges)

#### Rust Core

Batch write p50 (100 nodes):

| Sync Mode | GC Off | GC On |
|-----------|--------|-------|
| Normal | 63.83us | 2.65ms |
| Full | 87.46us | 81.00us |
| Off | 60.17us | 50.17us |

Set vectors p50 (batch 100):

| Sync Mode | GC Off | GC On |
|-----------|--------|-------|
| Normal | 100.21us | 2.69ms |
| Full | 172.21us | 172.54us |
| Off | 82.46us | 81.21us |

#### Python Bindings

Batch write p50 (100 nodes):

| Sync Mode | GC Off | GC On |
|-----------|--------|-------|
| Normal | 73.67us | 2.63ms |
| Full | 109.00us | 122.92us |
| Off | 84.12us | 64.79us |

Set vectors p50 (batch 100):

| Sync Mode | GC Off | GC On |
|-----------|--------|-------|
| Normal | 220.71us | 2.81ms |
| Full | 334.08us | 282.58us |
| Off | 210.00us | 205.50us |

#### TypeScript Fluent vs Low-Level (Insert p50, low-level)

| Sync Mode | GC Off | GC On |
|-----------|--------|-------|
| Normal | 11.96us | 13.71us |
| Full | 34.29us | 36.38us |
| Off | 7.42us | 7.75us |

### Multi-writer Throughput (Rust Core, Normal Sync)

Config: 8 threads, 200 tx/thread, batch=200 nodes, edges/node=1, 3 edge types,
10 edge props, WAL=1GB.

| Group Commit | Tx Rate | Node Rate | Edge Rate |
|--------------|---------|-----------|-----------|
| Off | 724.42/s | 144.88K/s | 144.88K/s |
| On | 868.44/s | 173.69K/s | 173.69K/s |

## Prior Results (2026-02-03)

Raw logs:

- `docs/benchmarks/results/2026-02-03-single-file-raw-rust-gc.txt`
- `docs/benchmarks/results/2026-02-03-single-file-raw-rust-nogc.txt`
- `docs/benchmarks/results/2026-02-03-single-file-raw-python-gc.txt`
- `docs/benchmarks/results/2026-02-03-single-file-raw-python-nogc.txt`
- `docs/benchmarks/results/2026-02-03-bench-fluent-vs-lowlevel-gc.txt`
- `docs/benchmarks/results/2026-02-03-bench-fluent-vs-lowlevel-nogc.txt`
- `docs/benchmarks/results/2026-02-03-vector-bench-rust.txt`

### Single-File Raw (Rust Core)

Config: 10k nodes, 50k edges, 3 edge types, 10 edge props, 10k iterations,
vector dims=128, vector count=1k, sync_mode=Normal, group_commit=true.

| Operation | p50 | p95 |
|-----------|-----|-----|
| Key lookup (random existing) | 125ns | 250ns |
| 1-hop traversal (out) | 208ns | 333ns |
| Edge exists (random) | 83ns | 125ns |
| Batch write (100 nodes) | 3.09ms | 3.17ms |
| get_node_vector() | 125ns | 250ns |
| has_node_vector() | 42ns | 84ns |
| Set vectors (batch 100) | 3.77ms | 6.02ms |

### Single-File Raw (Python Bindings)

Config: 10k nodes, 50k edges, 3 edge types, 10 edge props, 10k iterations,
vector dims=128, vector count=1k, sync_mode=Normal, group_commit=true.

| Operation | p50 | p95 |
|-----------|-----|-----|
| Key lookup (random existing) | 209ns | 417ns |
| 1-hop traversal (out) | 458ns | 708ns |
| Edge exists (random) | 167ns | 291ns |
| Batch write (100 nodes) | 2.60ms | 2.65ms |
| get_node_vector() | 1.17us | 1.54us |
| has_node_vector() | 166ns | 167ns |
| Set vectors (batch 100) | 2.81ms | 5.92ms |

### TypeScript Fluent API vs Low-Level (NAPI)

Config: 1k nodes, 5k edges, 3 edge types, 10 edge props, 1k iterations,
sync_mode=Normal, group_commit=true.

| Operation | Low-level p50 | Fluent p50 | Overhead |
|-----------|---------------|------------|----------|
| Insert (single node + props) | 7.88us | 8.92us | 1.13x |
| Key lookup (get w/ props) | 208ns | 1.71us | 8.21x |
| Key lookup (getRef) | 208ns | 792ns | 3.81x |
| Key lookup (getId) | 208ns | 417ns | 2.00x |
| 1-hop traversal (count) | 875ns | 4.96us | 5.67x |
| 1-hop traversal (nodes) | 875ns | 4.83us | 5.52x |
| 1-hop traversal (toArray) | 875ns | 6.29us | 7.19x |
| Pathfinding BFS (depth 5) | 6.04us | 8.25us | 1.37x |

## Group Commit vs No Group Commit (Single-Threaded)

These runs use the same dataset/configs as above, with only group-commit toggled.
Group commit is optimized for **concurrent** writers; it can **increase** per-commit
latency in single-threaded benchmarks because commits may wait up to the window.

### Rust (Single-File Raw)

| Operation | Group Commit p50 | No Group Commit p50 |
|-----------|------------------|---------------------|
| Batch write (100 nodes) | 3.09ms | 42.54us |
| Set vectors (batch 100) | 3.77ms | 110.29us |

### Python (Single-File Raw)

| Operation | Group Commit p50 | No Group Commit p50 |
|-----------|------------------|---------------------|
| Batch write (100 nodes) | 2.60ms | 57.29us |
| Set vectors (batch 100) | 2.81ms | 221.96us |

### Vector Index (Rust)

Config: 10k vectors, 768 dims, 1k iterations, k=10, nProbe=10.

| Operation | p50 | p95 |
|-----------|-----|-----|
| Set vectors (10k) | 833ns | 2.12us |
| build_index() | 801.95ms | 801.95ms |
| get (random) | 167ns | 459ns |
| search (k=10, nProbe=10) | 557.54us | 918.79us |

## Notes

- These are **local** results. Expect variation across machines and datasets.
- We do **not** publish third-party comparisons here; run those yourself if needed.
