/**
 * Snapshot writer buffer - builds CSR snapshots in memory
 * 
 * This module provides a buffer-only version of snapshot building that returns
 * a Uint8Array instead of writing to a file. It's used by both the regular
 * snapshot writer and the single-file compactor.
 */

import {
  MAGIC_SNAPSHOT,
  MIN_READER_SNAPSHOT,
  SECTION_ALIGNMENT,
  VERSION_SNAPSHOT,
} from "../constants.ts";
import {
  type ETypeID,
  KEY_INDEX_ENTRY_SIZE,
  type LabelID,
  type NodeID,
  type PhysNode,
  PROP_VALUE_DISK_SIZE,
  type PropKeyID,
  type PropValue,
  PropValueTag,
  SECTION_ENTRY_SIZE,
  SectionId,
  SNAPSHOT_HEADER_SIZE,
  SnapshotFlags,
  type StringID,
} from "../types.ts";
import {
  alignUp,
  encodeString,
  f64ToU64Bits,
} from "../util/binary.ts";
import {
  type CompressionOptions,
  CompressionType,
  DEFAULT_COMPRESSION_OPTIONS,
  maybeCompress,
} from "../util/compression.ts";
import { crc32c } from "../util/crc.ts";
import { xxhash64String } from "../util/hash.ts";

// ============================================================================
// Builder types
// ============================================================================

export interface NodeData {
  nodeId: NodeID;
  key?: string;
  labels: LabelID[];
  props: Map<PropKeyID, PropValue>;
}

export interface EdgeData {
  src: NodeID;
  etype: ETypeID;
  dst: NodeID;
  props: Map<PropKeyID, PropValue>;
}

interface StringTable {
  strings: string[];
  stringToId: Map<string, StringID>;
}

interface KeyEntry {
  hash64: bigint;
  stringId: StringID;
  nodeId: NodeID;
}

// ============================================================================
// String interning
// ============================================================================

function createStringTable(): StringTable {
  return {
    strings: [""], // StringID 0 is reserved/empty
    stringToId: new Map([["", 0]]),
  };
}

function internString(table: StringTable, str: string): StringID {
  let id = table.stringToId.get(str);
  if (id !== undefined) return id;

  id = table.strings.length;
  table.strings.push(str);
  table.stringToId.set(str, id);
  return id;
}

// ============================================================================
// CSR building
// ============================================================================

interface CSRData {
  offsets: Uint32Array;
  dst: Uint32Array;
  etype: Uint32Array;
  // For in-edges: index back to out-edge
  outIndex?: Uint32Array;
}

function buildOutEdgesCSR(
  nodes: NodeData[],
  edges: EdgeData[],
  nodeIdToPhys: Map<NodeID, PhysNode>,
): CSRData {
  const numNodes = nodes.length;
  const numEdges = edges.length;

  // Count edges per node
  const counts = new Uint32Array(numNodes);
  for (const edge of edges) {
    const srcPhys = nodeIdToPhys.get(edge.src);
    if (srcPhys !== undefined) {
      counts[srcPhys]!++;
    }
  }

  // Build offsets (prefix sum)
  const offsets = new Uint32Array(numNodes + 1);
  for (let i = 0; i < numNodes; i++) {
    offsets[i + 1] = offsets[i]! + counts[i]!;
  }

  // Fill edge arrays (sort by etype, dst within each node)
  const dst = new Uint32Array(numEdges);
  const etype = new Uint32Array(numEdges);

  // Group edges by source node
  const edgesByNode: Map<PhysNode, { etype: ETypeID; dstPhys: PhysNode }[]> =
    new Map();
  for (const edge of edges) {
    const srcPhys = nodeIdToPhys.get(edge.src);
    const dstPhys = nodeIdToPhys.get(edge.dst);
    if (srcPhys !== undefined && dstPhys !== undefined) {
      let nodeEdges = edgesByNode.get(srcPhys);
      if (!nodeEdges) {
        nodeEdges = [];
        edgesByNode.set(srcPhys, nodeEdges);
      }
      nodeEdges.push({ etype: edge.etype, dstPhys });
    }
  }

  // Sort and write edges for each node
  for (const [srcPhys, nodeEdges] of edgesByNode) {
    // Sort by (etype, dst)
    nodeEdges.sort((a, b) => {
      if (a.etype !== b.etype) return a.etype - b.etype;
      return a.dstPhys - b.dstPhys;
    });

    let pos = offsets[srcPhys]!;
    for (const edge of nodeEdges) {
      dst[pos] = edge.dstPhys;
      etype[pos] = edge.etype;
      pos++;
    }
  }

  return { offsets, dst, etype };
}

