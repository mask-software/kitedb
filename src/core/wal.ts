/**
 * WAL (Write-Ahead Log) - append, framing, and recovery
 */

import { join } from "node:path";
import {
  MAGIC_WAL,
  MIN_READER_WAL,
  VERSION_WAL,
  WAL_DIR,
  WAL_RECORD_ALIGNMENT,
  walFilename,
} from "../constants.ts";
import {
  type ETypeID,
  type LabelID,
  type NodeID,
  type PropKeyID,
  type PropValue,
  PropValueTag,
  WAL_HEADER_SIZE,
  WAL_RECORD_HEADER_SIZE,
  type WalHeaderV1,
  type WalRecordHeader,
  WalRecordType,
} from "../types.ts";
import {
  alignUp,
  BufferBuilder,
  decodeString,
  encodeString,
  f64ToU64Bits,
  paddingFor,
  readU8,
  readU16,
  readU32,
  readU64,
  u64BitsToF64,
  viewOf,
  writeU8,
  writeU16,
  writeU32,
  writeU64,
} from "../util/binary.ts";
import { crc32c } from "../util/crc.ts";

// ============================================================================
// WAL Header
// ============================================================================

export function createWalHeader(segmentId: bigint): WalHeaderV1 {
  return {
    magic: MAGIC_WAL,
    version: VERSION_WAL,
    minReaderVersion: MIN_READER_WAL,
    reserved: 0,
    segmentId,
    createdUnixNs: BigInt(Date.now()) * 1000000n,
    reserved2: [0n, 0n, 0n, 0n, 0n, 0n, 0n, 0n],
  };
}

export function serializeWalHeader(header: WalHeaderV1): Uint8Array {
  const buffer = new Uint8Array(WAL_HEADER_SIZE);
  const view = viewOf(buffer);

  let offset = 0;
  writeU32(view, offset, header.magic);
  offset += 4;
  writeU32(view, offset, header.version);
  offset += 4;
  writeU32(view, offset, header.minReaderVersion);
  offset += 4;
  writeU32(view, offset, header.reserved);
  offset += 4;
  writeU64(view, offset, header.segmentId);
  offset += 8;
  writeU64(view, offset, header.createdUnixNs);
  offset += 8;

  for (let i = 0; i < 8; i++) {
    writeU64(view, offset, header.reserved2[i] ?? 0n);
    offset += 8;
  }

  return buffer;
}

export function parseWalHeader(buffer: Uint8Array): WalHeaderV1 {
  if (buffer.length < WAL_HEADER_SIZE) {
    throw new Error(`WAL header too small: ${buffer.length}`);
  }

  const view = viewOf(buffer);
  let offset = 0;

  const magic = readU32(view, offset);
  offset += 4;
  if (magic !== MAGIC_WAL) {
    throw new Error(`Invalid WAL magic: 0x${magic.toString(16)}`);
  }

  const version = readU32(view, offset);
  offset += 4;
  const minReaderVersion = readU32(view, offset);
  offset += 4;

  if (MIN_READER_WAL < minReaderVersion) {
    throw new Error(
      `WAL requires reader version ${minReaderVersion}, we are ${MIN_READER_WAL}`,
    );
  }

  const reserved = readU32(view, offset);
  offset += 4;
  const segmentId = readU64(view, offset);
  offset += 8;
  const createdUnixNs = readU64(view, offset);
  offset += 8;

  const reserved2: bigint[] = [];
  for (let i = 0; i < 8; i++) {
    reserved2.push(readU64(view, offset));
    offset += 8;
  }

  return {
    magic,
    version,
    minReaderVersion,
    reserved,
    segmentId,
    createdUnixNs,
    reserved2,
  };
}

// ============================================================================
// WAL Record Building
// ============================================================================

export interface WalRecord {
  type: WalRecordType;
  txid: bigint;
  payload: Uint8Array;
}

/**
 * Build a WAL record with proper framing
 */
