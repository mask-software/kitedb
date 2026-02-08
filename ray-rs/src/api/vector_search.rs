//! Vector similarity search API
//!
//! Provides high-level API for vector similarity search integrated with the Kite API.
//!
//! Ported from src/api/vector-search.ts

use std::collections::HashMap;
use std::sync::Arc;

use crate::cache::lru::LruCache;
use crate::types::NodeId;
use crate::vector::{
  create_vector_store, vector_store_clear, vector_store_delete, vector_store_insert,
  vector_store_node_vector, vector_store_stats, DistanceMetric, IvfConfig, IvfError, IvfIndex,
  IvfPqConfig, IvfPqError, IvfPqIndex, IvfPqSearchOptions, SearchOptions, VectorManifest,
  VectorSearchResult, VectorStoreConfig,
};

// ============================================================================
// Constants
// ============================================================================

const DEFAULT_CACHE_MAX_SIZE: usize = 10_000;
const DEFAULT_TRAINING_THRESHOLD: usize = 1000;
const MIN_CLUSTERS: usize = 16;
const MAX_CLUSTERS: usize = 1024;
const DEFAULT_PQ_SUBSPACES: usize = 48;
const DEFAULT_PQ_CENTROIDS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnnAlgorithm {
  Ivf,
  #[default]
  IvfPq,
}

// ============================================================================
// Types
// ============================================================================

/// Configuration for creating a vector index
#[derive(Debug, Clone)]
pub struct VectorIndexOptions {
  /// Vector dimensions (required)
  pub dimensions: usize,
  /// Distance metric (default: Cosine)
  pub metric: DistanceMetric,
  /// Vectors per row group (default: 1024)
  pub row_group_size: usize,
  /// Vectors per fragment before sealing (default: 100_000)
  pub fragment_target_size: usize,
  /// Whether to auto-normalize vectors (default: true for cosine)
  pub normalize: bool,
  /// Number of IVF clusters (default: auto-computed)
  pub n_clusters: Option<usize>,
  /// Number of probes for IVF search (default: 10)
  pub n_probe: usize,
  /// Minimum training vectors before index training (default: 1000)
  pub training_threshold: usize,
  /// Maximum node refs to cache for search results (default: 10_000)
  pub cache_max_size: usize,
  /// ANN backend algorithm (default: IVF-PQ)
  pub ann_algorithm: AnnAlgorithm,
  /// PQ subspaces for IVF-PQ (default: 48)
  pub pq_subspaces: usize,
  /// PQ centroids per subspace for IVF-PQ (default: 256)
  pub pq_centroids: usize,
  /// Use residual encoding for IVF-PQ (default: false)
  pub pq_residuals: bool,
}

impl Default for VectorIndexOptions {
  fn default() -> Self {
    Self {
      dimensions: 0, // Must be set
      metric: DistanceMetric::Cosine,
      row_group_size: 1024,
      fragment_target_size: 100_000,
      normalize: true,
      n_clusters: None,
      n_probe: 10,
      training_threshold: DEFAULT_TRAINING_THRESHOLD,
      cache_max_size: DEFAULT_CACHE_MAX_SIZE,
      ann_algorithm: AnnAlgorithm::default(),
      pq_subspaces: DEFAULT_PQ_SUBSPACES,
      pq_centroids: DEFAULT_PQ_CENTROIDS,
      pq_residuals: false,
    }
  }
}

impl VectorIndexOptions {
  /// Create options with required dimensions
  pub fn new(dimensions: usize) -> Self {
    let metric = DistanceMetric::Cosine;
    Self {
      dimensions,
      normalize: metric == DistanceMetric::Cosine,
      ..Default::default()
    }
  }

