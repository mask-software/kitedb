#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

BENCH_NAME="${1:-write_path}"
OUT_TRACE="${2:-write-path-timeprofile.trace}"

cargo bench --bench "$BENCH_NAME" --no-run

BIN=""
for candidate in "$ROOT/target/release/deps/${BENCH_NAME}-"*; do
  if [[ -x "$candidate" && "${candidate##*.}" != "d" ]]; then
    BIN="$candidate"
    break
  fi
done

if [[ -z "$BIN" ]]; then
  echo "bench binary not found for ${BENCH_NAME}"
  exit 1
fi

if ! command -v xcrun >/dev/null 2>&1; then
  echo "xcrun not found. install Xcode Command Line Tools."
  exit 1
fi

if ! xcrun --find xctrace >/dev/null 2>&1; then
  echo "xctrace not found. install Instruments (Xcode)."
  exit 1
fi

xcrun xctrace record --template "Time Profiler" --output "$OUT_TRACE" --launch -- "$BIN"
echo "wrote $OUT_TRACE"
