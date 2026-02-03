# Kite API - Quick Start Guide

> Note: The directory-based GraphDB (multi-file) engine has been removed. KiteDB is single-file (`.kitedb`) only; GraphDB references below are historical.

Get up and running with Kite in 5 minutes.

## Installation

```bash
bun add @kitedb/core
# or
npm add @kitedb/core
```

## Basic Setup

```typescript
import { kite, defineNode, defineEdge, string, int, float, bool, optional } from '@kitedb/core';

// 1. Define your schema
const user = defineNode('user', {
  key: (id: string) => `user:${id}`,
  props: {
    name: string('name'),
    email: string('email'),
    age: optional(int('age')),
  },
});

const company = defineNode('company', {
  key: (id: string) => `company:${id}`,
  props: {
    name: string('name'),
  },
});

const knows = defineEdge('knows', {
  since: int('since'),
});

const worksAt = defineEdge('worksAt');

// 2. Open database
const db = await kite('./my-database', {
  nodes: [user, company],
  edges: [knows, worksAt],
});

// 3. Create nodes
const alice = await db
  .insert(user)
  .values({
    key: 'alice',
    name: 'Alice',
    email: 'alice@example.com',
    age: 30,
  })
  .returning();

const bob = await db
  .insert(user)
  .values({
    key: 'bob',
    name: 'Bob',
    email: 'bob@example.com',
  })
  .returning();

// 4. Create relationships
await db.link(alice, knows, bob, { since: 2020 });

// 5. Query graph
const friends = await db
  .from(alice)
  .out(knows)
  .nodes()
  .toArray();

console.log('Alice knows:', friends.map(f => f.name));
// Output: Alice knows: [ 'Bob' ]

// 6. Close database
await db.close();
```

Note: This snippet uses top-level await (ESM). If your project is CommonJS or can’t enable top-level await, wrap it:

```typescript
async function main() {
  const db = await kite('./my-database', {
    nodes: [user, company],
    edges: [knows, worksAt],
  });
  // ...
  await db.close();
}

main().catch(console.error);
```

Or use the sync open:

```typescript
const db = kiteSync('./my-database', {
  nodes: [user, company],
  edges: [knows, worksAt],
});
// ...
db.close();
```

## Common Operations

### Insert Nodes

```typescript
// Single insert
const user1 = await db
  .insert(user)
  .values({ key: 'user1', name: 'User One', email: 'one@example.com' })
  .returning();

// Bulk insert
const [user2, user3] = await db
  .insert(user)
  .values([
    { key: 'user2', name: 'User Two', email: 'two@example.com' },
    { key: 'user3', name: 'User Three', email: 'three@example.com' },
  ])
  .returning();
```

### Get Node by Key

```typescript
const alice = await db.get(user, 'alice');
// Returns: { id: 1n, key: 'user:alice', name: 'Alice', email: '...', age: 30 }

if (!alice) {
  console.log('User not found');
}
```

### Update Node

```typescript
// By key
await db
  .update(user, 'alice')
  .setAll({ name: 'Alice Updated', age: 31 })
  .execute();

// Single-field update
db.update(user, 'alice').set('email', 'newemail@example.com').execute();
```

### Delete Node

```typescript
const deleted = db.delete(user, 'alice');
// Returns: true if deleted, false if not found
```

### Create Relationships (Edges)

```typescript
const alice = await db.get(user, 'alice');
const bob = await db.get(user, 'bob');

// Link with properties
await db.link(alice, knows, bob, { since: 2020 });

// Link without properties
await db.link(alice, worksAt, acme);

// Check if edge exists
const hasEdge = await db.hasEdge(alice, knows, bob);
```

### Remove Relationships

```typescript
await db.unlink(alice, knows, bob);
```

### Traverse the Graph

```typescript
const alice = await db.get(user, 'alice');

// Get immediate neighbors
const friends = await db
  .from(alice)
  .out(knows)
  .nodes()
  .toArray();

// Get friends of friends
const foaf = await db
  .from(alice)
  .out(knows)
  .out(knows)
  .nodes()
  .toArray();

// Bidirectional traversal
const connections = await db
  .from(alice)
  .both(knows)
  .nodes()
  .toArray();

// Variable-depth traversal
const nearby = await db
  .from(alice)
  .traverse(knows, {
    direction: 'out',
    maxDepth: 3,
    minDepth: 1,
    unique: true,
  })
  .nodes()
  .toArray();

// With filters
const youngFriends = await db
  .from(alice)
  .out(knows)
  .whereNode(n => (n.age ?? 0) < 35)
  .nodes()
  .toArray();

// Get edges instead of nodes
const knowsEdges = await db
  .from(alice)
  .out(knows)
  .edges()
  .toArray();

// First/count
const firstFriend = db.from(alice).out(knows).toArray()[0];
const friendCount = db.from(alice).out(knows).count();
```

### Transactions

```typescript
const result = await db.transaction(async (ctx) => {
  const alice = await ctx
    .insert(user)
    .values({
      key: 'alice',
      name: 'Alice',
      email: 'alice@example.com',
    })
    .returning();

  const bob = await ctx
    .insert(user)
    .values({
      key: 'bob',
      name: 'Bob',
      email: 'bob@example.com',
    })
    .returning();

  await ctx.link(alice, knows, bob, { since: 2020 });

  return { alice, bob };
});

// If any operation throws, entire transaction is rolled back
```

