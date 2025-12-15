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
 */
export function getNodeByKey(db: GraphDB, key: string): NodeID | null {
  // Check current transaction first
  if (db._currentTx) {
    const pending = db._currentTx.pendingKeyUpdates.get(key);
    if (pending !== undefined) return pending;
    if (db._currentTx.pendingKeyDeletes.has(key)) return null;
  }

  return lookupByKey(db._snapshot, db._delta, key);
}

/**
 * Check if a node exists
 */
export function nodeExists(db: GraphDB, nodeId: NodeID): boolean {
  // MVCC mode: use version chains for visibility
  if (db._mvccEnabled) {
    const mvcc = getMvccManager(db);
    if (mvcc) {
      // Get current transaction snapshot
      let txSnapshotTs = mvcc.txManager.getNextCommitTs();
      let txid = 0n;
      
      if (db._currentTx) {
        const mvccTx = mvcc.txManager.getTx(db._currentTx.txid);
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
  
  // Non-MVCC mode: original logic
  // Check current transaction
  if (db._currentTx) {
    if (db._currentTx.pendingCreatedNodes.has(nodeId)) return true;
    if (db._currentTx.pendingDeletedNodes.has(nodeId)) return false;
  }

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
 */
export function getNodeProp(
  db: GraphDB,
  nodeId: NodeID,
  keyId: PropKeyID,
): PropValue | null {
  const cache = getCache(db);
  
  // MVCC mode: use version chains for visibility
  if (db._mvccEnabled) {
    const mvcc = getMvccManager(db);
    if (mvcc) {
      // Get current transaction snapshot (if in a transaction)
      // For reads outside transactions, use latest committed state
      let txSnapshotTs = mvcc.txManager.getNextCommitTs();
      let txid = 0n;
      
      // Check if we're in a transaction
      if (db._currentTx) {
        const mvccTx = mvcc.txManager.getTx(db._currentTx.txid);
        if (mvccTx) {
          txSnapshotTs = mvccTx.startTs;
          txid = mvccTx.txid;
          // Track read for conflict detection
          mvcc.txManager.recordRead(txid, `nodeprop:${nodeId}:${keyId}`);
        }
      }
      
      // Check cache first
      if (cache) {
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
        if (cache) {
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
          return deltaValue;
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
      
      if (cache) {
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
 */
export function getNodeProps(
  db: GraphDB,
  nodeId: NodeID,
): Map<PropKeyID, PropValue> | null {
  // Check if node is deleted
  if (isNodeDeleted(db._delta, nodeId)) {
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

  // Check if node exists at all
  if (!nodeDelta && db._snapshot) {
    const phys = getPhysNode(db._snapshot, nodeId);
    if (phys < 0) {
      return null;
    }
  } else if (!nodeDelta && !db._snapshot) {
    return null;
  }

  return props;
}

