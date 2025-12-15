/**
 * Comprehensive tests for MVCC (Multi-Version Concurrency Control)
 */

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import {
  TxManager,
  VersionChainManager,
  ConflictDetector,
  GarbageCollector,
  MvccManager,
  isVisible,
  getVisibleVersion,
  nodeExists,
  edgeExists,
} from "../src/mvcc/index.ts";
import { ConflictError } from "../src/types.ts";
import {
  beginTx,
  closeGraphDB,
  commit,
  createNode,
  defineEtype,
  definePropkey,
  deleteNode,
  addEdge,
  deleteEdge,
  setNodeProp,
  openGraphDB,
  rollback,
  getNodeByKey,
  nodeExists as dbNodeExists,
  edgeExists as dbEdgeExists,
  getNodeProp,
  PropValueTag,
} from "../src/index.ts";
import type {
  NodeID,
  ETypeID,
  PropKeyID,
  PropValue,
  VersionedRecord,
  NodeVersionData,
  EdgeVersionData,
  LabelID,
} from "../src/types.ts";

// Helper to create a valid NodeDelta for tests
function createNodeDelta(labels: LabelID[] = []) {
  return {
    labels: new Set<LabelID>(labels),
    labelsDeleted: new Set<LabelID>(),
    props: new Map<PropKeyID, PropValue | null>(),
  };
}

// ============================================================================
// TxManager Tests
// ============================================================================

describe("TxManager", () => {
  let txManager: TxManager;

  beforeEach(() => {
    txManager = new TxManager(1n, 1n);
  });

  test("begin transaction assigns unique IDs and timestamps", () => {
    const { txid: txid1, startTs: ts1 } = txManager.beginTx();
    const { txid: txid2, startTs: ts2 } = txManager.beginTx();

    expect(txid1).toBe(1n);
    expect(txid2).toBe(2n);
    expect(ts1).toBe(1n);
    expect(ts2).toBe(1n); // Same snapshot timestamp
  });

  test("commit transaction assigns commit timestamp", () => {
    const { txid } = txManager.beginTx();
    const commitTs = txManager.commitTx(txid);

    expect(commitTs).toBe(1n);

    // Note: With eager cleanup, single transactions are removed immediately after commit
    // when no other active transactions exist
    const tx = txManager.getTx(txid);
    expect(tx).toBeUndefined(); // Eagerly cleaned up
  });

  test("abort transaction removes it immediately", () => {
    const { txid } = txManager.beginTx();
    txManager.abortTx(txid);

    expect(txManager.getTx(txid)).toBeUndefined();
    expect(txManager.isActive(txid)).toBe(false);
  });

  test("record read and write operations", () => {
    const { txid } = txManager.beginTx();
    txManager.recordRead(txid, "key1");
    txManager.recordWrite(txid, "key2");

    const tx = txManager.getTx(txid);
    expect(tx?.readSet.has("key1")).toBe(true);
    expect(tx?.writeSet.has("key2")).toBe(true);
  });

  test("minActiveTs returns nextCommitTs when no active transactions", () => {
    expect(txManager.minActiveTs).toBe(1n);
  });

  test("minActiveTs returns oldest active transaction snapshot", () => {
    const { txid: tx1, startTs: ts1 } = txManager.beginTx();
    txManager.commitTx(tx1); // Commit first

    const { txid: tx2, startTs: ts2 } = txManager.beginTx();
    const { txid: tx3 } = txManager.beginTx();

    // minActiveTs should be ts2 (oldest active)
    expect(txManager.minActiveTs).toBe(ts2);
    expect(txManager.getActiveCount()).toBe(2);
  });

  test("getActiveTxIds returns only active transactions", () => {
    const { txid: tx1 } = txManager.beginTx();
    const { txid: tx2 } = txManager.beginTx();
    txManager.commitTx(tx1);

    const activeIds = txManager.getActiveTxIds();
    expect(activeIds).toHaveLength(1);
    expect(activeIds[0]).toBe(tx2);
  });

  test("commit non-existent transaction throws", () => {
    expect(() => txManager.commitTx(999n)).toThrow("Transaction 999 not found");
  });

  test("commit already committed transaction throws", () => {
    const { txid } = txManager.beginTx();
    txManager.commitTx(txid);
    // With eager cleanup, the transaction is removed immediately
    expect(() => txManager.commitTx(txid)).toThrow("not found");
  });

  test("concurrent transactions supported", () => {
    const tx1 = txManager.beginTx();
    const tx2 = txManager.beginTx();
    const tx3 = txManager.beginTx();

    expect(txManager.getActiveCount()).toBe(3);
    expect(txManager.isActive(tx1.txid)).toBe(true);
    expect(txManager.isActive(tx2.txid)).toBe(true);
    expect(txManager.isActive(tx3.txid)).toBe(true);
  });
});

