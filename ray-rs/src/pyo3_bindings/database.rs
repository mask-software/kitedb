//! Python bindings for RayDB Database
//!
//! Provides Python access to both single-file and multi-file database formats.
//! This module contains the main Database class and standalone functions.

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::backup as core_backup;
use crate::core::single_file::{
  close_single_file, is_single_file_path, open_single_file, SingleFileDB as RustSingleFileDB,
  SingleFileOpenOptions as RustOpenOptions, VacuumOptions as RustVacuumOptions,
};
use crate::graph::db::{close_graph_db, open_graph_db as open_multi_file, GraphDB as RustGraphDB};
use crate::metrics as core_metrics;
use crate::types::{ETypeId, NodeId, PropKeyId, TxState as GraphTxState};

// Import from modular structure
use super::helpers::{graph_check, graph_stats};
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

// ============================================================================
// Database Inner Enum
// ============================================================================

pub(crate) enum DatabaseInner {
  SingleFile(RustSingleFileDB),
  Graph(RustGraphDB),
}

// ============================================================================
// Dispatch Macros - Eliminate boilerplate for method dispatch
// ============================================================================

/// Dispatch to single-file or graph implementation (immutable, returns PyResult)
macro_rules! dispatch {
  ($self:expr, |$sf:ident| $sf_expr:expr, |$gf:ident| $gf_expr:expr) => {{
    let guard = $self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    match guard.as_ref() {
      Some(DatabaseInner::SingleFile($sf)) => $sf_expr,
      Some(DatabaseInner::Graph($gf)) => $gf_expr,
      None => Err(PyRuntimeError::new_err("Database is closed")),
    }
  }};
}

/// Dispatch returning Ok-wrapped value (immutable)
macro_rules! dispatch_ok {
  ($self:expr, |$sf:ident| $sf_expr:expr, |$gf:ident| $gf_expr:expr) => {{
    let guard = $self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    match guard.as_ref() {
      Some(DatabaseInner::SingleFile($sf)) => Ok($sf_expr),
      Some(DatabaseInner::Graph($gf)) => Ok($gf_expr),
      None => Err(PyRuntimeError::new_err("Database is closed")),
    }
  }};
}

/// Dispatch to mutable single-file or graph implementation
macro_rules! dispatch_mut {
  ($self:expr, |$sf:ident| $sf_expr:expr, |$gf:ident| $gf_expr:expr) => {{
    let mut guard = $self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    match guard.as_mut() {
      Some(DatabaseInner::SingleFile($sf)) => $sf_expr,
      Some(DatabaseInner::Graph($gf)) => $gf_expr,
      None => Err(PyRuntimeError::new_err("Database is closed")),
    }
  }};
}

/// Dispatch with graph transaction (for write operations on graph db)
macro_rules! dispatch_tx {
  ($self:expr, |$sf:ident| $sf_expr:expr, |$handle:ident| $gf_expr:expr) => {{
    let guard = $self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    match guard.as_ref() {
      Some(DatabaseInner::SingleFile($sf)) => $sf_expr,
      Some(DatabaseInner::Graph(db)) => {
        transaction::with_graph_tx(db, &$self.graph_tx, |$handle| $gf_expr)
      }
      None => Err(PyRuntimeError::new_err("Database is closed")),
    }
  }};
}

// ============================================================================
// Database Python Wrapper
// ============================================================================

