//! Python bindings for SingleFileDB
//!
//! Provides Python access to the single-file database format.

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use std::collections::HashSet;
use std::sync::Mutex;

use super::traversal::{PyPathEdge, PyPathResult, PyTraversalResult};
use crate::api::pathfinding::{bfs, dijkstra, PathConfig};
use crate::api::traversal::{
  TraversalBuilder as RustTraversalBuilder, TraversalDirection, TraverseOptions,
};
use crate::core::single_file::{
  close_single_file, open_single_file, SingleFileDB as RustSingleFileDB,
  SingleFileOpenOptions as RustOpenOptions, SyncMode as RustSyncMode,
};
use crate::types::{ETypeId, Edge, NodeId, PropKeyId, PropValue};

// ============================================================================
// Open Options
// ============================================================================

/// Synchronization mode for WAL writes
///
/// Controls the durability vs performance trade-off for commits.
/// - "full": Fsync on every commit (durable to OS, slowest)
/// - "normal": Fsync only on checkpoint (~1000x faster, safe from app crash)
/// - "off": No fsync (fastest, data may be lost on any crash)
#[pyclass(name = "SyncMode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PySyncMode {
  mode: RustSyncMode,
}

#[pymethods]
impl PySyncMode {
  /// Full durability: fsync on every commit
  #[staticmethod]
  fn full() -> Self {
    Self {
      mode: RustSyncMode::Full,
    }
  }

  /// Normal: fsync on checkpoint only (~1000x faster)
  /// Safe from application crashes, but not OS crashes.
  #[staticmethod]
  fn normal() -> Self {
    Self {
      mode: RustSyncMode::Normal,
    }
  }

  /// No fsync (fastest, for testing only)
  #[staticmethod]
  fn off() -> Self {
    Self {
      mode: RustSyncMode::Off,
    }
  }

  fn __repr__(&self) -> String {
    match self.mode {
      RustSyncMode::Full => "SyncMode.full()".to_string(),
      RustSyncMode::Normal => "SyncMode.normal()".to_string(),
      RustSyncMode::Off => "SyncMode.off()".to_string(),
    }
  }
}

/// Options for opening a database
#[pyclass(name = "OpenOptions")]
#[derive(Debug, Clone, Default)]
pub struct PyOpenOptions {
  /// Open in read-only mode
  #[pyo3(get, set)]
  pub read_only: Option<bool>,
  /// Create database if it doesn't exist
  #[pyo3(get, set)]
  pub create_if_missing: Option<bool>,
  /// Page size in bytes (default 4096)
  #[pyo3(get, set)]
  pub page_size: Option<u32>,
  /// WAL size in bytes (default 1MB)
  #[pyo3(get, set)]
  pub wal_size: Option<u32>,
  /// Enable auto-checkpoint when WAL usage exceeds threshold
  #[pyo3(get, set)]
  pub auto_checkpoint: Option<bool>,
  /// WAL usage threshold (0.0-1.0) to trigger auto-checkpoint
  #[pyo3(get, set)]
  pub checkpoint_threshold: Option<f64>,
  /// Use background (non-blocking) checkpoint
  #[pyo3(get, set)]
  pub background_checkpoint: Option<bool>,
  /// Enable caching
  #[pyo3(get, set)]
  pub cache_enabled: Option<bool>,
  /// Max node properties in cache
  #[pyo3(get, set)]
  pub cache_max_node_props: Option<i64>,
  /// Max edge properties in cache
  #[pyo3(get, set)]
  pub cache_max_edge_props: Option<i64>,
  /// Max traversal cache entries
  #[pyo3(get, set)]
  pub cache_max_traversal_entries: Option<i64>,
  /// Max query cache entries
  #[pyo3(get, set)]
  pub cache_max_query_entries: Option<i64>,
  /// Query cache TTL in milliseconds
  #[pyo3(get, set)]
  pub cache_query_ttl_ms: Option<i64>,
  /// Sync mode: "full", "normal", or "off"
  pub sync_mode: Option<PySyncMode>,
}

#[pymethods]
impl PyOpenOptions {
  #[new]
  #[pyo3(signature = (
    read_only=None,
    create_if_missing=None,
    page_size=None,
    wal_size=None,
    auto_checkpoint=None,
    checkpoint_threshold=None,
    background_checkpoint=None,
    cache_enabled=None,
    cache_max_node_props=None,
    cache_max_edge_props=None,
    cache_max_traversal_entries=None,
    cache_max_query_entries=None,
    cache_query_ttl_ms=None,
    sync_mode=None
  ))]
  fn new(
    read_only: Option<bool>,
    create_if_missing: Option<bool>,
    page_size: Option<u32>,
    wal_size: Option<u32>,
    auto_checkpoint: Option<bool>,
    checkpoint_threshold: Option<f64>,
    background_checkpoint: Option<bool>,
    cache_enabled: Option<bool>,
    cache_max_node_props: Option<i64>,
    cache_max_edge_props: Option<i64>,
    cache_max_traversal_entries: Option<i64>,
    cache_max_query_entries: Option<i64>,
    cache_query_ttl_ms: Option<i64>,
    sync_mode: Option<PySyncMode>,
  ) -> Self {
    Self {
      read_only,
      create_if_missing,
      page_size,
      wal_size,
      auto_checkpoint,
      checkpoint_threshold,
      background_checkpoint,
      cache_enabled,
      cache_max_node_props,
      cache_max_edge_props,
      cache_max_traversal_entries,
      cache_max_query_entries,
      cache_query_ttl_ms,
      sync_mode,
    }
  }
}

impl From<PyOpenOptions> for RustOpenOptions {
  fn from(opts: PyOpenOptions) -> Self {
    use crate::types::{CacheOptions, PropertyCacheConfig, QueryCacheConfig, TraversalCacheConfig};

    let mut rust_opts = RustOpenOptions::new();
    if let Some(v) = opts.read_only {
      rust_opts = rust_opts.read_only(v);
    }
    if let Some(v) = opts.create_if_missing {
      rust_opts = rust_opts.create_if_missing(v);
    }
    if let Some(v) = opts.page_size {
      rust_opts = rust_opts.page_size(v as usize);
    }
    if let Some(v) = opts.wal_size {
      rust_opts = rust_opts.wal_size(v as usize);
    }
    if let Some(v) = opts.auto_checkpoint {
      rust_opts = rust_opts.auto_checkpoint(v);
    }
    if let Some(v) = opts.checkpoint_threshold {
      rust_opts = rust_opts.checkpoint_threshold(v);
    }
    if let Some(v) = opts.background_checkpoint {
      rust_opts = rust_opts.background_checkpoint(v);
    }

    // Cache options
    if opts.cache_enabled == Some(true) {
      let property_cache = Some(PropertyCacheConfig {
        max_node_props: opts.cache_max_node_props.unwrap_or(10000) as usize,
        max_edge_props: opts.cache_max_edge_props.unwrap_or(10000) as usize,
      });

      let traversal_cache = Some(TraversalCacheConfig {
        max_entries: opts.cache_max_traversal_entries.unwrap_or(5000) as usize,
        max_neighbors_per_entry: 100,
      });

      let query_cache = Some(QueryCacheConfig {
        max_entries: opts.cache_max_query_entries.unwrap_or(1000) as usize,
        ttl_ms: opts.cache_query_ttl_ms.map(|v| v as u64),
      });

      rust_opts = rust_opts.cache(Some(CacheOptions {
        enabled: true,
        property_cache,
        traversal_cache,
        query_cache,
      }));
    }

    // Sync mode
    if let Some(sync) = opts.sync_mode {
      rust_opts = rust_opts.sync_mode(sync.mode);
    }

    rust_opts
  }
}

