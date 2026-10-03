#!/usr/bin/env bash
# Re-measure every benchmark the docs site publishes, plus a few extras the
# docs cite, and write one dated raw log per configuration to
# docs/benchmarks/results/<STAMP>-<name>.txt.
#
# Usage (from anywhere):
#   ray-rs/scripts/bench-refresh.sh [options]
#
# Options:
#   --rounds N        Rounds for the configurations that repeat (default: 5)
#   --groups LIST     Comma-separated groups: site, extra (default: site,extra)
#   --only REGEX      Run only configurations whose name matches REGEX (grep -E)
#   --list            Print the matrix (name, group, rounds, command) and exit
#   --smoke           Tiny sizes, one round, logs in a temp dir: checks that
#                     every command runs, measures nothing
#   --no-build        Use the binaries and bindings already built
#   -h, --help        Show this help
#
# Environment:
#   STAMP       Date prefix of the logs (default: today's UTC date)
#   OUT_DIR     Log directory (default: docs/benchmarks/results; --smoke: a temp dir)
#   MAX_BUSY    Percent of all CPUs other processes may keep busy (default: 20).
#               Sampled over one second before the first run (refuse above it)
#               and between runs (wait until it drops, noting the pause in the
#               log, for at most MAX_WAIT seconds, default 600). The load
#               average is only recorded: on macOS it counts threads that wait
#               for I/O, and our own runs raise it for a minute after they end.
#   PAUSE       Seconds to wait between two runs (default: 2)
#   FORCE=1     Start even above MAX_BUSY, on battery, in Low Power Mode, or
#               with uncommitted changes (the logs then do not match a commit)
#   PYTHON      Python with maturin for the bindings (default: ray-rs/.venv/bin/python)
#
# Setup, once per checkout: `bun install --frozen-lockfile` at the repository
# root (TS bench), and in ray-rs/ `uv venv .venv` plus
# `uv pip install --python .venv/bin/python maturin` (Python bench). The script
# builds the release binaries, the napi addon (--release) and the Python
# bindings (maturin develop --release) itself unless --no-build.
#
# Method:
# - Rounds are interleaved: round 1 runs every configuration once, then round 2,
#   and so on, starting each round one configuration further along, so a slow
#   minute on the machine lands on different configurations in different
#   rounds. Configurations that already repeat internally (query_core_bench,
#   mvcc_overhead_bench, vector_ann_bench) run fewer rounds.
# - Every run uses the same fixed seed (--seed 42 where the bench takes one;
#   the other benches are deterministic), so rounds differ only by machine noise.
# - A log holds a header (commit, toolchain, machine, power, load, the exact
#   command) and then every round's full output between "### run i/N" lines.
# - The site publishes, for each result row, the line of the round with the
#   median value of that row (p50 for latency rows, the rate for throughput
#   rows; the lower middle round for an even count), copied whole.
#   ray-docs/scripts/benchmark-logs.ts does the extraction (bun run bench:data).
# - Existing logs are never overwritten: rerun with another STAMP.
#
# MVCC is the library default, so the headline configurations run with MVCC
# on (passed explicitly as --mvcc). MVCC off runs only where the comparison is
# published: the graph latency table, one writer in the write-scaling table,
# the bulk load and mvcc_overhead_bench. Group commit is always on; the
# --group-commit-* flags have no effect and are not passed.
#
# Not in the matrix: other graph databases (ray_vs_memgraph_bench needs a
# Memgraph server, ray_vs_ladybug_bench a long C++ build, and the docs publish
# no numbers for other databases), and the replication, vector ANN and
# open/close gates, which have their own scripts in this directory.

set -euo pipefail

RAY_RS="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO="$(cd "$RAY_RS/.." && pwd)"
BIN_DIR="$RAY_RS/target/release/examples"
PYTHON="${PYTHON:-$RAY_RS/.venv/bin/python}"
MAX_BUSY="${MAX_BUSY:-20}"
MAX_WAIT="${MAX_WAIT:-600}"
PAUSE="${PAUSE:-2}"
FORCE="${FORCE:-0}"

ROUNDS=5
GROUPS_SELECTED="site,extra"
ONLY=""
LIST=0
SMOKE=0
BUILD=1

