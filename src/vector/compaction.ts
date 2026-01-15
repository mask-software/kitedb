/**
 * Fragment compaction to remove deleted vectors
 *
 * Compaction creates a new fragment containing only live vectors
 * from one or more source fragments.
 */

import type { Fragment, VectorManifest } from "./types.ts";
import {
  createFragment,
  fragmentAppend,
  fragmentSeal,
  fragmentIsDeleted,
  fragmentLiveCount,
} from "./fragment.ts";

/**
 * Compaction strategy configuration
 */
export interface CompactionStrategy {
  /** Minimum deletion ratio to trigger compaction (0-1) */
  minDeletionRatio: number;
  /** Maximum fragments to compact at once */
  maxFragmentsPerCompaction: number;
  /** Minimum total vectors across fragments to compact */
  minVectorsToCompact: number;
}

/**
 * Default compaction strategy
 */
export const DEFAULT_COMPACTION_STRATEGY: CompactionStrategy = {
  minDeletionRatio: 0.3, // 30% deleted
  maxFragmentsPerCompaction: 4,
  minVectorsToCompact: 10000,
};

/**
 * Find fragments that should be compacted
 *
 * @param manifest - The vector store manifest
 * @param strategy - Compaction strategy configuration
 * @returns Array of fragment IDs that should be compacted
 */
export function findFragmentsToCompact(
  manifest: VectorManifest,
  strategy: CompactionStrategy = DEFAULT_COMPACTION_STRATEGY
): number[] {
  const candidates: Array<{
    id: number;
    deletionRatio: number;
    liveVectors: number;
  }> = [];

  for (const fragment of manifest.fragments) {
    // Skip active fragment
    if (fragment.state === "active") continue;

    // Skip fragments with no vectors
    if (fragment.totalVectors === 0) continue;

    const deletionRatio = fragment.deletedCount / fragment.totalVectors;
    if (deletionRatio >= strategy.minDeletionRatio) {
      candidates.push({
        id: fragment.id,
        deletionRatio,
        liveVectors: fragment.totalVectors - fragment.deletedCount,
      });
    }
  }

  // Sort by deletion ratio (highest first)
  candidates.sort((a, b) => b.deletionRatio - a.deletionRatio);

  // Select fragments to compact
  const selected: number[] = [];
  let totalLiveVectors = 0;

  for (const candidate of candidates) {
    if (selected.length >= strategy.maxFragmentsPerCompaction) break;
    selected.push(candidate.id);
    totalLiveVectors += candidate.liveVectors;
  }

  // Only compact if we have enough vectors or multiple fragments
  if (
    totalLiveVectors < strategy.minVectorsToCompact &&
    selected.length < 2
  ) {
    return [];
  }

  return selected;
}

/**
 * Compact fragments into a new fragment
 *
 * @param manifest - The vector store manifest
 * @param fragmentIds - IDs of fragments to compact
 * @returns The new compacted fragment and updated location mappings
 */
export function compactFragments(
  manifest: VectorManifest,
  fragmentIds: number[]
): {
  newFragment: Fragment;
  updatedLocations: Map<number, { fragmentId: number; localIndex: number }>;
} {
  const config = manifest.config;
  const { dimensions, rowGroupSize } = config;
  const newFragmentId = manifest.fragments.length;
  const newFragment = createFragment(newFragmentId, config);
  const updatedLocations = new Map<
    number,
    { fragmentId: number; localIndex: number }
  >();

  // Process each source fragment
  for (const fragmentId of fragmentIds) {
    const fragment = manifest.fragments.find((f) => f.id === fragmentId);
    if (!fragment) continue;

    // Iterate over all vectors in fragment
    for (let localIdx = 0; localIdx < fragment.totalVectors; localIdx++) {
      // Skip deleted vectors
      if (fragmentIsDeleted(fragment, localIdx)) continue;

      // Get vector data
      const rowGroupIdx = Math.floor(localIdx / rowGroupSize);
      const localRowIdx = localIdx % rowGroupSize;
      const rowGroup = fragment.rowGroups[rowGroupIdx];
      if (!rowGroup) continue;

      const offset = localRowIdx * dimensions;
      const vector = rowGroup.data.subarray(offset, offset + dimensions);

      // Find the vectorId for this location
      let vectorId: number | undefined;
      for (const [vid, loc] of manifest.vectorIdToLocation) {
        if (loc.fragmentId === fragmentId && loc.localIndex === localIdx) {
          vectorId = vid;
          break;
        }
      }

      if (vectorId === undefined) continue;

      // Append to new fragment (skip normalization since already normalized)
      const newLocalIdx = fragmentAppend(newFragment, vector, {
        ...config,
        normalize: false, // Already normalized
      });

      // Record updated location
      updatedLocations.set(vectorId, {
        fragmentId: newFragmentId,
        localIndex: newLocalIdx,
      });
    }
  }

  // Seal the new fragment
  fragmentSeal(newFragment);

  return { newFragment, updatedLocations };
}

