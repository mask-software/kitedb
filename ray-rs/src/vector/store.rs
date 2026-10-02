//! Columnar vector store
//!
//! Manages fragments, handles inserts/deletes, coordinates with index.
//! This is the main entry point for vector storage operations.
//!
//! Ported from src/vector/columnar-store.ts

use std::collections::{HashMap, HashSet};

use crate::types::NodeId;

use super::distance::normalize_in_place;
use super::types::{
  Fragment, FragmentState, RowGroup, VectorLocation, VectorManifest, VectorStoreConfig,
};

// ============================================================================
// Store Operations
// ============================================================================

/// Create a new vector store with the given configuration
pub fn create_vector_store(config: VectorStoreConfig) -> VectorManifest {
  VectorManifest::new(config)
}

/// Insert a vector into the store
///
/// Returns the global vector ID
pub fn vector_store_insert(
  manifest: &mut VectorManifest,
  node_id: NodeId,
  vector: &[f32],
) -> Result<u64, VectorStoreError> {
  validate_insert(manifest, vector)?;
  delete_existing_vector(manifest, node_id);

  let vec_data = prepare_insert_vector(manifest, vector);

  ensure_active_fragment(manifest);
  let fragment_idx = active_fragment_index(manifest)?;

  let (fragment_id, local_index) = append_to_fragment(manifest, fragment_idx, &vec_data);
  let vector_id = register_vector(manifest, node_id, fragment_id, local_index);

  maybe_seal_fragment(manifest, fragment_idx);

  Ok(vector_id)
}

/// Delete a vector by node ID
///
/// Returns true if deleted, false if not found
pub fn vector_store_delete(manifest: &mut VectorManifest, node_id: NodeId) -> bool {
  let vector_id = match manifest.node_to_vector.get(&node_id).copied() {
    Some(id) => id,
    None => return false,
  };

  vector_store_delete_by_vector_id(manifest, vector_id)
}

/// Delete a vector by vector ID
fn vector_store_delete_by_vector_id(manifest: &mut VectorManifest, vector_id: u64) -> bool {
  let location = match manifest.vector_locations.get(&vector_id).copied() {
    Some(loc) => loc,
    None => return false,
  };

  let fragment = match manifest
    .fragments
    .iter_mut()
    .find(|f| f.id == location.fragment_id)
  {
    Some(f) => f,
    None => return false,
  };

  let deleted = fragment.delete(location.local_index);

  if deleted {
    // Clean up mappings
    if let Some(&node_id) = manifest.vector_to_node.get(&vector_id) {
      manifest.node_to_vector.remove(&node_id);
    }
    manifest.vector_to_node.remove(&vector_id);
    manifest.vector_locations.remove(&vector_id);
    manifest.total_deleted += 1;
  }

  deleted
}

/// Get a vector by node ID
///
/// Returns the vector data as a slice, or None if not found
pub fn vector_store_node_vector(manifest: &VectorManifest, node_id: NodeId) -> Option<&[f32]> {
  let vector_id = manifest.node_to_vector.get(&node_id)?;
  vector_store_vector_by_id(manifest, *vector_id)
}

/// Get a vector by vector ID
pub fn vector_store_vector_by_id(manifest: &VectorManifest, vector_id: u64) -> Option<&[f32]> {
  let location = manifest.vector_locations.get(&vector_id)?;
  let fragment = manifest
    .fragments
    .iter()
    .find(|f| f.id == location.fragment_id)?;

  // Check if deleted
  if fragment.is_deleted(location.local_index) {
    return None;
  }

  // Get from row group
  let row_group_idx = location.local_index / manifest.config.row_group_size;
  let local_row_idx = location.local_index % manifest.config.row_group_size;
  let row_group = fragment.row_groups.get(row_group_idx)?;

  row_group.get(local_row_idx, manifest.config.dimensions)
}

/// Check if a vector exists for a node
pub fn vector_store_has(manifest: &VectorManifest, node_id: NodeId) -> bool {
  let vector_id = match manifest.node_to_vector.get(&node_id) {
    Some(id) => *id,
    None => return false,
  };

  let location = match manifest.vector_locations.get(&vector_id) {
    Some(loc) => loc,
    None => return false,
  };

  let fragment = match manifest
    .fragments
    .iter()
    .find(|f| f.id == location.fragment_id)
  {
    Some(f) => f,
    None => return false,
  };

  !fragment.is_deleted(location.local_index)
}

/// Get the vector ID for a node
pub fn vector_store_vector_id(manifest: &VectorManifest, node_id: NodeId) -> Option<u64> {
  manifest.node_to_vector.get(&node_id).copied()
}

/// Get the node ID for a vector ID
pub fn vector_store_node_id(manifest: &VectorManifest, vector_id: u64) -> Option<NodeId> {
  manifest.vector_to_node.get(&vector_id).copied()
}

