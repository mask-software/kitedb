//! Python bindings for KiteDB Database
//!
//! Provides Python access to the single-file database format.
//! This module contains the main Database class and standalone functions.

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use std::path::PathBuf;
use std::sync::RwLock;

use crate::backup as core_backup;
use crate::core::single_file::{
  close_single_file, is_single_file_path, open_single_file, SingleFileDB as RustSingleFileDB,
  VacuumOptions as RustVacuumOptions,
};
use crate::metrics as core_metrics;
use crate::types::{ETypeId, EdgeWithProps as CoreEdgeWithProps, NodeId, PropKeyId};

// Import from modular structure
use super::ops::{
  cache, edges, export_import, graph_traversal, labels, maintenance, nodes, properties, schema,
  streaming as streaming_ops, transaction, vectors,
};
use super::options::{
  BackupOptions, BackupResult, ExportOptions, ExportResult, ImportOptions, ImportResult,
  OfflineBackupOptions, OpenOptions, PaginationOptions, RestoreOptions, SingleFileOptimizeOptions,
  StreamOptions,
};
use super::stats::{CacheStats, CheckResult, DatabaseMetrics, DbStats, HealthCheckResult};
use super::traversal::{PyPathEdge, PyPathResult, PyTraversalResult};
use super::types::{
  Edge, EdgePage, EdgeWithProps, FullEdge, NodePage, NodeProp, NodeWithProps, PropValue,
};

type EdgePropsInput = (i64, u32, i64, Vec<(u32, PropValue)>);

// ============================================================================
// Database Inner Enum
// ============================================================================

pub(crate) enum DatabaseInner {
  SingleFile(Box<RustSingleFileDB>),
}

// ============================================================================
// Dispatch Macros - Eliminate boilerplate for method dispatch
// ============================================================================

/// Dispatch to single-file implementation (immutable, returns PyResult)
/// Uses read lock for concurrent read access
macro_rules! dispatch {
  ($self:expr, |$sf:ident| $sf_expr:expr, |$gf:ident| $gf_expr:expr) => {{
    let guard = $self
      .inner
      .read()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    match guard.as_ref() {
      Some(DatabaseInner::SingleFile($sf)) => $sf_expr,
      None => Err(PyRuntimeError::new_err("Database is closed")),
    }
  }};
}

/// Dispatch returning Ok-wrapped value (immutable)
/// Uses read lock for concurrent read access
macro_rules! dispatch_ok {
  ($self:expr, |$sf:ident| $sf_expr:expr, |$gf:ident| $gf_expr:expr) => {{
    let guard = $self
      .inner
      .read()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    match guard.as_ref() {
      Some(DatabaseInner::SingleFile($sf)) => Ok($sf_expr),
      None => Err(PyRuntimeError::new_err("Database is closed")),
    }
  }};
}

/// Dispatch to mutable single-file implementation
/// Uses write lock for exclusive access
macro_rules! dispatch_mut {
  ($self:expr, |$sf:ident| $sf_expr:expr, |$gf:ident| $gf_expr:expr) => {{
    let mut guard = $self
      .inner
      .write()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    match guard.as_mut() {
      Some(DatabaseInner::SingleFile($sf)) => $sf_expr,
      None => Err(PyRuntimeError::new_err("Database is closed")),
    }
  }};
}

/// Dispatch for write operations
macro_rules! dispatch_tx {
  ($self:expr, |$sf:ident| $sf_expr:expr, |$handle:ident| $gf_expr:expr) => {{
    let guard = $self
      .inner
      .read()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    match guard.as_ref() {
      Some(DatabaseInner::SingleFile($sf)) => $sf_expr,
      None => Err(PyRuntimeError::new_err("Database is closed")),
    }
  }};
}

// ============================================================================
// Database Python Wrapper
// ============================================================================

/// Single-file database handle.
///
/// # Thread Safety and Concurrent Access
///
/// The Database class uses an internal RwLock to support concurrent operations:
///
/// - **Read operations** (`get_node_by_key`, `node_exists`, `get_neighbors`, etc.)
///   use a shared read lock, allowing multiple threads to read concurrently.
/// - **Write operations** (`create_node`, `add_edge`, `set_node_prop`, etc.)
///   use an exclusive write lock, blocking all other operations.
///
/// Example of concurrent reads from multiple threads:
///
/// ```python
/// from concurrent.futures import ThreadPoolExecutor
///
/// def read_node(key):
///     return db.node_by_key(key)
///
/// # These execute concurrently
/// with ThreadPoolExecutor(max_workers=4) as executor:
///     results = list(executor.map(read_node, ["user:1", "user:2", "user:3"]))
/// ```
///
/// Note: Python's GIL is released during Rust operations, enabling true
/// parallelism for database I/O operations.
#[pyclass(name = "Database")]
pub struct PyDatabase {
  pub(crate) inner: RwLock<Option<DatabaseInner>>,
}

#[pymethods]
impl PyDatabase {
  // ==========================================================================
  // Constructor and Lifecycle
  // ==========================================================================

  #[new]
  #[pyo3(signature = (path, options=None))]
  fn new(path: String, options: Option<OpenOptions>) -> PyResult<Self> {
    let options = options.unwrap_or_default();
    let path_buf = PathBuf::from(&path);

    if path_buf.exists() && path_buf.is_dir() {
      return Err(PyRuntimeError::new_err(
        "Single-file databases require a file path, not a directory",
      ));
    }

    let db_path = if is_single_file_path(&path_buf) {
      path_buf
    } else if path_buf.extension().is_none() {
      PathBuf::from(format!("{path}.kitedb"))
    } else {
      return Err(PyRuntimeError::new_err(
        "Single-file databases must use the .kitedb extension",
      ));
    };

    let opts = options
      .to_single_file_options()
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to parse options: {e}")))?;
    let db = open_single_file(&db_path, opts)
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to open database: {e}")))?;
    Ok(PyDatabase {
      inner: RwLock::new(Some(DatabaseInner::SingleFile(Box::new(db)))),
    })
  }

  fn close(&self) -> PyResult<()> {
    let mut guard = self
      .inner
      .write()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    if let Some(db) = guard.take() {
      match db {
        DatabaseInner::SingleFile(db) => close_single_file(*db)
          .map_err(|e| PyRuntimeError::new_err(format!("Failed to close: {e}")))?,
      }
    }
    Ok(())
  }

