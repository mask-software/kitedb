/**
 * KiteDB - A fast, lightweight, embedded graph database for Node.js
 *
 * @example
 * ```typescript
 * import { kite, defineNode, defineEdge, string, int, optional } from 'kitedb-core'
 *
 * // Define schema
 * const User = defineNode('user', {
 *   key: (id: string) => `user:${id}`,
 *   props: {
 *     name: string('name'),
 *     email: string('email'),
 *   },
 * })
 *
 * const knows = defineEdge('knows', {
 *   since: int('since'),
 * })
 *
 * // Open database
 * const db = await kite('./my.kitedb', {
 *   nodes: [User],
 *   edges: [knows],
 * })
 *
 * // Insert nodes
 * const alice = db.insert('user').values('alice', { name: 'Alice' }).returning()
 *
 * // Close when done
 * db.close()
 * ```
 *
 * @packageDocumentation
 */

// =============================================================================
// Schema Builders (clean API)
// =============================================================================

export {
  node,
  edge,
  prop,
  string,
  int,
  float,
  bool,
  vector,
  any,
  optional,
  withDefault,
  defineNode,
  defineEdge,
} from './schema'
export type {
  PropType,
  PropSpec,
  KeySpec,
  NodeSpec,
  NodeConfig,
  EdgeSpec,
  NodeRef,
  InferNodeInsert,
  InferNodeUpsert,
  InferNode,
  InferEdgeProps,
} from './schema'

// =============================================================================
// Native Bindings
// =============================================================================

// Import native bindings
import {
  kite as nativeKite,
  kiteSync as nativeKiteSync,
  Kite as NativeKite,
  KiteInsertBuilder as NativeKiteInsertBuilder,
  KiteUpsertBuilder as NativeKiteUpsertBuilder,
  KiteTraversal as NativeKiteTraversal,
  KitePath as NativeKitePath,
} from '../index'

import type {
  JsKiteOptions,
  JsNodeSpec,
  JsEdgeSpec,
  JsPropSpec,
  JsPropValue,
  JsSyncMode,
  JsTraverseOptions,
  JsPathResult,
  JsFullEdge,
  Database,
  KiteInsertExecutorSingle,
  KiteInsertExecutorMany,
  KiteUpsertExecutorSingle,
  KiteUpsertExecutorMany,
  KiteUpdateBuilder,
  KiteUpdateEdgeBuilder,
  KiteUpsertByIdBuilder,
  KiteUpsertEdgeBuilder,
} from '../index'

import type {
  NodeSpec,
  EdgeSpec,
  PropSpec,
  NodeRef,
  InferNodeInsert,
  InferNodeUpsert,
  InferNode,
  InferEdgeProps,
} from './schema'

// =============================================================================
// Clean Type Aliases (no Js prefix)
// =============================================================================

type NodeLike = string | NodeSpec
type EdgeLike = string | EdgeSpec
type InsertEntry = { key: unknown; props?: Record<string, unknown> | null } & Record<string, unknown>
type ArrayWithToArray<T, U = T> = T[] & { toArray(): U[] }
type NodeObject = NodeRef & Record<string, unknown>
type NodeIdLike = number | { id: number }
type NodePropsSelection = Array<string>
type SyncMode = JsSyncMode
type InsertExecutorSingle<N extends NodeSpec> = Omit<KiteInsertExecutorSingle, 'returning'> & {
  returning(): InferNode<N>
}
type InsertExecutorMany<N extends NodeSpec> = Omit<KiteInsertExecutorMany, 'returning'> & {
  returning(): Array<InferNode<N>>
}
type UpsertExecutorSingle<N extends NodeSpec> = Omit<KiteUpsertExecutorSingle, 'returning'> & {
  returning(): InferNode<N>
}
type UpsertExecutorMany<N extends NodeSpec> = Omit<KiteUpsertExecutorMany, 'returning'> & {
  returning(): Array<InferNode<N>>
}

function withToArray<T, U = T>(items: T[], toArray?: () => U[]): ArrayWithToArray<T, U> {
  const output = items as ArrayWithToArray<T, U>
  if (!output.toArray) {
    output.toArray = () => (toArray ? toArray() : (output as unknown as U[]))
  }
  return output
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
}

function nodeName(nodeType: NodeLike): string {
  return typeof nodeType === 'string' ? nodeType : nodeType.name
}

function nodeNameOptional(nodeType?: NodeLike | null): string | undefined | null {
  if (nodeType === undefined || nodeType === null) {
    return nodeType
  }
  return nodeName(nodeType)
}

function edgeName(edgeType: EdgeLike): string {
  return typeof edgeType === 'string' ? edgeType : edgeType.name
}

function nodeId(value: NodeIdLike): number {
  return typeof value === 'number' ? value : value.id
}

function edgeNameOptional(edgeType?: EdgeLike | null): string | undefined | null {
  if (edgeType === undefined || edgeType === null) {
    return edgeType
  }
  return edgeName(edgeType)
}

