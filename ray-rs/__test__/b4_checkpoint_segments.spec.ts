// raydb-b4 checkpoint-segments: checkpoints through the bindings.
//
// A full WAL spills into WAL segments, and automatic checkpoints run (on a
// thread of the database's own, or inline without it) once the log reaches
// the checkpoint trigger: a load past it installs new snapshots, and
// `checkpointError()` stays null. Without automatic checkpoints the WAL
// spills up to `walSegmentLimit`, then writes fail with a WAL-full error
// until a checkpoint makes room. Out-of-range options are refused, and the
// deprecated `checkpointThreshold` is still accepted.

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
    const generation = db.stats().snapshotGen
    // About 3 MiB of log: many spills, and checkpoints on the thread.
    for (let i = 0; i < 10_000; i += 1) {
      db.begin()
      db.createNode(`n-${i}-${KEY}`)
      db.commit()
    }
    t.true(db.stats().snapshotGen > generation, 'no checkpoint installed a snapshot during the load')
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

test('without automatic checkpoints, writes past the segment limit fail until a checkpoint', (t) => {
  const db = Database.open(makeDbPath(), {
    walSize: 64 * 1024,
    autoCheckpoint: false,
    walSegmentLimit: 128 * 1024,
  })
  try {
    let failure: unknown = null
    let written = 0
    for (let i = 0; i < 10_000 && failure === null; i += 1) {
      try {
        db.begin()
        db.createNode(`n-${i}-${KEY}`)
        db.commit()
        written += 1
      } catch (error) {
        failure = error
        try {
          db.rollback()
        } catch {
          // The failed commit ended the transaction.
        }
      }
    }
    t.true(written > 400, `only ${written} commits before the limit`)
    t.regex(String((failure as Error | null)?.message), /WAL buffer full/)
    db.checkpoint()
    db.begin()
    db.createNode('after-the-checkpoint')
    db.commit()
    t.truthy(db.get_node_by_key(`n-${written - 1}-${KEY}`))
    t.truthy(db.get_node_by_key('after-the-checkpoint'))
  } finally {
    db.close()
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

test('Kite checkpoints inline without the thread past the trigger, with no error', (t) => {
  const Item = node('item', { key: (id: string) => `item:${id}`, props: { name: string('name') } })
  const db = kiteSync(makeDbPath(), {
    nodes: [Item],
    edges: [],
    checkpointThread: false,
    checkpointLogRatio: 0.25,
    // A 64 KiB trigger: the load below passes it many times.
    checkpointLogBudget: 64 * 1024,
    walSegmentSize: 1024 * 1024,
    walSegmentLimit: 16 * 1024 * 1024,
  })
  try {
    const generation = db.stats().snapshotGen
    for (let i = 0; i < 3_000; i += 1) {
      db.insert(Item).values(`i${i}`, { name: `item ${i} ${KEY}` }).execute()
    }
    t.true(db.stats().snapshotGen > generation, 'no checkpoint installed a snapshot during the load')
    t.is(db.checkpointError(), null)
    t.truthy(db.get(Item, 'i2999'))
  } finally {
    db.close()
  }
})
