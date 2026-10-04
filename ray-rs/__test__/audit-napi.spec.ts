// Reproductions for the NAPI audit findings N1-N7. Each test encodes the
// contract a fix must satisfy; they fail against the unfixed bindings.
//
// Crashing paths (N1 checkpoint, N2) and GC observation (N7) run in a child
// process so an abort or --expose-gc cannot take down the ava worker.

import test from 'ava'
import { spawn } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

import { bruteForceSearch, edge, kiteSync, node, prop } from '../ts/index'
import type { Kite } from '../ts/index'

const rayRsDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const tsEntry = path.join(rayRsDir, 'ts', 'index.ts')
const nativeEntry = path.join(rayRsDir, 'index.js')

const makeDbPath = () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kitedb-audit-napi-'))
  return path.join(dir, 'test.kitedb')
}

const User = node('user', {
  key: (id: string) => `user:${id}`,
  props: {
    name: prop.string('name'),
  },
})

const Knows = edge('knows', {})

// =============================================================================
// Child process harness
// =============================================================================

type ChildRun = {
  code: number | null
  signal: string | null
  stdout: string
  stderr: string
  results: any[]
}

/**
 * Run an ESM snippet in a fresh Node process with the TS entry available via
 * `await import(process.env.KITE_TS_ENTRY)` and the native module via
 * `process.env.KITE_NATIVE_ENTRY`. The snippet reports with
 * `fs.writeSync(1, 'RESULT ' + JSON.stringify(x) + '\n')` (synchronous, so
 * results written before an abort are not lost).
 */
function runChild(source: string, options: { exposeGc?: boolean; timeoutMs?: number } = {}): Promise<ChildRun> {
  return new Promise((resolve, reject) => {
    const args = [
      ...(options.exposeGc ? ['--expose-gc'] : []),
      '--import',
      '@oxc-node/core/register',
      '--input-type=module',
      '-e',
      source,
    ]
    const child = spawn(process.execPath, args, {
      cwd: rayRsDir,
      env: {
        ...process.env,
        OXC_TSCONFIG_PATH: './__test__/tsconfig.json',
        KITE_TS_ENTRY: tsEntry,
        KITE_NATIVE_ENTRY: nativeEntry,
      },
      stdio: ['ignore', 'pipe', 'pipe'],
    })
    let stdout = ''
    let stderr = ''
    child.stdout.on('data', (chunk) => (stdout += chunk))
    child.stderr.on('data', (chunk) => (stderr += chunk))
    const timer = setTimeout(() => child.kill('SIGKILL'), options.timeoutMs ?? 90_000)
    child.on('error', (err) => {
      clearTimeout(timer)
      reject(err)
    })
    child.on('close', (code, signal) => {
      clearTimeout(timer)
      const results = stdout
        .split('\n')
        .filter((line) => line.startsWith('RESULT '))
        .map((line) => JSON.parse(line.slice('RESULT '.length)))
      resolve({ code, signal, stdout, stderr, results })
    })
  })
}

const tail = (text: string, lines = 6) => text.trim().split('\n').slice(-lines).join('\n')

const describeExit = (run: ChildRun) =>
  `child exited code=${run.code} signal=${run.signal}\n--- stderr (tail) ---\n${tail(run.stderr)}`

const CHILD_PRELUDE = `
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { createRequire } from 'node:module'
const kite = await import(process.env.KITE_TS_ENTRY)
const native = createRequire(process.env.KITE_NATIVE_ENTRY)(process.env.KITE_NATIVE_ENTRY)
const report = (value) => fs.writeSync(1, 'RESULT ' + JSON.stringify(value) + '\\n')
const makeDbPath = () => path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'kitedb-audit-napi-child-')), 'test.kitedb')
const outcome = (fn) => {
  try {
    const value = fn()
    return { threw: false, value: value === undefined ? null : value }
  } catch (err) {
    return { threw: true, isError: err instanceof Error, message: String(err && err.message) }
  }
}
`

