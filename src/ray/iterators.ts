/**
 * Merged neighbor iterators - combines snapshot edges with delta patches
 * 
 * Optimization: Uses Set-based lookups for O(1) edge existence checks in delta
 */

import { isEdgeDeleted, isNodeDeleted } from "../core/delta.ts";
import {
  getInEdges,
  getNodeId,
  getOutEdges,
  getPhysNode,
} from "../core/snapshot-reader.ts";
import type {
  DeltaState,
  Edge,
  EdgePatch,
  ETypeID,
  NodeID,
  SnapshotData,
} from "../types.ts";
import { readU32At } from "../util/binary.ts";

// ============================================================================
// Edge iteration with merged view
// ============================================================================

interface MergedEdge {
  etype: ETypeID;
  other: NodeID;
}

/**
 * Generate a unique key for an edge (etype, other) pair
 * Used for O(1) edge lookup in Sets
 */
function edgeKey(etype: ETypeID, other: NodeID): bigint {
  // Combine etype (u32) and other (safe integer up to 2^53-1) into a bigint
  // etype << 53n ensures no collision with other values
  return (BigInt(etype) << 53n) | BigInt(other);
}

/**
 * Build a Set of edge keys from patches for O(1) lookup
 */
function buildEdgeSet(patches: EdgePatch[]): Set<bigint> {
  const set = new Set<bigint>();
  for (const patch of patches) {
    set.add(edgeKey(patch.etype, patch.other));
  }
  return set;
}

/**
 * Merge snapshot edges with delta patches
 * Read order: snapshot - del + add
 */
function* mergeEdges(
  snapshotEdges: { etype: ETypeID; other: NodeID }[],
  delPatches: EdgePatch[],
  addPatches: EdgePatch[],
): Generator<MergedEdge> {
  // Build Set for O(1) lookup of deleted edges
  const deleted = buildEdgeSet(delPatches);

  // Yield snapshot edges that aren't deleted
  for (const edge of snapshotEdges) {
    const key = edgeKey(edge.etype, edge.other);
    if (!deleted.has(key)) {
      yield edge;
    }
  }

  // Yield added edges
  for (const patch of addPatches) {
    yield { etype: patch.etype, other: patch.other };
  }
}

/**
 * Get out-neighbors with merged view (snapshot + delta)
 */
export function* neighborsOut(
  snapshot: SnapshotData | null,
  delta: DeltaState,
  nodeId: NodeID,
  filterEtype?: ETypeID,
): Generator<Edge> {
  // Check if node is deleted
  if (isNodeDeleted(delta, nodeId)) {
    return;
  }

  // Get snapshot edges
  const snapshotEdges: { etype: ETypeID; other: NodeID }[] = [];

  if (snapshot) {
    const phys = getPhysNode(snapshot, nodeId);
    if (phys >= 0) {
      const edges = getOutEdges(snapshot, phys);
      for (const edge of edges) {
        const dstNodeId = getNodeId(snapshot, edge.dst);
        // Skip if destination node is deleted
        if (!isNodeDeleted(delta, dstNodeId)) {
          snapshotEdges.push({ etype: edge.etype, other: dstNodeId });
        }
      }
    }
  }

  // Get delta patches
  const delPatches = delta.outDel.get(nodeId) ?? [];
  const addPatches = delta.outAdd.get(nodeId) ?? [];

  // Filter added patches for deleted destinations
  const filteredAddPatches = addPatches.filter(
    (p) => !isNodeDeleted(delta, p.other),
  );

  // Merge and yield
  for (const edge of mergeEdges(
    snapshotEdges,
    delPatches,
    filteredAddPatches,
  )) {
    if (filterEtype === undefined || edge.etype === filterEtype) {
      yield { src: nodeId, etype: edge.etype, dst: edge.other };
    }
  }
}

/**
 * Get in-neighbors with merged view (snapshot + delta)
 */
