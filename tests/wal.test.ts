/**
 * WAL tests
 */

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { MAGIC_WAL } from "../src/constants.ts";
import {
  type WalRecord,
  appendToWal,
  buildAddEdgePayload,
  buildBeginPayload,
  buildCommitPayload,
  buildCreateNodePayload,
  buildWalRecord,
  createWalHeader,
  createWalSegment,
  extractCommittedTransactions,
  loadWalSegment,
  parseAddEdgePayload,
  parseCreateNodePayload,
  parseWalHeader,
  parseWalRecord,
  scanWal,
  serializeWalHeader,
  // Vector WAL functions
  buildSetNodeVectorPayload,
  buildDelNodeVectorPayload,
  buildBatchVectorsPayload,
  buildSealFragmentPayload,
  buildCompactFragmentsPayload,
  parseSetNodeVectorPayload,
  parseDelNodeVectorPayload,
  parseBatchVectorsPayload,
  parseSealFragmentPayload,
  parseCompactFragmentsPayload,
} from "../src/core/wal.ts";
import { WAL_HEADER_SIZE, WalRecordType } from "../src/types.ts";

describe("WAL Header", () => {
  test("create and serialize header", () => {
    const header = createWalHeader(1n);
    const bytes = serializeWalHeader(header);

    expect(bytes.length).toBe(WAL_HEADER_SIZE);

    // Check magic
    const view = new DataView(bytes.buffer);
    expect(view.getUint32(0, true)).toBe(MAGIC_WAL);
  });

  test("roundtrip header", () => {
    const header = createWalHeader(42n);
    const bytes = serializeWalHeader(header);
    const parsed = parseWalHeader(bytes);

    expect(parsed.magic).toBe(MAGIC_WAL);
    expect(parsed.segmentId).toBe(42n);
  });
});

describe("WAL Records", () => {
  test("build and parse record", () => {
    const record: WalRecord = {
      type: WalRecordType.CREATE_NODE,
      txid: 1n,
      payload: buildCreateNodePayload(100, "test-key"),
    };

    const bytes = buildWalRecord(record);
    const parsed = parseWalRecord(bytes, 0);

    expect(parsed).not.toBeNull();
    expect(parsed!.type).toBe(WalRecordType.CREATE_NODE);
    expect(parsed!.txid).toBe(1n);

    const data = parseCreateNodePayload(parsed!.payload);
    expect(data.nodeId).toBe(100);
    expect(data.key).toBe("test-key");
  });

  test("record alignment to 8 bytes", () => {
    const record: WalRecord = {
      type: WalRecordType.BEGIN,
      txid: 1n,
      payload: buildBeginPayload(),
    };

    const bytes = buildWalRecord(record);
    expect(bytes.length % 8).toBe(0);
  });

  test("invalid CRC detection", () => {
    const record: WalRecord = {
      type: WalRecordType.CREATE_NODE,
      txid: 1n,
      payload: buildCreateNodePayload(100),
    };

    const bytes = buildWalRecord(record);

    // Corrupt a byte in the payload
    bytes[25] = bytes[25]! ^ 0xff;

    const parsed = parseWalRecord(bytes, 0);
    expect(parsed).toBeNull(); // CRC check should fail
  });

  test("truncated record detection", () => {
    const record: WalRecord = {
      type: WalRecordType.CREATE_NODE,
      txid: 1n,
      payload: buildCreateNodePayload(100),
    };

    const bytes = buildWalRecord(record);
    const truncated = bytes.slice(0, bytes.length - 5);

    const parsed = parseWalRecord(truncated, 0);
    expect(parsed).toBeNull();
  });
});