  /// Set the distance metric
  pub fn with_metric(mut self, metric: DistanceMetric) -> Self {
    self.metric = metric;
    // Auto-adjust normalize for cosine
    if metric == DistanceMetric::Cosine {
      self.normalize = true;
    }
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

  /// Set whether to normalize vectors
  pub fn with_normalize(mut self, normalize: bool) -> Self {
    self.normalize = normalize;
    self
  }

  /// Set the number of IVF clusters
  pub fn with_n_clusters(mut self, n_clusters: usize) -> Self {
    self.n_clusters = Some(n_clusters);
    self
  }

  /// Set the number of probes for IVF search
  pub fn with_n_probe(mut self, n_probe: usize) -> Self {
    self.n_probe = n_probe;
    self
  }

  /// Set the training threshold
  pub fn with_training_threshold(mut self, threshold: usize) -> Self {
    self.training_threshold = threshold;
    self
  }

  /// Set the cache max size
  pub fn with_cache_max_size(mut self, size: usize) -> Self {
    self.cache_max_size = size;
    self
  }

  /// Set ANN backend algorithm.
  pub fn with_ann_algorithm(mut self, algorithm: AnnAlgorithm) -> Self {
    self.ann_algorithm = algorithm;
    self
  }

  /// Set PQ subspaces for IVF-PQ backend.
  pub fn with_pq_subspaces(mut self, subspaces: usize) -> Self {
    self.pq_subspaces = subspaces.max(1);
    self
  }

  /// Set PQ centroids per subspace for IVF-PQ backend.
  pub fn with_pq_centroids(mut self, centroids: usize) -> Self {
    self.pq_centroids = centroids.max(2);
    self
  }

  /// Set residual encoding mode for IVF-PQ backend.
  pub fn with_pq_residuals(mut self, residuals: bool) -> Self {
    self.pq_residuals = residuals;
    self
  }
}

/// Options for similarity search
/// Options for similarity search
pub struct SimilarOptions {
  /// Number of results to return
  pub k: usize,
  /// Minimum similarity threshold (0-1 for cosine)
  pub threshold: Option<f32>,
  /// Number of clusters to probe for IVF (default: 10)
  pub n_probe: Option<usize>,
  /// Optional filter function to exclude results
  pub filter: Option<std::sync::Arc<dyn Fn(NodeId) -> bool + Send + Sync>>,
}

impl std::fmt::Debug for SimilarOptions {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("SimilarOptions")
      .field("k", &self.k)
      .field("threshold", &self.threshold)
      .field("n_probe", &self.n_probe)
      .field("filter", &self.filter.is_some())
      .finish()
  }
}

impl SimilarOptions {
  /// Create search options with k results
  pub fn new(k: usize) -> Self {
    Self {
      k,
      threshold: None,
      n_probe: None,
      filter: None,
    }
  }

  /// Set the similarity threshold
  pub fn with_threshold(mut self, threshold: f32) -> Self {
    self.threshold = Some(threshold);
    self
  }

  /// Set the number of probes
  pub fn with_n_probe(mut self, n_probe: usize) -> Self {
    self.n_probe = Some(n_probe);
    self
  }

  /// Set a filter function to exclude certain nodes from results
  ///
  /// The filter function receives a node ID and should return `true` to include
  /// the node in results, or `false` to exclude it.
  ///
  /// # Example
  /// ```rust,no_run
  /// # use kitedb::api::vector_search::SimilarOptions;
  /// # fn main() {
  /// // Only include nodes with ID > 100
  /// let options = SimilarOptions::new(10)
  ///     .with_filter(|node_id| node_id > 100);
  /// # }
  /// ```
  pub fn with_filter<F>(mut self, filter: F) -> Self
  where
    F: Fn(NodeId) -> bool + Send + Sync + 'static,
  {
    self.filter = Some(Arc::new(filter));
    self
  }
}

/// A search result hit
#[derive(Debug, Clone)]
pub struct VectorSearchHit {
  /// Node ID
  pub node_id: NodeId,
  /// Distance to query vector (lower is better)
  pub distance: f32,
  /// Similarity score (0-1 for cosine, higher is better)
  pub similarity: f32,
}

/// Statistics about the vector index
#[derive(Debug, Clone)]
pub struct VectorIndexStats {
  /// Total vectors stored (including deleted)
  pub total_vectors: usize,
  /// Live vectors (excluding deleted)
  pub live_vectors: usize,
  /// Vector dimensions
  pub dimensions: usize,
  /// Distance metric
  pub metric: DistanceMetric,
  /// Whether the IVF index is trained
  pub index_trained: bool,
  /// Number of IVF clusters (if trained)
  pub index_clusters: Option<usize>,
}

// ============================================================================
// VectorIndex
// ============================================================================

/// VectorIndex - manages vector embeddings for a set of nodes
///
/// # Example
/// ```rust,no_run
/// # use kitedb::api::vector_search::{SimilarOptions, VectorIndex, VectorIndexOptions};
/// # use kitedb::api::vector_search::VectorIndexError;
/// # use kitedb::types::NodeId;
/// # fn main() -> Result<(), VectorIndexError> {
/// # let node_id: NodeId = 1;
/// # let embedding = vec![0.0_f32; 768];
/// # let query_vector = vec![0.0_f32; 768];
/// // Create a vector index for 768-dimensional embeddings
/// let mut embeddings = VectorIndex::new(VectorIndexOptions::new(768));
///
/// // Add vectors for nodes
/// embeddings.set(node_id, &embedding)?;
///
/// // Find similar nodes
/// let similar = embeddings.search(&query_vector, SimilarOptions::new(10))?;
/// for hit in similar {
///     println!("{}: {}", hit.node_id, hit.similarity);
/// }
/// # Ok(())
/// # }
/// ```
enum BuiltIndex {
  Ivf(IvfIndex),
  IvfPq(IvfPqIndex),
}

