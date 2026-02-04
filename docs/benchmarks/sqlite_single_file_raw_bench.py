#!/usr/bin/env python3
import argparse
import os
import random
import sqlite3
import tempfile
import time
from typing import List, Tuple


def percentile(samples: List[int], pct: float) -> int:
    if not samples:
        return 0
    idx = int(round((len(samples) - 1) * pct))
    idx = max(0, min(len(samples) - 1, idx))
    return samples[idx]


def format_latency(ns: int) -> str:
    if ns >= 1_000_000:
        return f"{ns/1_000_000:.2f}ms"
    if ns >= 1_000:
        return f"{ns/1_000:.2f}us"
    return f"{ns}ns"


def print_latency_table(label: str, samples_ns: List[int]) -> None:
    samples_ns.sort()
    p50 = percentile(samples_ns, 0.50)
    p95 = percentile(samples_ns, 0.95)
    p99 = percentile(samples_ns, 0.99)
    maxv = samples_ns[-1]
    ops_sec = 0
    total = sum(samples_ns)
    if total > 0:
        ops_sec = int((len(samples_ns) * 1e9) / total)
    print(f"{label:<40} p50={format_latency(p50):>9} p95={format_latency(p95):>9} p99={format_latency(p99):>9} max={format_latency(maxv):>9} ({ops_sec} ops/sec)")


def apply_pragmas(conn: sqlite3.Connection, sync_mode: str, wal_size: int) -> None:
    cur = conn.cursor()
    cur.execute("PRAGMA journal_mode=WAL;")
    cur.execute(f"PRAGMA synchronous={sync_mode.upper()};")
    cur.execute("PRAGMA temp_store=MEMORY;")
    cur.execute("PRAGMA locking_mode=EXCLUSIVE;")
    # cache_size in pages; negative = KB
    cur.execute("PRAGMA cache_size=-262144;")
    # WAL autocheckpoint size in pages (~page_size)
    cur.execute("PRAGMA wal_autocheckpoint=0;")
    # best-effort WAL limit
    cur.execute(f"PRAGMA journal_size_limit={wal_size};")
    cur.close()


def build_schema(conn: sqlite3.Connection) -> None:
    cur = conn.cursor()
    cur.execute("CREATE TABLE nodes (id INTEGER PRIMARY KEY, key TEXT UNIQUE);")
    cur.execute(
        "CREATE TABLE edges (src INTEGER NOT NULL, etype INTEGER NOT NULL, dst INTEGER NOT NULL, PRIMARY KEY (src, etype, dst));"
    )
    cur.execute(
        "CREATE TABLE edge_props (src INTEGER NOT NULL, etype INTEGER NOT NULL, dst INTEGER NOT NULL, key_id INTEGER NOT NULL, value INTEGER, PRIMARY KEY (src, etype, dst, key_id));"
    )
    cur.close()


