# KiteDB Benchmarks

This document summarizes **measured** benchmark results. Raw outputs live in
`docs/benchmarks/results/` so we can trace every number back to an actual run.

> Latest numbers below were captured on **February 4-5, 2026**. Prior results
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

### Rust (replication catch-up throughput)

```bash
cd ray-rs
cargo run --release --example replication_catchup_bench --no-default-features -- \
  --seed-commits 1000 --backlog-commits 5000 --max-frames 256 --sync-mode normal
```

Key outputs:
- `primary_frames_per_sec`
- `catchup_frames_per_sec`
- `throughput_ratio` (`catchup/primary`)

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

### Vector compaction strategy (Rust)

```bash
cd ray-rs
cargo run --release --example vector_compaction_bench --no-default-features -- \
  --vectors 50000 --dimensions 384 --fragment-target-size 5000 \
  --delete-ratio 0.35 --min-deletion-ratio 0.30 --max-fragments 4 --min-vectors-to-compact 10000
```

Use this to compare compaction threshold tradeoffs before changing default vector/ANN maintenance policy.

Automated matrix sweep:

```bash
cd ray-rs
./scripts/vector-compaction-matrix.sh
```

Latest matrix snapshot (2026-02-08, 50k vectors, 384 dims, fragment target 5k):
- Result artifacts:
  - `docs/benchmarks/results/2026-02-08-vector-compaction-matrix.txt`
  - `docs/benchmarks/results/2026-02-08-vector-compaction-matrix.csv`
  - `docs/benchmarks/results/2026-02-08-vector-compaction-min-vectors-sweep.txt`
  - `docs/benchmarks/results/2026-02-08-vector-compaction-min-vectors-sweep.csv`
- `min_deletion_ratio=0.30`, `max_fragments=4` gives balanced reclaim/latency:
  - `delete_ratio=0.35`: `14.32%` reclaim (single-run latency in low-double-digit ms on this host)
  - `delete_ratio=0.55`: `22.24%` reclaim (single-run latency in single-digit ms on this host)
- `max_fragments=8` reclaims more (`28.18%` / `44.18%`) but roughly doubles compaction latency.
- `min_deletion_ratio=0.40` can skip moderate-churn compaction (`delete_ratio=0.35`), so stale deleted bytes remain.
- Recommendation: keep defaults `min_deletion_ratio=0.30`, `max_fragments_per_compaction=4`, `min_vectors_to_compact=10000`.

### ANN algorithm matrix (Rust: IVF vs IVF-PQ)

Single run:

```bash
cd ray-rs
cargo run --release --example vector_ann_bench --no-default-features -- \
  --algorithm ivf --vectors 20000 --dimensions 384 --queries 200 --k 10 --n-probe 8
```

Matrix sweep:

```bash
cd ray-rs
./scripts/vector-ann-matrix.sh
```

Latest matrix snapshot (2026-02-08, 20k vectors, 384 dims, 200 queries, k=10):
- Result artifacts:
  - `docs/benchmarks/results/2026-02-08-vector-ann-matrix.txt`
  - `docs/benchmarks/results/2026-02-08-vector-ann-matrix.csv`
- At same `n_probe`, IVF had higher recall than IVF-PQ in this baseline:
  - `n_probe=8`: IVF `0.1660`, IVF-PQ `0.1195` (`residuals=false`)
  - `n_probe=16`: IVF `0.2905`, IVF-PQ `0.1775` (`residuals=false`)
- IVF-PQ (`residuals=false`) had lower search p95 latency than IVF:
  - `n_probe=8`: `0.4508ms` vs IVF `0.7660ms`
  - `n_probe=16`: `1.3993ms` vs IVF `4.0272ms`
- IVF-PQ build time was much higher than IVF in this baseline.
- Current recommendation: keep IVF as default ANN path for quality-first behavior; revisit IVF-PQ default candidacy after PQ tuning (subspaces/centroids/probe) and workload-specific recall targets.

PQ tuning sweep:

```bash
cd ray-rs
./scripts/vector-ann-pq-tuning.sh
```

Latest tuning snapshot (2026-02-08):
- Result artifacts:
  - `docs/benchmarks/results/2026-02-08-vector-ann-pq-tuning.txt`
  - `docs/benchmarks/results/2026-02-08-vector-ann-pq-tuning.csv`
- Best recall-preserving PQ config in this sweep:
  - `residuals=false`, `pq_subspaces=48`, `pq_centroids=256`
  - `n_probe=8`: recall ratio vs IVF `0.6875`, p95 ratio vs IVF `0.6155`
  - `n_probe=16`: recall ratio vs IVF `0.6636`, p95 ratio vs IVF `0.4634`
