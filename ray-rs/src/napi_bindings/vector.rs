//! NAPI bindings for Vector Search
//!
//! Exposes IVF and IVF-PQ indexes to Node.js/Bun.

use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::sync::RwLock;

use crate::api::vector_search::{
  SimilarOptions as RustSimilarOptions, VectorIndex as RustVectorIndex,
  VectorIndexError as RustVectorIndexError, VectorIndexOptions as RustVectorIndexOptions,
  VectorIndexStats as RustVectorIndexStats, VectorSearchHit as RustVectorSearchHit,
};
use crate::vector::{
  DistanceMetric as RustDistanceMetric, IvfConfig as RustIvfConfig, IvfIndex as RustIvfIndex,
  IvfPqConfig as RustIvfPqConfig, IvfPqIndex as RustIvfPqIndex, MultiQueryAggregation,
  PqConfig as RustPqConfig, SearchOptions as RustSearchOptions, VectorManifest, VectorSearchResult,
};

// ============================================================================
// Distance Metric
// ============================================================================

/// Distance metric for vector similarity
#[napi(string_enum)]
#[derive(Debug)]
pub enum JsDistanceMetric {
  /// Cosine similarity (1 - cosine)
  Cosine,
  /// Euclidean (L2) distance
  Euclidean,
  /// Dot product (negated for distance)
  DotProduct,
}

impl From<JsDistanceMetric> for RustDistanceMetric {
  fn from(m: JsDistanceMetric) -> Self {
    match m {
      JsDistanceMetric::Cosine => RustDistanceMetric::Cosine,
      JsDistanceMetric::Euclidean => RustDistanceMetric::Euclidean,
      JsDistanceMetric::DotProduct => RustDistanceMetric::DotProduct,
    }
  }
}

impl From<RustDistanceMetric> for JsDistanceMetric {
  fn from(m: RustDistanceMetric) -> Self {
    match m {
      RustDistanceMetric::Cosine => JsDistanceMetric::Cosine,
      RustDistanceMetric::Euclidean => JsDistanceMetric::Euclidean,
      RustDistanceMetric::DotProduct => JsDistanceMetric::DotProduct,
    }
  }
}

// ============================================================================
// Aggregation Method
// ============================================================================

/// Aggregation method for multi-query search
#[napi(string_enum)]
pub enum JsAggregation {
  /// Minimum distance (best match)
  Min,
  /// Maximum distance (worst match)
  Max,
  /// Average distance
  Avg,
  /// Sum of distances
  Sum,
}

impl From<JsAggregation> for MultiQueryAggregation {
  fn from(a: JsAggregation) -> Self {
    match a {
      JsAggregation::Min => MultiQueryAggregation::Min,
      JsAggregation::Max => MultiQueryAggregation::Max,
      JsAggregation::Avg => MultiQueryAggregation::Avg,
      JsAggregation::Sum => MultiQueryAggregation::Sum,
    }
  }
}

// ============================================================================
// IVF Configuration
// ============================================================================

/// Configuration for IVF index
#[napi(object)]
#[derive(Debug, Default)]
pub struct JsIvfConfig {
  /// Number of clusters (default: 100)
  pub n_clusters: Option<i32>,
  /// Number of clusters to probe during search (default: 10)
  pub n_probe: Option<i32>,
  /// Distance metric (default: Cosine)
  pub metric: Option<JsDistanceMetric>,
}

impl From<JsIvfConfig> for RustIvfConfig {
  fn from(c: JsIvfConfig) -> Self {
    let mut config = RustIvfConfig::default();
    if let Some(n) = c.n_clusters {
      config.n_clusters = n as usize;
    }
    if let Some(n) = c.n_probe {
      config.n_probe = n as usize;
    }
    if let Some(m) = c.metric {
      config.metric = m.into();
    }
    config
  }
}

// ============================================================================
// PQ Configuration
// ============================================================================

/// Configuration for Product Quantization
#[napi(object)]
#[derive(Debug, Default)]
pub struct JsPqConfig {
  /// Number of subspaces (must divide dimensions evenly)
  pub num_subspaces: Option<i32>,
  /// Number of centroids per subspace (default: 256)
  pub num_centroids: Option<i32>,
  /// Max k-means iterations for training (default: 25)
  pub max_iterations: Option<i32>,
}

