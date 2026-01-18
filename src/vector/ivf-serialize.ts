/**
 * Binary serialization for IVF index and vector store
 */

import type { IvfIndex, IvfConfig, VectorManifest, Fragment, RowGroup } from "./types.ts";

// ============================================================================
// Constants
// ============================================================================

const IVF_MAGIC = 0x49564631; // "IVF1"
const IVF_HEADER_SIZE = 32;
const MANIFEST_MAGIC = 0x56454331; // "VEC1"
const MANIFEST_HEADER_SIZE = 64;

// ============================================================================
// Bounds Checking Helper
// ============================================================================

/**
 * Ensure buffer has enough bytes remaining for a read operation
 */
function ensureBytes(bufferLength: number, offset: number, needed: number, context: string): void {
  if (offset + needed > bufferLength) {
    throw new Error(
      `Buffer underflow in ${context}: need ${needed} bytes at offset ${offset}, but buffer is only ${bufferLength} bytes`
    );
  }
}

// ============================================================================
// IVF Index Serialization
// ============================================================================

/**
 * Calculate serialized size of IVF index
 */
export function ivfSerializedSize(index: IvfIndex, dimensions: number): number {
  let size = IVF_HEADER_SIZE;

  // Centroids
  size += index.centroids.length * 4;

  // Number of lists
  size += 4;

  // Inverted lists
  for (const [, list] of index.invertedLists) {
    size += 4 + 4 + list.length * 4; // cluster ID + list length + vector IDs
  }

  return size;
}

/**
 * Serialize IVF index to binary
 *
 * Format:
 * - Header (32 bytes)
 *   - magic (4): "IVF1"
 *   - nClusters (4)
 *   - dimensions (4)
 *   - nProbe (4)
 *   - trained (1)
 *   - usePQ (1)
 *   - metric (1): 0=cosine, 1=euclidean, 2=dot
 *   - reserved (13)
 * - Centroids (nClusters * dimensions * 4 bytes)
 * - numLists (4)
 * - For each inverted list:
 *   - cluster ID (4)
 *   - list length (4)
 *   - vector IDs (length * 4)
 */
export function serializeIvf(index: IvfIndex, dimensions: number): Uint8Array {
  const size = ivfSerializedSize(index, dimensions);
  const buffer = new Uint8Array(size);
  const view = new DataView(buffer.buffer);
  let offset = 0;

  // Header
  view.setUint32(offset, IVF_MAGIC, true);
  offset += 4;
  view.setUint32(offset, index.config.nClusters, true);
  offset += 4;
  view.setUint32(offset, dimensions, true);
  offset += 4;
  view.setUint32(offset, index.config.nProbe, true);
  offset += 4;
  view.setUint8(offset, index.trained ? 1 : 0);
  offset += 1;
  view.setUint8(offset, index.config.usePQ ? 1 : 0);
  offset += 1;
  view.setUint8(offset, metricToNumber(index.config.metric));
  offset += 1;
  offset += 13; // Reserved

  // Centroids
  for (let i = 0; i < index.centroids.length; i++) {
    view.setFloat32(offset, index.centroids[i], true);
    offset += 4;
  }

  // Inverted lists
  view.setUint32(offset, index.invertedLists.size, true);
  offset += 4;

  for (const [cluster, list] of index.invertedLists) {
    view.setUint32(offset, cluster, true);
    offset += 4;
    view.setUint32(offset, list.length, true);
    offset += 4;
    for (const vectorId of list) {
      view.setUint32(offset, vectorId, true);
      offset += 4;
    }
  }

  return buffer;
}

/**
 * Deserialize IVF index from binary
 */
