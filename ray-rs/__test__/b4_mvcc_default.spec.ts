// b4 mvcc-default lane: MVCC is the default for the low-level Database and for
// kite()/kiteSync(); `mvcc: false` (deprecated) still opts out. Bulk loads
// (`beginBulk`, `bulkWrite`) work under MVCC.

import test from 'ava'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'

import { bulkWrite, Database, edge, kite, kiteSync, node, optional, string } from '../ts/index'

const makeDbPath = () =>
  path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'kitedb-b4-mvcc-default-')), 'test.kitedb')

const Person = node('person', {
  key: (id: string) => `person:${id}`,
  props: { name: optional(string('name')) },
})
const Knows = edge('knows', {})

test('Database.open enables MVCC by default; mvcc: false opts out', (t) => {
  const db = Database.open(makeDbPath())
  try {
    t.truthy(db.stats().mvccStats, 'the default open has MVCC')
  } finally {
    db.close()
  }
  const plain = Database.open(makeDbPath(), { mvcc: false })
  try {
    t.is(plain.stats().mvccStats, undefined, 'mvcc: false has no MVCC')
  } finally {
    plain.close()
  }
})

test('kite() and kiteSync() enable MVCC by default; mvcc: false opts out', async (t) => {
  const asyncDb = await kite(makeDbPath(), { nodes: [Person], edges: [Knows] })
  try {
    t.truthy(asyncDb.stats().mvccStats, 'kite() has MVCC')
  } finally {
    asyncDb.close()
  }
  const syncDb = kiteSync(makeDbPath(), { nodes: [Person], edges: [Knows] })
  try {
    t.truthy(syncDb.stats().mvccStats, 'kiteSync() has MVCC')
  } finally {
    syncDb.close()
  }
  const plain = kiteSync(makeDbPath(), { nodes: [Person], edges: [Knows], mvcc: false })
  try {
    t.is(plain.stats().mvccStats, undefined, 'mvcc: false has no MVCC')
  } finally {
    plain.close()
  }
})

test('beginBulk works under the default (MVCC)', (t) => {
  const db = Database.open(makeDbPath())
  try {
    db.beginBulk()
    db.createNode('bulk-a')
    db.createNode('bulk-b')
    db.commit()
    t.not(db.getNodeByKey('bulk-a'), null)
    t.not(db.getNodeByKey('bulk-b'), null)
  } finally {
    db.close()
  }
})

test('Kite.beginBulk works under the default (MVCC)', (t) => {
  const db = kiteSync(makeDbPath(), { nodes: [Person], edges: [Knows] })
  try {
    db.beginBulk()
    db.commit()
    t.truthy(db.stats().mvccStats)
  } finally {
    db.close()
  }
})

test('bulkWrite surfaces a failing beginBulk instead of retrying in a normal transaction', (t) => {
  const calls: Array<string> = []
  const failing = {
    hasTransaction: () => false,
    beginBulk: (): number => {
      throw new Error('bulk refused')
    },
    begin: (): number => {
      calls.push('begin')
      return 1
    },
    commit: () => calls.push('commit'),
    rollback: () => calls.push('rollback'),
    shouldCheckpoint: () => false,
    checkpoint: () => calls.push('checkpoint'),
  }
  t.throws(() => bulkWrite(failing as unknown as Database, [() => 1]), { message: /bulk refused/ })
  t.deepEqual(calls, [], 'bulkWrite fell back to a normal transaction')
})
