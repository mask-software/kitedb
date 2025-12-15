/**
 * Ray Database
 *
 * High-level Drizzle-style API for the graph database.
 */

import { optimize } from "../core/compactor.ts";
import {
  beginTx,
  closeGraphDB,
  commit,
  defineEtype,
  definePropkey,
  edgeExists,
  getNodeByKey,
  getNodeProp,
  nodeExists,
  openGraphDB,
  check as rawCheck,
  stats as rawStats,
  rollback,
} from "../ray/__graph-db.ts";
import type {
  CheckResult,
  DbStats,
  ETypeID,
  GraphDB,
  NodeID,
  OpenOptions,
  PropKeyID,
  TxHandle,
} from "../types.ts";
import {
  type BatchOperation,
  createDeleteBuilder,
  createInsertBuilder,
  createLinkExecutor,
  createNodeRef,
  createUnlinkExecutor,
  createUpdateBuilder,
  createUpdateByRefBuilder,
  createUpdateEdgeBuilder,
  type DeleteBuilder,
  type InsertBuilder,
  type LinkExecutor,
  type NodeRef,
  type UpdateBuilder,
  type UpdateByRefBuilder,
  type UpdateEdgeBuilder,
} from "./builders.ts";
import type {
  EdgeDef,
  InferEdgeProps,
  InferNode,
  InferNodeInsert,
  NodeDef,
  RaySchema,
} from "./schema.ts";
import { createTraversalBuilder, type TraversalBuilder } from "./traversal.ts";
import {
  createPathFindingBuilder,
  type PathFindingBuilder,
  type WeightSpec,
} from "./pathfinding.ts";
import type { EdgeDef } from "./schema.ts";

// ============================================================================
// Ray Options
// ============================================================================

export interface RayOptions extends OpenOptions {
  nodes: NodeDef[];
  edges: EdgeDef[];
}

// ============================================================================
// Transaction Context
// ============================================================================

export interface TransactionContext {
  insert<N extends NodeDef>(node: N): InsertBuilder<N>;
  update<N extends NodeDef>(node: N): UpdateBuilder<N>;
  update<N extends NodeDef>(nodeRef: NodeRef<N>): UpdateByRefBuilder<N>;
  delete<N extends NodeDef>(node: N): DeleteBuilder<N>;
  delete<N extends NodeDef>(nodeRef: NodeRef<N>): Promise<boolean>;
  link<E extends EdgeDef>(
    src: NodeRef,
    edge: E,
    dst: NodeRef,
    props?: InferEdgeProps<E>,
  ): Promise<void>;
  unlink<E extends EdgeDef>(src: NodeRef, edge: E, dst: NodeRef): Promise<void>;
  updateEdge<E extends EdgeDef>(
    src: NodeRef,
    edge: E,
    dst: NodeRef,
  ): UpdateEdgeBuilder<E>;
  from<N extends NodeDef>(node: NodeRef<N>): TraversalBuilder<N>;
  get<N extends NodeDef>(
    node: N,
    key: Parameters<N["keyFn"]>[0],
  ): Promise<(NodeRef<N> & InferNode<N>) | null>;
}

// ============================================================================
// Ray Class
// ============================================================================

export class Ray {
  private readonly _db: GraphDB;
  private readonly _nodes: Map<string, NodeDef>;
  private readonly _edges: Map<string, EdgeDef>;
  private readonly _etypeIds: Map<EdgeDef, ETypeID>;
  private readonly _propKeyIds: Map<string, PropKeyID>;

  private constructor(
    db: GraphDB,
    nodes: NodeDef[],
    edges: EdgeDef[],
    etypeIds: Map<EdgeDef, ETypeID>,
    propKeyIds: Map<string, PropKeyID>,
  ) {
    this._db = db;
    this._nodes = new Map(nodes.map((n) => [n.name, n]));
    this._edges = new Map(edges.map((e) => [e.name, e]));
    this._etypeIds = etypeIds;
    this._propKeyIds = propKeyIds;
  }

