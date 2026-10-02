// b4 commit-pipeline lane: savepoints on the low-level Database.
//
// `savepoint()` marks the current write transaction; `rollbackTo(savepoint)`
// undoes what it did since (and keeps the savepoint); `releaseSavepoint`
// keeps it. Rolled-back writes never reach the WAL.

import test from 'ava'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'

import { Database } from '../ts/index'

const makeDbPath = () =>
  path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'kitedb-b4-commit-pipeline-')), 'test.kitedb')

test('rollbackTo undoes the writes since the savepoint; the rest commits', (t) => {
  const dbPath = makeDbPath()
  const db = Database.open(dbPath)
  try {
    db.begin()
    db.createNode('kept')
    const savepoint = db.savepoint()
    db.createNode('rolled-back')
    db.rollbackTo(savepoint)
    t.is(db.getNodeByKey('rolled-back'), null)
    db.createNode('after')
    db.releaseSavepoint(savepoint)
    db.commit()
    t.not(db.getNodeByKey('kept'), null)
    t.not(db.getNodeByKey('after'), null)
    t.is(db.getNodeByKey('rolled-back'), null)
  } finally {
    db.close()
  }

  // Reopening replays the WAL: no rolled-back record comes back.
  const reopened = Database.open(dbPath)
  try {
    t.not(reopened.getNodeByKey('kept'), null)
    t.is(reopened.getNodeByKey('rolled-back'), null)
  } finally {
    reopened.close()
  }
})

test('a released or ended savepoint cannot be used again', (t) => {
  const db = Database.open(makeDbPath())
  try {
    t.throws(() => db.savepoint(), { message: /No active transaction/ })
    db.begin()
    const outer = db.savepoint()
    const inner = db.savepoint()
    db.rollbackTo(outer)
    t.throws(() => db.rollbackTo(inner), { message: /Savepoint is not live/ })
    db.releaseSavepoint(outer)
    t.throws(() => db.releaseSavepoint(outer), { message: /released already/ })
    t.throws(() => db.rollbackTo(outer), { message: /it was released/ })
    db.commit()
  } finally {
    db.close()
  }
})
