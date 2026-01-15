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

use crate::constants::*;
use crate::core::pager::{create_pager, open_pager, is_valid_page_size, pages_to_store, FilePager};
use crate::core::snapshot::writer::{build_snapshot_to_memory, NodeData, EdgeData, SnapshotBuildInput};
use crate::core::snapshot::reader::SnapshotData;
use crate::core::wal::buffer::WalBuffer;
use crate::core::wal::record::{
    extract_committed_transactions, parse_create_node_payload, parse_delete_node_payload,
    parse_add_edge_payload, parse_delete_edge_payload, parse_set_node_prop_payload,
    parse_del_node_prop_payload, parse_define_label_payload, parse_define_etype_payload,
    parse_define_propkey_payload, ParsedWalRecord, WalRecord,
    build_begin_payload, build_commit_payload, build_rollback_payload,
    build_create_node_payload, build_delete_node_payload, build_add_edge_payload,
    build_delete_edge_payload, build_set_node_prop_payload, build_del_node_prop_payload,
    build_define_label_payload, build_define_etype_payload, build_define_propkey_payload,
};
use crate::error::{RayError, Result};
use crate::types::*;

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
    next_node_id: AtomicU64,
    next_label_id: AtomicU32,
    next_etype_id: AtomicU32,
    next_propkey_id: AtomicU32,
    next_tx_id: AtomicU64,

    /// Current active transaction
    pub current_tx: Mutex<Option<TxState>>,

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

    /// Enable auto-checkpoint when WAL usage exceeds threshold
    auto_checkpoint: bool,
    /// WAL usage threshold (0.0-1.0) to trigger auto-checkpoint
    checkpoint_threshold: f64,
    /// Use background (non-blocking) checkpoint instead of blocking
    background_checkpoint: bool,
    /// Current checkpoint state
    checkpoint_status: Mutex<CheckpointStatus>,
}

/// Transaction state
#[derive(Debug)]
pub struct TxState {
    pub txid: TxId,
    pub read_only: bool,
    pub snapshot_ts: u64,
}

