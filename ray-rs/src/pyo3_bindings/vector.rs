//! Python bindings for Vector Search
//!
//! Exposes IVF and IVF-PQ indexes to Python.

use pyo3::prelude::*;
use pyo3::exceptions::PyRuntimeError;
use pyo3::types::PyBytes;
use std::sync::RwLock;

use crate::vector::{
  DistanceMetric as RustDistanceMetric, IvfConfig as RustIvfConfig, IvfIndex as RustIvfIndex,
  IvfPqConfig as RustIvfPqConfig, IvfPqIndex as RustIvfPqIndex, MultiQueryAggregation,
  PqConfig as RustPqConfig, SearchOptions as RustSearchOptions, VectorManifest, VectorSearchResult,
};

// ============================================================================
// Distance Metric
// ============================================================================

/// Distance metric conversion
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PyDistanceMetricEnum {
  Cosine,
  Euclidean,
  DotProduct,
}

impl From<&str> for PyDistanceMetricEnum {
  fn from(s: &str) -> Self {
    match s.to_lowercase().as_str() {
      "euclidean" | "l2" => PyDistanceMetricEnum::Euclidean,
      "dot" | "dotproduct" | "dot_product" => PyDistanceMetricEnum::DotProduct,
      _ => PyDistanceMetricEnum::Cosine,
    }
  }
}

impl From<PyDistanceMetricEnum> for RustDistanceMetric {
  fn from(m: PyDistanceMetricEnum) -> Self {
    match m {
      PyDistanceMetricEnum::Cosine => RustDistanceMetric::Cosine,
      PyDistanceMetricEnum::Euclidean => RustDistanceMetric::Euclidean,
      PyDistanceMetricEnum::DotProduct => RustDistanceMetric::DotProduct,
    }
  }
}

impl From<RustDistanceMetric> for PyDistanceMetricEnum {
  fn from(m: RustDistanceMetric) -> Self {
    match m {
      RustDistanceMetric::Cosine => PyDistanceMetricEnum::Cosine,
      RustDistanceMetric::Euclidean => PyDistanceMetricEnum::Euclidean,
      RustDistanceMetric::DotProduct => PyDistanceMetricEnum::DotProduct,
    }
  }
}

// ============================================================================
// Aggregation Method
// ============================================================================

/// Aggregation method conversion
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PyAggregationEnum {
  Min,
  Max,
  Avg,
  Sum,
}

impl From<&str> for PyAggregationEnum {
  fn from(s: &str) -> Self {
    match s.to_lowercase().as_str() {
      "max" => PyAggregationEnum::Max,
      "avg" | "average" => PyAggregationEnum::Avg,
      "sum" => PyAggregationEnum::Sum,
      _ => PyAggregationEnum::Min,
    }
  }
}

impl From<PyAggregationEnum> for MultiQueryAggregation {
  fn from(a: PyAggregationEnum) -> Self {
    match a {
      PyAggregationEnum::Min => MultiQueryAggregation::Min,
      PyAggregationEnum::Max => MultiQueryAggregation::Max,
      PyAggregationEnum::Avg => MultiQueryAggregation::Avg,
      PyAggregationEnum::Sum => MultiQueryAggregation::Sum,
    }
  }
}

// ============================================================================
// IVF Configuration
// ============================================================================

/// Configuration for IVF index
#[pyclass(name = "IvfConfig")]
#[derive(Debug, Clone)]
pub struct PyIvfConfig {
  /// Number of clusters (default: 100)
  #[pyo3(get, set)]
  pub n_clusters: Option<i32>,
  /// Number of clusters to probe during search (default: 10)
  #[pyo3(get, set)]
  pub n_probe: Option<i32>,
  /// Distance metric ("cosine", "euclidean", "dot_product")
  #[pyo3(get, set)]
  pub metric: Option<String>,
}

#[pymethods]
impl PyIvfConfig {
  #[new]
  #[pyo3(signature = (n_clusters=None, n_probe=None, metric=None))]
  fn new(n_clusters: Option<i32>, n_probe: Option<i32>, metric: Option<String>) -> Self {
    Self {
      n_clusters,
      n_probe,
      metric,
    }
  }

  fn __repr__(&self) -> String {
    format!(
      "IvfConfig(n_clusters={:?}, n_probe={:?}, metric={:?})",
      self.n_clusters, self.n_probe, self.metric
    )
  }
}

