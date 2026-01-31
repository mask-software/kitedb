//! Main GraphDB struct and lifecycle
//!
//! The GraphDB is the main entry point for all graph operations. It manages:
//! - Snapshot (immutable base data)
//! - Delta (uncommitted changes)  
//! - WAL (write-ahead log for durability)
//! - Transactions
//! - ID allocation

use std::collections::HashMap;
use std::fs::OpenOptions as FsOpenOptions;
use std::fs::{self, File};
use std::io::{Seek, SeekFrom, Write};
#[cfg(target_os = "macos")]
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use parking_lot::{Mutex, RwLock};

use crate::constants::*;
use crate::core::manifest::{create_empty_manifest, read_manifest, write_manifest};
use crate::core::snapshot::reader::SnapshotData;
use crate::core::wal::reader::{load_wal_segment_by_id, recover_from_segment};
use crate::core::wal::record::WalRecord;
use crate::core::wal::writer::WalWriter;
use crate::error::{RayError, Result};
use crate::graph::checkpoint::CheckpointStatus;
use crate::mvcc::{GcConfig, MvccManager};
use crate::types::*;
use crate::util::lock::{FileLock, LockType};
use crate::util::mmap::map_file;
use crate::vector::store::{create_vector_store, vector_store_delete, vector_store_insert};
use crate::vector::types::{VectorManifest, VectorStoreConfig};

// ============================================================================
// Open Options
// ============================================================================

/// Options for opening a database
#[derive(Debug, Clone, Default)]
pub struct OpenOptions {
  /// Open in read-only mode
  pub read_only: bool,
  /// Create database if it doesn't exist
  pub create_if_missing: bool,
  /// Acquire file lock
  pub lock_file: bool,
  /// Enable MVCC
  pub mvcc: bool,
  /// MVCC GC interval in ms
  pub mvcc_gc_interval_ms: Option<u64>,
  /// MVCC retention in ms
  pub mvcc_retention_ms: Option<u64>,
  /// MVCC max version chain depth
  pub mvcc_max_chain_depth: Option<usize>,
  /// Cache options
  pub cache: Option<CacheOptions>,
}

impl OpenOptions {
  pub fn new() -> Self {
    Self {
      read_only: false,
      create_if_missing: true,
      lock_file: true,
      mvcc: false,
      mvcc_gc_interval_ms: None,
      mvcc_retention_ms: None,
      mvcc_max_chain_depth: None,
      cache: None,
    }
  }

  pub fn read_only(mut self, value: bool) -> Self {
    self.read_only = value;
    self
  }

  pub fn create_if_missing(mut self, value: bool) -> Self {
    self.create_if_missing = value;
    self
  }

  pub fn lock_file(mut self, value: bool) -> Self {
    self.lock_file = value;
    self
  }

  pub fn mvcc(mut self, value: bool) -> Self {
    self.mvcc = value;
    self
  }

  pub fn mvcc_gc_interval_ms(mut self, value: u64) -> Self {
    self.mvcc_gc_interval_ms = Some(value);
    self
  }

  pub fn mvcc_retention_ms(mut self, value: u64) -> Self {
    self.mvcc_retention_ms = Some(value);
    self
  }

  pub fn mvcc_max_chain_depth(mut self, value: usize) -> Self {
    self.mvcc_max_chain_depth = Some(value);
    self
  }
}

// ============================================================================
// GraphDB
// ============================================================================

/// The main graph database handle
pub struct GraphDB {
  /// Database path
  pub path: PathBuf,
  /// Read-only mode
  pub read_only: bool,
  /// Is single-file format
  pub is_single_file: bool,

  // ---- Multi-file format fields ----
  /// Manifest (multi-file only)
  pub manifest: Option<ManifestV1>,
  /// Loaded snapshot data
  pub snapshot: Option<SnapshotData>,
  /// WAL file handle
  wal_fd: Option<File>,
  /// Current WAL write offset
  wal_offset: AtomicU64,

  // ---- Single-file format fields ----
  // pub header: Option<DbHeaderV1>,
  // pub pager: Option<FilePager>,

  // ---- Shared fields ----
  /// Delta state (uncommitted changes)
  pub delta: RwLock<DeltaState>,

  /// Vector stores keyed by property key ID
  pub vector_stores: RwLock<HashMap<PropKeyId, VectorManifest>>,

  /// Next node ID to allocate
  next_node_id: AtomicU64,
  /// Next label ID to allocate
  next_label_id: AtomicU32,
  /// Next edge type ID to allocate  
  next_etype_id: AtomicU32,
  /// Next property key ID to allocate
  next_propkey_id: AtomicU32,
  /// Next transaction ID to allocate
  next_tx_id: AtomicU64,

