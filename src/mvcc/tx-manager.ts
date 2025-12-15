/**
 * MVCC Transaction Manager
 * 
 * Manages transaction lifecycle, timestamps, and active transaction tracking
 */

import type { MvccTransaction } from "../types.ts";

export class TxManager {
  private activeTxs: Map<bigint, MvccTransaction> = new Map();
  private nextTxId: bigint;
  private nextCommitTs: bigint;
  // Inverted index: key -> max commitTs for conflict detection
  private committedWrites: Map<string, bigint> = new Map();
  // O(1) tracking of active transaction count
  private activeCount: number = 0;

  constructor(initialTxId: bigint = 1n, initialCommitTs: bigint = 1n) {
    this.nextTxId = initialTxId;
    this.nextCommitTs = initialCommitTs;
  }

  /**
   * Get the minimum active timestamp (oldest active transaction snapshot)
   * Used for GC horizon calculation
   */
  get minActiveTs(): bigint {
    if (this.activeTxs.size === 0) {
      return this.nextCommitTs;
    }

    let min = this.nextCommitTs;
    for (const tx of this.activeTxs.values()) {
      if (tx.status === 'active' && tx.startTs < min) {
        min = tx.startTs;
      }
    }
    return min;
  }

  /**
   * Begin a new transaction
   * Returns transaction ID and snapshot timestamp
   */
  beginTx(): { txid: bigint; startTs: bigint } {
    const txid = this.nextTxId++;
    const startTs = this.nextCommitTs; // Snapshot at current commit timestamp

    const tx: MvccTransaction = {
      txid,
      startTs,
      commitTs: null,
      status: 'active',
      readSet: new Set(),
      writeSet: new Set(),
    };

    this.activeTxs.set(txid, tx);
    this.activeCount++;
    return { txid, startTs };
  }

  /**
   * Get transaction by ID
   */
  getTx(txid: bigint): MvccTransaction | undefined {
    return this.activeTxs.get(txid);
  }

  /**
   * Check if transaction is active
   */
  isActive(txid: bigint): boolean {
    const tx = this.activeTxs.get(txid);
    return tx !== undefined && tx.status === 'active';
  }

  /**
   * Record a read operation
   */
  recordRead(txid: bigint, key: string): void {
    const tx = this.activeTxs.get(txid);
    if (tx && tx.status === 'active') {
      tx.readSet.add(key);
    }
  }

  /**
   * Record a write operation
   */
  recordWrite(txid: bigint, key: string): void {
    const tx = this.activeTxs.get(txid);
    if (tx && tx.status === 'active') {
      tx.writeSet.add(key);
    }
  }

  /**
   * Commit a transaction
   * Returns commit timestamp
   */
  commitTx(txid: bigint): bigint {
    const tx = this.activeTxs.get(txid);
    if (!tx) {
      throw new Error(`Transaction ${txid} not found`);
    }
    if (tx.status !== 'active') {
      throw new Error(`Transaction ${txid} is not active (status: ${tx.status})`);
    }

    this.activeCount--;
    const commitTs = this.nextCommitTs++;
    tx.commitTs = commitTs;
    tx.status = 'committed';

    // Index writes for fast conflict detection
    // Store only the max commitTs per key (simpler and faster than array)
    const writeSet = tx.writeSet;
    const committedWrites = this.committedWrites;
    
    for (const key of writeSet) {
      const existing = committedWrites.get(key);
      if (existing === undefined || commitTs > existing) {
        committedWrites.set(key, commitTs);
      }
    }

    // Eager cleanup: if no other active transactions, clean up immediately
    // This prevents unbounded growth of activeTxs in serial workloads
    if (this.activeCount === 0) {
      this.cleanupCommittedTx(txid, tx);
    }

    return commitTs;
  }

  /**
   * Clean up a committed transaction immediately (eager cleanup path)
   */
  private cleanupCommittedTx(txid: bigint, tx: MvccTransaction): void {
    // Clear the write set since we stored max commitTs, not per-tx writes
    // No need to clean committedWrites since it only stores max timestamp
    this.activeTxs.delete(txid);
  }

  /**
   * Abort a transaction
   */
  abortTx(txid: bigint): void {
    const tx = this.activeTxs.get(txid);
    if (!tx) {
      return; // Already removed or never existed
    }

    if (tx.status === 'active') {
      this.activeCount--;
    }
    tx.status = 'aborted';
    tx.commitTs = null;
    // Remove immediately on abort
    this.activeTxs.delete(txid);
  }

  /**
   * Remove a committed transaction (called by GC when safe)
   */
  removeTx(txid: bigint): void {
    const tx = this.activeTxs.get(txid);
    if (tx) {
      if (tx.status === 'active') {
        this.activeCount--;
      }
      // No need to clean committedWrites since it only stores max timestamp
    }
    this.activeTxs.delete(txid);
  }

  /**
   * Get all active transaction IDs
   */
  getActiveTxIds(): bigint[] {
    return Array.from(this.activeTxs.values())
      .filter(tx => tx.status === 'active')
      .map(tx => tx.txid);
  }

  /**
   * Get transaction count (O(1) using tracked counter)
   */
  getActiveCount(): number {
    return this.activeCount;
  }

  /**
   * Check if there are other active transactions besides the given one
   * Fast path for determining if version chains are needed
   * O(1) using tracked counter
   */
  hasOtherActiveTransactions(excludeTxid: bigint): boolean {
    // Fast path: if only 0 or 1 active, no need to iterate
    return this.activeCount > 1;
  }

  /**
   * Get the next commit timestamp (for snapshot reads outside transactions)
   */
  getNextCommitTs(): bigint {
    return this.nextCommitTs;
  }

  /**
   * Get all transactions (for debugging/recovery)
   * Returns iterator to avoid Map copy overhead
   */
  getAllTxs(): IterableIterator<[bigint, MvccTransaction]> {
    return this.activeTxs.entries();
  }

  /**
   * Get committed writes for a key (for conflict detection)
   * Returns the max commitTs for the key if >= minCommitTs, otherwise null
   */
  getCommittedWriteTs(key: string, minCommitTs: bigint): bigint | null {
    const maxTs = this.committedWrites.get(key);
    if (maxTs === undefined || maxTs < minCommitTs) {
      return null;
    }
    return maxTs;
  }

  /**
   * Check if there's a conflicting write for a key (fast path for conflict detection)
   * Returns true if any transaction wrote this key with commitTs >= minCommitTs
   * Note: currentTxid is no longer needed since we track max timestamp, not per-tx writes
   */
  hasConflictingWrite(key: string, minCommitTs: bigint, _currentTxid?: bigint): boolean {
    const maxTs = this.committedWrites.get(key);
    return maxTs !== undefined && maxTs >= minCommitTs;
  }

  /**
   * Clear all transactions (for testing/recovery)
   */
  clear(): void {
    this.activeTxs.clear();
    this.committedWrites.clear();
    this.activeCount = 0;
  }
}

