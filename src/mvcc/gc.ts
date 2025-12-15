/**
 * MVCC Background Garbage Collection
 * 
 * Periodically prunes old versions that are no longer needed
 */

import { TxManager } from "./tx-manager.ts";
import { VersionChainManager } from "./version-chain.ts";

export interface GcStats {
  versionsPruned: bigint;
  gcRuns: number;
  lastGcTime: bigint;
}

export class GarbageCollector {
  private intervalId: ReturnType<typeof setInterval> | null = null;
  private stats: GcStats = {
    versionsPruned: 0n,
    gcRuns: 0,
    lastGcTime: 0n,
  };
  private running = false;

  constructor(
    private txManager: TxManager,
    private versionChain: VersionChainManager,
    private intervalMs: number = 5000,
    private retentionMs: number = 60000,
  ) {}

  /**
   * Start the GC thread
   */
  start(): void {
    if (this.running) {
      return;
    }

    this.running = true;
    this.intervalId = setInterval(() => {
      this.runGc();
    }, this.intervalMs);

    // Run immediately on start
    this.runGc();
  }

  /**
   * Stop the GC thread
   */
  stop(): void {
    if (!this.running) {
      return;
    }

    this.running = false;
    if (this.intervalId) {
      clearInterval(this.intervalId);
      this.intervalId = null;
    }
  }

  /**
   * Run a single GC cycle
   */
  private runGc(): void {
    try {
      // Calculate GC horizon
      // Versions older than this can be pruned if they have newer successors
      const now = BigInt(Date.now());
      const minActiveTs = this.txManager.minActiveTs;
      const retentionTs = now - BigInt(this.retentionMs);
      
      // GC horizon is the minimum of:
      // 1. Oldest active transaction snapshot (can't prune versions needed by active reads)
      // 2. Retention period (keep versions for at least retentionMs)
      const horizonTs = minActiveTs < retentionTs ? minActiveTs : retentionTs;

      // Prune old versions
      const pruned = this.versionChain.pruneOldVersions(horizonTs);

      // Clean up old committed transactions
      // This ensures activeTxs doesn't grow unboundedly in concurrent workloads
      this.cleanupOldTransactions(horizonTs);

      // Update stats
      this.stats.versionsPruned += BigInt(pruned);
      this.stats.gcRuns++;
      this.stats.lastGcTime = BigInt(Date.now());
    } catch (error) {
      // Log error but don't crash
      console.error("GC error:", error);
    }
  }

  /**
   * Clean up committed transactions that are older than the horizon
   * These transactions are no longer needed for visibility calculations
   */
  private cleanupOldTransactions(horizonTs: bigint): void {
    const txsToRemove: bigint[] = [];
    
    for (const [txid, tx] of this.txManager.getAllTxs()) {
      // Only remove committed transactions older than horizon
      if (tx.status === 'committed' && tx.commitTs !== null && tx.commitTs < horizonTs) {
        txsToRemove.push(txid);
      }
    }
    
    // Remove in a separate loop to avoid iterator invalidation
    for (const txid of txsToRemove) {
      this.txManager.removeTx(txid);
    }
  }

  /**
   * Force a GC run (for testing/manual triggers)
   */
  forceGc(): number {
    this.runGc();
    return Number(this.stats.versionsPruned);
  }

  /**
   * Get GC statistics
   */
  getStats(): GcStats {
    return { ...this.stats };
  }

  /**
   * Check if GC is running
   */
  isRunning(): boolean {
    return this.running;
  }
}

