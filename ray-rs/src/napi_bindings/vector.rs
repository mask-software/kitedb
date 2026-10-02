//! NAPI bindings for Vector Search
//!
//! Exposes IVF and IVF-PQ indexes to Node.js/Bun.

use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::borrow::Cow;
use std::sync::{Arc, Mutex, RwLock};

use crate::api::vector_search::{
  AnnAlgorithm as RustAnnAlgorithm, SimilarOptions as RustSimilarOptions,
  VectorIndex as RustVectorIndex, VectorIndexError as RustVectorIndexError,
  VectorIndexOptions as RustVectorIndexOptions, VectorIndexStats as RustVectorIndexStats,
  VectorSearchHit as RustVectorSearchHit,
};
use crate::napi_bindings::database::BlockingTask;
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
// Vector Input
// ============================================================================

/// A vector from JS as f32s: a Float32Array is read in place (one N-API
/// call), a number[] is converted element by element.
pub(crate) fn js_vector_f32<'a>(
  vector: &'a Either<Float32ArraySlice<'_>, Vec<f64>>,
) -> Cow<'a, [f32]> {
  match vector {
    Either::A(values) => Cow::Borrowed(values.as_ref()),
    Either::B(values) => Cow::Owned(values.iter().map(|&v| v as f32).collect()),
  }
}

/// The manifest a search last parsed, so repeated searches with the same
/// manifest JSON skip re-parsing it (the JSON holds every vector).
#[derive(Default)]
struct ManifestCache(Mutex<Option<(String, Arc<VectorManifest>)>>);

impl ManifestCache {
  fn get(&self, json: String) -> Result<Arc<VectorManifest>> {
    let mut cached = self
      .0
      .lock()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    if let Some((cached_json, manifest)) = cached.as_ref() {
      if *cached_json == json {
        return Ok(Arc::clone(manifest));
      }
    }
    let manifest: Arc<VectorManifest> = Arc::new(
      serde_json::from_str(&json)
        .map_err(|e| Error::from_reason(format!("Failed to parse manifest: {e}")))?,
    );
    *cached = Some((json, Arc::clone(&manifest)));
    Ok(manifest)
  }
}

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
  /// Training seed, an integer from 0 to Number.MAX_SAFE_INTEGER (default:
  /// a fresh seed per training). With a seed, training the same vectors in
  /// the same order builds the same index on any machine.
  pub seed: Option<f64>,
}

