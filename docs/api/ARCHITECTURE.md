# Kite API - Architecture & Design

> Note: The directory-based GraphDB (multi-file) engine has been removed. KiteDB is single-file (`.kitedb`) only; GraphDB references below are historical.

This document describes the architecture and design decisions of the Kite high-level API.

## Overview

The Kite API is a **type-safe, schema-first wrapper** around a low-level embedded graph database. It provides a Drizzle-like developer experience with full TypeScript type inference.

## Design Philosophy

1. **Schema-First**: Define types upfront with `defineNode` and `defineEdge`
2. **Type-Safe**: Full TypeScript inference for all operations
3. **Ergonomic**: Fluent builder pattern for queries
4. **Composable**: Chain operations naturally
5. **Zero-Cost**: Abstraction doesn't sacrifice performance
6. **Escapable**: Access raw database when needed via `$raw`

## Core Concepts

### 1. Schema Layer (`schema.ts`)

The schema layer defines the structure of your graph:

**Property Types:**
```typescript
string('name')     // UTF-8 strings
int('age')         // 64-bit integers (number)
float('score')     // 64-bit floats
bool('active')     // Booleans
```
Builders are also available under `prop` if you prefer namespacing (e.g. `prop.string('name')`).

**Node Definitions:**
```typescript
const user = defineNode('user', {
  key: (id: string) => `user:${id}`,  // Key generation
  props: {
    name: string('name'),
    email: string('email'),
    age: optional(int('age')),   // Optional property
  },
});
```

**Edge Definitions:**
```typescript
const knows = defineEdge('knows', {
  since: int('since'),
  confidence: optional(float('confidence')),
});

const follows = defineEdge('follows');  // Edge without properties
```

**Type Inference:**

The schema layer uses advanced TypeScript generics to automatically infer:

- `InferNodeInsert<N>` - What you pass to `insert().values()`
- `InferNode<N>` - What you get back from queries
- `InferEdgeProps<E>` - Edge properties type

```typescript
type InsertUser = InferNodeInsert<typeof user>;
// Result: { key: string; name: string; email: string; age?: number; }

type User = InferNode<typeof user>;
// Result: { id: number; key: string; name: string; email: string; age?: number; }
```

### 2. Database Context (`kite.ts`)

The `Kite` class is the main interface for all operations:

**Initialization:**
```typescript
const db = await kite('./my-db', {
  nodes: [user, company],
  edges: [knows, worksAt],
});
```

The database handles:
- Schema registration with the underlying database
- Transaction management
- Property key ID resolution
- Edge type ID resolution
- Node ref creation with properties

**Key Responsibilities:**

1. **Schema Resolution**: Maps user-defined types to internal IDs
2. **Transaction Context**: Provides `transaction()` and `batch()` APIs
3. **CRUD Builders**: Returns appropriate builders for operations
4. **Traversal**: Creates traversal builders starting from nodes
5. **Maintenance**: Stats, check, optimize operations

### 3. Query Builders (`builders.ts`)

Builders implement the fluent API pattern:

**Insert Builder Chain:**
```
Insert → values() → InsertExecutor → returning() / execute()
```

**Update Builder Chain:**
```
Update → set() → UpdateExecutor → where() → execute()
```

**Delete Builder Chain:**
```
Delete → where() → DeleteExecutor → execute()
```

**Link/Unlink:**
```
Direct methods on Kite (no builder needed)
```

**Design Notes:**

- Builders are chainable and return new instances
- Batch operations execute a list of builder operations synchronously

### 4. Traversal (`traversal.ts`)

Traversal returns arrays (with `.toArray()` helper methods):

**Builder Chaining:**
```
from(node) → out(edge) → whereNode() → take(n) → nodes()
                                                 ↓
                                           Array results
```

**Step Types:**
- `out(edge)` - Follow outgoing edges
- `in(edge)` - Follow incoming edges
- `both(edge)` - Follow both directions
- `traverse(edge, options)` - Variable-depth BFS

**Filtering:**
- `whereEdge(predicate)` - Filter by edge properties
- `whereNode(predicate)` - Filter by node properties
- `take(limit)` - Limit results

**Results:**
- `nodes()` - Return nodes as an array (with `.toArray()` helper)
- `edges()` - Return edges as an array (IDs only)
- `count()` - Count results
- `toArray()` - Collect node results

**Implementation Details:**

- Uses BFS (breadth-first search) for variable-depth traversal
- Arrays for in-process traversal results
- Optional deduplication with `unique` flag
- Supports early termination with limit/take

## Data Flow

### Insert Operation

```
User calls: db.insert(user).values({...}).returning()
                ↓
createInsertBuilder() returns InsertBuilder
                ↓
.values() returns InsertExecutor with lazy execute function
                ↓
.returning() calls execute() which:
  1. createNode() for each value
  2. setNodeProp() for each property
  3. commit() transaction
  4. Return NodeRef objects
```

### Traversal Operation

