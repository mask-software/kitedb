import test from 'ava'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'

import { Database, kiteSync, node, prop } from '../dist/index.js'

const makeDbPath = () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kitedb-ergonomics-'))
  return path.join(dir, 'test.kitedb')
}

const User = node('user', {
  key: (id: string) => `user:${id}`,
  props: {
    name: prop.string('name'),
    age: prop.int('age'),
  },
})

// The upsert builder chains (`upsert(...).values(...).returning()`); the update
// builder should too. Today set/unset/setAll return void, so chaining throws.
test('update builder methods chain', (t) => {
  const db = kiteSync(makeDbPath(), { nodes: [User], edges: [] })
  try {
    db.insert(User).values('alice', { name: 'Alice', age: 30 }).execute()

    db.update(User, 'alice').set('name', 'Alicia').set('age', 31).execute()
    let alice = db.get(User, 'alice') as any
    t.is(alice.name, 'Alicia')
    t.is(Number(alice.age), 31)

    db.update(User, 'alice').setAll({ age: 32 }).unset('name').execute()
    alice = db.get(User, 'alice') as any
    t.is(Number(alice.age), 32)
    t.falsy(alice.name)
  } finally {
    db.close()
  }
})

// Every other Database method is camelCase in JS; the get_* family is the
// exception. Expose camelCase names without breaking existing snake_case callers.
test('Database get_* methods are also available in camelCase', (t) => {
  const db = Database.open(makeDbPath(), {})
  try {
    db.begin()
    const id = db.createNode('user:alice')
    db.commit()

    const pairs: Array<[camel: string, snake: string]> = [
      ['getNodeByKey', 'get_node_by_key'],
      ['getNodeKey', 'get_node_key'],
      ['getOutEdges', 'get_out_edges'],
      ['getInEdges', 'get_in_edges'],
      ['getOutDegree', 'get_out_degree'],
      ['getInDegree', 'get_in_degree'],
      ['getNodesPage', 'get_nodes_page'],
      ['getEdgesPage', 'get_edges_page'],
      ['getNodeProp', 'get_node_prop'],
      ['getNodeProps', 'get_node_props'],
      ['getEdgeProp', 'get_edge_prop'],
      ['getEdgeProps', 'get_edge_props'],
      ['getNodeVector', 'get_node_vector'],
      ['getOrCreateLabel', 'get_or_create_label'],
      ['getLabelId', 'get_label_id'],
      ['getLabelName', 'get_label_name'],
      ['getOrCreateEtype', 'get_or_create_etype'],
      ['getEtypeId', 'get_etype_id'],
      ['getEtypeName', 'get_etype_name'],
      ['getOrCreatePropkey', 'get_or_create_propkey'],
      ['getPropkeyId', 'get_propkey_id'],
      ['getPropkeyName', 'get_propkey_name'],
      ['getNodeLabels', 'get_node_labels'],
    ]

    const anyDb = db as any
    for (const [camel, snake] of pairs) {
      t.is(typeof anyDb[snake], 'function', `${snake} should keep working`)
      t.is(typeof anyDb[camel], 'function', `${camel} should exist`)
    }

    t.is(anyDb.getNodeByKey('user:alice'), id)
    t.is(anyDb.get_node_by_key('user:alice'), id)
    t.is(anyDb.getNodeKey(id), 'user:alice')
  } finally {
    db.close()
  }
})
