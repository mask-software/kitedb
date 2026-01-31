"""
Vector Index Benchmark (Python)

Benchmarks vector index operations via the Python bindings.
Intended for comparison with NAPI and Rust vector benchmarks.

Prerequisites:
  cd ray-rs && maturin develop --features python

Usage:
  python benchmark_vector.py [options]

Options:
  --vectors N        Number of vectors (default: 10000)
  --dimensions D     Vector dimensions (default: 768)
  --iterations I     Iterations for latency benchmarks (default: 1000)
  --k N              Number of nearest neighbors (default: 10)
  --n-probe N         IVF nProbe (default: 10)
  --output FILE      Output file path (default: auto-generated)
  --no-output        Disable file output
"""

from __future__ import annotations

import argparse
import random
import sys
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import List, Optional

sys.path.insert(0, str(Path(__file__).parent.parent))

try:
  from kitedb import (
      VectorIndexOptions,
      SimilarOptions,
      create_vector_index,
      define_node,
      prop,
      NodeRef,
  )
except ImportError:
  print("Error: raydb module not found. Build the Python bindings first:")
  print("  cd ray-rs && maturin develop --features python")
  sys.exit(1)


@dataclass
class BenchConfig:
  vectors: int = 10000
  dimensions: int = 768
  iterations: int = 1000
  k: int = 10
  n_probe: int = 10
  output_file: Optional[str] = None


def parse_args() -> BenchConfig:
  parser = argparse.ArgumentParser(description="Vector Index Benchmark (Python)")
  parser.add_argument("--vectors", type=int, default=10000)
  parser.add_argument("--dimensions", type=int, default=768)
  parser.add_argument("--iterations", type=int, default=1000)
  parser.add_argument("--k", type=int, default=10)
  parser.add_argument("--n-probe", type=int, default=10)
  parser.add_argument("--output", type=str, default=None)
  parser.add_argument("--no-output", action="store_true")

  args = parser.parse_args()

  if args.output is None and not args.no_output:
    timestamp = datetime.now().strftime("%Y-%m-%dT%H-%M-%S")
    output_dir = Path(__file__).parent / "results"
    output_dir.mkdir(exist_ok=True)
    output_file = str(output_dir / f"benchmark-python-vector-{timestamp}.txt")
  elif args.no_output:
    output_file = None
  else:
    output_file = args.output

  return BenchConfig(
    vectors=args.vectors,
    dimensions=args.dimensions,
    iterations=args.iterations,
    k=args.k,
    n_probe=args.n_probe,
    output_file=output_file,
  )


class Logger:
  def __init__(self, output_file: Optional[str]):
    self.output_file = output_file
    self.buffer: List[str] = []

  def log(self, message: str = ""):
    print(message)
    self.buffer.append(message)

  def flush(self):
    if self.output_file and self.buffer:
      Path(self.output_file).parent.mkdir(parents=True, exist_ok=True)
      Path(self.output_file).write_text("\n".join(self.buffer) + "\n")


@dataclass
class LatencyStats:
  count: int
  min: int
  max: int
  sum: int
  p50: int
  p95: int
  p99: int


class LatencyTracker:
  def __init__(self):
    self.samples: List[int] = []

  def record(self, latency_ns: int):
    self.samples.append(latency_ns)

  def stats(self) -> LatencyStats:
    if not self.samples:
      return LatencyStats(0, 0, 0, 0, 0, 0, 0)
    sorted_samples = sorted(self.samples)
    count = len(sorted_samples)
    return LatencyStats(
      count=count,
      min=sorted_samples[0],
      max=sorted_samples[-1],
      sum=sum(sorted_samples),
      p50=sorted_samples[int(count * 0.50)],
      p95=sorted_samples[int(count * 0.95)],
      p99=sorted_samples[int(count * 0.99)],
    )


def format_latency(ns: int) -> str:
  if ns < 1000:
    return f"{ns}ns"
  if ns < 1_000_000:
    return f"{ns / 1000:.2f}us"
  return f"{ns / 1_000_000:.2f}ms"


def format_number(n: int) -> str:
  return f"{n:,}"


def print_latency_table(logger: Logger, name: str, stats: LatencyStats):
  ops_per_sec = stats.count / (stats.sum / 1_000_000_000) if stats.sum > 0 else 0
  logger.log(
    f"{name:<40} p50={format_latency(stats.p50):>10} p95={format_latency(stats.p95):>10} p99={format_latency(stats.p99):>10} ({format_number(round(ops_per_sec))} ops/sec)"
  )