impl BuiltIndex {
  fn trained(&self) -> bool {
    match self {
      BuiltIndex::Ivf(index) => index.trained,
      BuiltIndex::IvfPq(index) => index.trained,
    }
  }

  fn n_clusters(&self) -> usize {
    match self {
      BuiltIndex::Ivf(index) => index.config.n_clusters,
      BuiltIndex::IvfPq(index) => index.config.ivf.n_clusters,
    }
  }
}

pub struct VectorIndex {
  /// The underlying vector store manifest
  manifest: VectorManifest,
  /// ANN index for approximate search (None if not trained)
  index: Option<BuiltIndex>,
  /// Cache of node IDs for quick lookup
  node_cache: LruCache<NodeId, ()>,
  /// Node ID to vector ID mapping for cache lookups
  cached_node_ids: HashMap<NodeId, u32>,
  /// Configuration
  options: VectorIndexOptions,
  /// Whether the index needs training
  needs_training: bool,
  /// Whether index build is in progress
  is_building: bool,
}

impl VectorIndex {
  /// Create a new vector index
  pub fn new(options: VectorIndexOptions) -> Self {
    let config = VectorStoreConfig::new(options.dimensions)
      .with_metric(options.metric)
      .with_row_group_size(options.row_group_size)
      .with_fragment_target_size(options.fragment_target_size)
      .with_normalize(options.normalize);

    let manifest = create_vector_store(config);

    Self {
      manifest,
      index: None,
      node_cache: LruCache::new(options.cache_max_size),
      cached_node_ids: HashMap::new(),
      options,
      needs_training: true,
      is_building: false,
    }
  }

  /// Set/update a vector for a node
  ///
  /// # Errors
  /// Returns an error if called while buildIndex() is in progress,
  /// or if the vector dimensions don't match.
  pub fn set(&mut self, node_id: NodeId, vector: &[f32]) -> Result<(), VectorIndexError> {
    if self.is_building {
      return Err(VectorIndexError::BuildInProgress);
    }

    if vector.len() != self.options.dimensions {
      return Err(VectorIndexError::DimensionMismatch {
        expected: self.options.dimensions,
        got: vector.len(),
      });
    }

    // Check if we need to delete from index first
    if let Some(&existing_vector_id) = self.manifest.node_to_vector.get(&node_id) {
      if let Some(ref mut index) = self.index {
        if index.trained() {
          if let Some(existing_vector) = vector_store_node_vector(&self.manifest, node_id) {
            match index {
              BuiltIndex::Ivf(ivf_index) => {
                ivf_index.delete(existing_vector_id, existing_vector);
              }
              BuiltIndex::IvfPq(ivf_pq_index) => {
                ivf_pq_index.delete(existing_vector_id, existing_vector);
              }
            }
          }
        }
      }
    }

    // Insert into store
    let vector_id = vector_store_insert(&mut self.manifest, node_id, vector)
      .map_err(|e| VectorIndexError::StoreError(e.to_string()))?;

    // Cache the node ID
    self.node_cache.set(node_id, ());
    self.cached_node_ids.insert(node_id, vector_id as u32);

    // Add to index if trained, otherwise mark for training
    if let Some(ref mut index) = self.index {
      if index.trained() {
        if let Some(stored_vector) = vector_store_node_vector(&self.manifest, node_id) {
          let insert_result = match index {
            BuiltIndex::Ivf(ivf_index) => ivf_index
              .insert(vector_id as u64, stored_vector)
              .map_err(ivf_error_to_index_error),
            BuiltIndex::IvfPq(ivf_pq_index) => ivf_pq_index
              .insert(vector_id as u64, stored_vector)
              .map_err(ivf_pq_error_to_index_error),
          };
          if let Err(err) = insert_result {
            self.index = None;
            self.needs_training = true;
            return Err(err);
          }
        }
      } else {
        self.needs_training = true;
      }
    } else {
      self.needs_training = true;
    }

    Ok(())
  }

  /// Get the vector for a node (if any)
  pub fn get(&self, node_id: NodeId) -> Option<Vec<f32>> {
    vector_store_node_vector(&self.manifest, node_id).map(|s| s.to_vec())
  }

