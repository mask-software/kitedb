# KiteDB Benchmarks

This document summarizes **measured** benchmark results. Raw outputs live in
`docs/benchmarks/results/` so we can trace every number back to an actual run.

> The results below come from runs dated **2026-02-03 through 2026-02-08**.
> Dates follow the log file names (UTC); some logs print a local timestamp from
> the evening before. The 2026-02-03 runs are kept under "Prior Results" for
> comparison. For fresh numbers, rerun the commands in the next section and
> update this doc with the new output files.

## Test Environment

- Apple M4 (16GB)
- macOS, Darwin 25.3.0
- Rust 1.88.0
- Node 24.12.0
- Bun 1.3.5
- Python 3.12.8

## Running Benchmarks

### Rust (core, single-file raw)

```bash
cd ray-rs
cargo run --release --example single_file_raw_bench --no-default-features -- \
  --nodes 10000 --edges 50000 --iterations 10000 \
  --wal-size 268435456 --no-auto-checkpoint --sync-mode normal
```

This is the configuration of the 2026-02-04 edges-heavy logs
(`2026-02-04-single-file-raw-rust-edges-*.txt`). The sweep ran it once per
`--sync-mode` value (`normal`, `full`, `off`), without and with
`--group-commit-enabled` (files ending in `-nogc` and `-gc`).

Flags for the other logged Rust runs:
- 100k nodes / 500k edges (`2026-02-04-single-file-raw-rust-100k-500k-*.txt`):
  `--nodes 100000 --edges 500000 --iterations 5000 --wal-size 1073741824 --no-auto-checkpoint --skip-checkpoint`,
  plus the same sync-mode and group-commit flags.
- 2026-02-03 prior results (`2026-02-03-single-file-raw-rust-{gc,nogc}.txt`):
  `--nodes 10000 --edges 50000 --iterations 10000`, plus `--group-commit-enabled`
  for the `-gc` log. These used the default 64MB WAL with auto-checkpoint on.

Optional knobs (Rust):
- `--edge-types N` (default: 3)
- `--edge-props N` (default: 10)
- `--sync-mode full|normal|off` (default: normal)
- `--group-commit-enabled`
- `--group-commit-window-ms N` (default: 2)
- `--wal-size BYTES` (default: 67108864)
- `--no-auto-checkpoint` (auto-checkpoint is on by default)
- `--skip-checkpoint` (skip the checkpoint between vector setup and the read benchmarks)

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
  --nodes 10000 --edges 50000 --iterations 10000 \
  --wal-size 268435456 --no-auto-checkpoint --sync-mode normal
```

This is the configuration of the 2026-02-04 edges-heavy logs
(`2026-02-04-single-file-raw-python-edges-*.txt`), swept over `--sync-mode` and
`--group-commit-enabled` the same way as the Rust runs.

Flags for the other logged Python runs:
- Nodes-only (`2026-02-04-single-file-raw-python-nodes-*.txt`): the command above
  plus `--edges 0 --edge-types 1 --edge-props 0`.
- 100k nodes / 500k edges (`2026-02-04-single-file-raw-python-100k-500k-*.txt`):
  `--nodes 100000 --edges 500000 --iterations 5000 --wal-size 1073741824 --no-auto-checkpoint`.
- 2026-02-03 prior results (`2026-02-03-single-file-raw-python-{gc,nogc}.txt`):
  `--nodes 10000 --edges 50000 --iterations 10000`, plus `--group-commit-enabled`
  for the `-gc` log. These used the default 64MB WAL with auto-checkpoint on.

The script also saves its output under `ray-rs/python/benchmarks/results/`
unless you pass `--output PATH` or `--no-output`.

Optional knobs (Python):
- `--edge-types N` (default: 3)
- `--edge-props N` (default: 10)
- `--sync-mode full|normal|off` (default: normal)
- `--group-commit-enabled`
- `--group-commit-window-ms N` (default: 2)
- `--wal-size BYTES` (default: 67108864)
- `--no-auto-checkpoint` (auto-checkpoint is on by default)
- `--skip-compact` (skip the compaction between vector setup and the read benchmarks)

### TypeScript API overhead (fluent vs low-level)

```bash
cd ray-rs
node --import @oxc-node/core/register benchmark/bench-fluent-vs-lowlevel.ts
```

The defaults (1k nodes, 5k edges, 3 edge types, 10 edge props, 1k iterations,
sync=normal, group commit off) match
`2026-02-04-bench-fluent-vs-lowlevel-edges-normal-nogc.txt`. The script accepts
`--nodes`, `--edges`, `--edge-types`, `--edge-props`, `--iterations`,
`--sync-mode`, `--group-commit-enabled`, and `--group-commit-window-ms`. The
other logs add:
- Sweep logs: `--sync-mode full|normal|off`, plus `--group-commit-enabled` for `-gc`.
- Nodes-only logs: `--edges 0 --edge-types 1 --edge-props 0`.
- 100k/500k logs: `--nodes 100000 --edges 500000`.
- 2026-02-03 logs: the defaults, plus `--group-commit-enabled` for `-gc`.

The script has no WAL or checkpoint flags; it opens both databases with a fixed
64MB WAL.

### Vector index (Rust)

```bash
cd ray-rs
cargo run --release --example vector_bench --no-default-features -- \
  --vectors 10000 --dimensions 768 --iterations 1000 --k 10 --n-probe 10
