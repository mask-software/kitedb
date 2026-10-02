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

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use crate::types::NodeId;
use crate::vector::distance::{normalize, normalize_in_place, with_metric_distance};
use crate::vector::store::{validate_manifest_layout, FragmentLookup};
use crate::vector::top_k::TopK;
use crate::vector::types::{
  DistanceMetric, IvfConfig, MultiQueryAggregation, VectorManifest, VectorSearchResult,
};

use super::kmeans::{
  kmeans_parallel, nearest_centroid, training_sample, KMeansConfig, MAX_TRAINING_POINTS_PER_CLUSTER,
};

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
  ///
  /// The configuration is checked when the index trains: zero clusters is
  /// rejected there, and an `n_probe` of zero searches one cluster.
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
  ///
  /// Trains on at most 256 vectors per cluster, sampled from the buffer.
  ///
  /// # Errors
  /// Fails if the configuration has zero clusters or zero dimensions, or if
  /// fewer vectors than clusters were added. A failed train keeps the
  /// buffered vectors, so adding more and retrying works.
  pub fn train(&mut self) -> Result<(), IvfError> {
    if self.trained {
      return Ok(());
    }

    // Validate before taking the buffer, so a rejected train keeps it.
    if self.training_vectors.is_none() {
      return Err(IvfError::NoTrainingVectors);
    }
    self.check_trainable(self.training_count)?;
    let Some(mut vectors) = self.training_vectors.take() else {
      return Err(IvfError::NoTrainingVectors);
    };

    // Cluster directions, not magnitudes: insert/search/delete compare
    // normalized vectors against the centroids. Normalizing the buffer in
    // place keeps it valid for a retry.
    if self.config.metric == DistanceMetric::Cosine {
      for vector in vectors.chunks_exact_mut(self.dimensions) {
        normalize_in_place(vector);
      }
    }

    match self.train_on(&vectors, self.training_count, false) {
      Ok(()) => {
        self.training_count = 0;
        Ok(())
      }
      Err(err) => {
        self.training_vectors = Some(vectors);
        Err(err)
      }
    }
  }

  /// Train on `n` vectors the caller keeps, without copying them into the
  /// training buffer. `unit_vectors` says they are already unit length (a
  /// normalizing store), which saves a normalized copy for cosine.
  pub(crate) fn train_from(
    &mut self,
    vectors: &[f32],
    n: usize,
    unit_vectors: bool,
  ) -> Result<(), IvfError> {
    if self.trained {
      return Err(IvfError::AlreadyTrained);
    }
    self.check_trainable(n)?;
    let expected_len = n * self.dimensions;
    if vectors.len() < expected_len {
      return Err(IvfError::DimensionMismatch {
        expected: expected_len,
        got: vectors.len(),
      });
    }
    let normalize = self.config.metric == DistanceMetric::Cosine && !unit_vectors;
    self.train_on(&vectors[..expected_len], n, normalize)
  }

  fn check_trainable(&self, n: usize) -> Result<(), IvfError> {
    if self.dimensions == 0 {
      return Err(IvfError::TrainingFailed(
        "dimensions must be nonzero".into(),
      ));
    }
    if self.config.n_clusters == 0 {
      return Err(IvfError::TrainingFailed(
        "n_clusters must be nonzero".into(),
      ));
    }
    if n < self.config.n_clusters {
      return Err(IvfError::NotEnoughTrainingVectors {
        n,
        k: self.config.n_clusters,
      });
    }
    Ok(())
  }

  /// k-means over a sample of `vectors` (normalized first when `normalize`
  /// is set), then unit centroids for cosine.
  fn train_on(&mut self, vectors: &[f32], n: usize, normalize: bool) -> Result<(), IvfError> {
    let dimensions = self.dimensions;
    let n_clusters = self.config.n_clusters;
    let (mut sample, sample_n) = training_sample(
      vectors,
      n,
      dimensions,
      n_clusters.saturating_mul(MAX_TRAINING_POINTS_PER_CLUSTER),
    );
    if normalize {
      for vector in sample.to_mut().chunks_exact_mut(dimensions) {
        normalize_in_place(vector);
      }
    }

    let kmeans_config = KMeansConfig::new(n_clusters)
      .with_max_iterations(25)
      .with_tolerance(1e-4);
    let result = with_metric_distance!(self.config.metric, |dist| kmeans_parallel(
      &sample,
      sample_n,
      dimensions,
      &kmeans_config,
      dist
    ))
    .map_err(|e| IvfError::TrainingFailed(e.to_string()))?;

    let mut centroids = result.centroids;
    if self.config.metric == DistanceMetric::Cosine {
      // k-means moves centroids to arithmetic means, which are shorter than
      // unit length (more so for spread-out clusters). `1 - dot` against them
      // would favor tight clusters, so use their directions.
      for centroid in centroids.chunks_exact_mut(dimensions) {
        normalize_in_place(centroid);
      }
    }

    self.centroids = centroids;
    self.inverted_lists = (0..n_clusters).map(|c| (c, Vec::new())).collect();
    self.trained = true;
    self.training_vectors = None;
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
  /// Vectors without a node mapping are skipped. An `n_probe` of zero (in
  /// the options or the config) searches one cluster.
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
    let fragments = FragmentLookup::new(manifest);

    let hits = self.search_prepared(manifest, &fragments, &query_vec, k, &options, true);
    Ok(
      hits
        .into_iter()
        .map(|(candidate, distance)| self.to_result(candidate, distance))
        .collect(),
    )
  }

  /// Top-k candidates for a prepared query, closest first. Multi-query search
  /// disables `apply_threshold` and thresholds the aggregated distance instead.
  fn search_prepared(
    &self,
    manifest: &VectorManifest,
    fragments: &FragmentLookup<'_>,
    query_vec: &[f32],
    k: usize,
    options: &SearchOptions,
    apply_threshold: bool,
  ) -> Vec<(Candidate, f32)> {
    let n_probe = options.n_probe.unwrap_or(self.config.n_probe).max(1);
    let probe_clusters = self.find_nearest_centroids(query_vec, n_probe);

    let params = SearchClusterParams {
      manifest,
      fragments,
      query_vec,
      options,
      apply_threshold,
    };
    let mut top = TopK::new(k);
    with_metric_distance!(
      self.config.metric,
      stored_normalized = manifest.config.normalize_on_insert,
      |dist| {
        for &cluster in &probe_clusters {
          self.search_cluster(cluster, &params, dist, &mut top);
        }
      }
    );
    top.into_sorted_vec()
  }

  fn search_cluster<D>(
    &self,
    cluster: usize,
    params: &SearchClusterParams<'_>,
    distance_fn: D,
    top: &mut TopK<Candidate>,
  ) where
    D: Fn(&[f32], &[f32]) -> f32,
  {
    let Some(vector_ids) = self.inverted_lists.get(&cluster) else {
      return;
    };
    let manifest = params.manifest;

    for &vector_id in vector_ids {
      // A vector without a node mapping is not a result. Check it before the
      // filter, so such a vector can never bypass it.
      let Some(&node_id) = manifest.vector_to_node.get(&vector_id) else {
        continue;
      };
      if let Some(filter) = &params.options.filter {
        if !filter(node_id) {
          continue;
        }
      }
      let Some(vector) = params.fragments.vector_by_id(manifest, vector_id) else {
        continue;
      };

      let dist = distance_fn(params.query_vec, vector);
      if params.apply_threshold && !passes_threshold(self.config.metric, params.options, dist) {
        continue;
      }
      top.push(Candidate { vector_id, node_id }, dist);
    }
  }

  fn to_result(&self, candidate: Candidate, distance: f32) -> VectorSearchResult {
    VectorSearchResult {
      vector_id: candidate.vector_id,
      node_id: candidate.node_id,
      distance,
      similarity: self.config.metric.distance_to_similarity(distance),
    }
  }

  fn prepare_query<'a>(&self, query: &'a [f32]) -> Cow<'a, [f32]> {
    if self.config.metric == DistanceMetric::Cosine {
      Cow::Owned(normalize(query))
    } else {
      Cow::Borrowed(query)
    }
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
    let prepared: Vec<Cow<'_, [f32]>> = queries
      .iter()
      .map(|query| self.prepare_query(query))
      .collect();
    let fragments = FragmentLookup::new(manifest);

    // Candidates: union of each query's over-fetched top-k (filter and n_probe
    // apply here; the threshold applies to the aggregated distance).
    let expanded_k = k.saturating_mul(2);
    let mut seen = HashSet::new();
    let mut candidates = Vec::new();
    for query_vec in &prepared {
      let hits = self.search_prepared(manifest, &fragments, query_vec, expanded_k, &options, false);
      for (candidate, _) in hits {
        if seen.insert(candidate.vector_id) {
          candidates.push(candidate);
        }
      }
    }

    // Score every candidate against every query. A candidate found by only
    // one query must not be aggregated over that query's distance alone.
    let mut top = TopK::new(k);
    let mut distances = Vec::with_capacity(prepared.len());
    with_metric_distance!(
      self.config.metric,
      stored_normalized = manifest.config.normalize_on_insert,
      |dist| {
        for candidate in candidates {
          let Some(vector) = fragments.vector_by_id(manifest, candidate.vector_id) else {
            continue;
          };
          distances.clear();
          distances.extend(prepared.iter().map(|query_vec| dist(query_vec, vector)));
          let distance = aggregation.aggregate(&distances);
          if !passes_threshold(self.config.metric, &options, distance) {
            continue;
          }
          top.push(candidate, distance);
        }
      }
    );

    Ok(
      top
        .into_sorted_vec()
        .into_iter()
        .map(|(candidate, distance)| self.to_result(candidate, distance))
        .collect(),
    )
  }

  /// Build index from all vectors in the store
  ///
  /// Trains on the store's live vectors (plus any vectors already added for
  /// training), then indexes every live vector.
  pub fn build_from_store(&mut self, manifest: &VectorManifest) -> Result<(), IvfError> {
    self.check_manifest(manifest)?;
    if self.trained {
      return Err(IvfError::AlreadyTrained);
    }
    let fragments = FragmentLookup::new(manifest);
    let live: Vec<(u64, &[f32])> = manifest
      .vector_locations
      .iter()
      .filter_map(|(&vector_id, location)| {
        fragments
          .vector(&manifest.config, location)
          .map(|vector| (vector_id, vector))
      })
      .collect();

    self
      .training_vectors
      .get_or_insert_with(Vec::new)
      .reserve(live.len() * self.dimensions);
    for (_, vector) in &live {
      self.add_training_vectors(vector, 1)?;
    }
    self.train()?;

    for (vector_id, vector) in live {
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
    let query = self.prepare_query(vector);
    with_metric_distance!(self.config.metric, |dist| nearest_centroid(
      &query,
      &self.centroids,
      self.dimensions,
      &dist
    )
    .0)
  }

  /// Find the `n` nearest centroids, closest first.
  fn find_nearest_centroids(&self, query: &[f32], n: usize) -> Vec<usize> {
    let mut centroid_dists: Vec<(usize, f32)> = with_metric_distance!(self.config.metric, |dist| {
      self
        .centroids
        .chunks_exact(self.dimensions)
        .map(|centroid| dist(query, centroid))
        .enumerate()
        .collect()
    });
    nearest_first(&mut centroid_dists, n)
  }
}