```
User calls: db.from(alice).out(knows).nodes().toArray()
                ↓
createTraversalBuilder() sets up execution state
                ↓
.out(knows) pushes traversal step
                ↓
.nodes() returns AsyncTraversalResult with execute generator
                ↓
.toArray() iterates through generator which:
  1. Starts with startNodes
  2. Applies each step via executeStep()
  3. Filters results with whereEdge/whereNode
  4. Applies limit with take()
  5. Yields results
```

## Property Type System

```
Kite Type           TypeScript Type    Storage        Tag
─────────────────────────────────────────────────────────
string()      string             String         4
int()         number             i64            2
float()       number             f64            3
bool()        boolean            bool           1
```

When inserting:
```typescript
await db.insert(user).values({
  key: 'alice',
  name: 'Alice',        // string → stored as STRING
  age: 30,              // number (use bigint for lossless i64)
  score: 95.5,          // number → stored as F64
  active: true,         // boolean → stored as BOOL
});
```

When reading:
```typescript
const alice = await db.get(user, 'alice');
typeof alice.name;      // 'string'
typeof alice.age;       // 'number'
typeof alice.score;     // 'number'
typeof alice.active;    // 'boolean'
```

## Key Design Patterns

### 1. NodeRef Pattern

```typescript
export interface NodeRef<N extends NodeDef = NodeDef> {
  readonly id: NodeID;      // Internal numeric ID
  readonly key: string;     // Application key
  readonly def: N;          // Node definition
  [key: string]: unknown;    // Properties
}
```

NodeRefs combine:
- System fields (ID, key, definition)
- Properties from the node
- Type information for type safety

### 2. Builder Pattern

All query builders follow this pattern:

```typescript
interface SomeBuilder {
  config(): SomeExecutor;    // Set configuration
}

interface SomeExecutor {
  execute(): Promise<void>;  // Execute and get result
  _toBatchOp(): BatchOperation;  // For batching
}
```

Benefits:
- Flexible configuration
- Lazy execution
- Support for transactions
- Batch operation composition

### 3. Where Conditions

Flexible WHERE matching:

```typescript
type WhereCondition = { key: string } | { id: NodeID };
```

Allows:
- `where({ key: 'user:alice' })` - String lookup
- `where({ id: 123n })` - Direct ID (from NodeRef)

### 4. Escape Hatch

```typescript
const raw: GraphDB = db.$raw;
```

Provides:
- Full access to low-level API
- Direct transaction handling
- Numeric ID operations
- When the high-level API doesn't fit

## Transaction Model

**Single-Writer Pattern:**
- Only one transaction at a time per database
- Other operations wait for transaction to complete
- Lock is managed by underlying GraphDB

**Automatic Rollback:**
```typescript
try {
  await db.transaction(async (ctx) => {
    // ... operations ...
    throw new Error('Fail');
  });
} catch (e) {
  // Transaction automatically rolled back
}
```

**Batch vs Transaction:**
- `batch()` - Multiple operations in one transaction
- `transaction()` - Full control with async callback

## Error Handling

| Scenario | Behavior | Throws |
|----------|----------|--------|
| Node not found with get() | Returns null | No |
| Node not found with where() | Operation skipped | No |
| Update/delete has no matches | Operation skipped | No |
| Invalid property type | Throws | Yes |
| Unknown edge type | Throws | Yes |
| Transaction error | Rollback | Yes |
| Disk error | Throws | Yes |

## Performance Characteristics

| Operation | Complexity | Notes |
|-----------|-----------|-------|
| Insert node | O(1) | WAL append |
| Create edge | O(log n) | After compaction |
| Key lookup | O(1) | Hash index |
| Edge exists? | O(log n) | Binary search |
| Traversal | O(k) | k = edges touched |
| Read snapshot | O(1) | Zero-copy mmap |

## Comparison with Other APIs

### vs. Raw GraphDB (`src/db/`)

| Feature | Kite API | Raw GraphDB |
|---------|-------------|-----------|
| Type safety | Full | Manual |
| Schema | Defined | Implicit |
| Property types | Inferred | Manual tags |
| Key lookup | `get(node, id)` | `getNodeByKey()` |
| Builders | Yes | No |
| Ergonomics | High | Low |

**When to use Kite API:**
- Building applications
- Need type safety
- Want ergonomic API

**When to use Raw GraphDB:**
- Building frameworks
- Need maximum control
- Working with IDs directly

### vs. Other Databases

| Aspect | Kite | SQL | NoSQL | Neo4j |
|--------|----------|-----|-------|-------|
| Embedded | Yes | No | Sometimes | No |
| Graph-first | Yes | No | No | Yes |
| Type safety | Strong | Varies | Weak | Weak |
| Schema | Required | Required | Optional | Optional |
| Performance | Fast reads | Varies | Fast | Slower |

## Future Considerations

Several items from the original roadmap (MVCC, pathfinding, and the caching layer) are now implemented and documented elsewhere — see [Kite README](../../README.md) for the MVCC, pathfinding, and caching sections, and [API architecture](../API.md) for MVCC layer details. This section tracks only features that remain future work.

1. **Advanced Traversal**
   - Subgraph matching
   - GQL query language

2. **Advanced Schema**
   - Constraints (unique, foreign keys)
   - Validation functions
   - Schema versioning

3. **Distributed**
   - Multi-node replication
   - Sharding support