  /** Open or create a ray database */
  static async open(path: string, options: RayOptions): Promise<Ray> {
    const { nodes, edges, ...dbOptions } = options;

    const db = await openGraphDB(path, dbOptions);

    // Initialize schema in a transaction
    const tx = beginTx(db);

    // Define edge types
    const etypeIds = new Map<EdgeDef, ETypeID>();
    for (const edge of edges) {
      const etypeId = defineEtype(tx, edge.name);
      etypeIds.set(edge, etypeId);
      edge._etypeId = etypeId;
    }

    // Define property keys for all nodes and edges
    const propKeyIds = new Map<string, PropKeyID>();

    for (const node of nodes) {
      node._propKeyIds = new Map();
      for (const [propName, propDef] of Object.entries(node.props)) {
        const key = `${node.name}:${propDef.name}`;
        if (!propKeyIds.has(key)) {
          const propKeyId = definePropkey(tx, propDef.name);
          propKeyIds.set(key, propKeyId);
        }
        node._propKeyIds.set(propName, propKeyIds.get(key)!);
      }
    }

    for (const edge of edges) {
      edge._propKeyIds = new Map();
      for (const [propName, propDef] of Object.entries(edge.props)) {
        const key = `${edge.name}:${propDef.name}`;
        if (!propKeyIds.has(key)) {
          const propKeyId = definePropkey(tx, propDef.name);
          propKeyIds.set(key, propKeyId);
        }
        edge._propKeyIds.set(propName, propKeyIds.get(key)!);
      }
    }

    await commit(tx);

    return new Ray(db, nodes, edges, etypeIds, propKeyIds);
  }

  // ==========================================================================
  // Schema Resolution Helpers
  // ==========================================================================

  private resolveEtypeId(edgeDef: EdgeDef): ETypeID {
    const id = this._etypeIds.get(edgeDef);
    if (id === undefined) {
      throw new Error(`Unknown edge type: ${edgeDef.name}`);
    }
    return id;
  }

  private resolvePropKeyId(
    def: NodeDef | EdgeDef,
    propName: string,
  ): PropKeyID {
    const id = def._propKeyIds?.get(propName);
    if (id === undefined) {
      throw new Error(`Unknown property: ${propName} on ${def.name}`);
    }
    return id;
  }

  private getNodeDef(_nodeId: NodeID): NodeDef | null {
    // For now, return the first node def. In a real implementation,
    // we'd track which def created which node, or use labels.
    const first = this._nodes.values().next();
    return first.done ? null : first.value;
  }

  // ==========================================================================
  // Node Operations
  // ==========================================================================

  /** Insert a new node */
  insert<N extends NodeDef>(node: N): InsertBuilder<N> {
    return createInsertBuilder(
      this._db,
      node,
      this.resolvePropKeyId.bind(this),
    );
  }

  /** Update a node by definition or reference */
  update<N extends NodeDef>(node: N): UpdateBuilder<N>;
  update<N extends NodeDef>(nodeRef: NodeRef<N>): UpdateByRefBuilder<N>;
  update<N extends NodeDef>(
    nodeOrRef: N | NodeRef<N>,
  ): UpdateBuilder<N> | UpdateByRefBuilder<N> {
    if ("$id" in nodeOrRef) {
      // It's a NodeRef
      return createUpdateByRefBuilder(
        this._db,
        nodeOrRef,
        this.resolvePropKeyId.bind(this),
      );
    }
    // It's a NodeDef
    return createUpdateBuilder(
      this._db,
      nodeOrRef,
      this.resolvePropKeyId.bind(this),
    );
  }

  /** Delete a node by definition or reference */
  delete<N extends NodeDef>(node: N): DeleteBuilder<N>;
  delete<N extends NodeDef>(nodeRef: NodeRef<N>): Promise<boolean>;
  delete<N extends NodeDef>(
    nodeOrRef: N | NodeRef<N>,
  ): DeleteBuilder<N> | Promise<boolean> {
    if ("$id" in nodeOrRef) {
      // It's a NodeRef - delete directly
      return createDeleteBuilder(this._db, nodeOrRef.$def as N)
        .where({ $id: nodeOrRef.$id })
        .execute();
    }
    // It's a NodeDef
    return createDeleteBuilder(this._db, nodeOrRef);
  }

  /** Get a node by key */
  async get<N extends NodeDef>(
    node: N,
    key: Parameters<N["keyFn"]>[0],
  ): Promise<(NodeRef<N> & InferNode<N>) | null> {
    const fullKey = node.keyFn(key as never);
    const nodeId = getNodeByKey(this._db, fullKey);

    if (nodeId === null) {
      return null;
    }

    // Load properties
    const props: Record<string, unknown> = {};
    for (const [propName, propDef] of Object.entries(node.props)) {
      const propKeyId = this.resolvePropKeyId(node, propName);
      const propValue = getNodeProp(this._db, nodeId, propKeyId);
      if (propValue) {
        props[propName] = this.fromPropValue(propValue);
      }
    }

    return createNodeRef(node, nodeId, fullKey, props);
  }

