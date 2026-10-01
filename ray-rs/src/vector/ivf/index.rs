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

use std::collections::{HashMap, HashSet};

use crate::types::NodeId;
use crate::vector::distance::{normalize, normalize_in_place};
use crate::vector::store::validate_manifest_layout;
use crate::vector::types::{
  DistanceMetric, Fragment, IvfConfig, MultiQueryAggregation, VectorLocation, VectorManifest,
  VectorSearchResult,
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

    let mut training_vectors = self
      .training_vectors
      .take()
      .ok_or(IvfError::NoTrainingVectors)?;

    if self.training_count < self.config.n_clusters {
      return Err(IvfError::NotEnoughTrainingVectors {
        n: self.training_count,
        k: self.config.n_clusters,
      });
    }

    // Cluster directions, not magnitudes: insert/search/delete compare
    // normalized vectors against these centroids.
    if self.config.metric == DistanceMetric::Cosine && self.dimensions > 0 {
      for vector in training_vectors.chunks_exact_mut(self.dimensions) {
        normalize_in_place(vector);
      }
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
    self.check_dimensions(vector)?;

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
  ///
  /// # Errors
  /// Returns an error if the vector length does not match the index dimensions.
  pub fn delete(&mut self, vector_id: u64, vector: &[f32]) -> Result<bool, IvfError> {
    self.check_dimensions(vector)?;
    if !self.trained {
      return Ok(false);
    }

    // Find which cluster it's in
    let cluster = self.find_nearest_centroid(vector);

    if let Some(list) = self.inverted_lists.get_mut(&cluster) {
      if let Some(idx) = list.iter().position(|&id| id == vector_id) {
        // Remove from list (swap with last for O(1))
        list.swap_remove(idx);
        return Ok(true);
      }
    }

    Ok(false)
  }

  /// Search for k nearest neighbors
  ///
  /// # Errors
  /// Returns an error if the query or the manifest does not match the index
  /// dimensions, or if the manifest's row groups are malformed.
  pub fn search(
    &self,
    manifest: &VectorManifest,
    query: &[f32],
    k: usize,
    options: Option<SearchOptions>,
  ) -> Result<Vec<VectorSearchResult>, IvfError> {
    self.check_dimensions(query)?;
    self.check_manifest(manifest)?;
    if !self.trained {
      return Ok(Vec::new());
    }

    let options = options.unwrap_or_default();
    let query_vec = self.prepare_query(query);

    // Build fragment lookup map for O(1) access (avoid .find() in hot loop)
    let fragment_map = build_fragment_map(manifest);

    let hits = self.search_prepared(manifest, &fragment_map, &query_vec, k, &options, true);
    Ok(
      hits
        .into_iter()
        .map(|(vector_id, distance)| self.to_result(manifest, vector_id, distance))
        .collect(),
    )
  }

  /// Top-k `(vector_id, distance)` for a prepared query. Multi-query search
  /// disables `apply_threshold` and thresholds the aggregated distance instead.
  fn search_prepared(
    &self,
    manifest: &VectorManifest,
    fragment_map: &HashMap<usize, &Fragment>,
    query_vec: &[f32],
    k: usize,
    options: &SearchOptions,
    apply_threshold: bool,
  ) -> Vec<(u64, f32)> {
    let n_probe = options.n_probe.unwrap_or(self.config.n_probe);

    // Find top n_probe nearest centroids
    let probe_clusters = self.find_nearest_centroids(query_vec, n_probe);

    // Use max-heap to track top-k candidates
    let mut heap = MaxHeap::new();

    let params = SearchClusterParams {
      manifest,
      query_vec,
      options,
      fragment_map,
      distance_fn: self.stored_distance_fn(manifest),
      k,
      apply_threshold,
    };

    // Search within selected clusters
    for cluster in probe_clusters {
      self.search_cluster(cluster, &params, &mut heap);
    }

    heap.into_sorted_vec()
  }

  fn to_result(
    &self,
    manifest: &VectorManifest,
    vector_id: u64,
    distance: f32,
  ) -> VectorSearchResult {
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
  }

  fn prepare_query(&self, query: &[f32]) -> Vec<f32> {
    if self.config.metric == DistanceMetric::Cosine {
      normalize(query)
    } else {
      query.to_vec()
    }
  }

  fn stored_distance_fn(&self, manifest: &VectorManifest) -> fn(&[f32], &[f32]) -> f32 {
    self
      .config
      .metric
      .stored_distance_fn(manifest.config.normalize_on_insert)
  }

  fn check_dimensions(&self, vector: &[f32]) -> Result<(), IvfError> {
    if vector.len() != self.dimensions {
      return Err(IvfError::DimensionMismatch {
        expected: self.dimensions,
        got: vector.len(),
      });
    }
    Ok(())
  }

  fn check_manifest(&self, manifest: &VectorManifest) -> Result<(), IvfError> {
    validate_manifest_layout(manifest, self.dimensions)
      .map_err(|e| IvfError::InvalidManifest(e.to_string()))
  }

  fn search_cluster(&self, cluster: usize, params: &SearchClusterParams<'_>, heap: &mut MaxHeap) {
    let vector_ids = match self.inverted_lists.get(&cluster) {
      Some(list) if !list.is_empty() => list,
      _ => return,
    };

    for &vector_id in vector_ids {
      let vec = match lookup_vector(params.manifest, params.fragment_map, vector_id) {
        Some(vec) => vec,
        None => continue,
      };

      if !passes_filter(params.options, params.manifest, vector_id) {
        continue;
      }

      let dist = (params.distance_fn)(params.query_vec, vec);

      if params.apply_threshold && !passes_threshold(self.config.metric, params.options, dist) {
        continue;
      }

      update_heap(heap, vector_id, dist, params.k);
    }
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
  ) -> Result<Vec<VectorSearchResult>, IvfError> {
    for query in queries {
      self.check_dimensions(query)?;
    }
    self.check_manifest(manifest)?;
    if !self.trained || queries.is_empty() {
      return Ok(Vec::new());
    }

    let options = options.unwrap_or_default();
    let prepared: Vec<Vec<f32>> = queries
      .iter()
      .map(|query| self.prepare_query(query))
      .collect();
    let fragment_map = build_fragment_map(manifest);

    // Candidates: union of each query's over-fetched top-k (filter and n_probe
    // apply here; the threshold applies to the aggregated distance).
    let expanded_k = k.saturating_mul(2);
    let mut seen = HashSet::new();
    let mut candidates = Vec::new();
    for query_vec in &prepared {
      let hits = self.search_prepared(
        manifest,
        &fragment_map,
        query_vec,
        expanded_k,
        &options,
        false,
      );
      for (vector_id, _) in hits {
        if seen.insert(vector_id) {
          candidates.push(vector_id);
        }
      }
    }

    // Score every candidate against every query. A candidate found by only
    // one query must not be aggregated over that query's distance alone.
    let distance_fn = self.stored_distance_fn(manifest);
    let mut distances = Vec::with_capacity(prepared.len());
    let mut scored = Vec::with_capacity(candidates.len());
    for vector_id in candidates {
      let vector = match lookup_vector(manifest, &fragment_map, vector_id) {
        Some(vector) => vector,
        None => continue,
      };
      distances.clear();
      distances.extend(
        prepared
          .iter()
          .map(|query_vec| distance_fn(query_vec, vector)),
      );
      let result = self.to_result(manifest, vector_id, aggregation.aggregate(&distances));
      if options
        .threshold
        .is_some_and(|threshold| result.similarity < threshold)
      {
        continue;
      }
      scored.push(result);
    }

    // Sort by distance and return top k
    scored.sort_by(|a, b| {
      a.distance
        .partial_cmp(&b.distance)
        .unwrap_or(std::cmp::Ordering::Equal)
    });
    scored.truncate(k);

    Ok(scored)
  }

  /// Build index from all vectors in the store
  pub fn build_from_store(&mut self, manifest: &VectorManifest) -> Result<(), IvfError> {
    self.check_manifest(manifest)?;

    // Collect training vectors
    for fragment in &manifest.fragments {
      for row_group in &fragment.row_groups {
        self.add_training_vectors(&row_group.data, row_group.count)?;
      }
    }

    // Train the index
    self.train()?;

    // Build fragment lookup map for O(1) access
    let fragment_map = build_fragment_map(manifest);

    // Insert all vectors
    for &vector_id in manifest.node_to_vector.values() {
      if let Some(vector) = lookup_vector(manifest, &fragment_map, vector_id) {
        self.insert(vector_id, vector)?;
      }
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

fn build_fragment_map(manifest: &VectorManifest) -> HashMap<usize, &Fragment> {
  manifest.fragments.iter().map(|f| (f.id, f)).collect()
}

fn passes_filter(options: &SearchOptions, manifest: &VectorManifest, vector_id: u64) -> bool {
  if let Some(ref filter) = options.filter {
    if let Some(&node_id) = manifest.vector_to_node.get(&vector_id) {
      return filter(node_id);
    }
  }
  true
}

fn passes_threshold(metric: DistanceMetric, options: &SearchOptions, dist: f32) -> bool {
  if let Some(threshold) = options.threshold {
    let similarity = metric.distance_to_similarity(dist);
    if similarity < threshold {
      return false;
    }
  }
  true
}

/// Live vector data for `vector_id`, or None if unmapped or deleted.
///
/// The manifest must have passed `validate_manifest_layout`.
fn lookup_vector<'a>(
  manifest: &'a VectorManifest,
  fragment_map: &HashMap<usize, &'a Fragment>,
  vector_id: u64,
) -> Option<&'a [f32]> {
  let location = manifest.vector_locations.get(&vector_id)?;
  let fragment = *fragment_map.get(&location.fragment_id)?;
  if fragment.is_deleted(location.local_index) {
    return None;
  }
  vector_slice(manifest, fragment, location)
}

fn vector_slice<'a>(
  manifest: &'a VectorManifest,
  fragment: &'a Fragment,
  location: &VectorLocation,
) -> Option<&'a [f32]> {
  let row_group_idx = location.local_index / manifest.config.row_group_size;
  let local_row_idx = location.local_index % manifest.config.row_group_size;
  let row_group = match fragment.row_groups.get(row_group_idx) {
    Some(rg) if local_row_idx < rg.count => rg,
    _ => return None,
  };

  let offset = local_row_idx * manifest.config.dimensions;
  Some(&row_group.data[offset..offset + manifest.config.dimensions])
}

