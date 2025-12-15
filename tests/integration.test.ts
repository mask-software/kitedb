/**
 * Integration tests for the full database
 */

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import {
  PropValueTag,
  addEdge,
  beginTx,
  check,
  closeGraphDB,
  commit,
  createNode,
  defineEtype,
  definePropkey,
  delNodeProp,
  deleteEdge,
  deleteNode,
  edgeExists,
  getEdgeProp,
  getEdgeProps,
  getNeighborsIn,
  getNeighborsOut,
  getNodeByKey,
  getNodeProp,
  getNodeProps,
  nodeExists,
  openGraphDB,
  optimize,
  rollback,
  setEdgeProp,
  setNodeProp,
  stats,
} from "../src/index.ts";

describe("Database Lifecycle", () => {
  let testDir: string;

  beforeEach(async () => {
    testDir = await mkdtemp(join(tmpdir(), "ray-int-test-"));
  });

  afterEach(async () => {
    await rm(testDir, { recursive: true, force: true });
  });

  test("create new database", async () => {
    const db = await openGraphDB(testDir);

    const s = stats(db);
    expect(s.snapshotGen).toBe(0n);
    expect(s.snapshotNodes).toBe(0n);

    await closeGraphDB(db);
  });

  test("reopen existing database", async () => {
    // Create and populate
    const db1 = await openGraphDB(testDir);

    const tx = beginTx(db1);
    const n1 = createNode(tx, { key: "node1" });
    await commit(tx);

    await closeGraphDB(db1);

    // Reopen
    const db2 = await openGraphDB(testDir);

    // Node should still exist via WAL recovery
    expect(getNodeByKey(db2, "node1")).toBe(n1);

    await closeGraphDB(db2);
  });

  test("read-only mode", async () => {
    // Create database
    const db1 = await openGraphDB(testDir);
    const tx = beginTx(db1);
    createNode(tx, { key: "test" });
    await commit(tx);
    await closeGraphDB(db1);

    // Open read-only
    const db2 = await openGraphDB(testDir, { readOnly: true });

    expect(getNodeByKey(db2, "test")).not.toBeNull();
    expect(() => beginTx(db2)).toThrow();

    await closeGraphDB(db2);
  });
});

describe("Node Operations", () => {
  let testDir: string;

  beforeEach(async () => {
    testDir = await mkdtemp(join(tmpdir(), "ray-node-test-"));
  });

  afterEach(async () => {
    await rm(testDir, { recursive: true, force: true });
  });

  test("create nodes with keys", async () => {
    const db = await openGraphDB(testDir);

    const tx = beginTx(db);
    const n1 = createNode(tx, { key: "user:alice" });
    const n2 = createNode(tx, { key: "user:bob" });
    const n3 = createNode(tx); // No key
    await commit(tx);

    expect(getNodeByKey(db, "user:alice")).toBe(n1);
    expect(getNodeByKey(db, "user:bob")).toBe(n2);
    expect(getNodeByKey(db, "user:charlie")).toBeNull();

    expect(nodeExists(db, n1)).toBe(true);
    expect(nodeExists(db, n3)).toBe(true);
    expect(nodeExists(db, 999n)).toBe(false);

    await closeGraphDB(db);
  });

  test("delete node", async () => {
    const db = await openGraphDB(testDir);

    const tx1 = beginTx(db);
    const n1 = createNode(tx1, { key: "to-delete" });
    await commit(tx1);

    expect(nodeExists(db, n1)).toBe(true);

    const tx2 = beginTx(db);
    deleteNode(tx2, n1);
    await commit(tx2);

    expect(nodeExists(db, n1)).toBe(false);
    expect(getNodeByKey(db, "to-delete")).toBeNull();

    await closeGraphDB(db);
  });

  test("rollback node creation", async () => {
    const db = await openGraphDB(testDir);

    const tx = beginTx(db);
    const n1 = createNode(tx, { key: "temp" });
    rollback(tx);

    expect(nodeExists(db, n1)).toBe(false);
    expect(getNodeByKey(db, "temp")).toBeNull();

    await closeGraphDB(db);
  });
});