// =============================================================================
// N1: JS node IDs are never range-checked
// =============================================================================

// Each bad id gets fresh databases. Calls are recorded as they happen; then the
// allocator is probed and every database checkpointed. Databases are never
// closed after accepting a bad id: the child SIGKILLs itself at the end so
// teardown cannot crash on corrupted state.
const N1_SOURCE = `${CHILD_PRELUDE}
const { kiteSync, node, edge, prop, Database } = kite
const User = node('user', { key: (id) => 'user:' + id, props: { name: prop.string('name') } })
const Knows = edge('knows', {})
const BAD_IDS = [
  ['-1', -1],
  ['NaN', NaN],
  ['Infinity', Infinity],
  ['1.5', 1.5],
  ['2**53 (MAX_SAFE_INTEGER + 1)', 2 ** 53],
]
const opened = []
for (const [label, bad] of BAD_IDS) {
  const db = kiteSync(makeDbPath(), { nodes: [User], edges: [Knows] })
  opened.push([label, 'Kite', db])
  const anchor = db.insert(User).values('anchor', { name: 'anchor' }).returning()
  const kiteCalls = {
    'Kite.upsertById': () => db.upsertById(User, bad).set('name', 'x').execute(),
    'Kite.updateById': () => db.updateById(bad).set('name', 'x').execute(),
    'Kite.setProp': () => db.setProp(bad, 'name', 'x'),
    'Kite.link(src)': () => db.link(bad, Knows, anchor.id),
    'Kite.link(dst)': () => db.link(anchor.id, Knows, bad),
    'Kite.deleteById': () => db.deleteById(bad),
  }
  for (const [api, fn] of Object.entries(kiteCalls)) report({ kind: 'call', id: label, api, ...outcome(fn) })

  const raw = Database.open(makeDbPath(), {})
  opened.push([label, 'Database', raw])
  raw.begin()
  const rawAnchor = raw.createNode('anchor')
  const etype = raw.getOrCreateEtype('knows')
  const keyId = raw.getOrCreatePropkey('name')
  const value = { propType: 'String', stringValue: 'x' }
  const dbCalls = {
    'Database.upsertNodeById': () => raw.upsertNodeById(bad, []),
    'Database.setNodeProp': () => raw.setNodeProp(bad, keyId, value),
    'Database.addEdge(src)': () => raw.addEdge(bad, etype, rawAnchor),
    'Database.addEdge(dst)': () => raw.addEdge(rawAnchor, etype, bad),
    'Database.deleteNode': () => raw.deleteNode(bad),
  }
  for (const [api, fn] of Object.entries(dbCalls)) report({ kind: 'call', id: label, api, ...outcome(fn) })
  raw.commit()
}

// Control: valid ids must keep working.
{
  const db = kiteSync(makeDbPath(), { nodes: [User], edges: [Knows] })
  const r = outcome(() => {
    db.upsertById(User, 42).set('name', 'ok').execute()
    return db.getById(42) ? 'found' : 'missing'
  })
  report({ kind: 'control', api: 'Kite.upsertById(42)', ...r })
  db.close()
}

// Allocator state after the bad calls, then checkpoint each database.
for (const [label, flavor, db] of opened) {
  const r = flavor === 'Kite'
    ? outcome(() => db.insert(User).values('after', { name: 'after' }).returning().id)
    : outcome(() => { db.begin(); const id = db.createNode('after'); db.commit(); return id })
  report({ kind: 'nextId', id: label, flavor, ...r })
}
for (const [label, flavor, db] of opened) {
  report({ kind: 'checkpoint-start', id: label, flavor })
  report({ kind: 'checkpoint', id: label, flavor, ...outcome(() => db.checkpoint()) })
}
report({ kind: 'done' })
process.kill(process.pid, 'SIGKILL')
`

let n1Run: Promise<ChildRun> | undefined
const getN1Run = () => (n1Run ??= runChild(N1_SOURCE))

// A clear validation error names the parameter or the constraint.
const CLEAR_NODE_ID_ERROR = /id|src|dst|integer|negative|range|safe/i

