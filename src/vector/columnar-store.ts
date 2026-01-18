/**
 * Lance-style columnar vector store
 *
 * Manages fragments, handles inserts/deletes, coordinates with index.
 * This is the main entry point for vector storage operations.
 */

import type {
  VectorManifest,
  VectorStoreConfig,
  Fragment,
} from "./types.ts";
import { DEFAULT_VECTOR_CONFIG } from "./types.ts";
import {
  createFragment,
  fragmentAppend,
  fragmentDelete,
  fragmentSeal,
  fragmentShouldSeal,
  fragmentIsDeleted,
  fragmentGetVector,
  fragmentLiveCount,
} from "./fragment.ts";
import { validateVector } from "./normalize.ts";
import type { NodeID } from "../types.ts";

/**
 * Create a new vector store
 *
 * @param dimensions - Number of dimensions per vector
 * @param config - Optional configuration overrides
 */
export function createVectorStore(
  dimensions: number,
  config?: Partial<Omit<VectorStoreConfig, "dimensions">>
): VectorManifest {
  const fullConfig: VectorStoreConfig = {
    ...DEFAULT_VECTOR_CONFIG,
    ...config,
    dimensions,
  };

  const initialFragment = createFragment(0, fullConfig);

  return {
    config: fullConfig,
    fragments: [initialFragment],
    activeFragmentId: 0,
    totalVectors: 0,
    totalDeleted: 0,
    nextVectorId: 0,
    nodeIdToVectorId: new Map(),
    vectorIdToNodeId: new Map(),
    vectorIdToLocation: new Map(),
  };
}

/**
 * Insert a vector into the store
 *
 * @param manifest - The vector store manifest
 * @param nodeId - Graph node ID to associate with this vector
 * @param vector - The vector to insert
 * @param skipValidation - Skip validation for performance (use with caution)
 * @returns The global vector ID
 * @throws Error if vector dimensions don't match or vector contains invalid values
 */
export function vectorStoreInsert(
  manifest: VectorManifest,
  nodeId: NodeID,
  vector: Float32Array,
  skipValidation: boolean = false
): number {
  // Check dimensions
  if (vector.length !== manifest.config.dimensions) {
    throw new Error(
      `Vector dimension mismatch: expected ${manifest.config.dimensions}, got ${vector.length}`
    );
  }

  // Validate vector for NaN, Infinity, and zero vectors
  if (!skipValidation) {
    const validation = validateVector(vector);
    if (!validation.valid) {
      throw new Error(`Invalid vector: ${validation.message}`);
    }
  }

  // Check if node already has a vector - delete old one first
  const existingVectorId = manifest.nodeIdToVectorId.get(nodeId);
  if (existingVectorId !== undefined) {
    vectorStoreDelete(manifest, nodeId);
  }

  // Get active fragment
  let fragment = manifest.fragments.find(
    (f) => f.id === manifest.activeFragmentId
  );

  if (!fragment || fragment.state === "sealed") {
    // Create new fragment
    const newId = manifest.fragments.length;
    fragment = createFragment(newId, manifest.config);
    manifest.fragments.push(fragment);
    manifest.activeFragmentId = newId;
  }

  // Check if fragment should be sealed
  if (fragmentShouldSeal(fragment, manifest.config)) {
    fragmentSeal(fragment);
    const newId = manifest.fragments.length;
    fragment = createFragment(newId, manifest.config);
    manifest.fragments.push(fragment);
    manifest.activeFragmentId = newId;
  }

  // Append to fragment
  const localIdx = fragmentAppend(fragment, vector, manifest.config);

  // Assign global vector ID
  const vectorId = manifest.nextVectorId++;

  // Update mappings
  manifest.nodeIdToVectorId.set(nodeId, vectorId);
  manifest.vectorIdToNodeId.set(vectorId, nodeId);
  manifest.vectorIdToLocation.set(vectorId, {
    fragmentId: fragment.id,
    localIndex: localIdx,
  });

  manifest.totalVectors++;

  return vectorId;
}

/**
 * Delete a vector from the store (soft delete via bitmap)
 *
 * @param manifest - The vector store manifest
 * @param nodeId - Graph node ID of the vector to delete
 * @returns true if deleted, false if not found
 */
export function vectorStoreDelete(
  manifest: VectorManifest,
  nodeId: NodeID
): boolean {
  const vectorId = manifest.nodeIdToVectorId.get(nodeId);
  if (vectorId === undefined) return false;

  const location = manifest.vectorIdToLocation.get(vectorId);
  if (!location) return false;

  const fragment = manifest.fragments.find((f) => f.id === location.fragmentId);
  if (!fragment) return false;

  const deleted = fragmentDelete(fragment, location.localIndex);
  if (deleted) {
    manifest.nodeIdToVectorId.delete(nodeId);
    manifest.vectorIdToNodeId.delete(vectorId);
    manifest.vectorIdToLocation.delete(vectorId);
    manifest.totalDeleted++;
  }

  return deleted;
}