describe("Edge Operations", () => {
  let testDir: string;

  beforeEach(async () => {
    testDir = await mkdtemp(join(tmpdir(), "ray-edge-test-"));
  });

  afterEach(async () => {
    await rm(testDir, { recursive: true, force: true });
  });

  test("create and traverse edges", async () => {
    const db = await openGraphDB(testDir);

    const tx = beginTx(db);
    const knows = defineEtype(tx, "knows");
    const follows = defineEtype(tx, "follows");

    const alice = createNode(tx, { key: "alice" });
    const bob = createNode(tx, { key: "bob" });
    const charlie = createNode(tx, { key: "charlie" });

    addEdge(tx, alice, knows, bob);
    addEdge(tx, alice, knows, charlie);
    addEdge(tx, bob, follows, alice);

    await commit(tx);

    // Check edge existence
    expect(edgeExists(db, alice, knows, bob)).toBe(true);
    expect(edgeExists(db, alice, knows, charlie)).toBe(true);
    expect(edgeExists(db, bob, follows, alice)).toBe(true);
    expect(edgeExists(db, alice, follows, bob)).toBe(false);

    // Traverse out-edges
    const aliceKnows = [...getNeighborsOut(db, alice, knows)];
    expect(aliceKnows).toHaveLength(2);

    const aliceAll = [...getNeighborsOut(db, alice)];
    expect(aliceAll).toHaveLength(2);

    // Traverse in-edges
    const bobIncoming = [...getNeighborsIn(db, bob)];
    expect(bobIncoming).toHaveLength(1);
    expect(bobIncoming[0]!.src).toBe(alice);

    await closeGraphDB(db);
  });

  test("delete edge", async () => {
    const db = await openGraphDB(testDir);

    const tx1 = beginTx(db);
    const knows = defineEtype(tx1, "knows");
    const a = createNode(tx1);
    const b = createNode(tx1);
    addEdge(tx1, a, knows, b);
    await commit(tx1);

    expect(edgeExists(db, a, knows, b)).toBe(true);

    const tx2 = beginTx(db);
    deleteEdge(tx2, a, knows, b);
    await commit(tx2);

    expect(edgeExists(db, a, knows, b)).toBe(false);

    await closeGraphDB(db);
  });

  test("edges to deleted nodes are invisible", async () => {
    const db = await openGraphDB(testDir);

    const tx1 = beginTx(db);
    const knows = defineEtype(tx1, "knows");
    const a = createNode(tx1);
    const b = createNode(tx1);
    addEdge(tx1, a, knows, b);
    await commit(tx1);

    const tx2 = beginTx(db);
    deleteNode(tx2, b);
    await commit(tx2);

    // Edge should no longer be visible
    expect(edgeExists(db, a, knows, b)).toBe(false);

    const outEdges = [...getNeighborsOut(db, a)];
    expect(outEdges).toHaveLength(0);

    await closeGraphDB(db);
  });
});

