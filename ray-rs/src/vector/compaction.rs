//! Fragment compaction to remove deleted vectors
//!
//! Compaction creates a new fragment containing only live vectors
//! from one or more source fragments.
//!
//! Ported from src/vector/compaction.ts

use std::collections::{HashMap, HashSet};

use crate::vector::store::{fragment_append, next_fragment_id};
use crate::vector::types::{Fragment, FragmentState, VectorLocation, VectorManifest};

// ============================================================================
// Compaction Strategy
// ============================================================================

/// Compaction strategy configuration
#[derive(Debug, Clone)]
pub struct CompactionStrategy {
  /// Minimum deletion ratio to trigger compaction (0-1)
  pub min_deletion_ratio: f32,
  /// Maximum fragments to compact at once
  pub max_fragments_per_compaction: usize,
  /// Minimum total vectors across fragments to compact
  pub min_vectors_to_compact: usize,
}

impl Default for CompactionStrategy {
  fn default() -> Self {
    Self {
      min_deletion_ratio: 0.3, // 30% deleted
      max_fragments_per_compaction: 4,
      min_vectors_to_compact: 10_000,
    }
  }
}

// ============================================================================
// Compaction Statistics
// ============================================================================

/// Compaction statistics
#[derive(Debug, Clone, Default)]
pub struct CompactionStats {
  /// Number of fragments needing compaction
  pub fragments_needing_compaction: usize,
  /// Potential space reclaim in bytes
  pub potential_space_reclaim: usize,
  /// Total deleted vectors
  pub total_deleted_vectors: usize,
  /// Average deletion ratio across all sealed fragments
  pub average_deletion_ratio: f32,
}

// ============================================================================
// Core Functions
// ============================================================================

/// Find fragments that should be compacted
///
/// # Arguments
/// * `manifest` - The vector store manifest
/// * `strategy` - Compaction strategy configuration
///
/// # Returns
/// Array of fragment IDs that should be compacted
pub fn find_fragments_to_compact(
  manifest: &VectorManifest,
  strategy: &CompactionStrategy,
) -> Vec<usize> {
  let mut candidates: Vec<(usize, f32, usize)> = Vec::new();

  for fragment in &manifest.fragments {
    // Skip active fragment
    if fragment.state == FragmentState::Active {
      continue;
    }

    // Skip fragments with no vectors (already compacted/cleared)
    if fragment.total_vectors == 0 {
      continue;
    }

    let deletion_ratio = fragment.deleted_count as f32 / fragment.total_vectors as f32;
    if deletion_ratio >= strategy.min_deletion_ratio {
      let live_vectors = fragment.total_vectors - fragment.deleted_count;
      candidates.push((fragment.id, deletion_ratio, live_vectors));
    }
  }

  // Sort by deletion ratio (highest first)
  candidates.sort_by(|a, b| b.1.total_cmp(&a.1));

  // Select fragments to compact
  let mut selected: Vec<usize> = Vec::new();
  let mut total_live_vectors = 0;

  for (id, _, live_vectors) in candidates {
    if selected.len() >= strategy.max_fragments_per_compaction {
      break;
    }
    selected.push(id);
    total_live_vectors += live_vectors;
  }

  // Only compact if we have enough vectors or multiple fragments
  // Exception: Always allow compaction of fully-deleted fragments (live_vectors = 0)
  if total_live_vectors < strategy.min_vectors_to_compact
    && selected.len() < 2
    && total_live_vectors > 0
  {
    return Vec::new();
  }

  selected
}