impl From<PyIvfConfig> for RustIvfConfig {
  fn from(c: PyIvfConfig) -> Self {
    let mut config = RustIvfConfig::default();
    if let Some(n) = c.n_clusters {
      config.n_clusters = n as usize;
    }
    if let Some(n) = c.n_probe {
      config.n_probe = n as usize;
    }
    if let Some(m) = c.metric {
      let metric: PyDistanceMetricEnum = m.as_str().into();
      config.metric = metric.into();
    }
    config
  }
}

// ============================================================================
// PQ Configuration
// ============================================================================

/// Configuration for Product Quantization
#[pyclass(name = "PqConfig")]
#[derive(Debug, Clone)]
pub struct PyPqConfig {
  /// Number of subspaces (must divide dimensions evenly)
  #[pyo3(get, set)]
  pub num_subspaces: Option<i32>,
  /// Number of centroids per subspace (default: 256)
  #[pyo3(get, set)]
  pub num_centroids: Option<i32>,
  /// Max k-means iterations for training (default: 25)
  #[pyo3(get, set)]
  pub max_iterations: Option<i32>,
}

#[pymethods]
impl PyPqConfig {
  #[new]
  #[pyo3(signature = (num_subspaces=None, num_centroids=None, max_iterations=None))]
  fn new(
    num_subspaces: Option<i32>,
    num_centroids: Option<i32>,
    max_iterations: Option<i32>,
  ) -> Self {
    Self {
      num_subspaces,
      num_centroids,
      max_iterations,
    }
  }

  fn __repr__(&self) -> String {
    format!(
      "PqConfig(num_subspaces={:?}, num_centroids={:?})",
      self.num_subspaces, self.num_centroids
    )
  }
}

