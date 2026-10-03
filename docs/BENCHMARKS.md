# KiteDB Benchmarks

This document summarizes **measured** benchmark results. Raw outputs live in
`docs/benchmarks/results/` so we can trace every number back to an actual run.

> **Latest results: 2026-10-03**, Apple M5 Pro, MVCC on, measured with
> `ray-rs/scripts/bench-refresh.sh` (see "Latest results"). The February 2026
> runs (Apple M4, MVCC off, and the group-commit option of the time) are kept
> under "Prior results (2026-02, Apple M4, MVCC off)" for comparison. The
> vector compaction, ANN matrix, index pipeline and replication gate snapshots
> under "Running Benchmarks" carry their own dates. Dates follow the log file
> names (UTC).

## Test Environment

Latest results (from the log headers):
- Apple M5 Pro, 15 cores (5 Super + 10 Performance), 48 GB
- macOS 26.6 (Darwin 25.6.0); power: AC Power, energy mode high power
- Rust 1.88.0, Node 24.21.0, Python 3.12.8, SQLite 3.47.1

Prior results (2026-02): Apple M4 (16GB), macOS (Darwin 25.3.0), Rust 1.88.0,
Node 24.12.0, Bun 1.3.5, Python 3.12.8.

## Running Benchmarks

### Everything the docs publish (bench-refresh.sh)

```bash
ray-rs/scripts/bench-refresh.sh --list    # the matrix, one line per configuration
ray-rs/scripts/bench-refresh.sh --smoke   # tiny sizes: checks that every command runs
ray-rs/scripts/bench-refresh.sh           # the published run
cd ray-docs && bun run bench:data         # the site's data from the new logs
```

The script builds the release binaries and bindings, refuses to start in Low
Power Mode, on battery, with uncommitted changes, or while other processes keep
more than 20% of the CPUs busy (it waits for that, and pauses between runs
too), runs 5 interleaved rounds with seed 42, and writes
`docs/benchmarks/results/<date>-<name>.txt` plus a `<date>-bench-refresh.txt`
manifest. Each log header names the commit, toolchain, machine, energy mode and
the exact command. The script header lists its options. The sections below are
the individual commands.

### Rust (core, single-file raw)

```bash
cd ray-rs
cargo run --release --example single_file_raw_bench --no-default-features -- \
  --nodes 10000 --edges 50000 --iterations 10000 \
  --wal-size 268435456 --no-auto-checkpoint --seed 42 --sync-mode normal --mvcc
```

This is the configuration of the latest graph results
(`2026-10-03-single-file-raw-rust-mvcc-{normal,full,off}.txt`, one per
`--sync-mode`, and `2026-10-03-single-file-raw-rust-nomvcc-normal.txt` with
`--no-mvcc`). The prior 2026-02-04 edges-heavy logs
(`2026-02-04-single-file-raw-rust-edges-*.txt`) used the same flags without
`--seed 42 --mvcc` (MVCC was off by default), once per `--sync-mode` value,
without and with `--group-commit-enabled` (files ending in `-nogc` and `-gc`).

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
- `--mvcc` / `--no-mvcc` (default: the library default, MVCC on)
- `--seed N` (default: 42): the graph, vectors and the keys and nodes the read
  benchmarks pick
- `--group-commit-enabled`, `--group-commit-window-ms N`: accepted for old
  command lines, no effect (every commit is group-committed)
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
  --wal-size 268435456 --no-auto-checkpoint --seed 42 --sync-mode normal --mvcc
