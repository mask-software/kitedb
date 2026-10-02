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
use crate::napi_bindings::validation;
use crate::vector::distance::l2_norm;
use crate::vector::top_k::TopK;
use crate::vector::{
  DistanceMetric as RustDistanceMetric, IvfConfig as RustIvfConfig, IvfIndex as RustIvfIndex,
  IvfPqConfig as RustIvfPqConfig, IvfPqIndex as RustIvfPqIndex,
  IvfPqSearchOptions as RustIvfPqSearchOptions, MultiQueryAggregation, PqConfig as RustPqConfig,
  SearchOptions as RustSearchOptions, VectorManifest, VectorSearchResult,
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

impl JsIvfConfig {
  fn into_rust(self) -> Result<RustIvfConfig> {
    let c = self;
    let mut config = RustIvfConfig::default();
    if let Some(n) = c.n_clusters {
      config.n_clusters =
        validation::positive_usize("nClusters", n as i64, validation::MAX_VECTOR_PARAM)?;
    }
    if let Some(n) = c.n_probe {
      config.n_probe =
        validation::positive_usize("nProbe", n as i64, validation::MAX_VECTOR_PARAM)?;
    }
    if let Some(m) = c.metric {
      config.metric = m.into();
    }
    Ok(config)
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

impl JsPqConfig {
  fn into_rust(self) -> Result<RustPqConfig> {
    let c = self;
    let mut config = RustPqConfig::default();
    if let Some(n) = c.num_subspaces {
      config.num_subspaces =
        validation::positive_usize("numSubspaces", n as i64, validation::MAX_VECTOR_PARAM)?;
    }
    if let Some(n) = c.num_centroids {
      config.num_centroids =
        validation::positive_usize("numCentroids", n as i64, validation::MAX_VECTOR_PARAM)?;
    }
    if let Some(n) = c.max_iterations {
      config.max_iterations =
        validation::positive_usize("maxIterations", n as i64, validation::MAX_VECTOR_PARAM)?;
    }
    Ok(config)
  }
}

// ============================================================================
// Search Options
// ============================================================================

/// Options for vector search
#[napi(object)]
#[derive(Debug, Default)]
pub struct JsSearchOptions {
  /// Number of clusters to probe (overrides index default; must be positive)
  pub n_probe: Option<i32>,
  /// Minimum similarity threshold (0-1)
  pub threshold: Option<f64>,
  /// IVF-PQ only: re-rank the best `max(k * rerankFactor, 80)` PQ candidates
  /// by exact distance (default 4; 0 returns the approximate PQ ranking and
  /// distances). IVF search is exact and ignores it.
  pub rerank_factor: Option<i32>,
}

/// Validated `JsSearchOptions`.
struct SearchParams {
  n_probe: Option<usize>,
  threshold: Option<f32>,
  rerank_factor: Option<usize>,
}

impl SearchParams {
  fn ivf(self) -> RustSearchOptions {
    RustSearchOptions {
      n_probe: self.n_probe,
      filter: None,
      threshold: self.threshold,
    }
  }

  fn ivf_pq(self) -> RustIvfPqSearchOptions {
    RustIvfPqSearchOptions {
      n_probe: self.n_probe,
      filter: None,
      threshold: self.threshold,
      rerank_factor: self.rerank_factor,
    }
  }
}

impl JsSearchOptions {
  fn validated(&self) -> Result<SearchParams> {
    let n_probe = self
      .n_probe
      .map(|n| validation::positive_usize("nProbe", n as i64, validation::MAX_VECTOR_PARAM))
      .transpose()?;
    let threshold = self
      .threshold
      .map(|value| validation::ratio("threshold", value).map(|value| value as f32))
      .transpose()?;
    let rerank_factor = self
      .rerank_factor
      .map(|factor| validate_rerank_factor("rerankFactor", factor))
      .transpose()?;
    Ok(SearchParams {
      n_probe,
      threshold,
      rerank_factor,
    })
  }
}

fn validate_rerank_factor(field: &str, factor: i32) -> Result<usize> {
  validation::non_negative_usize(field, factor as i64, validation::MAX_VECTOR_PARAM)
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
    let dimensions = validation::positive_usize(
      "dimensions",
      dimensions as i64,
      validation::MAX_VECTOR_DIMENSIONS,
    )?;
    let rust_config = config.unwrap_or_default().into_rust()?;
    Ok(JsIvfIndex {
      inner: RwLock::new(RustIvfIndex::new(dimensions, rust_config)),
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
  #[napi(catch_unwind)]
  pub fn add_training_vectors(&self, vectors: Vec<f64>, num_vectors: i32) -> Result<()> {
    let num_vectors =
      validation::non_negative_usize("numVectors", num_vectors as i64, validation::MAX_COUNT)?;
    let mut index = self
      .inner
      .write()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    let vectors_f32: Vec<f32> = vectors.iter().map(|&v| v as f32).collect();
    index
      .add_training_vectors(&vectors_f32, num_vectors)
      .map_err(|e| Error::from_reason(format!("Failed to add training vectors: {e}")))
  }

  /// Train the index on added training vectors
  ///
  /// This runs k-means clustering to create the inverted file structure.
  #[napi(catch_unwind)]
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
  #[napi(catch_unwind)]
  pub fn insert(&self, vector_id: f64, vector: Vec<f64>) -> Result<()> {
    let vector_id = validation::node_id("vectorId", vector_id)?;
    let mut index = self
      .inner
      .write()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    let vector_f32: Vec<f32> = vector.iter().map(|&v| v as f32).collect();
    index
      .insert(vector_id, &vector_f32)
      .map_err(|e| Error::from_reason(format!("Failed to insert vector: {e}")))
  }

  /// Delete a vector from the index
  ///
  /// Requires the vector data to determine which cluster to remove from.
  #[napi(catch_unwind)]
  pub fn delete(&self, vector_id: f64, vector: Vec<f64>) -> Result<bool> {
    let vector_id = validation::node_id("vectorId", vector_id)?;
    let mut index = self
      .inner
      .write()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    validation::vector_len("vector", vector.len(), index.dimensions)?;
    let vector_f32: Vec<f32> = vector.iter().map(|&v| v as f32).collect();
    index
      .delete(vector_id, &vector_f32)
      .map_err(|e| Error::from_reason(format!("Failed to delete vector: {e}")))
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
  #[napi(catch_unwind)]
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
    validation::vector_len("query", query.len(), index.dimensions)?;

    // Parse manifest from JSON
    let manifest: VectorManifest = serde_json::from_str(&manifest_json)
      .map_err(|e| Error::from_reason(format!("Failed to parse manifest: {e}")))?;
    validation::vector_len("manifest", manifest.config.dimensions, index.dimensions)?;

    let query_f32: Vec<f32> = query.iter().map(|&v| v as f32).collect();

    let rust_options = options
      .as_ref()
      .map(JsSearchOptions::validated)
      .transpose()?
      .map(SearchParams::ivf);

    let k = validation::non_negative_usize("k", k as i64, validation::MAX_COUNT)?;
    let results = index
      .search(&manifest, &query_f32, k, rust_options)
      .map_err(|e| Error::from_reason(e.to_string()))?;
    Ok(results.into_iter().map(|r| r.into()).collect())
  }

  /// Search with multiple query vectors
  ///
  /// Aggregates results using the specified method.
  #[napi(catch_unwind)]
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
    for (i, query) in queries.iter().enumerate() {
      validation::vector_len(format_args!("queries[{i}]"), query.len(), index.dimensions)?;
    }

    // Parse manifest from JSON
    let manifest: VectorManifest = serde_json::from_str(&manifest_json)
      .map_err(|e| Error::from_reason(format!("Failed to parse manifest: {e}")))?;
    validation::vector_len("manifest", manifest.config.dimensions, index.dimensions)?;

    let queries_f32: Vec<Vec<f32>> = queries
      .iter()
      .map(|q| q.iter().map(|&v| v as f32).collect())
      .collect();

    let query_refs: Vec<&[f32]> = queries_f32.iter().map(|q| q.as_slice()).collect();

    let rust_options = options
      .as_ref()
      .map(JsSearchOptions::validated)
      .transpose()?
      .map(SearchParams::ivf);

    let k = validation::non_negative_usize("k", k as i64, validation::MAX_COUNT)?;
    let results = index
      .search_multi(&manifest, &query_refs, k, aggregation.into(), rust_options)
      .map_err(|e| Error::from_reason(e.to_string()))?;
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
  #[napi(factory, catch_unwind)]
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
    let dimensions = validation::positive_usize(
      "dimensions",
      dimensions as i64,
      validation::MAX_VECTOR_DIMENSIONS,
    )?;
    let config = RustIvfPqConfig {
      ivf: ivf_config.unwrap_or_default().into_rust()?,
      pq: pq_config.unwrap_or_default().into_rust()?,
      use_residuals: use_residuals.unwrap_or(true),
    };

    let index = RustIvfPqIndex::new(dimensions, config)
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
  #[napi(catch_unwind)]
  pub fn add_training_vectors(&self, vectors: Vec<f64>, num_vectors: i32) -> Result<()> {
    let num_vectors =
      validation::non_negative_usize("numVectors", num_vectors as i64, validation::MAX_COUNT)?;
    let mut index = self
      .inner
      .write()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    let vectors_f32: Vec<f32> = vectors.iter().map(|&v| v as f32).collect();
    index
      .add_training_vectors(&vectors_f32, num_vectors)
      .map_err(|e| Error::from_reason(format!("Failed to add training vectors: {e}")))
  }

  /// Train the index
  #[napi(catch_unwind)]
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
  #[napi(catch_unwind)]
  pub fn insert(&self, vector_id: f64, vector: Vec<f64>) -> Result<()> {
    let vector_id = validation::node_id("vectorId", vector_id)?;
    let mut index = self
      .inner
      .write()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    let vector_f32: Vec<f32> = vector.iter().map(|&v| v as f32).collect();
    index
      .insert(vector_id, &vector_f32)
      .map_err(|e| Error::from_reason(format!("Failed to insert vector: {e}")))
  }

  /// Delete a vector
  ///
  /// Requires the vector data to determine which cluster to remove from.
  #[napi(catch_unwind)]
  pub fn delete(&self, vector_id: f64, vector: Vec<f64>) -> Result<bool> {
    let vector_id = validation::node_id("vectorId", vector_id)?;
    let mut index = self
      .inner
      .write()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    validation::vector_len("vector", vector.len(), index.dimensions)?;
    let vector_f32: Vec<f32> = vector.iter().map(|&v| v as f32).collect();
    index
      .delete(vector_id, &vector_f32)
      .map_err(|e| Error::from_reason(format!("Failed to delete vector: {e}")))
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
  #[napi(catch_unwind)]
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
    validation::vector_len("query", query.len(), index.dimensions)?;

    // Parse manifest from JSON
    let manifest: VectorManifest = serde_json::from_str(&manifest_json)
      .map_err(|e| Error::from_reason(format!("Failed to parse manifest: {e}")))?;

    let query_f32: Vec<f32> = query.iter().map(|&v| v as f32).collect();

    let rust_options = options
      .as_ref()
      .map(JsSearchOptions::validated)
      .transpose()?
      .map(SearchParams::ivf_pq);

    let k = validation::non_negative_usize("k", k as i64, validation::MAX_COUNT)?;
    let results = index
      .search(&manifest, &query_f32, k, rust_options)
      .map_err(|e| Error::from_reason(e.to_string()))?;
    Ok(results.into_iter().map(|r| r.into()).collect())
  }

  /// Search with multiple query vectors
  #[napi(catch_unwind)]
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
    for (i, query) in queries.iter().enumerate() {
      validation::vector_len(format_args!("queries[{i}]"), query.len(), index.dimensions)?;
    }

    // Parse manifest from JSON
    let manifest: VectorManifest = serde_json::from_str(&manifest_json)
      .map_err(|e| Error::from_reason(format!("Failed to parse manifest: {e}")))?;

    let queries_f32: Vec<Vec<f32>> = queries
      .iter()
      .map(|q| q.iter().map(|&v| v as f32).collect())
      .collect();

    let query_refs: Vec<&[f32]> = queries_f32.iter().map(|q| q.as_slice()).collect();

    let rust_options = options
      .as_ref()
      .map(JsSearchOptions::validated)
      .transpose()?
      .map(SearchParams::ivf_pq);

    let k = validation::non_negative_usize("k", k as i64, validation::MAX_COUNT)?;
    let results = index
      .search_multi(&manifest, &query_refs, k, aggregation.into(), rust_options)
      .map_err(|e| Error::from_reason(e.to_string()))?;
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
  #[napi(factory, catch_unwind)]
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

/// Convert a JS vector for brute-force search.
///
/// Core cosine distance is `1 - dot` and assumes unit vectors, so cosine
/// inputs are normalized here; a zero vector has no direction and is rejected.
fn search_vector(field: impl std::fmt::Display, values: &[f64], cosine: bool) -> Result<Vec<f32>> {
  let mut vector: Vec<f32> = values.iter().map(|&v| v as f32).collect();
  if cosine {
    let norm = l2_norm(&vector);
    if norm <= 0.0 {
      return Err(validation::invalid_argument(format!(
        "{field} is a zero vector; cosine distance needs a non-zero vector"
      )));
    }
    vector.iter_mut().for_each(|v| *v /= norm);
  }
  Ok(vector)
}

/// Perform brute-force search over all vectors
///
/// Useful for small datasets or verifying IVF results.
#[napi(catch_unwind)]
pub fn brute_force_search(
  vectors: Vec<Vec<f64>>,
  node_ids: Vec<f64>,
  query: Vec<f64>,
  k: i32,
  metric: Option<JsDistanceMetric>,
) -> Result<Vec<JsBruteForceResult>> {
  if vectors.len() != node_ids.len() {
    return Err(Error::from_reason(
      "vectors and node_ids must have same length",
    ));
  }
  let node_ids = validation::node_ids("nodeIds", &node_ids)?;

  let metric = metric.unwrap_or(JsDistanceMetric::Cosine);
  let rust_metric: RustDistanceMetric = metric.into();
  let distance_fn = rust_metric.distance_fn();
  let cosine = rust_metric == RustDistanceMetric::Cosine;

  let query_f32 = search_vector("query", &query, cosine)?;
  let k = validation::non_negative_usize("k", k as i64, validation::MAX_COUNT)?;

  // Bounded top-k in a total order; NaN distances (from NaN components)
  // never enter it.
  let mut top = TopK::new(k);
  for (i, (v, &node_id)) in vectors.iter().zip(node_ids.iter()).enumerate() {
    validation::vector_len(format_args!("vectors[{i}]"), v.len(), query.len())?;
    let v_f32 = search_vector(format_args!("vectors[{i}]"), v, cosine)?;
    top.push(node_id as i64, distance_fn(&query_f32, &v_f32));
  }

  Ok(
    top
      .into_sorted_vec()
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
  /// Maximum node IDs to cache for search results (0 disables this cache)
  pub cache_max_size: Option<i32>,
}

impl VectorIndexOptions {
  fn into_rust(self) -> Result<RustVectorIndexOptions> {
    let dimensions = validation::positive_usize(
      "dimensions",
      self.dimensions as i64,
      validation::MAX_VECTOR_DIMENSIONS,
    )?;
    let mut options = RustVectorIndexOptions::new(dimensions);

    if let Some(metric) = self.metric {
      options = options.with_metric(metric.into());
    }

    if let Some(row_group_size) = self.row_group_size {
      let row_group_size = validation::positive_usize(
        "rowGroupSize",
        row_group_size as i64,
        validation::MAX_VECTOR_PARAM,
      )?;
      options = options.with_row_group_size(row_group_size);
    }

    if let Some(fragment_target_size) = self.fragment_target_size {
      let fragment_target_size = validation::positive_usize(
        "fragmentTargetSize",
        fragment_target_size as i64,
        validation::MAX_VECTOR_PARAM,
      )?;
      options = options.with_fragment_target_size(fragment_target_size);
    }

    if let Some(ivf) = self.ivf {
      let JsIvfConfig {
        n_clusters,
        n_probe,
        metric,
      } = ivf;
      JsIvfConfig {
        n_clusters,
        n_probe,
        metric,
      }
      .into_rust()?;
      if let Some(n_clusters) = n_clusters {
        options = options.with_n_clusters(validation::positive_usize(
          "ivf.nClusters",
          n_clusters as i64,
          validation::MAX_VECTOR_PARAM,
        )?);
      }
      if let Some(n_probe) = n_probe {
        options = options.with_n_probe(validation::positive_usize(
          "ivf.nProbe",
          n_probe as i64,
          validation::MAX_VECTOR_PARAM,
        )?);
      }
    }

    if let Some(training_threshold) = self.training_threshold {
      let training_threshold = validation::positive_usize(
        "trainingThreshold",
        training_threshold as i64,
        validation::MAX_COUNT,
      )?;
      options = options.with_training_threshold(training_threshold);
    }

    if let Some(cache_max_size) = self.cache_max_size {
      let cache_max_size = validation::non_negative_usize(
        "cacheMaxSize",
        cache_max_size as i64,
        validation::MAX_CACHE_ENTRIES,
      )?;
      options = options.with_cache_max_size(cache_max_size);
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
  /// Number of results to return (0 returns an empty result)
  pub k: i32,
  /// Minimum similarity threshold (0-1 for cosine)
  pub threshold: Option<f64>,
  /// Number of clusters to probe for IVF (must be positive)
  pub n_probe: Option<i32>,
  /// Re-rank the best `max(k * rerankFactor, 80)` IVF-PQ candidates by exact
  /// distance (default 4; 0 returns the approximate PQ ranking and distances)
  pub rerank_factor: Option<i32>,
}

impl SimilarOptions {
  fn into_rust(self) -> Result<RustSimilarOptions> {
    let k = validation::non_negative_usize("k", self.k as i64, validation::MAX_COUNT)?;
    let mut options = RustSimilarOptions::new(k);
    if let Some(threshold) = self.threshold {
      options = options.with_threshold(validation::ratio("threshold", threshold)? as f32);
    }
    if let Some(n_probe) = self.n_probe {
      let n_probe =
        validation::positive_usize("nProbe", n_probe as i64, validation::MAX_VECTOR_PARAM)?;
      options = options.with_n_probe(n_probe);
    }
    if let Some(factor) = self.rerank_factor {
      options = options.with_rerank_factor(validate_rerank_factor("rerankFactor", factor)?);
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
  #[napi(catch_unwind)]
  pub fn set(&self, node_id: f64, vector: Vec<f64>) -> Result<()> {
    let node_id = validation::node_id("nodeId", node_id)?;
    let mut index = self
      .inner
      .write()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    let vector_f32: Vec<f32> = vector.iter().map(|&v| v as f32).collect();
    index
      .set(node_id, &vector_f32)
      .map_err(map_vector_index_error)
  }

  /// Get the vector for a node (if any)
  #[napi]
  pub fn get(&self, node_id: f64) -> Result<Option<Vec<f64>>> {
    let node_id = validation::node_id("nodeId", node_id)?;
    let index = self
      .inner
      .read()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    Ok(
      index
        .get(node_id)
        .map(|v| v.iter().map(|&x| x as f64).collect()),
    )
  }

  /// Delete the vector for a node
  #[napi]
  pub fn delete(&self, node_id: f64) -> Result<bool> {
    let node_id = validation::node_id("nodeId", node_id)?;
    let mut index = self
      .inner
      .write()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    index.delete(node_id).map_err(map_vector_index_error)
  }

  /// Check if a node has a vector
  #[napi]
  pub fn has(&self, node_id: f64) -> Result<bool> {
    let node_id = validation::node_id("nodeId", node_id)?;
    let index = self
      .inner
      .read()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    Ok(index.has(node_id))
  }

  /// Build/rebuild the IVF index for faster search
  #[napi(catch_unwind)]
  pub fn build_index(&self) -> Result<()> {
    let mut index = self
      .inner
      .write()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    index.build_index().map_err(map_vector_index_error)
  }

  /// Search for similar vectors
  #[napi(catch_unwind)]
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn validates_ivf_pq_and_search_options() {
    assert!(JsIvfConfig {
      n_clusters: Some(1),
      n_probe: Some(1),
      metric: None,
    }
    .into_rust()
    .is_ok());
    for value in [0, -1, (validation::MAX_VECTOR_PARAM + 1) as i32] {
      assert!(JsIvfConfig {
        n_clusters: Some(value),
        n_probe: None,
        metric: None,
      }
      .into_rust()
      .is_err());
    }
    assert!(JsPqConfig {
      num_subspaces: Some(1),
      num_centroids: Some(1),
      max_iterations: Some(1),
    }
    .into_rust()
    .is_ok());
    assert!(JsPqConfig {
      num_subspaces: Some(0),
      ..Default::default()
    }
    .into_rust()
    .is_err());
    assert!(JsSearchOptions {
      n_probe: Some(1),
      threshold: Some(0.0),
      rerank_factor: Some(0),
    }
    .validated()
    .is_ok());
    assert!(JsSearchOptions {
      n_probe: Some(0),
      ..Default::default()
    }
    .validated()
    .is_err());
    assert!(JsSearchOptions {
      threshold: Some(2.0),
      ..Default::default()
    }
    .validated()
    .is_err());
    for factor in [-1, (validation::MAX_VECTOR_PARAM + 1) as i32] {
      assert!(JsSearchOptions {
        rerank_factor: Some(factor),
        ..Default::default()
      }
      .validated()
      .is_err());
      assert!(SimilarOptions {
        k: 1,
        threshold: None,
        n_probe: None,
        rerank_factor: Some(factor),
      }
      .into_rust()
      .is_err());
    }
    let params = JsSearchOptions {
      rerank_factor: Some(3),
      ..Default::default()
    }
    .validated()
    .expect("valid options");
    assert_eq!(params.ivf_pq().rerank_factor, Some(3));
  }

  #[test]
  fn validates_index_dimensions_counts_and_zero_result_limits() {
    assert!(JsIvfIndex::new(1, None).is_ok());
    assert!(JsIvfIndex::new(0, None).is_err());
    assert!(JsIvfIndex::new(-1, None).is_err());
    assert!(JsIvfIndex::new((validation::MAX_VECTOR_DIMENSIONS + 1) as i32, None).is_err());
    assert!(VectorIndexOptions {
      dimensions: 1,
      metric: None,
      row_group_size: Some(1),
      fragment_target_size: Some(1),
      normalize: None,
      ivf: None,
      training_threshold: Some(1),
      cache_max_size: Some(0),
    }
    .into_rust()
    .is_ok());
    assert!(VectorIndexOptions {
      dimensions: 1,
      metric: None,
      row_group_size: Some(0),
      fragment_target_size: None,
      normalize: None,
      ivf: None,
      training_threshold: None,
      cache_max_size: None,
    }
    .into_rust()
    .is_err());
    assert!(SimilarOptions {
      k: 0,
      threshold: Some(0.0),
      n_probe: None,
      rerank_factor: Some(0),
    }
    .into_rust()
    .is_ok());
    assert!(SimilarOptions {
      k: -1,
      threshold: None,
      n_probe: None,
      rerank_factor: None,
    }
    .into_rust()
    .is_err());
  }
}
