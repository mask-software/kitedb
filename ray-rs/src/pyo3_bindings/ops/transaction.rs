//! Transaction operations for Python bindings

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use crate::core::single_file::SingleFileDB as RustSingleFileDB;

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

/// Begin transaction on single-file database
pub fn begin_single_file(db: &RustSingleFileDB, read_only: bool) -> PyResult<i64> {
  let txid = db
    .begin(read_only)
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to begin transaction: {e}")))?;
  Ok(txid as i64)
}

/// Commit transaction on single-file database
pub fn commit_single_file(db: &RustSingleFileDB) -> PyResult<()> {
  db.commit()
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to commit: {e}")))
}

/// Rollback transaction on single-file database
pub fn rollback_single_file(db: &RustSingleFileDB) -> PyResult<()> {
  db.rollback()
    .map_err(|e| PyRuntimeError::new_err(format!("Failed to rollback: {e}")))
}

#[cfg(test)]
mod tests {
  // Transaction tests require database instances
  // Better tested through integration tests
}