  /// Current active transaction (only one at a time for now)
  pub current_tx: Mutex<Option<TxState>>,

  /// Current checkpoint state
  pub checkpoint_status: Mutex<CheckpointStatus>,

  /// MVCC manager (if enabled)
  pub mvcc: Option<std::sync::Arc<MvccManager>>,

  /// File lock handle
  lock_handle: Option<FileLock>,

  /// Label name -> ID mapping
  label_names: RwLock<HashMap<String, LabelId>>,
  /// ID -> label name mapping
  label_ids: RwLock<HashMap<LabelId, String>>,
  /// Edge type name -> ID mapping
  etype_names: RwLock<HashMap<String, ETypeId>>,
  /// ID -> edge type name mapping
  etype_ids: RwLock<HashMap<ETypeId, String>>,
  /// Property key name -> ID mapping
  propkey_names: RwLock<HashMap<String, PropKeyId>>,
  /// ID -> property key name mapping
  propkey_ids: RwLock<HashMap<PropKeyId, String>>,
}

impl GraphDB {
  // ========================================================================
  // ID Allocation
  // ========================================================================

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

  /// Get current next node ID (without incrementing)
  pub fn peek_next_node_id(&self) -> NodeId {
    self.next_node_id.load(Ordering::SeqCst)
  }

  /// Get current WAL size in bytes
  pub fn wal_bytes(&self) -> u64 {
    self.wal_offset.load(Ordering::SeqCst)
  }

  // ========================================================================
  // Schema Lookups
  // ========================================================================

  /// Get or create a label ID by name
  pub fn get_or_create_label(&self, name: &str) -> LabelId {
    // Check if exists
    {
      let names = self.label_names.read();
      if let Some(&id) = names.get(name) {
        return id;
      }
    }

    // Create new
    let id = self.alloc_label_id();
    {
      let mut names = self.label_names.write();
      let mut ids = self.label_ids.write();
      // Double-check in case another thread created it
      if let Some(&existing) = names.get(name) {
        return existing;
      }
      names.insert(name.to_string(), id);
      ids.insert(id, name.to_string());
    }
    id
  }

  /// Get label ID by name (returns None if not found)
  pub fn get_label_id(&self, name: &str) -> Option<LabelId> {
    self.label_names.read().get(name).copied()
  }

  /// Get label name by ID
  pub fn get_label_name(&self, id: LabelId) -> Option<String> {
    self.label_ids.read().get(&id).cloned()
  }

  /// Get or create an edge type ID by name
  pub fn get_or_create_etype(&self, name: &str) -> ETypeId {
    {
      let names = self.etype_names.read();
      if let Some(&id) = names.get(name) {
        return id;
      }
    }

    let id = self.alloc_etype_id();
    {
      let mut names = self.etype_names.write();
      let mut ids = self.etype_ids.write();
      if let Some(&existing) = names.get(name) {
        return existing;
      }
      names.insert(name.to_string(), id);
      ids.insert(id, name.to_string());
    }
    id
  }

  /// Get edge type ID by name
  pub fn get_etype_id(&self, name: &str) -> Option<ETypeId> {
    self.etype_names.read().get(name).copied()
  }

  /// Get edge type name by ID
  pub fn get_etype_name(&self, id: ETypeId) -> Option<String> {
    self.etype_ids.read().get(&id).cloned()
  }

  /// Get or create a property key ID by name
  pub fn get_or_create_propkey(&self, name: &str) -> PropKeyId {
    {
      let names = self.propkey_names.read();
      if let Some(&id) = names.get(name) {
        return id;
      }
    }

    let id = self.alloc_propkey_id();
    {
      let mut names = self.propkey_names.write();
      let mut ids = self.propkey_ids.write();
      if let Some(&existing) = names.get(name) {
        return existing;
      }
      names.insert(name.to_string(), id);
      ids.insert(id, name.to_string());
    }
    id
  }

  /// Get property key ID by name
  pub fn get_propkey_id(&self, name: &str) -> Option<PropKeyId> {
    self.propkey_names.read().get(name).copied()
  }

  /// Get property key name by ID
  pub fn get_propkey_name(&self, id: PropKeyId) -> Option<String> {
    self.propkey_ids.read().get(&id).cloned()
  }

  /// Update label name/id mappings
  pub fn update_label_mapping(&self, id: LabelId, name: &str) {
    let mut names = self.label_names.write();
    let mut ids = self.label_ids.write();
    names.insert(name.to_string(), id);
    ids.insert(id, name.to_string());
  }

  /// Update edge type name/id mappings
  pub fn update_etype_mapping(&self, id: ETypeId, name: &str) {
    let mut names = self.etype_names.write();
    let mut ids = self.etype_ids.write();
    names.insert(name.to_string(), id);
    ids.insert(id, name.to_string());
  }

