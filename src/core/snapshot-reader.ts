/**
 * Snapshot reader - mmap-based CSR snapshot reading
 */

import { join } from "node:path";
import {
  MAGIC_SNAPSHOT,
  MIN_READER_SNAPSHOT,
  SNAPSHOTS_DIR,
  snapshotFilename,
} from "../constants.ts";
import {
  type ETypeID,
  KEY_INDEX_ENTRY_SIZE,
  type KeyIndexEntry,
  type NodeID,
  type PhysNode,
  PROP_VALUE_DISK_SIZE,
  type PropValue,
  PropValueTag,
  SECTION_ENTRY_SIZE,
  type SectionEntry,
  SectionId,
  SNAPSHOT_HEADER_SIZE,
  type SnapshotData,
  SnapshotFlags,
  type SnapshotHeaderV1,
  type StringID,
} from "../types.ts";
import {
  decodeString,
  readI32,
  readI32At,
  readU32,
  readU32At,
  readU64,
  readU64At,
  u64BitsToF64,
  viewOf,
} from "../util/binary.ts";
import {
  CompressionType,
  decompress,
  isValidCompressionType,
} from "../util/compression.ts";
import { crc32c } from "../util/crc.ts";
import { xxhash64String } from "../util/hash.ts";

// ============================================================================
// Snapshot loading
// ============================================================================

/**
 * Load and mmap a snapshot file
 */
export async function loadSnapshot(
  dbPath: string,
  generation: bigint,
): Promise<SnapshotData> {
  const filename = snapshotFilename(generation);
  const filepath = join(dbPath, SNAPSHOTS_DIR, filename);

  // Use Bun.mmap for zero-copy reading
  const buffer = Bun.mmap(filepath);

  return parseSnapshot(buffer);
}

/**
 * Options for parsing a snapshot
 */
export interface ParseSnapshotOptions {
  /**
   * Skip CRC validation for performance when reading cached/trusted data.
   * 
   * **SECURITY WARNING**: Enabling this option allows loading potentially
   * corrupted or tampered snapshot data. Only use this when:
   * 
   * 1. The snapshot has already been validated (e.g., cached in memory)
   * 2. The snapshot source is fully trusted (e.g., just written by this process)
   * 3. Performance is critical and data integrity risks are acceptable
   * 
   * **Risks when enabled**:
   * - Corrupted data may cause crashes or undefined behavior
   * - Maliciously crafted snapshots could exploit parsing vulnerabilities
   * - Silent data corruption may go undetected
   * 
   * **Default**: false (CRC validation enabled)
   */
  skipCrcValidation?: boolean;
}

/**
 * Parse a snapshot from a buffer (mmap'd or regular)
 */