// ============================================================================
// Database Statistics
// ============================================================================

/// Database statistics
#[pyclass(name = "DbStats")]
#[derive(Debug, Clone)]
pub struct PyDbStats {
  #[pyo3(get)]
  pub snapshot_gen: i64,
  #[pyo3(get)]
  pub snapshot_nodes: i64,
  #[pyo3(get)]
  pub snapshot_edges: i64,
  #[pyo3(get)]
  pub snapshot_max_node_id: i64,
  #[pyo3(get)]
  pub delta_nodes_created: i64,
  #[pyo3(get)]
  pub delta_nodes_deleted: i64,
  #[pyo3(get)]
  pub delta_edges_added: i64,
  #[pyo3(get)]
  pub delta_edges_deleted: i64,
  #[pyo3(get)]
  pub wal_bytes: i64,
  #[pyo3(get)]
  pub recommend_compact: bool,
}

#[pymethods]
impl PyDbStats {
  fn __repr__(&self) -> String {
    format!(
      "DbStats(nodes={}, edges={}, wal_bytes={}, recommend_compact={})",
      self.snapshot_nodes + self.delta_nodes_created - self.delta_nodes_deleted,
      self.snapshot_edges + self.delta_edges_added - self.delta_edges_deleted,
      self.wal_bytes,
      self.recommend_compact
    )
  }
}

/// Cache statistics
#[pyclass(name = "CacheStats")]
#[derive(Debug, Clone)]
pub struct PyCacheStats {
  #[pyo3(get)]
  pub property_cache_hits: i64,
  #[pyo3(get)]
  pub property_cache_misses: i64,
  #[pyo3(get)]
  pub property_cache_size: i64,
  #[pyo3(get)]
  pub traversal_cache_hits: i64,
  #[pyo3(get)]
  pub traversal_cache_misses: i64,
  #[pyo3(get)]
  pub traversal_cache_size: i64,
  #[pyo3(get)]
  pub query_cache_hits: i64,
  #[pyo3(get)]
  pub query_cache_misses: i64,
  #[pyo3(get)]
  pub query_cache_size: i64,
}

#[pymethods]
impl PyCacheStats {
  fn __repr__(&self) -> String {
    format!(
      "CacheStats(property_hits={}, traversal_hits={}, query_hits={})",
      self.property_cache_hits, self.traversal_cache_hits, self.query_cache_hits
    )
  }
}

// ============================================================================
// Property Value
// ============================================================================

/// Property value types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropType {
  Null,
  Bool,
  Int,
  Float,
  String,
}

/// Property value wrapper for Python
#[pyclass(name = "PropValue")]
#[derive(Debug, Clone)]
pub struct PyPropValue {
  #[pyo3(get)]
  pub prop_type: String,
  #[pyo3(get)]
  pub bool_value: Option<bool>,
  #[pyo3(get)]
  pub int_value: Option<i64>,
  #[pyo3(get)]
  pub float_value: Option<f64>,
  #[pyo3(get)]
  pub string_value: Option<String>,
}

#[pymethods]
impl PyPropValue {
  /// Create a null value
  #[staticmethod]
  fn null() -> Self {
    PyPropValue {
      prop_type: "null".to_string(),
      bool_value: None,
      int_value: None,
      float_value: None,
      string_value: None,
    }
  }

  /// Create a boolean value
  #[staticmethod]
  fn bool(value: bool) -> Self {
    PyPropValue {
      prop_type: "bool".to_string(),
      bool_value: Some(value),
      int_value: None,
      float_value: None,
      string_value: None,
    }
  }

  /// Create an integer value
  #[staticmethod]
  fn int(value: i64) -> Self {
    PyPropValue {
      prop_type: "int".to_string(),
      bool_value: None,
      int_value: Some(value),
      float_value: None,
      string_value: None,
    }
  }

  /// Create a float value
  #[staticmethod]
  #[pyo3(name = "float")]
  fn float_val(value: f64) -> Self {
    PyPropValue {
      prop_type: "float".to_string(),
      bool_value: None,
      int_value: None,
      float_value: Some(value),
      string_value: None,
    }
  }

  /// Create a string value
  #[staticmethod]
  fn string(value: String) -> Self {
    PyPropValue {
      prop_type: "string".to_string(),
      bool_value: None,
      int_value: None,
      float_value: None,
      string_value: Some(value),
    }
  }

  /// Get the Python value
  fn value(&self, py: Python<'_>) -> PyObject {
    use pyo3::ToPyObject;
    match self.prop_type.as_str() {
      "null" => py.None(),
      "bool" => self.bool_value.unwrap_or(false).to_object(py),
      "int" => self.int_value.unwrap_or(0).to_object(py),
      "float" => self.float_value.unwrap_or(0.0).to_object(py),
      "string" => self.string_value.clone().unwrap_or_default().to_object(py),
      _ => py.None(),
    }
  }

  fn __repr__(&self) -> String {
    match self.prop_type.as_str() {
      "null" => "PropValue(null)".to_string(),
      "bool" => format!("PropValue({})", self.bool_value.unwrap_or(false)),
      "int" => format!("PropValue({})", self.int_value.unwrap_or(0)),
      "float" => format!("PropValue({})", self.float_value.unwrap_or(0.0)),
      "string" => format!(
        "PropValue(\"{}\")",
        self.string_value.clone().unwrap_or_default()
      ),
      _ => "PropValue(unknown)".to_string(),
    }
  }
}

impl From<PropValue> for PyPropValue {
  fn from(value: PropValue) -> Self {
    match value {
      PropValue::Null => PyPropValue::null(),
      PropValue::Bool(v) => PyPropValue::bool(v),
      PropValue::I64(v) => PyPropValue::int(v),
      PropValue::F64(v) => PyPropValue::float_val(v),
      PropValue::String(v) => PyPropValue::string(v),
      PropValue::VectorF32(_) => PyPropValue::null(), // Vector handled separately
    }
  }
}

impl From<PyPropValue> for PropValue {
  fn from(value: PyPropValue) -> Self {
    match value.prop_type.as_str() {
      "null" => PropValue::Null,
      "bool" => PropValue::Bool(value.bool_value.unwrap_or(false)),
      "int" => PropValue::I64(value.int_value.unwrap_or(0)),
      "float" => PropValue::F64(value.float_value.unwrap_or(0.0)),
      "string" => PropValue::String(value.string_value.unwrap_or_default()),
      _ => PropValue::Null,
    }
  }
}

// ============================================================================
// Edge Result
// ============================================================================

/// Edge representation (neighbor style)
#[pyclass(name = "Edge")]
#[derive(Debug, Clone)]
pub struct PyEdge {
  #[pyo3(get)]
  pub etype: u32,
  #[pyo3(get)]
  pub node_id: i64,
}

#[pymethods]
impl PyEdge {
  fn __repr__(&self) -> String {
    format!("Edge(etype={}, node_id={})", self.etype, self.node_id)
  }
}

