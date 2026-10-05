#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${OUT_DIR:-$ROOT_DIR/../docs/benchmarks/results}"
STAMP="${STAMP:-$(date +%F)}"

VECTORS="${VECTORS:-50000}"
DIMENSIONS="${DIMENSIONS:-384}"
FRAGMENT_TARGET_SIZE="${FRAGMENT_TARGET_SIZE:-5000}"
MIN_DELETION_RATIOS="${MIN_DELETION_RATIOS:-0.20 0.30 0.40}"
MAX_FRAGMENTS_SET="${MAX_FRAGMENTS_SET:-2 4 8}"
DELETE_RATIOS="${DELETE_RATIOS:-0.35 0.55}"
MIN_VECTORS_TO_COMPACT="${MIN_VECTORS_TO_COMPACT:-10000}"

mkdir -p "$OUT_DIR"
RAW_OUT="$OUT_DIR/${STAMP}-vector-compaction-matrix.txt"
CSV_OUT="$OUT_DIR/${STAMP}-vector-compaction-matrix.csv"

echo "Vector compaction matrix benchmark" >"$RAW_OUT"
echo "date=${STAMP}" >>"$RAW_OUT"
echo "vectors=${VECTORS} dimensions=${DIMENSIONS} fragment_target_size=${FRAGMENT_TARGET_SIZE}" >>"$RAW_OUT"
echo "delete_ratios={${DELETE_RATIOS}}" >>"$RAW_OUT"
echo "min_deletion_ratios={${MIN_DELETION_RATIOS}}" >>"$RAW_OUT"
echo "max_fragments_set={${MAX_FRAGMENTS_SET}}" >>"$RAW_OUT"
echo "min_vectors_to_compact=${MIN_VECTORS_TO_COMPACT}" >>"$RAW_OUT"
echo >>"$RAW_OUT"

printf "delete_ratio,min_deletion_ratio,max_fragments,min_vectors_to_compact,compaction_performed,compaction_elapsed_ms,bytes_before,bytes_after,reclaim_percent,fragments_before,fragments_after\n" >"$CSV_OUT"

for delete_ratio in $DELETE_RATIOS; do
  for min_deletion_ratio in $MIN_DELETION_RATIOS; do
    for max_fragments in $MAX_FRAGMENTS_SET; do
      echo "RUN delete_ratio=${delete_ratio} min_del=${min_deletion_ratio} max_frag=${max_fragments}" | tee -a "$RAW_OUT"
      run_out="$(
        cd "$ROOT_DIR"
        cargo run --release --no-default-features --example vector_compaction_bench -- \
          --vectors "$VECTORS" \
          --dimensions "$DIMENSIONS" \
          --fragment-target-size "$FRAGMENT_TARGET_SIZE" \
          --delete-ratio "$delete_ratio" \
          --min-deletion-ratio "$min_deletion_ratio" \
          --max-fragments "$max_fragments" \
          --min-vectors-to-compact "$MIN_VECTORS_TO_COMPACT"
      )"
      echo "$run_out" >>"$RAW_OUT"
      echo >>"$RAW_OUT"

      compaction_performed="$(echo "$run_out" | grep '^compaction_performed:' | awk '{print $2}')"
      elapsed_ms="$(echo "$run_out" | grep '^compaction_elapsed_ms:' | awk '{print $2}')"
      bytes_line="$(echo "$run_out" | grep '^  bytes_used:')"
      bytes_before="$(echo "$bytes_line" | awk -F': ' '{print $2}' | awk -F' -> ' '{print $1}' | tr -d ',')"
      bytes_after="$(echo "$bytes_line" | awk -F' -> ' '{print $2}' | tr -d ',')"
      fragments_line="$(echo "$run_out" | grep '^  fragments_needing_compaction:')"
      fragments_before="$(echo "$fragments_line" | awk -F': ' '{print $2}' | awk -F' -> ' '{print $1}')"
      fragments_after="$(echo "$fragments_line" | awk -F' -> ' '{print $2}')"
      reclaim_percent="$(awk -v b="$bytes_before" -v a="$bytes_after" 'BEGIN { if (b<=0) {print "0.00"} else { printf "%.2f", ((b-a)/b)*100.0 } }')"

      printf "%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s\n" \
        "$delete_ratio" \
        "$min_deletion_ratio" \
        "$max_fragments" \
        "$MIN_VECTORS_TO_COMPACT" \
        "$compaction_performed" \
        "$elapsed_ms" \
        "$bytes_before" \
        "$bytes_after" \
        "$reclaim_percent" \
        "$fragments_before" \
        "$fragments_after" >>"$CSV_OUT"
    done
  done
done

{
  echo "raw_output=${RAW_OUT}"
  echo "csv_output=${CSV_OUT}"
  echo "SUMMARY (mean by strategy):"
  awk -F, '
    NR > 1 {
      key = $2 "," $3
      count[key]++
      elapsed[key] += $6
      reclaim[key] += $9
      compaction[key] += ($5 == "true" ? 1 : 0)
    }
    END {
      print "min_deletion_ratio,max_fragments,runs,mean_compaction_elapsed_ms,mean_reclaim_percent,compaction_performed_ratio"
      for (k in count) {
        split(k, parts, ",")
        printf "%s,%s,%d,%.3f,%.3f,%.3f\n", parts[1], parts[2], count[k], elapsed[k] / count[k], reclaim[k] / count[k], compaction[k] / count[k]
      }
    }
  ' "$CSV_OUT" | {
    IFS= read -r header
    echo "$header"
    sort -t, -k1,1 -k2,2n
  }
} | tee -a "$RAW_OUT"