def generate_random_vector(dimensions: int) -> List[float]:
  return [random.uniform(-1.0, 1.0) for _ in range(dimensions)]


def generate_random_vectors(count: int, dimensions: int) -> List[List[float]]:
  return [generate_random_vector(dimensions) for _ in range(count)]


def run_benchmarks(config: BenchConfig) -> int:
  logger = Logger(config.output_file)

  logger.log("=" * 120)
  logger.log("Vector Index Benchmark (Python)")
  logger.log("=" * 120)
  logger.log(f"Date: {datetime.now(timezone.utc).isoformat()}")
  logger.log(f"Vectors: {format_number(config.vectors)}")
  logger.log(f"Dimensions: {config.dimensions}")
  logger.log(f"Iterations: {format_number(config.iterations)}")
  logger.log(f"k: {config.k}")
  logger.log(f"nProbe: {config.n_probe}")
  logger.log("=" * 120)

  node_def = define_node(
    "vector_node",
    key=lambda node_id: f"vec:{node_id}",
    props={"embedding": prop.vector("embedding")},
  )

  index = create_vector_index(
    VectorIndexOptions(
      dimensions=config.dimensions,
      metric="cosine",
      ivf={"n_probe": config.n_probe},
      training_threshold=1000,
    )
  )

  vectors = generate_random_vectors(config.vectors, config.dimensions)
  node_refs = [NodeRef(i, f"vec:{i}", node_def) for i in range(config.vectors)]

  logger.log("\n--- Vector Index Benchmarks (Python) ---")

  logger.log("\n  Insert benchmarks:")
  insert_tracker = LatencyTracker()
  insert_start = time.perf_counter_ns()
  for i in range(config.vectors):
    start = time.perf_counter_ns()
    index.set(node_refs[i], vectors[i])
    insert_tracker.record(time.perf_counter_ns() - start)
  insert_total = time.perf_counter_ns() - insert_start
  print_latency_table(logger, f"Set ({format_number(config.vectors)} vectors)", insert_tracker.stats())
  logger.log(
    f"  Total set time: {format_latency(insert_total)} ({format_number(round((config.vectors * 1_000_000_000) / insert_total))} vectors/sec)"
  )

  logger.log("\n  Index build:")
  build_start = time.perf_counter_ns()
  index.build_index()
  build_time = time.perf_counter_ns() - build_start
  logger.log(f"  build_index(): {format_latency(build_time)}")

  logger.log("\n  Lookup benchmarks:")
  lookup_tracker = LatencyTracker()
  for _ in range(config.iterations):
    node_ref = node_refs[random.randrange(config.vectors)]
    start = time.perf_counter_ns()
    index.get(node_ref)
    lookup_tracker.record(time.perf_counter_ns() - start)
  print_latency_table(logger, "Random get", lookup_tracker.stats())

  logger.log("\n  Search benchmarks:")
  search_tracker = LatencyTracker()
  search_opts = SimilarOptions(k=config.k, n_probe=config.n_probe)
  for _ in range(config.iterations):
    query = generate_random_vector(config.dimensions)
    start = time.perf_counter_ns()
    index.search(query, search_opts)
    search_tracker.record(time.perf_counter_ns() - start)
  print_latency_table(
    logger,
    f"Search (k={config.k}, nProbe={config.n_probe})",
    search_tracker.stats(),
  )

  logger.log("\n  Index stats:")
  try:
    stats = index.stats()
    logger.log(f"    Total vectors: {format_number(int(stats.get('totalVectors', 0)))}")
    logger.log(f"    Live vectors: {format_number(int(stats.get('liveVectors', 0)))}")
    logger.log(f"    Dimensions: {stats.get('dimensions', 0)}")
    logger.log(f"    Metric: {stats.get('metric', 'unknown')}")
    logger.log(f"    Index trained: {stats.get('indexTrained', False)}")
    if stats.get("indexClusters") is not None:
      logger.log(f"    Index clusters: {stats.get('indexClusters')}")
  except Exception as exc:
    logger.log(f"    Stats unavailable: {exc}")

  logger.log("\n" + "=" * 120)
  logger.log("Vector benchmark complete.")
  logger.log("=" * 120)

  logger.flush()
  if config.output_file:
    print(f"\nResults saved to: {config.output_file}")
  return 0


if __name__ == "__main__":
  sys.exit(run_benchmarks(parse_args()))
