// Reproductions for the wave-3 napi-ts findings T1-T10 (TS wrapper and Kite
// NAPI bindings). Each test encodes the contract a fix must satisfy; they fail
// against the unfixed bindings, except T8: deleteById already returns false for
// missing ids (Kite::delete_node checks node_exists), so its test is a
// regression guard.
//
// `bulkWrite({ chunkSize: NaN })` (T4) spins forever, so it runs in a child
// process that the test kills after a timeout.

import test from 'ava'
import { spawn } from 'node:child_process'
import crypto from 'node:crypto'
import fs from 'node:fs'
import { syncBuiltinESMExports } from 'node:module'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

import {
  bool,
  bulkWrite,
  Database,
  edge,
  float,
  int,
  kite,
  kiteSync,
  node,
  optional,
  string,
  TraversalDirection,
  withDefault,
} from '../ts/index'
import type { Kite, KiteOptions } from '../ts/index'
import {
  createReplicationAdminAuthorizer,
  isReplicationAdminAuthorized,
  type ReplicationAdminAuthConfig,
  type ReplicationAdminAuthRequest,
} from '../ts/replication_transport'

const rayRsDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const tsEntry = path.join(rayRsDir, 'ts', 'index.ts')

const makeDbPath = () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kitedb-w3-napi-ts-'))
  return path.join(dir, 'test.kitedb')
}

const Person = node('person', {
  key: (id: string) => `person:${id}`,
  props: {
    name: string('name'),
    group: optional(string('group')),
    age: optional(int('age')),
  },
})

const Follows = edge('follows', {
  weight: optional(int('weight')),
})

const openKite = (extra: Partial<KiteOptions> = {}): Kite =>
  kiteSync(makeDbPath(), { nodes: [Person], edges: [Follows], ...extra })

type PersonRow = { id: number; key: string; name: string; group?: string; age?: number }

const addPerson = (db: Kite, name: string, props: { group?: string; age?: number } = {}): PersonRow =>
  db
    .insert(Person)
    .values(name, { name, ...props })
    .returning() as unknown as PersonRow

const names = (db: Kite, ids: number[]) =>
  ids.map((id) => (db.getById(id) as PersonRow | null)?.name ?? `#${id}`).sort()

// =============================================================================
// Child process harness (T4 hang)
// =============================================================================

type ChildRun = {
  code: number | null
  signal: string | null
  timedOut: boolean
  stdout: string
  stderr: string
  results: any[]
}

/**
 * Run an ESM snippet in a fresh Node process with the TS entry importable via
 * `await import(process.env.KITE_TS_ENTRY)`. The snippet reports with
 * `fs.writeSync(1, 'RESULT ' + JSON.stringify(x) + '\n')`.
 */
function runChild(source: string, timeoutMs: number): Promise<ChildRun> {
  return new Promise((resolve, reject) => {
    const child = spawn(
      process.execPath,
      ['--import', '@oxc-node/core/register', '--input-type=module', '-e', source],
      {
        cwd: rayRsDir,
        env: { ...process.env, OXC_TSCONFIG_PATH: './__test__/tsconfig.json', KITE_TS_ENTRY: tsEntry },
        stdio: ['ignore', 'pipe', 'pipe'],
      },
    )
    let stdout = ''
    let stderr = ''
    let timedOut = false
    child.stdout.on('data', (chunk) => (stdout += chunk))
    child.stderr.on('data', (chunk) => (stderr += chunk))
    const timer = setTimeout(() => {
      timedOut = true
      child.kill('SIGKILL')
    }, timeoutMs)
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
      resolve({ code, signal, timedOut, stdout, stderr, results })
    })
  })
}

const tail = (text: string, lines = 6) => text.trim().split('\n').slice(-lines).join('\n')

// =============================================================================
// T1: traversal filters run after take(), replace each other, and ignore hops
// =============================================================================

