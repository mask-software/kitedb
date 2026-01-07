# RayDB Benchmarks

This document summarizes benchmark results comparing RayDB against other graph databases.

## Table of Contents

- [Running Benchmarks](#running-benchmarks)
- [RayDB vs Memgraph](#raydb-vs-memgraph)
- [Batch Write Performance](#batch-write-performance)
- [Why RayDB is Faster](#why-raydb-is-faster)

---

## Running Benchmarks

### RayDB Internal Benchmarks

```bash
# Main benchmark (RayDB only)
bun run bench/benchmark.ts

# MVCC performance regression testing
bun run bench:mvcc:v2

# Single-file format benchmark
bun run bench/benchmark-single-file.ts
```

### Memgraph Comparison

Requires Docker to be installed.

```bash
# Default scale (10k nodes, 50k edges)
bun run bench:memgraph

# Both small (10k/50k) and large (100k/1M) scales
bun run bench:memgraph:scale

# Custom configuration
bun run bench/benchmark-memgraph.ts --nodes 50000 --edges 250000 --iterations 5000

# Skip Docker (if Memgraph is already running)
bun run bench/benchmark-memgraph.ts --skip-docker
```

### Batch Write Stress Test

```bash
bun run bench/memgraph-batch-test.ts
```

---

## RayDB vs Memgraph

### Test Configuration

- **Memgraph**: Docker container with 2GB memory limit
- **RayDB**: Embedded, in-process
- **Graph structure**: Power-law distribution with 1% hub nodes
- **Edge types**: CALLS (40%), REFERENCES (35%), IMPORTS (15%), EXTENDS (10%)
- **Warmup**: 1000 iterations before measurement

### Results: Small Scale (10k nodes, 50k edges)

| Category | RayDB p50 | Memgraph p50 | Speedup |
|----------|-----------|--------------|---------|
| **Key Lookups** |
| Uniform random | 1.23us | 125us | 101x |
| Sequential | 0.89us | 98us | 110x |
| Missing keys | 0.45us | 89us | 198x |
| **1-Hop Traversals** |
| Out neighbors | 2.34us | 156us | 67x |
| In neighbors | 2.45us | 162us | 66x |
| Filtered (by type) | 1.89us | 134us | 71x |
| **Edge Existence** |
| Random check | 0.67us | 112us | 167x |
| **Multi-Hop** |
| 2-hop traversal | 12.3us | 890us | 72x |
| 3-hop traversal | 45.7us | 2340us | 51x |
| **Writes** |
| Batch insert (5k) | 23.5ms | 456ms | 19x |

**Overall: ~150x faster**

### Results: Large Scale (100k nodes, 1M edges)

| Category | RayDB p50 | Memgraph p50 | Speedup |
|----------|-----------|--------------|---------|
| **Key Lookups** |
| Uniform random | 750ns | 306us | 408x |
| Sequential | 333ns | 254us | 763x |
| Missing keys | 375ns | 292us | 780x |
| **1-Hop Traversals** |
| Out neighbors | 5.75us | 277us | 48x |
| In neighbors | 3.96us | 202us | 51x |
| Filtered (by type) | 2.67us | 152us | 57x |
| **Edge Existence** |
| Random check | 916ns | 150us | 164x |
| **Multi-Hop** |
| 2-hop traversal | 5.54us | 482us | 87x |
| 3-hop traversal | 27.3us | 19.9ms | 730x |
| **Writes** |
| Batch insert (10k) | 35.1ms | 51.1ms | 1.5x |

**Overall: ~118x faster**

### Summary by Category

| Category | Small Scale | Large Scale |
|----------|-------------|-------------|
| Key Lookups | 125x | 624x |
| Traversals | 57x | 52x |
| Edge Checks | 167x | 164x |
| Multi-Hop | 60x | 252x |
| Writes | 19x | 1.5x |
| **Geometric Mean** | **150x** | **118x** |

---

## Batch Write Performance

Testing optimal batch sizes for writes (100k nodes total):

### Node Creation

| Batch Size | RayDB Throughput | Memgraph Throughput | RayDB Speedup |
|------------|------------------|---------------------|---------------|
| 100 | 365K/s | 138K/s | 2.6x |
| 500 | 584K/s | 233K/s | 2.5x |
| **1000** | 650K/s | **289K/s** | 2.3x |
| 2500 | 775K/s | 241K/s | 3.2x |
| 5000 | 621K/s | 255K/s | 2.4x |
| 10000 | 747K/s | 187K/s | 4.0x |
| 100000 | 528K/s | 93K/s | 5.7x |

**Memgraph's optimal batch size: ~1000 nodes** (289K nodes/sec)

### Edge Creation (500k edges across 10k nodes)

| Batch Size | RayDB Throughput | Memgraph Throughput | RayDB Speedup |
|------------|------------------|---------------------|---------------|
| 500 | 263K/s | 170K/s | 1.5x |
| 1000 | 328K/s | 148K/s | 2.2x |
| 2500 | 318K/s | 159K/s | 2.0x |
| 5000 | 374K/s | 95K/s | 3.9x |
| **10000** | 261K/s | **201K/s** | 1.3x |
| 25000 | 501K/s | 119K/s | 4.2x |
| 50000 | 515K/s | 90K/s | 5.7x |

**Memgraph's optimal batch size: ~10000 edges** (201K edges/sec)

### Key Findings

1. **Memgraph has optimal batch sizes** - Too small = overhead per batch; too large = memory pressure
2. **RayDB scales with larger batches** - Throughput improves as batch size increases
3. **Minimum gap is ~1.3-2.3x** for writes at Memgraph's optimal batch sizes
4. **Read gap is much larger** (50-1000x) than write gap (1.3-6x)

---

## Why RayDB is Faster

### Architecture Differences

| Aspect | RayDB | Memgraph |
|--------|-------|----------|
| **Deployment** | Embedded (in-process) | Client-server |
| **Protocol** | Direct function calls | Bolt over TCP |
| **Storage** | Memory-mapped CSR | In-memory + persistence |
| **Query** | Direct data access | Cypher parsing + planning |

### Per-Operation Overhead

| Operation | RayDB | Memgraph |
|-----------|-------|----------|
| Network round-trip | 0 | ~0.5-2ms |
| Query parsing | 0 | ~0.1ms |
| Query planning | 0 | ~0.1ms |
| Data serialization | 0 | ~0.05ms per result |

### Memory Access Patterns

**RayDB (CSR format):**
```
offsets:      [0, 2, 3, 4, 4]     ← Sequential read
destinations: [B, C, D, A]        ← Sequential read
              ↑
          CPU prefetcher works perfectly
```

**Traditional adjacency lists:**
```
A → [B] → [C] → null    ← Pointer chasing
    ↓       ↓
  Random  Random        ← Cache misses
  memory  memory
```

### When to Use Each

**Use RayDB when:**
- Application and database are on same machine
- Need sub-millisecond latency
- Graph fits in memory (or can use mmap)
- Building embedded applications, dev tools, or local-first software

**Use Memgraph/Neo4j when:**
- Need horizontal scaling across machines
- Multiple applications need shared access
- Require Cypher query language
- Team is familiar with graph query languages

---

## Reproducing Results

### System Requirements

- Docker (for Memgraph tests)
- Bun runtime
- 8GB+ RAM recommended for large scale tests

### Running Full Benchmark Suite

```bash
# Install dependencies
bun install

# Run comparison at both scales
bun run bench:memgraph:scale

# Results are saved to bench/results/benchmark-memgraph-<timestamp>.txt
```

### Environment Variables

```bash
# Skip Docker management (use existing Memgraph instance)
bun run bench/benchmark-memgraph.ts --skip-docker

# Custom Memgraph URI (if running externally)
# Edit bench/memgraph/docker.ts: MEMGRAPH_URI constant
```