```

Build the bindings with `maturin develop --release --features python` first; a
debug build is several times slower. This is the configuration of
`2026-10-03-single-file-raw-python-mvcc-normal.txt`. The prior 2026-02-04
edges-heavy logs (`2026-02-04-single-file-raw-python-edges-*.txt`) used it
without `--seed 42 --mvcc`, swept over `--sync-mode` and
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
- `--mvcc` / `--no-mvcc` (default: the library default, MVCC on)
- `--seed N` (default: 42)
- `--group-commit-enabled`, `--group-commit-window-ms N`: accepted for old
  command lines, no effect (every commit is group-committed)
- `--wal-size BYTES` (default: 67108864)
- `--no-auto-checkpoint` (auto-checkpoint is on by default)
- `--skip-compact` (skip the compaction between vector setup and the read benchmarks)

### TypeScript API overhead (fluent vs low-level)

```bash
cd ray-rs
node --import @oxc-node/core/register benchmark/bench-fluent-vs-lowlevel.ts --mvcc --seed 42
```

It needs the release addon and the TS build (`bun run build`, which runs
`napi build --platform --release` and `bun run build:ts`). The defaults (1k
nodes, 5k edges, 3 edge types, 10 edge props, 1k iterations, sync=normal) plus
`--mvcc --seed 42` match `2026-10-03-bench-fluent-vs-lowlevel-mvcc-normal.txt`; the
defaults alone (MVCC off then) match
`2026-02-04-bench-fluent-vs-lowlevel-edges-normal-nogc.txt`. The script accepts
`--nodes`, `--edges`, `--edge-types`, `--edge-props`, `--iterations`,
`--sync-mode`, `--mvcc` / `--no-mvcc`, and `--seed N` (default: 42);
`--group-commit-enabled` and `--group-commit-window-ms` are accepted for old
command lines and have no effect. The other logs add:
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
  --vectors 10000 --dimensions 768 --iterations 1000 --k 10 --n-probe 10 --seed 42 --no-output
```

This is the configuration of `2026-10-03-vector-bench-rust.txt`; the 2026-02-03
log used it without `--seed 42` (the index training was unseeded then).

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

This reproduces `2026-10-03-sqlite-single-file-raw-edges-normal.txt` and, on the
prior machine, `2026-02-04-sqlite-single-file-raw-edges-normal.txt`. The other
2026-02-04 SQLite logs change `--sync-mode`; the nodes-only logs add
`--edges 0 --edge-props 0`.

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
- `NODE_BATCHES=2000`: commits in the timed 100-node batch-write sample whose
  p95 the gate compares (must be `>= 200`; a p95 over fewer commits is a handful
  of outliers)
- `SYNC_MODE=normal`
- `ATTEMPTS=7` (median ratio across attempts is used for pass/fail)
- Pass threshold: `P95_MAX_RATIO=1.30` (replication-on p95 / baseline p95)

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
- `SEGMENT_MAX_BYTES=67108864`, `RETENTION_MIN=20000`
- `ATTEMPTS=3` (retry count for noisy host variance)
- Pass threshold: `MIN_CATCHUP_FPS=2000`
- Pass threshold: `MIN_THROUGHPUT_RATIO=0.09` (catch-up fps / primary fps)
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
- Pass threshold: `MIN_RESEEDS=0`
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

## Latest results (October 3, 2026, Apple M5 Pro, MVCC on)

Measured with `ray-rs/scripts/bench-refresh.sh` at commit `e3b064c` on an Apple
M5 Pro, 15 cores (5 Super + 10 Performance), 48 GB, macOS 26.6 (Darwin 25.6.0),
power: AC Power, energy mode high power; Rust 1.88.0, Node 24.21.0, Python
3.12.8, SQLite 3.47.1. MVCC is on (the default) unless a table says otherwise;
group commit is always on. Each configuration ran 5 interleaved rounds with seed
42 (3 rounds for `query_core_bench` and `vector_ann_bench`, 1 for
`mvcc_overhead_bench`, which repeat internally). Each row below is the line of
the round with the median value, copied from the log, as on the docs site
(`ray-docs`, `bun run bench:data`). Every log header names the exact command.

The machine differs from the prior results (Apple M4, 16 GB), so a change
against them mixes code and hardware; the SQLite baseline, rerun on this
machine, is the same-hardware reference. macOS's clock ticks every 41.67 ns, so
42 ns, the smallest nonzero latency these benches report, means one tick or
less. "Set vectors" times 10 batches (1,000 vectors), so its p95 is close to its
slowest batch.

### Graph latency (Rust core)

