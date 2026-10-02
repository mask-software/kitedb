//! Python bindings for KiteDB Database
//!
//! Provides Python access to the single-file database format.
//! This module contains the main Database class and standalone functions.

use pyo3::exceptions::PyValueError;

use crate::pyo3_bindings::errors;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use crate::api::kite::KiteRuntimeProfile as RustKiteRuntimeProfile;
use crate::api::traversal::TraversalDirection;
use crate::backup as core_backup;
use crate::core::single_file::{
  close_single_file, is_single_file_path, open_single_file, SingleFileDB as RustSingleFileDB,
  VacuumOptions as RustVacuumOptions,
};
use crate::metrics as core_metrics;
use crate::replication::types::CommitToken;
use crate::types::{ETypeId, EdgeWithProps as CoreEdgeWithProps, NodeId, PropKeyId};

// Import from modular structure
use super::ops::streaming::{EdgeBatchIterator, NodeBatchIterator};
use super::ops::{
  cache, edges, export_import, graph_traversal, labels, maintenance, nodes, properties, schema,
  streaming as streaming_ops, transaction, vectors,
};
use super::options::{
  BackupOptions, BackupResult, ExportOptions, ExportResult, ImportOptions, ImportResult,
  OfflineBackupOptions, OpenOptions, PaginationOptions, RestoreOptions, RuntimeProfile,
  SingleFileOptimizeOptions, StreamOptions,
};
use super::stats::{CacheStats, CheckResult, DatabaseMetrics, DbStats, HealthCheckResult};
use super::traversal::{PyPathEdge, PyPathResult, PyTraversalResult};
use super::types::{Edge, EdgePage, FullEdge, NodePage, NodeProp, PropValue};
use super::validation;

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
    let guard = $self.inner.read().map_err(errors::poisoned)?;
    match guard.as_ref() {
      Some(DatabaseInner::SingleFile($sf)) => $sf_expr,
      None => Err(errors::closed()),
    }
  }};
}

/// Dispatch returning Ok-wrapped value (immutable)
/// Uses read lock for concurrent read access
macro_rules! dispatch_ok {
  ($self:expr, |$sf:ident| $sf_expr:expr, |$gf:ident| $gf_expr:expr) => {{
    let guard = $self.inner.read().map_err(errors::poisoned)?;
    match guard.as_ref() {
      Some(DatabaseInner::SingleFile($sf)) => Ok($sf_expr),
      None => Err(errors::closed()),
    }
  }};
}

