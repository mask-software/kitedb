/**
 * Main GraphDB handle with transaction logic
 * 
 * This module re-exports all public APIs from the graph-db submodules
 * to maintain backward compatibility with existing imports.
 */

// Lifecycle
export { openGraphDB, closeGraphDB } from "./lifecycle.ts";

// Transactions
export { beginTx, commit, rollback } from "./tx.ts";

// Node operations
export {
  createNode,
  deleteNode,
  getNodeByKey,
  nodeExists,
  setNodeProp,
  delNodeProp,
  getNodeProp,
  getNodeProps,
  listNodes,
  countNodes,
} from "./nodes.ts";

// Edge operations
export {
  addEdge,
  deleteEdge,
  getNeighborsOut,
  getNeighborsIn,
  edgeExists,
  setEdgeProp,
  delEdgeProp,
  getEdgeProp,
  getEdgeProps,
  listEdges,
  countEdges,
} from "./edges.ts";

// Schema definitions
export { defineLabel, defineEtype, definePropkey } from "./definitions.ts";

// Cache API
export {
  invalidateNodeCache,
  invalidateEdgeCache,
  clearCache,
  getCacheStats,
} from "./cache-api.ts";

// Stats and maintenance
export { stats, check } from "./stats.ts";

// Vector operations
export {
  setNodeVector,
  getNodeVector,
  delNodeVector,
  hasNodeVector,
  getVectorStore,
  getVectorStats,
} from "./vectors.ts";