  /// Delete the vector for a node
  ///
  /// # Errors
  /// Returns an error if called while buildIndex() is in progress.
  pub fn delete(&mut self, node_id: NodeId) -> Result<bool, VectorIndexError> {
    if self.is_building {
      return Err(VectorIndexError::BuildInProgress);
    }

    // Remove from index if trained
    if let Some(ref mut index) = self.index {
      if index.trained() {
        if let Some(&vector_id) = self.manifest.node_to_vector.get(&node_id) {
          if let Some(vector) = vector_store_node_vector(&self.manifest, node_id) {
            match index {
              BuiltIndex::Ivf(ivf_index) => {
                ivf_index.delete(vector_id, vector);
              }
              BuiltIndex::IvfPq(ivf_pq_index) => {
                ivf_pq_index.delete(vector_id, vector);
              }
            }
          }
        }
      }
    }

    // Remove from cache
    self.node_cache.remove(&node_id);
    self.cached_node_ids.remove(&node_id);

    // Remove from store
    let deleted = vector_store_delete(&mut self.manifest, node_id);
    Ok(deleted)
  }

  /// Check if a node has a vector
  pub fn has(&self, node_id: NodeId) -> bool {
    self.manifest.node_to_vector.contains_key(&node_id)
  }

  /// Build/rebuild the configured ANN index for faster search
  ///
  /// Call this after bulk loading vectors, or periodically as vectors are updated.
  /// Uses IVF or IVF-PQ based on configured ANN backend.
  ///
  /// Note: Modifications (set/delete) are blocked while building is in progress.
  pub fn build_index(&mut self) -> Result<(), VectorIndexError> {
    if self.is_building {
      return Err(VectorIndexError::BuildAlreadyInProgress);
    }

    self.is_building = true;
    let result = self.build_index_internal();
    self.is_building = false;
    result
  }

  fn build_index_internal(&mut self) -> Result<(), VectorIndexError> {
    let dimensions = self.options.dimensions;
    let stats = vector_store_stats(&self.manifest);
    let live_vectors = stats.live_vectors;

    if live_vectors < self.options.training_threshold {
      // Not enough vectors for index - will use brute force search
      self.index = None;
      self.needs_training = false;
      return Ok(());
    }

    // Determine number of clusters (sqrt rule, min 16, max 1024)
    let n_clusters = self.options.n_clusters.unwrap_or_else(|| {
      let sqrt_n = (live_vectors as f64).sqrt() as usize;
      sqrt_n.clamp(MIN_CLUSTERS, MAX_CLUSTERS)
    });

    // Collect training vectors
    let mut training_data = Vec::with_capacity(live_vectors * dimensions);
    let mut vector_ids = Vec::with_capacity(live_vectors);

    for (&node_id, &vector_id) in &self.manifest.node_to_vector {
      if let Some(vector) = vector_store_node_vector(&self.manifest, node_id) {
        training_data.extend_from_slice(vector);
        vector_ids.push(vector_id);
      }
    }

    // Create and train the configured ANN index.
    self.index = Some(match self.options.ann_algorithm {
      AnnAlgorithm::Ivf => {
        let ivf_config = IvfConfig::new(n_clusters)
          .with_n_probe(self.options.n_probe)
          .with_metric(self.options.metric);
        let mut index = IvfIndex::new(dimensions, ivf_config);

        index
          .add_training_vectors(&training_data, vector_ids.len())
          .map_err(|e| VectorIndexError::TrainingError(e.to_string()))?;

        index
          .train()
          .map_err(|e| VectorIndexError::TrainingError(e.to_string()))?;

        for (i, &vector_id) in vector_ids.iter().enumerate() {
          let offset = i * dimensions;
          let vector = &training_data[offset..offset + dimensions];
          if let Err(err) = index.insert(vector_id, vector) {
            self.index = None;
            self.needs_training = true;
            return Err(ivf_error_to_index_error(err));
          }
        }
        BuiltIndex::Ivf(index)
      }
      AnnAlgorithm::IvfPq => {
        let pq_subspaces = resolve_pq_subspaces(self.options.pq_subspaces, dimensions);
        let pq_centroids = self.options.pq_centroids.max(2).min(live_vectors.max(2));
        let ivf_pq_config = IvfPqConfig::new()
          .with_n_clusters(n_clusters)
          .with_n_probe(self.options.n_probe)
          .with_metric(self.options.metric)
          .with_num_subspaces(pq_subspaces)
          .with_num_centroids(pq_centroids)
          .with_residuals(self.options.pq_residuals);
        let mut index =
          IvfPqIndex::new(dimensions, ivf_pq_config).map_err(ivf_pq_error_to_index_error)?;

        index
          .add_training_vectors(&training_data, vector_ids.len())
          .map_err(ivf_pq_error_to_index_error)?;
        index.train().map_err(ivf_pq_error_to_index_error)?;

        for (i, &vector_id) in vector_ids.iter().enumerate() {
          let offset = i * dimensions;
          let vector = &training_data[offset..offset + dimensions];
          if let Err(err) = index.insert(vector_id, vector) {
            self.index = None;
            self.needs_training = true;
            return Err(ivf_pq_error_to_index_error(err));
          }
        }
        BuiltIndex::IvfPq(index)
      }
    });
    self.needs_training = false;

    Ok(())
  }

