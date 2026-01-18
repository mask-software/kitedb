import {
  getNodeDelta,
  isNodeCreated,
  isNodeDeleted,
} from "../../core/delta.ts";
import { getPhysNode, getNodeId, getNodeProp as snapshotGetNodeProp, getNodeProps as snapshotGetNodeProps } from "../../core/snapshot-reader.ts";
import type {
  GraphDB,
  NodeID,
  NodeOpts,
  PropKeyID,
  PropValue,
  TxHandle,
  LabelID,
} from "../../types.ts";
import { lookupByKey } from "../key-index.ts";
import { getCache } from "./cache-helper.ts";
import { getMvccManager, isMvccEnabled } from "../../mvcc/index.ts";
import { getVisibleVersion, nodeExists as mvccNodeExists } from "../../mvcc/visibility.ts";
import { getSnapshot } from "./snapshot-helper.ts";
import type { VectorManifest } from "../../vector/types.ts";

/**
 * Helper to detect if argument is a TxHandle (duck-typing)
 */
function isTxHandle(arg: GraphDB | TxHandle): arg is TxHandle {
  return '_tx' in arg && '_db' in arg;
}

/**
 * Create a new node
 */
export function createNode(handle: TxHandle, opts: NodeOpts = {}): NodeID {
  const { _db: db, _tx: tx } = handle;

  const nodeId = db._nextNodeId++;

  const nodeDelta = {
    key: opts.key,
    labels: new Set(opts.labels ?? []),
    labelsDeleted: new Set<LabelID>(),
    props: new Map<PropKeyID, PropValue | null>(),
  };

  tx.pendingCreatedNodes.set(nodeId, nodeDelta);

  if (opts.key) {
    tx.pendingKeyUpdates.set(opts.key, nodeId);
  }

  if (opts.props) {
    tx.pendingNodeProps.set(
      nodeId,
      new Map([...opts.props].map(([k, v]) => [k, v])),
    );
  }

  // Record write for MVCC conflict detection
  const mvcc = getMvccManager(db);
  if (mvcc) {
    mvcc.txManager.recordWrite(tx.txid, `node:${nodeId}`);
  }

  // Write-through cache invalidation (node creation affects cache)
  const cache = getCache(db);
  if (cache) {
    cache.invalidateNode(nodeId);
  }

  return nodeId;
}

/**
 * Delete a node
 */
export function deleteNode(handle: TxHandle, nodeId: NodeID): boolean {
  const { _db: db, _tx: tx } = handle;

  // Check if it was created in this transaction
  if (tx.pendingCreatedNodes.has(nodeId)) {
    const nodeDelta = tx.pendingCreatedNodes.get(nodeId)!;
    if (nodeDelta.key) {
      tx.pendingKeyUpdates.delete(nodeDelta.key);
    }
    tx.pendingCreatedNodes.delete(nodeId);
    tx.pendingNodeProps.delete(nodeId);
    tx.pendingOutAdd.delete(nodeId);
    tx.pendingOutDel.delete(nodeId);
    tx.pendingInAdd.delete(nodeId);
    tx.pendingInDel.delete(nodeId);
    
    // Also clean up any pending vector sets for this node
    for (const key of tx.pendingVectorSets.keys()) {
      if (key.startsWith(`${nodeId}:`)) {
        tx.pendingVectorSets.delete(key);
      }
    }

    // Write-through cache invalidation
    const cache = getCache(db);
    if (cache) {
      cache.invalidateNode(nodeId);
    }
    return true;
  }

  // Check if node exists
  const snapshot = getSnapshot(db);
  const existsInSnapshot =
    snapshot && getPhysNode(snapshot, nodeId) >= 0;
  const existsInDelta = isNodeCreated(db._delta, nodeId);

  if (!existsInSnapshot && !existsInDelta) {
    return false;
  }

  if (isNodeDeleted(db._delta, nodeId)) {
    return false;
  }

  tx.pendingDeletedNodes.add(nodeId);
  
  // Cascade delete: mark all vectors for this node as deleted
  if (db._vectorStores) {
    for (const [propKeyId, store] of db._vectorStores as Map<PropKeyID, VectorManifest>) {
      if (store.nodeIdToVectorId.has(nodeId)) {
        tx.pendingVectorDeletes.add(`${nodeId}:${propKeyId}`);
      }
    }
  }

  // Record write for MVCC conflict detection
  const mvcc = getMvccManager(db);
  if (mvcc) {
    mvcc.txManager.recordWrite(tx.txid, `node:${nodeId}`);
  }

  // Write-through cache invalidation
  const cache = getCache(db);
  if (cache) {
    cache.invalidateNode(nodeId);
  }

  return true;
}