export function parseSnapshot(
  buffer: Uint8Array,
  options?: ParseSnapshotOptions,
): SnapshotData {
  if (buffer.length < SNAPSHOT_HEADER_SIZE) {
    throw new Error(`Snapshot too small: ${buffer.length} bytes`);
  }

  const view = viewOf(buffer);

  // Parse header
  let offset = 0;
  const magic = readU32(view, offset);
  offset += 4;
  if (magic !== MAGIC_SNAPSHOT) {
    throw new Error(`Invalid snapshot magic: 0x${magic.toString(16)}`);
  }

  const version = readU32(view, offset);
  offset += 4;
  const minReaderVersion = readU32(view, offset);
  offset += 4;

  if (MIN_READER_SNAPSHOT < minReaderVersion) {
    throw new Error(
      `Snapshot requires reader version ${minReaderVersion}, we are ${MIN_READER_SNAPSHOT}`,
    );
  }

  const flags = readU32(view, offset);
  offset += 4;
  const generation = readU64(view, offset);
  offset += 8;
  const createdUnixNs = readU64(view, offset);
  offset += 8;
  const numNodes = readU64(view, offset);
  offset += 8;
  const numEdges = readU64(view, offset);
  offset += 8;
  const maxNodeId = readU64(view, offset);
  offset += 8;
  const numLabels = readU64(view, offset);
  offset += 8;
  const numEtypes = readU64(view, offset);
  offset += 8;
  const numPropkeys = readU64(view, offset);
  offset += 8;
  const numStrings = readU64(view, offset);
  offset += 8;

  const header: SnapshotHeaderV1 = {
    magic,
    version,
    minReaderVersion,
    flags,
    generation,
    createdUnixNs,
    numNodes,
    numEdges,
    maxNodeId,
    numLabels,
    numEtypes,
    numPropkeys,
    numStrings,
  };

  // Parse section table
  const sections: SectionEntry[] = [];
  offset = SNAPSHOT_HEADER_SIZE;

  for (let i = 0; i < SectionId._COUNT; i++) {
    const sectionOffset = readU64(view, offset);
    offset += 8;
    const sectionLength = readU64(view, offset);
    offset += 8;
    const compression = readU32(view, offset);
    offset += 4;
    const uncompressedSize = readU32(view, offset);
    offset += 4;

    sections.push({
      offset: sectionOffset,
      length: sectionLength,
      compression,
      uncompressedSize,
    });
  }

  // Calculate actual snapshot size from section table
  // The actual size is: max(section.offset + section.length) + 4 (for CRC)
  // This handles page-aligned buffers where buffer.length may be larger than actual data
  let maxSectionEnd = SNAPSHOT_HEADER_SIZE + SectionId._COUNT * SECTION_ENTRY_SIZE;
  for (const section of sections) {
    if (section.length > 0n) {
      const sectionEnd = Number(section.offset) + Number(section.length);
      if (sectionEnd > maxSectionEnd) {
        maxSectionEnd = sectionEnd;
      }
    }
  }
  // Round up to next 64-byte alignment (sections are aligned)
  const SECTION_ALIGNMENT = 64;
  const alignedEnd = Math.ceil(maxSectionEnd / SECTION_ALIGNMENT) * SECTION_ALIGNMENT;
  const actualSnapshotSize = alignedEnd + 4; // +4 for CRC

  // Verify footer CRC (optional - can be skipped for performance on trusted/cached data)
  if (!options?.skipCrcValidation) {
    // Use actualSnapshotSize if buffer is page-aligned (larger than actual data)
    const crcOffset = actualSnapshotSize <= buffer.length ? actualSnapshotSize - 4 : buffer.length - 4;
    const footerCrc = readU32(view, crcOffset);
    const computedCrc = crc32c(buffer.subarray(0, crcOffset));
    if (footerCrc !== computedCrc) {
      throw new Error(
        `Snapshot CRC mismatch: stored=0x${footerCrc.toString(16)}, computed=0x${computedCrc.toString(16)}`,
      );
    }
  }

  // Cache for decompressed section data
  const decompressedCache: Map<SectionId, Uint8Array> = new Map();

  // Helper to get raw section bytes (possibly compressed)
  function getRawSectionBytes(id: SectionId): Uint8Array | null {
    const section = sections[id];
    if (!section || section.length === 0n) return null;
    return buffer.subarray(
      Number(section.offset),
      Number(section.offset) + Number(section.length),
    );
  }

  // Helper to get decompressed section bytes
  function getSectionBytes(id: SectionId): Uint8Array | null {
    const section = sections[id];
    if (!section || section.length === 0n) return null;

    // Check if already decompressed
    const cached = decompressedCache.get(id);
    if (cached) return cached;

    const rawBytes = getRawSectionBytes(id);
    if (!rawBytes) return null;

    // If not compressed, return raw bytes directly (zero-copy from mmap)
    if (
      section.compression === CompressionType.NONE ||
      !isValidCompressionType(section.compression)
    ) {
      return rawBytes;
    }

    // Decompress the section
    const decompressed = decompress(
      rawBytes,
      section.compression as CompressionType,
    );
    decompressedCache.set(id, decompressed);
    return decompressed;
  }

  // Helper to get section view (with decompression if needed)
  function getSection(id: SectionId): DataView | null {
    const bytes = getSectionBytes(id);
    if (!bytes) return null;
    return viewOf(bytes);
  }

  // Build section views
  const physToNodeId = getSection(SectionId.PHYS_TO_NODEID)!;
  const nodeIdToPhys = getSection(SectionId.NODEID_TO_PHYS)!;
  const outOffsets = getSection(SectionId.OUT_OFFSETS)!;
  const outDst = getSection(SectionId.OUT_DST)!;
  const outEtype = getSection(SectionId.OUT_ETYPE)!;

  const hasInEdges = (flags & SnapshotFlags.HAS_IN_EDGES) !== 0;
  const inOffsets = hasInEdges ? getSection(SectionId.IN_OFFSETS) : null;
  const inSrc = hasInEdges ? getSection(SectionId.IN_SRC) : null;
  const inEtype = hasInEdges ? getSection(SectionId.IN_ETYPE) : null;
  const inOutIndex = hasInEdges ? getSection(SectionId.IN_OUT_INDEX) : null;

  const stringOffsets = getSection(SectionId.STRING_OFFSETS)!;
  const stringBytes =
    getSectionBytes(SectionId.STRING_BYTES) ?? new Uint8Array(0);

  const labelStringIds = getSection(SectionId.LABEL_STRING_IDS)!;
  const etypeStringIds = getSection(SectionId.ETYPE_STRING_IDS)!;
  const propkeyStringIds = getSection(SectionId.PROPKEY_STRING_IDS);

  const nodeKeyString = getSection(SectionId.NODE_KEY_STRING)!;
  const keyEntries = getSection(SectionId.KEY_ENTRIES)!;
  const keyBuckets = getSection(SectionId.KEY_BUCKETS);

  const hasProperties = (flags & SnapshotFlags.HAS_PROPERTIES) !== 0;
  const nodePropOffsets = hasProperties
    ? getSection(SectionId.NODE_PROP_OFFSETS)
    : null;
  const nodePropKeys = hasProperties
    ? getSection(SectionId.NODE_PROP_KEYS)
    : null;
  const nodePropVals = hasProperties
    ? getSection(SectionId.NODE_PROP_VALS)
    : null;
  const edgePropOffsets = hasProperties
    ? getSection(SectionId.EDGE_PROP_OFFSETS)
    : null;
  const edgePropKeys = hasProperties
    ? getSection(SectionId.EDGE_PROP_KEYS)
    : null;
  const edgePropVals = hasProperties
    ? getSection(SectionId.EDGE_PROP_VALS)
    : null;

  return {
    buffer,
    view,
    header,
    sections,
    physToNodeId,
    nodeIdToPhys,
    outOffsets,
    outDst,
    outEtype,
    inOffsets,
    inSrc,
    inEtype,
    inOutIndex,
    stringOffsets,
    stringBytes,
    labelStringIds,
    etypeStringIds,
    propkeyStringIds,
    nodeKeyString,
    keyEntries,
    keyBuckets,
    nodePropOffsets,
    nodePropKeys,
    nodePropVals,
    edgePropOffsets,
    edgePropKeys,
    edgePropVals,
  };
}

