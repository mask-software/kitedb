/**
 * Delta overlay - in-memory patches for uncommitted and committed changes
 */

import { DELTA_SET_UPGRADE_THRESHOLD } from "../constants.ts";
import type {
  DeltaState,
  EdgePatch,
  ETypeID,
  LabelID,
  NodeDelta,
  NodeID,
  PropKeyID,
  PropValue,
} from "../types.ts";

// ============================================================================
// Delta creation
// ============================================================================

/**
 * Create an empty delta state
 */
export function createDelta(): DeltaState {
  return {
    createdNodes: new Map(),
    deletedNodes: new Set(),
    modifiedNodes: new Map(),
    outAdd: new Map(),
    outDel: new Map(),
    inAdd: new Map(),
    inDel: new Map(),
    edgeProps: new Map(),
    newLabels: new Map(),
    newEtypes: new Map(),
    newPropkeys: new Map(),
    keyIndex: new Map(),
    keyIndexDeleted: new Set(),
  };
}

/**
 * Create an empty node delta
 */
export function createNodeDelta(): NodeDelta {
  return {
    labels: new Set(),
    labelsDeleted: new Set(),
    props: new Map(),
  };
}

// ============================================================================
// Edge patch helpers
// ============================================================================

/**
 * Compare two edge patches for sorting
 */
function compareEdgePatch(a: EdgePatch, b: EdgePatch): number {
  if (a.etype !== b.etype) return a.etype - b.etype;
  if (a.other < b.other) return -1;
  if (a.other > b.other) return 1;
  return 0;
}

/**
 * Binary search for edge patch in sorted array
 */
function findEdgePatch(
  patches: EdgePatch[],
  etype: ETypeID,
  other: NodeID,
): number {
  let lo = 0;
  let hi = patches.length;

  while (lo < hi) {
    const mid = (lo + hi) >>> 1;
    const patch = patches[mid]!;

    if (patch.etype < etype || (patch.etype === etype && patch.other < other)) {
      lo = mid + 1;
    } else {
      hi = mid;
    }
  }

  return lo;
}

/**
 * Check if edge patch exists in sorted array
 */
function hasEdgePatch(
  patches: EdgePatch[],
  etype: ETypeID,
  other: NodeID,
): boolean {
  const idx = findEdgePatch(patches, etype, other);
  if (idx >= patches.length) return false;
  const patch = patches[idx]!;
  return patch.etype === etype && patch.other === other;
}

/**
 * Insert edge patch into sorted array (maintains sorted order)
 */
function insertEdgePatch(
  patches: EdgePatch[],
  etype: ETypeID,
  other: NodeID,
): boolean {
  const idx = findEdgePatch(patches, etype, other);

  // Check if already exists
  if (idx < patches.length) {
    const existing = patches[idx]!;
    if (existing.etype === etype && existing.other === other) {
      return false; // Already exists
    }
  }

  // Insert at position
  patches.splice(idx, 0, { etype, other });
  return true;
}

/**
 * Remove edge patch from sorted array
 */
function removeEdgePatch(
  patches: EdgePatch[],
  etype: ETypeID,
  other: NodeID,
): boolean {
  const idx = findEdgePatch(patches, etype, other);

  if (idx < patches.length) {
    const existing = patches[idx]!;
    if (existing.etype === etype && existing.other === other) {
      patches.splice(idx, 1);
      return true;
    }
  }

  return false;
}

// ============================================================================
// Edge operations with cancellation rules
// ============================================================================

/**
 * Get or create edge patch array for a node
 */
function getOrCreatePatches(
  map: Map<NodeID, EdgePatch[]>,
  nodeId: NodeID,
): EdgePatch[] {
  let patches = map.get(nodeId);
  if (!patches) {
    patches = [];
    map.set(nodeId, patches);
  }
  return patches;
}

/**
 * Add an edge with proper cancellation
 * Rule: if in del set, cancel delete; else add to add set
 */