/**
 * Get a node by key
 * Accepts GraphDB for auto-commit reads or TxHandle for transactional reads
 */
export function getNodeByKey(handle: GraphDB | TxHandle, key: string): NodeID | null {
  const db = isTxHandle(handle) ? handle._db : handle;
  const tx = isTxHandle(handle) ? handle._tx : null;
  
  // Check current transaction first (pending changes)
  if (tx) {
    const pending = tx.pendingKeyUpdates.get(key);
    if (pending !== undefined) return pending;
    if (tx.pendingKeyDeletes.has(key)) return null;
  }

  // MVCC mode: record read for conflict detection
  if (tx && isMvccEnabled(db)) {
    const mvcc = getMvccManager(db);
    if (mvcc) {
      mvcc.txManager.recordRead(tx.txid, `key:${key}`);
    }
  }

  return lookupByKey(getSnapshot(db), db._delta, key);
}

/**
 * Check if a node exists
 * Accepts GraphDB for auto-commit reads or TxHandle for transactional reads
 */
export function nodeExists(handle: GraphDB | TxHandle, nodeId: NodeID): boolean {
  const db = isTxHandle(handle) ? handle._db : handle;
  const tx = isTxHandle(handle) ? handle._tx : null;
  
  // Check pending transaction changes first
  if (tx) {
    if (tx.pendingCreatedNodes.has(nodeId)) return true;
    if (tx.pendingDeletedNodes.has(nodeId)) return false;
  }
  
  // MVCC mode: use version chains for visibility
  if (isMvccEnabled(db)) {
    const mvcc = getMvccManager(db);
    if (mvcc) {
      // Get transaction snapshot timestamp
      let txSnapshotTs = mvcc.txManager.getNextCommitTs();
      let txid = 0n;
      
      // If we have a TxHandle, use its transaction info
      if (tx) {
        const mvccTx = mvcc.txManager.getTx(tx.txid);
        if (mvccTx) {
          txSnapshotTs = mvccTx.startTs;
          txid = mvccTx.txid;
          // Track read for conflict detection
          mvcc.txManager.recordRead(txid, `node:${nodeId}`);
        }
      }
      
      // Check version chain first
      const nodeVersion = mvcc.versionChain.getNodeVersion(nodeId);
      if (nodeVersion) {
        // Version chain exists - use MVCC visibility
        return mvccNodeExists(nodeVersion, txSnapshotTs, txid);
      }
      
      // No version chain - fall back to delta/snapshot
      // This handles entities created without version chains (single-tx optimization)
      if (isNodeDeleted(db._delta, nodeId)) {
        return false;
      }
      if (isNodeCreated(db._delta, nodeId)) {
        return true;
      }
      const snapshot = getSnapshot(db);
      if (snapshot) {
        return getPhysNode(snapshot, nodeId) >= 0;
      }
      
      return false;
    }
  }
  
  // Non-MVCC mode: check delta and snapshot directly

  // Check delta
  if (isNodeDeleted(db._delta, nodeId)) return false;
  if (isNodeCreated(db._delta, nodeId)) return true;

  // Check snapshot
  const snapshot = getSnapshot(db);
  if (snapshot) {
    return getPhysNode(snapshot, nodeId) >= 0;
  }

  return false;
}

/**
 * Set a node property
 */
export function setNodeProp(
  handle: TxHandle,
  nodeId: NodeID,
  keyId: PropKeyID,
  value: PropValue,
): void {
  const { _db: db, _tx: tx } = handle;

  let props = tx.pendingNodeProps.get(nodeId);
  if (!props) {
    props = new Map();
    tx.pendingNodeProps.set(nodeId, props);
  }
  props.set(keyId, value);

  // Record write for MVCC conflict detection
  const mvcc = getMvccManager(db);
  if (mvcc && isMvccEnabled(db)) {
    mvcc.txManager.recordWrite(tx.txid, `nodeprop:${nodeId}:${keyId}`);
  }

  // Write-through cache invalidation (immediate)
  const cache = getCache(db);
  if (cache) {
    cache.invalidateNode(nodeId);
  }
}

/**
 * Delete a node property
 */