function normalizeEntry(entry: InsertEntry): { key: unknown; props?: object | null } {
  const hasProps = Object.prototype.hasOwnProperty.call(entry, 'props')
  const { key, props, ...rest } = entry
  if (hasProps) {
    return { key, props: props as object | null | undefined }
  }
  const restKeys = Object.keys(rest)
  if (restKeys.length === 0) {
    return { key }
  }
  return { key, props: rest as Record<string, unknown> }
}

// =============================================================================
// Fluent Builder Wrappers
// =============================================================================

export class KiteInsertBuilder<N extends NodeSpec = NodeSpec> extends NativeKiteInsertBuilder {
  static wrap(builder: NativeKiteInsertBuilder): KiteInsertBuilder {
    Object.setPrototypeOf(builder, KiteInsertBuilder.prototype)
    return builder as KiteInsertBuilder
  }

  values(key: InferNodeInsert<N>['key'], props?: Omit<InferNodeInsert<N>, 'key'> | null): InsertExecutorSingle<N>
  values(entry: InferNodeInsert<N>): InsertExecutorSingle<N>
  values(key: unknown, props?: object | null): KiteInsertExecutorSingle
  values(entry: InsertEntry): KiteInsertExecutorSingle
  values(keyOrEntry: unknown, props?: object | null): KiteInsertExecutorSingle {
    if (props === undefined && isRecord(keyOrEntry) && 'key' in keyOrEntry) {
      const normalized = normalizeEntry(keyOrEntry as InsertEntry)
      return super.values(normalized.key, normalized.props) as InsertExecutorSingle<N>
    }
    return super.values(keyOrEntry, props ?? undefined) as InsertExecutorSingle<N>
  }

  valuesMany(entries: Array<InferNodeInsert<N>>): InsertExecutorMany<N>
  valuesMany(entries: Array<unknown>): KiteInsertExecutorMany
  valuesMany(entries: Array<InsertEntry>): KiteInsertExecutorMany
  valuesMany(entries: Array<unknown>): KiteInsertExecutorMany {
    const normalized = entries.map((entry) => {
      if (isRecord(entry) && 'key' in entry) {
        return normalizeEntry(entry as InsertEntry)
      }
      return entry
    })
    return super.valuesMany(normalized) as InsertExecutorMany<N>
  }
}

export class KiteUpsertBuilder<N extends NodeSpec = NodeSpec> extends NativeKiteUpsertBuilder {
  static wrap(builder: NativeKiteUpsertBuilder): KiteUpsertBuilder {
    Object.setPrototypeOf(builder, KiteUpsertBuilder.prototype)
    return builder as KiteUpsertBuilder
  }

  values(key: InferNodeUpsert<N>['key'], props?: Omit<InferNodeUpsert<N>, 'key'> | null): UpsertExecutorSingle<N>
  values(entry: InferNodeUpsert<N>): UpsertExecutorSingle<N>
  values(key: unknown, props?: object | null): KiteUpsertExecutorSingle
  values(entry: InsertEntry): KiteUpsertExecutorSingle
  values(keyOrEntry: unknown, props?: object | null): KiteUpsertExecutorSingle {
    if (props === undefined && isRecord(keyOrEntry) && 'key' in keyOrEntry) {
      const normalized = normalizeEntry(keyOrEntry as InsertEntry)
      return super.values(normalized.key, normalized.props) as UpsertExecutorSingle<N>
    }
    return super.values(keyOrEntry, props ?? undefined) as UpsertExecutorSingle<N>
  }

  valuesMany(entries: Array<InferNodeUpsert<N>>): UpsertExecutorMany<N>
  valuesMany(entries: Array<unknown>): KiteUpsertExecutorMany
  valuesMany(entries: Array<InsertEntry>): KiteUpsertExecutorMany
  valuesMany(entries: Array<unknown>): KiteUpsertExecutorMany {
    const normalized = entries.map((entry) => {
      if (isRecord(entry) && 'key' in entry) {
        return normalizeEntry(entry as InsertEntry)
      }
      return entry
    })
    return super.valuesMany(normalized) as UpsertExecutorMany<N>
  }
}

export class KiteTraversal extends NativeKiteTraversal {
  static wrap(traversal: NativeKiteTraversal, db?: Kite): KiteTraversal {
    Object.setPrototypeOf(traversal, KiteTraversal.prototype)
    if (db) {
      ;(traversal as { __db?: Kite }).__db = db
    }
    return traversal as KiteTraversal
  }

  whereEdge(func: unknown): KiteTraversal {
    return KiteTraversal.wrap(super.whereEdge(func), (this as { __db?: Kite }).__db)
  }

  whereNode(func: unknown): KiteTraversal {
    return KiteTraversal.wrap(super.whereNode(func), (this as { __db?: Kite }).__db)
  }

  out(edgeType?: EdgeLike | null): KiteTraversal {
    return KiteTraversal.wrap(
      super.out(edgeNameOptional(edgeType)),
      (this as { __db?: Kite }).__db,
    )
  }

  ['in'](edgeType?: EdgeLike | null): KiteTraversal {
    return KiteTraversal.wrap(
      super['in'](edgeNameOptional(edgeType)),
      (this as { __db?: Kite }).__db,
    )
  }