describe("Properties", () => {
  let testDir: string;

  beforeEach(async () => {
    testDir = await mkdtemp(join(tmpdir(), "ray-prop-test-"));
  });

  afterEach(async () => {
    await rm(testDir, { recursive: true, force: true });
  });

  test("set and get node properties", async () => {
    const db = await openGraphDB(testDir);

    const tx = beginTx(db);
    const nameProp = definePropkey(tx, "name");
    const ageProp = definePropkey(tx, "age");

    const n = createNode(tx);
    setNodeProp(tx, n, nameProp, { tag: PropValueTag.STRING, value: "Alice" });
    setNodeProp(tx, n, ageProp, { tag: PropValueTag.I64, value: 30n });

    await commit(tx);

    // Verify properties can be read
    const namePropVal = getNodeProp(db, n, nameProp);
    expect(namePropVal).not.toBeNull();
    expect(namePropVal?.tag).toBe(PropValueTag.STRING);
    expect((namePropVal as { tag: 4; value: string }).value).toBe("Alice");

    const agePropVal = getNodeProp(db, n, ageProp);
    expect(agePropVal).not.toBeNull();
    expect(agePropVal?.tag).toBe(PropValueTag.I64);
    expect((agePropVal as { tag: 2; value: bigint }).value).toBe(30n);

    // Verify getNodeProps returns all properties
    const allProps = getNodeProps(db, n);
    expect(allProps).not.toBeNull();
    expect(allProps?.size).toBe(2);

    await closeGraphDB(db);
  });

  test("node properties persist across reopen", async () => {
    // Create and set properties
    let db = await openGraphDB(testDir);

    const tx = beginTx(db);
    const nameProp = definePropkey(tx, "name");
    const n = createNode(tx);
    setNodeProp(tx, n, nameProp, { tag: PropValueTag.STRING, value: "Bob" });
    await commit(tx);

    // Compact to write to snapshot
    await optimize(db);
    await closeGraphDB(db);

    // Reopen and verify
    db = await openGraphDB(testDir);
    const namePropVal = getNodeProp(db, n, nameProp);
    expect(namePropVal).not.toBeNull();
    expect((namePropVal as { tag: 4; value: string }).value).toBe("Bob");

    await closeGraphDB(db);
  });

  test("delete node property", async () => {
    const db = await openGraphDB(testDir);

    const tx1 = beginTx(db);
    const nameProp = definePropkey(tx1, "name");
    const n = createNode(tx1);
    setNodeProp(tx1, n, nameProp, { tag: PropValueTag.STRING, value: "Alice" });
    await commit(tx1);

    // Verify property exists
    expect(getNodeProp(db, n, nameProp)).not.toBeNull();

    // Delete property
    const tx2 = beginTx(db);
    delNodeProp(tx2, n, nameProp);
    await commit(tx2);

    // Verify property is gone
    expect(getNodeProp(db, n, nameProp)).toBeNull();

    await closeGraphDB(db);
  });

  test("set and get edge properties", async () => {
    const db = await openGraphDB(testDir);

    const tx = beginTx(db);
    const knows = defineEtype(tx, "knows");
    const weightProp = definePropkey(tx, "weight");
    const sinceProp = definePropkey(tx, "since");

    const a = createNode(tx);
    const b = createNode(tx);
    addEdge(tx, a, knows, b);
    setEdgeProp(tx, a, knows, b, weightProp, {
      tag: PropValueTag.F64,
      value: 0.75,
    });
    setEdgeProp(tx, a, knows, b, sinceProp, {
      tag: PropValueTag.I64,
      value: 2020n,
    });

    await commit(tx);

    // Verify edge properties
    const weightVal = getEdgeProp(db, a, knows, b, weightProp);
    expect(weightVal).not.toBeNull();
    expect(weightVal?.tag).toBe(PropValueTag.F64);
    expect((weightVal as { tag: 3; value: number }).value).toBeCloseTo(0.75);

    const sinceVal = getEdgeProp(db, a, knows, b, sinceProp);
    expect(sinceVal).not.toBeNull();
    expect((sinceVal as { tag: 2; value: bigint }).value).toBe(2020n);

    // Verify getEdgeProps returns all properties
    const allProps = getEdgeProps(db, a, knows, b);
    expect(allProps).not.toBeNull();
    expect(allProps?.size).toBe(2);

    await closeGraphDB(db);
  });

  test("edge properties persist through compaction", async () => {
    const db = await openGraphDB(testDir);

    const tx = beginTx(db);
    const knows = defineEtype(tx, "knows");
    const weightProp = definePropkey(tx, "weight");

    const a = createNode(tx);
    const b = createNode(tx);
    addEdge(tx, a, knows, b);
    setEdgeProp(tx, a, knows, b, weightProp, {
      tag: PropValueTag.F64,
      value: 0.5,
    });

    await commit(tx);

    // Compact
    await optimize(db);

    // Verify property still exists after compaction
    const weightVal = getEdgeProp(db, a, knows, b, weightProp);
    expect(weightVal).not.toBeNull();
    expect((weightVal as { tag: 3; value: number }).value).toBeCloseTo(0.5);

    await closeGraphDB(db);
  });
});

