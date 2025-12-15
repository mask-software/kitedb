/**
 * MVCC Version Chain Store
 * 
 * Manages version chains for nodes, edges, and properties
 */

import type {
  VersionedRecord,
  VersionChainStore,
  NodeVersionData,
  EdgeVersionData,
  NodeID,
  ETypeID,
  PropKeyID,
  PropValue,
} from "../types.ts";

export class VersionChainManager {
  private store: VersionChainStore;

  constructor() {
    this.store = {
      nodeVersions: new Map(),
      edgeVersions: new Map(),
      nodePropVersions: new Map(),
      edgePropVersions: new Map(),
    };
  }

  /**
   * Append a new version to a node's version chain
   */
  appendNodeVersion(
    nodeId: NodeID,
    data: NodeVersionData,
    txid: bigint,
    commitTs: bigint,
  ): void {
    const existing = this.store.nodeVersions.get(nodeId);
    const newVersion: VersionedRecord<NodeVersionData> = {
      data,
      txid,
      commitTs,
      prev: existing || null,
      deleted: false,
    };
    this.store.nodeVersions.set(nodeId, newVersion);
  }

  /**
   * Mark a node as deleted
   */
  deleteNodeVersion(
    nodeId: NodeID,
    txid: bigint,
    commitTs: bigint,
  ): void {
    const existing = this.store.nodeVersions.get(nodeId);
    const deletedVersion: VersionedRecord<NodeVersionData> = {
      data: { nodeId, delta: { labels: new Set(), labelsDeleted: new Set(), props: new Map() } },
      txid,
      commitTs,
      prev: existing || null,
      deleted: true,
    };
    this.store.nodeVersions.set(nodeId, deletedVersion);
  }

  /**
   * Append a new version to an edge's version chain
   */
  appendEdgeVersion(
    src: NodeID,
    etype: ETypeID,
    dst: NodeID,
    added: boolean,
    txid: bigint,
    commitTs: bigint,
  ): void {
    const key = `${src}:${etype}:${dst}`;
    const existing = this.store.edgeVersions.get(key);
    const newVersion: VersionedRecord<EdgeVersionData> = {
      data: { src, etype, dst, added },
      txid,
      commitTs,
      prev: existing || null,
      deleted: false,
    };
    this.store.edgeVersions.set(key, newVersion);
  }

  /**
   * Append a new version to a node property's version chain
   */
  appendNodePropVersion(
    nodeId: NodeID,
    propKeyId: PropKeyID,
    value: PropValue | null,
    txid: bigint,
    commitTs: bigint,
  ): void {
    const key = `${nodeId}:${propKeyId}`;
    const existing = this.store.nodePropVersions.get(key);
    const newVersion: VersionedRecord<PropValue | null> = {
      data: value,
      txid,
      commitTs,
      prev: existing || null,
      deleted: false,
    };
    this.store.nodePropVersions.set(key, newVersion);
  }

  /**
   * Append a new version to an edge property's version chain
   */
  appendEdgePropVersion(
    src: NodeID,
    etype: ETypeID,
    dst: NodeID,
    propKeyId: PropKeyID,
    value: PropValue | null,
    txid: bigint,
    commitTs: bigint,
  ): void {
    const key = `${src}:${etype}:${dst}:${propKeyId}`;
    const existing = this.store.edgePropVersions.get(key);
    const newVersion: VersionedRecord<PropValue | null> = {
      data: value,
      txid,
      commitTs,
      prev: existing || null,
      deleted: false,
    };
    this.store.edgePropVersions.set(key, newVersion);
  }

  /**
   * Get the latest version for a node (for visibility checking)
   */
  getNodeVersion(nodeId: NodeID): VersionedRecord<NodeVersionData> | null {
    return this.store.nodeVersions.get(nodeId) || null;
  }

  /**
   * Get the latest version for an edge (for visibility checking)
   */
  getEdgeVersion(
    src: NodeID,
    etype: ETypeID,
    dst: NodeID,
  ): VersionedRecord<EdgeVersionData> | null {
    const key = `${src}:${etype}:${dst}`;
    return this.store.edgeVersions.get(key) || null;
  }

