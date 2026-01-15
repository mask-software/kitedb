//! Vector embedding types
//!
//! Defines core types for vector storage and search.
//!
//! Ported from src/vector/types.ts

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::types::NodeId;

// ============================================================================
// Distance Metric
// ============================================================================

/// Distance metric for vector similarity
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DistanceMetric {
  /// Cosine similarity (converted to distance as 1 - similarity)
  #[default]
  Cosine,
  /// Euclidean distance (L2)
  Euclidean,
  /// Dot product (for inner product search)
  DotProduct,
}

impl DistanceMetric {
  /// Get distance function for this metric
  pub fn distance_fn(&self) -> fn(&[f32], &[f32]) -> f32 {
    match self {
      DistanceMetric::Cosine => super::distance::cosine_distance,
      DistanceMetric::Euclidean => super::distance::euclidean_distance,
      DistanceMetric::DotProduct => |a, b| -super::distance::dot_product(a, b),
    }
  }

  /// Convert distance to similarity score (0-1 range, higher is more similar)
  pub fn distance_to_similarity(&self, distance: f32) -> f32 {
    match self {
      DistanceMetric::Cosine => 1.0 - distance,
      DistanceMetric::Euclidean => 1.0 / (1.0 + distance),
      DistanceMetric::DotProduct => -distance, // Negate back to get dot product
    }
  }
}

// ============================================================================
// Multi-Query Aggregation
// ============================================================================

/// Aggregation method for multi-query vector search
///
/// When searching with multiple query vectors, this determines how
/// distances from different queries are combined for each candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MultiQueryAggregation {
  /// Use minimum distance (best match across any query)
  #[default]
  Min,
  /// Use maximum distance (worst match across queries)
  Max,
  /// Use average distance
  Avg,
  /// Use sum of distances
  Sum,
}

impl MultiQueryAggregation {
  /// Aggregate a slice of distances
  pub fn aggregate(&self, distances: &[f32]) -> f32 {
    if distances.is_empty() {
      return f32::INFINITY;
    }

    match self {
      MultiQueryAggregation::Min => distances.iter().cloned().fold(f32::INFINITY, f32::min),
      MultiQueryAggregation::Max => distances.iter().cloned().fold(f32::NEG_INFINITY, f32::max),
      MultiQueryAggregation::Avg => distances.iter().sum::<f32>() / distances.len() as f32,
      MultiQueryAggregation::Sum => distances.iter().sum(),
    }
  }
}

// ============================================================================
// Vector Store Configuration
// ============================================================================

/// Configuration for vector store
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorStoreConfig {
  /// Number of dimensions per vector
  pub dimensions: usize,
  /// Distance metric for similarity search
  pub metric: DistanceMetric,
  /// Number of vectors per row group (affects memory locality)
  pub row_group_size: usize,
  /// Target number of vectors per fragment before sealing
  pub fragment_target_size: usize,
  /// Whether to normalize vectors on insert (for cosine similarity)
  pub normalize_on_insert: bool,
}

impl Default for VectorStoreConfig {
  fn default() -> Self {
    Self {
      dimensions: 384,
      metric: DistanceMetric::Cosine,
      row_group_size: 1024,
      fragment_target_size: 100_000,
      normalize_on_insert: true,
    }
  }
}

impl VectorStoreConfig {
  /// Create a new config with the given dimensions
  pub fn new(dimensions: usize) -> Self {
    Self {
      dimensions,
      ..Default::default()
    }
  }

  /// Set the distance metric
  pub fn with_metric(mut self, metric: DistanceMetric) -> Self {
    self.metric = metric;
    self
  }

  /// Set the row group size
  pub fn with_row_group_size(mut self, size: usize) -> Self {
    self.row_group_size = size;
    self
  }

  /// Set the fragment target size
  pub fn with_fragment_target_size(mut self, size: usize) -> Self {
    self.fragment_target_size = size;
    self
  }