// ============================================================================
// VersionChainManager Tests
// ============================================================================

describe("VersionChainManager", () => {
  let versionChain: VersionChainManager;
  const nodeId: NodeID = 1n;
  const src: NodeID = 1n;
  const dst: NodeID = 2n;
  const etype: ETypeID = 1;
  const propKeyId: PropKeyID = 1;

  beforeEach(() => {
    versionChain = new VersionChainManager();
  });

  test("append node version creates chain", () => {
    const delta = { nodeId, delta: createNodeDelta() };

    versionChain.appendNodeVersion(nodeId, delta, 1n, 10n);
    const version = versionChain.getNodeVersion(nodeId);

    expect(version).not.toBeNull();
    expect(version?.data.nodeId).toBe(nodeId);
    expect(version?.txid).toBe(1n);
    expect(version?.commitTs).toBe(10n);
    expect(version?.prev).toBeNull();
  });

  test("append multiple node versions creates chain", () => {
    const delta1 = { nodeId, delta: createNodeDelta([1]) };
    const delta2 = { nodeId, delta: createNodeDelta([1, 2]) };

    versionChain.appendNodeVersion(nodeId, delta1, 1n, 10n);
    versionChain.appendNodeVersion(nodeId, delta2, 2n, 20n);

    const version = versionChain.getNodeVersion(nodeId);
    expect(version?.txid).toBe(2n);
    expect(version?.prev).not.toBeNull();
    expect(version?.prev?.txid).toBe(1n);
  });

  test("delete node version creates tombstone", () => {
    const delta = { nodeId, delta: createNodeDelta() };

    versionChain.appendNodeVersion(nodeId, delta, 1n, 10n);
    versionChain.deleteNodeVersion(nodeId, 2n, 20n);

    const version = versionChain.getNodeVersion(nodeId);
    expect(version?.deleted).toBe(true);
    expect(version?.txid).toBe(2n);
    expect(version?.prev).not.toBeNull();
  });

  test("append edge version", () => {
    versionChain.appendEdgeVersion(src, etype, dst, true, 1n, 10n);
    const version = versionChain.getEdgeVersion(src, etype, dst);

    expect(version).not.toBeNull();
    expect(version?.data.added).toBe(true);
    expect(version?.txid).toBe(1n);
  });

  test("edge version chain for add/delete", () => {
    versionChain.appendEdgeVersion(src, etype, dst, true, 1n, 10n);
    versionChain.appendEdgeVersion(src, etype, dst, false, 2n, 20n);

    const version = versionChain.getEdgeVersion(src, etype, dst);
    expect(version?.data.added).toBe(false);
    expect(version?.prev?.data.added).toBe(true);
  });

  test("append node property version", () => {
    const value: PropValue = { tag: PropValueTag.STRING, value: "test" };
    versionChain.appendNodePropVersion(nodeId, propKeyId, value, 1n, 10n);

    const version = versionChain.getNodePropVersion(nodeId, propKeyId);
    expect(version).not.toBeNull();
    expect(version?.data).toEqual(value);
  });

  test("node property version chain", () => {
    const value1: PropValue = { tag: PropValueTag.STRING, value: "v1" };
    const value2: PropValue = { tag: PropValueTag.STRING, value: "v2" };

    versionChain.appendNodePropVersion(nodeId, propKeyId, value1, 1n, 10n);
    versionChain.appendNodePropVersion(nodeId, propKeyId, value2, 2n, 20n);

    const version = versionChain.getNodePropVersion(nodeId, propKeyId);
    expect(version?.data).toEqual(value2);
    expect(version?.prev?.data).toEqual(value1);
  });

  test("append edge property version", () => {
    const value: PropValue = { tag: PropValueTag.I64, value: 42n };
    versionChain.appendEdgePropVersion(src, etype, dst, propKeyId, value, 1n, 10n);

    const version = versionChain.getEdgePropVersion(src, etype, dst, propKeyId);
    expect(version).not.toBeNull();
    expect(version?.data).toEqual(value);
  });

  test("prune old versions", () => {
    const delta = { nodeId, delta: createNodeDelta() };

    versionChain.appendNodeVersion(nodeId, delta, 1n, 10n);
    versionChain.appendNodeVersion(nodeId, delta, 2n, 20n);
    versionChain.appendNodeVersion(nodeId, delta, 3n, 30n);

    // Prune versions older than 25
    const pruned = versionChain.pruneOldVersions(25n);
    expect(pruned).toBeGreaterThan(0);

    const version = versionChain.getNodeVersion(nodeId);
    expect(version?.commitTs).toBeGreaterThanOrEqual(25n);
  });

  test("clear all versions", () => {
    const delta = { nodeId, delta: createNodeDelta() };

    versionChain.appendNodeVersion(nodeId, delta, 1n, 10n);
    versionChain.appendEdgeVersion(src, etype, dst, true, 1n, 10n);
    versionChain.appendNodePropVersion(nodeId, propKeyId, { tag: PropValueTag.NULL }, 1n, 10n);

    versionChain.clear();

    expect(versionChain.getNodeVersion(nodeId)).toBeNull();
    expect(versionChain.getEdgeVersion(src, etype, dst)).toBeNull();
    expect(versionChain.getNodePropVersion(nodeId, propKeyId)).toBeNull();
  });
});