export function addEdge(
  delta: DeltaState,
  src: NodeID,
  etype: ETypeID,
  dst: NodeID,
): void {
  // Out-edge: src -> dst
  const outDelPatches = delta.outDel.get(src);
  if (outDelPatches && removeEdgePatch(outDelPatches, etype, dst)) {
    // Cancelled a pending delete
    if (outDelPatches.length === 0) delta.outDel.delete(src);
  } else {
    // Add to add set
    const outAddPatches = getOrCreatePatches(delta.outAdd, src);
    insertEdgePatch(outAddPatches, etype, dst);
  }

  // In-edge: dst <- src
  const inDelPatches = delta.inDel.get(dst);
  if (inDelPatches && removeEdgePatch(inDelPatches, etype, src)) {
    // Cancelled a pending delete
    if (inDelPatches.length === 0) delta.inDel.delete(dst);
  } else {
    // Add to add set
    const inAddPatches = getOrCreatePatches(delta.inAdd, dst);
    insertEdgePatch(inAddPatches, etype, src);
  }
}

/**
 * Delete an edge with proper cancellation
 * Rule: if in add set, cancel add; else add to del set
 */
export function deleteEdge(
  delta: DeltaState,
  src: NodeID,
  etype: ETypeID,
  dst: NodeID,
): void {
  // Out-edge: src -> dst
  const outAddPatches = delta.outAdd.get(src);
  if (outAddPatches && removeEdgePatch(outAddPatches, etype, dst)) {
    // Cancelled a pending add
    if (outAddPatches.length === 0) delta.outAdd.delete(src);
  } else {
    // Add to del set
    const outDelPatches = getOrCreatePatches(delta.outDel, src);
    insertEdgePatch(outDelPatches, etype, dst);
  }

  // In-edge: dst <- src
  const inAddPatches = delta.inAdd.get(dst);
  if (inAddPatches && removeEdgePatch(inAddPatches, etype, src)) {
    // Cancelled a pending add
    if (inAddPatches.length === 0) delta.inAdd.delete(dst);
  } else {
    // Add to del set
    const inDelPatches = getOrCreatePatches(delta.inDel, dst);
    insertEdgePatch(inDelPatches, etype, src);
  }
}

/**
 * Check if an edge is deleted in the delta
 */
export function isEdgeDeleted(
  delta: DeltaState,
  src: NodeID,
  etype: ETypeID,
  dst: NodeID,
): boolean {
  const patches = delta.outDel.get(src);
  if (!patches) return false;
  return hasEdgePatch(patches, etype, dst);
}

/**
 * Check if an edge is added in the delta
 */
export function isEdgeAdded(
  delta: DeltaState,
  src: NodeID,
  etype: ETypeID,
  dst: NodeID,
): boolean {
  const patches = delta.outAdd.get(src);
  if (!patches) return false;
  return hasEdgePatch(patches, etype, dst);
}

// ============================================================================
// Node operations
// ============================================================================

/**
 * Create a new node in delta
 */
export function createNode(
  delta: DeltaState,
  nodeId: NodeID,
  key?: string,
): void {
  const nodeDelta = createNodeDelta();
  if (key) {
    nodeDelta.key = key;
    delta.keyIndex.set(key, nodeId);
  }
  delta.createdNodes.set(nodeId, nodeDelta);
}

/**
 * Remove edges involving a node from all edge maps
 */
function removeEdgesInvolving(delta: DeltaState, nodeId: NodeID): void {
  // Remove edges FROM this node
  delta.outAdd.delete(nodeId);
  delta.outDel.delete(nodeId);
  delta.inAdd.delete(nodeId);
  delta.inDel.delete(nodeId);

  // Remove edges TO this node (from other nodes' outAdd/outDel)
  for (const [src, patches] of delta.outAdd) {
    const filtered = patches.filter((p) => p.other !== nodeId);
    if (filtered.length === 0) {
      delta.outAdd.delete(src);
    } else if (filtered.length !== patches.length) {
      delta.outAdd.set(src, filtered);
    }
  }

  for (const [src, patches] of delta.outDel) {
    const filtered = patches.filter((p) => p.other !== nodeId);
    if (filtered.length === 0) {
      delta.outDel.delete(src);
    } else if (filtered.length !== patches.length) {
      delta.outDel.set(src, filtered);
    }
  }

  // Remove in-edges FROM this node (from other nodes' inAdd/inDel)
  for (const [dst, patches] of delta.inAdd) {
    const filtered = patches.filter((p) => p.other !== nodeId);
    if (filtered.length === 0) {
      delta.inAdd.delete(dst);
    } else if (filtered.length !== patches.length) {
      delta.inAdd.set(dst, filtered);
    }
  }

  for (const [dst, patches] of delta.inDel) {
    const filtered = patches.filter((p) => p.other !== nodeId);
    if (filtered.length === 0) {
      delta.inDel.delete(dst);
    } else if (filtered.length !== patches.length) {
      delta.inDel.set(dst, filtered);
    }
  }
}