  /// Set whether to normalize vectors on insert
  pub fn with_normalize(mut self, normalize: bool) -> Self {
    self.normalize_on_insert = normalize;
    self
  }
}

// ============================================================================
// Row Group
// ============================================================================

/// A row group containing a batch of vectors in columnar format
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RowGroup {
  /// Row group ID within fragment
  pub id: usize,
  /// Number of vectors in this row group
  pub count: usize,
  /// Vector data (contiguous f32 array: count * dimensions)
  pub data: Vec<f32>,
}

impl RowGroup {
  /// Create a new empty row group
  pub fn new(id: usize, capacity: usize, dimensions: usize) -> Self {
    Self {
      id,
      count: 0,
      data: Vec::with_capacity(capacity * dimensions),
    }
  }

  /// Check if row group is full
  pub fn is_full(&self, row_group_size: usize) -> bool {
    self.count >= row_group_size
  }

  /// Get a vector at the given local index
  pub fn get(&self, index: usize, dimensions: usize) -> Option<&[f32]> {
    if index >= self.count {
      return None;
    }
    let offset = index * dimensions;
    Some(&self.data[offset..offset + dimensions])
  }

  /// Append a vector to this row group
  /// Returns the local index within the row group
  pub fn append(&mut self, vector: &[f32]) -> usize {
    let index = self.count;
    self.data.extend_from_slice(vector);
    self.count += 1;
    index
  }
}

// ============================================================================
// Fragment
// ============================================================================

/// Fragment state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FragmentState {
  /// Fragment is still accepting writes
  #[default]
  Active,
  /// Fragment is sealed (read-only)
  Sealed,
}

/// A fragment containing multiple row groups
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fragment {
  /// Fragment ID
  pub id: usize,
  /// Fragment state
  pub state: FragmentState,
  /// Row groups within this fragment
  pub row_groups: Vec<RowGroup>,
  /// Total number of vectors across all row groups
  pub total_vectors: usize,
  /// Deletion bitmap (bit per vector, 1 = deleted)
  pub deletion_bitmap: Vec<u32>,
  /// Count of deleted vectors
  pub deleted_count: usize,
}

impl Fragment {
  /// Create a new empty fragment
  pub fn new(id: usize) -> Self {
    Self {
      id,
      state: FragmentState::Active,
      row_groups: Vec::new(),
      total_vectors: 0,
      deletion_bitmap: Vec::new(),
      deleted_count: 0,
    }
  }

  /// Check if a vector at the given index is deleted
  pub fn is_deleted(&self, index: usize) -> bool {
    let word_idx = index / 32;
    let bit_idx = index % 32;
    if word_idx >= self.deletion_bitmap.len() {
      return false;
    }
    (self.deletion_bitmap[word_idx] & (1 << bit_idx)) != 0
  }

  /// Mark a vector as deleted
  pub fn delete(&mut self, index: usize) -> bool {
    if index >= self.total_vectors || self.is_deleted(index) {
      return false;
    }

    let word_idx = index / 32;
    let bit_idx = index % 32;

    // Extend bitmap if needed
    while self.deletion_bitmap.len() <= word_idx {
      self.deletion_bitmap.push(0);
    }

    self.deletion_bitmap[word_idx] |= 1 << bit_idx;
    self.deleted_count += 1;
    true
  }

  /// Get the number of live (non-deleted) vectors
  pub fn live_count(&self) -> usize {
    self.total_vectors - self.deleted_count
  }

  /// Check if fragment should be sealed based on config
  pub fn should_seal(&self, config: &VectorStoreConfig) -> bool {
    self.total_vectors >= config.fragment_target_size
  }

  /// Seal the fragment (make read-only)
  pub fn seal(&mut self) {
    self.state = FragmentState::Sealed;
  }
}

// ============================================================================
// Vector Location
// ============================================================================

/// Location of a vector in the store
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct VectorLocation {
  /// Fragment ID
  pub fragment_id: usize,
  /// Local index within the fragment
  pub local_index: usize,
}