impl From<PyPqConfig> for RustPqConfig {
  fn from(c: PyPqConfig) -> Self {
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
#[pyclass(name = "SearchOptions")]
#[derive(Debug, Clone)]
pub struct PySearchOptions {
  /// Number of clusters to probe (overrides index default)
  #[pyo3(get, set)]
  pub n_probe: Option<i32>,
  /// Minimum similarity threshold (0-1)
  #[pyo3(get, set)]
  pub threshold: Option<f64>,
}

#[pymethods]
impl PySearchOptions {
  #[new]
  #[pyo3(signature = (n_probe=None, threshold=None))]
  fn new(n_probe: Option<i32>, threshold: Option<f64>) -> Self {
    Self { n_probe, threshold }
  }

  fn __repr__(&self) -> String {
    format!(
      "SearchOptions(n_probe={:?}, threshold={:?})",
      self.n_probe, self.threshold
    )
  }
}

// ============================================================================
// Search Result
// ============================================================================

/// Result of a vector search
#[pyclass(name = "SearchResult")]
#[derive(Debug, Clone)]
pub struct PySearchResult {
  /// Vector ID
  #[pyo3(get)]
  pub vector_id: i64,
  /// Associated node ID
  #[pyo3(get)]
  pub node_id: i64,
  /// Distance from query
  #[pyo3(get)]
  pub distance: f64,
  /// Similarity score (0-1, higher is more similar)
  #[pyo3(get)]
  pub similarity: f64,
}

#[pymethods]
impl PySearchResult {
  fn __repr__(&self) -> String {
    format!(
      "SearchResult(node_id={}, distance={:.4}, similarity={:.4})",
      self.node_id, self.distance, self.similarity
    )
  }
}

impl From<VectorSearchResult> for PySearchResult {
  fn from(r: VectorSearchResult) -> Self {
    PySearchResult {
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
#[pyclass(name = "IvfStats")]
#[derive(Debug, Clone)]
pub struct PyIvfStats {
  /// Whether the index is trained
  #[pyo3(get)]
  pub trained: bool,
  /// Number of clusters
  #[pyo3(get)]
  pub n_clusters: i32,
  /// Total vectors in the index
  #[pyo3(get)]
  pub total_vectors: i64,
  /// Average vectors per cluster
  #[pyo3(get)]
  pub avg_vectors_per_cluster: f64,
  /// Number of empty clusters
  #[pyo3(get)]
  pub empty_cluster_count: i32,
  /// Minimum cluster size
  #[pyo3(get)]
  pub min_cluster_size: i32,
  /// Maximum cluster size
  #[pyo3(get)]
  pub max_cluster_size: i32,
}

#[pymethods]
impl PyIvfStats {
  fn __repr__(&self) -> String {
    format!(
      "IvfStats(trained={}, n_clusters={}, total_vectors={})",
      self.trained, self.n_clusters, self.total_vectors
    )
  }
}

// ============================================================================
// IVF Index Python Wrapper
// ============================================================================

/// IVF (Inverted File) index for approximate nearest neighbor search
#[pyclass(name = "IvfIndex")]
pub struct PyIvfIndex {
  inner: RwLock<RustIvfIndex>,
}

#[pymethods]
impl PyIvfIndex {
  /// Create a new IVF index
  #[new]
  #[pyo3(signature = (dimensions, config=None))]
  fn new(dimensions: i32, config: Option<PyIvfConfig>) -> PyResult<Self> {
    let rust_config = config.map(Into::into).unwrap_or_default();
    Ok(PyIvfIndex {
      inner: RwLock::new(RustIvfIndex::new(dimensions as usize, rust_config)),
    })
  }

  /// Get the number of dimensions
  #[getter]
  fn dimensions(&self) -> PyResult<i32> {
    let index = self.inner.read().map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    Ok(index.dimensions as i32)
  }

  /// Check if the index is trained
  #[getter]
  fn trained(&self) -> PyResult<bool> {
    let index = self.inner.read().map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    Ok(index.trained)
  }

  /// Add training vectors
  fn add_training_vectors(&self, vectors: Vec<f64>, num_vectors: i32) -> PyResult<()> {
    let mut index = self.inner.write().map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let vectors_f32: Vec<f32> = vectors.iter().map(|&v| v as f32).collect();
    index
      .add_training_vectors(&vectors_f32, num_vectors as usize)
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to add training vectors: {e}")))
  }

  /// Train the index on added training vectors
  fn train(&self) -> PyResult<()> {
    let mut index = self.inner.write().map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    index
      .train()
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to train index: {e}")))
  }

  /// Insert a vector into the index
  fn insert(&self, vector_id: i64, vector: Vec<f64>) -> PyResult<()> {
    let mut index = self.inner.write().map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let vector_f32: Vec<f32> = vector.iter().map(|&v| v as f32).collect();
    index
      .insert(vector_id as u64, &vector_f32)
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to insert vector: {e}")))
  }

  /// Delete a vector from the index
  fn delete(&self, vector_id: i64, vector: Vec<f64>) -> PyResult<bool> {
    let mut index = self.inner.write().map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let vector_f32: Vec<f32> = vector.iter().map(|&v| v as f32).collect();
    Ok(index.delete(vector_id as u64, &vector_f32))
  }

  /// Clear all data from the index
  fn clear(&self) -> PyResult<()> {
    let mut index = self.inner.write().map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    index.clear();
    Ok(())
  }

  /// Search for k nearest neighbors
  #[pyo3(signature = (manifest_json, query, k, options=None))]
  fn search(
    &self,
    manifest_json: String,
    query: Vec<f64>,
    k: i32,
    options: Option<PySearchOptions>,
  ) -> PyResult<Vec<PySearchResult>> {
    let index = self.inner.read().map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

    let manifest: VectorManifest = serde_json::from_str(&manifest_json)
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to parse manifest: {e}")))?;

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
  #[pyo3(signature = (manifest_json, queries, k, aggregation, options=None))]
  fn search_multi(
    &self,
    manifest_json: String,
    queries: Vec<Vec<f64>>,
    k: i32,
    aggregation: String,
    options: Option<PySearchOptions>,
  ) -> PyResult<Vec<PySearchResult>> {
    let index = self.inner.read().map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

    let manifest: VectorManifest = serde_json::from_str(&manifest_json)
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to parse manifest: {e}")))?;

    let queries_f32: Vec<Vec<f32>> = queries
      .iter()
      .map(|q| q.iter().map(|&v| v as f32).collect())
      .collect();

    let query_refs: Vec<&[f32]> = queries_f32.iter().map(|q| q.as_slice()).collect();

    let agg: PyAggregationEnum = aggregation.as_str().into();

    let rust_options = options.map(|o| RustSearchOptions {
      n_probe: o.n_probe.map(|n| n as usize),
      filter: None,
      threshold: o.threshold.map(|t| t as f32),
    });

    let results = index.search_multi(&manifest, &query_refs, k as usize, agg.into(), rust_options);
    Ok(results.into_iter().map(|r| r.into()).collect())
  }

  /// Get index statistics
  fn stats(&self) -> PyResult<PyIvfStats> {
    let index = self.inner.read().map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let s = index.stats();
    Ok(PyIvfStats {
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
  fn serialize<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
    let index = self.inner.read().map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let bytes = crate::vector::ivf::serialize::serialize_ivf(&index);
    Ok(PyBytes::new_bound(py, &bytes))
  }

  /// Deserialize an index from bytes
  #[staticmethod]
  fn deserialize(data: &[u8]) -> PyResult<PyIvfIndex> {
    let index = crate::vector::ivf::serialize::deserialize_ivf(data)
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to deserialize: {e}")))?;
    Ok(PyIvfIndex {
      inner: RwLock::new(index),
    })
  }

  fn __repr__(&self) -> PyResult<String> {
    let index = self.inner.read().map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    Ok(format!(
      "IvfIndex(dimensions={}, trained={})",
      index.dimensions, index.trained
    ))
  }
}

// ============================================================================
// IVF-PQ Index Python Wrapper
// ============================================================================

/// IVF-PQ combined index for memory-efficient approximate nearest neighbor search
#[pyclass(name = "IvfPqIndex")]
pub struct PyIvfPqIndex {
  inner: RwLock<RustIvfPqIndex>,
}

#[pymethods]
impl PyIvfPqIndex {
  /// Create a new IVF-PQ index
  #[new]
  #[pyo3(signature = (dimensions, ivf_config=None, pq_config=None, use_residuals=None))]
  fn new(
    dimensions: i32,
    ivf_config: Option<PyIvfConfig>,
    pq_config: Option<PyPqConfig>,
    use_residuals: Option<bool>,
  ) -> PyResult<Self> {
    let config = RustIvfPqConfig {
      ivf: ivf_config.map(Into::into).unwrap_or_default(),
      pq: pq_config.map(Into::into).unwrap_or_default(),
      use_residuals: use_residuals.unwrap_or(true),
    };

    let index = RustIvfPqIndex::new(dimensions as usize, config)
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to create index: {e}")))?;

    Ok(PyIvfPqIndex {
      inner: RwLock::new(index),
    })
  }

  /// Get the number of dimensions
  #[getter]
  fn dimensions(&self) -> PyResult<i32> {
    let index = self.inner.read().map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    Ok(index.dimensions as i32)
  }

  /// Check if the index is trained
  #[getter]
  fn trained(&self) -> PyResult<bool> {
    let index = self.inner.read().map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    Ok(index.trained)
  }

  /// Add training vectors
  fn add_training_vectors(&self, vectors: Vec<f64>, num_vectors: i32) -> PyResult<()> {
    let mut index = self.inner.write().map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let vectors_f32: Vec<f32> = vectors.iter().map(|&v| v as f32).collect();
    index
      .add_training_vectors(&vectors_f32, num_vectors as usize)
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to add training vectors: {e}")))
  }

  /// Train the index
  fn train(&self) -> PyResult<()> {
    let mut index = self.inner.write().map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    index
      .train()
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to train index: {e}")))
  }

  /// Insert a vector
  fn insert(&self, vector_id: i64, vector: Vec<f64>) -> PyResult<()> {
    let mut index = self.inner.write().map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let vector_f32: Vec<f32> = vector.iter().map(|&v| v as f32).collect();
    index
      .insert(vector_id as u64, &vector_f32)
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to insert vector: {e}")))
  }

  /// Delete a vector
  fn delete(&self, vector_id: i64, vector: Vec<f64>) -> PyResult<bool> {
    let mut index = self.inner.write().map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let vector_f32: Vec<f32> = vector.iter().map(|&v| v as f32).collect();
    Ok(index.delete(vector_id as u64, &vector_f32))
  }

  /// Clear the index
  fn clear(&self) -> PyResult<()> {
    let mut index = self.inner.write().map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    index.clear();
    Ok(())
  }

  /// Search for k nearest neighbors using PQ distance approximation
  #[pyo3(signature = (manifest_json, query, k, options=None))]
  fn search(
    &self,
    manifest_json: String,
    query: Vec<f64>,
    k: i32,
    options: Option<PySearchOptions>,
  ) -> PyResult<Vec<PySearchResult>> {
    let index = self.inner.read().map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

    let manifest: VectorManifest = serde_json::from_str(&manifest_json)
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to parse manifest: {e}")))?;

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
  #[pyo3(signature = (manifest_json, queries, k, aggregation, options=None))]
  fn search_multi(
    &self,
    manifest_json: String,
    queries: Vec<Vec<f64>>,
    k: i32,
    aggregation: String,
    options: Option<PySearchOptions>,
  ) -> PyResult<Vec<PySearchResult>> {
    let index = self.inner.read().map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

    let manifest: VectorManifest = serde_json::from_str(&manifest_json)
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to parse manifest: {e}")))?;

    let queries_f32: Vec<Vec<f32>> = queries
      .iter()
      .map(|q| q.iter().map(|&v| v as f32).collect())
      .collect();

    let query_refs: Vec<&[f32]> = queries_f32.iter().map(|q| q.as_slice()).collect();

    let agg: PyAggregationEnum = aggregation.as_str().into();

    let rust_options = options.map(|o| crate::vector::ivf_pq::IvfPqSearchOptions {
      n_probe: o.n_probe.map(|n| n as usize),
      filter: None,
      threshold: o.threshold.map(|t| t as f32),
    });

    let results = index.search_multi(&manifest, &query_refs, k as usize, agg.into(), rust_options);
    Ok(results.into_iter().map(|r| r.into()).collect())
  }

  /// Get index statistics
  fn stats(&self) -> PyResult<PyIvfStats> {
    let index = self.inner.read().map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let s = index.stats();
    Ok(PyIvfStats {
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
  fn serialize<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
    let index = self.inner.read().map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let bytes = crate::vector::ivf_pq::serialize_ivf_pq(&index);
    Ok(PyBytes::new_bound(py, &bytes))
  }

  /// Deserialize an index from bytes
  #[staticmethod]
  fn deserialize(data: &[u8]) -> PyResult<PyIvfPqIndex> {
    let index = crate::vector::ivf_pq::deserialize_ivf_pq(data)
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to deserialize: {e}")))?;
    Ok(PyIvfPqIndex {
      inner: RwLock::new(index),
    })
  }

  fn __repr__(&self) -> PyResult<String> {
    let index = self.inner.read().map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    Ok(format!(
      "IvfPqIndex(dimensions={}, trained={})",
      index.dimensions, index.trained
    ))
  }
}

// ============================================================================
// Brute Force Search
// ============================================================================

/// Brute force search result
#[pyclass(name = "BruteForceResult")]
#[derive(Debug, Clone)]
pub struct PyBruteForceResult {
  #[pyo3(get)]
  pub node_id: i64,
  #[pyo3(get)]
  pub distance: f64,
  #[pyo3(get)]
  pub similarity: f64,
}

#[pymethods]
impl PyBruteForceResult {
  fn __repr__(&self) -> String {
    format!(
      "BruteForceResult(node_id={}, distance={:.4}, similarity={:.4})",
      self.node_id, self.distance, self.similarity
    )
  }
}

/// Perform brute-force search over all vectors
#[pyfunction]
#[pyo3(signature = (vectors, node_ids, query, k, metric=None))]
pub fn brute_force_search(
  vectors: Vec<Vec<f64>>,
  node_ids: Vec<i64>,
  query: Vec<f64>,
  k: i32,
  metric: Option<String>,
) -> PyResult<Vec<PyBruteForceResult>> {
  if vectors.len() != node_ids.len() {
    return Err(PyRuntimeError::new_err("vectors and node_ids must have same length"));
  }

  let metric_enum: PyDistanceMetricEnum = metric.as_deref().unwrap_or("cosine").into();
  let rust_metric: RustDistanceMetric = metric_enum.into();
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
      .map(|(node_id, distance)| PyBruteForceResult {
        node_id,
        distance: distance as f64,
        similarity: rust_metric.distance_to_similarity(distance) as f64,
      })
      .collect(),
  )
}