/// Clear fragments that have all vectors deleted (100% deletion ratio)
/// This is more efficient than compaction for fully-deleted fragments.
///
/// Removes those fragments from the manifest, together with sealed fragments
/// that are already empty, and adjusts the manifest totals to match.
///
/// # Returns
/// Number of fragments cleared (empty fragments removed along the way are not
/// counted)
pub fn clear_deleted_fragments(manifest: &mut VectorManifest) -> usize {
  let mut cleared = 0;
  let mut removed_vectors = 0;
  let mut removed_deleted = 0;

  manifest.fragments.retain(|fragment| {
    // Keep the active fragment, and any fragment with a live vector.
    if fragment.state == FragmentState::Active || fragment.deleted_count < fragment.total_vectors {
      return true;
    }
    if fragment.total_vectors > 0 {
      cleared += 1;
    }
    removed_vectors += fragment.total_vectors;
    removed_deleted += fragment.deleted_count;
    false
  });

  manifest.total_vectors -= removed_vectors;
  manifest.total_deleted -= removed_deleted;
  cleared
}

/// Result of a compaction operation
pub struct CompactionResult {
  /// The new compacted fragment
  pub new_fragment: Fragment,
  /// Updated vector locations (vector_id -> new location)
  pub updated_locations: HashMap<u64, VectorLocation>,
}

/// Compact fragments into a new fragment
///
/// Copies the live vectors of the given sealed fragments into one new sealed
/// fragment. The active fragment, and ids that name no fragment, are skipped.
///
/// # Arguments
/// * `manifest` - The vector store manifest
/// * `fragment_ids` - IDs of fragments to compact
///
/// # Returns
/// The new compacted fragment and updated location mappings
pub fn compact_fragments(manifest: &VectorManifest, fragment_ids: &[usize]) -> CompactionResult {
  let config = &manifest.config;

  let new_fragment_id = next_fragment_id(manifest);
  let mut new_fragment = Fragment::new(new_fragment_id);
  let mut updated_locations: HashMap<u64, VectorLocation> = HashMap::new();

  // Build reverse lookup: (fragment_id, local_index) -> vector_id
  let fragment_id_set: HashSet<usize> = fragment_ids.iter().copied().collect();
  let mut location_to_vector_id: HashMap<(usize, usize), u64> = HashMap::new();

  for (&vector_id, loc) in &manifest.vector_locations {
    if fragment_id_set.contains(&loc.fragment_id) {
      location_to_vector_id.insert((loc.fragment_id, loc.local_index), vector_id);
    }
  }

  // Process each source fragment
  for &fragment_id in fragment_ids {
    let fragment = match manifest
      .fragments
      .iter()
      .find(|f| f.id == fragment_id && f.state == FragmentState::Sealed)
    {
      Some(f) => f,
      None => continue,
    };

    for local_idx in 0..fragment.total_vectors {
      if fragment.is_deleted(local_idx) {
        continue;
      }

      let row_group = match fragment.row_groups.get(local_idx / config.row_group_size) {
        Some(rg) => rg,
        None => continue,
      };
      let vector = match row_group.get(local_idx % config.row_group_size, config.dimensions) {
        Some(vector) => vector,
        None => continue,
      };

      let vector_id = match location_to_vector_id.get(&(fragment_id, local_idx)) {
        Some(&id) => id,
        None => continue,
      };

      // Stored vectors are already normalized if the store normalizes.
      let new_local_idx = fragment_append(
        &mut new_fragment,
        vector,
        config.row_group_size,
        config.dimensions,
      );

      updated_locations.insert(
        vector_id,
        VectorLocation {
          fragment_id: new_fragment_id,
          local_index: new_local_idx,
        },
      );
    }
  }

  new_fragment.seal();

  CompactionResult {
    new_fragment,
    updated_locations,
  }
}