// ============================================================================
// Snapshot accessors
// ============================================================================

/**
 * Get NodeID for a physical node index
 */
export function getNodeId(snapshot: SnapshotData, phys: PhysNode): NodeID {
  return Number(readU64At(snapshot.physToNodeId, phys));
}

/**
 * Get physical node index for a NodeID, or -1 if not present
 */
export function getPhysNode(snapshot: SnapshotData, nodeId: NodeID): PhysNode {
  if (nodeId < 0 || nodeId >= snapshot.nodeIdToPhys.byteLength / 4) {
    return -1;
  }
  return readI32At(snapshot.nodeIdToPhys, nodeId);
}

/**
 * Check if a NodeID exists in the snapshot
 */
export function hasNode(snapshot: SnapshotData, nodeId: NodeID): boolean {
  return getPhysNode(snapshot, nodeId) >= 0;
}

/**
 * Get string by StringID
 */
export function getString(snapshot: SnapshotData, stringId: StringID): string {
  if (stringId === 0) return "";

  const startOffset = readU32At(snapshot.stringOffsets, stringId);
  const endOffset = readU32At(snapshot.stringOffsets, stringId + 1);

  return decodeString(snapshot.stringBytes.subarray(startOffset, endOffset));
}

/**
 * Get out-edges for a physical node
 */
