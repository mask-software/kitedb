/**
 * MVCC Conflict Detection
 * 
 * Detects read-write and write-write conflicts using optimistic concurrency control
 */

import type { MvccTransaction } from "../types.ts";
import { ConflictError } from "../types.ts";
import { TxManager } from "./tx-manager.ts";
import { VersionChainManager } from "./version-chain.ts";

export class ConflictDetector {
  constructor(
    private txManager: TxManager,
    private versionChain: VersionChainManager,
  ) {}

  /**
   * Check for conflicts before committing a transaction
   * 
   * Conflicts occur when:
   * 1. Read-Write: Transaction read a key that was modified by a concurrent committed transaction
   * 2. Write-Write: Transaction wrote a key that was also written by a concurrent committed transaction
   * 
   * Returns array of conflicting keys if conflicts found, empty array otherwise
   */
  checkConflicts(txid: bigint): string[] {
    const tx = this.txManager.getTx(txid);
    if (!tx || tx.status !== 'active') {
      return [];
    }

    // Fast path: if nothing was read or written, no conflicts possible
    if (tx.readSet.size === 0 && tx.writeSet.size === 0) {
      return [];
    }

    const conflicts: string[] = [];
    const txSnapshotTs = tx.startTs;

    // Check read-write conflicts
    if (tx.readSet.size > 0) {
      for (const readKey of tx.readSet) {
        if (this.hasConflict(readKey, txSnapshotTs, txid)) {
          conflicts.push(readKey);
        }
      }
    }

    // Check write-write conflicts
    if (tx.writeSet.size > 0) {
      for (const writeKey of tx.writeSet) {
        if (this.hasConflict(writeKey, txSnapshotTs, txid)) {
          conflicts.push(writeKey);
        }
      }
    }

    return conflicts;
  }

  /**
   * Check if there's a conflict for a key (fast path - no array allocations)
   */
  private hasConflict(
    key: string,
    txSnapshotTs: bigint,
    currentTxid: bigint,
  ): boolean {
    return this.txManager.hasConflictingWrite(key, txSnapshotTs, currentTxid);
  }

  /**
   * Validate transaction can commit (throws ConflictError if conflicts found)
   */
  validateCommit(txid: bigint): void {
    const conflicts = this.checkConflicts(txid);
    if (conflicts.length > 0) {
      throw new ConflictError(
        `Transaction ${txid} conflicts with concurrent transactions on keys: ${conflicts.join(', ')}`,
        txid,
        conflicts,
      );
    }
  }
}