  both(edgeType?: EdgeLike | null): KiteTraversal {
    return KiteTraversal.wrap(
      super.both(edgeNameOptional(edgeType)),
      (this as { __db?: Kite }).__db,
    )
  }

  traverse(edgeType: EdgeLike | undefined | null, options: JsTraverseOptions): KiteTraversal {
    return KiteTraversal.wrap(
      super.traverse(edgeNameOptional(edgeType), options),
      (this as { __db?: Kite }).__db,
    )
  }

  take(limit: number): KiteTraversal {
    return KiteTraversal.wrap(super.take(limit), (this as { __db?: Kite }).__db)
  }

  select(props: Array<string>): KiteTraversal {
    return KiteTraversal.wrap(super.select(props), (this as { __db?: Kite }).__db)
  }

  nodes(): number[] {
    const ids = super.nodes()
    const db = (this as { __db?: Kite }).__db
    if (!db) {
      return withToArray(ids)
    }
    const loadNodes = () => {
      const traversal = this as unknown as { nodesWithProps?: () => Array<NodeObject> }
      if (typeof traversal.nodesWithProps === 'function') {
        return traversal.nodesWithProps()
      }
      return ids
        .map((id) => db.getById(id))
        .filter((node): node is NodeObject => Boolean(node))
    }
    return withToArray(ids, loadNodes)
  }

  nodesWithProps(): Array<NodeObject> {
    const native = NativeKiteTraversal.prototype as unknown as {
      nodesWithProps: (this: KiteTraversal) => Array<NodeObject>
    }
    return native.nodesWithProps.call(this)
  }

  edges(): Array<JsFullEdge> {
    return withToArray(super.edges()) as unknown as Array<JsFullEdge>
  }

  toArray(): ArrayWithToArray<NodeObject> {
    const traversal = this as unknown as { nodesWithProps?: () => Array<NodeObject> }
    if (typeof traversal.nodesWithProps === 'function') {
      const nodes = traversal.nodesWithProps()
      return withToArray(nodes)
    }

    const ids = super.nodes()
    const db = (this as { __db?: Kite }).__db
    if (!db) {
      return withToArray(ids as unknown as NodeObject[])
    }
    return withToArray(
      ids.map((id) => db.getById(id)).filter((node): node is NodeObject => Boolean(node)),
    )
  }
}

export class KitePath extends NativeKitePath {
  static wrap(path: NativeKitePath): KitePath {
    Object.setPrototypeOf(path, KitePath.prototype)
    return path as KitePath
  }

  via(edgeType: EdgeLike): this {
    super.via(edgeName(edgeType))
    return this
  }

  maxDepth(depth: number): this {
    super.maxDepth(depth)
    return this
  }

  direction(direction: string): this {
    super.direction(direction)
    return this
  }

  bidirectional(): this {
    super.bidirectional()
    return this
  }

  dijkstra(): JsPathResult {
    return super.find()
  }

  bfs(): JsPathResult {
    return super.findBfs()
  }

  kShortest(k: number): Array<JsPathResult> {
    return super.findKShortest(k)
  }
}

export class KiteShortestPathBuilder {
  private readonly db: Kite
  private readonly source: number
  private target: number | null = null
  private edgeTypes: EdgeLike[] = []
  private maxDepthValue: number | null = null
  private directionValue: string | null = null
  private useBidirectional = false

  constructor(db: Kite, source: number) {
    this.db = db
    this.source = source
  }

  via(edgeType: EdgeLike): this {
    this.edgeTypes.push(edgeType)
    return this
  }

  to(target: NodeIdLike): this {
    this.target = nodeId(target)
    return this
  }

  maxDepth(depth: number): this {
    this.maxDepthValue = depth
    return this
  }

  direction(direction: string): this {
    this.directionValue = direction
    return this
  }

  bidirectional(): this {
    this.useBidirectional = true
    return this
  }

  dijkstra(): JsPathResult {
    const path = this.buildPath()
    return path.find()
  }

  bfs(): JsPathResult {
    const path = this.buildPath()
    return path.findBfs()
  }

  kShortest(k: number): Array<JsPathResult> {
    const path = this.buildPath()
    return path.findKShortest(k)
  }

  private buildPath(): KitePath {
    if (this.target === null) {
      throw new Error('shortestPath: target not set (call .to(target) first)')
    }
    const path = this.db.path(this.source, this.target)
    for (const edgeType of this.edgeTypes) {
      path.via(edgeType)
    }
    if (this.maxDepthValue !== null) {
      path.maxDepth(this.maxDepthValue)
    }
    if (this.useBidirectional) {
      path.bidirectional()
    } else if (this.directionValue !== null) {
      path.direction(this.directionValue)
    }
    return path
  }
}

export class KiteLinkBuilder<E extends EdgeSpec = EdgeSpec> {
  private readonly db: Kite
  private readonly src: number
  private dst: number | null = null
  private edgeType: EdgeLike | null = null
  private edgeProps: object | null | undefined