  /// Update property key name/id mappings
  pub fn update_propkey_mapping(&self, id: PropKeyId, name: &str) {
    let mut names = self.propkey_names.write();
    let mut ids = self.propkey_ids.write();
    names.insert(name.to_string(), id);
    ids.insert(id, name.to_string());
  }

  // ========================================================================
  // WAL Operations
  // ========================================================================

  /// Flush WAL records to disk
  pub fn flush_wal(&self, records: &[WalRecord]) -> Result<()> {
    if let Some(ref fd) = self.wal_fd {
      // For now, create a temporary writer for each flush
      // In production, we'd want to keep a persistent writer
      let mut fd_clone = fd.try_clone()?;
      let offset = self.wal_offset.load(Ordering::SeqCst);
      fd_clone.seek(SeekFrom::Start(offset))?;

      let mut new_offset = offset;
      for record in records {
        let bytes = record.build();
        fd_clone.write_all(&bytes)?;
        new_offset += bytes.len() as u64;
      }

      // On macOS, use regular fsync() instead of F_FULLFSYNC for performance
      // This matches Node.js/Bun behavior. F_FULLFSYNC is 190x slower but provides
      // true durability guarantees (data hits physical disk platter).
      // For production use cases requiring full durability, use sync_all() instead.
      #[cfg(target_os = "macos")]
      {
        let ret = unsafe { libc::fsync(fd_clone.as_raw_fd()) };
        if ret != 0 {
          return Err(std::io::Error::last_os_error().into());
        }
      }
      #[cfg(not(target_os = "macos"))]
      {
        fd_clone.sync_all()?;
      }

      self.wal_offset.store(new_offset, Ordering::SeqCst);
      Ok(())
    } else {
      Err(RayError::Internal("WAL not initialized".to_string()))
    }
  }

  /// Check if MVCC is enabled
  pub fn mvcc_enabled(&self) -> bool {
    self.mvcc.is_some()
  }

  // ========================================================================
  // Compaction / Optimize
  // ========================================================================

  /// Optimize the database by compacting snapshot + delta into a new snapshot
  ///
  /// This operation:
  /// 1. Collects all live nodes and edges from snapshot + delta
  /// 2. Builds a new snapshot with the merged data
  /// 3. Updates manifest to point to new snapshot
  /// 4. Creates a new WAL segment
  /// 5. Clears delta
  /// 6. Garbage collects old snapshots
  pub fn optimize(&mut self) -> Result<()> {
    use crate::core::compactor::{optimize, OptimizeOptions};
    use crate::core::snapshot::reader::{ParseSnapshotOptions, SnapshotData};
    use std::sync::Arc;

    if self.read_only {
      return Err(RayError::ReadOnly);
    }

    // Must have manifest for multi-file format
    let manifest = self
      .manifest
      .as_ref()
      .ok_or_else(|| RayError::Internal("No manifest for multi-file database".to_string()))?;

    // Run compaction
    let delta = self.delta.read();
    let (new_manifest, snapshot_path) = optimize(
      &self.path,
      self.snapshot.as_ref(),
      &delta,
      manifest,
      &OptimizeOptions::default(),
    )?;
    drop(delta);

    // Update manifest reference
    self.manifest = Some(new_manifest.clone());

    // Clear delta
    self.delta.write().clear();

    // Reload snapshot from new file
    let file = File::open(&snapshot_path)?;
    let mmap = Arc::new(map_file(&file)?);
    let snapshot_data = SnapshotData::parse(mmap, &ParseSnapshotOptions::default())?;
    self.snapshot = Some(snapshot_data);

    // Update WAL offset (new WAL segment starts at 0)
    self.wal_offset.store(0, Ordering::SeqCst);

    // Reopen WAL file descriptor for new segment
    let wal_path = self
      .path
      .join(WAL_DIR)
      .join(wal_filename(new_manifest.active_wal_seg));
    let wal_fd = FsOpenOptions::new()
      .create(true)
      .read(true)
      .write(true)
      .truncate(true)
      .open(&wal_path)?;
    self.wal_fd = Some(wal_fd);

    // Best-effort snapshot pruning (keep active + previous)
    let _ = crate::graph::checkpoint::prune_snapshots(self, 2);

    Ok(())
  }