  fn __enter__(slf: PyRef<'_, Self>) -> PyResult<PyRef<'_, Self>> {
    Ok(slf)
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

  #[getter]
  fn is_open(&self) -> PyResult<bool> {
    Ok(
      self
        .inner
        .read()
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))?
        .is_some(),
    )
  }

  #[getter]
  fn path(&self) -> PyResult<String> {
    dispatch_ok!(self, |db| db.path.to_string_lossy().to_string(), |db| db
      .path
      .to_string_lossy()
      .to_string())
  }

  #[getter]
  fn read_only(&self) -> PyResult<bool> {
    dispatch_ok!(self, |db| db.read_only, |db| db.read_only)
  }

  // ==========================================================================
  // Transaction Methods
  // ==========================================================================

  #[pyo3(signature = (read_only=None))]
  fn begin(&self, read_only: Option<bool>) -> PyResult<i64> {
    let read_only = read_only.unwrap_or(false);
    dispatch!(
      self,
      |db| transaction::begin_single_file(db, read_only),
      |_db| { unreachable!("multi-file database support removed") }
    )
  }

  /// Begin a bulk-load transaction (fast path, MVCC disabled)
  fn begin_bulk(&self) -> PyResult<i64> {
    dispatch!(self, |db| transaction::begin_bulk_single_file(db), |_db| {
      unreachable!("multi-file database support removed")
    })
  }

  fn commit(&self) -> PyResult<()> {
    dispatch!(self, |db| transaction::commit_single_file(db), |_db| {
      unreachable!("multi-file database support removed")
    })
  }

  fn rollback(&self) -> PyResult<()> {
    dispatch!(self, |db| transaction::rollback_single_file(db), |_db| {
      unreachable!("multi-file database support removed")
    })
  }

  fn has_transaction(&self) -> PyResult<bool> {
    dispatch_ok!(self, |db| db.has_transaction(), |_db| false)
  }

  // ==========================================================================
  // Node Operations
  // ==========================================================================

  #[pyo3(signature = (key=None))]
  fn create_node(&self, key: Option<String>) -> PyResult<i64> {
    dispatch_tx!(
      self,
      |db| nodes::create_node_single(db, key.as_deref()),
      |h| nodes::create_node_single(h, key.clone())
    )
  }

  fn delete_node(&self, node_id: i64) -> PyResult<()> {
    dispatch_tx!(
      self,
      |db| nodes::delete_node_single(db, node_id as NodeId),
      |h| nodes::delete_node_single(h, node_id as NodeId)
    )
  }

  fn node_exists(&self, node_id: i64) -> PyResult<bool> {
    dispatch_ok!(
      self,
      |db| nodes::node_exists_single(db, node_id as NodeId),
      |db| nodes::node_exists_single(db, node_id as NodeId)
    )
  }

  fn get_node_by_key(&self, key: &str) -> PyResult<Option<i64>> {
    dispatch_ok!(self, |db| nodes::get_node_by_key_single(db, key), |db| {
      nodes::get_node_by_key_single(db, key)
    })
  }

  fn get_node_key(&self, node_id: i64) -> PyResult<Option<String>> {
    dispatch_ok!(
      self,
      |db| nodes::get_node_key_single(db, node_id as NodeId),
      |db| nodes::get_node_key_single(db, node_id as NodeId)
    )
  }

  fn list_nodes(&self) -> PyResult<Vec<i64>> {
    dispatch_ok!(self, |db| nodes::list_nodes_single(db), |db| {
      nodes::list_nodes_single(db)
    })
  }

  fn count_nodes(&self) -> PyResult<i64> {
    dispatch_ok!(self, |db| nodes::count_nodes_single(db), |db| {
      nodes::count_nodes_single(db)
    })
  }

  fn list_nodes_with_prefix(&self, prefix: &str) -> PyResult<Vec<i64>> {
    dispatch_ok!(
      self,
      |db| nodes::list_nodes_with_prefix_single(db, prefix),
      |db| nodes::list_nodes_with_prefix_single(db, prefix)
    )
  }

  fn count_nodes_with_prefix(&self, prefix: &str) -> PyResult<i64> {
    dispatch_ok!(
      self,
      |db| nodes::count_nodes_with_prefix_single(db, prefix),
      |db| nodes::count_nodes_with_prefix_single(db, prefix)
    )
  }

