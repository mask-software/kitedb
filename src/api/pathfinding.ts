/**
 * Path Finding Algorithms
 *
 * Dijkstra and A* shortest path algorithms for graph traversal.
 * Supports weighted edges via properties or custom functions.
 */

import {
  getEdgeProp,
  getNeighborsIn,
  getNeighborsOut,
  getNodeProp,
  nodeExists,
} from "../ray/graph-db/index.ts";
import { getNodeKey } from "../ray/key-index.ts";
import type {
  ETypeID,
  GraphDB,
  NodeID,
  PropKeyID,
  PropValue,
} from "../types.ts";
import { PropValueTag } from "../types.ts";
import { MinHeap } from "../util/heap.ts";
import { createNodeRef, type NodeRef } from "./builders.ts";
import type { EdgeDef, InferEdgeProps, NodeDef } from "./schema.ts";
import type { EdgeResult, TraversalDirection } from "./traversal.ts";

// ============================================================================
// Types
// ============================================================================

/**
 * Weight specification for pathfinding
 * Supports both edge property-based weights and custom weight functions
 */
export type WeightSpec<E extends EdgeDef> =
  | { property: keyof InferEdgeProps<E> }
  | { fn: (edge: EdgeResult) => number };

/**
 * Heuristic function for A* algorithm
 * Estimates the distance from current node to goal node
 */
export type Heuristic<N extends NodeDef> = (
  current: NodeRef<N>,
  goal: NodeRef<N>,
) => number;

/**
 * Result of a pathfinding query
 */
export interface PathResult<N extends NodeDef> {
  /** Nodes in order from source to target */
  path: NodeRef<N>[];
  /** Edges traversed in order */
  edges: EdgeResult[];
  /** Sum of edge weights along the path */
  totalWeight: number;
  /** Whether a path was found */
  found: boolean;
}

/**
 * Builder for configuring pathfinding queries
 */
export interface PathFindingBuilder<N extends NodeDef> {
  /** Set the target node */
  to(target: NodeRef<N>): PathExecutor<N>;
  /** Set multiple target nodes (find path to any) */
  toAny(targets: NodeRef<N>[]): PathExecutor<N>;
  /** Restrict traversal to specific edge type (can be chained) */
  via<E extends EdgeDef>(edge: E): PathFindingBuilder<N>;
  /** Maximum depth to search */
  maxDepth(depth: number): PathFindingBuilder<N>;
  /** Traversal direction */
  direction(dir: TraversalDirection): PathFindingBuilder<N>;
}

/**
 * Executor for pathfinding algorithms
 */
export interface PathExecutor<N extends NodeDef> {
  /** Execute Dijkstra's algorithm */
  dijkstra(): Promise<PathResult<N>>;
  /** Execute A* algorithm with heuristic */
  aStar(heuristic: Heuristic<N>): Promise<PathResult<N>>;
  /** Find all paths up to maxPaths (optional) */
  allPaths(maxPaths?: number): AsyncIterable<PathResult<N>>;
}

// ============================================================================
// Internal Types
// ============================================================================

interface PathState {
  nodeId: NodeID;
  cost: number; // g(n) - actual cost from source
  depth: number; // Hop count from source
  parent: NodeID | null;
  edge: EdgeResult | null; // Edge used to reach this node
}

interface PathFindingConfig {
  source: NodeRef;
  targets: NodeRef[];
  allowedEtypes: Set<ETypeID>;
  direction: TraversalDirection;
  maxDepth: number;
  weightSpec: WeightSpec<EdgeDef> | undefined;
}

// ============================================================================
// Helper Functions
// ============================================================================

/**
 * Convert PropValue to number for weight calculation
 */
function propValueToNumber(pv: PropValue | null): number {
  if (!pv) return 1.0;

  switch (pv.tag) {
    case PropValueTag.I64:
      return Number(pv.value);
    case PropValueTag.F64:
      return pv.value;
    case PropValueTag.BOOL:
      return pv.value ? 1.0 : 0.0;
    case PropValueTag.STRING:
      // Try to parse string as number
      const parsed = Number.parseFloat(pv.value);
      return Number.isNaN(parsed) ? 1.0 : parsed;
    default:
      return 1.0;
  }
}