/// Dispatch for write operations
macro_rules! dispatch_tx {
  ($self:expr, |$sf:ident| $sf_expr:expr, |$handle:ident| $gf_expr:expr) => {{
    let guard = $self.inner.read().map_err(errors::poisoned)?;
    match guard.as_ref() {
      Some(DatabaseInner::SingleFile($sf)) => $sf_expr,
      None => Err(errors::closed()),
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
/// - **Read operations** (`node_by_key`, `node_exists`, `neighbors`, etc.)
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
/// Long-running operations (open, close, commit, checkpoint, optimize, vacuum,
/// export/import, backup, replication catch-up and `wait_for_token`) release
/// the GIL, so other Python threads keep running while they block on I/O.
#[pyclass(name = "Database")]
pub struct PyDatabase {
  pub(crate) inner: RwLock<Option<DatabaseInner>>,
}

/// Poll interval for `wait_for_token`, matching the core wait loop.
const TOKEN_POLL_INTERVAL: Duration = Duration::from_millis(10);

impl PyDatabase {
  fn with_db<T>(&self, f: impl FnOnce(&RustSingleFileDB) -> PyResult<T>) -> PyResult<T> {
    let guard = self.inner.read().map_err(errors::poisoned)?;
    match guard.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => f(db),
      None => Err(errors::closed()),
    }
  }

  /// Runs `f` under the read lock with the GIL released.
  ///
  /// The lock is taken inside `detach`, so this thread never holds it
  /// while waiting for the GIL (which would deadlock against a thread that
  /// holds the GIL and waits for the lock).
  pub(crate) fn with_db_nogil<T: Send>(
    &self,
    py: Python<'_>,
    f: impl FnOnce(&RustSingleFileDB) -> PyResult<T> + Send,
  ) -> PyResult<T> {
    py.detach(|| self.with_db(f))
  }

  /// Closes the database with the GIL released, after an optional close-time
  /// checkpoint (when WAL usage is at least `checkpoint_threshold`).
  ///
  /// The checkpoint runs under the shared lock: core waits for transactions
  /// open on other threads to finish, and their commits need that lock. Only
  /// the close itself takes the exclusive lock, so closing can't deadlock
  /// against another thread's open transaction. A transaction still open when
  /// the close runs is discarded, as with a plain close. If the checkpoint
  /// fails the database is still closed (the WAL keeps every commit), and the
  /// checkpoint error is raised afterwards.
  fn close_nogil(&self, py: Python<'_>, checkpoint_threshold: Option<f64>) -> PyResult<()> {
    py.detach(|| {
      let checkpoint = match checkpoint_threshold {
        Some(threshold) => self.close_checkpoint(threshold),
        None => Ok(()),
      };
      let mut guard = self.inner.write().map_err(errors::poisoned)?;
      if let Some(DatabaseInner::SingleFile(db)) = guard.take() {
        close_single_file(*db).map_err(|e| errors::wrap(e, "Failed to close"))?;
      }
      checkpoint
    })
  }

  fn close_checkpoint(&self, threshold: f64) -> PyResult<()> {
    let guard = self.inner.read().map_err(errors::poisoned)?;
    match guard.as_ref() {
      Some(DatabaseInner::SingleFile(db)) if !db.read_only && db.should_checkpoint(threshold) => db
        .checkpoint()
        .map_err(|e| errors::wrap(e, "Failed to checkpoint on close")),
      _ => Ok(()),
    }
  }
}

#[pymethods]
impl PyDatabase {
  // ==========================================================================
  // Constructor and Lifecycle
  // ==========================================================================

  #[new]
  #[pyo3(signature = (path, options=None))]
  fn new(py: Python<'_>, path: String, options: Option<OpenOptions>) -> PyResult<Self> {
    let options = options.unwrap_or_default();
    let path_buf = PathBuf::from(&path);

    if path_buf.exists() && path_buf.is_dir() {
      return Err(errors::KiteError::new_err(
        "Single-file databases require a file path, not a directory",
      ));
    }

    let db_path = if is_single_file_path(&path_buf) {
      path_buf
    } else if path_buf.extension().is_none() {
      PathBuf::from(format!("{path}.kitedb"))
    } else {
      return Err(errors::KiteError::new_err(
        "Single-file databases must use the .kitedb extension",
      ));
    };

    let opts = options.to_single_file_options()?;
    let db = py
      .detach(|| open_single_file(&db_path, opts))
      .map_err(|e| errors::wrap(e, "Failed to open database"))?;
    Ok(PyDatabase {
      inner: RwLock::new(Some(DatabaseInner::SingleFile(Box::new(db)))),
    })
  }

  #[staticmethod]
  #[pyo3(signature = (path, options=None))]
  fn open(py: Python<'_>, path: String, options: Option<OpenOptions>) -> PyResult<Self> {
    Self::new(py, path, options)
  }

  fn close(&self, py: Python<'_>) -> PyResult<()> {
    self.close_nogil(py, None)
  }

  #[pyo3(signature = (threshold))]
  fn close_with_checkpoint_if_wal_over(&self, py: Python<'_>, threshold: f64) -> PyResult<()> {
    let threshold = validation::ratio("threshold", threshold)?;
    self.close_nogil(py, Some(threshold))
  }

  fn __enter__(slf: PyRef<'_, Self>) -> PyResult<PyRef<'_, Self>> {
    Ok(slf)
  }

  #[pyo3(signature = (_exc_type=None, _exc_value=None, _traceback=None))]
  fn __exit__(
    &self,
    py: Python<'_>,
    _exc_type: Option<Py<PyAny>>,
    _exc_value: Option<Py<PyAny>>,
    _traceback: Option<Py<PyAny>>,
  ) -> PyResult<bool> {
    self.close(py)?;
    Ok(false)
  }

  #[getter]
  fn is_open(&self) -> PyResult<bool> {
    Ok(self.inner.read().map_err(errors::poisoned)?.is_some())
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
  fn begin(&self, py: Python<'_>, read_only: Option<bool>) -> PyResult<i64> {
    let read_only = read_only.unwrap_or(false);
    // Begin waits on the core checkpoint gate. A checkpoint holding it waits for
    // other threads' transactions to finish, so blocking here with the GIL held
    // would starve those threads and deadlock.
    self.with_db_nogil(py, |db| transaction::begin_single_file(db, read_only))
  }

  /// Begin a bulk-load transaction (fast path, MVCC disabled)
  fn begin_bulk(&self, py: Python<'_>) -> PyResult<i64> {
    self.with_db_nogil(py, transaction::begin_bulk_single_file)
  }

  fn commit(&self, py: Python<'_>) -> PyResult<()> {
    self.with_db_nogil(py, transaction::commit_single_file)
  }

  fn rollback(&self) -> PyResult<()> {
    dispatch!(self, |db| transaction::rollback_single_file(db), |_db| {
      unreachable!("multi-file database support removed")
    })
  }

  fn has_transaction(&self) -> PyResult<bool> {
    dispatch_ok!(self, |db| db.has_transaction(), |_db| false)
  }

  /// Commit and return replication commit token (e.g. "2:41") when available.
  fn commit_with_token(&self, py: Python<'_>) -> PyResult<Option<String>> {
    self.with_db_nogil(py, |db| {
      db.commit_with_token()
        .map(|token| token.map(|value| value.to_string()))
        .map_err(|e| errors::wrap(e, "Failed to commit"))
    })
  }

  /// Wait until this DB has observed at least the provided commit token.
  ///
  /// Polls with the GIL released and holds the read lock only for each check,
  /// so other threads can catch up, run maintenance or close meanwhile.
  fn wait_for_token(&self, py: Python<'_>, token: String, timeout_ms: i64) -> PyResult<bool> {
    let timeout_ms =
      validation::non_negative_u64("timeout_ms", timeout_ms, validation::MAX_DURATION_MS as u64)?;
    let token = CommitToken::from_str(&token).map_err(|e| errors::wrap(e, "Invalid token"))?;
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    py.detach(|| loop {
      // A zero timeout makes the core call a single non-blocking check.
      let observed = self.with_db(|db| {
        db.wait_for_token(token, 0)
          .map_err(|e| errors::wrap(e, "Failed waiting for token"))
      })?;
      if observed {
        return Ok(true);
      }
      let now = Instant::now();
      if now >= deadline {
        return Ok(false);
      }
      std::thread::sleep(TOKEN_POLL_INTERVAL.min(deadline - now));
    })
  }

  /// Primary replication status dictionary when role=primary, else None.
  fn primary_replication_status(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
    let guard = self.inner.read().map_err(errors::poisoned)?;
    match guard.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        let Some(status) = db.primary_replication_status() else {
          return Ok(None);
        };

        let out = PyDict::new(py);
        out.set_item("role", status.role.to_string())?;
        out.set_item("epoch", status.epoch)?;
        out.set_item("head_log_index", status.head_log_index)?;
        out.set_item("retained_floor", status.retained_floor)?;
        out.set_item(
          "sidecar_path",
          status.sidecar_path.to_string_lossy().to_string(),
        )?;
        out.set_item(
          "last_token",
          status.last_token.map(|token| token.to_string()),
        )?;
        out.set_item("last_replication_error", status.last_replication_error)?;
        out.set_item("sidecar_needs_repair", status.sidecar_needs_repair)?;
        out.set_item("append_attempts", status.append_attempts)?;
        out.set_item("append_failures", status.append_failures)?;
        out.set_item("append_successes", status.append_successes)?;

        let lags = PyList::empty(py);
        for lag in status.replica_lags {
          let lag_item = PyDict::new(py);
          lag_item.set_item("replica_id", lag.replica_id)?;
          lag_item.set_item("epoch", lag.epoch)?;
          lag_item.set_item("applied_log_index", lag.applied_log_index)?;
          lags.append(lag_item)?;
        }
        out.set_item("replica_lags", lags)?;

        Ok(Some(out.into_any().unbind()))
      }
      None => Err(errors::closed()),
    }
  }

  /// Replica replication status dictionary when role=replica, else None.
  fn replica_replication_status(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
    let guard = self.inner.read().map_err(errors::poisoned)?;
    match guard.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        let Some(status) = db.replica_replication_status() else {
          return Ok(None);
        };

        let out = PyDict::new(py);
        out.set_item("role", status.role.to_string())?;
        out.set_item(
          "source_db_path",
          status
            .source_db_path
            .map(|path| path.to_string_lossy().to_string()),
        )?;
        out.set_item(
          "source_sidecar_path",
          status
            .source_sidecar_path
            .map(|path| path.to_string_lossy().to_string()),
        )?;
        out.set_item("applied_epoch", status.applied_epoch)?;
        out.set_item("applied_log_index", status.applied_log_index)?;
        out.set_item("last_error", status.last_error)?;
        out.set_item("needs_reseed", status.needs_reseed)?;
        Ok(Some(out.into_any().unbind()))
      }
      None => Err(errors::closed()),
    }
  }

  /// Promote this primary to the next replication epoch.
  fn primary_promote_to_next_epoch(&self) -> PyResult<i64> {
    dispatch!(
      self,
      |db| db
        .primary_promote_to_next_epoch()
        .map(|value| value as i64)
        .map_err(|e| errors::wrap(e, "Failed to promote primary")),
      |_db| { unreachable!("multi-file database support removed") }
    )
  }

  /// Report replica progress cursor to primary.
  fn primary_report_replica_progress(
    &self,
    replica_id: String,
    epoch: i64,
    applied_log_index: i64,
  ) -> PyResult<()> {
    let epoch = validation::non_negative_u64("epoch", epoch, i64::MAX as u64)?;
    let applied_log_index =
      validation::non_negative_u64("applied_log_index", applied_log_index, i64::MAX as u64)?;
    dispatch!(
      self,
      |db| db
        .primary_report_replica_progress(&replica_id, epoch, applied_log_index)
        .map_err(|e| errors::wrap(e, "Failed to report replica progress")),
      |_db| { unreachable!("multi-file database support removed") }
    )
  }

  /// Run primary retention and return (pruned_segments, retained_floor).
  fn primary_run_retention(&self) -> PyResult<(i64, i64)> {
    dispatch!(
      self,
      |db| db
        .primary_run_retention()
        .map(|outcome| (
          outcome.pruned_segments as i64,
          outcome.retained_floor as i64
        ))
        .map_err(|e| errors::wrap(e, "Failed to run retention")),
      |_db| { unreachable!("multi-file database support removed") }
    )
  }

  /// Export latest primary snapshot metadata and optional bytes as transport JSON.
  #[pyo3(signature = (include_data=false))]
  fn export_replication_snapshot_transport_json(&self, include_data: bool) -> PyResult<String> {
    dispatch!(
      self,
      |db| db
        .primary_export_snapshot_transport_json(include_data)
        .map_err(|e| errors::wrap(e, "Failed to export replication snapshot")),
      |_db| { unreachable!("multi-file database support removed") }
    )
  }

  /// Export primary replication log page (cursor + limits) as transport JSON.
  #[pyo3(signature = (cursor=None, max_frames=128, max_bytes=1048576, include_payload=true))]
  fn export_replication_log_transport_json(
    &self,
    cursor: Option<String>,
    max_frames: i64,
    max_bytes: i64,
    include_payload: bool,
  ) -> PyResult<String> {
    let max_frames = validation::positive_usize("max_frames", max_frames, validation::MAX_COUNT)?;
    let max_bytes = validation::positive_usize("max_bytes", max_bytes, validation::MAX_BYTES)?;
    dispatch!(
      self,
      |db| db
        .primary_export_log_transport_json(
          cursor.as_deref(),
          max_frames,
          max_bytes,
          include_payload,
        )
        .map_err(|e| errors::wrap(e, "Failed to export replication log")),
      |_db| { unreachable!("multi-file database support removed") }
    )
  }

  /// Bootstrap replica state from source snapshot.
  fn replica_bootstrap_from_snapshot(&self, py: Python<'_>) -> PyResult<()> {
    self.with_db_nogil(py, |db| {
      db.replica_bootstrap_from_snapshot()
        .map_err(|e| errors::wrap(e, "Failed to bootstrap replica"))
    })
  }

  /// Pull and apply at most max_frames frames on replica.
  fn replica_catch_up_once(&self, py: Python<'_>, max_frames: i64) -> PyResult<i64> {
    let max_frames =
      validation::non_negative_usize("max_frames", max_frames, validation::MAX_COUNT)?;
    self.with_db_nogil(py, |db| {
      db.replica_catch_up_once(max_frames)
        .map(|count| count as i64)
        .map_err(|e| errors::wrap(e, "Failed replica catch-up"))
    })
  }

  /// Force a replica reseed from source snapshot.
  fn replica_reseed_from_snapshot(&self, py: Python<'_>) -> PyResult<()> {
    self.with_db_nogil(py, |db| {
      db.replica_reseed_from_snapshot()
        .map_err(|e| errors::wrap(e, "Failed to reseed replica"))
    })
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
    let node_id = validation::node_id("node_id", node_id)?;
    dispatch_tx!(self, |db| nodes::delete_node_single(db, node_id), |h| {
      nodes::delete_node_single(h, node_id)
    })
  }

  fn node_exists(&self, node_id: i64) -> PyResult<bool> {
    let node_id = validation::node_id("node_id", node_id)?;
    dispatch_ok!(self, |db| nodes::node_exists_single(db, node_id), |db| {
      nodes::node_exists_single(db, node_id)
    })
  }

  #[pyo3(name = "get_node_by_key")]
  fn node_by_key(&self, key: &str) -> PyResult<Option<i64>> {
    dispatch_ok!(self, |db| nodes::node_by_key_single(db, key), |db| {
      nodes::node_by_key_single(db, key)
    })
  }

  #[pyo3(name = "get_node_key")]
  fn node_key(&self, node_id: i64) -> PyResult<Option<String>> {
    let node_id = validation::node_id("node_id", node_id)?;
    dispatch_ok!(self, |db| nodes::node_key_single(db, node_id), |db| {
      nodes::node_key_single(db, node_id)
    })
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

  /// Create keyed nodes with properties (and optional labels) in one batch.
  ///
  /// Inside an open transaction the batch joins it. Otherwise it runs in its
  /// own transaction: a bulk-load one (the fast path) unless MVCC is enabled,
  /// which bulk load does not support.
  #[pyo3(signature = (input_nodes, labels=None))]
  fn batch_create_nodes(
    &self,
    py: Python<'_>,
    input_nodes: Vec<(String, Vec<(u32, PropValue)>)>,
    labels: Option<Vec<u32>>,
  ) -> PyResult<Vec<i64>> {
    let labels = labels.unwrap_or_default();
    self.with_db_nogil(py, |db| {
      let write = || -> PyResult<Vec<i64>> {
        let key_refs: Vec<Option<&str>> = input_nodes
          .iter()
          .map(|(key, _)| Some(key.as_str()))
          .collect();
        let node_ids = db
          .create_nodes_batch(&key_refs)
          .map_err(|e| errors::wrap(e, "Failed to create nodes"))?;
        for (node_id, (_, props)) in node_ids.iter().copied().zip(&input_nodes) {
          for &label_id in &labels {
            db.add_node_label(node_id, label_id)
              .map_err(|e| errors::wrap(e, "Failed to add label"))?;
          }
          for (key_id, value) in props {
            db.set_node_prop(node_id, *key_id as PropKeyId, value.clone().into())
              .map_err(|e| errors::wrap(e, "Failed to set property"))?;
          }
        }
        Ok(node_ids.into_iter().map(|id| id as i64).collect())
      };

      if db.has_transaction() {
        return write();
      }
      if db.mvcc_enabled() {
        db.begin(false)
          .map_err(|e| errors::wrap(e, "Failed to begin transaction"))?;
      } else {
        db.begin_bulk()
          .map_err(|e| errors::wrap(e, "Failed to begin bulk"))?;
      }
      match write() {
        Ok(ids) => {
          db.commit()
            .map_err(|e| errors::wrap(e, "Failed to commit"))?;
          Ok(ids)
        }
        Err(e) => {
          let _ = db.rollback();
          Err(e)
        }
      }
    })
  }

  /// Create multiple nodes in a single WAL record (fast path)
  fn create_nodes_batch(&self, keys: Vec<Option<String>>) -> PyResult<Vec<i64>> {
    let guard = self.inner.read().map_err(errors::poisoned)?;
    match guard.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        let key_refs: Vec<Option<&str>> = keys.iter().map(|k| k.as_deref()).collect();
        let node_ids = db
          .create_nodes_batch(&key_refs)
          .map_err(|e| errors::wrap(e, "Failed to create nodes"))?;
        Ok(node_ids.into_iter().map(|id| id as i64).collect())
      }
      None => Err(errors::closed()),
    }
  }

  /// Add multiple edges in a single WAL record (fast path)
  fn add_edges_batch(&self, edges: Vec<(i64, u32, i64)>) -> PyResult<()> {
    let core_edges: Vec<(NodeId, ETypeId, NodeId)> = edges
      .into_iter()
      .map(|(src, etype, dst)| {
        Ok((
          validation::node_id("src", src)?,
          etype as ETypeId,
          validation::node_id("dst", dst)?,
        ))
      })
      .collect::<PyResult<_>>()?;
    let guard = self.inner.read().map_err(errors::poisoned)?;
    match guard.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .add_edges_batch(&core_edges)
        .map_err(|e| errors::wrap(e, "Failed to add edges")),
      None => Err(errors::closed()),
    }
  }

  /// Add multiple edges with props in a single WAL record (fast path)
  fn add_edges_with_props_batch(&self, edges: Vec<EdgePropsInput>) -> PyResult<()> {
    let core_edges: Vec<CoreEdgeWithProps> = edges
      .into_iter()
      .map(|(src, etype, dst, props)| {
        let core_props = props
          .into_iter()
          .map(|(key_id, value)| (key_id as PropKeyId, value.into()))
          .collect();
        Ok((
          validation::node_id("src", src)?,
          etype as ETypeId,
          validation::node_id("dst", dst)?,
          core_props,
        ))
      })
      .collect::<PyResult<_>>()?;
    let guard = self.inner.read().map_err(errors::poisoned)?;
    match guard.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => db
        .add_edges_with_props_batch(core_edges)
        .map_err(|e| errors::wrap(e, "Failed to add edges")),
      None => Err(errors::closed()),
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
    let node_id = validation::node_id("node_id", node_id)?;
    let core_props: Vec<(PropKeyId, Option<crate::types::PropValue>)> = props
      .into_iter()
      .map(|(k, v)| (k as PropKeyId, v.map(|value| value.into())))
      .collect();

    dispatch_tx!(
      self,
      |db| nodes::upsert_node_by_id_single(db, node_id, &core_props),
      |h| nodes::upsert_node_by_id_single(h, node_id, &core_props)
    )
  }

  // ==========================================================================
  // Edge Operations
  // ==========================================================================

  fn add_edge(&self, src: i64, etype: u32, dst: i64) -> PyResult<()> {
    let src = validation::node_id("src", src)?;
    let dst = validation::node_id("dst", dst)?;
    dispatch_tx!(
      self,
      |db| edges::add_edge_single(db, src, etype as ETypeId, dst),
      |h| edges::add_edge_single(h, src, etype as ETypeId, dst)
    )
  }

  fn add_edge_by_name(&self, src: i64, etype_name: &str, dst: i64) -> PyResult<()> {
    let src = validation::node_id("src", src)?;
    let dst = validation::node_id("dst", dst)?;
    let guard = self.inner.read().map_err(errors::poisoned)?;
    match guard.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        edges::add_edge_by_name_single(db, src, etype_name, dst)
      }
      None => Err(errors::closed()),
    }
  }

  fn delete_edge(&self, src: i64, etype: u32, dst: i64) -> PyResult<()> {
    let src = validation::node_id("src", src)?;
    let dst = validation::node_id("dst", dst)?;
    dispatch_tx!(
      self,
      |db| edges::delete_edge_single(db, src, etype as ETypeId, dst),
      |h| edges::delete_edge_single(h, src, etype as ETypeId, dst)
    )
  }

  fn upsert_edge(
    &self,
    src: i64,
    etype: u32,
    dst: i64,
    props: Vec<(u32, Option<PropValue>)>,
  ) -> PyResult<bool> {
    let src = validation::node_id("src", src)?;
    let dst = validation::node_id("dst", dst)?;
    let core_props: Vec<(PropKeyId, Option<crate::types::PropValue>)> = props
      .into_iter()
      .map(|(k, v)| (k as PropKeyId, v.map(|value| value.into())))
      .collect();

    dispatch_tx!(
      self,
      |db| edges::upsert_edge_single(db, src, etype as ETypeId, dst, &core_props),
      |h| edges::upsert_edge_single(h, src, etype as ETypeId, dst, &core_props)
    )
  }

  fn edge_exists(&self, src: i64, etype: u32, dst: i64) -> PyResult<bool> {
    let src = validation::node_id("src", src)?;
    let dst = validation::node_id("dst", dst)?;
    dispatch_ok!(
      self,
      |db| edges::edge_exists_single(db, src, etype as ETypeId, dst),
      |db| edges::edge_exists_single(db, src, etype as ETypeId, dst)
    )
  }

  #[pyo3(name = "get_out_edges")]
  fn out_edges(&self, node_id: i64) -> PyResult<Vec<Edge>> {
    let node_id = validation::node_id("node_id", node_id)?;
    dispatch_ok!(self, |db| edges::out_edges_single(db, node_id), |db| {
      edges::out_edges_single(db, node_id)
    })
  }

  #[pyo3(name = "get_in_edges")]
  fn in_edges(&self, node_id: i64) -> PyResult<Vec<Edge>> {
    let node_id = validation::node_id("node_id", node_id)?;
    dispatch_ok!(self, |db| edges::in_edges_single(db, node_id), |db| {
      edges::in_edges_single(db, node_id)
    })
  }

  #[pyo3(name = "get_out_degree")]
  fn out_degree(&self, node_id: i64) -> PyResult<i64> {
    let node_id = validation::node_id("node_id", node_id)?;
    dispatch_ok!(self, |db| edges::out_degree_single(db, node_id), |db| {
      edges::out_degree_single(db, node_id)
    })
  }

  #[pyo3(name = "get_in_degree")]
  fn in_degree(&self, node_id: i64) -> PyResult<i64> {
    let node_id = validation::node_id("node_id", node_id)?;
    dispatch_ok!(self, |db| edges::in_degree_single(db, node_id), |db| {
      edges::in_degree_single(db, node_id)
    })
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
    let node_id = validation::node_id("node_id", node_id)?;
    dispatch_tx!(
      self,
      |db| properties::set_node_prop_single(db, node_id, key_id as PropKeyId, value.into()),
      |h| properties::set_node_prop_single(h, node_id, key_id as PropKeyId, value.clone().into())
    )
  }

  fn set_node_prop_by_name(&self, node_id: i64, key_name: &str, value: PropValue) -> PyResult<()> {
    let node_id = validation::node_id("node_id", node_id)?;
    let guard = self.inner.read().map_err(errors::poisoned)?;
    match guard.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        properties::set_node_prop_by_name_single(db, node_id, key_name, value.into())
      }
      None => Err(errors::closed()),
    }
  }

  #[pyo3(name = "get_node_prop")]
  fn node_prop(&self, node_id: i64, key_id: u32) -> PyResult<Option<PropValue>> {
    let node_id = validation::node_id("node_id", node_id)?;
    dispatch_ok!(
      self,
      |db| properties::node_prop_single(db, node_id, key_id as PropKeyId),
      |db| properties::node_prop_single(db, node_id, key_id as PropKeyId)
    )
  }

  fn delete_node_prop(&self, node_id: i64, key_id: u32) -> PyResult<()> {
    let node_id = validation::node_id("node_id", node_id)?;
    dispatch_tx!(
      self,
      |db| properties::delete_node_prop_single(db, node_id, key_id as PropKeyId),
      |h| properties::delete_node_prop_single(h, node_id, key_id as PropKeyId)
    )
  }

  #[pyo3(name = "get_node_props")]
  fn node_props(&self, node_id: i64) -> PyResult<Option<Vec<NodeProp>>> {
    let node_id = validation::node_id("node_id", node_id)?;
    dispatch_ok!(
      self,
      |db| properties::node_props_single(db, node_id),
      |db| properties::node_props_single(db, node_id)
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
    let src = validation::node_id("src", src)?;
    let dst = validation::node_id("dst", dst)?;
    dispatch_tx!(
      self,
      |db| properties::set_edge_prop_single(
        db,
        src,
        etype as ETypeId,
        dst,
        key_id as PropKeyId,
        value.into()
      ),
      |h| properties::set_edge_prop_single(
        h,
        src,
        etype as ETypeId,
        dst,
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
    let src = validation::node_id("src", src)?;
    let dst = validation::node_id("dst", dst)?;
    let guard = self.inner.read().map_err(errors::poisoned)?;
    match guard.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => properties::set_edge_prop_by_name_single(
        db,
        src,
        etype as ETypeId,
        dst,
        key_name,
        value.into(),
      ),
      None => Err(errors::closed()),
    }
  }

  #[pyo3(name = "get_edge_prop")]
  fn edge_prop(&self, src: i64, etype: u32, dst: i64, key_id: u32) -> PyResult<Option<PropValue>> {
    let src = validation::node_id("src", src)?;
    let dst = validation::node_id("dst", dst)?;
    dispatch_ok!(
      self,
      |db| properties::edge_prop_single(db, src, etype as ETypeId, dst, key_id as PropKeyId),
      |db| properties::edge_prop_single(db, src, etype as ETypeId, dst, key_id as PropKeyId)
    )
  }

  fn delete_edge_prop(&self, src: i64, etype: u32, dst: i64, key_id: u32) -> PyResult<()> {
    let src = validation::node_id("src", src)?;
    let dst = validation::node_id("dst", dst)?;
    dispatch_tx!(
      self,
      |db| properties::delete_edge_prop_single(db, src, etype as ETypeId, dst, key_id as PropKeyId),
      |h| properties::delete_edge_prop_single(h, src, etype as ETypeId, dst, key_id as PropKeyId)
    )
  }

  #[pyo3(name = "get_edge_props")]
  fn edge_props(&self, src: i64, etype: u32, dst: i64) -> PyResult<Option<Vec<NodeProp>>> {
    let src = validation::node_id("src", src)?;
    let dst = validation::node_id("dst", dst)?;
    dispatch_ok!(
      self,
      |db| properties::edge_props_single(db, src, etype as ETypeId, dst),
      |db| properties::edge_props_single(db, src, etype as ETypeId, dst)
    )
  }

  // Direct type property getters
  #[pyo3(name = "get_node_prop_string")]
  fn node_prop_string(&self, node_id: i64, key_id: u32) -> PyResult<Option<String>> {
    let node_id = validation::node_id("node_id", node_id)?;
    dispatch_ok!(
      self,
      |db| properties::node_prop_string_single(db, node_id, key_id as PropKeyId),
      |db| properties::node_prop_string_single(db, node_id, key_id as PropKeyId)
    )
  }

  #[pyo3(name = "get_node_prop_int")]
  fn node_prop_int(&self, node_id: i64, key_id: u32) -> PyResult<Option<i64>> {
    let node_id = validation::node_id("node_id", node_id)?;
    dispatch_ok!(
      self,
      |db| properties::node_prop_int_single(db, node_id, key_id as PropKeyId),
      |db| properties::node_prop_int_single(db, node_id, key_id as PropKeyId)
    )
  }

  #[pyo3(name = "get_node_prop_float")]
  fn node_prop_float(&self, node_id: i64, key_id: u32) -> PyResult<Option<f64>> {
    let node_id = validation::node_id("node_id", node_id)?;
    dispatch_ok!(
      self,
      |db| properties::node_prop_float_single(db, node_id, key_id as PropKeyId),
      |db| properties::node_prop_float_single(db, node_id, key_id as PropKeyId)
    )
  }

  #[pyo3(name = "get_node_prop_bool")]
  fn node_prop_bool(&self, node_id: i64, key_id: u32) -> PyResult<Option<bool>> {
    let node_id = validation::node_id("node_id", node_id)?;
    dispatch_ok!(
      self,
      |db| properties::node_prop_bool_single(db, node_id, key_id as PropKeyId),
      |db| properties::node_prop_bool_single(db, node_id, key_id as PropKeyId)
    )
  }

  // ==========================================================================
  // Schema Operations
  // ==========================================================================

  #[pyo3(name = "get_or_create_label")]
  fn ensure_label(&self, name: &str) -> PyResult<u32> {
    dispatch!(self, |db| schema::ensure_label_single(db, name), |db| {
      schema::ensure_label_single(db, name)
    })
  }

  #[pyo3(name = "get_label_id")]
  fn label_id(&self, name: &str) -> PyResult<Option<u32>> {
    dispatch_ok!(self, |db| schema::label_id_single(db, name), |db| {
      schema::label_id_single(db, name)
    })
  }

  #[pyo3(name = "get_label_name")]
  fn label_name(&self, id: u32) -> PyResult<Option<String>> {
    dispatch_ok!(self, |db| schema::label_name_single(db, id), |db| {
      schema::label_name_single(db, id)
    })
  }

  #[pyo3(name = "get_or_create_etype")]
  fn ensure_etype(&self, name: &str) -> PyResult<u32> {
    dispatch!(self, |db| schema::ensure_etype_single(db, name), |db| {
      schema::ensure_etype_single(db, name)
    })
  }

  #[pyo3(name = "get_etype_id")]
  fn etype_id(&self, name: &str) -> PyResult<Option<u32>> {
    dispatch_ok!(self, |db| schema::etype_id_single(db, name), |db| {
      schema::etype_id_single(db, name)
    })
  }

  #[pyo3(name = "get_etype_name")]
  fn etype_name(&self, id: u32) -> PyResult<Option<String>> {
    dispatch_ok!(self, |db| schema::etype_name_single(db, id), |db| {
      schema::etype_name_single(db, id)
    })
  }

  #[pyo3(name = "get_or_create_propkey")]
  fn ensure_propkey(&self, name: &str) -> PyResult<u32> {
    dispatch!(self, |db| schema::ensure_propkey_single(db, name), |db| {
      schema::ensure_propkey_single(db, name)
    })
  }

  #[pyo3(name = "get_propkey_id")]
  fn propkey_id(&self, name: &str) -> PyResult<Option<u32>> {
    dispatch_ok!(self, |db| schema::propkey_id_single(db, name), |db| {
      schema::propkey_id_single(db, name)
    })
  }

  #[pyo3(name = "get_propkey_name")]
  fn propkey_name(&self, id: u32) -> PyResult<Option<String>> {
    dispatch_ok!(self, |db| schema::propkey_name_single(db, id), |db| {
      schema::propkey_name_single(db, id)
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
    let node_id = validation::node_id("node_id", node_id)?;
    dispatch_tx!(
      self,
      |db| labels::add_node_label_single(db, node_id, label_id),
      |h| labels::add_node_label_single(h, node_id, label_id)
    )
  }

  fn add_node_label_by_name(&self, node_id: i64, label_name: &str) -> PyResult<()> {
    let node_id = validation::node_id("node_id", node_id)?;
    let guard = self.inner.read().map_err(errors::poisoned)?;
    match guard.as_ref() {
      Some(DatabaseInner::SingleFile(db)) => {
        labels::add_node_label_by_name_single(db, node_id, label_name)
      }
      None => Err(errors::closed()),
    }
  }

  fn remove_node_label(&self, node_id: i64, label_id: u32) -> PyResult<()> {
    let node_id = validation::node_id("node_id", node_id)?;
    dispatch_tx!(
      self,
      |db| labels::remove_node_label_single(db, node_id, label_id),
      |h| labels::remove_node_label_single(h, node_id, label_id)
    )
  }

  fn node_has_label(&self, node_id: i64, label_id: u32) -> PyResult<bool> {
    let node_id = validation::node_id("node_id", node_id)?;
    dispatch_ok!(
      self,
      |db| labels::node_has_label_single(db, node_id, label_id),
      |db| labels::node_has_label_single(db, node_id, label_id)
    )
  }

  #[pyo3(name = "get_node_labels")]
  fn node_labels(&self, node_id: i64) -> PyResult<Vec<u32>> {
    let node_id = validation::node_id("node_id", node_id)?;
    dispatch_ok!(self, |db| labels::node_labels_single(db, node_id), |db| {
      labels::node_labels_single(db, node_id)
    })
  }

  // ==========================================================================
  // Vector Operations
  // ==========================================================================

  fn set_node_vector(&self, node_id: i64, prop_key_id: u32, vector: Vec<f64>) -> PyResult<()> {
    let node_id = validation::node_id("node_id", node_id)?;
    let v: Vec<f32> = vector.iter().map(|&x| x as f32).collect();
    dispatch_tx!(
      self,
      |db| vectors::set_node_vector_single(db, node_id, prop_key_id as PropKeyId, &v),
      |h| vectors::set_node_vector_single(h, node_id, prop_key_id as PropKeyId, &v)
    )
  }

  #[pyo3(name = "get_node_vector")]
  fn node_vector(&self, node_id: i64, prop_key_id: u32) -> PyResult<Option<Vec<f64>>> {
    let node_id = validation::node_id("node_id", node_id)?;
    dispatch_ok!(
      self,
      |db| vectors::node_vector_single(db, node_id, prop_key_id as PropKeyId),
      |db| vectors::node_vector_single(db, node_id, prop_key_id as PropKeyId)
    )
  }

  fn delete_node_vector(&self, node_id: i64, prop_key_id: u32) -> PyResult<()> {
    let node_id = validation::node_id("node_id", node_id)?;
    dispatch_tx!(
      self,
      |db| vectors::delete_node_vector_single(db, node_id, prop_key_id as PropKeyId),
      |h| vectors::delete_node_vector_single(h, node_id, prop_key_id as PropKeyId)
    )
  }

  fn has_node_vector(&self, node_id: i64, prop_key_id: u32) -> PyResult<bool> {
    let node_id = validation::node_id("node_id", node_id)?;
    dispatch_ok!(
      self,
      |db| vectors::has_node_vector_single(db, node_id, prop_key_id as PropKeyId),
      |db| vectors::has_node_vector_single(db, node_id, prop_key_id as PropKeyId)
    )
  }

  // ==========================================================================
  // Traversal Operations
  // ==========================================================================

  #[pyo3(signature = (node_id, etype=None))]
  fn traverse_out(&self, node_id: i64, etype: Option<u32>) -> PyResult<Vec<i64>> {
    let node_id = validation::node_id("node_id", node_id)?;
    dispatch_ok!(
      self,
      |db| graph_traversal::traverse_out_single(db, node_id, etype),
      |db| graph_traversal::traverse_out_single(db, node_id, etype)
    )
  }

  #[pyo3(signature = (node_id, etype=None))]
  fn traverse_out_with_keys(
    &self,
    node_id: i64,
    etype: Option<u32>,
  ) -> PyResult<Vec<(i64, Option<String>)>> {
    let node_id = validation::node_id("node_id", node_id)?;
    dispatch_ok!(
      self,
      |db| graph_traversal::traverse_out_with_keys_single(db, node_id, etype),
      |db| graph_traversal::traverse_out_with_keys_single(db, node_id, etype)
    )
  }

  #[pyo3(signature = (node_id, etype=None))]
  fn traverse_out_count(&self, node_id: i64, etype: Option<u32>) -> PyResult<i64> {
    let node_id = validation::node_id("node_id", node_id)?;
    dispatch_ok!(
      self,
      |db| graph_traversal::traverse_out_count_single(db, node_id, etype),
      |db| graph_traversal::traverse_out_count_single(db, node_id, etype)
    )
  }

  #[pyo3(signature = (node_id, etype=None))]
  fn traverse_in(&self, node_id: i64, etype: Option<u32>) -> PyResult<Vec<i64>> {
    let node_id = validation::node_id("node_id", node_id)?;
    dispatch_ok!(
      self,
      |db| graph_traversal::traverse_in_single(db, node_id, etype),
      |db| graph_traversal::traverse_in_single(db, node_id, etype)
    )
  }

  #[pyo3(signature = (node_id, etype=None))]
  fn traverse_in_with_keys(
    &self,
    node_id: i64,
    etype: Option<u32>,
  ) -> PyResult<Vec<(i64, Option<String>)>> {
    let node_id = validation::node_id("node_id", node_id)?;
    dispatch_ok!(
      self,
      |db| graph_traversal::traverse_in_with_keys_single(db, node_id, etype),
      |db| graph_traversal::traverse_in_with_keys_single(db, node_id, etype)
    )
  }

  #[pyo3(signature = (node_id, etype=None))]
  fn traverse_in_count(&self, node_id: i64, etype: Option<u32>) -> PyResult<i64> {
    let node_id = validation::node_id("node_id", node_id)?;
    dispatch_ok!(
      self,
      |db| graph_traversal::traverse_in_count_single(db, node_id, etype),
      |db| graph_traversal::traverse_in_count_single(db, node_id, etype)
    )
  }

  fn traverse_multi(
    &self,
    start_ids: Vec<i64>,
    steps: Vec<(String, Option<u32>)>,
  ) -> PyResult<Vec<(i64, Option<String>)>> {
    for &node_id in &start_ids {
      validation::node_id("start_ids", node_id)?;
    }
    let steps = validation::traversal_steps(steps)?;
    self.with_db(|db| Ok(graph_traversal::traverse_multi_single(db, start_ids, steps)))
  }

  fn traverse_multi_count(
    &self,
    start_ids: Vec<i64>,
    steps: Vec<(String, Option<u32>)>,
  ) -> PyResult<i64> {
    for &node_id in &start_ids {
      validation::node_id("start_ids", node_id)?;
    }
    let steps = validation::traversal_steps(steps)?;
    self.with_db(|db| {
      Ok(graph_traversal::traverse_multi_count_single(
        db, start_ids, steps,
      ))
    })
  }

  #[pyo3(signature = (node_id, max_depth, etype=None, min_depth=None, direction=None, unique=None))]
  fn traverse(
    &self,
    node_id: i64,
    max_depth: i64,
    etype: Option<u32>,
    min_depth: Option<i64>,
    direction: Option<String>,
    unique: Option<bool>,
  ) -> PyResult<Vec<PyTraversalResult>> {
    let node_id = validation::node_id("node_id", node_id)?;
    let max_depth = validation::non_negative_usize("max_depth", max_depth, validation::MAX_DEPTH)?;
    let min_depth = min_depth
      .map(|depth| validation::non_negative_usize("min_depth", depth, validation::MAX_DEPTH))
      .transpose()?;
    if min_depth.unwrap_or(1) > max_depth {
      return Err(PyValueError::new_err("min_depth must be <= max_depth"));
    }
    let direction = validation::direction("direction", direction.as_deref())?;
    self.with_db(|db| {
      Ok(graph_traversal::traverse_single(
        db, node_id, max_depth, etype, min_depth, direction, unique,
      ))
    })
  }

  #[pyo3(signature = (source, target, etype=None, max_depth=None, direction=None))]
  fn find_path_bfs(
    &self,
    source: i64,
    target: i64,
    etype: Option<u32>,
    max_depth: Option<i64>,
    direction: Option<String>,
  ) -> PyResult<PyPathResult> {
    let (source, target, max_depth, direction) =
      path_args(source, target, max_depth, direction.as_deref())?;
    self.with_db(|db| {
      Ok(graph_traversal::find_path_bfs_single(
        db, source, target, etype, max_depth, direction,
      ))
    })
  }

  #[pyo3(signature = (source, target, etype=None, max_depth=None, direction=None))]
  fn find_path_dijkstra(
    &self,
    source: i64,
    target: i64,
    etype: Option<u32>,
    max_depth: Option<i64>,
    direction: Option<String>,
  ) -> PyResult<PyPathResult> {
    let (source, target, max_depth, direction) =
      path_args(source, target, max_depth, direction.as_deref())?;
    self.with_db(|db| {
      Ok(graph_traversal::find_path_dijkstra_single(
        db, source, target, etype, max_depth, direction,
      ))
    })
  }

  #[pyo3(signature = (source, target, etype=None, max_depth=None, direction=None))]
  fn has_path(
    &self,
    source: i64,
    target: i64,
    etype: Option<u32>,
    max_depth: Option<i64>,
    direction: Option<String>,
  ) -> PyResult<bool> {
    let (source, target, max_depth, direction) =
      path_args(source, target, max_depth, direction.as_deref())?;
    self.with_db(|db| {
      Ok(
        graph_traversal::find_path_bfs_single(db, source, target, etype, max_depth, direction)
          .found,
      )
    })
  }

  #[pyo3(signature = (source, max_depth, etype=None))]
  fn reachable_nodes(&self, source: i64, max_depth: i64, etype: Option<u32>) -> PyResult<Vec<i64>> {
    let source = validation::node_id("source", source)?;
    let max_depth = validation::non_negative_usize("max_depth", max_depth, validation::MAX_DEPTH)?;
    let results = self.with_db(|db| {
      Ok(graph_traversal::traverse_single(
        db,
        source,
        max_depth,
        etype,
        Some(1),
        TraversalDirection::Out,
        Some(true),
      ))
    })?;
    Ok(results.into_iter().map(|r| r.node_id).collect())
  }

  // ==========================================================================
  // Maintenance Operations
  // ==========================================================================

  fn checkpoint(&self, py: Python<'_>) -> PyResult<()> {
    self.with_db_nogil(py, maintenance::checkpoint_single)
  }

  fn background_checkpoint(&self, py: Python<'_>) -> PyResult<()> {
    self.with_db_nogil(py, maintenance::background_checkpoint_single)
  }

  #[pyo3(signature = (threshold=0.5))]
  fn should_checkpoint(&self, threshold: f64) -> PyResult<bool> {
    let threshold = validation::ratio("threshold", threshold)?;
    dispatch_ok!(
      self,
      |db| maintenance::should_checkpoint_single(db, threshold),
      |_db| false
    )
  }

  #[pyo3(signature = (options=None))]
  fn optimize(&self, py: Python<'_>, options: Option<SingleFileOptimizeOptions>) -> PyResult<()> {
    let opts = options.map(|o| o.to_core()).transpose()?;
    // Shared lock on purpose: core serializes maintenance on its checkpoint gate
    // and waits for open transactions to drain, and those need this lock to
    // commit. Holding the exclusive lock here would deadlock against them.
    self.with_db_nogil(py, |db| maintenance::optimize_single(db, opts))
  }

  #[pyo3(signature = (shrink_wal=true, min_wal_size=None))]
  fn vacuum(&self, py: Python<'_>, shrink_wal: bool, min_wal_size: Option<i64>) -> PyResult<()> {
    let min_wal_size = min_wal_size
      .map(|value| {
        validation::non_negative_u64("min_wal_size", value, validation::MAX_BYTES as u64)
      })
      .transpose()?;
    let options = RustVacuumOptions {
      shrink_wal,
      min_wal_size,
    };
    self.with_db_nogil(py, |db| maintenance::vacuum_single(db, Some(options)))
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
    let node_id = validation::node_id("node_id", node_id)?;
    dispatch_ok!(
      self,
      |db| {
        cache::cache_invalidate_node(db, node_id);
      },
      |_db| ()
    )
  }

  fn cache_invalidate_edge(&self, src: i64, etype: u32, dst: i64) -> PyResult<()> {
    let src = validation::node_id("src", src)?;
    let dst = validation::node_id("dst", dst)?;
    dispatch_ok!(
      self,
      |db| {
        cache::cache_invalidate_edge(db, src, etype as ETypeId, dst);
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

  /// Iterate node ids in batches (lists of ints), built lazily.
  #[pyo3(signature = (options=None))]
  fn stream_nodes(
    slf: PyRef<'_, Self>,
    py: Python<'_>,
    options: Option<StreamOptions>,
  ) -> PyResult<NodeBatchIterator> {
    let opts = options.unwrap_or_default().to_rust()?;
    NodeBatchIterator::new(py, slf.into(), opts, false)
  }

  /// Iterate nodes with keys and properties in batches, built lazily.
  #[pyo3(signature = (options=None))]
  fn stream_nodes_with_props(
    slf: PyRef<'_, Self>,
    py: Python<'_>,
    options: Option<StreamOptions>,
  ) -> PyResult<NodeBatchIterator> {
    let opts = options.unwrap_or_default().to_rust()?;
    NodeBatchIterator::new(py, slf.into(), opts, true)
  }

  /// Iterate edges in batches (lists of FullEdge), built lazily.
  #[pyo3(signature = (options=None))]
  fn stream_edges(
    slf: PyRef<'_, Self>,
    py: Python<'_>,
    options: Option<StreamOptions>,
  ) -> PyResult<EdgeBatchIterator> {
    let opts = options.unwrap_or_default().to_rust()?;
    EdgeBatchIterator::new(py, slf.into(), opts, false)
  }

  /// Iterate edges with properties in batches, built lazily.
  #[pyo3(signature = (options=None))]
  fn stream_edges_with_props(
    slf: PyRef<'_, Self>,
    py: Python<'_>,
    options: Option<StreamOptions>,
  ) -> PyResult<EdgeBatchIterator> {
    let opts = options.unwrap_or_default().to_rust()?;
    EdgeBatchIterator::new(py, slf.into(), opts, true)
  }

  #[pyo3(signature = (options=None))]
  #[pyo3(name = "get_nodes_page")]
  fn nodes_page(&self, options: Option<PaginationOptions>) -> PyResult<NodePage> {
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
  #[pyo3(name = "get_edges_page")]
  fn edges_page(&self, options: Option<PaginationOptions>) -> PyResult<EdgePage> {
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
  fn export_to_json(
    &self,
    py: Python<'_>,
    path: String,
    options: Option<ExportOptions>,
  ) -> PyResult<ExportResult> {
    let opts = options.unwrap_or_default();
    self.with_db_nogil(py, |db| {
      export_import::export_to_json_single(db, path, opts)
    })
  }

  #[pyo3(signature = (path, options=None))]
  fn export_to_jsonl(
    &self,
    py: Python<'_>,
    path: String,
    options: Option<ExportOptions>,
  ) -> PyResult<ExportResult> {
    let opts = options.unwrap_or_default();
    self.with_db_nogil(py, |db| {
      export_import::export_to_jsonl_single(db, path, opts)
    })
  }

  #[pyo3(signature = (path, options=None))]
  fn import_from_json(
    &self,
    py: Python<'_>,
    path: String,
    options: Option<ImportOptions>,
  ) -> PyResult<ImportResult> {
    let opts = options.unwrap_or_default();
    self.with_db_nogil(py, |db| {
      export_import::import_from_json_single(db, path, opts)
    })
  }
}

// ============================================================================
// Standalone Functions
// ============================================================================

#[pyfunction]
#[pyo3(signature = (path, options=None))]
pub fn open_database(
  py: Python<'_>,
  path: String,
  options: Option<OpenOptions>,
) -> PyResult<PyDatabase> {
  PyDatabase::new(py, path, options)
}

#[pyfunction]
pub fn recommended_safe_profile() -> RuntimeProfile {
  RuntimeProfile::from_kite_runtime_profile(RustKiteRuntimeProfile::safe())
}

#[pyfunction]
pub fn recommended_balanced_profile() -> RuntimeProfile {
  RuntimeProfile::from_kite_runtime_profile(RustKiteRuntimeProfile::balanced())
}

#[pyfunction]
pub fn recommended_reopen_heavy_profile() -> RuntimeProfile {
  RuntimeProfile::from_kite_runtime_profile(RustKiteRuntimeProfile::reopen_heavy())
}

#[pyfunction]
pub fn collect_metrics(db: &PyDatabase) -> PyResult<DatabaseMetrics> {
  let guard = db.inner.read().map_err(errors::poisoned)?;
  match guard.as_ref() {
    Some(DatabaseInner::SingleFile(d)) => Ok(DatabaseMetrics::from(
      core_metrics::collect_metrics_single_file(d),
    )),
    None => Err(errors::closed()),
  }
}

#[pyfunction]
pub fn collect_replication_metrics_prometheus(db: &PyDatabase) -> PyResult<String> {
  let guard = db.inner.read().map_err(errors::poisoned)?;
  match guard.as_ref() {
    Some(DatabaseInner::SingleFile(d)) => {
      Ok(core_metrics::collect_replication_metrics_prometheus_single_file(d))
    }
    None => Err(errors::closed()),
  }
}

#[pyfunction]
pub fn collect_replication_metrics_otel_json(db: &PyDatabase) -> PyResult<String> {
  let guard = db.inner.read().map_err(errors::poisoned)?;
  match guard.as_ref() {
    Some(DatabaseInner::SingleFile(d)) => {
      Ok(core_metrics::collect_replication_metrics_otel_json_single_file(d))
    }
    None => Err(errors::closed()),
  }
}

#[pyfunction]
pub fn collect_replication_metrics_otel_protobuf(db: &PyDatabase) -> PyResult<Vec<u8>> {
  let guard = db.inner.read().map_err(errors::poisoned)?;
  match guard.as_ref() {
    Some(DatabaseInner::SingleFile(d)) => {
      Ok(core_metrics::collect_replication_metrics_otel_protobuf_single_file(d))
    }
    None => Err(errors::closed()),
  }
}

#[pyfunction]
#[pyo3(signature = (db, include_data=false))]
pub fn collect_replication_snapshot_transport_json(
  db: &PyDatabase,
  include_data: bool,
) -> PyResult<String> {
  let guard = db.inner.read().map_err(errors::poisoned)?;
  match guard.as_ref() {
    Some(DatabaseInner::SingleFile(d)) => d
      .primary_export_snapshot_transport_json(include_data)
      .map_err(|e| errors::wrap(e, "Failed to export replication snapshot")),
    None => Err(errors::closed()),
  }
}

#[pyfunction]
#[pyo3(signature = (db, cursor=None, max_frames=128, max_bytes=1048576, include_payload=true))]
pub fn collect_replication_log_transport_json(
  db: &PyDatabase,
  cursor: Option<String>,
  max_frames: i64,
  max_bytes: i64,
  include_payload: bool,
) -> PyResult<String> {
  let max_frames = validation::positive_usize("max_frames", max_frames, validation::MAX_COUNT)?;
  let max_bytes = validation::positive_usize("max_bytes", max_bytes, validation::MAX_BYTES)?;

  let guard = db.inner.read().map_err(errors::poisoned)?;
  match guard.as_ref() {
    Some(DatabaseInner::SingleFile(d)) => d
      .primary_export_log_transport_json(cursor.as_deref(), max_frames, max_bytes, include_payload)
      .map_err(|e| errors::wrap(e, "Failed to export replication log")),
    None => Err(errors::closed()),
  }
}

#[allow(clippy::too_many_arguments)]
fn build_otel_push_options_py(
  timeout_ms: i64,
  bearer_token: Option<String>,
  retry_max_attempts: i64,
  retry_backoff_ms: i64,
  retry_backoff_max_ms: i64,
  retry_jitter_ratio: f64,
  adaptive_retry: bool,
  adaptive_retry_mode: Option<String>,
  adaptive_retry_ewma_alpha: f64,
  circuit_breaker_failure_threshold: i64,
  circuit_breaker_open_ms: i64,
  circuit_breaker_half_open_probes: i64,
  circuit_breaker_state_path: Option<String>,
  circuit_breaker_state_url: Option<String>,
  circuit_breaker_state_patch: bool,
  circuit_breaker_state_patch_batch: bool,
  circuit_breaker_state_patch_batch_max_keys: i64,
  circuit_breaker_state_patch_merge: bool,
  circuit_breaker_state_patch_merge_max_keys: i64,
  circuit_breaker_state_patch_retry_max_attempts: i64,
  circuit_breaker_state_cas: bool,
  circuit_breaker_state_lease_id: Option<String>,
  circuit_breaker_scope_key: Option<String>,
  compression_gzip: bool,
  https_only: bool,
  ca_cert_pem_path: Option<String>,
  client_cert_pem_path: Option<String>,
  client_key_pem_path: Option<String>,
) -> PyResult<core_metrics::OtlpHttpPushOptions> {
  let timeout_ms =
    validation::positive_u64("timeout_ms", timeout_ms, validation::MAX_DURATION_MS as u64)?;
  let retry_max_attempts = validation::positive_u32(
    "retry_max_attempts",
    retry_max_attempts,
    validation::MAX_COUNT,
  )?;
  let retry_backoff_ms = validation::non_negative_u64(
    "retry_backoff_ms",
    retry_backoff_ms,
    validation::MAX_DURATION_MS as u64,
  )?;
  let retry_backoff_max_ms = validation::non_negative_u64(
    "retry_backoff_max_ms",
    retry_backoff_max_ms,
    validation::MAX_DURATION_MS as u64,
  )?;
  if retry_backoff_max_ms > 0 && retry_backoff_max_ms < retry_backoff_ms {
    return Err(PyValueError::new_err(
      "retry_backoff_max_ms must be >= retry_backoff_ms when non-zero",
    ));
  }
  let retry_jitter_ratio = validation::ratio("retry_jitter_ratio", retry_jitter_ratio)?;
  let adaptive_retry_mode = match adaptive_retry_mode
    .as_deref()
    .map(str::trim)
    .filter(|value| !value.is_empty())
  {
    None => core_metrics::OtlpAdaptiveRetryMode::Linear,
    Some(value) if value.eq_ignore_ascii_case("linear") => {
      core_metrics::OtlpAdaptiveRetryMode::Linear
    }
    Some(value) if value.eq_ignore_ascii_case("ewma") => core_metrics::OtlpAdaptiveRetryMode::Ewma,
    Some(_) => {
      return Err(errors::KiteError::new_err(
        "adaptive_retry_mode must be one of: linear, ewma",
      ));
    }
  };
  let adaptive_retry_ewma_alpha =
    validation::ratio("adaptive_retry_ewma_alpha", adaptive_retry_ewma_alpha)?;
  let circuit_breaker_failure_threshold = validation::non_negative_u32(
    "circuit_breaker_failure_threshold",
    circuit_breaker_failure_threshold,
    validation::MAX_COUNT,
  )?;
  let circuit_breaker_open_ms = validation::non_negative_u64(
    "circuit_breaker_open_ms",
    circuit_breaker_open_ms,
    validation::MAX_DURATION_MS as u64,
  )?;
  if circuit_breaker_failure_threshold > 0 && circuit_breaker_open_ms == 0 {
    return Err(PyValueError::new_err(
      "circuit_breaker_open_ms must be > 0 when circuit_breaker_failure_threshold is enabled",
    ));
  }
  let circuit_breaker_half_open_probes = validation::non_negative_u32(
    "circuit_breaker_half_open_probes",
    circuit_breaker_half_open_probes,
    validation::MAX_COUNT,
  )?;
  if circuit_breaker_failure_threshold > 0 && circuit_breaker_half_open_probes == 0 {
    return Err(PyValueError::new_err(
      "circuit_breaker_half_open_probes must be > 0 when circuit_breaker_failure_threshold is enabled",
    ));
  }
  if let Some(path) = circuit_breaker_state_path.as_deref() {
    if path.trim().is_empty() {
      return Err(errors::KiteError::new_err(
        "circuit_breaker_state_path must not be empty when provided",
      ));
    }
  }
  if let Some(url) = circuit_breaker_state_url.as_deref() {
    let trimmed = url.trim();
    if trimmed.is_empty() {
      return Err(errors::KiteError::new_err(
        "circuit_breaker_state_url must not be empty when provided",
      ));
    }
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
      return Err(errors::KiteError::new_err(
        "circuit_breaker_state_url must use http:// or https://",
      ));
    }
    if https_only && trimmed.starts_with("http://") {
      return Err(errors::KiteError::new_err(
        "circuit_breaker_state_url must use https when https_only is enabled",
      ));
    }
  }
  if circuit_breaker_state_path.is_some() && circuit_breaker_state_url.is_some() {
    return Err(errors::KiteError::new_err(
      "circuit_breaker_state_path and circuit_breaker_state_url are mutually exclusive",
    ));
  }
  if circuit_breaker_state_patch && circuit_breaker_state_url.is_none() {
    return Err(errors::KiteError::new_err(
      "circuit_breaker_state_patch requires circuit_breaker_state_url",
    ));
  }
  if circuit_breaker_state_patch_batch && !circuit_breaker_state_patch {
    return Err(errors::KiteError::new_err(
      "circuit_breaker_state_patch_batch requires circuit_breaker_state_patch",
    ));
  }
  if circuit_breaker_state_patch_merge && !circuit_breaker_state_patch {
    return Err(errors::KiteError::new_err(
      "circuit_breaker_state_patch_merge requires circuit_breaker_state_patch",
    ));
  }
  let circuit_breaker_state_patch_batch_max_keys = validation::positive_u32(
    "circuit_breaker_state_patch_batch_max_keys",
    circuit_breaker_state_patch_batch_max_keys,
    validation::MAX_COUNT,
  )?;
  let circuit_breaker_state_patch_merge_max_keys = validation::positive_u32(
    "circuit_breaker_state_patch_merge_max_keys",
    circuit_breaker_state_patch_merge_max_keys,
    validation::MAX_COUNT,
  )?;
  let circuit_breaker_state_patch_retry_max_attempts = validation::positive_u32(
    "circuit_breaker_state_patch_retry_max_attempts",
    circuit_breaker_state_patch_retry_max_attempts,
    validation::MAX_COUNT,
  )?;
  if circuit_breaker_state_cas && circuit_breaker_state_url.is_none() {
    return Err(errors::KiteError::new_err(
      "circuit_breaker_state_cas requires circuit_breaker_state_url",
    ));
  }
  if let Some(lease_id) = circuit_breaker_state_lease_id.as_deref() {
    if lease_id.trim().is_empty() {
      return Err(errors::KiteError::new_err(
        "circuit_breaker_state_lease_id must not be empty when provided",
      ));
    }
    if circuit_breaker_state_url.is_none() {
      return Err(errors::KiteError::new_err(
        "circuit_breaker_state_lease_id requires circuit_breaker_state_url",
      ));
    }
  }
  if let Some(scope_key) = circuit_breaker_scope_key.as_deref() {
    if scope_key.trim().is_empty() {
      return Err(errors::KiteError::new_err(
        "circuit_breaker_scope_key must not be empty when provided",
      ));
    }
  }

  Ok(core_metrics::OtlpHttpPushOptions {
    timeout_ms,
    bearer_token,
    retry_max_attempts,
    retry_backoff_ms,
    retry_backoff_max_ms,
    retry_jitter_ratio,
    adaptive_retry_mode,
    adaptive_retry_ewma_alpha,
    adaptive_retry,
    circuit_breaker_failure_threshold,
    circuit_breaker_open_ms,
    circuit_breaker_half_open_probes,
    circuit_breaker_state_path,
    circuit_breaker_state_url,
    circuit_breaker_state_patch,
    circuit_breaker_state_patch_batch,
    circuit_breaker_state_patch_batch_max_keys,
    circuit_breaker_state_patch_merge,
    circuit_breaker_state_patch_merge_max_keys,
    circuit_breaker_state_patch_retry_max_attempts,
    circuit_breaker_state_cas,
    circuit_breaker_state_lease_id,
    circuit_breaker_scope_key,
    compression_gzip,
    tls: core_metrics::OtlpHttpTlsOptions {
      https_only,
      ca_cert_pem_path,
      client_cert_pem_path,
      client_key_pem_path,
    },
  })
}

#[pyfunction]
#[pyo3(signature = (
  db,
  endpoint,
  timeout_ms=5000,
  bearer_token=None,
  retry_max_attempts=1,
  retry_backoff_ms=100,
  retry_backoff_max_ms=2000,
  retry_jitter_ratio=0.0,
  adaptive_retry=false,
  adaptive_retry_mode=None,
  adaptive_retry_ewma_alpha=0.3,
  circuit_breaker_failure_threshold=0,
  circuit_breaker_open_ms=0,
  circuit_breaker_half_open_probes=1,
  circuit_breaker_state_path=None,
  circuit_breaker_state_url=None,
  circuit_breaker_state_patch=false,
  circuit_breaker_state_patch_batch=false,
  circuit_breaker_state_patch_batch_max_keys=8,
  circuit_breaker_state_patch_merge=false,
  circuit_breaker_state_patch_merge_max_keys=32,
  circuit_breaker_state_patch_retry_max_attempts=1,
  circuit_breaker_state_cas=false,
  circuit_breaker_state_lease_id=None,
  circuit_breaker_scope_key=None,
  compression_gzip=false,
  https_only=false,
  ca_cert_pem_path=None,
  client_cert_pem_path=None,
  client_key_pem_path=None
))]
#[allow(clippy::too_many_arguments)]
pub fn push_replication_metrics_otel_json(
  py: Python<'_>,
  db: &PyDatabase,
  endpoint: String,
  timeout_ms: i64,
  bearer_token: Option<String>,
  retry_max_attempts: i64,
  retry_backoff_ms: i64,
  retry_backoff_max_ms: i64,
  retry_jitter_ratio: f64,
  adaptive_retry: bool,
  adaptive_retry_mode: Option<String>,
  adaptive_retry_ewma_alpha: f64,
  circuit_breaker_failure_threshold: i64,
  circuit_breaker_open_ms: i64,
  circuit_breaker_half_open_probes: i64,
  circuit_breaker_state_path: Option<String>,
  circuit_breaker_state_url: Option<String>,
  circuit_breaker_state_patch: bool,
  circuit_breaker_state_patch_batch: bool,
  circuit_breaker_state_patch_batch_max_keys: i64,
  circuit_breaker_state_patch_merge: bool,
  circuit_breaker_state_patch_merge_max_keys: i64,
  circuit_breaker_state_patch_retry_max_attempts: i64,
  circuit_breaker_state_cas: bool,
  circuit_breaker_state_lease_id: Option<String>,
  circuit_breaker_scope_key: Option<String>,
  compression_gzip: bool,
  https_only: bool,
  ca_cert_pem_path: Option<String>,
  client_cert_pem_path: Option<String>,
  client_key_pem_path: Option<String>,
) -> PyResult<(i64, String)> {
  let options = build_otel_push_options_py(
    timeout_ms,
    bearer_token,
    retry_max_attempts,
    retry_backoff_ms,
    retry_backoff_max_ms,
    retry_jitter_ratio,
    adaptive_retry,
    adaptive_retry_mode,
    adaptive_retry_ewma_alpha,
    circuit_breaker_failure_threshold,
    circuit_breaker_open_ms,
    circuit_breaker_half_open_probes,
    circuit_breaker_state_path,
    circuit_breaker_state_url,
    circuit_breaker_state_patch,
    circuit_breaker_state_patch_batch,
    circuit_breaker_state_patch_batch_max_keys,
    circuit_breaker_state_patch_merge,
    circuit_breaker_state_patch_merge_max_keys,
    circuit_breaker_state_patch_retry_max_attempts,
    circuit_breaker_state_cas,
    circuit_breaker_state_lease_id,
    circuit_breaker_scope_key,
    compression_gzip,
    https_only,
    ca_cert_pem_path,
    client_cert_pem_path,
    client_key_pem_path,
  )?;

  db.with_db_nogil(py, |d| {
    let result = core_metrics::push_replication_metrics_otel_json_single_file_with_options(
      d, &endpoint, &options,
    )
    .map_err(|e| errors::wrap(e, "Failed to push replication metrics"))?;
    Ok((result.status_code, result.response_body))
  })
}

#[pyfunction]
#[pyo3(signature = (
  db,
  endpoint,
  timeout_ms=5000,
  bearer_token=None,
  retry_max_attempts=1,
  retry_backoff_ms=100,
  retry_backoff_max_ms=2000,
  retry_jitter_ratio=0.0,
  adaptive_retry=false,
  adaptive_retry_mode=None,
  adaptive_retry_ewma_alpha=0.3,
  circuit_breaker_failure_threshold=0,
  circuit_breaker_open_ms=0,
  circuit_breaker_half_open_probes=1,
  circuit_breaker_state_path=None,
  circuit_breaker_state_url=None,
  circuit_breaker_state_patch=false,
  circuit_breaker_state_patch_batch=false,
  circuit_breaker_state_patch_batch_max_keys=8,
  circuit_breaker_state_patch_merge=false,
  circuit_breaker_state_patch_merge_max_keys=32,
  circuit_breaker_state_patch_retry_max_attempts=1,
  circuit_breaker_state_cas=false,
  circuit_breaker_state_lease_id=None,
  circuit_breaker_scope_key=None,
  compression_gzip=false,
  https_only=false,
  ca_cert_pem_path=None,
  client_cert_pem_path=None,
  client_key_pem_path=None
))]
#[allow(clippy::too_many_arguments)]
pub fn push_replication_metrics_otel_protobuf(
  py: Python<'_>,
  db: &PyDatabase,
  endpoint: String,
  timeout_ms: i64,
  bearer_token: Option<String>,
  retry_max_attempts: i64,
  retry_backoff_ms: i64,
  retry_backoff_max_ms: i64,
  retry_jitter_ratio: f64,
  adaptive_retry: bool,
  adaptive_retry_mode: Option<String>,
  adaptive_retry_ewma_alpha: f64,
  circuit_breaker_failure_threshold: i64,
  circuit_breaker_open_ms: i64,
  circuit_breaker_half_open_probes: i64,
  circuit_breaker_state_path: Option<String>,
  circuit_breaker_state_url: Option<String>,
  circuit_breaker_state_patch: bool,
  circuit_breaker_state_patch_batch: bool,
  circuit_breaker_state_patch_batch_max_keys: i64,
  circuit_breaker_state_patch_merge: bool,
  circuit_breaker_state_patch_merge_max_keys: i64,
  circuit_breaker_state_patch_retry_max_attempts: i64,
  circuit_breaker_state_cas: bool,
  circuit_breaker_state_lease_id: Option<String>,
  circuit_breaker_scope_key: Option<String>,
  compression_gzip: bool,
  https_only: bool,
  ca_cert_pem_path: Option<String>,
  client_cert_pem_path: Option<String>,
  client_key_pem_path: Option<String>,
) -> PyResult<(i64, String)> {
  let options = build_otel_push_options_py(
    timeout_ms,
    bearer_token,
    retry_max_attempts,
    retry_backoff_ms,
    retry_backoff_max_ms,
    retry_jitter_ratio,
    adaptive_retry,
    adaptive_retry_mode,
    adaptive_retry_ewma_alpha,
    circuit_breaker_failure_threshold,
    circuit_breaker_open_ms,
    circuit_breaker_half_open_probes,
    circuit_breaker_state_path,
    circuit_breaker_state_url,
    circuit_breaker_state_patch,
    circuit_breaker_state_patch_batch,
    circuit_breaker_state_patch_batch_max_keys,
    circuit_breaker_state_patch_merge,
    circuit_breaker_state_patch_merge_max_keys,
    circuit_breaker_state_patch_retry_max_attempts,
    circuit_breaker_state_cas,
    circuit_breaker_state_lease_id,
    circuit_breaker_scope_key,
    compression_gzip,
    https_only,
    ca_cert_pem_path,
    client_cert_pem_path,
    client_key_pem_path,
  )?;

  db.with_db_nogil(py, |d| {
    let result = core_metrics::push_replication_metrics_otel_protobuf_single_file_with_options(
      d, &endpoint, &options,
    )
    .map_err(|e| errors::wrap(e, "Failed to push replication metrics"))?;
    Ok((result.status_code, result.response_body))
  })
}

#[pyfunction]
#[pyo3(signature = (
  db,
  endpoint,
  timeout_ms=5000,
  bearer_token=None,
  retry_max_attempts=1,
  retry_backoff_ms=100,
  retry_backoff_max_ms=2000,
  retry_jitter_ratio=0.0,
  adaptive_retry=false,
  adaptive_retry_mode=None,
  adaptive_retry_ewma_alpha=0.3,
  circuit_breaker_failure_threshold=0,
  circuit_breaker_open_ms=0,
  circuit_breaker_half_open_probes=1,
  circuit_breaker_state_path=None,
  circuit_breaker_state_url=None,
  circuit_breaker_state_patch=false,
  circuit_breaker_state_patch_batch=false,
  circuit_breaker_state_patch_batch_max_keys=8,
  circuit_breaker_state_patch_merge=false,
  circuit_breaker_state_patch_merge_max_keys=32,
  circuit_breaker_state_patch_retry_max_attempts=1,
  circuit_breaker_state_cas=false,
  circuit_breaker_state_lease_id=None,
  circuit_breaker_scope_key=None,
  compression_gzip=false,
  https_only=false,
  ca_cert_pem_path=None,
  client_cert_pem_path=None,
  client_key_pem_path=None
))]
#[allow(clippy::too_many_arguments)]
pub fn push_replication_metrics_otel_grpc(
  py: Python<'_>,
  db: &PyDatabase,
  endpoint: String,
  timeout_ms: i64,
  bearer_token: Option<String>,
  retry_max_attempts: i64,
  retry_backoff_ms: i64,
  retry_backoff_max_ms: i64,
  retry_jitter_ratio: f64,
  adaptive_retry: bool,
  adaptive_retry_mode: Option<String>,
  adaptive_retry_ewma_alpha: f64,
  circuit_breaker_failure_threshold: i64,
  circuit_breaker_open_ms: i64,
  circuit_breaker_half_open_probes: i64,
  circuit_breaker_state_path: Option<String>,
  circuit_breaker_state_url: Option<String>,
  circuit_breaker_state_patch: bool,
  circuit_breaker_state_patch_batch: bool,
  circuit_breaker_state_patch_batch_max_keys: i64,
  circuit_breaker_state_patch_merge: bool,
  circuit_breaker_state_patch_merge_max_keys: i64,
  circuit_breaker_state_patch_retry_max_attempts: i64,
  circuit_breaker_state_cas: bool,
  circuit_breaker_state_lease_id: Option<String>,
  circuit_breaker_scope_key: Option<String>,
  compression_gzip: bool,
  https_only: bool,
  ca_cert_pem_path: Option<String>,
  client_cert_pem_path: Option<String>,
  client_key_pem_path: Option<String>,
) -> PyResult<(i64, String)> {
  let options = build_otel_push_options_py(
    timeout_ms,
    bearer_token,
    retry_max_attempts,
    retry_backoff_ms,
    retry_backoff_max_ms,
    retry_jitter_ratio,
    adaptive_retry,
    adaptive_retry_mode,
    adaptive_retry_ewma_alpha,
    circuit_breaker_failure_threshold,
    circuit_breaker_open_ms,
    circuit_breaker_half_open_probes,
    circuit_breaker_state_path,
    circuit_breaker_state_url,
    circuit_breaker_state_patch,
    circuit_breaker_state_patch_batch,
    circuit_breaker_state_patch_batch_max_keys,
    circuit_breaker_state_patch_merge,
    circuit_breaker_state_patch_merge_max_keys,
    circuit_breaker_state_patch_retry_max_attempts,
    circuit_breaker_state_cas,
    circuit_breaker_state_lease_id,
    circuit_breaker_scope_key,
    compression_gzip,
    https_only,
    ca_cert_pem_path,
    client_cert_pem_path,
    client_key_pem_path,
  )?;

  db.with_db_nogil(py, |d| {
    let result = core_metrics::push_replication_metrics_otel_grpc_single_file_with_options(
      d, &endpoint, &options,
    )
    .map_err(|e| errors::wrap(e, "Failed to push replication metrics"))?;
    Ok((result.status_code, result.response_body))
  })
}