/// Graph database handle (single-file or multi-file)
#[pyclass(name = "Database")]
pub struct PyDatabase {
  pub(crate) inner: Mutex<Option<DatabaseInner>>,
  pub(crate) graph_tx: Mutex<Option<GraphTxState>>,
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
      let graph_opts = options.to_graph_options();
      let db = open_multi_file(&path_buf, graph_opts)
        .map_err(|e| PyRuntimeError::new_err(format!("Failed to open database: {e}")))?;
      return Ok(PyDatabase {
        inner: Mutex::new(Some(DatabaseInner::Graph(db))),
        graph_tx: Mutex::new(None),
      });
    }

    let db_path = if is_single_file_path(&path_buf) {
      path_buf
    } else {
      PathBuf::from(format!("{path}.raydb"))
    };

    let opts: RustOpenOptions = options.into();
    let db = open_single_file(&db_path, opts)
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to open database: {e}")))?;
    Ok(PyDatabase {
      inner: Mutex::new(Some(DatabaseInner::SingleFile(db))),
      graph_tx: Mutex::new(None),
    })
  }

  fn close(&self) -> PyResult<()> {
    let mut guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    if let Some(db) = guard.take() {
      match db {
        DatabaseInner::SingleFile(db) => close_single_file(db)
          .map_err(|e| PyRuntimeError::new_err(format!("Failed to close: {e}")))?,
        DatabaseInner::Graph(db) => close_graph_db(db)
          .map_err(|e| PyRuntimeError::new_err(format!("Failed to close: {e}")))?,
      }
    }
    let _ = self.graph_tx.lock().map(|mut tx| tx.take());
    Ok(())
  }

  #[getter]
  fn is_open(&self) -> PyResult<bool> {
    Ok(
      self
        .inner
        .lock()
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
      |db| transaction::begin_graph(db, &self.graph_tx, read_only)
    )
  }

  fn commit(&self) -> PyResult<()> {
    dispatch!(self, |db| transaction::commit_single_file(db), |db| {
      transaction::commit_graph(db, &self.graph_tx)
    })
  }

  fn rollback(&self) -> PyResult<()> {
    dispatch!(self, |db| transaction::rollback_single_file(db), |db| {
      transaction::rollback_graph(db, &self.graph_tx)
    })
  }

  fn has_transaction(&self) -> PyResult<bool> {
    dispatch_ok!(self, |db| db.has_transaction(), |_db| self
      .graph_tx
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?
      .is_some())
  }

  // ==========================================================================
  // Node Operations
  // ==========================================================================

  #[pyo3(signature = (key=None))]
  fn create_node(&self, key: Option<String>) -> PyResult<i64> {
    dispatch_tx!(
      self,
      |db| nodes::create_node_single(db, key.as_deref()),
      |h| nodes::create_node_graph(h, key.clone())
    )
  }

  fn delete_node(&self, node_id: i64) -> PyResult<()> {
    dispatch_tx!(
      self,
      |db| nodes::delete_node_single(db, node_id as NodeId),
      |h| nodes::delete_node_graph(h, node_id as NodeId)
    )
  }

  fn node_exists(&self, node_id: i64) -> PyResult<bool> {
    dispatch_ok!(
      self,
      |db| nodes::node_exists_single(db, node_id as NodeId),
      |db| nodes::node_exists_graph(db, node_id as NodeId)
    )
  }

  fn get_node_by_key(&self, key: &str) -> PyResult<Option<i64>> {
    dispatch_ok!(self, |db| nodes::get_node_by_key_single(db, key), |db| {
      nodes::get_node_by_key_graph(db, key)
    })
  }

  fn get_node_key(&self, node_id: i64) -> PyResult<Option<String>> {
    dispatch_ok!(
      self,
      |db| nodes::get_node_key_single(db, node_id as NodeId),
      |db| nodes::get_node_key_graph(db, node_id as NodeId)
    )
  }

  fn list_nodes(&self) -> PyResult<Vec<i64>> {
    dispatch_ok!(self, |db| nodes::list_nodes_single(db), |db| {
      nodes::list_nodes_graph(db)
    })
  }

  fn count_nodes(&self) -> PyResult<i64> {
    dispatch_ok!(self, |db| nodes::count_nodes_single(db), |db| {
      nodes::count_nodes_graph(db)
    })
  }

  fn list_nodes_with_prefix(&self, prefix: &str) -> PyResult<Vec<i64>> {
    dispatch_ok!(
      self,
      |db| nodes::list_nodes_with_prefix_single(db, prefix),
      |db| nodes::list_nodes_with_prefix_graph(db, prefix)
    )
  }

  fn count_nodes_with_prefix(&self, prefix: &str) -> PyResult<i64> {
    dispatch_ok!(
      self,
      |db| nodes::count_nodes_with_prefix_single(db, prefix),
      |db| nodes::count_nodes_with_prefix_graph(db, prefix)
    )
  }

  fn batch_create_nodes(
    &self,
    input_nodes: Vec<(String, Vec<(u32, PropValue)>)>,
  ) -> PyResult<Vec<i64>> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    match guard.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        let mut ids = Vec::with_capacity(input_nodes.len());
        db.begin(false)
          .map_err(|e| PyRuntimeError::new_err(format!("Failed to begin: {e}")))?;
        let result: Result<(), PyErr> = (|| {
          for (key, props) in input_nodes {
            let id = db
              .create_node(Some(&key))
              .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
            for (k, v) in props {
              db.set_node_prop(id, k as PropKeyId, v.into())
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
            }
            ids.push(id as i64);
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
      Some(DatabaseInner::Graph(db)) => transaction::with_graph_tx(db, &self.graph_tx, |h| {
        let mut ids = Vec::with_capacity(input_nodes.len());
        for (key, props) in &input_nodes {
          let id = nodes::create_node_graph(h, Some(key.clone()))?;
          for (k, v) in props {
            properties::set_node_prop_graph(h, id as NodeId, *k as PropKeyId, v.clone().into())?;
          }
          ids.push(id);
        }
        Ok(ids)
      }),
      None => Err(PyRuntimeError::new_err("Database is closed")),
    }
  }

  // ==========================================================================
  // Edge Operations
  // ==========================================================================

  fn add_edge(&self, src: i64, etype: u32, dst: i64) -> PyResult<()> {
    dispatch_tx!(
      self,
      |db| edges::add_edge_single(db, src as NodeId, etype as ETypeId, dst as NodeId),
      |h| edges::add_edge_graph(h, src as NodeId, etype as ETypeId, dst as NodeId)
    )
  }

  fn add_edge_by_name(&self, src: i64, etype_name: &str, dst: i64) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    match guard.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        edges::add_edge_by_name_single(db, src as NodeId, etype_name, dst as NodeId)
      }
      Some(DatabaseInner::Graph(db)) => {
        let etype = db.get_or_create_etype(etype_name);
        transaction::with_graph_tx(db, &self.graph_tx, |h| {
          edges::add_edge_graph(h, src as NodeId, etype as ETypeId, dst as NodeId)
        })
      }
      None => Err(PyRuntimeError::new_err("Database is closed")),
    }
  }

  fn delete_edge(&self, src: i64, etype: u32, dst: i64) -> PyResult<()> {
    dispatch_tx!(
      self,
      |db| edges::delete_edge_single(db, src as NodeId, etype as ETypeId, dst as NodeId),
      |h| edges::delete_edge_graph(h, src as NodeId, etype as ETypeId, dst as NodeId)
    )
  }

  fn edge_exists(&self, src: i64, etype: u32, dst: i64) -> PyResult<bool> {
    dispatch_ok!(
      self,
      |db| edges::edge_exists_single(db, src as NodeId, etype as ETypeId, dst as NodeId),
      |db| edges::edge_exists_graph(db, src as NodeId, etype as ETypeId, dst as NodeId)
    )
  }

  fn get_out_edges(&self, node_id: i64) -> PyResult<Vec<Edge>> {
    dispatch_ok!(
      self,
      |db| edges::get_out_edges_single(db, node_id as NodeId),
      |db| edges::get_out_edges_graph(db, node_id as NodeId)
    )
  }

  fn get_in_edges(&self, node_id: i64) -> PyResult<Vec<Edge>> {
    dispatch_ok!(
      self,
      |db| edges::get_in_edges_single(db, node_id as NodeId),
      |db| edges::get_in_edges_graph(db, node_id as NodeId)
    )
  }

  fn get_out_degree(&self, node_id: i64) -> PyResult<i64> {
    dispatch_ok!(
      self,
      |db| edges::get_out_degree_single(db, node_id as NodeId),
      |db| edges::get_out_degree_graph(db, node_id as NodeId)
    )
  }

  fn get_in_degree(&self, node_id: i64) -> PyResult<i64> {
    dispatch_ok!(
      self,
      |db| edges::get_in_degree_single(db, node_id as NodeId),
      |db| edges::get_in_degree_graph(db, node_id as NodeId)
    )
  }

  fn count_edges(&self) -> PyResult<i64> {
    dispatch_ok!(self, |db| edges::count_edges_single(db), |db| {
      edges::count_edges_graph(db, None)
    })
  }

  fn count_edges_by_type(&self, etype: u32) -> PyResult<i64> {
    dispatch_ok!(
      self,
      |db| edges::count_edges_by_type_single(db, etype as ETypeId),
      |db| edges::count_edges_graph(db, Some(etype as ETypeId))
    )
  }

  #[pyo3(signature = (etype=None))]
  fn list_edges(&self, etype: Option<u32>) -> PyResult<Vec<FullEdge>> {
    dispatch_ok!(
      self,
      |db| edges::list_edges_single(db, etype.map(|e| e as ETypeId)),
      |db| edges::list_edges_graph(db, etype.map(|e| e as ETypeId))
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
      |h| properties::set_node_prop_graph(
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
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    match guard.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        properties::set_node_prop_by_name_single(db, node_id as NodeId, key_name, value.into())
      }
      Some(DatabaseInner::Graph(db)) => {
        let key_id = db.get_or_create_propkey(key_name);
        transaction::with_graph_tx(db, &self.graph_tx, |h| {
          properties::set_node_prop_graph(
            h,
            node_id as NodeId,
            key_id as PropKeyId,
            value.clone().into(),
          )
        })
      }
      None => Err(PyRuntimeError::new_err("Database is closed")),
    }
  }

  fn get_node_prop(&self, node_id: i64, key_id: u32) -> PyResult<Option<PropValue>> {
    dispatch_ok!(
      self,
      |db| properties::get_node_prop_single(db, node_id as NodeId, key_id as PropKeyId),
      |db| properties::get_node_prop_graph(db, node_id as NodeId, key_id as PropKeyId)
    )
  }

  fn delete_node_prop(&self, node_id: i64, key_id: u32) -> PyResult<()> {
    dispatch_tx!(
      self,
      |db| properties::delete_node_prop_single(db, node_id as NodeId, key_id as PropKeyId),
      |h| properties::delete_node_prop_graph(h, node_id as NodeId, key_id as PropKeyId)
    )
  }

  fn get_node_props(&self, node_id: i64) -> PyResult<Option<Vec<NodeProp>>> {
    dispatch_ok!(
      self,
      |db| properties::get_node_props_single(db, node_id as NodeId),
      |db| properties::get_node_props_graph(db, node_id as NodeId)
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
      |h| properties::set_edge_prop_graph(
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
      .lock()
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
      Some(DatabaseInner::Graph(db)) => {
        let key_id = db.get_or_create_propkey(key_name);
        transaction::with_graph_tx(db, &self.graph_tx, |h| {
          properties::set_edge_prop_graph(
            h,
            src as NodeId,
            etype as ETypeId,
            dst as NodeId,
            key_id as PropKeyId,
            value.clone().into(),
          )
        })
      }
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
      |db| properties::get_edge_prop_graph(
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
      |h| properties::delete_edge_prop_graph(
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
      |db| properties::get_edge_props_graph(db, src as NodeId, etype as ETypeId, dst as NodeId)
    )
  }

  // Direct type property getters
  fn get_node_prop_string(&self, node_id: i64, key_id: u32) -> PyResult<Option<String>> {
    dispatch_ok!(
      self,
      |db| properties::get_node_prop_string_single(db, node_id as NodeId, key_id as PropKeyId),
      |db| properties::get_node_prop_string_graph(db, node_id as NodeId, key_id as PropKeyId)
    )
  }

  fn get_node_prop_int(&self, node_id: i64, key_id: u32) -> PyResult<Option<i64>> {
    dispatch_ok!(
      self,
      |db| properties::get_node_prop_int_single(db, node_id as NodeId, key_id as PropKeyId),
      |db| properties::get_node_prop_int_graph(db, node_id as NodeId, key_id as PropKeyId)
    )
  }

  fn get_node_prop_float(&self, node_id: i64, key_id: u32) -> PyResult<Option<f64>> {
    dispatch_ok!(
      self,
      |db| properties::get_node_prop_float_single(db, node_id as NodeId, key_id as PropKeyId),
      |db| properties::get_node_prop_float_graph(db, node_id as NodeId, key_id as PropKeyId)
    )
  }

  fn get_node_prop_bool(&self, node_id: i64, key_id: u32) -> PyResult<Option<bool>> {
    dispatch_ok!(
      self,
      |db| properties::get_node_prop_bool_single(db, node_id as NodeId, key_id as PropKeyId),
      |db| properties::get_node_prop_bool_graph(db, node_id as NodeId, key_id as PropKeyId)
    )
  }

  // ==========================================================================
  // Schema Operations
  // ==========================================================================

  fn get_or_create_label(&self, name: &str) -> PyResult<u32> {
    dispatch_ok!(
      self,
      |db| schema::get_or_create_label_single(db, name),
      |db| schema::get_or_create_label_graph(db, name)
    )
  }

  fn get_label_id(&self, name: &str) -> PyResult<Option<u32>> {
    dispatch_ok!(self, |db| schema::get_label_id_single(db, name), |db| {
      schema::get_label_id_graph(db, name)
    })
  }

  fn get_label_name(&self, id: u32) -> PyResult<Option<String>> {
    dispatch_ok!(self, |db| schema::get_label_name_single(db, id), |db| {
      schema::get_label_name_graph(db, id)
    })
  }

  fn get_or_create_etype(&self, name: &str) -> PyResult<u32> {
    dispatch_ok!(
      self,
      |db| schema::get_or_create_etype_single(db, name),
      |db| schema::get_or_create_etype_graph(db, name)
    )
  }

  fn get_etype_id(&self, name: &str) -> PyResult<Option<u32>> {
    dispatch_ok!(self, |db| schema::get_etype_id_single(db, name), |db| {
      schema::get_etype_id_graph(db, name)
    })
  }

  fn get_etype_name(&self, id: u32) -> PyResult<Option<String>> {
    dispatch_ok!(self, |db| schema::get_etype_name_single(db, id), |db| {
      schema::get_etype_name_graph(db, id)
    })
  }

  fn get_or_create_propkey(&self, name: &str) -> PyResult<u32> {
    dispatch_ok!(
      self,
      |db| schema::get_or_create_propkey_single(db, name),
      |db| schema::get_or_create_propkey_graph(db, name)
    )
  }

  fn get_propkey_id(&self, name: &str) -> PyResult<Option<u32>> {
    dispatch_ok!(self, |db| schema::get_propkey_id_single(db, name), |db| {
      schema::get_propkey_id_graph(db, name)
    })
  }

  fn get_propkey_name(&self, id: u32) -> PyResult<Option<String>> {
    dispatch_ok!(self, |db| schema::get_propkey_name_single(db, id), |db| {
      schema::get_propkey_name_graph(db, id)
    })
  }

  // ==========================================================================
  // Label Operations
  // ==========================================================================

  fn define_label(&self, name: &str) -> PyResult<u32> {
    dispatch_tx!(self, |db| labels::define_label_single(db, name), |h| {
      labels::define_label_graph(h, name)
    })
  }

  fn add_node_label(&self, node_id: i64, label_id: u32) -> PyResult<()> {
    dispatch_tx!(
      self,
      |db| labels::add_node_label_single(db, node_id as NodeId, label_id),
      |h| labels::add_node_label_graph(h, node_id as NodeId, label_id)
    )
  }

  fn add_node_label_by_name(&self, node_id: i64, label_name: &str) -> PyResult<()> {
    let guard = self
      .inner
      .lock()
      .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    match guard.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        labels::add_node_label_by_name_single(db, node_id as NodeId, label_name)
      }
      Some(DatabaseInner::Graph(db)) => {
        let label_id = db.get_or_create_label(label_name);
        transaction::with_graph_tx(db, &self.graph_tx, |h| {
          labels::add_node_label_graph(h, node_id as NodeId, label_id)
        })
      }
      None => Err(PyRuntimeError::new_err("Database is closed")),
    }
  }

  fn remove_node_label(&self, node_id: i64, label_id: u32) -> PyResult<()> {
    dispatch_tx!(
      self,
      |db| labels::remove_node_label_single(db, node_id as NodeId, label_id),
      |h| labels::remove_node_label_graph(h, node_id as NodeId, label_id)
    )
  }

  fn node_has_label(&self, node_id: i64, label_id: u32) -> PyResult<bool> {
    dispatch_ok!(
      self,
      |db| labels::node_has_label_single(db, node_id as NodeId, label_id),
      |db| labels::node_has_label_graph(db, node_id as NodeId, label_id)
    )
  }

  fn get_node_labels(&self, node_id: i64) -> PyResult<Vec<u32>> {
    dispatch_ok!(
      self,
      |db| labels::get_node_labels_single(db, node_id as NodeId),
      |db| labels::get_node_labels_graph(db, node_id as NodeId)
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
      |h| vectors::set_node_vector_graph(h, node_id as NodeId, prop_key_id as PropKeyId, &v)
    )
  }

  fn get_node_vector(&self, node_id: i64, prop_key_id: u32) -> PyResult<Option<Vec<f64>>> {
    dispatch_ok!(
      self,
      |db| vectors::get_node_vector_single(db, node_id as NodeId, prop_key_id as PropKeyId),
      |db| vectors::get_node_vector_graph(db, node_id as NodeId, prop_key_id as PropKeyId)
    )
  }

  fn delete_node_vector(&self, node_id: i64, prop_key_id: u32) -> PyResult<()> {
    dispatch_tx!(
      self,
      |db| vectors::delete_node_vector_single(db, node_id as NodeId, prop_key_id as PropKeyId),
      |h| vectors::delete_node_vector_graph(h, node_id as NodeId, prop_key_id as PropKeyId)
    )
  }

  fn has_node_vector(&self, node_id: i64, prop_key_id: u32) -> PyResult<bool> {
    dispatch_ok!(
      self,
      |db| vectors::has_node_vector_single(db, node_id as NodeId, prop_key_id as PropKeyId),
      |db| vectors::has_node_vector_graph(db, node_id as NodeId, prop_key_id as PropKeyId)
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
      |db| graph_traversal::traverse_out_graph(db, node_id as NodeId, etype)
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
      |db| graph_traversal::traverse_out_with_keys_graph(db, node_id as NodeId, etype)
    )
  }

  #[pyo3(signature = (node_id, etype=None))]
  fn traverse_out_count(&self, node_id: i64, etype: Option<u32>) -> PyResult<i64> {
    dispatch_ok!(
      self,
      |db| graph_traversal::traverse_out_count_single(db, node_id as NodeId, etype),
      |db| graph_traversal::traverse_out_count_graph(db, node_id as NodeId, etype)
    )
  }

  #[pyo3(signature = (node_id, etype=None))]
  fn traverse_in(&self, node_id: i64, etype: Option<u32>) -> PyResult<Vec<i64>> {
    dispatch_ok!(
      self,
      |db| graph_traversal::traverse_in_single(db, node_id as NodeId, etype),
      |db| graph_traversal::traverse_in_graph(db, node_id as NodeId, etype)
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
      |db| graph_traversal::traverse_in_with_keys_graph(db, node_id as NodeId, etype)
    )
  }

  #[pyo3(signature = (node_id, etype=None))]
  fn traverse_in_count(&self, node_id: i64, etype: Option<u32>) -> PyResult<i64> {
    dispatch_ok!(
      self,
      |db| graph_traversal::traverse_in_count_single(db, node_id as NodeId, etype),
      |db| graph_traversal::traverse_in_count_graph(db, node_id as NodeId, etype)
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
      |db| graph_traversal::traverse_multi_graph(db, start_ids.clone(), steps.clone())
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
      |db| graph_traversal::traverse_multi_count_graph(db, start_ids.clone(), steps.clone())
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
      |db| graph_traversal::traverse_graph(
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
      |db| graph_traversal::find_path_bfs_graph(
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
      |db| graph_traversal::find_path_dijkstra_graph(
        db,
        source as NodeId,
        target as NodeId,
        etype,
        max_depth,
        direction.clone()
      )
    )
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
      |db| maintenance::optimize_graph(db)
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
    dispatch_ok!(self, |db| maintenance::stats_single(db), |db| graph_stats(
      db
    ))
  }

  fn check(&self) -> PyResult<CheckResult> {
    dispatch_ok!(self, |db| maintenance::check_single(db), |db| graph_check(
      db
    )
    .into())
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
      |db| streaming_ops::stream_nodes_graph(db, opts.clone())
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
      |db| streaming_ops::stream_nodes_with_props_graph(db, opts.clone())
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
      |db| streaming_ops::stream_edges_graph(db, opts.clone())
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
      |db| streaming_ops::stream_edges_with_props_graph(db, opts.clone())
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
      |db| streaming_ops::get_nodes_page_single(db, opts.clone()),
      |db| streaming_ops::get_nodes_page_graph(db, opts.clone())
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
      |db| streaming_ops::get_edges_page_single(db, opts.clone()),
      |db| streaming_ops::get_edges_page_graph(db, opts.clone())
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
      |db| export_import::export_to_json_graph(db, path.clone(), opts.clone())
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
      |db| export_import::export_to_jsonl_graph(db, path.clone(), opts.clone())
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
      |db| export_import::import_from_json_graph(db, path.clone(), opts.clone())
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
    .lock()
    .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
  match guard.as_ref() {
    Some(DatabaseInner::SingleFile(d)) => Ok(DatabaseMetrics::from(
      core_metrics::collect_metrics_single_file(d),
    )),
    Some(DatabaseInner::Graph(d)) => Ok(DatabaseMetrics::from(
      core_metrics::collect_metrics_graph(d),
    )),
    None => Err(PyRuntimeError::new_err("Database is closed")),
  }
}

#[pyfunction]
pub fn health_check(db: &PyDatabase) -> PyResult<HealthCheckResult> {
  let guard = db
    .inner
    .lock()
    .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
  match guard.as_ref() {
    Some(DatabaseInner::SingleFile(d)) => Ok(HealthCheckResult::from(
      core_metrics::health_check_single_file(d),
    )),
    Some(DatabaseInner::Graph(d)) => {
      Ok(HealthCheckResult::from(core_metrics::health_check_graph(d)))
    }
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
    .lock()
    .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
  match guard.as_ref() {
    Some(DatabaseInner::SingleFile(d)) => core_backup::create_backup_single_file(d, &path, opts)
      .map(BackupResult::from)
      .map_err(|e| PyRuntimeError::new_err(e.to_string())),
    Some(DatabaseInner::Graph(d)) => core_backup::create_backup_graph(d, &path, opts)
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
pub fn get_backup_info(backup_path: String) -> PyResult<BackupResult> {
  core_backup::get_backup_info(backup_path)
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