usage() {
  sed -n '2,/^$/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --rounds)
      ROUNDS="${2:?--rounds needs a value}"
      shift 2
      ;;
    --groups)
      GROUPS_SELECTED="${2:?--groups needs a value}"
      shift 2
      ;;
    --only)
      ONLY="${2:?--only needs a value}"
      shift 2
      ;;
    --list)
      LIST=1
      shift
      ;;
    --smoke)
      SMOKE=1
      shift
      ;;
    --no-build)
      BUILD=0
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown option $1 (see --help)" >&2
      exit 2
      ;;
  esac
done

if ! [[ "$ROUNDS" =~ ^[1-9][0-9]*$ ]]; then
  echo "error: --rounds must be a positive integer" >&2
  exit 2
fi
if (( ROUNDS % 2 == 0 )); then
  echo "warning: an even --rounds has no single median round; the extractor takes the lower middle one" >&2
fi

# ---------------------------------------------------------------------------
# Sizes (the published configurations, or tiny ones for --smoke)
# ---------------------------------------------------------------------------

SEED=42
SMOKE_NOTE=""
if [[ "$SMOKE" == 1 ]]; then
  ROUNDS=1
  PAUSE=0
  SMOKE_NOTE="; smoke sizes (not for publishing)"
  RAW_SIZE=(--nodes 1000 --edges 5000 --iterations 200)
  TS_SIZE=(--nodes 200 --edges 1000 --iterations 100)
  VECTOR_SIZE=(--vectors 1000 --iterations 100)
  ANN_SIZE=(--vectors 2000 --queries 20)
  MW_TX_200=10
  MW_TX_1=200
  MW_TX_FULLFSYNC=20
  OPEN_LARGE=(--nodes 20000)
  OPEN_SMALL=(--nodes 5000)
  QUERY_SIZE=(--nodes 20000 --page 10 --type-nodes 5000 --hub-edges 5000 --repeat 2)
  OPEN_REPEAT=(--repeat 2)
  MVCC_OVERHEAD_SIZE=(--nodes 2000 --threads 1,2 --duration-ms 50 --repeat 1 --history-nodes 200)
  BULK_SIZE=(--nodes 5000 --edges 20000 --batch 1000)
else
  RAW_SIZE=(--nodes 10000 --edges 50000 --iterations 10000)
  TS_SIZE=(--nodes 1000 --edges 5000 --iterations 1000)
  VECTOR_SIZE=(--vectors 10000 --iterations 1000)
  ANN_SIZE=(--vectors 50000 --queries 200)
  MW_TX_200=200
  MW_TX_1=50000
  MW_TX_FULLFSYNC=500
  OPEN_LARGE=(--nodes 1000000)
  OPEN_SMALL=(--nodes 100000)
  QUERY_SIZE=(--nodes 1000000 --page 1000 --type-nodes 100000 --hub-edges 200000 --repeat 7)
  OPEN_REPEAT=(--repeat 9)
  MVCC_OVERHEAD_SIZE=(--nodes 20000 --threads 1,4,8 --duration-ms 1000 --repeat 5)
  BULK_SIZE=(--nodes 200000 --edges 1000000 --batch 5000)
fi

# The 2026-02-04 published configuration: 256 MB WAL, auto-checkpoint off, so
# write timings show the raw commit cost.
RAW_COMMON=("${RAW_SIZE[@]}" --wal-size 268435456 --no-auto-checkpoint --seed "$SEED")
# multi_writer_throughput_bench, 200-node transactions: the 2026-02-04 shape.
MW_200=(--tx-per-thread "$MW_TX_200" --batch-size 200 --edges-per-node 1 --edge-types 3
  --edge-props 10 --wal-size 1073741824)
# One-node transactions: one keyed node per commit, no edges.
MW_1=(--tx-per-thread "$MW_TX_1" --batch-size 1 --edges-per-node 0 --edge-props 0
  --wal-size 1073741824)

# ---------------------------------------------------------------------------
# The matrix
# ---------------------------------------------------------------------------
# add NAME GROUP ROUNDS KIND COMMAND...
#   ROUNDS: "all" (--rounds) or a number (at most --rounds)
#   KIND:   rust (COMMAND starts with an example name), python, ts, sqlite