/// Get the location of a vector
pub fn vector_store_location(manifest: &VectorManifest, vector_id: u64) -> Option<VectorLocation> {
  manifest.vector_locations.get(&vector_id).copied()
}

// ============================================================================
// Batch Operations
// ============================================================================

/// Batch insert vectors
///
/// Returns array of assigned vector IDs
pub fn vector_store_batch_insert(
  manifest: &mut VectorManifest,
  entries: &[(NodeId, Vec<f32>)],
) -> Result<Vec<u64>, VectorStoreError> {
  let mut vector_ids = Vec::with_capacity(entries.len());

  for (node_id, vector) in entries {
    let vector_id = vector_store_insert(manifest, *node_id, vector)?;
    vector_ids.push(vector_id);
  }

  Ok(vector_ids)
}

/// Get all vectors as a flat Vec<f32> (for training/serialization)
/// Only includes non-deleted vectors
pub fn vector_store_all_vectors(manifest: &VectorManifest) -> (Vec<f32>, Vec<NodeId>, Vec<u64>) {
  let mut data = Vec::new();
  let mut node_ids = Vec::new();
  let mut vector_ids = Vec::new();

  for (&node_id, &vector_id) in &manifest.node_to_vector {
    if let Some(vec) = vector_store_vector_by_id(manifest, vector_id) {
      data.extend_from_slice(vec);
      node_ids.push(node_id);
      vector_ids.push(vector_id);
    }
  }

  (data, node_ids, vector_ids)
}

// ============================================================================
// Store Statistics
// ============================================================================

/// Get store statistics
#[derive(Debug, Clone)]
pub struct VectorStoreStats {
  pub total_vectors: usize,
  pub total_deleted: usize,
  pub live_vectors: usize,
  pub fragment_count: usize,
  pub sealed_fragments: usize,
  pub active_fragment_vectors: usize,
  pub dimensions: usize,
  pub row_group_size: usize,
  pub fragment_target_size: usize,
  pub bytes_used: usize,
}

pub fn vector_store_stats(manifest: &VectorManifest) -> VectorStoreStats {
  let active_fragment = manifest.active_fragment();

  let mut bytes_used = 0;
  for fragment in &manifest.fragments {
    for rg in &fragment.row_groups {
      bytes_used += rg.data.len() * std::mem::size_of::<f32>();
    }
    bytes_used += fragment.deletion_bitmap.len() * std::mem::size_of::<u32>();
  }

  VectorStoreStats {
    total_vectors: manifest.total_vectors,
    total_deleted: manifest.total_deleted,
    live_vectors: manifest.live_count(),
    fragment_count: manifest.fragments.len(),
    sealed_fragments: manifest
      .fragments
      .iter()
      .filter(|f| f.state == FragmentState::Sealed)
      .count(),
    active_fragment_vectors: active_fragment.map(|f| f.total_vectors).unwrap_or(0),
    dimensions: manifest.config.dimensions,
    row_group_size: manifest.config.row_group_size,
    fragment_target_size: manifest.config.fragment_target_size,
    bytes_used,
  }
}

/// Get fragment statistics
#[derive(Debug, Clone)]
pub struct FragmentStats {
  pub id: usize,
  pub state: FragmentState,
  pub total_vectors: usize,
  pub deleted_vectors: usize,
  pub live_vectors: usize,
  pub deletion_ratio: f32,
  pub row_group_count: usize,
}

pub fn vector_store_fragment_stats(manifest: &VectorManifest) -> Vec<FragmentStats> {
  manifest
    .fragments
    .iter()
    .map(|f| FragmentStats {
      id: f.id,
      state: f.state,
      total_vectors: f.total_vectors,
      deleted_vectors: f.deleted_count,
      live_vectors: f.live_count(),
      deletion_ratio: if f.total_vectors > 0 {
        f.deleted_count as f32 / f.total_vectors as f32
      } else {
        0.0
      },
      row_group_count: f.row_groups.len(),
    })
    .collect()
}

// ============================================================================
// Utility Operations
// ============================================================================

/// Seal the active fragment and create a new one
pub fn vector_store_seal_active(manifest: &mut VectorManifest) {
  if let Some(fragment) = manifest.active_fragment_mut() {
    if fragment.state == FragmentState::Active {
      fragment.seal();
      start_active_fragment(manifest);
    }
  }
}

/// Clear all data from the store
pub fn vector_store_clear(manifest: &mut VectorManifest) {
  manifest.fragments.clear();
  manifest.fragments.push(Fragment::new(0));
  manifest.active_fragment_id = 0;
  manifest.total_vectors = 0;
  manifest.total_deleted = 0;
  manifest.next_vector_id = 0;
  manifest.node_to_vector.clear();
  manifest.vector_to_node.clear();
  manifest.vector_locations.clear();
}

