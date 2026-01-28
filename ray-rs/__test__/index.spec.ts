import test from 'ava'

import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'

import {
  Database,
  JsTraversalDirection,
  PropType,
  pathConfig,
  plus100,
  traversalStep,
} from '../index'

const makeDbPath = () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'raydb-'))
  return path.join(dir, 'test.raydb')
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
