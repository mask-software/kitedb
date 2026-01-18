/**
 * Delta overlay tests
 */

import { describe, expect, test } from "bun:test";

import {
  addEdge,
  clearDelta,
  createDelta,
  createNode,
  defineEtype,
  defineLabel,
  definePropkey,
  deleteEdge,
  deleteNode,
  deleteNodeProp,
  getDeltaStats,
  getNodeDelta,
  isEdgeAdded,
  isEdgeDeleted,
  isNodeCreated,
  isNodeDeleted,
  setNodeProp,
} from "../src/core/delta.ts";
import { PropValueTag } from "../src/types.ts";

describe("Delta Node Operations", () => {
  test("create node", () => {
    const delta = createDelta();

    createNode(delta, 1, "user:alice");

    expect(isNodeCreated(delta, 1)).toBe(true);
    expect(isNodeCreated(delta, 2)).toBe(false);

    const nodeDelta = getNodeDelta(delta, 1);
    expect(nodeDelta).not.toBeNull();
    expect(nodeDelta!.key).toBe("user:alice");
  });

  test("delete created node", () => {
    const delta = createDelta();

    createNode(delta, 1, "user:alice");
    expect(isNodeCreated(delta, 1)).toBe(true);

    deleteNode(delta, 1);
    expect(isNodeCreated(delta, 1)).toBe(false);
    expect(isNodeDeleted(delta, 1)).toBe(false); // Was never committed
  });

  test("delete existing node", () => {
    const delta = createDelta();

    // Simulate deleting a node that exists in snapshot
    deleteNode(delta, 100);

    expect(isNodeDeleted(delta, 100)).toBe(true);
  });

  test("node properties", () => {
    const delta = createDelta();

    createNode(delta, 1);

    setNodeProp(
      delta,
      1,
      1,
      { tag: PropValueTag.STRING, value: "Alice" },
      true,
    );
    setNodeProp(delta, 1, 2, { tag: PropValueTag.I64, value: 30n }, true);

    const nodeDelta = getNodeDelta(delta, 1);
    expect(nodeDelta?.props?.get(1)).toEqual({
      tag: PropValueTag.STRING,
      value: "Alice",
    });
    expect(nodeDelta?.props?.get(2)).toEqual({
      tag: PropValueTag.I64,
      value: 30n,
    });

    deleteNodeProp(delta, 1, 1, true);
    expect(nodeDelta?.props?.get(1)).toBeNull();
  });
});

describe("Delta Edge Operations", () => {
  test("add edge", () => {
    const delta = createDelta();

    addEdge(delta, 1, 1, 2);

    expect(isEdgeAdded(delta, 1, 1, 2)).toBe(true);
    expect(isEdgeAdded(delta, 1, 1, 3)).toBe(false);
  });

  test("delete edge cancels add", () => {
    const delta = createDelta();

    addEdge(delta, 1, 1, 2);
    expect(isEdgeAdded(delta, 1, 1, 2)).toBe(true);

    deleteEdge(delta, 1, 1, 2);
    expect(isEdgeAdded(delta, 1, 1, 2)).toBe(false);
    expect(isEdgeDeleted(delta, 1, 1, 2)).toBe(false); // Cancelled, not deleted
  });

  test("add edge cancels delete", () => {
    const delta = createDelta();

    // Simulate deleting an edge from snapshot
    deleteEdge(delta, 1, 1, 2);
    expect(isEdgeDeleted(delta, 1, 1, 2)).toBe(true);

    // Re-add the edge
    addEdge(delta, 1, 1, 2);
    expect(isEdgeDeleted(delta, 1, 1, 2)).toBe(false); // Cancelled
    expect(isEdgeAdded(delta, 1, 1, 2)).toBe(false); // Not added, just restored
  });

  test("multiple edges from same node", () => {
    const delta = createDelta();

    addEdge(delta, 1, 1, 2);
    addEdge(delta, 1, 1, 3);
    addEdge(delta, 1, 2, 2);

    expect(isEdgeAdded(delta, 1, 1, 2)).toBe(true);
    expect(isEdgeAdded(delta, 1, 1, 3)).toBe(true);
    expect(isEdgeAdded(delta, 1, 2, 2)).toBe(true);

    const stats = getDeltaStats(delta);
    expect(stats.edgesAdded).toBe(3);
  });

  test("edge patches are sorted", () => {
    const delta = createDelta();

    // Add edges in random order
    addEdge(delta, 1, 2, 5);
    addEdge(delta, 1, 1, 3);
    addEdge(delta, 1, 2, 2);
    addEdge(delta, 1, 1, 1);

    const patches = delta.outAdd.get(1)!;

    // Should be sorted by (etype, other)
    for (let i = 1; i < patches.length; i++) {
      const prev = patches[i - 1]!;
      const curr = patches[i]!;

      const cmp =
        prev.etype < curr.etype
          ? -1
          : prev.etype > curr.etype
            ? 1
            : prev.other < curr.other
              ? -1
              : prev.other > curr.other
                ? 1
                : 0;

      expect(cmp).toBeLessThanOrEqual(0);
    }
  });
});

describe("Delta Definitions", () => {
  test("define label", () => {
    const delta = createDelta();

    defineLabel(delta, 1, "Person");
    defineLabel(delta, 2, "Company");

    expect(delta.newLabels.get(1)).toBe("Person");
    expect(delta.newLabels.get(2)).toBe("Company");
  });

  test("define edge type", () => {
    const delta = createDelta();

    defineEtype(delta, 1, "knows");

    expect(delta.newEtypes.get(1)).toBe("knows");
  });

  test("define property key", () => {
    const delta = createDelta();

    definePropkey(delta, 1, "name");

    expect(delta.newPropkeys.get(1)).toBe("name");
  });
});

describe("Delta Statistics", () => {
  test("empty delta stats", () => {
    const delta = createDelta();
    const stats = getDeltaStats(delta);

    expect(stats.nodesCreated).toBe(0);
    expect(stats.nodesDeleted).toBe(0);
    expect(stats.edgesAdded).toBe(0);
    expect(stats.edgesDeleted).toBe(0);
  });

  test("stats after operations", () => {
    const delta = createDelta();

    createNode(delta, 1);
    createNode(delta, 2);
    deleteNode(delta, 100);

    addEdge(delta, 1, 1, 2);
    addEdge(delta, 1, 2, 2);
    deleteEdge(delta, 100, 1, 101);

    const stats = getDeltaStats(delta);

    expect(stats.nodesCreated).toBe(2);
    expect(stats.nodesDeleted).toBe(1);
    expect(stats.edgesAdded).toBe(2);
    expect(stats.edgesDeleted).toBe(1);
  });
});

describe("Delta Clear", () => {
  test("clear resets all state", () => {
    const delta = createDelta();

    createNode(delta, 1, "test");
    addEdge(delta, 1, 1, 2);
    deleteNode(delta, 100);
    deleteEdge(delta, 100, 1, 101);
    defineLabel(delta, 1, "Test");

    clearDelta(delta);

    const stats = getDeltaStats(delta);
    expect(stats.nodesCreated).toBe(0);
    expect(stats.nodesDeleted).toBe(0);
    expect(stats.edgesAdded).toBe(0);
    expect(stats.edgesDeleted).toBe(0);

    expect(delta.newLabels.size).toBe(0);
    expect(delta.keyIndex.size).toBe(0);
  });
});