  constructor(db: Kite, src: number) {
    this.db = db
    this.src = src
  }

  to(dst: NodeIdLike): this {
    this.dst = nodeId(dst)
    return this
  }

  via<E2 extends EdgeSpec>(edgeType: E2): KiteLinkBuilder<E2> {
    this.edgeType = edgeType
    return this as unknown as KiteLinkBuilder<E2>
  }

  props(props: InferEdgeProps<E> | object | null): this {
    this.edgeProps = props as object | null
    return this
  }

  execute(): void {
    if (!this.edgeType) {
      throw new Error('link: edge type not set (call .via(edgeType) first)')
    }
    if (this.dst === null) {
      throw new Error('link: destination not set (call .to(dst) first)')
    }
    this.db.link(this.src, edgeName(this.edgeType), this.dst, this.edgeProps ?? undefined)
  }
}

// =============================================================================
// Kite Wrapper (transactions + batch)
// =============================================================================

export class Kite extends NativeKite {
  static open(path: string, options: JsKiteOptions): Kite {
    const native = NativeKite.open(path, options)
    Object.setPrototypeOf(native, Kite.prototype)
    return native as unknown as Kite
  }

  transaction<T>(fn: (ctx: Kite) => T | Promise<T>): T | Promise<T> {
    if (this.hasTransaction()) {
      return fn(this)
    }

    this.begin()
    try {
      const result = fn(this)
      if (result && typeof (result as Promise<T>).then === 'function') {
        return (result as Promise<T>).then(
          (value) => {
            this.commit()
            return value
          },
          (err) => {
            this.rollback()
            throw err
          },
        )
      }
      this.commit()
      return result
    } catch (err) {
      this.rollback()
      throw err
    }
  }

  checkpoint(): void {
    return super.checkpoint()
  }

  batch(operations: Array<any>): any[] {
    if (operations.length === 0) {
      return []
    }

    const nativeOps = new Set([
      'createNode',
      'deleteNode',
      'link',
      'linkWithProps',
      'unlink',
      'setProp',
      'setEdgeProp',
      'setEdgeProps',
      'delProp',
    ])
    const isNativeBatch = operations.every((op) => {
      if (!op || typeof op !== 'object') {
        return false
      }
      const opName = (op as { op?: string }).op ?? (op as { type?: string }).type
      return typeof opName === 'string' && nativeOps.has(opName)
    })

    if (isNativeBatch) {
      return super.batch(operations as Array<object>) as unknown as any[]
    }

    const inTransaction = this.hasTransaction()
    if (!inTransaction) {
      this.begin()
    }

    try {
      const results: any[] = []
      for (const op of operations) {
        let value
        if (typeof op === 'function') {
          value = op(this)
        } else if (op && typeof op.returning === 'function') {
          value = op.returning()
        } else if (op && typeof op.execute === 'function') {
          value = op.execute()
        } else {
          throw new Error('Unsupported batch operation')
        }

        if (value && typeof (value as { then?: unknown }).then === 'function') {
          if (!inTransaction) {
            this.rollback()
          }
          throw new Error('Batch operations must be synchronous')
        }

        results.push(value)
      }

      if (!inTransaction) {
        this.commit()
      }

      return results
    } catch (err) {
      if (!inTransaction) {
        this.rollback()
      }
      throw err
    }
  }

  batchAdaptive(
    operations: Array<any>,
    options?: { maxBatch?: number; minBatch?: number; autoCheckpointOnWalFull?: boolean } | null,
  ): any[] {
    if (operations.length === 0) {
      return []
    }

    let maxBatch = options?.maxBatch ?? 3000
    let minBatch = options?.minBatch ?? 1
    const autoCheckpointOnWalFull = options?.autoCheckpointOnWalFull ?? true
    if (maxBatch < 1) maxBatch = 1
    if (minBatch < 1) minBatch = 1
    if (minBatch > maxBatch) minBatch = maxBatch

    const results: any[] = []
    let cursor = 0
    let batchSize = Math.min(maxBatch, operations.length)
    let checkpointed = false

    while (cursor < operations.length) {
      const end = Math.min(cursor + batchSize, operations.length)
      const slice = operations.slice(cursor, end)
      try {
        const out = this.batch(slice)
        results.push(...out)
        cursor = end
        checkpointed = false
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err)
        if (/wal buffer full/i.test(message)) {
          if (autoCheckpointOnWalFull && !this.hasTransaction() && !checkpointed) {
            try {
              this.checkpoint()
              checkpointed = true
              continue
            } catch {
              // fall through to size reduction
            }
          }
          if (batchSize > minBatch) {
            batchSize = Math.max(minBatch, Math.floor(batchSize / 2))
            continue
          }
        }
        throw err
      }
    }

