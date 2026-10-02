//! Python exception hierarchy for KiteDB errors.
//!
//! Every error the native module raises for a failed database operation is a
//! `KiteError`. It subclasses `RuntimeError`, which earlier versions raised for
//! everything, so existing `except RuntimeError` handlers keep working. Core
//! error variants map to subclasses so callers can catch the case they handle.
//! Invalid arguments still raise `ValueError`.

use std::any::Any;
use std::fmt::Display;

use pyo3::create_exception;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use crate::error::KiteError as CoreError;

create_exception!(
  kitedb._kitedb,
  KiteError,
  PyRuntimeError,
  "Base class for errors raised by KiteDB (a RuntimeError subclass)."
);
create_exception!(
  kitedb._kitedb,
  ConflictError,
  KiteError,
  "A transaction conflicted with a concurrent commit (MVCC); retry it."
);
create_exception!(
  kitedb._kitedb,
  ReadOnlyError,
  KiteError,
  "A write was attempted on a read-only database."
);
create_exception!(
  kitedb._kitedb,
  NotFoundError,
  KiteError,
  "A node, edge or key does not exist."
);
create_exception!(
  kitedb._kitedb,
  ClosedError,
  KiteError,
  "The database handle is closed."
);
create_exception!(
  kitedb._kitedb,
  TransactionError,
  KiteError,
  "No transaction is open on this thread, or one already is."
);
create_exception!(
  kitedb._kitedb,
  DuplicateKeyError,
  KiteError,
  "A node with this key already exists."
);
create_exception!(
  kitedb._kitedb,
  LockError,
  KiteError,
  "The database file lock could not be acquired (another process has it open)."
);
create_exception!(
  kitedb._kitedb,
  CorruptionError,
  KiteError,
  "On-disk data failed validation (bad magic, checksum, snapshot or WAL)."
);
create_exception!(
  kitedb._kitedb,
  WalFullError,
  KiteError,
  "The WAL is full; checkpoint before writing more."
);

/// Adds the exception classes to the native module.
pub(crate) fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
  let py = m.py();
  m.add("KiteError", py.get_type::<KiteError>())?;
  m.add("ConflictError", py.get_type::<ConflictError>())?;
  m.add("ReadOnlyError", py.get_type::<ReadOnlyError>())?;
  m.add("NotFoundError", py.get_type::<NotFoundError>())?;
  m.add("ClosedError", py.get_type::<ClosedError>())?;
  m.add("TransactionError", py.get_type::<TransactionError>())?;
  m.add("DuplicateKeyError", py.get_type::<DuplicateKeyError>())?;
  m.add("LockError", py.get_type::<LockError>())?;
  m.add("CorruptionError", py.get_type::<CorruptionError>())?;
  m.add("WalFullError", py.get_type::<WalFullError>())?;
  Ok(())
}

/// The Python exception for a core error, with `message` as its text.
pub(crate) fn core_error(err: &CoreError, message: String) -> PyErr {
  match err {
    CoreError::Conflict { .. } => ConflictError::new_err(message),
    CoreError::ReadOnly => ReadOnlyError::new_err(message),
    CoreError::NodeNotFound(_) | CoreError::EdgeNotFound { .. } | CoreError::KeyNotFound(_) => {
      NotFoundError::new_err(message)
    }
    CoreError::DatabaseClosed => ClosedError::new_err(message),
    CoreError::NoTransaction | CoreError::TransactionInProgress => {
      TransactionError::new_err(message)
    }
    CoreError::DuplicateKey(_) => DuplicateKeyError::new_err(message),
    CoreError::LockFailed(_) => LockError::new_err(message),
    CoreError::InvalidMagic { .. }
    | CoreError::CrcMismatch { .. }
    | CoreError::InvalidSnapshot(_)
    | CoreError::InvalidWal(_)
    | CoreError::InvalidSection(_)
    | CoreError::InvalidPropTag(_)
    | CoreError::InvalidWalRecordType(_) => CorruptionError::new_err(message),
    CoreError::WalBufferFull => WalFullError::new_err(message),
    _ => KiteError::new_err(message),
  }
}

fn classify<E: Display + 'static>(err: &E, message: String) -> PyErr {
  match (err as &dyn Any).downcast_ref::<CoreError>() {
    Some(core) => core_error(core, message),
    None => KiteError::new_err(message),
  }
}

/// Wraps an operation error as "`context`: `err`". Core errors map to their
/// subclass; any other error type becomes a plain `KiteError`.
pub(crate) fn wrap<E: Display + 'static>(err: E, context: &str) -> PyErr {
  let message = format!("{context}: {err}");
  classify(&err, message)
}

/// Like [`wrap`], with the error's own text as the message.
pub(crate) fn wrap_plain<E: Display + 'static>(err: E) -> PyErr {
  let message = err.to_string();
  classify(&err, message)
}

/// A poisoned binding lock (a thread panicked while holding it).
pub(crate) fn poisoned<E: Display>(err: E) -> PyErr {
  KiteError::new_err(format!("internal lock poisoned: {err}"))
}

/// The error for any call on a closed database handle.
pub(crate) fn closed() -> PyErr {
  ClosedError::new_err("Database is closed")
}