Config: 10k nodes, 50k edges, 3 edge types, 10 edge props, 1k vectors of 128
dims, sync=normal, MVCC on; 10k iterations, 256MB WAL, auto-checkpoint off.

| Operation | p50 | p95 | p50, MVCC off | p95, MVCC off |
|---|---|---|---|---|
| Key lookup (random existing key) | 42 ns | 209 ns | 42 ns | 208 ns |
| 1-hop traversal (out, random node) | 42 ns | 125 ns | 42 ns | 166 ns |
| Edge exists (random pair) | 42 ns | 83 ns | 42 ns | 83 ns |
| Batch write (100 nodes) | 16.33 µs | 36.00 µs | 16.88 µs | 35.96 µs |
| Batch write (100 edges) | 13.33 µs | 18.04 µs | 13.71 µs | 20.50 µs |
| Batch write (100 edges + props) | 63.79 µs | 79.71 µs | 62.04 µs | 76.79 µs |
| Get vector (random) | 83 ns | 166 ns | 83 ns | 166 ns |
| Has vector (random) | 42 ns | 42 ns | 42 ns | 42 ns |
| Set vectors (batch of 100) | 67.46 µs | 338.21 µs | 62.71 µs | 193.17 µs |

```
docs/benchmarks/results/2026-10-03-single-file-raw-rust-mvcc-normal.txt
docs/benchmarks/results/2026-10-03-single-file-raw-rust-nomvcc-normal.txt
```

### Graph latency (Python bindings)

Config: 10k nodes, 50k edges, 3 edge types, 10 edge props, 1k vectors of 128
dims, sync=normal, MVCC on.

| Operation | p50 | p95 |
|---|---|---|
| Key lookup (random existing key) | 125 ns | 291 ns |
| 1-hop traversal (out, random node) | 291 ns | 417 ns |
| Edge exists (random pair) | 125 ns | 125 ns |
| Batch write (100 nodes) | 25.33 µs | 53.71 µs |
| Batch write (100 edges) | 18.50 µs | 24.62 µs |
| Batch write (100 edges + props) | 206.12 µs | 219.25 µs |
| Get vector (random) | 709 ns | 834 ns |
| Has vector (random) | 125 ns | 125 ns |
| Set vectors (batch of 100) | 105.79 µs | 419.96 µs |

```
docs/benchmarks/results/2026-10-03-single-file-raw-python-mvcc-normal.txt
```

### Sync modes (Rust core, MVCC on)

Batch writes on the same graph in each sync mode. `full` uses plain `fsync`,
which on macOS leaves writes in the drive's cache (see `full_fsync` below for
`F_FULLFSYNC`).

| Sync mode | 100 nodes p50 | 100 nodes p95 | 100 edges p50 | Set vectors (100) p50 |
|---|---|---|---|---|
| `normal` | 16.33 µs | 36.00 µs | 13.33 µs | 67.46 µs |
| `full` | 76.00 µs | 94.50 µs | 49.62 µs | 165.08 µs |
| `off` | 13.96 µs | 38.67 µs | 12.88 µs | 59.00 µs |

```
docs/benchmarks/results/2026-10-03-single-file-raw-rust-mvcc-normal.txt
docs/benchmarks/results/2026-10-03-single-file-raw-rust-mvcc-full.txt
docs/benchmarks/results/2026-10-03-single-file-raw-rust-mvcc-off.txt
```

### SQLite baseline (single-file raw)

Batch write (100 nodes), 10k nodes, 50k edges, 3 edge types, 10 edge props, WAL
mode, synchronous=normal, SQLite 3.47.1, same machine:

| Engine | p50 | p95 |
|---|---|---|
| SQLite | 151.62 µs | 187.54 µs |
| KiteDB (Rust, MVCC on) | 16.33 µs | 36.00 µs |

```
docs/benchmarks/results/2026-10-03-sqlite-single-file-raw-edges-normal.txt
```

### TypeScript fluent vs low-level API

Config: 1k nodes, 5k edges, 3 edge types, 10 edge props, 1k iterations,
sync=normal, MVCC on, Node 24.21.0.

