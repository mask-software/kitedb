import {
  getNodeDelta,
  isNodeCreated,
  isNodeDeleted,
} from "../../core/delta.ts";
import { getPhysNode, getNodeProp as snapshotGetNodeProp, getNodeProps as snapshotGetNodeProps } from "../../core/snapshot-reader.ts";
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

    // Write-through cache invalidation
    const cache = getCache(db);
    if (cache) {
      cache.invalidateNode(nodeId);
    }
    return true;
  }

  // Check if node exists
  const existsInSnapshot =
    db._snapshot && getPhysNode(db._snapshot, nodeId) >= 0;
  const existsInDelta = isNodeCreated(db._delta, nodeId);

  if (!existsInSnapshot && !existsInDelta) {
    return false;
  }

  if (isNodeDeleted(db._delta, nodeId)) {
    return false;
  }

  tx.pendingDeletedNodes.add(nodeId);

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

  return lookupByKey(db._snapshot, db._delta, key);
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
      if (db._snapshot) {
        return getPhysNode(db._snapshot, nodeId) >= 0;
      }
      
      return false;
    }
  }
  
  // Non-MVCC mode: check delta and snapshot directly

  // Check delta
  if (isNodeDeleted(db._delta, nodeId)) return false;
  if (isNodeCreated(db._delta, nodeId)) return true;

  // Check snapshot
  if (db._snapshot) {
    return getPhysNode(db._snapshot, nodeId) >= 0;
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
        } else if (db._snapshot) {
          nodeKnownToExist = getPhysNode(db._snapshot, nodeId) >= 0;
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
      if (nodeDelta) {
        const deltaValue = nodeDelta.props.get(keyId);
        if (deltaValue !== undefined) {
          if (!tx && cache) {
            cache.setNodeProp(nodeId, keyId, deltaValue);
          }
          return deltaValue;
        }
      }
      
      // Fall back to snapshot
      if (db._snapshot) {
        const phys = getPhysNode(db._snapshot, nodeId);
        if (phys >= 0) {
          const value = snapshotGetNodeProp(db._snapshot, phys, keyId);
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
  if (nodeDelta) {
    const deltaValue = nodeDelta.props.get(keyId);
    if (deltaValue !== undefined) {
      if (cache) {
        cache.setNodeProp(nodeId, keyId, deltaValue);
      }
      return deltaValue; // null means deleted
    }
  }

  // Fall back to snapshot
  if (db._snapshot) {
    const phys = getPhysNode(db._snapshot, nodeId);
    if (phys >= 0) {
      const value = snapshotGetNodeProp(db._snapshot, phys, keyId);
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

  // Get from snapshot first
  if (db._snapshot) {
    const phys = getPhysNode(db._snapshot, nodeId);
    if (phys >= 0) {
      const snapshotProps = snapshotGetNodeProps(db._snapshot, phys);
      if (snapshotProps) {
        for (const [keyId, value] of snapshotProps) {
          props.set(keyId, value);
        }
      }
    }
  }

  // Apply delta modifications
  const nodeDelta = getNodeDelta(db._delta, nodeId);
  if (nodeDelta) {
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
  if (!nodeDelta && !nodeExistsInPending && db._snapshot) {
    const phys = getPhysNode(db._snapshot, nodeId);
    if (phys < 0) {
      return null;
    }
  } else if (!nodeDelta && !nodeExistsInPending && !db._snapshot) {
    return null;
  }

  return props;
}

