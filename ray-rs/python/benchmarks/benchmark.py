#!/usr/bin/env python3
"""
RayDB Python Bindings Benchmark

Benchmarks for the Python bindings (PyO3 pyo3_bindings).
Tests the SingleFileDB exposed via the Database class.

Usage:
    python benchmark.py [options]

Options:
    --nodes N         Number of nodes (default: 10000)
    --edges M         Number of edges (default: 50000)
    --iterations I    Iterations for latency benchmarks (default: 10000)
    --output FILE     Output file path (default: auto-generated)
    --no-output       Disable file output
    --keep-db         Keep the database file after benchmark
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
from typing import Dict, List, Optional, Set, Tuple

# Add parent directory to path for raydb import
sys.path.insert(0, str(Path(__file__).parent.parent))

try:
    from raydb import Database, PropValue
except ImportError:
    print("Error: raydb module not found. Make sure to build the Python bindings first:")
    print("  cd ray-rs && maturin develop --features python")
    sys.exit(1)


# =============================================================================
# Configuration
# =============================================================================

@dataclass
class BenchConfig:
    nodes: int = 10000
    edges: int = 50000
    iterations: int = 10000
    output_file: Optional[str] = None
    keep_db: bool = False


def parse_args() -> BenchConfig:
    parser = argparse.ArgumentParser(description="RayDB Python Bindings Benchmark")
    parser.add_argument("--nodes", type=int, default=10000, help="Number of nodes")
    parser.add_argument("--edges", type=int, default=50000, help="Number of edges")
    parser.add_argument("--iterations", type=int, default=10000, help="Iterations for latency benchmarks")
    parser.add_argument("--output", type=str, default=None, help="Output file path")
    parser.add_argument("--no-output", action="store_true", help="Disable file output")
    parser.add_argument("--keep-db", action="store_true", help="Keep database after benchmark")
    
    args = parser.parse_args()
    
    # Generate default output filename
    if args.output is None and not args.no_output:
        timestamp = datetime.now().strftime("%Y-%m-%dT%H-%M-%S")
        output_dir = Path(__file__).parent / "results"
        output_dir.mkdir(exist_ok=True)
        output_file = str(output_dir / f"benchmark-python-{timestamp}.txt")
    elif args.no_output:
        output_file = None
    else:
        output_file = args.output
    
    return BenchConfig(
        nodes=args.nodes,
        edges=args.edges,
        iterations=args.iterations,
        output_file=output_file,
        keep_db=args.keep_db,
    )


# =============================================================================
# Logger
# =============================================================================

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


# =============================================================================
# Latency Tracking
# =============================================================================

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
    
    def clear(self):
        self.samples = []


def format_latency(ns: float) -> str:
    if ns < 1000:
        return f"{ns:.0f}ns"
    if ns < 1_000_000:
        return f"{ns / 1000:.2f}us"
    return f"{ns / 1_000_000:.2f}ms"


def format_number(n: int) -> str:
    return f"{n:,}"


def format_bytes(b: int) -> str:
    if b < 1024:
        return f"{b} B"
    if b < 1024 * 1024:
        return f"{b / 1024:.2f} KB"
    return f"{b / (1024 * 1024):.2f} MB"


def print_latency_table(name: str, stats: LatencyStats):
    ops_per_sec = stats.count / (stats.sum_ns / 1_000_000_000) if stats.sum_ns > 0 else 0
    logger.log(
        f"{name:<45} p50={format_latency(stats.p50):>10} "
        f"p95={format_latency(stats.p95):>10} p99={format_latency(stats.p99):>10} "
        f"max={format_latency(stats.max_ns):>10} ({format_number(int(ops_per_sec))} ops/sec)"
    )


# =============================================================================
# Graph Structure
# =============================================================================

@dataclass
class GraphData:
    node_ids: List[int]
    node_keys: List[str]
    hub_nodes: List[int]
    leaf_nodes: List[int]
    out_degree: Dict[int, int]
    in_degree: Dict[int, int]
    etypes: Dict[str, int]


def build_realistic_graph(db: Database, config: BenchConfig) -> GraphData:
    node_ids: List[int] = []
    node_keys: List[str] = []
    out_degree: Dict[int, int] = {}
    in_degree: Dict[int, int] = {}
    
    batch_size = 5000
    
    # Create edge types
    etypes = {
        "calls": db.get_or_create_etype("CALLS"),
        "references": db.get_or_create_etype("REFERENCES"),
        "imports": db.get_or_create_etype("IMPORTS"),
        "extends": db.get_or_create_etype("EXTENDS"),
    }
    
    print("  Creating nodes...")
    for batch in range(0, config.nodes, batch_size):
        db.begin()
        
        end = min(batch + batch_size, config.nodes)
        for i in range(batch, end):
            key = f"pkg.module{i // 100}.Class{i % 100}"
            node_id = db.create_node(key)
            node_ids.append(node_id)
            node_keys.append(key)
            out_degree[node_id] = 0
            in_degree[node_id] = 0
        
        db.commit()
        print(f"\r  Created {end} / {config.nodes} nodes", end="", flush=True)
    print()
    
    # Identify hub nodes
    num_hubs = max(1, int(config.nodes * 0.01))
    hub_indices: Set[int] = set()
    while len(hub_indices) < num_hubs:
        hub_indices.add(random.randint(0, len(node_ids) - 1))
    
    hub_nodes = [node_ids[i] for i in hub_indices]
    leaf_nodes = [nid for i, nid in enumerate(node_ids) if i not in hub_indices]
    
    # Create edges
    edge_types = [etypes["calls"], etypes["references"], etypes["imports"], etypes["extends"]]
    edge_type_weights = [0.4, 0.35, 0.15, 0.1]
    
    print("  Creating edges...")
    edges_created = 0
    attempts = 0
    max_attempts = config.edges * 3
    
    while edges_created < config.edges and attempts < max_attempts:
        db.begin()
        batch_target = min(edges_created + batch_size, config.edges)
        
        while edges_created < batch_target and attempts < max_attempts:
            attempts += 1
            
            # 30% from hubs, 20% to hubs
            if random.random() < 0.3 and hub_nodes:
                src = random.choice(hub_nodes)
            else:
                src = random.choice(node_ids)
            
            if random.random() < 0.2 and hub_nodes:
                dst = random.choice(hub_nodes)
            else:
                dst = random.choice(node_ids)
            
            if src != dst:
                r = random.random()
                cumulative = 0.0
                etype = edge_types[0]
                for j, weight in enumerate(edge_type_weights):
                    cumulative += weight
                    if r < cumulative:
                        etype = edge_types[j]
                        break
                
                db.add_edge(src, etype, dst)
                out_degree[src] = out_degree.get(src, 0) + 1
                in_degree[dst] = in_degree.get(dst, 0) + 1
                edges_created += 1
        
        db.commit()
        
        # Checkpoint periodically to avoid WAL overflow
        if edges_created % 10000 == 0:
            db.checkpoint()
        
        print(f"\r  Created {edges_created} / {config.edges} edges", end="", flush=True)
    print()
    
    return GraphData(
        node_ids=node_ids,
        node_keys=node_keys,
        hub_nodes=hub_nodes,
        leaf_nodes=leaf_nodes,
        out_degree=out_degree,
        in_degree=in_degree,
        etypes=etypes,
    )


# =============================================================================
# Benchmarks
# =============================================================================

def benchmark_key_lookups(db: Database, graph: GraphData, iterations: int):
    logger.log("\n--- Key Lookups (get_node_by_key) ---")
    
    # Uniform random
    tracker = LatencyTracker()
    for _ in range(iterations):
        key = random.choice(graph.node_keys)
        start = time.perf_counter_ns()
        db.get_node_by_key(key)
        tracker.record(time.perf_counter_ns() - start)
    print_latency_table("Uniform random keys", tracker.get_stats())
    
    # Sequential
    tracker = LatencyTracker()
    for i in range(iterations):
        key = graph.node_keys[i % len(graph.node_keys)]
        start = time.perf_counter_ns()
        db.get_node_by_key(key)
        tracker.record(time.perf_counter_ns() - start)
    print_latency_table("Sequential keys", tracker.get_stats())
    
    # Missing keys
    tracker = LatencyTracker()
    for i in range(min(iterations, 1000)):
        key = f"nonexistent.key.{i}"
        start = time.perf_counter_ns()
        db.get_node_by_key(key)
        tracker.record(time.perf_counter_ns() - start)
    print_latency_table("Missing keys", tracker.get_stats())


def benchmark_node_operations(db: Database, graph: GraphData, iterations: int):
    logger.log("\n--- Node Operations ---")
    
    # node_exists
    tracker = LatencyTracker()
    for _ in range(iterations):
        node_id = random.choice(graph.node_ids)
        start = time.perf_counter_ns()
        db.node_exists(node_id)
        tracker.record(time.perf_counter_ns() - start)
    print_latency_table("node_exists() random", tracker.get_stats())
    
    # get_node_key
    tracker = LatencyTracker()
    for _ in range(iterations):
        node_id = random.choice(graph.node_ids)
        start = time.perf_counter_ns()
        db.get_node_key(node_id)
        tracker.record(time.perf_counter_ns() - start)
    print_latency_table("get_node_key() random", tracker.get_stats())
    
    # count_nodes
    tracker = LatencyTracker()
    for _ in range(min(iterations, 1000)):
        start = time.perf_counter_ns()
        db.count_nodes()
        tracker.record(time.perf_counter_ns() - start)
    print_latency_table("count_nodes()", tracker.get_stats())


def benchmark_edge_operations(db: Database, graph: GraphData, iterations: int):
    logger.log("\n--- Edge Operations ---")
    
    # edge_exists
    tracker = LatencyTracker()
    for _ in range(iterations):
        src = random.choice(graph.node_ids)
        dst = random.choice(graph.node_ids)
        start = time.perf_counter_ns()
        db.edge_exists(src, graph.etypes["calls"], dst)
        tracker.record(time.perf_counter_ns() - start)
    print_latency_table("edge_exists() random", tracker.get_stats())
    
    # count_edges
    tracker = LatencyTracker()
    for _ in range(min(iterations, 1000)):
        start = time.perf_counter_ns()
        db.count_edges()
        tracker.record(time.perf_counter_ns() - start)
    print_latency_table("count_edges()", tracker.get_stats())
    
    # get_out_degree
    tracker = LatencyTracker()
    for _ in range(iterations):
        node_id = random.choice(graph.node_ids)
        start = time.perf_counter_ns()
        db.get_out_degree(node_id)
        tracker.record(time.perf_counter_ns() - start)
    print_latency_table("get_out_degree() random", tracker.get_stats())
    
    # get_in_degree
    tracker = LatencyTracker()
    for _ in range(iterations):
        node_id = random.choice(graph.node_ids)
        start = time.perf_counter_ns()
        db.get_in_degree(node_id)
        tracker.record(time.perf_counter_ns() - start)
    print_latency_table("get_in_degree() random", tracker.get_stats())


def benchmark_traversals(db: Database, graph: GraphData, iterations: int):
    logger.log("\n--- 1-Hop Traversals ---")
    
    # Find worst-case nodes
    worst_out_node = graph.node_ids[0]
    worst_out_degree = 0
    for node_id, degree in graph.out_degree.items():
        if degree > worst_out_degree:
            worst_out_degree = degree
            worst_out_node = node_id
    
    worst_in_node = graph.node_ids[0]
    worst_in_degree = 0
    for node_id, degree in graph.in_degree.items():
        if degree > worst_in_degree:
            worst_in_degree = degree
            worst_in_node = node_id
    
    logger.log(f"  Worst-case out-degree: {worst_out_degree}, in-degree: {worst_in_degree}")
    
    # get_out_edges - uniform random
    tracker = LatencyTracker()
    for _ in range(iterations):
        node_id = random.choice(graph.node_ids)
        start = time.perf_counter_ns()
        edges = db.get_out_edges(node_id)
        tracker.record(time.perf_counter_ns() - start)
    print_latency_table("get_out_edges() uniform random", tracker.get_stats())
    
    # get_in_edges - uniform random
    tracker = LatencyTracker()
    for _ in range(iterations):
        node_id = random.choice(graph.node_ids)
        start = time.perf_counter_ns()
        edges = db.get_in_edges(node_id)
        tracker.record(time.perf_counter_ns() - start)
    print_latency_table("get_in_edges() uniform random", tracker.get_stats())
    
    # Hub nodes
    if graph.hub_nodes:
        tracker = LatencyTracker()
        for _ in range(iterations):
            node_id = random.choice(graph.hub_nodes)
            start = time.perf_counter_ns()
            edges = db.get_out_edges(node_id)
            tracker.record(time.perf_counter_ns() - start)
        print_latency_table("get_out_edges() hub nodes", tracker.get_stats())
        
        tracker = LatencyTracker()
        for _ in range(iterations):
            node_id = random.choice(graph.hub_nodes)
            start = time.perf_counter_ns()
            edges = db.get_in_edges(node_id)
            tracker.record(time.perf_counter_ns() - start)
        print_latency_table("get_in_edges() hub nodes", tracker.get_stats())
    
    # Worst-case node
    tracker = LatencyTracker()
    for _ in range(min(iterations, 1000)):
        start = time.perf_counter_ns()
        edges = db.get_out_edges(worst_out_node)
        tracker.record(time.perf_counter_ns() - start)
    print_latency_table(f"get_out_edges() worst-case (deg={worst_out_degree})", tracker.get_stats())


def benchmark_traversal_api(db: Database, graph: GraphData, iterations: int):
    logger.log("\n--- Traversal API ---")
    
    # traverse_out
    tracker = LatencyTracker()
    for _ in range(iterations):
        node_id = random.choice(graph.node_ids)
        start = time.perf_counter_ns()
        neighbors = db.traverse_out(node_id, graph.etypes["calls"])
        tracker.record(time.perf_counter_ns() - start)
    print_latency_table("traverse_out() with etype filter", tracker.get_stats())
    
    # traverse_in
    tracker = LatencyTracker()
    for _ in range(iterations):
        node_id = random.choice(graph.node_ids)
        start = time.perf_counter_ns()
        neighbors = db.traverse_in(node_id, graph.etypes["calls"])
        tracker.record(time.perf_counter_ns() - start)
    print_latency_table("traverse_in() with etype filter", tracker.get_stats())
    
    # Variable depth traverse
    tracker = LatencyTracker()
    iters = min(iterations, 1000)
    for _ in range(iters):
        node_id = random.choice(graph.node_ids)
        start = time.perf_counter_ns()
        results = db.traverse(node_id, max_depth=2, etype=graph.etypes["calls"])
        tracker.record(time.perf_counter_ns() - start)
    print_latency_table("traverse() max_depth=2", tracker.get_stats())


def benchmark_pathfinding(db: Database, graph: GraphData, iterations: int):
    logger.log("\n--- Pathfinding ---")
    
    iters = min(iterations, 500)
    
    # BFS
    tracker = LatencyTracker()
    found_count = 0
    for _ in range(iters):
        src = random.choice(graph.node_ids)
        dst = random.choice(graph.node_ids)
        start = time.perf_counter_ns()
        result = db.find_path_bfs(src, dst, etype=graph.etypes["calls"], max_depth=10)
        tracker.record(time.perf_counter_ns() - start)
        if result.found:
            found_count += 1
    print_latency_table(f"find_path_bfs() (found {found_count}/{iters})", tracker.get_stats())
    
    # Dijkstra
    tracker = LatencyTracker()
    found_count = 0
    for _ in range(iters):
        src = random.choice(graph.node_ids)
        dst = random.choice(graph.node_ids)
        start = time.perf_counter_ns()
        result = db.find_path_dijkstra(src, dst, etype=graph.etypes["calls"], max_depth=10)
        tracker.record(time.perf_counter_ns() - start)
        if result.found:
            found_count += 1
    print_latency_table(f"find_path_dijkstra() (found {found_count}/{iters})", tracker.get_stats())
    
    # has_path
    tracker = LatencyTracker()
    for _ in range(iters):
        src = random.choice(graph.node_ids)
        dst = random.choice(graph.node_ids)
        start = time.perf_counter_ns()
        db.has_path(src, dst, etype=graph.etypes["calls"], max_depth=10)
        tracker.record(time.perf_counter_ns() - start)
    print_latency_table("has_path()", tracker.get_stats())
    
    # reachable_nodes
    tracker = LatencyTracker()
    for _ in range(min(iters, 200)):
        node_id = random.choice(graph.node_ids)
        start = time.perf_counter_ns()
        reachable = db.reachable_nodes(node_id, max_depth=3, etype=graph.etypes["calls"])
        tracker.record(time.perf_counter_ns() - start)
    print_latency_table("reachable_nodes() max_depth=3", tracker.get_stats())


def benchmark_properties(db: Database, graph: GraphData, iterations: int):
    logger.log("\n--- Property Operations ---")
    
    # Setup: Add some properties
    prop_key = db.get_or_create_propkey("name")
    num_nodes_with_props = min(1000, len(graph.node_ids))
    
    db.begin()
    for i in range(num_nodes_with_props):
        node_id = graph.node_ids[i]
        db.set_node_prop(node_id, prop_key, PropValue.string(f"Node_{node_id}"))
    db.commit()
    
    # get_node_prop
    tracker = LatencyTracker()
    for _ in range(iterations):
        node_id = graph.node_ids[random.randint(0, num_nodes_with_props - 1)]
        start = time.perf_counter_ns()
        db.get_node_prop(node_id, prop_key)
        tracker.record(time.perf_counter_ns() - start)
    print_latency_table("get_node_prop() random", tracker.get_stats())
    
    # get_node_props
    tracker = LatencyTracker()
    for _ in range(iterations):
        node_id = graph.node_ids[random.randint(0, num_nodes_with_props - 1)]
        start = time.perf_counter_ns()
        db.get_node_props(node_id)
        tracker.record(time.perf_counter_ns() - start)
    print_latency_table("get_node_props() random", tracker.get_stats())


def benchmark_vector_operations(db: Database, graph: GraphData, iterations: int):
    logger.log("\n--- Vector Operations ---")
    
    vector_prop_key = db.get_or_create_propkey("embedding")
    dimensions = 128
    num_nodes_with_vectors = min(1000, len(graph.node_ids))
    
    # Generate random vectors
    vectors = [[random.random() for _ in range(dimensions)] for _ in range(num_nodes_with_vectors)]
    
    # Set vectors
    tracker = LatencyTracker()
    db.begin()
    for i in range(num_nodes_with_vectors):
        node_id = graph.node_ids[i]
        start = time.perf_counter_ns()
        db.set_node_vector(node_id, vector_prop_key, vectors[i])
        tracker.record(time.perf_counter_ns() - start)
    db.commit()
    print_latency_table(f"set_node_vector() ({dimensions}D)", tracker.get_stats())
    
    # Get vectors
    tracker = LatencyTracker()
    for _ in range(iterations):
        node_id = graph.node_ids[random.randint(0, num_nodes_with_vectors - 1)]
        start = time.perf_counter_ns()
        db.get_node_vector(node_id, vector_prop_key)
        tracker.record(time.perf_counter_ns() - start)
    print_latency_table(f"get_node_vector() ({dimensions}D)", tracker.get_stats())
    
    # has_node_vector
    tracker = LatencyTracker()
    for _ in range(iterations):
        node_id = random.choice(graph.node_ids)
        start = time.perf_counter_ns()
        db.has_node_vector(node_id, vector_prop_key)
        tracker.record(time.perf_counter_ns() - start)
    print_latency_table("has_node_vector()", tracker.get_stats())


def benchmark_writes(db: Database, graph: GraphData, iterations: int):
    logger.log("\n--- Write Performance ---")
    
    # Checkpoint before writes to ensure we have space
    db.checkpoint()
    
    batch_sizes = [10, 100, 1000]
    
    for batch_size in batch_sizes:
        tracker = LatencyTracker()
        batches = min(iterations // batch_size, 20)  # Reduced to avoid WAL overflow
        for b in range(batches):
            start = time.perf_counter_ns()
            db.begin()
            for i in range(batch_size):
                db.create_node(f"bench:batch{batch_size}:{b}:{i}")
            db.commit()
            tracker.record(time.perf_counter_ns() - start)
            
            # Checkpoint periodically
            if b > 0 and b % 5 == 0:
                db.checkpoint()
        
        stats = tracker.get_stats()
        ops_per_sec = (batch_size * stats.count) / (stats.sum_ns / 1_000_000_000) if stats.sum_ns > 0 else 0
        logger.log(
            f"{'Batch of ' + str(batch_size).rjust(4) + ' nodes':<45} "
            f"p50={format_latency(stats.p50):>10} p95={format_latency(stats.p95):>10} "
            f"({format_number(int(ops_per_sec))} nodes/sec)"
        )
        db.checkpoint()
    
    logger.log("\n--- Edge Creation ---")
    for batch_size in batch_sizes:
        tracker = LatencyTracker()
        batches = min(iterations // batch_size, 20)  # Reduced to avoid WAL overflow
        for b in range(batches):
            start = time.perf_counter_ns()
            db.begin()
            for i in range(batch_size):
                src = random.choice(graph.node_ids)
                dst = random.choice(graph.node_ids)
                if src != dst:
                    db.add_edge(src, graph.etypes["calls"], dst)
            db.commit()
            tracker.record(time.perf_counter_ns() - start)
            
            # Checkpoint periodically
            if b > 0 and b % 5 == 0:
                db.checkpoint()
        
        stats = tracker.get_stats()
        ops_per_sec = (batch_size * stats.count) / (stats.sum_ns / 1_000_000_000) if stats.sum_ns > 0 else 0
        logger.log(
            f"{'Batch of ' + str(batch_size).rjust(4) + ' edges':<45} "
            f"p50={format_latency(stats.p50):>10} p95={format_latency(stats.p95):>10} "
            f"({format_number(int(ops_per_sec))} edges/sec)"
        )
        db.checkpoint()


def benchmark_list_operations(db: Database, graph: GraphData, iterations: int):
    logger.log("\n--- List Operations ---")
    
    # list_nodes
    tracker = LatencyTracker()
    list_iters = min(iterations, 100)
    for _ in range(list_iters):
        start = time.perf_counter_ns()
        nodes = db.list_nodes()
        tracker.record(time.perf_counter_ns() - start)
    stats = tracker.get_stats()
    nodes_per_sec = (len(graph.node_ids) * stats.count) / (stats.sum_ns / 1_000_000_000) if stats.sum_ns > 0 else 0
    logger.log(
        f"{'list_nodes() full iteration':<45} p50={format_latency(stats.p50):>10} "
        f"p95={format_latency(stats.p95):>10} ({format_number(int(nodes_per_sec))} nodes/sec)"
    )
    
    # list_edges
    tracker = LatencyTracker()
    list_iters = min(iterations, 50)
    for _ in range(list_iters):
        start = time.perf_counter_ns()
        edges = db.list_edges()
        tracker.record(time.perf_counter_ns() - start)
    stats = tracker.get_stats()
    logger.log(
        f"{'list_edges() full iteration':<45} p50={format_latency(stats.p50):>10} "
        f"p95={format_latency(stats.p95):>10}"
    )


def benchmark_schema_operations(db: Database, iterations: int):
    logger.log("\n--- Schema Operations ---")
    
    # get_or_create_etype
    tracker = LatencyTracker()
    for i in range(iterations):
        name = f"EDGE_TYPE_{i % 10}"
        start = time.perf_counter_ns()
        db.get_or_create_etype(name)
        tracker.record(time.perf_counter_ns() - start)
    print_latency_table("get_or_create_etype() (90% hits)", tracker.get_stats())
    
    # get_or_create_propkey
    tracker = LatencyTracker()
    for i in range(iterations):
        name = f"prop_key_{i % 10}"
        start = time.perf_counter_ns()
        db.get_or_create_propkey(name)
        tracker.record(time.perf_counter_ns() - start)
    print_latency_table("get_or_create_propkey() (90% hits)", tracker.get_stats())
    
    # get_or_create_label
    tracker = LatencyTracker()
    for i in range(iterations):
        name = f"Label_{i % 10}"
        start = time.perf_counter_ns()
        db.get_or_create_label(name)
        tracker.record(time.perf_counter_ns() - start)
    print_latency_table("get_or_create_label() (90% hits)", tracker.get_stats())


def benchmark_transaction_overhead(db: Database, iterations: int):
    logger.log("\n--- Transaction Overhead ---")
    
    # begin + commit (empty)
    tracker = LatencyTracker()
    for _ in range(min(iterations, 1000)):
        start = time.perf_counter_ns()
        db.begin()
        db.commit()
        tracker.record(time.perf_counter_ns() - start)
    print_latency_table("Empty transaction (begin+commit)", tracker.get_stats())
    
    # begin + rollback
    tracker = LatencyTracker()
    for _ in range(min(iterations, 1000)):
        start = time.perf_counter_ns()
        db.begin()
        db.rollback()
        tracker.record(time.perf_counter_ns() - start)
    print_latency_table("Empty transaction (begin+rollback)", tracker.get_stats())


# =============================================================================
# Main
# =============================================================================

def run_benchmarks(config: BenchConfig):
    global logger
    logger = Logger(config.output_file)
    
    now = datetime.now()
    logger.log("=" * 120)
    logger.log("RayDB Python Bindings Benchmark")
    logger.log("=" * 120)
    logger.log(f"Date: {now.isoformat()}")
    logger.log(f"Nodes: {format_number(config.nodes)}")
    logger.log(f"Edges: {format_number(config.edges)}")
    logger.log(f"Iterations: {format_number(config.iterations)}")
    logger.log(f"Keep database: {config.keep_db}")
    logger.log("=" * 120)
    
    # Create temporary database file
    tmp_dir = tempfile.mkdtemp(prefix="ray-python-bench-")
    db_path = os.path.join(tmp_dir, "benchmark.raydb")
    
    try:
        logger.log("\n[1/12] Opening database...")
        db = Database(db_path)
        logger.log(f"  Database opened at: {db_path}")
        
        logger.log("\n[2/12] Building graph...")
        start_build = time.perf_counter()
        graph = build_realistic_graph(db, config)
        logger.log(f"  Built in {(time.perf_counter() - start_build) * 1000:.0f}ms")
        
        # Degree stats
        degrees = sorted(graph.out_degree.values(), reverse=True)
        avg_degree = sum(degrees) / len(degrees) if degrees else 0
        logger.log(f"  Avg out-degree: {avg_degree:.1f}, Top 5: {', '.join(map(str, degrees[:5]))}")
        
        logger.log("\n[3/12] Compacting...")
        start_compact = time.perf_counter()
        db.optimize()
        logger.log(f"  Compacted in {(time.perf_counter() - start_compact) * 1000:.0f}ms")
        
        stats = db.stats()
        logger.log(f"  Snapshot: {format_number(stats.snapshot_nodes)} nodes, {format_number(stats.snapshot_edges)} edges")
        
        logger.log("\n[4/12] Key lookup benchmarks...")
        benchmark_key_lookups(db, graph, config.iterations)
        
        logger.log("\n[5/12] Node operation benchmarks...")
        benchmark_node_operations(db, graph, config.iterations)
        
        logger.log("\n[6/12] Edge operation benchmarks...")
        benchmark_edge_operations(db, graph, config.iterations)
        
        logger.log("\n[7/12] Traversal benchmarks...")
        benchmark_traversals(db, graph, config.iterations)
        
        logger.log("\n[8/12] Traversal API benchmarks...")
        benchmark_traversal_api(db, graph, config.iterations)
        
        logger.log("\n[9/12] Pathfinding benchmarks...")
        benchmark_pathfinding(db, graph, config.iterations)
        
        logger.log("\n[10/12] Property benchmarks...")
        benchmark_properties(db, graph, config.iterations)
        
        logger.log("\n[11/12] Vector operation benchmarks...")
        benchmark_vector_operations(db, graph, config.iterations)
        
        logger.log("\n[12/12] Schema and transaction benchmarks...")
        benchmark_schema_operations(db, config.iterations)
        benchmark_transaction_overhead(db, config.iterations)
        
        logger.log("\n[Bonus] List operation benchmarks...")
        benchmark_list_operations(db, graph, config.iterations)
        
        logger.log("\n[Bonus] Write benchmarks...")
        benchmark_writes(db, graph, config.iterations)
        
        # Final compaction
        db.optimize()
        
        # Database size
        try:
            file_size = os.path.getsize(db_path)
            logger.log("\n--- Database Size ---")
            logger.log(f"  File size: {format_bytes(file_size)}")
            logger.log(f"  Bytes per node: {file_size / config.nodes:.1f}")
            logger.log(f"  Bytes per edge: {file_size / config.edges:.1f}")
        except:
            pass
        
        db.close()
        
    finally:
        if config.keep_db:
            logger.log(f"\nDatabase preserved at: {db_path}")
        else:
            import shutil
            shutil.rmtree(tmp_dir, ignore_errors=True)
    
    logger.log(f"\n{'=' * 120}")
    logger.log("Benchmark complete.")
    logger.log("=" * 120)
    
    logger.flush()
    if config.output_file:
        print(f"\nResults saved to: {config.output_file}")


if __name__ == "__main__":
    config = parse_args()
    run_benchmarks(config)
