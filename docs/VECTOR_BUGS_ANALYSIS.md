# Vector Functionality Analysis - Bugs, Issues & Edge Cases

**Date:** January 14, 2026  
**Analyst:** Code Review  
**Scope:** Full vector embedding system (`src/vector/`, `src/api/vector-search.ts`, `src/ray/graph-db/vectors.ts`)

---

## Summary Table

| Severity | Count | Description |
|----------|-------|-------------|
| Critical | 4 | Memory leaks, incorrect distance functions |
| Medium | 6 | Logic errors, metric inconsistencies |
| Edge Cases | 7 | Missing validation, boundary conditions |
| Performance | 3 | Algorithmic inefficiencies |
| Missing Tests | 7 | Coverage gaps |

---

## Critical Issues

### 1. `vectorStoreDelete` doesn't remove from `vectorIdToLocation` mapping

**File:** `src/vector/columnar-store.ts` (lines 129-149)  
**Status:** ✅ FIXED

```typescript
export function vectorStoreDelete(
  manifest: VectorManifest,
  nodeId: NodeID
): boolean {
  // ...
  if (deleted) {
    manifest.nodeIdToVectorId.delete(nodeId);  // Removes from nodeIdToVectorId
    manifest.totalDeleted++;
  }
  // MISSING: manifest.vectorIdToLocation.delete(vectorId);
  return deleted;
}
```

**Problem:** The `vectorIdToLocation` map is never cleaned up on delete. This causes:
- Memory leak over time as deleted vectors accumulate
- `vectorStoreGetNodeId()` iterates over stale entries
- Serialization includes orphaned entries

**Fix:** Add `manifest.vectorIdToLocation.delete(vectorId)` after successful deletion.

---

### 2. `vectorStoreGetNodeId` has O(n) complexity - scales poorly

**File:** `src/vector/columnar-store.ts` (lines 224-234)  
**Status:** ✅ FIXED - Added `vectorIdToNodeId` reverse map to VectorManifest

```typescript
export function vectorStoreGetNodeId(
  manifest: VectorManifest,
  vectorId: number
): NodeID | undefined {
  for (const [nodeId, vid] of manifest.nodeIdToVectorId) {  // O(n) scan
    if (vid === vectorId) {
      return nodeId;
    }
  }
  return undefined;
}
```

**Problem:** Linear scan for every call. Should maintain a reverse map.

**Fix:** Add `vectorIdToNodeId: Map<number, NodeID>` to VectorManifest and keep it synchronized.

---

### 3. `getBatchDistanceFunction` returns wrong function for "dot" metric

**File:** `src/vector/distance.ts` (lines 237-253)  
**Status:** ✅ FIXED - Added `batchDotProductDistance` function

```typescript
export function getBatchDistanceFunction(
  metric: "cosine" | "euclidean" | "dot"
): (...) => Float32Array {
  switch (metric) {
    case "cosine":
    case "dot":
      return batchCosineDistance;  // Both use same function - WRONG
    case "euclidean":
      return batchSquaredEuclidean;
  }
}
```

**Problem:** Dot product metric should return negated dot products (higher = better = lower distance), but `batchCosineDistance` computes `1 - dot` which is cosine distance. For dot product similarity, this is incorrect semantics.

**Fix:** Create `batchDotProductDistance` that returns `-dot` for each vector.

---

### 4. IVF index search doesn't use the correct distance function for non-cosine metrics

**File:** `src/vector/ivf-index.ts` (lines 455-540)  
**Status:** ✅ FIXED - `ivfSearch` and `bruteForceSearch` now use `getDistanceFunction(metric)`

```typescript
export function ivfSearch(...): VectorSearchResult[] {
  // ...
  // Normalize query
  const queryNorm = normalize(query);  // Always normalizes
  
  // ...
  // Compute distance
  const dist = cosineDistance(queryNorm, vec);  // Always uses cosine
```

**Problem:** The search always uses `cosineDistance` regardless of the `metric` in the manifest config. For euclidean or dot product metrics, this gives incorrect results.

**Fix:** Use `getDistanceFunction(manifest.config.metric)` to get the appropriate distance function.

---

## Medium Issues

### 5. `fragmentGetVector` calculates `rowGroupSize` incorrectly for partial fragments

**File:** `src/vector/fragment.ts` (lines 205-231)  
**Status:** ✅ FIXED

**Problem:** The first row group may have a smaller `data.length` after trimming (sealed fragment with partial last row group).

**Solution:** Added smarter row group size calculation:
- If `rowGroupCapacity` is provided, use it
- If fragment has multiple row groups, use first row group's capacity (never trimmed)
- If single row group, use max of data length and count (handles trimmed case)