  /**
   * Get the latest version for a node property (for visibility checking)
   */
  getNodePropVersion(
    nodeId: NodeID,
    propKeyId: PropKeyID,
  ): VersionedRecord<PropValue | null> | null {
    const key = `${nodeId}:${propKeyId}`;
    return this.store.nodePropVersions.get(key) || null;
  }

  /**
   * Get the latest version for an edge property (for visibility checking)
   */
  getEdgePropVersion(
    src: NodeID,
    etype: ETypeID,
    dst: NodeID,
    propKeyId: PropKeyID,
  ): VersionedRecord<PropValue | null> | null {
    const key = `${src}:${etype}:${dst}:${propKeyId}`;
    return this.store.edgePropVersions.get(key) || null;
  }

  /**
   * Prune old versions older than the given timestamp
   * Returns number of versions pruned
   */
  pruneOldVersions(horizonTs: bigint): number {
    let pruned = 0;

    // Prune node versions
    for (const [nodeId, version] of this.store.nodeVersions.entries()) {
      const prunedCount = this.pruneChain(version, horizonTs);
      if (prunedCount === -1) {
        // Entire chain was pruned
        this.store.nodeVersions.delete(nodeId);
        pruned++;
      } else {
        pruned += prunedCount;
      }
    }

    // Prune edge versions
    for (const [key, version] of this.store.edgeVersions.entries()) {
      const prunedCount = this.pruneChain(version, horizonTs);
      if (prunedCount === -1) {
        this.store.edgeVersions.delete(key);
        pruned++;
      } else {
        pruned += prunedCount;
      }
    }

    // Prune node property versions
    for (const [key, version] of this.store.nodePropVersions.entries()) {
      const prunedCount = this.pruneChain(version, horizonTs);
      if (prunedCount === -1) {
        this.store.nodePropVersions.delete(key);
        pruned++;
      } else {
        pruned += prunedCount;
      }
    }

    // Prune edge property versions
    for (const [key, version] of this.store.edgePropVersions.entries()) {
      const prunedCount = this.pruneChain(version, horizonTs);
      if (prunedCount === -1) {
        this.store.edgePropVersions.delete(key);
        pruned++;
      } else {
        pruned += prunedCount;
      }
    }

    return pruned;
  }

  /**
   * Prune a version chain, removing versions older than horizonTs
   * Returns: -1 if entire chain should be deleted, otherwise count of pruned versions
   */
  private pruneChain<T>(
    version: VersionedRecord<T>,
    horizonTs: bigint,
  ): number {
    // Find the first version we need to keep (newest version < horizonTs)
    // and count versions to prune
    let current: VersionedRecord<T> | null = version;
    let keepPoint: VersionedRecord<T> | null = null;
    let prunedCount = 0;

    // Walk to find the boundary
    while (current) {
      if (current.commitTs < horizonTs) {
        // This is an old version
        if (!keepPoint) {
          // Keep the newest old version as the boundary
          keepPoint = current;
        } else {
          // This is older than keepPoint, will be pruned
          prunedCount++;
        }
      }
      current = current.prev;
    }

    // If no old version found, nothing to prune
    if (!keepPoint) {
      return 0;
    }

    // If keepPoint is the head and all versions are old, delete entire chain
    if (keepPoint === version && version.commitTs < horizonTs) {
      return -1;
    }

    // Truncate the chain at keepPoint (remove versions older than keepPoint)
    if (keepPoint.prev !== null) {
      keepPoint.prev = null;
    }
    
    return prunedCount;
  }

  /**
   * Get the store (for recovery/debugging)
   */
  getStore(): VersionChainStore {
    return this.store;
  }

  /**
   * Clear all versions (for testing/recovery)
   */
  clear(): void {
    this.store.nodeVersions.clear();
    this.store.edgeVersions.clear();
    this.store.nodePropVersions.clear();
    this.store.edgePropVersions.clear();
  }
}

