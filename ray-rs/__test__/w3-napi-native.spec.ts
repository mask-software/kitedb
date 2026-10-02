// Reproductions for the wave-3 napi-native findings X1, X3, X4 and X6. Each test
// encodes the contract a fix must satisfy; they fail against the unfixed bindings.
// X2 and X5 are perf-only and have no tests here.
//
// New native exports (the *Async variants) are looked up dynamically on the
// native module, never imported by name: a static import of a missing export
// would fail the whole file at link time.

import test from 'ava'
import type { ExecutionContext } from 'ava'
import fs from 'node:fs'
import http from 'node:http'
import type { AddressInfo } from 'node:net'
import { createRequire } from 'node:module'
import os from 'node:os'
import path from 'node:path'

import { Database, kiteSync, node, prop } from '../ts/index'

const native = createRequire(import.meta.url)('../index.js')

const makeDbPath = () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kitedb-w3-napi-native-'))
  return path.join(dir, 'test.kitedb')
}

const openDb = (t: ExecutionContext, dbPath = makeDbPath()) => {
  const db = Database.open(dbPath, {})
  t.teardown(() => {
    try {
      db.close()
    } catch {}
  })
  return db
}

/** Seed a few nodes and edges so maintenance calls have work to do. */
const seed = (db: Database, nodes = 50) => {
  db.begin()
  const etype = db.getOrCreateEtype('next')
  const ids: number[] = []
  for (let i = 0; i < nodes; i++) ids.push(db.createNode(`n:${i}`))
  for (let i = 1; i < ids.length; i++) db.addEdge(ids[i - 1], etype, ids[i])
  db.commit()
  return { ids, etype }
}

type Outcome = { threw: false; value: unknown } | { threw: true; message: string }

const outcome = (fn: () => unknown): Outcome => {
  try {
    return { threw: false, value: fn() }
  } catch (err) {
    return { threw: true, message: String((err as Error)?.message ?? err) }
  }
}

const show = (value: unknown) =>
  JSON.stringify(value, (_k, v) => (typeof v === 'bigint' ? `${v}n` : v instanceof Float32Array ? Array.from(v) : v))

/**
 * Count setInterval ticks while `work` runs. A call that blocks the JS thread
 * lets no tick fire; a call that does its work off-thread lets them run.
 */
async function ticksDuring(work: () => unknown): Promise<{ ticks: number; elapsedMs: number }> {
  let ticks = 0
  const timer = setInterval(() => ticks++, 5)
  const start = performance.now()
  try {
    await work()
  } finally {
    clearInterval(timer)
  }
  return { ticks, elapsedMs: performance.now() - start }
}

/** Return `owner[name]` bound to `owner`, failing the test when it is not a function. */
function requireAsyncVariant(t: ExecutionContext, owner: any, label: string, name: string) {
  const fn = owner?.[name]
  t.is(typeof fn, 'function', `${label}.${name} is missing: the only variant is synchronous and blocks the event loop`)
  return typeof fn === 'function' ? (fn.bind(owner) as (...args: any[]) => any) : undefined
}

/** Call an async variant and assert it returned a Promise before awaiting it. */
async function awaitPromise<T>(t: ExecutionContext, label: string, call: () => T): Promise<Awaited<T>> {
  const pending = call()
  t.true(pending instanceof Promise, `${label} must return a Promise, got ${show(pending)}`)
  return await pending
}

// =============================================================================
// X1: long synchronous calls block the Node event loop
// =============================================================================

const UNREACHED_TOKEN = '999:999'