export function delNodeProp(
  handle: TxHandle,
  nodeId: NodeID,
  keyId: PropKeyID,
): void {
  const { _db: db, _tx: tx } = handle;

  let props = tx.pendingNodeProps.get(nodeId);
  if (!props) {
    props = new Map();
    tx.pendingNodeProps.set(nodeId, props);
  }
  props.set(keyId, null);

  // Record write for MVCC conflict detection
  const mvcc = getMvccManager(db);
  if (mvcc && isMvccEnabled(db)) {
    mvcc.txManager.recordWrite(tx.txid, `nodeprop:${nodeId}:${keyId}`);
  }

  // Write-through cache invalidation (immediate)
  const cache = getCache(db);
  if (cache) {
    cache.invalidateNode(nodeId);
  }
}

/**
 * Get a specific property for a node
 * Returns null if the node doesn't exist, is deleted, or the property is not set
 * Accepts GraphDB for auto-commit reads or TxHandle for transactional reads
 */
export function getNodeProp(
  handle: GraphDB | TxHandle,
  nodeId: NodeID,
  keyId: PropKeyID,
): PropValue | null {
  const db = isTxHandle(handle) ? handle._db : handle;
  const tx = isTxHandle(handle) ? handle._tx : null;
  const cache = getCache(db);
  
  // Check pending transaction changes first
  if (tx) {
    const pendingProps = tx.pendingNodeProps.get(nodeId);
    if (pendingProps) {
      const pendingValue = pendingProps.get(keyId);
      if (pendingValue !== undefined) {
        return pendingValue; // null means deleted
      }
    }
  }
  
  // MVCC mode: use version chains for visibility
  if (isMvccEnabled(db)) {
    const mvcc = getMvccManager(db);
    if (mvcc) {
      // Get transaction snapshot timestamp
      let txSnapshotTs = mvcc.txManager.getNextCommitTs();
      let txid = 0n;
      
      // If we have a TxHandle, use its transaction info
      if (tx) {
        const mvccTx = mvcc.txManager.getTx(tx.txid);
        if (mvccTx) {
          txSnapshotTs = mvccTx.startTs;
          txid = mvccTx.txid;
          // Track read for conflict detection
          mvcc.txManager.recordRead(txid, `nodeprop:${nodeId}:${keyId}`);
        }
      }
      
      // Note: We don't use cache in MVCC mode when we have a specific transaction
      // because the cache stores the latest committed value, not snapshot-specific values
      // We only use cache for reads outside transactions
      if (!tx && cache) {
        const cached = cache.getNodeProp(nodeId, keyId);
        if (cached !== undefined) {
          return cached;
        }
      }
      
      // Get visible version from version chain
      const propVersion = mvcc.versionChain.getNodePropVersion(nodeId, keyId);
      const visibleVersion = getVisibleVersion(propVersion, txSnapshotTs, txid);
      
      if (visibleVersion) {
        const value = visibleVersion.data;
        // Only cache if reading outside a transaction (latest committed value)
        if (!tx && cache) {
          cache.setNodeProp(nodeId, keyId, value);
        }
        return value;
      }
      
      // Fall back to snapshot/delta
      // This handles entities created without version chains (optimization for single-tx commits)
      // or entities that existed before MVCC was enabled
      
      // Check if node exists via version chain or delta/snapshot
      const nodeVersion = mvcc.versionChain.getNodeVersion(nodeId);
      let nodeKnownToExist = false;
      
      const snapshot = getSnapshot(db);
      if (nodeVersion) {
        // Version chain exists - use MVCC visibility
        nodeKnownToExist = mvccNodeExists(nodeVersion, txSnapshotTs, txid);
      } else {
        // No version chain - check delta/snapshot directly
        // This is safe because no version chain means no concurrent modifications were tracked
        if (isNodeDeleted(db._delta, nodeId)) {
          nodeKnownToExist = false;
        } else if (isNodeCreated(db._delta, nodeId)) {
          nodeKnownToExist = true;
        } else if (snapshot) {
          nodeKnownToExist = getPhysNode(snapshot, nodeId) >= 0;
        }
      }
      
      if (!nodeKnownToExist) {
        if (!tx && cache) {
          cache.setNodeProp(nodeId, keyId, null);
        }
        return null;
      }
      
      // Check delta first (modifications take precedence)
      const nodeDelta = getNodeDelta(db._delta, nodeId);
      if (nodeDelta?.props) {
        const deltaValue = nodeDelta.props.get(keyId);
        if (deltaValue !== undefined) {
          if (!tx && cache) {
            cache.setNodeProp(nodeId, keyId, deltaValue);
          }
          return deltaValue;
        }
      }
      
      // Fall back to snapshot
      if (snapshot) {
        const phys = getPhysNode(snapshot, nodeId);
        if (phys >= 0) {
          const value = snapshotGetNodeProp(snapshot, phys, keyId);
          if (!tx && cache) {
            cache.setNodeProp(nodeId, keyId, value);
          }
          return value;
        }
      }
      
      if (!tx && cache) {
        cache.setNodeProp(nodeId, keyId, null);
      }
      return null;
    }
  }
  
  // Non-MVCC mode: original logic
  if (cache) {
    const cached = cache.getNodeProp(nodeId, keyId);
    if (cached !== undefined) {
      return cached;
    }
  }

  // Check if node is deleted
  if (isNodeDeleted(db._delta, nodeId)) {
    if (cache) {
      cache.setNodeProp(nodeId, keyId, null);
    }
    return null;
  }

  // Check delta first (modifications take precedence)
  const nodeDelta = getNodeDelta(db._delta, nodeId);
  if (nodeDelta?.props) {
    const deltaValue = nodeDelta.props.get(keyId);
    if (deltaValue !== undefined) {
      if (cache) {
        cache.setNodeProp(nodeId, keyId, deltaValue);
      }
      return deltaValue; // null means deleted
    }
  }

  // Fall back to snapshot
  const snapshot = getSnapshot(db);
  if (snapshot) {
    const phys = getPhysNode(snapshot, nodeId);
    if (phys >= 0) {
      const value = snapshotGetNodeProp(snapshot, phys, keyId);
      if (cache) {
        cache.setNodeProp(nodeId, keyId, value);
      }
      return value;
    }
  }

  // Property not found
  if (cache) {
    cache.setNodeProp(nodeId, keyId, null);
  }
  return null;
}