test('audit N1: node ids outside 0..=MAX_SAFE_INTEGER are rejected with a clear error', async (t) => {
  const run = await getN1Run()
  const calls = run.results.filter((r) => r.kind === 'call')
  t.true(calls.length > 0, `no call results reported; ${describeExit(run)}`)

  const accepted = calls.filter((r) => !r.threw).map((r) => `${r.api} accepted id ${r.id}`)
  t.deepEqual(accepted, [], 'invalid node ids must throw instead of being cast to u64')

  const unclear = calls
    .filter((r) => r.threw && !(r.isError && CLEAR_NODE_ID_ERROR.test(r.message)))
    .map((r) => `${r.api} with id ${r.id} threw: ${r.message}`)
  t.deepEqual(unclear, [], 'rejections must be JS Errors that explain the node id problem')

  const control = run.results.find((r) => r.kind === 'control')
  t.deepEqual(
    control && { threw: control.threw, value: control.value },
    { threw: false, value: 'found' },
    'a valid node id (42) must still be accepted',
  )
})

test('audit N1: invalid node ids do not corrupt the node id allocator', async (t) => {
  const run = await getN1Run()
  const nextIds = run.results.filter((r) => r.kind === 'nextId')
  t.true(nextIds.length > 0, `no allocator results reported; ${describeExit(run)}`)

  const badNextIds = nextIds
    .filter((r) => r.threw || !Number.isSafeInteger(r.value) || r.value < 0 || r.value > 1000)
    .map((r) => `${r.flavor} after id ${r.id}: next allocated id = ${r.threw ? `threw ${r.message}` : r.value}`)
  t.deepEqual(badNextIds, [], 'the next allocated node id must stay small and non-negative')
})

test('audit N1: checkpoint survives after calls with invalid node ids', async (t) => {
  const run = await getN1Run()
  const started = run.results.filter((r) => r.kind === 'checkpoint-start')
  const finished = run.results.filter((r) => r.kind === 'checkpoint')
  const crashedAt = started.find((s) => !finished.some((f) => f.id === s.id && f.flavor === s.flavor))
  t.is(
    crashedAt,
    undefined,
    `checkpoint crashed the process (${crashedAt?.flavor} after ${crashedAt?.id}); ${describeExit(run)}`,
  )
  const failed = finished.filter((r) => r.threw).map((r) => `${r.flavor} after ${r.id}: ${r.message}`)
  t.deepEqual(failed, [], 'checkpoint must succeed after invalid ids were rejected')
  t.truthy(
    run.results.find((r) => r.kind === 'done'),
    `child did not finish; ${describeExit(run)}`,
  )
})

// =============================================================================
// N2: wrong-length vectors abort the process
// =============================================================================

const N2_SETUP = `
const manifest = JSON.stringify({
  config: { dimensions: 2, metric: 'Euclidean', row_group_size: 1024, fragment_target_size: 100000, normalize_on_insert: false },
  fragments: [], active_fragment_id: 0, total_vectors: 0, total_deleted: 0, next_vector_id: 0,
  node_to_vector: {}, vector_to_node: {}, vector_locations: {},
})
const trainingVectors = (count) => {
  const out = []
  for (let i = 0; i < count; i++) out.push(((i * 37) % 100) / 100, ((i * 61) % 100) / 100)
  return out
}
const trainedIvf = () => {
  const index = new native.JsIvfIndex(2, { nClusters: 1, nProbe: 1 })
  index.addTrainingVectors(trainingVectors(8), 8)
  index.train()
  index.insert(1, [1, 0])
  // Sanity: a correctly sized query works.
  index.search(manifest, [1, 0], 1)
  return index
}
const trainedIvfPq = () => {
  const index = new native.JsIvfPqIndex(2, { nClusters: 1, nProbe: 1 }, { numSubspaces: 1, numCentroids: 2 })
  index.addTrainingVectors(trainingVectors(64), 64)
  index.train()
  index.insert(1, [1, 0])
  index.search(manifest, [1, 0], 1)
  return index
}
`

