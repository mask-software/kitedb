/**
 * MVCC Module
 * 
 * Multi-Version Concurrency Control for snapshot isolation
 */

export { TxManager } from "./tx-manager.ts";
export { VersionChainManager } from "./version-chain.ts";
export { ConflictDetector } from "./conflict-detector.ts";
export { GarbageCollector, type GcStats } from "./gc.ts";
export * from "./visibility.ts";

import { TxManager } from "./tx-manager.ts";
import { VersionChainManager } from "./version-chain.ts";
import { ConflictDetector } from "./conflict-detector.ts";
import { GarbageCollector } from "./gc.ts";
import type { GraphDB, OpenOptions } from "../types.ts";

/**
 * MVCC Manager - coordinates all MVCC components
 */
export class MvccManager {
  public readonly txManager: TxManager;
  public readonly versionChain: VersionChainManager;
  public readonly conflictDetector: ConflictDetector;
  public readonly gc: GarbageCollector;

  constructor(
    initialTxId: bigint = 1n,
    initialCommitTs: bigint = 1n,
    gcIntervalMs: number = 5000,
    retentionMs: number = 60000,
  ) {
    this.txManager = new TxManager(initialTxId, initialCommitTs);
    this.versionChain = new VersionChainManager();
    this.conflictDetector = new ConflictDetector(
      this.txManager,
      this.versionChain,
    );
    this.gc = new GarbageCollector(
      this.txManager,
      this.versionChain,
      gcIntervalMs,
      retentionMs,
    );
  }

  /**
   * Initialize MVCC (start GC thread)
   */
  start(): void {
    this.gc.start();
  }

  /**
   * Shutdown MVCC (stop GC thread)
   */
  stop(): void {
    this.gc.stop();
  }
}

/**
 * Get or create MVCC manager for a database
 */
export function getMvccManager(db: GraphDB): MvccManager | null {
  return (db._mvcc as MvccManager | undefined) || null;
}

/**
 * Check if MVCC is enabled for a database
 * Uses cached flag for fast checks
 */
export function isMvccEnabled(db: GraphDB): boolean {
  // Use cached flag if available (set at open time)
  if (db._mvccEnabled !== undefined) {
    return db._mvccEnabled;
  }
  // Fallback for backwards compatibility
  return db._mvcc !== undefined && db._mvcc !== null;
}

