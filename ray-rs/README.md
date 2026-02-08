# kitedb

KiteDB native bindings for Node.js (and WASI/browser builds), powered by Rust + N-API.

Docs: https://kitedb.vercel.com/docs

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
import { kite, node, edge, prop, optional } from 'kitedb'

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
const db = await kite('./social.kitedb', {
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

Note: The example uses top-level await (ESM). If your project is CommonJS or can’t enable top-level await, wrap it:

```ts
async function main() {
  const db = await kite('./social.kitedb', { nodes: [User], edges: [Knows] })
  // ...
  db.close()
}

main().catch(console.error)
```

Or use the sync open:

```ts
const db = kiteSync('./social.kitedb', { nodes: [User], edges: [Knows] })
// ...
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

## Replication Admin (low-level API)

Phase D replication controls are available on the low-level `Database` API.

```ts
import { Database } from 'kitedb'
import {
  collectReplicationLogTransportJson,
  collectReplicationMetricsOtelJson,
  collectReplicationMetricsOtelProtobuf,
  collectReplicationMetricsPrometheus,
  collectReplicationSnapshotTransportJson,
  createReplicationTransportAdapter,
  pushReplicationMetricsOtelGrpc,
  pushReplicationMetricsOtelGrpcWithOptions,
  pushReplicationMetricsOtelJson,
  pushReplicationMetricsOtelJsonWithOptions,
  pushReplicationMetricsOtelProtobuf,
  pushReplicationMetricsOtelProtobufWithOptions,
} from 'kitedb/native'

const primary = Database.open('cluster-primary.kitedb', {
  replicationRole: 'Primary',
  replicationSidecarPath: './cluster-primary.sidecar',
  replicationSegmentMaxBytes: 64 * 1024 * 1024,
  replicationRetentionMinEntries: 1024,
})

primary.begin()
primary.createNode('n:1')
const token = primary.commitWithToken()

primary.primaryReportReplicaProgress('replica-a', 1, 42)
const retention = primary.primaryRunRetention()
const primaryStatus = primary.primaryReplicationStatus()

const replica = Database.open('cluster-replica.kitedb', {
  replicationRole: 'Replica',
  replicationSidecarPath: './cluster-replica.sidecar',
  replicationSourceDbPath: 'cluster-primary.kitedb',
  replicationSourceSidecarPath: './cluster-primary.sidecar',
})

replica.replicaBootstrapFromSnapshot()
replica.replicaCatchUpOnce(256)
if (token) replica.waitForToken(token, 2_000)
const replicaStatus = replica.replicaReplicationStatus()
if (replicaStatus?.needsReseed) replica.replicaReseedFromSnapshot()

const prometheus = collectReplicationMetricsPrometheus(primary)
console.log(prometheus)

const otelJson = collectReplicationMetricsOtelJson(primary)
console.log(otelJson)

const otelProtobuf = collectReplicationMetricsOtelProtobuf(primary)
console.log(otelProtobuf.length)

const exportResult = pushReplicationMetricsOtelJson(
  primary,
  'http://127.0.0.1:4318/v1/metrics',
  5_000,
)
console.log(exportResult.statusCode, exportResult.responseBody)

const protoExport = pushReplicationMetricsOtelProtobuf(
  primary,
  'http://127.0.0.1:4318/v1/metrics',
  5_000,
)
console.log(protoExport.statusCode, protoExport.responseBody)

const grpcExport = pushReplicationMetricsOtelGrpc(
  primary,
  'http://127.0.0.1:4317',
  5_000,
)
console.log(grpcExport.statusCode, grpcExport.responseBody)

const secureExport = pushReplicationMetricsOtelJsonWithOptions(
  primary,
  'https://collector.internal:4318/v1/metrics',
  {
    timeoutMs: 5_000,
    retryMaxAttempts: 3,
    retryBackoffMs: 200,
    retryBackoffMaxMs: 2_000,
    retryJitterRatio: 0.2,
    adaptiveRetry: true,
    adaptiveRetryMode: 'ewma',
    adaptiveRetryEwmaAlpha: 0.35,
    circuitBreakerFailureThreshold: 3,
    circuitBreakerOpenMs: 30_000,
    circuitBreakerHalfOpenProbes: 2,
    circuitBreakerStateUrl: 'https://state-store.internal/otlp/breakers',
    circuitBreakerStatePatch: true,
    circuitBreakerStatePatchBatch: true,
    circuitBreakerStatePatchBatchMaxKeys: 4,
    circuitBreakerStatePatchMerge: true,
    circuitBreakerStatePatchMergeMaxKeys: 16,
    circuitBreakerStatePatchRetryMaxAttempts: 2,
    circuitBreakerStateCas: true,
    circuitBreakerStateLeaseId: 'otlp-writer-a',
    circuitBreakerScopeKey: 'collector-a',
    compressionGzip: true,
    httpsOnly: true,
    caCertPemPath: './tls/collector-ca.pem',
    clientCertPemPath: './tls/client.pem',
    clientKeyPemPath: './tls/client-key.pem',
  },
)
console.log(secureExport.statusCode, secureExport.responseBody)

const secureProtoExport = pushReplicationMetricsOtelProtobufWithOptions(
  primary,
  'https://collector.internal:4318/v1/metrics',
  {
    timeoutMs: 5_000,
    retryMaxAttempts: 3,
    retryBackoffMs: 200,
    retryBackoffMaxMs: 2_000,
    retryJitterRatio: 0.2,
    adaptiveRetry: true,
    adaptiveRetryMode: 'ewma',
    adaptiveRetryEwmaAlpha: 0.35,
    circuitBreakerFailureThreshold: 3,
    circuitBreakerOpenMs: 30_000,
    circuitBreakerHalfOpenProbes: 2,
    circuitBreakerStatePath: './runtime/otlp-breakers.json',
    circuitBreakerScopeKey: 'collector-a',
    compressionGzip: true,
    httpsOnly: true,
    caCertPemPath: './tls/collector-ca.pem',
    clientCertPemPath: './tls/client.pem',
    clientKeyPemPath: './tls/client-key.pem',
  },
)
console.log(secureProtoExport.statusCode, secureProtoExport.responseBody)

const secureGrpcExport = pushReplicationMetricsOtelGrpcWithOptions(
  primary,
  'https://collector.internal:4317',
  {
    timeoutMs: 5_000,
    retryMaxAttempts: 3,
    retryBackoffMs: 200,
    retryBackoffMaxMs: 2_000,
    retryJitterRatio: 0.2,
    adaptiveRetry: true,
    adaptiveRetryMode: 'ewma',
    adaptiveRetryEwmaAlpha: 0.35,
    circuitBreakerFailureThreshold: 3,
    circuitBreakerOpenMs: 30_000,
    circuitBreakerHalfOpenProbes: 2,
    circuitBreakerStatePath: './runtime/otlp-breakers.json',
    circuitBreakerScopeKey: 'collector-a',
    compressionGzip: true,
    httpsOnly: true,
    caCertPemPath: './tls/collector-ca.pem',
    clientCertPemPath: './tls/client.pem',
    clientKeyPemPath: './tls/client-key.pem',
  },
)
console.log(secureGrpcExport.statusCode, secureGrpcExport.responseBody)

const snapshotJson = collectReplicationSnapshotTransportJson(primary, false)
console.log(snapshotJson)

const logPageJson = collectReplicationLogTransportJson(primary, null, 128, 1_048_576, false)
console.log(logPageJson)

const adapter = createReplicationTransportAdapter(primary)
const snapshot = adapter.snapshot(false)
const logPage = adapter.log({ maxFrames: 128, maxBytes: 1_048_576, includePayload: false })
console.log(snapshot, logPage)

replica.close()
primary.close()
```

## API surface

The Node bindings expose both low-level graph primitives (`Database`) and higher-level APIs (Kite) for schema-driven workflows, plus metrics, backups, traversal, and vector search. For full API details and guides, see the docs:

https://kitedb.vercel.com/docs

## License

MIT

# trigger
