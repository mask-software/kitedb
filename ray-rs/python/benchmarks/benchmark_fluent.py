#!/usr/bin/env python3
"""
RayDB Python Fluent API Benchmark

Compares the new fluent API against the low-level API for common operations.
This helps measure the overhead of the Python wrapper layer.

Usage:
    python benchmark_fluent.py [options]

Options:
    --nodes N         Number of nodes (default: 1000)
    --edges M         Number of edges (default: 5000)
    --iterations I    Iterations for latency benchmarks (default: 1000)
    --no-output       Disable file output
"""

import argparse
import os
import random
import shutil
import sys
import tempfile
import time
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import Dict, List, Optional

# Add parent directory to path for raydb import
sys.path.insert(0, str(Path(__file__).parent.parent))

try:
    # Low-level API
    from raydb import Database, PropValue
    # Fluent API
    from raydb import ray, define_node, define_edge, prop, optional
except ImportError as e:
    print(f"Error: raydb module not found ({e}). Make sure to build the Python bindings first:")
    print("  cd ray-rs && maturin develop --features python")
    sys.exit(1)


# =============================================================================
# Schema Definition (for fluent API)
# =============================================================================

user = define_node("user",
    key=lambda id: f"user:{id}",
    props={
        "name": prop.string("name"),
        "email": prop.string("email"),
        "age": optional(prop.int("age")),
    }
)

knows = define_edge("knows", {
    "since": prop.int("since"),
})


# =============================================================================
# Configuration
# =============================================================================

@dataclass
class BenchConfig:
    nodes: int = 1000
    edges: int = 5000
    iterations: int = 1000
    no_output: bool = False


def parse_args() -> BenchConfig:
    parser = argparse.ArgumentParser(description="RayDB Python Fluent API Benchmark")
    parser.add_argument("--nodes", type=int, default=1000, help="Number of nodes")
    parser.add_argument("--edges", type=int, default=5000, help="Number of edges")
    parser.add_argument("--iterations", type=int, default=1000, help="Iterations for latency benchmarks")
    parser.add_argument("--no-output", action="store_true", help="Disable file output")
    
    args = parser.parse_args()
    return BenchConfig(
        nodes=args.nodes,
        edges=args.edges,
        iterations=args.iterations,
        no_output=args.no_output,
    )


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
    
    @property
    def avg_ns(self) -> float:
        return self.sum_ns / self.count if self.count > 0 else 0
    
    @property
    def ops_per_sec(self) -> float:
        return self.count / (self.sum_ns / 1_000_000_000) if self.sum_ns > 0 else 0


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


def print_comparison(name: str, low_level: LatencyStats, fluent: LatencyStats):
    overhead = fluent.p50 / low_level.p50 if low_level.p50 > 0 else 0
    print(
        f"{name:<35} "
        f"low-level p50={format_latency(low_level.p50):>10}  "
        f"fluent p50={format_latency(fluent.p50):>10}  "
        f"overhead={overhead:.2f}x"
    )


# =============================================================================
# Benchmark Functions
# =============================================================================

def benchmark_low_level_inserts(db: Database, config: BenchConfig) -> LatencyStats:
    """Benchmark low-level insert operations."""
    tracker = LatencyTracker()
    name_key = db.get_or_create_propkey("name")
    email_key = db.get_or_create_propkey("email")
    age_key = db.get_or_create_propkey("age")
    
    for i in range(config.iterations):
        start = time.perf_counter_ns()
        db.begin()
        node_id = db.create_node(f"bench:low:{i}")
        db.set_node_prop(node_id, name_key, PropValue.string(f"User {i}"))
        db.set_node_prop(node_id, email_key, PropValue.string(f"user{i}@example.com"))
        db.set_node_prop(node_id, age_key, PropValue.int(20 + (i % 50)))
        db.commit()
        tracker.record(time.perf_counter_ns() - start)
    
    return tracker.get_stats()


def benchmark_fluent_inserts(db, config: BenchConfig) -> LatencyStats:
    """Benchmark fluent API insert operations."""
    tracker = LatencyTracker()
    
    for i in range(config.iterations):
        start = time.perf_counter_ns()
        db.insert(user).values(
            key=f"bench:fluent:{i}",
            name=f"User {i}",
            email=f"user{i}@example.com",
            age=20 + (i % 50),
        ).returning()
        tracker.record(time.perf_counter_ns() - start)
    
    return tracker.get_stats()


