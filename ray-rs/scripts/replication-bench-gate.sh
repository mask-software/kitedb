#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${OUT_DIR:-$ROOT_DIR/../docs/benchmarks/results}"

ITERATIONS="${ITERATIONS:-20000}"
# Commits (each a bulk batch of 100 nodes) sampled for the p95 write latency.
# See the P95_MAX_RATIO note for why this is large.
NODE_BATCHES="${NODE_BATCHES:-2000}"
NODES="${NODES:-10000}"
EDGES="${EDGES:-0}"
EDGE_TYPES="${EDGE_TYPES:-1}"
EDGE_PROPS="${EDGE_PROPS:-0}"
VECTOR_COUNT="${VECTOR_COUNT:-0}"
SYNC_MODE="${SYNC_MODE:-normal}"
REPLICATION_SEGMENT_MAX_BYTES="${REPLICATION_SEGMENT_MAX_BYTES:-1073741824}"
# P95_MAX_RATIO bounds the p95 commit latency with a primary sidecar over the
# same run with replication off, as the median over ATTEMPTS pairs of runs.
#
# In Normal and Off sync the sidecar buffers frames in memory, so a commit adds
# only its epoch-fence check (a stat of the manifest), the frame bookkeeping,
# and, once per 100 ms publish, a primary-unflushed marker create. That is
# irreducible work, a few microseconds on a ~40 us commit.
#
# The sample used to be fixed at 50 commits, so the p95 was the third slowest:
# a few slow commits (warm-up after the checkpoint, the first marker create)
# and runner noise decided it, not the steady-state cost. Gate values measured
# 2026-10-02:
#   50 commits, median of 3: CI 1.33 and 1.54 (the last two main pushes;
#                 earlier pushes, with slower baseline commits, 0.86-1.18),
#                 Linux in Docker 1.16-1.31, macOS 1.18-1.30.
#   2000 commits, 8 gate runs each (median of 3 or 5): Linux in Docker
#                 1.06-1.10, macOS 0.94-1.10. Single pairs ranged 0.95-1.47
#                 and 0.71-1.46; the median outvotes them, and CI runs 5
#                 attempts so two noisy pairs cannot fail it.
# Regressions it still catches, injected and measured on Linux at 2000 commits:
# persisting the manifest on every append 47x, syncing every append 44x. One
# write(2) per append (no buffering) costs 1.13x on Linux and passes, as would
# any cost under about 30% of a commit (some 12 us here).
P95_MAX_RATIO="${P95_MAX_RATIO:-1.30}"
ATTEMPTS="${ATTEMPTS:-7}"

if [[ "$NODE_BATCHES" -lt 200 ]]; then
  echo "NODE_BATCHES must be >= 200 (a p95 over fewer commits is a handful of outliers)"
  exit 1
fi
if [[ "$ATTEMPTS" -lt 1 ]]; then
  echo "ATTEMPTS must be >= 1"
  exit 1
fi

mkdir -p "$OUT_DIR"
STAMP="${STAMP:-$(date +%F)}"
BASELINE_LOG_BASE="$OUT_DIR/${STAMP}-replication-gate-baseline"
PRIMARY_LOG_BASE="$OUT_DIR/${STAMP}-replication-gate-primary"

run_bench() {
  local logfile="$1"
  shift
  (
    cd "$ROOT_DIR"
    cargo run --release --example single_file_raw_bench --no-default-features -- \
      --nodes "$NODES" \
      --edges "$EDGES" \
      --edge-types "$EDGE_TYPES" \
      --edge-props "$EDGE_PROPS" \
      --vector-count "$VECTOR_COUNT" \
      --iterations "$ITERATIONS" \
      --node-batches "$NODE_BATCHES" \
      --sync-mode "$SYNC_MODE" \
      --replication-segment-max-bytes "$REPLICATION_SEGMENT_MAX_BYTES" \
      --no-auto-checkpoint \
      "$@" >"$logfile"
  )
}

