//! Database open/close operations for SingleFileDB
//!
//! Handles opening, creating, and closing single-file databases.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use parking_lot::{Mutex, RwLock};

use crate::cache::manager::CacheManager;
use crate::constants::*;
use crate::core::pager::{create_pager, is_valid_page_size, open_pager, pages_to_store};
use crate::core::snapshot::reader::SnapshotData;
use crate::core::wal::buffer::WalBuffer;
use crate::error::{RayError, Result};
use crate::types::*;
use crate::util::mmap::map_file;
use crate::vector::store::{create_vector_store, vector_store_delete, vector_store_insert};
use crate::vector::types::VectorStoreConfig;

use super::recovery::{get_committed_transactions, replay_wal_record, scan_wal_records};
use super::vector::vector_stores_from_snapshot;
use super::{CheckpointStatus, SingleFileDB};

// ============================================================================
// Open Options
// ============================================================================

/// Synchronization mode for WAL writes
///
/// Controls the durability vs performance trade-off for commits.
/// Similar to SQLite's PRAGMA synchronous setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SyncMode {
  /// Fsync on every commit (durable to OS, slowest)
  /// On macOS this uses fsync for parity with Node/Bun.
  #[default]
  Full,

  /// Fsync only on checkpoint (balanced)
  /// WAL writes are buffered in OS cache. Data may be lost if OS crashes,
  /// but not if application crashes. ~1000x faster than Full.
  Normal,

  /// No fsync (fastest, least safe)
  /// Data may be lost on any crash. Only for testing/ephemeral data.
  Off,
}

/// Options for opening a single-file database
#[derive(Debug, Clone)]
pub struct SingleFileOpenOptions {
  /// Open in read-only mode
  pub read_only: bool,
  /// Create database if it doesn't exist
  pub create_if_missing: bool,
  /// Page size (default 4KB, must be power of 2 between 4KB and 64KB)
  pub page_size: usize,
  /// WAL size in bytes (default 1MB)
  pub wal_size: usize,
  /// Enable auto-checkpoint when WAL usage exceeds threshold
  pub auto_checkpoint: bool,
  /// WAL usage threshold (0.0-1.0) to trigger auto-checkpoint (default 0.8)
  pub checkpoint_threshold: f64,
  /// Use background (non-blocking) checkpoint instead of blocking (default true)
  pub background_checkpoint: bool,
  /// Cache options (None = disabled)
  pub cache: Option<CacheOptions>,
  /// Synchronization mode for WAL writes (default: Full)
  pub sync_mode: SyncMode,
}

impl Default for SingleFileOpenOptions {
  fn default() -> Self {
    Self {
      read_only: false,
      create_if_missing: true,
      page_size: DEFAULT_PAGE_SIZE,
      wal_size: WAL_DEFAULT_SIZE,
      auto_checkpoint: false,
      checkpoint_threshold: 0.8,
      background_checkpoint: true,
      cache: None,
      sync_mode: SyncMode::Full,
    }
  }
}

impl SingleFileOpenOptions {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn read_only(mut self, value: bool) -> Self {
    self.read_only = value;
    self
  }

  pub fn create_if_missing(mut self, value: bool) -> Self {
    self.create_if_missing = value;
    self
  }

  pub fn page_size(mut self, value: usize) -> Self {
    self.page_size = value;
    self
  }

  pub fn wal_size(mut self, value: usize) -> Self {
    self.wal_size = value;
    self
  }

  pub fn auto_checkpoint(mut self, value: bool) -> Self {
    self.auto_checkpoint = value;
    self
  }

  pub fn checkpoint_threshold(mut self, value: f64) -> Self {
    self.checkpoint_threshold = value.clamp(0.0, 1.0);
    self
  }

  pub fn background_checkpoint(mut self, value: bool) -> Self {
    self.background_checkpoint = value;
    self
  }

  pub fn cache(mut self, options: Option<CacheOptions>) -> Self {
    self.cache = options;
    self
  }

  pub fn enable_cache(mut self) -> Self {
    self.cache = Some(CacheOptions {
      enabled: true,
      ..Default::default()
    });
    self
  }

  pub fn sync_mode(mut self, mode: SyncMode) -> Self {
    self.sync_mode = mode;
    self
  }

  /// Set sync mode to Normal (fsync on checkpoint only)
  /// This is ~1000x faster than Full mode but data may be lost if OS crashes.
  pub fn sync_normal(mut self) -> Self {
    self.sync_mode = SyncMode::Normal;
    self
  }

  /// Set sync mode to Off (no fsync)
  /// Only for testing or ephemeral data. Data may be lost on any crash.
  pub fn sync_off(mut self) -> Self {
    self.sync_mode = SyncMode::Off;
    self
  }
}

// ============================================================================
// Open / Close
// ============================================================================