function buildInEdgesCSR(
  nodes: NodeData[],
  outCSR: CSRData,
): CSRData {
  const numNodes = nodes.length;
  const numEdges = outCSR.dst.length;

  // Count in-edges per node
  const counts = new Uint32Array(numNodes);
  for (let i = 0; i < numEdges; i++) {
    counts[outCSR.dst[i]!]!++;
  }

  // Build offsets (prefix sum)
  const offsets = new Uint32Array(numNodes + 1);
  for (let i = 0; i < numNodes; i++) {
    offsets[i + 1] = offsets[i]! + counts[i]!;
  }

  // Fill in-edge arrays
  const src = new Uint32Array(numEdges);
  const etype = new Uint32Array(numEdges);
  const outIndex = new Uint32Array(numEdges);

  // Collect in-edges with their out-edge indices
  const inEdgesByNode: Map<
    PhysNode,
    { srcPhys: PhysNode; etype: ETypeID; outIdx: number }[]
  > = new Map();

  for (let srcPhys = 0; srcPhys < numNodes; srcPhys++) {
    const start = outCSR.offsets[srcPhys]!;
    const end = outCSR.offsets[srcPhys + 1]!;

    for (let outIdx = start; outIdx < end; outIdx++) {
      const dstPhys = outCSR.dst[outIdx]!;
      const edgeEtype = outCSR.etype[outIdx]!;

      let nodeInEdges = inEdgesByNode.get(dstPhys);
      if (!nodeInEdges) {
        nodeInEdges = [];
        inEdgesByNode.set(dstPhys, nodeInEdges);
      }
      nodeInEdges.push({ srcPhys, etype: edgeEtype, outIdx });
    }
  }

  // Sort and write in-edges for each node
  for (const [dstPhys, nodeInEdges] of inEdgesByNode) {
    // Sort by (etype, src)
    nodeInEdges.sort((a, b) => {
      if (a.etype !== b.etype) return a.etype - b.etype;
      return a.srcPhys - b.srcPhys;
    });

    let pos = offsets[dstPhys]!;
    for (const edge of nodeInEdges) {
      src[pos] = edge.srcPhys;
      etype[pos] = edge.etype;
      outIndex[pos] = edge.outIdx;
      pos++;
    }
  }

  return { offsets, dst: src, etype, outIndex };
}

// ============================================================================
// Key index building
// ============================================================================

interface KeyIndexData {
  entries: KeyEntry[];
  buckets: Uint32Array;
}

function buildKeyIndex(
  nodes: NodeData[],
  stringTable: StringTable,
  nodeKeyStrings: StringID[],
): KeyIndexData {
  const rawEntries: KeyEntry[] = [];

  for (let i = 0; i < nodes.length; i++) {
    const node = nodes[i]!;
    if (node.key) {
      const stringId = nodeKeyStrings[i]!;
      rawEntries.push({
        hash64: xxhash64String(node.key),
        stringId,
        nodeId: node.nodeId,
      });
    }
  }

  // Build bucket array for O(1) bucket lookup
  // Use 2x entries for reasonable load factor, minimum 16 buckets
  const numBuckets = Math.max(16, rawEntries.length * 2);
  const buckets = new Uint32Array(numBuckets + 1);

  if (rawEntries.length === 0) {
    // All buckets point to 0 (empty)
    return { entries: rawEntries, buckets };
  }

  // Sort entries by (bucket, hash64, stringId, nodeId) so entries in same bucket are contiguous
  const numBucketsBigInt = BigInt(numBuckets);
  rawEntries.sort((a, b) => {
    const aBucket = Number(a.hash64 % numBucketsBigInt);
    const bBucket = Number(b.hash64 % numBucketsBigInt);
    if (aBucket !== bBucket) return aBucket - bBucket;
    if (a.hash64 < b.hash64) return -1;
    if (a.hash64 > b.hash64) return 1;
    if (a.stringId !== b.stringId) return a.stringId - b.stringId;
    if (a.nodeId < b.nodeId) return -1;
    if (a.nodeId > b.nodeId) return 1;
    return 0;
  });

  // Count entries per bucket
  const counts = new Uint32Array(numBuckets);
  for (const entry of rawEntries) {
    const bucket = Number(entry.hash64 % numBucketsBigInt);
    counts[bucket]!++;
  }

  // Build offsets (prefix sum)
  for (let i = 0; i < numBuckets; i++) {
    buckets[i + 1] = buckets[i]! + counts[i]!;
  }

  return { entries: rawEntries, buckets };
}