#[pyfunction]
pub fn health_check(db: &PyDatabase) -> PyResult<HealthCheckResult> {
  let guard = db.inner.read().map_err(errors::poisoned)?;
  match guard.as_ref() {
    Some(DatabaseInner::SingleFile(d)) => Ok(HealthCheckResult::from(
      core_metrics::health_check_single_file(d),
    )),
    None => Err(errors::closed()),
  }
}

#[pyfunction]
#[pyo3(signature = (db, backup_path, options=None))]
pub fn create_backup(
  py: Python<'_>,
  db: &PyDatabase,
  backup_path: String,
  options: Option<BackupOptions>,
) -> PyResult<BackupResult> {
  let opts: core_backup::BackupOptions = options.unwrap_or_default().into();
  let path = PathBuf::from(backup_path);
  db.with_db_nogil(py, |d| {
    core_backup::create_backup_single_file(d, &path, opts)
      .map(BackupResult::from)
      .map_err(errors::wrap_plain)
  })
}

#[pyfunction]
#[pyo3(signature = (backup_path, restore_path, options=None))]
pub fn restore_backup(
  py: Python<'_>,
  backup_path: String,
  restore_path: String,
  options: Option<RestoreOptions>,
) -> PyResult<String> {
  let opts: core_backup::RestoreOptions = options.unwrap_or_default().into();
  py.detach(|| core_backup::restore_backup(backup_path, restore_path, opts))
    .map(|p| p.to_string_lossy().to_string())
    .map_err(errors::wrap_plain)
}