/// Validate all invariants required by vector-manifest readers and hot paths.
///
/// This is deliberately kept out of individual lookup functions. A manifest
/// loaded from bytes is validated once, so those functions can retain their
/// branch-light traversal behavior.
pub(crate) fn validate_vector_manifest(manifest: &VectorManifest) -> Result<(), VectorStoreError> {
  let config = &manifest.config;
  validate_store_config(config)?;
  if manifest.fragments.is_empty() {
    return Err(VectorStoreError::Invariant(
      "manifest must contain at least one fragment".into(),
    ));
  }

  let mut fragment_ids = HashSet::with_capacity(manifest.fragments.len());
  let mut active_count = 0usize;
  let mut total_vectors = 0usize;
  let mut total_deleted = 0usize;

  for fragment in &manifest.fragments {
    if !fragment_ids.insert(fragment.id) {
      return Err(VectorStoreError::Invariant(format!(
        "duplicate fragment id {}",
        fragment.id
      )));
    }

    if fragment.state == FragmentState::Active {
      active_count += 1;
    }

    let mut row_group_ids = HashSet::with_capacity(fragment.row_groups.len());
    let mut row_count = 0usize;
    for row_group in &fragment.row_groups {
      if !row_group_ids.insert(row_group.id) {
        return Err(VectorStoreError::Invariant(format!(
          "fragment {} has duplicate row group id {}",
          fragment.id, row_group.id
        )));
      }
      if row_group.count > config.row_group_size {
        return Err(VectorStoreError::Invariant(format!(
          "fragment {} row group {} count {} exceeds row_group_size {}",
          fragment.id, row_group.id, row_group.count, config.row_group_size
        )));
      }

      let expected_data_len = row_group
        .count
        .checked_mul(config.dimensions)
        .ok_or_else(|| {
          VectorStoreError::Invariant(format!(
            "fragment {} row group {} data length overflow",
            fragment.id, row_group.id
          ))
        })?;
      if row_group.data.len() != expected_data_len {
        return Err(VectorStoreError::Invariant(format!(
          "fragment {} row group {} data length {} does not match count {} * dimensions {}",
          fragment.id,
          row_group.id,
          row_group.data.len(),
          row_group.count,
          config.dimensions
        )));
      }
      row_count = row_count.checked_add(row_group.count).ok_or_else(|| {
        VectorStoreError::Invariant(format!("fragment {} row count overflow", fragment.id))
      })?;
    }

    if fragment.total_vectors != row_count {
      return Err(VectorStoreError::Invariant(format!(
        "fragment {} total_vectors {} does not match row count {}",
        fragment.id, fragment.total_vectors, row_count
      )));
    }
    if fragment.deleted_count > fragment.total_vectors {
      return Err(VectorStoreError::Invariant(format!(
        "fragment {} deleted_count {} exceeds total_vectors {}",
        fragment.id, fragment.deleted_count, fragment.total_vectors
      )));
    }

    let expected_bitmap_words = if fragment.total_vectors == 0 {
      0
    } else {
      (fragment.total_vectors - 1) / 32 + 1
    };
    if fragment.deletion_bitmap.len() != expected_bitmap_words {
      return Err(VectorStoreError::Invariant(format!(
        "fragment {} deletion bitmap has {} words, expected {}",
        fragment.id,
        fragment.deletion_bitmap.len(),
        expected_bitmap_words
      )));
    }

    let mut bitmap_deleted = 0usize;
    for (word_index, &word) in fragment.deletion_bitmap.iter().enumerate() {
      if word_index + 1 == expected_bitmap_words {
        let remaining = fragment.total_vectors % 32;
        if remaining != 0 && word & (u32::MAX << remaining) != 0 {
          return Err(VectorStoreError::Invariant(format!(
            "fragment {} deletion bitmap sets bits beyond total_vectors",
            fragment.id
          )));
        }
      }
      bitmap_deleted = bitmap_deleted
        .checked_add(word.count_ones() as usize)
        .ok_or_else(|| {
          VectorStoreError::Invariant(format!(
            "fragment {} deletion bitmap count overflow",
            fragment.id
          ))
        })?;
    }
    if bitmap_deleted != fragment.deleted_count {
      return Err(VectorStoreError::Invariant(format!(
        "fragment {} deletion bitmap count {} does not match deleted_count {}",
        fragment.id, bitmap_deleted, fragment.deleted_count
      )));
    }

    total_vectors = total_vectors
      .checked_add(fragment.total_vectors)
      .ok_or_else(|| VectorStoreError::Invariant("manifest total_vectors overflow".into()))?;
    total_deleted = total_deleted
      .checked_add(fragment.deleted_count)
      .ok_or_else(|| VectorStoreError::Invariant("manifest total_deleted overflow".into()))?;
  }

  if active_count != 1 {
    return Err(VectorStoreError::Invariant(format!(
      "manifest must contain exactly one active fragment, found {active_count}"
    )));
  }
  let active_fragment = manifest
    .fragments
    .iter()
    .find(|fragment| fragment.id == manifest.active_fragment_id)
    .ok_or_else(|| {
      VectorStoreError::Invariant(format!(
        "active fragment id {} is missing",
        manifest.active_fragment_id
      ))
    })?;
  if active_fragment.state != FragmentState::Active {
    return Err(VectorStoreError::Invariant(format!(
      "active fragment id {} is not active",
      manifest.active_fragment_id
    )));
  }

  if manifest.total_vectors != total_vectors {
    return Err(VectorStoreError::Invariant(format!(
      "manifest total_vectors {} does not match fragment total {}",
      manifest.total_vectors, total_vectors
    )));
  }
  if manifest.total_deleted != total_deleted {
    return Err(VectorStoreError::Invariant(format!(
      "manifest total_deleted {} does not match fragment total {}",
      manifest.total_deleted, total_deleted
    )));
  }
  if manifest.total_deleted > manifest.total_vectors {
    return Err(VectorStoreError::Invariant(
      "manifest total_deleted exceeds total_vectors".into(),
    ));
  }

  let live_count = manifest.total_vectors - manifest.total_deleted;
  if manifest.node_to_vector.len() != live_count
    || manifest.vector_to_node.len() != live_count
    || manifest.vector_locations.len() != live_count
  {
    return Err(VectorStoreError::Invariant(format!(
      "live mapping counts do not match live vector count {live_count}"
    )));
  }

  for (&node_id, &vector_id) in &manifest.node_to_vector {
    if manifest.vector_to_node.get(&vector_id) != Some(&node_id) {
      return Err(VectorStoreError::Invariant(format!(
        "node-to-vector mapping for node {node_id} is not bijective"
      )));
    }
    if vector_id >= manifest.next_vector_id {
      return Err(VectorStoreError::Invariant(format!(
        "vector id {vector_id} is not below next_vector_id {}",
        manifest.next_vector_id
      )));
    }
    let location = manifest.vector_locations.get(&vector_id).ok_or_else(|| {
      VectorStoreError::Invariant(format!("vector id {vector_id} has no location"))
    })?;
    validate_live_location(manifest, vector_id, location)?;
  }

  for (&vector_id, &node_id) in &manifest.vector_to_node {
    if manifest.node_to_vector.get(&node_id) != Some(&vector_id) {
      return Err(VectorStoreError::Invariant(format!(
        "vector-to-node mapping for vector {vector_id} is not bijective"
      )));
    }
  }

  for (&vector_id, location) in &manifest.vector_locations {
    if !manifest.vector_to_node.contains_key(&vector_id) {
      return Err(VectorStoreError::Invariant(format!(
        "location exists for unmapped vector id {vector_id}"
      )));
    }
    validate_live_location(manifest, vector_id, location)?;
  }

  Ok(())
}

