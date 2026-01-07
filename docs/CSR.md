# CSR (Compressed Sparse Row) Format

This document explains how RayDB stores graph edges using the CSR format for maximum traversal performance.

## Table of Contents

- [The Problem](#the-problem)
- [The Solution](#the-solution)
- [How Traversal Works](#how-traversal-works)
- [Why It's Fast](#why-its-fast)
- [Bidirectional Edges](#bidirectional-edges)
- [Edge Types](#edge-types)
- [Implementation Details](#implementation-details)

---

## The Problem

Graphs are sparse - most nodes only connect to a few others. If you stored edges as an adjacency matrix:

```
     A  B  C  D
A  [ 0  1  1  0 ]   ← A connects to B and C
B  [ 0  0  0  1 ]   ← B connects to D  
C  [ 1  0  0  0 ]   ← C connects to A
D  [ 0  0  0  0 ]   ← D connects to nothing
```

With 100k nodes, that's 100k × 100k = **10 billion cells**. Wasteful when you only have 1M edges (0.01% density).

Alternative: Adjacency lists with pointers:

```
A → [B] → [C] → null
B → [D] → null
C → [A] → null
D → null
```

Better space, but **pointer chasing causes cache misses** - each hop goes to random memory.

---

## The Solution

CSR stores only the edges that exist, in a way that makes traversal cache-friendly.

### Step 1: List all edges in order by source node

```
A → B
A → C
B → D
C → A
```

### Step 2: Create two arrays

**destinations**: Just the targets, concatenated in order:

```
destinations = [B, C, D, A]
               └─A's─┘ └B┘ └C┘
```

**offsets**: Where each node's edges START in the destinations array:

```
offsets = [0, 2, 3, 4, 4]
           ↑  ↑  ↑  ↑  ↑
           A  B  C  D  end
```

The `offsets` array has N+1 entries (one extra for the end marker).

---

## How Traversal Works

### Example 1: "Who does node A connect to?"

```
A's index = 0

start = offsets[0] = 0
end   = offsets[1] = 2

destinations[0..2] = [B, C]

Answer: A connects to B and C
```

### Example 2: "Who does node B connect to?"

```
B's index = 1

start = offsets[1] = 2
end   = offsets[2] = 3

destinations[2..3] = [D]

Answer: B connects to D
```

### Example 3: "Who does node D connect to?"

```
D's index = 3

start = offsets[3] = 4
end   = offsets[4] = 4

destinations[4..4] = []  (empty range)

Answer: D connects to nobody
```

### The Pattern

```typescript
function getNeighbors(node: number): number[] {
  const start = offsets[node];
  const end = offsets[node + 1];
  return destinations.slice(start, end);
}
```

---

## Why It's Fast

### Memory Layout Comparison

**Traditional adjacency list (linked):**

```
┌───┐    ┌───┐    ┌───┐
│ A │ →  │ B │ →  │ C │    ← Pointer chasing
└───┘    └───┘    └───┘
  ↓        ↓        ↓
Random   Random   Random    ← Cache misses!
memory   memory   memory
```

**CSR format:**

```
offsets:      [0, 2, 3, 4, 4]     ← Contiguous array
destinations: [B, C, D, A]        ← Contiguous array
                ↑
            Sequential read = CPU cache loves this
```

### Performance Benefits

| Aspect | Benefit |
|--------|---------|
| **O(1) start** | Just two array lookups to begin iteration |
| **Sequential access** | All neighbors are adjacent in RAM |
| **Cache-friendly** | CPU prefetcher works perfectly |
| **Compact** | Only stores edges that exist |
| **mmap-able** | Can memory-map directly from disk |

### Numbers

On modern CPUs:
- L1 cache hit: ~1ns
- L2 cache hit: ~3ns
- L3 cache hit: ~10ns
- RAM access: ~100ns
- **Cache miss penalty: 10-100x slower**

CSR keeps neighbors contiguous, so after the first access, subsequent neighbors are likely already in cache.

---

## Bidirectional Edges

RayDB stores **both directions** for fast traversal either way:

### Out-Edges (A → B means "A connects to B")

```
out_offsets = [0, 2, 3, 4, 4]
out_dst     = [B, C, D, A]
```

### In-Edges (A ← C means "A is connected FROM C")

```
in_offsets = [0, 1, 2, 3, 4]
in_src     = [C, A, A, B]
```

This doubles storage but makes incoming neighbor queries equally fast.

---

## Edge Types

RayDB supports multiple edge types (like "FOLLOWS", "LIKES", "KNOWS"). These are stored in a parallel array:

```
out_offsets = [0, 2, 3, 4, 4]
out_dst     = [B, C, D, A]
out_etype   = [0, 1, 0, 0]    ← Edge type IDs
              ↑  ↑
           KNOWS LIKES
```

### Filtered Traversal

Edges within each node are **sorted by (etype, dst)**. This enables:

1. **Binary search** to find where a specific edge type starts
2. **Early termination** when you've passed the desired type

```typescript
// "Get A's KNOWS edges" - doesn't scan all edges
function getNeighborsByType(node: number, etype: number): number[] {
  const start = offsets[node];
  const end = offsets[node + 1];
  
  // Binary search to find etype range
  const typeStart = binarySearchStart(start, end, etype);
  const typeEnd = binarySearchEnd(start, end, etype);
  
  return destinations.slice(typeStart, typeEnd);
}
```

---

## Implementation Details

### Snapshot Sections

RayDB snapshots contain these CSR-related sections:

```typescript
enum SectionId {
  // Node ID mappings
  PHYS_TO_NODEID,      // u64[num_nodes] - Physical index → NodeID
  NODEID_TO_PHYS,      // i32[max_node_id+1] - NodeID → Physical index (-1 if deleted)
  
  // Out-edges CSR
  OUT_OFFSETS,         // u32[num_nodes+1]
  OUT_DST,             // u32[num_edges]
  OUT_ETYPE,           // u32[num_edges]
  
  // In-edges CSR
  IN_OFFSETS,          // u32[num_nodes+1]
  IN_SRC,              // u32[num_edges]
  IN_ETYPE,            // u32[num_edges]
  IN_OUT_INDEX,        // u32[num_edges] - Maps back to out-edge index
}
```

### Building CSR from Edge List

```typescript
function buildCSR(nodes: NodeID[], edges: Edge[]): CSR {
  const numNodes = nodes.length;
  
  // 1. Count edges per node
  const counts = new Uint32Array(numNodes);
  for (const edge of edges) {
    counts[nodeToPhys(edge.src)]++;
  }
  
  // 2. Build offsets (prefix sum)
  const offsets = new Uint32Array(numNodes + 1);
  for (let i = 0; i < numNodes; i++) {
    offsets[i + 1] = offsets[i] + counts[i];
  }
  
  // 3. Fill destinations (use counts as cursors)
  const destinations = new Uint32Array(edges.length);
  const etypes = new Uint32Array(edges.length);
  counts.fill(0); // Reset for use as write cursors
  
  for (const edge of edges) {
    const phys = nodeToPhys(edge.src);
    const idx = offsets[phys] + counts[phys];
    destinations[idx] = nodeToPhys(edge.dst);
    etypes[idx] = edge.etype;
    counts[phys]++;
  }
  
  // 4. Sort edges within each node by (etype, dst)
  for (let i = 0; i < numNodes; i++) {
    sortRange(destinations, etypes, offsets[i], offsets[i + 1]);
  }
  
  return { offsets, destinations, etypes };
}
```

### Memory Layout on Disk

```
[Section: OUT_OFFSETS]
┌────────────────────────────────────────┐
│ 0x00000000  0x00000002  0x00000003 ... │  ← 4 bytes per entry
└────────────────────────────────────────┘

[Section: OUT_DST]
┌────────────────────────────────────────┐
│ 0x00000001  0x00000002  0x00000003 ... │  ← Physical node IDs
└────────────────────────────────────────┘

[Section: OUT_ETYPE]
┌────────────────────────────────────────┐
│ 0x00000000  0x00000001  0x00000000 ... │  ← Edge type IDs
└────────────────────────────────────────┘
```

Each section can be independently compressed (zstd by default) and is memory-mapped for zero-copy access.

---

## Visual Summary

```
Graph:                          CSR Representation:
                               
     ┌──────┐                   offsets = [0, 2, 3, 4, 4]
     │  A   │──────┐                       A  B  C  D  end
     └──┬───┘      │           
        │          ▼            destinations = [B, C, D, A]
        ▼       ┌──────┐                      └─A─┘ └B┘└C┘
     ┌──────┐   │  C   │       
     │  B   │   └──┬───┘        Query: "A's neighbors?"
     └──┬───┘      │            → offsets[0]=0, offsets[1]=2
        │          │            → destinations[0..2] = [B, C] ✓
        ▼          │           
     ┌──────┐      │            Query: "D's neighbors?"
     │  D   │◄─────┘            → offsets[3]=4, offsets[4]=4
     └──────┘                   → destinations[4..4] = [] ✓
```

---

## Further Reading

- [Wikipedia: Sparse Matrix - CSR](https://en.wikipedia.org/wiki/Sparse_matrix#Compressed_sparse_row_(CSR,_CRS_or_Yale_format))
- [ARCHITECTURE.md](./ARCHITECTURE.md) - Full RayDB architecture overview