export function* neighborsIn(
  snapshot: SnapshotData | null,
  delta: DeltaState,
  nodeId: NodeID,
  filterEtype?: ETypeID,
): Generator<Edge> {
  // Check if node is deleted
  if (isNodeDeleted(delta, nodeId)) {
    return;
  }

  // Get snapshot edges
  const snapshotEdges: { etype: ETypeID; other: NodeID }[] = [];

  if (snapshot) {
    const phys = getPhysNode(snapshot, nodeId);
    if (phys >= 0) {
      const edges = getInEdges(snapshot, phys);
      if (edges) {
        for (const edge of edges) {
          const srcNodeId = getNodeId(snapshot, edge.src);
          // Skip if source node is deleted
          if (!isNodeDeleted(delta, srcNodeId)) {
            snapshotEdges.push({ etype: edge.etype, other: srcNodeId });
          }
        }
      }
    }
  }

  // Get delta patches
  const delPatches = delta.inDel.get(nodeId) ?? [];
  const addPatches = delta.inAdd.get(nodeId) ?? [];

  // Filter added patches for deleted sources
  const filteredAddPatches = addPatches.filter(
    (p) => !isNodeDeleted(delta, p.other),
  );

  // Merge and yield
  for (const edge of mergeEdges(
    snapshotEdges,
    delPatches,
    filteredAddPatches,
  )) {
    if (filterEtype === undefined || edge.etype === filterEtype) {
      yield { src: edge.other, etype: edge.etype, dst: nodeId };
    }
  }
}

/**
 * Check if an edge exists with merged view
 * 
 * Optimization: Uses O(1) bigint Set lookup instead of O(n) linear scan
 */
export function hasEdgeMerged(
  snapshot: SnapshotData | null,
  delta: DeltaState,
  src: NodeID,
  etype: ETypeID,
  dst: NodeID,
): boolean {
  // Check if either endpoint is deleted
  if (isNodeDeleted(delta, src) || isNodeDeleted(delta, dst)) {
    return false;
  }

  // Check if edge is deleted in delta
  if (isEdgeDeleted(delta, src, etype, dst)) {
    return false;
  }

  // Check if edge is added in delta - O(1) lookup using bigint key
  const addPatches = delta.outAdd.get(src);
  if (addPatches) {
    const targetKey = edgeKey(etype, dst);
    // For small patch arrays, linear scan is faster than building a Set
    // For larger arrays, we could maintain a persistent Set in DeltaState
    if (addPatches.length <= 10) {
      for (const patch of addPatches) {
        if (patch.etype === etype && patch.other === dst) {
          return true;
        }
      }
    } else {
      // Build Set for O(1) lookup on larger arrays
      const addSet = buildEdgeSet(addPatches);
      if (addSet.has(targetKey)) {
        return true;
      }
    }
  }

  // Check snapshot
  if (snapshot) {
    const srcPhys = getPhysNode(snapshot, src);
    const dstPhys = getPhysNode(snapshot, dst);

    if (srcPhys >= 0 && dstPhys >= 0) {
      // Binary search in snapshot
      const start = readU32At(snapshot.outOffsets, srcPhys);
      const end = readU32At(snapshot.outOffsets, srcPhys + 1);

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
          return true;
        }
      }
    }
  }

  return false;
}

/**
 * Get out-degree with merged view
 */
export function outDegreeMerged(
  snapshot: SnapshotData | null,
  delta: DeltaState,
  nodeId: NodeID,
  filterEtype?: ETypeID,
): number {
  let count = 0;
  for (const _ of neighborsOut(snapshot, delta, nodeId, filterEtype)) {
    count++;
  }
  return count;
}

/**
 * Get in-degree with merged view
 */
export function inDegreeMerged(
  snapshot: SnapshotData | null,
  delta: DeltaState,
  nodeId: NodeID,
  filterEtype?: ETypeID,
): number {
  let count = 0;
  for (const _ of neighborsIn(snapshot, delta, nodeId, filterEtype)) {
    count++;
  }
  return count;
}
