import { edgePropKey, isNodeDeleted } from "../../core/delta.ts";
import { findEdgeIndex, getPhysNode, getEdgeProp as snapshotGetEdgeProp, getEdgeProps as snapshotGetEdgeProps } from "../../core/snapshot-reader.ts";
import type {
  GraphDB,
  NodeID,
  ETypeID,
  PropKeyID,
  PropValue,
  TxHandle,
  Edge,
} from "../../types.ts";
import { hasEdgeMerged, neighborsIn, neighborsOut } from "../iterators.ts";
import { getCache } from "./cache-helper.ts";
import { getMvccManager, isMvccEnabled } from "../../mvcc/index.ts";
import { getVisibleVersion, edgeExists as mvccEdgeExists } from "../../mvcc/visibility.ts";

/**
 * Helper to detect if argument is a TxHandle (duck-typing)
 */
function isTxHandle(arg: GraphDB | TxHandle): arg is TxHandle {
  return '_tx' in arg && '_db' in arg;
}

/**
 * Add an edge
 */
export function addEdge(
  handle: TxHandle,
  src: NodeID,
  etype: ETypeID,
  dst: NodeID,
): void {
  const { _db: db, _tx: tx } = handle;

  // Add to pending out-edges
  let outPatches = tx.pendingOutAdd.get(src);
  if (!outPatches) {
    outPatches = [];
    tx.pendingOutAdd.set(src, outPatches);
  }
  outPatches.push({ etype, other: dst });

  // Add to pending in-edges
  let inPatches = tx.pendingInAdd.get(dst);
  if (!inPatches) {
    inPatches = [];
    tx.pendingInAdd.set(dst, inPatches);
  }
  inPatches.push({ etype, other: src });

  // Record write for MVCC conflict detection
  const mvcc = getMvccManager(db);
  if (mvcc) {
    mvcc.txManager.recordWrite(tx.txid, `edge:${src}:${etype}:${dst}`);
  }

  // Write-through cache invalidation (immediate)
  const cache = getCache(db);
  if (cache) {
    cache.invalidateEdge(src, etype, dst);
    // Also invalidate traversal caches for both nodes
    cache.invalidateNode(src);
    cache.invalidateNode(dst);
  }
}

/**
 * Delete an edge
 */
export function deleteEdge(
  handle: TxHandle,
  src: NodeID,
  etype: ETypeID,
  dst: NodeID,
): boolean {
  const { _db: db, _tx: tx } = handle;

  // Check if edge exists
  if (!hasEdgeMerged(db._snapshot, db._delta, src, etype, dst)) {
    // Check pending adds
    const outPatches = tx.pendingOutAdd.get(src);
    if (outPatches) {
      const idx = outPatches.findIndex(
        (p) => p.etype === etype && p.other === dst,
      );
      if (idx >= 0) {
        outPatches.splice(idx, 1);

        // Remove from in-adds too
        const inPatches = tx.pendingInAdd.get(dst);
        if (inPatches) {
          const inIdx = inPatches.findIndex(
            (p) => p.etype === etype && p.other === src,
          );
          if (inIdx >= 0) inPatches.splice(inIdx, 1);
        }

        // Record write for MVCC conflict detection
        const mvcc = getMvccManager(db);
        if (mvcc) {
          mvcc.txManager.recordWrite(tx.txid, `edge:${src}:${etype}:${dst}`);
        }

        // Write-through cache invalidation
        const cache = getCache(db);
        if (cache) {
          cache.invalidateEdge(src, etype, dst);
          cache.invalidateNode(src);
          cache.invalidateNode(dst);
        }

        return true;
      }
    }
    return false;
  }

  // Add to pending deletions
  let outPatches = tx.pendingOutDel.get(src);
  if (!outPatches) {
    outPatches = [];
    tx.pendingOutDel.set(src, outPatches);
  }
  outPatches.push({ etype, other: dst });

  let inPatches = tx.pendingInDel.get(dst);
  if (!inPatches) {
    inPatches = [];
    tx.pendingInDel.set(dst, inPatches);
  }
  inPatches.push({ etype, other: src });

  // Record write for MVCC conflict detection
  const mvcc = getMvccManager(db);
  if (mvcc) {
    mvcc.txManager.recordWrite(tx.txid, `edge:${src}:${etype}:${dst}`);
  }

  // Write-through cache invalidation (immediate)
  const cache = getCache(db);
  if (cache) {
    cache.invalidateEdge(src, etype, dst);
    cache.invalidateNode(src);
    cache.invalidateNode(dst);
  }

  return true;
}

