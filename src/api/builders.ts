/**
 * Query Builders
 *
 * Drizzle-style fluent builders for insert, update, delete operations.
 */

import {
  addEdge,
  beginTx,
  commit,
  createNode,
  delEdgeProp,
  deleteEdge,
  deleteNode,
  delNodeProp,
  getNodeByKey,
  setEdgeProp,
  setNodeProp,
} from "../ray/__graph-db.ts";
import type {
  ETypeID,
  GraphDB,
  NodeID,
  PropKeyID,
  PropValue,
  TxHandle,
} from "../types.ts";
import { PropValueTag } from "../types.ts";
import type {
  EdgeDef,
  InferEdgeProps,
  InferNode,
  InferNodeInsert,
  NodeDef,
} from "./schema.ts";

// ============================================================================
// Node Reference (returned from operations)
// ============================================================================

/** A node reference with its ID, key, and properties */
export interface NodeRef<N extends NodeDef = NodeDef> {
  readonly $id: NodeID;
  readonly $key: string;
  readonly $def: N;
  [key: string]: unknown;
}

/** Create a node reference from raw data */
export function createNodeRef<N extends NodeDef>(
  def: N,
  id: NodeID,
  key: string,
  props: Record<string, unknown>,
): NodeRef<N> & InferNode<N> {
  return {
    $id: id,
    $key: key,
    $def: def,
    ...props,
  } as NodeRef<N> & InferNode<N>;
}

// ============================================================================
// Insert Builder
// ============================================================================

export interface InsertBuilder<N extends NodeDef> {
  values(data: InferNodeInsert<N>): InsertExecutorSingle<N>;
  values(data: InferNodeInsert<N>[]): InsertExecutorMultiple<N>;
}

export interface InsertExecutorSingle<N extends NodeDef> {
  /** Execute insert and return the created node */
  returning(): Promise<NodeRef<N> & InferNode<N>>;
  /** Execute insert without returning */
  execute(): Promise<void>;
  /** For batch operations - returns the operation descriptor */
  _toBatchOp(): BatchOperation;
}

export interface InsertExecutorMultiple<N extends NodeDef> {
  /** Execute insert and return the created nodes */
  returning(): Promise<(NodeRef<N> & InferNode<N>)[]>;
  /** Execute insert without returning */
  execute(): Promise<void>;
  /** For batch operations - returns the operation descriptor */
  _toBatchOp(): BatchOperation;
}

/** @deprecated Use InsertExecutorSingle or InsertExecutorMultiple */
export type InsertExecutor<N extends NodeDef> =
  | InsertExecutorSingle<N>
  | InsertExecutorMultiple<N>;

export interface BatchOperation {
  type: "insert" | "update" | "delete" | "link" | "unlink";
  execute(tx: TxHandle): Promise<unknown>;
}

export function createInsertBuilder<N extends NodeDef>(
  db: GraphDB,
  nodeDef: N,
  resolvePropKeyId: (nodeDef: NodeDef, propName: string) => PropKeyID,
): InsertBuilder<N> {
  function values(data: InferNodeInsert<N>): InsertExecutorSingle<N>;
  function values(data: InferNodeInsert<N>[]): InsertExecutorMultiple<N>;
  function values(
    data: InferNodeInsert<N> | InferNodeInsert<N>[],
  ): InsertExecutorSingle<N> | InsertExecutorMultiple<N> {
    const dataArray = Array.isArray(data) ? data : [data];
    const isSingle = !Array.isArray(data);

    const execute = async (
      tx?: TxHandle,
    ): Promise<(NodeRef<N> & InferNode<N>)[]> => {
      const ownTx = !tx;
      const handle = tx ?? beginTx(db);
      const results: (NodeRef<N> & InferNode<N>)[] = [];

      for (const item of dataArray) {
        const { key: keyArg, ...props } = item as {
          key: string | number;
        } & Record<string, unknown>;
        const fullKey = nodeDef.keyFn(keyArg as never);

        // Create the node
        const nodeId = createNode(handle, { key: fullKey });

        // Set properties
        for (const [propName, value] of Object.entries(props)) {
          if (value === undefined) continue;
          const propDef = nodeDef.props[propName];
          if (!propDef) continue;

          const propKeyId = resolvePropKeyId(nodeDef, propName);
          const propValue = toPropValue(propDef, value);
          setNodeProp(handle, nodeId, propKeyId, propValue);
        }

        results.push(createNodeRef(nodeDef, nodeId, fullKey, props));
      }

      if (ownTx) {
        await commit(handle);
      }

      return results;
    };

    if (isSingle) {
      return {
        async returning() {
          const results = await execute();
          return results[0];
        },
        async execute() {
          await execute();
        },
        _toBatchOp(): BatchOperation {
          return {
            type: "insert",
            execute: async (tx) => execute(tx),
          };
        },
      };
    }
    return {
      async returning() {
        return execute();
      },
      async execute() {
        await execute();
      },
      _toBatchOp(): BatchOperation {
        return {
          type: "insert",
          execute: async (tx) => execute(tx),
        };
      },
    };
  }

  return { values };
}

