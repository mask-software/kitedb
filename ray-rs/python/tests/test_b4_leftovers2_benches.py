"""The benchmark scripts with an --iterations budget under one 100-op batch.

Both used to compute `iterations // 100` batches: the KiteDB raw bench then
printed an all-zero "Batch of 100 nodes" row, and the SQLite bench crashed
on the empty sample list.
"""

import re
import subprocess
import sys
from pathlib import Path

RAY_RS = Path(__file__).resolve().parents[2]
REPO = RAY_RS.parent
SMALL = ["--nodes", "200", "--edges", "400", "--iterations", "50"]


def run(script: Path, *args: str) -> str:
    result = subprocess.run(
        [sys.executable, str(script), *SMALL, *args],
        capture_output=True,
        text=True,
        timeout=300,
    )
    assert result.returncode == 0, result.stdout + result.stderr
    return result.stdout


def batch_row(output: str, label: str) -> str:
    rows = [line for line in output.splitlines() if line.startswith(label)]
    assert rows, f"no {label!r} row in:\n{output}"
    return rows[0]


def ops_per_sec(row: str) -> int:
    match = re.search(r"\(([\d,]+) ops/sec\)", row)
    assert match, row
    return int(match.group(1).replace(",", ""))


def test_sqlite_bench_runs_a_batch_for_small_iteration_counts():
    output = run(REPO / "docs" / "benchmarks" / "sqlite_single_file_raw_bench.py")
    assert ops_per_sec(batch_row(output, "Batch of 100 nodes")) > 0


def test_raw_bench_runs_a_batch_for_small_iteration_counts():
    output = run(
        RAY_RS / "python" / "benchmarks" / "benchmark_single_file_raw.py",
        "--no-output",
        "--vector-count",
        "10",
    )
    assert ops_per_sec(batch_row(output, "Batch of 100 nodes")) > 0
    assert ops_per_sec(batch_row(output, "Batch of 100 edges")) > 0
