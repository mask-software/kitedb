//! Background checkpointing
//!
//! Checkpoint (compaction) merges the delta state into a new snapshot,
//! allowing the WAL to be truncated. This is important for:
//! - Reducing WAL size
//! - Improving read performance (fewer delta lookups)
//! - Controlling memory usage

use std::collections::HashSet;
use std::fs;
use std::sync::Arc;

use crate::constants::{parse_snapshot_gen, SNAPSHOTS_DIR, TRASH_DIR};
use crate::error::{RayError, Result};

use super::db::GraphDB;

// ============================================================================
// Checkpoint State
// ============================================================================

/// State of an ongoing checkpoint
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointStatus {
  /// No checkpoint in progress
  Idle,
  /// Checkpoint is running
  Running,
  /// Checkpoint is completing (finalizing)
  Completing,
}

/// Statistics from a checkpoint operation
#[derive(Debug, Clone)]
pub struct CheckpointStats {
  /// Number of nodes in the new snapshot
  pub num_nodes: u64,
  /// Number of edges in the new snapshot
  pub num_edges: u64,
  /// New snapshot generation number
  pub snapshot_gen: u64,
  /// Time taken in milliseconds
  pub duration_ms: u64,
}

// ============================================================================
// Checkpoint Operations
// ============================================================================

/// Check if a checkpoint should be triggered
/// Returns true if the WAL size or delta size exceeds thresholds
pub fn should_checkpoint(db: &GraphDB) -> bool {
  let delta = db.delta.read();

  // Simple heuristic: checkpoint if delta has significant data
  let created_nodes = delta.created_nodes.len();
  let deleted_nodes = delta.deleted_nodes.len();
  let total_edges = delta.total_edges_added() + delta.total_edges_deleted();

  // Trigger checkpoint if we have more than 10k modifications
  created_nodes + deleted_nodes + total_edges > 10_000
}

/// Check if a checkpoint is currently running
pub fn is_checkpoint_running(db: &GraphDB) -> bool {
  matches!(
    *db.checkpoint_status.lock(),
    CheckpointStatus::Running | CheckpointStatus::Completing
  )
}

/// Trigger a blocking checkpoint
/// This will:
/// 1. Build a new snapshot from current state
/// 2. Write the snapshot to disk
/// 3. Clear the delta state
/// 4. Reset the WAL
fn checkpoint_impl(db: &mut GraphDB, manage_status: bool) -> Result<CheckpointStats> {
  use std::time::Instant;

  let start = Instant::now();

  if manage_status {
    let mut status = db.checkpoint_status.lock();
    if !matches!(*status, CheckpointStatus::Idle) {
      return Err(RayError::Internal("Checkpoint already running".to_string()));
    }
    *status = CheckpointStatus::Running;
  }

  // Use the GraphDB::optimize() method which handles all the checkpoint logic
  if let Err(err) = db.optimize() {
    *db.checkpoint_status.lock() = CheckpointStatus::Idle;
    return Err(err);
  }

  *db.checkpoint_status.lock() = CheckpointStatus::Completing;

  let duration_ms = start.elapsed().as_millis() as u64;

  // Get stats from the new snapshot
  let (num_nodes, num_edges, snapshot_gen) = if let Some(ref snapshot) = db.snapshot {
    (
      snapshot.header.num_nodes,
      snapshot.header.num_edges,
      snapshot.header.generation,
    )
  } else {
    (0, 0, 0)
  };

  *db.checkpoint_status.lock() = CheckpointStatus::Idle;

  Ok(CheckpointStats {
    num_nodes,
    num_edges,
    snapshot_gen,
    duration_ms,
  })
}

/// Trigger a blocking checkpoint
/// This will:
/// 1. Build a new snapshot from current state
/// 2. Write the snapshot to disk
/// 3. Clear the delta state
/// 4. Reset the WAL
pub fn checkpoint(db: &mut GraphDB) -> Result<CheckpointStats> {
  checkpoint_impl(db, true)
}

/// Trigger a background (non-blocking) checkpoint
/// For single-file format, this switches writes to the secondary WAL region
/// while the checkpoint runs
pub fn trigger_background_checkpoint(db: &mut GraphDB) -> Result<()> {
  if is_checkpoint_running(db) {
    return Ok(());
  }

  checkpoint(db).map(|_| ())
}