export function getOutEdges(
  snapshot: SnapshotData,
  phys: PhysNode,
): { dst: PhysNode; etype: ETypeID }[] {
  const start = readU32At(snapshot.outOffsets, phys);
  const end = readU32At(snapshot.outOffsets, phys + 1);

  const edges: { dst: PhysNode; etype: ETypeID }[] = [];
  for (let i = start; i < end; i++) {
    edges.push({
      dst: readU32At(snapshot.outDst, i),
      etype: readU32At(snapshot.outEtype, i),
    });
  }
  return edges;
}

/**
 * Iterate out-edges for a physical node (generator version)
 * 
 * Optimization: Avoids array allocation for large degree nodes.
 * Use this when you don't need to materialize all edges at once.
 */
export function* iterateOutEdges(
  snapshot: SnapshotData,
  phys: PhysNode,
): Generator<{ dst: PhysNode; etype: ETypeID }> {
  const start = readU32At(snapshot.outOffsets, phys);
  const end = readU32At(snapshot.outOffsets, phys + 1);

  for (let i = start; i < end; i++) {
    yield {
      dst: readU32At(snapshot.outDst, i),
      etype: readU32At(snapshot.outEtype, i),
    };
  }
}

/**
 * Get in-edges for a physical node
 */
export function getInEdges(
  snapshot: SnapshotData,
  phys: PhysNode,
): { src: PhysNode; etype: ETypeID; outIndex: number }[] | null {
  if (!snapshot.inOffsets || !snapshot.inSrc || !snapshot.inEtype) {
    return null;
  }

  const start = readU32At(snapshot.inOffsets, phys);
  const end = readU32At(snapshot.inOffsets, phys + 1);

  const edges: { src: PhysNode; etype: ETypeID; outIndex: number }[] = [];
  for (let i = start; i < end; i++) {
    edges.push({
      src: readU32At(snapshot.inSrc, i),
      etype: readU32At(snapshot.inEtype, i),
      outIndex: snapshot.inOutIndex ? readU32At(snapshot.inOutIndex, i) : 0,
    });
  }
  return edges;
}

/**
 * Iterate in-edges for a physical node (generator version)
 * 
 * Optimization: Avoids array allocation for large degree nodes.
 * Use this when you don't need to materialize all edges at once.
 */
export function* iterateInEdges(
  snapshot: SnapshotData,
  phys: PhysNode,
): Generator<{ src: PhysNode; etype: ETypeID; outIndex: number }> {
  if (!snapshot.inOffsets || !snapshot.inSrc || !snapshot.inEtype) {
    return;
  }

  const start = readU32At(snapshot.inOffsets, phys);
  const end = readU32At(snapshot.inOffsets, phys + 1);

  for (let i = start; i < end; i++) {
    yield {
      src: readU32At(snapshot.inSrc, i),
      etype: readU32At(snapshot.inEtype, i),
      outIndex: snapshot.inOutIndex ? readU32At(snapshot.inOutIndex, i) : 0,
    };
  }
}

/**
 * Get out-edge count for a physical node
 */
export function getOutDegree(snapshot: SnapshotData, phys: PhysNode): number {
  const start = readU32At(snapshot.outOffsets, phys);
  const end = readU32At(snapshot.outOffsets, phys + 1);
  return end - start;
}

/**
 * Get in-edge count for a physical node
 */