export function buildWalRecord(record: WalRecord): Uint8Array {
  const { type, txid, payload } = record;

  // Calculate sizes
  const headerSize = WAL_RECORD_HEADER_SIZE;
  const crcSize = 4;
  const unpadded = headerSize + payload.length + crcSize;
  const padLen = paddingFor(unpadded, WAL_RECORD_ALIGNMENT);
  const totalSize = unpadded + padLen;

  const buffer = new Uint8Array(totalSize);
  const view = viewOf(buffer);

  // Write header
  let offset = 0;
  writeU32(view, offset, unpadded);
  offset += 4; // recLen (unpadded)
  writeU8(view, offset, type);
  offset += 1;
  writeU8(view, offset, 0);
  offset += 1; // flags
  writeU16(view, offset, 0);
  offset += 2; // reserved
  writeU64(view, offset, txid);
  offset += 8;
  writeU32(view, offset, payload.length);
  offset += 4;

  // Write payload
  buffer.set(payload, offset);
  offset += payload.length;

  // Compute CRC (over type + flags + reserved + txid + payloadLen + payload)
  const crcStart = 4; // After recLen
  const crcEnd = offset;
  const crcValue = crc32c(buffer.subarray(crcStart, crcEnd));
  writeU32(view, offset, crcValue);
  offset += 4;

  // Padding is already zeros

  return buffer;
}

// ============================================================================
// Payload builders for different record types
// ============================================================================

export function buildBeginPayload(): Uint8Array {
  return new Uint8Array(0);
}

export function buildCommitPayload(): Uint8Array {
  return new Uint8Array(0);
}

export function buildRollbackPayload(): Uint8Array {
  return new Uint8Array(0);
}

export function buildCreateNodePayload(
  nodeId: NodeID,
  key?: string,
): Uint8Array {
  const keyBytes = key ? encodeString(key) : new Uint8Array(0);
  const buffer = new Uint8Array(8 + 4 + keyBytes.length);
  const view = viewOf(buffer);

  writeU64(view, 0, nodeId);
  writeU32(view, 8, keyBytes.length);
  buffer.set(keyBytes, 12);

  return buffer;
}

export function buildDeleteNodePayload(nodeId: NodeID): Uint8Array {
  const buffer = new Uint8Array(8);
  const view = viewOf(buffer);
  writeU64(view, 0, nodeId);
  return buffer;
}

export function buildAddEdgePayload(
  src: NodeID,
  etype: ETypeID,
  dst: NodeID,
): Uint8Array {
  const buffer = new Uint8Array(8 + 4 + 8);
  const view = viewOf(buffer);
  writeU64(view, 0, src);
  writeU32(view, 8, etype);
  writeU64(view, 12, dst);
  return buffer;
}

export function buildDeleteEdgePayload(
  src: NodeID,
  etype: ETypeID,
  dst: NodeID,
): Uint8Array {
  const buffer = new Uint8Array(8 + 4 + 8);
  const view = viewOf(buffer);
  writeU64(view, 0, src);
  writeU32(view, 8, etype);
  writeU64(view, 12, dst);
  return buffer;
}

export function buildDefineLabelPayload(
  labelId: LabelID,
  name: string,
): Uint8Array {
  const nameBytes = encodeString(name);
  const buffer = new Uint8Array(4 + 4 + nameBytes.length);
  const view = viewOf(buffer);
  writeU32(view, 0, labelId);
  writeU32(view, 4, nameBytes.length);
  buffer.set(nameBytes, 8);
  return buffer;
}

export function buildDefineEtypePayload(
  etypeId: ETypeID,
  name: string,
): Uint8Array {
  const nameBytes = encodeString(name);
  const buffer = new Uint8Array(4 + 4 + nameBytes.length);
  const view = viewOf(buffer);
  writeU32(view, 0, etypeId);
  writeU32(view, 4, nameBytes.length);
  buffer.set(nameBytes, 8);
  return buffer;
}

export function buildDefinePropkeyPayload(
  propkeyId: PropKeyID,
  name: string,
): Uint8Array {
  const nameBytes = encodeString(name);
  const buffer = new Uint8Array(4 + 4 + nameBytes.length);
  const view = viewOf(buffer);
  writeU32(view, 0, propkeyId);
  writeU32(view, 4, nameBytes.length);
  buffer.set(nameBytes, 8);
  return buffer;
}

function serializePropValue(value: PropValue): Uint8Array {
  switch (value.tag) {
    case PropValueTag.NULL:
      return new Uint8Array([0]);
    case PropValueTag.BOOL: {
      const buf = new Uint8Array(2);
      buf[0] = 1;
      buf[1] = value.value ? 1 : 0;
      return buf;
    }
    case PropValueTag.I64: {
      const buf = new Uint8Array(9);
      const view = viewOf(buf);
      buf[0] = 2;
      view.setBigInt64(1, value.value, true);
      return buf;
    }
    case PropValueTag.F64: {
      const buf = new Uint8Array(9);
      const view = viewOf(buf);
      buf[0] = 3;
      view.setFloat64(1, value.value, true);
      return buf;
    }
    case PropValueTag.STRING: {
      const strBytes = encodeString(value.value);
      const buf = new Uint8Array(1 + 4 + strBytes.length);
      const view = viewOf(buf);
      buf[0] = 4;
      view.setUint32(1, strBytes.length, true);
      buf.set(strBytes, 5);
      return buf;
    }
  }
}

