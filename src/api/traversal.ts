/**
 * Traversal Query Builder
 *
 * Fluent API for graph traversal with lazy AsyncIterable results.
 */

import {
  edgeExists,
  getEdgeProp,
  getNeighborsIn,
  getNeighborsOut,
  getNodeProp,
  nodeExists,
} from "../ray/__graph-db.ts";
import type {
  ETypeID,
  GraphDB,
  NodeID,
  PropKeyID,
  PropValue,
} from "../types.ts";
import { PropValueTag } from "../types.ts";
import { createNodeRef, type NodeRef } from "./builders.ts";
import type { EdgeDef, InferEdge, InferNode, NodeDef } from "./schema.ts";

// ============================================================================
// Traversal Options
// ============================================================================

export type TraversalDirection = "out" | "in" | "both";

export interface TraverseOptions {
  direction: TraversalDirection;
  minDepth?: number;
  maxDepth: number;
  unique?: boolean;
  whereEdge?: (edge: Record<string, unknown>) => boolean;
  whereNode?: (node: Record<string, unknown>) => boolean;
}

// ============================================================================
// Traversal Builder
// ============================================================================

export interface TraversalBuilder<N extends NodeDef = NodeDef> {
  /** Traverse outgoing edges of the given type */
  out<E extends EdgeDef>(edge: E): TraversalBuilder<N>;

  /** Traverse incoming edges of the given type */
  in<E extends EdgeDef>(edge: E): TraversalBuilder<N>;

  /** Traverse edges in both directions */
  both<E extends EdgeDef>(edge: E): TraversalBuilder<N>;

  /** Variable-depth traversal */
  traverse<E extends EdgeDef>(
    edge: E,
    options: TraverseOptions,
  ): TraversalBuilder<N>;

  /** Filter by edge properties */
  whereEdge(
    predicate: (edge: Record<string, unknown>) => boolean,
  ): TraversalBuilder<N>;

  /** Filter by node properties */
  whereNode(
    predicate: (node: Record<string, unknown>) => boolean,
  ): TraversalBuilder<N>;

  /** Limit the number of results */
  take(limit: number): TraversalBuilder<N>;

  /** Get the resulting nodes as an async iterable */
  nodes(): AsyncTraversalResult<NodeRef<N> & InferNode<N>>;

  /** Get the resulting edges as an async iterable */
  edges(): AsyncTraversalResult<EdgeResult>;

  /** Get the first result */
  first(): Promise<(NodeRef<N> & InferNode<N>) | null>;

  /** Count the results */
  count(): Promise<number>;

  /** Collect all results into an array */
  toArray(): Promise<(NodeRef<N> & InferNode<N>)[]>;

  /** Make the traversal iterable */
  [Symbol.asyncIterator](): AsyncIterator<NodeRef<N> & InferNode<N>>;
}

export interface EdgeResult {
  $src: NodeID;
  $dst: NodeID;
  $etype: ETypeID;
  [key: string]: unknown;
}

export interface AsyncTraversalResult<T> extends AsyncIterable<T> {
  toArray(): Promise<T[]>;
  first(): Promise<T | null>;
  count(): Promise<number>;
}

// ============================================================================
// Traversal Step
// ============================================================================

interface TraversalStep {
  type: "out" | "in" | "both" | "traverse";
  edgeDef: EdgeDef;
  options?: TraverseOptions;
}

// ============================================================================
// Implementation
// ============================================================================