/** A hub with 10 non-matching neighbours (created first) and 10 matching ones. */
function hubWithMixedNeighbours(db: Kite) {
  const hub = addPerson(db, 'hub')
  for (let i = 0; i < 10; i++) {
    const n = addPerson(db, `skip${i}`, { group: 'skip', age: 100 })
    db.link(hub.id, Follows, n.id, { weight: 1 })
  }
  for (let i = 0; i < 10; i++) {
    const n = addPerson(db, `hit${i}`, { group: 'hit', age: i })
    db.link(hub.id, Follows, n.id, { weight: 2 })
  }
  return hub
}

test('w3 T1: whereNode/whereEdge then take(n) returns n matches when more than n match', (t) => {
  const db = openKite()
  try {
    const hub = hubWithMixedNeighbours(db)
    const isHit = (n: any) => n.group === 'hit'

    // Precondition: 10 of the 20 neighbours match.
    t.is(db.from(hub.id).out(Follows).whereNode(isHit).nodes().length, 10)

    const byNode = db.from(hub.id).out(Follows).whereNode(isHit).take(5)
    t.is(byNode.nodes().length, 5, 'whereNode(f).take(5).nodes(): the limit is applied before the filter')
    t.is(byNode.count(), 5, 'whereNode(f).take(5).count()')
    t.is(byNode.toArray().length, 5, 'whereNode(f).take(5).toArray()')
    t.is(byNode.edges().length, 5, 'whereNode(f).take(5).edges()')
    t.true(
      byNode.toArray().every((n: any) => n.group === 'hit'),
      'every returned node must satisfy the filter',
    )

    const byEdge = db
      .from(hub.id)
      .out(Follows)
      .whereEdge((e: any) => e.weight === 2)
      .take(5)
    t.is(byEdge.nodes().length, 5, 'whereEdge(f).take(5).nodes(): the limit is applied before the filter')
  } finally {
    db.close()
  }
})

test('w3 T1: a second whereNode/whereEdge narrows the first instead of replacing it', (t) => {
  const db = openKite()
  try {
    const hub = hubWithMixedNeighbours(db)

    // hit6..hit9 are the only nodes with group 'hit' AND age > 5 (skip* have age 100).
    const both = db
      .from(hub.id)
      .out(Follows)
      .whereNode((n: any) => n.group === 'hit')
      .whereNode((n: any) => n.age > 5)
      .nodes()
    t.deepEqual(names(db, both), ['hit6', 'hit7', 'hit8', 'hit9'], 'the second whereNode replaced the first')

    // Edges with weight 2 lead to hit*; of those, dst age > 5 is hit6..hit9.
    const ages = new Map<number, number>()
    for (const n of db.all(Person) as unknown as PersonRow[]) ages.set(n.id, n.age ?? -1)
    const bothEdges = db
      .from(hub.id)
      .out(Follows)
      .whereEdge((e: any) => e.weight === 2)
      .whereEdge((e: any) => (ages.get(e.dst) ?? -1) > 5)
      .nodes()
    t.deepEqual(names(db, bothEdges), ['hit6', 'hit7', 'hit8', 'hit9'], 'the second whereEdge replaced the first')
  } finally {
    db.close()
  }
})

/** a -> b1 -> c1 and a -> b2 -> c2; the first hop's edges have weight 1 and 2. */
function twoHopTree(db: Kite) {
  const a = addPerson(db, 'a')
  const b1 = addPerson(db, 'b1')
  const b2 = addPerson(db, 'b2')
  const c1 = addPerson(db, 'c1')
  const c2 = addPerson(db, 'c2')
  db.link(a.id, Follows, b1.id, { weight: 1 })
  db.link(a.id, Follows, b2.id, { weight: 2 })
  db.link(b1.id, Follows, c1.id, { weight: 9 })
  db.link(b2.id, Follows, c2.id, { weight: 9 })
  return { a, b1, b2, c1, c2 }
}

test('w3 T1: a whereNode between hops filters that hop, not the last one', (t) => {
  const db = openKite()
  try {
    const { a } = twoHopTree(db)
    const viaB1 = db
      .from(a.id)
      .out(Follows)
      .whereNode((n: any) => n.name === 'b1')
      .out(Follows)
      .nodes()
    t.deepEqual(names(db, viaB1), ['c1'], 'whereNode after the first out() must keep only paths through b1')
  } finally {
    db.close()
  }
})