/// Cheap shape check for a manifest handed to an ANN index entry point.
///
/// O(row groups), so search can run it on every call (unlike
/// `validate_vector_manifest`, which walks every vector). Together with the
/// `local_row < count` check at lookup time it keeps every vector slice in
/// bounds.
pub(crate) fn validate_manifest_layout(
  manifest: &VectorManifest,
  dimensions: usize,
) -> Result<(), VectorStoreError> {
  let config = &manifest.config;
  if config.dimensions != dimensions {
    return Err(VectorStoreError::DimensionMismatch {
      expected: dimensions,
      got: config.dimensions,
    });
  }
  if config.row_group_size == 0 {
    return Err(VectorStoreError::Invariant(
      "manifest row_group_size must be nonzero".into(),
    ));
  }
  for fragment in &manifest.fragments {
    for row_group in &fragment.row_groups {
      let expected = row_group.count.checked_mul(dimensions);
      if expected != Some(row_group.data.len()) {
        return Err(VectorStoreError::Invariant(format!(
          "fragment {} row group {} data length {} does not match count {} * dimensions {}",
          fragment.id,
          row_group.id,
          row_group.data.len(),
          row_group.count,
          dimensions
        )));
      }
    }
  }
  Ok(())
}

fn validate_live_location(
  manifest: &VectorManifest,
  vector_id: u64,
  location: &VectorLocation,
) -> Result<(), VectorStoreError> {
  let fragment = manifest
    .fragments
    .iter()
    .find(|fragment| fragment.id == location.fragment_id)
    .ok_or_else(|| {
      VectorStoreError::Invariant(format!(
        "vector id {vector_id} references missing fragment {}",
        location.fragment_id
      ))
    })?;
  let row_group_index = location.local_index / manifest.config.row_group_size;
  let local_row_index = location.local_index % manifest.config.row_group_size;
  let row_group = fragment.row_groups.get(row_group_index).ok_or_else(|| {
    VectorStoreError::Invariant(format!(
      "vector id {vector_id} references missing row group {row_group_index}"
    ))
  })?;
  if local_row_index >= row_group.count {
    return Err(VectorStoreError::Invariant(format!(
      "vector id {vector_id} local index {} is outside row group",
      location.local_index
    )));
  }
  if fragment.is_deleted(location.local_index) {
    return Err(VectorStoreError::Invariant(format!(
      "vector id {vector_id} references a deleted vector"
    )));
  }
  Ok(())
}