    return results
  }

  get(nodeType: NodeLike, key: unknown, props?: NodePropsSelection): object | null {
    return super.get(nodeName(nodeType), key, props)
  }

  getRef(nodeType: NodeLike, key: unknown): object | null {
    return super.getRef(nodeName(nodeType), key)
  }

  getId(nodeType: NodeLike, key: unknown): number | null {
    return super.getId(nodeName(nodeType), key)
  }

  getById(nodeId: number, props?: NodePropsSelection): object | null {
    return super.getById(nodeId, props)
  }

  getByIds(nodeIds: Array<NodeIdLike>, props?: NodePropsSelection): Array<object> {
    const ids = nodeIds.map((id) => nodeId(id))
    return super.getByIds(ids, props)
  }

  getProp(node: NodeIdLike, propName: string): JsPropValue | null {
    return super.getProp(nodeId(node), propName)
  }

  setProp(node: NodeIdLike, propName: string, value: unknown): void {
    return super.setProp(nodeId(node), propName, value)
  }

  setProps(node: NodeIdLike, props: Record<string, unknown>): void {
    return super.setProps(nodeId(node), props)
  }

  deleteByKey(nodeType: NodeLike, key: unknown): boolean {
    return super.deleteByKey(nodeName(nodeType), key)
  }

  delete(nodeType: NodeLike, key: unknown): boolean {
    return this.deleteByKey(nodeType, key)
  }

  insert(nodeType: NodeLike): KiteInsertBuilder {
    return KiteInsertBuilder.wrap(super.insert(nodeName(nodeType)))
  }

  upsert(nodeType: NodeLike): KiteUpsertBuilder {
    return KiteUpsertBuilder.wrap(super.upsert(nodeName(nodeType)))
  }

  updateByKey(nodeType: NodeLike, key: unknown): KiteUpdateBuilder {
    return super.updateByKey(nodeName(nodeType), key)
  }

  update(nodeType: NodeLike, key: unknown): KiteUpdateBuilder {
    return this.updateByKey(nodeType, key)
  }

  upsertById(nodeType: NodeLike, nodeId: number): KiteUpsertByIdBuilder {
    return super.upsertById(nodeName(nodeType), nodeId)
  }

  link(src: NodeIdLike, edgeType: EdgeLike, dst: NodeIdLike, props?: object | null): void
  link(src: NodeIdLike): KiteLinkBuilder
  link(
    src: NodeIdLike,
    edgeType?: EdgeLike,
    dst?: NodeIdLike,
    props?: object | null,
  ): void | KiteLinkBuilder {
    if (!edgeType || dst === undefined) {
      return new KiteLinkBuilder(this, nodeId(src))
    }
    return super.link(
      nodeId(src),
      edgeName(edgeType),
      nodeId(dst),
      props as object | undefined | null,
    )
  }

  unlink(src: NodeIdLike, edgeType: EdgeLike, dst: NodeIdLike): boolean {
    return super.unlink(nodeId(src), edgeName(edgeType), nodeId(dst))
  }

  hasEdge(src: NodeIdLike, edgeType: EdgeLike, dst: NodeIdLike): boolean {
    return super.hasEdge(nodeId(src), edgeName(edgeType), nodeId(dst))
  }

  getEdgeProp(src: NodeIdLike, edgeType: EdgeLike, dst: NodeIdLike, propName: string): JsPropValue | null {
    return super.getEdgeProp(nodeId(src), edgeName(edgeType), nodeId(dst), propName)
  }

  getEdgeProps(src: NodeIdLike, edgeType: EdgeLike, dst: NodeIdLike): Record<string, JsPropValue> {
    return super.getEdgeProps(nodeId(src), edgeName(edgeType), nodeId(dst))
  }

  setEdgeProp(
    src: NodeIdLike,
    edgeType: EdgeLike,
    dst: NodeIdLike,
    propName: string,
    value: unknown,
  ): void {
    return super.setEdgeProp(nodeId(src), edgeName(edgeType), nodeId(dst), propName, value)
  }

  setEdgeProps(
    src: NodeIdLike,
    edgeType: EdgeLike,
    dst: NodeIdLike,
    props: Record<string, unknown>,
  ): void {
    return super.setEdgeProps(nodeId(src), edgeName(edgeType), nodeId(dst), props)
  }

  delEdgeProp(src: NodeIdLike, edgeType: EdgeLike, dst: NodeIdLike, propName: string): void {
    return super.delEdgeProp(nodeId(src), edgeName(edgeType), nodeId(dst), propName)
  }

  updateEdge(src: NodeIdLike, edgeType: EdgeLike, dst: NodeIdLike): KiteUpdateEdgeBuilder {
    return super.updateEdge(nodeId(src), edgeName(edgeType), nodeId(dst))
  }

  upsertEdge(src: NodeIdLike, edgeType: EdgeLike, dst: NodeIdLike): KiteUpsertEdgeBuilder {
    return super.upsertEdge(nodeId(src), edgeName(edgeType), nodeId(dst))
  }

  all(nodeType: NodeLike): Array<object> {
    return super.all(nodeName(nodeType))
  }

  countNodes(nodeType?: NodeLike | null): number {
    return super.countNodes(nodeNameOptional(nodeType))
  }

  countEdges(edgeType?: EdgeLike | null): number {
    return super.countEdges(edgeNameOptional(edgeType))
  }

  allEdges(edgeType?: EdgeLike | null): Array<JsFullEdge> {
    return super.allEdges(edgeNameOptional(edgeType))
  }

  hasPath(source: NodeIdLike, target: NodeIdLike, edgeType?: EdgeLike | null): boolean {
    return super.hasPath(nodeId(source), nodeId(target), edgeNameOptional(edgeType))
  }

  reachableFrom(source: NodeIdLike, maxDepth: number, edgeType?: EdgeLike | null): Array<number> {
    return super.reachableFrom(nodeId(source), maxDepth, edgeNameOptional(edgeType))
  }

  from(node: NodeIdLike): KiteTraversal {
    return KiteTraversal.wrap(super.from(nodeId(node)), this)
  }

  fromNodes(nodeIds: Array<NodeIdLike>): KiteTraversal {
    return KiteTraversal.wrap(super.fromNodes(nodeIds.map((id) => nodeId(id))), this)
  }

  path(source: NodeIdLike, target: NodeIdLike): KitePath {
    return KitePath.wrap(super.path(nodeId(source), nodeId(target)))
  }

  pathToAny(source: NodeIdLike, targets: Array<NodeIdLike>): KitePath {
    return KitePath.wrap(
      super.pathToAny(nodeId(source), targets.map((target) => nodeId(target))),
    )
  }

  shortestPath(source: NodeIdLike): KiteShortestPathBuilder {
    return new KiteShortestPathBuilder(this, nodeId(source))
  }
}

