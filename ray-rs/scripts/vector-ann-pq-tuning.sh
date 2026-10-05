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
PQ_SUBSPACES_SET="${PQ_SUBSPACES_SET:-24 48}"
PQ_CENTROIDS_SET="${PQ_CENTROIDS_SET:-128 256}"
RESIDUALS_SET="${RESIDUALS_SET:-false}"
SEED="${SEED:-42}"

mkdir -p "$OUT_DIR"
RAW_OUT="$OUT_DIR/${STAMP}-vector-ann-pq-tuning.txt"
CSV_OUT="$OUT_DIR/${STAMP}-vector-ann-pq-tuning.csv"

echo "Vector ANN PQ tuning benchmark" >"$RAW_OUT"
echo "date=${STAMP}" >>"$RAW_OUT"
echo "vectors=${VECTORS} dimensions=${DIMENSIONS} queries=${QUERIES} k=${K}" >>"$RAW_OUT"
echo "n_probes={${N_PROBES}}" >>"$RAW_OUT"
echo "pq_subspaces_set={${PQ_SUBSPACES_SET}}" >>"$RAW_OUT"
echo "pq_centroids_set={${PQ_CENTROIDS_SET}}" >>"$RAW_OUT"
echo "residuals_set={${RESIDUALS_SET}}" >>"$RAW_OUT"
echo "seed=${SEED}" >>"$RAW_OUT"
echo >>"$RAW_OUT"

printf "algorithm,residuals,n_probe,pq_subspaces,pq_centroids,build_elapsed_ms,search_p50_ms,search_p95_ms,mean_recall_at_k,recall_ratio_vs_ivf,p95_ratio_vs_ivf\n" >"$CSV_OUT"

declare -A IVF_BASE_RECALL
declare -A IVF_BASE_P95

run_ann() {
  local algorithm="$1"
  local residuals="$2"
  local n_probe="$3"
  local pq_subspaces="$4"
  local pq_centroids="$5"

  local extra_args=()
  if [[ -n "$N_CLUSTERS" ]]; then
    extra_args+=(--n-clusters "$N_CLUSTERS")
  fi
  if [[ "$algorithm" == "ivf_pq" ]]; then
    extra_args+=(--pq-subspaces "$pq_subspaces" --pq-centroids "$pq_centroids" --residuals "$residuals")
  fi

  (
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
  )
}

for n_probe in $N_PROBES; do
  echo "RUN baseline algorithm=ivf n_probe=${n_probe}" | tee -a "$RAW_OUT"
  ivf_out="$(run_ann "ivf" "na" "$n_probe" "na" "na")"
  echo "$ivf_out" >>"$RAW_OUT"
  echo >>"$RAW_OUT"

  ivf_build="$(echo "$ivf_out" | grep '^build_elapsed_ms:' | awk '{print $2}')"
  ivf_p50="$(echo "$ivf_out" | grep '^search_p50_ms:' | awk '{print $2}')"
  ivf_p95="$(echo "$ivf_out" | grep '^search_p95_ms:' | awk '{print $2}')"
  ivf_recall="$(echo "$ivf_out" | grep '^mean_recall_at_k:' | awk '{print $2}')"

  IVF_BASE_RECALL["$n_probe"]="$ivf_recall"
  IVF_BASE_P95["$n_probe"]="$ivf_p95"

  printf "ivf,na,%s,na,na,%s,%s,%s,%s,1.000000,1.000000\n" \
    "$n_probe" "$ivf_build" "$ivf_p50" "$ivf_p95" "$ivf_recall" >>"$CSV_OUT"
done

for n_probe in $N_PROBES; do
  ivf_recall="${IVF_BASE_RECALL[$n_probe]}"
  ivf_p95="${IVF_BASE_P95[$n_probe]}"

  for residuals in $RESIDUALS_SET; do
    for pq_subspaces in $PQ_SUBSPACES_SET; do
      for pq_centroids in $PQ_CENTROIDS_SET; do
        echo "RUN algorithm=ivf_pq residuals=${residuals} n_probe=${n_probe} pq_subspaces=${pq_subspaces} pq_centroids=${pq_centroids}" | tee -a "$RAW_OUT"
        pq_out="$(run_ann "ivf_pq" "$residuals" "$n_probe" "$pq_subspaces" "$pq_centroids")"
        echo "$pq_out" >>"$RAW_OUT"
        echo >>"$RAW_OUT"

        pq_build="$(echo "$pq_out" | grep '^build_elapsed_ms:' | awk '{print $2}')"
        pq_p50="$(echo "$pq_out" | grep '^search_p50_ms:' | awk '{print $2}')"
        pq_p95="$(echo "$pq_out" | grep '^search_p95_ms:' | awk '{print $2}')"
        pq_recall="$(echo "$pq_out" | grep '^mean_recall_at_k:' | awk '{print $2}')"

        recall_ratio="$(awk -v pq="$pq_recall" -v ivf="$ivf_recall" 'BEGIN { if (ivf <= 0) { print "0.000000" } else { printf "%.6f", pq / ivf } }')"
        p95_ratio="$(awk -v pq="$pq_p95" -v ivf="$ivf_p95" 'BEGIN { if (ivf <= 0) { print "0.000000" } else { printf "%.6f", pq / ivf } }')"

        printf "ivf_pq,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s\n" \
          "$residuals" "$n_probe" "$pq_subspaces" "$pq_centroids" \
          "$pq_build" "$pq_p50" "$pq_p95" "$pq_recall" "$recall_ratio" "$p95_ratio" >>"$CSV_OUT"
      done
    done
  done
done

{
  echo "raw_output=${RAW_OUT}"
  echo "csv_output=${CSV_OUT}"
  echo "SUMMARY (best PQ configs by recall_ratio, then p95_ratio):"
  echo "algorithm,residuals,n_probe,pq_subspaces,pq_centroids,build_elapsed_ms,search_p50_ms,search_p95_ms,mean_recall_at_k,recall_ratio_vs_ivf,p95_ratio_vs_ivf"
  awk -F, 'NR == 1 || $1 == "ivf_pq"' "$CSV_OUT" | tail -n +2 | sort -t, -k3,3n -k10,10gr -k11,11g
} | tee -a "$RAW_OUT"