NAMES=()
GROUPS_OF=()
ROUNDS_OF=()
KINDS=()
CMDS=()

add() {
  local name="$1" group="$2" rounds="$3" kind="$4"
  shift 4
  if [[ ",$GROUPS_SELECTED," != *",$group,"* ]]; then
    return
  fi
  if [[ -n "$ONLY" ]] && ! grep -Eq -- "$ONLY" <<<"$name"; then
    return
  fi
  if [[ "$rounds" == all ]] || (( rounds > ROUNDS )); then
    rounds="$ROUNDS"
  fi
  NAMES+=("$name")
  GROUPS_OF+=("$group")
  ROUNDS_OF+=("$rounds")
  KINDS+=("$kind")
  CMDS+=("$(printf '%q ' "$@")")
}

# Graph latency (homepage, /docs/benchmarks, graph and cross-language pages).
for sync in normal full off; do
  add "single-file-raw-rust-mvcc-$sync" site all rust \
    single_file_raw_bench "${RAW_COMMON[@]}" --sync-mode "$sync" --mvcc
done
add single-file-raw-rust-nomvcc-normal site all rust \
  single_file_raw_bench "${RAW_COMMON[@]}" --sync-mode normal --no-mvcc
add single-file-raw-python-mvcc-normal site all python \
  "${RAW_COMMON[@]}" --sync-mode normal --mvcc --no-output
add bench-fluent-vs-lowlevel-mvcc-normal site all ts \
  "${TS_SIZE[@]}" --edge-types 3 --edge-props 10 --sync-mode normal --mvcc --seed "$SEED"
add sqlite-single-file-raw-edges-normal site all sqlite \
  "${RAW_SIZE[@]}" --wal-size 268435456 --sync-mode normal

# Vector index (vector page, homepage chart). 10k vectors: VectorIndex's auto
# backend builds plain IVF at this size, as the published run did.
add vector-bench-rust site all rust \
  vector_bench "${VECTOR_SIZE[@]}" --dimensions 768 --k 10 --n-probe 10 --seed "$SEED" --no-output

# Write scaling (graph page): 1, 4 and 8 writer threads, MVCC.
for sync in normal full; do
  for threads in 1 4 8; do
    add "multi-writer-throughput-mvcc-$sync-200node-${threads}w" site all rust \
      multi_writer_throughput_bench --threads "$threads" "${MW_200[@]}" --sync-mode "$sync" --mvcc
    add "multi-writer-throughput-mvcc-$sync-1node-${threads}w" site all rust \
      multi_writer_throughput_bench --threads "$threads" "${MW_1[@]}" --sync-mode "$sync" --mvcc
  done
done
# One non-MVCC writer, the baseline the docs compare MVCC writers with.
add multi-writer-throughput-nomvcc-normal-200node-1w site all rust \
  multi_writer_throughput_bench --threads 1 "${MW_200[@]}" --sync-mode normal --no-mvcc
add multi-writer-throughput-nomvcc-normal-1node-1w site all rust \
  multi_writer_throughput_bench --threads 1 "${MW_1[@]}" --sync-mode normal --no-mvcc

# Extras the docs cite.
# Full sync with F_FULLFSYNC (macOS), which reaches the drive; plain fsync on
# macOS leaves writes in the drive's cache.
for threads in 1 8; do
  add "multi-writer-throughput-mvcc-fullfsync-1node-${threads}w" extra all rust \
    multi_writer_throughput_bench --threads "$threads" --tx-per-thread "$MW_TX_FULLFSYNC" \
    --batch-size 1 --edges-per-node 0 --edge-props 0 --wal-size 1073741824 \
    --sync-mode full --full-fsync --mvcc
done
# Open time of a checkpointed database (read-only open + close).
add query-core-open-1m extra 3 rust \
  query_core_bench --sections open "${OPEN_LARGE[@]}" --edges-per-node 5 "${OPEN_REPEAT[@]}" --mvcc
add query-core-open-100k extra 3 rust \
  query_core_bench --sections open "${OPEN_SMALL[@]}" --edges-per-node 5 "${OPEN_REPEAT[@]}" --mvcc