impl JsIvfConfig {
  fn into_rust(self) -> Result<RustIvfConfig> {
    let c = self;
    let mut config = RustIvfConfig::default();
    if let Some(seed) = c.seed {
      config.seed = Some(training_seed("seed", seed)?);
    }
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

/// A training seed passed as a JS number: an integer from 0 to
/// Number.MAX_SAFE_INTEGER (the range node ids use).
fn training_seed(field: &str, seed: f64) -> Result<u64> {
  validation::node_id(field, seed)
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
  inner: Arc<RwLock<RustIvfIndex>>,
  manifest: ManifestCache,
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
      inner: Arc::new(RwLock::new(RustIvfIndex::new(dimensions, rust_config))),
      manifest: ManifestCache::default(),
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
  pub fn add_training_vectors(
    &self,
    vectors: Either<Float32ArraySlice<'_>, Vec<f64>>,
    num_vectors: i32,
  ) -> Result<()> {
    let num_vectors =
      validation::non_negative_usize("numVectors", num_vectors as i64, validation::MAX_COUNT)?;
    let mut index = self
      .inner
      .write()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    let vectors_f32 = js_vector_f32(&vectors);
    index
      .add_training_vectors(&vectors_f32, num_vectors)
      .map_err(|e| Error::from_reason(format!("Failed to add training vectors: {e}")))
  }

  /// Train the index on added training vectors
  ///
  /// This runs k-means clustering to create the inverted file structure.
  #[napi(catch_unwind)]
  pub fn train(&self) -> Result<()> {
    train_ivf(&self.inner)
  }

  /// Train the index on the libuv thread pool. Other calls on this index
  /// wait until training finishes.
  #[napi(ts_return_type = "Promise<void>")]
  pub fn train_async(&self) -> AsyncTask<BlockingTask<()>> {
    let inner = Arc::clone(&self.inner);
    BlockingTask::spawn(Ok(move || train_ivf(&inner)))
  }

  /// Insert a vector into the index
  ///
  /// The index must be trained first.
  #[napi(catch_unwind)]
  pub fn insert(
    &self,
    vector_id: f64,
    vector: Either<Float32ArraySlice<'_>, Vec<f64>>,
  ) -> Result<()> {
    let vector_id = validation::node_id("vectorId", vector_id)?;
    let mut index = self
      .inner
      .write()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    let vector_f32 = js_vector_f32(&vector);
    index
      .insert(vector_id, &vector_f32)
      .map_err(|e| Error::from_reason(format!("Failed to insert vector: {e}")))
  }

  /// Delete a vector from the index
  ///
  /// Requires the vector data to determine which cluster to remove from.
  #[napi(catch_unwind)]
  pub fn delete(
    &self,
    vector_id: f64,
    vector: Either<Float32ArraySlice<'_>, Vec<f64>>,
  ) -> Result<bool> {
    let vector_id = validation::node_id("vectorId", vector_id)?;
    let mut index = self
      .inner
      .write()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    let vector_f32 = js_vector_f32(&vector);
    validation::vector_len("vector", vector_f32.len(), index.dimensions)?;
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
    query: Either<Float32ArraySlice<'_>, Vec<f64>>,
    k: i32,
    options: Option<JsSearchOptions>,
  ) -> Result<Vec<JsSearchResult>> {
    let index = self
      .inner
      .read()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    let query_f32 = js_vector_f32(&query);
    validation::vector_len("query", query_f32.len(), index.dimensions)?;

    let manifest = self.manifest.get(manifest_json)?;
    validation::vector_len("manifest", manifest.config.dimensions, index.dimensions)?;

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
    queries: Vec<Either<Float32ArraySlice<'_>, Vec<f64>>>,
    k: i32,
    aggregation: JsAggregation,
    options: Option<JsSearchOptions>,
  ) -> Result<Vec<JsSearchResult>> {
    let index = self
      .inner
      .read()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    let queries_f32: Vec<Cow<'_, [f32]>> = queries.iter().map(js_vector_f32).collect();
    for (i, query) in queries_f32.iter().enumerate() {
      validation::vector_len(format_args!("queries[{i}]"), query.len(), index.dimensions)?;
    }

    let manifest = self.manifest.get(manifest_json)?;
    validation::vector_len("manifest", manifest.config.dimensions, index.dimensions)?;

    let query_refs: Vec<&[f32]> = queries_f32.iter().map(|q| q.as_ref()).collect();

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
      inner: Arc::new(RwLock::new(index)),
      manifest: ManifestCache::default(),
    })
  }
}

fn train_ivf(inner: &RwLock<RustIvfIndex>) -> Result<()> {
  let mut index = inner
    .write()
    .map_err(|e| Error::from_reason(e.to_string()))?;
  index
    .train()
    .map_err(|e| Error::from_reason(format!("Failed to train index: {e}")))
}

fn train_ivf_pq(inner: &RwLock<RustIvfPqIndex>) -> Result<()> {
  let mut index = inner
    .write()
    .map_err(|e| Error::from_reason(e.to_string()))?;
  index
    .train()
    .map_err(|e| Error::from_reason(format!("Failed to train index: {e}")))
}

// ============================================================================
// IVF-PQ Index NAPI Wrapper
// ============================================================================