export function createTraversalBuilder<N extends NodeDef>(
  db: GraphDB,
  startNodes: NodeRef[],
  resolveEtypeId: (edgeDef: EdgeDef) => ETypeID,
  resolvePropKeyId: (def: NodeDef | EdgeDef, propName: string) => PropKeyID,
  getNodeDef: (nodeId: NodeID) => NodeDef | null,
): TraversalBuilder<N> {
  const steps: TraversalStep[] = [];
  let edgeFilter: ((edge: Record<string, unknown>) => boolean) | null = null;
  let nodeFilter: ((node: Record<string, unknown>) => boolean) | null = null;
  let limit: number | null = null;

  // Helper to load node properties
  const loadNodeProps = (
    nodeId: NodeID,
    nodeDef: NodeDef,
  ): Record<string, unknown> => {
    const props: Record<string, unknown> = {};
    for (const [propName, propDef] of Object.entries(nodeDef.props)) {
      const propKeyId = resolvePropKeyId(nodeDef, propName);
      const propValue = getNodeProp(db, nodeId, propKeyId);
      if (propValue) {
        props[propName] = fromPropValue(propValue);
      }
    }
    return props;
  };

  // Helper to load edge properties
  const loadEdgeProps = (
    src: NodeID,
    etypeId: ETypeID,
    dst: NodeID,
    edgeDef: EdgeDef,
  ): Record<string, unknown> => {
    const props: Record<string, unknown> = {};
    for (const [propName, propDef] of Object.entries(edgeDef.props)) {
      const propKeyId = resolvePropKeyId(edgeDef, propName);
      const propValue = getEdgeProp(db, src, etypeId, dst, propKeyId);
      if (propValue) {
        props[propName] = fromPropValue(propValue);
      }
    }
    return props;
  };

  // Execute a single step
  async function* executeStep(
    currentNodes: AsyncIterable<NodeRef>,
    step: TraversalStep,
  ): AsyncGenerator<{ node: NodeRef; edge: EdgeResult }> {
    const etypeId = resolveEtypeId(step.edgeDef);

    for await (const node of currentNodes) {
      if (step.type === "traverse") {
        // Variable-depth traversal
        yield* executeTraverse(node, step, etypeId);
      } else {
        // Single-hop traversal
        yield* executeSingleHop(node, step.type, step.edgeDef, etypeId);
      }
    }
  }

  // Execute single-hop traversal
  function* executeSingleHop(
    node: NodeRef,
    direction: "out" | "in" | "both",
    edgeDef: EdgeDef,
    etypeId: ETypeID,
  ): Generator<{ node: NodeRef; edge: EdgeResult }> {
    const directions: ("out" | "in")[] =
      direction === "both" ? ["out", "in"] : [direction];

    for (const dir of directions) {
      const neighbors =
        dir === "out"
          ? getNeighborsOut(db, node.$id, etypeId)
          : getNeighborsIn(db, node.$id, etypeId);

      for (const edge of neighbors) {
        const neighborId = dir === "out" ? edge.dst : edge.src;

        // Skip if node doesn't exist
        if (!nodeExists(db, neighborId)) continue;

        // Get node definition (for now, use a generic one)
        const neighborDef = getNodeDef(neighborId);
        if (!neighborDef) continue;

        const props = loadNodeProps(neighborId, neighborDef);
        const neighborKey = ""; // We'd need to look this up from the key index

        const neighborRef = createNodeRef(
          neighborDef,
          neighborId,
          neighborKey,
          props,
        );

        const edgeProps = loadEdgeProps(edge.src, etypeId, edge.dst, edgeDef);
        const edgeResult: EdgeResult = {
          $src: edge.src,
          $dst: edge.dst,
          $etype: etypeId,
          ...edgeProps,
        };

        yield { node: neighborRef, edge: edgeResult };
      }
    }
  }

  // Execute variable-depth traversal (BFS)
  async function* executeTraverse(
    startNode: NodeRef,
    step: TraversalStep,
    etypeId: ETypeID,
  ): AsyncGenerator<{ node: NodeRef; edge: EdgeResult }> {
    const options = step.options!;
    const minDepth = options.minDepth ?? 1;
    const maxDepth = options.maxDepth;
    const unique = options.unique ?? true;

    const visited = new Set<bigint>();
    if (unique) {
      visited.add(startNode.$id);
    }

    // BFS queue: [nodeRef, depth]
    const queue: [NodeRef, number][] = [[startNode, 0]];

    while (queue.length > 0) {
      const [currentNode, depth] = queue.shift()!;

      if (depth >= maxDepth) continue;

      // Get neighbors
      for (const result of executeSingleHop(
        currentNode,
        options.direction,
        step.edgeDef,
        etypeId,
      )) {
        const neighborId = result.node.$id;

        // Check uniqueness
        if (unique && visited.has(neighborId)) continue;
        if (unique) visited.add(neighborId);

        // Apply filters
        if (options.whereEdge && !options.whereEdge(result.edge)) continue;
        if (options.whereNode && !options.whereNode(result.node)) continue;

        // Yield if at or past minDepth
        if (depth + 1 >= minDepth) {
          yield result;
        }

        // Continue traversal
        if (depth + 1 < maxDepth) {
          queue.push([result.node, depth + 1]);
        }
      }
    }
  }

  // Main execution generator
  async function* execute(): AsyncGenerator<{
    node: NodeRef;
    edge: EdgeResult | null;
  }> {
    let currentResults: AsyncIterable<{
      node: NodeRef;
      edge: EdgeResult | null;
    }> = (async function* () {
      for (const node of startNodes) {
        yield { node, edge: null };
      }
    })();

    // Apply each step
    for (const step of steps) {
      const prevResults = currentResults;
      currentResults = (async function* () {
        const nodeIter = (async function* () {
          for await (const r of prevResults) {
            yield r.node;
          }
        })();
        yield* executeStep(nodeIter, step);
      })();
    }

    // Apply filters and limit
    let count = 0;
    for await (const result of currentResults) {
      // Apply edge filter (only if we have an edge from traversal)
      if (edgeFilter && result.edge && !edgeFilter(result.edge)) continue;

      // Apply node filter
      if (nodeFilter && !nodeFilter(result.node)) continue;

      // Check limit
      if (limit !== null && count >= limit) break;

      yield result;
      count++;
    }
  }

  const builder: TraversalBuilder<N> = {
    out<E extends EdgeDef>(edge: E) {
      steps.push({ type: "out", edgeDef: edge });
      return builder;
    },

    in<E extends EdgeDef>(edge: E) {
      steps.push({ type: "in", edgeDef: edge });
      return builder;
    },

    both<E extends EdgeDef>(edge: E) {
      steps.push({ type: "both", edgeDef: edge });
      return builder;
    },

    traverse<E extends EdgeDef>(edge: E, options: TraverseOptions) {
      steps.push({ type: "traverse", edgeDef: edge, options });
      return builder;
    },

    whereEdge(predicate) {
      edgeFilter = predicate;
      return builder;
    },

    whereNode(predicate) {
      nodeFilter = predicate;
      return builder;
    },

    take(n) {
      limit = n;
      return builder;
    },

    nodes(): AsyncTraversalResult<NodeRef<N> & InferNode<N>> {
      const iter = async function* () {
        for await (const result of execute()) {
          yield result.node as NodeRef<N> & InferNode<N>;
        }
      };

      return {
        [Symbol.asyncIterator]: iter,
        async toArray() {
          const results: (NodeRef<N> & InferNode<N>)[] = [];
          for await (const node of iter()) {
            results.push(node);
          }
          return results;
        },
        async first() {
          for await (const node of iter()) {
            return node;
          }
          return null;
        },
        async count() {
          let c = 0;
          for await (const _ of iter()) {
            c++;
          }
          return c;
        },
      };
    },

    edges(): AsyncTraversalResult<EdgeResult> {
      const iter = async function* () {
        for await (const result of execute()) {
          if (result.edge) {
            yield result.edge;
          }
        }
      };

      return {
        [Symbol.asyncIterator]: iter,
        async toArray() {
          const results: EdgeResult[] = [];
          for await (const edge of iter()) {
            results.push(edge);
          }
          return results;
        },
        async first() {
          for await (const edge of iter()) {
            return edge;
          }
          return null;
        },
        async count() {
          let c = 0;
          for await (const _ of iter()) {
            c++;
          }
          return c;
        },
      };
    },

    async first() {
      return this.nodes().first();
    },

    async count() {
      return this.nodes().count();
    },

    async toArray() {
      return this.nodes().toArray();
    },

    [Symbol.asyncIterator]() {
      return this.nodes()[Symbol.asyncIterator]();
    },
  };

  return builder;
}

// ============================================================================
// Helpers
// ============================================================================

function fromPropValue(pv: PropValue): unknown {
  switch (pv.tag) {
    case PropValueTag.NULL:
      return null;
    case PropValueTag.BOOL:
      return pv.value;
    case PropValueTag.I64:
      return pv.value;
    case PropValueTag.F64:
      return pv.value;
    case PropValueTag.STRING:
      return pv.value;
    default:
      return null;
  }
}
