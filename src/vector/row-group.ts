/**
 * Row group operations for columnar vector storage
 *
 * A row group is a batch of vectors stored contiguously in memory.
 * This enables efficient batch operations and SIMD processing.
 */

import type { RowGroup } from "./types.ts";
import { normalizeVectorAt } from "./normalize.ts";

/**
 * Create a new row group with pre-allocated capacity
 *
 * @param id - Row group ID within fragment
 * @param dimensions - Number of dimensions per vector
 * @param capacity - Maximum number of vectors this row group can hold
 */
export function createRowGroup(
  id: number,
  dimensions: number,
  capacity: number
): RowGroup {
  return {
    id,
    count: 0,
    data: new Float32Array(capacity * dimensions),
  };
}

/**
 * Append a vector to a row group
 *
 * @param rowGroup - The row group to append to
 * @param vector - The vector to append
 * @param dimensions - Number of dimensions per vector
 * @param shouldNormalize - Whether to normalize the vector after copying
 * @returns The local index within the row group
 */
export function rowGroupAppend(
  rowGroup: RowGroup,
  vector: Float32Array,
  dimensions: number,
  shouldNormalize: boolean
): number {
  if (vector.length !== dimensions) {
    throw new Error(
      `Vector dimension mismatch: expected ${dimensions}, got ${vector.length}`
    );
  }

  const localIdx = rowGroup.count;
  const offset = localIdx * dimensions;

  // Copy vector data
  rowGroup.data.set(vector, offset);

  // Normalize if needed
  if (shouldNormalize) {
    normalizeVectorAt(rowGroup.data, dimensions, localIdx);
  }

  rowGroup.count++;
  return localIdx;
}

/**
 * Get a vector from a row group (returns a view, not a copy)
 *
 * @param rowGroup - The row group to read from
 * @param localIdx - Local index within the row group
 * @param dimensions - Number of dimensions per vector
 * @returns A subarray view of the vector (modifications affect original)
 */
export function rowGroupGet(
  rowGroup: RowGroup,
  localIdx: number,
  dimensions: number
): Float32Array {
  if (localIdx < 0 || localIdx >= rowGroup.count) {
    throw new Error(
      `Index out of bounds: ${localIdx} (count: ${rowGroup.count})`
    );
  }
  const offset = localIdx * dimensions;
  return rowGroup.data.subarray(offset, offset + dimensions);
}

/**
 * Get a copy of a vector from a row group
 *
 * @param rowGroup - The row group to read from
 * @param localIdx - Local index within the row group
 * @param dimensions - Number of dimensions per vector
 * @returns A new Float32Array containing the vector data
 */
export function rowGroupGetCopy(
  rowGroup: RowGroup,
  localIdx: number,
  dimensions: number
): Float32Array {
  const view = rowGroupGet(rowGroup, localIdx, dimensions);
  return new Float32Array(view);
}

/**
 * Check if row group is full
 *
 * @param rowGroup - The row group to check
 * @param dimensions - Number of dimensions per vector
 * @param capacity - Maximum capacity of the row group
 */
export function rowGroupIsFull(
  rowGroup: RowGroup,
  dimensions: number,
  capacity: number
): boolean {
  return rowGroup.count >= capacity;
}

/**
 * Get the remaining capacity of a row group
 */
export function rowGroupRemainingCapacity(
  rowGroup: RowGroup,
  capacity: number
): number {
  return capacity - rowGroup.count;
}

/**
 * Get the byte size of a row group's data
 */
export function rowGroupByteSize(rowGroup: RowGroup): number {
  return rowGroup.data.byteLength;
}

/**
 * Get the actual byte size used (may be less than allocated)
 */
export function rowGroupUsedByteSize(
  rowGroup: RowGroup,
  dimensions: number
): number {
  return rowGroup.count * dimensions * Float32Array.BYTES_PER_ELEMENT;
}

/**
 * Trim a row group's data array to actual size (for sealed row groups)
 * Returns a new row group with trimmed data
 */
export function rowGroupTrim(
  rowGroup: RowGroup,
  dimensions: number
): RowGroup {
  if (rowGroup.count * dimensions === rowGroup.data.length) {
    return rowGroup; // Already trimmed
  }

  return {
    id: rowGroup.id,
    count: rowGroup.count,
    data: rowGroup.data.slice(0, rowGroup.count * dimensions),
  };
}

/**
 * Create a row group from existing data (for deserialization)
 */
export function rowGroupFromData(
  id: number,
  count: number,
  data: Float32Array
): RowGroup {
  return {
    id,
    count,
    data,
  };
}

/**
 * Iterate over all vectors in a row group
 */
export function* rowGroupIterator(
  rowGroup: RowGroup,
  dimensions: number
): Generator<{ localIdx: number; vector: Float32Array }> {
  for (let i = 0; i < rowGroup.count; i++) {
    const offset = i * dimensions;
    yield {
      localIdx: i,
      vector: rowGroup.data.subarray(offset, offset + dimensions),
    };
  }
}

/**
 * Copy vectors from one row group to another
 *
 * @param src - Source row group
 * @param srcStart - Starting index in source
 * @param dst - Destination row group
 * @param count - Number of vectors to copy
 * @param dimensions - Number of dimensions per vector
 */
export function rowGroupCopy(
  src: RowGroup,
  srcStart: number,
  dst: RowGroup,
  count: number,
  dimensions: number
): void {
  if (srcStart + count > src.count) {
    throw new Error("Source range out of bounds");
  }

  const srcOffset = srcStart * dimensions;
  const dstOffset = dst.count * dimensions;
  const length = count * dimensions;

  dst.data.set(src.data.subarray(srcOffset, srcOffset + length), dstOffset);
  dst.count += count;
}