const n2Cases: Array<{ name: string; call: string }> = [
  {
    name: 'bruteForceSearch with a query shorter than the vectors',
    call: 'kite.bruteForceSearch([[1, 2, 3]], [1], [1, 2], 1)',
  },
  {
    name: 'bruteForceSearch with vectors of mixed lengths',
    call: 'kite.bruteForceSearch([[1, 2], [1, 2, 3]], [1, 2], [1, 2], 2)',
  },
  { name: 'JsIvfIndex.search with a wrong-length query', call: 'trainedIvf().search(manifest, [1, 2, 3], 1)' },
  {
    name: 'JsIvfIndex.searchMulti with a wrong-length query',
    call: "trainedIvf().searchMulti(manifest, [[1, 2, 3]], 1, 'Min')",
  },
  { name: 'JsIvfIndex.delete with a wrong-length vector', call: 'trainedIvf().delete(1, [1, 2, 3])' },
  { name: 'JsIvfPqIndex.search with a wrong-length query', call: 'trainedIvfPq().search(manifest, [1, 2, 3], 1)' },
  {
    name: 'JsIvfPqIndex.searchMulti with a wrong-length query',
    call: "trainedIvfPq().searchMulti(manifest, [[1, 2, 3]], 1, 'Min')",
  },
  { name: 'JsIvfPqIndex.delete with a wrong-length vector', call: 'trainedIvfPq().delete(1, [1, 2, 3])' },
]

for (const c of n2Cases) {
  test(`audit N2: ${c.name} throws a JS error instead of aborting`, async (t) => {
    const run = await runChild(`${CHILD_PRELUDE}${N2_SETUP}
report({ kind: 'call', ...outcome(() => ${c.call}) })
`)
    t.is(run.signal, null, `process was killed instead of throwing; ${describeExit(run)}`)
    t.is(run.code, 0, describeExit(run))
    const result = run.results.find((r) => r.kind === 'call')
    t.truthy(result, `no result reported; ${describeExit(run)}`)
    t.true(result?.threw, `expected a thrown error, got ${JSON.stringify(result)}`)
    t.true(result?.isError, 'thrown value must be an Error')
    t.regex(result?.message ?? '', /dimension|length|mismatch/i)
  })
}

// =============================================================================
// N3: cosine bruteForceSearch assumes unit-length vectors
// =============================================================================

test('audit N3: cosine bruteForceSearch returns the true cosine distance for non-unit vectors', (t) => {
  const [hit] = bruteForceSearch([[3, 4]], [1], [3, 4], 1, 'Cosine' as any)
  t.is(hit.nodeId, 1)
  t.true(Math.abs(hit.distance) < 1e-6, `identical vectors must have cosine distance 0, got ${hit.distance}`)
  t.true(Math.abs(hit.similarity - 1) < 1e-6, `identical vectors must have similarity 1, got ${hit.similarity}`)
})

test('audit N3: cosine bruteForceSearch ranks by angle, not magnitude', (t) => {
  // Node 1 points exactly along the query; node 2 is 45 degrees off but longer.
  const vectors = [
    [1, 0],
    [5, 5],
  ]
  for (const metric of ['Cosine' as any, undefined]) {
    const hits = bruteForceSearch(vectors, [1, 2], [1, 0], 2, metric)
    t.deepEqual(
      hits.map((h) => h.nodeId),
      [1, 2],
      `metric=${metric ?? 'default'}: exact direction must rank first`,
    )
    for (const h of hits) {
      t.true(h.distance >= -1e-6 && h.distance <= 2 + 1e-6, `cosine distance must be in [0, 2], got ${h.distance}`)
    }
  }
})

test('audit N3: cosine bruteForceSearch rejects zero vectors', (t) => {
  t.throws(() => bruteForceSearch([[1, 0]], [1], [0, 0], 1, 'Cosine' as any), { message: /zero/i })
  t.throws(() => bruteForceSearch([[0, 0]], [1], [1, 0], 1, 'Cosine' as any), { message: /zero/i })
})