/**
 * Get out-neighbors
 * Accepts GraphDB for auto-commit reads or TxHandle for transactional reads
 */
export function* getNeighborsOut(
  handle: GraphDB | TxHandle,
  nodeId: NodeID,
  etype?: ETypeID,
): Generator<Edge> {
  const db = isTxHandle(handle) ? handle._db : handle;
  const tx = isTxHandle(handle) ? handle._tx : null;
  const cache = getCache(db);
  
  // Record read for MVCC conflict detection
  if (tx && isMvccEnabled(db)) {
    const mvcc = getMvccManager(db);
    if (mvcc) {
      mvcc.txManager.recordRead(tx.txid, `neighbors_out:${nodeId}:${etype ?? '*'}`);
    }
  }
  
  // Include pending edges from transaction
  if (tx) {
    const pendingOut = tx.pendingOutAdd.get(nodeId);
    if (pendingOut) {
      for (const patch of pendingOut) {
        if (etype === undefined || patch.etype === etype) {
          yield { src: nodeId, etype: patch.etype, dst: patch.other };
        }
      }
    }
  }
  
  // Check cache first (only for non-transaction reads)
  if (!tx && cache) {
    const cached = cache.getTraversal(nodeId, etype, "out");
    if (cached) {
      for (const edge of cached.neighbors) {
        yield edge;
      }
      // If truncated, fall through to get remaining neighbors
      if (!cached.truncated) {
        return;
      }
    }
  }

  // Collect neighbors
  const neighbors: Edge[] = [];
  for (const edge of neighborsOut(db._snapshot, db._delta, nodeId, etype)) {
    // Skip edges that are pending deletion in this transaction
    if (tx) {
      const pendingDel = tx.pendingOutDel.get(nodeId);
      if (pendingDel?.some(p => p.etype === edge.etype && p.other === edge.dst)) {
        continue;
      }
    }
    neighbors.push(edge);
    yield edge;
  }

  // Cache the results (only for non-transaction reads)
  if (!tx && cache) {
    cache.setTraversal(nodeId, etype, "out", neighbors);
  }
}

/**
 * Get in-neighbors
 * Accepts GraphDB for auto-commit reads or TxHandle for transactional reads
 */
export function* getNeighborsIn(
  handle: GraphDB | TxHandle,
  nodeId: NodeID,
  etype?: ETypeID,
): Generator<Edge> {
  const db = isTxHandle(handle) ? handle._db : handle;
  const tx = isTxHandle(handle) ? handle._tx : null;
  const cache = getCache(db);
  
  // Record read for MVCC conflict detection
  if (tx && isMvccEnabled(db)) {
    const mvcc = getMvccManager(db);
    if (mvcc) {
      mvcc.txManager.recordRead(tx.txid, `neighbors_in:${nodeId}:${etype ?? '*'}`);
    }
  }
  
  // Include pending edges from transaction
  if (tx) {
    const pendingIn = tx.pendingInAdd.get(nodeId);
    if (pendingIn) {
      for (const patch of pendingIn) {
        if (etype === undefined || patch.etype === etype) {
          yield { src: patch.other, etype: patch.etype, dst: nodeId };
        }
      }
    }
  }
  
  // Check cache first (only for non-transaction reads)
  if (!tx && cache) {
    const cached = cache.getTraversal(nodeId, etype, "in");
    if (cached) {
      for (const edge of cached.neighbors) {
        yield edge;
      }
      // If truncated, fall through to get remaining neighbors
      if (!cached.truncated) {
        return;
      }
    }
  }

  // Collect neighbors
  const neighbors: Edge[] = [];
  for (const edge of neighborsIn(db._snapshot, db._delta, nodeId, etype)) {
    // Skip edges that are pending deletion in this transaction
    if (tx) {
      const pendingDel = tx.pendingInDel.get(nodeId);
      if (pendingDel?.some(p => p.etype === edge.etype && p.other === edge.src)) {
        continue;
      }
    }
    neighbors.push(edge);
    yield edge;
  }

  // Cache the results (only for non-transaction reads)
  if (!tx && cache) {
    cache.setTraversal(nodeId, etype, "in", neighbors);
  }
}

