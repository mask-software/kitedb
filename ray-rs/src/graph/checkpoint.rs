//! Background checkpointing
//!
//! Checkpoint (compaction) merges the delta state into a new snapshot,
//! allowing the WAL to be truncated. This is important for:
//! - Reducing WAL size
//! - Improving read performance (fewer delta lookups)
//! - Controlling memory usage

use crate::error::Result;

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
pub fn is_checkpoint_running(_db: &GraphDB) -> bool {
  // TODO: Track checkpoint state in GraphDB
  false
}

/// Trigger a blocking checkpoint
/// This will:
/// 1. Build a new snapshot from current state
/// 2. Write the snapshot to disk
/// 3. Clear the delta state
/// 4. Reset the WAL
pub fn checkpoint(db: &mut GraphDB) -> Result<CheckpointStats> {
  use std::time::Instant;
  
  let start = Instant::now();
  
  // Use the GraphDB::optimize() method which handles all the checkpoint logic
  db.optimize()?;
  
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

  Ok(CheckpointStats {
    num_nodes,
    num_edges,
    snapshot_gen,
    duration_ms,
  })
}

/// Trigger a background (non-blocking) checkpoint
/// For single-file format, this switches writes to the secondary WAL region
/// while the checkpoint runs
pub fn trigger_background_checkpoint(_db: &mut GraphDB) -> Result<()> {
  // TODO: Implement background checkpointing
  // This requires dual-buffer WAL support
  Ok(())
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
pub fn prune_snapshots(_db: &GraphDB, _keep_count: usize) -> Result<usize> {
  // TODO: Implement snapshot pruning
  // Returns number of snapshots deleted
  Ok(0)
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