// =============================================================================
// N4: an async Kite.transaction() absorbs every other write
// =============================================================================

function deferred(): { promise: Promise<void>; resolve: () => void } {
  let resolve!: () => void
  const promise = new Promise<void>((r) => (resolve = r))
  return { promise, resolve }
}

const failingAsyncTransaction = (db: Kite, gate: Promise<void>) =>
  db.transaction(async (ctx) => {
    ctx.insert(User).values('a', { name: 'A' }).execute()
    await gate
    throw new Error('A failed')
  }) as Promise<void>

test('audit N4: a write from outside an open async transaction is not rolled back with it', async (t) => {
  const db = kiteSync(makeDbPath(), { nodes: [User], edges: [] })
  try {
    const gate = deferred()
    const txA = failingAsyncTransaction(db, gate.promise)

    // Another request writes while A is suspended at an await.
    let outsideError: unknown
    try {
      db.insert(User).values('b', { name: 'B' }).execute()
    } catch (err) {
      outsideError = err
    }

    gate.resolve()
    await t.throwsAsync(txA, { message: 'A failed' })
    t.is(db.get(User, 'a'), null, "A's own write must roll back")

    if (outsideError === undefined) {
      t.truthy(db.get(User, 'b'), 'the outside write returned success, so it must survive the rollback of A')
    } else {
      t.regex(String((outsideError as Error).message), /transaction/i, 'a rejected outside write must say why')
    }
  } finally {
    db.close()
  }
})

test('audit N4: a concurrent async transaction() does not join another open async transaction', async (t) => {
  const db = kiteSync(makeDbPath(), { nodes: [User], edges: [] })
  try {
    const gate = deferred()
    const txA = failingAsyncTransaction(db, gate.promise)
    // Started from outside A's async context while A is suspended. Queueing B
    // until A settles and rejecting it with a clear error are both acceptable.
    let txB: Promise<string>
    try {
      txB = db.transaction(async (ctx) => {
        ctx.insert(User).values('b', { name: 'B' }).execute()
        return 'B committed'
      }) as Promise<string>
    } catch (err) {
      txB = Promise.reject(err)
    }

    gate.resolve()
    const [a, b] = await Promise.allSettled([txA, txB])
    t.is(a.status, 'rejected')
    t.is(db.get(User, 'a'), null, "A's own write must roll back")

    if (b.status === 'fulfilled') {
      t.is(b.value, 'B committed')
      t.truthy(db.get(User, 'b'), "B's transaction resolved, so its write must be committed independently of A")
    } else {
      t.regex(String((b.reason as Error)?.message), /transaction/i, 'a rejected transaction must say why')
    }
  } finally {
    db.close()
  }
})

test('audit N4: a sync transaction() started outside an open async transaction does not join it', async (t) => {
  const db = kiteSync(makeDbPath(), { nodes: [User], edges: [] })
  try {
    const gate = deferred()
    const txA = failingAsyncTransaction(db, gate.promise)

    let syncResult: unknown
    let syncError: unknown
    try {
      syncResult = db.transaction(() => {
        db.insert(User).values('b', { name: 'B' }).execute()
        return 'B committed'
      })
    } catch (err) {
      syncError = err
    }

    gate.resolve()
    await t.throwsAsync(txA, { message: 'A failed' })
    t.is(db.get(User, 'a'), null, "A's own write must roll back")

    if (syncError === undefined) {
      t.is(syncResult, 'B committed')
      t.truthy(db.get(User, 'b'), "B's transaction returned, so its write must survive the rollback of A")
    } else {
      t.regex(String((syncError as Error).message), /transaction/i, 'a rejected transaction must say why')
    }
  } finally {
    db.close()
  }
})

// =============================================================================
// N5: props named id/key/type overwrite node identity
// =============================================================================