extract_batch_write_p95() {
  local logfile="$1"
  grep "Batch of 100 nodes" "$logfile" | tail -1 | sed -E 's/.*p95= *([^ ]+).*/\1/'
}

latency_to_ns() {
  local token="$1"
  awk -v value="$token" 'BEGIN {
    if (value ~ /ns$/) {
      sub(/ns$/, "", value)
      printf "%.0f", value + 0
      exit
    }
    if (value ~ /us$/) {
      sub(/us$/, "", value)
      printf "%.0f", (value + 0) * 1000
      exit
    }
    if (value ~ /ms$/) {
      sub(/ms$/, "", value)
      printf "%.0f", (value + 0) * 1000000
      exit
    }
    printf "-1"
  }'
}

declare -a ratios

echo "== Replication gate: baseline vs primary (attempts: $ATTEMPTS)"
for attempt in $(seq 1 "$ATTEMPTS"); do
  if [[ "$ATTEMPTS" -eq 1 ]]; then
    baseline_log="${BASELINE_LOG_BASE}.txt"
    primary_log="${PRIMARY_LOG_BASE}.txt"
  else
    baseline_log="${BASELINE_LOG_BASE}.attempt${attempt}.txt"
    primary_log="${PRIMARY_LOG_BASE}.attempt${attempt}.txt"
  fi

  echo "attempt $attempt/$ATTEMPTS: baseline (replication disabled)"
  run_bench "$baseline_log"

  echo "attempt $attempt/$ATTEMPTS: primary sidecar enabled"
  run_bench "$primary_log" --replication-primary

  baseline_token="$(extract_batch_write_p95 "$baseline_log")"
  primary_token="$(extract_batch_write_p95 "$primary_log")"

  if [[ -z "$baseline_token" || -z "$primary_token" ]]; then
    echo "failed: could not parse p95 batch write metric from benchmark output"
    echo "baseline log: $baseline_log"
    echo "primary log:  $primary_log"
    exit 1
  fi

  baseline_ns="$(latency_to_ns "$baseline_token")"
  primary_ns="$(latency_to_ns "$primary_token")"
  if [[ "$baseline_ns" -le 0 || "$primary_ns" -le 0 ]]; then
    echo "failed: unsupported latency token(s): baseline=$baseline_token primary=$primary_token"
    exit 1
  fi

  ratio="$(awk -v base="$baseline_ns" -v primary="$primary_ns" 'BEGIN { printf "%.6f", primary / base }')"
  ratios+=("$ratio")

  echo "attempt $attempt/$ATTEMPTS metrics: baseline=$baseline_token ($baseline_ns ns) primary=$primary_token ($primary_ns ns) ratio=$ratio"
  echo "logs:"
  echo "  $baseline_log"
  echo "  $primary_log"
done

ratio_count="${#ratios[@]}"
median_ratio="$(
  printf '%s\n' "${ratios[@]}" \
    | sort -g \
    | awk '{
        a[NR]=$1
      }
      END {
        if (NR == 0) {
          print "NaN"
        } else if (NR % 2 == 1) {
          printf "%.6f", a[(NR + 1) / 2]
        } else {
          printf "%.6f", (a[NR / 2] + a[NR / 2 + 1]) / 2
        }
      }'
)"

if [[ "$median_ratio" == "NaN" ]]; then
  echo "failed: no ratios captured"
  exit 1
fi

pass="$(awk -v ratio="$median_ratio" -v max="$P95_MAX_RATIO" 'BEGIN { if (ratio <= max) print "yes"; else print "no" }')"
echo "median ratio across $ratio_count attempt(s): $median_ratio (max allowed: $P95_MAX_RATIO)"

if [[ "$pass" != "yes" ]]; then
  echo "failed: replication-on p95 median ratio exceeded gate"
  exit 1
fi

echo "pass: replication p95 gate satisfied"