/**
 * Load edge properties into EdgeResult
 */
function loadEdgeProperties(
  db: GraphDB,
  src: NodeID,
  etypeId: ETypeID,
  dst: NodeID,
  edgeDef: EdgeDef,
  resolvePropKeyId: (def: EdgeDef, propName: string) => PropKeyID,
): EdgeResult {
  const props: Record<string, unknown> = {};
  for (const [propName, propDef] of Object.entries(edgeDef.props)) {
    const propKeyId = resolvePropKeyId(edgeDef, propName);
    const propValue = getEdgeProp(db, src, etypeId, dst, propKeyId);
    if (propValue) {
      props[propName] = propValueToNumber(propValue);
    }
  }

  return {
    $src: src,
    $dst: dst,
    $etype: etypeId,
    ...props,
  };
}

/**
 * Create weight function from WeightSpec
 */
function createWeightFunction<E extends EdgeDef>(
  db: GraphDB,
  weightSpec: WeightSpec<E> | undefined,
  edgeDef: EdgeDef,
  resolvePropKeyId: (def: EdgeDef, propName: string) => PropKeyID,
): (edge: EdgeResult) => number {
  if (!weightSpec) {
    // Default: unweighted (all edges have weight 1)
    return () => 1.0;
  }

  if ("property" in weightSpec) {
    // Use edge property
    const propName = weightSpec.property as string;
    const propKeyId = resolvePropKeyId(edgeDef, propName);

    return (edge: EdgeResult) => {
      const propValue = getEdgeProp(
        db,
        edge.$src as NodeID,
        edge.$etype,
        edge.$dst as NodeID,
        propKeyId,
      );
      const weight = propValueToNumber(propValue);
      return weight > 0 ? weight : 1.0; // Ensure positive weights
    };
  } else {
    // Use custom function
    return (edge: EdgeResult) => {
      const weight = weightSpec.fn(edge);
      return weight > 0 ? weight : 1.0; // Ensure positive weights
    };
  }
}

/**
 * Load node reference with properties
 */
function loadNodeRef(
  db: GraphDB,
  nodeId: NodeID,
  nodeDef: NodeDef,
  resolvePropKeyId: (def: NodeDef, propName: string) => PropKeyID,
  getNodeDef: (nodeId: NodeID) => NodeDef | null,
): NodeRef | null {
  if (!nodeExists(db, nodeId)) {
    return null;
  }

  const def = getNodeDef(nodeId) || nodeDef;
  const key = getNodeKey(db._snapshot, db._delta, nodeId) || "";

  const props: Record<string, unknown> = {};
  for (const [propName, propDef] of Object.entries(def.props)) {
    const propKeyId = resolvePropKeyId(def, propName);
    const propValue = getNodeProp(db, nodeId, propKeyId);
    if (propValue) {
      switch (propValue.tag) {
        case PropValueTag.NULL:
          props[propName] = null;
          break;
        case PropValueTag.BOOL:
          props[propName] = propValue.value;
          break;
        case PropValueTag.I64:
          props[propName] = propValue.value;
          break;
        case PropValueTag.F64:
          props[propName] = propValue.value;
          break;
        case PropValueTag.STRING:
          props[propName] = propValue.value;
          break;
      }
    }
  }

  return createNodeRef(def, nodeId, key, props);
}

/**
 * Reconstruct path from parent pointers
 */
