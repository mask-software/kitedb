// b4 core-misc lane: the Kite traversal expands its hops with the core's
// `Kite::neighbors`, which lists a self-loop once in both directions (A13).
//
// Every hop skips nodes the traversal already visited, and a self-loop leads
// back to the node it leaves, so a self-loop listed twice never reached these
// results; they pin the contract, on both the core runner (no predicates) and
// the step-by-step runner (with predicates).

import test from 'ava'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'

import { edge, kiteSync, node, string } from '../ts/index'
import type { Kite } from '../ts/index'

const makeDbPath = () => path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'kitedb-b4-core-misc-')), 'test.kitedb')

const Person = node('person', {
  key: (id: string) => `person:${id}`,
  props: { name: string('name') },
})

const Follows = edge('follows', {})

const addPerson = (db: Kite, name: string): number =>
  (db.insert(Person).values(name, { name }).returning() as unknown as { id: number }).id

const edgeList = (edges: Array<{ src: number; dst: number }>) => edges.map(({ src, dst }) => [src, dst])
const ids = (nodes: number[]) => Array.from(nodes)

test('both() over a self-loop yields each neighbor and edge once', (t) => {
  const db = kiteSync(makeDbPath(), { nodes: [Person], edges: [Follows] })
  try {
    const a = addPerson(db, 'a')
    const b = addPerson(db, 'b')
    db.link(a, Follows, a)
    db.link(a, Follows, b)

    // From b, the hop reaches a once, over b's in-edge.
    t.deepEqual(ids(db.from(b).both(Follows).nodes()), [a])
    t.deepEqual(edgeList(db.from(b).both(Follows).edges()), [[a, b]])
    t.is(db.from(b).both(Follows).count(), 1)

    // From a, the self-loop leads back to a: only b is a result.
    t.deepEqual(ids(db.from(a).both(Follows).nodes()), [b])
    t.deepEqual(edgeList(db.from(a).both(Follows).edges()), [[a, b]])
    t.is(db.from(a).both(Follows).count(), 1)

    // The same through the predicate runner; the predicate sees each candidate edge once.
    const seen: number[][] = []
    const filtered = db
      .from(a)
      .both(Follows)
      .whereEdge((e: any) => {
        seen.push([e.src, e.dst])
        return true
      })
      .edges()
    t.deepEqual(edgeList(filtered), [[a, b]])
    t.deepEqual(seen, [[a, b]])

    // A second hop back over the self-loop's node adds nothing.
    t.deepEqual(ids(db.from(b).both(Follows).both(Follows).nodes()), [])
  } finally {
    db.close()
  }
})