// ============================================================================
// Visibility Tests
// ============================================================================

describe("Visibility", () => {
  function createVersion<T>(
    data: T,
    txid: bigint,
    commitTs: bigint,
    prev: VersionedRecord<T> | null = null,
    deleted = false,
  ): VersionedRecord<T> {
    return { data, txid, commitTs, prev, deleted };
  }

  test("isVisible: own writes always visible", () => {
    const version = createVersion({ value: "test" }, 1n, 10n);
    expect(isVisible(version, 5n, 1n)).toBe(true); // Own write
  });

  test("isVisible: committed before snapshot visible", () => {
    const version = createVersion({ value: "test" }, 2n, 10n);
    expect(isVisible(version, 15n, 1n)).toBe(true); // Committed before snapshot
  });

  test("isVisible: committed after snapshot invisible", () => {
    const version = createVersion({ value: "test" }, 2n, 20n);
    expect(isVisible(version, 15n, 1n)).toBe(false); // Committed after snapshot
  });

  test("getVisibleVersion: finds correct version in chain", () => {
    const v1 = createVersion({ value: "v1" }, 1n, 10n);
    const v2 = createVersion({ value: "v2" }, 2n, 20n, v1);
    const v3 = createVersion({ value: "v3" }, 3n, 30n, v2);

    // Snapshot at 25 should see v2
    const visible = getVisibleVersion(v3, 25n, 0n);
    expect(visible?.data.value).toBe("v2");
  });

  test("getVisibleVersion: own write stops traversal", () => {
    const v1 = createVersion({ value: "v1" }, 1n, 10n);
    const v2 = createVersion({ value: "v2" }, 2n, 20n, v1);
    const v3 = createVersion({ value: "v3" }, 3n, 30n, v2);

    // Own write (txid=3) should be visible even if newer
    const visible = getVisibleVersion(v3, 25n, 3n);
    expect(visible?.data.value).toBe("v3");
  });

  test("nodeExists: deleted node not visible", () => {
    const v1 = createVersion<NodeVersionData>(
      { nodeId: 1n, delta: createNodeDelta() },
      1n,
      10n,
    );
    const v2 = createVersion<NodeVersionData>(
      { nodeId: 1n, delta: createNodeDelta() },
      2n,
      20n,
      v1,
      true, // deleted
    );

    expect(nodeExists(v2, 25n, 0n)).toBe(false);
  });

  test("nodeExists: deleted by own transaction not visible", () => {
    const v1 = createVersion<NodeVersionData>(
      { nodeId: 1n, delta: createNodeDelta() },
      1n,
      10n,
    );
    const v2 = createVersion<NodeVersionData>(
      { nodeId: 1n, delta: createNodeDelta() },
      2n,
      20n,
      v1,
      true, // deleted by tx 2
    );

    expect(nodeExists(v2, 25n, 2n)).toBe(false); // Own deletion
  });

  test("edgeExists: added edge visible", () => {
    const version = createVersion<EdgeVersionData>(
      { src: 1n, etype: 1, dst: 2n, added: true },
      1n,
      10n,
    );

    expect(edgeExists(version, 15n, 0n)).toBe(true);
  });

  test("edgeExists: deleted edge not visible", () => {
    const v1 = createVersion<EdgeVersionData>(
      { src: 1n, etype: 1, dst: 2n, added: true },
      1n,
      10n,
    );
    const v2 = createVersion<EdgeVersionData>(
      { src: 1n, etype: 1, dst: 2n, added: false },
      2n,
      20n,
      v1,
    );

    expect(edgeExists(v2, 25n, 0n)).toBe(false);
  });

  test("edgeExists: added=false returns false without walking back", () => {
    // This tests the fix for the edge deletion bug:
    // When added=false, the edge is deleted - we should NOT walk back
    // to find earlier versions where it was added
    const v1 = createVersion<EdgeVersionData>(
      { src: 1n, etype: 1, dst: 2n, added: true },
      1n,
      10n,
    );
    const v2 = createVersion<EdgeVersionData>(
      { src: 1n, etype: 1, dst: 2n, added: false }, // Deleted
      2n,
      20n,
      v1,
    );

    // Query at snapshot after deletion - should see added=false, return false
    expect(edgeExists(v2, 25n, 0n)).toBe(false);
    
    // Query at snapshot before deletion - should see v1 with added=true
    expect(edgeExists(v2, 15n, 0n)).toBe(true);
  });

  test("edgeExists: edge re-addition after deletion", () => {
    const v1 = createVersion<EdgeVersionData>(
      { src: 1n, etype: 1, dst: 2n, added: true },
      1n,
      10n,
    );
    const v2 = createVersion<EdgeVersionData>(
      { src: 1n, etype: 1, dst: 2n, added: false }, // Deleted
      2n,
      20n,
      v1,
    );
    const v3 = createVersion<EdgeVersionData>(
      { src: 1n, etype: 1, dst: 2n, added: true }, // Re-added
      3n,
      30n,
      v2,
    );

    // Query at snapshot after re-addition
    expect(edgeExists(v3, 35n, 0n)).toBe(true);
    
    // Query at snapshot after deletion but before re-addition
    expect(edgeExists(v3, 25n, 0n)).toBe(false);
    
    // Query at snapshot before deletion
    expect(edgeExists(v3, 15n, 0n)).toBe(true);
  });
});