function reconstructPath<N extends NodeDef>(
  states: Map<NodeID, PathState>,
  targetId: NodeID,
  sourceId: NodeID,
  db: GraphDB,
  nodeDef: NodeDef,
  resolvePropKeyId: (def: NodeDef, propName: string) => PropKeyID,
  getNodeDef: (nodeId: NodeID) => NodeDef | null,
): PathResult<N> {
  const path: NodeRef[] = [];
  const edges: EdgeResult[] = [];

  let currentId: NodeID | null = targetId;
  const pathStates: PathState[] = [];

  // Build path backwards from target to source
  while (currentId !== null) {
    const state = states.get(currentId);
    if (!state) break;

    pathStates.unshift(state); // Add to front

    if (currentId === sourceId) {
      // Reached source
      break;
    }

    currentId = state.parent;
    if (currentId === null) {
      // No parent means we didn't reach source - invalid path
      break;
    }
  }

  // Calculate total weight and build edges array
  let totalWeight = 0;
  for (let i = 1; i < pathStates.length; i++) {
    const state = pathStates[i]!;
    if (state.edge) {
      edges.push(state.edge);
      // Calculate edge weight as difference in costs
      const prevState = pathStates[i - 1]!;
      totalWeight += state.cost - prevState.cost;
    }
  }

  // Load node references
  for (const state of pathStates) {
    const nodeRef = loadNodeRef(
      db,
      state.nodeId,
      nodeDef,
      resolvePropKeyId,
      getNodeDef,
    );
    if (nodeRef) {
      path.push(nodeRef);
    }
  }

  const found =
    path.length > 0 &&
    path[0]?.$id === sourceId &&
    path[path.length - 1]?.$id === targetId;

  return {
    path: path as NodeRef<N>[],
    edges,
    totalWeight,
    found,
  };
}

// ============================================================================
// Dijkstra's Algorithm
// ============================================================================

/**
 * Execute Dijkstra's shortest path algorithm
 */
export async function dijkstra<N extends NodeDef>(
  db: GraphDB,
  config: PathFindingConfig,
  nodeDef: N,
  resolveEtypeId: (edgeDef: EdgeDef) => ETypeID,
  resolvePropKeyId: (def: NodeDef | EdgeDef, propName: string) => PropKeyID,
  getNodeDef: (nodeId: NodeID) => NodeDef | null,
  edgeDef: EdgeDef,
): Promise<PathResult<N>> {
  const sourceId = config.source.$id;
  const targetIds = new Set(config.targets.map((t) => t.$id));

  // Distance map: nodeId -> PathState
  const distances = new Map<NodeID, PathState>();
  const visited = new Set<NodeID>();

  // Priority queue: (nodeId, cost)
  const queue = new MinHeap<NodeID>();

  // Initialize source
  distances.set(sourceId, {
    nodeId: sourceId,
    cost: 0,
    depth: 0,
    parent: null,
    edge: null,
  });
  queue.insert(sourceId, 0);

  const etypeId = resolveEtypeId(edgeDef);
  const weightFn = createWeightFunction(
    db,
    config.weightSpec,
    edgeDef,
    resolvePropKeyId,
  );

  while (!queue.isEmpty()) {
    const currentId = queue.extractMin();
    if (!currentId) break;

    if (visited.has(currentId)) continue;
    visited.add(currentId);

    // Check if we reached a target
    if (targetIds.has(currentId)) {
      return reconstructPath(
        distances,
        currentId,
        sourceId,
        db,
        nodeDef,
        resolvePropKeyId,
        getNodeDef,
      );
    }

    const currentState = distances.get(currentId)!;
    if (currentState.depth >= config.maxDepth) {
      // Depth limit reached
      continue;
    }

    // Get neighbors
    const directions: ("out" | "in")[] =
      config.direction === "both" ? ["out", "in"] : [config.direction];

    for (const dir of directions) {
      const neighbors =
        dir === "out"
          ? getNeighborsOut(db, currentId, config.allowedEtypes.has(etypeId) ? etypeId : undefined)
          : getNeighborsIn(db, currentId, config.allowedEtypes.has(etypeId) ? etypeId : undefined);

      for (const edge of neighbors) {
        const neighborId = dir === "out" ? edge.dst : edge.src;

        // Optimization: removed redundant nodeExists check
        // getNeighborsOut/In already filters out deleted nodes in the merged view
        if (visited.has(neighborId)) {
          continue;
        }

        // Load edge properties for weight calculation
        const edgeResult = loadEdgeProperties(
          db,
          edge.src,
          edge.etype,
          edge.dst,
          edgeDef,
          resolvePropKeyId,
        );

        const weight = weightFn(edgeResult);
        const newCost = currentState.cost + weight;

        const existingState = distances.get(neighborId);
        if (!existingState || newCost < existingState.cost) {
          distances.set(neighborId, {
            nodeId: neighborId,
            cost: newCost,
            depth: currentState.depth + 1,
            parent: currentId,
            edge: edgeResult,
          });

          if (existingState) {
            queue.decreasePriority(neighborId, newCost);
          } else {
            queue.insert(neighborId, newCost);
          }
        }
      }
    }
  }

  // No path found
  return {
    path: [],
    edges: [],
    totalWeight: Infinity,
    found: false,
  };
}

