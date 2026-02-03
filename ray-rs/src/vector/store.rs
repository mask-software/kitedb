//! Columnar vector store
//!
//! Manages fragments, handles inserts/deletes, coordinates with index.
//! This is the main entry point for vector storage operations.
//!
//! Ported from src/vector/columnar-store.ts

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
pub fn vector_store_get(manifest: &VectorManifest, node_id: NodeId) -> Option<&[f32]> {
  let vector_id = manifest.node_to_vector.get(&node_id)?;
  vector_store_get_by_id(manifest, *vector_id)
}

/// Get a vector by vector ID
pub fn vector_store_get_by_id(manifest: &VectorManifest, vector_id: u64) -> Option<&[f32]> {
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
pub fn vector_store_get_vector_id(manifest: &VectorManifest, node_id: NodeId) -> Option<u64> {
  manifest.node_to_vector.get(&node_id).copied()
}

/// Get the node ID for a vector ID
pub fn vector_store_get_node_id(manifest: &VectorManifest, vector_id: u64) -> Option<NodeId> {
  manifest.vector_to_node.get(&vector_id).copied()
}

/// Get the location of a vector
pub fn vector_store_get_location(
  manifest: &VectorManifest,
  vector_id: u64,
) -> Option<VectorLocation> {
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
pub fn vector_store_get_all_vectors(
  manifest: &VectorManifest,
) -> (Vec<f32>, Vec<NodeId>, Vec<u64>) {
  let live_count = manifest.live_count();
  let dimensions = manifest.config.dimensions;

  let mut data = Vec::with_capacity(live_count * dimensions);
  let mut node_ids = Vec::with_capacity(live_count);
  let mut vector_ids = Vec::with_capacity(live_count);

  for (&node_id, &vector_id) in &manifest.node_to_vector {
    if let Some(vec) = vector_store_get_by_id(manifest, vector_id) {
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

      let new_id = manifest.fragments.len();
      manifest.fragments.push(Fragment::new(new_id));
      manifest.active_fragment_id = new_id;
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
    let new_id = manifest.fragments.len();
    manifest.fragments.push(Fragment::new(new_id));
    manifest.active_fragment_id = new_id;
  }
}

fn validate_insert(manifest: &VectorManifest, vector: &[f32]) -> Result<(), VectorStoreError> {
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
  let fragment_id = manifest.fragments[fragment_idx].id;

  let row_group_idx = ensure_row_group(
    &mut manifest.fragments[fragment_idx],
    row_group_size,
    dimensions,
  );

  let fragment = &mut manifest.fragments[fragment_idx];
  let local_row_idx = fragment.row_groups[row_group_idx].append(vec_data);
  let local_index = row_group_idx * row_group_size + local_row_idx;

  extend_deletion_bitmap(fragment, local_index);
  fragment.total_vectors += 1;

  (fragment_id, local_index)
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
    let new_id = manifest.fragments.len();
    manifest.fragments.push(Fragment::new(new_id));
    manifest.active_fragment_id = new_id;
  }
}

/// Validate vector for NaN, Infinity, and zero vectors
fn validate_vector(vector: &[f32]) -> Result<(), VectorStoreError> {
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
    let vector_id = vector_store_insert(&mut manifest, 1, &vector).unwrap();

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
  fn test_get_vector() {
    let config = test_config().with_normalize(false);
    let mut manifest = create_vector_store(config);

    let vector = vec![1.0, 2.0, 3.0, 4.0];
    vector_store_insert(&mut manifest, 1, &vector).unwrap();

    let retrieved = vector_store_get(&manifest, 1).unwrap();
    assert_eq!(retrieved, &vector[..]);
  }

  #[test]
  fn test_get_vector_normalized() {
    let config = test_config().with_normalize(true);
    let mut manifest = create_vector_store(config);

    let vector = vec![3.0, 4.0, 0.0, 0.0]; // norm = 5
    vector_store_insert(&mut manifest, 1, &vector).unwrap();

    let retrieved = vector_store_get(&manifest, 1).unwrap();
    assert!((retrieved[0] - 0.6).abs() < 1e-6);
    assert!((retrieved[1] - 0.8).abs() < 1e-6);
  }

  #[test]
  fn test_delete_vector() {
    let config = test_config();
    let mut manifest = create_vector_store(config);

    let vector = vec![1.0, 0.0, 0.0, 0.0];
    vector_store_insert(&mut manifest, 1, &vector).unwrap();

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
    let id1 = vector_store_insert(&mut manifest, 1, &vector1).unwrap();

    let vector2 = vec![0.0, 1.0, 0.0, 0.0];
    let id2 = vector_store_insert(&mut manifest, 1, &vector2).unwrap();

    // Should have different IDs (old was deleted)
    assert_ne!(id1, id2);
    assert_eq!(manifest.total_deleted, 1);

    // Should retrieve new vector
    let retrieved = vector_store_get(&manifest, 1).unwrap();
    assert_eq!(retrieved, &vector2[..]);
  }

  #[test]
  fn test_multiple_vectors() {
    let config = test_config().with_normalize(false);
    let mut manifest = create_vector_store(config);

    for i in 0..20 {
      let vector = vec![i as f32, 0.0, 0.0, 1.0];
      vector_store_insert(&mut manifest, i as u64, &vector).unwrap();
    }

    assert_eq!(manifest.total_vectors, 20);

    // Check a few
    let v5 = vector_store_get(&manifest, 5).unwrap();
    assert_eq!(v5[0], 5.0);

    let v15 = vector_store_get(&manifest, 15).unwrap();
    assert_eq!(v15[0], 15.0);
  }

  #[test]
  fn test_batch_insert() {
    let config = test_config().with_normalize(false);
    let mut manifest = create_vector_store(config);

    let entries: Vec<(NodeId, Vec<f32>)> = (0..10)
      .map(|i| (i as u64, vec![i as f32, 0.0, 0.0, 1.0]))
      .collect();

    let ids = vector_store_batch_insert(&mut manifest, &entries).unwrap();

    assert_eq!(ids.len(), 10);
    assert_eq!(manifest.total_vectors, 10);
  }

  #[test]
  fn test_get_all_vectors() {
    let config = test_config().with_normalize(false);
    let mut manifest = create_vector_store(config);

    for i in 0..5 {
      let vector = vec![i as f32, 0.0, 0.0, 1.0];
      vector_store_insert(&mut manifest, i as u64, &vector).unwrap();
    }

    // Delete one
    vector_store_delete(&mut manifest, 2);

    let (data, node_ids, vector_ids) = vector_store_get_all_vectors(&manifest);

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
      vector_store_insert(&mut manifest, i as u64, &vector).unwrap();
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
      vector_store_insert(&mut manifest, i as u64, &vector).unwrap();
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
    vector_store_insert(&mut manifest, 1, &vector).unwrap();

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
      vector_store_insert(&mut manifest, i as u64, &vector).unwrap();
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
