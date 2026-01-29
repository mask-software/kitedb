//! Backup and restore utilities.
//!
//! Core implementation used by bindings.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::constants::{EXT_RAYDB, MANIFEST_FILENAME, SNAPSHOTS_DIR, WAL_DIR};
use crate::core::single_file::SingleFileDB;
use crate::error::{RayError, Result};
use crate::graph::db::GraphDB;

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
    return Err(RayError::Internal(
      "Backup already exists at path (use overwrite: true)".to_string(),
    ));
  }

  if !backup_path.to_string_lossy().ends_with(EXT_RAYDB) {
    backup_path = PathBuf::from(format!("{}{}", backup_path.to_string_lossy(), EXT_RAYDB));
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

pub fn create_backup_graph(
  db: &GraphDB,
  backup_path: impl AsRef<Path>,
  options: BackupOptions,
) -> Result<BackupResult> {
  let backup_path = PathBuf::from(backup_path.as_ref());

  if backup_path.exists() && !options.overwrite {
    return Err(RayError::Internal(
      "Backup already exists at path (use overwrite: true)".to_string(),
    ));
  }

  if options.overwrite && backup_path.exists() {
    remove_existing(&backup_path)?;
  }

  fs::create_dir_all(&backup_path)?;
  fs::create_dir_all(backup_path.join(SNAPSHOTS_DIR))?;
  fs::create_dir_all(backup_path.join(WAL_DIR))?;

  let mut total_size = 0u64;

  let manifest_src = db.path.join(MANIFEST_FILENAME);
  if manifest_src.exists() {
    total_size += copy_file_with_size(&manifest_src, &backup_path.join(MANIFEST_FILENAME))?;
  }

  let snapshots_dir = db.path.join(SNAPSHOTS_DIR);
  if snapshots_dir.exists() {
    for entry in fs::read_dir(&snapshots_dir)? {
      let entry = entry?;
      let src = entry.path();
      let dst = backup_path.join(SNAPSHOTS_DIR).join(entry.file_name());
      if src.is_file() {
        total_size += copy_file_with_size(&src, &dst)?;
      }
    }
  }

  let wal_dir = db.path.join(WAL_DIR);
  if wal_dir.exists() {
    for entry in fs::read_dir(&wal_dir)? {
      let entry = entry?;
      let src = entry.path();
      let dst = backup_path.join(WAL_DIR).join(entry.file_name());
      if src.is_file() {
        total_size += copy_file_with_size(&src, &dst)?;
      }
    }
  }

  Ok(backup_result(
    &backup_path,
    total_size,
    "multi-file",
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
    return Err(RayError::Internal("Backup not found at path".to_string()));
  }

  if restore_path.exists() && !options.overwrite {
    return Err(RayError::Internal(
      "Database already exists at restore path (use overwrite: true)".to_string(),
    ));
  }

  let metadata = fs::metadata(&backup_path)?;
  if metadata.is_file() {
    if !restore_path.to_string_lossy().ends_with(EXT_RAYDB) {
      restore_path = PathBuf::from(format!("{}{}", restore_path.to_string_lossy(), EXT_RAYDB));
    }

    ensure_parent_dir(&restore_path)?;

    if options.overwrite && restore_path.exists() {
      remove_existing(&restore_path)?;
    }

    copy_file_with_size(&backup_path, &restore_path)?;
    Ok(restore_path)
  } else if metadata.is_dir() {
    if options.overwrite && restore_path.exists() {
      remove_existing(&restore_path)?;
    }
    let _size = copy_dir_recursive(&backup_path, &restore_path)?;
    Ok(restore_path)
  } else {
    Err(RayError::Internal(
      "Backup path is not a file or directory".to_string(),
    ))
  }
}

pub fn get_backup_info(backup_path: impl AsRef<Path>) -> Result<BackupResult> {
  let backup_path = PathBuf::from(backup_path.as_ref());
  if !backup_path.exists() {
    return Err(RayError::Internal("Backup not found at path".to_string()));
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
  } else if metadata.is_dir() {
    let size = dir_size(&backup_path)?;
    Ok(backup_result(&backup_path, size, "multi-file", timestamp))
  } else {
    Err(RayError::Internal(
      "Backup path is not a file or directory".to_string(),
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
    return Err(RayError::Internal("Database not found at path".to_string()));
  }

  if backup_path.exists() && !options.overwrite {
    return Err(RayError::Internal(
      "Backup already exists at path (use overwrite: true)".to_string(),
    ));
  }

  let metadata = fs::metadata(&db_path)?;
  if metadata.is_file() {
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
  } else if metadata.is_dir() {
    if options.overwrite && backup_path.exists() {
      remove_existing(&backup_path)?;
    }
    let size = copy_dir_recursive(&db_path, &backup_path)?;
    Ok(backup_result(
      &backup_path,
      size,
      "multi-file",
      SystemTime::now(),
    ))
  } else {
    Err(RayError::Internal(
      "Database path is not a file or directory".to_string(),
    ))
  }
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

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<u64> {
  fs::create_dir_all(dst)?;
  let mut total = 0u64;
  for entry in fs::read_dir(src)? {
    let entry = entry?;
    let src_path = entry.path();
    let dst_path = dst.join(entry.file_name());
    let metadata = entry.metadata()?;
    if metadata.is_dir() {
      total += copy_dir_recursive(&src_path, &dst_path)?;
    } else {
      total += copy_file_with_size(&src_path, &dst_path)?;
    }
  }
  Ok(total)
}

fn dir_size(path: &Path) -> Result<u64> {
  let mut total = 0u64;
  for entry in fs::read_dir(path)? {
    let entry = entry?;
    let metadata = entry.metadata()?;
    if metadata.is_dir() {
      total += dir_size(&entry.path())?;
    } else {
      total += metadata.len();
    }
  }
  Ok(total)
}