/**
 * Apply compaction results to manifest
 *
 * @param manifest - The vector store manifest
 * @param fragmentIds - IDs of source fragments that were compacted
 * @param newFragment - The new compacted fragment
 * @param updatedLocations - Updated vector locations
 */
export function applyCompaction(
  manifest: VectorManifest,
  fragmentIds: number[],
  newFragment: Fragment,
  updatedLocations: Map<number, { fragmentId: number; localIndex: number }>
): void {
  // Add new fragment
  manifest.fragments.push(newFragment);

  // Update vector locations
  for (const [vectorId, location] of updatedLocations) {
    manifest.vectorIdToLocation.set(vectorId, location);
  }

  // Update deleted count
  let removedDeleted = 0;
  for (const fragmentId of fragmentIds) {
    const fragment = manifest.fragments.find((f) => f.id === fragmentId);
    if (fragment) {
      removedDeleted += fragment.deletedCount;
    }
  }
  manifest.totalDeleted -= removedDeleted;

  // Mark old fragments as empty (keep IDs but clear data)
  for (const fragmentId of fragmentIds) {
    const idx = manifest.fragments.findIndex((f) => f.id === fragmentId);
    if (idx !== -1) {
      // Clear fragment data but keep metadata for ID tracking
      manifest.fragments[idx].rowGroups = [];
      manifest.fragments[idx].deletionBitmap = new Uint32Array(0);
      manifest.fragments[idx].totalVectors = 0;
      manifest.fragments[idx].deletedCount = 0;
      manifest.fragments[idx].state = "sealed";
    }
  }
}

/**
 * Run compaction if needed
 *
 * @param manifest - The vector store manifest
 * @param strategy - Compaction strategy configuration
 * @returns true if compaction was performed
 */
export function runCompactionIfNeeded(
  manifest: VectorManifest,
  strategy: CompactionStrategy = DEFAULT_COMPACTION_STRATEGY
): boolean {
  const fragmentIds = findFragmentsToCompact(manifest, strategy);
  if (fragmentIds.length === 0) {
    return false;
  }

  const { newFragment, updatedLocations } = compactFragments(
    manifest,
    fragmentIds
  );
  applyCompaction(manifest, fragmentIds, newFragment, updatedLocations);

  return true;
}

/**
 * Get compaction statistics
 */
export function getCompactionStats(manifest: VectorManifest): {
  fragmentsNeedingCompaction: number;
  potentialSpaceReclaim: number;
  totalDeletedVectors: number;
  averageDeletionRatio: number;
} {
  let fragmentsNeedingCompaction = 0;
  let potentialSpaceReclaim = 0;
  let totalDeletedVectors = 0;
  let totalVectors = 0;

  for (const fragment of manifest.fragments) {
    if (fragment.state === "active") continue;
    if (fragment.totalVectors === 0) continue;

    const deletionRatio = fragment.deletedCount / fragment.totalVectors;
    if (deletionRatio >= 0.3) {
      // Default threshold
      fragmentsNeedingCompaction++;
    }

    totalDeletedVectors += fragment.deletedCount;
    totalVectors += fragment.totalVectors;

    // Estimate space reclaim (deleted vectors * vector size)
    potentialSpaceReclaim +=
      fragment.deletedCount *
      manifest.config.dimensions *
      Float32Array.BYTES_PER_ELEMENT;
  }

  return {
    fragmentsNeedingCompaction,
    potentialSpaceReclaim,
    totalDeletedVectors,
    averageDeletionRatio: totalVectors > 0 ? totalDeletedVectors / totalVectors : 0,
  };
}

/**
 * Force compaction of all sealed fragments into one
 * Useful for optimizing storage after many deletions
 */
export function forceFullCompaction(manifest: VectorManifest): void {
  const sealedFragmentIds = manifest.fragments
    .filter((f) => f.state === "sealed" && f.totalVectors > 0)
    .map((f) => f.id);

  if (sealedFragmentIds.length === 0) return;

  const { newFragment, updatedLocations } = compactFragments(
    manifest,
    sealedFragmentIds
  );
  applyCompaction(manifest, sealedFragmentIds, newFragment, updatedLocations);
}