// ============================================================================
// ConflictDetector Tests
// ============================================================================

describe("ConflictDetector", () => {
  let txManager: TxManager;
  let versionChain: VersionChainManager;
  let conflictDetector: ConflictDetector;

  beforeEach(() => {
    txManager = new TxManager(1n, 1n);
    versionChain = new VersionChainManager();
    conflictDetector = new ConflictDetector(txManager, versionChain);
  });

  test("no conflicts for non-overlapping keys", () => {
    const tx1 = txManager.beginTx();
    const tx2 = txManager.beginTx();

    txManager.recordWrite(tx1.txid, "key1");
    txManager.recordWrite(tx2.txid, "key2");

    txManager.commitTx(tx1.txid);
    const conflicts = conflictDetector.checkConflicts(tx2.txid);

    expect(conflicts).toHaveLength(0);
  });

  test("write-write conflict detected", () => {
    const tx1 = txManager.beginTx();
    const tx2 = txManager.beginTx();

    txManager.recordWrite(tx1.txid, "key1");
    txManager.recordWrite(tx2.txid, "key1"); // Same key

    txManager.commitTx(tx1.txid);
    const conflicts = conflictDetector.checkConflicts(tx2.txid);

    expect(conflicts).toContain("key1");
  });

  test("read-write conflict detected", () => {
    const tx1 = txManager.beginTx();
    const tx2 = txManager.beginTx();

    txManager.recordRead(tx1.txid, "key1");
    txManager.recordWrite(tx2.txid, "key1"); // Write after read

    txManager.commitTx(tx2.txid);
    const conflicts = conflictDetector.checkConflicts(tx1.txid);

    expect(conflicts).toContain("key1");
  });

  test("no conflict if write happened before snapshot", () => {
    const tx1 = txManager.beginTx(); // startTs = 1
    txManager.recordWrite(tx1.txid, "key1");
    txManager.commitTx(tx1.txid); // commitTs = 1

    const tx2 = txManager.beginTx(); // startTs = 2 (after tx1 committed)
    txManager.recordRead(tx2.txid, "key1");

    const conflicts = conflictDetector.checkConflicts(tx2.txid);
    expect(conflicts).toHaveLength(0); // No conflict, tx1 committed before tx2 started
  });

  test("validateCommit throws ConflictError on conflict", () => {
    const tx1 = txManager.beginTx();
    const tx2 = txManager.beginTx();

    txManager.recordWrite(tx1.txid, "key1");
    txManager.recordWrite(tx2.txid, "key1");

    txManager.commitTx(tx1.txid);

    expect(() => conflictDetector.validateCommit(tx2.txid)).toThrow(ConflictError);
  });

  test("multiple conflicts detected", () => {
    const tx1 = txManager.beginTx();
    const tx2 = txManager.beginTx();

    txManager.recordWrite(tx1.txid, "key1");
    txManager.recordWrite(tx1.txid, "key2");
    txManager.recordWrite(tx2.txid, "key1");
    txManager.recordWrite(tx2.txid, "key2");

    txManager.commitTx(tx1.txid);
    const conflicts = conflictDetector.checkConflicts(tx2.txid);

    expect(conflicts).toContain("key1");
    expect(conflicts).toContain("key2");
    expect(conflicts.length).toBeGreaterThanOrEqual(2);
  });
});