test('X1: waitForTokenAsync waits without blocking the event loop (sync waitForToken blocks)', async (t) => {
  const db = openDb(t)

  const sync = await ticksDuring(() => db.waitForToken(UNREACHED_TOKEN, 300))
  t.log(`sync waitForToken(300ms): ${sync.elapsedMs.toFixed(0)}ms elapsed, ${sync.ticks} timer ticks`)

  const waitForTokenAsync = requireAsyncVariant(t, db, 'Database', 'waitForTokenAsync')
  if (!waitForTokenAsync) return

  let result: unknown
  const run = await ticksDuring(async () => {
    result = await awaitPromise(t, 'waitForTokenAsync', () => waitForTokenAsync(UNREACHED_TOKEN, 300))
  })
  t.is(result, false, 'an unreached token must resolve false after the timeout')
  t.true(run.elapsedMs >= 250, `waitForTokenAsync returned after ${run.elapsedMs.toFixed(0)}ms, before its timeout`)
  t.true(
    run.ticks >= 10,
    `timers must keep firing while waitForTokenAsync waits: ${run.ticks} ticks in ${run.elapsedMs.toFixed(0)}ms`,
  )
})

// A collector served by this same process can only answer while the JS thread
// is free. The sync push blocks the thread, so it times out against a healthy
// collector; an async push must get the collector's 200.
test('X1: async OTEL push reaches a collector served by the same process (sync push times out)', async (t) => {
  let received = 0
  const server = http.createServer((req, res) => {
    req.resume()
    req.on('end', () => {
      received++
      res.writeHead(200, { 'content-type': 'application/json' })
      res.end('{}')
    })
  })
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve))
  t.teardown(() => new Promise<void>((resolve) => server.close(() => resolve())))
  server.unref()
  const url = `http://127.0.0.1:${(server.address() as AddressInfo).port}/v1/metrics`

  const db = openDb(t)
  seed(db, 5)

  const syncStart = performance.now()
  const sync = outcome(() => native.pushReplicationMetricsOtelJson(db, url, 500))
  t.log(`sync pushReplicationMetricsOtelJson: ${(performance.now() - syncStart).toFixed(0)}ms -> ${show(sync)}`)

  const pushAsync = requireAsyncVariant(t, native, 'native', 'pushReplicationMetricsOtelJsonAsync')
  if (!pushAsync) return
  const before = received
  const result = await awaitPromise(t, 'pushReplicationMetricsOtelJsonAsync', () => pushAsync(db, url, 5_000))
  t.is(result?.statusCode, 200, `async push must reach the in-process collector, got ${show(result)}`)
  t.true(received > before, 'collector must have received the async push')
})

test('X1: every OTEL push function has a Promise variant', (t) => {
  const missing = [
    'pushReplicationMetricsOtelJson',
    'pushReplicationMetricsOtelJsonWithOptions',
    'pushReplicationMetricsOtelProtobuf',
    'pushReplicationMetricsOtelProtobufWithOptions',
    'pushReplicationMetricsOtelGrpc',
    'pushReplicationMetricsOtelGrpcWithOptions',
  ]
    .map((name) => `${name}Async`)
    .filter((name) => typeof native[name] !== 'function')
  t.deepEqual(missing, [], 'OTEL pushes do network I/O with retries/backoff on the JS thread')
})

test('X1: checkpointAsync / optimizeAsync / vacuumAsync resolve and keep the data', async (t) => {
  const dbPath = makeDbPath()
  const db = Database.open(dbPath, {})
  const { ids } = seed(db)

  for (const name of ['checkpointAsync', 'optimizeAsync', 'vacuumAsync']) {
    const fn = requireAsyncVariant(t, db, 'Database', name)
    if (!fn) continue
    await awaitPromise(t, name, () => fn())
    t.is(db.countNodes(), ids.length, `${name} lost nodes`)
  }
  db.close()

  const reopened = openDb(t, dbPath)
  t.is(reopened.countNodes(), ids.length)
  t.is(reopened.countEdges(), ids.length - 1)
})