| Operation | Low-level p50 | Fluent p50 | Overhead |
|---|---|---|---|
| Insert (single node + props) | 5.67 µs | 6.83 µs | 1.21x |
| Key lookup (get, with props) | 167 ns | 1.25 µs | 7.49x |
| Key lookup (getRef, no props) | 167 ns | 584 ns | 3.50x |
| Key lookup (getId, id only) | 167 ns | 292 ns | 1.75x |
| 1-hop traversal (count) | 750 ns | 1.17 µs | 1.56x |
| 1-hop traversal (node ids) | 750 ns | 1.29 µs | 1.72x |
| 1-hop traversal (toArray, with props) | 750 ns | 3.29 µs | 4.39x |
| Pathfinding BFS (max depth 5) | 3.63 µs | 2.88 µs | 0.79x |

```
docs/benchmarks/results/2026-10-03-bench-fluent-vs-lowlevel-mvcc-normal.txt
```

### Vector index (Rust)

Config: 10k vectors, 768 dims, 1k iterations, k=10, nProbe=10, cosine, IVF with
100 clusters.

| Operation | p50 | p95 |
|---|---|---|
| Set (10,000 vectors, one at a time) | 834 ns | 1.71 µs |
| build_index() (one build) | 84.88 ms | n/a |
| Get (random) | 166 ns | 334 ns |
| Search | 144.12 µs | 182.25 µs |

```
docs/benchmarks/results/2026-10-03-vector-bench-rust.txt
```

IVF vs IVF-PQ (`vector_ann_bench`): 50k vectors of 768 dims (lowrank dataset),
200 queries, k=10, nProbe=10, 223 clusters, IVF-PQ with 48 subspaces and the
default re-rank.

| Algorithm | Build | Search p50 | Search p95 | Recall@10 |
|---|---|---|---|---|
| IVF | 1.25 s | 543.63 µs | 717.54 µs | 1.0000 |
| IVF-PQ | 4.78 s | 199.75 µs | 241.50 µs | 0.9395 |

```
docs/benchmarks/results/2026-10-03-vector-ann-768d-ivf.txt
docs/benchmarks/results/2026-10-03-vector-ann-768d-ivf-pq.txt
```

### Write scaling (Rust core, MVCC on)

Transactions per second with 1, 4 and 8 writer threads
(`multi_writer_throughput_bench`, 1 GB WAL, auto-checkpoint off):

- 200-node transactions: 200 nodes and 200 edges (10 props each) per transaction, 200 transactions per writer
- 1-node transactions: one keyed node per transaction, 50,000 transactions per writer

| Transactions | Sync | 1 writer | 4 writers | 8 writers | 8 vs 1 |
|---|---|---|---|---|---|
| 200-node transactions | `normal` | 5.13K/s | 10.53K/s | 13.18K/s | 2.6x |
| 200-node transactions | `full` | 3.44K/s | 6.50K/s | 10.20K/s | 3.0x |
| 1-node transactions | `normal` | 417.89K/s | 380.91K/s | 376.42K/s | 0.9x |
| 1-node transactions | `full` | 25.50K/s | 43.26K/s | 59.75K/s | 2.3x |

One writer with MVCC off (write transactions then run one at a time),
sync=normal: 6.11K/s for 200-node transactions, 435.47K/s for 1-node
transactions.

