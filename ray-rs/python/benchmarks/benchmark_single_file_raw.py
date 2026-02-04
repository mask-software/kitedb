#!/usr/bin/env python3
"""
Single-file Raw Benchmark (Python)

Benchmarks low-level Database operations via the Python bindings.
Intended for apples-to-apples comparison with bench/benchmark-single-file-raw.ts.

Prerequisites:
  maturin develop --features python

Usage:
  python benchmark_single_file_raw.py [options]

Options:
  --nodes N                 Number of nodes (default: 10000)
  --edges M                 Number of edges (default: 50000)
  --edge-types N            Number of edge types (default: 3)
  --edge-props N            Number of props per edge (default: 10)
  --iterations I            Iterations for latency benchmarks (default: 10000)
  --output FILE             Output file path (default: auto-generated)
  --no-output               Disable file output
  --keep-db                 Keep the database file after benchmark
  --wal-size BYTES          WAL size in bytes (default: 67108864)
  --sync-mode MODE          Sync mode: full|normal|off (default: normal)
  --group-commit-enabled    Enable group commit (default: false)
  --group-commit-window-ms  Group commit window in ms (default: 2)
  --checkpoint-threshold P  Auto-checkpoint threshold (default: 0.8)
  --no-auto-checkpoint      Disable auto-checkpoint
  --cache-enabled           Enable cache
  --vector-dims N            Vector dimensions (default: 128)
  --vector-count N           Number of vectors to set (default: 1000)
  --skip-compact            Skip optimize/compaction step
  --reopen-readonly         Re-open database in read-only mode after compaction
"""

import argparse
import os
import random
import sys
import tempfile
import time
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import List, Optional

sys.path.insert(0, str(Path(__file__).parent.parent))

try:
  from kitedb import Database, OpenOptions, PropValue, SyncMode
except ImportError:
  print("Error: kitedb module not found. Build the Python bindings first:")
  print("  maturin develop --features python")
  sys.exit(1)


@dataclass
class BenchConfig:
  nodes: int = 10000
  edges: int = 50000
  edge_types: int = 3
  edge_props: int = 10
  iterations: int = 10000
  output_file: Optional[str] = None
  keep_db: bool = False
  wal_size: int = 64 * 1024 * 1024
  sync_mode: str = "normal"
  group_commit_enabled: bool = False
  group_commit_window_ms: int = 2
  checkpoint_threshold: float = 0.8
  auto_checkpoint: bool = True
  cache_enabled: bool = False
  vector_dims: int = 128
  vector_count: int = 1000
  skip_compact: bool = False
  reopen_readonly: bool = False


