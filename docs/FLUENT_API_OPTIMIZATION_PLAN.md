# Fluent API Optimization Plan

## Executive Summary

The fluent API has significant overhead compared to raw graph operations:
- **Key lookup**: 13x slower (125ns → 1.63µs)
- **1-hop traversal**: 16x slower (1.29µs → 20.63µs)
- **Insert**: Nearly identical (1.01x) - no optimization needed

The root causes are:
1. Unnecessary property loading on every operation
2. Async generators where sync would suffice
3. Expensive node definition resolution via key prefix matching
4. Object allocations that could be avoided

---

## Benchmark Results (Baseline)

```
Insert (single)    raw p50=   63.38µs  fluent p50=   64.13µs  overhead=1.01x
Key lookup         raw p50=     125ns  fluent p50=    1.63µs  overhead=13.00x
1-hop traversal    raw p50=    1.29µs  fluent p50=   20.63µs  overhead=15.96x
```

---

## Phase 1: Key Lookup Optimization (Target: <3x overhead)

### Problem

`db.get(user, keyArg)` loads ALL properties for the node even when only the reference is needed.

**Current code path** (`ray.ts:287-309`):
```ts
async get<N extends NodeDef>(node: N, key: ...): Promise<...> {
  const fullKey = node.keyFn(key as never);           // 1. Key transformation
  const nodeId = getNodeByKey(this._db, fullKey);     // 2. Key lookup (fast)
  
  // 3. Load ALL properties - SLOW
  for (const [propName, propDef] of Object.entries(node.props)) {
    const propKeyId = this.resolvePropKeyId(node, propName);
    const propValue = getNodeProp(this._db, nodeId, propKeyId);
    if (propValue) {
      props[propName] = this.fromPropValue(propValue);
    }
  }
  
  return createNodeRef(node, nodeId, fullKey, props); // 4. Object allocation
}
```

### Solution 1.1: Add lightweight `getRef()` method

Add a new method that returns only the node reference without loading properties:

```ts
// New fast path - returns NodeRef without properties
async getRef<N extends NodeDef>(
  node: N,
  key: Parameters<N["keyFn"]>[0],
): Promise<NodeRef<N> | null> {
  const fullKey = node.keyFn(key as never);
  const nodeId = getNodeByKey(this._db, fullKey);
  if (nodeId === null) return null;
  return { $id: nodeId, $key: fullKey, $def: node } as NodeRef<N>;
}
```

**Files to modify:**
- `src/api/ray.ts`: Add `getRef()` method

**Estimated impact**: 10x improvement for cases where only the reference is needed.

### Solution 1.2: Cache key prefix → NodeDef mapping

Currently `getNodeDef()` iterates all definitions and calls `keyFn("test")` for each:

```ts
// Current: O(n) per call where n = number of node types
private getNodeDef(nodeId: NodeID): NodeDef | null {
  const key = getNodeKey(this._db._snapshot, this._db._delta, nodeId);
  if (key) {
    for (const nodeDef of this._nodes.values()) {
      const testKey = nodeDef.keyFn("test" as never);
      if (testKey.startsWith(keyPrefix)) {
        return nodeDef;
      }
    }
  }
  return first node def; // fallback
}
```

**Fix**: Build a prefix → NodeDef map once at initialization:

```ts
// In Ray constructor
private readonly _keyPrefixToNodeDef: Map<string, NodeDef>;

// During init
this._keyPrefixToNodeDef = new Map();
for (const nodeDef of nodes) {
  const testKey = nodeDef.keyFn("__test__" as never);
  const prefix = testKey.replace("__test__", "");
  this._keyPrefixToNodeDef.set(prefix, nodeDef);
}

// Fast lookup
private getNodeDef(nodeId: NodeID): NodeDef | null {
  const key = getNodeKey(...);
  if (!key) return this._nodes.values().next().value ?? null;
  
  // Find matching prefix
  for (const [prefix, def] of this._keyPrefixToNodeDef) {
    if (key.startsWith(prefix)) return def;
  }
  return this._nodes.values().next().value ?? null;
}
```

**Files to modify:**
- `src/api/ray.ts`: Add `_keyPrefixToNodeDef` map, update `getNodeDef()`

**Estimated impact**: 2-3x improvement for traversals that resolve node definitions.

---

## Phase 2: Traversal Optimization (Target: <5x overhead)