impl From<JsPqConfig> for RustPqConfig {
  fn from(c: JsPqConfig) -> Self {
    let mut config = RustPqConfig::default();
    if let Some(n) = c.num_subspaces {
      config.num_subspaces = n as usize;
    }
    if let Some(n) = c.num_centroids {
      config.num_centroids = n as usize;
    }
    if let Some(n) = c.max_iterations {
      config.max_iterations = n as usize;
    }
    config
  }
}

// ============================================================================
// Search Options
// ============================================================================

/// Options for vector search
#[napi(object)]
#[derive(Debug, Default)]
pub struct JsSearchOptions {
  /// Number of clusters to probe (overrides index default)
  pub n_probe: Option<i32>,
  /// Minimum similarity threshold (0-1)
  pub threshold: Option<f64>,
}

// ============================================================================
// Search Result
// ============================================================================

/// Result of a vector search
#[napi(object)]
pub struct JsSearchResult {
  /// Vector ID
  pub vector_id: i64,
  /// Associated node ID
  pub node_id: i64,
  /// Distance from query
  pub distance: f64,
  /// Similarity score (0-1, higher is more similar)
  pub similarity: f64,
}

impl From<VectorSearchResult> for JsSearchResult {
  fn from(r: VectorSearchResult) -> Self {
    JsSearchResult {
      vector_id: r.vector_id as i64,
      node_id: r.node_id as i64,
      distance: r.distance as f64,
      similarity: r.similarity as f64,
    }
  }
}

// ============================================================================
// IVF Index Statistics
// ============================================================================

/// Statistics for IVF index
#[napi(object)]
pub struct JsIvfStats {
  /// Whether the index is trained
  pub trained: bool,
  /// Number of clusters
  pub n_clusters: i32,
  /// Total vectors in the index
  pub total_vectors: i64,
  /// Average vectors per cluster
  pub avg_vectors_per_cluster: f64,
  /// Number of empty clusters
  pub empty_cluster_count: i32,
  /// Minimum cluster size
  pub min_cluster_size: i32,
  /// Maximum cluster size
  pub max_cluster_size: i32,
}

// ============================================================================
// IVF Index NAPI Wrapper
// ============================================================================

/// IVF (Inverted File) index for approximate nearest neighbor search
#[napi]
pub struct JsIvfIndex {
  inner: RwLock<RustIvfIndex>,
}

#[napi]
impl JsIvfIndex {
  /// Create a new IVF index
  #[napi(constructor)]
  pub fn new(dimensions: i32, config: Option<JsIvfConfig>) -> Result<JsIvfIndex> {
    let rust_config = config.unwrap_or_default().into();
    Ok(JsIvfIndex {
      inner: RwLock::new(RustIvfIndex::new(dimensions as usize, rust_config)),
    })
  }

  /// Get the number of dimensions
  #[napi(getter)]
  pub fn dimensions(&self) -> Result<i32> {
    let index = self
      .inner
      .read()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    Ok(index.dimensions as i32)
  }

  /// Check if the index is trained
  #[napi(getter)]
  pub fn trained(&self) -> Result<bool> {
    let index = self
      .inner
      .read()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    Ok(index.trained)
  }

  /// Add training vectors
  ///
  /// Call this before train() with representative vectors from your dataset.
  #[napi]
  pub fn add_training_vectors(&self, vectors: Vec<f64>, num_vectors: i32) -> Result<()> {
    let mut index = self
      .inner
      .write()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    let vectors_f32: Vec<f32> = vectors.iter().map(|&v| v as f32).collect();
    index
      .add_training_vectors(&vectors_f32, num_vectors as usize)
      .map_err(|e| Error::from_reason(format!("Failed to add training vectors: {e}")))
  }

  /// Train the index on added training vectors
  ///
  /// This runs k-means clustering to create the inverted file structure.
  #[napi]
  pub fn train(&self) -> Result<()> {
    let mut index = self
      .inner
      .write()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    index
      .train()
      .map_err(|e| Error::from_reason(format!("Failed to train index: {e}")))
  }

  /// Insert a vector into the index
  ///
  /// The index must be trained first.
  #[napi]
  pub fn insert(&self, vector_id: i64, vector: Vec<f64>) -> Result<()> {
    let mut index = self
      .inner
      .write()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    let vector_f32: Vec<f32> = vector.iter().map(|&v| v as f32).collect();
    index
      .insert(vector_id as u64, &vector_f32)
      .map_err(|e| Error::from_reason(format!("Failed to insert vector: {e}")))
  }

