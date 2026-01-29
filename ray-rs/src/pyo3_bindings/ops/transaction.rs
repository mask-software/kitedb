//! Transaction operations for Python bindings

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use crate::core::single_file::SingleFileDB as RustSingleFileDB;
use crate::graph::db::GraphDB as RustGraphDB;
use crate::graph::tx::{
  begin_read_tx as graph_begin_read_tx, begin_tx as graph_begin_tx, commit as graph_commit,
  rollback as graph_rollback, TxHandle as GraphTxHandle,
};
use crate::types::TxState as GraphTxState;

use std::sync::Mutex;

/// Trait for transaction operations
pub trait TransactionOps {
  /// Begin a new transaction
  fn begin_impl(&self, read_only: bool) -> PyResult<i64>;

  /// Commit the current transaction
  fn commit_impl(&self) -> PyResult<()>;

  /// Rollback the current transaction
  fn rollback_impl(&self) -> PyResult<()>;

  /// Check if there's an active transaction
  fn has_transaction_impl(&self) -> PyResult<bool>;
}

/// Internal database type enum
pub enum DatabaseInner {
  SingleFile(RustSingleFileDB),
  Graph(RustGraphDB),
}

/// Helper for transaction operations on graph databases
pub fn with_graph_tx<F, R>(
  db: &RustGraphDB,
  graph_tx: &Mutex<Option<GraphTxState>>,
  f: F,
) -> PyResult<R>
where
  F: FnOnce(&mut GraphTxHandle) -> PyResult<R>,
{
  let mut guard = graph_tx
    .lock()
    .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
  let tx_state = guard
    .take()
    .ok_or_else(|| PyRuntimeError::new_err("No active transaction"))?;
  let mut handle = GraphTxHandle::new(db, tx_state);
  let result = f(&mut handle);
  *guard = Some(handle.tx);
  result
}

/// Begin transaction on single-file database
pub fn begin_single_file(db: &RustSingleFileDB, read_only: bool) -> PyResult<i64> {
  let txid = db
    .begin(read_only)
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to begin transaction: {e}")))?;
  Ok(txid as i64)
}

/// Begin transaction on graph database
pub fn begin_graph(
  db: &RustGraphDB,
  graph_tx: &Mutex<Option<GraphTxState>>,
  read_only: bool,
) -> PyResult<i64> {
  let mut tx_guard = graph_tx
    .lock()
    .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
  if tx_guard.is_some() {
    return Err(PyRuntimeError::new_err("Transaction already active"));
  }

  let handle = if read_only {
    graph_begin_read_tx(db)
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to begin transaction: {e}")))?
  } else {
    graph_begin_tx(db)
      .map_err(|e| PyRuntimeError::new_err(format!("Failed to begin transaction: {e}")))?
  };
  let txid = handle.tx.txid as i64;
  *tx_guard = Some(handle.tx);
  Ok(txid)
}

/// Commit transaction on single-file database
pub fn commit_single_file(db: &RustSingleFileDB) -> PyResult<()> {
  db.commit()
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to commit: {e}")))
}

/// Commit transaction on graph database
pub fn commit_graph(db: &RustGraphDB, graph_tx: &Mutex<Option<GraphTxState>>) -> PyResult<()> {
  let mut tx_guard = graph_tx
    .lock()
    .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
  let tx_state = tx_guard
    .take()
    .ok_or_else(|| PyRuntimeError::new_err("No active transaction"))?;
  let mut handle = GraphTxHandle::new(db, tx_state);
  graph_commit(&mut handle)
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to commit: {e}")))?;
  Ok(())
}

/// Rollback transaction on single-file database
pub fn rollback_single_file(db: &RustSingleFileDB) -> PyResult<()> {
  db.rollback()
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to rollback: {e}")))
}

/// Rollback transaction on graph database
pub fn rollback_graph(db: &RustGraphDB, graph_tx: &Mutex<Option<GraphTxState>>) -> PyResult<()> {
  let mut tx_guard = graph_tx
    .lock()
    .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
  let tx_state = tx_guard
    .take()
    .ok_or_else(|| PyRuntimeError::new_err("No active transaction"))?;
  let mut handle = GraphTxHandle::new(db, tx_state);
  graph_rollback(&mut handle)
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to rollback: {e}")))?;
  Ok(())
}

#[cfg(test)]
mod tests {
  // Transaction tests require database instances
  // Better tested through integration tests
}