`full` with `full_fsync` (`F_FULLFSYNC`, which flushes the drive's cache),
1-node transactions: 1 writer 276.06/s, 8 writers 1.16K/s.

```
docs/benchmarks/results/2026-10-03-multi-writer-throughput-mvcc-normal-200node-1w.txt
docs/benchmarks/results/2026-10-03-multi-writer-throughput-mvcc-normal-200node-4w.txt
docs/benchmarks/results/2026-10-03-multi-writer-throughput-mvcc-normal-200node-8w.txt
docs/benchmarks/results/2026-10-03-multi-writer-throughput-mvcc-full-200node-1w.txt
docs/benchmarks/results/2026-10-03-multi-writer-throughput-mvcc-full-200node-4w.txt
docs/benchmarks/results/2026-10-03-multi-writer-throughput-mvcc-full-200node-8w.txt
docs/benchmarks/results/2026-10-03-multi-writer-throughput-mvcc-normal-1node-1w.txt
docs/benchmarks/results/2026-10-03-multi-writer-throughput-mvcc-normal-1node-4w.txt
docs/benchmarks/results/2026-10-03-multi-writer-throughput-mvcc-normal-1node-8w.txt
docs/benchmarks/results/2026-10-03-multi-writer-throughput-mvcc-full-1node-1w.txt
docs/benchmarks/results/2026-10-03-multi-writer-throughput-mvcc-full-1node-4w.txt
docs/benchmarks/results/2026-10-03-multi-writer-throughput-mvcc-full-1node-8w.txt
docs/benchmarks/results/2026-10-03-multi-writer-throughput-nomvcc-normal-200node-1w.txt
docs/benchmarks/results/2026-10-03-multi-writer-throughput-nomvcc-normal-1node-1w.txt
docs/benchmarks/results/2026-10-03-multi-writer-throughput-mvcc-fullfsync-1node-1w.txt
docs/benchmarks/results/2026-10-03-multi-writer-throughput-mvcc-fullfsync-1node-8w.txt
```

### Open time and paging (Rust core, MVCC on)

Read-only open + close of a checkpointed database (`query_core_bench --sections
open`, median of 9 per round):

| Database | Open + close |
|---|---|
| 100k nodes, 500k edges | 2.06 ms |
| 1M nodes, 5M edges | 18.31 ms |

```
docs/benchmarks/results/2026-10-03-query-core-open-100k.txt
docs/benchmarks/results/2026-10-03-query-core-open-1m.txt
```

Pages, counts and hub reads (`query_core_bench`, median of 7 per round; paging:
1000000 nodes, 5000000 edges, page size 100, pages 1 and 1000; types: 100000
nodes over 5 types, all(T2) and count_nodes_by_type(T2); take(1) from a node
with 200000 out-edges), with every change still in the WAL and after a
checkpoint:

| Operation | In the WAL | After a checkpoint |
|---|---|---|
| Node page 1 (100 nodes) | 4.95 ms | 500 ns |
| Node page 1000 | 4.82 ms | 600 ns |
| Edge page 1 (100 edges) | 1.43 ms | 1.50 µs |
| Edge page 1000 | 1.32 ms | 1.60 µs |
| Count nodes (page total) | 9.91 ms | 0 ns |
| Count edges (page total) | 17.77 ms | 0 ns |
| Count nodes of one type | 3.27 ms | 2.38 ms |
| First neighbor of a 200k-edge hub, take(1) | 300 ns | 500 ns |
| All neighbors of a 200k-edge hub | 597.00 µs | 1.17 ms |

```
docs/benchmarks/results/2026-10-03-query-core-paging.txt
```

### MVCC cost

`mvcc_overhead_bench` (modes=off,on nodes=20000 edges/node=8 props/node=4
duration=1s repeat=5 history=2000x8 sync=Off cpus=15), operations per second:

| Workload | MVCC off | MVCC on | Change |
|---|---|---|---|
| node_prop, 1 reader | 54.61M/s | 55.31M/s | +1.3% |
| node_props, 1 reader | 11.47M/s | 11.56M/s | +0.7% |
| out_edges, 1 reader | 11.78M/s | 11.95M/s | +1.4% |
| node_prop in read transactions, 1 reader | 50.21M/s | 49.30M/s | -1.8% |
| node_prop, 8 readers | 12.60M/s | 12.56M/s | -0.3% |
| node_prop, 4 readers beside 1 writer | 5.75M/s | 5.44M/s | -5.3% |
| Update one prop, 1 writer | 1.50M/s | 1.26M/s | -15.5% |
| Insert a node + prop + edge, 1 writer | 709.63K/s | 584.63K/s | -17.6% |

```
docs/benchmarks/results/2026-10-03-mvcc-overhead.txt
```

### Bulk load

`bulk_load_bench`: 200,000 nodes and 1,000,000 edges with 2 props each, in bulk
transactions of 5,000, sync=normal, 64MB WAL:

| Mode | Nodes/s | Edges/s |
|---|---|---|
| MVCC on | 3.05M/s | 1.05M/s |
| MVCC off | 3.05M/s | 1.06M/s |
| MVCC on, a read transaction open | 3.05M/s | 690.23K/s |

```
docs/benchmarks/results/2026-10-03-bulk-load-mvcc.txt
docs/benchmarks/results/2026-10-03-bulk-load-nomvcc.txt
docs/benchmarks/results/2026-10-03-bulk-load-mvcc-reader.txt
```

## Prior results (2026-02, Apple M4, MVCC off)

These runs predate MVCC as the default and the group commit of every commit:
they ran without MVCC, and the `-gc` logs used the group-commit option of the
time, which in sync=normal made a single writer wait up to its 2 ms window. They
ran on an Apple M4 (16GB), so they compare with the latest results only
loosely.

### Runs of 2026-02-04 and 2026-02-05

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

#### Edge Write Microbench (Rust, edges-heavy, sync=Normal, GC off)

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

#### SQLite Baseline (single-file raw)

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

#### Sync Mode + Group Commit Sweep (Rust Core)

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

#### Sync Mode + Group Commit Sweep (Python Bindings)

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

#### Sync Mode + Group Commit Sweep (TypeScript Fluent vs Low-Level)

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

#### Large Dataset Sweep (100k nodes / 500k edges)

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

##### Rust Core

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

##### Python Bindings

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

##### TypeScript Fluent vs Low-Level (Insert p50, low-level)

| Sync Mode | GC Off | GC On |
|-----------|--------|-------|
| Normal | 11.96us | 13.71us |
| Full | 34.29us | 36.38us |
| Off | 7.42us | 7.75us |

#### Multi-writer Throughput (Rust Core, Normal Sync)

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

##### Parallel write scaling notes

The thread-count sweep added on 2026-02-05 (1 to 16 writer threads, using
`multi_writer_throughput_bench` and `multi_writer_vector_throughput_bench`) has
no raw log in `docs/benchmarks/results/`, so its numbers are not published here.

At the time, commits were serialized by `commit_lock`, and the advice was to
funnel writes through one writer. Writer threads now build MVCC transactions in
parallel and commit in groups: see "Write scaling" under "Latest results".

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

### Runs of 2026-02-03

Raw logs:

- `docs/benchmarks/results/2026-02-03-single-file-raw-rust-gc.txt`
- `docs/benchmarks/results/2026-02-03-single-file-raw-rust-nogc.txt`
- `docs/benchmarks/results/2026-02-03-single-file-raw-python-gc.txt`
- `docs/benchmarks/results/2026-02-03-single-file-raw-python-nogc.txt`
- `docs/benchmarks/results/2026-02-03-bench-fluent-vs-lowlevel-gc.txt`
- `docs/benchmarks/results/2026-02-03-bench-fluent-vs-lowlevel-nogc.txt`
- `docs/benchmarks/results/2026-02-03-vector-bench-rust.txt`

#### Single-File Raw (Rust Core)

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

#### Single-File Raw (Python Bindings)

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

#### TypeScript Fluent API vs Low-Level (NAPI)

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

#### Group Commit vs No Group Commit (Single-Threaded)

These runs use the same dataset/configs as above, with only group-commit toggled.
Group commit is optimized for **concurrent** writers; it can **increase** per-commit
latency in single-threaded benchmarks because commits may wait up to the window.

##### Rust (Single-File Raw)

| Operation | Group Commit p50 | No Group Commit p50 |
|-----------|------------------|---------------------|
| Batch write (100 nodes) | 3.09ms | 42.54us |
| Set vectors (batch 100) | 3.77ms | 110.29us |

##### Python (Single-File Raw)

| Operation | Group Commit p50 | No Group Commit p50 |
|-----------|------------------|---------------------|
| Batch write (100 nodes) | 2.60ms | 57.29us |
| Set vectors (batch 100) | 2.81ms | 221.96us |

#### Vector Index (Rust)

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