export interface Kite {
  get<N extends NodeSpec>(
    nodeType: N,
    key: InferNodeInsert<N>['key'],
    props?: Array<keyof InferNode<N>> | Array<string>,
  ): InferNode<N> | null
  getRef<N extends NodeSpec>(nodeType: N, key: InferNodeInsert<N>['key']): NodeRef<N> | null
  getId<N extends NodeSpec>(nodeType: N, key: InferNodeInsert<N>['key']): number | null
  getById(nodeId: number, props?: Array<string>): NodeObject | null
  getByIds(nodeIds: Array<NodeIdLike>, props?: Array<string>): Array<NodeObject>
  delete<N extends NodeSpec>(nodeType: N, key: InferNodeInsert<N>['key']): boolean
  insert<N extends NodeSpec>(nodeType: N): KiteInsertBuilder<N>
  upsert<N extends NodeSpec>(nodeType: N): KiteUpsertBuilder<N>
  update<N extends NodeSpec>(nodeType: N, key: InferNodeInsert<N>['key']): KiteUpdateBuilder
  updateByKey<N extends NodeSpec>(nodeType: N, key: InferNodeInsert<N>['key']): KiteUpdateBuilder
  upsertById<N extends NodeSpec>(nodeType: N, nodeId: number): KiteUpsertByIdBuilder
  all<N extends NodeSpec>(nodeType: N): Array<InferNode<N>>
  countNodes(nodeType?: NodeLike | null): number
  countEdges(edgeType?: EdgeLike | null): number
  allEdges(edgeType?: EdgeLike | null): Array<JsFullEdge>
  link<E extends EdgeSpec>(
    src: NodeIdLike,
    edgeType: E,
    dst: NodeIdLike,
    props?: InferEdgeProps<E> | object | null,
  ): void
  link(src: NodeIdLike): KiteLinkBuilder
  unlink(src: NodeIdLike, edgeType: EdgeLike, dst: NodeIdLike): boolean
  hasEdge(src: NodeIdLike, edgeType: EdgeLike, dst: NodeIdLike): boolean
  getEdgeProp(src: NodeIdLike, edgeType: EdgeLike, dst: NodeIdLike, propName: string): JsPropValue | null
  getEdgeProps(src: NodeIdLike, edgeType: EdgeLike, dst: NodeIdLike): Record<string, JsPropValue>
  setEdgeProp(
    src: NodeIdLike,
    edgeType: EdgeLike,
    dst: NodeIdLike,
    propName: string,
    value: unknown,
  ): void
  batchAdaptive(
    operations: Array<any>,
    options?: { maxBatch?: number; minBatch?: number; autoCheckpointOnWalFull?: boolean } | null,
  ): Array<any>
  checkpoint(): void
  setEdgeProps(
    src: NodeIdLike,
    edgeType: EdgeLike,
    dst: NodeIdLike,
    props: Record<string, unknown>,
  ): void
  delEdgeProp(src: NodeIdLike, edgeType: EdgeLike, dst: NodeIdLike, propName: string): void
  updateEdge(src: NodeIdLike, edgeType: EdgeLike, dst: NodeIdLike): KiteUpdateEdgeBuilder
  upsertEdge(src: NodeIdLike, edgeType: EdgeLike, dst: NodeIdLike): KiteUpsertEdgeBuilder
  hasPath(source: NodeIdLike, target: NodeIdLike, edgeType?: EdgeLike | null): boolean
  reachableFrom(source: NodeIdLike, maxDepth: number, edgeType?: EdgeLike | null): Array<number>
  from(node: NodeIdLike): KiteTraversal
  fromNodes(nodeIds: Array<NodeIdLike>): KiteTraversal
  path(source: NodeIdLike, target: NodeIdLike): KitePath
  pathToAny(source: NodeIdLike, targets: Array<NodeIdLike>): KitePath
  shortestPath(source: NodeIdLike): KiteShortestPathBuilder
}