// ============================================================================
// Vector Manifest
// ============================================================================

/// Manifest for the vector store
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorManifest {
  /// Store configuration
  pub config: VectorStoreConfig,
  /// All fragments
  pub fragments: Vec<Fragment>,
  /// Active fragment ID (receiving writes)
  pub active_fragment_id: usize,
  /// Total number of vectors inserted
  pub total_vectors: usize,
  /// Total number of deleted vectors
  pub total_deleted: usize,
  /// Next vector ID to assign
  pub next_vector_id: u64,
  /// NodeId -> VectorId mapping
  pub node_to_vector: HashMap<NodeId, u64>,
  /// VectorId -> NodeId mapping
  pub vector_to_node: HashMap<u64, NodeId>,
  /// VectorId -> Location mapping
  pub vector_locations: HashMap<u64, VectorLocation>,
}

impl VectorManifest {
  /// Create a new manifest with the given config
  pub fn new(config: VectorStoreConfig) -> Self {
    let initial_fragment = Fragment::new(0);
    Self {
      config,
      fragments: vec![initial_fragment],
      active_fragment_id: 0,
      total_vectors: 0,
      total_deleted: 0,
      next_vector_id: 0,
      node_to_vector: HashMap::new(),
      vector_to_node: HashMap::new(),
      vector_locations: HashMap::new(),
    }
  }

  /// Get the active fragment
  pub fn active_fragment(&self) -> Option<&Fragment> {
    self
      .fragments
      .iter()
      .find(|f| f.id == self.active_fragment_id)
  }

  /// Get the active fragment mutably
  pub fn active_fragment_mut(&mut self) -> Option<&mut Fragment> {
    let id = self.active_fragment_id;
    self.fragments.iter_mut().find(|f| f.id == id)
  }

  /// Get live vector count
  pub fn live_count(&self) -> usize {
    self.total_vectors - self.total_deleted
  }
}

// ============================================================================
// IVF Configuration
// ============================================================================

/// Configuration for IVF (Inverted File) index
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IvfConfig {
  /// Number of clusters (centroids)
  pub n_clusters: usize,
  /// Number of clusters to probe during search
  pub n_probe: usize,
  /// Distance metric
  pub metric: DistanceMetric,
}

impl Default for IvfConfig {
  fn default() -> Self {
    Self {
      n_clusters: 100,
      n_probe: 10,
      metric: DistanceMetric::Cosine,
    }
  }
}

impl IvfConfig {
  /// Create a new IVF config with the given number of clusters
  pub fn new(n_clusters: usize) -> Self {
    Self {
      n_clusters,
      ..Default::default()
    }
  }

  /// Set the number of clusters to probe
  pub fn with_n_probe(mut self, n_probe: usize) -> Self {
    self.n_probe = n_probe;
    self
  }

  /// Set the distance metric
  pub fn with_metric(mut self, metric: DistanceMetric) -> Self {
    self.metric = metric;
    self
  }
}

// ============================================================================
// PQ Configuration
// ============================================================================

/// Configuration for Product Quantization
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PqConfig {
  /// Number of subspaces (M). Dimensions must be divisible by M
  pub num_subspaces: usize,
  /// Number of centroids per subspace (K). Typically 256 for uint8 codes
  pub num_centroids: usize,
  /// K-means iterations for training
  pub max_iterations: usize,
}

impl Default for PqConfig {
  fn default() -> Self {
    Self {
      num_subspaces: 48,  // Good for 384D (8 dims per subspace)
      num_centroids: 256, // uint8 codes
      max_iterations: 20,
    }
  }
}

// ============================================================================
// Search Results
// ============================================================================