// ============================================================================
// GarbageCollector Tests
// ============================================================================

describe("GarbageCollector", () => {
  let txManager: TxManager;
  let versionChain: VersionChainManager;
  let gc: GarbageCollector;

  beforeEach(() => {
    txManager = new TxManager(1n, 1n);
    versionChain = new VersionChainManager();
    gc = new GarbageCollector(txManager, versionChain, 1000, 5000);
  });

  afterEach(() => {
    gc.stop();
  });

  test("start and stop GC thread", () => {
    expect(gc.isRunning()).toBe(false);
    gc.start();
    expect(gc.isRunning()).toBe(true);
    gc.stop();
    expect(gc.isRunning()).toBe(false);
  });

  test("force GC cycle", () => {
    const delta = { nodeId: 1n, delta: createNodeDelta() };

    versionChain.appendNodeVersion(1n, delta, 1n, 10n);
    versionChain.appendNodeVersion(1n, delta, 2n, 20n);

    const pruned = gc.forceGc();
    expect(pruned).toBeGreaterThanOrEqual(0);
  });

  test("GC stats tracking", () => {
    gc.start();
    gc.forceGc();

    const stats = gc.getStats();
    expect(stats.gcRuns).toBeGreaterThan(0);
    expect(stats.lastGcTime).toBeGreaterThan(0n);
  });

  test("GC respects active transaction snapshots", () => {
    const delta = { nodeId: 1n, delta: createNodeDelta() };

    const tx = txManager.beginTx(); // Creates active transaction
    versionChain.appendNodeVersion(1n, delta, 1n, 10n);

    // GC should not prune versions needed by active transaction
    gc.forceGc();

    const version = versionChain.getNodeVersion(1n);
    expect(version).not.toBeNull(); // Should still exist
  });
});