for (const reserved of ['id', 'key', 'type'] as const) {
  test(`audit N5: a prop named "${reserved}" cannot shadow node identity`, (t) => {
    const shadow = reserved === 'id' ? 777 : 'shadow'
    let Spec: any
    let db: Kite
    try {
      Spec = node('user', {
        key: (id: string) => `user:${id}`,
        props: {
          [reserved]: reserved === 'id' ? prop.int(reserved) : prop.string(reserved),
          name: prop.string('name'),
        },
      })
      db = kiteSync(makeDbPath(), { nodes: [Spec], edges: [Knows] })
    } catch (err) {
      // Rejecting the reserved name at schema definition is a valid fix, as
      // long as the error names the offending prop.
      t.true(err instanceof Error)
      t.regex((err as Error).message, new RegExp(`\\b${reserved}\\b`))
      return
    }

    try {
      db.insert(Spec)
        .values('a', { [reserved]: shadow, name: 'A' })
        .execute()
      db.insert(Spec).values('b', { name: 'B' }).execute()
      const aId = db.getId(Spec, 'a') as number
      const bId = db.getId(Spec, 'b') as number

      const identity = (n: any) => n && { id: n.id, key: n.key, type: n.type }
      const expected = { id: aId, key: 'user:a', type: 'user' }
      t.deepEqual(identity(db.get(Spec, 'a')), expected, 'get() identity fields')
      t.deepEqual(identity(db.getById(aId)), expected, 'getById() identity fields')

      // link() resolves node objects through `.id`; it must connect the real nodes.
      db.link(db.get(Spec, 'a') as any, Knows, db.get(Spec, 'b') as any)
      t.true(db.hasEdge(aId, Knows, bId), 'link(a, b) must connect the real a and b')
    } finally {
      db.close()
    }
  })
}

// =============================================================================
// N6: retried insert/upsert executors silently drop props
// =============================================================================

const Profile = node('profile', {
  key: (id: string) => `profile:${id}`,
  props: {
    name: prop.string('name'),
    bio: prop.string('bio'),
  },
})

const BIO = 'x'.repeat(4096)
// More than a 1 MiB WAL holds (~180 rows): the prefill spills the WAL once.
const PREFILL_ROWS = 300
const RETRIED_ROWS = 100

type N6Kind = 'insert' | 'upsert' | 'insert valuesMany' | 'upsert valuesMany'

function n6Operations(db: Kite, kind: N6Kind): any[] {
  const row = (i: number) => ({ key: `u${i}`, name: `n${i}`, bio: BIO })
  const ops: any[] = []
  if (kind === 'insert' || kind === 'upsert') {
    for (let i = 0; i < RETRIED_ROWS; i++) {
      const builder = kind === 'insert' ? db.insert(Profile) : db.upsert(Profile)
      ops.push(builder.values(`u${i}`, { name: `n${i}`, bio: BIO }))
    }
  } else {
    for (let start = 0; start < RETRIED_ROWS; start += 10) {
      const builder = kind === 'insert valuesMany' ? db.insert(Profile) : db.upsert(Profile)
      ops.push(builder.valuesMany(Array.from({ length: 10 }, (_, j) => row(start + j))))
    }
  }
  return ops
}

