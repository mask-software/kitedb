// raydb-b4 checkpoint-segments: the checkpoint options of the bindings.
//
// A full WAL spills into WAL segments, and automatic checkpoints run on a
// thread of the database's own once the log reaches the checkpoint trigger.
// The options that tune them reach the core, out-of-range values are refused,
// the deprecated `checkpointThreshold` is still accepted, and
// `checkpointError()` reports the last automatic checkpoint's failure (none
// here).

import test from 'ava'

import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'

import { Database } from '../index'
import { kiteSync, node, string } from '../ts/index'

const makeDbPath = () => path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'kitedb-segments-')), 'test.kitedb')

const KEY = 'k'.repeat(200)

test('a small WAL spills and checkpoints with the log options; every commit stays', (t) => {
  const dbPath = makeDbPath()
  const options = {
    walSize: 64 * 1024,
    checkpointThread: true,
    checkpointLogRatio: 0.5,
    checkpointLogBudget: 1024 * 1024,
    walSegmentSize: 256 * 1024,
    walSegmentLimit: 8 * 1024 * 1024,
  }
  const db = Database.open(dbPath, options)
  try {
    // About 3 MiB of log: many spills, and checkpoints on the thread.
    for (let i = 0; i < 10_000; i += 1) {
      db.begin()
      db.createNode(`n-${i}-${KEY}`)
      db.commit()
    }
    t.is(db.checkpointError(), null)
  } finally {
    db.close()
  }

  const reopened = Database.open(dbPath, options)
  try {
    t.truthy(reopened.get_node_by_key(`n-0-${KEY}`))
    t.truthy(reopened.get_node_by_key(`n-9999-${KEY}`))
  } finally {
    reopened.close()
  }
})

test('out-of-range checkpoint options are refused; the deprecated threshold is accepted', (t) => {
  for (const options of [
    { checkpointLogRatio: -1 },
    { checkpointLogRatio: Number.NaN },
    { checkpointLogBudget: 0 },
    { walSegmentSize: 1.5 },
    { walSegmentLimit: -1 },
    { checkpointThreshold: 2 },
  ]) {
    t.throws(() => Database.open(makeDbPath(), options), undefined, JSON.stringify(options))
  }
  const db = Database.open(makeDbPath(), { checkpointThreshold: 0.5 })
  db.close()
  t.pass()
})

test('Kite takes the checkpoint options and reports no checkpoint error', (t) => {
  const Item = node('item', { key: (id: string) => `item:${id}`, props: { name: string('name') } })
  const db = kiteSync(makeDbPath(), {
    nodes: [Item],
    edges: [],
    checkpointThread: false,
    checkpointLogRatio: 0.25,
    checkpointLogBudget: 4 * 1024 * 1024,
    walSegmentSize: 1024 * 1024,
    walSegmentLimit: 16 * 1024 * 1024,
  })
  try {
    for (let i = 0; i < 100; i += 1) {
      db.insert(Item).values(`i${i}`, { name: `item ${i}` }).execute()
    }
    t.is(db.checkpointError(), null)
    t.truthy(db.get(Item, 'i99'))
  } finally {
    db.close()
  }
})