export interface KiteTraversal {
  whereEdge(func: unknown): KiteTraversal
  whereNode(func: unknown): KiteTraversal
  out(edgeType?: EdgeLike | null): KiteTraversal
  ['in'](edgeType?: EdgeLike | null): KiteTraversal
  both(edgeType?: EdgeLike | null): KiteTraversal
  traverse(edgeType: EdgeLike | undefined | null, options: JsTraverseOptions): KiteTraversal
  take(limit: number): KiteTraversal
  select(props: Array<string>): KiteTraversal
  nodes(): ArrayWithToArray<number, NodeObject>
  nodesWithProps(): Array<NodeObject>
  edges(): ArrayWithToArray<JsFullEdge>
  toArray(): ArrayWithToArray<NodeObject>
  count(): number
}

// =============================================================================
// Bulk Helpers (non-atomic)
// =============================================================================

export interface BulkWriteOptions {
  /** Max operations per transaction (default: 1000) */
  chunkSize?: number
  /** Call checkpoint when recommended between chunks (default: false) */
  checkpoint?: boolean
  /** Threshold for shouldCheckpoint (default: 0.8) */
  checkpointThreshold?: number
}

export function bulkWrite<T>(
  db: Database,
  operations: Array<(db: Database) => T>,
  options?: BulkWriteOptions,
): Array<T> {
  if (operations.length === 0) {
    return []
  }

  if (db.hasTransaction()) {
    throw new Error('bulkWrite cannot run inside an active transaction')
  }

  const chunkSize = options?.chunkSize ?? 1000
  if (chunkSize <= 0) {
    throw new Error('chunkSize must be greater than 0')
  }

  const checkpointThreshold = options?.checkpointThreshold ?? 0.8
  const shouldCheckpoint = options?.checkpoint ?? false

  const results: Array<T> = []
  let index = 0
  const beginBulk = typeof (db as unknown as { beginBulk?: () => number }).beginBulk === 'function'
    ? () => (db as unknown as { beginBulk: () => number }).beginBulk()
    : null

  while (index < operations.length) {
    if (beginBulk) {
      try {
        beginBulk()
      } catch {
        db.begin()
      }
    } else {
      db.begin()
    }
    try {
      const end = Math.min(index + chunkSize, operations.length)
      for (; index < end; index += 1) {
        const op = operations[index]
        const value = op(db)
        if (value && typeof (value as { then?: unknown }).then === 'function') {
          throw new Error('bulkWrite operations must be synchronous')
        }
        results.push(value)
      }
      db.commit()
    } catch (err) {
      db.rollback()
      throw err
    }

    if (shouldCheckpoint && db.shouldCheckpoint(checkpointThreshold)) {
      db.checkpoint()
    }
  }

  return results
}

// Re-export other classes with clean names
export {
  Database,
  VectorIndex,
  KiteInsertExecutorSingle,
  KiteInsertExecutorMany,
  KiteUpdateBuilder,
  KiteUpdateEdgeBuilder,
  KiteUpsertExecutorSingle,
  KiteUpsertExecutorMany,
  KiteUpsertByIdBuilder,
  KiteUpsertEdgeBuilder,
} from '../index'

// Re-export enums with clean names
export {
  JsTraversalDirection as TraversalDirection,
  JsDistanceMetric as DistanceMetric,
  JsAggregation as Aggregation,
  JsSyncMode as SyncMode,
  JsCompressionType as CompressionType,
  PropType as PropValueType,
} from '../index'

// Re-export utility functions
export {
  openDatabase,
  createBackup,
  restoreBackup,
  getBackupInfo,
  createOfflineBackup,
  collectMetrics,
  healthCheck,
  createVectorIndex,
  bruteForceSearch,
  pathConfig,
  traversalStep,
  version,
} from '../index'

// Re-export common types with clean names
export type {
  // Database
  DbStats,
  CheckResult,
  OpenOptions,
  // Export/Import
  ExportOptions,
  ExportResult,
  ImportOptions,
  ImportResult,
  // Backup
  BackupOptions,
  BackupResult,
  RestoreOptions,
  OfflineBackupOptions,
  // Streaming
  StreamOptions,
  PaginationOptions,
  NodePage,
  EdgePage,
  NodeWithProps,
  EdgeWithProps,
  // Metrics
  DatabaseMetrics,
  DataMetrics,
  CacheMetrics,
  CacheLayerMetrics,
  MemoryMetrics,
  MvccMetrics,
  MvccStats,
  HealthCheckResult,
  HealthCheckEntry,
  // Traversal
  JsTraverseOptions as TraverseOptions,
  JsTraversalStep as TraversalStep,
  JsTraversalResult as TraversalResult,
  // Pathfinding
  JsPathConfig as PathConfig,
  JsPathResult as PathResult,
  JsPathEdge as PathEdge,
  // Vectors
  VectorIndexOptions,
  VectorIndexStats,
  VectorSearchHit,
  SimilarOptions,
  JsIvfConfig as IvfConfig,
  JsIvfStats as IvfStats,
  JsPqConfig as PqConfig,
  JsSearchOptions as SearchOptions,
  JsSearchResult as SearchResult,
  JsBruteForceResult as BruteForceResult,
  // Compression
  CompressionOptions,
  SingleFileOptimizeOptions,
  VacuumOptions,
  // Cache
  JsCacheStats as CacheStats,
  // Low-level (for advanced use)
  JsEdge as Edge,
  JsFullEdge as FullEdge,
  JsNodeProp as NodeProp,
  JsPropValue as PropValue,
  JsEdgeInput as EdgeInput,
} from '../index'