/**
 * Get a vector by node ID
 *
 * @param manifest - The vector store manifest
 * @param nodeId - Graph node ID
 * @returns The vector as a Float32Array view, or null if not found
 */
export function vectorStoreGet(
  manifest: VectorManifest,
  nodeId: NodeID
): Float32Array | null {
  const vectorId = manifest.nodeIdToVectorId.get(nodeId);
  if (vectorId === undefined) return null;

  return vectorStoreGetById(manifest, vectorId);
}

/**
 * Get a vector by vector ID
 *
 * @param manifest - The vector store manifest
 * @param vectorId - Global vector ID
 * @returns The vector as a Float32Array view, or null if not found/deleted
 */
export function vectorStoreGetById(
  manifest: VectorManifest,
  vectorId: number
): Float32Array | null {
  const location = manifest.vectorIdToLocation.get(vectorId);
  if (!location) return null;

  const fragment = manifest.fragments.find((f) => f.id === location.fragmentId);
  if (!fragment) return null;

  return fragmentGetVector(
    fragment,
    location.localIndex,
    manifest.config.dimensions
  );
}

/**
 * Check if a vector exists for a node
 */
export function vectorStoreHas(
  manifest: VectorManifest,
  nodeId: NodeID
): boolean {
  const vectorId = manifest.nodeIdToVectorId.get(nodeId);
  if (vectorId === undefined) return false;

  const location = manifest.vectorIdToLocation.get(vectorId);
  if (!location) return false;

  const fragment = manifest.fragments.find((f) => f.id === location.fragmentId);
  if (!fragment) return false;

  return !fragmentIsDeleted(fragment, location.localIndex);
}

/**
 * Get the vector ID for a node
 */
export function vectorStoreGetVectorId(
  manifest: VectorManifest,
  nodeId: NodeID
): number | undefined {
  return manifest.nodeIdToVectorId.get(nodeId);
}

/**
 * Get the node ID for a vector ID
 */
export function vectorStoreGetNodeId(
  manifest: VectorManifest,
  vectorId: number
): NodeID | undefined {
  return manifest.vectorIdToNodeId.get(vectorId);
}

/**
 * Get the location of a vector
 */
export function vectorStoreGetLocation(
  manifest: VectorManifest,
  vectorId: number
): { fragmentId: number; localIndex: number } | undefined {
  return manifest.vectorIdToLocation.get(vectorId);
}

/**
 * Iterate over all non-deleted vectors
 * Yields (vectorId, nodeId, vector) tuples
 */
export function* vectorStoreIterator(
  manifest: VectorManifest
): Generator<[number, NodeID, Float32Array]> {
  const { dimensions, rowGroupSize } = manifest.config;

  for (const [nodeId, vectorId] of manifest.nodeIdToVectorId) {
    const location = manifest.vectorIdToLocation.get(vectorId);
    if (!location) continue;

    const fragment = manifest.fragments.find(
      (f) => f.id === location.fragmentId
    );
    if (!fragment) continue;

    if (fragmentIsDeleted(fragment, location.localIndex)) continue;

    const rowGroupIdx = Math.floor(location.localIndex / rowGroupSize);
    const localRowIdx = location.localIndex % rowGroupSize;
    const rowGroup = fragment.rowGroups[rowGroupIdx];
    if (!rowGroup) continue;

    const offset = localRowIdx * dimensions;
    const vector = rowGroup.data.subarray(offset, offset + dimensions);

    yield [vectorId, nodeId, vector];
  }
}

/**
 * Iterate over vectors with their IDs
 */
export function* vectorStoreIteratorWithIds(
  manifest: VectorManifest
): Generator<{ vectorId: number; nodeId: NodeID; vector: Float32Array }> {
  for (const [vectorId, nodeId, vector] of vectorStoreIterator(manifest)) {
    yield { vectorId, nodeId, vector };
  }
}

/**
 * Batch insert vectors
 *
 * @param manifest - The vector store manifest
 * @param entries - Array of (nodeId, vector) pairs
 * @param onProgress - Optional progress callback
 * @param skipValidation - Skip validation for performance (use with caution)
 * @returns Array of assigned vector IDs
 */
export function vectorStoreBatchInsert(
  manifest: VectorManifest,
  entries: Array<{ nodeId: NodeID; vector: Float32Array }>,
  onProgress?: (inserted: number, total: number) => void,
  skipValidation: boolean = false
): number[] {
  const vectorIds: number[] = [];
  const total = entries.length;

  for (let i = 0; i < entries.length; i++) {
    const { nodeId, vector } = entries[i];
    const vectorId = vectorStoreInsert(manifest, nodeId, vector, skipValidation);
    vectorIds.push(vectorId);

    if (onProgress && (i + 1) % 1000 === 0) {
      onProgress(i + 1, total);
    }
  }

  if (onProgress) {
    onProgress(total, total);
  }

  return vectorIds;
}

/**
 * Get store statistics
 */
