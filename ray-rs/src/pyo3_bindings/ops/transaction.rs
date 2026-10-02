//! Transaction operations for Python bindings

use crate::pyo3_bindings::errors;
use pyo3::prelude::*;

use crate::core::single_file::{Savepoint as RustSavepoint, SingleFileDB as RustSingleFileDB};

/// A savepoint in a write transaction, from `Database.savepoint()`: roll back
/// to it with `Database.rollback_to`, or keep what came after it with
/// `Database.release_savepoint`.
#[pyclass(name = "Savepoint")]
pub struct PySavepoint {
  inner: std::sync::Mutex<Option<RustSavepoint>>,
}

impl PySavepoint {
  /// The core savepoint, taken out (put it back with `put`).
  pub(crate) fn take(&self, action: &str) -> PyResult<RustSavepoint> {
    self
      .inner
      .lock()
      .map_err(errors::poisoned)?
      .take()
      .ok_or_else(|| {
        pyo3::exceptions::PyValueError::new_err(format!("{action}: the savepoint was released"))
      })
  }

  pub(crate) fn put(&self, savepoint: RustSavepoint) -> PyResult<()> {
    *self.inner.lock().map_err(errors::poisoned)? = Some(savepoint);
    Ok(())
  }
}

/// Take a savepoint in the current write transaction
pub fn savepoint_single_file(db: &RustSingleFileDB) -> PyResult<PySavepoint> {
  let savepoint = db
    .savepoint()
    .map_err(|e| errors::wrap(e, "Failed to take a savepoint"))?;
  Ok(PySavepoint {
    inner: std::sync::Mutex::new(Some(savepoint)),
  })
}

/// Roll back to `savepoint`
pub fn rollback_to_single_file(db: &RustSingleFileDB, savepoint: &RustSavepoint) -> PyResult<()> {
  db.rollback_to(savepoint)
    .map_err(|e| errors::wrap(e, "Failed to roll back to the savepoint"))
}

/// Release `savepoint`
pub fn release_savepoint_single_file(
  db: &RustSingleFileDB,
  savepoint: RustSavepoint,
) -> PyResult<()> {
  db.release_savepoint(savepoint)
    .map_err(|e| errors::wrap(e, "Failed to release the savepoint"))
}

/// Trait for transaction operations
pub trait TransactionOps {
  /// Begin a new transaction
  fn begin_impl(&self, read_only: bool) -> PyResult<i64>;

  /// Begin a bulk-load transaction
  fn begin_bulk_impl(&self) -> PyResult<i64>;

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
    .map_err(|e| errors::wrap(e, "Failed to begin transaction"))?;
  Ok(txid as i64)
}

/// Begin bulk-load transaction on single-file database
pub fn begin_bulk_single_file(db: &RustSingleFileDB) -> PyResult<i64> {
  let txid = db
    .begin_bulk()
    .map_err(|e| errors::wrap(e, "Failed to begin bulk transaction"))?;
  Ok(txid as i64)
}

/// Commit transaction on single-file database
pub fn commit_single_file(db: &RustSingleFileDB) -> PyResult<()> {
  db.commit().map_err(|e| errors::wrap(e, "Failed to commit"))
}

/// Rollback transaction on single-file database
pub fn rollback_single_file(db: &RustSingleFileDB) -> PyResult<()> {
  db.rollback()
    .map_err(|e| errors::wrap(e, "Failed to rollback"))
}

#[cfg(test)]
mod tests {
  // Transaction tests require database instances
  // Better tested through integration tests
}