export function buildSetNodePropPayload(
  nodeId: NodeID,
  keyId: PropKeyID,
  value: PropValue,
): Uint8Array {
  const valueBytes = serializePropValue(value);
  const buffer = new Uint8Array(8 + 4 + valueBytes.length);
  const view = viewOf(buffer);
  writeU64(view, 0, nodeId);
  writeU32(view, 8, keyId);
  buffer.set(valueBytes, 12);
  return buffer;
}

export function buildDelNodePropPayload(
  nodeId: NodeID,
  keyId: PropKeyID,
): Uint8Array {
  const buffer = new Uint8Array(8 + 4);
  const view = viewOf(buffer);
  writeU64(view, 0, nodeId);
  writeU32(view, 8, keyId);
  return buffer;
}

export function buildSetEdgePropPayload(
  src: NodeID,
  etype: ETypeID,
  dst: NodeID,
  keyId: PropKeyID,
  value: PropValue,
): Uint8Array {
  const valueBytes = serializePropValue(value);
  const buffer = new Uint8Array(8 + 4 + 8 + 4 + valueBytes.length);
  const view = viewOf(buffer);
  writeU64(view, 0, src);
  writeU32(view, 8, etype);
  writeU64(view, 12, dst);
  writeU32(view, 20, keyId);
  buffer.set(valueBytes, 24);
  return buffer;
}

export function buildDelEdgePropPayload(
  src: NodeID,
  etype: ETypeID,
  dst: NodeID,
  keyId: PropKeyID,
): Uint8Array {
  const buffer = new Uint8Array(8 + 4 + 8 + 4);
  const view = viewOf(buffer);
  writeU64(view, 0, src);
  writeU32(view, 8, etype);
  writeU64(view, 12, dst);
  writeU32(view, 20, keyId);
  return buffer;
}

// ============================================================================
// WAL Reading and Recovery
// ============================================================================

export interface ParsedWalRecord {
  type: WalRecordType;
  flags: number;
  txid: bigint;
  payload: Uint8Array;
  recordEnd: number; // Offset after this record (including padding)
}

/**
 * Parse a single WAL record from buffer at given offset
 * Returns null if record is invalid or truncated
 */
export function parseWalRecord(
  buffer: Uint8Array,
  offset: number,
): ParsedWalRecord | null {
  if (offset + 4 > buffer.length) return null;

  const view = viewOf(buffer, offset);

  // Read record length
  const recLen = readU32(view, 0);
  if (recLen < WAL_RECORD_HEADER_SIZE + 4) return null; // Too small

  // Check if full record is available
  const padLen = paddingFor(recLen, WAL_RECORD_ALIGNMENT);
  const totalLen = recLen + padLen;

  if (offset + totalLen > buffer.length) return null; // Truncated

  // Read header fields
  const type = readU8(view, 4);
  const flags = readU8(view, 5);
  const reserved = readU16(view, 6);
  const txid = readU64(view, 8);
  const payloadLen = readU32(view, 16);

  // Validate payload length
  if (WAL_RECORD_HEADER_SIZE + payloadLen + 4 !== recLen) return null;

  // Extract payload
  const payload = buffer.subarray(
    offset + WAL_RECORD_HEADER_SIZE,
    offset + WAL_RECORD_HEADER_SIZE + payloadLen,
  );

  // Verify CRC (covers type + flags + reserved + txid + payloadLen + payload)
  const crcStart = offset + 4;
  const crcEnd = offset + WAL_RECORD_HEADER_SIZE + payloadLen;
  const storedCrc = readU32(view, WAL_RECORD_HEADER_SIZE + payloadLen);
  const computedCrc = crc32c(buffer.subarray(crcStart, crcEnd));

  if (storedCrc !== computedCrc) return null; // CRC mismatch

  return {
    type,
    flags,
    txid,
    payload,
    recordEnd: offset + totalLen,
  };
}

/**
 * Scan WAL and return all valid records
 */
export function scanWal(buffer: Uint8Array): ParsedWalRecord[] {
  const records: ParsedWalRecord[] = [];
  let offset = WAL_HEADER_SIZE;

  while (offset < buffer.length) {
    const record = parseWalRecord(buffer, offset);
    if (!record) break; // Invalid or truncated record

    records.push(record);
    offset = record.recordEnd;
  }

  return records;
}