### Problem

`.count()` on traversals loads ALL node properties, edge properties, and creates objects for every result.

**Current code path** (`traversal.ts:185-230`):
```ts
function* executeSingleHop(...) {
  for (const edge of getNeighborsOut(db, node.$id, etypeId)) {
    // 1. Check node exists - REDUNDANT
    if (!nodeExists(db, neighborId)) continue;
    
    // 2. Resolve node definition - SLOW
    const neighborDef = getNodeDef(neighborId);
    
    // 3. Load ALL node properties - SLOW & UNNECESSARY for count()
    const props = loadNodeProps(neighborId, neighborDef);
    
    // 4. Load ALL edge properties - SLOW & UNNECESSARY for count()
    const edgeProps = loadEdgeProps(edge.src, etypeId, edge.dst, edgeDef);
    
    // 5. Create objects - UNNECESSARY for count()
    const neighborRef = createNodeRef(neighborDef, neighborId, "", props);
    const edgeResult = { $src, $dst, $etype, ...edgeProps };
    
    yield { node: neighborRef, edge: edgeResult };
  }
}
```

### Solution 2.1: Remove redundant `nodeExists()` check

The check at line 204 is redundant because `getNeighborsOut` already filters deleted edges:

```ts
// REMOVE THIS - getNeighborsOut already handles deletions
if (!nodeExists(db, neighborId)) continue;
```

**Files to modify:**
- `src/api/traversal.ts`: Remove `nodeExists()` check in `executeSingleHop()`

**Estimated impact**: 5-10% improvement.

### Solution 2.2: Add fast `countOnly` execution path

When only counting is needed, skip all property loading and object creation:

```ts
// In TraversalBuilder
async count(): Promise<number> {
  // Fast path: only count edges, don't load anything
  if (steps.length > 0 && !edgeFilter && !nodeFilter) {
    return this._countFast();
  }
  return this.nodes().count();
}

private _countFast(): number {
  let count = 0;
  let currentNodeIds: NodeID[] = startNodes.map(n => n.$id);
  
  for (const step of steps) {
    const etypeId = resolveEtypeId(step.edgeDef);
    const nextNodeIds: NodeID[] = [];
    
    for (const nodeId of currentNodeIds) {
      if (step.type === "out" || step.type === "both") {
        for (const edge of getNeighborsOut(db, nodeId, etypeId)) {
          nextNodeIds.push(edge.dst);
        }
      }
      if (step.type === "in" || step.type === "both") {
        for (const edge of getNeighborsIn(db, nodeId, etypeId)) {
          nextNodeIds.push(edge.src);
        }
      }
    }
    currentNodeIds = nextNodeIds;
  }
  
  return currentNodeIds.length;
}
```

**Files to modify:**
- `src/api/traversal.ts`: Add `_countFast()` method, modify `count()` to use it

**Estimated impact**: 10x improvement for `.count()` operations.

### Solution 2.3: Defer property loading until terminal operations need them

Restructure traversal to separate edge iteration from property loading:

```ts
// Internal: yields only IDs
function* iterateNeighborIds(nodeId, etypeId, direction): Generator<{neighborId, edge}> {
  for (const edge of getNeighborsOut(db, nodeId, etypeId)) {
    yield { neighborId: edge.dst, edge };
  }
}

// Load properties only when needed
function materializeNode(neighborId, edge, edgeDef): { node, edge } {
  const neighborDef = getNodeDef(neighborId);
  const props = loadNodeProps(neighborId, neighborDef);
  const edgeProps = loadEdgeProps(...);
  return { node: createNodeRef(...), edge: {...} };
}

// Terminal operations decide what to materialize
async toArray() {
  const results = [];
  for (const {neighborId, edge} of iterate()) {
    results.push(materializeNode(neighborId, edge, edgeDef));
  }
  return results;
}

async count() {
  let count = 0;
  for (const _ of iterate()) count++;
  return count;
}
```

**Files to modify:**
- `src/api/traversal.ts`: Major restructure of execution model

**Estimated impact**: 5-10x improvement for non-materializing operations.

### Solution 2.4: Use sync generators where possible

The underlying `getNeighborsOut` is synchronous. Convert inner loops to sync:

```ts
// Current (slow - async overhead)
async function* executeStep(currentNodes, step) {
  for await (const node of currentNodes) { ... }
}

// Optimized: sync inner loop
function* executeSingleHopSync(nodeId, direction, edgeDef, etypeId): Generator<...> {
  for (const edge of getNeighborsOut(db, nodeId, etypeId)) {
    yield { neighborId: edge.dst, edge };
  }
}
```

**Files to modify:**
- `src/api/traversal.ts`: Add sync execution path for single-hop

**Estimated impact**: 2-3x improvement by avoiding async iterator overhead.

---

## Phase 3: Structural Improvements

### Solution 3.1: Optimize `createNodeRef` object creation

```ts
// Current - object spread is expensive
return {
  $id: id,
  $key: key,
  $def: def,
  ...props,
} as NodeRef<N> & InferNode<N>;

// Optimized - use Object.assign
const ref = { $id: id, $key: key, $def: def };
if (props && Object.keys(props).length > 0) {
  Object.assign(ref, props);
}
return ref as NodeRef<N> & InferNode<N>;
```

**Files to modify:**
- `src/api/builders.ts`: Optimize `createNodeRef()`

**Estimated impact**: Minor (5-10% in hot paths).

### Solution 3.2: Cache propKeyIds at traversal builder level

```ts
// In createTraversalBuilder
const propKeyCache = new Map<NodeDef | EdgeDef, Map<string, PropKeyID>>();

function getCachedPropKeyId(def: NodeDef | EdgeDef, propName: string): PropKeyID {
  let defCache = propKeyCache.get(def);
  if (!defCache) {
    defCache = new Map();
    propKeyCache.set(def, defCache);
  }
  let keyId = defCache.get(propName);
  if (keyId === undefined) {
    keyId = resolvePropKeyId(def, propName);
    defCache.set(propName, keyId);
  }
  return keyId;
}
```

**Files to modify:**
- `src/api/traversal.ts`: Add per-traversal prop key cache

**Estimated impact**: Minor (5% for traversals with many properties).

---

## Phase 4: API Additions for Power Users

### Solution 4.1: Expose raw iterator for zero-copy traversal

```ts
// New method - returns raw edge iterator without any materialization
db.from(node).out(edge).rawEdges(): Generator<Edge>
```

**Files to modify:**
- `src/api/traversal.ts`: Add `rawEdges()` method

### Solution 4.2: Add `select()` for partial property loading

```ts
// Only load specified properties
const users = await db.from(alice)
  .out(knows)
  .select(['name'])  // Only load 'name', not all props
  .toArray();
```

**Files to modify:**
- `src/api/traversal.ts`: Add `select()` method and selective loading

---

## Implementation Order

| Priority | Task | Impact | Effort | Target |
|----------|------|--------|--------|--------|
| **1** | Remove `nodeExists()` check | High | Low | Phase 2.1 |
| **2** | Add fast `.count()` path | High | Medium | Phase 2.2 |
| **3** | Add `getRef()` method | Medium | Low | Phase 1.1 |
| **4** | Cache key prefix → NodeDef | Medium | Low | Phase 1.2 |
| **5** | Defer property loading | High | High | Phase 2.3 |
| **6** | Sync generators | Medium | Medium | Phase 2.4 |
| **7** | Cache propKeyIds | Low | Low | Phase 3.2 |
| **8** | Optimize createNodeRef | Low | Low | Phase 3.1 |
| **9** | Add `rawEdges()` | Low | Low | Phase 4.1 |
| **10** | Add `select()` | Low | Medium | Phase 4.2 |

---

## Target Performance

After optimization:

| Operation | Current | Target | Improvement |
|-----------|---------|--------|-------------|
| Key lookup | 13x | <3x | 4x faster |
| 1-hop traversal `.count()` | 16x | <3x | 5x faster |
| 1-hop traversal `.toArray()` | 16x | <5x | 3x faster |
| Insert | 1.01x | 1.01x | (no change needed) |

---

## Testing Strategy

1. **Benchmark after each phase** using `bench/benchmark-api-vs-raw.ts`
2. **Add micro-benchmarks** for individual optimizations
3. **Regression tests** to ensure correctness is maintained
4. **Memory profiling** to ensure no memory leaks from caching

---

## Open Questions

1. Should `getRef()` be a separate method or should `get()` accept an options parameter?
2. Is lazy property loading via Proxy acceptable, or too "magic"?
3. Should we expose `rawEdges()` or keep it internal?
4. What's the acceptable API surface increase?