  /// Delete a vector from the index
  ///
  /// Requires the vector data to determine which cluster to remove from.
  #[napi]
  pub fn delete(&self, vector_id: i64, vector: Vec<f64>) -> Result<bool> {
    let mut index = self
      .inner
      .write()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    let vector_f32: Vec<f32> = vector.iter().map(|&v| v as f32).collect();
    Ok(index.delete(vector_id as u64, &vector_f32))
  }

  /// Clear all data from the index
  #[napi]
  pub fn clear(&self) -> Result<()> {
    let mut index = self
      .inner
      .write()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    index.clear();
    Ok(())
  }

  /// Search for k nearest neighbors
  ///
  /// Requires a VectorManifest to look up actual vector data.
  #[napi]
  pub fn search(
    &self,
    manifest_json: String,
    query: Vec<f64>,
    k: i32,
    options: Option<JsSearchOptions>,
  ) -> Result<Vec<JsSearchResult>> {
    let index = self
      .inner
      .read()
      .map_err(|e| Error::from_reason(e.to_string()))?;

    // Parse manifest from JSON
    let manifest: VectorManifest = serde_json::from_str(&manifest_json)
      .map_err(|e| Error::from_reason(format!("Failed to parse manifest: {e}")))?;

    let query_f32: Vec<f32> = query.iter().map(|&v| v as f32).collect();

    let rust_options = options.map(|o| RustSearchOptions {
      n_probe: o.n_probe.map(|n| n as usize),
      filter: None,
      threshold: o.threshold.map(|t| t as f32),
    });

    let results = index.search(&manifest, &query_f32, k as usize, rust_options);
    Ok(results.into_iter().map(|r| r.into()).collect())
  }

  /// Search with multiple query vectors
  ///
  /// Aggregates results using the specified method.
  #[napi]
  pub fn search_multi(
    &self,
    manifest_json: String,
    queries: Vec<Vec<f64>>,
    k: i32,
    aggregation: JsAggregation,
    options: Option<JsSearchOptions>,
  ) -> Result<Vec<JsSearchResult>> {
    let index = self
      .inner
      .read()
      .map_err(|e| Error::from_reason(e.to_string()))?;

    // Parse manifest from JSON
    let manifest: VectorManifest = serde_json::from_str(&manifest_json)
      .map_err(|e| Error::from_reason(format!("Failed to parse manifest: {e}")))?;

    let queries_f32: Vec<Vec<f32>> = queries
      .iter()
      .map(|q| q.iter().map(|&v| v as f32).collect())
      .collect();

    let query_refs: Vec<&[f32]> = queries_f32.iter().map(|q| q.as_slice()).collect();

    let rust_options = options.map(|o| RustSearchOptions {
      n_probe: o.n_probe.map(|n| n as usize),
      filter: None,
      threshold: o.threshold.map(|t| t as f32),
    });

    let results = index.search_multi(
      &manifest,
      &query_refs,
      k as usize,
      aggregation.into(),
      rust_options,
    );
    Ok(results.into_iter().map(|r| r.into()).collect())
  }

  /// Get index statistics
  #[napi]
  pub fn stats(&self) -> Result<JsIvfStats> {
    let index = self
      .inner
      .read()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    let s = index.stats();
    Ok(JsIvfStats {
      trained: s.trained,
      n_clusters: s.n_clusters as i32,
      total_vectors: s.total_vectors as i64,
      avg_vectors_per_cluster: s.avg_vectors_per_cluster as f64,
      empty_cluster_count: s.empty_cluster_count as i32,
      min_cluster_size: s.min_cluster_size as i32,
      max_cluster_size: s.max_cluster_size as i32,
    })
  }

  /// Serialize the index to bytes
  #[napi]
  pub fn serialize(&self) -> Result<Buffer> {
    let index = self
      .inner
      .read()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    let bytes = crate::vector::ivf::serialize::serialize_ivf(&index);
    Ok(Buffer::from(bytes))
  }

