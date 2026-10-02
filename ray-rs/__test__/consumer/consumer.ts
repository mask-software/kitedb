// A consumer of the published typings. `scripts/typecheck-consumer.mjs` copies
// this file into a throwaway project that depends on @kitedb/core and
// type-checks it with `strict` and `skipLibCheck: false`, so the shipped
// `dist/*.d.ts` and the native `index.d.ts` are checked as a user would see
// them. Nothing here runs; every function only has to compile.

import {
  Database,
  Kite,
  KitePath,
  KiteTraversal,
  bulkWrite,
  defineEdge,
  defineNode,
  int,
  kite,
  kiteSync,
  openDatabase,
  optional,
  string,
  type FullEdge,
  type InferNode,
  type NodeRef,
  type PathResult,
  type PropValue,
} from '@kitedb/core'
import { Database as NativeDatabase, Kite as NativeKite } from '@kitedb/core/native'

type Equal<A, B> = (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2 ? true : false
function assertType<T extends true>(): T | undefined {
  return undefined
}

const User = defineNode('user', {
  key: (id: string) => `user:${id}`,
  props: {
    name: string('name'),
    age: optional(int('age')),
  },
})

const knows = defineEdge('knows', {
  since: int('since'),
})

type UserNode = InferNode<typeof User>

// The wrapper classes are the native ones with a friendlier API: an instance
// must still be usable wherever the native type is expected.
export function wrappersExtendNative(db: Kite, raw: Database): void {
  const native: NativeKite = db
  const nativeDb: NativeDatabase = raw
  void native
  void nativeDb
}

export async function open(): Promise<void> {
  const db: Kite = await kite('./app.kitedb', { nodes: [User], edges: [knows] })
  const sync: Kite = kiteSync('./app.kitedb', { nodes: [User], edges: [knows], readOnly: true })
  const reopened: Kite = Kite.open('./app.kitedb', { nodes: [], edges: [] })
  sync.close()
  reopened.close()
  db.close()
}

export function readsBySpec(db: Kite): void {
  const alice = db.get(User, 'alice')
  assertType<Equal<typeof alice, UserNode | null>>()
  const name: string | undefined = alice?.name
  const age: number | undefined = alice?.age

  const some = db.get(User, 'alice', ['name'])
  const ref = db.getRef(User, 'alice')
  assertType<Equal<typeof ref, NodeRef<typeof User> | null>>()
  const id: number | null = db.getId(User, 'alice')
  const users: Array<UserNode> = db.all(User)
  void [name, age, some, id, users]
}

export function readsByName(db: Kite): void {
  const alice = db.get('user', 'alice')
  const aliceId: number | undefined = alice?.id
  const props: unknown = alice?.name
  const ref = db.getRef('user', 'alice')
  const refKey: string | undefined = ref?.key
  const id: number | null = db.getId('user', 'alice')
  const all = db.all('user')
  const firstId: number | undefined = all[0]?.id
  const byId = db.getById(1, ['name'])
  const byIds = db.getByIds([1, { id: 2 }])
  const prop: PropValue | null = db.getProp(1, 'name')
  const exists: boolean = db.exists(1)
  void [aliceId, props, refKey, id, firstId, byId, byIds, prop, exists]
}

export function writes(db: Kite): void {
  const bob: UserNode = db.insert(User).values('bob', { name: 'Bob' }).returning()
  const many: Array<UserNode> = db
    .insert(User)
    .valuesMany([
      { key: 'carol', name: 'Carol' },
      { key: 'dave', name: 'Dave', age: 40 },
    ])
    .returning()
  db.insert('user').values('erin', { name: 'Erin' }).execute()
  const upserted: UserNode = db.upsert(User).values({ key: 'bob', name: 'Robert' }).returning()

  db.update(User, 'bob').set('name', 'Bobby').unset('age').execute()
  db.updateByKey('user', 'bob').setAll({ name: 'Bob' }).execute()
  db.updateById(bob).set('age', 41).execute()
  db.upsertById(User, 99).set('name', 'Nine').execute()

  db.setProp(bob, 'age', 42)
  db.setProps(bob.id, { age: 43 })

  db.link(bob, knows, many[0]!, { since: 2020 })
  db.link(bob.id, 'knows', upserted.id)
  db.link(bob).to(upserted).via(knows).props({ since: 2021 }).execute()
  db.setEdgeProp(bob, knows, upserted, 'since', 2022)
  db.setEdgeProps(bob, 'knows', upserted, { since: 2023 })
  db.updateEdge(bob, knows, upserted).set('since', 2024).execute()
  db.upsertEdge(bob, knows, upserted).setAll({ since: 2025 }).execute()
  const since: PropValue | null = db.getEdgeProp(bob, knows, upserted, 'since')
  const edgeProps: Record<string, PropValue> = db.getEdgeProps(bob, knows, upserted)
  db.delEdgeProp(bob, knows, upserted, 'since')
  const linked: boolean = db.hasEdge(bob, knows, upserted)
  const unlinked: boolean = db.unlink(bob, knows, upserted)

  const deleted: boolean = db.delete(User, 'erin')
  const deletedByName: boolean = db.deleteByKey('user', 'erin')
  const deletedById: boolean = db.deleteById(bob)
  void [since, edgeProps, linked, unlinked, deleted, deletedByName, deletedById]
}

export function counts(db: Kite): void {
  const nodes: number = db.countNodes(User)
  const named: number = db.countNodes('user')
  const all: number = db.countNodes()
  const edges: number = db.countEdges(knows)
  const allEdges: Array<FullEdge> = db.allEdges('knows')
  const reachable: boolean = db.hasPath(1, { id: 2 }, knows)
  const within: Array<number> = db.reachableFrom(1, 3)
  void [nodes, named, all, edges, allEdges, reachable, within]
}

export function traversals(db: Kite): void {
  const traversal: KiteTraversal = db
    .from(1)
    .out(knows)
    .in('knows')
    .both()
    .whereNode((n: { id: number }) => n.id > 0)
    .take(10)
  const ids: Array<number> = traversal.nodes()
  const loaded = traversal.nodes().toArray()
  const loadedId: number | undefined = loaded[0]?.id
  const edges: Array<FullEdge> = traversal.edges()
  const count: number = traversal.count()
  const nodes = db.fromNodes([1, { id: 2 }]).traverse(knows, { maxDepth: 2 }).toArray()
  void [ids, loadedId, edges, count, nodes]
}

export function paths(db: Kite): void {
  const path: KitePath = db.path(1, { id: 2 }).via(knows).maxDepth(4).direction('both').weight('since')
  const shortest: PathResult = path.dijkstra()
  const bfs: PathResult = db.pathToAny(1, [2, 3]).bfs()
  const top: Array<PathResult> = db.shortestPath(1).to(2).via('knows').bidirectional().kShortest(3)
  void [shortest, bfs, top]
}

export async function transactions(db: Kite): Promise<void> {
  const total: number = db.transaction((tx) => tx.countNodes()) as number
  const later: string = await db.transaction(async (tx) => {
    tx.insert(User).values('zed', { name: 'Zed' }).execute()
    return 'done'
  })
  db.begin()
  db.commit()
  const results = db.batch([(tx: Kite) => tx.countNodes()])
  const adaptive = db.batchAdaptive([], { maxBatch: 100 })
  db.checkpoint()
  void [total, later, results, adaptive]
}

export function lowLevel(): void {
  const db: Database = Database.open('./raw.kitedb', { readOnly: false })
  const other: Database = openDatabase('./raw.kitedb')
  const nodeId: number | null = db.getNodeByKey('user:alice')
  const count: number = db.transaction((tx) => tx.countNodes()) as number
  db.begin()
  const savepoint = db.savepoint()
  db.rollbackTo(savepoint)
  db.releaseSavepoint(savepoint)
  db.commit()
  const ids: Array<number> = bulkWrite(db, [(tx) => tx.createNode('user:x')], { chunkSize: 10 })
  db.close()
  other.close()
  void [nodeId, count, ids]
}
