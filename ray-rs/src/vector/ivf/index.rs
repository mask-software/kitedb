//! IVF (Inverted File) index for approximate nearest neighbor search
//!
//! Algorithm:
//! 1. Training: Run k-means to find cluster centroids
//! 2. Insert: Assign each vector to nearest centroid
//! 3. Search: Find nearest centroids, then search their vectors
//!
//! This is more disk-friendly than HNSW and works well with columnar storage.
//!
//! Ported from src/vector/ivf-index.ts

use std::collections::HashMap;

use crate::types::NodeId;
use crate::vector::distance::normalize;
use crate::vector::types::{
  DistanceMetric, IvfConfig, MultiQueryAggregation, VectorManifest, VectorSearchResult,
};

use super::kmeans::{kmeans_parallel, KMeansConfig};

// ============================================================================
// IVF Index
// ============================================================================

/// IVF (Inverted File) index for approximate nearest neighbor search
#[derive(Debug)]
pub struct IvfIndex {
  /// Configuration
  pub config: IvfConfig,
  /// Cluster centroids (n_clusters * dimensions)
  pub centroids: Vec<f32>,
  /// Inverted lists: cluster -> vector IDs
  pub inverted_lists: HashMap<usize, Vec<u64>>,
  /// Number of dimensions
  pub dimensions: usize,
  /// Whether the index has been trained
  pub trained: bool,
  /// Training vectors buffer
  training_vectors: Option<Vec<f32>>,
  /// Number of training vectors
  training_count: usize,
}

impl IvfIndex {
  /// Create a new IVF index
  pub fn new(dimensions: usize, config: IvfConfig) -> Self {
    Self {
      config,
      centroids: Vec::new(),
      inverted_lists: HashMap::new(),
      dimensions,
      trained: false,
      training_vectors: Some(Vec::new()),
      training_count: 0,
    }
  }

  /// Create a new IVF index with default configuration
  pub fn with_defaults(dimensions: usize) -> Self {
    Self::new(dimensions, IvfConfig::default())
  }

  /// Create an IVF index from serialized data
  ///
  /// Used by deserialization to reconstruct an index.
  pub fn from_serialized(
    config: IvfConfig,
    centroids: Vec<f32>,
    inverted_lists: HashMap<usize, Vec<u64>>,
    dimensions: usize,
    trained: bool,
  ) -> Self {
    Self {
      config,
      centroids,
      inverted_lists,
      dimensions,
      trained,
      training_vectors: None,
      training_count: 0,
    }
  }

  /// Add vectors for training
  ///
  /// Call this before `train()` to provide training data.
  pub fn add_training_vectors(&mut self, vectors: &[f32], count: usize) -> Result<(), IvfError> {
    if self.trained {
      return Err(IvfError::AlreadyTrained);
    }

    let expected_len = count * self.dimensions;
    if vectors.len() < expected_len {
      return Err(IvfError::DimensionMismatch {
        expected: expected_len,
        got: vectors.len(),
      });
    }

    let training_buf = self.training_vectors.get_or_insert_with(Vec::new);
    training_buf.extend_from_slice(&vectors[..expected_len]);
    self.training_count += count;

    Ok(())
  }

  /// Train the index using k-means clustering
  pub fn train(&mut self) -> Result<(), IvfError> {
    if self.trained {
      return Ok(());
    }

    let training_vectors = self
      .training_vectors
      .take()
      .ok_or(IvfError::NoTrainingVectors)?;

    if self.training_count < self.config.n_clusters {
      return Err(IvfError::NotEnoughTrainingVectors {
        n: self.training_count,
        k: self.config.n_clusters,
      });
    }

    // Get distance function
    let distance_fn = self.config.metric.distance_fn();

    // Run k-means (uses parallel version for large datasets)
    let kmeans_config = KMeansConfig::new(self.config.n_clusters)
      .with_max_iterations(25)
      .with_tolerance(1e-4);

    let result = kmeans_parallel(
      &training_vectors,
      self.training_count,
      self.dimensions,
      &kmeans_config,
      distance_fn,
    )
    .map_err(|e| IvfError::TrainingFailed(e.to_string()))?;

    self.centroids = result.centroids;

    // Initialize inverted lists
    for c in 0..self.config.n_clusters {
      self.inverted_lists.insert(c, Vec::new());
    }

    self.trained = true;
    self.training_vectors = None;
    self.training_count = 0;

    Ok(())
  }