for (const kind of ['insert', 'upsert', 'insert valuesMany', 'upsert valuesMany'] as const) {
  test(`audit N6: batchAdaptive retry after WAL-full keeps every ${kind} row and its props`, (t) => {
    // ~180 rows of 4 KiB fit in a 1 MiB WAL, and its WAL segments may hold
    // one spill's worth. The 300 prefill rows spill the WAL once and leave
    // ~120 rows in it, so the 100-row batch needs a second spill, which the
    // segment limit refuses: the checkpoint that runs then keeps the batch's
    // own records (its transaction is open), and the batch fails with
    // WAL-full. It fits after batchAdaptive's checkpoint.
    const db = kiteSync(makeDbPath(), {
      nodes: [Profile],
      edges: [],
      walSizeMb: 1,
      walSegmentLimit: 1,
    })
    try {
      db.transaction(() => {
        for (let i = 0; i < PREFILL_ROWS; i++) {
          db.insert(Profile)
            .values(`p${i}`, { name: `p${i}`, bio: BIO })
            .execute()
        }
      })

      let checkpoints = 0
      const checkpoint = db.checkpoint.bind(db)
      db.checkpoint = () => {
        checkpoints += 1
        return checkpoint()
      }

      db.batchAdaptive(n6Operations(db, kind))
      t.true(checkpoints > 0, 'precondition: the batch must hit WAL-full and be retried after a checkpoint')

      const broken: string[] = []
      for (let i = 0; i < RETRIED_ROWS; i++) {
        const row = db.get(Profile, `u${i}`) as any
        if (!row) {
          broken.push(`u${i}: missing`)
        } else if (row.name !== `n${i}` || row.bio !== BIO) {
          broken.push(`u${i}: props lost (name=${JSON.stringify(row.name)}, bio ${row.bio ? 'set' : 'missing'})`)
        }
      }
      t.deepEqual(
        { broken: broken.length, first: broken.slice(0, 3) },
        { broken: 0, first: [] },
        `retried rows must be inserted with full props (${broken.length} of ${RETRIED_ROWS} broken)`,
      )
    } finally {
      db.close()
    }
  })
}

// =============================================================================
// N7: whereNode/whereEdge leak the JS callback forever
// =============================================================================

const N7_SOURCE = `${CHILD_PRELUDE}
const { kiteSync, node, edge, prop } = kite
const User = node('user', { key: (id) => 'user:' + id, props: { name: prop.string('name') } })
const Knows = edge('knows', {})
const db = kiteSync(makeDbPath(), { nodes: [User], edges: [Knows] })
const a = db.insert(User).values('a', { name: 'A' }).returning()
const b = db.insert(User).values('b', { name: 'B' }).returning()
db.link(a, Knows, b)

const collected = new Set()
const registry = new FinalizationRegistry((label) => collected.add(label))
const counts = (() => {
  const control = () => true
  registry.register(control, 'control')
  const nodePredicate = (n) => n.name !== 'nobody'
  registry.register(nodePredicate, 'whereNode callback')
  const edgePredicate = () => true
  registry.register(edgePredicate, 'whereEdge callback')
  const byNode = db.from(a.id).out(Knows).whereNode(nodePredicate)
  registry.register(byNode, 'whereNode traversal')
  const byEdge = db.from(a.id).out(Knows).whereEdge(edgePredicate)
  registry.register(byEdge, 'whereEdge traversal')
  return [byNode.toArray().length, byEdge.toArray().length]
})()

const wanted = ['control', 'whereNode traversal', 'whereEdge traversal', 'whereNode callback', 'whereEdge callback']
for (let i = 0; i < 50 && !wanted.every((label) => collected.has(label)); i++) {
  globalThis.gc()
  await new Promise((resolve) => setTimeout(resolve, 10))
}
report({ counts, collected: [...collected].sort() })
db.close()
`

test('audit N7: whereNode/whereEdge callbacks are released once the traversal is collected', async (t) => {
  const run = await runChild(N7_SOURCE, { exposeGc: true })
  t.is(run.code, 0, describeExit(run))
  const result = run.results[0]
  t.truthy(result, `no result reported; ${describeExit(run)}`)
  t.deepEqual(result.counts, [1, 1], 'sanity: both filtered traversals return the one neighbor')

  const collected: string[] = result.collected
  // Preconditions: GC ran and the traversal objects themselves were collected,
  // so their native side has been dropped.
  t.true(collected.includes('control'), `GC did not collect the control; inconclusive (${collected})`)
  t.true(collected.includes('whereNode traversal'), `traversal not collected; inconclusive (${collected})`)
  t.true(collected.includes('whereEdge traversal'), `traversal not collected; inconclusive (${collected})`)

  t.true(collected.includes('whereNode callback'), `whereNode callback leaked: collected=[${collected}]`)
  t.true(collected.includes('whereEdge callback'), `whereEdge callback leaked: collected=[${collected}]`)
})