// ============================================================================
// Update Builder
// ============================================================================

export interface UpdateBuilder<N extends NodeDef> {
  set(data: Partial<InferNode<N>>): UpdateExecutor<N>;
}

export interface UpdateExecutor<N extends NodeDef> {
  where(condition: WhereCondition<N>): UpdateExecutor<N>;
  execute(): Promise<void>;
  _toBatchOp(): BatchOperation;
}

export type WhereCondition<N extends NodeDef> =
  | { $key: string }
  | { $id: NodeID };

export function createUpdateBuilder<N extends NodeDef>(
  db: GraphDB,
  nodeDef: N,
  resolvePropKeyId: (nodeDef: NodeDef, propName: string) => PropKeyID,
): UpdateBuilder<N> {
  return {
    set(data) {
      let whereCondition: WhereCondition<N> | null = null;

      const execute = async (tx?: TxHandle): Promise<void> => {
        if (!whereCondition) {
          throw new Error("Update requires a where condition");
        }

        const ownTx = !tx;
        const handle = tx ?? beginTx(db);

        // Resolve node ID
        let nodeId: NodeID | null = null;
        if ("$id" in whereCondition) {
          nodeId = whereCondition.$id;
        } else if ("$key" in whereCondition) {
          nodeId = getNodeByKey(db, whereCondition.$key);
        }

        if (nodeId === null) {
          throw new Error("Node not found");
        }

        // Update properties
        for (const [propName, value] of Object.entries(data)) {
          if (propName.startsWith("$")) continue;
          const propDef = nodeDef.props[propName];
          if (!propDef) continue;

          const propKeyId = resolvePropKeyId(nodeDef, propName);

          if (value === undefined || value === null) {
            delNodeProp(handle, nodeId, propKeyId);
          } else {
            const propValue = toPropValue(propDef, value);
            setNodeProp(handle, nodeId, propKeyId, propValue);
          }
        }

        if (ownTx) {
          await commit(handle);
        }
      };

      const executor: UpdateExecutor<N> = {
        where(condition) {
          whereCondition = condition;
          return executor;
        },
        async execute() {
          await execute();
        },
        _toBatchOp(): BatchOperation {
          return {
            type: "update",
            execute: async (tx) => execute(tx),
          };
        },
      };

      return executor;
    },
  };
}

// ============================================================================
// Update by Node Reference
// ============================================================================

export interface UpdateByRefBuilder<N extends NodeDef> {
  set(data: Partial<InferNode<N>>): UpdateByRefExecutor;
}

export interface UpdateByRefExecutor {
  execute(): Promise<void>;
  _toBatchOp(): BatchOperation;
}

export function createUpdateByRefBuilder<N extends NodeDef>(
  db: GraphDB,
  nodeRef: NodeRef<N>,
  resolvePropKeyId: (nodeDef: NodeDef, propName: string) => PropKeyID,
): UpdateByRefBuilder<N> {
  return {
    set(data) {
      const execute = async (tx?: TxHandle): Promise<void> => {
        const ownTx = !tx;
        const handle = tx ?? beginTx(db);

        for (const [propName, value] of Object.entries(data)) {
          if (propName.startsWith("$")) continue;
          const propDef = nodeRef.$def.props[propName];
          if (!propDef) continue;

          const propKeyId = resolvePropKeyId(nodeRef.$def, propName);

          if (value === undefined || value === null) {
            delNodeProp(handle, nodeRef.$id, propKeyId);
          } else {
            const propValue = toPropValue(propDef, value);
            setNodeProp(handle, nodeRef.$id, propKeyId, propValue);
          }
        }

        if (ownTx) {
          await commit(handle);
        }
      };

      return {
        async execute() {
          await execute();
        },
        _toBatchOp(): BatchOperation {
          return {
            type: "update",
            execute: async (tx) => execute(tx),
          };
        },
      };
    },
  };
}

// ============================================================================
// Delete Builder
// ============================================================================

export interface DeleteBuilder<N extends NodeDef> {
  where(condition: WhereCondition<N>): DeleteExecutor;
}

export interface DeleteExecutor {
  execute(): Promise<boolean>;
  _toBatchOp(): BatchOperation;
}

export function createDeleteBuilder<N extends NodeDef>(
  db: GraphDB,
  _nodeDef: N,
): DeleteBuilder<N> {
  return {
    where(condition) {
      const execute = async (tx?: TxHandle): Promise<boolean> => {
        const ownTx = !tx;
        const handle = tx ?? beginTx(db);

        let nodeId: NodeID | null = null;
        if ("$id" in condition) {
          nodeId = condition.$id;
        } else if ("$key" in condition) {
          nodeId = getNodeByKey(db, condition.$key);
        }

        if (nodeId === null) {
          if (ownTx) {
            await commit(handle);
          }
          return false;
        }

        const result = deleteNode(handle, nodeId);

        if (ownTx) {
          await commit(handle);
        }

        return result;
      };

      return {
        async execute() {
          return execute();
        },
        _toBatchOp(): BatchOperation {
          return {
            type: "delete",
            execute: async (tx) => execute(tx),
          };
        },
      };
    },
  };
}