  /// Insert a vector into the index
  ///
  /// The vector should already be stored in the manifest; this just adds it to the index.
  pub fn insert(&mut self, vector_id: u64, vector: &[f32]) -> Result<(), IvfError> {
    if !self.trained {
      return Err(IvfError::NotTrained);
    }

    if vector.len() != self.dimensions {
      return Err(IvfError::DimensionMismatch {
        expected: self.dimensions,
        got: vector.len(),
      });
    }

    // Find nearest centroid
    let cluster = self.find_nearest_centroid(vector);

    // Add to inverted list
    self
      .inverted_lists
      .entry(cluster)
      .or_default()
      .push(vector_id);

    Ok(())
  }

  /// Delete a vector from the index
  ///
  /// Returns true if deleted, false if not found.
  pub fn delete(&mut self, vector_id: u64, vector: &[f32]) -> bool {
    if !self.trained {
      return false;
    }

    // Find which cluster it's in
    let cluster = self.find_nearest_centroid(vector);

    if let Some(list) = self.inverted_lists.get_mut(&cluster) {
      if let Some(idx) = list.iter().position(|&id| id == vector_id) {
        // Remove from list (swap with last for O(1))
        list.swap_remove(idx);
        return true;
      }
    }

    false
  }

  /// Search for k nearest neighbors
  pub fn search(
    &self,
    manifest: &VectorManifest,
    query: &[f32],
    k: usize,
    options: Option<SearchOptions>,
  ) -> Vec<VectorSearchResult> {
    if !self.trained {
      return Vec::new();
    }

    let options = options.unwrap_or_default();
    let n_probe = options.n_probe.unwrap_or(self.config.n_probe);

    // Prepare query vector
    let query_vec = if self.config.metric == DistanceMetric::Cosine {
      normalize(query)
    } else {
      query.to_vec()
    };

    // Find top n_probe nearest centroids
    let probe_clusters = self.find_nearest_centroids(&query_vec, n_probe);

    // Get distance function
    let distance_fn = self.config.metric.distance_fn();

    // Build fragment lookup map for O(1) access (avoid .find() in hot loop)
    let fragment_map: HashMap<usize, &_> = manifest.fragments.iter().map(|f| (f.id, f)).collect();

    // Use max-heap to track top-k candidates
    let mut heap = MaxHeap::new();

    // Search within selected clusters
    for cluster in probe_clusters {
      let vector_ids = match self.inverted_lists.get(&cluster) {
        Some(list) if !list.is_empty() => list,
        _ => continue,
      };

      for &vector_id in vector_ids {
        // Get vector location
        let location = match manifest.vector_locations.get(&vector_id) {
          Some(loc) => loc,
          None => continue,
        };

        // Get fragment with O(1) lookup
        let fragment = match fragment_map.get(&location.fragment_id) {
          Some(f) => *f,
          None => continue,
        };

        // Check deletion bitmap
        if fragment.is_deleted(location.local_index) {
          continue;
        }

        // Apply filter if provided
        if let Some(ref filter) = options.filter {
          if let Some(&node_id) = manifest.vector_to_node.get(&vector_id) {
            if !filter(node_id) {
              continue;
            }
          }
        }

        // Get vector data
        let row_group_idx = location.local_index / manifest.config.row_group_size;
        let local_row_idx = location.local_index % manifest.config.row_group_size;
        let row_group = match fragment.row_groups.get(row_group_idx) {
          Some(rg) if local_row_idx < rg.count => rg,
          _ => continue,
        };

        let offset = local_row_idx * manifest.config.dimensions;
        let vec = &row_group.data[offset..offset + manifest.config.dimensions];

        // Compute distance
        let dist = distance_fn(&query_vec, vec);

        // Apply threshold filter
        if let Some(threshold) = options.threshold {
          let similarity = self.config.metric.distance_to_similarity(dist);
          if similarity < threshold {
            continue;
          }
        }

        // Add to heap
        if heap.len() < k {
          heap.push(vector_id, dist);
        } else if let Some(&(_, max_dist)) = heap.peek() {
          if dist < max_dist {
            heap.pop();
            heap.push(vector_id, dist);
          }
        }
      }
    }

    // Convert to results
    let results = heap.into_sorted_vec();

    results
      .into_iter()
      .map(|(vector_id, distance)| {
        let node_id = manifest
          .vector_to_node
          .get(&vector_id)
          .copied()
          .unwrap_or(0);
        VectorSearchResult {
          vector_id,
          node_id,
          distance,
          similarity: self.config.metric.distance_to_similarity(distance),
        }
      })
      .collect()
  }