/// Trigger a background checkpoint using a shared, lock-protected GraphDB.
///
/// This is the safe async entrypoint for multi-file databases when the caller
/// holds the DB behind `Arc<Mutex<_>>`.
#[cfg(not(target_arch = "wasm32"))]
pub fn trigger_background_checkpoint_async(db: Arc<parking_lot::Mutex<GraphDB>>) -> Result<()> {
  let db_guard = db.lock();
  {
    let mut status = db_guard.checkpoint_status.lock();
    if !matches!(*status, CheckpointStatus::Idle) {
      return Ok(());
    }
    *status = CheckpointStatus::Running;
  }
  drop(db_guard);

  std::thread::spawn(move || {
    let mut db = db.lock();
    let result = checkpoint_impl(&mut db, false);
    if result.is_err() {
      *db.checkpoint_status.lock() = CheckpointStatus::Idle;
    }
  });

  Ok(())
}

#[cfg(target_arch = "wasm32")]
pub fn trigger_background_checkpoint_async(db: Arc<parking_lot::Mutex<GraphDB>>) -> Result<()> {
  let mut db = db.lock();
  checkpoint_impl(&mut db, false).map(|_| ())
}

/// Force a full compaction
/// This is similar to checkpoint but may also:
/// - Reclaim free space
/// - Rebuild indexes
/// - Optimize storage layout
pub fn compact(db: &mut GraphDB) -> Result<CheckpointStats> {
  // For now, compact is the same as checkpoint
  // In the future, this could do additional optimizations like:
  // - Defragmenting the snapshot file
  // - Rebuilding indexes
  // - Reclaiming deleted node/edge space
  checkpoint(db)
}

// ============================================================================
// Multi-file Format Checkpointing
// ============================================================================

/// Create a new snapshot for multi-file format
/// This writes a new snapshot file and updates the manifest
pub fn create_snapshot(db: &mut GraphDB) -> Result<u64> {
  // Use checkpoint which handles snapshot creation
  let stats = checkpoint(db)?;
  Ok(stats.snapshot_gen)
}

/// Delete old snapshots (keeping only the N most recent)
pub fn prune_snapshots(db: &GraphDB, keep_count: usize) -> Result<usize> {
  let manifest = match db.manifest.as_ref() {
    Some(m) => m,
    None => return Ok(0),
  };

  let snapshots_dir = db.path.join(SNAPSHOTS_DIR);
  if !snapshots_dir.exists() {
    return Ok(0);
  }

  let entries = match fs::read_dir(&snapshots_dir) {
    Ok(e) => e,
    Err(_) => return Ok(0),
  };

  let mut snapshots: Vec<(u64, std::path::PathBuf, std::ffi::OsString)> = Vec::new();
  for entry in entries.flatten() {
    let filename = entry.file_name();
    let filename_str = filename.to_string_lossy();
    if let Some(gen) = parse_snapshot_gen(&filename_str) {
      snapshots.push((gen, entry.path(), filename));
    }
  }

  if snapshots.is_empty() {
    return Ok(0);
  }

  snapshots.sort_by(|a, b| b.0.cmp(&a.0));

  let mut keep: HashSet<u64> = snapshots
    .iter()
    .take(keep_count)
    .map(|(gen, _, _)| *gen)
    .collect();

  if manifest.active_snapshot_gen != 0 {
    keep.insert(manifest.active_snapshot_gen);
  }
  if manifest.prev_snapshot_gen != 0 {
    keep.insert(manifest.prev_snapshot_gen);
  }

  let mut deleted = 0usize;

  for (gen, path, filename) in snapshots {
    if keep.contains(&gen) {
      continue;
    }

    if fs::remove_file(&path).is_ok() {
      deleted += 1;
      continue;
    }

    let trash_dir = db.path.join(TRASH_DIR);
    let _ = fs::create_dir_all(&trash_dir);
    if fs::rename(&path, trash_dir.join(&filename)).is_ok() {
      deleted += 1;
    }
  }

  Ok(deleted)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;
  use crate::graph::db::{close_graph_db, open_graph_db, OpenOptions};
  use tempfile::tempdir;

  #[test]
  fn test_should_checkpoint_empty() {
    let temp_dir = tempdir().unwrap();
    let db = open_graph_db(temp_dir.path(), OpenOptions::new()).unwrap();

    // Empty database shouldn't need checkpoint
    assert!(!should_checkpoint(&db));

    close_graph_db(db).unwrap();
  }

  #[test]
  fn test_is_checkpoint_running() {
    let temp_dir = tempdir().unwrap();
    let db = open_graph_db(temp_dir.path(), OpenOptions::new()).unwrap();

    assert!(!is_checkpoint_running(&db));

    close_graph_db(db).unwrap();
  }
}