// ============================================================================
// Link Builder (create edge)
// ============================================================================

export interface LinkExecutor {
  execute(): Promise<void>;
  _toBatchOp(): BatchOperation;
}

export function createLinkExecutor<E extends EdgeDef>(
  db: GraphDB,
  src: NodeRef,
  edgeDef: E,
  dst: NodeRef,
  props: InferEdgeProps<E> | undefined,
  resolveEtypeId: (edgeDef: EdgeDef) => ETypeID,
  resolvePropKeyId: (edgeDef: EdgeDef, propName: string) => PropKeyID,
): LinkExecutor {
  const execute = async (tx?: TxHandle): Promise<void> => {
    const ownTx = !tx;
    const handle = tx ?? beginTx(db);

    const etypeId = resolveEtypeId(edgeDef);
    addEdge(handle, src.$id, etypeId, dst.$id);

    // Set edge properties if provided
    if (props) {
      for (const [propName, value] of Object.entries(props)) {
        if (value === undefined) continue;
        const propDef = edgeDef.props[propName];
        if (!propDef) continue;

        const propKeyId = resolvePropKeyId(edgeDef, propName);
        const propValue = toPropValue(propDef, value);
        setEdgeProp(handle, src.$id, etypeId, dst.$id, propKeyId, propValue);
      }
    }

    if (ownTx) {
      await commit(handle);
    }
  };

  return {
    async execute() {
      await execute();
    },
    _toBatchOp(): BatchOperation {
      return {
        type: "link",
        execute: async (tx) => execute(tx),
      };
    },
  };
}

// ============================================================================
// Unlink Builder (delete edge)
// ============================================================================

export function createUnlinkExecutor<E extends EdgeDef>(
  db: GraphDB,
  src: NodeRef,
  edgeDef: E,
  dst: NodeRef,
  resolveEtypeId: (edgeDef: EdgeDef) => ETypeID,
): LinkExecutor {
  const execute = async (tx?: TxHandle): Promise<void> => {
    const ownTx = !tx;
    const handle = tx ?? beginTx(db);

    const etypeId = resolveEtypeId(edgeDef);
    deleteEdge(handle, src.$id, etypeId, dst.$id);

    if (ownTx) {
      await commit(handle);
    }
  };

  return {
    async execute() {
      await execute();
    },
    _toBatchOp(): BatchOperation {
      return {
        type: "unlink",
        execute: async (tx) => execute(tx),
      };
    },
  };
}

// ============================================================================
// Update Edge Builder
// ============================================================================

export interface UpdateEdgeBuilder<E extends EdgeDef> {
  set(data: Partial<InferEdgeProps<E>>): UpdateEdgeExecutor;
}

export interface UpdateEdgeExecutor {
  execute(): Promise<void>;
  _toBatchOp(): BatchOperation;
}

export function createUpdateEdgeBuilder<E extends EdgeDef>(
  db: GraphDB,
  src: NodeRef,
  edgeDef: E,
  dst: NodeRef,
  resolveEtypeId: (edgeDef: EdgeDef) => ETypeID,
  resolvePropKeyId: (edgeDef: EdgeDef, propName: string) => PropKeyID,
): UpdateEdgeBuilder<E> {
  return {
    set(data) {
      const execute = async (tx?: TxHandle): Promise<void> => {
        const ownTx = !tx;
        const handle = tx ?? beginTx(db);

        const etypeId = resolveEtypeId(edgeDef);

        for (const [propName, value] of Object.entries(data)) {
          const propDef = edgeDef.props[propName];
          if (!propDef) continue;

          const propKeyId = resolvePropKeyId(edgeDef, propName);

          if (value === undefined || value === null) {
            delEdgeProp(handle, src.$id, etypeId, dst.$id, propKeyId);
          } else {
            const propValue = toPropValue(propDef, value);
            setEdgeProp(
              handle,
              src.$id,
              etypeId,
              dst.$id,
              propKeyId,
              propValue,
            );
          }
        }

        if (ownTx) {
          await commit(handle);
        }
      };

      return {
        async execute() {
          await execute();
        },
        _toBatchOp(): BatchOperation {
          return {
            type: "update",
            execute: async (tx) => execute(tx),
          };
        },
      };
    },
  };
}

// ============================================================================
// Helpers
// ============================================================================

interface PropDefLike {
  type: "string" | "int" | "float" | "bool";
}

function toPropValue(propDef: PropDefLike, value: unknown): PropValue {
  switch (propDef.type) {
    case "string":
      return { tag: PropValueTag.STRING, value: value as string };
    case "int":
      return { tag: PropValueTag.I64, value: BigInt(value as number | bigint) };
    case "float":
      return { tag: PropValueTag.F64, value: value as number };
    case "bool":
      return { tag: PropValueTag.BOOL, value: value as boolean };
    default:
      throw new Error(`Unknown prop type: ${propDef.type}`);
  }
}