  /// Search with multiple query vectors
  ///
  /// This is more efficient than running multiple separate searches because it:
  /// 1. Collects all candidate vectors across all queries
  /// 2. Aggregates distances per node using the specified aggregation method
  /// 3. Returns the top-k results based on aggregated distances
  ///
  /// # Arguments
  /// * `manifest` - The vector store manifest
  /// * `queries` - Array of query vectors (all must have same dimensions)
  /// * `k` - Number of results to return
  /// * `aggregation` - How to aggregate distances from multiple queries
  /// * `options` - Search options (n_probe, filter, threshold)
  ///
  /// # Returns
  /// Vector of search results sorted by aggregated distance
  pub fn search_multi(
    &self,
    manifest: &VectorManifest,
    queries: &[&[f32]],
    k: usize,
    aggregation: MultiQueryAggregation,
    options: Option<SearchOptions>,
  ) -> Vec<VectorSearchResult> {
    if !self.trained || queries.is_empty() {
      return Vec::new();
    }

    let options = options.unwrap_or_default();

    // Run individual searches with higher k to ensure we have enough candidates
    let expanded_k = k * 2;
    let all_results: Vec<Vec<VectorSearchResult>> = queries
      .iter()
      .map(|query| self.search(manifest, query, expanded_k, None))
      .collect();

    // Aggregate by node_id
    let mut aggregated: HashMap<NodeId, (Vec<f32>, u64)> = HashMap::new();

    for results in &all_results {
      for result in results {
        let entry = aggregated
          .entry(result.node_id)
          .or_insert_with(|| (Vec::new(), result.vector_id));
        entry.0.push(result.distance);
      }
    }

    // Apply filter if provided
    let aggregated: HashMap<NodeId, (Vec<f32>, u64)> = if let Some(ref filter) = options.filter {
      aggregated
        .into_iter()
        .filter(|(node_id, _)| filter(*node_id))
        .collect()
    } else {
      aggregated
    };

    // Compute aggregated scores and build results
    let mut scored: Vec<VectorSearchResult> = aggregated
      .into_iter()
      .map(|(node_id, (distances, vector_id))| {
        let distance = aggregation.aggregate(&distances);
        let similarity = self.config.metric.distance_to_similarity(distance);
        VectorSearchResult {
          vector_id,
          node_id,
          distance,
          similarity,
        }
      })
      .collect();

    // Apply threshold filter
    if let Some(threshold) = options.threshold {
      scored.retain(|r| r.similarity >= threshold);
    }

    // Sort by distance and return top k
    scored.sort_by(|a, b| {
      a.distance
        .partial_cmp(&b.distance)
        .unwrap_or(std::cmp::Ordering::Equal)
    });
    scored.truncate(k);

    scored
  }

