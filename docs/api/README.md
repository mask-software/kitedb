# Kite High-Level API

> Note: The directory-based GraphDB (multi-file) engine has been removed. KiteDB is single-file (`.kitedb`) only; GraphDB references below are historical.

The `api` module provides a **Drizzle-style, type-safe API** for Kite, a high-performance embedded graph database. It wraps the lower-level database primitives with a fluent, ergonomic interface featuring full TypeScript type inference.

## Overview

The API consists of five main modules:

- **`kite.ts`** - Main entry point and database context
- **`schema.ts`** - Schema definition with property and relation types
- **`builders.ts`** - Query builders for insert, update, delete operations
- **`traversal.ts`** - Graph traversal with filtering and aggregation
- **`index.ts`** - Barrel export of public types and functions

## Key Concepts

### Schema-First Design

Define your graph structure upfront using `node()` and `edge()`:

```typescript
import { node, edge, string, int, float, bool, optional } from "@kitedb/core";

// Define node types
const user = node("user", {
  key: (id: string) => `user:${id}`,
  props: {
    name: string("name"),
    email: string("email"),
    age: optional(int("age")),
  },
});

const company = node("company", {
  key: (id: string) => `company:${id}`,
  props: {
    name: string("name"),
  },
});

// Define edge types
const knows = edge("knows", {
  since: int("since"),
  strength: optional(float("strength")),
});

const worksAt = edge("worksAt", {
  startDate: int("startDate"),
});
```

### Type Inference

The API uses advanced TypeScript inference to automatically derive types:

- `InferNodeInsert<N>` - Type for inserting a node
- `InferNodeUpsert<N>` - Type for upserting a node (partial props)
- `InferNode<N>` - Type for returned nodes (includes `id` and `key`)
- `InferEdgeProps<E>` - Type for edge properties

```typescript
type InsertUser = InferNodeInsert<typeof user>;
// Result: { key: string; name: string; email: string; age?: number; }

type ReturnedUser = InferNode<typeof user>;
// Result: { id: number; key: string; name: string; email: string; age?: number; }
```

## Module Reference

### `kite.ts` - Main Database API

#### `kite(path, options): Promise<Kite>`

Opens or creates a database with the given schema.

**Parameters:**

- `path: string` - Directory path for database files
- `options: KiteOptions` - Schema and database options
  - `nodes: NodeDef[]` - Node type definitions
  - `edges: EdgeDef[]` - Edge type definitions
  - `readOnly?: boolean` - Open in read-only mode
  - `createIfMissing?: boolean` - Create if doesn't exist (default: true)
  - `lockFile?: boolean` - Use file locking (default: true)

**Example:**

```typescript
const db = await kite("./my-graph", {
  nodes: [user, company],
  edges: [knows, worksAt],
});
```

#### `Kite` Class

The main database context with methods for CRUD operations, transactions, and maintenance.

**Node Operations:**

- `insert<N>(node: N): InsertBuilder<N>` - Insert one or more nodes
- `upsert<N>(node: N): UpsertBuilder<N>` - Insert or update nodes by key
- `update<N>(node: N, key: any): UpdateBuilder` - Update a node by key
- `delete<N>(node: N, key: any): boolean` - Delete a node by key
- `get<N>(node: N, key: any): InferNode<N> | null` - Look up node by key
- `getRef<N>(node: N, key: any): NodeRef<N> | null` - Lightweight reference lookup
- `all<N>(nodeDef: N): Array<InferNode<N>>` - List all nodes of type
- `countNodes<N>(nodeDef?: N): number` - Count nodes (optionally filtered by type)

**Edge Operations:**

- `link<E>(src: NodeRef, edge: E, dst: NodeRef, props?): void` - Create edge (direct)
- `link(src).to(dst).via(edge).props(...).execute()` - Fluent edge builder
- `unlink<E>(src: NodeRef, edge: E, dst: NodeRef): boolean` - Delete edge
- `hasEdge<E>(src: NodeRef, edge: E, dst: NodeRef): boolean` - Check edge exists
- `updateEdge<E>(src, edge, dst): UpdateEdgeBuilder<E>` - Update edge properties
- `allEdges<E>(edgeDef?: E): Array<JsFullEdge>` - List all edges (optionally filtered by type)
- `countEdges<E>(edgeDef?: E): number` - Count edges (optionally filtered by type)

**Traversal:**

- `from<N>(node: NodeRef<N> | number): KiteTraversal` - Start traversal from a node

**Batch Operations:**

- `batch<T>(operations): Results[]` - Execute multiple ops in a single transaction (sync only)