/// Vector search result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorSearchResult {
  /// Vector ID
  pub vector_id: u64,
  /// Associated node ID
  pub node_id: NodeId,
  /// Distance to query (lower is closer)
  pub distance: f32,
  /// Similarity score (higher is more similar)
  pub similarity: f32,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_distance_metric_default() {
    assert_eq!(DistanceMetric::default(), DistanceMetric::Cosine);
  }

  #[test]
  fn test_vector_store_config_default() {
    let config = VectorStoreConfig::default();
    assert_eq!(config.dimensions, 384);
    assert_eq!(config.metric, DistanceMetric::Cosine);
    assert!(config.normalize_on_insert);
  }

  #[test]
  fn test_vector_store_config_builder() {
    let config = VectorStoreConfig::new(768)
      .with_metric(DistanceMetric::Euclidean)
      .with_row_group_size(512)
      .with_normalize(false);

    assert_eq!(config.dimensions, 768);
    assert_eq!(config.metric, DistanceMetric::Euclidean);
    assert_eq!(config.row_group_size, 512);
    assert!(!config.normalize_on_insert);
  }

  #[test]
  fn test_row_group_new() {
    let rg = RowGroup::new(0, 100, 128);
    assert_eq!(rg.id, 0);
    assert_eq!(rg.count, 0);
    assert!(!rg.is_full(100));
  }

  #[test]
  fn test_row_group_append() {
    let mut rg = RowGroup::new(0, 10, 4);
    let vec1 = [1.0, 2.0, 3.0, 4.0];
    let idx = rg.append(&vec1);

    assert_eq!(idx, 0);
    assert_eq!(rg.count, 1);

    let retrieved = rg.get(0, 4).unwrap();
    assert_eq!(retrieved, &vec1);
  }

  #[test]
  fn test_row_group_full() {
    let mut rg = RowGroup::new(0, 10, 4);
    for i in 0..10 {
      rg.append(&[i as f32; 4]);
    }
    assert!(rg.is_full(10));
  }

  #[test]
  fn test_fragment_new() {
    let frag = Fragment::new(0);
    assert_eq!(frag.id, 0);
    assert_eq!(frag.state, FragmentState::Active);
    assert_eq!(frag.total_vectors, 0);
    assert_eq!(frag.live_count(), 0);
  }

  #[test]
  fn test_fragment_deletion() {
    let mut frag = Fragment::new(0);
    frag.total_vectors = 100;

    // Delete vector at index 5
    assert!(frag.delete(5));
    assert!(frag.is_deleted(5));
    assert!(!frag.is_deleted(4));
    assert_eq!(frag.deleted_count, 1);
    assert_eq!(frag.live_count(), 99);

    // Try to delete again (should fail)
    assert!(!frag.delete(5));
    assert_eq!(frag.deleted_count, 1);
  }

  #[test]
  fn test_fragment_seal() {
    let mut frag = Fragment::new(0);
    assert_eq!(frag.state, FragmentState::Active);

    frag.seal();
    assert_eq!(frag.state, FragmentState::Sealed);
  }

  #[test]
  fn test_vector_manifest_new() {
    let config = VectorStoreConfig::new(128);
    let manifest = VectorManifest::new(config);

    assert_eq!(manifest.total_vectors, 0);
    assert_eq!(manifest.fragments.len(), 1);
    assert_eq!(manifest.active_fragment_id, 0);
  }

  #[test]
  fn test_ivf_config_default() {
    let config = IvfConfig::default();
    assert_eq!(config.n_clusters, 100);
    assert_eq!(config.n_probe, 10);
    assert_eq!(config.metric, DistanceMetric::Cosine);
  }

  #[test]
  fn test_pq_config_default() {
    let config = PqConfig::default();
    assert_eq!(config.num_subspaces, 48);
    assert_eq!(config.num_centroids, 256);
    assert_eq!(config.max_iterations, 20);
  }

  #[test]
  fn test_distance_to_similarity() {
    // Cosine: 1 - distance
    assert_eq!(DistanceMetric::Cosine.distance_to_similarity(0.2), 0.8);

    // Euclidean: 1 / (1 + distance)
    assert_eq!(DistanceMetric::Euclidean.distance_to_similarity(1.0), 0.5);
  }
}