/// Full edge representation (src, etype, dst)
#[pyclass(name = "FullEdge")]
#[derive(Debug, Clone)]
pub struct PyFullEdge {
  #[pyo3(get)]
  pub src: i64,
  #[pyo3(get)]
  pub etype: u32,
  #[pyo3(get)]
  pub dst: i64,
}

#[pymethods]
impl PyFullEdge {
  fn __repr__(&self) -> String {
    format!(
      "FullEdge(src={}, etype={}, dst={})",
      self.src, self.etype, self.dst
    )
  }
}

// ============================================================================
// Node Property Result
// ============================================================================

/// Node property key-value pair
#[pyclass(name = "NodeProp")]
#[derive(Debug, Clone)]
pub struct PyNodeProp {
  #[pyo3(get)]
  pub key_id: u32,
  #[pyo3(get)]
  pub value: PyPropValue,
}

#[pymethods]
impl PyNodeProp {
  fn __repr__(&self) -> String {
    format!("NodeProp(key_id={}, value={:?})", self.key_id, self.value)
  }
}

// ============================================================================
// SingleFileDB Python Wrapper
// ============================================================================

/// Single-file graph database
#[pyclass(name = "Database")]
pub struct PyDatabase {
  inner: Mutex<Option<RustSingleFileDB>>,
}

#[pymethods]
impl PyDatabase {
  /// Open a database file
  #[new]
  #[pyo3(signature = (path, options=None))]
  fn new(path: String, options: Option<PyOpenOptions>) -> PyResult<Self> {
    let opts: RustOpenOptions = options.unwrap_or_default().into();
    let db = open_single_file(&path, opts)
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to open database: {e}")))?;
    Ok(PyDatabase {
      inner: Mutex::new(Some(db)),
    })
  }

  /// Close the database
  fn close(&self) -> PyResult<()> {
    let mut guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    if let Some(db) = guard.take() {
      close_single_file(db)
        .map_err(|e| PyRuntimeError::new_err(format!("Failed to close database: {e}")))?;
    }
    Ok(())
  }