  /** Check if a node exists */
  async exists(nodeRef: NodeRef): Promise<boolean> {
    return nodeExists(this._db, nodeRef.$id);
  }

  // ==========================================================================
  // Edge Operations
  // ==========================================================================

  /** Create an edge between two nodes */
  async link<E extends EdgeDef>(
    src: NodeRef,
    edge: E,
    dst: NodeRef,
    props?: InferEdgeProps<E>,
  ): Promise<void> {
    const executor = createLinkExecutor(
      this._db,
      src,
      edge,
      dst,
      props,
      this.resolveEtypeId.bind(this),
      this.resolvePropKeyId.bind(this),
    );
    await executor.execute();
  }

  /** Remove an edge between two nodes */
  async unlink<E extends EdgeDef>(
    src: NodeRef,
    edge: E,
    dst: NodeRef,
  ): Promise<void> {
    const executor = createUnlinkExecutor(
      this._db,
      src,
      edge,
      dst,
      this.resolveEtypeId.bind(this),
    );
    await executor.execute();
  }

  /** Check if an edge exists */
  async hasEdge<E extends EdgeDef>(
    src: NodeRef,
    edge: E,
    dst: NodeRef,
  ): Promise<boolean> {
    const etypeId = this.resolveEtypeId(edge);
    return edgeExists(this._db, src.$id, etypeId, dst.$id);
  }

  /** Update edge properties */
  updateEdge<E extends EdgeDef>(
    src: NodeRef,
    edge: E,
    dst: NodeRef,
  ): UpdateEdgeBuilder<E> {
    return createUpdateEdgeBuilder(
      this._db,
      src,
      edge,
      dst,
      this.resolveEtypeId.bind(this),
      this.resolvePropKeyId.bind(this),
    );
  }

  // ==========================================================================
  // Traversal
  // ==========================================================================

  /** Start a traversal from a node */
  from<N extends NodeDef>(node: NodeRef<N>): TraversalBuilder<N> {
    return createTraversalBuilder(
      this._db,
      [node],
      this.resolveEtypeId.bind(this),
      this.resolvePropKeyId.bind(this),
      this.getNodeDef.bind(this),
    );
  }

  /** Start a path finding query from a node */
  shortestPath<N extends NodeDef>(
    source: NodeRef<N>,
    weight?: WeightSpec<EdgeDef>,
  ): PathFindingBuilder<N> {
    return createPathFindingBuilder(
      this._db,
      source,
      weight,
      this.resolveEtypeId.bind(this),
      this.resolvePropKeyId.bind(this),
      this.getNodeDef.bind(this),
      source.$def,
    );
  }

  // ==========================================================================
  // Batch Operations
  // ==========================================================================

  /** Execute multiple operations in a single transaction */
  async batch<T extends BatchOperation[]>(
    operations: [...{ [K in keyof T]: { _toBatchOp(): T[K] } }],
  ): Promise<{ [K in keyof T]: Awaited<ReturnType<T[K]["execute"]>> }> {
    const tx = beginTx(this._db);
    const results: unknown[] = [];

    try {
      for (const op of operations) {
        const batchOp = op._toBatchOp();
        const result = await batchOp.execute(tx);
        results.push(result);
      }

      await commit(tx);
      return results as {
        [K in keyof T]: Awaited<ReturnType<T[K]["execute"]>>;
      };
    } catch (e) {
      rollback(tx);
      throw e;
    }
  }

  // ==========================================================================
  // Transactions
  // ==========================================================================