def parse_args() -> BenchConfig:
  parser = argparse.ArgumentParser(description="Single-file Raw Benchmark (Python)")
  parser.add_argument("--nodes", type=int, default=10000)
  parser.add_argument("--edges", type=int, default=50000)
  parser.add_argument("--edge-types", type=int, default=3)
  parser.add_argument("--edge-props", type=int, default=10)
  parser.add_argument("--iterations", type=int, default=10000)
  parser.add_argument("--output", type=str, default=None)
  parser.add_argument("--no-output", action="store_true")
  parser.add_argument("--keep-db", action="store_true")
  parser.add_argument("--wal-size", type=int, default=64 * 1024 * 1024)
  parser.add_argument("--sync-mode", type=str, default="normal")
  parser.add_argument("--group-commit-enabled", action="store_true")
  parser.add_argument("--group-commit-window-ms", type=int, default=2)
  parser.add_argument("--checkpoint-threshold", type=float, default=0.8)
  parser.add_argument("--no-auto-checkpoint", action="store_true")
  parser.add_argument("--cache-enabled", action="store_true")
  parser.add_argument("--vector-dims", type=int, default=128)
  parser.add_argument("--vector-count", type=int, default=1000)
  parser.add_argument("--skip-compact", action="store_true")
  parser.add_argument("--reopen-readonly", action="store_true")

  args = parser.parse_args()

  if args.output is None and not args.no_output:
    timestamp = datetime.now().strftime("%Y-%m-%dT%H-%M-%S")
    output_dir = Path(__file__).parent / "results"
    output_dir.mkdir(exist_ok=True)
    output_file = str(output_dir / f"benchmark-python-raw-{timestamp}.txt")
  elif args.no_output:
    output_file = None
  else:
    output_file = args.output

  return BenchConfig(
    nodes=args.nodes,
    edges=args.edges,
    edge_types=max(1, args.edge_types),
    edge_props=max(0, args.edge_props),
    iterations=args.iterations,
    output_file=output_file,
    keep_db=args.keep_db,
    wal_size=args.wal_size,
    sync_mode=str(args.sync_mode).lower(),
    group_commit_enabled=args.group_commit_enabled,
    group_commit_window_ms=args.group_commit_window_ms,
    checkpoint_threshold=args.checkpoint_threshold,
    auto_checkpoint=not args.no_auto_checkpoint,
    cache_enabled=args.cache_enabled,
    vector_dims=args.vector_dims,
    vector_count=args.vector_count,
    skip_compact=args.skip_compact,
    reopen_readonly=args.reopen_readonly,
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
      with open(self.output_file, "w") as f:
        f.write("\n".join(self.buffer) + "\n")


logger: Logger


@dataclass
class LatencyStats:
  count: int
  min_ns: float
  max_ns: float
  sum_ns: float
  p50: float
  p95: float
  p99: float


class LatencyTracker:
  def __init__(self):
    self.samples: List[float] = []

  def record(self, latency_ns: float):
    self.samples.append(latency_ns)

  def get_stats(self) -> LatencyStats:
    if not self.samples:
      return LatencyStats(0, 0, 0, 0, 0, 0, 0)

    sorted_samples = sorted(self.samples)
    count = len(sorted_samples)
    return LatencyStats(
      count=count,
      min_ns=sorted_samples[0],
      max_ns=sorted_samples[-1],
      sum_ns=sum(sorted_samples),
      p50=sorted_samples[int(count * 0.5)],
      p95=sorted_samples[int(count * 0.95)],
      p99=sorted_samples[int(count * 0.99)],
    )


def format_latency(ns: float) -> str:
  if ns < 1000:
    return f"{ns:.0f}ns"
  if ns < 1_000_000:
    return f"{ns / 1000:.2f}us"
  return f"{ns / 1_000_000:.2f}ms"


def format_number(n: int) -> str:
  return f"{n:,}"


def print_latency_table(name: str, stats: LatencyStats):
  ops_per_sec = stats.count / (stats.sum_ns / 1_000_000_000) if stats.sum_ns > 0 else 0
  logger.log(
    f"{name:<45} p50={format_latency(stats.p50):>10} "
    f"p95={format_latency(stats.p95):>10} p99={format_latency(stats.p99):>10} "
    f"max={format_latency(stats.max_ns):>10} ({format_number(int(ops_per_sec))} ops/sec)"
  )


@dataclass
class GraphData:
  node_ids: List[int]
  node_keys: List[str]
  edge_types: List[int]
  edge_prop_keys: List[int]


def build_random_vector(dimensions: int) -> List[float]:
  return [random.random() for _ in range(dimensions)]


def build_graph(db: Database, config: BenchConfig) -> GraphData:
  node_ids: List[int] = []
  node_keys: List[str] = []
  batch_size = 5000

  logger.log("  Creating nodes...")
  edge_types: List[int] = []
  edge_prop_keys: List[int] = []
  for batch in range(0, config.nodes, batch_size):
    db.begin_bulk()
    if batch == 0:
      for idx in range(config.edge_types):
        edge_types.append(db.get_or_create_etype(f"CALLS_{idx}"))
      for idx in range(config.edge_props):
        edge_prop_keys.append(db.get_or_create_propkey(f"edge_prop_{idx}"))

    end = min(batch + batch_size, config.nodes)
    keys = []
    for i in range(batch, end):
      keys.append(f"pkg.module{i // 100}.Class{i % 100}")
    batch_ids = db.create_nodes_batch(keys)
    node_ids.extend(batch_ids)
    node_keys.extend(keys)

    db.commit()
    print(f"\r  Created {end} / {config.nodes} nodes", end="", flush=True)
  print()

  logger.log("  Creating edges...")
  edges_created = 0
  attempts = 0
  max_attempts = config.edges * 3

  while edges_created < config.edges and attempts < max_attempts:
    db.begin_bulk()
    batch_target = min(edges_created + batch_size, config.edges)
    edges_batch = []
    edges_with_props_batch = []

    while edges_created < batch_target and attempts < max_attempts:
      attempts += 1
      src = random.choice(node_ids)
      dst = random.choice(node_ids)
      if src != dst:
        etype = random.choice(edge_types)
        if edge_prop_keys:
          props = []
          for offset, key_id in enumerate(edge_prop_keys):
            props.append((key_id, PropValue.int(edges_created + offset)))
          edges_with_props_batch.append((src, etype, dst, props))
        else:
          edges_batch.append((src, etype, dst))
        edges_created += 1

    if edge_prop_keys:
      db.add_edges_with_props_batch(edges_with_props_batch)
    else:
      db.add_edges_batch(edges_batch)

    db.commit()
    print(f"\r  Created {edges_created} / {config.edges} edges", end="", flush=True)
  print()

  return GraphData(
    node_ids=node_ids,
    node_keys=node_keys,
    edge_types=edge_types,
    edge_prop_keys=edge_prop_keys,
  )


def benchmark_key_lookups(db: Database, graph: GraphData, iterations: int):
  logger.log("\n--- Key Lookups (get_node_by_key) ---")
  tracker = LatencyTracker()
  for _ in range(iterations):
    key = random.choice(graph.node_keys)
    start = time.perf_counter_ns()
    db.get_node_by_key(key)
    tracker.record(time.perf_counter_ns() - start)
  print_latency_table("Random existing keys", tracker.get_stats())


def benchmark_traversals(db: Database, graph: GraphData, iterations: int):
  logger.log("\n--- 1-Hop Traversals (out) ---")
  tracker = LatencyTracker()
  for _ in range(iterations):
    node_id = random.choice(graph.node_ids)
    start = time.perf_counter_ns()
    edges = db.get_out_edges(node_id)
    _count = len(edges)
    tracker.record(time.perf_counter_ns() - start)
  print_latency_table("Random nodes", tracker.get_stats())


def benchmark_edge_exists(db: Database, graph: GraphData, iterations: int):
  logger.log("\n--- Edge Exists ---")
  tracker = LatencyTracker()
  for _ in range(iterations):
    src = random.choice(graph.node_ids)
    dst = random.choice(graph.node_ids)
    etype = random.choice(graph.edge_types)
    start = time.perf_counter_ns()
    db.edge_exists(src, etype, dst)
    tracker.record(time.perf_counter_ns() - start)
  print_latency_table("Random edge exists", tracker.get_stats())


def benchmark_vectors(db: Database, graph: GraphData, config: BenchConfig) -> Optional[tuple[int, List[int]]]:
  if config.vector_count <= 0 or config.vector_dims <= 0:
    logger.log("\n--- Vector Operations ---")
    logger.log("  Skipped (vector_count/vector_dims <= 0)")
    return None

  logger.log("\n--- Vector Operations ---")
  vector_count = min(config.vector_count, len(graph.node_ids))
  vector_nodes = graph.node_ids[:vector_count]

  db.begin()
  prop_key_id = db.get_or_create_propkey("embedding")
  db.commit()

  vectors = [build_random_vector(config.vector_dims) for _ in range(vector_count)]

  batch_size = 100
  tracker = LatencyTracker()

  for i in range(0, vector_count, batch_size):
    end = min(i + batch_size, vector_count)
    start = time.perf_counter_ns()
    db.begin()
    for j in range(i, end):
      db.set_node_vector(vector_nodes[j], prop_key_id, vectors[j])
    db.commit()
    tracker.record(time.perf_counter_ns() - start)

  print_latency_table(f"Set vectors (batch {batch_size})", tracker.get_stats())
  return prop_key_id, vector_nodes


def benchmark_vector_reads(db: Database, vector_nodes: List[int], prop_key_id: int, iterations: int):
  tracker = LatencyTracker()
  for _ in range(iterations):
    node_id = random.choice(vector_nodes)
    start = time.perf_counter_ns()
    db.get_node_vector(node_id, prop_key_id)
    tracker.record(time.perf_counter_ns() - start)
  print_latency_table("get_node_vector() random", tracker.get_stats())

  tracker = LatencyTracker()
  for _ in range(iterations):
    node_id = random.choice(vector_nodes)
    start = time.perf_counter_ns()
    db.has_node_vector(node_id, prop_key_id)
    tracker.record(time.perf_counter_ns() - start)
  print_latency_table("has_node_vector() random", tracker.get_stats())


def create_bench_nodes(db: Database, label: str, count: int) -> List[int]:
  if count <= 0:
    return []
  db.begin_bulk()
  keys = [f"{label}:{idx}" for idx in range(count)]
  node_ids = db.create_nodes_batch(keys)
  db.commit()
  return node_ids


def benchmark_writes(db: Database, graph: GraphData, iterations: int):
  logger.log("\n--- Batch Writes (100 nodes) ---")
  batch_size = 100
  batches = min(iterations // batch_size, 50)
  tracker = LatencyTracker()
  for b in range(batches):
    start = time.perf_counter_ns()
    db.begin_bulk()
    keys = [f"bench:raw:{b}:{i}" for i in range(batch_size)]
    db.create_nodes_batch(keys)
    db.commit()
    tracker.record(time.perf_counter_ns() - start)
  print_latency_table("Batch of 100 nodes", tracker.get_stats())

  if batches == 0:
    return

  edge_batch_size = 100
  edge_batches = min(iterations // edge_batch_size, 50)
  if edge_batches == 0:
    return

  edge_etype = graph.edge_types[0] if graph.edge_types else db.get_or_create_etype("BENCH_EDGE")

  logger.log("\n--- Batch Writes (100 edges) ---")
  total_edges = edge_batches * edge_batch_size
  edge_nodes = create_bench_nodes(db, "bench:edge", total_edges * 2)
  tracker = LatencyTracker()
  for b in range(edge_batches):
    start = time.perf_counter_ns()
    db.begin_bulk()
    base = b * edge_batch_size * 2
    edges = []
    for i in range(edge_batch_size):
      src = edge_nodes[base + i * 2]
      dst = edge_nodes[base + i * 2 + 1]
      edges.append((src, edge_etype, dst))
    db.add_edges_batch(edges)
    db.commit()
    tracker.record(time.perf_counter_ns() - start)
  print_latency_table("Batch of 100 edges", tracker.get_stats())

  if not graph.edge_prop_keys:
    return

  logger.log("\n--- Batch Writes (100 edges + props) ---")
  edge_prop_nodes = create_bench_nodes(db, "bench:edge-props", total_edges * 2)
  tracker = LatencyTracker()
  for b in range(edge_batches):
    start = time.perf_counter_ns()
    db.begin_bulk()
    base = b * edge_batch_size * 2
    edges = []
    for i in range(edge_batch_size):
      src = edge_prop_nodes[base + i * 2]
      dst = edge_prop_nodes[base + i * 2 + 1]
      props = []
      for idx, key_id in enumerate(graph.edge_prop_keys):
        props.append((key_id, PropValue.int(b * edge_batch_size + i + idx)))
      edges.append((src, edge_etype, dst, props))
    db.add_edges_with_props_batch(edges)
    db.commit()
    tracker.record(time.perf_counter_ns() - start)
  print_latency_table("Batch of 100 edges + props", tracker.get_stats())


def run_benchmarks(config: BenchConfig):
  global logger
  logger = Logger(config.output_file)

  logger.log("=" * 120)
  logger.log("Single-file Raw Benchmark (Python)")
  logger.log("=" * 120)
  logger.log(f"Date: {datetime.now().isoformat()}")
  logger.log(f"Nodes: {format_number(config.nodes)}")
  logger.log(f"Edges: {format_number(config.edges)}")
  logger.log(f"Edge types: {format_number(config.edge_types)}")
  logger.log(f"Edge props: {format_number(config.edge_props)}")
  logger.log(f"Iterations: {format_number(config.iterations)}")
  logger.log(f"WAL size: {format_number(config.wal_size)} bytes")
  logger.log(f"Sync mode: {config.sync_mode}")
  logger.log(f"Group commit: {config.group_commit_enabled} (window {config.group_commit_window_ms}ms)")
  logger.log(f"Auto-checkpoint: {config.auto_checkpoint}")
  logger.log(f"Checkpoint threshold: {config.checkpoint_threshold}")
  logger.log(f"Cache enabled: {config.cache_enabled}")
  logger.log(f"Vector dims: {format_number(config.vector_dims)}")
  logger.log(f"Vector count: {format_number(config.vector_count)}")
  logger.log(f"Skip compact: {config.skip_compact}")
  logger.log(f"Reopen read-only: {config.reopen_readonly}")
  logger.log("=" * 120)

  tmp_dir = tempfile.mkdtemp(prefix="kitedb-python-raw-")
  db_path = os.path.join(tmp_dir, "benchmark.kitedb")

  db: Optional[Database] = None
  try:
    logger.log("\n[1/6] Building graph...")
    sync_mode = SyncMode.normal()
    if config.sync_mode == "full":
      sync_mode = SyncMode.full()
    elif config.sync_mode == "off":
      sync_mode = SyncMode.off()

    options = OpenOptions(
      wal_size=config.wal_size,
      auto_checkpoint=config.auto_checkpoint,
      checkpoint_threshold=config.checkpoint_threshold,
      cache_enabled=config.cache_enabled,
      sync_mode=sync_mode,
      group_commit_enabled=config.group_commit_enabled,
      group_commit_window_ms=config.group_commit_window_ms,
    )
    db = Database(db_path, options)
    start_build = time.perf_counter()
    graph = build_graph(db, config)
    logger.log(f"  Built in {(time.perf_counter() - start_build) * 1000:.0f}ms")

    logger.log("\n[2/6] Vector setup...")
    vector_setup = benchmark_vectors(db, graph, config)

    logger.log("\n[3/6] Compacting...")
    if config.skip_compact:
      logger.log("  Skipped compaction")
    else:
      start_compact = time.perf_counter()
      db.optimize()
      logger.log(f"  Compacted in {(time.perf_counter() - start_compact) * 1000:.0f}ms")

    if config.reopen_readonly:
      db.close()
      db = Database(db_path, OpenOptions(read_only=True, create_if_missing=False, cache_enabled=config.cache_enabled))
      logger.log("  Re-opened database in read-only mode")

    logger.log("\n[4/6] Key lookup benchmarks...")
    benchmark_key_lookups(db, graph, config.iterations)

    logger.log("\n[5/6] Traversal and edge benchmarks...")
    benchmark_traversals(db, graph, config.iterations)
    benchmark_edge_exists(db, graph, config.iterations)

    if vector_setup is not None:
      prop_key_id, vector_nodes = vector_setup
      if vector_nodes:
        benchmark_vector_reads(db, vector_nodes, prop_key_id, config.iterations)

    logger.log("\n[6/6] Write benchmarks...")
    if db.read_only:
      logger.log("  Skipped write benchmarks (read-only)")
    else:
      benchmark_writes(db, graph, config.iterations)

  finally:
    if db is not None and db.is_open:
      db.close()
    if config.keep_db:
      logger.log(f"\nDatabase preserved at: {db_path}")
    else:
      try:
        import shutil
        shutil.rmtree(tmp_dir, ignore_errors=True)
      except Exception:
        pass

  logger.log(f"\n{'=' * 120}")
  logger.log("Benchmark complete.")
  logger.log("=" * 120)
  logger.flush()
  if config.output_file:
    print(f"\nResults saved to: {config.output_file}")


if __name__ == "__main__":
  run_benchmarks(parse_args())