```

`vector_bench` builds `VectorIndex` with its default ANN algorithm and has no
flag to choose another. That default changed from IVF to IVF-PQ on 2026-02-08
(commit `b90b91e`), and then to `auto`, which builds plain IVF below 50,000
vectors or 512 dimensions and IVF-PQ from there on. With 10,000 vectors this
command measures IVF again, as the published vector results (2026-02-03) did.

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
- `max_fragments=8` reclaims more (`28.18%` / `44.18%`) but compacts more slowly
  (`22.29ms` vs `14.21ms` and `8.68ms` vs `4.33ms` at `min_deletion_ratio=0.30`, single runs).
- `min_deletion_ratio=0.40` can skip moderate-churn compaction (`delete_ratio=0.35`), so stale deleted bytes remain.
- Recommendation: keep defaults `min_deletion_ratio=0.30`, `max_fragments_per_compaction=4`, `min_vectors_to_compact=10000`.

### ANN algorithm matrix (Rust: IVF vs IVF-PQ)

Single run:

```bash
cd ray-rs
cargo run --release --example vector_ann_bench --no-default-features -- \
  --algorithm ivf_pq --vectors 20000 --dimensions 384 --queries 200 --k 10 --n-probe 16 \
  --pq-subspaces 48 --pq-centroids 256 --residuals false
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
- Current recommendation: use latency-first IVF-PQ as default ANN path with
  `residuals=false`, `pq_subspaces=48`, `pq_centroids=256`; monitor recall floor via ANN gate.
  (Superseded: `VectorIndex` now defaults to `auto`, plain IVF below 50,000 vectors or 512
  dimensions and IVF-PQ from there on, and IVF-PQ re-ranks its best candidates by exact
  distance. See the CHANGELOG.)

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
- `ALGORITHM=ivf_pq`, `RESIDUALS=false`, `PQ_SUBSPACES=48`, `PQ_CENTROIDS=256`
- `N_PROBE=16`, `ATTEMPTS=3`
- `MIN_RECALL_AT_K=0.16`
- `MAX_P95_MS=8.0`

Latest gate snapshot (2026-02-08): see `docs/benchmarks/results/2026-02-08-vector-ann-gate.attempt*.txt` (pass).

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

This reproduces `2026-02-05-index-pipeline-hypothesis-embed200.txt`. The
`embed50` log used `--tree-sitter-latency-ms 1 --scip-latency-ms 1 --embed-latency-ms 50`
with the other flags unchanged.

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

This reproduces `2026-02-04-sqlite-single-file-raw-edges-normal.txt`. The other
SQLite logs change `--sync-mode`; the nodes-only logs add `--edges 0 --edge-props 0`.

Notes (SQLite):
- WAL mode, `synchronous=normal`
- `temp_store=MEMORY`, `locking_mode=EXCLUSIVE`, `cache_size=256MB`
- WAL autocheckpoint disabled; `journal_size_limit` set to match WAL size
- Edge props stored in a separate table; edges use `INSERT OR IGNORE` and props use `INSERT OR REPLACE`

### RayDB vs Memgraph (local 1-hop traversal comparison)

This is a **local-only** comparison harness for your own machine. It builds the
same graph in both engines and benchmarks a query equivalent to:

`db.from(alice).out(Knows).toArray()`

Prerequisites:
- Memgraph running locally (default `127.0.0.1:7687`)