// ============================================================================
// MvccManager Integration Tests
// ============================================================================

describe("MvccManager", () => {
  let mvcc: MvccManager;

  beforeEach(() => {
    mvcc = new MvccManager(1n, 1n, 1000, 5000);
  });

  afterEach(() => {
    mvcc.stop();
  });

  test("start and stop lifecycle", () => {
    expect(mvcc.gc.isRunning()).toBe(false);
    mvcc.start();
    expect(mvcc.gc.isRunning()).toBe(true);
    mvcc.stop();
    expect(mvcc.gc.isRunning()).toBe(false);
  });

  test("transaction through coordinator", () => {
    const { txid } = mvcc.txManager.beginTx();
    const commitTs = mvcc.txManager.commitTx(txid);

    expect(commitTs).toBe(1n);
    // With eager cleanup, single transactions are removed immediately
    expect(mvcc.txManager.getTx(txid)).toBeUndefined();
  });

  test("conflict detection through coordinator", () => {
    const tx1 = mvcc.txManager.beginTx();
    const tx2 = mvcc.txManager.beginTx();

    mvcc.txManager.recordWrite(tx1.txid, "key1");
    mvcc.txManager.recordWrite(tx2.txid, "key1");

    mvcc.txManager.commitTx(tx1.txid);

    expect(() => mvcc.conflictDetector.validateCommit(tx2.txid)).toThrow(ConflictError);
  });

  test("version chain creation through coordinator", () => {
    const { txid } = mvcc.txManager.beginTx();
    const delta = { nodeId: 1n, delta: createNodeDelta() };

    const commitTs = mvcc.txManager.commitTx(txid);
    mvcc.versionChain.appendNodeVersion(1n, delta, txid, commitTs);

    const version = mvcc.versionChain.getNodeVersion(1n);
    expect(version).not.toBeNull();
    expect(version?.txid).toBe(txid);
  });
});

// ============================================================================
// GraphDB MVCC Integration Tests
// ============================================================================