// ============================================================================
// A* Algorithm
// ============================================================================

/**
 * A* node state - consolidated for better cache locality and fewer Map lookups
 */
interface AStarNodeState {
  gScore: number;      // Actual cost from source
  fScore: number;      // g(n) + h(n)
  depth: number;       // Hop count
  parent: NodeID | null;
  edge: EdgeResult | null;
}

/**
 * Execute A* shortest path algorithm with heuristic
 * 
 * Optimization: Uses a single consolidated state Map instead of four separate Maps
 * for better cache locality and fewer lookups.
 */
export async function aStar<N extends NodeDef>(
  db: GraphDB,
  config: PathFindingConfig,
  nodeDef: N,
  heuristic: Heuristic<N>,
  resolveEtypeId: (edgeDef: EdgeDef) => ETypeID,
  resolvePropKeyId: (def: NodeDef | EdgeDef, propName: string) => PropKeyID,
  getNodeDef: (nodeId: NodeID) => NodeDef | null,
  edgeDef: EdgeDef,
): Promise<PathResult<N>> {
  const sourceId = config.source.$id;
  const targetIds = new Set(config.targets.map((t) => t.$id));

  // Consolidated node state - single Map lookup instead of four
  const states = new Map<NodeID, AStarNodeState>();
  const visited = new Set<NodeID>();
  const queue = new MinHeap<NodeID>();

  // Initialize source
  const sourceHeuristic = heuristic(config.source as NodeRef<N>, config.targets[0] as NodeRef<N>);
  states.set(sourceId, {
    gScore: 0,
    fScore: sourceHeuristic,
    depth: 0,
    parent: null,
    edge: null,
  });
  queue.insert(sourceId, sourceHeuristic);

  const etypeId = resolveEtypeId(edgeDef);
  const weightFn = createWeightFunction(
    db,
    config.weightSpec,
    edgeDef,
    resolvePropKeyId,
  );

  while (!queue.isEmpty()) {
    const currentId = queue.extractMin();
    if (!currentId) break;

    if (visited.has(currentId)) continue;
    visited.add(currentId);

    // Check if we reached a target
    if (targetIds.has(currentId)) {
      // Reconstruct path using consolidated states
      const pathStates = new Map<NodeID, PathState>();
      for (const [nodeId, state] of states) {
        pathStates.set(nodeId, {
          nodeId,
          cost: state.gScore,
          depth: state.depth,
          parent: state.parent,
          edge: state.edge,
        });
      }
      return reconstructPath(
        pathStates,
        currentId,
        sourceId,
        db,
        nodeDef,
        resolvePropKeyId,
        getNodeDef,
      );
    }

    const currentState = states.get(currentId)!;
    if (currentState.depth >= config.maxDepth) {
      continue;
    }

    // Get neighbors
    const directions: ("out" | "in")[] =
      config.direction === "both" ? ["out", "in"] : [config.direction];

    for (const dir of directions) {
      const neighbors =
        dir === "out"
          ? getNeighborsOut(db, currentId, config.allowedEtypes.has(etypeId) ? etypeId : undefined)
          : getNeighborsIn(db, currentId, config.allowedEtypes.has(etypeId) ? etypeId : undefined);

      for (const edge of neighbors) {
        const neighborId = dir === "out" ? edge.dst : edge.src;

        // Optimization: removed redundant nodeExists check
        // getNeighborsOut/In already filters out deleted nodes in the merged view
        if (visited.has(neighborId)) {
          continue;
        }

        // Load edge properties for weight calculation
        const edgeResult = loadEdgeProperties(
          db,
          edge.src,
          edge.etype,
          edge.dst,
          edgeDef,
          resolvePropKeyId,
        );

        const weight = weightFn(edgeResult);
        const tentativeG = currentState.gScore + weight;

        const existingState = states.get(neighborId);
        if (!existingState || tentativeG < existingState.gScore) {
          // Calculate heuristic for this neighbor
          const neighborRef = loadNodeRef(
            db,
            neighborId,
            nodeDef,
            resolvePropKeyId,
            getNodeDef,
          );
          if (!neighborRef) continue;

          const h = heuristic(neighborRef as NodeRef<N>, config.targets[0] as NodeRef<N>);
          const f = tentativeG + h;

          // Single state update instead of four Map updates
          states.set(neighborId, {
            gScore: tentativeG,
            fScore: f,
            depth: currentState.depth + 1,
            parent: currentId,
            edge: edgeResult,
          });

          if (existingState) {
            queue.decreasePriority(neighborId, f);
          } else {
            queue.insert(neighborId, f);
          }
        }
      }
    }
  }

  // No path found
  return {
    path: [],
    edges: [],
    totalWeight: Infinity,
    found: false,
  };
}