export function getInDegree(
  snapshot: SnapshotData,
  phys: PhysNode,
): number | null {
  if (!snapshot.inOffsets) return null;
  const start = readU32At(snapshot.inOffsets, phys);
  const end = readU32At(snapshot.inOffsets, phys + 1);
  return end - start;
}

/**
 * Check if an edge exists in the snapshot
 */
export function hasEdge(
  snapshot: SnapshotData,
  srcPhys: PhysNode,
  etype: ETypeID,
  dstPhys: PhysNode,
): boolean {
  const start = readU32At(snapshot.outOffsets, srcPhys);
  const end = readU32At(snapshot.outOffsets, srcPhys + 1);

  // Binary search since edges are sorted by (etype, dst)
  let lo = start;
  let hi = end;

  while (lo < hi) {
    const mid = (lo + hi) >>> 1;
    const midEtype = readU32At(snapshot.outEtype, mid);
    const midDst = readU32At(snapshot.outDst, mid);

    if (midEtype < etype || (midEtype === etype && midDst < dstPhys)) {
      lo = mid + 1;
    } else {
      hi = mid;
    }
  }

  if (lo < end) {
    const foundEtype = readU32At(snapshot.outEtype, lo);
    const foundDst = readU32At(snapshot.outDst, lo);
    return foundEtype === etype && foundDst === dstPhys;
  }

  return false;
}

// ============================================================================
// Key index lookup
// ============================================================================

/**
 * Look up a node by key in the snapshot
 */
export function lookupByKey(
  snapshot: SnapshotData,
  key: string,
): NodeID | null {
  const hash64 = xxhash64String(key);

  const numEntries = snapshot.keyEntries.byteLength / KEY_INDEX_ENTRY_SIZE;
  if (numEntries === 0) return null;

  let lo: number;
  let hi: number;

  // Use bucket index if available for O(1) bucket lookup
  if (snapshot.keyBuckets && snapshot.keyBuckets.byteLength > 4) {
    const numBuckets = snapshot.keyBuckets.byteLength / 4 - 1;
    const bucket = Number(hash64 % BigInt(numBuckets));
    lo = readU32At(snapshot.keyBuckets, bucket);
    hi = readU32At(snapshot.keyBuckets, bucket + 1);
  } else {
    // Fall back to binary search over all entries
    lo = 0;
    hi = numEntries;

    // Find first entry with matching hash
    while (lo < hi) {
      const mid = (lo + hi) >>> 1;
      const midHash = snapshot.keyEntries.getBigUint64(
        mid * KEY_INDEX_ENTRY_SIZE,
        true,
      );

      if (midHash < hash64) {
        lo = mid + 1;
      } else {
        hi = mid;
      }
    }
    hi = numEntries; // Search from lo to end
  }

  // Check all entries in range with matching hash (handle collisions)
  for (let i = lo; i < hi; i++) {
    const offset = i * KEY_INDEX_ENTRY_SIZE;
    const entryHash = snapshot.keyEntries.getBigUint64(offset, true);

    if (entryHash !== hash64) continue; // Different bucket collision

    const stringId = snapshot.keyEntries.getUint32(offset + 8, true);
    const nodeId = Number(snapshot.keyEntries.getBigUint64(offset + 16, true));

    // Compare actual key bytes
    const entryKey = getString(snapshot, stringId);
    if (entryKey === key) {
      return nodeId;
    }
  }

  return null;
}

/**
 * Get the key for a node, if any
 */
export function getNodeKey(
  snapshot: SnapshotData,
  phys: PhysNode,
): string | null {
  const stringId = readU32At(snapshot.nodeKeyString, phys);
  if (stringId === 0) return null;
  return getString(snapshot, stringId);
}

// ============================================================================
// Property access
// ============================================================================

/**
 * Decode a property value from disk format
 */