  /// Deserialize an index from bytes
  #[napi(factory)]
  pub fn deserialize(data: Buffer) -> Result<JsIvfIndex> {
    let index = crate::vector::ivf::serialize::deserialize_ivf(&data)
      .map_err(|e| Error::from_reason(format!("Failed to deserialize: {e}")))?;
    Ok(JsIvfIndex {
      inner: RwLock::new(index),
    })
  }
}

// ============================================================================
// IVF-PQ Index NAPI Wrapper
// ============================================================================

/// IVF-PQ combined index for memory-efficient approximate nearest neighbor search
#[napi]
pub struct JsIvfPqIndex {
  inner: RwLock<RustIvfPqIndex>,
}

#[napi]
impl JsIvfPqIndex {
  /// Create a new IVF-PQ index
  #[napi(constructor)]
  pub fn new(
    dimensions: i32,
    ivf_config: Option<JsIvfConfig>,
    pq_config: Option<JsPqConfig>,
    use_residuals: Option<bool>,
  ) -> Result<JsIvfPqIndex> {
    let config = RustIvfPqConfig {
      ivf: ivf_config.unwrap_or_default().into(),
      pq: pq_config.unwrap_or_default().into(),
      use_residuals: use_residuals.unwrap_or(true),
    };

    let index = RustIvfPqIndex::new(dimensions as usize, config)
      .map_err(|e| Error::from_reason(format!("Failed to create index: {e}")))?;

    Ok(JsIvfPqIndex {
      inner: RwLock::new(index),
    })
  }

  /// Get the number of dimensions
  #[napi(getter)]
  pub fn dimensions(&self) -> Result<i32> {
    let index = self
      .inner
      .read()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    Ok(index.dimensions as i32)
  }

  /// Check if the index is trained
  #[napi(getter)]
  pub fn trained(&self) -> Result<bool> {
    let index = self
      .inner
      .read()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    Ok(index.trained)
  }

  /// Add training vectors
  #[napi]
  pub fn add_training_vectors(&self, vectors: Vec<f64>, num_vectors: i32) -> Result<()> {
    let mut index = self
      .inner
      .write()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    let vectors_f32: Vec<f32> = vectors.iter().map(|&v| v as f32).collect();
    index
      .add_training_vectors(&vectors_f32, num_vectors as usize)
      .map_err(|e| Error::from_reason(format!("Failed to add training vectors: {e}")))
  }

  /// Train the index
  #[napi]
  pub fn train(&self) -> Result<()> {
    let mut index = self
      .inner
      .write()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    index
      .train()
      .map_err(|e| Error::from_reason(format!("Failed to train index: {e}")))
  }

  /// Insert a vector
  #[napi]
  pub fn insert(&self, vector_id: i64, vector: Vec<f64>) -> Result<()> {
    let mut index = self
      .inner
      .write()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    let vector_f32: Vec<f32> = vector.iter().map(|&v| v as f32).collect();
    index
      .insert(vector_id as u64, &vector_f32)
      .map_err(|e| Error::from_reason(format!("Failed to insert vector: {e}")))
  }

  /// Delete a vector
  ///
  /// Requires the vector data to determine which cluster to remove from.
  #[napi]
  pub fn delete(&self, vector_id: i64, vector: Vec<f64>) -> Result<bool> {
    let mut index = self
      .inner
      .write()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    let vector_f32: Vec<f32> = vector.iter().map(|&v| v as f32).collect();
    Ok(index.delete(vector_id as u64, &vector_f32))
  }

  /// Clear the index
  #[napi]
  pub fn clear(&self) -> Result<()> {
    let mut index = self
      .inner
      .write()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    index.clear();
    Ok(())
  }

  /// Search for k nearest neighbors using PQ distance approximation
  #[napi]
  pub fn search(
    &self,
    manifest_json: String,
    query: Vec<f64>,
    k: i32,
    options: Option<JsSearchOptions>,
  ) -> Result<Vec<JsSearchResult>> {
    let index = self
      .inner
      .read()
      .map_err(|e| Error::from_reason(e.to_string()))?;

    // Parse manifest from JSON
    let manifest: VectorManifest = serde_json::from_str(&manifest_json)
      .map_err(|e| Error::from_reason(format!("Failed to parse manifest: {e}")))?;

    let query_f32: Vec<f32> = query.iter().map(|&v| v as f32).collect();

    let rust_options = options.map(|o| crate::vector::ivf_pq::IvfPqSearchOptions {
      n_probe: o.n_probe.map(|n| n as usize),
      filter: None,
      threshold: o.threshold.map(|t| t as f32),
    });

    let results = index.search(&manifest, &query_f32, k as usize, rust_options);
    Ok(results.into_iter().map(|r| r.into()).collect())
  }

