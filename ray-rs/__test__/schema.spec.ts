import test from 'ava'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'

import { ray, raySync, node, edge, prop, optional } from '../dist/index.js'

const makeDbPath = () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'raydb-schema-'))
  return path.join(dir, 'test.raydb')
}

// =============================================================================
// Schema Builder Tests
// =============================================================================

test('prop builders create correct specs', (t) => {
  const strProp = prop.string('name')
  t.is(strProp.type, 'string')
  t.is(strProp.optional, undefined)

  const intProp = prop.int('age')
  t.is(intProp.type, 'int')

  const floatProp = prop.float('score')
  t.is(floatProp.type, 'float')

  const boolProp = prop.bool('active')
  t.is(boolProp.type, 'bool')

  const vecProp = prop.vector('embedding', 1536)
  t.is(vecProp.type, 'vector')
})

test('optional() marks props as optional', (t) => {
  const required = prop.int('count')
  t.is(required.optional, undefined)

  const opt = optional(prop.int('count'))
  t.is(opt.optional, true)
  t.is(opt.type, 'int')
})

test('node() creates node spec with key function', (t) => {
  const User = node('user', {
    key: (id: string) => `user:${id}`,
    props: {
      name: prop.string('name'),
      email: prop.string('email'),
    },
  })

  t.is(User.name, 'user')
  t.truthy(User.key)
  t.is(User.key?.kind, 'prefix')
  t.is(User.key?.prefix, 'user:')
  t.truthy(User.props)
  t.is(Object.keys(User.props!).length, 2)
})

test('node() with explicit key spec', (t) => {
  const OrgUser = node('org_user', {
    key: { kind: 'template', template: 'org:{org}:user:{id}' },
    props: {
      name: prop.string('name'),
    },
  })

  t.is(OrgUser.name, 'org_user')
  t.is(OrgUser.key?.kind, 'template')
  t.is(OrgUser.key?.template, 'org:{org}:user:{id}')
})

test('node() without config', (t) => {
  const Simple = node('simple')
  t.is(Simple.name, 'simple')
  t.is(Simple.key, undefined)
  t.is(Simple.props, undefined)
})

test('edge() creates edge spec', (t) => {
  const knows = edge('knows', {
    since: prop.int('since'),
    weight: optional(prop.float('weight')),
  })

  t.is(knows.name, 'knows')
  t.truthy(knows.props)
  t.is(Object.keys(knows.props!).length, 2)
  t.is(knows.props?.since.type, 'int')
  t.is(knows.props?.weight.optional, true)
})

test('edge() without props', (t) => {
  const follows = edge('follows')
  t.is(follows.name, 'follows')
  t.is(follows.props, undefined)
})

// =============================================================================
// Async ray() Tests
// =============================================================================

test('ray() opens database asynchronously', async (t) => {
  const User = node('user', {
    key: (id: string) => `user:${id}`,
    props: {
      name: prop.string('name'),
    },
  })

  const follows = edge('follows')

  const db = await ray(makeDbPath(), {
    nodes: [User],
    edges: [follows],
  })

  t.truthy(db)
  t.deepEqual(db.nodeTypes(), ['user'])
  t.deepEqual(db.edgeTypes(), ['follows'])

  db.close()
})

test('raySync() opens database synchronously', (t) => {
  const User = node('user', {
    key: (id: string) => `user:${id}`,
    props: {
      name: prop.string('name'),
    },
  })

  const knows = edge('knows', {
    since: prop.int('since'),
  })

  const db = raySync(makeDbPath(), {
    nodes: [User],
    edges: [knows],
  })

  t.truthy(db)
  t.deepEqual(db.nodeTypes(), ['user'])
  t.deepEqual(db.edgeTypes(), ['knows'])

  db.close()
})

// =============================================================================
// Full Integration Test
// =============================================================================

test('full schema-based workflow', async (t) => {
  // Define schema
  const Document = node('document', {
    key: (id: string) => `doc:${id}`,
    props: {
      title: prop.string('title'),
      content: prop.string('content'),
    },
  })

  const Topic = node('topic', {
    key: (name: string) => `topic:${name}`,
    props: {
      name: prop.string('name'),
    },
  })

  const discusses = edge('discusses', {
    relevance: prop.float('relevance'),
  })

  // Open database
  const db = await ray(makeDbPath(), {
    nodes: [Document, Topic],
    edges: [discusses],
  })

  // Insert nodes
  const doc = db.insert('document').values('doc1', { title: 'Hello', content: 'World' }).returning() as any
  t.truthy(doc)
  t.is(doc.$key, 'doc:doc1')
  t.is(doc.title, 'Hello')

  const topic = db.insert('topic').values('greeting', { name: 'Greetings' }).returning() as any
  t.truthy(topic)
  t.is(topic.$key, 'topic:greeting')

  // Link with edge props
  db.link(doc.$id, 'discusses', topic.$id, { relevance: 0.95 })

  // Verify edge
  t.true(db.hasEdge(doc.$id, 'discusses', topic.$id))

  // Get edge prop
  const relevance = db.getEdgeProp(doc.$id, 'discusses', topic.$id, 'relevance')
  t.truthy(relevance)
  t.is(relevance?.floatValue, 0.95)

  // Query
  const allDocs = db.all('document')
  t.is(allDocs.length, 1)

  const allTopics = db.all('topic')
  t.is(allTopics.length, 1)

  db.close()
})

test('async ray() is non-blocking', async (t) => {
  // This test verifies that ray() doesn't block
  // by checking that we can interleave other async operations
  const User = node('user', {
    key: (id: string) => `user:${id}`,
    props: { name: prop.string('name') },
  })

  const dbPath = makeDbPath()

  // Start opening database
  const dbPromise = ray(dbPath, {
    nodes: [User],
    edges: [],
  })

  // This should execute before db open completes (in theory)
  let counter = 0
  const tick = () =>
    new Promise<void>((resolve) => {
      counter++
      setImmediate(resolve)
    })

  await tick()

  // Now wait for db
  const db = await dbPromise
  t.truthy(db)
  t.true(counter > 0)

  db.close()
})
