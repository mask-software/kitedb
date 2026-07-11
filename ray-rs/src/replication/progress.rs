//! Replica progress persistence shared by primary and replicas.

use crate::error::{KiteError, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

const REPLICA_PROGRESS_FILE_NAME: &str = "replica-progress.json";
const REPLICA_PROGRESS_LOCK_FILE_NAME: &str = "replica-progress.lock";
const REPLICA_PROGRESS_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplicaProgress {
  pub epoch: u64,
  pub applied_log_index: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReplicaProgressEnvelope {
  version: u32,
  #[serde(default)]
  replicas: HashMap<String, ReplicaProgress>,
}

pub fn load_replica_progress(sidecar_path: &Path) -> Result<HashMap<String, ReplicaProgress>> {
  std::fs::create_dir_all(sidecar_path)?;
  with_progress_lock(sidecar_path, || {
    read_progress_file(&progress_file_path(sidecar_path))
  })
}

pub fn upsert_replica_progress(
  sidecar_path: &Path,
  replica_id: &str,
  epoch: u64,
  applied_log_index: u64,
) -> Result<()> {
  std::fs::create_dir_all(sidecar_path)?;
  with_progress_lock(sidecar_path, || {
    let file_path = progress_file_path(sidecar_path);
    let mut progress = read_progress_file(&file_path)?;
    progress.insert(
      replica_id.to_string(),
      ReplicaProgress {
        epoch,
        applied_log_index,
      },
    );
    write_progress_file(&file_path, &progress)
  })
}

pub fn clear_replica_progress(sidecar_path: &Path) -> Result<()> {
  std::fs::create_dir_all(sidecar_path)?;
  with_progress_lock(sidecar_path, || {
    write_progress_file(&progress_file_path(sidecar_path), &HashMap::new())
  })
}

fn progress_file_path(sidecar_path: &Path) -> PathBuf {
  sidecar_path.join(REPLICA_PROGRESS_FILE_NAME)
}

fn lock_file_path(sidecar_path: &Path) -> PathBuf {
  sidecar_path.join(REPLICA_PROGRESS_LOCK_FILE_NAME)
}

fn read_progress_file(path: &Path) -> Result<HashMap<String, ReplicaProgress>> {
  if !path.exists() {
    return Ok(HashMap::new());
  }

  let bytes = fs::read(path)?;
  let envelope: ReplicaProgressEnvelope = serde_json::from_slice(&bytes).map_err(|error| {
    KiteError::Serialization(format!("decode replica progress envelope: {error}"))
  })?;

  if envelope.version != REPLICA_PROGRESS_VERSION {
    return Err(KiteError::VersionMismatch {
      required: envelope.version,
      current: REPLICA_PROGRESS_VERSION,
    });
  }

  Ok(envelope.replicas)
}

fn write_progress_file(path: &Path, progress: &HashMap<String, ReplicaProgress>) -> Result<()> {
  let envelope = ReplicaProgressEnvelope {
    version: REPLICA_PROGRESS_VERSION,
    replicas: progress.clone(),
  };
  let bytes = serde_json::to_vec(&envelope).map_err(|error| {
    KiteError::Serialization(format!("encode replica progress envelope: {error}"))
  })?;

  let temp_path = temp_file_path(path);
  let mut file = OpenOptions::new()
    .create(true)
    .truncate(true)
    .write(true)
    .open(&temp_path)?;
  file.write_all(&bytes)?;
  file.sync_all()?;
  fs::rename(&temp_path, path)?;
  sync_parent_dir(path.parent())?;
  Ok(())
}

fn temp_file_path(path: &Path) -> PathBuf {
  match path.extension().and_then(|extension| extension.to_str()) {
    Some(extension) => path.with_extension(format!("{extension}.tmp")),
    None => path.with_extension("tmp"),
  }
}

fn with_progress_lock<T>(sidecar_path: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
  let lock_file = OpenOptions::new()
    .create(true)
    .truncate(false)
    .read(true)
    .write(true)
    .open(lock_file_path(sidecar_path))?;
  lock_file.lock_exclusive()?;

  let result = f();
  let unlock_result = fs2::FileExt::unlock(&lock_file);
  match (result, unlock_result) {
    (Ok(value), Ok(())) => Ok(value),
    (Ok(_), Err(error)) => Err(error.into()),
    (Err(error), _) => Err(error),
  }
}

fn sync_parent_dir(parent: Option<&Path>) -> Result<()> {
  #[cfg(unix)]
  {
    if let Some(parent) = parent {
      std::fs::File::open(parent)?.sync_all()?;
    }
  }

  #[cfg(not(unix))]
  {
    let _ = parent;
  }

  Ok(())
}
