#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${OUT_DIR:-$ROOT_DIR/../docs/benchmarks/results}"

REPLICAS="${REPLICAS:-5}"
CYCLES="${CYCLES:-6}"
COMMITS_PER_CYCLE="${COMMITS_PER_CYCLE:-40}"
ACTIVE_REPLICAS="${ACTIVE_REPLICAS:-3}"
CHURN_INTERVAL="${CHURN_INTERVAL:-2}"
PROMOTION_INTERVAL="${PROMOTION_INTERVAL:-3}"
RESEED_CHECK_INTERVAL="${RESEED_CHECK_INTERVAL:-2}"
MAX_FRAMES="${MAX_FRAMES:-128}"
RECOVERY_MAX_LOOPS="${RECOVERY_MAX_LOOPS:-80}"
SEGMENT_MAX_BYTES="${SEGMENT_MAX_BYTES:-1}"
RETENTION_MIN="${RETENTION_MIN:-64}"
SYNC_MODE="${SYNC_MODE:-normal}"
ATTEMPTS="${ATTEMPTS:-1}"

MAX_ALLOWED_LAG="${MAX_ALLOWED_LAG:-1200}"
MIN_PROMOTIONS="${MIN_PROMOTIONS:-2}"
MIN_RESEEDS="${MIN_RESEEDS:-1}"

if [[ "$ATTEMPTS" -lt 1 ]]; then
  echo "ATTEMPTS must be >= 1"
  exit 1
fi

mkdir -p "$OUT_DIR"
STAMP="${STAMP:-$(date +%F)}"
LOGFILE_BASE="$OUT_DIR/${STAMP}-replication-soak-gate"

extract_metric() {
  local key="$1"
  local file="$2"
  grep "^${key}:" "$file" | tail -1 | awk '{print $2}'
}

echo "== Replication soak gate (attempts: $ATTEMPTS)"
for attempt in $(seq 1 "$ATTEMPTS"); do
  if [[ "$ATTEMPTS" -eq 1 ]]; then
    logfile="${LOGFILE_BASE}.txt"
  else
    logfile="${LOGFILE_BASE}.attempt${attempt}.txt"
  fi

  (
    cd "$ROOT_DIR"
    cargo run --release --example replication_soak_bench --no-default-features -- \
      --replicas "$REPLICAS" \
      --cycles "$CYCLES" \
      --commits-per-cycle "$COMMITS_PER_CYCLE" \
      --active-replicas "$ACTIVE_REPLICAS" \
      --churn-interval "$CHURN_INTERVAL" \
      --promotion-interval "$PROMOTION_INTERVAL" \
      --reseed-check-interval "$RESEED_CHECK_INTERVAL" \
      --max-frames "$MAX_FRAMES" \
      --recovery-max-loops "$RECOVERY_MAX_LOOPS" \
      --segment-max-bytes "$SEGMENT_MAX_BYTES" \
      --retention-min "$RETENTION_MIN" \
      --sync-mode "$SYNC_MODE" >"$logfile"
  )

  divergence="$(extract_metric divergence_violations "$logfile")"
  promotions="$(extract_metric promotion_count "$logfile")"
  stale_fence="$(extract_metric stale_fence_rejections "$logfile")"
  reseeds="$(extract_metric reseed_count "$logfile")"
  recovery_loops="$(extract_metric max_recovery_loops "$logfile")"
  max_lag="$(extract_metric max_observed_lag "$logfile")"

  if [[ -z "$divergence" || -z "$promotions" || -z "$stale_fence" || -z "$reseeds" || -z "$recovery_loops" || -z "$max_lag" ]]; then
    echo "failed: could not parse soak metrics"
    echo "log: $logfile"
    exit 1
  fi

  divergence_pass="no"
  stale_pass="no"
  promotions_pass="no"
  reseed_pass="no"
  recovery_pass="no"
  lag_pass="no"

  [[ "$divergence" -eq 0 ]] && divergence_pass="yes"
  [[ "$stale_fence" -eq "$promotions" ]] && stale_pass="yes"
  [[ "$promotions" -ge "$MIN_PROMOTIONS" ]] && promotions_pass="yes"
  [[ "$reseeds" -ge "$MIN_RESEEDS" ]] && reseed_pass="yes"
  [[ "$recovery_loops" -le "$RECOVERY_MAX_LOOPS" ]] && recovery_pass="yes"
  [[ "$max_lag" -le "$MAX_ALLOWED_LAG" ]] && lag_pass="yes"

  echo "attempt $attempt/$ATTEMPTS: divergence=$divergence promotions=$promotions stale_fence=$stale_fence reseeds=$reseeds max_recovery_loops=$recovery_loops max_lag=$max_lag"

  if [[ "$divergence_pass" == "yes" && "$stale_pass" == "yes" && "$promotions_pass" == "yes" && "$reseed_pass" == "yes" && "$recovery_pass" == "yes" && "$lag_pass" == "yes" ]]; then
    echo "pass: replication soak gate satisfied"
    echo "log:"
    echo "  $logfile"
    exit 0
  fi
done

echo "failed: replication soak gate did not pass in $ATTEMPTS attempt(s)"
echo "thresholds: divergence=0, stale_fence==promotions, promotions>=${MIN_PROMOTIONS}, reseeds>=${MIN_RESEEDS}, max_recovery_loops<=${RECOVERY_MAX_LOOPS}, max_lag<=${MAX_ALLOWED_LAG}"
echo "last log:"
echo "  $logfile"
exit 1