export function deserializeIvf(buffer: Uint8Array): {
  index: IvfIndex;
  dimensions: number;
} {
  const bufLen = buffer.byteLength;
  ensureBytes(bufLen, 0, IVF_HEADER_SIZE, "IVF header");
  
  const view = new DataView(buffer.buffer, buffer.byteOffset, buffer.byteLength);
  let offset = 0;

  // Header
  const magic = view.getUint32(offset, true);
  offset += 4;
  if (magic !== IVF_MAGIC) {
    throw new Error(`Invalid IVF magic: expected ${IVF_MAGIC}, got ${magic}`);
  }

  const nClusters = view.getUint32(offset, true);
  offset += 4;
  const dimensions = view.getUint32(offset, true);
  offset += 4;
  const nProbe = view.getUint32(offset, true);
  offset += 4;
  const trained = view.getUint8(offset) === 1;
  offset += 1;
  const usePQ = view.getUint8(offset) === 1;
  offset += 1;
  const metric = numberToMetric(view.getUint8(offset));
  offset += 1;
  offset += 13; // Skip reserved

  const config: IvfConfig = {
    nClusters,
    nProbe,
    metric,
    usePQ,
  };

  // Centroids
  const centroidsSize = nClusters * dimensions * 4;
  ensureBytes(bufLen, offset, centroidsSize, "IVF centroids");
  const centroids = new Float32Array(nClusters * dimensions);
  for (let i = 0; i < centroids.length; i++) {
    centroids[i] = view.getFloat32(offset, true);
    offset += 4;
  }

  // Inverted lists
  ensureBytes(bufLen, offset, 4, "IVF inverted list count");
  const numLists = view.getUint32(offset, true);
  offset += 4;
  const invertedLists = new Map<number, number[]>();

  for (let i = 0; i < numLists; i++) {
    ensureBytes(bufLen, offset, 8, `IVF inverted list ${i} header`);
    const cluster = view.getUint32(offset, true);
    offset += 4;
    const listLength = view.getUint32(offset, true);
    offset += 4;
    
    ensureBytes(bufLen, offset, listLength * 4, `IVF inverted list ${i} data`);
    const list: number[] = [];
    for (let j = 0; j < listLength; j++) {
      list.push(view.getUint32(offset, true));
      offset += 4;
    }

    invertedLists.set(cluster, list);
  }

  return {
    index: {
      config,
      centroids,
      invertedLists,
      trained,
    },
    dimensions,
  };
}

// ============================================================================
// Vector Manifest Serialization
// ============================================================================

const FRAGMENT_HEADER_SIZE = 32;
const ROW_GROUP_HEADER_SIZE = 16;

/**
 * Calculate serialized size of vector manifest
 */
export function manifestSerializedSize(manifest: VectorManifest): number {
  let size = MANIFEST_HEADER_SIZE;

  // Fragments
  for (const fragment of manifest.fragments) {
    size += FRAGMENT_HEADER_SIZE;

    // Row groups
    for (const rg of fragment.rowGroups) {
      size += ROW_GROUP_HEADER_SIZE;
      size += rg.data.byteLength;
    }

    // Deletion bitmap
    size += fragment.deletionBitmap.byteLength;
  }

  // Node ID to Vector ID mapping
  size += 4; // count
  size += manifest.nodeIdToVectorId.size * 16; // nodeId (8) + vectorId (4) + padding (4)

  // Vector ID to Location mapping
  size += 4; // count
  size += manifest.vectorIdToLocation.size * 16; // vectorId (4) + fragmentId (4) + localIndex (4) + padding (4)

  return size;
}

/**
 * Serialize vector manifest to binary
 */