test('X1: exportToJsonAsync / exportToJsonlAsync / importFromJsonAsync round-trip', async (t) => {
  const source = openDb(t)
  const { ids } = seed(source, 20)
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kitedb-w3-export-'))

  const exportJson = requireAsyncVariant(t, source, 'Database', 'exportToJsonAsync')
  const exportJsonl = requireAsyncVariant(t, source, 'Database', 'exportToJsonlAsync')
  const target = openDb(t)
  const importJson = requireAsyncVariant(t, target, 'Database', 'importFromJsonAsync')
  if (!exportJson || !exportJsonl || !importJson) return

  const jsonPath = path.join(dir, 'export.json')
  const exported = await awaitPromise(t, 'exportToJsonAsync', () => exportJson(jsonPath))
  t.is(exported.nodeCount, ids.length)
  const jsonl = await awaitPromise(t, 'exportToJsonlAsync', () => exportJsonl(path.join(dir, 'export.jsonl')))
  t.is(jsonl.nodeCount, ids.length)

  const imported = await awaitPromise(t, 'importFromJsonAsync', () => importJson(jsonPath))
  t.is(imported.nodeCount, ids.length)
  t.is(target.countNodes(), ids.length)
  t.is(target.countEdges(), ids.length - 1)
})

test('X1: createBackupAsync / restoreBackupAsync round-trip', async (t) => {
  const db = openDb(t)
  const { ids } = seed(db, 10)
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kitedb-w3-backup-'))

  const createBackupAsync = requireAsyncVariant(t, native, 'native', 'createBackupAsync')
  const restoreBackupAsync = requireAsyncVariant(t, native, 'native', 'restoreBackupAsync')
  if (!createBackupAsync || !restoreBackupAsync) return

  const backupPath = path.join(dir, 'backup.kitedb')
  const backup = await awaitPromise(t, 'createBackupAsync', () => createBackupAsync(db, backupPath))
  t.true(backup.size > 0)
  const restoredPath = path.join(dir, 'restored.kitedb')
  const restored = await awaitPromise(t, 'restoreBackupAsync', () => restoreBackupAsync(backupPath, restoredPath))
  t.is(typeof restored, 'string')

  const reopened = openDb(t, restored)
  t.is(reopened.countNodes(), ids.length)
})

test('X1: VectorIndex.buildIndexAsync and IVF trainAsync resolve with a usable index', async (t) => {
  const vectors = createVectorIndex4()
  const buildIndexAsync = requireAsyncVariant(t, vectors, 'VectorIndex', 'buildIndexAsync')

  const ivf = new native.JsIvfIndex(2, { nClusters: 1, nProbe: 1 })
  ivf.addTrainingVectors(trainingVectors(8), 8)
  const ivfTrainAsync = requireAsyncVariant(t, ivf, 'JsIvfIndex', 'trainAsync')

  const ivfPq = new native.JsIvfPqIndex(2, { nClusters: 1, nProbe: 1 }, { numSubspaces: 1, numCentroids: 2 })
  ivfPq.addTrainingVectors(trainingVectors(64), 64)
  const ivfPqTrainAsync = requireAsyncVariant(t, ivfPq, 'JsIvfPqIndex', 'trainAsync')

  if (buildIndexAsync) {
    await awaitPromise(t, 'VectorIndex.buildIndexAsync', () => buildIndexAsync())
    t.true(vectors.stats().indexTrained)
    t.is(vectors.search([1, 0, 0, 0], { k: 1 })[0]?.nodeId, 1)
  }
  if (ivfTrainAsync) {
    await awaitPromise(t, 'JsIvfIndex.trainAsync', () => ivfTrainAsync())
    t.true(ivf.trained)
  }
  if (ivfPqTrainAsync) {
    await awaitPromise(t, 'JsIvfPqIndex.trainAsync', () => ivfPqTrainAsync())
    t.true(ivfPq.trained)
  }
})

function createVectorIndex4() {
  const index = native.createVectorIndex({ dimensions: 4, trainingThreshold: 2, ivf: { nClusters: 1, nProbe: 1 } })
  index.set(1, [1, 0, 0, 0])
  index.set(2, [0, 1, 0, 0])
  return index
}

function trainingVectors(count: number) {
  const out: number[] = []
  for (let i = 0; i < count; i++) out.push(((i * 37) % 100) / 100, ((i * 61) % 100) / 100)
  return out
}

// =============================================================================
// X3: numbers lose precision or wrap silently
// =============================================================================