  /// Search with multiple query vectors
  #[napi]
  pub fn search_multi(
    &self,
    manifest_json: String,
    queries: Vec<Vec<f64>>,
    k: i32,
    aggregation: JsAggregation,
    options: Option<JsSearchOptions>,
  ) -> Result<Vec<JsSearchResult>> {
    let index = self
      .inner
      .read()
      .map_err(|e| Error::from_reason(e.to_string()))?;

    // Parse manifest from JSON
    let manifest: VectorManifest = serde_json::from_str(&manifest_json)
      .map_err(|e| Error::from_reason(format!("Failed to parse manifest: {e}")))?;

    let queries_f32: Vec<Vec<f32>> = queries
      .iter()
      .map(|q| q.iter().map(|&v| v as f32).collect())
      .collect();

    let query_refs: Vec<&[f32]> = queries_f32.iter().map(|q| q.as_slice()).collect();

    let rust_options = options.map(|o| crate::vector::ivf_pq::IvfPqSearchOptions {
      n_probe: o.n_probe.map(|n| n as usize),
      filter: None,
      threshold: o.threshold.map(|t| t as f32),
    });

    let results = index.search_multi(
      &manifest,
      &query_refs,
      k as usize,
      aggregation.into(),
      rust_options,
    );
    Ok(results.into_iter().map(|r| r.into()).collect())
  }

  /// Get index statistics
  #[napi]
  pub fn stats(&self) -> Result<JsIvfStats> {
    let index = self
      .inner
      .read()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    let s = index.stats();
    Ok(JsIvfStats {
      trained: s.trained,
      n_clusters: s.n_clusters as i32,
      total_vectors: s.total_vectors as i64,
      avg_vectors_per_cluster: s.avg_vectors_per_cluster as f64,
      empty_cluster_count: s.empty_cluster_count as i32,
      min_cluster_size: s.min_cluster_size as i32,
      max_cluster_size: s.max_cluster_size as i32,
    })
  }

  /// Serialize the index to bytes
  #[napi]
  pub fn serialize(&self) -> Result<Buffer> {
    let index = self
      .inner
      .read()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    let bytes = crate::vector::ivf_pq::serialize_ivf_pq(&index);
    Ok(Buffer::from(bytes))
  }

  /// Deserialize an index from bytes
  #[napi(factory)]
  pub fn deserialize(data: Buffer) -> Result<JsIvfPqIndex> {
    let index = crate::vector::ivf_pq::deserialize_ivf_pq(&data)
      .map_err(|e| Error::from_reason(format!("Failed to deserialize: {e}")))?;
    Ok(JsIvfPqIndex {
      inner: RwLock::new(index),
    })
  }
}

// ============================================================================
// Brute Force Search (for small datasets or verification)
// ============================================================================

/// Brute force search result
#[napi(object)]
pub struct JsBruteForceResult {
  pub node_id: i64,
  pub distance: f64,
  pub similarity: f64,
}

/// Perform brute-force search over all vectors
///
/// Useful for small datasets or verifying IVF results.
#[napi]
pub fn brute_force_search(
  vectors: Vec<Vec<f64>>,
  node_ids: Vec<i64>,
  query: Vec<f64>,
  k: i32,
  metric: Option<JsDistanceMetric>,
) -> Result<Vec<JsBruteForceResult>> {
  if vectors.len() != node_ids.len() {
    return Err(Error::from_reason(
      "vectors and node_ids must have same length",
    ));
  }

  let metric = metric.unwrap_or(JsDistanceMetric::Cosine);
  let rust_metric: RustDistanceMetric = metric.into();
  let distance_fn = rust_metric.distance_fn();

  let query_f32: Vec<f32> = query.iter().map(|&v| v as f32).collect();

  let mut results: Vec<(i64, f32)> = vectors
    .iter()
    .zip(node_ids.iter())
    .map(|(v, &node_id)| {
      let v_f32: Vec<f32> = v.iter().map(|&x| x as f32).collect();
      let dist = distance_fn(&query_f32, &v_f32);
      (node_id, dist)
    })
    .collect();

  // Sort by distance
  results.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
  results.truncate(k as usize);

  Ok(
    results
      .into_iter()
      .map(|(node_id, distance)| JsBruteForceResult {
        node_id,
        distance: distance as f64,
        similarity: rust_metric.distance_to_similarity(distance) as f64,
      })
      .collect(),
  )
}

