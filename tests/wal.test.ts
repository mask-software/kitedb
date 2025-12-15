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
      payload: buildCreateNodePayload(100n, "test-key"),
    };

    const bytes = buildWalRecord(record);
    const parsed = parseWalRecord(bytes, 0);

    expect(parsed).not.toBeNull();
    expect(parsed!.type).toBe(WalRecordType.CREATE_NODE);
    expect(parsed!.txid).toBe(1n);

    const data = parseCreateNodePayload(parsed!.payload);
    expect(data.nodeId).toBe(100n);
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
      payload: buildCreateNodePayload(100n),
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
      payload: buildCreateNodePayload(100n),
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
        payload: buildCreateNodePayload(1n),
      },
      {
        type: WalRecordType.ADD_EDGE,
        txid: 1n,
        payload: buildAddEdgePayload(1n, 1, 2n),
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
        payload: buildCreateNodePayload(1n),
      },
      { type: WalRecordType.COMMIT, txid: 1n, payload: buildCommitPayload() },

      // Transaction 2 - uncommitted (no COMMIT)
      { type: WalRecordType.BEGIN, txid: 2n, payload: buildBeginPayload() },
      {
        type: WalRecordType.CREATE_NODE,
        txid: 2n,
        payload: buildCreateNodePayload(2n),
      },

      // Transaction 3 - committed
      { type: WalRecordType.BEGIN, txid: 3n, payload: buildBeginPayload() },
      {
        type: WalRecordType.ADD_EDGE,
        txid: 3n,
        payload: buildAddEdgePayload(1n, 1, 2n),
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
        payload: buildCreateNodePayload(1n, "test"),
      },
      { type: WalRecordType.COMMIT, txid: 1n, payload: buildCommitPayload() },
    ];

    const walPath = join(testDir, "wal", "wal_0000000000000001.gdw");
    await appendToWal(walPath, records);

    const loaded = await loadWalSegment(testDir, 1n);
    expect(loaded!.records).toHaveLength(3);
  });
});