/// IVF-PQ combined index for memory-efficient approximate nearest neighbor search
#[napi]
pub struct JsIvfPqIndex {
  inner: Arc<RwLock<RustIvfPqIndex>>,
  manifest: ManifestCache,
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
      inner: Arc::new(RwLock::new(index)),
      manifest: ManifestCache::default(),
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
  pub fn add_training_vectors(
    &self,
    vectors: Either<Float32ArraySlice<'_>, Vec<f64>>,
    num_vectors: i32,
  ) -> Result<()> {
    let num_vectors =
      validation::non_negative_usize("numVectors", num_vectors as i64, validation::MAX_COUNT)?;
    let mut index = self
      .inner
      .write()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    let vectors_f32 = js_vector_f32(&vectors);
    index
      .add_training_vectors(&vectors_f32, num_vectors)
      .map_err(|e| Error::from_reason(format!("Failed to add training vectors: {e}")))
  }

  /// Train the index
  #[napi(catch_unwind)]
  pub fn train(&self) -> Result<()> {
    train_ivf_pq(&self.inner)
  }

  /// Train the index on the libuv thread pool. Other calls on this index
  /// wait until training finishes.
  #[napi(ts_return_type = "Promise<void>")]
  pub fn train_async(&self) -> AsyncTask<BlockingTask<()>> {
    let inner = Arc::clone(&self.inner);
    BlockingTask::spawn(Ok(move || train_ivf_pq(&inner)))
  }

  /// Insert a vector
  #[napi(catch_unwind)]
  pub fn insert(
    &self,
    vector_id: f64,
    vector: Either<Float32ArraySlice<'_>, Vec<f64>>,
  ) -> Result<()> {
    let vector_id = validation::node_id("vectorId", vector_id)?;
    let mut index = self
      .inner
      .write()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    let vector_f32 = js_vector_f32(&vector);
    index
      .insert(vector_id, &vector_f32)
      .map_err(|e| Error::from_reason(format!("Failed to insert vector: {e}")))
  }