/// The `n` entries with the smallest distances, closest first (NaN last).
pub(crate) fn nearest_first(entries: &mut [(usize, f32)], n: usize) -> Vec<usize> {
  let by_distance = |a: &(usize, f32), b: &(usize, f32)| a.1.total_cmp(&b.1);
  let n = n.min(entries.len());
  if n == 0 {
    return Vec::new();
  }
  if n < entries.len() {
    entries.select_nth_unstable_by(n - 1, by_distance);
  }
  entries[..n].sort_unstable_by(by_distance);
  entries[..n].iter().map(|&(index, _)| index).collect()
}

fn passes_threshold(metric: DistanceMetric, options: &SearchOptions, dist: f32) -> bool {
  match options.threshold {
    Some(threshold) => metric.distance_to_similarity(dist) >= threshold,
    None => true,
  }
}

/// A search hit before it becomes a `VectorSearchResult`.
#[derive(Debug, Clone, Copy)]
struct Candidate {
  vector_id: u64,
  node_id: NodeId,
}

struct SearchClusterParams<'a> {
  manifest: &'a VectorManifest,
  fragments: &'a FragmentLookup<'a>,
  query_vec: &'a [f32],
  options: &'a SearchOptions,
  apply_threshold: bool,
}

// ============================================================================
// Search Options
// ============================================================================

/// Options for IVF search
#[derive(Default)]
pub struct SearchOptions {
  /// Number of clusters to probe (overrides config; zero probes one cluster)
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