describe("Compaction", () => {
  let testDir: string;

  beforeEach(async () => {
    testDir = await mkdtemp(join(tmpdir(), "ray-compact-test-"));
  });

  afterEach(async () => {
    await rm(testDir, { recursive: true, force: true });
  });

  test("basic compaction", async () => {
    const db = await openGraphDB(testDir);

    // Create some data
    const tx = beginTx(db);
    const knows = defineEtype(tx, "knows");

    const nodes = [];
    for (let i = 0; i < 10; i++) {
      nodes.push(createNode(tx, { key: `node${i}` }));
    }

    for (let i = 0; i < 9; i++) {
      addEdge(tx, nodes[i]!, knows, nodes[i + 1]!);
    }

    await commit(tx);

    let s = stats(db);
    expect(s.snapshotGen).toBe(0n);
    expect(s.deltaNodesCreated).toBe(10n);

    // Compact
    await optimize(db);

    s = stats(db);
    expect(s.snapshotGen).toBe(1n);
    expect(s.snapshotNodes).toBe(10n);
    expect(s.snapshotEdges).toBe(9n);
    expect(s.deltaNodesCreated).toBe(0n);
    expect(s.deltaEdgesAdded).toBe(0n);

    // Data should still be accessible
    expect(getNodeByKey(db, "node0")).not.toBeNull();
    expect(getNodeByKey(db, "node5")).not.toBeNull();

    const n0 = getNodeByKey(db, "node0")!;
    const outEdges = [...getNeighborsOut(db, n0)];
    expect(outEdges).toHaveLength(1);

    // Check snapshot integrity
    const result = check(db);
    expect(result.valid).toBe(true);

    await closeGraphDB(db);
  });

  test("compaction preserves deletes", async () => {
    const db = await openGraphDB(testDir);

    // Create data
    const tx1 = beginTx(db);
    const knows = defineEtype(tx1, "knows");
    const n1 = createNode(tx1, { key: "keep" });
    const n2 = createNode(tx1, { key: "delete" });
    addEdge(tx1, n1, knows, n2);
    await commit(tx1);

    // Delete
    const tx2 = beginTx(db);
    deleteNode(tx2, n2);
    await commit(tx2);

    // Compact
    await optimize(db);

    // Deleted node should not be in snapshot
    const s = stats(db);
    expect(s.snapshotNodes).toBe(1n);
    expect(s.snapshotEdges).toBe(0n);

    expect(nodeExists(db, n1)).toBe(true);
    expect(nodeExists(db, n2)).toBe(false);

    await closeGraphDB(db);
  });

  test("multiple compactions", async () => {
    const db = await openGraphDB(testDir);

    for (let round = 0; round < 3; round++) {
      const tx = beginTx(db);
      if (round === 0) {
        defineEtype(tx, "link");
      }

      for (let i = 0; i < 5; i++) {
        createNode(tx, { key: `r${round}n${i}` });
      }
      await commit(tx);

      await optimize(db);

      const s = stats(db);
      expect(s.snapshotGen).toBe(BigInt(round + 1));
      expect(s.snapshotNodes).toBe(BigInt((round + 1) * 5));
    }

    // All data should be accessible
    for (let round = 0; round < 3; round++) {
      for (let i = 0; i < 5; i++) {
        expect(getNodeByKey(db, `r${round}n${i}`)).not.toBeNull();
      }
    }

    await closeGraphDB(db);
  });
});

describe("Recovery", () => {
  let testDir: string;

  beforeEach(async () => {
    testDir = await mkdtemp(join(tmpdir(), "ray-recovery-test-"));
  });

  afterEach(async () => {
    await rm(testDir, { recursive: true, force: true });
  });

  test("recover uncommitted transactions are ignored", async () => {
    const db1 = await openGraphDB(testDir);

    // Committed transaction
    const tx1 = beginTx(db1);
    const n1 = createNode(tx1, { key: "committed" });
    await commit(tx1);

    // Uncommitted transaction
    const tx2 = beginTx(db1);
    const n2 = createNode(tx2, { key: "uncommitted" });
    // Don't commit!

    await closeGraphDB(db1);

    // Reopen
    const db2 = await openGraphDB(testDir);

    expect(getNodeByKey(db2, "committed")).toBe(n1);
    expect(getNodeByKey(db2, "uncommitted")).toBeNull();

    await closeGraphDB(db2);
  });

  test("recovery after compaction", async () => {
    const db1 = await openGraphDB(testDir);

    const tx1 = beginTx(db1);
    const knows = defineEtype(tx1, "knows");
    const n1 = createNode(tx1, { key: "before-compact" });
    const n2 = createNode(tx1, { key: "before-compact-2" });
    addEdge(tx1, n1, knows, n2);
    await commit(tx1);

    await optimize(db1);

    const tx2 = beginTx(db1);
    const n3 = createNode(tx2, { key: "after-compact" });
    addEdge(tx2, n2, knows, n3);
    await commit(tx2);

    await closeGraphDB(db1);

    // Reopen - should recover WAL after snapshot
    const db2 = await openGraphDB(testDir);

    expect(getNodeByKey(db2, "before-compact")).toBe(n1);
    expect(getNodeByKey(db2, "after-compact")).toBe(n3);
    expect(edgeExists(db2, n2, knows, n3)).toBe(true);

    await closeGraphDB(db2);
  });
});