const Item = node('item', {
  key: (id: string) => `item:${id}`,
  props: {
    n: prop.any('n'),
    data: prop.any('data'),
    embedding: prop.vector('embedding'),
  },
})

const openKite = (t: ExecutionContext, dbPath = makeDbPath()) => {
  const db = kiteSync(dbPath, { nodes: [Item], edges: [] })
  t.teardown(() => {
    try {
      db.close()
    } catch {}
  })
  return db
}

const I64_MAX = 2n ** 63n - 1n
const I64_MIN = -(2n ** 63n)

test('X3: BigInt prop values outside i64 are rejected, not wrapped', (t) => {
  const db = openKite(t)
  const item = db.insert(Item).values('a', { n: 1n }).returning()

  for (const big of [I64_MAX + 1n, I64_MIN - 1n, 2n ** 64n + 5n]) {
    const result = outcome(() => db.setProp(item.id, 'n', big))
    const stored = db.getProp(item.id, 'n')
    t.true(result.threw, `setProp(${big}n) must throw (value does not fit i64); it stored ${show(stored)} instead`)
  }
  // Control: the i64 bounds themselves are accepted.
  t.notThrows(() => db.setProp(item.id, 'n', I64_MAX))
  t.notThrows(() => db.setProp(item.id, 'n', I64_MIN))
})

test('X3: BigInt key fields outside i64 are rejected or kept exact, not wrapped', (t) => {
  const db = openKite(t)
  const big = 2n ** 64n + 1n
  const result = outcome(() =>
    db
      .insert(Item)
      .values({ id: big } as any, { n: 1n })
      .returning(),
  )
  if (result.threw) {
    t.pass(`rejected: ${result.message}`)
    return
  }
  const created = result.value as { key: string }
  t.is(created.key, `item:${big}`, `BigInt key ${big}n was silently rewritten`)
})

test('X3: I64 props beyond 2^53 read back exactly (BigInt), safe-range ints stay numbers', (t) => {
  const db = openKite(t)
  const exact = 2n ** 53n + 1n
  const item = db.insert(Item).values('a', { n: exact, data: 42n }).returning()

  const viaGetProp = db.getProp(item.id, 'n')?.intValue as unknown
  t.is(viaGetProp, exact, `getProp().intValue lost precision: ${show(viaGetProp)}`)

  const viaGetById = (db.getById(item.id) as any)?.n
  t.is(viaGetById, exact, `getById().n lost precision: ${show(viaGetById)}`)

  // Non-breaking: an I64 inside the safe range still comes back as a number.
  t.is(db.getProp(item.id, 'data')?.intValue as unknown, 42)
  t.is((db.getById(item.id) as any)?.data, 42)
})

test('X3: Database.getNodeProp returns I64 beyond 2^53 exactly', (t) => {
  const dbPath = makeDbPath()
  const exact = -(2n ** 60n) - 3n
  {
    const db = kiteSync(dbPath, { nodes: [Item], edges: [] })
    db.insert(Item).values('a', { n: exact }).returning()
    db.close()
  }
  const db = openDb(t, dbPath)
  const nodeId = db.getNodeByKey('item:a')
  const keyId = db.getPropkeyId('n')
  t.not(nodeId, null)
  t.not(keyId, null)
  const value = db.getNodeProp(nodeId!, keyId!)
  t.is(value?.propType, 'Int')
  t.is(value?.intValue as unknown, exact, `getNodeProp().intValue lost precision: ${show(value?.intValue)}`)
})

test('X3: a plain number[] for an untyped prop is not silently narrowed to f32', (t) => {
  const db = openKite(t)
  const item = db.insert(Item).values('a', { n: 1n }).returning()
  const input = [16_777_217, 0.1, 2 ** 40 + 1]

  const result = outcome(() => db.setProp(item.id, 'data', input))
  if (result.threw) {
    t.pass(`rejected: ${result.message}`)
    return
  }
  const stored = (db.getById(item.id) as any)?.data
  t.deepEqual(
    stored === undefined || stored === null ? stored : Array.from(stored as ArrayLike<number>),
    input,
    `number[] ${show(input)} was stored as an f32 vector ${show(stored)}`,
  )
})