test('w3 T1: a whereEdge between hops filters that hop, not the last one', (t) => {
  const db = openKite()
  try {
    const { a } = twoHopTree(db)
    const viaWeight1 = db
      .from(a.id)
      .out(Follows)
      .whereEdge((e: any) => e.weight === 1)
      .out(Follows)
      .nodes()
    t.deepEqual(names(db, viaWeight1), ['c1'], 'whereEdge after the first out() must keep only the weight-1 first hop')
  } finally {
    db.close()
  }
})

// =============================================================================
// T2: withDefault() breaks kite(); strict_schema is not exposed
// =============================================================================

const Task = node('task', {
  key: (id: string) => `task:${id}`,
  props: {
    title: string('title'),
    status: withDefault(string('status'), 'active'),
    priority: withDefault(int('priority'), 3),
    done: withDefault(bool('done'), false),
    score: withDefault(float('score'), 0.5),
  },
})

test('w3 T2: withDefault() (documented) is accepted by kite()/kiteSync() and applied on insert', async (t) => {
  let db: Kite | undefined
  let openError: Error | undefined
  try {
    db = kiteSync(makeDbPath(), { nodes: [Task], edges: [] })
  } catch (err) {
    openError = err as Error
  }
  t.is(openError, undefined, `kiteSync() rejected withDefault(): ${openError?.message}`)

  let asyncError: Error | undefined
  try {
    const asyncDb = await kite(makeDbPath(), { nodes: [Task], edges: [] })
    asyncDb.close()
  } catch (err) {
    asyncError = err as Error
  }
  t.is(asyncError, undefined, `kite() rejected withDefault(): ${asyncError?.message}`)

  if (!db) return
  try {
    db.insert(Task)
      .values('t1', { title: 'first' } as any)
      .execute()
    t.like(db.get(Task, 't1') as object, { title: 'first', status: 'active', priority: 3, done: false, score: 0.5 })
  } finally {
    db.close()
  }
})

const StrictUser = node('strict_user', {
  key: (id: string) => `strict_user:${id}`,
  props: {
    name: string('name'),
    age: optional(int('age')),
  },
})

test('w3 T2: the strictSchema option is exposed and enforces required props and prop types', (t) => {
  const options = { nodes: [StrictUser], edges: [], strictSchema: true } as KiteOptions & { strictSchema: boolean }
  const db = kiteSync(makeDbPath(), options)
  try {
    t.notThrows(() => db.insert(StrictUser).values('ok', { name: 'Ok', age: 3 }).execute())
    t.throws(
      () =>
        db
          .insert(StrictUser)
          .values('missing', { age: 3 } as any)
          .execute(),
      { message: /name/ },
      'strictSchema: a required (non-optional) prop must be enforced on insert',
    )
    t.throws(
      () =>
        db
          .insert(StrictUser)
          .values('badtype', { name: 42 } as any)
          .execute(),
      { message: /name/ },
      'strictSchema: a declared prop type must be enforced on insert',
    )
  } finally {
    db.close()
  }

  // Default (non-strict) mode keeps accepting both.
  const lenient = kiteSync(makeDbPath(), { nodes: [StrictUser], edges: [] })
  try {
    t.notThrows(() =>
      lenient
        .insert(StrictUser)
        .values('missing', { age: 3 } as any)
        .execute(),
    )
  } finally {
    lenient.close()
  }
})

// =============================================================================
// T3: KitePath.direction() ignores 'In' and unknown values; dijkstra is unweighted
// =============================================================================

