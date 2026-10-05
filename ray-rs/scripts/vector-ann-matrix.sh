#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${OUT_DIR:-$ROOT_DIR/../docs/benchmarks/results}"
STAMP="${STAMP:-$(date +%F)}"

VECTORS="${VECTORS:-20000}"
DIMENSIONS="${DIMENSIONS:-384}"
QUERIES="${QUERIES:-200}"
K="${K:-10}"
N_CLUSTERS="${N_CLUSTERS:-}"
N_PROBES="${N_PROBES:-8 16}"
PQ_SUBSPACES="${PQ_SUBSPACES:-48}"
PQ_CENTROIDS="${PQ_CENTROIDS:-256}"
SEED="${SEED:-42}"

mkdir -p "$OUT_DIR"
RAW_OUT="$OUT_DIR/${STAMP}-vector-ann-matrix.txt"
CSV_OUT="$OUT_DIR/${STAMP}-vector-ann-matrix.csv"

echo "Vector ANN matrix benchmark" >"$RAW_OUT"
echo "date=${STAMP}" >>"$RAW_OUT"
echo "vectors=${VECTORS} dimensions=${DIMENSIONS} queries=${QUERIES} k=${K}" >>"$RAW_OUT"
echo "n_probes={${N_PROBES}}" >>"$RAW_OUT"
echo "pq_subspaces=${PQ_SUBSPACES} pq_centroids=${PQ_CENTROIDS}" >>"$RAW_OUT"
echo "seed=${SEED}" >>"$RAW_OUT"
echo >>"$RAW_OUT"

printf "algorithm,residuals,n_probe,build_elapsed_ms,search_p50_ms,search_p95_ms,mean_recall_at_k\n" >"$CSV_OUT"

run_case() {
  local algorithm="$1"
  local residuals="$2"
  local n_probe="$3"

  local extra_args=()
  if [[ -n "$N_CLUSTERS" ]]; then
    extra_args+=(--n-clusters "$N_CLUSTERS")
  fi
  if [[ "$algorithm" == "ivf_pq" ]]; then
    extra_args+=(--pq-subspaces "$PQ_SUBSPACES" --pq-centroids "$PQ_CENTROIDS" --residuals "$residuals")
  fi

  echo "RUN algorithm=${algorithm} residuals=${residuals} n_probe=${n_probe}" | tee -a "$RAW_OUT"
  run_out="$(
    cd "$ROOT_DIR"
    cargo run --release --no-default-features --example vector_ann_bench -- \
      --algorithm "$algorithm" \
      --vectors "$VECTORS" \
      --dimensions "$DIMENSIONS" \
      --queries "$QUERIES" \
      --k "$K" \
      --n-probe "$n_probe" \
      --seed "$SEED" \
      "${extra_args[@]}"
  )"
  echo "$run_out" >>"$RAW_OUT"
  echo >>"$RAW_OUT"

  build_ms="$(echo "$run_out" | grep '^build_elapsed_ms:' | awk '{print $2}')"
  p50_ms="$(echo "$run_out" | grep '^search_p50_ms:' | awk '{print $2}')"
  p95_ms="$(echo "$run_out" | grep '^search_p95_ms:' | awk '{print $2}')"
  recall="$(echo "$run_out" | grep '^mean_recall_at_k:' | awk '{print $2}')"

  printf "%s,%s,%s,%s,%s,%s,%s\n" \
    "$algorithm" \
    "$residuals" \
    "$n_probe" \
    "$build_ms" \
    "$p50_ms" \
    "$p95_ms" \
    "$recall" >>"$CSV_OUT"
}

for n_probe in $N_PROBES; do
  run_case "ivf" "na" "$n_probe"
  run_case "ivf_pq" "true" "$n_probe"
  run_case "ivf_pq" "false" "$n_probe"
done

{
  echo "raw_output=${RAW_OUT}"
  echo "csv_output=${CSV_OUT}"
  echo "SUMMARY (best recall then p95 latency):"
  echo "algorithm,residuals,n_probe,build_elapsed_ms,search_p50_ms,search_p95_ms,mean_recall_at_k"
  tail -n +2 "$CSV_OUT" | sort -t, -k7,7gr -k6,6g
} | tee -a "$RAW_OUT"
