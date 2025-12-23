import { COMPACT_WAL_SIZE } from "../../constants.ts";
import { getDeltaStats } from "../../core/delta.ts";
import { checkSnapshot as checkSnapshotFn } from "../../check/checker.ts";
import type { CheckResult, DbStats, GraphDB } from "../../types.ts";
import { getMvccManager, isMvccEnabled } from "../../mvcc/index.ts";
import { getSnapshot } from "./snapshot-helper.ts";

/**
 * Get database statistics
 */
export function stats(db: GraphDB): DbStats {
  const snapshot = getSnapshot(db);
  const snapshotNodes = snapshot ? snapshot.header.numNodes : 0n;
  const snapshotEdges = snapshot ? snapshot.header.numEdges : 0n;
  const snapshotMaxNodeId = snapshot ? Number(snapshot.header.maxNodeId) : 0;

  const deltaStats = getDeltaStats(db._delta);

  // Single-file uses _walWritePos, multi-file uses _walOffset
  const walBytes = db._isSingleFile ? db._walWritePos : db._walOffset;
  
  const recommendCompact =
    BigInt(deltaStats.edgesAdded + deltaStats.edgesDeleted) >
      snapshotEdges / 10n ||
    BigInt(deltaStats.nodesCreated + deltaStats.nodesDeleted) >
      snapshotNodes / 10n ||
    walBytes > COMPACT_WAL_SIZE;

  // Single-file and multi-file have different fields
  const snapshotGen = db._isSingleFile 
    ? (db._header?.activeSnapshotGen ?? 0n)
    : (db._manifest?.activeSnapshotGen ?? 0n);
  const walSegment = db._isSingleFile
    ? 0n  // Single-file doesn't have WAL segments
    : (db._manifest?.activeWalSeg ?? 0n);

  const result: DbStats = {
    snapshotGen,
    snapshotNodes,
    snapshotEdges,
    snapshotMaxNodeId,
    deltaNodesCreated: deltaStats.nodesCreated,
    deltaNodesDeleted: deltaStats.nodesDeleted,
    deltaEdgesAdded: deltaStats.edgesAdded,
    deltaEdgesDeleted: deltaStats.edgesDeleted,
    walSegment,
    walBytes: BigInt(walBytes),
    recommendCompact,
  };

  // Add MVCC stats if enabled
  if (isMvccEnabled(db)) {
    const mvcc = getMvccManager(db);
    if (mvcc) {
      const gcStats = mvcc.gc.getStats();
      result.mvccStats = {
        activeTransactions: mvcc.txManager.getActiveCount(),
        minActiveTs: mvcc.txManager.minActiveTs,
        versionsPruned: gcStats.versionsPruned,
        gcRuns: gcStats.gcRuns,
        lastGcTime: gcStats.lastGcTime,
      };
    }
  }

  return result;
}

/**
 * Check database integrity
 */
export function check(db: GraphDB): CheckResult {
  const snapshot = getSnapshot(db);
  if (!snapshot) {
    return { valid: true, errors: [], warnings: ["No snapshot to check"] };
  }

  return checkSnapshotFn(snapshot);
}

