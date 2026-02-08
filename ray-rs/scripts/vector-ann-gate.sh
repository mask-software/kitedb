#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${OUT_DIR:-$ROOT_DIR/../docs/benchmarks/results}"

ALGORITHM="${ALGORITHM:-ivf_pq}"
RESIDUALS="${RESIDUALS:-false}"
VECTORS="${VECTORS:-20000}"
DIMENSIONS="${DIMENSIONS:-384}"
QUERIES="${QUERIES:-200}"
K="${K:-10}"
N_CLUSTERS="${N_CLUSTERS:-}"
N_PROBE="${N_PROBE:-16}"
PQ_SUBSPACES="${PQ_SUBSPACES:-48}"
PQ_CENTROIDS="${PQ_CENTROIDS:-256}"
SEED="${SEED:-42}"
ATTEMPTS="${ATTEMPTS:-3}"

MIN_RECALL_AT_K="${MIN_RECALL_AT_K:-0.16}"
MAX_P95_MS="${MAX_P95_MS:-8.0}"

if [[ "$ATTEMPTS" -lt 1 ]]; then
  echo "ATTEMPTS must be >= 1"
  exit 1
fi

mkdir -p "$OUT_DIR"
STAMP="${STAMP:-$(date +%F)}"
LOG_BASE="$OUT_DIR/${STAMP}-vector-ann-gate"

declare -a recalls=()
declare -a p95s=()
last_log=""

run_once() {
  local logfile="$1"
  local extra_args=()
  if [[ -n "$N_CLUSTERS" ]]; then
    extra_args+=(--n-clusters "$N_CLUSTERS")
  fi
  if [[ "$ALGORITHM" == "ivf_pq" ]]; then
    extra_args+=(--pq-subspaces "$PQ_SUBSPACES" --pq-centroids "$PQ_CENTROIDS" --residuals "$RESIDUALS")
  fi

  (
    cd "$ROOT_DIR"
    cargo run --release --no-default-features --example vector_ann_bench -- \
      --algorithm "$ALGORITHM" \
      --vectors "$VECTORS" \
      --dimensions "$DIMENSIONS" \
      --queries "$QUERIES" \
      --k "$K" \
      --n-probe "$N_PROBE" \
      --seed "$SEED" \
      "${extra_args[@]}" >"$logfile"
  )
}

echo "== Vector ANN gate (attempts: $ATTEMPTS)"
for attempt in $(seq 1 "$ATTEMPTS"); do
  if [[ "$ATTEMPTS" -eq 1 ]]; then
    logfile="${LOG_BASE}.txt"
  else
    logfile="${LOG_BASE}.attempt${attempt}.txt"
  fi

  run_once "$logfile"
  last_log="$logfile"

  recall="$(grep '^mean_recall_at_k:' "$logfile" | tail -1 | awk '{print $2}')"
  p95="$(grep '^search_p95_ms:' "$logfile" | tail -1 | awk '{print $2}')"

  if [[ -z "$recall" || -z "$p95" ]]; then
    echo "failed: could not parse ANN metrics"
    echo "log: $logfile"
    exit 1
  fi

  recalls+=("$recall")
  p95s+=("$p95")
  echo "attempt $attempt/$ATTEMPTS: recall_at_k=$recall p95_ms=$p95"
done

median() {
  printf '%s\n' "$@" | sort -g | awk '
    {
      a[NR] = $1
    }
    END {
      if (NR == 0) {
        print "NaN"
      } else if (NR % 2 == 1) {
        printf "%.6f", a[(NR + 1) / 2]
      } else {
        printf "%.6f", (a[NR / 2] + a[NR / 2 + 1]) / 2
      }
    }
  '
}

median_recall="$(median "${recalls[@]}")"
median_p95="$(median "${p95s[@]}")"

if [[ "$median_recall" == "NaN" || "$median_p95" == "NaN" ]]; then
  echo "failed: no metrics captured"
  exit 1
fi

recall_pass="$(awk -v actual="$median_recall" -v min="$MIN_RECALL_AT_K" 'BEGIN { if (actual >= min) print "yes"; else print "no" }')"
p95_pass="$(awk -v actual="$median_p95" -v max="$MAX_P95_MS" 'BEGIN { if (actual <= max) print "yes"; else print "no" }')"

echo "median recall_at_k across $ATTEMPTS attempt(s): $median_recall (min required: $MIN_RECALL_AT_K)"
echo "median p95_ms across $ATTEMPTS attempt(s): $median_p95 (max allowed: $MAX_P95_MS)"
echo "log: $last_log"

if [[ "$recall_pass" != "yes" || "$p95_pass" != "yes" ]]; then
  echo "failed: ANN gate not satisfied"
  exit 1
fi

echo "pass: ANN gate satisfied"