  /// Search for similar vectors
  ///
  /// Returns the k most similar nodes to the query vector.
  /// Uses configured ANN index if available, otherwise falls back to brute force.
  pub fn search(
    &mut self,
    query: &[f32],
    options: SimilarOptions,
  ) -> Result<Vec<VectorSearchHit>, VectorIndexError> {
    let dimensions = self.options.dimensions;

    if query.len() != dimensions {
      return Err(VectorIndexError::DimensionMismatch {
        expected: dimensions,
        got: query.len(),
      });
    }

    // Validate query vector
    if !is_valid_vector(query) {
      return Err(VectorIndexError::InvalidVector);
    }

    // Auto-build index if needed and we have enough vectors
    if self.needs_training {
      self.build_index()?;
    }

    let SimilarOptions {
      k,
      threshold,
      n_probe,
      filter,
    } = options;

    let n_probe = n_probe.unwrap_or(self.options.n_probe);

    let results: Vec<VectorSearchResult> = if let Some(ref index) = self.index {
      if index.trained() {
        match index {
          BuiltIndex::Ivf(ivf_index) => {
            let filter_box = filter.as_ref().map(|f| {
              let f = Arc::clone(f);
              Box::new(move |node_id: NodeId| f(node_id)) as Box<dyn Fn(NodeId) -> bool>
            });
            let search_opts = SearchOptions {
              n_probe: Some(n_probe),
              filter: filter_box,
              threshold,
            };
            ivf_index.search(&self.manifest, query, k, Some(search_opts))
          }
          BuiltIndex::IvfPq(ivf_pq_index) => {
            let filter_box = filter.as_ref().map(|f| {
              let f = Arc::clone(f);
              Box::new(move |node_id: NodeId| f(node_id)) as Box<dyn Fn(NodeId) -> bool>
            });
            let search_opts = IvfPqSearchOptions {
              n_probe: Some(n_probe),
              filter: filter_box,
              threshold,
            };
            ivf_pq_index.search(&self.manifest, query, k, Some(search_opts))
          }
        }
      } else {
        self.brute_force_search_filtered(query, k, threshold, filter.as_ref())
      }
    } else {
      self.brute_force_search_filtered(query, k, threshold, filter.as_ref())
    };

    Ok(
      results
        .into_iter()
        .take(k)
        .map(|r| VectorSearchHit {
          node_id: r.node_id,
          distance: r.distance,
          similarity: r.similarity,
        })
        .collect(),
    )
  }

  /// Brute force search (fallback when index not available)
  fn brute_force_search(&self, query: &[f32], k: usize) -> Vec<VectorSearchResult> {
    use crate::vector::{cosine_distance, dot_product, euclidean_distance, normalize};

    let metric = self.options.metric;

    // Normalize query for cosine similarity
    let query_normalized: Vec<f32>;
    let query_for_search = if metric == DistanceMetric::Cosine {
      query_normalized = normalize(query);
      &query_normalized
    } else {
      query
    };

    let mut candidates: Vec<VectorSearchResult> = Vec::new();

    for (&node_id, &vector_id) in &self.manifest.node_to_vector {
      if let Some(vector) = vector_store_node_vector(&self.manifest, node_id) {
        let distance = match metric {
          DistanceMetric::Cosine => cosine_distance(query_for_search, vector),
          DistanceMetric::Euclidean => euclidean_distance(query_for_search, vector),
          DistanceMetric::DotProduct => -dot_product(query_for_search, vector), // Negate for sorting
        };

        let similarity = metric.distance_to_similarity(distance);

        candidates.push(VectorSearchResult {
          vector_id,
          node_id,
          distance,
          similarity,
        });
      }
    }

    // Sort by distance and return top k
    candidates.sort_by(|a, b| {
      a.distance
        .partial_cmp(&b.distance)
        .unwrap_or(std::cmp::Ordering::Equal)
    });
    candidates.truncate(k);
    candidates
  }