  /// Apply pending vector operations to vector stores
  pub fn apply_pending_vectors(&self) {
    let mut delta = self.delta.write();
    let pending: Vec<_> = delta.pending_vectors.drain().collect();
    drop(delta);

    if pending.is_empty() {
      return;
    }

    let mut stores = self.vector_stores.write();

    for ((node_id, prop_key_id), operation) in pending {
      match operation {
        Some(vector) => {
          let store = stores.entry(prop_key_id).or_insert_with(|| {
            let config = VectorStoreConfig::new(vector.len());
            create_vector_store(config)
          });
          let _ = vector_store_insert(store, node_id, &vector);
        }
        None => {
          if let Some(store) = stores.get_mut(&prop_key_id) {
            vector_store_delete(store, node_id);
          }
        }
      }
    }
  }
}

// ============================================================================
// Opening and Closing
// ============================================================================

/// Open a graph database (multi-file format)
pub fn open_graph_db<P: AsRef<Path>>(path: P, options: OpenOptions) -> Result<GraphDB> {
  let path = path.as_ref();

  // Ensure directory exists
  if !path.exists() {
    if !options.create_if_missing {
      return Err(RayError::InvalidPath(format!(
        "Database does not exist at {}",
        path.display()
      )));
    }
    fs::create_dir_all(path)?;
    fs::create_dir_all(path.join(SNAPSHOTS_DIR))?;
    fs::create_dir_all(path.join(WAL_DIR))?;
  }

  // Acquire lock
  let lock_handle = if options.lock_file {
    let lock_type = if options.read_only {
      LockType::Shared
    } else {
      LockType::Exclusive
    };
    Some(FileLock::acquire(path, lock_type)?)
  } else {
    None
  };

  // Read or create manifest
  let manifest = match read_manifest(path)? {
    Some(m) => m,
    None => {
      if options.read_only {
        return Err(RayError::ReadOnly);
      }
      let m = create_empty_manifest();
      write_manifest(path, &m)?;
      m
    }
  };

  // Load snapshot if exists
  let snapshot: Option<SnapshotData> = if manifest.active_snapshot_gen > 0 {
    let snapshot_name = crate::constants::snapshot_filename(manifest.active_snapshot_gen);
    let snapshot_path = path.join(SNAPSHOTS_DIR).join(&snapshot_name);

    match SnapshotData::load(&snapshot_path) {
      Ok(snap) => Some(snap),
      Err(e) => {
        // Log warning but don't fail - database can work without snapshot
        eprintln!("Warning: Failed to load snapshot {snapshot_name}: {e}");
        None
      }
    }
  } else {
    None
  };

  // Initialize ID allocators from snapshot
  let (mut next_node_id, mut next_label_id, mut next_etype_id, mut next_propkey_id) =
    if let Some(ref snap) = snapshot {
      (
        snap.header.max_node_id + 1,
        snap.header.num_labels as u32 + 1,
        snap.header.num_etypes as u32 + 1,
        snap.header.num_propkeys as u32 + 1,
      )
    } else {
      (
        INITIAL_NODE_ID,
        INITIAL_LABEL_ID,
        INITIAL_ETYPE_ID,
        INITIAL_PROPKEY_ID,
      )
    };

  // Open or create WAL
  let wal_dir = path.join(WAL_DIR);
  if !options.read_only && !wal_dir.exists() {
    fs::create_dir_all(&wal_dir)?;
  }
  let wal_filename = wal_filename(manifest.active_wal_seg);
  let wal_path = wal_dir.join(&wal_filename);

  let (wal_fd, wal_offset) = if !options.read_only {
    if !wal_path.exists() {
      // Create new WAL file with header
      let writer = WalWriter::create(&wal_path, manifest.active_wal_seg)?;
      let offset = writer.position() as u64;
      let fd = writer.into_inner();
      (Some(fd), offset)
    } else {
      // Open existing WAL
      let fd = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&wal_path)?;
      let metadata = fd.metadata()?;
      (Some(fd), metadata.len())
    }
  } else {
    (None, 0)
  };

  // Replay WAL for recovery
  let mut next_tx_id = INITIAL_TX_ID;
  let mut next_commit_ts: u64 = 1;
  let mut delta = DeltaState::new();

  // Schema maps
  let mut label_names: HashMap<String, LabelId> = HashMap::new();
  let mut label_ids: HashMap<LabelId, String> = HashMap::new();
  let mut etype_names: HashMap<String, ETypeId> = HashMap::new();
  let mut etype_ids: HashMap<ETypeId, String> = HashMap::new();
  let mut propkey_names: HashMap<String, PropKeyId> = HashMap::new();
  let mut propkey_ids: HashMap<PropKeyId, String> = HashMap::new();

  // Load schema from snapshot if available
  if let Some(ref snap) = snapshot {
    // Load labels
    for i in 1..=snap.header.num_labels as u32 {
      if let Some(name) = snap.get_label_name(i) {
        label_names.insert(name.to_string(), i);
        label_ids.insert(i, name.to_string());
      }
    }
    // Load edge types
    for i in 1..=snap.header.num_etypes as u32 {
      if let Some(name) = snap.get_etype_name(i) {
        etype_names.insert(name.to_string(), i);
        etype_ids.insert(i, name.to_string());
      }
    }
    // Load property keys
    for i in 1..=snap.header.num_propkeys as u32 {
      if let Some(name) = snap.get_propkey_name(i) {
        propkey_names.insert(name.to_string(), i);
        propkey_ids.insert(i, name.to_string());
      }
    }
  }

  let mut committed_in_order: Vec<(TxId, Vec<crate::core::wal::record::ParsedWalRecord>)> =
    Vec::new();

  if let Ok(Some(wal_segment)) = load_wal_segment_by_id(path, manifest.active_wal_seg) {
    let recovery = recover_from_segment(&wal_segment);

    // Update next_tx_id
    if recovery.max_txid >= next_tx_id {
      next_tx_id = recovery.max_txid + 1;
    }

    // Replay committed transactions in commit order
    use crate::core::wal::record::{
      parse_add_edge_payload, parse_add_node_label_payload, parse_create_node_payload,
      parse_define_etype_payload, parse_define_label_payload, parse_define_propkey_payload,
      parse_del_edge_prop_payload, parse_del_node_prop_payload, parse_del_node_vector_payload,
      parse_delete_edge_payload, parse_delete_node_payload, parse_remove_node_label_payload,
      parse_set_edge_prop_payload, parse_set_node_prop_payload, parse_set_node_vector_payload,
    };
    use crate::types::WalRecordType;

    let mut pending: std::collections::HashMap<TxId, Vec<_>> = std::collections::HashMap::new();

    for record in &wal_segment.records {
      match record.record_type {
        WalRecordType::Begin => {
          pending.insert(record.txid, Vec::new());
        }
        WalRecordType::Commit => {
          if let Some(records) = pending.remove(&record.txid) {
            committed_in_order.push((record.txid, records));
          }
        }
        WalRecordType::Rollback => {
          pending.remove(&record.txid);
        }
        _ => {
          if let Some(records) = pending.get_mut(&record.txid) {
            records.push(record.clone());
          }
        }
      }
    }

    for (_txid, records) in &committed_in_order {
      next_commit_ts += 1;

      for record in records {
        match record.record_type {
          WalRecordType::CreateNode => {
            if let Some(data) = parse_create_node_payload(&record.payload) {
              delta.create_node(data.node_id, data.key.as_deref());
              if data.node_id >= next_node_id {
                next_node_id = data.node_id + 1;
              }
            }
          }
          WalRecordType::DeleteNode => {
            if let Some(data) = parse_delete_node_payload(&record.payload) {
              delta.delete_node(data.node_id);
            }
          }
          WalRecordType::AddEdge => {
            if let Some(data) = parse_add_edge_payload(&record.payload) {
              delta.add_edge(data.src, data.etype, data.dst);
            }
          }
          WalRecordType::DeleteEdge => {
            if let Some(data) = parse_delete_edge_payload(&record.payload) {
              delta.delete_edge(data.src, data.etype, data.dst);
            }
          }
          WalRecordType::SetNodeProp => {
            if let Some(data) = parse_set_node_prop_payload(&record.payload) {
              delta.set_node_prop(data.node_id, data.key_id, data.value);
            }
          }
          WalRecordType::DelNodeProp => {
            if let Some(data) = parse_del_node_prop_payload(&record.payload) {
              delta.delete_node_prop(data.node_id, data.key_id);
            }
          }
          WalRecordType::SetEdgeProp => {
            if let Some(data) = parse_set_edge_prop_payload(&record.payload) {
              delta.set_edge_prop(data.src, data.etype, data.dst, data.key_id, data.value);
            }
          }
          WalRecordType::DelEdgeProp => {
            if let Some(data) = parse_del_edge_prop_payload(&record.payload) {
              delta.delete_edge_prop(data.src, data.etype, data.dst, data.key_id);
            }
          }
          WalRecordType::DefineLabel => {
            if let Some(data) = parse_define_label_payload(&record.payload) {
              delta.define_label(data.label_id, &data.name);
              label_names.insert(data.name.clone(), data.label_id);
              label_ids.insert(data.label_id, data.name);
              if data.label_id >= next_label_id {
                next_label_id = data.label_id + 1;
              }
            }
          }
          WalRecordType::AddNodeLabel => {
            if let Some(data) = parse_add_node_label_payload(&record.payload) {
              delta.add_node_label(data.node_id, data.label_id);
            }
          }
          WalRecordType::RemoveNodeLabel => {
            if let Some(data) = parse_remove_node_label_payload(&record.payload) {
              delta.remove_node_label(data.node_id, data.label_id);
            }
          }
          WalRecordType::DefineEtype => {
            if let Some(data) = parse_define_etype_payload(&record.payload) {
              delta.define_etype(data.label_id, &data.name);
              etype_names.insert(data.name.clone(), data.label_id);
              etype_ids.insert(data.label_id, data.name);
              if data.label_id >= next_etype_id {
                next_etype_id = data.label_id + 1;
              }
            }
          }
          WalRecordType::DefinePropkey => {
            if let Some(data) = parse_define_propkey_payload(&record.payload) {
              delta.define_propkey(data.label_id, &data.name);
              propkey_names.insert(data.name.clone(), data.label_id);
              propkey_ids.insert(data.label_id, data.name);
              if data.label_id >= next_propkey_id {
                next_propkey_id = data.label_id + 1;
              }
            }
          }
          WalRecordType::SetNodeVector => {
            if let Some(data) = parse_set_node_vector_payload(&record.payload) {
              delta
                .pending_vectors
                .insert((data.node_id, data.prop_key_id), Some(data.vector));
            }
          }
          WalRecordType::DelNodeVector => {
            if let Some(data) = parse_del_node_vector_payload(&record.payload) {
              delta
                .pending_vectors
                .insert((data.node_id, data.prop_key_id), None);
            }
          }
          _ => {}
        }
      }
    }
  }

  // Apply pending vector operations from WAL replay
  let mut vector_stores: HashMap<PropKeyId, VectorManifest> = HashMap::new();
  for ((node_id, prop_key_id), operation) in delta.pending_vectors.drain() {
    match operation {
      Some(vector) => {
        let store = vector_stores.entry(prop_key_id).or_insert_with(|| {
          let config = VectorStoreConfig::new(vector.len());
          create_vector_store(config)
        });
        let _ = vector_store_insert(store, node_id, &vector);
      }
      None => {
        if let Some(store) = vector_stores.get_mut(&prop_key_id) {
          vector_store_delete(store, node_id);
        }
      }
    }
  }

  // Initialize MVCC if enabled (after WAL replay for correct tx/commit IDs)
  let mvcc = if options.mvcc {
    let mut gc_config = GcConfig::default();
    if let Some(v) = options.mvcc_gc_interval_ms {
      gc_config.interval_ms = v;
    }
    if let Some(v) = options.mvcc_retention_ms {
      gc_config.retention_ms = v;
    }
    if let Some(v) = options.mvcc_max_chain_depth {
      gc_config.max_chain_depth = v;
    }

    let mvcc = std::sync::Arc::new(MvccManager::new(next_tx_id, next_commit_ts, gc_config));

    // Rebuild version chains from WAL if MVCC is enabled
    if !committed_in_order.is_empty() {
      use crate::core::wal::record::{
        parse_add_edge_payload, parse_create_node_payload, parse_del_edge_prop_payload,
        parse_del_node_prop_payload, parse_delete_edge_payload, parse_delete_node_payload,
        parse_set_edge_prop_payload, parse_set_node_prop_payload,
      };
      use crate::types::WalRecordType;

      let mut commit_ts: u64 = 1;
      for (txid, records) in &committed_in_order {
        for record in records {
          match record.record_type {
            WalRecordType::CreateNode => {
              if let Some(data) = parse_create_node_payload(&record.payload) {
                if let Some(node_delta) = delta.created_nodes.get(&data.node_id) {
                  let mut vc = mvcc.version_chain.lock();
                  vc.append_node_version(
                    data.node_id,
                    NodeVersionData {
                      node_id: data.node_id,
                      delta: node_delta.clone(),
                    },
                    *txid,
                    commit_ts,
                  );
                }
              }
            }
            WalRecordType::DeleteNode => {
              if let Some(data) = parse_delete_node_payload(&record.payload) {
                let mut vc = mvcc.version_chain.lock();
                vc.delete_node_version(data.node_id, *txid, commit_ts);
              }
            }
            WalRecordType::AddEdge => {
              if let Some(data) = parse_add_edge_payload(&record.payload) {
                let mut vc = mvcc.version_chain.lock();
                vc.append_edge_version(data.src, data.etype, data.dst, true, *txid, commit_ts);
              }
            }
            WalRecordType::DeleteEdge => {
              if let Some(data) = parse_delete_edge_payload(&record.payload) {
                let mut vc = mvcc.version_chain.lock();
                vc.append_edge_version(data.src, data.etype, data.dst, false, *txid, commit_ts);
              }
            }
            WalRecordType::SetNodeProp => {
              if let Some(data) = parse_set_node_prop_payload(&record.payload) {
                let mut vc = mvcc.version_chain.lock();
                vc.append_node_prop_version(
                  data.node_id,
                  data.key_id,
                  Some(data.value),
                  *txid,
                  commit_ts,
                );
              }
            }
            WalRecordType::DelNodeProp => {
              if let Some(data) = parse_del_node_prop_payload(&record.payload) {
                let mut vc = mvcc.version_chain.lock();
                vc.append_node_prop_version(data.node_id, data.key_id, None, *txid, commit_ts);
              }
            }
            WalRecordType::SetEdgeProp => {
              if let Some(data) = parse_set_edge_prop_payload(&record.payload) {
                let mut vc = mvcc.version_chain.lock();
                vc.append_edge_prop_version(
                  data.src,
                  data.etype,
                  data.dst,
                  data.key_id,
                  Some(data.value),
                  *txid,
                  commit_ts,
                );
              }
            }
            WalRecordType::DelEdgeProp => {
              if let Some(data) = parse_del_edge_prop_payload(&record.payload) {
                let mut vc = mvcc.version_chain.lock();
                vc.append_edge_prop_version(
                  data.src,
                  data.etype,
                  data.dst,
                  data.key_id,
                  None,
                  *txid,
                  commit_ts,
                );
              }
            }
            _ => {}
          }
        }
        commit_ts += 1;
      }
    }

    mvcc.start();
    Some(mvcc)
  } else {
    None
  };

  Ok(GraphDB {
    path: path.to_path_buf(),
    read_only: options.read_only,
    is_single_file: false,
    manifest: Some(manifest),
    snapshot,
    wal_fd,
    wal_offset: AtomicU64::new(wal_offset),
    delta: RwLock::new(delta),
    vector_stores: RwLock::new(vector_stores),
    next_node_id: AtomicU64::new(next_node_id),
    next_label_id: AtomicU32::new(next_label_id),
    next_etype_id: AtomicU32::new(next_etype_id),
    next_propkey_id: AtomicU32::new(next_propkey_id),
    next_tx_id: AtomicU64::new(next_tx_id),
    current_tx: Mutex::new(None),
    checkpoint_status: Mutex::new(CheckpointStatus::Idle),
    mvcc,
    lock_handle,
    label_names: RwLock::new(label_names),
    label_ids: RwLock::new(label_ids),
    etype_names: RwLock::new(etype_names),
    etype_ids: RwLock::new(etype_ids),
    propkey_names: RwLock::new(propkey_names),
    propkey_ids: RwLock::new(propkey_ids),
  })
}

