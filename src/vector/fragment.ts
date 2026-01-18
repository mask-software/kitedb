/**
 * Fragment operations for columnar vector storage
 *
 * A fragment is an immutable collection of row groups.
 * Fragments follow an append-only design and are sealed when full.
 */

import type { Fragment, RowGroup, VectorStoreConfig } from "./types.ts";
import {
  createRowGroup,
  rowGroupAppend,
  rowGroupIsFull,
  rowGroupTrim,
  rowGroupGet,
} from "./row-group.ts";

/**
 * Create a new fragment
 *
 * @param id - Fragment ID
 * @param config - Vector store configuration
 */
export function createFragment(
  id: number,
  config: VectorStoreConfig
): Fragment {
  return {
    id,
    state: "active",
    rowGroups: [createRowGroup(0, config.dimensions, config.rowGroupSize)],
    totalVectors: 0,
    deletionBitmap: new Uint32Array(
      Math.ceil(config.fragmentTargetSize / 32)
    ),
    deletedCount: 0,
  };
}

/**
 * Append a vector to a fragment
 *
 * @param fragment - The fragment to append to
 * @param vector - The vector to append
 * @param config - Vector store configuration
 * @returns The local index within the fragment
 */
export function fragmentAppend(
  fragment: Fragment,
  vector: Float32Array,
  config: VectorStoreConfig
): number {
  if (fragment.state === "sealed") {
    throw new Error("Cannot append to sealed fragment");
  }

  // Get current row group or create new one
  let rowGroup = fragment.rowGroups[fragment.rowGroups.length - 1];

  if (rowGroupIsFull(rowGroup, config.dimensions, config.rowGroupSize)) {
    // Create new row group
    rowGroup = createRowGroup(
      fragment.rowGroups.length,
      config.dimensions,
      config.rowGroupSize
    );
    fragment.rowGroups.push(rowGroup);
  }

  const localIdx = fragment.totalVectors;
  rowGroupAppend(rowGroup, vector, config.dimensions, config.normalize);
  fragment.totalVectors++;

  return localIdx;
}

/**
 * Mark a vector as deleted in a fragment (soft delete via bitmap)
 *
 * @param fragment - The fragment
 * @param localIdx - Local index within the fragment
 * @returns true if the vector was deleted, false if already deleted
 */
export function fragmentDelete(
  fragment: Fragment,
  localIdx: number
): boolean {
  if (localIdx < 0 || localIdx >= fragment.totalVectors) {
    throw new Error(
      `Index out of bounds: ${localIdx} (total: ${fragment.totalVectors})`
    );
  }

  const wordIdx = localIdx >>> 5; // localIdx / 32
  const bitIdx = localIdx & 31; // localIdx % 32
  const mask = 1 << bitIdx;

  if (fragment.deletionBitmap[wordIdx] & mask) {
    // Already deleted
    return false;
  }

  fragment.deletionBitmap[wordIdx] |= mask;
  fragment.deletedCount++;
  return true;
}

/**
 * Check if a vector is deleted
 *
 * @param fragment - The fragment
 * @param localIdx - Local index within the fragment
 */
export function fragmentIsDeleted(
  fragment: Fragment,
  localIdx: number
): boolean {
  if (localIdx < 0 || localIdx >= fragment.totalVectors) {
    return true; // Out of bounds is treated as deleted
  }

  const wordIdx = localIdx >>> 5;
  const bitIdx = localIdx & 31;
  return (fragment.deletionBitmap[wordIdx] & (1 << bitIdx)) !== 0;
}

/**
 * Undelete a vector (clear the deletion bit)
 *
 * @param fragment - The fragment
 * @param localIdx - Local index within the fragment
 * @returns true if the vector was undeleted, false if wasn't deleted
 */
export function fragmentUndelete(
  fragment: Fragment,
  localIdx: number
): boolean {
  if (localIdx < 0 || localIdx >= fragment.totalVectors) {
    return false;
  }

  const wordIdx = localIdx >>> 5;
  const bitIdx = localIdx & 31;
  const mask = 1 << bitIdx;

  if (!(fragment.deletionBitmap[wordIdx] & mask)) {
    // Not deleted
    return false;
  }

  fragment.deletionBitmap[wordIdx] &= ~mask;
  fragment.deletedCount--;
  return true;
}

/**
 * Seal a fragment (make it immutable)
 *
 * @param fragment - The fragment to seal
 */
export function fragmentSeal(fragment: Fragment): void {
  if (fragment.state === "sealed") {
    return; // Already sealed
  }

  fragment.state = "sealed";

  // Trim the last row group's data array if not full
  if (fragment.rowGroups.length > 0) {
    const lastRowGroup = fragment.rowGroups[fragment.rowGroups.length - 1];
    
    // Only trim if there's actually data to trim
    if (lastRowGroup.count > 0 && lastRowGroup.data.length > 0) {
      const dimensions = lastRowGroup.data.length / lastRowGroup.count;
      
      // Sanity check: dimensions should be a positive integer
      if (dimensions > 0 && Number.isInteger(dimensions)) {
        fragment.rowGroups[fragment.rowGroups.length - 1] = rowGroupTrim(
          lastRowGroup,
          dimensions
        );
      }
    }
  }

  // Trim deletion bitmap to actual size needed
  const neededWords = Math.ceil(fragment.totalVectors / 32);
  if (neededWords < fragment.deletionBitmap.length) {
    fragment.deletionBitmap = fragment.deletionBitmap.slice(0, neededWords);
  }
}