/**
 * Get all properties for a node
 * Returns null if the node doesn't exist or is deleted
 * Accepts GraphDB for auto-commit reads or TxHandle for transactional reads
 */
export function getNodeProps(
  handle: GraphDB | TxHandle,
  nodeId: NodeID,
): Map<PropKeyID, PropValue> | null {
  const db = isTxHandle(handle) ? handle._db : handle;
  const tx = isTxHandle(handle) ? handle._tx : null;
  
  // Check if node is deleted
  if (isNodeDeleted(db._delta, nodeId)) {
    return null;
  }
  
  // Check pending transaction deletions
  if (tx && tx.pendingDeletedNodes.has(nodeId)) {
    return null;
  }

  const props = new Map<PropKeyID, PropValue>();
  const snapshot = getSnapshot(db);

  // Get from snapshot first
  if (snapshot) {
    const phys = getPhysNode(snapshot, nodeId);
    if (phys >= 0) {
      const snapshotProps = snapshotGetNodeProps(snapshot, phys);
      if (snapshotProps) {
        for (const [keyId, value] of snapshotProps) {
          props.set(keyId, value);
        }
      }
    }
  }

  // Apply delta modifications
  const nodeDelta = getNodeDelta(db._delta, nodeId);
  if (nodeDelta?.props) {
    for (const [keyId, value] of nodeDelta.props) {
      if (value === null) {
        props.delete(keyId);
      } else {
        props.set(keyId, value);
      }
    }
  }
  
  // Apply pending transaction modifications
  if (tx) {
    const pendingProps = tx.pendingNodeProps.get(nodeId);
    if (pendingProps) {
      for (const [keyId, value] of pendingProps) {
        if (value === null) {
          props.delete(keyId);
        } else {
          props.set(keyId, value);
        }
      }
    }
  }

  // Check if node exists at all
  const nodeExistsInPending = tx?.pendingCreatedNodes.has(nodeId);
  if (!nodeDelta && !nodeExistsInPending && snapshot) {
    const phys = getPhysNode(snapshot, nodeId);
    if (phys < 0) {
      return null;
    }
  } else if (!nodeDelta && !nodeExistsInPending && !snapshot) {
    return null;
  }

  return props;
}

// ============================================================================
// Node Listing and Counting
// ============================================================================

/**
 * List all nodes in the database
 * 
 * This is a generator that yields node IDs lazily for memory efficiency.
 * It merges snapshot nodes with delta changes (created/deleted nodes).
 * 
 * Accepts GraphDB for auto-commit reads or TxHandle for transactional reads.
 * 
 * @example
 * ```ts
 * // Iterate all nodes
 * for (const nodeId of listNodes(db)) {
 *   console.log(nodeId);
 * }
 * 
 * // Collect to array (be careful with large datasets)
 * const allNodes = [...listNodes(db)];
 * ```
 */
