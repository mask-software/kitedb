//! Backup and restore utilities.
//!
//! Core implementation used by bindings.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::constants::EXT_KITEDB;
use crate::core::single_file::SingleFileDB;
use crate::error::{KiteError, Result};
use crate::util::fs::sync_parent_dir;

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
  let backup_path = with_kitedb_extension(backup_path.as_ref());
  check_target(
    &backup_path,
    options.overwrite,
    "Backup already exists at path (use overwrite: true)",
  )?;

  // The copy below holds the checkpoint gate, and a blocking checkpoint that
  // waits for this thread's open transaction to finish would never get it.
  if db.has_transaction() {
    return Err(KiteError::TransactionInProgress);
  }

  if options.checkpoint && !db.read_only {
    db.checkpoint()?;
  }

  ensure_parent_dir(&backup_path)?;

  // Commits take commit_lock; checkpoints, optimize and vacuum take the gate's
  // write side. Holding both keeps every writer out of the file mid-copy.
  let size = {
    let _checkpoint_gate = db.checkpoint_gate.read();
    let _commit_guard = db.commit_lock.lock();
    replace_file_durably(&db.path, &backup_path)?
  };

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
  let restore_path = with_kitedb_extension(restore_path.as_ref());

  if !backup_path.exists() {
    return Err(KiteError::Internal("Backup not found at path".to_string()));
  }

  check_target(
    &restore_path,
    options.overwrite,
    "Database already exists at restore path (use overwrite: true)",
  )?;

  let metadata = fs::metadata(&backup_path)?;
  if !metadata.is_file() {
    return Err(KiteError::Internal(
      "Backup path must be a single-file .kitedb backup".to_string(),
    ));
  }

  ensure_parent_dir(&restore_path)?;
  replace_file_durably(&backup_path, &restore_path)?;
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

  check_target(
    &backup_path,
    options.overwrite,
    "Backup already exists at path (use overwrite: true)",
  )?;

  let metadata = fs::metadata(&db_path)?;
  if !metadata.is_file() {
    return Err(KiteError::Internal(
      "Database path must be a single-file .kitedb database".to_string(),
    ));
  }

  ensure_parent_dir(&backup_path)?;
  let size = replace_file_durably(&db_path, &backup_path)?;
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

fn with_kitedb_extension(path: &Path) -> PathBuf {
  if path.to_string_lossy().ends_with(EXT_KITEDB) {
    return path.to_path_buf();
  }
  let mut with_extension = path.as_os_str().to_os_string();
  with_extension.push(EXT_KITEDB);
  PathBuf::from(with_extension)
}

/// Refuses an existing target unless `overwrite` is set. Never replaces a
/// directory.
fn check_target(path: &Path, overwrite: bool, exists_message: &str) -> Result<()> {
  if !path.exists() {
    return Ok(());
  }
  if !overwrite {
    return Err(KiteError::Internal(exists_message.to_string()));
  }
  if path.is_dir() {
    return Err(KiteError::Internal(format!(
      "Refusing to overwrite a directory: {}",
      path.display()
    )));
  }
  Ok(())
}

/// Copies `src` over `dst` so that `dst` is never partial: the bytes go to a
/// synced temp file in the same directory, which is renamed over `dst`, and
/// the directory is synced so the rename survives a crash. On error `dst` is
/// untouched. Returns the number of bytes copied.
fn replace_file_durably(src: &Path, dst: &Path) -> Result<u64> {
  let mut reader = File::open(src)?;
  let target_lock = lock_existing_target(dst)?;
  let temp_path = temp_path_for(dst);

  let copied = (|| -> Result<u64> {
    let mut writer = OpenOptions::new()
      .write(true)
      .create_new(true)
      .open(&temp_path)?;
    let size = io::copy(&mut reader, &mut writer)?;
    writer.sync_all()?;
    Ok(size)
  })();
  let size = match copied {
    Ok(size) => size,
    Err(error) => {
      let _ = fs::remove_file(&temp_path);
      return Err(error);
    }
  };

  // Windows cannot rename over a file this process still has open. Elsewhere
  // the lock is held through the rename, so nothing opens the old file late.
  #[cfg(windows)]
  drop(target_lock);
  if let Err(error) = fs::rename(&temp_path, dst) {
    let _ = fs::remove_file(&temp_path);
    return Err(error.into());
  }
  #[cfg(not(windows))]
  drop(target_lock);

  sync_parent_dir(dst)?;
  Ok(size)
}

/// Takes the exclusive file lock that an open database's pager holds, so a
/// database open in this or another process is never replaced underneath it.
/// Returns `None` when `path` does not exist.
fn lock_existing_target(path: &Path) -> Result<Option<File>> {
  let file = match File::open(path) {
    Ok(file) => file,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(error.into()),
  };

  #[cfg(not(target_arch = "wasm32"))]
  fs2::FileExt::try_lock_exclusive(&file).map_err(|error| {
    KiteError::LockFailed(format!(
      "database is open, refusing to replace it: {} ({error})",
      path.display()
    ))
  })?;

  Ok(Some(file))
}