test('w3 T3: KitePath.direction() honours TraversalDirection.In and any casing, and rejects unknown values', (t) => {
  const db = openKite()
  try {
    const a = addPerson(db, 'a')
    const b = addPerson(db, 'b')
    db.link(a.id, Follows, b.id) // only a -> b

    // b reaches a only against the edge direction.
    t.false(db.path(b.id, a.id).via(Follows).bfs().found, 'precondition: default direction is out')
    t.true(db.path(b.id, a.id).via(Follows).direction('in').bfs().found, "precondition: 'in' works")

    const accepted = [TraversalDirection.In, 'In', 'IN', TraversalDirection.Both, 'Both', 'BOTH'] as string[]
    for (const direction of accepted) {
      const result = db.path(b.id, a.id).via(Follows).direction(direction).bfs()
      t.true(result.found, `direction(${JSON.stringify(direction)}) was treated as 'out'`)
      const viaBuilder = db.shortestPath(b.id).via(Follows).to(a.id).direction(direction).bfs()
      t.true(viaBuilder.found, `shortestPath().direction(${JSON.stringify(direction)}) was treated as 'out'`)
    }

    for (const bogus of ['sideways', 'inbound', '']) {
      t.throws(
        () => db.path(b.id, a.id).via(Follows).direction(bogus).bfs(),
        { message: /direction/i },
        `direction(${JSON.stringify(bogus)}) must throw, not silently fall back to 'out'`,
      )
    }
  } finally {
    db.close()
  }
})

const Road = edge('road', { cost: int('cost') })

test('w3 T3: Kite dijkstra() can weight edges by an edge prop', (t) => {
  const db = kiteSync(makeDbPath(), { nodes: [Person], edges: [Road] })
  try {
    const a = addPerson(db, 'a')
    const b = addPerson(db, 'b')
    const c = addPerson(db, 'c')
    db.link(a.id, Road, b.id, { cost: 10 }) // direct, expensive
    db.link(a.id, Road, c.id, { cost: 1 }) // detour, cheap
    db.link(c.id, Road, b.id, { cost: 1 })

    // Unweighted behaviour stays the default.
    const unweighted = db.path(a.id, b.id).via(Road).dijkstra()
    t.deepEqual(unweighted.path, [a.id, b.id])
    t.is(unweighted.totalWeight, 1)

    const path = db.path(a.id, b.id).via(Road) as any
    t.is(typeof path.weight, 'function', 'KitePath has no weight-by-edge-prop option: dijkstra() always uses weight 1')
    if (typeof path.weight !== 'function') return
    const weighted = path.weight('cost').dijkstra()
    t.deepEqual(weighted.path, [a.id, c.id, b.id], 'weighted dijkstra must take the cheap detour')
    t.is(weighted.totalWeight, 2)

    const k = (db.path(a.id, b.id).via(Road) as any).weight('cost').kShortest(2)
    t.deepEqual(
      k.map((p: any) => [p.path, p.totalWeight]),
      [
        [[a.id, c.id, b.id], 2],
        [[a.id, b.id], 10],
      ],
    )

    const builder = db.shortestPath(a.id).via(Road).to(b.id) as any
    t.is(typeof builder.weight, 'function', 'shortestPath() builder has no weight-by-edge-prop option')
    if (typeof builder.weight !== 'function') return
    const viaBuilder = builder.weight('cost').dijkstra()
    t.deepEqual(viaBuilder.path, [a.id, c.id, b.id])
    t.is(viaBuilder.totalWeight, 2)
  } finally {
    db.close()
  }
})

// =============================================================================
// T4: NaN/invalid bulkWrite/batchAdaptive options
// =============================================================================

const BULK_WRITE_NAN_SOURCE = `
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
const { Database, bulkWrite } = await import(process.env.KITE_TS_ENTRY)
const report = (value) => fs.writeSync(1, 'RESULT ' + JSON.stringify(value) + '\\n')
const db = Database.open(path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'kitedb-w3-t4-')), 'test.kitedb'), {})
const ops = [(d) => d.createNode('a'), (d) => d.createNode('b')]
report({ phase: 'start' })
try {
  bulkWrite(db, ops, { chunkSize: NaN })
  report({ phase: 'done', threw: false, nodes: db.countNodes() })
} catch (err) {
  report({ phase: 'done', threw: true, message: String(err && err.message), nodes: db.countNodes() })
}
db.close()
`