/**
 * Extract committed transactions from WAL records
 * Returns records grouped by committed transaction
 */
export function extractCommittedTransactions(
  records: ParsedWalRecord[],
): Map<bigint, ParsedWalRecord[]> {
  const pending = new Map<bigint, ParsedWalRecord[]>();
  const committed = new Map<bigint, ParsedWalRecord[]>();

  for (const record of records) {
    const { txid, type } = record;

    switch (type) {
      case WalRecordType.BEGIN:
        pending.set(txid, []);
        break;

      case WalRecordType.COMMIT: {
        const txRecords = pending.get(txid);
        if (txRecords) {
          committed.set(txid, txRecords);
          pending.delete(txid);
        }
        break;
      }

      case WalRecordType.ROLLBACK:
        pending.delete(txid);
        break;

      default: {
        // Data record - add to pending transaction
        const txPending = pending.get(txid);
        if (txPending) {
          txPending.push(record);
        }
        break;
      }
    }
  }

  return committed;
}

// ============================================================================
// Payload Parsers
// ============================================================================

export interface CreateNodeData {
  nodeId: NodeID;
  key?: string;
}

export function parseCreateNodePayload(payload: Uint8Array): CreateNodeData {
  const view = viewOf(payload);
  const nodeId = readU64(view, 0);
  const keyLen = readU32(view, 8);
  const key =
    keyLen > 0 ? decodeString(payload.subarray(12, 12 + keyLen)) : undefined;
  return { nodeId, key };
}

export interface DeleteNodeData {
  nodeId: NodeID;
}

export function parseDeleteNodePayload(payload: Uint8Array): DeleteNodeData {
  const view = viewOf(payload);
  return { nodeId: readU64(view, 0) };
}

export interface AddEdgeData {
  src: NodeID;
  etype: ETypeID;
  dst: NodeID;
}

export function parseAddEdgePayload(payload: Uint8Array): AddEdgeData {
  const view = viewOf(payload);
  return {
    src: readU64(view, 0),
    etype: readU32(view, 8),
    dst: readU64(view, 12),
  };
}

export interface DeleteEdgeData {
  src: NodeID;
  etype: ETypeID;
  dst: NodeID;
}

export function parseDeleteEdgePayload(payload: Uint8Array): DeleteEdgeData {
  const view = viewOf(payload);
  return {
    src: readU64(view, 0),
    etype: readU32(view, 8),
    dst: readU64(view, 12),
  };
}

export interface DefineLabelData {
  labelId: LabelID;
  name: string;
}

export function parseDefineLabelPayload(payload: Uint8Array): DefineLabelData {
  const view = viewOf(payload);
  const labelId = readU32(view, 0);
  const nameLen = readU32(view, 4);
  const name = decodeString(payload.subarray(8, 8 + nameLen));
  return { labelId, name };
}

export interface DefineEtypeData {
  etypeId: ETypeID;
  name: string;
}

export function parseDefineEtypePayload(payload: Uint8Array): DefineEtypeData {
  const view = viewOf(payload);
  const etypeId = readU32(view, 0);
  const nameLen = readU32(view, 4);
  const name = decodeString(payload.subarray(8, 8 + nameLen));
  return { etypeId, name };
}

export interface DefinePropkeyData {
  propkeyId: PropKeyID;
  name: string;
}

export function parseDefinePropkeyPayload(
  payload: Uint8Array,
): DefinePropkeyData {
  const view = viewOf(payload);
  const propkeyId = readU32(view, 0);
  const nameLen = readU32(view, 4);
  const name = decodeString(payload.subarray(8, 8 + nameLen));
  return { propkeyId, name };
}

function parsePropValue(
  payload: Uint8Array,
  offset: number,
): { value: PropValue; bytesRead: number } {
  const tag = payload[offset]!;

  switch (tag) {
    case PropValueTag.NULL:
      return { value: { tag: PropValueTag.NULL }, bytesRead: 1 };
    case PropValueTag.BOOL:
      return {
        value: { tag: PropValueTag.BOOL, value: payload[offset + 1] !== 0 },
        bytesRead: 2,
      };
    case PropValueTag.I64: {
      const view = viewOf(payload, offset + 1);
      return {
        value: { tag: PropValueTag.I64, value: view.getBigInt64(0, true) },
        bytesRead: 9,
      };
    }
    case PropValueTag.F64: {
      const view = viewOf(payload, offset + 1);
      return {
        value: { tag: PropValueTag.F64, value: view.getFloat64(0, true) },
        bytesRead: 9,
      };
    }
    case PropValueTag.STRING: {
      const view = viewOf(payload, offset + 1);
      const strLen = view.getUint32(0, true);
      const str = decodeString(
        payload.subarray(offset + 5, offset + 5 + strLen),
      );
      return {
        value: { tag: PropValueTag.STRING, value: str },
        bytesRead: 5 + strLen,
      };
    }
    default:
      return { value: { tag: PropValueTag.NULL }, bytesRead: 1 };
  }
}