// =============================================================================
// Kite Options (clean API)
// =============================================================================

/** Options for opening a Kite database */
export interface KiteOptions {
  /** Node type definitions */
  nodes: NodeSpec[]
  /** Edge type definitions */
  edges: EdgeSpec[]
  /** Open in read-only mode (default: false) */
  readOnly?: boolean
  /** Create database if it doesn't exist (default: true) */
  createIfMissing?: boolean
  /** Sync mode for durability (default: "Full") */
  syncMode?: SyncMode
  /** Enable group commit (coalesce WAL flushes across commits) */
  groupCommitEnabled?: boolean
  /** Group commit window in milliseconds */
  groupCommitWindowMs?: number
  /** Acquire file lock (default: true) */
  lockFile?: boolean
  /** WAL size in megabytes (default: 1MB) */
  walSizeMb?: number
  /** WAL usage threshold (0.0-1.0) to trigger auto-checkpoint */
  checkpointThreshold?: number
}

// =============================================================================
// Type Conversion Helpers
// =============================================================================

function propSpecToNative(spec: PropSpec): JsPropSpec {
  return {
    type: spec.type,
    optional: spec.optional,
    default: spec.default as JsPropValue | undefined,
  }
}

function nodeSpecToNative(spec: NodeSpec): JsNodeSpec {
  let props: Record<string, JsPropSpec> | undefined

  if (spec.props) {
    props = {}
    for (const [k, v] of Object.entries(spec.props)) {
      props[k] = propSpecToNative(v)
    }
  }

  return {
    name: spec.name,
    key: spec.key,
    props,
  }
}

function edgeSpecToNative(spec: EdgeSpec): JsEdgeSpec {
  let props: Record<string, JsPropSpec> | undefined

  if (spec.props) {
    props = {}
    for (const [k, v] of Object.entries(spec.props)) {
      props[k] = propSpecToNative(v)
    }
  }

  return {
    name: spec.name,
    props,
  }
}

function optionsToNative(options: KiteOptions): JsKiteOptions {
  return {
    nodes: options.nodes.map(nodeSpecToNative),
    edges: options.edges.map(edgeSpecToNative),
    readOnly: options.readOnly,
    createIfMissing: options.createIfMissing,
    syncMode: options.syncMode,
    groupCommitEnabled: options.groupCommitEnabled,
    groupCommitWindowMs: options.groupCommitWindowMs,
    lockFile: options.lockFile,
    walSizeMb: options.walSizeMb,
    checkpointThreshold: options.checkpointThreshold,
  }
}

// =============================================================================
// Main Entry Points
// =============================================================================

/**
 * Open a Kite database asynchronously.
 *
 * This is the recommended way to open a database as it doesn't block
 * the Node.js event loop during file I/O.
 *
 * @param path - Path to the database file
 * @param options - Database options including schema
 * @returns Promise resolving to a Kite database instance
 *
 * @example
 * ```typescript
 * const db = await kite('./my.kitedb', {
 *   nodes: [User, Post],
 *   edges: [follows, authored],
 * })
 * ```
 */
export async function kite(path: string, options: KiteOptions): Promise<Kite> {
  const nativeOptions = optionsToNative(options)
  // Cast through unknown because NAPI-RS generates Promise<unknown> for async tasks
  const native = (await nativeKite(path, nativeOptions)) as NativeKite
  Object.setPrototypeOf(native, Kite.prototype)
  return native as unknown as Kite
}

/**
 * Open a Kite database synchronously.
 *
 * Use this when you need synchronous initialization (e.g., at module load time).
 * For most cases, prefer the async `kite()` function.
 *
 * @param path - Path to the database file
 * @param options - Database options including schema
 * @returns A Kite database instance
 *
 * @example
 * ```typescript
 * const db = kiteSync('./my.kitedb', {
 *   nodes: [User],
 *   edges: [knows],
 * })
 * ```
 */
export function kiteSync(path: string, options: KiteOptions): Kite {
  const nativeOptions = optionsToNative(options)
  const native = nativeKiteSync(path, nativeOptions)
  Object.setPrototypeOf(native, Kite.prototype)
  return native as unknown as Kite
}