# Paging, type listings and take(n) from a hub, in the WAL and after a checkpoint.
add query-core-paging extra 3 rust \
  query_core_bench --sections paging,types,hub "${QUERY_SIZE[@]}" --edges-per-node 5 --limit 100 --mvcc
# IVF vs IVF-PQ where the auto backend switches to IVF-PQ (>= 512 dims, >= 50k vectors).
for algorithm in ivf ivf_pq; do
  add "vector-ann-768d-${algorithm//_/-}" extra 3 rust \
    vector_ann_bench --algorithm "$algorithm" "${ANN_SIZE[@]}" --dimensions 768 --k 10 \
    --n-probe 10 --dataset lowrank --seed "$SEED"
done
# MVCC cost (internals/performance page): reads, mixed and writes, MVCC off vs on.
add mvcc-overhead extra 1 rust \
  mvcc_overhead_bench "${MVCC_OVERHEAD_SIZE[@]}" --modes off,on
# Bulk load (performance guide), MVCC on and off, and with a reader held open.
add bulk-load-mvcc extra all rust bulk_load_bench "${BULK_SIZE[@]}" --runs 1 --mvcc
add bulk-load-nomvcc extra all rust bulk_load_bench "${BULK_SIZE[@]}" --runs 1 --no-mvcc
add bulk-load-mvcc-reader extra all rust bulk_load_bench "${BULK_SIZE[@]}" --runs 1 --mvcc --reader

COUNT="${#NAMES[@]}"
if (( COUNT == 0 )); then
  echo "error: no configuration selected" >&2
  exit 2
fi

# The command a reader can paste, run from ray-rs/.
display_command() {
  local kind="$1" cmd="$2"
  local example rest
  cmd="${cmd//\\,/,}"
  cmd="${cmd% }"
  case "$kind" in
    rust)
      example="${cmd%% *}"
      rest="${cmd#* }"
      echo "cargo run --release --example $example --no-default-features -- $rest"
      ;;
    python) echo "python python/benchmarks/benchmark_single_file_raw.py $cmd" ;;
    ts) echo "node --import @oxc-node/core/register benchmark/bench-fluent-vs-lowlevel.ts $cmd" ;;
    sqlite) echo "python ../docs/benchmarks/sqlite_single_file_raw_bench.py $cmd" ;;
  esac
}

if [[ "$LIST" == 1 ]]; then
  for ((i = 0; i < COUNT; i++)); do
    printf '%-48s %-6s rounds=%s\n    %s\n' "${NAMES[i]}" "${GROUPS_OF[i]}" "${ROUNDS_OF[i]}" \
      "$(display_command "${KINDS[i]}" "${CMDS[i]}")"
  done
  exit 0
fi

# ---------------------------------------------------------------------------
# Preflight
# ---------------------------------------------------------------------------

STAMP="${STAMP:-$(date -u +%F)}"
if [[ "$SMOKE" == 1 ]]; then
  SMOKE_TMP="${TMPDIR:-/tmp}"
  OUT_DIR="${OUT_DIR:-$(mktemp -d "${SMOKE_TMP%/}/bench-refresh-smoke.XXXXXX")}"
else
  OUT_DIR="${OUT_DIR:-$REPO/docs/benchmarks/results}"
fi
mkdir -p "$OUT_DIR"

log_path() { echo "$OUT_DIR/$STAMP-$1.txt"; }
MANIFEST="$OUT_DIR/$STAMP-bench-refresh.txt"

for ((i = 0; i < COUNT; i++)); do
  if [[ -e "$(log_path "${NAMES[i]}")" ]]; then
    echo "error: $(log_path "${NAMES[i]}") exists; pick another STAMP" >&2
    exit 1
  fi
done
if [[ -e "$MANIFEST" ]]; then
  echo "error: $MANIFEST exists; pick another STAMP" >&2
  exit 1
fi

kinds_selected() {
  local kind="$1" i
  for ((i = 0; i < COUNT; i++)); do
    [[ "${KINDS[i]}" == "$kind" ]] && return 0
  done
  return 1
}