- Current implication: this configuration is the best IVF-PQ candidate for latency-first profiles, but still below IVF recall in this workload.

CI tracking:
- Main workflow (`.github/workflows/ray-rs.yml`) includes non-blocking `ann-pq-tracking`
  (weekly schedule + manual dispatch) running `./scripts/vector-ann-pq-tuning.sh`.
- Results are uploaded as artifact `ann-pq-tracking-logs`.
- Tracking logs are run-scoped with stamp `ci-<run_id>-<run_attempt>`.
- Scheduled runs skip release/publish gating jobs; schedule path is tracking-only.
- Manual dispatch input `ann_pq_profile`:
  - `fast` (default): lightweight trend sweep.
  - `full`: deeper sweep (`RESIDUALS_SET=false true`) for investigation.

ANN quality/latency gate:

```bash
cd ray-rs
./scripts/vector-ann-gate.sh
```

Defaults:
- `ALGORITHM=ivf`, `N_PROBE=16`, `ATTEMPTS=3`
- `MIN_RECALL_AT_K=0.25`
- `MAX_P95_MS=6.0`

Latest gate snapshot (2026-02-08): median recall@k `0.2835`, median p95 `1.1716ms` (pass).

CI:
- Main-branch workflow (`.github/workflows/ray-rs.yml`) runs `./scripts/vector-ann-gate.sh`
  and uploads logs as artifact `ann-quality-gate-logs`.
- Gate logs are run-scoped with stamp `ci-<run_id>-<run_attempt>`.

### Index pipeline hypothesis (network-dominant)

```bash
cd ray-rs
cargo run --release --example index_pipeline_hypothesis_bench --no-default-features -- \
  --mode both --changes 200 --working-set 200 --vector-dims 128 \
  --tree-sitter-latency-ms 2 --scip-latency-ms 6 --embed-latency-ms 200 \
  --embed-batch-size 32 --embed-flush-ms 20 --embed-inflight 4 \
  --vector-apply-batch-size 64 --sync-mode normal
```

Interpretation:
- If `parallel` hot-path elapsed is much lower than `sequential`, async embed queueing is working.
- If `parallel` hot-path p95 is lower than `sequential`, TS+SCIP parallel parse plus unified graph commit is working.
- If `parallel` freshness p95 is too high, tune `--embed-batch-size`, `--embed-flush-ms`,
  and `--embed-inflight` (or reduce overwrite churn with larger working set / dedupe rules).
- Replacement ratio (`Queue ... replaced=...`) quantifies stale embed work eliminated by dedupe.

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

### Replication performance gates (Phase D carry-over)

Run both replication perf gates:

```bash
cd ray-rs
./scripts/replication-perf-gate.sh
```

#### Gate A: primary commit overhead

Compares write latency with replication disabled vs enabled (`role=primary`)
using the same benchmark harness.

```bash
cd ray-rs
./scripts/replication-bench-gate.sh
```

Defaults:
- Dataset: `NODES=10000`, `EDGES=50000`, `EDGE_TYPES=3`, `EDGE_PROPS=10`
- `ITERATIONS=20000`
- `SYNC_MODE=normal`
- `ATTEMPTS=7` (median ratio across attempts is used for pass/fail)
- Pass threshold: `P95_MAX_RATIO=1.03` (replication-on p95 / baseline p95)
- `ITERATIONS` must be `>= 100`

Example override:

```bash
cd ray-rs
ITERATIONS=2000 ATTEMPTS=5 P95_MAX_RATIO=1.05 ./scripts/replication-bench-gate.sh
```

Outputs:
- `docs/benchmarks/results/YYYY-MM-DD-replication-gate-baseline.txt` (single-attempt mode)
- `docs/benchmarks/results/YYYY-MM-DD-replication-gate-primary.txt` (single-attempt mode)
- `docs/benchmarks/results/YYYY-MM-DD-replication-gate-{baseline,primary}.attemptN.txt` (multi-attempt mode)
- `STAMP` can be overridden for run-scoped output naming (used by CI).

#### Gate B: replica catch-up throughput

Ensures replica catch-up throughput stays healthy relative to primary commit
throughput on the same workload.

```bash
cd ray-rs
./scripts/replication-catchup-gate.sh
```

Defaults:
- `SEED_COMMITS=1000`
- `BACKLOG_COMMITS=5000`
- `MAX_FRAMES=256`
- `SYNC_MODE=normal`
- `ATTEMPTS=3` (retry count for noisy host variance)
- Pass threshold: `MIN_CATCHUP_FPS=3000`
- Pass threshold: `MIN_THROUGHPUT_RATIO=0.13` (catch-up fps / primary fps)
- `BACKLOG_COMMITS` must be `>= 100`