  fn brute_force_search_filtered(
    &self,
    query: &[f32],
    k: usize,
    threshold: Option<f32>,
    filter: Option<&Arc<dyn Fn(NodeId) -> bool + Send + Sync>>,
  ) -> Vec<VectorSearchResult> {
    if threshold.is_none() && filter.is_none() {
      return self.brute_force_search(query, k);
    }

    use crate::vector::{cosine_distance, dot_product, euclidean_distance, normalize};

    let metric = self.options.metric;

    let query_normalized: Vec<f32>;
    let query_for_search = if metric == DistanceMetric::Cosine {
      query_normalized = normalize(query);
      &query_normalized
    } else {
      query
    };

    let mut candidates: Vec<VectorSearchResult> = Vec::new();

    for (&node_id, &vector_id) in &self.manifest.node_to_vector {
      if let Some(filter) = filter {
        if !filter(node_id) {
          continue;
        }
      }

      if let Some(vector) = vector_store_node_vector(&self.manifest, node_id) {
        let distance = match metric {
          DistanceMetric::Cosine => cosine_distance(query_for_search, vector),
          DistanceMetric::Euclidean => euclidean_distance(query_for_search, vector),
          DistanceMetric::DotProduct => -dot_product(query_for_search, vector), // Negate for sorting
        };

        let similarity = metric.distance_to_similarity(distance);
        if let Some(threshold) = threshold {
          if similarity < threshold {
            continue;
          }
        }

        candidates.push(VectorSearchResult {
          vector_id,
          node_id,
          distance,
          similarity,
        });
      }
    }

    candidates.sort_by(|a, b| {
      a.distance
        .partial_cmp(&b.distance)
        .unwrap_or(std::cmp::Ordering::Equal)
    });
    candidates.truncate(k);
    candidates
  }

  /// Get index statistics
  pub fn stats(&self) -> VectorIndexStats {
    let store_stats = vector_store_stats(&self.manifest);
    VectorIndexStats {
      total_vectors: store_stats.total_vectors,
      live_vectors: store_stats.live_vectors,
      dimensions: self.options.dimensions,
      metric: self.options.metric,
      index_trained: self
        .index
        .as_ref()
        .map(BuiltIndex::trained)
        .unwrap_or(false),
      index_clusters: self.index.as_ref().map(BuiltIndex::n_clusters),
    }
  }

  /// Clear all vectors and reset the index
  pub fn clear(&mut self) {
    vector_store_clear(&mut self.manifest);
    self.node_cache = LruCache::new(self.options.cache_max_size);
    self.cached_node_ids.clear();
    self.index = None;
    self.needs_training = true;
  }

  /// Get the number of vectors in the index
  pub fn len(&self) -> usize {
    self.manifest.node_to_vector.len()
  }

  /// Check if the index is empty
  pub fn is_empty(&self) -> bool {
    self.manifest.node_to_vector.is_empty()
  }
}

// ============================================================================
// Errors
// ============================================================================

/// Errors that can occur during vector index operations
#[derive(Debug, Clone)]
pub enum VectorIndexError {
  /// Modification attempted while build is in progress
  BuildInProgress,
  /// Build already in progress
  BuildAlreadyInProgress,
  /// Vector dimension mismatch
  DimensionMismatch { expected: usize, got: usize },
  /// Invalid vector (contains NaN or Inf)
  InvalidVector,
  /// Store error
  StoreError(String),
  /// Training error
  TrainingError(String),
}

impl std::fmt::Display for VectorIndexError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      VectorIndexError::BuildInProgress => {
        write!(f, "Cannot modify vectors while index is being built")
      }
      VectorIndexError::BuildAlreadyInProgress => {
        write!(f, "Index build already in progress")
      }
      VectorIndexError::DimensionMismatch { expected, got } => {
        write!(
          f,
          "Vector dimension mismatch: expected {expected}, got {got}"
        )
      }
      VectorIndexError::InvalidVector => {
        write!(f, "Invalid vector: contains NaN or Inf values")
      }
      VectorIndexError::StoreError(msg) => {
        write!(f, "Store error: {msg}")
      }
      VectorIndexError::TrainingError(msg) => {
        write!(f, "Training error: {msg}")
      }
    }
  }
}

impl std::error::Error for VectorIndexError {}

// ============================================================================
// Helper Functions
// ============================================================================

/// Validate a vector (no NaN or Inf values)
fn is_valid_vector(vector: &[f32]) -> bool {
  vector.iter().all(|&v| v.is_finite())
}

fn ivf_error_to_index_error(err: IvfError) -> VectorIndexError {
  match err {
    IvfError::DimensionMismatch { expected, got } => {
      VectorIndexError::DimensionMismatch { expected, got }
    }
    other => VectorIndexError::TrainingError(other.to_string()),
  }
}

fn ivf_pq_error_to_index_error(err: IvfPqError) -> VectorIndexError {
  match err {
    IvfPqError::DimensionMismatch { expected, got } => {
      VectorIndexError::DimensionMismatch { expected, got }
    }
    other => VectorIndexError::TrainingError(other.to_string()),
  }
}

fn resolve_pq_subspaces(requested: usize, dimensions: usize) -> usize {
  let capped = requested.max(1).min(dimensions.max(1));
  for candidate in (1..=capped).rev() {
    if dimensions % candidate == 0 {
      return candidate;
    }
  }
  1
}

// ============================================================================
// Factory Function
// ============================================================================