function decodePropValue(
  view: DataView,
  offset: number,
  snapshot: SnapshotData,
): PropValue {
  const tag = view.getUint8(offset);
  const payload = view.getBigUint64(offset + 8, true);

  switch (tag) {
    case PropValueTag.NULL:
      return { tag: PropValueTag.NULL };
    case PropValueTag.BOOL:
      return { tag: PropValueTag.BOOL, value: payload !== 0n };
    case PropValueTag.I64:
      return { tag: PropValueTag.I64, value: payload };
    case PropValueTag.F64:
      return { tag: PropValueTag.F64, value: u64BitsToF64(payload) };
    case PropValueTag.STRING:
      return {
        tag: PropValueTag.STRING,
        value: getString(snapshot, Number(payload)),
      };
    default:
      return { tag: PropValueTag.NULL };
  }
}

/**
 * Get all properties for a node
 */
export function getNodeProps(
  snapshot: SnapshotData,
  phys: PhysNode,
): Map<number, PropValue> | null {
  if (
    !snapshot.nodePropOffsets ||
    !snapshot.nodePropKeys ||
    !snapshot.nodePropVals
  ) {
    return null;
  }

  const start = readU32At(snapshot.nodePropOffsets, phys);
  const end = readU32At(snapshot.nodePropOffsets, phys + 1);

  const props = new Map<number, PropValue>();
  for (let i = start; i < end; i++) {
    const keyId = readU32At(snapshot.nodePropKeys, i);
    const value = decodePropValue(
      snapshot.nodePropVals,
      i * PROP_VALUE_DISK_SIZE,
      snapshot,
    );
    props.set(keyId, value);
  }

  return props;
}

/**
 * Get a specific property for a node
 */
export function getNodeProp(
  snapshot: SnapshotData,
  phys: PhysNode,
  propKeyId: number,
): PropValue | null {
  if (
    !snapshot.nodePropOffsets ||
    !snapshot.nodePropKeys ||
    !snapshot.nodePropVals
  ) {
    return null;
  }

  const start = readU32At(snapshot.nodePropOffsets, phys);
  const end = readU32At(snapshot.nodePropOffsets, phys + 1);

  for (let i = start; i < end; i++) {
    const keyId = readU32At(snapshot.nodePropKeys, i);
    if (keyId === propKeyId) {
      return decodePropValue(
        snapshot.nodePropVals,
        i * PROP_VALUE_DISK_SIZE,
        snapshot,
      );
    }
  }

  return null;
}

/**
 * Get all properties for an edge by edge index
 */
export function getEdgeProps(
  snapshot: SnapshotData,
  edgeIdx: number,
): Map<number, PropValue> | null {
  if (
    !snapshot.edgePropOffsets ||
    !snapshot.edgePropKeys ||
    !snapshot.edgePropVals
  ) {
    return null;
  }

  const start = readU32At(snapshot.edgePropOffsets, edgeIdx);
  const end = readU32At(snapshot.edgePropOffsets, edgeIdx + 1);

  const props = new Map<number, PropValue>();
  for (let i = start; i < end; i++) {
    const keyId = readU32At(snapshot.edgePropKeys, i);
    const value = decodePropValue(
      snapshot.edgePropVals,
      i * PROP_VALUE_DISK_SIZE,
      snapshot,
    );
    props.set(keyId, value);
  }

  return props;
}

/**
 * Get a specific property for an edge by edge index
 */
export function getEdgeProp(
  snapshot: SnapshotData,
  edgeIdx: number,
  propKeyId: number,
): PropValue | null {
  if (
    !snapshot.edgePropOffsets ||
    !snapshot.edgePropKeys ||
    !snapshot.edgePropVals
  ) {
    return null;
  }

  const start = readU32At(snapshot.edgePropOffsets, edgeIdx);
  const end = readU32At(snapshot.edgePropOffsets, edgeIdx + 1);

  for (let i = start; i < end; i++) {
    const keyId = readU32At(snapshot.edgePropKeys, i);
    if (keyId === propKeyId) {
      return decodePropValue(
        snapshot.edgePropVals,
        i * PROP_VALUE_DISK_SIZE,
        snapshot,
      );
    }
  }

  return null;
}