Example override:

```bash
cd ray-rs
BACKLOG_COMMITS=10000 ATTEMPTS=5 MIN_THROUGHPUT_RATIO=1.10 ./scripts/replication-catchup-gate.sh
```

Output:
- `docs/benchmarks/results/YYYY-MM-DD-replication-catchup-gate.txt` (single-attempt mode)
- `docs/benchmarks/results/YYYY-MM-DD-replication-catchup-gate.attemptN.txt` (multi-attempt mode)
- `STAMP` can be overridden for run-scoped output naming (used by CI).

Notes:
- Gate A = commit-path overhead.
- Gate B = replica apply throughput.
- Keep replication correctness suite green alongside perf gates:
  - `cargo test --no-default-features --test replication_phase_a --test replication_phase_b --test replication_phase_c --test replication_phase_d --test replication_faults_phase_d`
  - `cargo test --no-default-features replication::`

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

#### Parallel write scaling notes (2026-02-05)

Single-file core write throughput does **not** scale linearly with writer threads because
commit/WAL + delta ordering is serialized (see `commit_lock` in `ray-rs/src/core/single_file/mod.rs`).
Best practice for max ingest: **parallelize prep**, funnel into **1 writer** doing **batched txns**
(or at most a small number of writers if you accept lower per-op latency / higher contention).

Hardware: 10 CPUs (local dev machine).

**Nodes + edges** (measured via `ray-rs/examples/multi_writer_throughput_bench.rs`, config:
`--tx-per-thread 400 --batch-size 500 --edges-per-node 1 --edge-types 3 --edge-props 0 --wal-size 268435456`):

Sync=Normal, GC off:

| Threads | Node rate |
|---------|-----------|
| 1 | 521.89K/s |
| 2 | 577.43K/s |
| 4 | 603.92K/s |
| 8 | 591.62K/s |
| 10 | 525.41K/s |
| 16 | 591.61K/s |

Sync=Off, GC off:

| Threads | Node rate |
|---------|-----------|
| 1 | 771.18K/s |
| 2 | 896.99K/s |
| 4 | 805.34K/s |
| 8 | 697.73K/s |
| 10 | 529.21K/s |
| 16 | 554.09K/s |

**Vector writes** (`set_node_vector`, dims=128) (measured via
`ray-rs/examples/multi_writer_vector_throughput_bench.rs`, config:
`--vector-dims 128 --tx-per-thread 200 --batch-size 500 --wal-size 1610612736 --sync-mode normal --no-auto-checkpoint`):

| Threads | Vector rate |
|---------|-------------|
| 1 | 529.31K/s |
| 2 | 452.36K/s |
| 4 | 388.78K/s |
| 8 | 349.01K/s |
| 10 | 313.67K/s |
| 16 | 296.99K/s |

#### Index pipeline hypothesis notes (2026-02-05)

Goal: validate whether remote embedding latency dominates enough that we should
decouple graph hot path from vector persistence using async batching + dedupe.

Harness:
- `ray-rs/examples/index_pipeline_hypothesis_bench.rs`
- Simulated tree-sitter + SCIP parse, graph writes, synthetic embed latency, batched vector apply.
- `sequential`: TS parse -> TS graph commit -> SCIP parse -> SCIP graph commit -> embed -> vector apply.
- `parallel`: TS+SCIP parse overlap -> unified graph commit -> async embed queue -> batched vector apply.

Sample runs (200 events, working set=200, batch=32, flush=20ms, inflight=4, vector-apply-batch=64):

| TS/SCIP parse | Embed latency | Mode | Hot path elapsed | Total elapsed | Hot p95 | Freshness p95 | Replaced jobs |
|---------------|---------------|------|------------------|---------------|---------|----------------|---------------|
| 1ms / 1ms | 50ms/batch | Sequential | 11.260s | 11.314s | 2.64ms | 55.09ms | n/a |
| 1ms / 1ms | 50ms/batch | Parallel | 0.255s | 0.329s | 1.30ms | 168.43ms | 6.00% |
| 2ms / 6ms | 200ms/batch | Sequential | 42.477s | 42.679s | 10.22ms | 205.11ms | n/a |
| 2ms / 6ms | 200ms/batch | Parallel | 1.448s | 1.687s | 7.60ms | 775.61ms | 5.50% |

Takeaway:
- Hot path throughput improves dramatically with async pipeline.
- Vector freshness depends on batching/queue pressure and overwrite churn; tune freshness separately
  from hot-path latency target.

Raw logs:
- `docs/benchmarks/results/2026-02-05-index-pipeline-hypothesis-embed50.txt`
- `docs/benchmarks/results/2026-02-05-index-pipeline-hypothesis-embed200.txt`

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