/// Close the database
pub fn close_graph_db(db: GraphDB) -> Result<()> {
  // Sync WAL if open
  if let Some(fd) = db.wal_fd {
    fd.sync_all()?;
  }

  if let Some(ref mvcc) = db.mvcc {
    mvcc.stop();
  }

  // Lock is released when db.lock_handle is dropped
  Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;
  use tempfile::tempdir;

  #[test]
  fn test_open_new_database() {
    let temp_dir = tempdir().unwrap();
    let db = open_graph_db(temp_dir.path(), OpenOptions::new()).unwrap();

    assert!(!db.read_only);
    assert!(!db.is_single_file);
    assert!(db.manifest.is_some());

    close_graph_db(db).unwrap();
  }

  #[test]
  fn test_id_allocation() {
    let temp_dir = tempdir().unwrap();
    let db = open_graph_db(temp_dir.path(), OpenOptions::new()).unwrap();

    let id1 = db.alloc_node_id();
    let id2 = db.alloc_node_id();
    assert_eq!(id2, id1 + 1);

    let label1 = db.alloc_label_id();
    let label2 = db.alloc_label_id();
    assert_eq!(label2, label1 + 1);

    close_graph_db(db).unwrap();
  }

  #[test]
  fn test_schema_lookup() {
    let temp_dir = tempdir().unwrap();
    let db = open_graph_db(temp_dir.path(), OpenOptions::new()).unwrap();

    // Create a label
    let label_id = db.get_or_create_label("Person");
    assert!(label_id >= INITIAL_LABEL_ID);

    // Lookup should return same ID
    let label_id2 = db.get_or_create_label("Person");
    assert_eq!(label_id, label_id2);

    // Lookup by name
    assert_eq!(db.get_label_id("Person"), Some(label_id));
    assert_eq!(db.get_label_name(label_id), Some("Person".to_string()));

    // Non-existent should return None
    assert_eq!(db.get_label_id("Unknown"), None);

    close_graph_db(db).unwrap();
  }

  #[test]
  fn test_reopen_database() {
    let temp_dir = tempdir().unwrap();

    // Open and create some state
    {
      let db = open_graph_db(temp_dir.path(), OpenOptions::new()).unwrap();
      let _label_id = db.get_or_create_label("Person");
      close_graph_db(db).unwrap();
    }

    // Reopen - should work
    let db = open_graph_db(temp_dir.path(), OpenOptions::new()).unwrap();
    // Note: Schema is not persisted to WAL yet, so it won't survive reopen
    // This will be fixed when we implement proper WAL recording for definitions
    close_graph_db(db).unwrap();
  }

  #[test]
  fn test_wal_replay_on_reopen() {
    use crate::graph::edges::add_edge;
    use crate::graph::nodes::{create_node, NodeOpts};
    use crate::graph::tx::{begin_tx, commit};

    let temp_dir = tempdir().unwrap();
    let mut node_ids = Vec::new();

    // First session: create nodes and edges
    {
      let db = open_graph_db(temp_dir.path(), OpenOptions::new()).unwrap();

      let mut tx = begin_tx(&db).unwrap();
      let node1 = create_node(&mut tx, NodeOpts::new().with_key("alice")).unwrap();
      let node2 = create_node(&mut tx, NodeOpts::new().with_key("bob")).unwrap();
      add_edge(&mut tx, node1, 1, node2).unwrap();
      commit(&mut tx).unwrap();

      node_ids.push(node1);
      node_ids.push(node2);

      close_graph_db(db).unwrap();
    }

    // Second session: verify data was recovered from WAL
    {
      let db = open_graph_db(temp_dir.path(), OpenOptions::new()).unwrap();

      // Check delta has the replayed data
      let delta = db.delta.read();

      // Nodes should exist in delta
      assert!(
        delta.is_node_created(node_ids[0]),
        "Node 1 should be in delta after WAL replay"
      );
      assert!(
        delta.is_node_created(node_ids[1]),
        "Node 2 should be in delta after WAL replay"
      );

      // Edge should exist in delta
      assert!(
        delta.is_edge_added(node_ids[0], 1, node_ids[1]),
        "Edge should be in delta after WAL replay"
      );

      // Keys should be indexed
      assert_eq!(delta.get_node_by_key("alice"), Some(node_ids[0]));
      assert_eq!(delta.get_node_by_key("bob"), Some(node_ids[1]));

      drop(delta);
      close_graph_db(db).unwrap();
    }
  }

  #[test]
  fn test_wal_replay_definitions() {
    use crate::graph::definitions::{define_etype, define_label, define_propkey};
    use crate::graph::tx::{begin_tx, commit};

    let temp_dir = tempdir().unwrap();

    // First session: define schema
    {
      let db = open_graph_db(temp_dir.path(), OpenOptions::new()).unwrap();

      let mut tx = begin_tx(&db).unwrap();
      define_label(&mut tx, "Person").unwrap();
      define_etype(&mut tx, "KNOWS").unwrap();
      define_propkey(&mut tx, "name").unwrap();
      commit(&mut tx).unwrap();

      close_graph_db(db).unwrap();
    }

    // Second session: verify schema was recovered
    {
      let db = open_graph_db(temp_dir.path(), OpenOptions::new()).unwrap();

      // Schema should be loaded from WAL replay
      assert!(
        db.get_label_id("Person").is_some(),
        "Label should exist after WAL replay"
      );
      assert!(
        db.get_etype_id("KNOWS").is_some(),
        "Etype should exist after WAL replay"
      );
      assert!(
        db.get_propkey_id("name").is_some(),
        "Propkey should exist after WAL replay"
      );

      close_graph_db(db).unwrap();
    }
  }

  #[test]
  fn test_wal_replay_node_properties() {
    use crate::graph::nodes::{create_node, get_node_prop, set_node_prop, NodeOpts};
    use crate::graph::tx::{begin_read_tx, begin_tx, commit};

    let temp_dir = tempdir().unwrap();
    let propkey_id = 1;

    // First session: create node with property
    let node_id = {
      let db = open_graph_db(temp_dir.path(), OpenOptions::new()).unwrap();

      let mut tx = begin_tx(&db).unwrap();
      let node_id = create_node(&mut tx, NodeOpts::new()).unwrap();
      set_node_prop(
        &mut tx,
        node_id,
        propkey_id,
        PropValue::String("test_value".to_string()),
      )
      .unwrap();
      commit(&mut tx).unwrap();

      close_graph_db(db).unwrap();
      node_id
    };

    // Second session: verify property was recovered
    {
      let db = open_graph_db(temp_dir.path(), OpenOptions::new()).unwrap();

      let tx = begin_read_tx(&db).unwrap();
      let prop = get_node_prop(&tx, node_id, propkey_id);
      assert_eq!(prop, Some(PropValue::String("test_value".to_string())));

      close_graph_db(db).unwrap();
    }
  }
}
