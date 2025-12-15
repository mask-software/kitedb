/**
 * MVCC Transaction Manager
 * 
 * Manages transaction lifecycle, timestamps, and active transaction tracking
 */

import type { MvccTransaction } from "../types.ts";

interface CommittedWrite {
  txid: bigint;
  commitTs: bigint;
}

export class TxManager {
  private activeTxs: Map<bigint, MvccTransaction> = new Map();
  private nextTxId: bigint;
  private nextCommitTs: bigint;
  // Inverted index: key -> array of transactions that wrote it (recent writes only)
  private committedWrites: Map<string, CommittedWrite[]> = new Map();

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

    const commitTs = this.nextCommitTs++;
    tx.commitTs = commitTs;
    tx.status = 'committed';

    // Index writes for fast conflict detection
    // Pre-create the write record once to avoid repeated object creation
    const writeRecord: CommittedWrite = { txid, commitTs };
    const writeSet = tx.writeSet;
    const committedWrites = this.committedWrites;
    
    for (const key of writeSet) {
      let writes = committedWrites.get(key);
      if (!writes) {
        // For single write, use single-element array directly
        committedWrites.set(key, [writeRecord]);
      } else {
        writes.push(writeRecord);
      }
    }

    // Keep transaction in map for GC visibility calculation
    // Will be removed when GC determines it's safe
    return commitTs;
  }

  /**
   * Abort a transaction
   */
  abortTx(txid: bigint): void {
    const tx = this.activeTxs.get(txid);
    if (!tx) {
      return; // Already removed or never existed
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
      // Clean up committed writes index
      for (const key of tx.writeSet) {
        const writes = this.committedWrites.get(key);
        if (writes) {
          // Remove this transaction's entry
          const index = writes.findIndex(w => w.txid === txid);
          if (index >= 0) {
            writes.splice(index, 1);
          }
          // Remove empty arrays to save memory
          if (writes.length === 0) {
            this.committedWrites.delete(key);
          }
        }
      }
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
   * Get transaction count
   */
  getActiveCount(): number {
    let count = 0;
    for (const tx of this.activeTxs.values()) {
      if (tx.status === 'active') count++;
    }
    return count;
  }

  /**
   * Check if there are other active transactions besides the given one
   * Fast path for determining if version chains are needed
   */
  hasOtherActiveTransactions(excludeTxid: bigint): boolean {
    for (const tx of this.activeTxs.values()) {
      if (tx.status === 'active' && tx.txid !== excludeTxid) {
        return true;
      }
    }
    return false;
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
   * Returns array of transactions that wrote this key with commitTs >= minCommitTs
   */
  getCommittedWrites(key: string, minCommitTs: bigint): CommittedWrite[] {
    const writes = this.committedWrites.get(key);
    if (!writes) {
      return [];
    }
    // Filter to only concurrent writes (commitTs >= minCommitTs)
    return writes.filter(w => w.commitTs >= minCommitTs);
  }

  /**
   * Check if there's a conflicting write for a key (fast path for conflict detection)
   * Returns true if any transaction (other than currentTxid) wrote this key with commitTs >= minCommitTs
   */
  hasConflictingWrite(key: string, minCommitTs: bigint, currentTxid: bigint): boolean {
    const writes = this.committedWrites.get(key);
    if (!writes) {
      return false;
    }
    // Check for any concurrent write (without allocating arrays)
    for (let i = 0; i < writes.length; i++) {
      const w = writes[i]!;
      if (w.commitTs >= minCommitTs && w.txid !== currentTxid) {
        return true;
      }
    }
    return false;
  }

  /**
   * Clear all transactions (for testing/recovery)
   */
  clear(): void {
    this.activeTxs.clear();
    this.committedWrites.clear();
  }
}