  /// Check if database is open
  #[getter]
  fn is_open(&self) -> PyResult<bool> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    Ok(guard.is_some())
  }

  /// Get database path
  #[getter]
  fn path(&self) -> PyResult<String> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(db.path.to_string_lossy().to_string())
  }

  /// Check if database is read-only
  #[getter]
  fn read_only(&self) -> PyResult<bool> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(db.read_only)
  }

  // ========================================================================
  // Transaction Methods
  // ========================================================================

  /// Begin a transaction
  #[pyo3(signature = (read_only=None))]
  fn begin(&self, read_only: Option<bool>) -> PyResult<i64> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    let txid = db
      .begin(read_only.unwrap_or(false))
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to begin transaction: {e}")))?;
    Ok(txid as i64)
  }

  /// Commit the current transaction
  fn commit(&self) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    db.commit()
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to commit: {e}")))
  }

  /// Rollback the current transaction
  fn rollback(&self) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    db.rollback()
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to rollback: {e}")))
  }

  /// Check if there's an active transaction
  fn has_transaction(&self) -> PyResult<bool> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(db.has_transaction())
  }

  // ========================================================================
  // Node Operations
  // ========================================================================

  /// Create a new node
  #[pyo3(signature = (key=None))]
  fn create_node(&self, key: Option<String>) -> PyResult<i64> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    let node_id = db
      .create_node(key.as_deref())
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to create node: {e}")))?;
    Ok(node_id as i64)
  }

  /// Batch create multiple nodes with properties in a single transaction
  ///
  /// This is optimized for bulk inserts - creates all nodes and sets all properties
  /// in a single transaction with minimal FFI overhead.
  ///
  /// Args:
  ///   nodes: List of (key, props) tuples where props is a list of (prop_key_id, PropValue)
  ///
  /// Returns:
  ///   List of created node IDs
  fn batch_create_nodes(
    &self,
    nodes: Vec<(String, Vec<(u32, PyPropValue)>)>,
  ) -> PyResult<Vec<i64>> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;

    let mut node_ids = Vec::with_capacity(nodes.len());

    db.begin(false)
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to begin transaction: {e}")))?;

    let result: Result<(), pyo3::PyErr> = (|| {
      for (key, props) in nodes {
        let node_id = db
          .create_node(Some(&key))
          .map_err(|e| PyRuntimeError::new_err(format!("Failed to create node: {e}")))?;

        for (prop_key_id, value) in props {
          db.set_node_prop(node_id, prop_key_id as PropKeyId, value.into())
            .map_err(|e| PyRuntimeError::new_err(format!("Failed to set property: {e}")))?;
        }

        node_ids.push(node_id as i64);
      }
      Ok(())
    })();

    match result {
      Ok(()) => {
        db.commit()
          .map_err(|e| PyRuntimeError::new_err(format!("Failed to commit: {e}")))?;
        Ok(node_ids)
      }
      Err(e) => {
        let _ = db.rollback();
        Err(e)
      }
    }
  }

  /// Delete a node
  fn delete_node(&self, node_id: i64) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    db.delete_node(node_id as NodeId)
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to delete node: {e}")))
  }

  /// Check if a node exists
  fn node_exists(&self, node_id: i64) -> PyResult<bool> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(db.node_exists(node_id as NodeId))
  }

  /// Get node by key
  fn get_node_by_key(&self, key: String) -> PyResult<Option<i64>> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(db.get_node_by_key(&key).map(|id| id as i64))
  }

  /// Get the key for a node
  fn get_node_key(&self, node_id: i64) -> PyResult<Option<String>> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(db.get_node_key(node_id as NodeId))
  }

  /// List all node IDs
  fn list_nodes(&self) -> PyResult<Vec<i64>> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(db.list_nodes().into_iter().map(|id| id as i64).collect())
  }

  /// List node IDs with keys matching a prefix
  ///
  /// This is optimized for filtering by node type (e.g., "user:" prefix).
  fn list_nodes_with_prefix(&self, prefix: String) -> PyResult<Vec<i64>> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;

    let results: Vec<i64> = db
      .list_nodes()
      .into_iter()
      .filter(|&id| {
        if let Some(key) = db.get_node_key(id) {
          key.starts_with(&prefix)
        } else {
          false
        }
      })
      .map(|id| id as i64)
      .collect();
    Ok(results)
  }

  /// Count nodes with keys matching a prefix
  ///
  /// This is optimized for counting by node type (e.g., "user:" prefix).
  fn count_nodes_with_prefix(&self, prefix: String) -> PyResult<i64> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;

    let count = db
      .list_nodes()
      .into_iter()
      .filter(|&id| {
        if let Some(key) = db.get_node_key(id) {
          key.starts_with(&prefix)
        } else {
          false
        }
      })
      .count();
    Ok(count as i64)
  }

  /// Count all nodes
  fn count_nodes(&self) -> PyResult<i64> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(db.count_nodes() as i64)
  }

  // ========================================================================
  // Edge Operations
  // ========================================================================

  /// Add an edge
  fn add_edge(&self, src: i64, etype: u32, dst: i64) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    db.add_edge(src as NodeId, etype as ETypeId, dst as NodeId)
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to add edge: {e}")))
  }

  /// Add an edge by type name
  fn add_edge_by_name(&self, src: i64, etype_name: String, dst: i64) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    db.add_edge_by_name(src as NodeId, &etype_name, dst as NodeId)
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to add edge: {e}")))
  }

  /// Delete an edge
  fn delete_edge(&self, src: i64, etype: u32, dst: i64) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    db.delete_edge(src as NodeId, etype as ETypeId, dst as NodeId)
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to delete edge: {e}")))
  }

  /// Check if an edge exists
  fn edge_exists(&self, src: i64, etype: u32, dst: i64) -> PyResult<bool> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(db.edge_exists(src as NodeId, etype as ETypeId, dst as NodeId))
  }

  /// Get outgoing edges for a node
  fn get_out_edges(&self, node_id: i64) -> PyResult<Vec<PyEdge>> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(
      db.get_out_edges(node_id as NodeId)
        .into_iter()
        .map(|(etype, dst)| PyEdge {
          etype,
          node_id: dst as i64,
        })
        .collect(),
    )
  }

  /// Get incoming edges for a node
  fn get_in_edges(&self, node_id: i64) -> PyResult<Vec<PyEdge>> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(
      db.get_in_edges(node_id as NodeId)
        .into_iter()
        .map(|(etype, src)| PyEdge {
          etype,
          node_id: src as i64,
        })
        .collect(),
    )
  }

  /// Get out-degree for a node
  fn get_out_degree(&self, node_id: i64) -> PyResult<i64> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(db.get_out_degree(node_id as NodeId) as i64)
  }

  /// Get in-degree for a node
  fn get_in_degree(&self, node_id: i64) -> PyResult<i64> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(db.get_in_degree(node_id as NodeId) as i64)
  }

  /// Count all edges
  fn count_edges(&self) -> PyResult<i64> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(db.count_edges() as i64)
  }

  /// List all edges in the database
  #[pyo3(signature = (etype=None))]
  fn list_edges(&self, etype: Option<u32>) -> PyResult<Vec<PyFullEdge>> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(
      db.list_edges(etype)
        .into_iter()
        .map(|e| PyFullEdge {
          src: e.src as i64,
          etype: e.etype,
          dst: e.dst as i64,
        })
        .collect(),
    )
  }

  /// List edges by type name
  fn list_edges_by_name(&self, etype_name: String) -> PyResult<Vec<PyFullEdge>> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    let etype = db
      .get_etype_id(&etype_name)
      .ok_or_else(|| PyRuntimeError::new_err(format!("Unknown edge type: {etype_name}")))?;
    Ok(
      db.list_edges(Some(etype))
        .into_iter()
        .map(|e| PyFullEdge {
          src: e.src as i64,
          etype: e.etype,
          dst: e.dst as i64,
        })
        .collect(),
    )
  }

  /// Count edges by type
  fn count_edges_by_type(&self, etype: u32) -> PyResult<i64> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(db.count_edges_by_type(etype) as i64)
  }

  /// Count edges by type name
  fn count_edges_by_name(&self, etype_name: String) -> PyResult<i64> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    let etype = db
      .get_etype_id(&etype_name)
      .ok_or_else(|| PyRuntimeError::new_err(format!("Unknown edge type: {etype_name}")))?;
    Ok(db.count_edges_by_type(etype) as i64)
  }

  // ========================================================================
  // Property Operations
  // ========================================================================

  /// Set a node property
  fn set_node_prop(&self, node_id: i64, key_id: u32, value: PyPropValue) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    db.set_node_prop(node_id as NodeId, key_id as PropKeyId, value.into())
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to set property: {e}")))
  }

  /// Set a node property by key name
  fn set_node_prop_by_name(
    &self,
    node_id: i64,
    key_name: String,
    value: PyPropValue,
  ) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    db.set_node_prop_by_name(node_id as NodeId, &key_name, value.into())
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to set property: {e}")))
  }

  /// Delete a node property
  fn delete_node_prop(&self, node_id: i64, key_id: u32) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    db.delete_node_prop(node_id as NodeId, key_id as PropKeyId)
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to delete property: {e}")))
  }

  /// Get a specific node property
  fn get_node_prop(&self, node_id: i64, key_id: u32) -> PyResult<Option<PyPropValue>> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(
      db.get_node_prop(node_id as NodeId, key_id as PropKeyId)
        .map(|v| v.into()),
    )
  }

  // ========================================================================
  // Direct Property Type Methods (skip PropValue wrapper for performance)
  // ========================================================================

  /// Get a string property directly (faster than get_node_prop)
  fn get_node_prop_string(&self, node_id: i64, key_id: u32) -> PyResult<Option<String>> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(
      db.get_node_prop(node_id as NodeId, key_id as PropKeyId)
        .and_then(|v| match v {
          PropValue::String(s) => Some(s),
          _ => None,
        }),
    )
  }

  /// Get an integer property directly (faster than get_node_prop)
  fn get_node_prop_int(&self, node_id: i64, key_id: u32) -> PyResult<Option<i64>> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(
      db.get_node_prop(node_id as NodeId, key_id as PropKeyId)
        .and_then(|v| match v {
          PropValue::I64(i) => Some(i),
          _ => None,
        }),
    )
  }

  /// Get a float property directly (faster than get_node_prop)
  fn get_node_prop_float(&self, node_id: i64, key_id: u32) -> PyResult<Option<f64>> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(
      db.get_node_prop(node_id as NodeId, key_id as PropKeyId)
        .and_then(|v| match v {
          PropValue::F64(f) => Some(f),
          _ => None,
        }),
    )
  }

  /// Get a bool property directly (faster than get_node_prop)
  fn get_node_prop_bool(&self, node_id: i64, key_id: u32) -> PyResult<Option<bool>> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(
      db.get_node_prop(node_id as NodeId, key_id as PropKeyId)
        .and_then(|v| match v {
          PropValue::Bool(b) => Some(b),
          _ => None,
        }),
    )
  }

  /// Set a string property directly (faster than set_node_prop)
  fn set_node_prop_string(&self, node_id: i64, key_id: u32, value: String) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    db.set_node_prop(
      node_id as NodeId,
      key_id as PropKeyId,
      PropValue::String(value),
    )
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to set property: {e}")))
  }

  /// Set an integer property directly (faster than set_node_prop)
  fn set_node_prop_int(&self, node_id: i64, key_id: u32, value: i64) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    db.set_node_prop(
      node_id as NodeId,
      key_id as PropKeyId,
      PropValue::I64(value),
    )
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to set property: {e}")))
  }

  /// Set a float property directly (faster than set_node_prop)
  fn set_node_prop_float(&self, node_id: i64, key_id: u32, value: f64) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    db.set_node_prop(
      node_id as NodeId,
      key_id as PropKeyId,
      PropValue::F64(value),
    )
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to set property: {e}")))
  }

  /// Set a bool property directly (faster than set_node_prop)
  fn set_node_prop_bool(&self, node_id: i64, key_id: u32, value: bool) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    db.set_node_prop(
      node_id as NodeId,
      key_id as PropKeyId,
      PropValue::Bool(value),
    )
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to set property: {e}")))
  }

  /// Get all properties for a node
  fn get_node_props(&self, node_id: i64) -> PyResult<Option<Vec<PyNodeProp>>> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(db.get_node_props(node_id as NodeId).map(|props| {
      props
        .into_iter()
        .map(|(k, v)| PyNodeProp {
          key_id: k,
          value: v.into(),
        })
        .collect()
    }))
  }

  // ========================================================================
  // Edge Property Operations
  // ========================================================================

  /// Set an edge property
  fn set_edge_prop(
    &self,
    src: i64,
    etype: u32,
    dst: i64,
    key_id: u32,
    value: PyPropValue,
  ) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    db.set_edge_prop(
      src as NodeId,
      etype as ETypeId,
      dst as NodeId,
      key_id as PropKeyId,
      value.into(),
    )
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to set edge property: {e}")))
  }

  /// Set an edge property by key name
  fn set_edge_prop_by_name(
    &self,
    src: i64,
    etype: u32,
    dst: i64,
    key_name: String,
    value: PyPropValue,
  ) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    db.set_edge_prop_by_name(
      src as NodeId,
      etype as ETypeId,
      dst as NodeId,
      &key_name,
      value.into(),
    )
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to set edge property: {e}")))
  }

  /// Delete an edge property
  fn delete_edge_prop(&self, src: i64, etype: u32, dst: i64, key_id: u32) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    db.delete_edge_prop(
      src as NodeId,
      etype as ETypeId,
      dst as NodeId,
      key_id as PropKeyId,
    )
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to delete edge property: {e}")))
  }

  /// Get a specific edge property
  fn get_edge_prop(
    &self,
    src: i64,
    etype: u32,
    dst: i64,
    key_id: u32,
  ) -> PyResult<Option<PyPropValue>> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(
      db.get_edge_prop(
        src as NodeId,
        etype as ETypeId,
        dst as NodeId,
        key_id as PropKeyId,
      )
      .map(|v| v.into()),
    )
  }

  /// Get all properties for an edge
  fn get_edge_props(&self, src: i64, etype: u32, dst: i64) -> PyResult<Option<Vec<PyNodeProp>>> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(
      db.get_edge_props(src as NodeId, etype as ETypeId, dst as NodeId)
        .map(|props| {
          props
            .into_iter()
            .map(|(k, v)| PyNodeProp {
              key_id: k,
              value: v.into(),
            })
            .collect()
        }),
    )
  }

  // ========================================================================
  // Vector Operations
  // ========================================================================

  /// Set a vector embedding for a node
  fn set_node_vector(&self, node_id: i64, prop_key_id: u32, vector: Vec<f64>) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    let vector_f32: Vec<f32> = vector.iter().map(|&v| v as f32).collect();
    db.set_node_vector(node_id as NodeId, prop_key_id as PropKeyId, &vector_f32)
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to set vector: {e}")))
  }

  /// Get a vector embedding for a node
  fn get_node_vector(&self, node_id: i64, prop_key_id: u32) -> PyResult<Option<Vec<f64>>> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(
      db.get_node_vector(node_id as NodeId, prop_key_id as PropKeyId)
        .map(|v| v.iter().map(|&f| f as f64).collect()),
    )
  }

  /// Delete a vector embedding for a node
  fn delete_node_vector(&self, node_id: i64, prop_key_id: u32) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    db.delete_node_vector(node_id as NodeId, prop_key_id as PropKeyId)
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to delete vector: {e}")))
  }

  /// Check if a node has a vector embedding
  fn has_node_vector(&self, node_id: i64, prop_key_id: u32) -> PyResult<bool> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(db.has_node_vector(node_id as NodeId, prop_key_id as PropKeyId))
  }

  // ========================================================================
  // Schema Operations
  // ========================================================================

  /// Get or create a label ID
  fn get_or_create_label(&self, name: String) -> PyResult<u32> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(db.get_or_create_label(&name))
  }

  /// Get label ID by name
  fn get_label_id(&self, name: String) -> PyResult<Option<u32>> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(db.get_label_id(&name))
  }

  /// Get label name by ID
  fn get_label_name(&self, id: u32) -> PyResult<Option<String>> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(db.get_label_name(id))
  }

  /// Get or create an edge type ID
  fn get_or_create_etype(&self, name: String) -> PyResult<u32> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(db.get_or_create_etype(&name))
  }

  /// Get edge type ID by name
  fn get_etype_id(&self, name: String) -> PyResult<Option<u32>> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(db.get_etype_id(&name))
  }

  /// Get edge type name by ID
  fn get_etype_name(&self, id: u32) -> PyResult<Option<String>> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(db.get_etype_name(id))
  }

  /// Get or create a property key ID
  fn get_or_create_propkey(&self, name: String) -> PyResult<u32> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(db.get_or_create_propkey(&name))
  }

  /// Get property key ID by name
  fn get_propkey_id(&self, name: String) -> PyResult<Option<u32>> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(db.get_propkey_id(&name))
  }

  /// Get property key name by ID
  fn get_propkey_name(&self, id: u32) -> PyResult<Option<String>> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(db.get_propkey_name(id))
  }

  // ========================================================================
  // Node Label Operations
  // ========================================================================

  /// Define a new label (requires transaction)
  fn define_label(&self, name: String) -> PyResult<u32> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    db.define_label(&name)
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to define label: {e}")))
  }

  /// Add a label to a node
  fn add_node_label(&self, node_id: i64, label_id: u32) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    db.add_node_label(node_id as NodeId, label_id)
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to add label: {e}")))
  }

  /// Add a label to a node by name
  fn add_node_label_by_name(&self, node_id: i64, label_name: String) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    db.add_node_label_by_name(node_id as NodeId, &label_name)
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to add label: {e}")))
  }

  /// Remove a label from a node
  fn remove_node_label(&self, node_id: i64, label_id: u32) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    db.remove_node_label(node_id as NodeId, label_id)
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to remove label: {e}")))
  }

  /// Check if a node has a label
  fn node_has_label(&self, node_id: i64, label_id: u32) -> PyResult<bool> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(db.node_has_label(node_id as NodeId, label_id))
  }

  /// Get all labels for a node
  fn get_node_labels(&self, node_id: i64) -> PyResult<Vec<u32>> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(db.get_node_labels(node_id as NodeId))
  }

  // ========================================================================
  // Checkpoint / Maintenance
  // ========================================================================

  /// Perform a checkpoint (compact WAL into snapshot)
  fn checkpoint(&self) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    db.checkpoint()
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to checkpoint: {e}")))
  }

  /// Perform a background (non-blocking) checkpoint
  fn background_checkpoint(&self) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    db.background_checkpoint()
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to background checkpoint: {e}")))
  }

  /// Check if checkpoint is recommended
  #[pyo3(signature = (threshold=None))]
  fn should_checkpoint(&self, threshold: Option<f64>) -> PyResult<bool> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(db.should_checkpoint(threshold.unwrap_or(0.8)))
  }

  /// Optimize (compact) the database
  fn optimize(&self) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    db.checkpoint()
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to optimize: {e}")))
  }

  /// Get database statistics
  fn stats(&self) -> PyResult<PyDbStats> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    let s = db.stats();
    Ok(PyDbStats {
      snapshot_gen: s.snapshot_gen as i64,
      snapshot_nodes: s.snapshot_nodes as i64,
      snapshot_edges: s.snapshot_edges as i64,
      snapshot_max_node_id: s.snapshot_max_node_id as i64,
      delta_nodes_created: s.delta_nodes_created as i64,
      delta_nodes_deleted: s.delta_nodes_deleted as i64,
      delta_edges_added: s.delta_edges_added as i64,
      delta_edges_deleted: s.delta_edges_deleted as i64,
      wal_bytes: s.wal_bytes as i64,
      recommend_compact: s.recommend_compact,
    })
  }

  // ========================================================================
  // Cache Operations
  // ========================================================================

  /// Check if caching is enabled
  fn cache_is_enabled(&self) -> PyResult<bool> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(db.cache_is_enabled())
  }

  /// Invalidate all caches for a node
  fn cache_invalidate_node(&self, node_id: i64) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    db.cache_invalidate_node(node_id as NodeId);
    Ok(())
  }

  /// Invalidate caches for a specific edge
  fn cache_invalidate_edge(&self, src: i64, etype: u32, dst: i64) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    db.cache_invalidate_edge(src as NodeId, etype as ETypeId, dst as NodeId);
    Ok(())
  }

  /// Invalidate a cached key lookup
  fn cache_invalidate_key(&self, key: String) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    db.cache_invalidate_key(&key);
    Ok(())
  }

  /// Clear all caches
  fn cache_clear(&self) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    db.cache_clear();
    Ok(())
  }

  /// Clear only the query cache
  fn cache_clear_query(&self) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    db.cache_clear_query();
    Ok(())
  }

  /// Clear only the key cache
  fn cache_clear_key(&self) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    db.cache_clear_key();
    Ok(())
  }

  /// Clear only the property cache
  fn cache_clear_property(&self) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    db.cache_clear_property();
    Ok(())
  }

  /// Clear only the traversal cache
  fn cache_clear_traversal(&self) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    db.cache_clear_traversal();
    Ok(())
  }

  /// Get cache statistics
  fn cache_stats(&self) -> PyResult<Option<PyCacheStats>> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    Ok(db.cache_stats().map(|s| PyCacheStats {
      property_cache_hits: s.property_cache_hits as i64,
      property_cache_misses: s.property_cache_misses as i64,
      property_cache_size: s.property_cache_size as i64,
      traversal_cache_hits: s.traversal_cache_hits as i64,
      traversal_cache_misses: s.traversal_cache_misses as i64,
      traversal_cache_size: s.traversal_cache_size as i64,
      query_cache_hits: s.query_cache_hits as i64,
      query_cache_misses: s.query_cache_misses as i64,
      query_cache_size: s.query_cache_size as i64,
    }))
  }

  /// Reset cache statistics
  fn cache_reset_stats(&self) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;
    db.cache_reset_stats();
    Ok(())
  }

  // ========================================================================
  // Graph Traversal
  // ========================================================================

  /// Traverse outgoing edges from a node
  ///
  /// Args:
  ///   node_id: Starting node ID
  ///   etype: Edge type ID (optional, None = all types)
  ///
  /// Returns:
  ///   List of neighboring node IDs
  #[pyo3(signature = (node_id, etype=None))]
  fn traverse_out(&self, node_id: i64, etype: Option<u32>) -> PyResult<Vec<i64>> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;

    let edges = db.get_out_edges(node_id as NodeId);
    let results: Vec<i64> = edges
      .into_iter()
      .filter(|(e, _)| etype.is_none() || etype == Some(*e))
      .map(|(_, dst)| dst as i64)
      .collect();
    Ok(results)
  }

  /// Traverse outgoing edges and return (node_id, key) pairs
  ///
  /// This is optimized for the fluent API - gets both ID and key in one FFI call.
  ///
  /// Args:
  ///   node_id: Starting node ID
  ///   etype: Edge type ID (optional, None = all types)
  ///
  /// Returns:
  ///   List of (node_id, key) tuples
  #[pyo3(signature = (node_id, etype=None))]
  fn traverse_out_with_keys(
    &self,
    node_id: i64,
    etype: Option<u32>,
  ) -> PyResult<Vec<(i64, Option<String>)>> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;

    let edges = db.get_out_edges(node_id as NodeId);
    let results: Vec<(i64, Option<String>)> = edges
      .into_iter()
      .filter(|(e, _)| etype.is_none() || etype == Some(*e))
      .map(|(_, dst)| {
        let key = db.get_node_key(dst);
        (dst as i64, key)
      })
      .collect();
    Ok(results)
  }

  /// Count outgoing edges from a node
  ///
  /// This is optimized for counting - doesn't allocate a list.
  ///
  /// Args:
  ///   node_id: Starting node ID
  ///   etype: Edge type ID (optional, None = all types)
  ///
  /// Returns:
  ///   Number of outgoing edges
  #[pyo3(signature = (node_id, etype=None))]
  fn traverse_out_count(&self, node_id: i64, etype: Option<u32>) -> PyResult<i64> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;

    if let Some(et) = etype {
      // Count only specific edge type
      let count = db
        .get_out_edges(node_id as NodeId)
        .into_iter()
        .filter(|(e, _)| *e == et)
        .count();
      Ok(count as i64)
    } else {
      // Count all - use optimized degree function
      Ok(db.get_out_degree(node_id as NodeId) as i64)
    }
  }

  /// Traverse incoming edges to a node
  ///
  /// Args:
  ///   node_id: Target node ID
  ///   etype: Edge type ID (optional, None = all types)
  ///
  /// Returns:
  ///   List of source node IDs
  #[pyo3(signature = (node_id, etype=None))]
  fn traverse_in(&self, node_id: i64, etype: Option<u32>) -> PyResult<Vec<i64>> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;

    let edges = db.get_in_edges(node_id as NodeId);
    let results: Vec<i64> = edges
      .into_iter()
      .filter(|(e, _)| etype.is_none() || etype == Some(*e))
      .map(|(_, src)| src as i64)
      .collect();
    Ok(results)
  }

  /// Traverse incoming edges and return (node_id, key) pairs
  ///
  /// This is optimized for the fluent API - gets both ID and key in one FFI call.
  ///
  /// Args:
  ///   node_id: Target node ID
  ///   etype: Edge type ID (optional, None = all types)
  ///
  /// Returns:
  ///   List of (node_id, key) tuples
  #[pyo3(signature = (node_id, etype=None))]
  fn traverse_in_with_keys(
    &self,
    node_id: i64,
    etype: Option<u32>,
  ) -> PyResult<Vec<(i64, Option<String>)>> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;

    let edges = db.get_in_edges(node_id as NodeId);
    let results: Vec<(i64, Option<String>)> = edges
      .into_iter()
      .filter(|(e, _)| etype.is_none() || etype == Some(*e))
      .map(|(_, src)| {
        let key = db.get_node_key(src);
        (src as i64, key)
      })
      .collect();
    Ok(results)
  }

  /// Count incoming edges to a node
  ///
  /// This is optimized for counting - doesn't allocate a list.
  ///
  /// Args:
  ///   node_id: Target node ID
  ///   etype: Edge type ID (optional, None = all types)
  ///
  /// Returns:
  ///   Number of incoming edges
  #[pyo3(signature = (node_id, etype=None))]
  fn traverse_in_count(&self, node_id: i64, etype: Option<u32>) -> PyResult<i64> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;

    if let Some(et) = etype {
      // Count only specific edge type
      let count = db
        .get_in_edges(node_id as NodeId)
        .into_iter()
        .filter(|(e, _)| *e == et)
        .count();
      Ok(count as i64)
    } else {
      // Count all - use optimized degree function
      Ok(db.get_in_degree(node_id as NodeId) as i64)
    }
  }

  /// Execute a multi-step traversal entirely in Rust
  ///
  /// This is optimized for the fluent API - executes all steps in a single FFI call.
  /// Each step is a tuple of (direction, etype) where direction is "out", "in", or "both".
  ///
  /// Args:
  ///   start_ids: Starting node IDs
  ///   steps: List of (direction, etype) tuples
  ///
  /// Returns:
  ///   List of (node_id, key) tuples for final results
  #[pyo3(signature = (start_ids, steps))]
  fn traverse_multi(
    &self,
    start_ids: Vec<i64>,
    steps: Vec<(String, Option<u32>)>,
  ) -> PyResult<Vec<(i64, Option<String>)>> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;

    let mut current_ids: Vec<NodeId> = start_ids.iter().map(|&id| id as NodeId).collect();

    for (direction, etype) in steps {
      let mut next_ids: Vec<NodeId> = Vec::new();
      let mut visited: HashSet<NodeId> = HashSet::new();

      for node_id in &current_ids {
        let neighbors: Vec<NodeId> = match direction.as_str() {
          "out" => db
            .get_out_edges(*node_id)
            .into_iter()
            .filter(|(e, _)| etype.is_none() || etype == Some(*e))
            .map(|(_, dst)| dst)
            .collect(),
          "in" => db
            .get_in_edges(*node_id)
            .into_iter()
            .filter(|(e, _)| etype.is_none() || etype == Some(*e))
            .map(|(_, src)| src)
            .collect(),
          _ => {
            // "both"
            let mut out: Vec<NodeId> = db
              .get_out_edges(*node_id)
              .into_iter()
              .filter(|(e, _)| etype.is_none() || etype == Some(*e))
              .map(|(_, dst)| dst)
              .collect();
            let in_edges: Vec<NodeId> = db
              .get_in_edges(*node_id)
              .into_iter()
              .filter(|(e, _)| etype.is_none() || etype == Some(*e))
              .map(|(_, src)| src)
              .collect();
            out.extend(in_edges);
            out
          }
        };

        for neighbor_id in neighbors {
          if !visited.contains(&neighbor_id) {
            visited.insert(neighbor_id);
            next_ids.push(neighbor_id);
          }
        }
      }

      current_ids = next_ids;
    }

    // Get keys for final results
    let results: Vec<(i64, Option<String>)> = current_ids
      .into_iter()
      .map(|id| {
        let key = db.get_node_key(id);
        (id as i64, key)
      })
      .collect();

    Ok(results)
  }

  /// Execute a multi-step traversal and return just the count
  ///
  /// This is the fastest option when you only need the count.
  #[pyo3(signature = (start_ids, steps))]
  fn traverse_multi_count(
    &self,
    start_ids: Vec<i64>,
    steps: Vec<(String, Option<u32>)>,
  ) -> PyResult<i64> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;

    let mut current_ids: Vec<NodeId> = start_ids.iter().map(|&id| id as NodeId).collect();

    for (direction, etype) in steps {
      let mut next_ids: Vec<NodeId> = Vec::new();
      let mut visited: HashSet<NodeId> = HashSet::new();

      for node_id in &current_ids {
        let neighbors: Vec<NodeId> = match direction.as_str() {
          "out" => db
            .get_out_edges(*node_id)
            .into_iter()
            .filter(|(e, _)| etype.is_none() || etype == Some(*e))
            .map(|(_, dst)| dst)
            .collect(),
          "in" => db
            .get_in_edges(*node_id)
            .into_iter()
            .filter(|(e, _)| etype.is_none() || etype == Some(*e))
            .map(|(_, src)| src)
            .collect(),
          _ => {
            // "both"
            let mut out: Vec<NodeId> = db
              .get_out_edges(*node_id)
              .into_iter()
              .filter(|(e, _)| etype.is_none() || etype == Some(*e))
              .map(|(_, dst)| dst)
              .collect();
            let in_edges: Vec<NodeId> = db
              .get_in_edges(*node_id)
              .into_iter()
              .filter(|(e, _)| etype.is_none() || etype == Some(*e))
              .map(|(_, src)| src)
              .collect();
            out.extend(in_edges);
            out
          }
        };

        for neighbor_id in neighbors {
          if !visited.contains(&neighbor_id) {
            visited.insert(neighbor_id);
            next_ids.push(neighbor_id);
          }
        }
      }

      current_ids = next_ids;
    }

    Ok(current_ids.len() as i64)
  }

  /// Variable-depth traversal from a node
  ///
  /// Similar to TypeScript: db.from(node).traverse(etype, { maxDepth: 3 })
  ///
  /// Args:
  ///   node_id: Starting node ID
  ///   max_depth: Maximum number of hops
  ///   etype: Edge type ID (optional, None = all types)
  ///   min_depth: Minimum depth (default 1)
  ///   direction: "out", "in", or "both" (default "out")
  ///   unique: Only visit each node once (default True)
  ///
  /// Returns:
  ///   List of TraversalResult objects
  #[pyo3(signature = (node_id, max_depth, etype=None, min_depth=None, direction=None, unique=None))]
  fn traverse(
    &self,
    node_id: i64,
    max_depth: u32,
    etype: Option<u32>,
    min_depth: Option<u32>,
    direction: Option<String>,
    unique: Option<bool>,
  ) -> PyResult<Vec<PyTraversalResult>> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;

    let dir = match direction.as_deref() {
      Some("in") => TraversalDirection::In,
      Some("both") => TraversalDirection::Both,
      _ => TraversalDirection::Out,
    };

    let opts = TraverseOptions {
      direction: dir,
      min_depth: min_depth.unwrap_or(1) as usize,
      max_depth: max_depth as usize,
      unique: unique.unwrap_or(true),
      where_edge: None,
      where_node: None,
    };

    let get_neighbors = |nid: NodeId, d: TraversalDirection, et: Option<ETypeId>| -> Vec<Edge> {
      get_neighbors_from_db(db, nid, d, et)
    };

    let results: Vec<PyTraversalResult> = RustTraversalBuilder::new(vec![node_id as NodeId])
      .traverse(etype, opts)
      .execute(get_neighbors)
      .map(|r| {
        let (edge_src, edge_dst, edge_type) = match r.edge {
          Some(e) => (Some(e.src as i64), Some(e.dst as i64), Some(e.etype)),
          None => (None, None, None),
        };
        PyTraversalResult {
          node_id: r.node_id as i64,
          depth: r.depth as u32,
          edge_src,
          edge_dst,
          edge_type,
        }
      })
      .collect();

    Ok(results)
  }

  // ========================================================================
  // Pathfinding
  // ========================================================================

  /// Find shortest path using BFS (unweighted)
  ///
  /// Similar to TypeScript: db.shortestPath(src).to(dst).bfs()
  ///
  /// Args:
  ///   source: Source node ID
  ///   target: Target node ID
  ///   etype: Edge type to traverse (optional, None = all)
  ///   max_depth: Maximum search depth (default 100)
  ///   direction: "out", "in", or "both" (default "out")
  ///
  /// Returns:
  ///   PathResult with path nodes and edges
  #[pyo3(signature = (source, target, etype=None, max_depth=None, direction=None))]
  fn find_path_bfs(
    &self,
    source: i64,
    target: i64,
    etype: Option<u32>,
    max_depth: Option<u32>,
    direction: Option<String>,
  ) -> PyResult<PyPathResult> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;

    let dir = match direction.as_deref() {
      Some("in") => TraversalDirection::In,
      Some("both") => TraversalDirection::Both,
      _ => TraversalDirection::Out,
    };

    let mut targets = HashSet::new();
    targets.insert(target as NodeId);

    let mut allowed_etypes = HashSet::new();
    if let Some(e) = etype {
      allowed_etypes.insert(e);
    }

    let config = PathConfig {
      source: source as NodeId,
      targets,
      allowed_etypes,
      direction: dir,
      max_depth: max_depth.unwrap_or(100) as usize,
    };

    let get_neighbors = |nid: NodeId, d: TraversalDirection, et: Option<ETypeId>| -> Vec<Edge> {
      get_neighbors_from_db(db, nid, d, et)
    };

    let result = bfs(config, get_neighbors);
    Ok(result.into())
  }

  /// Find shortest path using Dijkstra's algorithm (weighted)
  ///
  /// Similar to TypeScript: db.shortestPath(src).to(dst).dijkstra()
  ///
  /// Note: All edges have weight 1.0 by default. For custom weights,
  /// use find_path_dijkstra_weighted with edge property name.
  ///
  /// Args:
  ///   source: Source node ID
  ///   target: Target node ID
  ///   etype: Edge type to traverse (optional, None = all)
  ///   max_depth: Maximum search depth (default 100)
  ///   direction: "out", "in", or "both" (default "out")
  ///
  /// Returns:
  ///   PathResult with path nodes, edges, and total weight
  #[pyo3(signature = (source, target, etype=None, max_depth=None, direction=None))]
  fn find_path_dijkstra(
    &self,
    source: i64,
    target: i64,
    etype: Option<u32>,
    max_depth: Option<u32>,
    direction: Option<String>,
  ) -> PyResult<PyPathResult> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let db = guard
      .as_ref()
      .ok_or_else(|| PyRuntimeError::new_err("Database is closed"))?;

    let dir = match direction.as_deref() {
      Some("in") => TraversalDirection::In,
      Some("both") => TraversalDirection::Both,
      _ => TraversalDirection::Out,
    };

    let mut targets = HashSet::new();
    targets.insert(target as NodeId);

    let mut allowed_etypes = HashSet::new();
    if let Some(e) = etype {
      allowed_etypes.insert(e);
    }

    let config = PathConfig {
      source: source as NodeId,
      targets,
      allowed_etypes,
      direction: dir,
      max_depth: max_depth.unwrap_or(100) as usize,
    };

    let get_neighbors = |nid: NodeId, d: TraversalDirection, et: Option<ETypeId>| -> Vec<Edge> {
      get_neighbors_from_db(db, nid, d, et)
    };

    let get_weight = |_src: NodeId, _etype: ETypeId, _dst: NodeId| -> f64 {
      1.0 // Default weight
    };

    let result = dijkstra(config, get_neighbors, get_weight);
    Ok(result.into())
  }

  /// Check if a path exists between two nodes
  ///
  /// Args:
  ///   source: Source node ID
  ///   target: Target node ID
  ///   etype: Edge type (optional)
  ///   max_depth: Maximum search depth (default 100)
  ///
  /// Returns:
  ///   True if path exists
  #[pyo3(signature = (source, target, etype=None, max_depth=None))]
  fn has_path(
    &self,
    source: i64,
    target: i64,
    etype: Option<u32>,
    max_depth: Option<u32>,
  ) -> PyResult<bool> {
    let result = self.find_path_bfs(source, target, etype, max_depth, None)?;
    Ok(result.found)
  }

  /// Get all nodes reachable from a source within a certain depth
  ///
  /// Args:
  ///   source: Source node ID
  ///   max_depth: Maximum depth to traverse
  ///   etype: Edge type (optional)
  ///
  /// Returns:
  ///   List of reachable node IDs
  #[pyo3(signature = (source, max_depth, etype=None))]
  fn reachable_nodes(&self, source: i64, max_depth: u32, etype: Option<u32>) -> PyResult<Vec<i64>> {
    let results = self.traverse(
      source,
      max_depth,
      etype,
      Some(1),
      Some("out".to_string()),
      Some(true),
    )?;
    Ok(results.into_iter().map(|r| r.node_id).collect())
  }

  // ========================================================================
  // Context Manager Support
  // ========================================================================

  fn __enter__(slf: Py<Self>) -> Py<Self> {
    slf
  }

  #[pyo3(signature = (_exc_type=None, _exc_value=None, _traceback=None))]
  fn __exit__(
    &self,
    _exc_type: Option<PyObject>,
    _exc_value: Option<PyObject>,
    _traceback: Option<PyObject>,
  ) -> PyResult<bool> {
    self.close()?;
    Ok(false)
  }

  fn __repr__(&self) -> PyResult<String> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    if let Some(db) = guard.as_ref() {
      Ok(format!(
        "Database(path=\"{}\", read_only={})",
        db.path.display(),
        db.read_only
      ))
    } else {
      Ok("Database(closed)".to_string())
    }
  }
}