def benchmark_low_level_key_lookup(db: Database, keys: List[str], iterations: int) -> LatencyStats:
    """Benchmark low-level key lookups."""
    tracker = LatencyTracker()
    
    for _ in range(iterations):
        key = random.choice(keys)
        start = time.perf_counter_ns()
        db.get_node_by_key(key)
        tracker.record(time.perf_counter_ns() - start)
    
    return tracker.get_stats()


def benchmark_fluent_key_lookup(db, key_args: List[str], iterations: int) -> LatencyStats:
    """Benchmark fluent API key lookups (with property loading)."""
    tracker = LatencyTracker()
    
    for _ in range(iterations):
        key_arg = random.choice(key_args)
        start = time.perf_counter_ns()
        db.get(user, key_arg)
        tracker.record(time.perf_counter_ns() - start)
    
    return tracker.get_stats()


def benchmark_fluent_get_ref(db, key_args: List[str], iterations: int) -> LatencyStats:
    """Benchmark fluent API getRef (without property loading)."""
    tracker = LatencyTracker()
    
    for _ in range(iterations):
        key_arg = random.choice(key_args)
        start = time.perf_counter_ns()
        db.get_ref(user, key_arg)
        tracker.record(time.perf_counter_ns() - start)
    
    return tracker.get_stats()


def benchmark_low_level_traversal(db: Database, node_ids: List[int], etype_id: int, iterations: int) -> LatencyStats:
    """Benchmark low-level traversal."""
    tracker = LatencyTracker()
    
    for _ in range(iterations):
        node_id = random.choice(node_ids)
        start = time.perf_counter_ns()
        neighbors = db.traverse_out(node_id, etype_id)
        tracker.record(time.perf_counter_ns() - start)
    
    return tracker.get_stats()


def benchmark_fluent_traversal(db, user_refs: List, iterations: int) -> LatencyStats:
    """Benchmark fluent API traversal (count - no props loaded)."""
    tracker = LatencyTracker()
    
    for _ in range(iterations):
        user_ref = random.choice(user_refs)
        start = time.perf_counter_ns()
        db.from_(user_ref).out(knows).count()
        tracker.record(time.perf_counter_ns() - start)
    
    return tracker.get_stats()


def benchmark_fluent_traversal_ids(db, user_refs: List, iterations: int) -> LatencyStats:
    """Benchmark fluent API traversal - ids() (fastest)."""
    tracker = LatencyTracker()
    
    for _ in range(iterations):
        user_ref = random.choice(user_refs)
        start = time.perf_counter_ns()
        db.from_(user_ref).out(knows).ids()
        tracker.record(time.perf_counter_ns() - start)
    
    return tracker.get_stats()


def benchmark_fluent_traversal_to_list(db, user_refs: List, iterations: int) -> LatencyStats:
    """Benchmark fluent API traversal - to_list() (no props by default)."""
    tracker = LatencyTracker()
    
    for _ in range(iterations):
        user_ref = random.choice(user_refs)
        start = time.perf_counter_ns()
        db.from_(user_ref).out(knows).to_list()
        tracker.record(time.perf_counter_ns() - start)
    
    return tracker.get_stats()


def benchmark_fluent_traversal_with_props(db, user_refs: List, iterations: int) -> LatencyStats:
    """Benchmark fluent API traversal - with_props().to_list() (full loading)."""
    tracker = LatencyTracker()
    
    for _ in range(iterations):
        user_ref = random.choice(user_refs)
        start = time.perf_counter_ns()
        db.from_(user_ref).out(knows).with_props().to_list()
        tracker.record(time.perf_counter_ns() - start)
    
    return tracker.get_stats()


def benchmark_fluent_traversal_load_props(db, user_refs: List, iterations: int) -> LatencyStats:
    """Benchmark fluent API traversal - load_props("name").to_list() (selective)."""
    tracker = LatencyTracker()
    
    for _ in range(iterations):
        user_ref = random.choice(user_refs)
        start = time.perf_counter_ns()
        db.from_(user_ref).out(knows).load_props("name").to_list()
        tracker.record(time.perf_counter_ns() - start)
    
    return tracker.get_stats()


# =============================================================================
# Main
# =============================================================================