fn update_heap(heap: &mut MaxHeap, vector_id: u64, dist: f32, k: usize) {
  if heap.len() < k {
    heap.push(vector_id, dist);
  } else if let Some(&(_, max_dist)) = heap.peek() {
    if dist < max_dist {
      heap.pop();
      heap.push(vector_id, dist);
    }
  }
}

struct SearchClusterParams<'a> {
  manifest: &'a VectorManifest,
  query_vec: &'a [f32],
  options: &'a SearchOptions,
  fragment_map: &'a HashMap<usize, &'a Fragment>,
  distance_fn: fn(&[f32], &[f32]) -> f32,
  k: usize,
  apply_threshold: bool,
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
  InvalidManifest(String),
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
      IvfError::InvalidManifest(msg) => write!(f, "Invalid vector manifest: {msg}"),
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
    index
      .add_training_vectors(&vectors, 2)
      .expect("expected value");

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
    index
      .add_training_vectors(&vectors, 10)
      .expect("expected value");

    index.train().expect("expected value");

    assert!(index.trained);
    assert_eq!(index.centroids.len(), 2 * 4);
  }

  #[test]
  fn test_ivf_train_not_enough_vectors() {
    let mut index = create_test_index(4, 10);

    let vectors = vec![1.0, 0.0, 0.0, 0.0];
    index
      .add_training_vectors(&vectors, 1)
      .expect("expected value");

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
    index
      .add_training_vectors(&vectors, 10)
      .expect("expected value");
    index.train().expect("expected value");

    // Insert
    let vector = vec![5.0, 0.0, 0.0, 1.0];
    index.insert(0, &vector).expect("expected value");

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
    index
      .add_training_vectors(&vectors, 10)
      .expect("expected value");
    index.train().expect("expected value");

    // Insert and delete
    let vector = vec![5.0, 0.0, 0.0, 1.0];
    index.insert(0, &vector).expect("expected value");
    assert!(index.delete(0, &vector).expect("delete"));
    assert!(!index.delete(0, &vector).expect("delete")); // Already deleted

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
    index
      .add_training_vectors(&vectors, 10)
      .expect("expected value");
    index.train().expect("expected value");

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
    index
      .add_training_vectors(&vectors, 10)
      .expect("expected value");
    index.train().expect("expected value");

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
    let (id, dist) = *heap.peek().expect("expected value");
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

  #[test]
  fn test_ivf_build_from_store_rejects_mismatched_manifest_before_training() {
    let mut manifest = VectorManifest::new(VectorStoreConfig::new(8).with_normalize(false));
    for node_id in 1..=4u64 {
      let vector: Vec<f32> = (0..8).map(|d| (node_id * 8 + d) as f32).collect();
      crate::vector::store::vector_store_insert(&mut manifest, node_id, &vector)
        .expect("store insert");
    }
    let mut index = create_test_index(4, 1);

    assert!(matches!(
      index.build_from_store(&manifest),
      Err(IvfError::InvalidManifest(_))
    ));
    assert!(!index.trained);
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
    index
      .add_training_vectors(&vectors, 10)
      .expect("expected value");
    index.train().expect("expected value");

    // Create a minimal manifest
    let manifest = VectorManifest::new(VectorStoreConfig::new(4));

    // Empty queries should return empty results
    let results = index
      .search_multi(&manifest, &[], 5, MultiQueryAggregation::Min, None)
      .expect("search_multi");
    assert!(results.is_empty());
  }

  #[test]
  fn test_search_multi_not_trained() {
    let index = create_test_index(4, 2);
    let manifest = VectorManifest::new(VectorStoreConfig::new(4));

    let query = vec![1.0, 0.0, 0.0, 0.0];
    let results = index
      .search_multi(&manifest, &[&query], 5, MultiQueryAggregation::Min, None)
      .expect("search_multi");
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