/// Apply compaction results to manifest
///
/// Removes the sealed source fragments, adds the compacted fragment, points
/// the moved vectors at it, and keeps the manifest totals consistent, so the
/// manifest still passes reload validation.
///
/// A result computed before later changes to the manifest still applies
/// safely: a moved vector deleted in the meantime is marked deleted in the
/// new fragment, and a new fragment id taken in the meantime is replaced.
///
/// # Arguments
/// * `manifest` - The vector store manifest
/// * `fragment_ids` - IDs of source fragments that were compacted
/// * `result` - The compaction result
pub fn apply_compaction(
  manifest: &mut VectorManifest,
  fragment_ids: &[usize],
  result: CompactionResult,
) {
  let CompactionResult {
    mut new_fragment,
    updated_locations,
  } = result;
  let sources: HashSet<usize> = fragment_ids.iter().copied().collect();
  let is_source =
    |fragment: &Fragment| fragment.state == FragmentState::Sealed && sources.contains(&fragment.id);

  // The new fragment's id must not collide with a fragment that stays.
  if manifest
    .fragments
    .iter()
    .any(|fragment| fragment.id == new_fragment.id && !is_source(fragment))
  {
    new_fragment.id = next_fragment_id(manifest);
  }

  // Point each moved vector at its new slot, if it still lives in a source
  // fragment; otherwise it was deleted after the compaction was computed.
  let mut moves = Vec::with_capacity(updated_locations.len());
  for (vector_id, location) in updated_locations {
    let still_in_source = manifest
      .vector_locations
      .get(&vector_id)
      .is_some_and(|current| sources.contains(&current.fragment_id));
    if still_in_source {
      moves.push((vector_id, location.local_index));
    } else {
      new_fragment.delete(location.local_index);
    }
  }

  let mut removed_vectors = 0;
  let mut removed_deleted = 0;
  manifest.fragments.retain(|fragment| {
    if !is_source(fragment) {
      return true;
    }
    removed_vectors += fragment.total_vectors;
    removed_deleted += fragment.deleted_count;
    false
  });

  manifest.total_vectors = manifest.total_vectors - removed_vectors + new_fragment.total_vectors;
  manifest.total_deleted = manifest.total_deleted - removed_deleted + new_fragment.deleted_count;

  for (vector_id, local_index) in moves {
    manifest.vector_locations.insert(
      vector_id,
      VectorLocation {
        fragment_id: new_fragment.id,
        local_index,
      },
    );
  }
  if new_fragment.total_vectors > 0 {
    manifest.fragments.push(new_fragment);
  }
}

/// Run compaction if needed
///
/// # Returns
/// true if compaction was performed
pub fn run_compaction_if_needed(
  manifest: &mut VectorManifest,
  strategy: &CompactionStrategy,
) -> bool {
  let fragment_ids = find_fragments_to_compact(manifest, strategy);
  if fragment_ids.is_empty() {
    return false;
  }

  let result = compact_fragments(manifest, &fragment_ids);
  apply_compaction(manifest, &fragment_ids, result);

  true
}

/// Get compaction statistics
pub fn compaction_stats(manifest: &VectorManifest) -> CompactionStats {
  let mut fragments_needing_compaction = 0;
  let mut potential_space_reclaim = 0;
  let mut total_deleted_vectors = 0;
  let mut total_vectors = 0;

  for fragment in &manifest.fragments {
    if fragment.state == FragmentState::Active {
      continue;
    }
    if fragment.total_vectors == 0 {
      continue;
    }

    let deletion_ratio = fragment.deleted_count as f32 / fragment.total_vectors as f32;
    if deletion_ratio >= 0.3 {
      // Default threshold
      fragments_needing_compaction += 1;
    }

    total_deleted_vectors += fragment.deleted_count;
    total_vectors += fragment.total_vectors;

    // Estimate space reclaim (deleted vectors * vector size)
    potential_space_reclaim +=
      fragment.deleted_count * manifest.config.dimensions * std::mem::size_of::<f32>();
  }

  CompactionStats {
    fragments_needing_compaction,
    potential_space_reclaim,
    total_deleted_vectors,
    average_deletion_ratio: if total_vectors > 0 {
      total_deleted_vectors as f32 / total_vectors as f32
    } else {
      0.0
    },
  }
}