# Percent of all CPUs busy over the next second (nothing of ours runs then).
cpu_busy() {
  if [[ "$(uname -s)" == Darwin ]]; then
    top -l 2 -n 0 -s 1 | awk '/^CPU usage/ { idle = $7 } END { sub("%", "", idle); printf "%.1f", 100 - idle }'
  else
    local a b
    a="$(awk '/^cpu / {print $2 + $3 + $4 + $7 + $8, $5 + $6}' /proc/stat)"
    sleep 1
    b="$(awk '/^cpu / {print $2 + $3 + $4 + $7 + $8, $5 + $6}' /proc/stat)"
    echo "$a $b" | awk '{ busy = $3 - $1; idle = $4 - $2; printf "%.1f", 100 * busy / (busy + idle) }'
  fi
}

over_max_busy() {
  awk -v busy="$1" -v max="$MAX_BUSY" 'BEGIN { exit !(busy > max) }'
}

load_all() {
  if [[ "$(uname -s)" == Darwin ]]; then
    sysctl -n vm.loadavg | awk '{print $2, $3, $4}'
  else
    awk '{print $1, $2, $3}' /proc/loadavg
  fi
}

# macOS energy mode: "low power", "automatic", "high power" (pmset reports
# lowpowermode on older releases, powermode 0/1/2 on newer ones).
energy_mode() {
  pmset -g 2>/dev/null | awk '
    $1 == "lowpowermode" { print ($2 == 1 ? "low power" : "automatic"); found = 1; exit }
    $1 == "powermode" { print ($2 == 1 ? "low power" : $2 == 2 ? "high power" : "automatic"); found = 1; exit }
    END { if (!found) print "unknown" }'
}

refuse() {
  if [[ "$FORCE" == 1 || "$SMOKE" == 1 ]]; then
    echo "warning: $1 (going on: FORCE=1 or --smoke)" >&2
  else
    echo "error: $1 (FORCE=1 to run anyway)" >&2
    exit 1
  fi
}

DIRTY="$(git -C "$REPO" status --porcelain -- . ':(exclude)docs/benchmarks/results' | wc -l | tr -d ' ')"
if [[ "$DIRTY" != 0 ]]; then
  refuse "the working tree has $DIRTY uncommitted change(s) outside docs/benchmarks/results, so the logs would not match the commit"
fi
if [[ "$(uname -s)" == Darwin ]]; then
  if ! pmset -g batt 2>/dev/null | head -1 | grep -q "AC Power"; then
    refuse "not on AC power"
  fi
  if [[ "$(energy_mode)" == "low power" ]]; then
    refuse "Low Power Mode is on, which caps CPU speed (System Settings > Battery, or sudo pmset -a powermode 0)"
  fi
fi
BUSY="$(cpu_busy)"
if over_max_busy "$BUSY"; then
  refuse "other processes keep ${BUSY}% of the CPUs busy, above MAX_BUSY=$MAX_BUSY"
fi

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------