### Batch Operations

```typescript
const [result1, result2, result3] = await db.batch([
  db.insert(user).values({ key: 'u1', name: 'User 1', email: 'u1@example.com' }),
  db.insert(user).values({ key: 'u2', name: 'User 2', email: 'u2@example.com' }),
  db.insert(company).values({ key: 'acme', name: 'ACME Corp' }),
]);
```

## Tips & Tricks

### Property Types

Property builders are available as top-level exports or under `prop`:

- `string()` / `prop.string()` → `string`
- `int()` / `prop.int()` → `number` (stored as 64-bit signed)
- `float()` / `prop.float()` → `number` (f64)
- `bool()` / `prop.bool()` → `boolean`

Optional properties:

```typescript
const age = optional(int('age'));
```

### Keys

Keys are how you look up nodes. Design them carefully:

```typescript
// Good: hierarchical, human-readable
const user = defineNode('user', {
  key: (id: string) => `user:${id}`,
});

const comment = defineNode('comment', {
  key: (id: number) => `comment:${id}`,
});

const postComment = defineNode('postComment', {
  key: (postId: number, commentId: number) => `post:${postId}:comment:${commentId}`,
});
```

### Escape Hatch

If you need low-level access:

```typescript
const rawDb: GraphDB = db.$raw;
// Use raw graph-db.ts API
```

### Maintenance

```typescript
// Get stats
const stats = await db.stats();
console.log('Nodes:', stats.snapshotNodes);
console.log('Edges:', stats.snapshotEdges);
console.log('Should compact:', stats.recommendCompact);

// Check integrity
const check = await db.check();
if (!check.valid) {
  console.error('Database errors:', check.errors);
}

// Compact (merge deltas into snapshots)
await db.optimize();

// Close
await db.close();
```

## Common Patterns

### Many-to-Many Relationship

```typescript
const person = defineNode('person', {
  key: (id: string) => `person:${id}`,
  props: { name: string('name') },
});

const project = defineNode('project', {
  key: (id: string) => `project:${id}`,
  props: { title: string('title') },
});

const contributesTo = defineEdge('contributesTo', {
  role: string('role'),
  hours: float('hours'),
});

// Create relationships
const alice = await db.get(person, 'alice');
const projectA = await db.get(project, 'projectA');
const projectB = await db.get(project, 'projectB');

await db.link(alice, contributesTo, projectA, { role: 'lead', hours: 40.0 });
await db.link(alice, contributesTo, projectB, { role: 'contributor', hours: 20.0 });

// Query
const projects = await db
  .from(alice)
  .out(contributesTo)
  .nodes()
  .toArray();

const contributorCount = await db
  .from(projectA)
  .in(contributesTo)
  .count();
```

### Hierarchical Structure

```typescript
const category = defineNode('category', {
  key: (id: string) => `category:${id}`,
  props: { name: string('name') },
});

const parentOf = defineEdge('parentOf');

const root = await db.get(category, 'root');
const allDescendants = await db
  .from(root)
  .traverse(parentOf, { direction: 'out', maxDepth: 10 })
  .nodes()
  .toArray();
```

### Time-Indexed Relationships

```typescript
const event = defineEdge('event', {
  timestamp: int('timestamp'),
});

const recentEvents = await db
  .from(node)
  .out(event)
  .whereEdge(e => e.timestamp > Date.now() - 86400000) // Last 24h
  .nodes()
  .toArray();
```

## Error Handling

```typescript
try {
  const result = await db.transaction(async (ctx) => {
    // Operations...
  });
} catch (e) {
  // Transaction automatically rolled back
  console.error('Transaction failed:', e);
}

// Keys & IDs
const node = await db.get(user, 'alice'); // Returns null if not found
if (!node) {
  console.log('User not found');
}
```

## Performance Tips

1. **Use batches** for bulk inserts
2. **Use transactions** for atomicity
3. **Filter early** in traversals to reduce memory
4. **Call `optimize()`** periodically
5. **Close connections** with `db.close()` when done
6. **Use `unique: true`** (default) in variable-depth traversals

## See Also

- [Full API Documentation](./README.md)
- [Architecture Overview](../API.md)
- [Type Inference Guide](#type-inference)

## Type Inference

The API automatically infers correct types:

```typescript
const user = defineNode('user', {
  key: (id: string) => `user:${id}`,
  props: {
    name: string('name'),
    age: optional(int('age')),
  },
});

// Insert type (what you pass to .values())
type InsertUser = InferNodeInsert<typeof user>;
// { key: string; name: string; age?: number; }

// Return type (what you get back)
type User = InferNode<typeof user>;
// { id: number; key: string; name: string; age?: number; }

// Edge props
const knows = defineEdge('knows', { since: int('since') });
type KnowsProps = InferEdgeProps<typeof knows>;
// { since: number; }
```

## Troubleshooting

**Q: "Unknown edge type" error**
A: Make sure you passed all edge definitions to `kite()` options

**Q: Node not found in updates**
A: Updates by key throw if the node doesn't exist. Use `get()` first if unsure

**Q: Type errors with properties**
A: Properties are stored as `number` (int/float), `string`, or `boolean`. No `Date` objects

**Q: Traversal seems slow**
A: Call `optimize()` to compact snapshots. Check `stats()` to see if you need compaction