  fn batch_create_nodes(
    &self,
    input_nodes: Vec<(String, Vec<(u32, PropValue)>)>,
  ) -> PyResult<Vec<i64>> {
    let guard = self
      .inner
      .read()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    match guard.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        let mut ids = Vec::with_capacity(input_nodes.len());
        let mut keys = Vec::with_capacity(input_nodes.len());
        let mut props_list = Vec::with_capacity(input_nodes.len());
        for (key, props) in input_nodes {
          keys.push(key);
          props_list.push(props);
        }

        db.begin_bulk()
          .map_err(|e| PyRuntimeError::new_err(format!("Failed to begin bulk: {e}")))?;
        let result: Result<(), PyErr> = (|| {
          let key_refs: Vec<Option<&str>> = keys.iter().map(|k| Some(k.as_str())).collect();
          let node_ids = db
            .create_nodes_batch(&key_refs)
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
          for (node_id, props) in node_ids.iter().copied().zip(props_list.iter()) {
            for (k, v) in props.iter() {
              db.set_node_prop(node_id, *k as PropKeyId, v.clone().into())
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
            }
            ids.push(node_id as i64);
          }
          Ok(())
        })();
        match result {
          Ok(()) => {
            db.commit()
              .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
            Ok(ids)
          }
          Err(e) => {
            let _ = db.rollback();
            Err(e)
          }
        }
      }
      None => Err(PyRuntimeError::new_err("Database is closed")),
    }
  }

  /// Create multiple nodes in a single WAL record (fast path)
  fn create_nodes_batch(&self, keys: Vec<Option<String>>) -> PyResult<Vec<i64>> {
    let guard = self
      .inner
      .read()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    match guard.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        let key_refs: Vec<Option<&str>> = keys.iter().map(|k| k.as_deref()).collect();
        let node_ids = db
          .create_nodes_batch(&key_refs)
          .map_err(|e| PyRuntimeError::new_err(format!("Failed to create nodes: {e}")))?;
        Ok(node_ids.into_iter().map(|id| id as i64).collect())
      }
      None => Err(PyRuntimeError::new_err("Database is closed")),
    }
  }

  /// Add multiple edges in a single WAL record (fast path)
  fn add_edges_batch(&self, edges: Vec<(i64, u32, i64)>) -> PyResult<()> {
    let guard = self
      .inner
      .read()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    match guard.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        let core_edges: Vec<(NodeId, ETypeId, NodeId)> = edges
          .into_iter()
          .map(|(src, etype, dst)| (src as NodeId, etype as ETypeId, dst as NodeId))
          .collect();
        db.add_edges_batch(&core_edges)
          .map_err(|e| PyRuntimeError::new_err(format!("Failed to add edges: {e}")))
      }
      None => Err(PyRuntimeError::new_err("Database is closed")),
    }
  }

  /// Add multiple edges with props in a single WAL record (fast path)
  fn add_edges_with_props_batch(&self, edges: Vec<EdgePropsInput>) -> PyResult<()> {
    let guard = self
      .inner
      .read()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    match guard.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        let core_edges: Vec<CoreEdgeWithProps> = edges
          .into_iter()
          .map(|(src, etype, dst, props)| {
            let core_props = props
              .into_iter()
              .map(|(key_id, value)| (key_id as PropKeyId, value.into()))
              .collect();
            (src as NodeId, etype as ETypeId, dst as NodeId, core_props)
          })
          .collect();
        db.add_edges_with_props_batch(core_edges)
          .map_err(|e| PyRuntimeError::new_err(format!("Failed to add edges: {e}")))
      }
      None => Err(PyRuntimeError::new_err("Database is closed")),
    }
  }

  fn upsert_node(&self, key: String, props: Vec<(u32, Option<PropValue>)>) -> PyResult<i64> {
    let core_props: Vec<(PropKeyId, Option<crate::types::PropValue>)> = props
      .into_iter()
      .map(|(k, v)| (k as PropKeyId, v.map(|value| value.into())))
      .collect();

    dispatch_tx!(
      self,
      |db| nodes::upsert_node_single(db, &key, &core_props),
      |h| nodes::upsert_node_single(h, &key, &core_props)
    )
  }

  fn upsert_node_by_id(&self, node_id: i64, props: Vec<(u32, Option<PropValue>)>) -> PyResult<i64> {
    let core_props: Vec<(PropKeyId, Option<crate::types::PropValue>)> = props
      .into_iter()
      .map(|(k, v)| (k as PropKeyId, v.map(|value| value.into())))
      .collect();

    dispatch_tx!(
      self,
      |db| nodes::upsert_node_by_id_single(db, node_id as NodeId, &core_props),
      |h| nodes::upsert_node_by_id_single(h, node_id as NodeId, &core_props)
    )
  }

  // ==========================================================================
  // Edge Operations
  // ==========================================================================

  fn add_edge(&self, src: i64, etype: u32, dst: i64) -> PyResult<()> {
    dispatch_tx!(
      self,
      |db| edges::add_edge_single(db, src as NodeId, etype as ETypeId, dst as NodeId),
      |h| edges::add_edge_single(h, src as NodeId, etype as ETypeId, dst as NodeId)
    )
  }

  fn add_edge_by_name(&self, src: i64, etype_name: &str, dst: i64) -> PyResult<()> {
    let guard = self
      .inner
      .read()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    match guard.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        edges::add_edge_by_name_single(db, src as NodeId, etype_name, dst as NodeId)
      }
      None => Err(PyRuntimeError::new_err("Database is closed")),
    }
  }

  fn delete_edge(&self, src: i64, etype: u32, dst: i64) -> PyResult<()> {
    dispatch_tx!(
      self,
      |db| edges::delete_edge_single(db, src as NodeId, etype as ETypeId, dst as NodeId),
      |h| edges::delete_edge_single(h, src as NodeId, etype as ETypeId, dst as NodeId)
    )
  }

  fn upsert_edge(
    &self,
    src: i64,
    etype: u32,
    dst: i64,
    props: Vec<(u32, Option<PropValue>)>,
  ) -> PyResult<bool> {
    let core_props: Vec<(PropKeyId, Option<crate::types::PropValue>)> = props
      .into_iter()
      .map(|(k, v)| (k as PropKeyId, v.map(|value| value.into())))
      .collect();

    dispatch_tx!(
      self,
      |db| edges::upsert_edge_single(
        db,
        src as NodeId,
        etype as ETypeId,
        dst as NodeId,
        &core_props
      ),
      |h| edges::upsert_edge_single(
        h,
        src as NodeId,
        etype as ETypeId,
        dst as NodeId,
        &core_props
      )
    )
  }

  fn edge_exists(&self, src: i64, etype: u32, dst: i64) -> PyResult<bool> {
    dispatch_ok!(
      self,
      |db| edges::edge_exists_single(db, src as NodeId, etype as ETypeId, dst as NodeId),
      |db| edges::edge_exists_single(db, src as NodeId, etype as ETypeId, dst as NodeId)
    )
  }

  fn get_out_edges(&self, node_id: i64) -> PyResult<Vec<Edge>> {
    dispatch_ok!(
      self,
      |db| edges::get_out_edges_single(db, node_id as NodeId),
      |db| edges::get_out_edges_single(db, node_id as NodeId)
    )
  }

  fn get_in_edges(&self, node_id: i64) -> PyResult<Vec<Edge>> {
    dispatch_ok!(
      self,
      |db| edges::get_in_edges_single(db, node_id as NodeId),
      |db| edges::get_in_edges_single(db, node_id as NodeId)
    )
  }

  fn get_out_degree(&self, node_id: i64) -> PyResult<i64> {
    dispatch_ok!(
      self,
      |db| edges::get_out_degree_single(db, node_id as NodeId),
      |db| edges::get_out_degree_single(db, node_id as NodeId)
    )
  }

  fn get_in_degree(&self, node_id: i64) -> PyResult<i64> {
    dispatch_ok!(
      self,
      |db| edges::get_in_degree_single(db, node_id as NodeId),
      |db| edges::get_in_degree_single(db, node_id as NodeId)
    )
  }

  fn count_edges(&self) -> PyResult<i64> {
    dispatch_ok!(self, |db| edges::count_edges_single(db), |db| {
      edges::count_edges_single(db, None)
    })
  }

  fn count_edges_by_type(&self, etype: u32) -> PyResult<i64> {
    dispatch_ok!(
      self,
      |db| edges::count_edges_by_type_single(db, etype as ETypeId),
      |db| edges::count_edges_single(db, Some(etype as ETypeId))
    )
  }

  #[pyo3(signature = (etype=None))]
  fn list_edges(&self, etype: Option<u32>) -> PyResult<Vec<FullEdge>> {
    dispatch_ok!(
      self,
      |db| edges::list_edges_single(db, etype.map(|e| e as ETypeId)),
      |db| edges::list_edges_single(db, etype.map(|e| e as ETypeId))
    )
  }

  // ==========================================================================
  // Property Operations
  // ==========================================================================

  fn set_node_prop(&self, node_id: i64, key_id: u32, value: PropValue) -> PyResult<()> {
    dispatch_tx!(
      self,
      |db| properties::set_node_prop_single(
        db,
        node_id as NodeId,
        key_id as PropKeyId,
        value.into()
      ),
      |h| properties::set_node_prop_single(
        h,
        node_id as NodeId,
        key_id as PropKeyId,
        value.clone().into()
      )
    )
  }

  fn set_node_prop_by_name(&self, node_id: i64, key_name: &str, value: PropValue) -> PyResult<()> {
    let guard = self
      .inner
      .read()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    match guard.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        properties::set_node_prop_by_name_single(db, node_id as NodeId, key_name, value.into())
      }
      None => Err(PyRuntimeError::new_err("Database is closed")),
    }
  }

  fn get_node_prop(&self, node_id: i64, key_id: u32) -> PyResult<Option<PropValue>> {
    dispatch_ok!(
      self,
      |db| properties::get_node_prop_single(db, node_id as NodeId, key_id as PropKeyId),
      |db| properties::get_node_prop_single(db, node_id as NodeId, key_id as PropKeyId)
    )
  }

  fn delete_node_prop(&self, node_id: i64, key_id: u32) -> PyResult<()> {
    dispatch_tx!(
      self,
      |db| properties::delete_node_prop_single(db, node_id as NodeId, key_id as PropKeyId),
      |h| properties::delete_node_prop_single(h, node_id as NodeId, key_id as PropKeyId)
    )
  }

  fn get_node_props(&self, node_id: i64) -> PyResult<Option<Vec<NodeProp>>> {
    dispatch_ok!(
      self,
      |db| properties::get_node_props_single(db, node_id as NodeId),
      |db| properties::get_node_props_single(db, node_id as NodeId)
    )
  }

  fn set_edge_prop(
    &self,
    src: i64,
    etype: u32,
    dst: i64,
    key_id: u32,
    value: PropValue,
  ) -> PyResult<()> {
    dispatch_tx!(
      self,
      |db| properties::set_edge_prop_single(
        db,
        src as NodeId,
        etype as ETypeId,
        dst as NodeId,
        key_id as PropKeyId,
        value.into()
      ),
      |h| properties::set_edge_prop_single(
        h,
        src as NodeId,
        etype as ETypeId,
        dst as NodeId,
        key_id as PropKeyId,
        value.clone().into()
      )
    )
  }

  fn set_edge_prop_by_name(
    &self,
    src: i64,
    etype: u32,
    dst: i64,
    key_name: &str,
    value: PropValue,
  ) -> PyResult<()> {
    let guard = self
      .inner
      .read()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    match guard.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => properties::set_edge_prop_by_name_single(
        db,
        src as NodeId,
        etype as ETypeId,
        dst as NodeId,
        key_name,
        value.into(),
      ),
      None => Err(PyRuntimeError::new_err("Database is closed")),
    }
  }

  fn get_edge_prop(
    &self,
    src: i64,
    etype: u32,
    dst: i64,
    key_id: u32,
  ) -> PyResult<Option<PropValue>> {
    dispatch_ok!(
      self,
      |db| properties::get_edge_prop_single(
        db,
        src as NodeId,
        etype as ETypeId,
        dst as NodeId,
        key_id as PropKeyId
      ),
      |db| properties::get_edge_prop_single(
        db,
        src as NodeId,
        etype as ETypeId,
        dst as NodeId,
        key_id as PropKeyId
      )
    )
  }

  fn delete_edge_prop(&self, src: i64, etype: u32, dst: i64, key_id: u32) -> PyResult<()> {
    dispatch_tx!(
      self,
      |db| properties::delete_edge_prop_single(
        db,
        src as NodeId,
        etype as ETypeId,
        dst as NodeId,
        key_id as PropKeyId
      ),
      |h| properties::delete_edge_prop_single(
        h,
        src as NodeId,
        etype as ETypeId,
        dst as NodeId,
        key_id as PropKeyId
      )
    )
  }

  fn get_edge_props(&self, src: i64, etype: u32, dst: i64) -> PyResult<Option<Vec<NodeProp>>> {
    dispatch_ok!(
      self,
      |db| properties::get_edge_props_single(db, src as NodeId, etype as ETypeId, dst as NodeId),
      |db| properties::get_edge_props_single(db, src as NodeId, etype as ETypeId, dst as NodeId)
    )
  }

  // Direct type property getters
  fn get_node_prop_string(&self, node_id: i64, key_id: u32) -> PyResult<Option<String>> {
    dispatch_ok!(
      self,
      |db| properties::get_node_prop_string_single(db, node_id as NodeId, key_id as PropKeyId),
      |db| properties::get_node_prop_string_single(db, node_id as NodeId, key_id as PropKeyId)
    )
  }

  fn get_node_prop_int(&self, node_id: i64, key_id: u32) -> PyResult<Option<i64>> {
    dispatch_ok!(
      self,
      |db| properties::get_node_prop_int_single(db, node_id as NodeId, key_id as PropKeyId),
      |db| properties::get_node_prop_int_single(db, node_id as NodeId, key_id as PropKeyId)
    )
  }

  fn get_node_prop_float(&self, node_id: i64, key_id: u32) -> PyResult<Option<f64>> {
    dispatch_ok!(
      self,
      |db| properties::get_node_prop_float_single(db, node_id as NodeId, key_id as PropKeyId),
      |db| properties::get_node_prop_float_single(db, node_id as NodeId, key_id as PropKeyId)
    )
  }

  fn get_node_prop_bool(&self, node_id: i64, key_id: u32) -> PyResult<Option<bool>> {
    dispatch_ok!(
      self,
      |db| properties::get_node_prop_bool_single(db, node_id as NodeId, key_id as PropKeyId),
      |db| properties::get_node_prop_bool_single(db, node_id as NodeId, key_id as PropKeyId)
    )
  }

  // ==========================================================================
  // Schema Operations
  // ==========================================================================

  fn get_or_create_label(&self, name: &str) -> PyResult<u32> {
    dispatch_ok!(
      self,
      |db| schema::get_or_create_label_single(db, name),
      |db| schema::get_or_create_label_single(db, name)
    )
  }

  fn get_label_id(&self, name: &str) -> PyResult<Option<u32>> {
    dispatch_ok!(self, |db| schema::get_label_id_single(db, name), |db| {
      schema::get_label_id_single(db, name)
    })
  }

  fn get_label_name(&self, id: u32) -> PyResult<Option<String>> {
    dispatch_ok!(self, |db| schema::get_label_name_single(db, id), |db| {
      schema::get_label_name_single(db, id)
    })
  }

  fn get_or_create_etype(&self, name: &str) -> PyResult<u32> {
    dispatch_ok!(
      self,
      |db| schema::get_or_create_etype_single(db, name),
      |db| schema::get_or_create_etype_single(db, name)
    )
  }

  fn get_etype_id(&self, name: &str) -> PyResult<Option<u32>> {
    dispatch_ok!(self, |db| schema::get_etype_id_single(db, name), |db| {
      schema::get_etype_id_single(db, name)
    })
  }

  fn get_etype_name(&self, id: u32) -> PyResult<Option<String>> {
    dispatch_ok!(self, |db| schema::etype_name_single(db, id), |db| {
      schema::etype_name_single(db, id)
    })
  }

  fn get_or_create_propkey(&self, name: &str) -> PyResult<u32> {
    dispatch_ok!(
      self,
      |db| schema::get_or_create_propkey_single(db, name),
      |db| schema::get_or_create_propkey_single(db, name)
    )
  }

  fn get_propkey_id(&self, name: &str) -> PyResult<Option<u32>> {
    dispatch_ok!(self, |db| schema::get_propkey_id_single(db, name), |db| {
      schema::get_propkey_id_single(db, name)
    })
  }

  fn get_propkey_name(&self, id: u32) -> PyResult<Option<String>> {
    dispatch_ok!(self, |db| schema::get_propkey_name_single(db, id), |db| {
      schema::get_propkey_name_single(db, id)
    })
  }

  // ==========================================================================
  // Label Operations
  // ==========================================================================

  fn define_label(&self, name: &str) -> PyResult<u32> {
    dispatch_tx!(self, |db| labels::define_label_single(db, name), |h| {
      labels::define_label_single(h, name)
    })
  }

  fn add_node_label(&self, node_id: i64, label_id: u32) -> PyResult<()> {
    dispatch_tx!(
      self,
      |db| labels::add_node_label_single(db, node_id as NodeId, label_id),
      |h| labels::add_node_label_single(h, node_id as NodeId, label_id)
    )
  }

  fn add_node_label_by_name(&self, node_id: i64, label_name: &str) -> PyResult<()> {
    let guard = self
      .inner
      .read()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    match guard.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        labels::add_node_label_by_name_single(db, node_id as NodeId, label_name)
      }
      None => Err(PyRuntimeError::new_err("Database is closed")),
    }
  }

  fn remove_node_label(&self, node_id: i64, label_id: u32) -> PyResult<()> {
    dispatch_tx!(
      self,
      |db| labels::remove_node_label_single(db, node_id as NodeId, label_id),
      |h| labels::remove_node_label_single(h, node_id as NodeId, label_id)
    )
  }

  fn node_has_label(&self, node_id: i64, label_id: u32) -> PyResult<bool> {
    dispatch_ok!(
      self,
      |db| labels::node_has_label_single(db, node_id as NodeId, label_id),
      |db| labels::node_has_label_single(db, node_id as NodeId, label_id)
    )
  }

  fn get_node_labels(&self, node_id: i64) -> PyResult<Vec<u32>> {
    dispatch_ok!(
      self,
      |db| labels::get_node_labels_single(db, node_id as NodeId),
      |db| labels::get_node_labels_single(db, node_id as NodeId)
    )
  }

  // ==========================================================================
  // Vector Operations
  // ==========================================================================

  fn set_node_vector(&self, node_id: i64, prop_key_id: u32, vector: Vec<f64>) -> PyResult<()> {
    let v: Vec<f32> = vector.iter().map(|&x| x as f32).collect();
    dispatch_tx!(
      self,
      |db| vectors::set_node_vector_single(db, node_id as NodeId, prop_key_id as PropKeyId, &v),
      |h| vectors::set_node_vector_single(h, node_id as NodeId, prop_key_id as PropKeyId, &v)
    )
  }

  fn get_node_vector(&self, node_id: i64, prop_key_id: u32) -> PyResult<Option<Vec<f64>>> {
    dispatch_ok!(
      self,
      |db| vectors::get_node_vector_single(db, node_id as NodeId, prop_key_id as PropKeyId),
      |db| vectors::get_node_vector_single(db, node_id as NodeId, prop_key_id as PropKeyId)
    )
  }

  fn delete_node_vector(&self, node_id: i64, prop_key_id: u32) -> PyResult<()> {
    dispatch_tx!(
      self,
      |db| vectors::delete_node_vector_single(db, node_id as NodeId, prop_key_id as PropKeyId),
      |h| vectors::delete_node_vector_single(h, node_id as NodeId, prop_key_id as PropKeyId)
    )
  }

  fn has_node_vector(&self, node_id: i64, prop_key_id: u32) -> PyResult<bool> {
    dispatch_ok!(
      self,
      |db| vectors::has_node_vector_single(db, node_id as NodeId, prop_key_id as PropKeyId),
      |db| vectors::has_node_vector_single(db, node_id as NodeId, prop_key_id as PropKeyId)
    )
  }

  // ==========================================================================
  // Traversal Operations
  // ==========================================================================

  #[pyo3(signature = (node_id, etype=None))]
  fn traverse_out(&self, node_id: i64, etype: Option<u32>) -> PyResult<Vec<i64>> {
    dispatch_ok!(
      self,
      |db| graph_traversal::traverse_out_single(db, node_id as NodeId, etype),
      |db| graph_traversal::traverse_out_single(db, node_id as NodeId, etype)
    )
  }

  #[pyo3(signature = (node_id, etype=None))]
  fn traverse_out_with_keys(
    &self,
    node_id: i64,
    etype: Option<u32>,
  ) -> PyResult<Vec<(i64, Option<String>)>> {
    dispatch_ok!(
      self,
      |db| graph_traversal::traverse_out_with_keys_single(db, node_id as NodeId, etype),
      |db| graph_traversal::traverse_out_with_keys_single(db, node_id as NodeId, etype)
    )
  }

  #[pyo3(signature = (node_id, etype=None))]
  fn traverse_out_count(&self, node_id: i64, etype: Option<u32>) -> PyResult<i64> {
    dispatch_ok!(
      self,
      |db| graph_traversal::traverse_out_count_single(db, node_id as NodeId, etype),
      |db| graph_traversal::traverse_out_count_single(db, node_id as NodeId, etype)
    )
  }

  #[pyo3(signature = (node_id, etype=None))]
  fn traverse_in(&self, node_id: i64, etype: Option<u32>) -> PyResult<Vec<i64>> {
    dispatch_ok!(
      self,
      |db| graph_traversal::traverse_in_single(db, node_id as NodeId, etype),
      |db| graph_traversal::traverse_in_single(db, node_id as NodeId, etype)
    )
  }

  #[pyo3(signature = (node_id, etype=None))]
  fn traverse_in_with_keys(
    &self,
    node_id: i64,
    etype: Option<u32>,
  ) -> PyResult<Vec<(i64, Option<String>)>> {
    dispatch_ok!(
      self,
      |db| graph_traversal::traverse_in_with_keys_single(db, node_id as NodeId, etype),
      |db| graph_traversal::traverse_in_with_keys_single(db, node_id as NodeId, etype)
    )
  }

  #[pyo3(signature = (node_id, etype=None))]
  fn traverse_in_count(&self, node_id: i64, etype: Option<u32>) -> PyResult<i64> {
    dispatch_ok!(
      self,
      |db| graph_traversal::traverse_in_count_single(db, node_id as NodeId, etype),
      |db| graph_traversal::traverse_in_count_single(db, node_id as NodeId, etype)
    )
  }

  fn traverse_multi(
    &self,
    start_ids: Vec<i64>,
    steps: Vec<(String, Option<u32>)>,
  ) -> PyResult<Vec<(i64, Option<String>)>> {
    dispatch_ok!(
      self,
      |db| graph_traversal::traverse_multi_single(db, start_ids.clone(), steps.clone()),
      |db| graph_traversal::traverse_multi_single(db, start_ids.clone(), steps.clone())
    )
  }

  fn traverse_multi_count(
    &self,
    start_ids: Vec<i64>,
    steps: Vec<(String, Option<u32>)>,
  ) -> PyResult<i64> {
    dispatch_ok!(
      self,
      |db| graph_traversal::traverse_multi_count_single(db, start_ids.clone(), steps.clone()),
      |db| graph_traversal::traverse_multi_count_single(db, start_ids.clone(), steps.clone())
    )
  }

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
    dispatch_ok!(
      self,
      |db| graph_traversal::traverse_single(
        db,
        node_id as NodeId,
        max_depth,
        etype,
        min_depth,
        direction.clone(),
        unique
      ),
      |db| graph_traversal::traverse_single(
        db,
        node_id as NodeId,
        max_depth,
        etype,
        min_depth,
        direction.clone(),
        unique
      )
    )
  }

  #[pyo3(signature = (source, target, etype=None, max_depth=None, direction=None))]
  fn find_path_bfs(
    &self,
    source: i64,
    target: i64,
    etype: Option<u32>,
    max_depth: Option<u32>,
    direction: Option<String>,
  ) -> PyResult<PyPathResult> {
    dispatch_ok!(
      self,
      |db| graph_traversal::find_path_bfs_single(
        db,
        source as NodeId,
        target as NodeId,
        etype,
        max_depth,
        direction.clone()
      ),
      |db| graph_traversal::find_path_bfs_single(
        db,
        source as NodeId,
        target as NodeId,
        etype,
        max_depth,
        direction.clone()
      )
    )
  }

  #[pyo3(signature = (source, target, etype=None, max_depth=None, direction=None))]
  fn find_path_dijkstra(
    &self,
    source: i64,
    target: i64,
    etype: Option<u32>,
    max_depth: Option<u32>,
    direction: Option<String>,
  ) -> PyResult<PyPathResult> {
    dispatch_ok!(
      self,
      |db| graph_traversal::find_path_dijkstra_single(
        db,
        source as NodeId,
        target as NodeId,
        etype,
        max_depth,
        direction.clone()
      ),
      |db| graph_traversal::find_path_dijkstra_single(
        db,
        source as NodeId,
        target as NodeId,
        etype,
        max_depth,
        direction.clone()
      )
    )
  }

  #[pyo3(signature = (source, target, etype=None, max_depth=None, direction=None))]
  fn has_path(
    &self,
    source: i64,
    target: i64,
    etype: Option<u32>,
    max_depth: Option<u32>,
    direction: Option<String>,
  ) -> PyResult<bool> {
    let path = dispatch_ok!(
      self,
      |db| graph_traversal::find_path_bfs_single(
        db,
        source as NodeId,
        target as NodeId,
        etype,
        max_depth,
        direction.clone()
      ),
      |db| graph_traversal::find_path_bfs_single(
        db,
        source as NodeId,
        target as NodeId,
        etype,
        max_depth,
        direction.clone()
      )
    )?;
    Ok(path.found)
  }

  #[pyo3(signature = (source, max_depth, etype=None))]
  fn reachable_nodes(&self, source: i64, max_depth: u32, etype: Option<u32>) -> PyResult<Vec<i64>> {
    let min_depth = Some(1);
    let direction = Some("out".to_string());
    let unique = Some(true);
    let results = dispatch_ok!(
      self,
      |db| graph_traversal::traverse_single(
        db,
        source as NodeId,
        max_depth,
        etype,
        min_depth,
        direction.clone(),
        unique
      ),
      |db| graph_traversal::traverse_single(
        db,
        source as NodeId,
        max_depth,
        etype,
        min_depth,
        direction.clone(),
        unique
      )
    )?;
    Ok(results.into_iter().map(|r| r.node_id).collect())
  }

  // ==========================================================================
  // Maintenance Operations
  // ==========================================================================

  fn checkpoint(&self) -> PyResult<()> {
    dispatch!(self, |db| maintenance::checkpoint_single(db), |_db| Ok(()))
  }

  fn background_checkpoint(&self) -> PyResult<()> {
    dispatch!(
      self,
      |db| maintenance::background_checkpoint_single(db),
      |_db| Ok(())
    )
  }

  #[pyo3(signature = (threshold=0.5))]
  fn should_checkpoint(&self, threshold: f64) -> PyResult<bool> {
    dispatch_ok!(
      self,
      |db| maintenance::should_checkpoint_single(db, threshold),
      |_db| false
    )
  }

  #[pyo3(signature = (options=None))]
  fn optimize(&mut self, options: Option<SingleFileOptimizeOptions>) -> PyResult<()> {
    dispatch_mut!(
      self,
      |db| {
        let opts = match options {
          Some(o) => Some(o.to_core()?),
          None => None,
        };
        maintenance::optimize_single(db, opts)
      },
      |db| maintenance::optimize_single(db)
    )
  }

  #[pyo3(signature = (shrink_wal=true, min_wal_size=None))]
  fn vacuum(&mut self, shrink_wal: bool, min_wal_size: Option<u64>) -> PyResult<()> {
    dispatch_mut!(
      self,
      |db| maintenance::vacuum_single(
        db,
        Some(RustVacuumOptions {
          shrink_wal,
          min_wal_size
        })
      ),
      |_db| Ok(())
    )
  }

  fn stats(&self) -> PyResult<DbStats> {
    dispatch_ok!(self, |db| maintenance::stats_single(db), |_db| {
      unreachable!("multi-file database support removed")
    })
  }

  fn check(&self) -> PyResult<CheckResult> {
    dispatch_ok!(self, |db| maintenance::check_single(db), |_db| {
      unreachable!("multi-file database support removed")
    })
  }

  // ==========================================================================
  // Cache Operations (Single-file only)
  // ==========================================================================

  fn cache_is_enabled(&self) -> PyResult<bool> {
    dispatch_ok!(self, |db| cache::cache_is_enabled(db), |_db| false)
  }

  fn cache_invalidate_node(&self, node_id: i64) -> PyResult<()> {
    dispatch_ok!(
      self,
      |db| {
        cache::cache_invalidate_node(db, node_id as NodeId);
      },
      |_db| ()
    )
  }

  fn cache_invalidate_edge(&self, src: i64, etype: u32, dst: i64) -> PyResult<()> {
    dispatch_ok!(
      self,
      |db| {
        cache::cache_invalidate_edge(db, src as NodeId, etype as ETypeId, dst as NodeId);
      },
      |_db| ()
    )
  }

  fn cache_invalidate_key(&self, key: &str) -> PyResult<()> {
    dispatch_ok!(
      self,
      |db| {
        cache::cache_invalidate_key(db, key);
      },
      |_db| ()
    )
  }

  fn cache_clear(&self) -> PyResult<()> {
    dispatch_ok!(
      self,
      |db| {
        cache::cache_clear(db);
      },
      |_db| ()
    )
  }

  fn cache_clear_query(&self) -> PyResult<()> {
    dispatch_ok!(
      self,
      |db| {
        cache::cache_clear_query(db);
      },
      |_db| ()
    )
  }

  fn cache_clear_key(&self) -> PyResult<()> {
    dispatch_ok!(
      self,
      |db| {
        cache::cache_clear_key(db);
      },
      |_db| ()
    )
  }

  fn cache_clear_property(&self) -> PyResult<()> {
    dispatch_ok!(
      self,
      |db| {
        cache::cache_clear_property(db);
      },
      |_db| ()
    )
  }

  fn cache_clear_traversal(&self) -> PyResult<()> {
    dispatch_ok!(
      self,
      |db| {
        cache::cache_clear_traversal(db);
      },
      |_db| ()
    )
  }

  fn cache_stats(&self) -> PyResult<Option<CacheStats>> {
    dispatch_ok!(self, |db| cache::cache_stats(db), |_db| None)
  }

  fn cache_reset_stats(&self) -> PyResult<()> {
    dispatch_ok!(
      self,
      |db| {
        cache::cache_reset_stats(db);
      },
      |_db| ()
    )
  }

  // ==========================================================================
  // Streaming Operations
  // ==========================================================================

  #[pyo3(signature = (options=None))]
  fn stream_nodes(&self, options: Option<StreamOptions>) -> PyResult<Vec<Vec<i64>>> {
    let opts = match options {
      Some(o) => o.to_rust()?,
      None => crate::streaming::StreamOptions::default(),
    };
    dispatch_ok!(
      self,
      |db| streaming_ops::stream_nodes_single(db, opts.clone()),
      |db| streaming_ops::stream_nodes_single(db, opts.clone())
    )
  }

  #[pyo3(signature = (options=None))]
  fn stream_nodes_with_props(
    &self,
    options: Option<StreamOptions>,
  ) -> PyResult<Vec<Vec<NodeWithProps>>> {
    let opts = match options {
      Some(o) => o.to_rust()?,
      None => crate::streaming::StreamOptions::default(),
    };
    dispatch_ok!(
      self,
      |db| streaming_ops::stream_nodes_with_props_single(db, opts.clone()),
      |db| streaming_ops::stream_nodes_with_props_single(db, opts.clone())
    )
  }

  #[pyo3(signature = (options=None))]
  fn stream_edges(&self, options: Option<StreamOptions>) -> PyResult<Vec<Vec<FullEdge>>> {
    let opts = match options {
      Some(o) => o.to_rust()?,
      None => crate::streaming::StreamOptions::default(),
    };
    dispatch_ok!(
      self,
      |db| streaming_ops::stream_edges_single(db, opts.clone()),
      |db| streaming_ops::stream_edges_single(db, opts.clone())
    )
  }

  #[pyo3(signature = (options=None))]
  fn stream_edges_with_props(
    &self,
    options: Option<StreamOptions>,
  ) -> PyResult<Vec<Vec<EdgeWithProps>>> {
    let opts = match options {
      Some(o) => o.to_rust()?,
      None => crate::streaming::StreamOptions::default(),
    };
    dispatch_ok!(
      self,
      |db| streaming_ops::stream_edges_with_props_single(db, opts.clone()),
      |db| streaming_ops::stream_edges_with_props_single(db, opts.clone())
    )
  }

  #[pyo3(signature = (options=None))]
  fn get_nodes_page(&self, options: Option<PaginationOptions>) -> PyResult<NodePage> {
    let opts = match options {
      Some(o) => o.to_rust()?,
      None => crate::streaming::PaginationOptions::default(),
    };
    dispatch_ok!(
      self,
      |db| streaming_ops::nodes_page_single(db, opts.clone()),
      |db| streaming_ops::nodes_page_single(db, opts.clone())
    )
  }

  #[pyo3(signature = (options=None))]
  fn get_edges_page(&self, options: Option<PaginationOptions>) -> PyResult<EdgePage> {
    let opts = match options {
      Some(o) => o.to_rust()?,
      None => crate::streaming::PaginationOptions::default(),
    };
    dispatch_ok!(
      self,
      |db| streaming_ops::edges_page_single(db, opts.clone()),
      |db| streaming_ops::edges_page_single(db, opts.clone())
    )
  }

  // ==========================================================================
  // Export/Import Operations
  // ==========================================================================

  #[pyo3(signature = (path, options=None))]
  fn export_to_json(&self, path: String, options: Option<ExportOptions>) -> PyResult<ExportResult> {
    let opts = options.unwrap_or_default();
    dispatch!(
      self,
      |db| export_import::export_to_json_single(db, path.clone(), opts.clone()),
      |db| export_import::export_to_json_single(db, path.clone(), opts.clone())
    )
  }

  #[pyo3(signature = (path, options=None))]
  fn export_to_jsonl(
    &self,
    path: String,
    options: Option<ExportOptions>,
  ) -> PyResult<ExportResult> {
    let opts = options.unwrap_or_default();
    dispatch!(
      self,
      |db| export_import::export_to_jsonl_single(db, path.clone(), opts.clone()),
      |db| export_import::export_to_jsonl_single(db, path.clone(), opts.clone())
    )
  }

  #[pyo3(signature = (path, options=None))]
  fn import_from_json(
    &self,
    path: String,
    options: Option<ImportOptions>,
  ) -> PyResult<ImportResult> {
    let opts = options.unwrap_or_default();
    dispatch!(
      self,
      |db| export_import::import_from_json_single(db, path.clone(), opts.clone()),
      |db| export_import::import_from_json_single(db, path.clone(), opts.clone())
    )
  }
}