if [[ "$BUILD" == 1 ]]; then
  examples=()
  for ((i = 0; i < COUNT; i++)); do
    if [[ "${KINDS[i]}" == rust ]]; then
      example="${CMDS[i]%% *}"
      if [[ " ${examples[*]+${examples[*]}} " != *" --example $example "* ]]; then
        examples+=(--example "$example")
      fi
    fi
  done
  if (( ${#examples[@]} > 0 )); then
    echo "==> cargo build --release --no-default-features ${examples[*]}"
    (cd "$RAY_RS" && cargo build --release --no-default-features "${examples[@]}")
  fi
  if kinds_selected ts; then
    echo "==> napi build --platform --release; bun run build:ts"
    (cd "$RAY_RS" && ./node_modules/.bin/napi build --platform --release && bun run build:ts)
  fi
  if kinds_selected python; then
    echo "==> maturin develop --release --features python"
    (cd "$RAY_RS" && PYO3_PYTHON="$PYTHON" VIRTUAL_ENV="$(dirname "$(dirname "$PYTHON")")" \
      "$(dirname "$PYTHON")/maturin" develop --release --features python)
  fi
fi

# ---------------------------------------------------------------------------
# Headers
# ---------------------------------------------------------------------------

machine_lines() {
  if [[ "$(uname -s)" == Darwin ]]; then
    local cpu mem levels="" level name cores
    cpu="$(sysctl -n machdep.cpu.brand_string)"
    mem="$(( $(sysctl -n hw.memsize) / 1024 / 1024 / 1024 ))"
    for level in 0 1 2; do
      name="$(sysctl -n "hw.perflevel$level.name" 2>/dev/null || true)"
      cores="$(sysctl -n "hw.perflevel$level.physicalcpu" 2>/dev/null || true)"
      if [[ -n "$name" && -n "$cores" ]]; then
        levels="${levels:+$levels + }$cores $name"
      fi
    done
    echo "machine: $cpu, $(sysctl -n hw.ncpu) cores (${levels:-unknown}), $mem GB"
    echo "os: macOS $(sw_vers -productVersion) ($(sw_vers -buildVersion)), Darwin $(uname -r), $(uname -m)"
    echo "power: $(pmset -g batt 2>/dev/null | head -1 | sed "s/^Now drawing from //; s/'//g"), energy mode $(energy_mode)"
  else
    echo "machine: $(awk -F': ' '/model name/ {print $2; exit}' /proc/cpuinfo), $(nproc) cpus, $(awk '/MemTotal/ {printf "%d GB", $2 / 1024 / 1024}' /proc/meminfo)"
    echo "os: $(uname -sr), $(uname -m)"
  fi
  local tmp="${TMPDIR:-/tmp}"
  tmp="${tmp%/}"
  echo "temp dir: $tmp ($(df -h "$tmp" | awk 'NR == 2 {print $1}'))"
}

toolchain_lines() {
  local kind="$1"
  case "$kind" in
    rust)
      echo "toolchain: $(cd "$RAY_RS" && rustc -V), $(cd "$RAY_RS" && cargo -V)"
      echo "build: cargo build --release --no-default-features (profile.release: lto = true, codegen-units = 1)"
      ;;
    python)
      echo "toolchain: $(cd "$RAY_RS" && rustc -V); $("$PYTHON" --version 2>&1) ($PYTHON)"
      echo "build: maturin develop --release --features python"
      ;;
    ts)
      echo "toolchain: $(cd "$RAY_RS" && rustc -V); node $(node --version)"
      echo "build: napi build --platform --release; bun run build:ts"
      ;;
    sqlite)
      echo "toolchain: $("$PYTHON" --version 2>&1) ($PYTHON), SQLite $("$PYTHON" -c 'import sqlite3; print(sqlite3.sqlite_version)')"
      ;;
  esac
}

MACHINE="$(machine_lines)"
COMMIT="$(git -C "$REPO" rev-parse HEAD)"
DESCRIBE="$(git -C "$REPO" describe --always --dirty --exclude '*' 2>/dev/null || echo "$COMMIT")"
STARTED="$(date -u +%FT%TZ)"

write_header() {
  local i="$1" log
  log="$(log_path "${NAMES[i]}")"
  {
    echo "# KiteDB benchmark log, written by ray-rs/scripts/bench-refresh.sh"
    echo "# name: ${NAMES[i]}"
    echo "# group: ${GROUPS_OF[i]}"
    echo "# command (from ray-rs/): $(display_command "${KINDS[i]}" "${CMDS[i]}")"
    echo "# rounds: ${ROUNDS_OF[i]}, interleaved with the other $((COUNT - 1)) configurations of this refresh"
    echo "# published value: per row, the line of the round with the median p50 (or rate); see ray-docs/scripts/benchmark-logs.ts"
    echo "# refresh started: $STARTED"
    echo "# commit: $COMMIT ($DESCRIBE)"
    toolchain_lines "${KINDS[i]}" | sed 's/^/# /'
    echo "$MACHINE" | sed 's/^/# /'
    echo "# load average at start: $(load_all)"
    echo "#"
  } >"$log"
}