Added tests for sealed single and multi-row-group fragments.

---

### 6. Potential division by zero in `fragmentSeal`

**File:** `src/vector/fragment.ts` (lines 160-185)  
**Status:** ✅ FIXED

**Issue:** Edge case handling for empty row groups in fragmentSeal.

**Solution:** Added defensive checks:
- Check if fragment has row groups before accessing
- Only trim if `count > 0` and `data.length > 0`
- Verify dimensions is a positive integer before trimming

Note: In normal use, row groups are pre-allocated with capacity, so `data.length` is never 0. This fix handles malformed data edge cases.

---

### 7. IVF training always uses cosine distance

**File:** `src/vector/ivf-index.ts` (lines 114-238)  
**Status:** ✅ FIXED

The `ivfTrain` function always uses `cosineDistance` for k-means clustering regardless of the configured metric. This could lead to suboptimal cluster assignments for euclidean or dot product use cases.

**Solution:**
- Added `metric` field to `IvfConfig` type
- Updated `ivfTrain` to use `getDistanceFunction(metric)` for clustering
- Updated `findNearestCentroid` and `findNearestCentroids` to use the index's metric
- Updated `ivfSearch` to use the index's metric
- Updated IVF serialization to persist and restore the metric
- Added end-to-end tests for euclidean and dot product metrics

---

### 8. Zero vector handling in normalization

**File:** `src/vector/normalize.ts` (lines 38-60)  
**Status:** ✅ FIXED - Added `validateVector()` that rejects zero vectors at insert time

```typescript
export function normalizeInPlace(v: Float32Array): number {
  const norm = l2Norm(v);
  if (norm > 0) {
    // normalize
  }
  return norm;  // Returns 0 for zero vector, vector unchanged
}
```

**Original Issue:** Zero vectors pass through unchanged, which is technically correct but can cause issues:
- Cosine similarity with a zero vector is undefined (0/0)
- Distance calculations will give unexpected results
- No warning or error is raised

**Solution:** Added `validateVector()` function that checks for NaN, Infinity, and zero vectors.
The `vectorStoreInsert()` function now validates vectors by default (can be skipped for performance).

---

### 9. Compaction doesn't update IVF index

**Files:** `src/vector/compaction.ts` and `src/api/vector-search.ts`  
**Status:** ✅ NOT A BUG (by design)

When compaction runs:
1. Vector IDs remain the same ✓
2. Fragment IDs and local indices change ✓
3. `applyCompaction()` updates `vectorIdToLocation` map ✓
4. IVF index stores `vectorId`s (not locations) and uses `vectorStoreGetById()` which reads from `vectorIdToLocation`

**Conclusion:** The architecture correctly separates vector identity (vectorId) from storage location (fragmentId/localIndex). The IVF index remains valid after compaction.

---

### 10. Race condition in `VectorIndex.buildIndex()` and `set()`

**File:** `src/api/vector-search.ts` (lines 125-158, 202-255)  
**Status:** ✅ FIXED

If `set()` is called during `buildIndex()`:
- New vectors may not be included in training data
- Index may be partially built
- `_needsTraining` flag manipulation isn't atomic

**Solution:** Added `_isBuilding` guard flag:
- `buildIndex()` sets flag on entry, clears on exit (in finally block)
- `set()` and `delete()` throw if called while building
- Prevents concurrent modifications during index construction

---

## Edge Cases Not Covered

### 11. Empty query vector in search
~~No validation that the query vector isn't all zeros before search.~~
**Status:** ✅ FIXED - Query vectors are now validated in `VectorIndex.search()`

### 12. NaN/Infinity values in vectors
~~No validation for NaN or Infinity values during insertion, which could corrupt similarity calculations.~~
**Status:** ✅ FIXED - `validateVector()` now checks for NaN, Infinity, and zero vectors

### 13. Very large dimensions (e.g., 10000+)
May cause memory issues in row group allocation and slow distance calculations.

### 14. Concurrent read/write to same vector
**Status:** DOCUMENTED (by design)

The vector store and IVF index are **not thread-safe**. Concurrent access must be synchronized externally.

**Thread-Safety Notes:**
- `VectorManifest` contains mutable Maps that are not atomic
- IVF index training and insertion mutate shared state
- Compaction reads and writes to fragment data
- The `VectorIndex` class in `src/api/vector-search.ts` does not implement internal locking

**Recommendation:** Use a single writer with multiple readers pattern, or implement external synchronization (mutex/lock) when concurrent access is required.

