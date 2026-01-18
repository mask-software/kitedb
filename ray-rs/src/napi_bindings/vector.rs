//! NAPI bindings for Vector Search
//!
//! Exposes IVF and IVF-PQ indexes to Node.js/Bun.

use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::sync::RwLock;

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
    let index = self.inner.read().map_err(|e| Error::from_reason(e.to_string()))?;
    Ok(index.dimensions as i32)
  }

  /// Check if the index is trained
  #[napi(getter)]
  pub fn trained(&self) -> Result<bool> {
    let index = self.inner.read().map_err(|e| Error::from_reason(e.to_string()))?;
    Ok(index.trained)
  }

  /// Add training vectors
  ///
  /// Call this before train() with representative vectors from your dataset.
  #[napi]
  pub fn add_training_vectors(&self, vectors: Vec<f64>, num_vectors: i32) -> Result<()> {
    let mut index = self.inner.write().map_err(|e| Error::from_reason(e.to_string()))?;
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
    let mut index = self.inner.write().map_err(|e| Error::from_reason(e.to_string()))?;
    index
      .train()
      .map_err(|e| Error::from_reason(format!("Failed to train index: {e}")))
  }

  /// Insert a vector into the index
  ///
  /// The index must be trained first.
  #[napi]
  pub fn insert(&self, vector_id: i64, vector: Vec<f64>) -> Result<()> {
    let mut index = self.inner.write().map_err(|e| Error::from_reason(e.to_string()))?;
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
    let mut index = self.inner.write().map_err(|e| Error::from_reason(e.to_string()))?;
    let vector_f32: Vec<f32> = vector.iter().map(|&v| v as f32).collect();
    Ok(index.delete(vector_id as u64, &vector_f32))
  }

  /// Clear all data from the index
  #[napi]
  pub fn clear(&self) -> Result<()> {
    let mut index = self.inner.write().map_err(|e| Error::from_reason(e.to_string()))?;
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
    let index = self.inner.read().map_err(|e| Error::from_reason(e.to_string()))?;

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
    let index = self.inner.read().map_err(|e| Error::from_reason(e.to_string()))?;

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

    let results = index.search_multi(&manifest, &query_refs, k as usize, aggregation.into(), rust_options);
    Ok(results.into_iter().map(|r| r.into()).collect())
  }

  /// Get index statistics
  #[napi]
  pub fn stats(&self) -> Result<JsIvfStats> {
    let index = self.inner.read().map_err(|e| Error::from_reason(e.to_string()))?;
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
    let index = self.inner.read().map_err(|e| Error::from_reason(e.to_string()))?;
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
    let index = self.inner.read().map_err(|e| Error::from_reason(e.to_string()))?;
    Ok(index.dimensions as i32)
  }

  /// Check if the index is trained
  #[napi(getter)]
  pub fn trained(&self) -> Result<bool> {
    let index = self.inner.read().map_err(|e| Error::from_reason(e.to_string()))?;
    Ok(index.trained)
  }

  /// Add training vectors
  #[napi]
  pub fn add_training_vectors(&self, vectors: Vec<f64>, num_vectors: i32) -> Result<()> {
    let mut index = self.inner.write().map_err(|e| Error::from_reason(e.to_string()))?;
    let vectors_f32: Vec<f32> = vectors.iter().map(|&v| v as f32).collect();
    index
      .add_training_vectors(&vectors_f32, num_vectors as usize)
      .map_err(|e| Error::from_reason(format!("Failed to add training vectors: {e}")))
  }

  /// Train the index
  #[napi]
  pub fn train(&self) -> Result<()> {
    let mut index = self.inner.write().map_err(|e| Error::from_reason(e.to_string()))?;
    index
      .train()
      .map_err(|e| Error::from_reason(format!("Failed to train index: {e}")))
  }

  /// Insert a vector
  #[napi]
  pub fn insert(&self, vector_id: i64, vector: Vec<f64>) -> Result<()> {
    let mut index = self.inner.write().map_err(|e| Error::from_reason(e.to_string()))?;
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
    let mut index = self.inner.write().map_err(|e| Error::from_reason(e.to_string()))?;
    let vector_f32: Vec<f32> = vector.iter().map(|&v| v as f32).collect();
    Ok(index.delete(vector_id as u64, &vector_f32))
  }

  /// Clear the index
  #[napi]
  pub fn clear(&self) -> Result<()> {
    let mut index = self.inner.write().map_err(|e| Error::from_reason(e.to_string()))?;
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
    let index = self.inner.read().map_err(|e| Error::from_reason(e.to_string()))?;

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
    let index = self.inner.read().map_err(|e| Error::from_reason(e.to_string()))?;

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

    let results = index.search_multi(&manifest, &query_refs, k as usize, aggregation.into(), rust_options);
    Ok(results.into_iter().map(|r| r.into()).collect())
  }

  /// Get index statistics
  #[napi]
  pub fn stats(&self) -> Result<JsIvfStats> {
    let index = self.inner.read().map_err(|e| Error::from_reason(e.to_string()))?;
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
    let index = self.inner.read().map_err(|e| Error::from_reason(e.to_string()))?;
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
    return Err(Error::from_reason("vectors and node_ids must have same length"));
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