  /// Build index from all vectors in the store
  pub fn build_from_store(&mut self, manifest: &VectorManifest) -> Result<(), IvfError> {
    // Collect training vectors
    for fragment in &manifest.fragments {
      for row_group in &fragment.row_groups {
        self.add_training_vectors(&row_group.data, row_group.count)?;
      }
    }

    // Train the index
    self.train()?;

    // Build fragment lookup map for O(1) access
    let fragment_map: HashMap<usize, &_> = manifest.fragments.iter().map(|f| (f.id, f)).collect();

    // Insert all vectors
    for (&_node_id, &vector_id) in &manifest.node_to_vector {
      let location = match manifest.vector_locations.get(&vector_id) {
        Some(loc) => loc,
        None => continue,
      };

      // Get fragment with O(1) lookup
      let fragment = match fragment_map.get(&location.fragment_id) {
        Some(f) => *f,
        None => continue,
      };

      if fragment.is_deleted(location.local_index) {
        continue;
      }

      let row_group_idx = location.local_index / manifest.config.row_group_size;
      let local_row_idx = location.local_index % manifest.config.row_group_size;
      let row_group = match fragment.row_groups.get(row_group_idx) {
        Some(rg) => rg,
        None => continue,
      };

      let offset = local_row_idx * manifest.config.dimensions;
      let vector = &row_group.data[offset..offset + manifest.config.dimensions];

      self.insert(vector_id, vector)?;
    }

    Ok(())
  }

  /// Get index statistics
  pub fn stats(&self) -> IvfStats {
    let mut total = 0;
    let mut empty = 0;
    let mut min_size = usize::MAX;
    let mut max_size = 0;

    for list in self.inverted_lists.values() {
      total += list.len();
      if list.is_empty() {
        empty += 1;
      }
      min_size = min_size.min(list.len());
      max_size = max_size.max(list.len());
    }

    if self.inverted_lists.is_empty() {
      min_size = 0;
    }

    IvfStats {
      trained: self.trained,
      n_clusters: self.config.n_clusters,
      total_vectors: total,
      avg_vectors_per_cluster: if self.config.n_clusters > 0 {
        total as f32 / self.config.n_clusters as f32
      } else {
        0.0
      },
      empty_cluster_count: empty,
      min_cluster_size: min_size,
      max_cluster_size: max_size,
    }
  }

  /// Clear the index (but keep configuration)
  pub fn clear(&mut self) {
    self.centroids.clear();
    self.inverted_lists.clear();
    self.trained = false;
    self.training_vectors = Some(Vec::new());
    self.training_count = 0;
  }

  // ========================================================================
  // Helper Methods
  // ========================================================================

  /// Find nearest centroid for a vector
  fn find_nearest_centroid(&self, vector: &[f32]) -> usize {
    let distance_fn = self.config.metric.distance_fn();

    // Prepare query vector (normalize for cosine metric)
    let query_vec = if self.config.metric == DistanceMetric::Cosine {
      normalize(vector)
    } else {
      vector.to_vec()
    };

    let mut best_cluster = 0;
    let mut best_dist = f32::INFINITY;

    for c in 0..self.config.n_clusters {
      let cent_offset = c * self.dimensions;
      let centroid = &self.centroids[cent_offset..cent_offset + self.dimensions];
      let dist = distance_fn(&query_vec, centroid);

      if dist < best_dist {
        best_dist = dist;
        best_cluster = c;
      }
    }

    best_cluster
  }

  /// Find the top n nearest centroids
  fn find_nearest_centroids(&self, query: &[f32], n: usize) -> Vec<usize> {
    let distance_fn = self.config.metric.distance_fn();

    let mut centroid_dists: Vec<(usize, f32)> = (0..self.config.n_clusters)
      .map(|c| {
        let cent_offset = c * self.dimensions;
        let centroid = &self.centroids[cent_offset..cent_offset + self.dimensions];
        let dist = distance_fn(query, centroid);
        (c, dist)
      })
      .collect();

    centroid_dists.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

    centroid_dists.into_iter().take(n).map(|(c, _)| c).collect()
  }
}

// ============================================================================
// Search Options
// ============================================================================

/// Options for IVF search
#[derive(Default)]
pub struct SearchOptions {
  /// Number of clusters to probe (overrides config)
  pub n_probe: Option<usize>,
  /// Filter function (return true to include)
  pub filter: Option<Box<dyn Fn(NodeId) -> bool>>,
  /// Minimum similarity threshold
  pub threshold: Option<f32>,
}