#[pyfunction]
pub fn backup_info(backup_path: String) -> PyResult<BackupResult> {
  core_backup::backup_info(backup_path)
    .map(BackupResult::from)
    .map_err(errors::wrap_plain)
}

#[pyfunction]
#[pyo3(signature = (db_path, backup_path, options=None))]
pub fn create_offline_backup(
  py: Python<'_>,
  db_path: String,
  backup_path: String,
  options: Option<OfflineBackupOptions>,
) -> PyResult<BackupResult> {
  let opts: core_backup::OfflineBackupOptions = options.unwrap_or_default().into();
  py.detach(|| core_backup::create_offline_backup(db_path, backup_path, opts))
    .map(BackupResult::from)
    .map_err(errors::wrap_plain)
}

/// Validates the shared pathfinding arguments.
fn path_args(
  source: i64,
  target: i64,
  max_depth: Option<i64>,
  direction: Option<&str>,
) -> PyResult<(NodeId, NodeId, Option<usize>, TraversalDirection)> {
  let source = validation::node_id("source", source)?;
  let target = validation::node_id("target", target)?;
  let max_depth = max_depth
    .map(|depth| validation::non_negative_usize("max_depth", depth, validation::MAX_DEPTH))
    .transpose()?;
  let direction = validation::direction("direction", direction)?;
  Ok((source, target, max_depth, direction))
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