/**
 * Delete a node in delta
 */
export function deleteNode(delta: DeltaState, nodeId: NodeID): boolean {
  // If it was created in this delta, just remove it
  const created = delta.createdNodes.get(nodeId);
  if (created) {
    if (created.key) {
      delta.keyIndex.delete(created.key);
    }
    delta.createdNodes.delete(nodeId);

    // Also remove any edges involving this node
    removeEdgesInvolving(delta, nodeId);

    return true;
  }

  // Check if already deleted
  if (delta.deletedNodes.has(nodeId)) {
    return false;
  }

  // Mark as deleted
  delta.deletedNodes.add(nodeId);

  // Remove any modifications for this node
  const modified = delta.modifiedNodes.get(nodeId);
  if (modified?.key) {
    delta.keyIndex.delete(modified.key);
  }
  delta.modifiedNodes.delete(nodeId);

  return true;
}

/**
 * Check if a node is deleted in delta
 */
export function isNodeDeleted(delta: DeltaState, nodeId: NodeID): boolean {
  return delta.deletedNodes.has(nodeId);
}

/**
 * Check if a node exists in delta (was created)
 */
export function isNodeCreated(delta: DeltaState, nodeId: NodeID): boolean {
  return delta.createdNodes.has(nodeId);
}

/**
 * Get node delta for a node (created or modified)
 */
export function getNodeDelta(
  delta: DeltaState,
  nodeId: NodeID,
): NodeDelta | null {
  return (
    delta.createdNodes.get(nodeId) ?? delta.modifiedNodes.get(nodeId) ?? null
  );
}

/**
 * Get or create node delta for modification
 */
export function getOrCreateNodeDelta(
  delta: DeltaState,
  nodeId: NodeID,
  isNew: boolean,
): NodeDelta {
  if (isNew) {
    let nodeDelta = delta.createdNodes.get(nodeId);
    if (!nodeDelta) {
      nodeDelta = createNodeDelta();
      delta.createdNodes.set(nodeId, nodeDelta);
    }
    return nodeDelta;
  }

  // Check if node was created in delta - if so, modify that entry
  const createdDelta = delta.createdNodes.get(nodeId);
  if (createdDelta) {
    return createdDelta;
  }

  let nodeDelta = delta.modifiedNodes.get(nodeId);
  if (!nodeDelta) {
    nodeDelta = createNodeDelta();
    delta.modifiedNodes.set(nodeId, nodeDelta);
  }
  return nodeDelta;
}

// ============================================================================
// Property operations
// ============================================================================

/**
 * Set a node property in delta
 */
export function setNodeProp(
  delta: DeltaState,
  nodeId: NodeID,
  keyId: PropKeyID,
  value: PropValue,
  isNewNode: boolean,
): void {
  const nodeDelta = getOrCreateNodeDelta(delta, nodeId, isNewNode);
  nodeDelta.props.set(keyId, value);
}

/**
 * Delete a node property in delta
 */
export function deleteNodeProp(
  delta: DeltaState,
  nodeId: NodeID,
  keyId: PropKeyID,
  isNewNode: boolean,
): void {
  const nodeDelta = getOrCreateNodeDelta(delta, nodeId, isNewNode);
  nodeDelta.props.set(keyId, null); // null marks deletion
}

/**
 * Get edge property key
 */
export function edgePropKey(src: NodeID, etype: ETypeID, dst: NodeID): string {
  return `${src}:${etype}:${dst}`;
}

/**
 * Set an edge property in delta
 */
export function setEdgeProp(
  delta: DeltaState,
  src: NodeID,
  etype: ETypeID,
  dst: NodeID,
  keyId: PropKeyID,
  value: PropValue,
): void {
  const key = edgePropKey(src, etype, dst);
  let props = delta.edgeProps.get(key);
  if (!props) {
    props = new Map();
    delta.edgeProps.set(key, props);
  }
  props.set(keyId, value);
}