/// Rejects configurations a manifest cannot be stored with: they would divide
/// by zero on reads, and a reload rejects them.
pub(crate) fn validate_store_config(config: &VectorStoreConfig) -> Result<(), VectorStoreError> {
  if config.dimensions == 0 {
    return Err(VectorStoreError::Invariant(
      "manifest dimensions must be nonzero".into(),
    ));
  }
  if config.row_group_size == 0 {
    return Err(VectorStoreError::Invariant(
      "manifest row_group_size must be nonzero".into(),
    ));
  }
  if config.fragment_target_size == 0 {
    return Err(VectorStoreError::Invariant(
      "manifest fragment_target_size must be nonzero".into(),
    ));
  }
  Ok(())
}

/// An id no fragment in the manifest uses. Compaction removes fragments, so
/// the fragment count is not a free id.
pub(crate) fn next_fragment_id(manifest: &VectorManifest) -> usize {
  manifest
    .fragments
    .iter()
    .map(|fragment| fragment.id + 1)
    .max()
    .unwrap_or(0)
}

/// Appends `vector` to `fragment`, growing its row groups and deletion
/// bitmap, and returns the vector's local index.
pub(crate) fn fragment_append(
  fragment: &mut Fragment,
  vector: &[f32],
  row_group_size: usize,
  dimensions: usize,
) -> usize {
  let row_group_idx = ensure_row_group(fragment, row_group_size, dimensions);
  let local_row_idx = fragment.row_groups[row_group_idx].append(vector);
  let local_index = row_group_idx * row_group_size + local_row_idx;
  extend_deletion_bitmap(fragment, local_index);
  fragment.total_vectors += 1;
  local_index
}

/// O(1) fragment lookup by id, for loops over many vectors.
pub(crate) enum FragmentLookup<'a> {
  /// Every fragment's id equals its position (no compaction has run).
  Dense(&'a [Fragment]),
  Sparse(HashMap<usize, &'a Fragment>),
}

impl<'a> FragmentLookup<'a> {
  pub(crate) fn new(manifest: &'a VectorManifest) -> Self {
    let fragments = manifest.fragments.as_slice();
    if fragments
      .iter()
      .enumerate()
      .all(|(position, fragment)| fragment.id == position)
    {
      FragmentLookup::Dense(fragments)
    } else {
      FragmentLookup::Sparse(fragments.iter().map(|f| (f.id, f)).collect())
    }
  }

  #[inline]
  pub(crate) fn get(&self, fragment_id: usize) -> Option<&'a Fragment> {
    match self {
      FragmentLookup::Dense(fragments) => fragments.get(fragment_id),
      FragmentLookup::Sparse(map) => map.get(&fragment_id).copied(),
    }
  }

  /// Live vector data at `location`, or None if it is deleted or outside the
  /// fragment. The manifest must have passed `validate_manifest_layout`.
  #[inline]
  pub(crate) fn vector(
    &self,
    config: &VectorStoreConfig,
    location: &VectorLocation,
  ) -> Option<&'a [f32]> {
    let fragment = self.get(location.fragment_id)?;
    if fragment.is_deleted(location.local_index) {
      return None;
    }
    let row_group = fragment
      .row_groups
      .get(location.local_index / config.row_group_size)?;
    row_group.get(
      location.local_index % config.row_group_size,
      config.dimensions,
    )
  }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Ensure there's an active fragment available for writes
fn ensure_active_fragment(manifest: &mut VectorManifest) {
  let needs_new = manifest
    .active_fragment()
    .map(|f| f.state == FragmentState::Sealed)
    .unwrap_or(true);

  if needs_new {
    start_active_fragment(manifest);
  }
}

/// Adds an empty active fragment under a fresh id and makes it the target.
fn start_active_fragment(manifest: &mut VectorManifest) {
  let new_id = next_fragment_id(manifest);
  manifest.fragments.push(Fragment::new(new_id));
  manifest.active_fragment_id = new_id;
}