test('w3 T4: bulkWrite({ chunkSize: NaN }) throws instead of looping forever', async (t) => {
  const run = await runChild(BULK_WRITE_NAN_SOURCE, 20_000)
  t.true(
    run.results.some((r) => r.phase === 'start'),
    `child did not start:\n${tail(run.stderr)}`,
  )
  t.false(
    run.timedOut,
    'bulkWrite({ chunkSize: NaN }) never returned (killed after 20s): it loops begin/commit forever',
  )
  const done = run.results.find((r) => r.phase === 'done')
  t.truthy(done, `child exited code=${run.code} signal=${run.signal} without finishing\n${tail(run.stderr)}`)
  if (!done) return
  t.true(done.threw, 'bulkWrite({ chunkSize: NaN }) must throw')
  t.regex(String(done.message), /chunkSize/)
  t.is(done.nodes, 0, 'no operation may run when the options are invalid')
})

test('w3 T4: bulkWrite rejects a non-integer chunkSize', (t) => {
  const db = Database.open(makeDbPath(), {})
  try {
    const ops = [(d: Database) => d.createNode('a'), (d: Database) => d.createNode('b')]
    t.throws(() => bulkWrite(db, ops, { chunkSize: 1.5 }), { message: /chunkSize/ })
    t.is(db.countNodes(), 0)
  } finally {
    db.close()
  }
})

test('w3 T4: batchAdaptive rejects NaN/non-integer maxBatch and minBatch instead of dropping every op', (t) => {
  const db = openKite()
  try {
    const ops = () => Array.from({ length: 5 }, (_, i) => db.insert(Person).values(`p${i}`, { name: `p${i}` }))
    let result: unknown
    const error = t.throws(() => {
      result = db.batchAdaptive(ops(), { maxBatch: NaN })
    })
    t.is(
      error?.message.match(/maxBatch/)?.[0],
      'maxBatch',
      `batchAdaptive({ maxBatch: NaN }) must throw a maxBatch error; it returned ${JSON.stringify(result)} ` +
        `and wrote ${db.countNodes(Person)} of 5 nodes`,
    )
    t.throws(() => db.batchAdaptive(ops(), { minBatch: NaN }), { message: /minBatch/ })
    t.throws(() => db.batchAdaptive(ops(), { maxBatch: 2.5 }), { message: /maxBatch/ })
    t.is(db.countNodes(Person), 0, 'no operation may run when the options are invalid')
  } finally {
    db.close()
  }
})

// =============================================================================
// T5: node() key functions only contribute their prefix
// =============================================================================

test('w3 T5: node() rejects a key function that adds text after the id', (t) => {
  // Stored keys would silently become `user:<id>`, not `user:<id>:v2`.
  t.throws(
    () => node('versioned_user', { key: (id: string) => `user:${id}:v2` }),
    { message: /key/i },
    'a key function is probed for its prefix only; a suffix must be rejected at definition',
  )
  // Prefix-only key functions keep working.
  t.deepEqual(node('plain_user', { key: (id: string) => `user:${id}` }).key, { kind: 'prefix', prefix: 'user:' })
})

test('w3 T5: a validating key function gets a clear prefix-only error at definition', (t) => {
  const numericOnly = (id: string) => {
    if (!/^\d+$/.test(id)) throw new Error('order id must be numeric')
    return `order:${id}`
  }
  let spec: ReturnType<typeof node> | undefined
  let error: Error | undefined
  try {
    spec = node('order', { key: numericOnly })
  } catch (err) {
    error = err as Error
  }
  if (error) {
    t.regex(
      error.message,
      /prefix/i,
      `node() leaked the key function's own probe failure ("${error.message}") instead of explaining ` +
        'that key functions are probed once for a prefix',
    )
  } else {
    t.deepEqual(spec?.key, { kind: 'prefix', prefix: 'order:' })
  }
})

// =============================================================================
// T6: edge props named src/dst/etype shadow edge identity in whereEdge
// =============================================================================

const CLEAR_RESERVED_EDGE_ERROR = /src|dst|etype|reserved/i

