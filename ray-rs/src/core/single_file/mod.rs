//! Single-file database format (.raydb)
//!
//! Provides open/close/read/write operations for single-file databases.
//! Layout: [Header (1 page)] [WAL (N pages)] [Snapshot (M pages)]
//!
//! Ported from src/ray/graph-db/single-file.ts

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use parking_lot::{Mutex, RwLock};

use crate::cache::manager::CacheManager;
use crate::constants::*;
use crate::core::pager::FilePager;
use crate::core::snapshot::reader::SnapshotData;
use crate::core::wal::buffer::WalBuffer;
use crate::types::*;
use crate::vector::types::VectorManifest;

// Submodules
mod check;
mod checkpoint;
mod compactor;
mod iter;
mod open;
mod read;
mod recovery;
mod schema;
mod transaction;
mod vector;
mod write;

#[cfg(test)]
mod stress;

// Re-export everything for backward compatibility
pub use compactor::{SingleFileOptimizeOptions, VacuumOptions};
pub use iter::*;
pub use open::{close_single_file, open_single_file, SingleFileOpenOptions, SyncMode};

// Also re-export recovery items that are used externally
pub use recovery::replay_wal_record;

// ============================================================================
// Transaction State (for single-file DB)
// ============================================================================

/// Transaction state for SingleFileDB
///
/// This is simpler than the main TxState in types.rs, as SingleFileDB
/// handles operations differently.
#[derive(Debug)]
pub struct SingleFileTxState {
  pub txid: TxId,
  pub read_only: bool,
  pub snapshot_ts: u64,
  pub delta_snapshot: Option<DeltaState>,
}

impl SingleFileTxState {
  pub fn new(
    txid: TxId,
    read_only: bool,
    snapshot_ts: u64,
    delta_snapshot: Option<DeltaState>,
  ) -> Self {
    Self {
      txid,
      read_only,
      snapshot_ts,
      delta_snapshot,
    }
  }
}

// ============================================================================
// Single-File GraphDB
// ============================================================================

/// Single-file database handle
pub struct SingleFileDB {
  /// Database file path
  pub path: PathBuf,
  /// Read-only mode
  pub read_only: bool,
  /// Page-based I/O
  pub pager: Mutex<FilePager>,
  /// Database header
  pub header: RwLock<DbHeaderV1>,
  /// WAL circular buffer manager
  pub wal_buffer: Mutex<WalBuffer>,
  /// Memory-mapped snapshot data (if exists)
  pub snapshot: RwLock<Option<SnapshotData>>,
  /// Delta state (uncommitted changes)
  pub delta: RwLock<DeltaState>,

  // ID allocators
  pub(crate) next_node_id: AtomicU64,
  pub(crate) next_label_id: AtomicU32,
  pub(crate) next_etype_id: AtomicU32,
  pub(crate) next_propkey_id: AtomicU32,
  pub(crate) next_tx_id: AtomicU64,

  /// Current active transaction
  pub current_tx: Mutex<Option<SingleFileTxState>>,

  /// Label name -> ID mapping
  pub(crate) label_names: RwLock<HashMap<String, LabelId>>,
  /// ID -> label name mapping
  pub(crate) label_ids: RwLock<HashMap<LabelId, String>>,
  /// Edge type name -> ID mapping
  pub(crate) etype_names: RwLock<HashMap<String, ETypeId>>,
  /// ID -> edge type name mapping
  pub(crate) etype_ids: RwLock<HashMap<ETypeId, String>>,
  /// Property key name -> ID mapping
  pub(crate) propkey_names: RwLock<HashMap<String, PropKeyId>>,
  /// ID -> property key name mapping
  pub(crate) propkey_ids: RwLock<HashMap<PropKeyId, String>>,

  /// Enable auto-checkpoint when WAL usage exceeds threshold
  pub(crate) auto_checkpoint: bool,
  /// WAL usage threshold (0.0-1.0) to trigger auto-checkpoint
  pub(crate) checkpoint_threshold: f64,
  /// Use background (non-blocking) checkpoint instead of blocking
  pub(crate) background_checkpoint: bool,
  /// Current checkpoint state
  pub(crate) checkpoint_status: Mutex<CheckpointStatus>,

  /// Vector stores keyed by property key ID
  /// Each property key can have its own vector store with different dimensions
  pub(crate) vector_stores: RwLock<HashMap<PropKeyId, VectorManifest>>,

  /// Cache manager for property, traversal, query, and key caches
  pub cache: RwLock<Option<CacheManager>>,

  /// Synchronization mode for WAL writes
  pub(crate) sync_mode: open::SyncMode,
}

/// Checkpoint state for background checkpointing
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointStatus {
  /// No checkpoint in progress
  Idle,
  /// Background checkpoint is running (writes go to secondary WAL)
  Running,
  /// Completing checkpoint (brief lock for final updates)
  Completing,
}