/// Create a new vector index
///
/// # Example
/// ```rust,no_run
/// use kitedb::api::vector_search::{create_vector_index, VectorIndexOptions};
/// use kitedb::vector::types::DistanceMetric;
/// # fn main() {
///
/// // Create index for 768-dimensional embeddings (e.g., from OpenAI)
/// let index = create_vector_index(VectorIndexOptions::new(768));
///
/// // Or with custom configuration
/// let index = create_vector_index(
///     VectorIndexOptions::new(1536)
///         .with_metric(DistanceMetric::Cosine)
///         .with_training_threshold(500)
///         .with_n_probe(20)
/// );
/// # }
/// ```
pub fn create_vector_index(options: VectorIndexOptions) -> VectorIndex {
  VectorIndex::new(options)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_vector_index_options_default() {
    let opts = VectorIndexOptions::new(768);
    assert_eq!(opts.dimensions, 768);
    assert_eq!(opts.metric, DistanceMetric::Cosine);
    assert!(opts.normalize);
    assert_eq!(opts.training_threshold, DEFAULT_TRAINING_THRESHOLD);
    assert_eq!(opts.ann_algorithm, AnnAlgorithm::IvfPq);
    assert_eq!(opts.pq_subspaces, DEFAULT_PQ_SUBSPACES);
    assert_eq!(opts.pq_centroids, DEFAULT_PQ_CENTROIDS);
    assert!(!opts.pq_residuals);
  }

  #[test]
  fn test_vector_index_options_builder() {
    let opts = VectorIndexOptions::new(512)
      .with_metric(DistanceMetric::Euclidean)
      .with_normalize(false)
      .with_n_probe(20)
      .with_training_threshold(500)
      .with_ann_algorithm(AnnAlgorithm::Ivf)
      .with_pq_subspaces(32)
      .with_pq_centroids(128)
      .with_pq_residuals(true);

    assert_eq!(opts.dimensions, 512);
    assert_eq!(opts.metric, DistanceMetric::Euclidean);
    assert!(!opts.normalize);
    assert_eq!(opts.n_probe, 20);
    assert_eq!(opts.training_threshold, 500);
    assert_eq!(opts.ann_algorithm, AnnAlgorithm::Ivf);
    assert_eq!(opts.pq_subspaces, 32);
    assert_eq!(opts.pq_centroids, 128);
    assert!(opts.pq_residuals);
  }

  #[test]
  fn test_similar_options() {
    let opts = SimilarOptions::new(10).with_threshold(0.8).with_n_probe(5);

    assert_eq!(opts.k, 10);
    assert_eq!(opts.threshold, Some(0.8));
    assert_eq!(opts.n_probe, Some(5));
  }

  #[test]
  fn test_vector_index_new() {
    let index = VectorIndex::new(VectorIndexOptions::new(128));
    assert!(index.is_empty());
    assert_eq!(index.len(), 0);
  }

  #[test]
  fn test_vector_index_set_get() {
    let mut index = VectorIndex::new(VectorIndexOptions::new(4));

    let vector = vec![1.0, 0.0, 0.0, 0.0];
    index.set(1, &vector).expect("expected value");

    assert!(index.has(1));
    assert!(!index.has(2));

    let retrieved = index.get(1).expect("expected value");
    // Note: may be normalized if cosine metric
    assert_eq!(retrieved.len(), 4);
  }

  #[test]
  fn test_vector_index_delete() {
    let mut index = VectorIndex::new(VectorIndexOptions::new(4));

    let vector = vec![1.0, 0.0, 0.0, 0.0];
    index.set(1, &vector).expect("expected value");

    assert!(index.has(1));
    let deleted = index.delete(1).expect("expected value");
    assert!(deleted);
    assert!(!index.has(1));
  }

  #[test]
  fn test_vector_index_dimension_mismatch() {
    let mut index = VectorIndex::new(VectorIndexOptions::new(4));

    let vector = vec![1.0, 0.0, 0.0]; // Wrong dimension
    let result = index.set(1, &vector);

    assert!(matches!(
      result,
      Err(VectorIndexError::DimensionMismatch {
        expected: 4,
        got: 3
      })
    ));
  }

  #[test]
  fn test_vector_index_invalid_vector() {
    let mut index = VectorIndex::new(VectorIndexOptions::new(4));

    let vector = vec![1.0, f32::NAN, 0.0, 0.0];
    let result = index.set(1, &vector);
    assert!(matches!(result, Err(VectorIndexError::StoreError(_))));
  }

  #[test]
  fn test_vector_index_clear() {
    let mut index = VectorIndex::new(VectorIndexOptions::new(4));

    index.set(1, &[1.0, 0.0, 0.0, 0.0]).expect("expected value");
    index.set(2, &[0.0, 1.0, 0.0, 0.0]).expect("expected value");

    assert_eq!(index.len(), 2);

    index.clear();

    assert!(index.is_empty());
    assert_eq!(index.len(), 0);
  }

  #[test]
  fn test_vector_index_stats() {
    let mut index = VectorIndex::new(VectorIndexOptions::new(4));

    index.set(1, &[1.0, 0.0, 0.0, 0.0]).expect("expected value");
    index.set(2, &[0.0, 1.0, 0.0, 0.0]).expect("expected value");

    let stats = index.stats();
    assert_eq!(stats.dimensions, 4);
    assert_eq!(stats.live_vectors, 2);
    assert!(!stats.index_trained);
  }

  #[test]
  fn test_brute_force_search() {
    let mut index = VectorIndex::new(
      VectorIndexOptions::new(4).with_training_threshold(1000), // High threshold to force brute force
    );

    // Insert some vectors
    index.set(1, &[1.0, 0.0, 0.0, 0.0]).expect("expected value");
    index.set(2, &[0.0, 1.0, 0.0, 0.0]).expect("expected value");
    index
      .set(3, &[0.707, 0.707, 0.0, 0.0])
      .expect("expected value");

    // Search for vector similar to [1, 0, 0, 0]
    let query = vec![1.0, 0.0, 0.0, 0.0];
    let results = index
      .search(&query, SimilarOptions::new(3))
      .expect("expected value");

    assert!(!results.is_empty());
    // First result should be node 1 (exact match)
    assert_eq!(results[0].node_id, 1);
    assert!(results[0].similarity > 0.99);
  }

  #[test]
  fn test_search_with_threshold() {
    let mut index = VectorIndex::new(VectorIndexOptions::new(4).with_training_threshold(1000));

    index.set(1, &[1.0, 0.0, 0.0, 0.0]).expect("expected value");
    index.set(2, &[0.0, 1.0, 0.0, 0.0]).expect("expected value"); // Orthogonal

    let query = vec![1.0, 0.0, 0.0, 0.0];
    let results = index
      .search(&query, SimilarOptions::new(10).with_threshold(0.5))
      .expect("expected value");

    // Should only return node 1 (high similarity)
    // Node 2 is orthogonal (similarity ~0)
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].node_id, 1);
  }

  #[test]
  fn test_search_with_filter_returns_best_allowed_result_brute_force() {
    let mut index = VectorIndex::new(VectorIndexOptions::new(4).with_training_threshold(1000));

    // Disallowed exact matches - would dominate top-k without filter
    for node_id in 1..=10 {
      index
        .set(node_id, &[1.0, 0.0, 0.0, 0.0])
        .expect("expected value");
    }

    // Allowed but less-similar
    index
      .set(100, &[0.8, 0.6, 0.0, 0.0])
      .expect("expected value");
    index
      .set(101, &[0.0, 1.0, 0.0, 0.0])
      .expect("expected value");

    let query = vec![1.0, 0.0, 0.0, 0.0];
    let results = index
      .search(
        &query,
        SimilarOptions::new(1).with_filter(|node_id| node_id >= 100),
      )
      .expect("expected value");

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].node_id, 100);
  }

  #[test]
  fn test_ivf_search_trained_path_and_filter() {
    let mut index = VectorIndex::new(
      VectorIndexOptions::new(4)
        .with_training_threshold(2)
        .with_n_clusters(1)
        .with_n_probe(1),
    );

    // Disallowed exact matches
    for node_id in 1..=10 {
      index
        .set(node_id, &[1.0, 0.0, 0.0, 0.0])
        .expect("expected value");
    }
    index
      .set(100, &[0.8, 0.6, 0.0, 0.0])
      .expect("expected value");
    index
      .set(101, &[0.0, 1.0, 0.0, 0.0])
      .expect("expected value");

    index.build_index().expect("expected value");
    let stats = index.stats();
    assert!(stats.index_trained);
    assert_eq!(stats.index_clusters, Some(1));

    let query = vec![1.0, 0.0, 0.0, 0.0];
    let results = index
      .search(
        &query,
        SimilarOptions::new(1).with_filter(|node_id| node_id >= 100),
      )
      .expect("expected value");

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].node_id, 100);
  }

  #[test]
  fn test_is_valid_vector() {
    assert!(is_valid_vector(&[1.0, 2.0, 3.0]));
    assert!(is_valid_vector(&[0.0, 0.0, 0.0]));
    assert!(!is_valid_vector(&[1.0, f32::NAN, 3.0]));
    assert!(!is_valid_vector(&[1.0, f32::INFINITY, 3.0]));
    assert!(!is_valid_vector(&[f32::NEG_INFINITY, 2.0, 3.0]));
  }

  #[test]
  fn test_create_vector_index() {
    let index = create_vector_index(VectorIndexOptions::new(256));
    assert!(index.is_empty());
  }
}