fn validate_insert(manifest: &VectorManifest, vector: &[f32]) -> Result<(), VectorStoreError> {
  validate_store_config(&manifest.config)?;
  let dimensions = manifest.config.dimensions;
  if vector.len() != dimensions {
    return Err(VectorStoreError::DimensionMismatch {
      expected: dimensions,
      got: vector.len(),
    });
  }

  validate_vector(vector)
}

fn delete_existing_vector(manifest: &mut VectorManifest, node_id: NodeId) {
  if let Some(existing_vector_id) = manifest.node_to_vector.get(&node_id).copied() {
    vector_store_delete_by_vector_id(manifest, existing_vector_id);
  }
}

fn prepare_insert_vector(manifest: &VectorManifest, vector: &[f32]) -> Vec<f32> {
  let mut vec_data = vector.to_vec();
  if manifest.config.normalize_on_insert {
    normalize_in_place(&mut vec_data);
  }
  vec_data
}

fn active_fragment_index(manifest: &VectorManifest) -> Result<usize, VectorStoreError> {
  let fragment_id = manifest.active_fragment_id;
  manifest
    .fragments
    .iter()
    .position(|f| f.id == fragment_id)
    .ok_or_else(|| {
      VectorStoreError::Invariant(format!(
        "active fragment id {fragment_id} missing from manifest"
      ))
    })
}

fn append_to_fragment(
  manifest: &mut VectorManifest,
  fragment_idx: usize,
  vec_data: &[f32],
) -> (usize, usize) {
  let row_group_size = manifest.config.row_group_size;
  let dimensions = manifest.config.dimensions;
  let fragment = &mut manifest.fragments[fragment_idx];
  let local_index = fragment_append(fragment, vec_data, row_group_size, dimensions);
  (fragment.id, local_index)
}

fn ensure_row_group(fragment: &mut Fragment, row_group_size: usize, dimensions: usize) -> usize {
  if fragment
    .row_groups
    .last()
    .map(|rg| rg.is_full(row_group_size))
    .unwrap_or(true)
  {
    let rg_id = fragment.row_groups.len();
    fragment
      .row_groups
      .push(RowGroup::new(rg_id, row_group_size, dimensions));
    rg_id
  } else {
    fragment.row_groups.len() - 1
  }
}

fn extend_deletion_bitmap(fragment: &mut Fragment, local_index: usize) {
  let word_idx = local_index / 32;
  while fragment.deletion_bitmap.len() <= word_idx {
    fragment.deletion_bitmap.push(0);
  }
}

fn register_vector(
  manifest: &mut VectorManifest,
  node_id: NodeId,
  fragment_id: usize,
  local_index: usize,
) -> u64 {
  let vector_id = manifest.next_vector_id;
  manifest.next_vector_id += 1;

  manifest.node_to_vector.insert(node_id, vector_id);
  manifest.vector_to_node.insert(vector_id, node_id);
  manifest.vector_locations.insert(
    vector_id,
    VectorLocation {
      fragment_id,
      local_index,
    },
  );
  manifest.total_vectors += 1;

  vector_id
}

fn maybe_seal_fragment(manifest: &mut VectorManifest, fragment_idx: usize) {
  let fragment_target_size = manifest.config.fragment_target_size;
  let fragment = &mut manifest.fragments[fragment_idx];
  if fragment.total_vectors >= fragment_target_size {
    fragment.seal();
    start_active_fragment(manifest);
  }
}

/// Validate vector for NaN, Infinity, and zero vectors
pub(crate) fn validate_vector(vector: &[f32]) -> Result<(), VectorStoreError> {
  let mut all_zero = true;

  for &val in vector {
    if val.is_nan() {
      return Err(VectorStoreError::InvalidVector(
        "Vector contains NaN".into(),
      ));
    }
    if val.is_infinite() {
      return Err(VectorStoreError::InvalidVector(
        "Vector contains Infinity".into(),
      ));
    }
    if val != 0.0 {
      all_zero = false;
    }
  }

  if all_zero {
    return Err(VectorStoreError::InvalidVector(
      "Vector is all zeros".into(),
    ));
  }

  Ok(())
}

// ============================================================================
// Errors
// ============================================================================

/// Errors that can occur in the vector store
#[derive(Debug, Clone)]
pub enum VectorStoreError {
  /// Vector dimension mismatch
  DimensionMismatch { expected: usize, got: usize },
  /// Invalid vector (NaN, Infinity, or zero)
  InvalidVector(String),
  /// Vector not found
  NotFound(u64),
  /// Internal invariant violation
  Invariant(String),
}

