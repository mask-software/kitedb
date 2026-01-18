# Graph Implementation Improvements Plan

> Generated: 2026-01-15
> Status: Draft
> Author: Claude Code Analysis

This document outlines opportunities for improvement in RayDB's graph implementation, covering performance optimizations, edge cases, API improvements, and architectural enhancements.

---

## Table of Contents

1. [Performance Optimizations](#1-performance-optimizations)
2. [Traversal API Improvements](#2-traversal-api-improvements)
3. [Pathfinding Improvements](#3-pathfinding-improvements)
4. [Edge Cases & Error Handling](#4-edge-cases--error-handling)
5. [Cache Improvements](#5-cache-improvements)
6. [Delta/Snapshot Layer Improvements](#6-deltasnapshot-layer-improvements)
7. [Type Safety & API Improvements](#7-type-safety--api-improvements)
8. [Concurrency & MVCC Improvements](#8-concurrency--mvcc-improvements)
9. [Implementation Priority](#9-implementation-priority)

---

## 1. Performance Optimizations

### 1.1 Iterator Performance in `iterators.ts`

**File:** `src/ray/iterators.ts`

**Issue:** The `neighborsOut` and `neighborsIn` functions allocate arrays unnecessarily by calling `getOutEdges` which creates an intermediate array.

**Current Code (lines 95-108):**
```typescript
const snapshotEdges: { etype: ETypeID; other: NodeID }[] = [];
if (snapshot) {
  const phys = getPhysNode(snapshot, nodeId);
  if (phys >= 0) {
    const edges = getOutEdges(snapshot, phys);  // Creates array
    for (const edge of edges) {
      // ...
      snapshotEdges.push({ etype: edge.etype, other: dstNodeId });
    }
  }
}
```

**Improvement:** Use the existing `iterateOutEdges` generator instead of `getOutEdges`:

```typescript
if (snapshot) {
  const phys = getPhysNode(snapshot, nodeId);
  if (phys >= 0) {
    for (const edge of iterateOutEdges(snapshot, phys)) {
      const dstNodeId = getNodeId(snapshot, edge.dst);
      if (!isNodeDeleted(delta, dstNodeId)) {
        snapshotEdges.push({ etype: edge.etype, other: dstNodeId });
      }
    }
  }
}
```

**Impact:** Reduces memory allocations for high-degree nodes.

---

### 1.2 Edge Set Building Threshold in `hasEdgeMerged`

**File:** `src/ray/iterators.ts`

**Issue:** The threshold of 10 for building edge sets is arbitrary and rebuilds the Set on every call.

**Current Code (lines 214-226):**
```typescript
if (addPatches.length <= 10) {
  for (const patch of addPatches) {
    if (patch.etype === etype && patch.other === dst) {
      return true;
    }
  }
} else {
  const addSet = buildEdgeSet(addPatches);
  if (addSet.has(targetKey)) {
    return true;
  }
}
```

**Improvements:**
1. Cache the edge Set in `DeltaState` instead of rebuilding on each lookup
2. Profile to find optimal threshold (likely 20-50 based on Set construction cost)
3. Consider maintaining Sets lazily in delta when patches exceed threshold

**Suggested Implementation:**
```typescript
// In DeltaState type (types.ts)
export interface DeltaState {
  // ... existing fields
  
  // Cached edge sets (lazily populated when patch arrays exceed threshold)
  outAddSets?: Map<NodeID, Set<bigint>>;
  outDelSets?: Map<NodeID, Set<bigint>>;
}

// In delta.ts - populate when array exceeds threshold
const EDGE_SET_THRESHOLD = 32;

export function getEdgeSet(
  delta: DeltaState,
  nodeId: NodeID,
  type: 'add' | 'del'
): Set<bigint> | null {
  const patches = type === 'add' 
    ? delta.outAdd.get(nodeId) 
    : delta.outDel.get(nodeId);
  
  if (!patches || patches.length < EDGE_SET_THRESHOLD) {
    return null;
  }
  
  // Check cache
  const cache = type === 'add' ? delta.outAddSets : delta.outDelSets;
  if (cache?.has(nodeId)) {
    return cache.get(nodeId)!;
  }
  
  // Build and cache
  const set = buildEdgeSet(patches);
  if (!delta.outAddSets) delta.outAddSets = new Map();
  if (!delta.outDelSets) delta.outDelSets = new Map();
  (type === 'add' ? delta.outAddSets : delta.outDelSets).set(nodeId, set);
  
  return set;
}
```

**Impact:** O(1) edge existence checks for high-degree nodes with many delta changes.

---

### 1.3 Degree Calculation Optimization

**File:** `src/ray/iterators.ts`

**Issue:** `outDegreeMerged` and `inDegreeMerged` iterate through all neighbors just to count them.

**Current Code (lines 270-280):**
```typescript
export function outDegreeMerged(
  snapshot: SnapshotData | null,
  delta: DeltaState,
  nodeId: NodeID,
  filterEtype?: ETypeID,
): number {
  let count = 0;
  for (const _ of neighborsOut(snapshot, delta, nodeId, filterEtype)) {
    count++;
  }
  return count;
}
```

**Improvement:** Add optimized degree calculation using snapshot metadata:

```typescript
export function outDegreeMerged(
  snapshot: SnapshotData | null,
  delta: DeltaState,
  nodeId: NodeID,
  filterEtype?: ETypeID,
): number {
  if (isNodeDeleted(delta, nodeId)) return 0;
  
  // Fast path: no filter and can use snapshot metadata
  if (filterEtype === undefined && snapshot) {
    const phys = getPhysNode(snapshot, nodeId);
    if (phys >= 0) {
      let count = getOutDegree(snapshot, phys);
      
      // Adjust for delta changes
      const delPatches = delta.outDel.get(nodeId);
      if (delPatches) count -= delPatches.length;
      
      const addPatches = delta.outAdd.get(nodeId);
      if (addPatches) count += addPatches.length;
      
      // Subtract edges to deleted nodes (requires iteration unfortunately)
      // This could be optimized by tracking deleted-node edge counts
      
      return Math.max(0, count);
    }
  }
  
  // Slow path: iterate
  let count = 0;
  for (const _ of neighborsOut(snapshot, delta, nodeId, filterEtype)) {
    count++;
  }
  return count;
}
```

**Impact:** O(1) degree queries for common case (no type filter, no deleted destinations).

---

## 2. Traversal API Improvements

### 2.1 BFS Queue Efficiency

**File:** `src/api/traversal.ts`

**Issue:** The BFS in `executeTraverse` uses `Array.shift()` which is O(n).

**Current Code (lines 352-354):**
```typescript
const queue: [NodeRef, number][] = [[startNode, 0]];
while (queue.length > 0) {
  const [currentNode, depth] = queue.shift()!;  // O(n) operation
```

**Improvement:** Use index-based approach:

```typescript
async function* executeTraverse(
  startNode: NodeRef,
  step: TraversalStep,
  etypeId: ETypeID,
): AsyncGenerator<{ node: NodeRef; edge: EdgeResult }> {
  const options = step.options!;
  const minDepth = options.minDepth ?? 1;
  const maxDepth = options.maxDepth;
  const unique = options.unique ?? true;

  const visited = new Set<NodeID>();
  if (unique) {
    visited.add(startNode.$id);
  }

  // Use index-based queue for O(1) dequeue
  const queue: [NodeRef, number][] = [[startNode, 0]];
  let queueHead = 0;

  while (queueHead < queue.length) {
    const [currentNode, depth] = queue[queueHead++]!;

    if (depth >= maxDepth) continue;

    for (const result of executeSingleHop(
      currentNode,
      options.direction,
      step.edgeDef,
      etypeId,
    )) {
      const neighborId = result.node.$id;

      if (unique && visited.has(neighborId)) continue;
      if (unique) visited.add(neighborId);

      if (options.whereEdge && !options.whereEdge(result.edge)) continue;
      if (options.whereNode && !options.whereNode(result.node)) continue;

      if (depth + 1 >= minDepth) {
        yield result;
      }

      if (depth + 1 < maxDepth) {
        queue.push([result.node, depth + 1]);
      }
    }
  }
}
```

**Impact:** O(1) dequeue operations, significant speedup for large traversals.

---

### 2.2 Multi-Hop Traversal Deduplication

**File:** `src/api/traversal.ts`

**Issue:** In `countFast`, all intermediate node IDs are collected without deduplication.

**Current Code (lines 243-254):**
```typescript
let currentNodeIds: NodeID[] = startNodes.map(n => n.$id);
for (const step of steps) {
  const nextNodeIds: NodeID[] = [];
  for (const nodeId of currentNodeIds) {
    for (const neighborId of iterateSingleHopIds(...)) {
      nextNodeIds.push(neighborId);  // May have duplicates
    }
  }
  currentNodeIds = nextNodeIds;
}
```

**Improvement:** Add optional deduplication for accurate counting:

```typescript
function countFast(): number {
  if (edgeFilter !== null || nodeFilter !== null) {
    return -1;
  }
  
  for (const step of steps) {
    if (step.type === "traverse") {
      return -1;
    }
  }

  let currentNodeIds: Set<NodeID> = new Set(startNodes.map(n => n.$id));

  for (const step of steps) {
    const etypeId = resolveEtypeId(step.edgeDef);
    const nextNodeIds = new Set<NodeID>();

    for (const nodeId of currentNodeIds) {
      for (const neighborId of iterateSingleHopIds(
        nodeId, 
        step.type as "out" | "in" | "both", 
        etypeId
      )) {
        nextNodeIds.add(neighborId);
      }
    }

    currentNodeIds = nextNodeIds;
  }

  if (limit !== null && currentNodeIds.size > limit) {
    return limit;
  }

  return currentNodeIds.size;
}
```

**Impact:** Accurate counts for graphs with multiple paths to same node.

---

### 2.3 Fix `rawEdges()` Bidirectional Support

**File:** `src/api/traversal.ts`

**Issue:** `rawEdges()` doesn't correctly handle "both" direction - yields same nodeId as src.

**Current Code (line 493):**
```typescript
for (const neighborId of iterateSingleHopIds(nodeId, step.type, etypeId)) {
  yield { src: nodeId, dst: neighborId, etype: etypeId };  // Wrong for 'in'
```

**Improvement:**
```typescript
rawEdges(): Generator<RawEdge> {
  return (function* () {
    if (steps.length === 0) return;
    
    let currentNodeIds: NodeID[] = startNodes.map(n => n.$id);

    for (const step of steps) {
      if (step.type === "traverse") {
        throw new Error("rawEdges() does not support variable-depth traverse()");
      }

      const etypeId = resolveEtypeId(step.edgeDef);
      const directions: ("out" | "in")[] =
        step.type === "both" ? ["out", "in"] : [step.type as "out" | "in"];
      
      const nextNodeIds: NodeID[] = [];

      for (const nodeId of currentNodeIds) {
        for (const dir of directions) {
          const neighbors = dir === "out"
            ? getNeighborsOut(db, nodeId, etypeId)
            : getNeighborsIn(db, nodeId, etypeId);

          for (const edge of neighbors) {
            yield { src: edge.src, dst: edge.dst, etype: edge.etype };
            nextNodeIds.push(dir === "out" ? edge.dst : edge.src);
          }
        }
      }

      currentNodeIds = nextNodeIds;
    }
  })();
}
```

**Impact:** Correct edge direction for incoming edges.

---

## 3. Pathfinding Improvements

### 3.1 Remove Redundant Node Existence Check

**File:** `src/api/pathfinding.ts`

**Issue:** `nodeExists` is called for every neighbor, but `getNeighborsOut/In` already filters deleted nodes.

**Current Code (line 410):**
```typescript
if (visited.has(neighborId) || !nodeExists(db, neighborId)) {
  continue;
}
```

**Improvement:** Remove redundant check:
```typescript
if (visited.has(neighborId)) {
  continue;
}
```

**Impact:** Removes redundant snapshot/delta lookups per edge.

---

### 3.2 Consolidate A* State

**File:** `src/api/pathfinding.ts`

**Issue:** A* maintains four separate Maps for tracking state.

**Current Code (lines 477-487):**
```typescript
const gScores = new Map<NodeID, number>();
const fScores = new Map<NodeID, number>();
const parents = new Map<NodeID, { parent: NodeID | null; edge: EdgeResult | null }>();
const depths = new Map<NodeID, number>();
```

**Improvement:** Consolidate into single Map:

```typescript
interface AStarNodeState {
  gScore: number;
  fScore: number;
  depth: number;
  parent: NodeID | null;
  edge: EdgeResult | null;
}

export async function aStar<N extends NodeDef>(
  db: GraphDB,
  config: PathFindingConfig,
  // ...
): Promise<PathResult<N>> {
  const sourceId = config.source.$id;
  const targetIds = new Set(config.targets.map((t) => t.$id));

  const states = new Map<NodeID, AStarNodeState>();
  const visited = new Set<NodeID>();
  const queue = new MinHeap<NodeID>();

  // Initialize source
  const sourceHeuristic = heuristic(
    config.source as NodeRef<N>, 
    config.targets[0] as NodeRef<N>
  );
  
  states.set(sourceId, {
    gScore: 0,
    fScore: sourceHeuristic,
    depth: 0,
    parent: null,
    edge: null,
  });
  queue.insert(sourceId, sourceHeuristic);

  // ... rest of algorithm using states.get(nodeId)
}
```

**Impact:** Single Map lookup instead of four, better cache locality.

---

### 3.3 Implement K-Shortest Paths

**File:** `src/api/pathfinding.ts`

**Issue:** `allPaths` currently only returns the single shortest path.

**Current Code (lines 701-716):**
```typescript
async* allPaths(_maxPaths?: number) {
  // For now, just return the shortest path
  const result = await dijkstra(...);
  if (result.found) {
    yield result;
  }
}
```

**Improvement:** Implement Yen's K-Shortest Paths algorithm:

```typescript
async* allPaths(maxPaths: number = 10): AsyncGenerator<PathResult<N>> {
  // Find first shortest path
  const firstPath = await dijkstra(db, config, ...);
  if (!firstPath.found) return;
  
  yield firstPath;
  if (maxPaths === 1) return;

  // Yen's algorithm
  const paths: PathResult<N>[] = [firstPath];
  const candidates = new MinHeap<PathResult<N>>();
  
  for (let k = 1; k < maxPaths; k++) {
    const prevPath = paths[k - 1]!;
    
    // For each node in the previous path (except the last)
    for (let i = 0; i < prevPath.path.length - 1; i++) {
      const spurNode = prevPath.path[i]!;
      const rootPath = prevPath.path.slice(0, i + 1);
      
      // Remove edges that are part of previous paths with same root
      const removedEdges: Edge[] = [];
      for (const path of paths) {
        if (pathsShareRoot(path, rootPath)) {
          const edgeToRemove = path.edges[i];
          if (edgeToRemove) {
            removedEdges.push(edgeToRemove);
          }
        }
      }
      
      // Find spur path from spurNode to target
      // (with removed edges temporarily excluded)
      const spurPath = await dijkstraWithExclusions(
        db, config, spurNode, removedEdges
      );
      
      if (spurPath.found) {
        const totalPath = concatenatePaths(rootPath, spurPath);
        candidates.insert(totalPath, totalPath.totalWeight);
      }
    }
    
    if (candidates.isEmpty()) break;
    
    const nextPath = candidates.extractMin()!;
    paths.push(nextPath);
    yield nextPath;
  }
}
```

**Impact:** Enables finding alternative routes, essential for routing applications.

---

### 3.4 Edge Weight Caching

**File:** `src/api/pathfinding.ts`

**Issue:** Edge properties are loaded on every edge visit.

**Improvement:** Cache weights during pathfinding:

```typescript
const edgeWeightCache = new Map<string, number>();

function getEdgeWeight(src: NodeID, etype: ETypeID, dst: NodeID): number {
  const key = `${src}:${etype}:${dst}`;
  
  let weight = edgeWeightCache.get(key);
  if (weight !== undefined) {
    return weight;
  }
  
  const edgeResult = loadEdgeProperties(db, src, etype, dst, edgeDef, resolvePropKeyId);
  weight = weightFn(edgeResult);
  edgeWeightCache.set(key, weight);
  
  return weight;
}
```

**Impact:** Avoids repeated property lookups for same edge in pathfinding.

---

## 4. Edge Cases & Error Handling

### 4.1 Edge Counting with Node Deletions

**File:** `src/ray/graph-db/edges.ts`

**Issue:** `countEdges` may not correctly account for edges to/from deleted nodes.

**Current Code (lines 814-817):**
```typescript
// Note: We don't need to handle node deletions here because:
// - When a node is deleted, its edges are implicitly deleted
// ...
// However, for full correctness with node deletions, we should verify
```

**Improvement:** Add validation in debug mode:

```typescript
export function countEdges(
  handle: GraphDB | TxHandle,
  options?: { etype?: ETypeID },
): number {
  // ... existing code ...
  
  // In debug mode, validate count
  if (process.env.NODE_ENV === 'development') {
    let iteratedCount = 0;
    for (const _ of listEdges(handle, options)) {
      iteratedCount++;
    }
    if (iteratedCount !== count) {
      console.warn(
        `Edge count mismatch: metadata=${count}, iterated=${iteratedCount}. ` +
        `This may indicate edges to deleted nodes.`
      );
    }
  }
  
  return count;
}
```

---

### 4.2 Better Error Messages

**File:** `src/api/pathfinding.ts`

**Improvement:** Add context to error messages:

```typescript
// Current
if (!edgeDef) {
  throw new Error("Must specify at least one edge type with via()");
}

// Improved
if (!edgeDef) {
  throw new Error(
    `PathFindingBuilder: Must specify at least one edge type with via() ` +
    `before calling to() or toAny(). Example: db.shortestPath(source).via(edgeType).to(target)`
  );
}
```

---

### 4.3 Empty Start Nodes Validation

**File:** `src/api/traversal.ts`

**Improvement:** Add early validation:

```typescript
export function createTraversalBuilder<N extends NodeDef>(
  db: GraphDB,
  startNodes: NodeRef[],
  // ...
): TraversalBuilder<N> {
  if (startNodes.length === 0) {
    // Return a no-op builder that yields nothing
    return createEmptyTraversalBuilder<N>();
  }
  // ... rest of implementation
}

function createEmptyTraversalBuilder<N extends NodeDef>(): TraversalBuilder<N> {
  return {
    out: () => createEmptyTraversalBuilder(),
    in: () => createEmptyTraversalBuilder(),
    // ... all methods return empty results
    async count() { return 0; },
    async first() { return null; },
    async toArray() { return []; },
  };
}
```

---

### 4.4 Negative Weight Warning

**File:** `src/api/pathfinding.ts`

**Improvement:** Warn on negative weights:

```typescript
function createWeightFunction<E extends EdgeDef>(
  db: GraphDB,
  weightSpec: WeightSpec<E> | undefined,
  edgeDef: EdgeDef,
  resolvePropKeyId: (def: EdgeDef, propName: string) => PropKeyID,
): (edge: EdgeResult) => number {
  if (!weightSpec) {
    return () => 1.0;
  }

  const warnedEdges = new Set<string>();

  if ("property" in weightSpec) {
    const propName = weightSpec.property as string;
    const propKeyId = resolvePropKeyId(edgeDef, propName);

    return (edge: EdgeResult) => {
      const propValue = getEdgeProp(db, edge.$src, edge.$etype, edge.$dst, propKeyId);
      const weight = propValueToNumber(propValue);
      
      if (weight <= 0) {
        const key = `${edge.$src}:${edge.$etype}:${edge.$dst}`;
        if (!warnedEdges.has(key)) {
          warnedEdges.add(key);
          console.warn(
            `Pathfinding: Edge ${key} has non-positive weight ${weight}, ` +
            `using 1.0. Dijkstra/A* require positive weights.`
          );
        }
        return 1.0;
      }
      return weight;
    };
  }
  // ... custom function case
}
```

---

## 5. Cache Improvements

### 5.1 Numeric Cache Keys

**File:** `src/cache/traversal-cache.ts`

**Issue:** Cache keys are string concatenations, slower than numeric keys.

**Improvement:** Use BigInt packing:

```typescript
type TraversalKey = bigint;

// Pack: nodeId (53 bits) | etype (32 bits) | direction (1 bit)
// With etype=0xFFFFFFFF meaning "all"
private traversalKey(
  nodeId: NodeID,
  etype: ETypeID | undefined,
  direction: "out" | "in",
): TraversalKey {
  const etypeVal = etype === undefined ? 0xFFFFFFFF : etype;
  const dirVal = direction === "out" ? 0n : 1n;
  return (BigInt(nodeId) << 33n) | (BigInt(etypeVal) << 1n) | dirVal;
}

// For nodeKeyIndex, pack nodeId directly as bigint
private readonly nodeKeyIndex: Map<bigint, Set<TraversalKey>> = new Map();
```

**Impact:** Faster key comparison and hashing.

---

### 5.2 Track Destination Nodes for Invalidation

**File:** `src/cache/traversal-cache.ts`

**Issue:** Cache only tracks source nodes, not destinations.

**Current Code (line 109):**
```typescript
this.addToNodeIndex(nodeId, key);  // Only tracks source node
```

**Improvement:**

```typescript
set(
  nodeId: NodeID,
  etype: ETypeID | undefined,
  direction: "out" | "in",
  neighbors: Edge[],
): void {
  const key = this.traversalKey(nodeId, etype, direction);
  
  // ... existing truncation logic ...

  this.cache.set(key, { neighbors: cachedNeighbors, truncated });
  
  // Track source node
  this.addToNodeIndex(nodeId, key);
  
  // Track destination nodes for invalidation
  for (const edge of cachedNeighbors) {
    const destId = direction === "out" ? edge.dst : edge.src;
    this.addToNodeIndex(destId, key);
  }
}
```

**Impact:** Correct cache invalidation when destination nodes change.

---

## 6. Delta/Snapshot Layer Improvements

### 6.1 Edge Cleanup Reverse Index

**File:** `src/core/delta.ts`

**Issue:** Deleting a node requires iterating all edge maps to clean up edges.

**Improvement:** Maintain reverse index:

```typescript
export interface DeltaState {
  // ... existing fields ...
  
  // Reverse index: destination node -> set of source nodes with edges to it
  // Only populated for nodes that have incoming delta edges
  incomingEdgeSources?: Map<NodeID, Set<NodeID>>;
}

export function addEdge(
  delta: DeltaState,
  src: NodeID,
  etype: ETypeID,
  dst: NodeID,
): void {
  // ... existing code ...
  
  // Track reverse index
  if (!delta.incomingEdgeSources) {
    delta.incomingEdgeSources = new Map();
  }
  let sources = delta.incomingEdgeSources.get(dst);
  if (!sources) {
    sources = new Set();
    delta.incomingEdgeSources.set(dst, sources);
  }
  sources.add(src);
}

function removeEdgesInvolving(delta: DeltaState, nodeId: NodeID): void {
  // Fast path: use reverse index
  if (delta.incomingEdgeSources) {
    const sources = delta.incomingEdgeSources.get(nodeId);
    if (sources) {
      for (const src of sources) {
        // Remove edges from src to nodeId
        const outAdd = delta.outAdd.get(src);
        if (outAdd) {
          const filtered = outAdd.filter(p => p.other !== nodeId);
          if (filtered.length === 0) {
            delta.outAdd.delete(src);
          } else {
            delta.outAdd.set(src, filtered);
          }
        }
        // ... similar for outDel
      }
      delta.incomingEdgeSources.delete(nodeId);
    }
  }
  
  // Clean up outgoing edges from nodeId
  delta.outAdd.delete(nodeId);
  delta.outDel.delete(nodeId);
  delta.inAdd.delete(nodeId);
  delta.inDel.delete(nodeId);
}
```

**Impact:** O(k) edge cleanup where k = number of incoming edges, instead of O(n) where n = total edges.

---

## 7. Type Safety & API Improvements

### 7.1 Stricter Traversal Type Constraints

**File:** `src/api/traversal.ts`

**Improvement:** Add runtime type validation:

```typescript
function* executeSingleHop(
  node: NodeRef,
  direction: "out" | "in" | "both",
  edgeDef: EdgeDef,
  etypeId: ETypeID,
): Generator<{ node: NodeRef; edge: EdgeResult }> {
  // ... existing code ...

  for (const edge of neighbors) {
    const neighborId = dir === "out" ? edge.dst : edge.src;
    const neighborDef = getNodeDef(neighborId);
    
    if (!neighborDef) {
      // Log warning in development
      if (process.env.NODE_ENV === 'development') {
        console.warn(
          `Traversal: Node ${neighborId} has no definition. ` +
          `This may indicate a schema mismatch or deleted node.`
        );
      }
      continue;
    }
    
    // ... rest of code
  }
}
```

---

### 7.2 Consistent Async API

**File:** `src/api/traversal.ts`

**Improvement:** Provide both sync and async variants:

```typescript
export interface TraversalBuilder<N extends NodeDef = NodeDef> {
  // ... existing async methods ...

  /**
   * Synchronous raw edge iteration (no property loading)
   * Use this for maximum performance when you only need edge structure.
   */
  rawEdges(): Generator<RawEdge>;

  /**
   * Synchronous node ID iteration (no property loading)
   * Use this for counting or existence checks.
   */
  nodeIds(): Generator<NodeID>;
}
```

---

## 8. Concurrency & MVCC Improvements

### 8.1 Version-Aware Cache

**File:** `src/cache/traversal-cache.ts`

**Issue:** Cache is bypassed entirely in MVCC mode.

**Improvement:** Implement version-aware caching:

```typescript
interface VersionedCacheEntry<T> {
  value: T;
  commitTs: bigint;  // Version timestamp
}

class MvccTraversalCache {
  private cache: LRUCache<TraversalKey, VersionedCacheEntry<CachedNeighbors>>;
  
  get(
    nodeId: NodeID,
    etype: ETypeID | undefined,
    direction: "out" | "in",
    snapshotTs: bigint,
  ): CachedNeighbors | undefined {
    const key = this.traversalKey(nodeId, etype, direction);
    const entry = this.cache.get(key);
    
    // Only return if cached value is visible at snapshot timestamp
    if (entry && entry.commitTs <= snapshotTs) {
      return entry.value;
    }
    
    return undefined;
  }
  
  set(
    nodeId: NodeID,
    etype: ETypeID | undefined,
    direction: "out" | "in",
    neighbors: Edge[],
    commitTs: bigint,
  ): void {
    const key = this.traversalKey(nodeId, etype, direction);
    this.cache.set(key, { value: { neighbors, truncated: false }, commitTs });
  }
}
```

---

### 8.2 Bulk Read Tracking

**File:** `src/mvcc/tx-manager.ts`

**Improvement:** Add bulk read tracking for efficiency:

```typescript
export class TxManager {
  // ... existing code ...

  /**
   * Record multiple reads at once (for batch operations)
   */
  recordReads(txid: bigint, keys: string[]): void {
    const tx = this.transactions.get(txid);
    if (tx && tx.status === 'active') {
      for (const key of keys) {
        tx.readSet.add(key);
      }
    }
  }

  /**
   * Mark transaction as read-only (skips read tracking)
   */
  markReadOnly(txid: bigint): void {
    const tx = this.transactions.get(txid);
    if (tx) {
      (tx as any).readOnly = true;
    }
  }

  recordRead(txid: bigint, key: string): void {
    const tx = this.transactions.get(txid);
    if (tx && tx.status === 'active' && !(tx as any).readOnly) {
      tx.readSet.add(key);
    }
  }
}
```

---

## 9. Implementation Priority

### High Priority (Performance Impact)

| # | Area | File | Improvement |
|---|------|------|-------------|
| 1 | Traversal | `traversal.ts` | Replace `Array.shift()` with index-based BFS |
| 2 | Iterators | `iterators.ts` | Use generators instead of arrays for edges |
| 3 | Pathfinding | `pathfinding.ts` | Remove redundant `nodeExists` check |
| 4 | Cache | `traversal-cache.ts` | Track destination nodes for invalidation |

### Medium Priority (Correctness & Efficiency)

| # | Area | File | Improvement |
|---|------|------|-------------|
| 5 | Pathfinding | `pathfinding.ts` | Consolidate A* state into single Map |
| 6 | Cache | `traversal-cache.ts` | Use numeric keys instead of strings |
| 7 | Delta | `delta.ts` | Add reverse index for edge cleanup |
| 8 | Traversal | `traversal.ts` | Fix `rawEdges()` bidirectional support |
| 9 | Iterators | `iterators.ts` | Cache edge Sets in delta |

### Low Priority (Nice to Have)

| # | Area | File | Improvement |
|---|------|------|-------------|
| 10 | Pathfinding | `pathfinding.ts` | Implement K-shortest paths |
| 11 | Types | `traversal.ts` | Add stricter type constraints |
| 12 | MVCC | `traversal-cache.ts` | Implement version-aware cache |
| 13 | Errors | Various | Improve error messages |

---

## Appendix: Benchmarking Recommendations

Before implementing changes, establish baseline benchmarks:

```typescript
// bench/graph-improvements.ts
import { bench, run } from "mitata";

// 1. Traversal performance
bench("BFS traversal (current)", () => { /* current implementation */ });
bench("BFS traversal (optimized)", () => { /* new implementation */ });

// 2. Degree calculation
bench("outDegree iteration", () => { /* current */ });
bench("outDegree metadata", () => { /* optimized */ });

// 3. Cache key performance  
bench("string cache key", () => { /* current */ });
bench("bigint cache key", () => { /* optimized */ });

// 4. Pathfinding
bench("dijkstra 100 nodes", () => { /* baseline */ });
bench("dijkstra 1000 nodes", () => { /* baseline */ });
bench("dijkstra 10000 nodes", () => { /* baseline */ });

await run();
```

---

## Changelog

- **2026-01-15**: Initial plan created from code analysis
