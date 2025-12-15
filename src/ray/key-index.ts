/**
 * Key index - lookup across snapshot and delta
 */

import { isNodeDeleted, lookupKeyInDelta } from "../core/delta.ts";
import {
  getPhysNode,
  getNodeKey as snapshotGetNodeKey,
  lookupByKey as snapshotLookupByKey,
} from "../core/snapshot-reader.ts";
import type { DeltaState, NodeID, SnapshotData } from "../types.ts";

/**
 * Look up a node by key across snapshot and delta
 * Delta takes precedence over snapshot
 */
export function lookupByKey(
  snapshot: SnapshotData | null,
  delta: DeltaState,
  key: string,
): NodeID | null {
  // First check delta
  const deltaResult = lookupKeyInDelta(delta, key);

  if (deltaResult === "deleted") {
    // Key was explicitly deleted in delta
    return null;
  }

  if (deltaResult !== null) {
    // Found in delta
    return deltaResult;
  }

  // Fall back to snapshot
  if (snapshot) {
    const snapshotResult = snapshotLookupByKey(snapshot, key);

    if (snapshotResult !== null) {
      // Check if the node was deleted
      if (isNodeDeleted(delta, snapshotResult)) {
        return null;
      }
      return snapshotResult;
    }
  }

  return null;
}

/**
 * Check if a key exists
 */
export function hasKey(
  snapshot: SnapshotData | null,
  delta: DeltaState,
  key: string,
): boolean {
  return lookupByKey(snapshot, delta, key) !== null;
}

/**
 * Get the key for a node if it has one
 */
export function getNodeKey(
  snapshot: SnapshotData | null,
  delta: DeltaState,
  nodeId: NodeID,
): string | null {
  // Check if node is deleted
  if (isNodeDeleted(delta, nodeId)) {
    return null;
  }

  // Check delta for created nodes
  const created = delta.createdNodes.get(nodeId);
  if (created?.key) {
    return created.key;
  }

  // Check delta for modified nodes
  const modified = delta.modifiedNodes.get(nodeId);
  if (modified?.key !== undefined) {
    return modified.key ?? null;
  }

  // Fall back to snapshot
  if (snapshot) {
    const phys = getPhysNode(snapshot, nodeId);
    if (phys >= 0) {
      return snapshotGetNodeKey(snapshot, phys);
    }
  }

  return null;
}
