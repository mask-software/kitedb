# kitedb

KiteDB native bindings for Node.js (and WASI/browser builds), powered by Rust + N-API.

Docs: https://ray-kwaf.vercel.app/docs

## Install

```bash
npm install kitedb
# or
pnpm add kitedb
# or
yarn add kitedb
```

This package ships prebuilt binaries for major platforms. If a prebuild isn't available for your target, you'll need a Rust toolchain to build from source.

## Quickstart (fluent API)

The fluent API provides a high-level, type-safe interface for schema-driven workflows:

```ts
import { ray, node, edge, prop, optional } from 'kitedb'

// Define your schema
const User = node('user', {
  key: (id: string) => `user:${id}`,
  props: {
    name: prop.string('name'),
    email: prop.string('email'),
    age: optional(prop.int('age')),
  },
})

const Knows = edge('knows', {
  since: prop.int('since'),
})

// Open database (async)
const db = await ray('./social.kitedb', {
  nodes: [User],
  edges: [Knows],
})

// Insert nodes
const alice = db.insert(User).values({ key: 'alice', name: 'Alice', email: 'alice@example.com' }).returning()
const bob = db.insert(User).values({ key: 'bob', name: 'Bob', email: 'bob@example.com' }).returning()

// Create edges
db.link(alice, Knows, bob, { since: 2024 })

// Traverse
const friends = db.from(alice).out(Knows).toArray()

// Pathfinding
const path = db.shortestPath(alice).via(Knows).to(bob).dijkstra()

db.close()
```

## Quickstart (low-level API)

For direct control, use the low-level `Database` class:

```ts
import { Database, JsTraversalDirection, PropType, pathConfig, traversalStep } from 'kitedb'

const db = Database.open('example.kitedb', { createIfMissing: true })

// Transactions are explicit for write operations
db.begin()
const alice = db.createNode('user:alice')
const bob = db.createNode('user:bob')

const knows = db.getOrCreateEtype('knows')
const weight = db.getOrCreatePropkey('weight')

db.addEdge(alice, knows, bob)

// Set a typed edge property
db.setEdgeProp(alice, knows, bob, weight, {
  propType: PropType.Int,
  intValue: 1,
})

db.commit()

// Traverse
const oneHop = db.traverseSingle([alice], JsTraversalDirection.Out, knows)
console.log(oneHop)

// Multi-hop traversal
const steps = [traversalStep(JsTraversalDirection.Out, knows), traversalStep(JsTraversalDirection.Out, knows)]
const twoHop = db.traverse([alice], steps)
console.log(twoHop)

// Pathfinding
const config = pathConfig(alice, bob)
config.allowedEdgeTypes = [knows]
const shortest = db.bfs(config)
console.log(shortest)

db.close()
```

## Backups and health checks

```ts
import { createBackup, restoreBackup, healthCheck } from 'kitedb'

const backup = createBackup(db, 'backups/graph')
const restoredPath = restoreBackup(backup.path, 'restored/graph')

const health = healthCheck(db)
console.log(health.healthy)
```

## Vector search

```ts
import { createVectorIndex } from 'kitedb'

const index = createVectorIndex({ dimensions: 3 })
index.set(1, [0.1, 0.2, 0.3])
index.set(2, [0.1, 0.25, 0.35])
index.buildIndex()

const hits = index.search([0.1, 0.2, 0.3], { k: 5 })
console.log(hits)
```

## Browser/WASI builds

This package exposes a WASI-compatible build via the `browser` export for bundlers, backed by `kitedb-wasm32-wasi`. If you need to import it directly:

```ts
import { Database } from 'kitedb-wasm32-wasi'
```

## Concurrent Access

KiteDB supports concurrent read operations. Multiple async calls can read from the database simultaneously without blocking each other:

```ts
// These execute concurrently - reads don't block each other
const [user1, user2, user3] = await Promise.all([db.get(User, 'alice'), db.get(User, 'bob'), db.get(User, 'charlie')])

// Traversals can also run concurrently
const [aliceFriends, bobFriends] = await Promise.all([
  db.from(alice).out(Knows).toArray(),
  db.from(bob).out(Knows).toArray(),
])
```

**Concurrency model:**

- **Reads are concurrent**: Multiple `get()`, `from()`, `traverse()`, etc. can run in parallel
- **Writes are exclusive**: Write operations (`insert()`, `link()`, `update()`) require exclusive access
- **Read-write interaction**: A write will wait for in-progress reads to complete, then block new reads until done

This is implemented using a read-write lock (RwLock) internally, providing good read scalability while maintaining data consistency.

## API surface

The Node bindings expose both low-level graph primitives (`Database`) and higher-level APIs (Ray) for schema-driven workflows, plus metrics, backups, traversal, and vector search. For full API details and guides, see the docs:

https://ray-kwaf.vercel.app/docs

## License

MIT

# trigger