  /// Delete a vector
  ///
  /// Requires the vector data to determine which cluster to remove from.
  #[napi(catch_unwind)]
  pub fn delete(
    &self,
    vector_id: f64,
    vector: Either<Float32ArraySlice<'_>, Vec<f64>>,
  ) -> Result<bool> {
    let vector_id = validation::node_id("vectorId", vector_id)?;
    let mut index = self
      .inner
      .write()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    let vector_f32 = js_vector_f32(&vector);
    validation::vector_len("vector", vector_f32.len(), index.dimensions)?;
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
    query: Either<Float32ArraySlice<'_>, Vec<f64>>,
    k: i32,
    options: Option<JsSearchOptions>,
  ) -> Result<Vec<JsSearchResult>> {
    let index = self
      .inner
      .read()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    let query_f32 = js_vector_f32(&query);
    validation::vector_len("query", query_f32.len(), index.dimensions)?;

    let manifest = self.manifest.get(manifest_json)?;

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
    queries: Vec<Either<Float32ArraySlice<'_>, Vec<f64>>>,
    k: i32,
    aggregation: JsAggregation,
    options: Option<JsSearchOptions>,
  ) -> Result<Vec<JsSearchResult>> {
    let index = self
      .inner
      .read()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    let queries_f32: Vec<Cow<'_, [f32]>> = queries.iter().map(js_vector_f32).collect();
    for (i, query) in queries_f32.iter().enumerate() {
      validation::vector_len(format_args!("queries[{i}]"), query.len(), index.dimensions)?;
    }

    let manifest = self.manifest.get(manifest_json)?;

    let query_refs: Vec<&[f32]> = queries_f32.iter().map(|q| q.as_ref()).collect();

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
      inner: Arc::new(RwLock::new(index)),
      manifest: ManifestCache::default(),
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
fn search_vector(
  field: impl std::fmt::Display,
  values: &Either<Float32ArraySlice<'_>, Vec<f64>>,
  cosine: bool,
) -> Result<Vec<f32>> {
  let mut vector = js_vector_f32(values).into_owned();
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
  vectors: Vec<Either<Float32ArraySlice<'_>, Vec<f64>>>,
  node_ids: Vec<f64>,
  query: Either<Float32ArraySlice<'_>, Vec<f64>>,
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
    let v_f32 = search_vector(format_args!("vectors[{i}]"), v, cosine)?;
    validation::vector_len(format_args!("vectors[{i}]"), v_f32.len(), query_f32.len())?;
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

/// ANN backend for `VectorIndex`
#[napi(string_enum)]
#[derive(Debug, PartialEq, Eq)]
pub enum JsAnnAlgorithm {
  /// Plain IVF: exact distances over the probed clusters
  #[napi(value = "ivf")]
  Ivf,
  /// IVF-PQ: PQ-ranked candidates re-ranked by exact distance
  #[napi(value = "ivf_pq")]
  IvfPq,
  /// Plain IVF while the index is small or under 512 dimensions, IVF-PQ from
  /// 512 dimensions and 50,000 vectors on (the default)
  #[napi(value = "auto")]
  Auto,
}

impl From<JsAnnAlgorithm> for RustAnnAlgorithm {
  fn from(algorithm: JsAnnAlgorithm) -> Self {
    match algorithm {
      JsAnnAlgorithm::Ivf => RustAnnAlgorithm::Ivf,
      JsAnnAlgorithm::IvfPq => RustAnnAlgorithm::IvfPq,
      JsAnnAlgorithm::Auto => RustAnnAlgorithm::Auto,
    }
  }
}

impl From<RustAnnAlgorithm> for JsAnnAlgorithm {
  fn from(algorithm: RustAnnAlgorithm) -> Self {
    match algorithm {
      RustAnnAlgorithm::Ivf => JsAnnAlgorithm::Ivf,
      RustAnnAlgorithm::IvfPq => JsAnnAlgorithm::IvfPq,
      RustAnnAlgorithm::Auto => JsAnnAlgorithm::Auto,
    }
  }
}

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
  /// @deprecated No effect: `VectorIndex` keeps no node cache. Still accepted so existing callers keep working.
  pub cache_max_size: Option<i32>,
  /// ANN backend (default: 'auto': plain IVF while the index is small or
  /// under 512 dimensions, IVF-PQ from 512 dimensions and 50,000 vectors on;
  /// decided at each build)
  pub ann_algorithm: Option<JsAnnAlgorithm>,
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
        seed,
      } = ivf;
      JsIvfConfig {
        n_clusters,
        n_probe,
        metric,
        seed: None,
      }
      .into_rust()?;
      if let Some(seed) = seed {
        options = options.with_seed(training_seed("ivf.seed", seed)?);
      }
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

    if let Some(normalize) = self.normalize {
      options = options.with_normalize(normalize);
    }

    if let Some(algorithm) = self.ann_algorithm {
      options = options.with_ann_algorithm(algorithm.into());
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
  /// Backend of the built ANN index ('ivf' or 'ivf_pq'; absent before one is
  /// built)
  pub index_algorithm: Option<JsAnnAlgorithm>,
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
      index_algorithm: stats.index_algorithm.map(JsAnnAlgorithm::from),
    }
  }
}

fn map_vector_index_error(err: RustVectorIndexError) -> Error {
  Error::from_reason(err.to_string())
}

/// High-level vector index for similarity search
#[napi]
pub struct VectorIndex {
  // A Mutex, not an RwLock: the index is Send but not Sync (its LRU cache),
  // and `buildIndexAsync` shares it with a pool thread.
  inner: Arc<Mutex<RustVectorIndex>>,
}

#[napi]
impl VectorIndex {
  /// Create a new vector index
  #[napi(constructor)]
  pub fn new(options: VectorIndexOptions) -> Result<Self> {
    let options = options.into_rust()?;
    Ok(VectorIndex {
      inner: Arc::new(Mutex::new(RustVectorIndex::new(options))),
    })
  }