**Transactions:**

- `transaction<T>(fn): T | Promise<T>` - Execute operations in explicit transaction

**Maintenance:**

- `stats(): Promise<DbStats>` - Get database statistics
- `check(): Promise<CheckResult>` - Verify database integrity
- `optimize(): Promise<void>` - Compact snapshots with deltas
- `close(): Promise<void>` - Close database
- `$raw: GraphDB` - Escape hatch to raw database

### `schema.ts` - Schema Definition

#### Property Types

```typescript
string(name: string): PropBuilder<"string">
int(name: string): PropBuilder<"int">       // Stored as 64-bit signed (number)
float(name: string): PropBuilder<"float">   // f64
bool(name: string): PropBuilder<"bool">
```

Property builders are available as top-level exports or under `prop` (e.g. `string()` / `prop.string()`).
Use the `optional(...)` helper for optional properties:

```typescript
const email = optional(string("email"));
```

#### `node<Name, KeyArg, Props>(name, config): NodeDef`

Defines a node type with schema.

**Parameters:**

- `name: string` - Node type name
- `config: NodeConfig<KeyArg, Props>`
  - `key: (id: KeyArg) => string` - Key generation function
  - `props: PropsSchema` - Property definitions

```typescript
const user = node("user", {
  key: (id: string) => `user:${id}`,
  props: {
    name: string("name"),
    email: string("email"),
  },
});
```

#### `edge<Name, Props>(name, props?): EdgeDef`

Defines an edge type with optional properties.

```typescript
// Edge with properties
const knows = edge("knows", {
  since: int("since"),
  weight: optional(float("weight")),
});

// Edge without properties
const follows = edge("follows");
```

### `builders.ts` - Query Builders

#### Insert

```typescript
// Single insert
const alice = await db
  .insert(user)
  .values({ key: "alice", name: "Alice", email: "alice@example.com" })
  .returning();

// Bulk insert
const [bob, charlie] = await db
  .insert(user)
  .values([
    { key: "bob", name: "Bob", email: "bob@example.com" },
    { key: "charlie", name: "Charlie", email: "charlie@example.com" },
  ])
  .returning();

// Insert without returning
await db
  .insert(user)
  .values({ key: "dave", name: "Dave", email: "dave@example.com" })
  .execute();
```

#### Upsert

```typescript
// Insert or update by key (partial updates allowed)
const alice = await db
  .upsert(user)
  .values({ key: "alice", email: "alice@new.com" })
  .returning();
```

#### Update by Key

```typescript
await db
  .update(user, "alice")
  .setAll({ name: "Alice Updated", email: "newemail@example.com" })
  .execute();
```

#### Delete by Key

```typescript
const deleted = db.delete(user, "alice");
```

#### Link (Create Edge)

Direct `db.link(src, edge, dst, props)` is the fastest path; the fluent builder is more ergonomic.

```typescript
const alice = await db.get(user, "alice");
const bob = await db.get(user, "bob");

await db.link(alice, knows, bob, { since: 2020 });

// Fluent edge builder (DX-friendly)
await db.link(alice).to(bob).via(knows).props({ since: 2020 }).execute();
```

#### Unlink (Delete Edge)

```typescript
await db.unlink(alice, knows, bob);
```

#### Update Edge

```typescript
const edge = await db.updateEdge(alice, knows, bob);
await edge.setAll({ weight: 0.95 }).execute();
```

### `traversal.ts` - Graph Traversal

#### TraversalBuilder

Start traversal from a node and chain operations:

```typescript
const alice = await db.get(user, "alice");

// Get all friends of Alice
const friends = await db.from(alice).out(knows).nodes().toArray();

// Get friends' friends (depth 2)
const foaf = await db.from(alice).out(knows).out(knows).nodes().toArray();

// Multi-directional traversal
const connections = await db.from(alice).both(knows).nodes().toArray();

// Variable-depth traversal with limits
const nearby = await db
  .from(alice)
  .traverse(knows, { direction: "out", maxDepth: 3, unique: true })
  .nodes()
  .toArray();
```

#### Filtering

```typescript
// Filter by edge properties
await db
  .from(alice)
  .out(knows)
  .whereEdge((edge) => edge.since < 2020)
  .nodes()
  .toArray();

// Filter by node properties
await db
  .from(alice)
  .out(knows)
  .whereNode((node) => node.age > 25)
  .nodes()
  .toArray();
```

#### Results

Traversal results are returned as arrays:

```typescript
const all = db.from(alice).out(knows).nodes().toArray();
const first = all[0];
const count = db.from(alice).out(knows).count();
```