describe("WAL Scanning", () => {
  test("scan multiple records", () => {
    const records: WalRecord[] = [
      { type: WalRecordType.BEGIN, txid: 1n, payload: buildBeginPayload() },
      {
        type: WalRecordType.CREATE_NODE,
        txid: 1n,
        payload: buildCreateNodePayload(1),
      },
      {
        type: WalRecordType.ADD_EDGE,
        txid: 1n,
        payload: buildAddEdgePayload(1, 1, 2),
      },
      { type: WalRecordType.COMMIT, txid: 1n, payload: buildCommitPayload() },
    ];

    // Build combined buffer with header
    const header = serializeWalHeader(createWalHeader(1n));
    const recordBytes = records.map((r) => buildWalRecord(r));
    const totalSize =
      header.length + recordBytes.reduce((s, b) => s + b.length, 0);

    const buffer = new Uint8Array(totalSize);
    buffer.set(header, 0);
    let offset = header.length;
    for (const bytes of recordBytes) {
      buffer.set(bytes, offset);
      offset += bytes.length;
    }

    const parsed = scanWal(buffer);
    expect(parsed).toHaveLength(4);
    expect(parsed[0]!.type).toBe(WalRecordType.BEGIN);
    expect(parsed[1]!.type).toBe(WalRecordType.CREATE_NODE);
    expect(parsed[2]!.type).toBe(WalRecordType.ADD_EDGE);
    expect(parsed[3]!.type).toBe(WalRecordType.COMMIT);
  });

  test("extract committed transactions", () => {
    const records: WalRecord[] = [
      // Transaction 1 - committed
      { type: WalRecordType.BEGIN, txid: 1n, payload: buildBeginPayload() },
      {
        type: WalRecordType.CREATE_NODE,
        txid: 1n,
        payload: buildCreateNodePayload(1),
      },
      { type: WalRecordType.COMMIT, txid: 1n, payload: buildCommitPayload() },

      // Transaction 2 - uncommitted (no COMMIT)
      { type: WalRecordType.BEGIN, txid: 2n, payload: buildBeginPayload() },
      {
        type: WalRecordType.CREATE_NODE,
        txid: 2n,
        payload: buildCreateNodePayload(2),
      },

      // Transaction 3 - committed
      { type: WalRecordType.BEGIN, txid: 3n, payload: buildBeginPayload() },
      {
        type: WalRecordType.ADD_EDGE,
        txid: 3n,
        payload: buildAddEdgePayload(1, 1, 2),
      },
      { type: WalRecordType.COMMIT, txid: 3n, payload: buildCommitPayload() },
    ];

    // Build buffer
    const header = serializeWalHeader(createWalHeader(1n));
    const recordBytes = records.map((r) => buildWalRecord(r));
    const totalSize =
      header.length + recordBytes.reduce((s, b) => s + b.length, 0);

    const buffer = new Uint8Array(totalSize);
    buffer.set(header, 0);
    let offset = header.length;
    for (const bytes of recordBytes) {
      buffer.set(bytes, offset);
      offset += bytes.length;
    }

    const parsed = scanWal(buffer);
    const committed = extractCommittedTransactions(parsed);

    expect(committed.size).toBe(2);
    expect(committed.has(1n)).toBe(true);
    expect(committed.has(2n)).toBe(false); // Uncommitted
    expect(committed.has(3n)).toBe(true);

    expect(committed.get(1n)!).toHaveLength(1);
    expect(committed.get(3n)!).toHaveLength(1);
  });
});

describe("WAL File Operations", () => {
  let testDir: string;

  beforeEach(async () => {
    testDir = await mkdtemp(join(tmpdir(), "ray-wal-test-"));
  });

  afterEach(async () => {
    await rm(testDir, { recursive: true, force: true });
  });

  test("create and load WAL segment", async () => {
    const filepath = await createWalSegment(testDir, 1n);
    expect(filepath).toContain("wal_0000000000000001.gdw");

    const loaded = await loadWalSegment(testDir, 1n);
    expect(loaded).not.toBeNull();
    expect(loaded!.header.segmentId).toBe(1n);
    expect(loaded!.records).toHaveLength(0);
  });

  test("append records to WAL", async () => {
    await createWalSegment(testDir, 1n);

    const records: WalRecord[] = [
      { type: WalRecordType.BEGIN, txid: 1n, payload: buildBeginPayload() },
      {
        type: WalRecordType.CREATE_NODE,
        txid: 1n,
        payload: buildCreateNodePayload(1, "test"),
      },
      { type: WalRecordType.COMMIT, txid: 1n, payload: buildCommitPayload() },
    ];

    const walPath = join(testDir, "wal", "wal_0000000000000001.gdw");
    await appendToWal(walPath, records);

    const loaded = await loadWalSegment(testDir, 1n);
    expect(loaded!.records).toHaveLength(3);
  });
});

// ============================================================================
// Vector WAL Payload Tests
// ============================================================================