/// Open a single-file database
pub fn open_single_file<P: AsRef<Path>>(
  path: P,
  options: SingleFileOpenOptions,
) -> Result<SingleFileDB> {
  let path = path.as_ref();

  // Validate page size
  if !is_valid_page_size(options.page_size) {
    return Err(RayError::Internal(format!(
      "Invalid page size: {}. Must be power of 2 between 4KB and 64KB",
      options.page_size
    )));
  }

  // Check if file exists
  let file_exists = path.exists();

  if !file_exists && !options.create_if_missing {
    return Err(RayError::InvalidPath(format!(
      "Database does not exist at {}",
      path.display()
    )));
  }

  if !file_exists && options.read_only {
    return Err(RayError::ReadOnly);
  }

  // Open or create pager
  let (mut pager, mut header, is_new) = if file_exists {
    // Open existing database
    let mut pager = open_pager(path, options.page_size)?;

    // Read and validate header
    let header_data = pager.read_page(0)?;
    let header = DbHeaderV1::parse(&header_data)?;

    (pager, header, false)
  } else {
    // Create new database
    let mut pager = create_pager(path, options.page_size)?;

    // Calculate WAL page count
    let wal_page_count = pages_to_store(options.wal_size, options.page_size) as u64;

    // Create initial header
    let header = DbHeaderV1::new(options.page_size as u32, wal_page_count);

    // Write header
    let header_bytes = header.serialize_to_page();
    pager.write_page(0, &header_bytes)?;

    // Allocate WAL pages
    pager.allocate_pages(wal_page_count as u32)?;

    // Sync to disk
    pager.sync()?;

    (pager, header, true)
  };

  // Initialize WAL buffer
  let mut wal_buffer = WalBuffer::from_header(&header);

  // Recover from incomplete background checkpoint if needed
  if header.checkpoint_in_progress != 0 {
    wal_buffer.recover_incomplete_checkpoint(&mut pager)?;
    wal_buffer.flush(&mut pager)?;

    header.active_wal_region = 0;
    header.checkpoint_in_progress = 0;
    header.wal_head = wal_buffer.head();
    header.wal_tail = wal_buffer.tail();
    header.wal_primary_head = wal_buffer.primary_head();
    header.wal_secondary_head = wal_buffer.secondary_head();
    header.change_counter += 1;

    let header_bytes = header.serialize_to_page();
    pager.write_page(0, &header_bytes)?;
    pager.sync()?;
  }

  // Initialize ID allocators from header
  let mut next_node_id = INITIAL_NODE_ID;
  let mut next_label_id = INITIAL_LABEL_ID;
  let mut next_etype_id = INITIAL_ETYPE_ID;
  let mut next_propkey_id = INITIAL_PROPKEY_ID;
  let next_tx_id = header.next_tx_id;

  if header.max_node_id > 0 {
    next_node_id = header.max_node_id + 1;
  }

  // Initialize delta
  let mut delta = DeltaState::new();

  // Schema maps
  let mut label_names: HashMap<String, LabelId> = HashMap::new();
  let mut label_ids: HashMap<LabelId, String> = HashMap::new();
  let mut etype_names: HashMap<String, ETypeId> = HashMap::new();
  let mut etype_ids: HashMap<ETypeId, String> = HashMap::new();
  let mut propkey_names: HashMap<String, PropKeyId> = HashMap::new();
  let mut propkey_ids: HashMap<PropKeyId, String> = HashMap::new();

  // Load snapshot if exists
  let snapshot = if header.snapshot_page_count > 0 {
    // Calculate snapshot offset in bytes
    let snapshot_offset = (header.snapshot_start_page * header.page_size as u64) as usize;

    match SnapshotData::parse_at_offset(
      std::sync::Arc::new({
        // Safety handled inside map_file (native mmap) or in-memory read (wasm).
        map_file(pager.file())?
      }),
      snapshot_offset,
      &crate::core::snapshot::reader::ParseSnapshotOptions::default(),
    ) {
      Ok(snap) => {
        // Load schema from snapshot
        for i in 1..=snap.header.num_labels as u32 {
          if let Some(name) = snap.get_label_name(i) {
            label_names.insert(name.to_string(), i);
            label_ids.insert(i, name.to_string());
          }
        }
        for i in 1..=snap.header.num_etypes as u32 {
          if let Some(name) = snap.get_etype_name(i) {
            etype_names.insert(name.to_string(), i);
            etype_ids.insert(i, name.to_string());
          }
        }
        for i in 1..=snap.header.num_propkeys as u32 {
          if let Some(name) = snap.get_propkey_name(i) {
            propkey_names.insert(name.to_string(), i);
            propkey_ids.insert(i, name.to_string());
          }
        }

        // Update ID allocators from snapshot
        next_node_id = snap.header.max_node_id + 1;
        next_label_id = snap.header.num_labels as u32 + 1;
        next_etype_id = snap.header.num_etypes as u32 + 1;
        next_propkey_id = snap.header.num_propkeys as u32 + 1;

        Some(snap)
      }
      Err(e) => {
        eprintln!("Warning: Failed to parse snapshot: {e}");
        None
      }
    }
  } else {
    None
  };

  // Replay WAL for recovery (if not a new database)
  if !is_new && header.wal_head > 0 {
    // Read WAL records from the circular buffer
    let wal_records = scan_wal_records(&mut pager, &header)?;
    let committed = get_committed_transactions(&wal_records);

    // Replay committed transactions
    for (_txid, records) in committed {
      for record in records {
        replay_wal_record(
          record,
          snapshot.as_ref(),
          &mut delta,
          &mut next_node_id,
          &mut next_label_id,
          &mut next_etype_id,
          &mut next_propkey_id,
          &mut label_names,
          &mut label_ids,
          &mut etype_names,
          &mut etype_ids,
          &mut propkey_names,
          &mut propkey_ids,
        );
      }
    }
  }

  // Load vector stores from snapshot (if present)
  let mut vector_stores = if let Some(ref snapshot) = snapshot {
    vector_stores_from_snapshot(snapshot)?
  } else {
    HashMap::new()
  };

  // Apply pending vector operations from WAL replay
  for ((node_id, prop_key_id), operation) in delta.pending_vectors.drain() {
    match operation {
      Some(vector) => {
        // Get or create vector store
        let store = vector_stores.entry(prop_key_id).or_insert_with(|| {
          let config = VectorStoreConfig::new(vector.len());
          create_vector_store(config)
        });
        let _ = vector_store_insert(store, node_id, &vector);
      }
      None => {
        // Delete operation
        if let Some(store) = vector_stores.get_mut(&prop_key_id) {
          vector_store_delete(store, node_id);
        }
      }
    }
  }

  // Initialize cache if enabled
  let cache = options.cache.map(CacheManager::new);

  Ok(SingleFileDB {
    path: path.to_path_buf(),
    read_only: options.read_only,
    pager: Mutex::new(pager),
    header: RwLock::new(header),
    wal_buffer: Mutex::new(wal_buffer),
    snapshot: RwLock::new(snapshot),
    delta: RwLock::new(delta),
    next_node_id: AtomicU64::new(next_node_id),
    next_label_id: AtomicU32::new(next_label_id),
    next_etype_id: AtomicU32::new(next_etype_id),
    next_propkey_id: AtomicU32::new(next_propkey_id),
    next_tx_id: AtomicU64::new(next_tx_id),
    current_tx: Mutex::new(None),
    label_names: RwLock::new(label_names),
    label_ids: RwLock::new(label_ids),
    etype_names: RwLock::new(etype_names),
    etype_ids: RwLock::new(etype_ids),
    propkey_names: RwLock::new(propkey_names),
    propkey_ids: RwLock::new(propkey_ids),
    auto_checkpoint: options.auto_checkpoint,
    checkpoint_threshold: options.checkpoint_threshold,
    background_checkpoint: options.background_checkpoint,
    checkpoint_status: Mutex::new(CheckpointStatus::Idle),
    vector_stores: RwLock::new(vector_stores),
    cache: RwLock::new(cache),
    sync_mode: options.sync_mode,
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::core::single_file::close_single_file;
  use tempfile::tempdir;

  #[test]
  fn test_recover_incomplete_background_checkpoint() {
    let temp_dir = tempdir().unwrap();
    let db_path = temp_dir.path().join("checkpoint-recover.raydb");

    let db = open_single_file(&db_path, SingleFileOpenOptions::new()).unwrap();

    // Write a primary WAL record
    db.begin(false).unwrap();
    let _n1 = db.create_node(Some("n1")).unwrap();
    db.commit().unwrap();

    // Simulate beginning a background checkpoint (switch to secondary + header flag)
    {
      let mut pager = db.pager.lock();
      let mut wal = db.wal_buffer.lock();
      let mut header = db.header.write();

      wal.switch_to_secondary();
      header.active_wal_region = 1;
      header.checkpoint_in_progress = 1;
      header.wal_primary_head = wal.primary_head();
      header.wal_secondary_head = wal.secondary_head();
      header.wal_head = wal.head();
      header.wal_tail = wal.tail();
      header.change_counter += 1;

      let header_bytes = header.serialize_to_page();
      pager.write_page(0, &header_bytes).unwrap();
      pager.sync().unwrap();
    }

    // Write to secondary WAL region
    db.begin(false).unwrap();
    let _n2 = db.create_node(Some("n2")).unwrap();
    db.commit().unwrap();

    close_single_file(db).unwrap();

    // Reopen and ensure both records are recovered
    let db = open_single_file(&db_path, SingleFileOpenOptions::new()).unwrap();
    assert!(db.get_node_by_key("n1").is_some());
    assert!(db.get_node_by_key("n2").is_some());
    close_single_file(db).unwrap();
  }

  #[test]
  fn test_recover_checkpoint_with_partial_header_update() {
    let temp_dir = tempdir().unwrap();
    let db_path = temp_dir
      .path()
      .join("checkpoint-recover-partial-header.raydb");

    let db = open_single_file(&db_path, SingleFileOpenOptions::new()).unwrap();

    // Write a primary WAL record
    db.begin(false).unwrap();
    let _n1 = db.create_node(Some("n1")).unwrap();
    db.commit().unwrap();

    // Simulate beginning a background checkpoint (switch to secondary + header flag)
    {
      let mut pager = db.pager.lock();
      let mut wal = db.wal_buffer.lock();
      let mut header = db.header.write();

      wal.switch_to_secondary();
      header.active_wal_region = 1;
      header.checkpoint_in_progress = 1;
      header.wal_primary_head = wal.primary_head();
      header.wal_secondary_head = wal.secondary_head();
      header.wal_head = wal.head();
      header.wal_tail = wal.tail();
      header.change_counter += 1;

      let header_bytes = header.serialize_to_page();
      pager.write_page(0, &header_bytes).unwrap();
      pager.sync().unwrap();
    }

    // Write to secondary WAL region
    db.begin(false).unwrap();
    let _n2 = db.create_node(Some("n2")).unwrap();
    db.commit().unwrap();

    // Simulate an interrupted header update: wal_head advanced, secondary head missing
    {
      let mut pager = db.pager.lock();
      let wal = db.wal_buffer.lock();
      let mut header = db.header.write();

      header.active_wal_region = 1;
      header.checkpoint_in_progress = 1;
      header.wal_primary_head = wal.primary_head();
      header.wal_head = wal.head();
      header.wal_tail = wal.tail();
      header.wal_secondary_head = wal.primary_region_size();
      header.change_counter += 1;

      let header_bytes = header.serialize_to_page();
      pager.write_page(0, &header_bytes).unwrap();
      pager.sync().unwrap();
    }

    // Simulate crash by dropping without close
    drop(db);

    // Reopen and ensure both records are recovered
    let db = open_single_file(&db_path, SingleFileOpenOptions::new()).unwrap();
    assert!(db.get_node_by_key("n1").is_some());
    assert!(db.get_node_by_key("n2").is_some());
    close_single_file(db).unwrap();
  }

  #[test]
  fn test_recover_checkpoint_with_missing_primary_head() {
    let temp_dir = tempdir().unwrap();
    let db_path = temp_dir
      .path()
      .join("checkpoint-recover-missing-primary-head.raydb");

    let db = open_single_file(&db_path, SingleFileOpenOptions::new()).unwrap();

    // Write a primary WAL record
    db.begin(false).unwrap();
    let _n1 = db.create_node(Some("n1")).unwrap();
    db.commit().unwrap();

    // Simulate a crash where checkpoint flag is set but wal_primary_head is missing
    {
      let mut pager = db.pager.lock();
      let wal = db.wal_buffer.lock();
      let mut header = db.header.write();

      header.active_wal_region = 1;
      header.checkpoint_in_progress = 1;
      header.wal_primary_head = 0;
      header.wal_secondary_head = wal.secondary_head();
      header.wal_head = wal.head();
      header.wal_tail = wal.tail();
      header.change_counter += 1;

      let header_bytes = header.serialize_to_page();
      pager.write_page(0, &header_bytes).unwrap();
      pager.sync().unwrap();
    }

    drop(db);

    let db = open_single_file(&db_path, SingleFileOpenOptions::new()).unwrap();
    assert!(db.get_node_by_key("n1").is_some());
    close_single_file(db).unwrap();
  }
}

/// Close a single-file database
pub fn close_single_file(db: SingleFileDB) -> Result<()> {
  // Flush WAL and sync to disk
  let mut pager = db.pager.lock();
  let mut wal_buffer = db.wal_buffer.lock();

  // Flush any pending WAL writes
  wal_buffer.flush(&mut pager)?;

  // Update header with current WAL state
  {
    let mut header = db.header.write();
    header.wal_head = wal_buffer.head();
    header.wal_tail = wal_buffer.tail();
    header.max_node_id = db.next_node_id.load(Ordering::SeqCst).saturating_sub(1);
    header.next_tx_id = db.next_tx_id.load(Ordering::SeqCst);

    // Write header
    let header_bytes = header.serialize_to_page();
    pager.write_page(0, &header_bytes)?;
  }

  // Final sync
  pager.sync()?;
  Ok(())
}