// ============================================================================
// Standalone Functions
// ============================================================================

#[pyfunction]
#[pyo3(signature = (path, options=None))]
pub fn open_database(path: String, options: Option<OpenOptions>) -> PyResult<PyDatabase> {
  PyDatabase::new(path, options)
}

#[pyfunction]
pub fn collect_metrics(db: &PyDatabase) -> PyResult<DatabaseMetrics> {
  let guard = db
    .inner
    .read()
    .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
  match guard.as_ref() {
    Some(DatabaseInner::SingleFile(d)) => Ok(DatabaseMetrics::from(
      core_metrics::collect_metrics_single_file(d),
    )),
    None => Err(PyRuntimeError::new_err("Database is closed")),
  }
}

#[pyfunction]
pub fn health_check(db: &PyDatabase) -> PyResult<HealthCheckResult> {
  let guard = db
    .inner
    .read()
    .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
  match guard.as_ref() {
    Some(DatabaseInner::SingleFile(d)) => Ok(HealthCheckResult::from(
      core_metrics::health_check_single_file(d),
    )),
    None => Err(PyRuntimeError::new_err("Database is closed")),
  }
}

#[pyfunction]
#[pyo3(signature = (db, backup_path, options=None))]
pub fn create_backup(
  db: &PyDatabase,
  backup_path: String,
  options: Option<BackupOptions>,
) -> PyResult<BackupResult> {
  let opts: core_backup::BackupOptions = options.unwrap_or_default().into();
  let path = PathBuf::from(backup_path);
  let guard = db
    .inner
    .read()
    .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
  match guard.as_ref() {
    Some(DatabaseInner::SingleFile(d)) => core_backup::create_backup_single_file(d, &path, opts)
      .map(BackupResult::from)
      .map_err(|e| PyRuntimeError::new_err(e.to_string())),
    None => Err(PyRuntimeError::new_err("Database is closed")),
  }
}