/**
 * Check if fragment should be sealed (reached target size)
 */
export function fragmentShouldSeal(
  fragment: Fragment,
  config: VectorStoreConfig
): boolean {
  return fragment.totalVectors >= config.fragmentTargetSize;
}

/**
 * Get a vector from a fragment
 *
 * @param fragment - The fragment
 * @param localIdx - Local index within the fragment
 * @param dimensions - Number of dimensions per vector
 * @param rowGroupCapacity - Optional row group capacity (required for sealed single-rowgroup fragments)
 * @returns The vector as a Float32Array view, or null if deleted/not found
 */
export function fragmentGetVector(
  fragment: Fragment,
  localIdx: number,
  dimensions: number,
  rowGroupCapacity?: number
): Float32Array | null {
  if (fragmentIsDeleted(fragment, localIdx)) {
    return null;
  }

  // Handle empty fragment
  if (fragment.rowGroups.length === 0 || dimensions === 0) {
    return null;
  }

  // Calculate row group size:
  // - If rowGroupCapacity is provided, use it (most reliable)
  // - If fragment has multiple row groups, the first one is never trimmed
  // - If fragment has one row group, it may have been trimmed on seal
  let rowGroupSize: number;
  
  if (rowGroupCapacity !== undefined) {
    rowGroupSize = rowGroupCapacity;
  } else if (fragment.rowGroups.length > 1) {
    // First row group is never trimmed, use its capacity
    rowGroupSize = fragment.rowGroups[0].data.length / dimensions;
  } else {
    // Single row group - could be trimmed, so use its count as upper bound
    // This works because localIdx can't exceed totalVectors which equals count
    const singleRowGroup = fragment.rowGroups[0];
    rowGroupSize = Math.max(singleRowGroup.data.length / dimensions, singleRowGroup.count);
  }
  
  if (rowGroupSize === 0) {
    return null;
  }

  const rowGroupIdx = Math.floor(localIdx / rowGroupSize);
  const rowGroup = fragment.rowGroups[rowGroupIdx];
  if (!rowGroup) {
    return null;
  }

  const localRowIdx = localIdx % rowGroupSize;
  if (localRowIdx >= rowGroup.count) {
    return null;
  }

  return rowGroupGet(rowGroup, localRowIdx, dimensions);
}

/**
 * Get the number of live (non-deleted) vectors in a fragment
 */
export function fragmentLiveCount(fragment: Fragment): number {
  return fragment.totalVectors - fragment.deletedCount;
}

/**
 * Get the deletion ratio (deleted / total)
 */
export function fragmentDeletionRatio(fragment: Fragment): number {
  if (fragment.totalVectors === 0) return 0;
  return fragment.deletedCount / fragment.totalVectors;
}

/**
 * Calculate the byte size of a fragment
 */
export function fragmentByteSize(fragment: Fragment): number {
  let size = 0;

  // Row group data
  for (const rg of fragment.rowGroups) {
    size += rg.data.byteLength;
  }

  // Deletion bitmap
  size += fragment.deletionBitmap.byteLength;

  return size;
}

/**
 * Iterate over all non-deleted vectors in a fragment
 */
export function* fragmentIterator(
  fragment: Fragment,
  dimensions: number,
  rowGroupSize: number
): Generator<{ localIdx: number; vector: Float32Array }> {
  for (let localIdx = 0; localIdx < fragment.totalVectors; localIdx++) {
    if (fragmentIsDeleted(fragment, localIdx)) {
      continue;
    }

    const rowGroupIdx = Math.floor(localIdx / rowGroupSize);
    const rowGroup = fragment.rowGroups[rowGroupIdx];
    if (!rowGroup) continue;

    const localRowIdx = localIdx % rowGroupSize;
    if (localRowIdx >= rowGroup.count) continue;

    const offset = localRowIdx * dimensions;
    yield {
      localIdx,
      vector: rowGroup.data.subarray(offset, offset + dimensions),
    };
  }
}

/**
 * Create a fragment from existing data (for deserialization)
 */
export function fragmentFromData(
  id: number,
  state: "active" | "sealed",
  rowGroups: RowGroup[],
  totalVectors: number,
  deletionBitmap: Uint32Array,
  deletedCount: number
): Fragment {
  return {
    id,
    state,
    rowGroups,
    totalVectors,
    deletionBitmap,
    deletedCount,
  };
}

/**
 * Clone a fragment (deep copy)
 */
export function fragmentClone(fragment: Fragment): Fragment {
  return {
    id: fragment.id,
    state: fragment.state,
    rowGroups: fragment.rowGroups.map((rg) => ({
      id: rg.id,
      count: rg.count,
      data: new Float32Array(rg.data),
    })),
    totalVectors: fragment.totalVectors,
    deletionBitmap: new Uint32Array(fragment.deletionBitmap),
    deletedCount: fragment.deletedCount,
  };
}