// ============================================================================
// Builder Implementation
// ============================================================================

/**
 * Create a pathfinding builder
 */
export function createPathFindingBuilder<N extends NodeDef>(
  db: GraphDB,
  source: NodeRef<N>,
  weightSpec: WeightSpec<EdgeDef> | undefined,
  resolveEtypeId: (edgeDef: EdgeDef) => ETypeID,
  resolvePropKeyId: (def: NodeDef | EdgeDef, propName: string) => PropKeyID,
  getNodeDef: (nodeId: NodeID) => NodeDef | null,
  nodeDef: N,
): PathFindingBuilder<N> {
  const allowedEtypes = new Set<ETypeID>();
  let direction: TraversalDirection = "out";
  let maxDepth = 100;
  let edgeDef: EdgeDef | null = null;

  const builder: PathFindingBuilder<N> = {
    via<E extends EdgeDef>(edge: E) {
      const etypeId = resolveEtypeId(edge);
      allowedEtypes.add(etypeId);
      edgeDef = edge;
      return builder;
    },

    maxDepth(depth: number) {
      maxDepth = depth;
      return builder;
    },

    direction(dir: TraversalDirection) {
      direction = dir;
      return builder;
    },

    to(target: NodeRef<N>): PathExecutor<N> {
      return createPathExecutor([target]);
    },

    toAny(targets: NodeRef<N>[]): PathExecutor<N> {
      return createPathExecutor(targets);
    },
  };

  function createPathExecutor(targets: NodeRef<N>[]): PathExecutor<N> {
    if (!edgeDef) {
      throw new Error("Must specify at least one edge type with via()");
    }

    const config: PathFindingConfig = {
      source,
      targets,
      allowedEtypes,
      direction,
      maxDepth,
      weightSpec: weightSpec as WeightSpec<EdgeDef> | undefined,
    };

    return {
      async dijkstra() {
        return dijkstra(
          db,
          config,
          nodeDef,
          resolveEtypeId,
          resolvePropKeyId,
          getNodeDef,
          edgeDef!,
        );
      },

      async aStar(heuristic: Heuristic<N>) {
        return aStar(
          db,
          config,
          nodeDef,
          heuristic,
          resolveEtypeId,
          resolvePropKeyId,
          getNodeDef,
          edgeDef!,
        );
      },

      async* allPaths(_maxPaths?: number): AsyncGenerator<PathResult<N>, void, unknown> {
        // For now, just return the shortest path
        // Full implementation would require more complex algorithm
        const result = await dijkstra<N>(
          db,
          config,
          nodeDef,
          resolveEtypeId,
          resolvePropKeyId,
          getNodeDef,
          edgeDef!,
        );
        if (result.found) {
          yield result;
        }
      },
    };
  }

  return builder;
}