// =============================================================================
// High-level VectorIndex API
// =============================================================================

/// Options for creating a vector index
#[napi(object)]
pub struct VectorIndexOptions {
  /// Vector dimensions (required)
  pub dimensions: i32,
  /// Distance metric (default: Cosine)
  pub metric: Option<JsDistanceMetric>,
  /// Vectors per row group (default: 1024)
  pub row_group_size: Option<i32>,
  /// Vectors per fragment before sealing (default: 100_000)
  pub fragment_target_size: Option<i32>,
  /// Whether to auto-normalize vectors (default: true for cosine)
  pub normalize: Option<bool>,
  /// IVF index configuration
  pub ivf: Option<JsIvfConfig>,
  /// Minimum training vectors before index training (default: 1000)
  pub training_threshold: Option<i32>,
  /// Maximum node IDs to cache for search results (default: 10_000)
  pub cache_max_size: Option<i32>,
}

impl VectorIndexOptions {
  fn into_rust(self) -> Result<RustVectorIndexOptions> {
    if self.dimensions <= 0 {
      return Err(Error::from_reason("dimensions must be positive"));
    }

    let mut options = RustVectorIndexOptions::new(self.dimensions as usize);

    if let Some(metric) = self.metric {
      options = options.with_metric(metric.into());
    }

    if let Some(row_group_size) = self.row_group_size {
      if row_group_size <= 0 {
        return Err(Error::from_reason("rowGroupSize must be positive"));
      }
      options = options.with_row_group_size(row_group_size as usize);
    }

    if let Some(fragment_target_size) = self.fragment_target_size {
      if fragment_target_size <= 0 {
        return Err(Error::from_reason("fragmentTargetSize must be positive"));
      }
      options = options.with_fragment_target_size(fragment_target_size as usize);
    }

    if let Some(ivf) = self.ivf {
      if let Some(n_clusters) = ivf.n_clusters {
        if n_clusters <= 0 {
          return Err(Error::from_reason("ivf.nClusters must be positive"));
        }
        options = options.with_n_clusters(n_clusters as usize);
      }

      if let Some(n_probe) = ivf.n_probe {
        if n_probe <= 0 {
          return Err(Error::from_reason("ivf.nProbe must be positive"));
        }
        options = options.with_n_probe(n_probe as usize);
      }
    }

    if let Some(training_threshold) = self.training_threshold {
      if training_threshold <= 0 {
        return Err(Error::from_reason("trainingThreshold must be positive"));
      }
      options = options.with_training_threshold(training_threshold as usize);
    }

    if let Some(cache_max_size) = self.cache_max_size {
      if cache_max_size <= 0 {
        return Err(Error::from_reason("cacheMaxSize must be positive"));
      }
      options = options.with_cache_max_size(cache_max_size as usize);
    }

    if let Some(normalize) = self.normalize {
      options = options.with_normalize(normalize);
    }

    Ok(options)
  }
}

/// Options for similarity search
#[napi(object)]
pub struct SimilarOptions {
  /// Number of results to return
  pub k: i32,
  /// Minimum similarity threshold (0-1 for cosine)
  pub threshold: Option<f64>,
  /// Number of clusters to probe for IVF (default: 10)
  pub n_probe: Option<i32>,
}

impl SimilarOptions {
  fn into_rust(self) -> Result<RustSimilarOptions> {
    if self.k <= 0 {
      return Err(Error::from_reason("k must be positive"));
    }

    let mut options = RustSimilarOptions::new(self.k as usize);
    if let Some(threshold) = self.threshold {
      options = options.with_threshold(threshold as f32);
    }
    if let Some(n_probe) = self.n_probe {
      if n_probe <= 0 {
        return Err(Error::from_reason("nProbe must be positive"));
      }
      options = options.with_n_probe(n_probe as usize);
    }
    Ok(options)
  }
}