test('w3 T6: edge props named src/dst/etype do not shadow edge identity in whereEdge', (t) => {
  // Schema-declared props: rejecting the names at definition is acceptable.
  const Shadow = edge('shadow', { src: string('src'), dst: string('dst'), etype: string('etype') })
  const Plain = edge('plain', {})
  let db: Kite | undefined
  let defineError: Error | undefined
  try {
    db = kiteSync(makeDbPath(), { nodes: [Person], edges: [Shadow, Plain] })
  } catch (err) {
    defineError = err as Error
  }
  if (defineError) {
    t.regex(defineError.message, CLEAR_RESERVED_EDGE_ERROR)
    db = kiteSync(makeDbPath(), { nodes: [Person], edges: [Plain] })
  }

  try {
    const a = addPerson(db!, 'a')
    const b = addPerson(db!, 'b')
    const spoof = { src: 'spoof-src', dst: 'spoof-dst', etype: 'spoof-etype' }
    const edgeTypes = defineError ? [Plain] : [Shadow, Plain]

    for (const edgeType of edgeTypes) {
      let linkError: Error | undefined
      try {
        db!.link(a.id, edgeType, b.id, spoof)
      } catch (err) {
        linkError = err as Error
      }
      if (linkError) {
        t.regex(linkError.message, CLEAR_RESERVED_EDGE_ERROR, `${edgeType.name}: unclear link() rejection`)
        continue
      }

      const seen: Array<{ src: unknown; dst: unknown; etype: unknown }> = []
      db!
        .from(a.id)
        .out(edgeType)
        .whereEdge((e: any) => {
          seen.push({ src: e.src, dst: e.dst, etype: typeof e.etype })
          return true
        })
        .nodes()
      t.deepEqual(
        seen,
        [{ src: a.id, dst: b.id, etype: 'number' }],
        `${edgeType.name}: edge props overwrote the src/dst/etype identity passed to whereEdge`,
      )
    }
  } finally {
    db!.close()
  }
})

// =============================================================================
// T7: batchAdaptive stops checkpointing after a retry fails twice
// =============================================================================

// The engine writes a transaction's records to the WAL as each op runs, so a
// batch that overflows leaves the WAL full when it rolls back. This stub WAL
// models exactly that, so the test drives batchAdaptive's retry loop alone:
// after shrinking the batch, the next WAL-full failure must checkpoint again.
const WAL_FULL_MESSAGE = 'WAL buffer full: checkpoint required before continuing writes'

test('w3 T7: batchAdaptive checkpoints again after a shrunken retry hits WAL-full', (t) => {
  const db = openKite()
  try {
    const capacity = 4
    let used = 0
    let checkpoints = 0
    const attempts: string[] = []
    const stub = db as unknown as {
      batch: (ops: number[]) => number[]
      checkpoint: () => void
      hasTransaction: () => boolean
    }
    stub.batch = (ops) => {
      if (used + ops.length > capacity) {
        attempts.push(`${ops.length}:full`)
        used = capacity // the rolled-back records stay in the WAL until a checkpoint
        throw new Error(WAL_FULL_MESSAGE)
      }
      attempts.push(`${ops.length}:ok`)
      used += ops.length
      return ops
    }
    stub.checkpoint = () => {
      attempts.push('checkpoint')
      checkpoints += 1
      used = 0
    }
    stub.hasTransaction = () => false

    const ops = Array.from({ length: 10 }, (_, i) => i)
    let result: unknown
    let error: Error | undefined
    try {
      result = db.batchAdaptive(ops, { maxBatch: 8 })
    } catch (err) {
      error = err as Error
    }
    t.is(
      error,
      undefined,
      `batchAdaptive gave up with "${error?.message}" after ${checkpoints} checkpoint(s); ` +
        `attempts: ${attempts.join(' ')}`,
    )
    t.deepEqual(result, ops)
    t.true(checkpoints >= 2, `expected a checkpoint after the batch was shrunk; attempts: ${attempts.join(' ')}`)
  } finally {
    db.close()
  }
})

// =============================================================================
// T8: deleteById returns true for ids that do not exist
// =============================================================================

test('w3 T8: deleteById returns false for ids that do not exist', (t) => {
  const db = openKite()
  try {
    const a = addPerson(db, 'a')
    t.false(db.deleteById(a.id + 1000), 'deleteById(never-created id) returned true')
    t.true(db.deleteById(a.id))
    t.false(db.deleteById(a.id), 'deleteById(already deleted id) returned true')
    db.checkpoint()
    t.false(db.deleteById(a.id), 'deleteById(deleted id) after checkpoint returned true')
    t.false(db.deleteByKey(Person, 'nobody'))
  } finally {
    db.close()
  }
})