/**
 * Check if edge exists
 * Accepts GraphDB for auto-commit reads or TxHandle for transactional reads
 */
export function edgeExists(
  handle: GraphDB | TxHandle,
  src: NodeID,
  etype: ETypeID,
  dst: NodeID,
): boolean {
  const db = isTxHandle(handle) ? handle._db : handle;
  const tx = isTxHandle(handle) ? handle._tx : null;
  
  // Check pending transaction changes first
  if (tx) {
    // Check pending additions
    const pendingOut = tx.pendingOutAdd.get(src);
    if (pendingOut?.some(p => p.etype === etype && p.other === dst)) {
      return true;
    }
    // Check pending deletions
    const pendingDel = tx.pendingOutDel.get(src);
    if (pendingDel?.some(p => p.etype === etype && p.other === dst)) {
      return false;
    }
  }
  
  // MVCC mode: use version chains for visibility
  if (isMvccEnabled(db)) {
    const mvcc = getMvccManager(db);
    if (!mvcc) return hasEdgeMerged(db._snapshot, db._delta, src, etype, dst);
    
    // Fast-path: auto-commit read with no active transactions
    // Skip MVCC overhead entirely - no visibility conflicts possible
    if (!tx && mvcc.txManager.getActiveCount() === 0) {
      return hasEdgeMerged(db._snapshot, db._delta, src, etype, dst);
    }
    
    // Fast-path: no edge versions exist, skip version chain lookup
    if (!mvcc.versionChain.hasAnyEdgeVersions()) {
      return hasEdgeMerged(db._snapshot, db._delta, src, etype, dst);
    }
    
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
        mvcc.txManager.recordRead(txid, `edge:${src}:${etype}:${dst}`);
      }
    }
    
    // Check version chain
    const edgeVersion = mvcc.versionChain.getEdgeVersion(src, etype, dst);
    if (edgeVersion && mvccEdgeExists(edgeVersion, txSnapshotTs, txid)) {
      return true;
    }
    
    // Fall back to snapshot/delta check
    return hasEdgeMerged(db._snapshot, db._delta, src, etype, dst);
  }
  
  // Non-MVCC mode: original logic
  return hasEdgeMerged(db._snapshot, db._delta, src, etype, dst);
}

/**
 * Set an edge property
 */
export function setEdgeProp(
  handle: TxHandle,
  src: NodeID,
  etype: ETypeID,
  dst: NodeID,
  keyId: PropKeyID,
  value: PropValue,
): void {
  const { _db: db, _tx: tx } = handle;
  const edgeKey = `${src}:${etype}:${dst}`;

  let props = tx.pendingEdgeProps.get(edgeKey);
  if (!props) {
    props = new Map();
    tx.pendingEdgeProps.set(edgeKey, props);
  }
  props.set(keyId, value);

  // Record write for MVCC conflict detection
  const mvcc = getMvccManager(db);
  if (mvcc && isMvccEnabled(db)) {
    mvcc.txManager.recordWrite(tx.txid, `edgeprop:${edgeKey}:${keyId}`);
  }

  // Write-through cache invalidation (immediate)
  const cache = getCache(db);
  if (cache) {
    cache.invalidateEdge(src, etype, dst);
  }
}

/**
 * Delete an edge property
 */
export function delEdgeProp(
  handle: TxHandle,
  src: NodeID,
  etype: ETypeID,
  dst: NodeID,
  keyId: PropKeyID,
): void {
  const { _db: db, _tx: tx } = handle;
  const edgeKey = `${src}:${etype}:${dst}`;

  let props = tx.pendingEdgeProps.get(edgeKey);
  if (!props) {
    props = new Map();
    tx.pendingEdgeProps.set(edgeKey, props);
  }
  props.set(keyId, null);

  // Record write for MVCC conflict detection
  const mvcc = getMvccManager(db);
  if (mvcc && isMvccEnabled(db)) {
    mvcc.txManager.recordWrite(tx.txid, `edgeprop:${edgeKey}:${keyId}`);
  }

  // Write-through cache invalidation (immediate)
  const cache = getCache(db);
  if (cache) {
    cache.invalidateEdge(src, etype, dst);
  }
}

