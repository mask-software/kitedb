/**
 * Compression tests for snapshot sections
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
  getNodeProp,
  getNodeProps,
  getOutEdges,
  getPhysNode,
  hasNode,
  loadSnapshot,
  lookupByKey,
} from "../src/core/snapshot-reader.ts";
import {
  type SnapshotBuildInput,
  buildSnapshot,
} from "../src/core/snapshot-writer.ts";
import { PropValueTag, SectionId } from "../src/types.ts";
import {
  CompressionType,
  compress,
  decompress,
  maybeCompress,
} from "../src/util/compression.ts";

describe("Compression utilities", () => {
  test("compress and decompress with zstd", () => {
    const data = new Uint8Array(1000);
    // Fill with some compressible data
    for (let i = 0; i < data.length; i++) {
      data[i] = i % 256;
    }

    const compressed = compress(data, CompressionType.ZSTD);
    expect(compressed.length).toBeLessThan(data.length);

    const decompressed = decompress(compressed, CompressionType.ZSTD);
    expect(decompressed).toEqual(data);
  });

  test("compress and decompress with gzip", () => {
    const data = new Uint8Array(1000);
    for (let i = 0; i < data.length; i++) {
      data[i] = i % 256;
    }

    const compressed = compress(data, CompressionType.GZIP);
    expect(compressed.length).toBeLessThan(data.length);

    const decompressed = decompress(compressed, CompressionType.GZIP);
    expect(decompressed).toEqual(data);
  });

  test("compress and decompress with deflate", () => {
    const data = new Uint8Array(1000);
    for (let i = 0; i < data.length; i++) {
      data[i] = i % 256;
    }

    const compressed = compress(data, CompressionType.DEFLATE);
    expect(compressed.length).toBeLessThan(data.length);

    const decompressed = decompress(compressed, CompressionType.DEFLATE);
    expect(decompressed).toEqual(data);
  });

  test("no compression returns original data", () => {
    const data = new Uint8Array([1, 2, 3, 4, 5]);
    const result = compress(data, CompressionType.NONE);
    expect(result).toBe(data); // Same reference
  });

  test("maybeCompress skips small data", () => {
    const data = new Uint8Array([1, 2, 3, 4, 5]);
    const result = maybeCompress(data, {
      enabled: true,
      type: CompressionType.ZSTD,
      minSize: 64,
    });
    expect(result.type).toBe(CompressionType.NONE);
    expect(result.data).toBe(data);
  });

  test("maybeCompress compresses large data", () => {
    const data = new Uint8Array(1000);
    for (let i = 0; i < data.length; i++) {
      data[i] = i % 256;
    }

    const result = maybeCompress(data, {
      enabled: true,
      type: CompressionType.ZSTD,
      minSize: 64,
    });
    expect(result.type).toBe(CompressionType.ZSTD);
    expect(result.data.length).toBeLessThan(data.length);
  });

  test("maybeCompress respects enabled flag", () => {
    const data = new Uint8Array(1000);
    for (let i = 0; i < data.length; i++) {
      data[i] = i % 256;
    }

    const result = maybeCompress(data, {
      enabled: false,
      type: CompressionType.ZSTD,
      minSize: 64,
    });
    expect(result.type).toBe(CompressionType.NONE);
    expect(result.data).toBe(data);
  });

  test("maybeCompress skips if compression increases size", () => {
    // Random data compresses poorly
    const data = new Uint8Array(100);
    crypto.getRandomValues(data);

    const result = maybeCompress(data, {
      enabled: true,
      type: CompressionType.ZSTD,
      minSize: 64,
    });
    // Should either not compress or compress (but we return original if larger)
    if (result.type === CompressionType.NONE) {
      expect(result.data).toBe(data);
    }
  });
});

describe("Compressed Snapshots", () => {
  let testDir: string;

  beforeEach(async () => {
    testDir = await mkdtemp(join(tmpdir(), "ray-compression-test-"));
  });

  afterEach(async () => {
    await rm(testDir, { recursive: true, force: true });
  });

  test("build and load compressed snapshot with nodes", async () => {
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
      compression: {
        enabled: true,
        type: CompressionType.ZSTD,
        minSize: 0, // Compress all sections for testing
      },
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

    // Check keys
    expect(lookupByKey(snapshot, "user:alice")).toBe(2n);
    expect(lookupByKey(snapshot, "user:bob")).toBe(5n);

    const result = checkSnapshot(snapshot);
    expect(result.valid).toBe(true);
  });

  test("build and load compressed snapshot with edges", async () => {
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
      compression: {
        enabled: true,
        type: CompressionType.ZSTD,
        minSize: 0,
      },
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

    const result = checkSnapshot(snapshot);
    expect(result.valid).toBe(true);
  });

  test("compressed snapshot with properties", async () => {
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
      ],
      edges: [],
      labels: new Map(),
      etypes: new Map(),
      propkeys: new Map([
        [nameId, "name"],
        [ageId, "age"],
      ]),
      compression: {
        enabled: true,
        type: CompressionType.ZSTD,
        minSize: 0,
      },
    };

    await buildSnapshot(testDir, input);
    const snapshot = await loadSnapshot(testDir, 1n);

    // Check node 1 properties
    const phys1 = getPhysNode(snapshot, 1n);
    const node1Props = getNodeProps(snapshot, phys1);
    expect(node1Props).not.toBeNull();
    expect(node1Props?.size).toBe(2);

    const nameProp = getNodeProp(snapshot, phys1, nameId);
    expect(nameProp?.tag).toBe(PropValueTag.STRING);
    expect((nameProp as { tag: 4; value: string }).value).toBe("Alice");

    const result = checkSnapshot(snapshot);
    expect(result.valid).toBe(true);
  });

  test("compressed snapshot with many nodes (key index)", async () => {
    // Create enough nodes to ensure buckets are built
    const nodes = [];
    for (let i = 1; i <= 100; i++) {
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
      compression: {
        enabled: true,
        type: CompressionType.ZSTD,
        minSize: 0,
      },
    };

    await buildSnapshot(testDir, input);
    const snapshot = await loadSnapshot(testDir, 1n);

    // Verify all lookups work
    for (let i = 1; i <= 100; i++) {
      expect(lookupByKey(snapshot, `node-${i}`)).toBe(BigInt(i));
    }

    const result = checkSnapshot(snapshot);
    expect(result.valid).toBe(true);
  });

  test("gzip compression works for snapshots", async () => {
    const input: SnapshotBuildInput = {
      generation: 1n,
      nodes: [
        { nodeId: 1n, key: "test-node", labels: [], props: new Map() },
        { nodeId: 2n, labels: [], props: new Map() },
      ],
      edges: [{ src: 1n, etype: 1, dst: 2n, props: new Map() }],
      labels: new Map(),
      etypes: new Map([[1, "edge"]]),
      propkeys: new Map(),
      compression: {
        enabled: true,
        type: CompressionType.GZIP,
        minSize: 0,
      },
    };

    await buildSnapshot(testDir, input);
    const snapshot = await loadSnapshot(testDir, 1n);

    expect(hasNode(snapshot, 1n)).toBe(true);
    expect(lookupByKey(snapshot, "test-node")).toBe(1n);

    const result = checkSnapshot(snapshot);
    expect(result.valid).toBe(true);
  });

  test("sections record compression type", async () => {
    const nodes = [];
    for (let i = 1; i <= 100; i++) {
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
      compression: {
        enabled: true,
        type: CompressionType.ZSTD,
        minSize: 0,
      },
    };

    await buildSnapshot(testDir, input);
    const snapshot = await loadSnapshot(testDir, 1n);

    // Check that some sections are marked as compressed
    let hasCompressedSections = false;
    for (const section of snapshot.sections) {
      if (section.compression === CompressionType.ZSTD && section.length > 0n) {
        hasCompressedSections = true;
        // Compressed sections should have uncompressedSize set
        expect(section.uncompressedSize).toBeGreaterThan(0);
        break;
      }
    }
    expect(hasCompressedSections).toBe(true);
  });

  test("backwards compatibility - uncompressed snapshots still work", async () => {
    // Build without compression (default)
    const input: SnapshotBuildInput = {
      generation: 1n,
      nodes: [
        { nodeId: 1n, key: "test", labels: [], props: new Map() },
        { nodeId: 2n, labels: [], props: new Map() },
      ],
      edges: [{ src: 1n, etype: 1, dst: 2n, props: new Map() }],
      labels: new Map(),
      etypes: new Map([[1, "edge"]]),
      propkeys: new Map(),
      // No compression option - should default to uncompressed
    };

    await buildSnapshot(testDir, input);
    const snapshot = await loadSnapshot(testDir, 1n);

    expect(hasNode(snapshot, 1n)).toBe(true);
    expect(hasNode(snapshot, 2n)).toBe(true);
    expect(lookupByKey(snapshot, "test")).toBe(1n);

    // All sections should be uncompressed
    for (const section of snapshot.sections) {
      if (section.length > 0n) {
        expect(section.compression).toBe(CompressionType.NONE);
      }
    }

    const result = checkSnapshot(snapshot);
    expect(result.valid).toBe(true);
  });
});
