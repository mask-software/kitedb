// b4 cache lane: the cache layer was removed. Its open options are accepted and
// ignored (out-of-range values included), and the cache* methods plus the cache
// metrics are deprecated no-op stubs kept for one release so existing callers
// keep working.

import test from 'ava'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'

import { Database, PropType, collectMetrics, healthCheck } from '../index'

const makeDbPath = () => path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'kitedb-b4-cache-')), 'test.kitedb')

const cacheOptions = {
  cacheEnabled: true,
  cacheMaxNodeProps: -1,
  cacheMaxEdgeProps: 0,
  cacheMaxTraversalEntries: 1,
  cacheMaxQueryEntries: 2 ** 40,
  cacheQueryTtlMs: -1,
}

test('cache open options are accepted and ignored', (t) => {
  const dbPath = makeDbPath()
  const db = Database.open(dbPath, cacheOptions)
  db.begin()
  const a = db.createNode('a')
  const name = db.get_or_create_propkey('name')
  db.setNodeProp(a, name, { propType: PropType.String, stringValue: 'first' })
  db.commit()
  t.is(db.get_node_prop(a, name)?.stringValue, 'first')
  db.begin()
  db.setNodeProp(a, name, { propType: PropType.String, stringValue: 'second' })
  db.commit()
  t.is(db.get_node_prop(a, name)?.stringValue, 'second')
  db.close()

  const reopened = Database.open(dbPath, cacheOptions)
  t.is(reopened.get_node_by_key('a'), a)
  t.is(reopened.get_node_prop(a, name)?.stringValue, 'second')
  reopened.close()
})

test('deprecated cache methods are no-op stubs', (t) => {
  const db = Database.open(makeDbPath(), { cacheEnabled: true })
  db.begin()
  const a = db.createNode('a')
  db.commit()

  t.false(db.cacheIsEnabled())
  t.is(db.cacheStats(), null)
  t.notThrows(() => {
    db.cacheInvalidateNode(a)
    db.cacheInvalidateEdge(a, 1, a)
    db.cacheInvalidateKey('a')
    db.cacheClear()
    db.cacheClearQuery()
    db.cacheClearKey()
    db.cacheClearProperty()
    db.cacheClearTraversal()
    db.cacheResetStats()
  })
  t.is(db.get_node_by_key('a'), a)
  db.close()

  t.throws(() => db.cacheIsEnabled(), { message: /closed/ })
})

test('metrics report the removed cache as disabled and empty', (t) => {
  const db = Database.open(makeDbPath(), { cacheEnabled: true })
  const metrics = collectMetrics(db)
  t.false(metrics.cache.enabled)
  const empty = { hits: 0, misses: 0, hitRate: 0, size: 0, maxSize: 0, utilizationPercent: 0 }
  t.deepEqual(metrics.cache.propertyCache, empty)
  t.deepEqual(metrics.cache.traversalCache, empty)
  t.deepEqual(metrics.cache.queryCache, empty)
  t.is(metrics.memory.cacheEstimateBytes, 0)
  t.is(metrics.memory.totalEstimateBytes, metrics.memory.deltaEstimateBytes + metrics.memory.snapshotBytes)
  t.false(healthCheck(db).checks.some((check) => check.name === 'cache_efficiency'))
  db.close()
})
