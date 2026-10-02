#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${OUT_DIR:-$ROOT_DIR/../docs/benchmarks/results}"

ATTEMPTS="${ATTEMPTS:-1}"
# Limits on the criterion median of open + close, read-write and read-only, for
# a 10k-node/20k-edge graph (small) and a 100k/200k one (large), calibrated on
# CI's ubuntu-latest runner.
#
# Since the snapshot hardening in 9462079 (July 2026), open checks the whole
# snapshot structure (bounds, monotonic offsets, node ID maps, key index), and
# to do so inflates every compressed section up front (zstd checkpoint
# compression is on by default). Before, open verified the CRC and inflated
# sections lazily on first use. That made open 4-5x slower, in proportion to
# the snapshot size. Median of 3 attempts, in us:
#                               small-rw small-ro large-rw large-ro
#   macOS  0aefe83 (Feb 2026)        268      269     2002     1982
#   macOS  9462079 (hardening)      1206     1170    10887    10865
#   macOS  6ea5f99 (S4-S13 reader)  1180     1137    10610    10563
#   macOS  2026-10-02 main          1186     1142    10614    10608
#   Linux  0aefe83 (Docker)          601      631     2315     2423
#   Linux  2026-10-02 main (Docker) 1623     1073    10840    10333
# A large open at main spends ~70% inflating, ~25% in the structure checks and
# ~7% on the CRC. The previous limits (900 and 5000 us) predate the hardening;
# CI failed them on all eight main pushes of 2026-10-02, measuring:
#   small-rw 1776-2502 (median 1826), small-ro 831-1573 (1502),
#   large-rw 10525-20609 (16076), large-ro 8836-15391 (14826).
# The limits are about 1.75x CI's median read-only time and 1.9x its median
# read-write time (a read-write close also syncs, and slow runner disks widen
# its spread): a change that doubles open, such as a second parse or inflate
# pass or a superlinear check, fails on a typical runner, while the slowest
# runner seen keeps 1.4-1.7x of headroom.
MAX_SMALL_RW_US="${MAX_SMALL_RW_US:-3500.0}"
MAX_SMALL_RO_US="${MAX_SMALL_RO_US:-2600.0}"
MAX_LARGE_RW_US="${MAX_LARGE_RW_US:-30000.0}"
MAX_LARGE_RO_US="${MAX_LARGE_RO_US:-26000.0}"

if [[ "$ATTEMPTS" -lt 1 ]]; then
  echo "ATTEMPTS must be >= 1"
  exit 1
fi

mkdir -p "$OUT_DIR"
STAMP="${STAMP:-$(date +%F)}"
LOG_BASE="$OUT_DIR/${STAMP}-open-close-non-vector-gate"
BENCH_FILTER='single_file_open_close/open_close/(rw|ro)/graph_10k_20k$|single_file_open_close_limits/open_close/(rw|ro)/graph_100k_200k$'

extract_median_us() {
  local logfile="$1"
  local bench_id="$2"
  local line
  line="$(
    awk -v bench_id="$bench_id" '
      $0 == bench_id { in_block = 1; next }
      in_block && $1 == "time:" { print; exit }
    ' "$logfile"
  )"
  if [[ -z "$line" ]]; then
    return 1
  fi

  local value unit
  value="$(awk '{print $4}' <<<"$line")"
  unit="$(awk '{print $5}' <<<"$line")"
  unit="${unit//]/}"

  awk -v value="$value" -v unit="$unit" 'BEGIN {
    if (unit == "ns") {
      printf "%.6f", value / 1000.0
    } else if (unit == "us" || unit == "µs") {
      printf "%.6f", value + 0.0
    } else if (unit == "ms") {
      printf "%.6f", value * 1000.0
    } else if (unit == "s") {
      printf "%.6f", value * 1000000.0
    } else {
      exit 1
    }
  }'
}

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

declare -a small_rw_values=()
declare -a small_ro_values=()
declare -a large_rw_values=()
declare -a large_ro_values=()
last_log=""