/// Force compaction of all sealed fragments into one
/// Useful for optimizing storage after many deletions
pub fn force_full_compaction(manifest: &mut VectorManifest) {
  let sealed_fragment_ids: Vec<usize> = manifest
    .fragments
    .iter()
    .filter(|f| f.state == FragmentState::Sealed && f.total_vectors > 0)
    .map(|f| f.id)
    .collect();

  if sealed_fragment_ids.is_empty() {
    return;
  }

  let result = compact_fragments(manifest, &sealed_fragment_ids);
  apply_compaction(manifest, &sealed_fragment_ids, result);
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;
  use crate::vector::ivf::{deserialize_manifest, serialize_manifest};
  use crate::vector::types::VectorStoreConfig;
  use crate::vector::{
    create_vector_store, vector_store_delete, vector_store_insert, vector_store_node_vector,
    vector_store_seal_active,
  };

  fn create_test_manifest(dimensions: usize) -> VectorManifest {
    let config = VectorStoreConfig::new(dimensions)
      .with_row_group_size(10)
      .with_fragment_target_size(100)
      .with_normalize(false);
    create_vector_store(config)
  }

  #[test]
  fn test_compaction_strategy_default() {
    let strategy = CompactionStrategy::default();
    assert!((strategy.min_deletion_ratio - 0.3).abs() < 0.001);
    assert_eq!(strategy.max_fragments_per_compaction, 4);
    assert_eq!(strategy.min_vectors_to_compact, 10_000);
  }

  #[test]
  fn test_find_fragments_no_candidates() {
    let manifest = create_test_manifest(4);
    let strategy = CompactionStrategy::default();

    let fragments = find_fragments_to_compact(&manifest, &strategy);
    assert!(fragments.is_empty());
  }

  #[test]
  fn test_clear_deleted_fragments() {
    let mut manifest = create_test_manifest(4);

    // Insert some vectors (non-zero to pass validation)
    for i in 0..20 {
      let vector = vec![1.0 + i as f32, 2.0, 3.0, 4.0];
      vector_store_insert(&mut manifest, i, &vector).expect("expected value");
    }

    // Seal and delete all
    vector_store_seal_active(&mut manifest);
    for i in 0..20 {
      vector_store_delete(&mut manifest, i);
    }

    // Clear deleted fragments
    let cleared = clear_deleted_fragments(&mut manifest);
    assert!(cleared >= 1);
  }

  #[test]
  fn test_compaction_stats() {
    let manifest = create_test_manifest(4);
    let stats = compaction_stats(&manifest);

    assert_eq!(stats.fragments_needing_compaction, 0);
    assert_eq!(stats.total_deleted_vectors, 0);
    assert!((stats.average_deletion_ratio - 0.0).abs() < 0.001);
  }

  #[test]
  fn test_append_to_fragment() {
    let mut fragment = Fragment::new(0);

    let vector = vec![1.0, 2.0, 3.0, 4.0];
    let idx = fragment_append(&mut fragment, &vector, 10, 4);

    assert_eq!(idx, 0);
    assert_eq!(fragment.total_vectors, 1);
    assert_eq!(fragment.row_groups.len(), 1);
    assert_eq!(fragment.row_groups[0].count, 1);
    assert_eq!(fragment.deletion_bitmap.len(), 1);
  }

  fn reload(manifest: &VectorManifest) -> VectorManifest {
    deserialize_manifest(&serialize_manifest(manifest)).expect("manifest reloads")
  }

  /// A result computed before later deletes and inserts still applies: the
  /// vector deleted in between stays deleted, and the new fragment id taken
  /// in between by a seal is replaced.
  #[test]
  fn test_apply_stale_compaction_result() {
    let mut manifest = create_test_manifest(4);
    for i in 0..100u64 {
      vector_store_insert(&mut manifest, i, &[1.0 + i as f32, 2.0, 3.0, 4.0]).expect("insert");
    }
    // Fragment 0 is sealed; fragment 1 is active and empty.
    for i in 0..50u64 {
      vector_store_delete(&mut manifest, i);
    }
    let result = compact_fragments(&manifest, &[0]);
    assert_eq!(result.new_fragment.id, 2);

    vector_store_delete(&mut manifest, 60);
    for i in 100..200u64 {
      // Seals fragment 1 and starts fragment 2.
      vector_store_insert(&mut manifest, i, &[1.0 + i as f32, 2.0, 3.0, 4.0]).expect("insert");
    }
    apply_compaction(&mut manifest, &[0], result);

    let restored = reload(&manifest);
    assert!(vector_store_node_vector(&restored, 60).is_none());
    for i in (50..200u64).filter(|&i| i != 60) {
      assert_eq!(
        vector_store_node_vector(&restored, i),
        vector_store_node_vector(&manifest, i),
        "node {i}"
      );
      assert_eq!(
        vector_store_node_vector(&restored, i).map(|v| v[0]),
        Some(1.0 + i as f32)
      );
    }
  }

  /// Ids stay unique after compaction removes fragments.
  #[test]
  fn test_fragment_ids_stay_unique_after_compaction() {
    let mut manifest = create_test_manifest(4);
    for i in 0..300u64 {
      vector_store_insert(&mut manifest, i, &[1.0 + i as f32, 2.0, 3.0, 4.0]).expect("insert");
    }
    for i in 0..300u64 {
      if i % 3 != 0 {
        vector_store_delete(&mut manifest, i);
      }
    }
    force_full_compaction(&mut manifest);
    for i in 300..650u64 {
      vector_store_insert(&mut manifest, i, &[1.0 + i as f32, 2.0, 3.0, 4.0]).expect("insert");
    }
    let ids: HashSet<usize> = manifest.fragments.iter().map(|f| f.id).collect();
    assert_eq!(ids.len(), manifest.fragments.len());
    reload(&manifest);
  }

  #[test]
  fn test_compaction_result_structure() {
    let mut manifest = create_test_manifest(4);

    // Insert some vectors (non-zero to pass validation)
    for i in 0..10 {
      let vector = vec![1.0 + i as f32, 2.0, 3.0, 4.0];
      vector_store_insert(&mut manifest, i, &vector).expect("expected value");
    }

    // Seal
    vector_store_seal_active(&mut manifest);

    // Delete half
    for i in 0..5 {
      vector_store_delete(&mut manifest, i);
    }

    // Compact (force with low threshold)
    let strategy = CompactionStrategy {
      min_deletion_ratio: 0.1,
      max_fragments_per_compaction: 4,
      min_vectors_to_compact: 1,
    };

    let fragments_to_compact = find_fragments_to_compact(&manifest, &strategy);
    if !fragments_to_compact.is_empty() {
      let result = compact_fragments(&manifest, &fragments_to_compact);
      // New fragment should have the live vectors
      assert!(result.new_fragment.total_vectors <= 5);
    }
  }

  #[test]
  fn test_run_compaction_if_needed_no_work() {
    let mut manifest = create_test_manifest(4);
    let strategy = CompactionStrategy::default();

    let did_compact = run_compaction_if_needed(&mut manifest, &strategy);
    assert!(!did_compact);
  }

  #[test]
  fn test_compaction_preserves_live_vector_count() {
    let mut manifest = create_test_manifest(4);

    for i in 0..200 {
      let vector = vec![1.0 + i as f32, 2.0, 3.0, 4.0];
      vector_store_insert(&mut manifest, i, &vector).expect("expected value");
    }
    vector_store_seal_active(&mut manifest);

    for i in 0..80 {
      vector_store_delete(&mut manifest, i);
    }

    let live_before = manifest.live_count();
    let strategy = CompactionStrategy {
      min_deletion_ratio: 0.2,
      max_fragments_per_compaction: 4,
      min_vectors_to_compact: 1,
    };
    assert!(run_compaction_if_needed(&mut manifest, &strategy));
    assert_eq!(
      manifest.live_count(),
      live_before,
      "compaction must not change logical live vector count",
    );
  }
}