impl std::fmt::Display for VectorStoreError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      VectorStoreError::DimensionMismatch { expected, got } => {
        write!(
          f,
          "Vector dimension mismatch: expected {expected}, got {got}"
        )
      }
      VectorStoreError::InvalidVector(msg) => write!(f, "Invalid vector: {msg}"),
      VectorStoreError::NotFound(id) => write!(f, "Vector not found: {id}"),
      VectorStoreError::Invariant(msg) => write!(f, "Invariant violation: {msg}"),
    }
  }
}

impl std::error::Error for VectorStoreError {}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;

  fn test_config() -> VectorStoreConfig {
    VectorStoreConfig::new(4)
      .with_row_group_size(10)
      .with_fragment_target_size(100)
  }

  #[test]
  fn test_create_vector_store() {
    let config = test_config();
    let manifest = create_vector_store(config);

    assert_eq!(manifest.total_vectors, 0);
    assert_eq!(manifest.fragments.len(), 1);
  }

  #[test]
  fn test_insert_vector() {
    let config = test_config();
    let mut manifest = create_vector_store(config);

    let vector = vec![1.0, 0.0, 0.0, 0.0];
    let vector_id = vector_store_insert(&mut manifest, 1, &vector).expect("expected value");

    assert_eq!(vector_id, 0);
    assert_eq!(manifest.total_vectors, 1);
    assert!(vector_store_has(&manifest, 1));
  }

  #[test]
  fn test_insert_dimension_mismatch() {
    let config = test_config();
    let mut manifest = create_vector_store(config);

    let vector = vec![1.0, 0.0, 0.0]; // Wrong dimension
    let result = vector_store_insert(&mut manifest, 1, &vector);

    assert!(matches!(
      result,
      Err(VectorStoreError::DimensionMismatch { .. })
    ));
  }

  #[test]
  fn test_insert_invalid_nan() {
    let config = test_config();
    let mut manifest = create_vector_store(config);

    let vector = vec![1.0, f32::NAN, 0.0, 0.0];
    let result = vector_store_insert(&mut manifest, 1, &vector);

    assert!(matches!(result, Err(VectorStoreError::InvalidVector(_))));
  }

  #[test]
  fn test_insert_invalid_zero() {
    let config = test_config();
    let mut manifest = create_vector_store(config);

    let vector = vec![0.0, 0.0, 0.0, 0.0];
    let result = vector_store_insert(&mut manifest, 1, &vector);

    assert!(matches!(result, Err(VectorStoreError::InvalidVector(_))));
  }

  #[test]
  fn test_vector() {
    let config = test_config().with_normalize(false);
    let mut manifest = create_vector_store(config);

    let vector = vec![1.0, 2.0, 3.0, 4.0];
    vector_store_insert(&mut manifest, 1, &vector).expect("expected value");

    let retrieved = vector_store_node_vector(&manifest, 1).expect("expected value");
    assert_eq!(retrieved, &vector[..]);
  }

  #[test]
  fn test_vector_normalized() {
    let config = test_config().with_normalize(true);
    let mut manifest = create_vector_store(config);

    let vector = vec![3.0, 4.0, 0.0, 0.0]; // norm = 5
    vector_store_insert(&mut manifest, 1, &vector).expect("expected value");

    let retrieved = vector_store_node_vector(&manifest, 1).expect("expected value");
    assert!((retrieved[0] - 0.6).abs() < 1e-6);
    assert!((retrieved[1] - 0.8).abs() < 1e-6);
  }

  #[test]
  fn test_delete_vector() {
    let config = test_config();
    let mut manifest = create_vector_store(config);

    let vector = vec![1.0, 0.0, 0.0, 0.0];
    vector_store_insert(&mut manifest, 1, &vector).expect("expected value");

    assert!(vector_store_has(&manifest, 1));
    assert!(vector_store_delete(&mut manifest, 1));
    assert!(!vector_store_has(&manifest, 1));
    assert_eq!(manifest.total_deleted, 1);
  }

  #[test]
  fn test_delete_nonexistent() {
    let config = test_config();
    let mut manifest = create_vector_store(config);

    assert!(!vector_store_delete(&mut manifest, 999));
  }

  #[test]
  fn test_replace_vector() {
    let config = test_config().with_normalize(false);
    let mut manifest = create_vector_store(config);

    let vector1 = vec![1.0, 0.0, 0.0, 0.0];
    let id1 = vector_store_insert(&mut manifest, 1, &vector1).expect("expected value");

    let vector2 = vec![0.0, 1.0, 0.0, 0.0];
    let id2 = vector_store_insert(&mut manifest, 1, &vector2).expect("expected value");

    // Should have different IDs (old was deleted)
    assert_ne!(id1, id2);
    assert_eq!(manifest.total_deleted, 1);

    // Should retrieve new vector
    let retrieved = vector_store_node_vector(&manifest, 1).expect("expected value");
    assert_eq!(retrieved, &vector2[..]);
  }

  #[test]
  fn test_multiple_vectors() {
    let config = test_config().with_normalize(false);
    let mut manifest = create_vector_store(config);

    for i in 0..20 {
      let vector = vec![i as f32, 0.0, 0.0, 1.0];
      vector_store_insert(&mut manifest, i as u64, &vector).expect("expected value");
    }

    assert_eq!(manifest.total_vectors, 20);

    // Check a few
    let v5 = vector_store_node_vector(&manifest, 5).expect("expected value");
    assert_eq!(v5[0], 5.0);

    let v15 = vector_store_node_vector(&manifest, 15).expect("expected value");
    assert_eq!(v15[0], 15.0);
  }

  #[test]
  fn test_batch_insert() {
    let config = test_config().with_normalize(false);
    let mut manifest = create_vector_store(config);

    let entries: Vec<(NodeId, Vec<f32>)> = (0..10)
      .map(|i| (i as u64, vec![i as f32, 0.0, 0.0, 1.0]))
      .collect();

    let ids = vector_store_batch_insert(&mut manifest, &entries).expect("expected value");

    assert_eq!(ids.len(), 10);
    assert_eq!(manifest.total_vectors, 10);
  }

  #[test]
  fn test_all_vectors() {
    let config = test_config().with_normalize(false);
    let mut manifest = create_vector_store(config);

    for i in 0..5 {
      let vector = vec![i as f32, 0.0, 0.0, 1.0];
      vector_store_insert(&mut manifest, i as u64, &vector).expect("expected value");
    }

    // Delete one
    vector_store_delete(&mut manifest, 2);

    let (data, node_ids, vector_ids) = vector_store_all_vectors(&manifest);

    assert_eq!(node_ids.len(), 4); // 5 - 1 deleted
    assert_eq!(vector_ids.len(), 4);
    assert_eq!(data.len(), 4 * 4); // 4 vectors * 4 dimensions
  }

  #[test]
  fn test_store_stats() {
    let config = test_config();
    let mut manifest = create_vector_store(config);

    for i in 0..5 {
      let vector = vec![i as f32, 0.0, 0.0, 1.0];
      vector_store_insert(&mut manifest, i as u64, &vector).expect("expected value");
    }

    vector_store_delete(&mut manifest, 2);

    let stats = vector_store_stats(&manifest);
    assert_eq!(stats.total_vectors, 5);
    assert_eq!(stats.total_deleted, 1);
    assert_eq!(stats.live_vectors, 4);
    assert_eq!(stats.dimensions, 4);
  }

  #[test]
  fn test_clear() {
    let config = test_config();
    let mut manifest = create_vector_store(config);

    for i in 0..5 {
      let vector = vec![i as f32, 0.0, 0.0, 1.0];
      vector_store_insert(&mut manifest, i as u64, &vector).expect("expected value");
    }

    vector_store_clear(&mut manifest);

    assert_eq!(manifest.total_vectors, 0);
    assert_eq!(manifest.fragments.len(), 1);
    assert!(!vector_store_has(&manifest, 0));
  }

  #[test]
  fn test_seal_active() {
    let config = test_config();
    let mut manifest = create_vector_store(config);

    let vector = vec![1.0, 0.0, 0.0, 0.0];
    vector_store_insert(&mut manifest, 1, &vector).expect("expected value");

    vector_store_seal_active(&mut manifest);

    assert_eq!(manifest.fragments.len(), 2);
    assert_eq!(manifest.fragments[0].state, FragmentState::Sealed);
    assert_eq!(manifest.fragments[1].state, FragmentState::Active);
  }

  #[test]
  fn test_fragment_auto_seal() {
    let config = VectorStoreConfig::new(4)
      .with_row_group_size(10)
      .with_fragment_target_size(5); // Very small for testing

    let mut manifest = create_vector_store(config);

    // Insert more than fragment target size
    for i in 0..10 {
      let vector = vec![i as f32, 0.0, 0.0, 1.0];
      vector_store_insert(&mut manifest, i as u64, &vector).expect("expected value");
    }

    // Should have multiple fragments
    assert!(manifest.fragments.len() >= 2);

    // First fragment should be sealed
    assert_eq!(manifest.fragments[0].state, FragmentState::Sealed);
  }

  #[test]
  fn test_error_display() {
    let err1 = VectorStoreError::DimensionMismatch {
      expected: 128,
      got: 64,
    };
    assert!(err1.to_string().contains("128"));
    assert!(err1.to_string().contains("64"));

    let err2 = VectorStoreError::InvalidVector("test error".into());
    assert!(err2.to_string().contains("test error"));

    let err3 = VectorStoreError::NotFound(42);
    assert!(err3.to_string().contains("42"));

    let err4 = VectorStoreError::Invariant("broken".into());
    assert!(err4.to_string().contains("broken"));
  }
}
