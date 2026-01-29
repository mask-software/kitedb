//! Backup and restore options for Python bindings

use crate::backup as core_backup;
use pyo3::prelude::*;

/// Options for creating a backup
#[pyclass(name = "BackupOptions")]
#[derive(Debug, Clone, Default)]
pub struct BackupOptions {
  #[pyo3(get, set)]
  pub checkpoint: Option<bool>,
  #[pyo3(get, set)]
  pub overwrite: Option<bool>,
}

#[pymethods]
impl BackupOptions {
  #[new]
  #[pyo3(signature = (checkpoint=None, overwrite=None))]
  fn new(checkpoint: Option<bool>, overwrite: Option<bool>) -> Self {
    Self {
      checkpoint,
      overwrite,
    }
  }

  fn __repr__(&self) -> String {
    format!(
      "BackupOptions(checkpoint={:?}, overwrite={:?})",
      self.checkpoint, self.overwrite
    )
  }
}

impl From<BackupOptions> for core_backup::BackupOptions {
  fn from(options: BackupOptions) -> Self {
    Self {
      checkpoint: options.checkpoint.unwrap_or(true),
      overwrite: options.overwrite.unwrap_or(false),
    }
  }
}

/// Options for restoring a backup
#[pyclass(name = "RestoreOptions")]
#[derive(Debug, Clone, Default)]
pub struct RestoreOptions {
  #[pyo3(get, set)]
  pub overwrite: Option<bool>,
}

#[pymethods]
impl RestoreOptions {
  #[new]
  #[pyo3(signature = (overwrite=None))]
  fn new(overwrite: Option<bool>) -> Self {
    Self { overwrite }
  }

  fn __repr__(&self) -> String {
    format!("RestoreOptions(overwrite={:?})", self.overwrite)
  }
}

impl From<RestoreOptions> for core_backup::RestoreOptions {
  fn from(options: RestoreOptions) -> Self {
    Self {
      overwrite: options.overwrite.unwrap_or(false),
    }
  }
}

/// Options for offline backup
#[pyclass(name = "OfflineBackupOptions")]
#[derive(Debug, Clone, Default)]
pub struct OfflineBackupOptions {
  #[pyo3(get, set)]
  pub overwrite: Option<bool>,
}

#[pymethods]
impl OfflineBackupOptions {
  #[new]
  #[pyo3(signature = (overwrite=None))]
  fn new(overwrite: Option<bool>) -> Self {
    Self { overwrite }
  }

  fn __repr__(&self) -> String {
    format!("OfflineBackupOptions(overwrite={:?})", self.overwrite)
  }
}

impl From<OfflineBackupOptions> for core_backup::OfflineBackupOptions {
  fn from(options: OfflineBackupOptions) -> Self {
    Self {
      overwrite: options.overwrite.unwrap_or(false),
    }
  }
}

/// Backup result information
#[pyclass(name = "BackupResult")]
#[derive(Debug, Clone)]
pub struct BackupResult {
  #[pyo3(get)]
  pub path: String,
  #[pyo3(get)]
  pub size: i64,
  #[pyo3(get)]
  pub timestamp: i64,
  #[pyo3(get)]
  pub r#type: String,
}

#[pymethods]
impl BackupResult {
  fn __repr__(&self) -> String {
    format!(
      "BackupResult(path='{}', size={}, type='{}')",
      self.path, self.size, self.r#type
    )
  }
}

impl From<core_backup::BackupResult> for BackupResult {
  fn from(result: core_backup::BackupResult) -> Self {
    BackupResult {
      path: result.path,
      size: result.size as i64,
      timestamp: result.timestamp_ms as i64,
      r#type: result.kind,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_backup_options_default() {
    let opts = BackupOptions::default();
    let core: core_backup::BackupOptions = opts.into();
    assert!(core.checkpoint); // defaults to true
    assert!(!core.overwrite); // defaults to false
  }

  #[test]
  fn test_backup_options_custom() {
    let opts = BackupOptions {
      checkpoint: Some(false),
      overwrite: Some(true),
    };
    let core: core_backup::BackupOptions = opts.into();
    assert!(!core.checkpoint);
    assert!(core.overwrite);
  }

  #[test]
  fn test_restore_options_default() {
    let opts = RestoreOptions::default();
    let core: core_backup::RestoreOptions = opts.into();
    assert!(!core.overwrite);
  }

  #[test]
  fn test_offline_backup_options_default() {
    let opts = OfflineBackupOptions::default();
    let core: core_backup::OfflineBackupOptions = opts.into();
    assert!(!core.overwrite);
  }
}