/**
 * Get a specific property for an edge
 * Returns null if the edge doesn't exist or the property is not set
 * Accepts GraphDB for auto-commit reads or TxHandle for transactional reads
 */
export function getEdgeProp(
  handle: GraphDB | TxHandle,
  src: NodeID,
  etype: ETypeID,
  dst: NodeID,
  keyId: PropKeyID,
): PropValue | null {
  const db = isTxHandle(handle) ? handle._db : handle;
  const tx = isTxHandle(handle) ? handle._tx : null;
  const cache = getCache(db);
  
  // Check pending transaction changes first
  if (tx) {
    const edgeKey = `${src}:${etype}:${dst}`;
    const pendingProps = tx.pendingEdgeProps.get(edgeKey);
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
          mvcc.txManager.recordRead(txid, `edgeprop:${src}:${etype}:${dst}:${keyId}`);
        }
      }
      
      // Note: We don't use cache in MVCC mode when we have a specific transaction
      // because the cache stores the latest committed value, not snapshot-specific values
      if (!tx && cache) {
        const cached = cache.getEdgeProp(src, etype, dst, keyId);
        if (cached !== undefined) {
          return cached;
        }
      }
      
      // Get visible version from version chain
      const propVersion = mvcc.versionChain.getEdgePropVersion(src, etype, dst, keyId);
      const visibleVersion = getVisibleVersion(propVersion, txSnapshotTs, txid);
      
      if (visibleVersion) {
        const value = visibleVersion.data;
        if (!tx && cache) {
          cache.setEdgeProp(src, etype, dst, keyId, value);
        }
        return value;
      }
      
      // Fall back to snapshot/delta check
      // Check if endpoints are deleted
      if (isNodeDeleted(db._delta, src) || isNodeDeleted(db._delta, dst)) {
        if (!tx && cache) {
          cache.setEdgeProp(src, etype, dst, keyId, null);
        }
        return null;
      }

      // Check delta first
      const key = edgePropKey(src, etype, dst);
      const deltaProps = db._delta.edgeProps.get(key);
      if (deltaProps) {
        const deltaValue = deltaProps.get(keyId);
        if (deltaValue !== undefined) {
          if (!tx && cache) {
            cache.setEdgeProp(src, etype, dst, keyId, deltaValue);
          }
          return deltaValue;
        }
      }

      // Fall back to snapshot
      if (db._snapshot) {
        const srcPhys = getPhysNode(db._snapshot, src);
        const dstPhys = getPhysNode(db._snapshot, dst);
        if (srcPhys >= 0 && dstPhys >= 0) {
          const edgeIdx = findEdgeIndex(db._snapshot, srcPhys, etype, dstPhys);
          if (edgeIdx >= 0) {
            const value = snapshotGetEdgeProp(db._snapshot, edgeIdx, keyId);
            if (!tx && cache) {
              cache.setEdgeProp(src, etype, dst, keyId, value);
            }
            return value;
          }
        }
      }

      if (!tx && cache) {
        cache.setEdgeProp(src, etype, dst, keyId, null);
      }
      return null;
    }
  }
  
  // Non-MVCC mode: original logic
  if (cache) {
    const cached = cache.getEdgeProp(src, etype, dst, keyId);
    if (cached !== undefined) {
      return cached;
    }
  }

  // Check if endpoints are deleted
  if (isNodeDeleted(db._delta, src) || isNodeDeleted(db._delta, dst)) {
    if (cache) {
      cache.setEdgeProp(src, etype, dst, keyId, null);
    }
    return null;
  }

  // Check delta first
  const key = edgePropKey(src, etype, dst);
  const deltaProps = db._delta.edgeProps.get(key);
  if (deltaProps) {
    const deltaValue = deltaProps.get(keyId);
    if (deltaValue !== undefined) {
      if (cache) {
        cache.setEdgeProp(src, etype, dst, keyId, deltaValue);
      }
      return deltaValue; // null means deleted
    }
  }

  // Fall back to snapshot
  if (db._snapshot) {
    const srcPhys = getPhysNode(db._snapshot, src);
    const dstPhys = getPhysNode(db._snapshot, dst);
    if (srcPhys >= 0 && dstPhys >= 0) {
      const edgeIdx = findEdgeIndex(db._snapshot, srcPhys, etype, dstPhys);
      if (edgeIdx >= 0) {
        const value = snapshotGetEdgeProp(db._snapshot, edgeIdx, keyId);
        if (cache) {
          cache.setEdgeProp(src, etype, dst, keyId, value);
        }
        return value;
      }
    }
  }

  // Property not found
  if (cache) {
    cache.setEdgeProp(src, etype, dst, keyId, null);
  }
  return null;
}