Run with your requested shape (10k nodes, 20k edges, alice fan-out 10) using the
Rust benchmark:

```bash
cd ray-rs
cargo run --release --example ray_vs_memgraph_bench --no-default-features -- \
  --nodes 10000 --edges 20000 --query-results 10 --iterations 5000
```

Adjust result cardinality to your `5-20` target:
- `--query-results 5`
- `--query-results 20`

Optional Python harness is still available at:
- `ray-rs/python/benchmarks/benchmark_raydb_vs_memgraph.py`

### RayDB vs Ladybug (local 1-hop traversal comparison)

This follows the same workload shape and query semantics as the Memgraph harness
above, but compares two embedded Rust engines.

Prerequisites:
- none (Ladybug runs in-process via the `lbug` Rust crate). The example needs the
  `bench-ladybug` feature, which builds Ladybug's C++ engine (several minutes the
  first time), so default builds and `cargo test` skip it.

Run with your requested shape (10k nodes, 20k edges, alice fan-out 10):

```bash
cd ray-rs
cargo run --release --example ray_vs_ladybug_bench --no-default-features --features bench-ladybug -- \
  --nodes 10000 --edges 20000 --query-results 10 --iterations 5000
```

Adjust result cardinality to your `5-20` target:
- `--query-results 5`
- `--query-results 20`

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
- Dataset: `NODES=10000`, `EDGES=0`, `EDGE_TYPES=1`, `EDGE_PROPS=0`, `VECTOR_COUNT=0`
- Primary rotation guardrail: `REPLICATION_SEGMENT_MAX_BYTES=1073741824`
- `ITERATIONS=20000`
- `SYNC_MODE=normal`
- `ATTEMPTS=7` (median ratio across attempts is used for pass/fail)
- Pass threshold: `P95_MAX_RATIO=1.30` (replication-on p95 / baseline p95)
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

#### Gate C: replication soak stability (lag churn + promote/reseed)

Exercises a `1 primary + 5 replicas` soak-style scenario with rotating lag churn,
periodic promotion fence checks, and reseed recovery under retention pressure.

```bash
cd ray-rs
./scripts/replication-soak-gate.sh
```

Defaults:
- `REPLICAS=5`
- `CYCLES=6`
- `COMMITS_PER_CYCLE=40`
- `ACTIVE_REPLICAS=3`
- `CHURN_INTERVAL=2`
- `PROMOTION_INTERVAL=3`
- `RESEED_CHECK_INTERVAL=2`
- `MAX_FRAMES=128`
- `RECOVERY_MAX_LOOPS=80`
- `SEGMENT_MAX_BYTES=1`
- `RETENTION_MIN=64`
- `ATTEMPTS=1`
- Pass threshold: `MAX_ALLOWED_LAG=1200`
- Pass threshold: `MIN_PROMOTIONS=2`
- Pass threshold: `MIN_RESEEDS=1`
- Invariant checks: divergence must be `0`, stale-fence rejections must equal promotions.

Example override:

```bash
cd ray-rs
CYCLES=18 COMMITS_PER_CYCLE=120 CHURN_INTERVAL=3 PROMOTION_INTERVAL=6 RESEED_CHECK_INTERVAL=3 MAX_ALLOWED_LAG=3000 ATTEMPTS=2 ./scripts/replication-soak-gate.sh
```

Output:
- `docs/benchmarks/results/YYYY-MM-DD-replication-soak-gate.txt` (single-attempt mode)
- `docs/benchmarks/results/YYYY-MM-DD-replication-soak-gate.attemptN.txt` (multi-attempt mode)
- `STAMP` can be overridden for run-scoped output naming (used by CI tracking jobs).

## Latest Results (2026-02-04 and 2026-02-05)

Sync-mode sweep logs:

```
docs/benchmarks/results/2026-02-04-single-file-raw-rust-edges-{normal,full,off}-{gc,nogc}.txt
docs/benchmarks/results/2026-02-04-single-file-raw-python-{nodes,edges}-{normal,full,off}-{gc,nogc}.txt
docs/benchmarks/results/2026-02-04-bench-fluent-vs-lowlevel-{nodes,edges}-{normal,full,off}-{gc,nogc}.txt
```

Notes:
- Group commit only affects `SyncMode::Normal`; in `Full`/`Off` it is ignored.
- The Rust and Python runs use a 256MB WAL with auto-checkpoint off
  (`--wal-size 268435456 --no-auto-checkpoint`) to expose raw commit costs. The
  TypeScript harness has no such flags and uses a fixed 64MB WAL.