test('X3: Float32Array is accepted as an explicit vector value', (t) => {
  const db = openKite(t)
  const item = db.insert(Item).values('a', { n: 1n }).returning()
  const vector = new Float32Array([0.5, 0.25, 0.125])

  const result = outcome(() => db.setProp(item.id, 'embedding', vector))
  t.false(result.threw, `Float32Array vector rejected: ${show(result)}`)
  const stored = (db.getById(item.id) as any)?.embedding
  t.deepEqual(stored ? Array.from(stored as ArrayLike<number>) : stored, [0.5, 0.25, 0.125])

  // Non-breaking: a number[] for a declared vector prop still stores a vector.
  t.notThrows(() => db.setProp(item.id, 'embedding', [1, 2, 3]))
  const again = (db.getById(item.id) as any)?.embedding
  t.deepEqual(again ? Array.from(again as ArrayLike<number>) : again, [1, 2, 3])
})

// u32 parameters (etype, keyId, labelId, ...) take JS numbers. NAPI's uint32
// conversion wraps -1 to 4294967295 and maps NaN to 0 and 1.5 to 1.
const BAD_U32: Array<[string, number]> = [
  ['-1', -1],
  ['NaN', Number.NaN],
  ['1.5', 1.5],
  ['2**32', 2 ** 32],
]

test('X3: u32 id parameters reject negative, NaN, fractional and out-of-range numbers', (t) => {
  const db = openDb(t)
  db.begin()
  const a = db.createNode('a')
  const b = db.createNode('b')
  const etype = db.getOrCreateEtype('knows')
  db.getOrCreatePropkey('name')
  db.getOrCreateLabel('person')
  db.addEdge(a, etype, b)
  db.commit()

  const value = { propType: 'String', stringValue: 'x' } as any
  const calls: Record<string, (bad: number) => unknown> = {
    'addEdge(etype)': (bad) => db.addEdge(a, bad, b),
    'setNodeProp(keyId)': (bad) => db.setNodeProp(a, bad, value),
    'setEdgeProp(etype)': (bad) => db.setEdgeProp(a, bad, b, 0, value),
    'addNodeLabel(labelId)': (bad) => db.addNodeLabel(a, bad),
    'traverseSingle(edgeType)': (bad) => db.traverseSingle([a], 'Out' as any, bad),
    'kShortest(k)': (bad) => db.kShortest({ source: a, target: b }, bad),
  }

  const silent: string[] = []
  for (const [label, bad] of BAD_U32) {
    for (const [api, call] of Object.entries(calls)) {
      db.begin()
      const result = outcome(() => call(bad))
      try {
        db.rollback()
      } catch {}
      if (!result.threw) silent.push(`${api} with ${label} -> accepted (${show(result.value)})`)
    }
  }
  t.deepEqual(silent, [], 'invalid u32 arguments were silently coerced')
})

test('X3: a JsPropValue missing its value field is rejected, not stored as 0/false/""', (t) => {
  const db = openDb(t)
  db.begin()
  const a = db.createNode('a')
  const keyId = db.getOrCreatePropkey('p')
  db.commit()

  const silent: string[] = []
  for (const propType of ['Int', 'Float', 'Bool', 'String', 'Vector']) {
    db.begin()
    const result = outcome(() => db.setNodeProp(a, keyId, { propType } as any))
    if (result.threw) {
      db.rollback()
      continue
    }
    db.commit()
    silent.push(`Database.setNodeProp({ propType: '${propType}' }) stored ${show(db.getNodeProp(a, keyId))}`)
  }

  const kite = openKite(t)
  const item = kite.insert(Item).values('a', { n: 1n }).returning()
  for (const propType of ['Int', 'Float', 'Bool', 'String']) {
    const result = outcome(() => kite.setProp(item.id, 'data', { propType }))
    if (!result.threw)
      silent.push(`Kite.setProp({ propType: '${propType}' }) stored ${show(kite.getProp(item.id, 'data'))}`)
  }

  t.deepEqual(silent, [], 'missing value fields must be an error')
})