fn temp_path_for(path: &Path) -> PathBuf {
  static SEQUENCE: AtomicU64 = AtomicU64::new(0);
  let file_name = path
    .file_name()
    .map(|name| name.to_string_lossy().into_owned())
    .unwrap_or_default();
  path.with_file_name(format!(
    ".{file_name}.{}-{}-{}.tmp",
    std::process::id(),
    system_time_to_millis(SystemTime::now()),
    SEQUENCE.fetch_add(1, Ordering::Relaxed)
  ))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::core::single_file::{close_single_file, open_single_file, SingleFileOpenOptions};
  use std::collections::HashSet;
  use std::sync::atomic::AtomicBool;
  use std::sync::Arc;

  fn options() -> SingleFileOpenOptions {
    SingleFileOpenOptions::new()
      .auto_checkpoint(false)
      .background_checkpoint(false)
  }

  fn commit_node(db: &SingleFileDB, key: &str) {
    db.begin(false).expect("begin");
    db.create_node(Some(key)).expect("create node");
    db.commit().expect("commit");
  }

  fn node_keys(path: &Path) -> HashSet<String> {
    let db = open_single_file(path, options()).expect("open backup");
    let keys = db
      .list_nodes()
      .into_iter()
      .map(|id| db.node_key(id).expect("node key"))
      .collect();
    close_single_file(db).expect("close backup");
    keys
  }

  #[test]
  fn backup_refuses_open_transaction_on_calling_thread() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = open_single_file(dir.path().join("db.kitedb"), options()).expect("open");
    db.begin(true).expect("begin");

    let result = create_backup_single_file(
      &db,
      dir.path().join("backup"),
      BackupOptions {
        checkpoint: false,
        overwrite: false,
      },
    );

    assert!(matches!(result, Err(KiteError::TransactionInProgress)));
    assert!(!dir.path().join("backup.kitedb").exists());
    db.rollback().expect("rollback");
    close_single_file(db).expect("close");
  }

  #[test]
  fn overwrite_replaces_existing_backup_and_leaves_no_temp_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = open_single_file(dir.path().join("db.kitedb"), options()).expect("open");
    commit_node(&db, "first");
    let backup_options = BackupOptions {
      checkpoint: false,
      overwrite: true,
    };
    let first = create_backup_single_file(&db, dir.path().join("backup"), backup_options.clone())
      .expect("first backup");
    commit_node(&db, "second");
    let second =
      create_backup_single_file(&db, &first.path, backup_options).expect("second backup");
    close_single_file(db).expect("close");

    assert_eq!(first.path, second.path);
    assert_eq!(second.size, fs::metadata(&second.path).expect("stat").len());
    let expected: HashSet<String> = ["first", "second"].map(String::from).into();
    assert_eq!(node_keys(Path::new(&second.path)), expected);

    let restored = restore_backup(
      &second.path,
      dir.path().join("db"),
      RestoreOptions { overwrite: true },
    )
    .expect("restore over the closed source");
    assert_eq!(node_keys(&restored), expected);

    let mut entries: Vec<String> = fs::read_dir(dir.path())
      .expect("read dir")
      .map(|entry| {
        entry
          .expect("entry")
          .file_name()
          .to_string_lossy()
          .into_owned()
      })
      .collect();
    entries.sort();
    assert_eq!(entries, ["backup.kitedb", "db.kitedb"]);
  }

  #[test]
  fn overwrite_refuses_directory_target() {
    let dir = tempfile::tempdir().expect("tempdir");
    let backup = dir.path().join("backup.kitedb");
    let db = open_single_file(&backup, options()).expect("open");
    commit_node(&db, "node");
    close_single_file(db).expect("close");
    let target = dir.path().join("target.kitedb");
    fs::create_dir(&target).expect("mkdir");
    fs::write(target.join("keep"), b"keep").expect("write");

    let result = restore_backup(&backup, &target, RestoreOptions { overwrite: true });

    assert!(
      result.is_err(),
      "restore over a directory returned {result:?}"
    );
    assert_eq!(fs::read(target.join("keep")).expect("kept"), b"keep");
  }

  /// Commits are sequential, so a backup that no writer tore mid-copy holds
  /// exactly the first N committed keys.
  #[test]
  fn backups_taken_during_commits_and_checkpoints_are_consistent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Arc::new(open_single_file(dir.path().join("db.kitedb"), options()).expect("open"));
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
      let (db, stop) = (Arc::clone(&db), Arc::clone(&stop));
      std::thread::spawn(move || {
        let mut index = 0usize;
        while !stop.load(Ordering::Acquire) && index < 5_000 {
          commit_node(&db, &format!("n{index}"));
          index += 1;
          match index % 50 {
            0 => db.checkpoint().expect("checkpoint"),
            25 => db.background_checkpoint().expect("background checkpoint"),
            _ => {}
          }
        }
      })
    };

    let backup = dir.path().join("backup.kitedb");
    for _ in 0..25 {
      create_backup_single_file(
        &db,
        &backup,
        BackupOptions {
          checkpoint: false,
          overwrite: true,
        },
      )
      .expect("backup");
      let keys = node_keys(&backup);
      let expected: HashSet<String> = (0..keys.len()).map(|index| format!("n{index}")).collect();
      assert_eq!(
        keys, expected,
        "backup is not a prefix of the commit sequence"
      );
    }
    stop.store(true, Ordering::Release);
    writer.join().expect("writer");
  }
}