# Waits until other processes leave the CPUs quiet (MAX_BUSY), at most
# MAX_WAIT seconds; sets BUSY and notes any pause in the manifest and log $1.
wait_quiet() {
  local log="$1" waited=0
  BUSY="$(cpu_busy)"
  while over_max_busy "$BUSY"; do
    if (( waited >= MAX_WAIT )); then
      echo "### going on after ${waited}s with the CPUs ${BUSY}% busy (MAX_BUSY=$MAX_BUSY)" | tee -a "$log" "$MANIFEST"
      return
    fi
    echo "  paused: other processes keep ${BUSY}% of the CPUs busy (MAX_BUSY=$MAX_BUSY)"
    sleep 15
    waited=$((waited + 15))
    BUSY="$(cpu_busy)"
  done
  if (( waited > 0 )); then
    echo "### paused ${waited}s before this run: the CPUs were busier than MAX_BUSY=$MAX_BUSY" | tee -a "$log" >>"$MANIFEST"
  fi
}

run_one() {
  local i="$1" round="$2" log status start end elapsed
  log="$(log_path "${NAMES[i]}")"
  local -a cmd
  eval "cmd=(${CMDS[i]})"
  case "${KINDS[i]}" in
    rust) cmd=("$BIN_DIR/${cmd[0]}" "${cmd[@]:1}") ;;
    python) cmd=("$PYTHON" python/benchmarks/benchmark_single_file_raw.py "${cmd[@]}") ;;
    ts) cmd=(node --import @oxc-node/core/register benchmark/bench-fluent-vs-lowlevel.ts "${cmd[@]}") ;;
    sqlite) cmd=("$PYTHON" ../docs/benchmarks/sqlite_single_file_raw_bench.py "${cmd[@]}") ;;
  esac
  wait_quiet "$log"
  echo "### run $round/${ROUNDS_OF[i]} | start $(date -u +%FT%TZ) | load $(load_all) | cpu busy before ${BUSY}%" >>"$log"
  start="$(date +%s)"
  set +e
  (cd "$RAY_RS" && "${cmd[@]}") >>"$log" 2>&1
  status=$?
  set -e
  end="$(date +%s)"
  elapsed=$((end - start))
  echo "### run $round/${ROUNDS_OF[i]} | exit $status | ${elapsed}s" >>"$log"
  echo "$round ${NAMES[i]} exit=$status ${elapsed}s" >>"$MANIFEST"
  printf '  [%d/%d] %-48s exit %d, %ds\n' "$round" "${ROUNDS_OF[i]}" "${NAMES[i]}" "$status" "$elapsed"
  return "$status"
}

{
  echo "# KiteDB benchmark refresh, written by ray-rs/scripts/bench-refresh.sh"
  echo "# refresh started: $STARTED"
  echo "# commit: $COMMIT ($DESCRIBE)"
  echo "$MACHINE" | sed 's/^/# /'
  echo "# load average at start: $(load_all); CPUs busy at start: ${BUSY}%"
  echo "# rounds: $ROUNDS; groups: $GROUPS_SELECTED${ONLY:+; only: $ONLY}$SMOKE_NOTE"
  echo "#"
  echo "# configurations (log: $STAMP-<name>.txt):"
  for ((i = 0; i < COUNT; i++)); do
    echo "#   ${NAMES[i]} [${GROUPS_OF[i]}, rounds ${ROUNDS_OF[i]}]: $(display_command "${KINDS[i]}" "${CMDS[i]}")"
  done
  echo "#"
  echo "# runs (round name exit elapsed):"
} >"$MANIFEST"

for ((i = 0; i < COUNT; i++)); do
  write_header "$i"
done

echo "==> $COUNT configurations, up to $ROUNDS rounds, logs in $OUT_DIR ($STAMP-*.txt)"
FAILED=0
for ((round = 1; round <= ROUNDS; round++)); do
  echo "==> round $round/$ROUNDS"
  for ((step = 0; step < COUNT; step++)); do
    i=$(((step + round - 1) % COUNT))
    if (( round > ROUNDS_OF[i] )); then
      continue
    fi
    if ! run_one "$i" "$round"; then
      FAILED=$((FAILED + 1))
    fi
    sleep "$PAUSE"
  done
done

echo "# refresh finished: $(date -u +%FT%TZ), failed runs: $FAILED" >>"$MANIFEST"
echo "==> done: $FAILED failed run(s); manifest $MANIFEST"
if (( FAILED > 0 )); then
  exit 1
fi