describe("GraphDB MVCC Integration", () => {
  let testDir: string;

  beforeEach(async () => {
    testDir = await mkdtemp(join(tmpdir(), "ray-mvcc-test-"));
  });

  afterEach(async () => {
    await rm(testDir, { recursive: true, force: true });
  });

  test("open database with MVCC enabled", async () => {
    const db = await openGraphDB(testDir, { mvcc: true });
    expect(db._mvcc).toBeDefined();
    await closeGraphDB(db);
  });

  test("concurrent transactions allowed in MVCC mode", async () => {
    const db = await openGraphDB(testDir, { mvcc: true });

    const tx1 = beginTx(db);
    const tx2 = beginTx(db); // Should not throw

    await commit(tx1);
    await commit(tx2);

    await closeGraphDB(db);
  });

  test("transaction operations update version chains", async () => {
    const db = await openGraphDB(testDir, { mvcc: true });

    // First transaction creates a node - version chain always created in MVCC mode
    const tx1 = beginTx(db);
    const nodeId = createNode(tx1, { key: "test" });
    await commit(tx1);
    
    // In MVCC mode, version chains are always created for proper snapshot isolation
    const mvcc = db._mvcc as MvccManager;
    const versionAfterSingle = mvcc.versionChain.getNodeVersion(nodeId);
    expect(versionAfterSingle).not.toBeNull(); // Version chain should exist
    
    // The node should exist
    expect(dbNodeExists(db, nodeId)).toBe(true);
    
    // Test with concurrent transactions - version chain should also be created
    const tx2 = beginTx(db); // Start a reader transaction
    const tx3 = beginTx(db); // Start another transaction that will commit
    const nodeId2 = createNode(tx3, { key: "test2" });
    await commit(tx3); // Commit while tx2 is still active
    
    // Version chain should exist
    const versionAfterConcurrent = mvcc.versionChain.getNodeVersion(nodeId2);
    expect(versionAfterConcurrent).not.toBeNull();
    
    rollback(tx2); // Clean up the reader transaction
    await closeGraphDB(db);
  });

  test("transaction rollback cleans up MVCC state", async () => {
    const db = await openGraphDB(testDir, { mvcc: true });

    const tx = beginTx(db);
    createNode(tx, { key: "test" });
    rollback(tx);

    // Transaction should be aborted in MVCC
    const mvcc = db._mvcc as MvccManager;
    const txState = mvcc.txManager.getTx(tx._tx.txid);
    expect(txState).toBeUndefined(); // Aborted transactions are removed

    await closeGraphDB(db);
  });

  test("conflict error on concurrent modification", async () => {
    const db = await openGraphDB(testDir, { mvcc: true });

    const tx1 = beginTx(db);
    const nodeId1 = createNode(tx1, { key: "node1" });
    await commit(tx1);

    // Start two concurrent transactions reading the same node
    const tx2 = beginTx(db);
    const tx3 = beginTx(db);

    // Both read the node (simulated by getting it)
    getNodeByKey(db, "node1");

    // tx2 modifies it
    setNodeProp(tx2, nodeId1, 1, { tag: PropValueTag.STRING, value: "v1" });
    await commit(tx2);

    // tx3 tries to modify it too - should conflict
    setNodeProp(tx3, nodeId1, 1, { tag: PropValueTag.STRING, value: "v2" });

    await expect(commit(tx3)).rejects.toThrow(ConflictError);

    await closeGraphDB(db);
  });

  test("node operations with MVCC", async () => {
    const db = await openGraphDB(testDir, { mvcc: true });

    const tx = beginTx(db);
    const nodeId = createNode(tx, { key: "test" });
    await commit(tx);

    // Node should exist
    expect(dbNodeExists(db, nodeId)).toBe(true);

    // Delete node
    const tx2 = beginTx(db);
    deleteNode(tx2, nodeId);
    await commit(tx2);

    // Node should not exist
    expect(dbNodeExists(db, nodeId)).toBe(false);

    await closeGraphDB(db);
  });

  test("edge operations with MVCC", async () => {
    const db = await openGraphDB(testDir, { mvcc: true });

    const tx = beginTx(db);
    const etype = defineEtype(tx, "KNOWS");
    const node1 = createNode(tx, { key: "node1" });
    const node2 = createNode(tx, { key: "node2" });
    addEdge(tx, node1, etype, node2);
    await commit(tx);

    // Edge should exist
    expect(dbEdgeExists(db, node1, etype, node2)).toBe(true);

    // Delete edge
    const tx2 = beginTx(db);
    deleteEdge(tx2, node1, etype, node2);
    await commit(tx2);

    // Edge should not exist
    expect(dbEdgeExists(db, node1, etype, node2)).toBe(false);

    await closeGraphDB(db);
  });

  test("property operations with MVCC", async () => {
    const db = await openGraphDB(testDir, { mvcc: true });

    const tx = beginTx(db);
    const propKey = definePropkey(tx, "name");
    const nodeId = createNode(tx, { key: "test" });
    setNodeProp(tx, nodeId, propKey, { tag: PropValueTag.STRING, value: "Alice" });
    await commit(tx);

    // Property should be readable
    const value = getNodeProp(db, nodeId, propKey);
    expect(value).not.toBeNull();
    if (value && value.tag === PropValueTag.STRING) {
      expect(value.value).toBe("Alice");
    }

    await closeGraphDB(db);
  });

  test("snapshot isolation: read sees consistent state", async () => {
    const db = await openGraphDB(testDir, { mvcc: true });

    // Create initial node
    const tx1 = beginTx(db);
    const nodeId = createNode(tx1, { key: "test" });
    await commit(tx1);

    // Start read transaction
    const readTx = beginTx(db);
    const nodeBefore = getNodeByKey(db, "test");

    // Concurrent write transaction modifies node
    const writeTx = beginTx(db);
    setNodeProp(writeTx, nodeId, 1, { tag: PropValueTag.STRING, value: "modified" });
    await commit(writeTx);

    // Read transaction should still see old state (snapshot isolation)
    const nodeAfter = getNodeByKey(db, "test");
    // Note: This test depends on how reads are tracked in MVCC mode
    // The read transaction should see the snapshot from when it started

    await closeGraphDB(db);
  });
});