// ============================================================================
// SingleFileDB Implementation - ID Allocators
// ============================================================================

impl SingleFileDB {
  /// Allocate a new node ID
  pub fn alloc_node_id(&self) -> NodeId {
    self.next_node_id.fetch_add(1, Ordering::SeqCst)
  }

  /// Allocate a new label ID
  pub fn alloc_label_id(&self) -> LabelId {
    self.next_label_id.fetch_add(1, Ordering::SeqCst)
  }

  /// Allocate a new edge type ID
  pub fn alloc_etype_id(&self) -> ETypeId {
    self.next_etype_id.fetch_add(1, Ordering::SeqCst)
  }

  /// Allocate a new property key ID
  pub fn alloc_propkey_id(&self) -> PropKeyId {
    self.next_propkey_id.fetch_add(1, Ordering::SeqCst)
  }

  /// Allocate a new transaction ID
  pub fn alloc_tx_id(&self) -> TxId {
    self.next_tx_id.fetch_add(1, Ordering::SeqCst)
  }

  /// Check if a node exists
  pub fn node_exists(&self, node_id: NodeId) -> bool {
    let delta = self.delta.read();

    if delta.is_node_deleted(node_id) {
      return false;
    }

    if delta.is_node_created(node_id) {
      return true;
    }

    // Check snapshot
    if let Some(ref snapshot) = *self.snapshot.read() {
      return snapshot.has_node(node_id);
    }

    false
  }

  /// Check if an edge exists
  pub fn edge_exists(&self, src: NodeId, etype: ETypeId, dst: NodeId) -> bool {
    let delta = self.delta.read();

    if delta.is_edge_deleted(src, etype, dst) {
      return false;
    }

    if delta.is_edge_added(src, etype, dst) {
      return true;
    }

    // Check snapshot
    if let Some(ref snapshot) = *self.snapshot.read() {
      if let (Some(src_phys), Some(dst_phys)) =
        (snapshot.get_phys_node(src), snapshot.get_phys_node(dst))
      {
        return snapshot.has_edge(src_phys, etype, dst_phys);
      }
    }

    false
  }

  // ==========================================================================
  // Cache API
  // ==========================================================================

  /// Check if caching is enabled
  pub fn cache_is_enabled(&self) -> bool {
    self
      .cache
      .read()
      .as_ref()
      .map(|c| c.is_enabled())
      .unwrap_or(false)
  }

  /// Invalidate all caches for a node
  pub fn cache_invalidate_node(&self, node_id: NodeId) {
    if let Some(ref mut cache) = *self.cache.write() {
      cache.invalidate_node(node_id);
    }
  }

  /// Invalidate caches for a specific edge
  pub fn cache_invalidate_edge(&self, src: NodeId, etype: ETypeId, dst: NodeId) {
    if let Some(ref mut cache) = *self.cache.write() {
      cache.invalidate_edge(src, etype, dst);
    }
  }

  /// Invalidate a cached key lookup
  pub fn cache_invalidate_key(&self, key: &str) {
    if let Some(ref mut cache) = *self.cache.write() {
      cache.invalidate_key(key);
    }
  }

  /// Clear all caches
  pub fn cache_clear(&self) {
    if let Some(ref mut cache) = *self.cache.write() {
      cache.clear();
    }
  }

  /// Clear only the query cache
  pub fn cache_clear_query(&self) {
    if let Some(ref mut cache) = *self.cache.write() {
      cache.clear_query_cache();
    }
  }

  /// Clear only the key cache
  pub fn cache_clear_key(&self) {
    if let Some(ref mut cache) = *self.cache.write() {
      cache.clear_key_cache();
    }
  }

  /// Clear only the property cache
  pub fn cache_clear_property(&self) {
    if let Some(ref mut cache) = *self.cache.write() {
      cache.clear_property_cache();
    }
  }

  /// Clear only the traversal cache
  pub fn cache_clear_traversal(&self) {
    if let Some(ref mut cache) = *self.cache.write() {
      cache.clear_traversal_cache();
    }
  }

  /// Get cache statistics
  pub fn cache_stats(&self) -> Option<CacheStats> {
    self.cache.read().as_ref().map(|c| c.get_stats())
  }

  /// Reset cache statistics
  pub fn cache_reset_stats(&self) {
    if let Some(ref mut cache) = *self.cache.write() {
      cache.reset_stats();
    }
  }
}

// ============================================================================
// Utility Functions
// ============================================================================

/// Check if a path is a single-file database
pub fn is_single_file_path<P: AsRef<Path>>(path: P) -> bool {
  path
    .as_ref()
    .extension()
    .map(|ext| ext == "raydb")
    .unwrap_or(false)
}

/// Get the single-file extension
pub fn single_file_extension() -> &'static str {
  EXT_RAYDB
}