### 15. Fragment with all vectors deleted
**Status:** ✅ FIXED

After all vectors in a fragment are deleted:
- Fragment still takes memory
- `fragmentIterator` returns empty but fragment still exists
- Compaction check may not trigger if deletion ratio calculation has edge case

**Solution:** Added `clearDeletedFragments()` function that efficiently clears fragments with 100% deletion ratio without going through full compaction. Also updated `findFragmentsToCompact()` to allow compaction even when total live vectors is 0.

### 16. `ivfSearchMulti` with empty queries array
**Status:** ✅ FIXED

```typescript
export function ivfSearchMulti(
  index: IvfIndex,
  manifest: VectorManifest,
  queries: Float32Array[],
  k: number,
  aggregation: "min" | "max" | "avg" | "sum",
  ...
): VectorSearchResult[]
```
Now throws an error if queries array is empty or undefined:
```typescript
if (!queries || queries.length === 0) {
  throw new Error("ivfSearchMulti requires at least one query vector");
}
```

### 17. Serialization with BigInt NodeID overflow
**File:** `src/vector/ivf-serialize.ts` (lines 295-302)

```typescript
view.setBigInt64(offset, BigInt(nodeId), true);
// ...
const nodeId = Number(view.getBigInt64(offset, true));
```

If NodeID > `Number.MAX_SAFE_INTEGER`, precision loss occurs.

---

## Performance Issues

### 18. `compactFragments` has O(n*m) complexity for location lookup

**File:** `src/vector/compaction.ts` (lines 139-147)  
**Status:** ✅ FIXED

Previously did an O(m) lookup for each of n vectors. Now builds a reverse lookup map first:
```typescript
// Build reverse lookup: (fragmentId, localIndex) -> vectorId
const locationToVectorId = new Map<string, number>();
for (const [vectorId, loc] of manifest.vectorIdToLocation) {
  if (fragmentIdSet.has(loc.fragmentId)) {
    const key = `${loc.fragmentId}:${loc.localIndex}`;
    locationToVectorId.set(key, vectorId);
  }
}
```
Complexity is now O(n + m) instead of O(n * m).

### 19. `findKNearest` creates unnecessary array allocations

**File:** `src/vector/distance.ts` (lines 276-307)  
**Status:** ✅ FIXED

Now uses a max-heap to track the k smallest distances, avoiding allocation of pairs for all n distances. Only allocates O(k) pairs instead of O(n).

### 20. No vector caching in `VectorIndex.search()`

**Status:** LOW PRIORITY (by design)

Fetching same vector multiple times in different search calls.

**Analysis:**
- Current lookup is O(1) Map + O(f) fragment find + O(1) array slice
- Vectors are returned as views (subarrays), avoiding copies
- Adding an LRU cache would add memory overhead and complexity
- For most use cases, the current performance is sufficient

**Potential optimization:** Convert `fragments` array to a Map<fragmentId, Fragment> for O(1) fragment lookup if profiling shows this as a bottleneck.

---

## Missing Tests

1. ✅ **`ivfSearchMulti` aggregation modes** - Added tests for min, max, avg, sum aggregation
2. **No test for compaction + IVF index consistency** - NOT NEEDED (architecture ensures consistency via vectorIds)
3. ✅ **Serialization with large node IDs** - Tests added for IDs up to MAX_SAFE_INTEGER
4. ✅ **Euclidean metric end-to-end** - Test added in previous session
5. ✅ **Dot product metric end-to-end** - Test added in previous session
6. ✅ **Fuzz testing for malformed serialized data** - Tests added for empty, truncated, and corrupted buffers
7. ✅ **Concurrent access tests** - Added test for buildIndex guard that prevents modifications during index building

---

## Fix Priority

### Phase 1 - Critical (Do Now) ✅ COMPLETED
1. ✅ Fix `vectorStoreDelete` to clean up `vectorIdToLocation`
2. ✅ Add reverse map `vectorIdToNodeId` for O(1) lookup
3. ✅ Fix `getBatchDistanceFunction` for dot product
4. ✅ Fix `ivfSearch` to use correct distance function per metric

### Phase 2 - Medium (Completed)
5. ✅ Add validation for zero/NaN/Infinity vectors
6. ✅ Fix IVF training to respect metric configuration
7. ✅ Compaction + IVF index synchronization (verified correct by design)

### Phase 3 - Edge Cases (Completed)
8. ✅ Added tests for `ivfSearchMulti` aggregation modes
9. ✅ Documented thread safety limitations
10. ✅ Fixed compaction O(n*m) complexity with reverse lookup map