def run_benchmarks(config: BenchConfig):
    now = datetime.now()
    print("=" * 100)
    print("RayDB Python Fluent API vs Low-Level Benchmark")
    print("=" * 100)
    print(f"Date: {now.isoformat()}")
    print(f"Nodes: {format_number(config.nodes)}")
    print(f"Edges: {format_number(config.edges)}")
    print(f"Iterations: {format_number(config.iterations)}")
    print("=" * 100)
    
    # Create temporary directories for both databases
    low_level_dir = tempfile.mkdtemp(prefix="ray-low-level-")
    fluent_dir = tempfile.mkdtemp(prefix="ray-fluent-")
    
    try:
        # =================================================================
        # Setup Phase
        # =================================================================
        print("\n[1/6] Setting up databases...")
        
        # Low-level database setup
        low_level_db = Database(os.path.join(low_level_dir, "test.raydb"))
        knows_etype = low_level_db.get_or_create_etype("knows")
        name_key = low_level_db.get_or_create_propkey("name")
        email_key = low_level_db.get_or_create_propkey("email")
        age_key = low_level_db.get_or_create_propkey("age")
        
        # Fluent database setup
        fluent_db = ray(os.path.join(fluent_dir, "test.raydb"), 
                       nodes=[user], edges=[knows])
        
        # =================================================================
        # Build Test Data
        # =================================================================
        print("\n[2/6] Building test data...")
        
        # Low-level: create nodes
        low_level_node_ids: List[int] = []
        low_level_keys: List[str] = []
        
        low_level_db.begin()
        for i in range(config.nodes):
            key = f"user:{i}"
            node_id = low_level_db.create_node(key)
            low_level_db.set_node_prop(node_id, name_key, PropValue.string(f"User {i}"))
            low_level_db.set_node_prop(node_id, email_key, PropValue.string(f"user{i}@example.com"))
            low_level_db.set_node_prop(node_id, age_key, PropValue.int(20 + (i % 50)))
            low_level_node_ids.append(node_id)
            low_level_keys.append(key)
        low_level_db.commit()
        
        # Low-level: create edges
        low_level_db.begin()
        for _ in range(config.edges):
            src = random.choice(low_level_node_ids)
            dst = random.choice(low_level_node_ids)
            if src != dst:
                low_level_db.add_edge(src, knows_etype, dst)
        low_level_db.commit()
        
        print(f"  Low-level: {len(low_level_node_ids)} nodes, {config.edges} edges")
        
        # Fluent: create nodes
        fluent_user_refs = []
        fluent_key_args: List[str] = []
        
        for i in range(config.nodes):
            key_arg = str(i)
            user_ref = fluent_db.insert(user).values(
                key=key_arg,
                name=f"User {i}",
                email=f"user{i}@example.com",
                age=20 + (i % 50),
            ).returning()
            fluent_user_refs.append(user_ref)
            fluent_key_args.append(key_arg)
        
        # Fluent: create edges
        for _ in range(config.edges):
            src = random.choice(fluent_user_refs)
            dst = random.choice(fluent_user_refs)
            if src.id != dst.id:
                fluent_db.link(src, knows, dst, since=2020)
        
        print(f"  Fluent: {len(fluent_user_refs)} nodes, {config.edges} edges")
        
        # Optimize both databases
        low_level_db.optimize()
        fluent_db.optimize()
        
        # =================================================================
        # Benchmark: Insert Operations
        # =================================================================
        print("\n[3/6] Benchmarking insert operations...")
        
        low_level_insert_stats = benchmark_low_level_inserts(low_level_db, config)
        fluent_insert_stats = benchmark_fluent_inserts(fluent_db, config)
        
        # =================================================================
        # Benchmark: Key Lookups
        # =================================================================
        print("\n[4/6] Benchmarking key lookups...")
        
        low_level_lookup_stats = benchmark_low_level_key_lookup(
            low_level_db, low_level_keys, config.iterations
        )
        fluent_get_stats = benchmark_fluent_key_lookup(
            fluent_db, fluent_key_args, config.iterations
        )
        fluent_get_ref_stats = benchmark_fluent_get_ref(
            fluent_db, fluent_key_args, config.iterations
        )
        
        # =================================================================
        # Benchmark: Traversals
        # =================================================================
        print("\n[5/6] Benchmarking traversals...")
        
        low_level_traversal_stats = benchmark_low_level_traversal(
            low_level_db, low_level_node_ids, knows_etype, config.iterations
        )
        fluent_traversal_stats = benchmark_fluent_traversal(
            fluent_db, fluent_user_refs, config.iterations
        )
        fluent_traversal_ids_stats = benchmark_fluent_traversal_ids(
            fluent_db, fluent_user_refs, config.iterations
        )
        fluent_traversal_to_list_stats = benchmark_fluent_traversal_to_list(
            fluent_db, fluent_user_refs, config.iterations
        )
        fluent_traversal_with_props_stats = benchmark_fluent_traversal_with_props(
            fluent_db, fluent_user_refs, config.iterations
        )
        fluent_traversal_load_props_stats = benchmark_fluent_traversal_load_props(
            fluent_db, fluent_user_refs, config.iterations
        )
        
        # =================================================================
        # Results
        # =================================================================
        print("\n[6/6] Results")
        print("=" * 100)
        print("\n=== Comparison (lower latency is better, overhead closer to 1.0x is better) ===\n")
        
        print_comparison("Insert (single node + props)", low_level_insert_stats, fluent_insert_stats)
        print_comparison("Key lookup (raw)", low_level_lookup_stats, fluent_get_stats)
        print_comparison("Key lookup (getRef, no props)", low_level_lookup_stats, fluent_get_ref_stats)
        print_comparison("1-hop traversal (count)", low_level_traversal_stats, fluent_traversal_stats)
        print_comparison("1-hop traversal (ids)", low_level_traversal_stats, fluent_traversal_ids_stats)
        print_comparison("1-hop traversal (to_list)", low_level_traversal_stats, fluent_traversal_to_list_stats)
        print_comparison("1-hop traversal (with_props)", low_level_traversal_stats, fluent_traversal_with_props_stats)
        print_comparison("1-hop traversal (load_props)", low_level_traversal_stats, fluent_traversal_load_props_stats)
        
        print("\n--- Detailed Statistics ---\n")
        
        print("Insert Operations:")
        print(f"  Low-level:  p50={format_latency(low_level_insert_stats.p50):>10}  p95={format_latency(low_level_insert_stats.p95):>10}  ({format_number(int(low_level_insert_stats.ops_per_sec))} ops/sec)")
        print(f"  Fluent:     p50={format_latency(fluent_insert_stats.p50):>10}  p95={format_latency(fluent_insert_stats.p95):>10}  ({format_number(int(fluent_insert_stats.ops_per_sec))} ops/sec)")
        
        print("\nKey Lookups:")
        print(f"  Low-level:  p50={format_latency(low_level_lookup_stats.p50):>10}  p95={format_latency(low_level_lookup_stats.p95):>10}  ({format_number(int(low_level_lookup_stats.ops_per_sec))} ops/sec)")
        print(f"  Fluent get: p50={format_latency(fluent_get_stats.p50):>10}  p95={format_latency(fluent_get_stats.p95):>10}  ({format_number(int(fluent_get_stats.ops_per_sec))} ops/sec)")
        print(f"  Fluent ref: p50={format_latency(fluent_get_ref_stats.p50):>10}  p95={format_latency(fluent_get_ref_stats.p95):>10}  ({format_number(int(fluent_get_ref_stats.ops_per_sec))} ops/sec)")
        
        print("\nTraversals (1-hop):")
        print(f"  Low-level:      p50={format_latency(low_level_traversal_stats.p50):>10}  p95={format_latency(low_level_traversal_stats.p95):>10}  ({format_number(int(low_level_traversal_stats.ops_per_sec))} ops/sec)")
        print(f"  Fluent count:   p50={format_latency(fluent_traversal_stats.p50):>10}  p95={format_latency(fluent_traversal_stats.p95):>10}  ({format_number(int(fluent_traversal_stats.ops_per_sec))} ops/sec)")
        print(f"  Fluent ids:     p50={format_latency(fluent_traversal_ids_stats.p50):>10}  p95={format_latency(fluent_traversal_ids_stats.p95):>10}  ({format_number(int(fluent_traversal_ids_stats.ops_per_sec))} ops/sec)")
        print(f"  Fluent to_list: p50={format_latency(fluent_traversal_to_list_stats.p50):>10}  p95={format_latency(fluent_traversal_to_list_stats.p95):>10}  ({format_number(int(fluent_traversal_to_list_stats.ops_per_sec))} ops/sec)")
        print(f"  Fluent w/props: p50={format_latency(fluent_traversal_with_props_stats.p50):>10}  p95={format_latency(fluent_traversal_with_props_stats.p95):>10}  ({format_number(int(fluent_traversal_with_props_stats.ops_per_sec))} ops/sec)")
        print(f"  Fluent load_p:  p50={format_latency(fluent_traversal_load_props_stats.p50):>10}  p95={format_latency(fluent_traversal_load_props_stats.p95):>10}  ({format_number(int(fluent_traversal_load_props_stats.ops_per_sec))} ops/sec)")
        
        # Cleanup
        low_level_db.close()
        fluent_db.close()
        
    finally:
        shutil.rmtree(low_level_dir, ignore_errors=True)
        shutil.rmtree(fluent_dir, ignore_errors=True)
    
    print("\n" + "=" * 100)
    print("Benchmark complete.")
    print("=" * 100)


if __name__ == "__main__":
    config = parse_args()
    run_benchmarks(config)
