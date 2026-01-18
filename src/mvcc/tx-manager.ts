/**
 * MVCC Transaction Manager
 * 
 * Manages transaction lifecycle, timestamps, and active transaction tracking
 */

import type { MvccTransaction } from "../types.ts";

/** Maximum number of entries in committedWrites before pruning */
const MAX_COMMITTED_WRITES = 100_000;
/** Prune entries older than this when over limit (relative to minActiveTs) */
const PRUNE_THRESHOLD_ENTRIES = 50_000;

export class TxManager {
  private activeTxs: Map<bigint, MvccTransaction> = new Map();
  private nextTxId: bigint;
  private nextCommitTs: bigint;
  // Inverted index: key -> max commitTs for conflict detection
  private committedWrites: Map<string, bigint> = new Map();
  // O(1) tracking of active transaction count
  private activeCount: number = 0;
  // Track entries pruned for statistics
  private totalPruned: number = 0;
  // Track commit timestamp -> wall clock time mapping for GC retention
  // This allows converting from commit timestamps to wall clock time for retention calculations
  private commitTsToWallClock: Map<bigint, number> = new Map();

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
   * Get the oldest commit timestamp that is newer than the retention period
   * This converts a wall-clock retention period to a commit timestamp for GC
   * 
   * @param retentionMs Retention period in milliseconds
   * @returns The commit timestamp corresponding to the retention cutoff, or nextCommitTs if none
   */
  getRetentionHorizonTs(retentionMs: number): bigint {
    const cutoffTime = Date.now() - retentionMs;
    let oldestWithinRetention = this.nextCommitTs;
    
    // Find the oldest commit timestamp that is within the retention period
    for (const [commitTs, wallClock] of this.commitTsToWallClock) {
      if (wallClock >= cutoffTime && commitTs < oldestWithinRetention) {
        oldestWithinRetention = commitTs;
      }
    }
    
    return oldestWithinRetention;
  }

  /**
   * Prune old wall clock mappings that are older than the given commit timestamp
   * Called during GC to prevent unbounded growth
   */
  pruneWallClockMappings(horizonTs: bigint): void {
    for (const [commitTs] of this.commitTsToWallClock) {
      if (commitTs < horizonTs) {
        this.commitTsToWallClock.delete(commitTs);
      }
    }
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

    // Record wall clock time for this commit timestamp (used for GC retention)
    this.commitTsToWallClock.set(commitTs, Date.now());

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
    
    // Prune old entries if over limit to prevent unbounded growth
    if (committedWrites.size > MAX_COMMITTED_WRITES) {
      this.pruneCommittedWrites();
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
   * Prune old entries from committedWrites to prevent unbounded growth
   * Removes entries with commitTs older than minActiveTs (safe to remove)
   */
  private pruneCommittedWrites(): void {
    const minTs = this.minActiveTs;
    const entries = Array.from(this.committedWrites.entries());
    
    // Sort by commitTs (oldest first)
    entries.sort((a, b) => Number(a[1] - b[1]));
    
    // Remove oldest entries until we're under the prune threshold
    const targetSize = MAX_COMMITTED_WRITES - PRUNE_THRESHOLD_ENTRIES;
    let pruned = 0;
    
    for (const [key, commitTs] of entries) {
      if (this.committedWrites.size <= targetSize) {
        break;
      }
      
      // Only prune entries older than minActiveTs (safe - no active reader needs them)
      if (commitTs < minTs) {
        this.committedWrites.delete(key);
        pruned++;
      } else {
        // Once we hit entries >= minTs, stop pruning (they might be needed)
        break;
      }
    }
    
    this.totalPruned += pruned;
  }

  /**
   * Get statistics about committed writes
   */
  getCommittedWritesStats(): { size: number; pruned: number } {
    return {
      size: this.committedWrites.size,
      pruned: this.totalPruned,
    };
  }

  /**
   * Clear all transactions (for testing/recovery)
   */
  clear(): void {
    this.activeTxs.clear();
    this.committedWrites.clear();
    this.activeCount = 0;
    this.totalPruned = 0;
  }
}