export function serializeManifest(manifest: VectorManifest): Uint8Array {
  const size = manifestSerializedSize(manifest);
  const buffer = new Uint8Array(size);
  const view = new DataView(buffer.buffer);
  let offset = 0;

  // Header
  view.setUint32(offset, MANIFEST_MAGIC, true);
  offset += 4;
  view.setUint32(offset, manifest.config.dimensions, true);
  offset += 4;
  view.setUint32(offset, metricToNumber(manifest.config.metric), true);
  offset += 4;
  view.setUint32(offset, manifest.config.rowGroupSize, true);
  offset += 4;
  view.setUint32(offset, manifest.config.fragmentTargetSize, true);
  offset += 4;
  view.setUint8(offset, manifest.config.normalize ? 1 : 0);
  offset += 1;
  offset += 3; // padding
  view.setUint32(offset, manifest.fragments.length, true);
  offset += 4;
  view.setUint32(offset, manifest.activeFragmentId, true);
  offset += 4;
  view.setUint32(offset, manifest.totalVectors, true);
  offset += 4;
  view.setUint32(offset, manifest.totalDeleted, true);
  offset += 4;
  view.setUint32(offset, manifest.nextVectorId, true);
  offset += 4;
  offset += 20; // reserved

  // Fragments
  for (const fragment of manifest.fragments) {
    // Fragment header
    view.setUint32(offset, fragment.id, true);
    offset += 4;
    view.setUint8(offset, fragment.state === "active" ? 0 : 1);
    offset += 1;
    offset += 3; // padding
    view.setUint32(offset, fragment.rowGroups.length, true);
    offset += 4;
    view.setUint32(offset, fragment.totalVectors, true);
    offset += 4;
    view.setUint32(offset, fragment.deletedCount, true);
    offset += 4;
    view.setUint32(offset, fragment.deletionBitmap.byteLength, true);
    offset += 4;
    offset += 8; // reserved

    // Row groups
    for (const rg of fragment.rowGroups) {
      view.setUint32(offset, rg.id, true);
      offset += 4;
      view.setUint32(offset, rg.count, true);
      offset += 4;
      view.setUint32(offset, rg.data.byteLength, true);
      offset += 4;
      offset += 4; // reserved

      // Row group data
      buffer.set(new Uint8Array(rg.data.buffer, rg.data.byteOffset, rg.data.byteLength), offset);
      offset += rg.data.byteLength;
    }

    // Deletion bitmap
    buffer.set(
      new Uint8Array(
        fragment.deletionBitmap.buffer,
        fragment.deletionBitmap.byteOffset,
        fragment.deletionBitmap.byteLength
      ),
      offset
    );
    offset += fragment.deletionBitmap.byteLength;
  }

  // Node ID to Vector ID mapping
  view.setUint32(offset, manifest.nodeIdToVectorId.size, true);
  offset += 4;

  for (const [nodeId, vectorId] of manifest.nodeIdToVectorId) {
    // NodeID as BigInt64 to handle large IDs
    view.setBigInt64(offset, BigInt(nodeId), true);
    offset += 8;
    view.setUint32(offset, vectorId, true);
    offset += 4;
    offset += 4; // padding
  }

  // Vector ID to Location mapping
  view.setUint32(offset, manifest.vectorIdToLocation.size, true);
  offset += 4;

  for (const [vectorId, location] of manifest.vectorIdToLocation) {
    view.setUint32(offset, vectorId, true);
    offset += 4;
    view.setUint32(offset, location.fragmentId, true);
    offset += 4;
    view.setUint32(offset, location.localIndex, true);
    offset += 4;
    offset += 4; // padding
  }

  return buffer;
}

/**
 * Deserialize vector manifest from binary
 */