- The `2026-02-04-single-file-raw-rust-nodes-*.txt` files are not nodes-only
  runs. Commit `f5beb7b` overwrote them with runs of the edges-heavy config (their
  headers show 50,000 edges, 3 edge types, 10 edge props). No nodes-only Rust log
  remains, so this doc publishes no nodes-only Rust numbers.

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

Batch write (100 nodes), edges-heavy dataset (10k nodes, 50k edges, 3 edge
types, 10 edge props), sync=normal:

| Metric | Value |
|--------|-------|
| p50 | 120.67us |
| p95 | 2.98ms |

Raw log for this table:

```
docs/benchmarks/results/2026-02-04-sqlite-single-file-raw-edges-normal.txt
```

All SQLite logs:

```
docs/benchmarks/results/2026-02-04-sqlite-single-file-raw-{nodes,edges}-{normal,full,off}.txt
```

### Sync Mode + Group Commit Sweep (Rust Core)

Config (edges-heavy): 10k nodes, 50k edges, 3 edge types, 10 edge props,
iterations=10k, WAL=256MB, auto-checkpoint off, checkpoint step not skipped.

Batch write (100 nodes) p50, edges-heavy:

| Sync Mode | GC Off | GC On |
|-----------|--------|-------|
| Normal | 34.08us | 2.57ms |
| Full | 54.92us | 61.42us |
| Off | 29.25us | 29.33us |

Set vectors (batch 100) p50, edges-heavy:

| Sync Mode | GC Off | GC On |
|-----------|--------|-------|
| Normal | 94.33us | 2.69ms |
| Full | 149.00us | 179.00us |
| Off | 71.17us | 70.62us |

No nodes-only Rust table: see the note at the top of this section.

### Sync Mode + Group Commit Sweep (Python Bindings)

Config (nodes-only): 10k nodes, 0 edges, 1 edge type, edge props=0,
iterations=10k, WAL=256MB, auto-checkpoint off.

Batch write (100 nodes) p50, nodes-only:

| Sync Mode | GC Off | GC On |
|-----------|--------|-------|
| Normal | 66.96us | 2.63ms |
| Full | 85.62us | 90.79us |
| Off | 53.92us | 55.21us |

Set vectors (batch 100) p50, nodes-only:

| Sync Mode | GC Off | GC On |
|-----------|--------|-------|
| Normal | 210.50us | 2.96ms |
| Full | 271.38us | 295.62us |
| Off | 209.83us | 208.46us |

Config (edges-heavy): 10k nodes, 50k edges, 3 edge types, 10 edge props,
iterations=10k, WAL=256MB, auto-checkpoint off.

Batch write (100 nodes) p50, edges-heavy:

| Sync Mode | GC Off | GC On |
|-----------|--------|-------|
| Normal | 49.71us | 2.62ms |
| Full | 95.38us | 81.96us |
| Off | 55.67us | 52.50us |

Set vectors (batch 100) p50, edges-heavy:

| Sync Mode | GC Off | GC On |
|-----------|--------|-------|
| Normal | 241.83us | 2.79ms |
| Full | 262.38us | 264.25us |
| Off | 179.50us | 182.92us |

The edges-heavy Normal / GC-off log is a rerun from commit `f5beb7b` that
replaced the original run; the other Python sweep logs are from the original
sweep (commit `13d5fb7`).

### Sync Mode + Group Commit Sweep (TypeScript Fluent vs Low-Level)

Config (nodes-only): 1k nodes, 0 edges, 1 edge type, edge props=0,
iterations=1k, 64MB WAL.

Insert p50 (low-level), nodes-only:

| Sync Mode | GC Off | GC On |
|-----------|--------|-------|
| Normal | 7.79us | 7.71us |
| Full | 28.54us | 28.63us |
| Off | 3.50us | 3.58us |

Config (edges-heavy): 1k nodes, 5k edges, 3 edge types, 10 edge props,
iterations=1k, 64MB WAL.

Insert p50 (low-level), edges-heavy:

| Sync Mode | GC Off | GC On |
|-----------|--------|-------|
| Normal | 7.71us | 7.75us |
| Full | 28.50us | 28.04us |
| Off | 3.63us | 3.67us |

### Large Dataset Sweep (100k nodes / 500k edges)

Logs:

```
docs/benchmarks/results/2026-02-04-single-file-raw-rust-100k-500k-{normal,full,off}-{gc,nogc}.txt
docs/benchmarks/results/2026-02-04-single-file-raw-python-100k-500k-{normal,full,off}-{gc,nogc}.txt
docs/benchmarks/results/2026-02-04-bench-fluent-vs-lowlevel-100k-500k-{normal,full,off}-{gc,nogc}.txt
```

Config:
- Rust and Python: 100k nodes, 500k edges, 3 edge types, 10 edge props,
  iterations=5k, WAL=1GB, auto-checkpoint off. Rust runs skip the checkpoint
  step (`--skip-checkpoint`); Python runs compact.
- TypeScript: 100k nodes, 500k edges, 3 edge types, 10 edge props,
  iterations=1k, 64MB WAL.

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

```bash
cd ray-rs
cargo run --release --example multi_writer_throughput_bench --no-default-features -- \
  --threads 8 --tx-per-thread 200 --batch-size 200 --edges-per-node 1 \
  --edge-types 3 --edge-props 10 --wal-size 1073741824 --sync-mode normal
```

Add `--group-commit-enabled` for the group-commit-on run.

| Group Commit | Tx Rate | Node Rate | Edge Rate |
|--------------|---------|-----------|-----------|
| Off | 724.42/s | 144.88K/s | 144.88K/s |
| On | 868.44/s | 173.69K/s | 173.69K/s |

Raw logs:

```
docs/benchmarks/results/2026-02-04-multi-writer-throughput-normal-{nogc,gc}.txt
```

#### Parallel write scaling notes

The thread-count sweep added on 2026-02-05 (1 to 16 writer threads, using
`multi_writer_throughput_bench` and `multi_writer_vector_throughput_bench`) has
no raw log in `docs/benchmarks/results/`, so its numbers are not published here.

Guidance that does not depend on that sweep: single-file commits are serialized
by `commit_lock` (`ray-rs/src/core/single_file/mod.rs`) to keep WAL and delta
ordering, so write throughput should not be expected to scale linearly with
writer threads. For maximum ingest, parallelize data preparation and funnel it
into one writer that commits batched transactions (or a small number of writers,
if you accept more contention).

### Index pipeline hypothesis notes (2026-02-05)

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
vector dims=128, vector count=1k, sync_mode=Normal, group_commit=true,
WAL=64MB, auto-checkpoint on.

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
vector dims=128, vector count=1k, sync_mode=Normal, group_commit=true,
WAL=64MB, auto-checkpoint on.

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

### Group Commit vs No Group Commit (Single-Threaded)

These runs use the same dataset/configs as above, with only group-commit toggled.
Group commit is optimized for **concurrent** writers; it can **increase** per-commit
latency in single-threaded benchmarks because commits may wait up to the window.

#### Rust (Single-File Raw)

| Operation | Group Commit p50 | No Group Commit p50 |
|-----------|------------------|---------------------|
| Batch write (100 nodes) | 3.09ms | 42.54us |
| Set vectors (batch 100) | 3.77ms | 110.29us |

#### Python (Single-File Raw)

| Operation | Group Commit p50 | No Group Commit p50 |
|-----------|------------------|---------------------|
| Batch write (100 nodes) | 2.60ms | 57.29us |
| Set vectors (batch 100) | 2.81ms | 221.96us |

### Vector Index (Rust)

Config: 10k vectors, 768 dims, 1k iterations, k=10, nProbe=10, cosine metric,
IVF index with 100 clusters.

Raw log: `docs/benchmarks/results/2026-02-03-vector-bench-rust.txt`

| Operation | p50 | p95 |
|-----------|-----|-----|
| Set vectors (10k) | 833ns | 2.12us |
| get (random) | 167ns | 459ns |
| search (k=10, nProbe=10) | 557.54us | 918.79us |

`build_index()`: 801.95ms (one build, so no percentiles).

These numbers are for IVF, which `vector_bench` builds again at this size now
that the default is `auto` (between 2026-02-08, commit `b90b91e`, and that
change it built IVF-PQ). See "Vector index (Rust)" under Running Benchmarks.

## Notes

- These are **local** results. Expect variation across machines and datasets.
- The SQLite baseline is included as a reference point. The Memgraph and Ladybug
  harnesses above are for running comparisons on your own machine; this doc
  publishes no numbers for other graph databases.
