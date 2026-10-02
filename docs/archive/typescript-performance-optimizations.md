# RayDB Single-File Performance Optimizations

> Note: The directory-based GraphDB (multi-file) engine has been removed. KiteDB is single-file (`.kitedb`) only; any GraphDB references below are historical.

This document outlines identified performance optimization opportunities for the single-file format, prioritized by impact and implementation effort.

## Background

After benchmarking and profiling the single-file format, we identified and fixed the primary performance issue: the pager was using `readSync()` to copy snapshot data into memory instead of `Bun.mmap()` for zero-copy access.

The `.kitedb` single-file format is the only supported storage path.

With that fix applied, the single-file format now achieves:
- **~2.82 MB storage** for 5K nodes, 25K edges
- **Comparable read performance** (some operations faster, some slightly slower)
- **~108K nodes/sec** for 10-node batches

The remaining optimizations below can further improve performance.

---

## High Impact Optimizations

### 1. CRC Validation on Every Parse

**Location:** `src/core/snapshot-reader.ts:176-183`

**Problem:** CRC32C is computed over the entire snapshot buffer on every call to `parseSnapshot()`. For a 100MB snapshot, this means ~100MB of memory reads just for validation.

```typescript
// Current: Always validates CRC
const computedCrc = crc32c(buffer.subarray(0, crcOffset));  // O(snapshot_size)
if (footerCrc !== computedCrc) {
  throw new Error(`Snapshot CRC mismatch`);
}
```

**Impact:** First read after open and after checkpoint must scan entire snapshot.

**Solution:** Make CRC validation optional/lazy:

```typescript
export function parseSnapshot(
  buffer: Uint8Array, 
  options?: { skipCrcValidation?: boolean }
): SnapshotData {
  // ...header parsing...
  
  if (!options?.skipCrcValidation) {
    const computedCrc = crc32c(buffer.subarray(0, crcOffset));
    if (footerCrc !== computedCrc) {
      throw new Error(`Snapshot CRC mismatch`);
    }
  }
  // ...continue parsing...
}
```

**Effort:** Low
**Impact:** High (eliminates O(n) scan on cached reads)

---

### 2. Aggressive Cache Invalidation

**Location:** `src/cache/property-cache.ts:95-115`, `src/cache/traversal-cache.ts:93-107`

**Problem:** Any node/edge modification clears the ENTIRE cache:

```typescript
// property-cache.ts
invalidateNode(nodeId: NodeID): void {
  this.nodeCache.clear();  // CLEARS ALL node properties!
}

// traversal-cache.ts  
invalidateEdge(src: NodeID, etype: ETypeID, dst: NodeID): void {
  this.cache.clear();  // CLEARS ALL traversal data!
}
```

**Impact:** Write operations destroy cache effectiveness entirely.

**Solution:** Track node -> keys mapping for targeted invalidation:

```typescript
class PropertyCache {
  private readonly nodeCache: LRUCache<NodePropKey, PropValue | null>;
  private readonly nodeKeyIndex: Map<NodeID, Set<NodePropKey>> = new Map();
  
  setNodeProp(nodeId: NodeID, propKeyId: PropKeyID, value: PropValue | null): void {
    const key = this.nodePropKey(nodeId, propKeyId);
    this.nodeCache.set(key, value);
    
    // Track which keys belong to this node
    let keys = this.nodeKeyIndex.get(nodeId);
    if (!keys) {
      keys = new Set();
      this.nodeKeyIndex.set(nodeId, keys);
    }
    keys.add(key);
  }
  
  invalidateNode(nodeId: NodeID): void {
    const keys = this.nodeKeyIndex.get(nodeId);
    if (keys) {
      for (const key of keys) {
        this.nodeCache.delete(key);
      }
      this.nodeKeyIndex.delete(nodeId);
    }
  }
}
```

**Effort:** Medium
**Impact:** High (cache remains useful during writes)

---

### 3. WAL Records Built Twice

**Location:** `graph-db/checkpoint.ts:27-39`

**Problem:** `buildWalRecord()` allocates memory and computes CRC just to estimate size:

```typescript
export function shouldCheckpoint(db: GraphDB, pendingRecords: WalRecord[]): boolean {
  let pendingSize = 0;
  for (const record of pendingRecords) {
    pendingSize += buildWalRecord(record).length;  // BUILDS EACH RECORD for size
  }
}
```