/**
 * Get all properties for an edge
 * Returns null if the edge doesn't exist
 * Accepts GraphDB for auto-commit reads or TxHandle for transactional reads
 */
export function getEdgeProps(
  handle: GraphDB | TxHandle,
  src: NodeID,
  etype: ETypeID,
  dst: NodeID,
): Map<PropKeyID, PropValue> | null {
  const db = isTxHandle(handle) ? handle._db : handle;
  const tx = isTxHandle(handle) ? handle._tx : null;
  
  // Check if endpoints are deleted
  if (isNodeDeleted(db._delta, src) || isNodeDeleted(db._delta, dst)) {
    return null;
  }
  
  // Check pending transaction deletions for endpoints
  if (tx) {
    if (tx.pendingDeletedNodes.has(src) || tx.pendingDeletedNodes.has(dst)) {
      return null;
    }
  }

  const props = new Map<PropKeyID, PropValue>();
  let edgeExistsFlag = false;

  // Get from snapshot first
  if (db._snapshot) {
    const srcPhys = getPhysNode(db._snapshot, src);
    const dstPhys = getPhysNode(db._snapshot, dst);
    if (srcPhys >= 0 && dstPhys >= 0) {
      const edgeIdx = findEdgeIndex(db._snapshot, srcPhys, etype, dstPhys);
      if (edgeIdx >= 0) {
        edgeExistsFlag = true;
        const snapshotProps = snapshotGetEdgeProps(db._snapshot, edgeIdx);
        if (snapshotProps) {
          for (const [keyId, value] of snapshotProps) {
            props.set(keyId, value);
          }
        }
      }
    }
  }

  // Apply delta modifications
  const key = edgePropKey(src, etype, dst);
  const deltaProps = db._delta.edgeProps.get(key);
  if (deltaProps) {
    for (const [keyId, value] of deltaProps) {
      if (value === null) {
        props.delete(keyId);
      } else {
        props.set(keyId, value);
      }
    }
  }

  // Check if edge was added in delta
  const addedEdges = db._delta.outAdd.get(src);
  if (addedEdges) {
    for (const patch of addedEdges) {
      if (patch.etype === etype && patch.other === dst) {
        edgeExistsFlag = true;
        break;
      }
    }
  }
  
  // Apply pending transaction modifications
  if (tx) {
    const edgeKey = `${src}:${etype}:${dst}`;
    const pendingProps = tx.pendingEdgeProps.get(edgeKey);
    if (pendingProps) {
      for (const [keyId, value] of pendingProps) {
        if (value === null) {
          props.delete(keyId);
        } else {
          props.set(keyId, value);
        }
      }
    }
    
    // Check if edge was added in this transaction
    const pendingOut = tx.pendingOutAdd.get(src);
    if (pendingOut?.some(p => p.etype === etype && p.other === dst)) {
      edgeExistsFlag = true;
    }
    
    // Check if edge was deleted in this transaction
    const pendingDel = tx.pendingOutDel.get(src);
    if (pendingDel?.some(p => p.etype === etype && p.other === dst)) {
      return null;
    }
  }

  if (!edgeExistsFlag) {
    return null;
  }

  return props;
}