export function deserializeManifest(buffer: Uint8Array): VectorManifest {
  const bufLen = buffer.byteLength;
  ensureBytes(bufLen, 0, MANIFEST_HEADER_SIZE, "manifest header");
  
  const view = new DataView(buffer.buffer, buffer.byteOffset, buffer.byteLength);
  let offset = 0;

  // Header
  const magic = view.getUint32(offset, true);
  offset += 4;
  if (magic !== MANIFEST_MAGIC) {
    throw new Error(`Invalid manifest magic: expected ${MANIFEST_MAGIC}, got ${magic}`);
  }

  const dimensions = view.getUint32(offset, true);
  offset += 4;
  const metric = numberToMetric(view.getUint32(offset, true));
  offset += 4;
  const rowGroupSize = view.getUint32(offset, true);
  offset += 4;
  const fragmentTargetSize = view.getUint32(offset, true);
  offset += 4;
  const normalize = view.getUint8(offset) === 1;
  offset += 1;
  offset += 3; // padding
  const numFragments = view.getUint32(offset, true);
  offset += 4;
  const activeFragmentId = view.getUint32(offset, true);
  offset += 4;
  const totalVectors = view.getUint32(offset, true);
  offset += 4;
  const totalDeleted = view.getUint32(offset, true);
  offset += 4;
  const nextVectorId = view.getUint32(offset, true);
  offset += 4;
  offset += 20; // reserved

  const config = {
    dimensions,
    metric,
    rowGroupSize,
    fragmentTargetSize,
    normalize,
  };

  // Fragments
  const fragments: Fragment[] = [];

  for (let f = 0; f < numFragments; f++) {
    // Fragment header
    ensureBytes(bufLen, offset, FRAGMENT_HEADER_SIZE, `fragment ${f} header`);
    const id = view.getUint32(offset, true);
    offset += 4;
    const state = view.getUint8(offset) === 0 ? "active" : "sealed";
    offset += 1;
    offset += 3; // padding
    const numRowGroups = view.getUint32(offset, true);
    offset += 4;
    const fragTotalVectors = view.getUint32(offset, true);
    offset += 4;
    const deletedCount = view.getUint32(offset, true);
    offset += 4;
    const deletionBitmapLength = view.getUint32(offset, true);
    offset += 4;
    offset += 8; // reserved

    // Row groups
    const rowGroups: RowGroup[] = [];

    for (let r = 0; r < numRowGroups; r++) {
      ensureBytes(bufLen, offset, ROW_GROUP_HEADER_SIZE, `fragment ${f} row group ${r} header`);
      const rgId = view.getUint32(offset, true);
      offset += 4;
      const count = view.getUint32(offset, true);
      offset += 4;
      const dataLength = view.getUint32(offset, true);
      offset += 4;
      offset += 4; // reserved

      // Copy row group data
      ensureBytes(bufLen, offset, dataLength, `fragment ${f} row group ${r} data`);
      const data = new Float32Array(dataLength / 4);
      const srcView = new DataView(buffer.buffer, buffer.byteOffset + offset, dataLength);
      for (let i = 0; i < data.length; i++) {
        data[i] = srcView.getFloat32(i * 4, true);
      }
      offset += dataLength;

      rowGroups.push({ id: rgId, count, data });
    }

    // Deletion bitmap
    ensureBytes(bufLen, offset, deletionBitmapLength, `fragment ${f} deletion bitmap`);
    const deletionBitmap = new Uint32Array(deletionBitmapLength / 4);
    const bitmapView = new DataView(buffer.buffer, buffer.byteOffset + offset, deletionBitmapLength);
    for (let i = 0; i < deletionBitmap.length; i++) {
      deletionBitmap[i] = bitmapView.getUint32(i * 4, true);
    }
    offset += deletionBitmapLength;

    fragments.push({
      id,
      state: state as "active" | "sealed",
      rowGroups,
      totalVectors: fragTotalVectors,
      deletionBitmap,
      deletedCount,
    });
  }

  // Node ID to Vector ID mapping
  ensureBytes(bufLen, offset, 4, "node-to-vector mapping count");
  const nodeIdToVectorIdCount = view.getUint32(offset, true);
  offset += 4;
  
  ensureBytes(bufLen, offset, nodeIdToVectorIdCount * 16, "node-to-vector mapping data");
  const nodeIdToVectorId = new Map<number, number>();
  const vectorIdToNodeId = new Map<number, number>();

  for (let i = 0; i < nodeIdToVectorIdCount; i++) {
    const nodeId = Number(view.getBigInt64(offset, true));
    offset += 8;
    const vectorId = view.getUint32(offset, true);
    offset += 4;
    offset += 4; // padding
    nodeIdToVectorId.set(nodeId, vectorId);
    vectorIdToNodeId.set(vectorId, nodeId);
  }

  // Vector ID to Location mapping
  ensureBytes(bufLen, offset, 4, "vector-to-location mapping count");
  const vectorIdToLocationCount = view.getUint32(offset, true);
  offset += 4;
  
  ensureBytes(bufLen, offset, vectorIdToLocationCount * 16, "vector-to-location mapping data");
  const vectorIdToLocation = new Map<number, { fragmentId: number; localIndex: number }>();

  for (let i = 0; i < vectorIdToLocationCount; i++) {
    const vectorId = view.getUint32(offset, true);
    offset += 4;
    const fragmentId = view.getUint32(offset, true);
    offset += 4;
    const localIndex = view.getUint32(offset, true);
    offset += 4;
    offset += 4; // padding
    vectorIdToLocation.set(vectorId, { fragmentId, localIndex });
  }

  return {
    config,
    fragments,
    activeFragmentId,
    totalVectors,
    totalDeleted,
    nextVectorId,
    nodeIdToVectorId,
    vectorIdToNodeId,
    vectorIdToLocation,
  };
}

// ============================================================================
// Helpers
// ============================================================================

function metricToNumber(metric: "cosine" | "euclidean" | "dot"): number {
  switch (metric) {
    case "cosine":
      return 0;
    case "euclidean":
      return 1;
    case "dot":
      return 2;
  }
}

function numberToMetric(n: number): "cosine" | "euclidean" | "dot" {
  switch (n) {
    case 0:
      return "cosine";
    case 1:
      return "euclidean";
    case 2:
      return "dot";
    default:
      throw new Error(`Unknown metric value: ${n}. Expected 0 (cosine), 1 (euclidean), or 2 (dot)`);
  }
}
