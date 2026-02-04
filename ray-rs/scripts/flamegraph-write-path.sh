#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

BENCH_NAME="${1:-write_path}"
OUT_SVG="${2:-flamegraph-write-path.svg}"

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

if ! command -v perf >/dev/null 2>&1; then
  echo "perf not found. install perf to capture call stacks."
  exit 1
fi

perf record --call-graph=dwarf "$BIN"

if command -v inferno-flamegraph >/dev/null 2>&1; then
  perf script | inferno-flamegraph > "$OUT_SVG"
  echo "wrote $OUT_SVG"
  exit 0
fi

if command -v stackcollapse-perf.pl >/dev/null 2>&1 && command -v flamegraph.pl >/dev/null 2>&1; then
  perf script | stackcollapse-perf.pl | flamegraph.pl > "$OUT_SVG"
  echo "wrote $OUT_SVG"
  exit 0
fi

echo "perf.data captured. install inferno or FlameGraph scripts to render SVG."