impl std::fmt::Debug for SearchOptions {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("SearchOptions")
      .field("n_probe", &self.n_probe)
      .field("filter", &self.filter.as_ref().map(|_| "<fn>"))
      .field("threshold", &self.threshold)
      .finish()
  }
}

// ============================================================================
// Statistics
// ============================================================================

/// IVF index statistics
#[derive(Debug, Clone)]
pub struct IvfStats {
  pub trained: bool,
  pub n_clusters: usize,
  pub total_vectors: usize,
  pub avg_vectors_per_cluster: f32,
  pub empty_cluster_count: usize,
  pub min_cluster_size: usize,
  pub max_cluster_size: usize,
}

// ============================================================================
// Max Heap for Top-K
// ============================================================================

/// Simple max-heap for top-k selection
struct MaxHeap {
  items: Vec<(u64, f32)>, // (vector_id, distance)
}

impl MaxHeap {
  fn new() -> Self {
    Self { items: Vec::new() }
  }

  fn len(&self) -> usize {
    self.items.len()
  }

  fn push(&mut self, id: u64, dist: f32) {
    self.items.push((id, dist));
    self.sift_up(self.items.len() - 1);
  }

  fn pop(&mut self) -> Option<(u64, f32)> {
    if self.items.is_empty() {
      return None;
    }
    let len = self.items.len();
    self.items.swap(0, len - 1);
    let result = self.items.pop();
    if !self.items.is_empty() {
      self.sift_down(0);
    }
    result
  }

  fn peek(&self) -> Option<&(u64, f32)> {
    self.items.first()
  }

  fn sift_up(&mut self, mut idx: usize) {
    while idx > 0 {
      let parent = (idx - 1) / 2;
      if self.items[idx].1 > self.items[parent].1 {
        self.items.swap(idx, parent);
        idx = parent;
      } else {
        break;
      }
    }
  }

  fn sift_down(&mut self, mut idx: usize) {
    let len = self.items.len();
    loop {
      let left = 2 * idx + 1;
      let right = 2 * idx + 2;
      let mut largest = idx;

      if left < len && self.items[left].1 > self.items[largest].1 {
        largest = left;
      }
      if right < len && self.items[right].1 > self.items[largest].1 {
        largest = right;
      }

      if largest != idx {
        self.items.swap(idx, largest);
        idx = largest;
      } else {
        break;
      }
    }
  }

  fn into_sorted_vec(mut self) -> Vec<(u64, f32)> {
    let mut result = Vec::with_capacity(self.items.len());
    while let Some(item) = self.pop() {
      result.push(item);
    }
    result.reverse();
    result
  }
}

// ============================================================================
// Errors
// ============================================================================

#[derive(Debug, Clone)]
pub enum IvfError {
  AlreadyTrained,
  NotTrained,
  NoTrainingVectors,
  NotEnoughTrainingVectors { n: usize, k: usize },
  DimensionMismatch { expected: usize, got: usize },
  TrainingFailed(String),
}

impl std::fmt::Display for IvfError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      IvfError::AlreadyTrained => write!(f, "Index already trained"),
      IvfError::NotTrained => write!(f, "Index not trained"),
      IvfError::NoTrainingVectors => write!(f, "No training vectors provided"),
      IvfError::NotEnoughTrainingVectors { n, k } => {
        write!(f, "Not enough training vectors: {n} < {k} clusters")
      }
      IvfError::DimensionMismatch { expected, got } => {
        write!(f, "Dimension mismatch: expected {expected}, got {got}")
      }
      IvfError::TrainingFailed(msg) => write!(f, "Training failed: {msg}"),
    }
  }
}

