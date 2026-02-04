//! Backup and restore utilities.
//!
//! Core implementation used by bindings.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::constants::EXT_KITEDB;
use crate::core::single_file::SingleFileDB;
use crate::error::{KiteError, Result};

/// Backup options
#[derive(Debug, Clone)]
pub struct BackupOptions {
  /// Force a checkpoint before backup (single-file only)
  pub checkpoint: bool,
  /// Overwrite existing backup if it exists
  pub overwrite: bool,
}

impl Default for BackupOptions {
  fn default() -> Self {
    Self {
      checkpoint: true,
      overwrite: false,
    }
  }
}

/// Restore options
#[derive(Debug, Clone, Default)]
pub struct RestoreOptions {
  /// Overwrite existing database if it exists
  pub overwrite: bool,
}

/// Offline backup options
#[derive(Debug, Clone, Default)]
pub struct OfflineBackupOptions {
  /// Overwrite existing backup if it exists
  pub overwrite: bool,
}

/// Backup result information
#[derive(Debug, Clone)]
pub struct BackupResult {
  pub path: String,
  pub size: u64,
  pub timestamp_ms: u64,
  pub kind: String,
}

pub fn create_backup_single_file(
  db: &SingleFileDB,
  backup_path: impl AsRef<Path>,
  options: BackupOptions,
) -> Result<BackupResult> {
  let mut backup_path = PathBuf::from(backup_path.as_ref());

  if backup_path.exists() && !options.overwrite {
    return Err(KiteError::Internal(
      "Backup already exists at path (use overwrite: true)".to_string(),
    ));
  }

  if !backup_path.to_string_lossy().ends_with(EXT_KITEDB) {
    backup_path = PathBuf::from(format!("{}{}", backup_path.to_string_lossy(), EXT_KITEDB));
  }

  if options.checkpoint && !db.read_only {
    db.checkpoint()?;
  }

  ensure_parent_dir(&backup_path)?;

  if options.overwrite && backup_path.exists() {
    remove_existing(&backup_path)?;
  }

  copy_file_with_size(&db.path, &backup_path)?;
  let size = fs::metadata(&backup_path)?.len();

  Ok(backup_result(
    &backup_path,
    size,
    "single-file",
    SystemTime::now(),
  ))
}

pub fn restore_backup(
  backup_path: impl AsRef<Path>,
  restore_path: impl AsRef<Path>,
  options: RestoreOptions,
) -> Result<PathBuf> {
  let backup_path = PathBuf::from(backup_path.as_ref());
  let mut restore_path = PathBuf::from(restore_path.as_ref());

  if !backup_path.exists() {
    return Err(KiteError::Internal("Backup not found at path".to_string()));
  }

  if restore_path.exists() && !options.overwrite {
    return Err(KiteError::Internal(
      "Database already exists at restore path (use overwrite: true)".to_string(),
    ));
  }

  let metadata = fs::metadata(&backup_path)?;
  if !metadata.is_file() {
    return Err(KiteError::Internal(
      "Backup path must be a single-file .kitedb backup".to_string(),
    ));
  }

  if !restore_path.to_string_lossy().ends_with(EXT_KITEDB) {
    restore_path = PathBuf::from(format!("{}{}", restore_path.to_string_lossy(), EXT_KITEDB));
  }

  ensure_parent_dir(&restore_path)?;

  if options.overwrite && restore_path.exists() {
    remove_existing(&restore_path)?;
  }

  copy_file_with_size(&backup_path, &restore_path)?;
  Ok(restore_path)
}

pub fn backup_info(backup_path: impl AsRef<Path>) -> Result<BackupResult> {
  let backup_path = PathBuf::from(backup_path.as_ref());
  if !backup_path.exists() {
    return Err(KiteError::Internal("Backup not found at path".to_string()));
  }

  let metadata = fs::metadata(&backup_path)?;
  let timestamp = metadata.modified().unwrap_or(SystemTime::now());

  if metadata.is_file() {
    Ok(backup_result(
      &backup_path,
      metadata.len(),
      "single-file",
      timestamp,
    ))
  } else {
    Err(KiteError::Internal(
      "Backup path must be a single-file .kitedb backup".to_string(),
    ))
  }
}

pub fn create_offline_backup(
  db_path: impl AsRef<Path>,
  backup_path: impl AsRef<Path>,
  options: OfflineBackupOptions,
) -> Result<BackupResult> {
  let db_path = PathBuf::from(db_path.as_ref());
  let backup_path = PathBuf::from(backup_path.as_ref());

  if !db_path.exists() {
    return Err(KiteError::Internal(
      "Database not found at path".to_string(),
    ));
  }

  if backup_path.exists() && !options.overwrite {
    return Err(KiteError::Internal(
      "Backup already exists at path (use overwrite: true)".to_string(),
    ));
  }

  let metadata = fs::metadata(&db_path)?;
  if !metadata.is_file() {
    return Err(KiteError::Internal(
      "Database path must be a single-file .kitedb database".to_string(),
    ));
  }

  ensure_parent_dir(&backup_path)?;
  if options.overwrite && backup_path.exists() {
    remove_existing(&backup_path)?;
  }
  copy_file_with_size(&db_path, &backup_path)?;
  let size = fs::metadata(&backup_path)?.len();
  Ok(backup_result(
    &backup_path,
    size,
    "single-file",
    SystemTime::now(),
  ))
}

fn backup_result(path: &Path, size: u64, kind: &str, timestamp: SystemTime) -> BackupResult {
  BackupResult {
    path: path.to_string_lossy().to_string(),
    size,
    timestamp_ms: system_time_to_millis(timestamp),
    kind: kind.to_string(),
  }
}

fn system_time_to_millis(time: SystemTime) -> u64 {
  time
    .duration_since(UNIX_EPOCH)
    .unwrap_or_default()
    .as_millis() as u64
}

fn ensure_parent_dir(path: &Path) -> Result<()> {
  if let Some(parent) = path.parent() {
    if !parent.exists() {
      fs::create_dir_all(parent)?;
    }
  }
  Ok(())
}

fn remove_existing(path: &Path) -> Result<()> {
  if path.is_dir() {
    fs::remove_dir_all(path)?;
  } else if path.exists() {
    fs::remove_file(path)?;
  }
  Ok(())
}

fn copy_file_with_size(src: &Path, dst: &Path) -> Result<u64> {
  fs::copy(src, dst)?;
  Ok(fs::metadata(dst)?.len())
}