// ============================================================================
// Property encoding
// ============================================================================

function encodePropValue(value: PropValue): { tag: number; payload: bigint } {
  switch (value.tag) {
    case PropValueTag.NULL:
      return { tag: 0, payload: 0n };
    case PropValueTag.BOOL:
      return { tag: 1, payload: value.value ? 1n : 0n };
    case PropValueTag.I64:
      return { tag: 2, payload: value.value };
    case PropValueTag.F64:
      return { tag: 3, payload: f64ToU64Bits(value.value) };
    case PropValueTag.STRING:
      // String values need to be interned first
      throw new Error(
        "String props must be converted to StringID before encoding",
      );
    case PropValueTag.VECTOR_F32:
      // Vector values are stored separately in vector store
      throw new Error(
        "Vector props must be stored in vector store, not in snapshot properties",
      );
    default: {
      // Exhaustive check - should never reach here
      const _exhaustive: never = value;
      throw new Error(`Unknown property value tag: ${(_exhaustive as PropValue).tag}`);
    }
  }
}

// ============================================================================
// Main snapshot building (buffer only)
// ============================================================================

export interface SnapshotBufferInput {
  generation: bigint;
  nodes: NodeData[];
  edges: EdgeData[];
  labels: Map<LabelID, string>;
  etypes: Map<ETypeID, string>;
  propkeys: Map<PropKeyID, string>;
  /** Compression options for snapshot sections */
  compression?: CompressionOptions;
}

/**
 * Build a snapshot and return as a Uint8Array buffer.
 * 
 * This is the core snapshot building logic used by both the file-based
 * snapshot writer and the single-file compactor.
 */