// =============================================================================
// T9: Database has no async-transaction tracking
// =============================================================================

function deferred(): { promise: Promise<void>; resolve: () => void } {
  let resolve!: () => void
  const promise = new Promise<void>((r) => (resolve = r))
  return { promise, resolve }
}

type DatabaseWithTransaction = Database & {
  transaction?: <T>(fn: (db: Database) => T | Promise<T>) => T | Promise<T>
}

/** Start an async Database transaction that writes `a`, waits for `gate`, then fails. */
function failingDatabaseTransaction(db: DatabaseWithTransaction, gate: Promise<void>): Promise<void> {
  return db.transaction!(async (tx) => {
    tx.createNode('a')
    await gate
    throw new Error('A failed')
  }) as Promise<void>
}

test('w3 T9: Database.transaction(async) exists and isolates writes from other async contexts', async (t) => {
  const db = Database.open(makeDbPath(), {}) as DatabaseWithTransaction
  try {
    t.is(
      typeof db.transaction,
      'function',
      'Database has no transaction() helper with async-context tracking (Kite has one)',
    )
    if (typeof db.transaction !== 'function') return

    // Sync transactions commit and roll back.
    db.transaction(() => db.createNode('sync-ok'))
    t.not(db.getNodeByKey('sync-ok'), null)
    t.throws(() =>
      db.transaction!(() => {
        db.createNode('sync-bad')
        throw new Error('boom')
      }),
    )
    t.is(db.getNodeByKey('sync-bad'), null)

    // Another request writes while A is suspended at an await.
    const gate = deferred()
    const txA = failingDatabaseTransaction(db, gate.promise)
    let outsideError: unknown
    try {
      db.createNode('b')
    } catch (err) {
      outsideError = err
    }

    gate.resolve()
    await t.throwsAsync(txA, { message: 'A failed' })
    t.is(db.getNodeByKey('a'), null, "A's own write must roll back")
    if (outsideError === undefined) {
      t.not(db.getNodeByKey('b'), null, 'the outside write returned success, so it must survive the rollback of A')
    } else {
      t.regex(String((outsideError as Error).message), /transaction/i, 'a rejected outside write must say why')
    }
  } finally {
    db.close()
  }
})

test('w3 T9: a concurrent async Database.transaction() does not join another open one', async (t) => {
  const db = Database.open(makeDbPath(), {}) as DatabaseWithTransaction
  try {
    t.is(typeof db.transaction, 'function', 'Database has no transaction() helper with async-context tracking')
    if (typeof db.transaction !== 'function') return

    const gate = deferred()
    const txA = failingDatabaseTransaction(db, gate.promise)
    let txB: Promise<string>
    try {
      txB = db.transaction(async (tx) => {
        tx.createNode('b')
        return 'B committed'
      }) as Promise<string>
    } catch (err) {
      txB = Promise.reject(err)
    }

    gate.resolve()
    const [a, b] = await Promise.allSettled([txA, txB])
    t.is(a.status, 'rejected')
    t.is(db.getNodeByKey('a'), null, "A's own write must roll back")
    if (b.status === 'fulfilled') {
      t.is(b.value, 'B committed')
      t.not(db.getNodeByKey('b'), null, "B's transaction resolved, so its write must be committed independently of A")
    } else {
      t.regex(String((b.reason as Error)?.message), /transaction/i, 'a rejected transaction must say why')
    }
  } finally {
    db.close()
  }
})

// =============================================================================
// T10: replication admin auth helper
// =============================================================================

const authRequest = (headers: Record<string, string | undefined> = {}): ReplicationAdminAuthRequest => ({ headers })

/** True when the config denies the request, by returning false or by throwing. */
function denies(request: ReplicationAdminAuthRequest, config: ReplicationAdminAuthConfig): boolean {
  try {
    return isReplicationAdminAuthorized(request, config) === false
  } catch {
    return true
  }
}

type TrustingAuthConfig = ReplicationAdminAuthConfig & { trustForwardedClientCert?: boolean }