// =============================================================================
// X4: zero / negative edge weights become 1.0 in DB-backed Dijkstra
// =============================================================================

/** a->b (weight `direct`), a->c (weight `viaFirst`), c->b (weight `viaSecond`). */
function weightedTriangle(t: ExecutionContext, direct: number, viaFirst: number, viaSecond: number) {
  const db = openDb(t)
  db.begin()
  const a = db.createNode('a')
  const b = db.createNode('b')
  const c = db.createNode('c')
  const etype = db.getOrCreateEtype('road')
  const weight = db.getOrCreatePropkey('weight')
  const set = (src: number, dst: number, w: number) => {
    db.addEdge(src, etype, dst)
    db.setEdgeProp(src, etype, dst, weight, { propType: 'Float', floatValue: w } as any)
  }
  set(a, b, direct)
  set(a, c, viaFirst)
  set(c, b, viaSecond)
  db.commit()
  return { db, a, b, c }
}

test('X4: Dijkstra honours zero edge weights', (t) => {
  const { db, a, b, c } = weightedTriangle(t, 1.5, 0, 1)
  const result = db.dijkstra({ source: a, target: b, weightKeyName: 'weight' })
  t.true(result.found)
  t.deepEqual(result.path, [a, c, b], `zero-weight edge a->c was treated as 1.0: ${show(result)}`)
  t.is(result.totalWeight, 1)

  const [best] = db.kShortest({ source: a, target: b, weightKeyName: 'weight' }, 1)
  t.deepEqual(best?.path, [a, c, b], `kShortest: ${show(best)}`)
  t.is(best?.totalWeight, 1)
})

test('X4: Dijkstra rejects negative edge weights', (t) => {
  const { db, a, b } = weightedTriangle(t, 5, -1, 1)
  const dijkstra = outcome(() => db.dijkstra({ source: a, target: b, weightKeyName: 'weight' }))
  t.true(dijkstra.threw, `negative weight was silently replaced by 1.0: ${show(dijkstra)}`)
  if (dijkstra.threw) t.regex(dijkstra.message, /negative|weight/i)

  const kShortest = outcome(() => db.kShortest({ source: a, target: b, weightKeyName: 'weight' }, 2))
  t.true(kShortest.threw, `kShortest: negative weight was silently replaced by 1.0: ${show(kShortest)}`)
})

// =============================================================================
// X6: pagination ends early when the cursor's node/edge was deleted
// =============================================================================

test('X6: getNodesPage continues after the cursor node is deleted', (t) => {
  const db = openDb(t)
  const { ids } = seed(db, 10)

  const first = db.getNodesPage({ limit: 3 })
  t.deepEqual(first.items, ids.slice(0, 3))
  t.true(first.hasMore)

  db.begin()
  db.deleteNode(ids[2])
  db.commit()

  const second = db.getNodesPage({ limit: 3, cursor: first.nextCursor })
  t.deepEqual(second.items, ids.slice(3, 6), `iteration ended early: ${show(second)}`)
  t.true(second.hasMore)
})

test('X6: getEdgesPage continues after the cursor edge is deleted', (t) => {
  const db = openDb(t)
  const { etype } = seed(db, 10)
  const all = db.getEdgesPage({ limit: 100 }).items

  const first = db.getEdgesPage({ limit: 3 })
  t.deepEqual(first.items, all.slice(0, 3))
  t.true(first.hasMore)

  const cursorEdge = all[2]
  db.begin()
  db.deleteEdge(cursorEdge.src, etype, cursorEdge.dst)
  db.commit()

  const second = db.getEdgesPage({ limit: 3, cursor: first.nextCursor })
  t.deepEqual(second.items, all.slice(3, 6), `iteration ended early: ${show(second)}`)
  t.true(second.hasMore)
})