export function* listNodes(
  handle: GraphDB | TxHandle,
): Generator<NodeID> {
  const db = isTxHandle(handle) ? handle._db : handle;
  const tx = isTxHandle(handle) ? handle._tx : null;
  const snapshot = getSnapshot(db);
  const delta = db._delta;
  
  // Track nodes we've already yielded from snapshot to avoid duplicates
  // when we process delta created nodes
  const yieldedFromSnapshot = new Set<NodeID>();
  
  // 1. Iterate snapshot nodes (if snapshot exists)
  if (snapshot) {
    const numNodes = Number(snapshot.header.numNodes);
    
    for (let phys = 0; phys < numNodes; phys++) {
      const nodeId = getNodeId(snapshot, phys);
      
      // Skip if deleted in delta
      if (delta.deletedNodes.has(nodeId)) {
        continue;
      }
      
      // Skip if deleted in pending transaction
      if (tx?.pendingDeletedNodes.has(nodeId)) {
        continue;
      }
      
      yieldedFromSnapshot.add(nodeId);
      yield nodeId;
    }
  }
  
  // 2. Yield nodes created in delta (not in snapshot)
  for (const nodeId of delta.createdNodes.keys()) {
    // Skip if already yielded from snapshot (shouldn't happen, but defensive)
    if (yieldedFromSnapshot.has(nodeId)) {
      continue;
    }
    
    // Skip if deleted in delta (created then deleted = net nothing)
    if (delta.deletedNodes.has(nodeId)) {
      continue;
    }
    
    // Skip if deleted in pending transaction
    if (tx?.pendingDeletedNodes.has(nodeId)) {
      continue;
    }
    
    yield nodeId;
  }
  
  // 3. Yield nodes created in pending transaction
  if (tx) {
    for (const nodeId of tx.pendingCreatedNodes.keys()) {
      // Skip if already yielded (shouldn't happen for pending, but defensive)
      if (yieldedFromSnapshot.has(nodeId)) {
        continue;
      }
      
      // Skip if already in delta created nodes (shouldn't happen)
      if (delta.createdNodes.has(nodeId)) {
        continue;
      }
      
      yield nodeId;
    }
  }
}

/**
 * Count total nodes in the database
 * 
 * This is optimized to avoid full iteration when possible by using
 * snapshot metadata and delta size adjustments.
 * 
 * Accepts GraphDB for auto-commit reads or TxHandle for transactional reads.
 * 
 * @example
 * ```ts
 * const total = countNodes(db);
 * console.log(`Database has ${total} nodes`);
 * ```
 */
export function countNodes(handle: GraphDB | TxHandle): number {
  const db = isTxHandle(handle) ? handle._db : handle;
  const tx = isTxHandle(handle) ? handle._tx : null;
  const snapshot = getSnapshot(db);
  const delta = db._delta;
  
  // Start with snapshot count
  let count = snapshot ? Number(snapshot.header.numNodes) : 0;
  
  // Subtract snapshot nodes that were deleted in delta
  // (We need to check if deleted nodes were actually in snapshot)
  for (const nodeId of delta.deletedNodes) {
    // Only subtract if it was a snapshot node (not a delta-created node)
    if (!delta.createdNodes.has(nodeId)) {
      // Check if it existed in snapshot
      if (snapshot && getPhysNode(snapshot, nodeId) >= 0) {
        count--;
      }
    }
  }
  
  // Add delta created nodes (that weren't deleted)
  for (const nodeId of delta.createdNodes.keys()) {
    if (!delta.deletedNodes.has(nodeId)) {
      count++;
    }
  }
  
  // Handle pending transaction changes
  if (tx) {
    // Subtract pending deletions (that weren't already counted)
    for (const nodeId of tx.pendingDeletedNodes) {
      // Check if it exists (in snapshot or delta created)
      const inSnapshot = snapshot && getPhysNode(snapshot, nodeId) >= 0;
      const inDeltaCreated = delta.createdNodes.has(nodeId);
      const deletedInDelta = delta.deletedNodes.has(nodeId);
      
      if ((inSnapshot || inDeltaCreated) && !deletedInDelta) {
        count--;
      }
    }
    
    // Add pending creations
    count += tx.pendingCreatedNodes.size;
  }
  
  return count;
}

