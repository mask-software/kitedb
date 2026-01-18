/**
 * Traversal Query Builder
 *
 * Fluent API for graph traversal with lazy AsyncIterable results.
 */

import {
  getEdgeProp,
  getNeighborsIn,
  getNeighborsOut,
  getNodeProp,
} from "../ray/graph-db/index.ts";
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

  /**
   * Select specific properties to load (optimization)
   * Only the specified properties will be loaded, reducing overhead.
   * @param props - Array of property names to load
   */
  select<K extends keyof InferNode<N>>(props: K[]): TraversalBuilder<N>;

  /** Get the resulting nodes as an async iterable */
  nodes(): AsyncTraversalResult<NodeRef<N> & InferNode<N>>;

  /** Get the resulting edges as an async iterable */
  edges(): AsyncTraversalResult<EdgeResult>;

  /**
   * Get raw edge data without materializing nodes (zero-copy traversal)
   * This is the fastest way to iterate through edges without any property loading.
   * Returns raw Edge objects from the underlying graph.
   */
  rawEdges(): Generator<RawEdge>;

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

/** Raw edge data without any property loading */
export interface RawEdge {
  src: NodeID;
  dst: NodeID;
  etype: ETypeID;
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
  let selectedProps: string[] | null = null;

  // Cache for propKeyIds to avoid repeated lookups
  const propKeyCache = new Map<string, PropKeyID>();
  const getCachedPropKeyId = (def: NodeDef | EdgeDef, propName: string): PropKeyID => {
    const cacheKey = `${def.name}:${propName}`;
    let keyId = propKeyCache.get(cacheKey);
    if (keyId === undefined) {
      keyId = resolvePropKeyId(def, propName);
      propKeyCache.set(cacheKey, keyId);
    }
    return keyId;
  };

  // Helper to load node properties (with optional selective loading)
  const loadNodeProps = (
    nodeId: NodeID,
    nodeDef: NodeDef,
  ): Record<string, unknown> => {
    const props: Record<string, unknown> = {};
    // If selectedProps is set, only load those properties
    const propsToLoad = selectedProps 
      ? Object.entries(nodeDef.props).filter(([name]) => selectedProps!.includes(name))
      : Object.entries(nodeDef.props);
    
    for (const [propName, propDef] of propsToLoad) {
      const propKeyId = getCachedPropKeyId(nodeDef, propName);
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
      const propKeyId = getCachedPropKeyId(edgeDef, propName);
      const propValue = getEdgeProp(db, src, etypeId, dst, propKeyId);
      if (propValue) {
        props[propName] = fromPropValue(propValue);
      }
    }
    return props;
  };

  // ============================================================================
  // Fast count path - only iterates through node IDs without loading properties
  // ============================================================================
  
  // Fast single-hop iteration - yields only node IDs without property loading
  function* iterateSingleHopIds(
    nodeId: NodeID,
    direction: "out" | "in" | "both",
    etypeId: ETypeID,
  ): Generator<NodeID> {
    const directions: ("out" | "in")[] =
      direction === "both" ? ["out", "in"] : [direction];

    for (const dir of directions) {
      const neighbors =
        dir === "out"
          ? getNeighborsOut(db, nodeId, etypeId)
          : getNeighborsIn(db, nodeId, etypeId);

      for (const edge of neighbors) {
        yield dir === "out" ? edge.dst : edge.src;
      }
    }
  }

  // Fast count for simple traversals without filters
  // Optimization: Uses Set for deduplication to get accurate unique node counts
  function countFast(): number {
    // Can only use fast path if no filters are set
    if (edgeFilter !== null || nodeFilter !== null) {
      return -1; // Signal to use slow path
    }
    
    // Can only use fast path for simple single-hop traversals (no variable-depth)
    for (const step of steps) {
      if (step.type === "traverse") {
        return -1; // Variable-depth requires the slow path
      }
    }

    // Use Set for deduplication to get accurate counts for graphs with multiple paths
    let currentNodeIds = new Set<NodeID>(startNodes.map(n => n.$id));

    for (const step of steps) {
      const etypeId = resolveEtypeId(step.edgeDef);
      const nextNodeIds = new Set<NodeID>();

      for (const nodeId of currentNodeIds) {
        for (const neighborId of iterateSingleHopIds(nodeId, step.type as "out" | "in" | "both", etypeId)) {
          nextNodeIds.add(neighborId);
        }
      }

      currentNodeIds = nextNodeIds;
    }

    // Apply limit if set
    if (limit !== null && currentNodeIds.size > limit) {
      return limit;
    }

    return currentNodeIds.size;
  }