def build_graph(
    conn: sqlite3.Connection,
    nodes: int,
    edges: int,
    edge_types: int,
    edge_props: int,
    batch_size: int,
) -> Tuple[List[int], List[str], List[int]]:
    node_ids: List[int] = []
    node_keys: List[str] = []
    edge_type_ids = list(range(1, edge_types + 1))

    cur = conn.cursor()

    print("  Creating nodes...")
    for batch_start in range(0, nodes, batch_size):
        end = min(batch_start + batch_size, nodes)
        conn.execute("BEGIN;")
        batch = []
        for i in range(batch_start, end):
            key = f"pkg.module{i // 100}.Class{i % 100}"
            node_id = i + 1
            batch.append((node_id, key))
            node_ids.append(node_id)
            node_keys.append(key)
        cur.executemany("INSERT INTO nodes (id, key) VALUES (?, ?);", batch)
        conn.execute("COMMIT;")
        print(f"\r  Created {end} / {nodes} nodes", end="")
    print()

    print("  Creating edges...")
    rng = random.Random(0xC0FFEE)
    edges_created = 0
    attempts = 0
    max_attempts = edges * 3
    while edges_created < edges and attempts < max_attempts:
        batch_target = min(edges_created + batch_size, edges)
        edge_rows: List[Tuple[int, int, int]] = []
        prop_rows: List[Tuple[int, int, int, int, int]] = []
        while edges_created < batch_target and attempts < max_attempts:
            attempts += 1
            src = node_ids[rng.randrange(len(node_ids))]
            dst = node_ids[rng.randrange(len(node_ids))]
            if src == dst:
                continue
            etype = edge_type_ids[rng.randrange(len(edge_type_ids))]
            edge_rows.append((src, etype, dst))
            if edge_props > 0:
                for idx in range(edge_props):
                    prop_rows.append((src, etype, dst, idx, edges_created + idx))
            edges_created += 1

        conn.execute("BEGIN;")
        if edge_rows:
            cur.executemany(
                "INSERT OR IGNORE INTO edges (src, etype, dst) VALUES (?, ?, ?);",
                edge_rows,
            )
        if prop_rows:
            cur.executemany(
                "INSERT OR REPLACE INTO edge_props (src, etype, dst, key_id, value) VALUES (?, ?, ?, ?, ?);",
                prop_rows,
            )
        conn.execute("COMMIT;")
        print(f"\r  Created {edges_created} / {edges} edges", end="")
    print()

    cur.close()
    return node_ids, node_keys, edge_type_ids


def benchmark_writes(conn: sqlite3.Connection, iterations: int) -> None:
    print("\n--- Batch Writes (100 nodes) ---")
    batch_size = 100
    batches = min(iterations // batch_size, 50)
    samples_ns: List[int] = []
    cur = conn.cursor()

    for b in range(batches):
        start = time.perf_counter_ns()
        conn.execute("BEGIN;")
        batch = []
        for i in range(batch_size):
            key = f"bench:raw:{b}:{i}"
            batch.append((None, key))
        cur.executemany("INSERT INTO nodes (id, key) VALUES (?, ?);", batch)
        conn.execute("COMMIT;")
        samples_ns.append(time.perf_counter_ns() - start)

    cur.close()
    print_latency_table("Batch of 100 nodes", samples_ns)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--nodes", type=int, default=10000)
    parser.add_argument("--edges", type=int, default=50000)
    parser.add_argument("--edge-types", type=int, default=3)
    parser.add_argument("--edge-props", type=int, default=10)
    parser.add_argument("--iterations", type=int, default=10000)
    parser.add_argument("--wal-size", type=int, default=256 * 1024 * 1024)
    parser.add_argument("--sync-mode", type=str, default="normal")
    parser.add_argument("--keep-db", action="store_true")
    args = parser.parse_args()

    sync_mode = args.sync_mode.lower()
    if sync_mode not in ("full", "normal", "off"):
        sync_mode = "normal"

    print("=" * 120)
    print("SQLite Benchmark (single-file raw)")
    print("=" * 120)
    print(f"Nodes: {args.nodes}")
    print(f"Edges: {args.edges}")
    print(f"Edge types: {args.edge_types}")
    print(f"Edge props: {args.edge_props}")
    print(f"Iterations: {args.iterations}")
    print(f"WAL size: {args.wal_size} bytes")
    print(f"Sync mode: {sync_mode}")
    print("=" * 120)

    tmpdir = tempfile.TemporaryDirectory()
    db_path = os.path.join(tmpdir.name, "sqlite-bench.db")

    conn = sqlite3.connect(db_path)
    apply_pragmas(conn, sync_mode, args.wal_size)
    build_schema(conn)

    print("\n[1/2] Building graph...")
    start_build = time.perf_counter()
    build_graph(
        conn,
        nodes=args.nodes,
        edges=args.edges,
        edge_types=args.edge_types,
        edge_props=args.edge_props,
        batch_size=5000,
    )
    print(f"  Built in {int((time.perf_counter() - start_build) * 1000)}ms")

    print("\n[2/2] Write benchmarks...")
    benchmark_writes(conn, args.iterations)

    conn.close()
    if args.keep_db:
        print(f"\nDatabase preserved at: {db_path}")
        tmpdir.cleanup = lambda: None


if __name__ == "__main__":
    main()
