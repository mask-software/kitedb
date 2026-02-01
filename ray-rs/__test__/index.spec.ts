import test from 'ava'

import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'

import {
  Database,
  JsTraversalDirection,
  PropType,
  collectMetrics,
  createBackup,
  createOfflineBackup,
  getBackupInfo,
  healthCheck,
  pathConfig,
  plus100,
  restoreBackup,
  traversalStep,
} from '../index'

const makeDbPath = () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kitedb-'))
  return path.join(dir, 'test.kitedb')
}

test('sync function from native code', (t) => {
  const fixture = 42
  t.is(plus100(fixture), fixture + 100)
})

test('db-backed traversal APIs', (t) => {
  const db = Database.open(makeDbPath())
  db.begin()

  const a = db.createNode('a')
  const b = db.createNode('b')
  const c = db.createNode('c')

  const knows = db.getOrCreateEtype('knows')
  db.addEdge(a, knows, b)
  db.addEdge(b, knows, c)
  db.commit()

  const single = db.traverseSingle([a], JsTraversalDirection.Out, knows)
  t.is(single.length, 1)
  t.is(single[0].nodeId, b)

  const steps = [
    traversalStep(JsTraversalDirection.Out, knows),
    traversalStep(JsTraversalDirection.Out, knows),
  ]
  const multi = db.traverse([a], steps)
  t.true(multi.some((r) => r.nodeId === c))

  const count = db.traverseCount([a], steps)
  t.is(count, 1)

  const ids = db.traverseNodeIds([a], steps)
  t.deepEqual(ids, [c])

  const depth = db.traverseDepth([a], knows, {
    maxDepth: 2,
    direction: JsTraversalDirection.Out,
  })
  const depthIds = depth.map((r) => r.nodeId).sort()
  t.deepEqual(depthIds, [b, c].sort())

  db.close()
})

test('db-backed upsertNode', (t) => {
  const db = Database.open(makeDbPath())

  db.begin()
  const nameKey = db.getOrCreatePropkey('name')
  const ageKey = db.getOrCreatePropkey('age')
  const nodeId = db.upsertNode('user:alice', [
    {
      keyId: nameKey,
      value: { propType: PropType.String, stringValue: 'Alice' },
    },
  ])
  db.commit()

  t.is(db.getNodeByKey('user:alice'), nodeId)

  db.begin()
  const sameId = db.upsertNode('user:alice', [
    {
      keyId: ageKey,
      value: { propType: PropType.Int, intValue: 30 },
    },
    {
      keyId: nameKey,
      value: { propType: PropType.Null },
    },
  ])
  db.commit()

  t.is(sameId, nodeId)

  const props = db.getNodeProps(nodeId) ?? []
  const propsByKey = new Map(props.map((p) => [p.keyId, p.value]))
  t.is(propsByKey.get(ageKey)?.intValue, 30)
  t.true(!propsByKey.has(nameKey))

  db.close()
})

test('db-backed upsertNodeById', (t) => {
  const db = Database.open(makeDbPath())

  db.begin()
  const nameKey = db.getOrCreatePropkey('name')
  const ageKey = db.getOrCreatePropkey('age')
  const nodeId = 42
  const createdId = db.upsertNodeById(nodeId, [
    {
      keyId: nameKey,
      value: { propType: PropType.String, stringValue: 'Alice' },
    },
  ])
  db.commit()

  t.is(createdId, nodeId)
  t.true(db.nodeExists(nodeId))

  db.begin()
  const updatedId = db.upsertNodeById(nodeId, [
    {
      keyId: ageKey,
      value: { propType: PropType.Int, intValue: 31 },
    },
    {
      keyId: nameKey,
      value: { propType: PropType.Null },
    },
  ])
  db.commit()

  t.is(updatedId, nodeId)

  const props = db.getNodeProps(nodeId) ?? []
  const propsByKey = new Map(props.map((p) => [p.keyId, p.value]))
  t.is(propsByKey.get(ageKey)?.intValue, 31)
  t.true(!propsByKey.has(nameKey))

  db.close()
})

test('db-backed pathfinding APIs', (t) => {
  const db = Database.open(makeDbPath())
  db.begin()

  const a = db.createNode('a')
  const b = db.createNode('b')
  const c = db.createNode('c')

  const knows = db.getOrCreateEtype('knows')
  db.addEdge(a, knows, b)
  db.addEdge(b, knows, c)
  db.commit()

  const config = pathConfig(a, c)
  config.allowedEdgeTypes = [knows]

  const bfsResult = db.bfs(config)
  t.true(bfsResult.found)
  t.deepEqual(bfsResult.path, [a, b, c])

  const dijkstraResult = db.dijkstra(config)
  t.true(dijkstraResult.found)
  t.is(dijkstraResult.totalWeight, 2)

  t.true(db.hasPath(a, c, knows))
  const reachable = db.reachableNodes(a, 2, knows)
  t.true(reachable.includes(c))

  db.close()
})

test('weighted dijkstra uses edge property', (t) => {
  const db = Database.open(makeDbPath())
  db.begin()

  const a = db.createNode('a')
  const b = db.createNode('b')
  const c = db.createNode('c')

  const knows = db.getOrCreateEtype('knows')
  const weightKey = db.getOrCreatePropkey('weight')

  db.addEdge(a, knows, b)
  db.addEdge(a, knows, c)
  db.addEdge(c, knows, b)

  db.setEdgeProp(a, knows, b, weightKey, {
    propType: PropType.Int,
    intValue: 10,
  })
  db.setEdgeProp(a, knows, c, weightKey, {
    propType: PropType.Int,
    intValue: 1,
  })
  db.setEdgeProp(c, knows, b, weightKey, {
    propType: PropType.Int,
    intValue: 1,
  })
  db.commit()

  const config = {
    source: a,
    target: b,
    allowedEdgeTypes: [knows],
    weightKeyId: weightKey,
  }

  const result = db.dijkstra(config)
  t.true(result.found)
  t.is(result.totalWeight, 2)
  t.deepEqual(result.path, [a, c, b])

  const paths = db.kShortest(config, 2)
  t.is(paths[0].totalWeight, 2)
  t.true(paths.length >= 1)

  db.close()
})

test('backup/restore APIs', (t) => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kitedb-'))
  const dbPath = path.join(dir, 'source.kitedb')
  const db = Database.open(dbPath)
  db.begin()

  const nodeId = db.createNode('user:alice')
  db.commit()

  const backupBase = path.join(dir, 'backup')
  const backup = createBackup(db, backupBase)
  t.true(backup.path.endsWith('.kitedb'))

  const info = getBackupInfo(backup.path)
  t.is(info.path, backup.path)

  db.close()

  const restoreBase = path.join(dir, 'restore')
  const restoredPath = restoreBackup(backup.path, restoreBase)
  const restored = Database.open(restoredPath)
  t.true(restored.nodeExists(nodeId))
  restored.close()

  const offlineBackup = createOfflineBackup(restoredPath, path.join(dir, 'offline'))
  t.true(offlineBackup.size >= 0)
})

test('metrics and health APIs', (t) => {
  const db = Database.open(makeDbPath())
  db.begin()
  db.createNode('metrics:test')
  db.commit()

  const metrics = collectMetrics(db)
  t.true(metrics.data.nodeCount >= 1)
  t.is(metrics.readOnly, false)

  const health = healthCheck(db)
  t.true(health.healthy)
  t.true(health.checks.some((check) => check.name === 'database_open'))

  db.close()
})