describe("Vector WAL Payloads", () => {
  test("SET_NODE_VECTOR roundtrip", () => {
    const nodeId = 12345;
    const propKeyId = 42;
    const vector = new Float32Array([0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8]);

    const payload = buildSetNodeVectorPayload(nodeId, propKeyId, vector);
    const parsed = parseSetNodeVectorPayload(payload);

    expect(parsed.nodeId).toBe(nodeId);
    expect(parsed.propKeyId).toBe(propKeyId);
    expect(parsed.dimensions).toBe(8);
    expect(parsed.vector.length).toBe(8);
    expect(parsed.vector[0]).toBeCloseTo(0.1, 5);
    expect(parsed.vector[7]).toBeCloseTo(0.8, 5);
  });

  test("SET_NODE_VECTOR with large vector (768 dims)", () => {
    const nodeId = 999;
    const propKeyId = 1;
    const vector = new Float32Array(768);
    for (let i = 0; i < 768; i++) {
      vector[i] = i / 768;
    }

    const payload = buildSetNodeVectorPayload(nodeId, propKeyId, vector);
    const parsed = parseSetNodeVectorPayload(payload);

    expect(parsed.nodeId).toBe(nodeId);
    expect(parsed.propKeyId).toBe(propKeyId);
    expect(parsed.dimensions).toBe(768);
    expect(parsed.vector.length).toBe(768);
    expect(parsed.vector[0]).toBeCloseTo(0 / 768, 5);
    expect(parsed.vector[767]).toBeCloseTo(767 / 768, 5);
  });

  test("DEL_NODE_VECTOR roundtrip", () => {
    const nodeId = 54321;
    const propKeyId = 99;

    const payload = buildDelNodeVectorPayload(nodeId, propKeyId);
    const parsed = parseDelNodeVectorPayload(payload);

    expect(parsed.nodeId).toBe(nodeId);
    expect(parsed.propKeyId).toBe(propKeyId);
  });

  test("BATCH_VECTORS roundtrip", () => {
    const propKeyId = 5;
    const dimensions = 4;
    const entries = [
      { nodeId: 1, vector: new Float32Array([1.0, 2.0, 3.0, 4.0]) },
      { nodeId: 2, vector: new Float32Array([5.0, 6.0, 7.0, 8.0]) },
      { nodeId: 3, vector: new Float32Array([9.0, 10.0, 11.0, 12.0]) },
    ];

    const payload = buildBatchVectorsPayload(propKeyId, dimensions, entries);
    const parsed = parseBatchVectorsPayload(payload);

    expect(parsed.propKeyId).toBe(propKeyId);
    expect(parsed.dimensions).toBe(dimensions);
    expect(parsed.entries.length).toBe(3);

    expect(parsed.entries[0]!.nodeId).toBe(1);
    expect(parsed.entries[0]!.vector[0]).toBeCloseTo(1.0, 5);
    expect(parsed.entries[0]!.vector[3]).toBeCloseTo(4.0, 5);

    expect(parsed.entries[1]!.nodeId).toBe(2);
    expect(parsed.entries[1]!.vector[0]).toBeCloseTo(5.0, 5);

    expect(parsed.entries[2]!.nodeId).toBe(3);
    expect(parsed.entries[2]!.vector[3]).toBeCloseTo(12.0, 5);
  });

  test("BATCH_VECTORS with large batch", () => {
    const propKeyId = 1;
    const dimensions = 768;
    const entries: Array<{ nodeId: number; vector: Float32Array }> = [];
    
    for (let i = 0; i < 100; i++) {
      const vector = new Float32Array(dimensions);
      for (let d = 0; d < dimensions; d++) {
        vector[d] = (i * dimensions + d) / 1000;
      }
      entries.push({ nodeId: i + 1, vector });
    }

    const payload = buildBatchVectorsPayload(propKeyId, dimensions, entries);
    const parsed = parseBatchVectorsPayload(payload);

    expect(parsed.entries.length).toBe(100);
    expect(parsed.entries[0]!.nodeId).toBe(1);
    expect(parsed.entries[99]!.nodeId).toBe(100);
  });

  test("SEAL_FRAGMENT roundtrip", () => {
    const fragmentId = 42;
    const newFragmentId = 43;

    const payload = buildSealFragmentPayload(fragmentId, newFragmentId);
    const parsed = parseSealFragmentPayload(payload);

    expect(parsed.fragmentId).toBe(fragmentId);
    expect(parsed.newFragmentId).toBe(newFragmentId);
  });

  test("COMPACT_FRAGMENTS roundtrip", () => {
    const sourceFragmentIds = [1, 2, 3, 4, 5];
    const targetFragmentId = 10;

    const payload = buildCompactFragmentsPayload(sourceFragmentIds, targetFragmentId);
    const parsed = parseCompactFragmentsPayload(payload);

    expect(parsed.targetFragmentId).toBe(targetFragmentId);
    expect(parsed.sourceFragmentIds).toEqual(sourceFragmentIds);
  });

  test("vector WAL record can be built and parsed", () => {
    const vector = new Float32Array([0.1, 0.2, 0.3, 0.4]);
    const record: WalRecord = {
      type: WalRecordType.SET_NODE_VECTOR,
      txid: 1n,
      payload: buildSetNodeVectorPayload(100, 5, vector),
    };

    const bytes = buildWalRecord(record);
    const parsed = parseWalRecord(bytes, 0);

    expect(parsed).not.toBeNull();
    expect(parsed!.type).toBe(WalRecordType.SET_NODE_VECTOR);
    expect(parsed!.txid).toBe(1n);

    const data = parseSetNodeVectorPayload(parsed!.payload);
    expect(data.nodeId).toBe(100);
    expect(data.propKeyId).toBe(5);
    expect(data.vector.length).toBe(4);
    expect(data.vector[0]).toBeCloseTo(0.1, 5);
  });

  test("vector records are properly aligned", () => {
    const vector = new Float32Array(768);
    for (let i = 0; i < 768; i++) vector[i] = Math.random();

    const record: WalRecord = {
      type: WalRecordType.SET_NODE_VECTOR,
      txid: 1n,
      payload: buildSetNodeVectorPayload(1, 1, vector),
    };

    const bytes = buildWalRecord(record);
    expect(bytes.length % 8).toBe(0); // Should be 8-byte aligned
  });
});