/**
 * Delete an edge property in delta
 */
export function deleteEdgeProp(
  delta: DeltaState,
  src: NodeID,
  etype: ETypeID,
  dst: NodeID,
  keyId: PropKeyID,
): void {
  const key = edgePropKey(src, etype, dst);
  let props = delta.edgeProps.get(key);
  if (!props) {
    props = new Map();
    delta.edgeProps.set(key, props);
  }
  props.set(keyId, null); // null marks deletion
}

// ============================================================================
// Label operations
// ============================================================================

/**
 * Add a label to a node
 */
export function addNodeLabel(
  delta: DeltaState,
  nodeId: NodeID,
  labelId: LabelID,
  isNewNode: boolean,
): void {
  const nodeDelta = getOrCreateNodeDelta(delta, nodeId, isNewNode);
  nodeDelta.labelsDeleted.delete(labelId);
  nodeDelta.labels.add(labelId);
}

/**
 * Remove a label from a node
 */
export function removeNodeLabel(
  delta: DeltaState,
  nodeId: NodeID,
  labelId: LabelID,
  isNewNode: boolean,
): void {
  const nodeDelta = getOrCreateNodeDelta(delta, nodeId, isNewNode);
  nodeDelta.labels.delete(labelId);
  if (!isNewNode) {
    nodeDelta.labelsDeleted.add(labelId);
  }
}

// ============================================================================
// Definition operations
// ============================================================================

export function defineLabel(
  delta: DeltaState,
  labelId: LabelID,
  name: string,
): void {
  delta.newLabels.set(labelId, name);
}

export function defineEtype(
  delta: DeltaState,
  etypeId: ETypeID,
  name: string,
): void {
  delta.newEtypes.set(etypeId, name);
}

export function definePropkey(
  delta: DeltaState,
  propkeyId: PropKeyID,
  name: string,
): void {
  delta.newPropkeys.set(propkeyId, name);
}

// ============================================================================
// Key index operations
// ============================================================================

/**
 * Set node key in delta
 */
export function setNodeKey(
  delta: DeltaState,
  nodeId: NodeID,
  key: string,
): void {
  delta.keyIndex.set(key, nodeId);
  delta.keyIndexDeleted.delete(key);
}

/**
 * Delete node key in delta
 */
export function deleteNodeKey(delta: DeltaState, key: string): void {
  delta.keyIndex.delete(key);
  delta.keyIndexDeleted.add(key);
}

/**
 * Look up node by key in delta
 */
export function lookupKeyInDelta(
  delta: DeltaState,
  key: string,
): NodeID | null | "deleted" {
  if (delta.keyIndexDeleted.has(key)) {
    return "deleted";
  }
  return delta.keyIndex.get(key) ?? null;
}

// ============================================================================
// Delta statistics
// ============================================================================

export function getDeltaStats(delta: DeltaState): {
  nodesCreated: number;
  nodesDeleted: number;
  edgesAdded: number;
  edgesDeleted: number;
} {
  let edgesAdded = 0;
  for (const patches of delta.outAdd.values()) {
    edgesAdded += patches.length;
  }

  let edgesDeleted = 0;
  for (const patches of delta.outDel.values()) {
    edgesDeleted += patches.length;
  }

  return {
    nodesCreated: delta.createdNodes.size,
    nodesDeleted: delta.deletedNodes.size,
    edgesAdded,
    edgesDeleted,
  };
}

// ============================================================================
// Delta clearing
// ============================================================================

/**
 * Clear all delta state (after compaction)
 */
export function clearDelta(delta: DeltaState): void {
  delta.createdNodes.clear();
  delta.deletedNodes.clear();
  delta.modifiedNodes.clear();
  delta.outAdd.clear();
  delta.outDel.clear();
  delta.inAdd.clear();
  delta.inDel.clear();
  delta.edgeProps.clear();
  delta.newLabels.clear();
  delta.newEtypes.clear();
  delta.newPropkeys.clear();
  delta.keyIndex.clear();
  delta.keyIndexDeleted.clear();
}