#[pyfunction]
#[pyo3(signature = (backup_path, restore_path, options=None))]
pub fn restore_backup(
  backup_path: String,
  restore_path: String,
  options: Option<RestoreOptions>,
) -> PyResult<String> {
  let opts: core_backup::RestoreOptions = options.unwrap_or_default().into();
  core_backup::restore_backup(backup_path, restore_path, opts)
    .map(|p| p.to_string_lossy().to_string())
    .map_err(|e| PyRuntimeError::new_err(e.to_string()))
}

#[pyfunction]
pub fn backup_info(backup_path: String) -> PyResult<BackupResult> {
  core_backup::backup_info(backup_path)
    .map(BackupResult::from)
    .map_err(|e| PyRuntimeError::new_err(e.to_string()))
}

#[pyfunction]
#[pyo3(signature = (db_path, backup_path, options=None))]
pub fn create_offline_backup(
  db_path: String,
  backup_path: String,
  options: Option<OfflineBackupOptions>,
) -> PyResult<BackupResult> {
  let opts: core_backup::OfflineBackupOptions = options.unwrap_or_default().into();
  core_backup::create_offline_backup(db_path, backup_path, opts)
    .map(BackupResult::from)
    .map_err(|e| PyRuntimeError::new_err(e.to_string()))
}

// ============================================================================
// PathResult Conversion
// ============================================================================

impl From<crate::api::pathfinding::PathResult> for PyPathResult {
  fn from(r: crate::api::pathfinding::PathResult) -> Self {
    Self {
      path: r.path.iter().map(|&id| id as i64).collect(),
      edges: r
        .edges
        .iter()
        .map(|&(s, e, d)| PyPathEdge {
          src: s as i64,
          etype: e,
          dst: d as i64,
        })
        .collect(),
      total_weight: r.total_weight,
      found: r.found,
    }
  }
}