echo "== Open/close non-vector gate (attempts: $ATTEMPTS)"
for attempt in $(seq 1 "$ATTEMPTS"); do
  if [[ "$ATTEMPTS" -eq 1 ]]; then
    logfile="${LOG_BASE}.txt"
  else
    logfile="${LOG_BASE}.attempt${attempt}.txt"
  fi
  last_log="$logfile"

  (
    cd "$ROOT_DIR"
    cargo bench --bench single_file --no-default-features -- "$BENCH_FILTER" >"$logfile"
  )

  small_rw_us="$(extract_median_us "$logfile" "single_file_open_close/open_close/rw/graph_10k_20k")"
  small_ro_us="$(extract_median_us "$logfile" "single_file_open_close/open_close/ro/graph_10k_20k")"
  large_rw_us="$(extract_median_us "$logfile" "single_file_open_close_limits/open_close/rw/graph_100k_200k")"
  large_ro_us="$(extract_median_us "$logfile" "single_file_open_close_limits/open_close/ro/graph_100k_200k")"

  if [[ -z "$small_rw_us" || -z "$small_ro_us" || -z "$large_rw_us" || -z "$large_ro_us" ]]; then
    echo "failed: could not parse one or more non-vector open/close medians"
    echo "log: $logfile"
    exit 1
  fi

  small_rw_values+=("$small_rw_us")
  small_ro_values+=("$small_ro_us")
  large_rw_values+=("$large_rw_us")
  large_ro_values+=("$large_ro_us")

  echo "attempt $attempt/$ATTEMPTS:"
  echo "  small-rw median_us = $small_rw_us"
  echo "  small-ro median_us = $small_ro_us"
  echo "  large-rw median_us = $large_rw_us"
  echo "  large-ro median_us = $large_ro_us"
done

median_small_rw="$(median "${small_rw_values[@]}")"
median_small_ro="$(median "${small_ro_values[@]}")"
median_large_rw="$(median "${large_rw_values[@]}")"
median_large_ro="$(median "${large_ro_values[@]}")"

if [[ "$median_small_rw" == "NaN" || "$median_small_ro" == "NaN" || "$median_large_rw" == "NaN" || "$median_large_ro" == "NaN" ]]; then
  echo "failed: no medians captured"
  exit 1
fi

small_rw_pass="$(awk -v actual="$median_small_rw" -v max="$MAX_SMALL_RW_US" 'BEGIN { if (actual <= max) print "yes"; else print "no" }')"
small_ro_pass="$(awk -v actual="$median_small_ro" -v max="$MAX_SMALL_RO_US" 'BEGIN { if (actual <= max) print "yes"; else print "no" }')"
large_rw_pass="$(awk -v actual="$median_large_rw" -v max="$MAX_LARGE_RW_US" 'BEGIN { if (actual <= max) print "yes"; else print "no" }')"
large_ro_pass="$(awk -v actual="$median_large_ro" -v max="$MAX_LARGE_RO_US" 'BEGIN { if (actual <= max) print "yes"; else print "no" }')"

echo "median small-rw across $ATTEMPTS attempt(s): ${median_small_rw}us (max allowed: ${MAX_SMALL_RW_US}us)"
echo "median small-ro across $ATTEMPTS attempt(s): ${median_small_ro}us (max allowed: ${MAX_SMALL_RO_US}us)"
echo "median large-rw across $ATTEMPTS attempt(s): ${median_large_rw}us (max allowed: ${MAX_LARGE_RW_US}us)"
echo "median large-ro across $ATTEMPTS attempt(s): ${median_large_ro}us (max allowed: ${MAX_LARGE_RO_US}us)"
echo "log: $last_log"

if [[ "$small_rw_pass" != "yes" || "$small_ro_pass" != "yes" || "$large_rw_pass" != "yes" || "$large_ro_pass" != "yes" ]]; then
  echo "failed: open/close non-vector gate not satisfied"
  exit 1
fi

echo "pass: open/close non-vector gate satisfied"