export interface SetNodePropData {
  nodeId: NodeID;
  keyId: PropKeyID;
  value: PropValue;
}

export function parseSetNodePropPayload(payload: Uint8Array): SetNodePropData {
  const view = viewOf(payload);
  const nodeId = readU64(view, 0);
  const keyId = readU32(view, 8);
  const { value } = parsePropValue(payload, 12);
  return { nodeId, keyId, value };
}

export interface DelNodePropData {
  nodeId: NodeID;
  keyId: PropKeyID;
}

export function parseDelNodePropPayload(payload: Uint8Array): DelNodePropData {
  const view = viewOf(payload);
  return {
    nodeId: readU64(view, 0),
    keyId: readU32(view, 8),
  };
}

export interface SetEdgePropData {
  src: NodeID;
  etype: ETypeID;
  dst: NodeID;
  keyId: PropKeyID;
  value: PropValue;
}

export function parseSetEdgePropPayload(payload: Uint8Array): SetEdgePropData {
  const view = viewOf(payload);
  const src = readU64(view, 0);
  const etype = readU32(view, 8);
  const dst = readU64(view, 12);
  const keyId = readU32(view, 20);
  const { value } = parsePropValue(payload, 24);
  return { src, etype, dst, keyId, value };
}

export interface DelEdgePropData {
  src: NodeID;
  etype: ETypeID;
  dst: NodeID;
  keyId: PropKeyID;
}

export function parseDelEdgePropPayload(payload: Uint8Array): DelEdgePropData {
  const view = viewOf(payload);
  return {
    src: readU64(view, 0),
    etype: readU32(view, 8),
    dst: readU64(view, 12),
    keyId: readU32(view, 20),
  };
}

// ============================================================================
// WAL File Operations
// ============================================================================

/**
 * Create a new WAL segment file
 */
export async function createWalSegment(
  dbPath: string,
  segmentId: bigint,
): Promise<string> {
  const walDir = join(dbPath, WAL_DIR);
  const fs = await import("node:fs/promises");
  await fs.mkdir(walDir, { recursive: true });

  const filename = walFilename(segmentId);
  const filepath = join(walDir, filename);

  const header = createWalHeader(segmentId);
  const headerBytes = serializeWalHeader(header);

  await Bun.write(filepath, headerBytes);

  // Sync to disk for durability
  const fd = await fs.open(filepath, "r+");
  await fd.sync();
  await fd.close();

  return filepath;
}

/**
 * Append records to WAL file
 */
export async function appendToWal(
  filepath: string,
  records: WalRecord[],
): Promise<number> {
  const fs = await import("node:fs/promises");

  // Build all record bytes first
  const recordBytes = records.map((r) => buildWalRecord(r));
  const totalNewBytes = recordBytes.reduce((sum, b) => sum + b.length, 0);

  // Combine all records into single buffer for efficient write
  const combined = new Uint8Array(totalNewBytes);
  let offset = 0;
  for (const bytes of recordBytes) {
    combined.set(bytes, offset);
    offset += bytes.length;
  }

  // Open in append mode
  const fd = await fs.open(filepath, "a");

  try {
    // Write only the new records
    await fd.write(combined);
    await fd.sync();

    // Get new file size for return value
    const stat = await fd.stat();
    return stat.size;
  } finally {
    await fd.close();
  }
}

/**
 * Load and scan a WAL segment file
 */
export async function loadWalSegment(
  dbPath: string,
  segmentId: bigint,
): Promise<{
  header: WalHeaderV1;
  records: ParsedWalRecord[];
} | null> {
  const walDir = join(dbPath, WAL_DIR);
  const filename = walFilename(segmentId);
  const filepath = join(walDir, filename);

  try {
    const file = Bun.file(filepath);
    if (!(await file.exists())) return null;

    const buffer = new Uint8Array(await file.arrayBuffer());
    const header = parseWalHeader(buffer);
    const records = scanWal(buffer);

    return { header, records };
  } catch {
    return null;
  }
}
