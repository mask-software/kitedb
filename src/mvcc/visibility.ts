/**
 * MVCC Visibility Rules
 * 
 * Determines which versions are visible to a transaction based on snapshot isolation
 */

import type {
  VersionedRecord,
  MvccTransaction,
} from "../types.ts";

/**
 * Check if a version is visible to a transaction
 * 
 * A version is visible if:
 * 1. It was committed before the transaction's snapshot timestamp (commitTs < startTs)
 * 2. OR it was created by the transaction itself (own writes)
 * 3. AND it's not deleted (unless it's the transaction's own deletion)
 */
export function isVisible<T>(
  version: VersionedRecord<T> | null,
  txSnapshotTs: bigint,
  txid: bigint,
): boolean {
  if (!version) {
    return false;
  }

  // Own writes are always visible (even if uncommitted)
  if (version.txid === txid) {
    return true;
  }

  // Must be committed before snapshot
  if (version.commitTs >= txSnapshotTs) {
    return false;
  }

  // Check if there's a newer visible version that supersedes this one
  // (This is handled by walking the chain in getVisibleVersion)
  return true;
}

/**
 * Get the visible version from a version chain
 * Walks the chain to find the newest version visible to the transaction
 */
export function getVisibleVersion<T>(
  head: VersionedRecord<T> | null,
  txSnapshotTs: bigint,
  txid: bigint,
): VersionedRecord<T> | null {
  if (!head) {
    return null;
  }

  let current: VersionedRecord<T> | null = head;

  // Walk the chain from newest to oldest
  while (current) {
    if (isVisible(current, txSnapshotTs, txid)) {
      // Found a visible version - return it immediately
      // (whether deleted or not, this is the authoritative version for this snapshot)
      return current;
    }
    
    // This version is not visible, check older versions
    current = current.prev;
  }

  return null;
}

/**
 * Check if a node exists (is visible and not deleted)
 */
export function nodeExists(
  version: VersionedRecord<any> | null,
  txSnapshotTs: bigint,
  txid: bigint,
): boolean {
  const visible = getVisibleVersion(version, txSnapshotTs, txid);
  if (!visible) {
    return false;
  }
  
  // Node exists if the visible version is not deleted
  return !visible.deleted;
}

/**
 * Check if an edge exists (is visible and was added, not deleted)
 * 
 * For edges, the `added` field directly indicates the edge state:
 * - added=true means the edge exists
 * - added=false means the edge was deleted
 * 
 * Unlike nodes which use tombstone markers, edges encode their existence
 * state directly in the `added` field, so we simply return that value.
 */
export function edgeExists(
  version: VersionedRecord<any> | null,
  txSnapshotTs: bigint,
  txid: bigint,
): boolean {
  const visible = getVisibleVersion(version, txSnapshotTs, txid);
  if (!visible) {
    return false;
  }
  
  // For edges, the 'added' flag directly indicates existence
  if (visible.data && typeof visible.data === 'object' && 'added' in visible.data) {
    return (visible.data as { added: boolean }).added;
  }
  
  return false;
}