export function vectorStoreStats(manifest: VectorManifest): {
  totalVectors: number;
  totalDeleted: number;
  liveVectors: number;
  fragmentCount: number;
  sealedFragments: number;
  activeFragmentVectors: number;
  dimensions: number;
  metric: string;
  rowGroupSize: number;
  fragmentTargetSize: number;
  bytesUsed: number;
} {
  const activeFragment = manifest.fragments.find(
    (f) => f.id === manifest.activeFragmentId
  );

  let bytesUsed = 0;
  for (const fragment of manifest.fragments) {
    for (const rg of fragment.rowGroups) {
      bytesUsed += rg.data.byteLength;
    }
    bytesUsed += fragment.deletionBitmap.byteLength;
  }

  return {
    totalVectors: manifest.totalVectors,
    totalDeleted: manifest.totalDeleted,
    liveVectors: manifest.totalVectors - manifest.totalDeleted,
    fragmentCount: manifest.fragments.length,
    sealedFragments: manifest.fragments.filter((f) => f.state === "sealed")
      .length,
    activeFragmentVectors: activeFragment?.totalVectors ?? 0,
    dimensions: manifest.config.dimensions,
    metric: manifest.config.metric,
    rowGroupSize: manifest.config.rowGroupSize,
    fragmentTargetSize: manifest.config.fragmentTargetSize,
    bytesUsed,
  };
}

/**
 * Get fragment statistics
 */
export function vectorStoreFragmentStats(
  manifest: VectorManifest
): Array<{
  id: number;
  state: string;
  totalVectors: number;
  deletedVectors: number;
  liveVectors: number;
  deletionRatio: number;
  rowGroupCount: number;
}> {
  return manifest.fragments.map((f) => ({
    id: f.id,
    state: f.state,
    totalVectors: f.totalVectors,
    deletedVectors: f.deletedCount,
    liveVectors: fragmentLiveCount(f),
    deletionRatio: f.totalVectors > 0 ? f.deletedCount / f.totalVectors : 0,
    rowGroupCount: f.rowGroups.length,
  }));
}

/**
 * Seal the active fragment and create a new one
 * Useful for forcing a checkpoint boundary
 */
export function vectorStoreSealActive(manifest: VectorManifest): void {
  const fragment = manifest.fragments.find(
    (f) => f.id === manifest.activeFragmentId
  );
  if (!fragment || fragment.state === "sealed") return;

  fragmentSeal(fragment);

  const newId = manifest.fragments.length;
  const newFragment = createFragment(newId, manifest.config);
  manifest.fragments.push(newFragment);
  manifest.activeFragmentId = newId;
}

/**
 * Get all vectors as a flat Float32Array (for training/serialization)
 * Only includes non-deleted vectors
 */
export function vectorStoreGetAllVectors(
  manifest: VectorManifest
): { data: Float32Array; nodeIds: NodeID[]; vectorIds: number[] } {
  const liveCount = manifest.totalVectors - manifest.totalDeleted;
  const dimensions = manifest.config.dimensions;
  const data = new Float32Array(liveCount * dimensions);
  const nodeIds: NodeID[] = [];
  const vectorIds: number[] = [];

  let idx = 0;
  for (const [vectorId, nodeId, vector] of vectorStoreIterator(manifest)) {
    data.set(vector, idx * dimensions);
    nodeIds.push(nodeId);
    vectorIds.push(vectorId);
    idx++;
  }

  return { data, nodeIds, vectorIds };
}

/**
 * Clear all data from the store
 */
export function vectorStoreClear(manifest: VectorManifest): void {
  manifest.fragments = [createFragment(0, manifest.config)];
  manifest.activeFragmentId = 0;
  manifest.totalVectors = 0;
  manifest.totalDeleted = 0;
  manifest.nextVectorId = 0;
  manifest.nodeIdToVectorId.clear();
  manifest.vectorIdToNodeId.clear();
  manifest.vectorIdToLocation.clear();
}

/**
 * Clone a manifest (deep copy)
 */
export function vectorStoreClone(manifest: VectorManifest): VectorManifest {
  return {
    config: { ...manifest.config },
    fragments: manifest.fragments.map((f) => ({
      id: f.id,
      state: f.state,
      rowGroups: f.rowGroups.map((rg) => ({
        id: rg.id,
        count: rg.count,
        data: new Float32Array(rg.data),
      })),
      totalVectors: f.totalVectors,
      deletionBitmap: new Uint32Array(f.deletionBitmap),
      deletedCount: f.deletedCount,
    })),
    activeFragmentId: manifest.activeFragmentId,
    totalVectors: manifest.totalVectors,
    totalDeleted: manifest.totalDeleted,
    nextVectorId: manifest.nextVectorId,
    nodeIdToVectorId: new Map(manifest.nodeIdToVectorId),
    vectorIdToNodeId: new Map(manifest.vectorIdToNodeId),
    vectorIdToLocation: new Map(
      [...manifest.vectorIdToLocation].map(([k, v]) => [k, { ...v }])
    ),
  };
}