// ============================================================================
// Convenience Functions
// ============================================================================

/// Open a database file (standalone function)
#[pyfunction]
#[pyo3(signature = (path, options=None))]
pub fn open_database(path: String, options: Option<PyOpenOptions>) -> PyResult<PyDatabase> {
  PyDatabase::new(path, options)
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Get neighbors from database for traversal
fn get_neighbors_from_db(
  db: &RustSingleFileDB,
  node_id: NodeId,
  direction: TraversalDirection,
  etype: Option<ETypeId>,
) -> Vec<Edge> {
  let mut edges = Vec::new();
  match direction {
    TraversalDirection::Out => {
      for (e, dst) in db.get_out_edges(node_id) {
        if etype.is_none() || etype == Some(e) {
          edges.push(Edge {
            src: node_id,
            etype: e,
            dst,
          });
        }
      }
    }
    TraversalDirection::In => {
      for (e, src) in db.get_in_edges(node_id) {
        if etype.is_none() || etype == Some(e) {
          edges.push(Edge {
            src,
            etype: e,
            dst: node_id,
          });
        }
      }
    }
    TraversalDirection::Both => {
      edges.extend(get_neighbors_from_db(
        db,
        node_id,
        TraversalDirection::Out,
        etype,
      ));
      edges.extend(get_neighbors_from_db(
        db,
        node_id,
        TraversalDirection::In,
        etype,
      ));
    }
  }
  edges
}

/// Convert Rust PathResult to Python PathResult
impl From<crate::api::pathfinding::PathResult> for PyPathResult {
  fn from(result: crate::api::pathfinding::PathResult) -> Self {
    Self {
      path: result.path.iter().map(|&id| id as i64).collect(),
      edges: result
        .edges
        .iter()
        .map(|&(src, etype, dst)| PyPathEdge {
          src: src as i64,
          etype,
          dst: dst as i64,
        })
        .collect(),
      total_weight: result.total_weight,
      found: result.found,
    }
  }
}