/**
 * Find the edge index for a specific edge (src, etype, dst)
 * Returns -1 if edge not found
 */
export function findEdgeIndex(
  snapshot: SnapshotData,
  srcPhys: PhysNode,
  etype: ETypeID,
  dstPhys: PhysNode,
): number {
  const start = readU32At(snapshot.outOffsets, srcPhys);
  const end = readU32At(snapshot.outOffsets, srcPhys + 1);

  // Binary search since edges are sorted by (etype, dst)
  let lo = start;
  let hi = end;

  while (lo < hi) {
    const mid = (lo + hi) >>> 1;
    const midEtype = readU32At(snapshot.outEtype, mid);
    const midDst = readU32At(snapshot.outDst, mid);

    if (midEtype < etype || (midEtype === etype && midDst < dstPhys)) {
      lo = mid + 1;
    } else {
      hi = mid;
    }
  }

  if (lo < end) {
    const foundEtype = readU32At(snapshot.outEtype, lo);
    const foundDst = readU32At(snapshot.outDst, lo);
    if (foundEtype === etype && foundDst === dstPhys) {
      return lo;
    }
  }

  return -1;
}

/**
 * Close/release a snapshot (let GC clean up mmap)
 * Clears all DataView references to allow the underlying buffer to be GC'd
 */
export function closeSnapshot(snapshot: SnapshotData): void {
  // Setting all references to null allows GC to clean up the mmap
  // Bun will automatically unmap when the buffer is garbage collected
  const mutableSnapshot = snapshot as {
    buffer: Uint8Array | null;
    view: DataView | null;
    physToNodeId: DataView | null;
    nodeIdToPhys: DataView | null;
    outOffsets: DataView | null;
    outDst: DataView | null;
    outEtype: DataView | null;
    inOffsets: DataView | null;
    inSrc: DataView | null;
    inEtype: DataView | null;
    inOutIndex: DataView | null;
    stringOffsets: DataView | null;
    stringBytes: Uint8Array | null;
    labelStringIds: DataView | null;
    etypeStringIds: DataView | null;
    propkeyStringIds: DataView | null;
    nodeKeyString: DataView | null;
    keyEntries: DataView | null;
    keyBuckets: DataView | null;
    nodePropOffsets: DataView | null;
    nodePropKeys: DataView | null;
    nodePropVals: DataView | null;
    edgePropOffsets: DataView | null;
    edgePropKeys: DataView | null;
    edgePropVals: DataView | null;
  };

  // Clear main buffer and view
  mutableSnapshot.buffer = null;
  mutableSnapshot.view = null;

  // Clear all cached section DataViews
  mutableSnapshot.physToNodeId = null;
  mutableSnapshot.nodeIdToPhys = null;
  mutableSnapshot.outOffsets = null;
  mutableSnapshot.outDst = null;
  mutableSnapshot.outEtype = null;
  mutableSnapshot.inOffsets = null;
  mutableSnapshot.inSrc = null;
  mutableSnapshot.inEtype = null;
  mutableSnapshot.inOutIndex = null;
  mutableSnapshot.stringOffsets = null;
  mutableSnapshot.stringBytes = null;
  mutableSnapshot.labelStringIds = null;
  mutableSnapshot.etypeStringIds = null;
  mutableSnapshot.propkeyStringIds = null;
  mutableSnapshot.nodeKeyString = null;
  mutableSnapshot.keyEntries = null;
  mutableSnapshot.keyBuckets = null;
  mutableSnapshot.nodePropOffsets = null;
  mutableSnapshot.nodePropKeys = null;
  mutableSnapshot.nodePropVals = null;
  mutableSnapshot.edgePropOffsets = null;
  mutableSnapshot.edgePropKeys = null;
  mutableSnapshot.edgePropVals = null;
}