export function buildSnapshotBuffer(input: SnapshotBufferInput): Uint8Array {
  const { generation, nodes, edges, labels, etypes, propkeys } = input;

  // Sort nodes by NodeID for deterministic ordering
  nodes.sort((a, b) =>
    a.nodeId < b.nodeId ? -1 : a.nodeId > b.nodeId ? 1 : 0,
  );

  // Build mappings
  const physToNodeId: NodeID[] = nodes.map((n) => n.nodeId);
  const nodeIdToPhys: Map<NodeID, PhysNode> = new Map();
  let maxNodeId = 0;

  for (let i = 0; i < nodes.length; i++) {
    const nodeId = nodes[i]!.nodeId;
    nodeIdToPhys.set(nodeId, i);
    if (nodeId > maxNodeId) maxNodeId = nodeId;
  }

  // Build string table
  const stringTable = createStringTable();

  // Intern label names
  const labelStringIds: StringID[] = [0]; // LabelID 0 reserved
  for (let i = 1; i <= labels.size; i++) {
    const name = labels.get(i);
    labelStringIds.push(name ? internString(stringTable, name) : 0);
  }

  // Intern etype names
  const etypeStringIds: StringID[] = [0]; // ETypeID 0 reserved
  for (let i = 1; i <= etypes.size; i++) {
    const name = etypes.get(i);
    etypeStringIds.push(name ? internString(stringTable, name) : 0);
  }

  // Intern propkey names
  const propkeyStringIds: StringID[] = [0]; // PropKeyID 0 reserved
  for (let i = 1; i <= propkeys.size; i++) {
    const name = propkeys.get(i);
    propkeyStringIds.push(name ? internString(stringTable, name) : 0);
  }

  // Intern node keys
  const nodeKeyStrings: StringID[] = [];
  for (const node of nodes) {
    nodeKeyStrings.push(node.key ? internString(stringTable, node.key) : 0);
  }

  // Build CSR
  const outCSR = buildOutEdgesCSR(nodes, edges, nodeIdToPhys);
  const inCSR = buildInEdgesCSR(nodes, outCSR);

  // Build key index with buckets
  const keyIndex = buildKeyIndex(nodes, stringTable, nodeKeyStrings);

  // Intern string property values before building property arrays
  for (const node of nodes) {
    for (const [, value] of node.props) {
      if (value.tag === PropValueTag.STRING) {
        internString(stringTable, value.value);
      }
    }
  }
  for (const edge of edges) {
    for (const [, value] of edge.props) {
      if (value.tag === PropValueTag.STRING) {
        internString(stringTable, value.value);
      }
    }
  }

  // Build property arrays
  const hasProperties =
    nodes.some((n) => n.props.size > 0) || edges.some((e) => e.props.size > 0);

  // Now build the binary snapshot
  const numNodes = nodes.length;
  const numEdges = edges.length;
  const numStrings = stringTable.strings.length;

  // Get compression options
  const compressionOpts = input.compression ?? DEFAULT_COMPRESSION_OPTIONS;

  // Calculate section sizes and offsets
  const sectionData: {
    id: SectionId;
    data: Uint8Array;
    compression: CompressionType;
    uncompressedSize: number;
  }[] = [];

  // Helper to add section (with optional compression)
  function addSection(id: SectionId, data: Uint8Array) {
    const result = maybeCompress(data, compressionOpts);
    sectionData.push({
      id,
      data: result.data,
      compression: result.type,
      uncompressedSize: data.length,
    });
  }

  // phys_to_nodeid: u64[num_nodes]
  {
    const data = new Uint8Array(numNodes * 8);
    const view = new DataView(data.buffer);
    for (let i = 0; i < numNodes; i++) {
      view.setBigUint64(i * 8, BigInt(physToNodeId[i]!), true);
    }
    addSection(SectionId.PHYS_TO_NODEID, data);
  }

  // nodeid_to_phys: i32[max_node_id + 1]
  {
    const size = maxNodeId + 1;
    const data = new Uint8Array(size * 4);
    const view = new DataView(data.buffer);
    // Initialize all to -1
    for (let i = 0; i < size; i++) {
      view.setInt32(i * 4, -1, true);
    }
    // Set valid mappings
    for (const [nodeId, phys] of nodeIdToPhys) {
      view.setInt32(nodeId * 4, phys, true);
    }
    addSection(SectionId.NODEID_TO_PHYS, data);
  }

  // out_offsets: u32[num_nodes + 1]
  {
    const data = new Uint8Array((numNodes + 1) * 4);
    new Uint8Array(data.buffer).set(new Uint8Array(outCSR.offsets.buffer));
    addSection(SectionId.OUT_OFFSETS, data);
  }

  // out_dst: u32[num_edges]
  {
    const data = new Uint8Array(numEdges * 4);
    new Uint8Array(data.buffer).set(new Uint8Array(outCSR.dst.buffer));
    addSection(SectionId.OUT_DST, data);
  }

  // out_etype: u32[num_edges]
  {
    const data = new Uint8Array(numEdges * 4);
    new Uint8Array(data.buffer).set(new Uint8Array(outCSR.etype.buffer));
    addSection(SectionId.OUT_ETYPE, data);
  }

  // in_offsets: u32[num_nodes + 1]
  {
    const data = new Uint8Array((numNodes + 1) * 4);
    new Uint8Array(data.buffer).set(new Uint8Array(inCSR.offsets.buffer));
    addSection(SectionId.IN_OFFSETS, data);
  }

  // in_src: u32[num_edges]
  {
    const data = new Uint8Array(numEdges * 4);
    new Uint8Array(data.buffer).set(new Uint8Array(inCSR.dst.buffer)); // dst is src for in-edges
    addSection(SectionId.IN_SRC, data);
  }

  // in_etype: u32[num_edges]
  {
    const data = new Uint8Array(numEdges * 4);
    new Uint8Array(data.buffer).set(new Uint8Array(inCSR.etype.buffer));
    addSection(SectionId.IN_ETYPE, data);
  }

  // in_out_index: u32[num_edges]
  {
    const data = new Uint8Array(numEdges * 4);
    if (inCSR.outIndex) {
      new Uint8Array(data.buffer).set(new Uint8Array(inCSR.outIndex.buffer));
    }
    addSection(SectionId.IN_OUT_INDEX, data);
  }

  // string_offsets: u32[num_strings + 1]
  // string_bytes: u8[...]
  {
    const encodedStrings = stringTable.strings.map((s) => encodeString(s));
    const totalBytes = encodedStrings.reduce((sum, s) => sum + s.length, 0);

    const offsetsData = new Uint8Array((numStrings + 1) * 4);
    const offsetsView = new DataView(offsetsData.buffer);
    const bytesData = new Uint8Array(totalBytes);

    let byteOffset = 0;
    for (let i = 0; i < numStrings; i++) {
      offsetsView.setUint32(i * 4, byteOffset, true);
      const encoded = encodedStrings[i]!;
      bytesData.set(encoded, byteOffset);
      byteOffset += encoded.length;
    }
    offsetsView.setUint32(numStrings * 4, byteOffset, true);

    addSection(SectionId.STRING_OFFSETS, offsetsData);
    addSection(SectionId.STRING_BYTES, bytesData);
  }

  // label_string_ids: u32[num_labels + 1]
  {
    const data = new Uint8Array(labelStringIds.length * 4);
    const view = new DataView(data.buffer);
    for (let i = 0; i < labelStringIds.length; i++) {
      view.setUint32(i * 4, labelStringIds[i]!, true);
    }
    addSection(SectionId.LABEL_STRING_IDS, data);
  }

  // etype_string_ids: u32[num_etypes + 1]
  {
    const data = new Uint8Array(etypeStringIds.length * 4);
    const view = new DataView(data.buffer);
    for (let i = 0; i < etypeStringIds.length; i++) {
      view.setUint32(i * 4, etypeStringIds[i]!, true);
    }
    addSection(SectionId.ETYPE_STRING_IDS, data);
  }

  // propkey_string_ids: u32[num_propkeys + 1]
  {
    const data = new Uint8Array(propkeyStringIds.length * 4);
    const view = new DataView(data.buffer);
    for (let i = 0; i < propkeyStringIds.length; i++) {
      view.setUint32(i * 4, propkeyStringIds[i]!, true);
    }
    addSection(SectionId.PROPKEY_STRING_IDS, data);
  }

  // node_key_string: u32[num_nodes]
  {
    const data = new Uint8Array(numNodes * 4);
    const view = new DataView(data.buffer);
    for (let i = 0; i < numNodes; i++) {
      view.setUint32(i * 4, nodeKeyStrings[i]!, true);
    }
    addSection(SectionId.NODE_KEY_STRING, data);
  }

  // key_entries
  {
    const data = new Uint8Array(keyIndex.entries.length * KEY_INDEX_ENTRY_SIZE);
    const view = new DataView(data.buffer);
    for (let i = 0; i < keyIndex.entries.length; i++) {
      const entry = keyIndex.entries[i]!;
      const offset = i * KEY_INDEX_ENTRY_SIZE;
      view.setBigUint64(offset, entry.hash64, true);
      view.setUint32(offset + 8, entry.stringId, true);
      view.setUint32(offset + 12, 0, true); // reserved
      view.setBigUint64(offset + 16, BigInt(entry.nodeId), true);
    }
    addSection(SectionId.KEY_ENTRIES, data);
  }

  // key_buckets: u32[num_buckets + 1] (CSR-style offsets)
  {
    const data = new Uint8Array(keyIndex.buckets.byteLength);
    new Uint8Array(data.buffer).set(new Uint8Array(keyIndex.buckets.buffer));
    addSection(SectionId.KEY_BUCKETS, data);
  }

  // Node property sections
  {
    const nodePropOffsets = new Uint32Array(numNodes + 1);
    const nodePropKeysList: number[] = [];
    const nodePropValsList: { tag: number; payload: bigint }[] = [];

    for (let i = 0; i < numNodes; i++) {
      nodePropOffsets[i] = nodePropKeysList.length;
      const node = nodes[i]!;

      // Sort props by key ID for deterministic output
      const sortedProps = [...node.props.entries()].sort((a, b) => a[0] - b[0]);
      for (const [keyId, value] of sortedProps) {
        nodePropKeysList.push(keyId);
        if (value.tag === PropValueTag.STRING) {
          // Convert string to StringID
          const stringId = stringTable.stringToId.get(value.value) ?? 0;
          nodePropValsList.push({
            tag: PropValueTag.STRING,
            payload: BigInt(stringId),
          });
        } else {
          nodePropValsList.push(encodePropValue(value));
        }
      }
    }
    nodePropOffsets[numNodes] = nodePropKeysList.length;

    // Write offsets
    const offsetsData = new Uint8Array((numNodes + 1) * 4);
    new Uint8Array(offsetsData.buffer).set(
      new Uint8Array(nodePropOffsets.buffer),
    );
    addSection(SectionId.NODE_PROP_OFFSETS, offsetsData);

    // Write keys
    const keysData = new Uint8Array(nodePropKeysList.length * 4);
    const keysView = new DataView(keysData.buffer);
    for (let i = 0; i < nodePropKeysList.length; i++) {
      keysView.setUint32(i * 4, nodePropKeysList[i]!, true);
    }
    addSection(SectionId.NODE_PROP_KEYS, keysData);

    // Write values (16 bytes each: 1 byte tag, 7 bytes pad, 8 bytes payload)
    const valsData = new Uint8Array(
      nodePropValsList.length * PROP_VALUE_DISK_SIZE,
    );
    const valsView = new DataView(valsData.buffer);
    for (let i = 0; i < nodePropValsList.length; i++) {
      const val = nodePropValsList[i]!;
      const offset = i * PROP_VALUE_DISK_SIZE;
      valsView.setUint8(offset, val.tag);
      // 7 bytes of padding (already 0)
      valsView.setBigUint64(offset + 8, val.payload, true);
    }
    addSection(SectionId.NODE_PROP_VALS, valsData);
  }

  // Edge property sections
  {
    // Build a mapping from edge key to edge props
    const edgePropMap = new Map<string, Map<PropKeyID, PropValue>>();
    for (const edge of edges) {
      if (edge.props.size > 0) {
        const srcPhys = nodeIdToPhys.get(edge.src);
        const dstPhys = nodeIdToPhys.get(edge.dst);
        if (srcPhys !== undefined && dstPhys !== undefined) {
          // Use CSR order key: srcPhys, etype, dstPhys
          const key = `${srcPhys}:${edge.etype}:${dstPhys}`;
          edgePropMap.set(key, edge.props);
        }
      }
    }

    const edgePropOffsets = new Uint32Array(numEdges + 1);
    const edgePropKeysList: number[] = [];
    const edgePropValsList: { tag: number; payload: bigint }[] = [];

    // Iterate edges in CSR order
    let edgeIdx = 0;
    for (let srcPhys = 0; srcPhys < numNodes; srcPhys++) {
      const start = outCSR.offsets[srcPhys]!;
      const end = outCSR.offsets[srcPhys + 1]!;

      for (let i = start; i < end; i++) {
        edgePropOffsets[edgeIdx] = edgePropKeysList.length;
        const dstPhys = outCSR.dst[i]!;
        const etype = outCSR.etype[i]!;
        const key = `${srcPhys}:${etype}:${dstPhys}`;

        const props = edgePropMap.get(key);
        if (props) {
          const sortedProps = [...props.entries()].sort((a, b) => a[0] - b[0]);
          for (const [keyId, value] of sortedProps) {
            edgePropKeysList.push(keyId);
            if (value.tag === PropValueTag.STRING) {
              const stringId = stringTable.stringToId.get(value.value) ?? 0;
              edgePropValsList.push({
                tag: PropValueTag.STRING,
                payload: BigInt(stringId),
              });
            } else {
              edgePropValsList.push(encodePropValue(value));
            }
          }
        }
        edgeIdx++;
      }
    }
    edgePropOffsets[numEdges] = edgePropKeysList.length;

    // Write offsets
    const offsetsData = new Uint8Array((numEdges + 1) * 4);
    new Uint8Array(offsetsData.buffer).set(
      new Uint8Array(edgePropOffsets.buffer),
    );
    addSection(SectionId.EDGE_PROP_OFFSETS, offsetsData);

    // Write keys
    const keysData = new Uint8Array(edgePropKeysList.length * 4);
    const keysView = new DataView(keysData.buffer);
    for (let i = 0; i < edgePropKeysList.length; i++) {
      keysView.setUint32(i * 4, edgePropKeysList[i]!, true);
    }
    addSection(SectionId.EDGE_PROP_KEYS, keysData);

    // Write values
    const valsData = new Uint8Array(
      edgePropValsList.length * PROP_VALUE_DISK_SIZE,
    );
    const valsView = new DataView(valsData.buffer);
    for (let i = 0; i < edgePropValsList.length; i++) {
      const val = edgePropValsList[i]!;
      const offset = i * PROP_VALUE_DISK_SIZE;
      valsView.setUint8(offset, val.tag);
      valsView.setBigUint64(offset + 8, val.payload, true);
    }
    addSection(SectionId.EDGE_PROP_VALS, valsData);
  }

  // Calculate total size with alignment
  const headerSize = SNAPSHOT_HEADER_SIZE;
  const sectionTableSize = SectionId._COUNT * SECTION_ENTRY_SIZE;
  let dataOffset = alignUp(headerSize + sectionTableSize, SECTION_ALIGNMENT);

  const sectionOffsets: Map<
    SectionId,
    {
      offset: bigint;
      length: bigint;
      compression: CompressionType;
      uncompressedSize: number;
    }
  > = new Map();

  for (const { id, data, compression, uncompressedSize } of sectionData) {
    sectionOffsets.set(id, {
      offset: BigInt(dataOffset),
      length: BigInt(data.length),
      compression,
      uncompressedSize,
    });
    dataOffset = alignUp(dataOffset + data.length, SECTION_ALIGNMENT);
  }

  // Build final buffer
  const totalSize = dataOffset + 4; // +4 for footer CRC
  const buffer = new Uint8Array(totalSize);
  const view = new DataView(buffer.buffer);

  // Write header
  let offset = 0;
  view.setUint32(offset, MAGIC_SNAPSHOT, true);
  offset += 4;
  view.setUint32(offset, VERSION_SNAPSHOT, true);
  offset += 4;
  view.setUint32(offset, MIN_READER_SNAPSHOT, true);
  offset += 4;

  // Flags
  let flags = SnapshotFlags.HAS_IN_EDGES;
  if (hasProperties) flags |= SnapshotFlags.HAS_PROPERTIES;
  if (keyIndex.buckets.length > 1) flags |= SnapshotFlags.HAS_KEY_BUCKETS;
  view.setUint32(offset, flags, true);
  offset += 4;

  view.setBigUint64(offset, generation, true);
  offset += 8;
  view.setBigUint64(offset, BigInt(Date.now()) * 1000000n, true);
  offset += 8; // unix ns
  view.setBigUint64(offset, BigInt(numNodes), true);
  offset += 8;
  view.setBigUint64(offset, BigInt(numEdges), true);
  offset += 8;
  view.setBigUint64(offset, BigInt(maxNodeId), true);
  offset += 8;
  view.setBigUint64(offset, BigInt(labels.size), true);
  offset += 8;
  view.setBigUint64(offset, BigInt(etypes.size), true);
  offset += 8;
  view.setBigUint64(offset, BigInt(propkeys.size), true);
  offset += 8;
  view.setBigUint64(offset, BigInt(numStrings), true);
  offset += 8;

  // Write section table
  offset = headerSize;
  for (let id = 0; id < SectionId._COUNT; id++) {
    const section = sectionOffsets.get(id) ?? {
      offset: 0n,
      length: 0n,
      compression: CompressionType.NONE,
      uncompressedSize: 0,
    };
    view.setBigUint64(offset, section.offset, true);
    offset += 8;
    view.setBigUint64(offset, section.length, true);
    offset += 8;
    view.setUint32(offset, section.compression, true);
    offset += 4; // compression type
    view.setUint32(offset, section.uncompressedSize, true);
    offset += 4; // uncompressed size (used when compression != NONE)
  }

  // Write section data
  for (const { id, data } of sectionData) {
    const section = sectionOffsets.get(id)!;
    buffer.set(data, Number(section.offset));
  }

  // Write footer CRC (over entire file except CRC itself)
  const footerCrc = crc32c(buffer.subarray(0, totalSize - 4));
  view.setUint32(totalSize - 4, footerCrc, true);

  return buffer;
}