/// Search result hit
#[napi(object)]
pub struct VectorSearchHit {
  pub node_id: i64,
  pub distance: f64,
  pub similarity: f64,
}

impl From<RustVectorSearchHit> for VectorSearchHit {
  fn from(hit: RustVectorSearchHit) -> Self {
    VectorSearchHit {
      node_id: hit.node_id as i64,
      distance: hit.distance as f64,
      similarity: hit.similarity as f64,
    }
  }
}

/// Vector index statistics
#[napi(object)]
pub struct VectorIndexStats {
  pub total_vectors: i64,
  pub live_vectors: i64,
  pub dimensions: i32,
  pub metric: JsDistanceMetric,
  pub index_trained: bool,
  pub index_clusters: Option<i32>,
}

impl From<RustVectorIndexStats> for VectorIndexStats {
  fn from(stats: RustVectorIndexStats) -> Self {
    VectorIndexStats {
      total_vectors: stats.total_vectors as i64,
      live_vectors: stats.live_vectors as i64,
      dimensions: stats.dimensions as i32,
      metric: stats.metric.into(),
      index_trained: stats.index_trained,
      index_clusters: stats.index_clusters.map(|v| v as i32),
    }
  }
}

fn map_vector_index_error(err: RustVectorIndexError) -> Error {
  Error::from_reason(err.to_string())
}

/// High-level vector index for similarity search
#[napi]
pub struct VectorIndex {
  inner: RwLock<RustVectorIndex>,
}

#[napi]
impl VectorIndex {
  /// Create a new vector index
  #[napi(constructor)]
  pub fn new(options: VectorIndexOptions) -> Result<Self> {
    let options = options.into_rust()?;
    Ok(VectorIndex {
      inner: RwLock::new(RustVectorIndex::new(options)),
    })
  }

  /// Set/update a vector for a node
  #[napi]
  pub fn set(&self, node_id: i64, vector: Vec<f64>) -> Result<()> {
    let mut index = self
      .inner
      .write()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    let vector_f32: Vec<f32> = vector.iter().map(|&v| v as f32).collect();
    index
      .set(node_id as u64, &vector_f32)
      .map_err(map_vector_index_error)
  }

  /// Get the vector for a node (if any)
  #[napi]
  pub fn get(&self, node_id: i64) -> Result<Option<Vec<f64>>> {
    let index = self
      .inner
      .read()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    Ok(
      index
        .get(node_id as u64)
        .map(|v| v.iter().map(|&x| x as f64).collect()),
    )
  }

  /// Delete the vector for a node
  #[napi]
  pub fn delete(&self, node_id: i64) -> Result<bool> {
    let mut index = self
      .inner
      .write()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    index.delete(node_id as u64).map_err(map_vector_index_error)
  }

  /// Check if a node has a vector
  #[napi]
  pub fn has(&self, node_id: i64) -> Result<bool> {
    let index = self
      .inner
      .read()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    Ok(index.has(node_id as u64))
  }

  /// Build/rebuild the IVF index for faster search
  #[napi]
  pub fn build_index(&self) -> Result<()> {
    let mut index = self
      .inner
      .write()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    index.build_index().map_err(map_vector_index_error)
  }

  /// Search for similar vectors
  #[napi]
  pub fn search(&self, query: Vec<f64>, options: SimilarOptions) -> Result<Vec<VectorSearchHit>> {
    let mut index = self
      .inner
      .write()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    let query_f32: Vec<f32> = query.iter().map(|&v| v as f32).collect();
    let options = options.into_rust()?;
    let hits = index
      .search(&query_f32, options)
      .map_err(map_vector_index_error)?;
    Ok(hits.into_iter().map(VectorSearchHit::from).collect())
  }

  /// Get index statistics
  #[napi]
  pub fn stats(&self) -> Result<VectorIndexStats> {
    let index = self
      .inner
      .read()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    Ok(VectorIndexStats::from(index.stats()))
  }

  /// Clear all vectors and reset the index
  #[napi]
  pub fn clear(&self) -> Result<()> {
    let mut index = self
      .inner
      .write()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    index.clear();
    Ok(())
  }
}

/// Create a new vector index
#[napi]
pub fn create_vector_index(options: VectorIndexOptions) -> Result<VectorIndex> {
  VectorIndex::new(options)
}
