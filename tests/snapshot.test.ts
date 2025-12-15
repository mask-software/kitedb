/**
 * Snapshot writer and reader tests
 */

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { checkSnapshot } from "../src/check/checker.ts";
import {
  getInEdges,
  getNodeId,
  getNodeKey,
  getOutEdges,
  getPhysNode,
  getString,
  hasNode,
  loadSnapshot,
  lookupByKey,
  parseSnapshot,
} from "../src/core/snapshot-reader.ts";
import {
  type SnapshotBuildInput,
  buildSnapshot,
} from "../src/core/snapshot-writer.ts";
import { PropValueTag } from "../src/types.ts";

describe("Snapshot", () => {
  let testDir: string;

  beforeEach(async () => {
    testDir = await mkdtemp(join(tmpdir(), "ray-test-"));
  });

  afterEach(async () => {
    await rm(testDir, { recursive: true, force: true });
  });

  test("build and load empty snapshot", async () => {
    const input: SnapshotBuildInput = {
      generation: 1n,
      nodes: [],
      edges: [],
      labels: new Map(),
      etypes: new Map(),
      propkeys: new Map(),
    };

    await buildSnapshot(testDir, input);
    const snapshot = await loadSnapshot(testDir, 1n);

    expect(snapshot.header.generation).toBe(1n);
    expect(snapshot.header.numNodes).toBe(0n);
    expect(snapshot.header.numEdges).toBe(0n);

    const result = checkSnapshot(snapshot);
    expect(result.valid).toBe(true);
    expect(result.errors).toHaveLength(0);
  });

  test("build and load snapshot with nodes", async () => {
    const input: SnapshotBuildInput = {
      generation: 1n,
      nodes: [
        { nodeId: 1n, labels: [], props: new Map() },
        { nodeId: 2n, key: "user:alice", labels: [], props: new Map() },
        { nodeId: 5n, key: "user:bob", labels: [], props: new Map() },
      ],
      edges: [],
      labels: new Map(),
      etypes: new Map(),
      propkeys: new Map(),
    };

    await buildSnapshot(testDir, input);
    const snapshot = await loadSnapshot(testDir, 1n);

    expect(snapshot.header.numNodes).toBe(3n);
    expect(snapshot.header.maxNodeId).toBe(5n);

    // Check mappings
    expect(hasNode(snapshot, 1n)).toBe(true);
    expect(hasNode(snapshot, 2n)).toBe(true);
    expect(hasNode(snapshot, 5n)).toBe(true);
    expect(hasNode(snapshot, 3n)).toBe(false);
    expect(hasNode(snapshot, 100n)).toBe(false);

    // Check keys
    expect(lookupByKey(snapshot, "user:alice")).toBe(2n);
    expect(lookupByKey(snapshot, "user:bob")).toBe(5n);
    expect(lookupByKey(snapshot, "user:charlie")).toBe(null);

    const result = checkSnapshot(snapshot);
    expect(result.valid).toBe(true);
  });

  test("build and load snapshot with edges", async () => {
    const input: SnapshotBuildInput = {
      generation: 1n,
      nodes: [
        { nodeId: 1n, labels: [], props: new Map() },
        { nodeId: 2n, labels: [], props: new Map() },
        { nodeId: 3n, labels: [], props: new Map() },
      ],
      edges: [
        { src: 1n, etype: 1, dst: 2n, props: new Map() },
        { src: 1n, etype: 1, dst: 3n, props: new Map() },
        { src: 2n, etype: 2, dst: 3n, props: new Map() },
      ],
      labels: new Map(),
      etypes: new Map([
        [1, "knows"],
        [2, "follows"],
      ]),
      propkeys: new Map(),
    };

    await buildSnapshot(testDir, input);
    const snapshot = await loadSnapshot(testDir, 1n);

    expect(snapshot.header.numNodes).toBe(3n);
    expect(snapshot.header.numEdges).toBe(3n);

    // Check out-edges
    const phys1 = getPhysNode(snapshot, 1n);
    const outEdges1 = getOutEdges(snapshot, phys1);
    expect(outEdges1).toHaveLength(2);

    const phys2 = getPhysNode(snapshot, 2n);
    const outEdges2 = getOutEdges(snapshot, phys2);
    expect(outEdges2).toHaveLength(1);

    // Check in-edges
    const phys3 = getPhysNode(snapshot, 3n);
    const inEdges3 = getInEdges(snapshot, phys3);
    expect(inEdges3).toHaveLength(2);

    // Validate reciprocity
    const result = checkSnapshot(snapshot);
    expect(result.valid).toBe(true);
    expect(result.errors).toHaveLength(0);
  });

  test("edge sorting within node", async () => {
    const input: SnapshotBuildInput = {
      generation: 1n,
      nodes: [
        { nodeId: 1n, labels: [], props: new Map() },
        { nodeId: 2n, labels: [], props: new Map() },
        { nodeId: 3n, labels: [], props: new Map() },
        { nodeId: 4n, labels: [], props: new Map() },
      ],
      edges: [
        // Deliberately out of order
        { src: 1n, etype: 2, dst: 4n, props: new Map() },
        { src: 1n, etype: 1, dst: 3n, props: new Map() },
        { src: 1n, etype: 2, dst: 2n, props: new Map() },
        { src: 1n, etype: 1, dst: 2n, props: new Map() },
      ],
      labels: new Map(),
      etypes: new Map([
        [1, "a"],
        [2, "b"],
      ]),
      propkeys: new Map(),
    };

    await buildSnapshot(testDir, input);
    const snapshot = await loadSnapshot(testDir, 1n);

    // Edges should be sorted by (etype, dst)
    const phys1 = getPhysNode(snapshot, 1n);
    const outEdges = getOutEdges(snapshot, phys1);

    expect(outEdges).toHaveLength(4);

    // Verify sorting
    for (let i = 1; i < outEdges.length; i++) {
      const prev = outEdges[i - 1]!;
      const curr = outEdges[i]!;

      const cmp =
        prev.etype < curr.etype
          ? -1
          : prev.etype > curr.etype
            ? 1
            : prev.dst < curr.dst
              ? -1
              : prev.dst > curr.dst
                ? 1
                : 0;

      expect(cmp).toBeLessThanOrEqual(0);
    }

    const result = checkSnapshot(snapshot);
    expect(result.valid).toBe(true);
  });

  test("key index collision handling", async () => {
    // Create nodes with potentially colliding keys
    const nodes = [];
    for (let i = 1; i <= 100; i++) {
      nodes.push({
        nodeId: BigInt(i),
        key: `key${i}`,
        labels: [],
        props: new Map(),
      });
    }

    const input: SnapshotBuildInput = {
      generation: 1n,
      nodes,
      edges: [],
      labels: new Map(),
      etypes: new Map(),
      propkeys: new Map(),
    };

    await buildSnapshot(testDir, input);
    const snapshot = await loadSnapshot(testDir, 1n);

    // All keys should be findable
    for (let i = 1; i <= 100; i++) {
      expect(lookupByKey(snapshot, `key${i}`)).toBe(BigInt(i));
    }

    const result = checkSnapshot(snapshot);
    expect(result.valid).toBe(true);
  });

  test("string table interning", async () => {
    const input: SnapshotBuildInput = {
      generation: 1n,
      nodes: [
        { nodeId: 1n, key: "shared_key", labels: [], props: new Map() },
        { nodeId: 2n, key: "shared_key2", labels: [], props: new Map() },
      ],
      edges: [],
      labels: new Map([
        [1, "Person"],
        [2, "Person"],
      ]), // Duplicate label name
      etypes: new Map([
        [1, "knows"],
        [2, "knows"],
      ]), // Duplicate etype name
      propkeys: new Map(),
    };

    await buildSnapshot(testDir, input);
    const snapshot = await loadSnapshot(testDir, 1n);

    // Verify string table works
    const phys1 = getPhysNode(snapshot, 1n);
    expect(getNodeKey(snapshot, phys1)).toBe("shared_key");

    const result = checkSnapshot(snapshot);
    expect(result.valid).toBe(true);
  });

  test("node properties in snapshot", async () => {
    const nameId = 1;
    const ageId = 2;

    const input: SnapshotBuildInput = {
      generation: 1n,
      nodes: [
        {
          nodeId: 1n,
          labels: [],
          props: new Map([
            [nameId, { tag: PropValueTag.STRING, value: "Alice" }],
            [ageId, { tag: PropValueTag.I64, value: 30n }],
          ]),
        },
        {
          nodeId: 2n,
          labels: [],
          props: new Map([
            [nameId, { tag: PropValueTag.STRING, value: "Bob" }],
          ]),
        },
        {
          nodeId: 3n,
          labels: [],
          props: new Map(), // no props
        },
      ],
      edges: [],
      labels: new Map(),
      etypes: new Map(),
      propkeys: new Map([
        [nameId, "name"],
        [ageId, "age"],
      ]),
    };

    await buildSnapshot(testDir, input);
    const snapshot = await loadSnapshot(testDir, 1n);

    // Import getNodeProps and getNodeProp
    const { getNodeProps, getNodeProp } = await import(
      "../src/core/snapshot-reader.ts"
    );

    // Check node 1 properties
    const phys1 = getPhysNode(snapshot, 1n);
    const node1Props = getNodeProps(snapshot, phys1);
    expect(node1Props).not.toBeNull();
    expect(node1Props?.size).toBe(2);

    const nameProp = getNodeProp(snapshot, phys1, nameId);
    expect(nameProp?.tag).toBe(PropValueTag.STRING);
    expect((nameProp as { tag: 4; value: string }).value).toBe("Alice");

    // Check node 2 properties
    const phys2 = getPhysNode(snapshot, 2n);
    const node2Props = getNodeProps(snapshot, phys2);
    expect(node2Props?.size).toBe(1);

    // Check node 3 has no properties
    const phys3 = getPhysNode(snapshot, 3n);
    const node3Props = getNodeProps(snapshot, phys3);
    expect(node3Props?.size).toBe(0);

    const result = checkSnapshot(snapshot);
    expect(result.valid).toBe(true);
  });

  test("key buckets are created", async () => {
    // Create enough nodes to ensure buckets are built
    const nodes = [];
    for (let i = 1; i <= 50; i++) {
      nodes.push({
        nodeId: BigInt(i),
        key: `node-${i}`,
        labels: [],
        props: new Map(),
      });
    }

    const input: SnapshotBuildInput = {
      generation: 1n,
      nodes,
      edges: [],
      labels: new Map(),
      etypes: new Map(),
      propkeys: new Map(),
    };

    await buildSnapshot(testDir, input);
    const snapshot = await loadSnapshot(testDir, 1n);

    // Verify key buckets section exists and has data
    expect(snapshot.keyBuckets).not.toBeNull();
    expect(snapshot.keyBuckets!.byteLength).toBeGreaterThan(4);

    // Verify all lookups work
    for (let i = 1; i <= 50; i++) {
      expect(lookupByKey(snapshot, `node-${i}`)).toBe(BigInt(i));
    }

    const result = checkSnapshot(snapshot);
    expect(result.valid).toBe(true);
  });
});