  /** Execute operations in an explicit transaction */
  async transaction<T>(
    fn: (ctx: TransactionContext) => Promise<T>,
  ): Promise<T> {
    const tx = beginTx(this._db);

    const ctx: TransactionContext = {
      insert: <N extends NodeDef>(node: N) => {
        // Return a modified builder that uses the transaction
        const builder = createInsertBuilder(
          this._db,
          node,
          this.resolvePropKeyId.bind(this),
        );
        // Wrap to use transaction - need to handle overloads properly
        return {
          values: ((data: InferNodeInsert<N> | InferNodeInsert<N>[]) => {
            const isSingle = !Array.isArray(data);
            const executor = isSingle
              ? builder.values(data as InferNodeInsert<N>)
              : builder.values(data as InferNodeInsert<N>[]);
            return {
              ...executor,
              async returning() {
                // Execute within transaction context
                const op = executor._toBatchOp();
                const results = (await op.execute(tx)) as unknown[];
                // Apply isSingle logic
                return (isSingle ? results[0] : results) as never;
              },
              async execute() {
                const op = executor._toBatchOp();
                await op.execute(tx);
              },
              _toBatchOp: executor._toBatchOp,
            };
          }) as InsertBuilder<N>["values"],
        };
      },

      update: ((nodeOrRef: NodeDef | NodeRef) => {
        if ("$id" in nodeOrRef) {
          return createUpdateByRefBuilder(
            this._db,
            nodeOrRef,
            this.resolvePropKeyId.bind(this),
          );
        }
        return createUpdateBuilder(
          this._db,
          nodeOrRef,
          this.resolvePropKeyId.bind(this),
        );
      }) as TransactionContext["update"],

      delete: ((nodeOrRef: NodeDef | NodeRef) => {
        if ("$id" in nodeOrRef) {
          return createDeleteBuilder(this._db, nodeOrRef.$def)
            .where({ $id: nodeOrRef.$id })
            .execute();
        }
        return createDeleteBuilder(this._db, nodeOrRef);
      }) as TransactionContext["delete"],

      link: async <E extends EdgeDef>(
        src: NodeRef,
        edge: E,
        dst: NodeRef,
        props?: InferEdgeProps<E>,
      ) => {
        const executor = createLinkExecutor(
          this._db,
          src,
          edge,
          dst,
          props,
          this.resolveEtypeId.bind(this),
          this.resolvePropKeyId.bind(this),
        );
        const op = executor._toBatchOp();
        await op.execute(tx);
      },

      unlink: async <E extends EdgeDef>(
        src: NodeRef,
        edge: E,
        dst: NodeRef,
      ) => {
        const executor = createUnlinkExecutor(
          this._db,
          src,
          edge,
          dst,
          this.resolveEtypeId.bind(this),
        );
        const op = executor._toBatchOp();
        await op.execute(tx);
      },

      updateEdge: <E extends EdgeDef>(src: NodeRef, edge: E, dst: NodeRef) => {
        return createUpdateEdgeBuilder(
          this._db,
          src,
          edge,
          dst,
          this.resolveEtypeId.bind(this),
          this.resolvePropKeyId.bind(this),
        );
      },

      from: <N extends NodeDef>(node: NodeRef<N>) => {
        return createTraversalBuilder(
          this._db,
          [node],
          this.resolveEtypeId.bind(this),
          this.resolvePropKeyId.bind(this),
          this.getNodeDef.bind(this),
        );
      },

      get: async <N extends NodeDef>(
        node: N,
        key: Parameters<N["keyFn"]>[0],
      ) => {
        return this.get(node, key);
      },
    };

    try {
      const result = await fn(ctx);
      await commit(tx);
      return result;
    } catch (e) {
      rollback(tx);
      throw e;
    }
  }

  // ==========================================================================
  // Maintenance
  // ==========================================================================

  /** Get database statistics */
  async stats(): Promise<DbStats> {
    return rawStats(this._db);
  }

  /** Check database integrity */
  async check(): Promise<CheckResult> {
    return rawCheck(this._db);
  }

  /** Optimize (compact) the database */
  async optimize(): Promise<void> {
    return optimize(this._db);
  }

  /** Close the database */
  async close(): Promise<void> {
    return closeGraphDB(this._db);
  }

  /** Get the raw database handle (escape hatch) */
  get $raw(): GraphDB {
    return this._db;
  }

  // ==========================================================================
  // Private Helpers
  // ==========================================================================

  private fromPropValue(pv: import("../types.ts").PropValue): unknown {
    switch (pv.tag) {
      case 0: // NULL
        return null;
      case 1: // BOOL
        return pv.value;
      case 2: // I64
        return pv.value;
      case 3: // F64
        return pv.value;
      case 4: // STRING
        return pv.value;
      default:
        return null;
    }
  }
}

// ============================================================================
// Main Entry Point
// ============================================================================

/**
 * Open or create a ray database
 *
 * @example
 * ```ts
 * const db = await ray('./my-graph', {
 *   nodes: [user, company],
 *   edges: [knows, worksAt],
 * });
 * ```
 */
export async function ray(path: string, options: RayOptions): Promise<Ray> {
  return Ray.open(path, options);
}
