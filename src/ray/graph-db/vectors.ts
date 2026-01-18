/**
 * Vector embeddings operations for GraphDB
 * 
 * These functions manage vector embeddings associated with nodes.
 * Vectors are persisted to WAL and replayed on recovery.
 */

import type {
  GraphDB,
  NodeID,
  PropKeyID,
  TxHandle,
} from "../../types.ts";
import type { VectorManifest } from "../../vector/types.ts";
import { vectorStoreGet, vectorStoreStats } from "../../vector/columnar-store.ts";
import { getOrCreateVectorStore } from "./wal-replay.ts";

/** Helper to create vector pending key */
function vectorKey(nodeId: NodeID, propKeyId: PropKeyID): string {
  return `${nodeId}:${propKeyId}`;
}

/**
 * Set a vector embedding for a node
 * 
 * @param handle - Transaction handle
 * @param nodeId - The node to attach the vector to
 * @param propKeyId - The property key ID for this vector property
 * @param vector - The vector embedding (Float32Array)
 */
export function setNodeVector(
  handle: TxHandle,
  nodeId: NodeID,
  propKeyId: PropKeyID,
  vector: Float32Array
): void {
  const { _db: db, _tx: tx } = handle;
  
  // Validate vector dimensions if store already exists
  const existingStore = db._vectorStores?.get(propKeyId) as VectorManifest | undefined;
  if (existingStore && existingStore.config.dimensions !== vector.length) {
    throw new Error(
      `Vector dimension mismatch for propKey ${propKeyId}: ` +
      `expected ${existingStore.config.dimensions}, got ${vector.length}`
    );
  }
  
  const key = vectorKey(nodeId, propKeyId);
  
  // Queue for commit
  tx.pendingVectorSets.set(key, { nodeId, propKeyId, vector });
  
  // Remove from delete queue if present
  tx.pendingVectorDeletes.delete(key);
}

/**
 * Get a vector embedding for a node
 * 
 * @param db - GraphDB or TxHandle
 * @param nodeId - The node ID
 * @param propKeyId - The property key ID for this vector property
 * @returns The vector embedding or null if not found
 */
export function getNodeVector(
  db: GraphDB | TxHandle,
  nodeId: NodeID,
  propKeyId: PropKeyID
): Float32Array | null {
  const actualDb = '_db' in db ? db._db : db;
  const tx = '_tx' in db ? db._tx : null;
  const key = vectorKey(nodeId, propKeyId);
  
  // Check pending operations first (for uncommitted reads)
  if (tx) {
    // Check if vector was set in this transaction
    const pendingSet = tx.pendingVectorSets.get(key);
    if (pendingSet) {
      return pendingSet.vector;
    }
    
    // Check if vector was deleted in this transaction
    if (tx.pendingVectorDeletes.has(key)) {
      return null;
    }
  }
  
  // Get from committed storage
  const store = actualDb._vectorStores?.get(propKeyId) as VectorManifest | undefined;
  if (!store) {
    return null;
  }
  
  return vectorStoreGet(store, nodeId);
}

/**
 * Delete a vector embedding for a node
 * 
 * @param handle - Transaction handle
 * @param nodeId - The node ID
 * @param propKeyId - The property key ID for this vector property
 * @returns true if vector existed and was deleted
 */
export function delNodeVector(
  handle: TxHandle,
  nodeId: NodeID,
  propKeyId: PropKeyID
): boolean {
  const { _db: db, _tx: tx } = handle;
  const key = vectorKey(nodeId, propKeyId);
  
  // Check if vector was set in this transaction
  const pendingSet = tx.pendingVectorSets.get(key);
  if (pendingSet) {
    tx.pendingVectorSets.delete(key);
    return true;
  }
  
  // Check if it exists in committed storage
  const store = db._vectorStores?.get(propKeyId) as VectorManifest | undefined;
  if (!store) {
    return false;
  }
  
  const exists = store.nodeIdToVectorId.has(nodeId);
  if (exists) {
    tx.pendingVectorDeletes.add(key);
  }
  
  return exists;
}

/**
 * Check if a node has a vector embedding
 * 
 * @param db - GraphDB or TxHandle
 * @param nodeId - The node ID
 * @param propKeyId - The property key ID for this vector property
 * @returns true if node has a vector for this property
 */
export function hasNodeVector(
  db: GraphDB | TxHandle,
  nodeId: NodeID,
  propKeyId: PropKeyID
): boolean {
  const actualDb = '_db' in db ? db._db : db;
  const tx = '_tx' in db ? db._tx : null;
  const key = vectorKey(nodeId, propKeyId);
  
  // Check pending operations first
  if (tx) {
    // Check if vector was set in this transaction
    if (tx.pendingVectorSets.has(key)) {
      return true;
    }
    
    // Check if vector was deleted in this transaction
    if (tx.pendingVectorDeletes.has(key)) {
      return false;
    }
  }
  
  // Check committed storage
  const store = actualDb._vectorStores?.get(propKeyId) as VectorManifest | undefined;
  if (!store) {
    return false;
  }
  
  return store.nodeIdToVectorId.has(nodeId);
}

/**
 * Get the vector store for a property key (creating it if needed)
 * 
 * @param db - GraphDB
 * @param propKeyId - The property key ID
 * @param dimensions - Vector dimensions (required if creating new store)
 * @returns The VectorManifest for this property
 */
export function getVectorStore(
  db: GraphDB,
  propKeyId: PropKeyID,
  dimensions?: number
): VectorManifest | null {
  const store = db._vectorStores?.get(propKeyId) as VectorManifest | undefined;
  if (store) {
    return store;
  }
  
  if (dimensions !== undefined) {
    return getOrCreateVectorStore(db, propKeyId, dimensions);
  }
  
  return null;
}

/**
 * Get vector store statistics for a property
 * 
 * @param db - GraphDB
 * @param propKeyId - The property key ID
 */
export function getVectorStats(
  db: GraphDB,
  propKeyId: PropKeyID
): { totalVectors: number; liveVectors: number; dimensions: number } | null {
  const store = db._vectorStores?.get(propKeyId) as VectorManifest | undefined;
  if (!store) {
    return null;
  }
  
  const stats = vectorStoreStats(store);
  
  return {
    totalVectors: stats.totalVectors,
    liveVectors: stats.liveVectors,
    dimensions: stats.dimensions,
  };
}