  /// Set/update a vector for a node
  ///
  /// A Float32Array is read in place; a number[] is converted to f32.
  #[napi(catch_unwind)]
  pub fn set(&self, node_id: f64, vector: Either<Float32ArraySlice<'_>, Vec<f64>>) -> Result<()> {
    let node_id = validation::node_id("nodeId", node_id)?;
    let mut index = self
      .inner
      .lock()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    let vector_f32 = js_vector_f32(&vector);
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
      .lock()
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
      .lock()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    index.delete(node_id).map_err(map_vector_index_error)
  }

  /// Check if a node has a vector
  #[napi]
  pub fn has(&self, node_id: f64) -> Result<bool> {
    let node_id = validation::node_id("nodeId", node_id)?;
    let index = self
      .inner
      .lock()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    Ok(index.has(node_id))
  }

  /// Build/rebuild the IVF index for faster search
  #[napi(catch_unwind)]
  pub fn build_index(&self) -> Result<()> {
    build_vector_index(&self.inner)
  }

  /// Build/rebuild the IVF index on the libuv thread pool. Other calls on
  /// this index wait until the build finishes.
  #[napi(ts_return_type = "Promise<void>")]
  pub fn build_index_async(&self) -> AsyncTask<BlockingTask<()>> {
    let inner = Arc::clone(&self.inner);
    BlockingTask::spawn(Ok(move || build_vector_index(&inner)))
  }

  /// Search for similar vectors
  #[napi(catch_unwind)]
  pub fn search(
    &self,
    query: Either<Float32ArraySlice<'_>, Vec<f64>>,
    options: SimilarOptions,
  ) -> Result<Vec<VectorSearchHit>> {
    let mut index = self
      .inner
      .lock()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    let query_f32 = js_vector_f32(&query);
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
      .lock()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    Ok(VectorIndexStats::from(index.stats()))
  }

  /// Clear all vectors and reset the index
  #[napi]
  pub fn clear(&self) -> Result<()> {
    let mut index = self
      .inner
      .lock()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    index.clear();
    Ok(())
  }
}

fn build_vector_index(inner: &Mutex<RustVectorIndex>) -> Result<()> {
  let mut index = inner
    .lock()
    .map_err(|e| Error::from_reason(e.to_string()))?;
  index.build_index().map_err(map_vector_index_error)
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
      seed: Some(0.0),
    }
    .into_rust()
    .is_ok());
    let seeded = JsIvfConfig {
      seed: Some(validation::MAX_SAFE_INTEGER),
      ..Default::default()
    }
    .into_rust()
    .expect("valid seed");
    assert_eq!(seeded.seed, Some(validation::MAX_SAFE_INTEGER as u64));
    for seed in [-1.0, 0.5, f64::NAN, validation::MAX_SAFE_INTEGER + 2.0] {
      assert!(JsIvfConfig {
        seed: Some(seed),
        ..Default::default()
      }
      .into_rust()
      .is_err());
    }
    for value in [0, -1, (validation::MAX_VECTOR_PARAM + 1) as i32] {
      assert!(JsIvfConfig {
        n_clusters: Some(value),
        n_probe: None,
        metric: None,
        seed: None,
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
  fn vector_index_options_carry_the_ivf_seed() {
    let options = |seed| VectorIndexOptions {
      dimensions: 4,
      metric: None,
      row_group_size: None,
      fragment_target_size: None,
      normalize: None,
      ivf: Some(JsIvfConfig {
        seed,
        ..Default::default()
      }),
      training_threshold: None,
      cache_max_size: None,
      ann_algorithm: None,
    };
    let seeded = options(Some(42.0)).into_rust().expect("valid options");
    assert_eq!(seeded.seed, Some(42));
    assert_eq!(options(None).into_rust().expect("valid").seed, None);
    assert!(options(Some(-3.0)).into_rust().is_err());
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
      ann_algorithm: Some(JsAnnAlgorithm::IvfPq),
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
      ann_algorithm: None,
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