  // ============================================================================
  // Full execution path with property loading (for toArray, first, iteration)
  // ============================================================================

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

  // Execute single-hop traversal with full property loading
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

        // Get node definition
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
  // Optimization: Uses index-based queue for O(1) dequeue instead of O(n) Array.shift()
  async function* executeTraverse(
    startNode: NodeRef,
    step: TraversalStep,
    etypeId: ETypeID,
  ): AsyncGenerator<{ node: NodeRef; edge: EdgeResult }> {
    const options = step.options!;
    const minDepth = options.minDepth ?? 1;
    const maxDepth = options.maxDepth;
    const unique = options.unique ?? true;

    const visited = new Set<NodeID>();
    if (unique) {
      visited.add(startNode.$id);
    }

    // BFS queue: [nodeRef, depth]
    // Use index-based approach for O(1) dequeue (Array.shift is O(n))
    // Compact the queue periodically to prevent unbounded memory growth
    let queue: [NodeRef, number][] = [[startNode, 0]];
    let queueHead = 0;
    const COMPACT_THRESHOLD = 1000; // Compact when head exceeds this

    while (queueHead < queue.length) {
      const [currentNode, depth] = queue[queueHead++]!;

      // Compact the queue to free memory from processed items
      if (queueHead >= COMPACT_THRESHOLD && queueHead > queue.length / 2) {
        queue = queue.slice(queueHead);
        queueHead = 0;
      }

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

  // Main execution generator (full property loading)
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

    select<K extends keyof InferNode<N>>(props: K[]) {
      // Store selected properties for selective loading
      selectedProps = props as string[];
      return builder;
    },

    rawEdges(): Generator<RawEdge> {
      // Return a generator that yields raw edge data without any property loading
      // This is the fastest possible traversal mode
      return (function* () {
        // Can only process single-hop simple traversals
        if (steps.length === 0) return;
        
        let currentNodeIds: NodeID[] = startNodes.map(n => n.$id);

        for (const step of steps) {
          if (step.type === "traverse") {
            // Variable-depth not supported in rawEdges
            throw new Error("rawEdges() does not support variable-depth traverse()");
          }

          const etypeId = resolveEtypeId(step.edgeDef);
          const directions: ("out" | "in")[] =
            step.type === "both" ? ["out", "in"] : [step.type as "out" | "in"];
          const nextNodeIds: NodeID[] = [];

          for (const nodeId of currentNodeIds) {
            for (const dir of directions) {
              const neighbors =
                dir === "out"
                  ? getNeighborsOut(db, nodeId, etypeId)
                  : getNeighborsIn(db, nodeId, etypeId);

              for (const edge of neighbors) {
                // Yield correct edge direction (src is always the source, dst is always the target)
                yield { src: edge.src, dst: edge.dst, etype: edge.etype };
                // Track neighbor for next hop
                nextNodeIds.push(dir === "out" ? edge.dst : edge.src);
              }
            }
          }

          currentNodeIds = nextNodeIds;
        }
      })();
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
          // Try fast path first
          const fastCount = countFast();
          if (fastCount >= 0) {
            return fastCount;
          }
          // Fall back to slow path
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
          // Try fast path first
          const fastCount = countFast();
          if (fastCount >= 0) {
            return fastCount;
          }
          // Fall back to slow path
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
      // Try fast path first
      const fastCount = countFast();
      if (fastCount >= 0) {
        return fastCount;
      }
      // Fall back to slow path
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