impl std::error::Error for IvfError {}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;
  use crate::vector::types::{MultiQueryAggregation, VectorManifest, VectorStoreConfig};

  fn create_test_index(dimensions: usize, n_clusters: usize) -> IvfIndex {
    IvfIndex::new(dimensions, IvfConfig::new(n_clusters).with_n_probe(2))
  }

  #[test]
  fn test_ivf_new() {
    let index = create_test_index(128, 10);
    assert!(!index.trained);
    assert_eq!(index.dimensions, 128);
    assert_eq!(index.config.n_clusters, 10);
  }

  #[test]
  fn test_ivf_add_training_vectors() {
    let mut index = create_test_index(4, 2);

    let vectors = vec![1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    index.add_training_vectors(&vectors, 2).unwrap();

    assert_eq!(index.training_count, 2);
  }

  #[test]
  fn test_ivf_train() {
    let mut index = create_test_index(4, 2);

    // Add enough training vectors
    let mut vectors = Vec::new();
    for i in 0..10 {
      vectors.extend_from_slice(&[i as f32, 0.0, 0.0, 1.0]);
    }
    index.add_training_vectors(&vectors, 10).unwrap();

    index.train().unwrap();

    assert!(index.trained);
    assert_eq!(index.centroids.len(), 2 * 4);
  }

  #[test]
  fn test_ivf_train_not_enough_vectors() {
    let mut index = create_test_index(4, 10);

    let vectors = vec![1.0, 0.0, 0.0, 0.0];
    index.add_training_vectors(&vectors, 1).unwrap();

    let result = index.train();
    assert!(matches!(
      result,
      Err(IvfError::NotEnoughTrainingVectors { .. })
    ));
  }

  #[test]
  fn test_ivf_insert() {
    let mut index = create_test_index(4, 2);

    // Train first
    let mut vectors = Vec::new();
    for i in 0..10 {
      vectors.extend_from_slice(&[i as f32, 0.0, 0.0, 1.0]);
    }
    index.add_training_vectors(&vectors, 10).unwrap();
    index.train().unwrap();

    // Insert
    let vector = vec![5.0, 0.0, 0.0, 1.0];
    index.insert(0, &vector).unwrap();

    let stats = index.stats();
    assert_eq!(stats.total_vectors, 1);
  }

  #[test]
  fn test_ivf_insert_not_trained() {
    let mut index = create_test_index(4, 2);

    let vector = vec![1.0, 0.0, 0.0, 0.0];
    let result = index.insert(0, &vector);

    assert!(matches!(result, Err(IvfError::NotTrained)));
  }

  #[test]
  fn test_ivf_delete() {
    let mut index = create_test_index(4, 2);

    // Train
    let mut vectors = Vec::new();
    for i in 0..10 {
      vectors.extend_from_slice(&[i as f32, 0.0, 0.0, 1.0]);
    }
    index.add_training_vectors(&vectors, 10).unwrap();
    index.train().unwrap();

    // Insert and delete
    let vector = vec![5.0, 0.0, 0.0, 1.0];
    index.insert(0, &vector).unwrap();
    assert!(index.delete(0, &vector));
    assert!(!index.delete(0, &vector)); // Already deleted

    let stats = index.stats();
    assert_eq!(stats.total_vectors, 0);
  }

  #[test]
  fn test_ivf_stats() {
    let mut index = create_test_index(4, 2);

    // Train
    let mut vectors = Vec::new();
    for i in 0..10 {
      vectors.extend_from_slice(&[i as f32, 0.0, 0.0, 1.0]);
    }
    index.add_training_vectors(&vectors, 10).unwrap();
    index.train().unwrap();

    let stats = index.stats();
    assert!(stats.trained);
    assert_eq!(stats.n_clusters, 2);
    assert_eq!(stats.total_vectors, 0);
  }

  #[test]
  fn test_ivf_clear() {
    let mut index = create_test_index(4, 2);

    // Train
    let mut vectors = Vec::new();
    for i in 0..10 {
      vectors.extend_from_slice(&[i as f32, 0.0, 0.0, 1.0]);
    }
    index.add_training_vectors(&vectors, 10).unwrap();
    index.train().unwrap();

    index.clear();

    assert!(!index.trained);
    assert!(index.centroids.is_empty());
    assert!(index.inverted_lists.is_empty());
  }

  #[test]
  fn test_max_heap() {
    let mut heap = MaxHeap::new();

    heap.push(1, 0.5);
    heap.push(2, 0.3);
    heap.push(3, 0.8);
    heap.push(4, 0.1);

    assert_eq!(heap.len(), 4);

    // Max should be 3 (distance 0.8)
    let (id, dist) = *heap.peek().unwrap();
    assert_eq!(id, 3);
    assert_eq!(dist, 0.8);

    let sorted = heap.into_sorted_vec();
    assert_eq!(sorted.len(), 4);
    // Should be sorted by distance ascending
    assert!(sorted[0].1 <= sorted[1].1);
    assert!(sorted[1].1 <= sorted[2].1);
    assert!(sorted[2].1 <= sorted[3].1);
  }

  #[test]
  fn test_error_display() {
    assert!(IvfError::AlreadyTrained.to_string().contains("already"));
    assert!(IvfError::NotTrained.to_string().contains("not trained"));
    assert!(IvfError::NoTrainingVectors.to_string().contains("training"));
  }

  // ========================================================================
  // Multi-Query Search Tests
  // ========================================================================

  #[test]
  fn test_search_multi_empty_queries() {
    let mut index = create_test_index(4, 2);

    // Train
    let mut vectors = Vec::new();
    for i in 0..10 {
      vectors.extend_from_slice(&[i as f32, 0.0, 0.0, 1.0]);
    }
    index.add_training_vectors(&vectors, 10).unwrap();
    index.train().unwrap();

    // Create a minimal manifest
    let manifest = VectorManifest::new(VectorStoreConfig::new(4));

    // Empty queries should return empty results
    let results = index.search_multi(&manifest, &[], 5, MultiQueryAggregation::Min, None);
    assert!(results.is_empty());
  }

  #[test]
  fn test_search_multi_not_trained() {
    let index = create_test_index(4, 2);
    let manifest = VectorManifest::new(VectorStoreConfig::new(4));

    let query = vec![1.0, 0.0, 0.0, 0.0];
    let results = index.search_multi(&manifest, &[&query], 5, MultiQueryAggregation::Min, None);
    assert!(results.is_empty());
  }

  #[test]
  fn test_multi_query_aggregation_min() {
    let agg = MultiQueryAggregation::Min;
    assert_eq!(agg.aggregate(&[1.0, 2.0, 3.0]), 1.0);
    assert_eq!(agg.aggregate(&[5.0, 2.0, 8.0]), 2.0);
    assert_eq!(agg.aggregate(&[3.0]), 3.0);
  }

  #[test]
  fn test_multi_query_aggregation_max() {
    let agg = MultiQueryAggregation::Max;
    assert_eq!(agg.aggregate(&[1.0, 2.0, 3.0]), 3.0);
    assert_eq!(agg.aggregate(&[5.0, 2.0, 8.0]), 8.0);
    assert_eq!(agg.aggregate(&[3.0]), 3.0);
  }

  #[test]
  fn test_multi_query_aggregation_avg() {
    let agg = MultiQueryAggregation::Avg;
    assert_eq!(agg.aggregate(&[1.0, 2.0, 3.0]), 2.0);
    assert_eq!(agg.aggregate(&[4.0, 6.0]), 5.0);
    assert_eq!(agg.aggregate(&[3.0]), 3.0);
  }

  #[test]
  fn test_multi_query_aggregation_sum() {
    let agg = MultiQueryAggregation::Sum;
    assert_eq!(agg.aggregate(&[1.0, 2.0, 3.0]), 6.0);
    assert_eq!(agg.aggregate(&[4.0, 6.0]), 10.0);
    assert_eq!(agg.aggregate(&[3.0]), 3.0);
  }

  #[test]
  fn test_multi_query_aggregation_empty() {
    // Empty distances should return infinity (handled by the aggregate function)
    // Note: The aggregate function returns f32::INFINITY for empty slices in all cases
    // This is a safe default as it ensures empty results are sorted to the end
    assert_eq!(MultiQueryAggregation::Min.aggregate(&[]), f32::INFINITY);
    assert_eq!(MultiQueryAggregation::Max.aggregate(&[]), f32::INFINITY);
    assert_eq!(MultiQueryAggregation::Avg.aggregate(&[]), f32::INFINITY);
    assert_eq!(MultiQueryAggregation::Sum.aggregate(&[]), f32::INFINITY);
  }
}
