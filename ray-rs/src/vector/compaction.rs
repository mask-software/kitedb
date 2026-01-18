//! Fragment compaction to remove deleted vectors
//!
//! Compaction creates a new fragment containing only live vectors
//! from one or more source fragments.
//!
//! Ported from src/vector/compaction.ts

use std::collections::{HashMap, HashSet};

use crate::vector::types::{
  Fragment, FragmentState, RowGroup, VectorLocation, VectorManifest, VectorStoreConfig,
};

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
  candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

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
/// # Returns
/// Number of fragments cleared
pub fn clear_deleted_fragments(manifest: &mut VectorManifest) -> usize {
  let mut cleared = 0;

  for fragment in &mut manifest.fragments {
    // Skip active fragment
    if fragment.state == FragmentState::Active {
      continue;
    }

    // Skip fragments with no vectors (already cleared)
    if fragment.total_vectors == 0 {
      continue;
    }

    // Check if all vectors are deleted
    if fragment.deleted_count == fragment.total_vectors {
      // Clear the fragment data
      fragment.row_groups.clear();
      fragment.deletion_bitmap.clear();
      manifest.total_deleted -= fragment.deleted_count;
      fragment.total_vectors = 0;
      fragment.deleted_count = 0;
      cleared += 1;
    }
  }

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
/// # Arguments
/// * `manifest` - The vector store manifest
/// * `fragment_ids` - IDs of fragments to compact
///
/// # Returns
/// The new compacted fragment and updated location mappings
pub fn compact_fragments(manifest: &VectorManifest, fragment_ids: &[usize]) -> CompactionResult {
  let config = &manifest.config;
  let dimensions = config.dimensions;
  let row_group_size = config.row_group_size;

  let new_fragment_id = manifest.fragments.len();
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
    let fragment = match manifest.fragments.iter().find(|f| f.id == fragment_id) {
      Some(f) => f,
      None => continue,
    };

    // Iterate over all vectors in fragment
    for local_idx in 0..fragment.total_vectors {
      // Skip deleted vectors
      if fragment.is_deleted(local_idx) {
        continue;
      }

      // Get vector data
      let row_group_idx = local_idx / row_group_size;
      let local_row_idx = local_idx % row_group_size;

      let row_group = match fragment.row_groups.get(row_group_idx) {
        Some(rg) => rg,
        None => continue,
      };

      let offset = local_row_idx * dimensions;
      if offset + dimensions > row_group.data.len() {
        continue;
      }
      let vector = &row_group.data[offset..offset + dimensions];

      // Find the vector_id for this location
      let vector_id = match location_to_vector_id.get(&(fragment_id, local_idx)) {
        Some(&id) => id,
        None => continue,
      };

      // Append to new fragment (skip normalization since already normalized)
      let new_local_idx = append_to_fragment(&mut new_fragment, vector, config);

      // Record updated location
      updated_locations.insert(
        vector_id,
        VectorLocation {
          fragment_id: new_fragment_id,
          local_index: new_local_idx,
        },
      );
    }
  }

  // Seal the new fragment
  new_fragment.seal();

  CompactionResult {
    new_fragment,
    updated_locations,
  }
}

/// Append a vector to a fragment
fn append_to_fragment(
  fragment: &mut Fragment,
  vector: &[f32],
  config: &VectorStoreConfig,
) -> usize {
  let dimensions = config.dimensions;
  let row_group_size = config.row_group_size;

  // Get or create the active row group
  let rg_idx = fragment.total_vectors / row_group_size;

  while fragment.row_groups.len() <= rg_idx {
    fragment.row_groups.push(RowGroup::new(
      fragment.row_groups.len(),
      row_group_size,
      dimensions,
    ));
  }

  let row_group = &mut fragment.row_groups[rg_idx];
  row_group.data.extend_from_slice(vector);
  row_group.count += 1;

  let local_idx = fragment.total_vectors;
  fragment.total_vectors += 1;

  local_idx
}

/// Apply compaction results to manifest
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
  // Add new fragment
  manifest.fragments.push(result.new_fragment);

  // Update vector locations
  for (vector_id, location) in result.updated_locations {
    manifest.vector_locations.insert(vector_id, location);
  }

  // Update deleted count
  let mut removed_deleted = 0;
  for &fragment_id in fragment_ids {
    if let Some(fragment) = manifest.fragments.iter().find(|f| f.id == fragment_id) {
      removed_deleted += fragment.deleted_count;
    }
  }
  manifest.total_deleted -= removed_deleted;

  // Mark old fragments as empty (keep IDs but clear data)
  for &fragment_id in fragment_ids {
    if let Some(fragment) = manifest.fragments.iter_mut().find(|f| f.id == fragment_id) {
      fragment.row_groups.clear();
      fragment.deletion_bitmap.clear();
      fragment.total_vectors = 0;
      fragment.deleted_count = 0;
      fragment.state = FragmentState::Sealed;
    }
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
pub fn get_compaction_stats(manifest: &VectorManifest) -> CompactionStats {
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
  use crate::vector::{
    create_vector_store, vector_store_delete, vector_store_insert, vector_store_seal_active,
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
      vector_store_insert(&mut manifest, i, &vector).unwrap();
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
    let stats = get_compaction_stats(&manifest);

    assert_eq!(stats.fragments_needing_compaction, 0);
    assert_eq!(stats.total_deleted_vectors, 0);
    assert!((stats.average_deletion_ratio - 0.0).abs() < 0.001);
  }

  #[test]
  fn test_append_to_fragment() {
    let config = VectorStoreConfig::new(4).with_row_group_size(10);
    let mut fragment = Fragment::new(0);

    let vector = vec![1.0, 2.0, 3.0, 4.0];
    let idx = append_to_fragment(&mut fragment, &vector, &config);

    assert_eq!(idx, 0);
    assert_eq!(fragment.total_vectors, 1);
    assert_eq!(fragment.row_groups.len(), 1);
    assert_eq!(fragment.row_groups[0].count, 1);
  }

  #[test]
  fn test_compaction_result_structure() {
    let mut manifest = create_test_manifest(4);

    // Insert some vectors (non-zero to pass validation)
    for i in 0..10 {
      let vector = vec![1.0 + i as f32, 2.0, 3.0, 4.0];
      vector_store_insert(&mut manifest, i, &vector).unwrap();
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
}