**Impact:** Double memory allocation and CRC computation for every commit.

**Solution:** Add size estimation function:

```typescript
export function estimateWalRecordSize(record: WalRecord): number {
  const headerSize = WAL_RECORD_HEADER_SIZE;
  const crcSize = 4;
  const unpadded = headerSize + record.payload.length + crcSize;
  return alignUp(unpadded, WAL_RECORD_ALIGNMENT);
}

// In shouldCheckpoint:
for (const record of pendingRecords) {
  pendingSize += estimateWalRecordSize(record);  // No allocation!
}
```

**Effort:** Low
**Impact:** Medium (reduces memory churn and CPU)

---

### 4. Page-by-Page WAL Writes

**Location:** `src/core/wal-buffer.ts:138-170`

**Problem:** Each small WAL record (~100 bytes) triggers a full 4KB page read-modify-write:

```typescript
private writeAtOffset(offset: number, data: Uint8Array): void {
  const page = this.pager.readPage(startPage);  // READ 4KB
  page.set(data, pageOffset);                    // MODIFY ~100 bytes
  this.pager.writePage(startPage, page);         // WRITE 4KB
}
```

**Impact:** Massive I/O amplification for write-heavy workloads.

**Solution:** Buffer writes in memory, flush pages once:

```typescript
class WalBuffer {
  private pendingWrites: Map<number, Uint8Array> = new Map();
  
  writeRecord(record: WalRecord): number {
    const recordBytes = buildWalRecord(record);
    this.bufferWrite(fileOffset, recordBytes);
    return this.head;
  }
  
  private bufferWrite(offset: number, data: Uint8Array): void {
    const pageNum = Math.floor(offset / this.pager.pageSize);
    let pageBuffer = this.pendingWrites.get(pageNum);
    if (!pageBuffer) {
      pageBuffer = this.pager.readPage(pageNum);
      this.pendingWrites.set(pageNum, pageBuffer);
    }
    pageBuffer.set(data, offset % this.pager.pageSize);
  }
  
  async flush(): Promise<void> {
    for (const [pageNum, data] of this.pendingWrites) {
      this.pager.writePage(pageNum, data);
    }
    this.pendingWrites.clear();
    await this.pager.sync();
  }
}
```

**Effort:** Medium
**Impact:** High (reduces I/O by 10-40x for small records)

---

## Medium Impact Optimizations

### 5. getSnapshot() Call Overhead

**Location:** `graph-db/snapshot-helper.ts:15-38`

**Problem:** Every read operation calls `getSnapshot()` which has branch checks:

```typescript
export function getSnapshot(db: GraphDB): SnapshotData | null {
  if (!db._isSingleFile) {        // Branch 1
    return db._snapshot;
  }
  if (db._snapshotCache) {        // Branch 2
    return db._snapshotCache;
  }
  // ...slow path...
}
```

**Impact:** ~5-10ns overhead per operation (adds up in tight loops).

**Solution Options:**
- Cache snapshot reference locally in hot functions
- Consider storing parsed snapshot directly in `db._snapshot` for single-file too

**Effort:** Low
**Impact:** Low-Medium

---

### 6. Linear Scan in Delta Edge Lookup

**Location:** `iterators.ts:188-195`

**Problem:** `hasEdgeMerged()` linearly scans delta patches:

```typescript
const addPatches = delta.outAdd.get(src);
if (addPatches) {
  for (const patch of addPatches) {  // O(n) scan!
    if (patch.etype === etype && patch.other === dst) {
      return true;
    }
  }
}
```

**Impact:** If a node has 1000 added edges, every edge existence check does 1000 comparisons.

**Solution:** Add Set-based lookup alongside array:

```typescript
// In DeltaState, maintain both:
outAdd: Map<NodeID, EdgePatch[]>           // For iteration
outAddSet: Map<NodeID, Set<bigint>>        // For O(1) lookup: (etype << 32n | BigInt(dst))
```

**Effort:** Medium
**Impact:** Medium (significant for edge-heavy workloads)

---

### 7. Array Allocation in getOutEdges

**Location:** `src/core/snapshot-reader.ts:360-367`

**Problem:** Creates array and objects for every traversal:

```typescript
export function getOutEdges(snapshot: SnapshotData, phys: PhysNode) {
  const edges: { dst: PhysNode; etype: ETypeID }[] = [];
  for (let i = start; i < end; i++) {
    edges.push({  // Object allocation per edge!
      dst: readU32At(snapshot.outDst, i),
      etype: readU32At(snapshot.outEtype, i),
    });
  }
  return edges;
}
```

**Impact:** GC pressure from allocations, especially for high-degree nodes.

**Solution:** Use generator/iterator pattern:

```typescript
export function* iterateOutEdges(
  snapshot: SnapshotData,
  phys: PhysNode,
): Generator<{ dst: PhysNode; etype: ETypeID }> {
  const start = readU32At(snapshot.outOffsets, phys);
  const end = readU32At(snapshot.outOffsets, phys + 1);
  
  for (let i = start; i < end; i++) {
    yield {
      dst: readU32At(snapshot.outDst, i),
      etype: readU32At(snapshot.outEtype, i),
    };
  }
}
```

**Effort:** Medium
**Impact:** Medium

---

### 8. Snapshot Cache Timing After Checkpoint

**Location:** `graph-db/checkpoint.ts`

**Problem:** Cache is invalidated after checkpoint, then next read must re-parse:

```typescript
// Current flow:
invalidateSnapshotCache(db);  // Cache = null
// ...later, on first read...
parseSnapshot(db._snapshotMmap);  // Re-parse entire snapshot
```

**Solution:** Pre-populate cache immediately after checkpoint:

```typescript
// After compaction completes:
const pager = db._pager as FilePager;
const newMmap = pager.mmapRange(
  Number(db._header.snapshotStartPage),
  Number(db._header.snapshotPageCount)
);
(db as any)._snapshotMmap = newMmap;
(db as any)._snapshotCache = parseSnapshot(newMmap);  // Pre-parse immediately
```

**Effort:** Low
**Impact:** Low-Medium (eliminates parse delay after checkpoint)

---

## Lower Priority Optimizations

### 9. Configurable fsync Behavior

**Location:** `src/core/pager.ts:209-218`

**Current:** Always does blocking fsync on commit.

**Solution:** Make durability configurable:

```typescript
interface OpenOptions {
  syncMode?: 'full' | 'off' | 'batch';
  // 'full'  = fsync every commit (default, safest)
  // 'off'   = no fsync (fastest, data loss on crash)
  // 'batch' = fsync every N commits or T milliseconds
}
```

**Effort:** Low
**Impact:** User-configurable (trading durability for speed)

---

### 10. Key Lookup Cache

**Location:** `src/cache/index.ts`

**Problem:** No dedicated cache for `getNodeByKey()` results.

**Solution:** Add key -> NodeID cache:

```typescript
class CacheManager {
  private keyCache: LRUCache<string, NodeID | null> = new LRUCache(10000);
  
  getNodeByKey(key: string): NodeID | null | undefined {
    return this.keyCache.get(key);
  }
  
  setNodeByKey(key: string, nodeId: NodeID | null): void {
    this.keyCache.set(key, nodeId);
  }
}
```

**Effort:** Medium
**Impact:** Medium (helps key-lookup-heavy workloads)

---

## Implementation Priority

### Phase 1: Quick Wins (Low effort, High impact)
1. CRC lazy validation
2. WAL record size estimation (avoid double build)
3. Snapshot cache pre-population after checkpoint

### Phase 2: Cache Improvements (Medium effort, High impact)
4. Targeted cache invalidation (property + traversal caches)
5. WAL write batching

### Phase 3: Hot Path Optimization (Medium effort, Medium impact)
6. Delta edge lookup with Set
7. Iterator-based edge traversal
8. Key lookup cache

### Phase 4: Configuration (Low effort, Variable impact)
9. Configurable fsync behavior
10. getSnapshot() inlining

---

## Benchmarking Notes

When implementing optimizations, benchmark with:

```bash
cd ray-rs
cargo run --release --example single_file_raw_bench --no-default-features -- \
  --nodes 10000 --edges 50000 --iterations 10000
```

Key metrics to track:
- Key lookup latency (p50, p95, p99)
- Traversal latency (1-hop, 2-hop, 3-hop)
- Write throughput (batch sizes: 10, 100, 1000)
- Database size
- Memory usage (for cache effectiveness)