impl TxState {
    pub fn new(txid: TxId, read_only: bool, snapshot_ts: u64) -> Self {
        Self {
            txid,
            read_only,
            snapshot_ts,
        }
    }
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
// Open Options
// ============================================================================

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
    let (mut pager, header, is_new) = if file_exists {
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
    let wal_buffer = WalBuffer::from_header(&header);

    // Initialize ID allocators from header
    let mut next_node_id = INITIAL_NODE_ID;
    let mut next_label_id = INITIAL_LABEL_ID;
    let mut next_etype_id = INITIAL_ETYPE_ID;
    let mut next_propkey_id = INITIAL_PROPKEY_ID;
    let mut next_tx_id = header.next_tx_id;

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
            std::sync::Arc::new(unsafe {
                // Safety: We're creating an owned Mmap from the file
                // This is safe because the pager keeps the file open
                memmap2::Mmap::map(pager.file())?
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
                eprintln!("Warning: Failed to parse snapshot: {}", e);
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
        let committed = extract_committed_transactions(&wal_records);

        // Replay committed transactions
        for (_txid, records) in committed {
            for record in records {
                replay_wal_record(
                    record,
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
    })
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

// ============================================================================
// WAL Scanning
// ============================================================================

/// Scan WAL records from the circular buffer
fn scan_wal_records(pager: &mut FilePager, header: &DbHeaderV1) -> Result<Vec<ParsedWalRecord>> {
    use crate::core::wal::record::parse_wal_record;

    let mut records = Vec::new();
    let wal_start = header.wal_start_page * header.page_size as u64;
    let wal_size = header.wal_page_count * header.page_size as u64;

    let mut pos = header.wal_tail;
    let head = header.wal_head;

    // If tail == head, WAL is empty
    if pos == head {
        return Ok(records);
    }

    // Read the WAL area into memory for scanning
    // This is simpler than page-by-page reading for now
    let wal_data = read_wal_area(pager, header)?;

    while pos != head {
        // Handle wrap-around
        let actual_pos = pos % wal_size;

        // Check for skip marker
        if actual_pos + 8 > wal_size as u64 {
            // Not enough space for header, wrap to start
            pos = 0;
            continue;
        }

        let offset = actual_pos as usize;
        if offset + 4 > wal_data.len() {
            break;
        }

        let rec_len = u32::from_le_bytes([
            wal_data[offset],
            wal_data[offset + 1],
            wal_data[offset + 2],
            wal_data[offset + 3],
        ]) as usize;

        // Skip marker check
        if rec_len == 0 {
            if offset + 8 <= wal_data.len() {
                let marker = u32::from_le_bytes([
                    wal_data[offset + 4],
                    wal_data[offset + 5],
                    wal_data[offset + 6],
                    wal_data[offset + 7],
                ]);
                if marker == 0xFFFFFFFF {
                    // Skip to start
                    pos = 0;
                    continue;
                }
            }
            break; // Invalid record
        }

        // Parse the record
        if let Some(record) = parse_wal_record(&wal_data, offset) {
            let aligned_size = crate::util::binary::align_up(rec_len, WAL_RECORD_ALIGNMENT);
            pos = (actual_pos + aligned_size as u64) % wal_size;
            records.push(record);
        } else {
            break; // Invalid record
        }
    }

    Ok(records)
}

/// Read the entire WAL area into memory
fn read_wal_area(pager: &mut FilePager, header: &DbHeaderV1) -> Result<Vec<u8>> {
    let wal_pages = header.wal_page_count as u32;
    let page_size = header.page_size as usize;
    let mut wal_data = Vec::with_capacity(wal_pages as usize * page_size);

    for i in 0..wal_pages {
        let page_num = header.wal_start_page as u32 + i;
        let page = pager.read_page(page_num)?;
        wal_data.extend_from_slice(&page);
    }

    Ok(wal_data)
}

/// Replay a single WAL record into delta and update allocators/schema
#[allow(clippy::too_many_arguments)]
fn replay_wal_record(
    record: &ParsedWalRecord,
    delta: &mut DeltaState,
    next_node_id: &mut u64,
    next_label_id: &mut u32,
    next_etype_id: &mut u32,
    next_propkey_id: &mut u32,
    label_names: &mut HashMap<String, LabelId>,
    label_ids: &mut HashMap<LabelId, String>,
    etype_names: &mut HashMap<String, ETypeId>,
    etype_ids: &mut HashMap<ETypeId, String>,
    propkey_names: &mut HashMap<String, PropKeyId>,
    propkey_ids: &mut HashMap<PropKeyId, String>,
) {
    match record.record_type {
        WalRecordType::CreateNode => {
            if let Some(data) = parse_create_node_payload(&record.payload) {
                delta.create_node(data.node_id, data.key.as_deref());
                if data.node_id >= *next_node_id {
                    *next_node_id = data.node_id + 1;
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
        WalRecordType::DefineLabel => {
            if let Some(data) = parse_define_label_payload(&record.payload) {
                delta.define_label(data.label_id, &data.name);
                label_names.insert(data.name.clone(), data.label_id);
                label_ids.insert(data.label_id, data.name);
                if data.label_id >= *next_label_id {
                    *next_label_id = data.label_id + 1;
                }
            }
        }
        WalRecordType::DefineEtype => {
            if let Some(data) = parse_define_etype_payload(&record.payload) {
                delta.define_etype(data.label_id, &data.name);
                etype_names.insert(data.name.clone(), data.label_id);
                etype_ids.insert(data.label_id, data.name);
                if data.label_id >= *next_etype_id {
                    *next_etype_id = data.label_id + 1;
                }
            }
        }
        WalRecordType::DefinePropkey => {
            if let Some(data) = parse_define_propkey_payload(&record.payload) {
                delta.define_propkey(data.label_id, &data.name);
                propkey_names.insert(data.name.clone(), data.label_id);
                propkey_ids.insert(data.label_id, data.name);
                if data.label_id >= *next_propkey_id {
                    *next_propkey_id = data.label_id + 1;
                }
            }
        }
        _ => {
            // Other record types (vectors, edge props, etc.) - skip for now
        }
    }
}

// ============================================================================
// SingleFileDB Implementation
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

    /// Get or create a label ID by name
    pub fn get_or_create_label(&self, name: &str) -> LabelId {
        {
            let names = self.label_names.read();
            if let Some(&id) = names.get(name) {
                return id;
            }
        }

        let id = self.alloc_label_id();
        {
            let mut names = self.label_names.write();
            let mut ids = self.label_ids.write();
            if let Some(&existing) = names.get(name) {
                return existing;
            }
            names.insert(name.to_string(), id);
            ids.insert(id, name.to_string());
        }
        id
    }

    /// Get label ID by name
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

    // ========================================================================
    // Transaction Methods
    // ========================================================================

    /// Begin a new transaction
    pub fn begin(&self, read_only: bool) -> Result<TxId> {
        if self.read_only && !read_only {
            return Err(RayError::ReadOnly);
        }

        let mut current_tx = self.current_tx.lock();
        if current_tx.is_some() {
            return Err(RayError::TransactionInProgress);
        }

        let txid = self.alloc_tx_id();
        let snapshot_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);

        // Write BEGIN record to WAL (for write transactions)
        if !read_only {
            let record = WalRecord::new(WalRecordType::Begin, txid, build_begin_payload());
            let mut pager = self.pager.lock();
            let mut wal = self.wal_buffer.lock();
            wal.write_record(&record, &mut pager)?;
        }

        *current_tx = Some(TxState::new(txid, read_only, snapshot_ts));
        Ok(txid)
    }

    /// Commit the current transaction
    pub fn commit(&self) -> Result<()> {
        // Take the transaction and release the lock immediately
        let tx = {
            let mut current_tx = self.current_tx.lock();
            current_tx.take().ok_or(RayError::NoTransaction)?
        };

        if tx.read_only {
            // Read-only transactions don't need WAL
            return Ok(());
        }

        // Write COMMIT record to WAL
        let record = WalRecord::new(WalRecordType::Commit, tx.txid, build_commit_payload());
        {
            let mut pager = self.pager.lock();
            let mut wal = self.wal_buffer.lock();
            wal.write_record(&record, &mut pager)?;

            // Flush WAL to disk
            wal.flush(&mut pager)?;
            pager.sync()?;
        }

        // Check if auto-checkpoint should be triggered
        // Note: We release all locks above first to avoid deadlock during checkpoint
        if self.auto_checkpoint && self.should_checkpoint(self.checkpoint_threshold) {
            // Don't trigger if checkpoint is already running
            if !self.is_checkpoint_running() {
                // Use background or blocking checkpoint based on config
                let result = if self.background_checkpoint {
                    self.background_checkpoint()
                } else {
                    self.checkpoint()
                };
                
                // Log errors but don't fail the commit
                if let Err(e) = result {
                    eprintln!("Warning: Auto-checkpoint failed: {}", e);
                }
            }
        }

        Ok(())
    }

    /// Rollback the current transaction
    pub fn rollback(&self) -> Result<()> {
        let mut current_tx = self.current_tx.lock();
        let tx = current_tx.take().ok_or(RayError::NoTransaction)?;

        if tx.read_only {
            // Read-only transactions don't need WAL
            return Ok(());
        }

        // Write ROLLBACK record to WAL
        let record = WalRecord::new(WalRecordType::Rollback, tx.txid, build_rollback_payload());
        let mut pager = self.pager.lock();
        let mut wal = self.wal_buffer.lock();
        wal.write_record(&record, &mut pager)?;

        // Discard pending writes (rollback doesn't need to be durable)
        wal.discard_pending();

        // TODO: Discard delta changes for this transaction

        Ok(())
    }

    /// Check if there's an active transaction
    pub fn has_transaction(&self) -> bool {
        self.current_tx.lock().is_some()
    }

    /// Get the current transaction ID (if any)
    pub fn current_txid(&self) -> Option<TxId> {
        self.current_tx.lock().as_ref().map(|tx| tx.txid)
    }

    // ========================================================================
    // Write Methods (require active transaction)
    // ========================================================================

    /// Write a WAL record (internal helper)
    fn write_wal(&self, record: WalRecord) -> Result<()> {
        let mut pager = self.pager.lock();
        let mut wal = self.wal_buffer.lock();
        wal.write_record(&record, &mut pager)?;
        Ok(())
    }

    /// Get current transaction ID or error
    fn require_write_tx(&self) -> Result<TxId> {
        let current_tx = self.current_tx.lock();
        match current_tx.as_ref() {
            Some(tx) if !tx.read_only => Ok(tx.txid),
            Some(_) => Err(RayError::ReadOnly),
            None => Err(RayError::NoTransaction),
        }
    }

    /// Create a node
    pub fn create_node(&self, key: Option<&str>) -> Result<NodeId> {
        let txid = self.require_write_tx()?;
        let node_id = self.alloc_node_id();

        // Write WAL record
        let record = WalRecord::new(
            WalRecordType::CreateNode,
            txid,
            build_create_node_payload(node_id, key),
        );
        self.write_wal(record)?;

        // Update delta
        self.delta.write().create_node(node_id, key);

        Ok(node_id)
    }

    /// Delete a node
    pub fn delete_node(&self, node_id: NodeId) -> Result<()> {
        let txid = self.require_write_tx()?;

        // Write WAL record
        let record = WalRecord::new(
            WalRecordType::DeleteNode,
            txid,
            build_delete_node_payload(node_id),
        );
        self.write_wal(record)?;

        // Update delta
        self.delta.write().delete_node(node_id);

        Ok(())
    }

    /// Add an edge
    pub fn add_edge(&self, src: NodeId, etype: ETypeId, dst: NodeId) -> Result<()> {
        let txid = self.require_write_tx()?;

        // Write WAL record
        let record = WalRecord::new(
            WalRecordType::AddEdge,
            txid,
            build_add_edge_payload(src, etype, dst),
        );
        self.write_wal(record)?;

        // Update delta
        self.delta.write().add_edge(src, etype, dst);

        Ok(())
    }

    /// Add an edge by type name
    pub fn add_edge_by_name(&self, src: NodeId, etype_name: &str, dst: NodeId) -> Result<()> {
        let etype = self.get_or_create_etype(etype_name);
        self.add_edge(src, etype, dst)
    }

    /// Delete an edge
    pub fn delete_edge(&self, src: NodeId, etype: ETypeId, dst: NodeId) -> Result<()> {
        let txid = self.require_write_tx()?;

        // Write WAL record
        let record = WalRecord::new(
            WalRecordType::DeleteEdge,
            txid,
            build_delete_edge_payload(src, etype, dst),
        );
        self.write_wal(record)?;

        // Update delta
        self.delta.write().delete_edge(src, etype, dst);

        Ok(())
    }

    /// Set a node property
    pub fn set_node_prop(&self, node_id: NodeId, key_id: PropKeyId, value: PropValue) -> Result<()> {
        let txid = self.require_write_tx()?;

        // Write WAL record
        let record = WalRecord::new(
            WalRecordType::SetNodeProp,
            txid,
            build_set_node_prop_payload(node_id, key_id, &value),
        );
        self.write_wal(record)?;

        // Update delta
        self.delta.write().set_node_prop(node_id, key_id, value);

        Ok(())
    }

    /// Set a node property by key name
    pub fn set_node_prop_by_name(&self, node_id: NodeId, key_name: &str, value: PropValue) -> Result<()> {
        let key_id = self.get_or_create_propkey(key_name);
        self.set_node_prop(node_id, key_id, value)
    }

    /// Delete a node property
    pub fn delete_node_prop(&self, node_id: NodeId, key_id: PropKeyId) -> Result<()> {
        let txid = self.require_write_tx()?;

        // Write WAL record
        let record = WalRecord::new(
            WalRecordType::DelNodeProp,
            txid,
            build_del_node_prop_payload(node_id, key_id),
        );
        self.write_wal(record)?;

        // Update delta
        self.delta.write().delete_node_prop(node_id, key_id);

        Ok(())
    }

    /// Define a new label (writes to WAL for durability)
    pub fn define_label(&self, name: &str) -> Result<LabelId> {
        let txid = self.require_write_tx()?;

        // Check if already exists
        if let Some(id) = self.get_label_id(name) {
            return Ok(id);
        }

        let label_id = self.alloc_label_id();

        // Write WAL record
        let record = WalRecord::new(
            WalRecordType::DefineLabel,
            txid,
            build_define_label_payload(label_id, name),
        );
        self.write_wal(record)?;

        // Update schema maps
        {
            let mut names = self.label_names.write();
            let mut ids = self.label_ids.write();
            names.insert(name.to_string(), label_id);
            ids.insert(label_id, name.to_string());
        }

        // Update delta
        self.delta.write().define_label(label_id, name);

        Ok(label_id)
    }

    /// Define a new edge type (writes to WAL for durability)
    pub fn define_etype(&self, name: &str) -> Result<ETypeId> {
        let txid = self.require_write_tx()?;

        // Check if already exists
        if let Some(id) = self.get_etype_id(name) {
            return Ok(id);
        }

        let etype_id = self.alloc_etype_id();

        // Write WAL record
        let record = WalRecord::new(
            WalRecordType::DefineEtype,
            txid,
            build_define_etype_payload(etype_id, name),
        );
        self.write_wal(record)?;

        // Update schema maps
        {
            let mut names = self.etype_names.write();
            let mut ids = self.etype_ids.write();
            names.insert(name.to_string(), etype_id);
            ids.insert(etype_id, name.to_string());
        }

        // Update delta
        self.delta.write().define_etype(etype_id, name);

        Ok(etype_id)
    }

    /// Define a new property key (writes to WAL for durability)
    pub fn define_propkey(&self, name: &str) -> Result<PropKeyId> {
        let txid = self.require_write_tx()?;

        // Check if already exists
        if let Some(id) = self.get_propkey_id(name) {
            return Ok(id);
        }

        let propkey_id = self.alloc_propkey_id();

        // Write WAL record
        let record = WalRecord::new(
            WalRecordType::DefinePropkey,
            txid,
            build_define_propkey_payload(propkey_id, name),
        );
        self.write_wal(record)?;

        // Update schema maps
        {
            let mut names = self.propkey_names.write();
            let mut ids = self.propkey_ids.write();
            names.insert(name.to_string(), propkey_id);
            ids.insert(propkey_id, name.to_string());
        }

        // Update delta
        self.delta.write().define_propkey(propkey_id, name);

        Ok(propkey_id)
    }

    // ========================================================================
    // WAL Statistics
    // ========================================================================

    /// Get WAL buffer statistics
    pub fn wal_stats(&self) -> crate::core::wal::buffer::WalBufferStats {
        self.wal_buffer.lock().stats()
    }

    // ========================================================================
    // Checkpoint / Compaction
    // ========================================================================

    /// Perform a checkpoint - merge snapshot + delta into new snapshot
    ///
    /// This:
    /// 1. Collects all graph data from snapshot + delta
    /// 2. Builds a new snapshot in memory
    /// 3. Writes the new snapshot to disk (after WAL)
    /// 4. Updates header to point to new snapshot
    /// 5. Clears WAL and delta
    pub fn checkpoint(&self) -> Result<()> {
        if self.read_only {
            return Err(RayError::ReadOnly);
        }

        // Don't checkpoint with active transaction
        if self.has_transaction() {
            return Err(RayError::TransactionInProgress);
        }

        // Collect all graph data
        let (nodes, edges, labels, etypes, propkeys) = self.collect_graph_data();

        // Get current header state
        let header = self.header.read().clone();
        let new_gen = header.active_snapshot_gen + 1;

        // Build new snapshot in memory
        let snapshot_buffer = build_snapshot_to_memory(SnapshotBuildInput {
            generation: new_gen,
            nodes,
            edges,
            labels,
            etypes,
            propkeys,
            compression: None, // TODO: Add compression support
        })?;

        // Calculate where to place new snapshot (after WAL)
        let wal_end_page = header.wal_start_page + header.wal_page_count;
        let new_snapshot_start_page = wal_end_page;
        let new_snapshot_page_count = pages_to_store(snapshot_buffer.len(), header.page_size as usize) as u64;

        // Write snapshot to file
        {
            let mut pager = self.pager.lock();
            self.write_snapshot_pages(&mut pager, new_snapshot_start_page as u32, &snapshot_buffer, header.page_size as usize)?;
        }

        // Update header
        {
            let mut pager = self.pager.lock();
            let mut wal_buffer = self.wal_buffer.lock();
            let mut header = self.header.write();

            // Update header fields
            header.active_snapshot_gen = new_gen;
            header.snapshot_start_page = new_snapshot_start_page;
            header.snapshot_page_count = new_snapshot_page_count;
            header.db_size_pages = new_snapshot_start_page + new_snapshot_page_count;
            header.max_node_id = self.next_node_id.load(Ordering::SeqCst).saturating_sub(1);
            header.next_tx_id = self.next_tx_id.load(Ordering::SeqCst);

            // Reset WAL
            header.wal_head = 0;
            header.wal_tail = 0;
            wal_buffer.reset();

            // Increment change counter
            header.change_counter += 1;

            // Write header to disk
            let header_bytes = header.serialize_to_page();
            pager.write_page(0, &header_bytes)?;
            pager.sync()?;
        }

        // Clear delta
        self.delta.write().clear();

        // Reload the new snapshot
        self.reload_snapshot()?;

        Ok(())
    }

    /// Reload snapshot from disk after checkpoint
    fn reload_snapshot(&self) -> Result<()> {
        let header = self.header.read();
        
        if header.snapshot_page_count == 0 {
            // No snapshot to load
            *self.snapshot.write() = None;
            return Ok(());
        }

        // Calculate snapshot offset in bytes
        let snapshot_offset = (header.snapshot_start_page * header.page_size as u64) as usize;
        
        // Re-mmap the file and parse snapshot
        let pager = self.pager.lock();
        let new_snapshot = SnapshotData::parse_at_offset(
            std::sync::Arc::new(unsafe {
                // Safety: We're creating an owned Mmap from the file
                // This is safe because the pager keeps the file open
                memmap2::Mmap::map(pager.file())?
            }),
            snapshot_offset,
            &crate::core::snapshot::reader::ParseSnapshotOptions::default(),
        )?;

        // Update the snapshot
        *self.snapshot.write() = Some(new_snapshot);

        Ok(())
    }

    // ========================================================================
    // Background Checkpoint (Non-Blocking)
    // ========================================================================

    /// Check if a background checkpoint is currently running
    pub fn is_checkpoint_running(&self) -> bool {
        let status = *self.checkpoint_status.lock();
        matches!(status, CheckpointStatus::Running | CheckpointStatus::Completing)
    }

    /// Get current checkpoint status
    pub fn checkpoint_status(&self) -> CheckpointStatus {
        *self.checkpoint_status.lock()
    }

    /// Trigger a background checkpoint (non-blocking)
    ///
    /// This switches writes to secondary WAL region immediately and starts
    /// the checkpoint process. Writes can continue while checkpoint is running.
    ///
    /// Steps:
    /// 1. Switch writes to secondary WAL region
    /// 2. Set checkpointInProgress flag (for crash recovery)
    /// 3. Build new snapshot from primary WAL + current snapshot + delta
    /// 4. Write new snapshot to disk
    /// 5. Merge secondary into primary, update header
    /// 6. Clear checkpointInProgress flag
    pub fn background_checkpoint(&self) -> Result<()> {
        if self.read_only {
            return Err(RayError::ReadOnly);
        }

        // Check if already running
        {
            let mut status = self.checkpoint_status.lock();
            match *status {
                CheckpointStatus::Running => {
                    // Already running, just return
                    return Ok(());
                }
                CheckpointStatus::Completing => {
                    // Wait for completion by returning
                    return Ok(());
                }
                CheckpointStatus::Idle => {
                    *status = CheckpointStatus::Running;
                }
            }
        }

        // Step 1: Switch writes to secondary region
        {
            let mut pager = self.pager.lock();
            let mut wal_buffer = self.wal_buffer.lock();
            let mut header = self.header.write();

            // Switch WAL to secondary region
            wal_buffer.switch_to_secondary();

            // Update header to reflect the switch
            header.active_wal_region = 1;
            header.checkpoint_in_progress = 1;
            header.wal_primary_head = wal_buffer.primary_head();
            header.wal_secondary_head = wal_buffer.secondary_head();
            header.change_counter += 1;

            // Write header to disk
            let header_bytes = header.serialize_to_page();
            pager.write_page(0, &header_bytes)?;
            pager.sync()?;
        }

        // Step 2-4: Build and write snapshot, get the info
        let snapshot_info = match self.build_and_write_snapshot() {
            Ok(info) => info,
            Err(e) => {
                // On error, try to recover
                self.recover_from_checkpoint_error();
                return Err(e);
            }
        };

        // Step 5: Complete the checkpoint
        self.complete_background_checkpoint(snapshot_info)?;

        Ok(())
    }

    /// Build and write the snapshot (called during background checkpoint)
    /// Returns (new_gen, new_snapshot_start_page, new_snapshot_page_count)
    fn build_and_write_snapshot(&self) -> Result<(u64, u64, u64)> {
        // Collect all graph data (reads from snapshot + delta)
        let (nodes, edges, labels, etypes, propkeys) = self.collect_graph_data();

        // Get current header state
        let header = self.header.read().clone();
        let new_gen = header.active_snapshot_gen + 1;

        // Build new snapshot in memory
        let snapshot_buffer = build_snapshot_to_memory(SnapshotBuildInput {
            generation: new_gen,
            nodes,
            edges,
            labels,
            etypes,
            propkeys,
            compression: None,
        })?;

        // Calculate where to place new snapshot (after WAL)
        let wal_end_page = header.wal_start_page + header.wal_page_count;
        let new_snapshot_start_page = wal_end_page;
        let new_snapshot_page_count = pages_to_store(snapshot_buffer.len(), header.page_size as usize) as u64;

        // Write snapshot to file
        {
            let mut pager = self.pager.lock();
            self.write_snapshot_pages(&mut pager, new_snapshot_start_page as u32, &snapshot_buffer, header.page_size as usize)?;
        }

        Ok((new_gen, new_snapshot_start_page, new_snapshot_page_count))
    }

    /// Complete the background checkpoint
    fn complete_background_checkpoint(&self, snapshot_info: (u64, u64, u64)) -> Result<()> {
        let (new_gen, new_snapshot_start_page, new_snapshot_page_count) = snapshot_info;
        
        // Mark as completing (brief lock period)
        *self.checkpoint_status.lock() = CheckpointStatus::Completing;

        // Merge secondary records into primary and update header
        {
            let mut pager = self.pager.lock();
            let mut wal_buffer = self.wal_buffer.lock();
            let mut header = self.header.write();

            // Merge secondary WAL records into primary
            wal_buffer.merge_secondary_into_primary(&mut pager)?;
            wal_buffer.flush(&mut pager)?;

            // Update header with new snapshot location
            header.active_snapshot_gen = new_gen;
            header.snapshot_start_page = new_snapshot_start_page;
            header.snapshot_page_count = new_snapshot_page_count;
            header.db_size_pages = new_snapshot_start_page + new_snapshot_page_count;
            header.max_node_id = self.next_node_id.load(Ordering::SeqCst).saturating_sub(1);
            header.next_tx_id = self.next_tx_id.load(Ordering::SeqCst);

            // Update WAL state
            header.wal_head = wal_buffer.head();
            header.wal_tail = wal_buffer.tail();
            header.wal_primary_head = wal_buffer.primary_head();
            header.wal_secondary_head = wal_buffer.secondary_head();
            header.active_wal_region = 0;
            header.checkpoint_in_progress = 0;
            header.change_counter += 1;

            // Write header to disk
            let header_bytes = header.serialize_to_page();
            pager.write_page(0, &header_bytes)?;
            pager.sync()?;
        }

        // Clear delta
        self.delta.write().clear();

        // Reload the new snapshot
        self.reload_snapshot()?;

        // Mark as idle
        *self.checkpoint_status.lock() = CheckpointStatus::Idle;

        Ok(())
    }

    /// Recover from a checkpoint error
    fn recover_from_checkpoint_error(&self) {
        // Try to switch back to primary region and clear the checkpoint flag
        if let Some(mut pager) = self.pager.try_lock() {
            if let Some(mut wal_buffer) = self.wal_buffer.try_lock() {
                if let Some(mut header) = self.header.try_write() {
                    // Switch back to primary
                    wal_buffer.switch_to_primary(false);

                    // Clear checkpoint flag
                    header.active_wal_region = 0;
                    header.checkpoint_in_progress = 0;

                    // Try to write header
                    let header_bytes = header.serialize_to_page();
                    let _ = pager.write_page(0, &header_bytes);
                    let _ = pager.sync();
                }
            }
        }

        // Mark as idle
        *self.checkpoint_status.lock() = CheckpointStatus::Idle;
    }

    /// Write snapshot buffer to file pages
    fn write_snapshot_pages(
        &self,
        pager: &mut FilePager,
        start_page: u32,
        buffer: &[u8],
        page_size: usize,
    ) -> Result<()> {
        let num_pages = pages_to_store(buffer.len(), page_size);

        // Ensure file is large enough
        let required_pages = start_page + num_pages;
        let current_pages = (pager.file_size() as usize + page_size - 1) / page_size;

        if required_pages as usize > current_pages {
            pager.allocate_pages(required_pages - current_pages as u32)?;
        }

        // Write pages
        for i in 0..num_pages {
            let mut page_data = vec![0u8; page_size];
            let src_offset = i as usize * page_size;
            let src_end = std::cmp::min(src_offset + page_size, buffer.len());
            page_data[..src_end - src_offset].copy_from_slice(&buffer[src_offset..src_end]);
            pager.write_page(start_page + i, &page_data)?;
        }

        // Sync to disk
        pager.sync()?;

        Ok(())
    }

    /// Collect all graph data from snapshot + delta
    fn collect_graph_data(&self) -> (
        Vec<NodeData>,
        Vec<EdgeData>,
        HashMap<LabelId, String>,
        HashMap<ETypeId, String>,
        HashMap<PropKeyId, String>,
    ) {
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let mut labels = HashMap::new();
        let mut etypes = HashMap::new();
        let mut propkeys = HashMap::new();

        let delta = self.delta.read();

        // First, copy schema from our in-memory maps
        for (&id, name) in self.label_ids.read().iter() {
            labels.insert(id, name.clone());
        }
        for (&id, name) in self.etype_ids.read().iter() {
            etypes.insert(id, name.clone());
        }
        for (&id, name) in self.propkey_ids.read().iter() {
            propkeys.insert(id, name.clone());
        }

        // Collect nodes from snapshot
        if let Some(ref snapshot) = *self.snapshot.read() {
            let num_nodes = snapshot.header.num_nodes as usize;

            for phys in 0..num_nodes {
                let node_id = match snapshot.get_node_id(phys as u32) {
                    Some(id) => id,
                    None => continue,
                };

                // Skip deleted nodes
                if delta.is_node_deleted(node_id) {
                    continue;
                }

                // Get key
                let key = snapshot.get_node_key(phys as u32);

                // Get properties from snapshot
                let mut props = HashMap::new();
                if let Some(snapshot_props) = snapshot.get_node_props(phys as u32) {
                    for (key_id, value) in snapshot_props {
                        props.insert(key_id, value);
                    }
                }

                // Apply delta modifications
                if let Some(node_delta) = delta.get_node_delta(node_id) {
                    if let Some(ref delta_props) = node_delta.props {
                        for (&key_id, value) in delta_props {
                            match value {
                                Some(v) => { props.insert(key_id, v.clone()); }
                                None => { props.remove(&key_id); }
                            }
                        }
                    }
                }

                // Collect node labels (simplified - labels handled differently in real impl)
                let node_labels = Vec::new();

                nodes.push(NodeData {
                    node_id,
                    key,
                    labels: node_labels,
                    props,
                });

                // Collect edges from this node
                for edge_info in snapshot.get_out_edges(phys as u32) {
                    let dst_node_id = match snapshot.get_node_id(edge_info.dst) {
                        Some(id) => id,
                        None => continue,
                    };

                    // Skip edges to deleted nodes
                    if delta.is_node_deleted(dst_node_id) {
                        continue;
                    }

                    // Skip deleted edges
                    if delta.is_edge_deleted(node_id, edge_info.etype, dst_node_id) {
                        continue;
                    }

                    // Get edge props (simplified)
                    let edge_props = HashMap::new();

                    edges.push(EdgeData {
                        src: node_id,
                        etype: edge_info.etype,
                        dst: dst_node_id,
                        props: edge_props,
                    });
                }
            }
        }

        // Add nodes created in delta
        for (&node_id, node_delta) in &delta.created_nodes {
            let mut props = HashMap::new();
            if let Some(ref delta_props) = node_delta.props {
                for (&key_id, value) in delta_props {
                    if let Some(v) = value {
                        props.insert(key_id, v.clone());
                    }
                }
            }

            nodes.push(NodeData {
                node_id,
                key: node_delta.key.clone(),
                labels: Vec::new(),
                props,
            });
        }

        // Add edges from delta
        for (&src, patches) in &delta.out_add {
            // Skip edges from deleted nodes
            if delta.is_node_deleted(src) {
                continue;
            }

            for patch in patches {
                // Skip edges to deleted nodes
                if delta.is_node_deleted(patch.other) {
                    continue;
                }

                edges.push(EdgeData {
                    src,
                    etype: patch.etype,
                    dst: patch.other,
                    props: HashMap::new(),
                });
            }
        }

        (nodes, edges, labels, etypes, propkeys)
    }

    /// Check if checkpoint is recommended based on WAL usage
    pub fn should_checkpoint(&self, threshold: f64) -> bool {
        let stats = self.wal_stats();
        stats.used as f64 / stats.capacity as f64 >= threshold
    }

    // ========================================================================
    // Query / Read Operations
    // ========================================================================

    /// Get all properties for a node
    /// 
    /// Returns None if the node doesn't exist or is deleted.
    /// Merges properties from snapshot with delta modifications.
    pub fn get_node_props(&self, node_id: NodeId) -> Option<HashMap<PropKeyId, PropValue>> {
        let delta = self.delta.read();

        // Check if node is deleted
        if delta.is_node_deleted(node_id) {
            return None;
        }

        let mut props = HashMap::new();
        let snapshot = self.snapshot.read();

        // Get properties from snapshot first
        if let Some(ref snap) = *snapshot {
            if let Some(phys) = snap.get_phys_node(node_id) {
                if let Some(snapshot_props) = snap.get_node_props(phys) {
                    props = snapshot_props;
                }
            }
        }

        // Apply delta modifications
        if let Some(node_delta) = delta.get_node_delta(node_id) {
            if let Some(ref delta_props) = node_delta.props {
                for (&key_id, value) in delta_props {
                    match value {
                        Some(v) => { props.insert(key_id, v.clone()); }
                        None => { props.remove(&key_id); }
                    }
                }
            }
        }

        // Check if node exists at all
        let node_exists_in_delta = delta.is_node_created(node_id) 
            || delta.get_node_delta(node_id).is_some();
        
        if !node_exists_in_delta {
            if let Some(ref snap) = *snapshot {
                if snap.get_phys_node(node_id).is_none() {
                    return None;
                }
            } else {
                // No snapshot and node not in delta
                return None;
            }
        }

        Some(props)
    }

    /// Get a specific property for a node
    /// 
    /// Returns None if the node doesn't exist, is deleted, or doesn't have the property.
    pub fn get_node_prop(&self, node_id: NodeId, key_id: PropKeyId) -> Option<PropValue> {
        let delta = self.delta.read();

        // Check if node is deleted
        if delta.is_node_deleted(node_id) {
            return None;
        }

        // Check delta first (for modifications)
        if let Some(node_delta) = delta.get_node_delta(node_id) {
            if let Some(ref delta_props) = node_delta.props {
                if let Some(value) = delta_props.get(&key_id) {
                    // None means explicitly deleted
                    return value.clone();
                }
            }
        }

        // Fall back to snapshot
        let snapshot = self.snapshot.read();
        if let Some(ref snap) = *snapshot {
            if let Some(phys) = snap.get_phys_node(node_id) {
                return snap.get_node_prop(phys, key_id);
            }
        }

        // Check if node exists at all (in delta as created)
        if delta.is_node_created(node_id) {
            // Node exists but doesn't have this property
            return None;
        }

        None
    }

    /// Get outgoing edges for a node
    /// 
    /// Returns edges as (edge_type_id, destination_node_id) pairs.
    /// Merges edges from snapshot with delta additions/deletions.
    /// Filters out edges to deleted nodes.
    pub fn get_out_edges(&self, node_id: NodeId) -> Vec<(ETypeId, NodeId)> {
        let delta = self.delta.read();

        // If node is deleted, no edges
        if delta.is_node_deleted(node_id) {
            return Vec::new();
        }

        let mut edges = Vec::new();
        let snapshot = self.snapshot.read();

        // Get edges from snapshot
        if let Some(ref snap) = *snapshot {
            if let Some(phys) = snap.get_phys_node(node_id) {
                for (dst_phys, etype) in snap.iter_out_edges(phys) {
                    // Convert physical dst to NodeId
                    if let Some(dst_node_id) = snap.get_node_id(dst_phys) {
                        // Skip edges to deleted nodes
                        if delta.is_node_deleted(dst_node_id) {
                            continue;
                        }
                        // Skip edges deleted in delta
                        if delta.is_edge_deleted(node_id, etype, dst_node_id) {
                            continue;
                        }
                        edges.push((etype, dst_node_id));
                    }
                }
            }
        }

        // Add edges from delta
        if let Some(added_edges) = delta.out_add.get(&node_id) {
            for edge_patch in added_edges {
                // Skip edges to deleted nodes
                if delta.is_node_deleted(edge_patch.other) {
                    continue;
                }
                edges.push((edge_patch.etype, edge_patch.other));
            }
        }

        // Sort by (etype, dst) for consistent ordering
        edges.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

        edges
    }

    /// Get incoming edges for a node
    /// 
    /// Returns edges as (edge_type_id, source_node_id) pairs.
    /// Merges edges from snapshot with delta additions/deletions.
    /// Filters out edges from deleted nodes.
    pub fn get_in_edges(&self, node_id: NodeId) -> Vec<(ETypeId, NodeId)> {
        let delta = self.delta.read();

        // If node is deleted, no edges
        if delta.is_node_deleted(node_id) {
            return Vec::new();
        }

        let mut edges = Vec::new();
        let snapshot = self.snapshot.read();

        // Get edges from snapshot
        if let Some(ref snap) = *snapshot {
            if let Some(phys) = snap.get_phys_node(node_id) {
                for (src_phys, etype, _out_index) in snap.iter_in_edges(phys) {
                    // Convert physical src to NodeId
                    if let Some(src_node_id) = snap.get_node_id(src_phys) {
                        // Skip edges from deleted nodes
                        if delta.is_node_deleted(src_node_id) {
                            continue;
                        }
                        // Skip edges deleted in delta
                        if delta.is_edge_deleted(src_node_id, etype, node_id) {
                            continue;
                        }
                        edges.push((etype, src_node_id));
                    }
                }
            }
        }

        // Add edges from delta (in_add stores patches where other=src)
        if let Some(added_edges) = delta.in_add.get(&node_id) {
            for edge_patch in added_edges {
                // Skip edges from deleted nodes
                if delta.is_node_deleted(edge_patch.other) {
                    continue;
                }
                edges.push((edge_patch.etype, edge_patch.other));
            }
        }

        // Sort by (etype, src) for consistent ordering
        edges.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

        edges
    }

    /// Get out-degree (number of outgoing edges) for a node
    pub fn get_out_degree(&self, node_id: NodeId) -> usize {
        self.get_out_edges(node_id).len()
    }

    /// Get in-degree (number of incoming edges) for a node
    pub fn get_in_degree(&self, node_id: NodeId) -> usize {
        self.get_in_edges(node_id).len()
    }

    /// Look up a node by its key
    /// 
    /// Returns the NodeId if found, None otherwise.
    /// Checks delta key index first, then falls back to snapshot.
    pub fn get_node_by_key(&self, key: &str) -> Option<NodeId> {
        let delta = self.delta.read();

        // Check delta key index first
        if delta.key_index_deleted.contains(key) {
            return None;
        }

        if let Some(&node_id) = delta.key_index.get(key) {
            // Verify node isn't deleted
            if !delta.is_node_deleted(node_id) {
                return Some(node_id);
            }
        }

        // Fall back to snapshot
        let snapshot = self.snapshot.read();
        if let Some(ref snap) = *snapshot {
            if let Some(node_id) = snap.lookup_by_key(key) {
                // Verify node isn't deleted in delta
                if !delta.is_node_deleted(node_id) {
                    return Some(node_id);
                }
            }
        }

        None
    }

    /// Get the key for a node
    /// 
    /// Returns the key string if the node has one, None otherwise.
    pub fn get_node_key(&self, node_id: NodeId) -> Option<String> {
        let delta = self.delta.read();

        // Check if node is deleted
        if delta.is_node_deleted(node_id) {
            return None;
        }

        // Check created nodes in delta first
        if let Some(node_delta) = delta.created_nodes.get(&node_id) {
            return node_delta.key.clone();
        }

        // Fall back to snapshot
        let snapshot = self.snapshot.read();
        if let Some(ref snap) = *snapshot {
            if let Some(phys) = snap.get_phys_node(node_id) {
                return snap.get_node_key(phys);
            }
        }

        None
    }

    /// Get neighbors via outgoing edges of a specific type
    /// 
    /// Returns destination node IDs for edges of the given type.
    pub fn get_out_neighbors(&self, node_id: NodeId, etype: ETypeId) -> Vec<NodeId> {
        self.get_out_edges(node_id)
            .into_iter()
            .filter(|(e, _)| *e == etype)
            .map(|(_, dst)| dst)
            .collect()
    }

    /// Get neighbors via incoming edges of a specific type
    /// 
    /// Returns source node IDs for edges of the given type.
    pub fn get_in_neighbors(&self, node_id: NodeId, etype: ETypeId) -> Vec<NodeId> {
        self.get_in_edges(node_id)
            .into_iter()
            .filter(|(e, _)| *e == etype)
            .map(|(_, src)| src)
            .collect()
    }

    /// Check if there are any outgoing edges of a specific type
    pub fn has_out_edges(&self, node_id: NodeId, etype: ETypeId) -> bool {
        self.get_out_edges(node_id)
            .iter()
            .any(|(e, _)| *e == etype)
    }

    /// Check if there are any incoming edges of a specific type
    pub fn has_in_edges(&self, node_id: NodeId, etype: ETypeId) -> bool {
        self.get_in_edges(node_id)
            .iter()
            .any(|(e, _)| *e == etype)
    }
}

// ============================================================================
// Utility Functions
// ============================================================================

/// Check if a path is a single-file database
pub fn is_single_file_path<P: AsRef<Path>>(path: P) -> bool {
    path.as_ref()
        .extension()
        .map(|ext| ext == "raydb")
        .unwrap_or(false)
}

/// Get the single-file extension
pub fn single_file_extension() -> &'static str {
    EXT_RAYDB
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::NamedTempFile;

    #[test]
    fn test_open_new_single_file_db() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();

        assert!(!db.read_only);
        assert_eq!(db.header.read().page_size, DEFAULT_PAGE_SIZE as u32);

        close_single_file(db).unwrap();

        // Clean up
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_reopen_single_file_db() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        // Create database
        {
            let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();
            close_single_file(db).unwrap();
        }

        // Reopen database
        {
            let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();
            assert!(!db.read_only);
            close_single_file(db).unwrap();
        }

        // Clean up
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_id_allocation() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();

        let id1 = db.alloc_node_id();
        let id2 = db.alloc_node_id();
        assert_eq!(id2, id1 + 1);

        let label1 = db.alloc_label_id();
        let label2 = db.alloc_label_id();
        assert_eq!(label2, label1 + 1);

        close_single_file(db).unwrap();

        // Clean up
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_schema_operations() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();

        // Create label
        let label_id = db.get_or_create_label("Person");
        assert!(label_id >= INITIAL_LABEL_ID);

        // Should return same ID
        let label_id2 = db.get_or_create_label("Person");
        assert_eq!(label_id, label_id2);

        // Lookup
        assert_eq!(db.get_label_id("Person"), Some(label_id));
        assert_eq!(db.get_label_name(label_id), Some("Person".to_string()));
        assert_eq!(db.get_label_id("Unknown"), None);

        close_single_file(db).unwrap();

        // Clean up
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_is_single_file_path() {
        assert!(is_single_file_path("test.raydb"));
        assert!(is_single_file_path("/path/to/db.raydb"));
        assert!(!is_single_file_path("test.db"));
        assert!(!is_single_file_path("/path/to/directory"));
    }

    #[test]
    fn test_custom_options() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        let options = SingleFileOpenOptions::new()
            .page_size(8192)
            .wal_size(2 * 1024 * 1024);

        let db = open_single_file(&path, options).unwrap();

        assert_eq!(db.header.read().page_size, 8192);

        close_single_file(db).unwrap();

        // Clean up
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_transaction_begin_commit() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();

        // Begin transaction
        let txid = db.begin(false).unwrap();
        assert!(txid > 0);
        assert!(db.has_transaction());

        // Commit transaction
        db.commit().unwrap();
        assert!(!db.has_transaction());

        close_single_file(db).unwrap();
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_transaction_begin_rollback() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();

        // Begin transaction
        db.begin(false).unwrap();
        assert!(db.has_transaction());

        // Rollback transaction
        db.rollback().unwrap();
        assert!(!db.has_transaction());

        close_single_file(db).unwrap();
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_create_node_in_transaction() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();

        // Begin transaction
        db.begin(false).unwrap();

        // Create node
        let node_id = db.create_node(Some("user:1")).unwrap();
        assert!(node_id >= INITIAL_NODE_ID);

        // Node should exist
        assert!(db.node_exists(node_id));

        // Commit
        db.commit().unwrap();

        // Node should still exist after commit
        assert!(db.node_exists(node_id));

        close_single_file(db).unwrap();
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_add_edge_in_transaction() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();

        db.begin(false).unwrap();

        // Create nodes
        let node1 = db.create_node(None).unwrap();
        let node2 = db.create_node(None).unwrap();

        // Add edge
        let etype = db.get_or_create_etype("KNOWS");
        db.add_edge(node1, etype, node2).unwrap();

        // Edge should exist
        assert!(db.edge_exists(node1, etype, node2));

        db.commit().unwrap();

        close_single_file(db).unwrap();
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_set_node_prop_in_transaction() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();

        db.begin(false).unwrap();

        let node_id = db.create_node(None).unwrap();
        
        // Set property
        db.set_node_prop_by_name(node_id, "name", PropValue::String("Alice".to_string())).unwrap();

        db.commit().unwrap();

        close_single_file(db).unwrap();
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_no_write_without_transaction() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();

        // Try to create node without transaction
        let result = db.create_node(None);
        assert!(result.is_err());

        close_single_file(db).unwrap();
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_no_write_in_readonly_transaction() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();

        // Begin read-only transaction
        db.begin(true).unwrap();

        // Try to create node
        let result = db.create_node(None);
        assert!(result.is_err());

        db.rollback().unwrap();

        close_single_file(db).unwrap();
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_wal_persistence() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");
        let node_id;

        // Create database and write some data
        {
            let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();
            db.begin(false).unwrap();
            node_id = db.create_node(Some("test:key")).unwrap();
            db.commit().unwrap();
            close_single_file(db).unwrap();
        }

        // Reopen and verify data persisted
        {
            let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();
            // After replay, node should exist
            assert!(db.node_exists(node_id));
            close_single_file(db).unwrap();
        }

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_checkpoint() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");
        let node1;
        let node2;
        let etype;

        // Create database and write data
        {
            let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();

            // Create some nodes and edges
            db.begin(false).unwrap();
            node1 = db.create_node(Some("user:alice")).unwrap();
            node2 = db.create_node(Some("user:bob")).unwrap();
            etype = db.get_or_create_etype("KNOWS");
            db.add_edge(node1, etype, node2).unwrap();
            db.set_node_prop_by_name(node1, "name", PropValue::String("Alice".to_string())).unwrap();
            db.commit().unwrap();

            // Check WAL has data
            let stats_before = db.wal_stats();
            assert!(stats_before.used > 0);

            // Checkpoint
            db.checkpoint().unwrap();

            // After checkpoint, WAL should be cleared
            let stats_after = db.wal_stats();
            assert_eq!(stats_after.head, 0);
            assert_eq!(stats_after.tail, 0);

            // Snapshot should have data
            assert!(db.header.read().snapshot_page_count > 0);
            assert_eq!(db.header.read().active_snapshot_gen, 1);

            close_single_file(db).unwrap();
        }

        // Reopen and verify data is in snapshot (not just WAL)
        {
            let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();
            
            // WAL should be empty (data is in snapshot)
            assert_eq!(db.header.read().wal_head, 0);
            assert_eq!(db.header.read().wal_tail, 0);
            
            // Snapshot should exist
            assert!(db.header.read().snapshot_page_count > 0);
            assert!(db.snapshot.read().is_some());
            
            // Data should be accessible from snapshot
            assert!(db.node_exists(node1));
            assert!(db.node_exists(node2));
            assert!(db.edge_exists(node1, etype, node2));

            close_single_file(db).unwrap();
        }

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_should_checkpoint() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        // Create database with small WAL
        let options = SingleFileOpenOptions::new()
            .wal_size(16 * 1024); // 16KB WAL

        let db = open_single_file(&path, options).unwrap();

        // Initially shouldn't need checkpoint
        assert!(!db.should_checkpoint(0.8));

        // Write a bunch of data
        for i in 0..50 {
            db.begin(false).unwrap();
            db.create_node(Some(&format!("node:{}", i))).unwrap();
            db.commit().unwrap();
        }

        // Now might need checkpoint (depending on WAL size)
        // With very small WAL, this should trigger
        let usage = db.wal_stats().used as f64 / db.wal_stats().capacity as f64;
        if usage >= 0.5 {
            assert!(db.should_checkpoint(0.5));
        }

        close_single_file(db).unwrap();
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_auto_checkpoint() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        // Create database with small WAL and auto-checkpoint enabled
        // Use very small WAL (8KB) and low threshold (30%) to trigger checkpoint quickly
        let options = SingleFileOpenOptions::new()
            .wal_size(8 * 1024)        // 8KB WAL (very small)
            .auto_checkpoint(true)
            .checkpoint_threshold(0.3); // 30% threshold

        let db = open_single_file(&path, options).unwrap();

        // Initially, no snapshot
        assert_eq!(db.header.read().snapshot_page_count, 0);
        assert_eq!(db.header.read().active_snapshot_gen, 0);

        // Write enough data to trigger auto-checkpoint
        // Each node write + commit is ~50-100 bytes, so 30 nodes should fill ~3KB
        let mut node_ids = Vec::new();
        for i in 0..30 {
            db.begin(false).unwrap();
            let node_id = db.create_node(Some(&format!("user:{}", i))).unwrap();
            node_ids.push(node_id);
            db.commit().unwrap();
            
            // Check if checkpoint happened
            if db.header.read().active_snapshot_gen >= 1 {
                break;
            }
        }

        // Auto-checkpoint should have been triggered
        let header = db.header.read();
        
        // At least one checkpoint should have happened
        assert!(header.active_snapshot_gen >= 1, 
            "Expected at least one checkpoint, got gen {}", header.active_snapshot_gen);
        
        // Snapshot should exist
        assert!(header.snapshot_page_count > 0,
            "Expected snapshot_page_count > 0, got {}", header.snapshot_page_count);

        drop(header);

        // All nodes should still exist
        for &node_id in &node_ids {
            assert!(db.node_exists(node_id), "Node {} should exist", node_id);
        }

        close_single_file(db).unwrap();

        // Reopen and verify data persisted correctly
        {
            let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();
            
            // All nodes should still exist after reopen
            for &node_id in &node_ids {
                assert!(db.node_exists(node_id), "Node {} should exist after reopen", node_id);
            }
            
            close_single_file(db).unwrap();
        }

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_auto_checkpoint_disabled_by_default() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        // Create database with default options (auto-checkpoint disabled)
        let options = SingleFileOpenOptions::new()
            .wal_size(16 * 1024); // 16KB WAL

        let db = open_single_file(&path, options).unwrap();

        // Write a bunch of data
        for i in 0..100 {
            db.begin(false).unwrap();
            db.create_node(Some(&format!("node:{}", i))).unwrap();
            db.commit().unwrap();
        }

        // Auto-checkpoint should NOT have happened (disabled by default)
        // WAL should still have data
        let stats = db.wal_stats();
        assert!(stats.used > 0, "WAL should have data since auto-checkpoint is disabled");
        
        // No snapshot should have been created
        assert_eq!(db.header.read().active_snapshot_gen, 0,
            "No checkpoint should have occurred with auto-checkpoint disabled");

        close_single_file(db).unwrap();
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_checkpoint_reloads_snapshot() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();

        // Create nodes
        db.begin(false).unwrap();
        let node1 = db.create_node(Some("alice")).unwrap();
        let node2 = db.create_node(Some("bob")).unwrap();
        let etype = db.get_or_create_etype("KNOWS");
        db.add_edge(node1, etype, node2).unwrap();
        db.commit().unwrap();

        // Verify nodes exist (from delta)
        assert!(db.node_exists(node1));
        assert!(db.node_exists(node2));
        assert!(db.edge_exists(node1, etype, node2));

        // Checkpoint
        db.checkpoint().unwrap();

        // After checkpoint, delta should be cleared and snapshot should be loaded
        // Nodes should still exist (now from snapshot)
        assert!(db.node_exists(node1), "Node1 should exist after checkpoint");
        assert!(db.node_exists(node2), "Node2 should exist after checkpoint");
        assert!(db.edge_exists(node1, etype, node2), "Edge should exist after checkpoint");

        // Snapshot should be loaded
        assert!(db.snapshot.read().is_some(), "Snapshot should be loaded after checkpoint");

        // Can continue to write after checkpoint
        db.begin(false).unwrap();
        let node3 = db.create_node(Some("charlie")).unwrap();
        db.add_edge(node2, etype, node3).unwrap();
        db.commit().unwrap();

        // All nodes should exist
        assert!(db.node_exists(node1));
        assert!(db.node_exists(node2));
        assert!(db.node_exists(node3));
        assert!(db.edge_exists(node1, etype, node2));
        assert!(db.edge_exists(node2, etype, node3));

        close_single_file(db).unwrap();
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_background_checkpoint() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();

        // Verify initial state
        assert_eq!(db.checkpoint_status(), CheckpointStatus::Idle);
        assert!(!db.is_checkpoint_running());

        // Create initial data
        db.begin(false).unwrap();
        let node1 = db.create_node(Some("alice")).unwrap();
        let node2 = db.create_node(Some("bob")).unwrap();
        let etype = db.get_or_create_etype("KNOWS");
        db.add_edge(node1, etype, node2).unwrap();
        db.commit().unwrap();

        // Verify data exists
        assert!(db.node_exists(node1));
        assert!(db.node_exists(node2));
        assert!(db.edge_exists(node1, etype, node2));

        // Check WAL has data
        let stats_before = db.wal_stats();
        assert!(stats_before.used > 0);
        assert_eq!(db.header.read().active_snapshot_gen, 0);

        // Perform background checkpoint
        db.background_checkpoint().unwrap();

        // After checkpoint, verify state
        assert_eq!(db.checkpoint_status(), CheckpointStatus::Idle);
        assert!(!db.is_checkpoint_running());

        // Snapshot should exist
        assert!(db.header.read().snapshot_page_count > 0);
        assert_eq!(db.header.read().active_snapshot_gen, 1);

        // WAL should be cleared (or only have merged secondary records)
        let stats_after = db.wal_stats();
        assert_eq!(stats_after.primary_head, 0);

        // Data should still be accessible
        assert!(db.node_exists(node1));
        assert!(db.node_exists(node2));
        assert!(db.edge_exists(node1, etype, node2));

        // Can write more data after checkpoint
        db.begin(false).unwrap();
        let node3 = db.create_node(Some("charlie")).unwrap();
        db.add_edge(node2, etype, node3).unwrap();
        db.commit().unwrap();

        // All data should exist
        assert!(db.node_exists(node1));
        assert!(db.node_exists(node2));
        assert!(db.node_exists(node3));
        assert!(db.edge_exists(node1, etype, node2));
        assert!(db.edge_exists(node2, etype, node3));

        close_single_file(db).unwrap();

        // Reopen and verify persistence
        {
            let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();
            assert!(db.node_exists(node1));
            assert!(db.node_exists(node2));
            assert!(db.node_exists(node3));
            assert!(db.edge_exists(node1, etype, node2));
            assert!(db.edge_exists(node2, etype, node3));
            close_single_file(db).unwrap();
        }

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_multiple_background_checkpoints() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();

        // First batch of data
        db.begin(false).unwrap();
        let node1 = db.create_node(Some("node1")).unwrap();
        db.commit().unwrap();

        // First background checkpoint
        db.background_checkpoint().unwrap();
        assert_eq!(db.header.read().active_snapshot_gen, 1);

        // Second batch of data
        db.begin(false).unwrap();
        let node2 = db.create_node(Some("node2")).unwrap();
        let etype = db.get_or_create_etype("KNOWS");
        db.add_edge(node1, etype, node2).unwrap();
        db.commit().unwrap();

        // Second background checkpoint
        db.background_checkpoint().unwrap();
        assert_eq!(db.header.read().active_snapshot_gen, 2);

        // Third batch of data
        db.begin(false).unwrap();
        let node3 = db.create_node(Some("node3")).unwrap();
        db.add_edge(node2, etype, node3).unwrap();
        db.commit().unwrap();

        // Third background checkpoint
        db.background_checkpoint().unwrap();
        assert_eq!(db.header.read().active_snapshot_gen, 3);

        // All data should be accessible
        assert!(db.node_exists(node1));
        assert!(db.node_exists(node2));
        assert!(db.node_exists(node3));
        assert!(db.edge_exists(node1, etype, node2));
        assert!(db.edge_exists(node2, etype, node3));

        close_single_file(db).unwrap();

        // Verify persistence
        {
            let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();
            assert!(db.node_exists(node1));
            assert!(db.node_exists(node2));
            assert!(db.node_exists(node3));
            assert!(db.edge_exists(node1, etype, node2));
            assert!(db.edge_exists(node2, etype, node3));
            assert_eq!(db.header.read().active_snapshot_gen, 3);
            close_single_file(db).unwrap();
        }

        let _ = fs::remove_file(&path);
    }

    // ========================================================================
    // Query / Read Operation Tests
    // ========================================================================

    #[test]
    fn test_get_node_props_from_delta() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();

        db.begin(false).unwrap();
        let node_id = db.create_node(None).unwrap();
        db.set_node_prop_by_name(node_id, "name", PropValue::String("Alice".to_string())).unwrap();
        db.set_node_prop_by_name(node_id, "age", PropValue::I64(30)).unwrap();
        db.commit().unwrap();

        // Get props (from delta, no checkpoint yet)
        let props = db.get_node_props(node_id).unwrap();
        assert_eq!(props.len(), 2);
        
        let name_key = db.get_propkey_id("name").unwrap();
        let age_key = db.get_propkey_id("age").unwrap();
        
        assert_eq!(props.get(&name_key), Some(&PropValue::String("Alice".to_string())));
        assert_eq!(props.get(&age_key), Some(&PropValue::I64(30)));

        close_single_file(db).unwrap();
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_get_node_props_after_checkpoint() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();

        db.begin(false).unwrap();
        let node_id = db.create_node(None).unwrap();
        db.set_node_prop_by_name(node_id, "name", PropValue::String("Bob".to_string())).unwrap();
        db.commit().unwrap();

        // Checkpoint to move data to snapshot
        db.checkpoint().unwrap();

        // Get props (from snapshot now)
        let props = db.get_node_props(node_id).unwrap();
        let name_key = db.get_propkey_id("name").unwrap();
        assert_eq!(props.get(&name_key), Some(&PropValue::String("Bob".to_string())));

        close_single_file(db).unwrap();
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_get_node_props_merge_snapshot_and_delta() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();

        // Create node with initial props
        db.begin(false).unwrap();
        let node_id = db.create_node(None).unwrap();
        db.set_node_prop_by_name(node_id, "name", PropValue::String("Charlie".to_string())).unwrap();
        db.set_node_prop_by_name(node_id, "age", PropValue::I64(25)).unwrap();
        db.commit().unwrap();

        // Checkpoint
        db.checkpoint().unwrap();

        // Modify one prop in new transaction
        db.begin(false).unwrap();
        db.set_node_prop_by_name(node_id, "age", PropValue::I64(26)).unwrap();
        db.set_node_prop_by_name(node_id, "city", PropValue::String("NYC".to_string())).unwrap();
        db.commit().unwrap();

        // Props should merge snapshot + delta
        let props = db.get_node_props(node_id).unwrap();
        let name_key = db.get_propkey_id("name").unwrap();
        let age_key = db.get_propkey_id("age").unwrap();
        let city_key = db.get_propkey_id("city").unwrap();

        assert_eq!(props.get(&name_key), Some(&PropValue::String("Charlie".to_string()))); // from snapshot
        assert_eq!(props.get(&age_key), Some(&PropValue::I64(26))); // overwritten in delta
        assert_eq!(props.get(&city_key), Some(&PropValue::String("NYC".to_string()))); // new in delta

        close_single_file(db).unwrap();
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_get_node_props_deleted_node() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();

        db.begin(false).unwrap();
        let node_id = db.create_node(None).unwrap();
        db.set_node_prop_by_name(node_id, "name", PropValue::String("DeleteMe".to_string())).unwrap();
        db.commit().unwrap();

        db.checkpoint().unwrap();

        // Delete the node
        db.begin(false).unwrap();
        db.delete_node(node_id).unwrap();
        db.commit().unwrap();

        // Props should be None for deleted node
        assert!(db.get_node_props(node_id).is_none());

        close_single_file(db).unwrap();
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_get_node_prop_single() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();

        db.begin(false).unwrap();
        let node_id = db.create_node(None).unwrap();
        db.set_node_prop_by_name(node_id, "x", PropValue::I64(42)).unwrap();
        db.commit().unwrap();

        let x_key = db.get_propkey_id("x").unwrap();
        assert_eq!(db.get_node_prop(node_id, x_key), Some(PropValue::I64(42)));
        
        // Non-existent prop
        let y_key = db.get_or_create_propkey("y");
        assert_eq!(db.get_node_prop(node_id, y_key), None);

        close_single_file(db).unwrap();
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_get_out_edges_from_delta() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();

        db.begin(false).unwrap();
        let node1 = db.create_node(None).unwrap();
        let node2 = db.create_node(None).unwrap();
        let node3 = db.create_node(None).unwrap();
        let knows = db.get_or_create_etype("KNOWS");
        let likes = db.get_or_create_etype("LIKES");
        db.add_edge(node1, knows, node2).unwrap();
        db.add_edge(node1, likes, node3).unwrap();
        db.commit().unwrap();

        let out_edges = db.get_out_edges(node1);
        assert_eq!(out_edges.len(), 2);
        assert!(out_edges.contains(&(knows, node2)));
        assert!(out_edges.contains(&(likes, node3)));

        // node2 and node3 should have no outgoing edges
        assert!(db.get_out_edges(node2).is_empty());
        assert!(db.get_out_edges(node3).is_empty());

        close_single_file(db).unwrap();
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_get_in_edges_from_delta() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();

        db.begin(false).unwrap();
        let node1 = db.create_node(None).unwrap();
        let node2 = db.create_node(None).unwrap();
        let node3 = db.create_node(None).unwrap();
        let knows = db.get_or_create_etype("KNOWS");
        db.add_edge(node1, knows, node3).unwrap();
        db.add_edge(node2, knows, node3).unwrap();
        db.commit().unwrap();

        let in_edges = db.get_in_edges(node3);
        assert_eq!(in_edges.len(), 2);
        assert!(in_edges.contains(&(knows, node1)));
        assert!(in_edges.contains(&(knows, node2)));

        // node1 and node2 should have no incoming edges
        assert!(db.get_in_edges(node1).is_empty());
        assert!(db.get_in_edges(node2).is_empty());

        close_single_file(db).unwrap();
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_get_edges_after_checkpoint() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();

        db.begin(false).unwrap();
        let node1 = db.create_node(None).unwrap();
        let node2 = db.create_node(None).unwrap();
        let knows = db.get_or_create_etype("KNOWS");
        db.add_edge(node1, knows, node2).unwrap();
        db.commit().unwrap();

        // Checkpoint to move to snapshot
        db.checkpoint().unwrap();

        // Edges should still be accessible from snapshot
        let out_edges = db.get_out_edges(node1);
        assert_eq!(out_edges.len(), 1);
        assert_eq!(out_edges[0], (knows, node2));

        let in_edges = db.get_in_edges(node2);
        assert_eq!(in_edges.len(), 1);
        assert_eq!(in_edges[0], (knows, node1));

        close_single_file(db).unwrap();
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_get_edges_merge_snapshot_and_delta() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();

        // Create initial graph
        db.begin(false).unwrap();
        let node1 = db.create_node(None).unwrap();
        let node2 = db.create_node(None).unwrap();
        let node3 = db.create_node(None).unwrap();
        let knows = db.get_or_create_etype("KNOWS");
        db.add_edge(node1, knows, node2).unwrap();
        db.commit().unwrap();

        // Checkpoint
        db.checkpoint().unwrap();

        // Add new edge in delta
        db.begin(false).unwrap();
        db.add_edge(node1, knows, node3).unwrap();
        db.commit().unwrap();

        // Should see both edges (snapshot + delta)
        let out_edges = db.get_out_edges(node1);
        assert_eq!(out_edges.len(), 2);
        assert!(out_edges.contains(&(knows, node2)));
        assert!(out_edges.contains(&(knows, node3)));

        close_single_file(db).unwrap();
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_get_edges_with_deleted_edge() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();

        db.begin(false).unwrap();
        let node1 = db.create_node(None).unwrap();
        let node2 = db.create_node(None).unwrap();
        let node3 = db.create_node(None).unwrap();
        let knows = db.get_or_create_etype("KNOWS");
        db.add_edge(node1, knows, node2).unwrap();
        db.add_edge(node1, knows, node3).unwrap();
        db.commit().unwrap();

        db.checkpoint().unwrap();

        // Delete one edge
        db.begin(false).unwrap();
        db.delete_edge(node1, knows, node2).unwrap();
        db.commit().unwrap();

        // Should only see the remaining edge
        let out_edges = db.get_out_edges(node1);
        assert_eq!(out_edges.len(), 1);
        assert_eq!(out_edges[0], (knows, node3));

        close_single_file(db).unwrap();
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_get_edges_skips_deleted_nodes() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();

        db.begin(false).unwrap();
        let node1 = db.create_node(None).unwrap();
        let node2 = db.create_node(None).unwrap();
        let node3 = db.create_node(None).unwrap();
        let knows = db.get_or_create_etype("KNOWS");
        db.add_edge(node1, knows, node2).unwrap();
        db.add_edge(node1, knows, node3).unwrap();
        db.commit().unwrap();

        db.checkpoint().unwrap();

        // Delete one destination node
        db.begin(false).unwrap();
        db.delete_node(node2).unwrap();
        db.commit().unwrap();

        // Should only see edge to non-deleted node
        let out_edges = db.get_out_edges(node1);
        assert_eq!(out_edges.len(), 1);
        assert_eq!(out_edges[0], (knows, node3));

        close_single_file(db).unwrap();
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_get_node_by_key_from_delta() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();

        db.begin(false).unwrap();
        let node_id = db.create_node(Some("user:alice")).unwrap();
        db.commit().unwrap();

        // Lookup by key (from delta)
        assert_eq!(db.get_node_by_key("user:alice"), Some(node_id));
        assert_eq!(db.get_node_by_key("user:bob"), None);

        close_single_file(db).unwrap();
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_get_node_by_key_after_checkpoint() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();

        db.begin(false).unwrap();
        let node_id = db.create_node(Some("user:bob")).unwrap();
        db.commit().unwrap();

        db.checkpoint().unwrap();

        // Lookup by key (from snapshot)
        assert_eq!(db.get_node_by_key("user:bob"), Some(node_id));

        close_single_file(db).unwrap();
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_get_node_by_key_deleted() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();

        db.begin(false).unwrap();
        let node_id = db.create_node(Some("user:charlie")).unwrap();
        db.commit().unwrap();

        db.checkpoint().unwrap();

        // Delete the node
        db.begin(false).unwrap();
        db.delete_node(node_id).unwrap();
        db.commit().unwrap();

        // Key lookup should return None for deleted node
        assert_eq!(db.get_node_by_key("user:charlie"), None);

        close_single_file(db).unwrap();
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_get_node_key() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();

        db.begin(false).unwrap();
        let node_with_key = db.create_node(Some("my:key")).unwrap();
        let node_without_key = db.create_node(None).unwrap();
        db.commit().unwrap();

        assert_eq!(db.get_node_key(node_with_key), Some("my:key".to_string()));
        assert_eq!(db.get_node_key(node_without_key), None);

        // After checkpoint
        db.checkpoint().unwrap();
        
        assert_eq!(db.get_node_key(node_with_key), Some("my:key".to_string()));
        assert_eq!(db.get_node_key(node_without_key), None);

        close_single_file(db).unwrap();
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_get_out_neighbors() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();

        db.begin(false).unwrap();
        let alice = db.create_node(None).unwrap();
        let bob = db.create_node(None).unwrap();
        let charlie = db.create_node(None).unwrap();
        let knows = db.get_or_create_etype("KNOWS");
        let likes = db.get_or_create_etype("LIKES");
        db.add_edge(alice, knows, bob).unwrap();
        db.add_edge(alice, knows, charlie).unwrap();
        db.add_edge(alice, likes, bob).unwrap();
        db.commit().unwrap();

        // Get only KNOWS neighbors
        let knows_neighbors = db.get_out_neighbors(alice, knows);
        assert_eq!(knows_neighbors.len(), 2);
        assert!(knows_neighbors.contains(&bob));
        assert!(knows_neighbors.contains(&charlie));

        // Get only LIKES neighbors
        let likes_neighbors = db.get_out_neighbors(alice, likes);
        assert_eq!(likes_neighbors.len(), 1);
        assert!(likes_neighbors.contains(&bob));

        close_single_file(db).unwrap();
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_get_in_neighbors() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();

        db.begin(false).unwrap();
        let alice = db.create_node(None).unwrap();
        let bob = db.create_node(None).unwrap();
        let charlie = db.create_node(None).unwrap();
        let follows = db.get_or_create_etype("FOLLOWS");
        db.add_edge(bob, follows, alice).unwrap();
        db.add_edge(charlie, follows, alice).unwrap();
        db.commit().unwrap();

        // Get FOLLOWS in-neighbors of alice
        let followers = db.get_in_neighbors(alice, follows);
        assert_eq!(followers.len(), 2);
        assert!(followers.contains(&bob));
        assert!(followers.contains(&charlie));

        close_single_file(db).unwrap();
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_degree_functions() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();

        db.begin(false).unwrap();
        let center = db.create_node(None).unwrap();
        let n1 = db.create_node(None).unwrap();
        let n2 = db.create_node(None).unwrap();
        let n3 = db.create_node(None).unwrap();
        let edge = db.get_or_create_etype("EDGE");
        
        // center has 2 out-edges and 1 in-edge
        db.add_edge(center, edge, n1).unwrap();
        db.add_edge(center, edge, n2).unwrap();
        db.add_edge(n3, edge, center).unwrap();
        db.commit().unwrap();

        assert_eq!(db.get_out_degree(center), 2);
        assert_eq!(db.get_in_degree(center), 1);
        assert_eq!(db.get_out_degree(n1), 0);
        assert_eq!(db.get_in_degree(n1), 1);

        close_single_file(db).unwrap();
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_has_edges_functions() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();

        db.begin(false).unwrap();
        let n1 = db.create_node(None).unwrap();
        let n2 = db.create_node(None).unwrap();
        let knows = db.get_or_create_etype("KNOWS");
        let likes = db.get_or_create_etype("LIKES");
        db.add_edge(n1, knows, n2).unwrap();
        db.commit().unwrap();

        assert!(db.has_out_edges(n1, knows));
        assert!(!db.has_out_edges(n1, likes));
        assert!(db.has_in_edges(n2, knows));
        assert!(!db.has_in_edges(n2, likes));

        close_single_file(db).unwrap();
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_query_nonexistent_node() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");

        let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();

        // Query for non-existent node
        let fake_node: NodeId = 999999;
        
        assert!(db.get_node_props(fake_node).is_none());
        assert!(db.get_out_edges(fake_node).is_empty());
        assert!(db.get_in_edges(fake_node).is_empty());
        assert!(db.get_node_key(fake_node).is_none());

        close_single_file(db).unwrap();
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_query_persistence_after_reopen() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().with_extension("raydb");
        let node1;
        let node2;
        let knows;

        // Create data and checkpoint
        {
            let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();
            
            db.begin(false).unwrap();
            node1 = db.create_node(Some("alice")).unwrap();
            node2 = db.create_node(Some("bob")).unwrap();
            knows = db.get_or_create_etype("KNOWS");
            db.add_edge(node1, knows, node2).unwrap();
            db.set_node_prop_by_name(node1, "name", PropValue::String("Alice".to_string())).unwrap();
            db.commit().unwrap();
            
            db.checkpoint().unwrap();
            close_single_file(db).unwrap();
        }

        // Reopen and verify queries work
        {
            let db = open_single_file(&path, SingleFileOpenOptions::new()).unwrap();
            
            // Key lookup
            assert_eq!(db.get_node_by_key("alice"), Some(node1));
            assert_eq!(db.get_node_by_key("bob"), Some(node2));
            
            // Node key
            assert_eq!(db.get_node_key(node1), Some("alice".to_string()));
            
            // Props
            let props = db.get_node_props(node1).unwrap();
            let name_key = db.get_propkey_id("name").unwrap();
            assert_eq!(props.get(&name_key), Some(&PropValue::String("Alice".to_string())));
            
            // Edges
            let out_edges = db.get_out_edges(node1);
            assert_eq!(out_edges.len(), 1);
            assert_eq!(out_edges[0], (knows, node2));
            
            let in_edges = db.get_in_edges(node2);
            assert_eq!(in_edges.len(), 1);
            assert_eq!(in_edges[0], (knows, node1));
            
            close_single_file(db).unwrap();
        }

        let _ = fs::remove_file(&path);
    }
}