#### Edges

Get edges instead of nodes:

```typescript
const edges = await db.from(alice).out(knows).edges().toArray();

// Each edge has: src, dst, etype (numeric IDs)
for (const edge of edges) {
  console.log(`${edge.src} --${edge.etype}--> ${edge.dst}`);
}
```

### Listing and Counting

List and count all nodes or edges with optional type filtering:

```typescript
// List all users
const users = db.all(user);
for (const u of users) {
  console.log(u.name, u.key);
}

// Count all nodes in database (fast - O(1) when no filter)
const totalNodes = db.countNodes();

// Count users only
const userCount = db.countNodes(user);

// List all edges (IDs only)
for (const edge of db.allEdges()) {
  console.log(`${edge.src} -> ${edge.dst}`);
}

// List only "knows" edges
for (const edge of db.allEdges(knows)) {
  console.log(`${edge.src} knows ${edge.dst}`);
}

// Count all edges
const totalEdges = db.countEdges();

// Count edges of specific type
const knowsCount = db.countEdges(knows);
```

## Advanced Patterns

### Transactions

Execute multiple operations atomically:

```typescript
const result = await db.transaction(async (ctx) => {
  // All operations in this callback are in a single transaction
  const alice = await ctx
    .insert(user)
    .values({ key: "alice", name: "Alice" })
    .returning();
  const bob = await ctx
    .insert(user)
    .values({ key: "bob", name: "Bob" })
    .returning();

  await ctx.link(alice, knows, bob);

  return { alice, bob };
});
```

### Batch Operations

Combine multiple operations into one transaction:

```typescript
const [result1, result2, result3] = await db.batch([
  db.insert(user).values({ key: "user1", name: "User 1" }),
  db.insert(user).values({ key: "user2", name: "User 2" }),
  db.insert(company).values({ key: "acme", name: "ACME Corp" }),
]);
```

### Key Lookups

Keys are the primary way to look up nodes:

```typescript
// Define key schema
const user = node("user", {
  key: (id: string) => `user:${id}`,
  // ...
});

// Later, look up by key
const alice = await db.get(user, "alice"); // Uses key 'user:alice' internally
```

### Property Type Conversion

Properties follow JS value types at runtime. Schema types are used for inference:

- **string** → `string`
- **int** → `number` (stored as 64-bit signed when passed as `bigint`)
- **float** → `number` (f64)
- **bool** → `boolean`

```typescript
const user = node("user", {
  key: (id: string) => `user:${id}`,
  props: {
    age: int("age"), // Stored as i64 when passed as bigint
    score: float("score"), // Stored as f64
  },
});

const alice = await db
  .insert(user)
  .values({
    key: "alice",
    age: 30, // number (use bigint for lossless i64)
    score: 95.5,
  })
  .returning();

console.log(typeof alice.age); // 'number'
console.log(typeof alice.score); // 'number'
```

## Error Handling

Most operations throw on error. Key error cases:

- **Node not found** - `get()` returns `null`, and `update(node, key)` throws if key is missing
- **Type mismatch** - Values are stored based on JS types; schema types are not enforced at runtime
- **Transaction rollback** - Errors in `transaction()` or `batch()` automatically rollback

```typescript
try {
  await db.transaction(async (ctx) => {
    const user = await ctx
      .insert(user)
      .values({
        /* ... */
      })
      .returning();
    throw new Error("Oops!");
  });
} catch (e) {
  // Transaction is automatically rolled back
}
```

## Performance Tips

1. **Use batches** for bulk operations - they're in a single transaction
2. **Use transactions** instead of separate operations for atomicity
3. **Filter early** in traversals to reduce memory usage
4. **Use unique: true** (default) in variable-depth traversals to avoid revisiting nodes
5. **Call `optimize()`** periodically to merge deltas into snapshots
6. **Close connections** with `db.close()` to flush writes

## Comparison with Lower-Level API

The high-level API (`src/api/`) is built on top of the lower-level database API (`src/db/`):

| Task         | Low-Level                             | High-Level                 |
| ------------ | ------------------------------------- | -------------------------- |
| Define types | Manual IDs                            | `node`, `edge`             |
| Type safety  | Manual                                | Full TypeScript inference  |
| Insert nodes | `createNode()` + `setNodeProp()`      | `insert().values()`        |
| Query nodes  | Raw ID lookups                        | `get()`, `traversals`      |
| Transactions | `beginTx()`, `commit()`, `rollback()` | `transaction()`, `batch()` |

The low-level API is useful when you need escape-hatch access via `db.$raw`.