test('w3 T10: the bearer token is compared with crypto.timingSafeEqual', (t) => {
  const config: ReplicationAdminAuthConfig = { mode: 'token', token: 'abc123' }
  const original = crypto.timingSafeEqual
  let calls = 0
  crypto.timingSafeEqual = ((a: NodeJS.ArrayBufferView, b: NodeJS.ArrayBufferView) => {
    calls += 1
    return original(a, b)
  }) as typeof crypto.timingSafeEqual
  syncBuiltinESMExports()
  let outcomes: unknown[]
  try {
    outcomes = [
      isReplicationAdminAuthorized(authRequest({ authorization: 'Bearer abc123' }), config),
      isReplicationAdminAuthorized(authRequest({ authorization: 'Bearer abc124' }), config),
      // Different length: must deny, not throw from timingSafeEqual.
      isReplicationAdminAuthorized(authRequest({ authorization: 'Bearer x' }), config),
    ]
  } finally {
    crypto.timingSafeEqual = original
    syncBuiltinESMExports()
  }
  t.deepEqual(outcomes, [true, false, false])
  t.true(calls > 0, 'the bearer token is compared with ===, which leaks timing; use crypto.timingSafeEqual')
})

test('w3 T10: a client-supplied forwarded client-cert header alone does not authorize', (t) => {
  const xfcc = authRequest({ 'x-forwarded-client-cert': 'CN=anyone' })
  t.true(denies(xfcc, { mode: 'mtls' }), "mode 'mtls': mere presence of x-forwarded-client-cert authorized")
  t.true(
    denies(xfcc, { mode: 'token_or_mtls', token: 'abc123' }),
    "mode 'token_or_mtls': mere presence of x-forwarded-client-cert authorized without the token",
  )
  t.true(
    denies(authRequest({ 'x-client-cert': 'CN=anyone' }), {
      mode: 'token_or_mtls',
      token: 'abc123',
      mtlsHeader: 'x-client-cert',
    }),
    'a custom mtlsHeader without trustForwardedClientCert and a subject matcher authorized',
  )
  t.true(
    denies(xfcc, { mode: 'mtls', trustForwardedClientCert: true } as TrustingAuthConfig),
    'trustForwardedClientCert without a subject matcher must not accept any certificate',
  )

  // Explicitly trusted header plus an anchored subject regex keeps working.
  const trusted: TrustingAuthConfig = {
    mode: 'mtls',
    trustForwardedClientCert: true,
    mtlsSubjectRegex: /^CN=replication-admin,O=RayDB$/,
  }
  t.true(
    isReplicationAdminAuthorized(authRequest({ 'x-forwarded-client-cert': 'CN=replication-admin,O=RayDB' }), trusted),
  )
  t.true(denies(authRequest({ 'x-forwarded-client-cert': 'CN=viewer,O=RayDB' }), trusted))
  // A custom mtlsMatcher stays the caller's decision.
  t.true(isReplicationAdminAuthorized(authRequest({}), { mode: 'mtls', mtlsMatcher: () => true }))
})

test('w3 T10: the mTLS subject regex must match the whole subject', (t) => {
  const config: TrustingAuthConfig = {
    mode: 'mtls',
    trustForwardedClientCert: true,
    mtlsSubjectRegex: /CN=replication-admin/,
  }
  const spoofed = authRequest({ 'x-forwarded-client-cert': 'CN=attacker,OU=CN=replication-admin' })
  t.true(denies(spoofed, config), 'an unanchored subject regex matched a substring of an attacker-controlled subject')
})

test('w3 T10: an auth config without a mode denies instead of allowing everything', (t) => {
  t.true(denies(authRequest(), {}), "no mode configured: the default 'none' authorizes every request")
  t.true(
    denies(authRequest(), { token: 'abc123' }),
    'a token without a mode: requests without the token are authorized',
  )
  t.throws(
    () => createReplicationAdminAuthorizer({})(authRequest()),
    undefined,
    'createReplicationAdminAuthorizer({}) allowed a request',
  )
  // An explicit opt-out stays possible.
  t.true(isReplicationAdminAuthorized(authRequest(), { mode: 'none' }))
})
