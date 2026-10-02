//! Database open/close operations for SingleFileDB
//!
//! Handles opening, creating, and closing single-file databases.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
#[cfg(feature = "bench-profile")]
use std::time::Instant;

use parking_lot::{Mutex, RwLock};

use crate::cache::manager::CacheManager;
use crate::constants::*;
use crate::core::header::{
  other_header_slot, read_header_slots, write_header_slot, HEADER_SLOT_A, HEADER_SLOT_B,
};
use crate::core::pager::{
  create_pager_with_locking, is_valid_page_size, open_pager_with_locking, pages_to_store, FilePager,
};
use crate::core::snapshot::reader::SnapshotData;
use crate::core::wal::buffer::WalBuffer;
use crate::error::{KiteError, Result};
use crate::mvcc::{GcConfig, MvccManager};
use crate::replication::primary::PrimaryReplication;
use crate::replication::replica::ReplicaReplication;
use crate::replication::types::ReplicationRole;
use crate::types::*;
use crate::util::compression::CompressionOptions;
use crate::util::mmap::{map_file_range, Mmap};
use crate::vector::store::{create_vector_store, vector_store_delete, vector_store_insert};
use crate::vector::types::VectorStoreConfig;

use super::recovery::{
  committed_transactions, drop_vectors_of_missing_nodes, replay_wal_record, scan_wal_records,
};
use super::vector::{materialize_vector_store_from_lazy_entries, vector_store_state_from_snapshot};
use super::{BackgroundCheckpointState, SchemaReservations, SingleFileDB};

// ============================================================================
// Open Options
// ============================================================================

/// Synchronization mode for WAL writes
///
/// Controls the durability vs performance trade-off for commits.
/// Similar to SQLite's PRAGMA synchronous setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SyncMode {
  /// Sync on every commit (durable against power loss, slowest).
  /// On macOS every sync is `F_FULLFSYNC`, which also flushes the drive's
  /// write cache (plain `fsync` there does not, so a power loss could drop or
  /// reorder acknowledged writes); it costs a few milliseconds per sync.
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

/// Snapshot parse behavior when opening single-file databases
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SnapshotParseMode {
  /// Treat snapshot parse errors as fatal
  #[default]
  Strict,
  /// Ignore snapshot parse errors and recover from WAL only
  Salvage,
}

/// Options for opening a single-file database
#[derive(Debug, Clone)]
pub struct SingleFileOpenOptions {
  /// Open in read-only mode
  pub read_only: bool,
  /// Create database if it doesn't exist
  pub create_if_missing: bool,
  /// Enable MVCC (snapshot isolation + conflict detection)
  pub mvcc: bool,
  /// MVCC GC interval in ms
  pub mvcc_gc_interval_ms: Option<u64>,
  /// MVCC retention in ms
  pub mvcc_retention_ms: Option<u64>,
  /// MVCC max version chain depth
  pub mvcc_max_chain_depth: Option<usize>,
  /// Page size (default 4KB, must be power of 2 between 4KB and 64KB)
  pub page_size: usize,
  /// WAL size in bytes. A database's WAL size is fixed when the file is
  /// created (change it later with `resize_wal`).
  ///
  /// `None` (default): a new file gets a `WAL_DEFAULT_SIZE` (4MB) WAL, and an
  /// existing file is opened with the WAL size recorded in its header.
  /// `Some(n)`: a new file gets an `n`-byte WAL, and opening an existing file
  /// whose WAL size differs fails.
  pub wal_size: Option<usize>,
  /// Enable auto-checkpoint when WAL usage exceeds threshold (default true)
  pub auto_checkpoint: bool,
  /// WAL usage threshold (0.0-1.0) to trigger auto-checkpoint (default 0.5)
  pub checkpoint_threshold: f64,
  /// Use background (non-blocking) checkpoint instead of blocking (default true)
  pub background_checkpoint: bool,
  /// Cache options (None = disabled)
  pub cache: Option<CacheOptions>,
  /// Compression options for checkpoint snapshots
  pub checkpoint_compression: Option<CompressionOptions>,
  /// Synchronization mode for WAL writes (default: Full)
  pub sync_mode: SyncMode,
  /// Enable group commit (coalesce WAL flushes across commits)
  pub group_commit_enabled: bool,
  /// Group commit window in milliseconds
  pub group_commit_window_ms: u64,
  /// Snapshot parse behavior (default: Strict)
  pub snapshot_parse_mode: SnapshotParseMode,
  /// Replication role (default: Disabled)
  pub replication_role: ReplicationRole,
  /// Optional replication sidecar path (defaults to derived from DB path)
  pub replication_sidecar_path: Option<PathBuf>,
  /// Source primary db path (replica role only)
  pub replication_source_db_path: Option<PathBuf>,
  /// Source primary sidecar path override (replica role only)
  pub replication_source_sidecar_path: Option<PathBuf>,
  /// Fault injection for tests: fail append once `n` successful appends reached
  pub replication_fail_after_append_for_testing: Option<u64>,
  /// Test-only abrupt stop after local commit durability and before sidecar append.
  #[doc(hidden)]
  pub replication_crash_after_local_commit_for_testing: bool,
  /// Rotate replication segments when active segment reaches/exceeds this size
  pub replication_segment_max_bytes: Option<u64>,
  /// Retain at least this many entries when pruning old segments
  pub replication_retention_min_entries: Option<u64>,
  /// Retain segments newer than this many milliseconds (primary role only)
  pub replication_retention_min_ms: Option<u64>,
  /// Skip all main-file locking solely to simulate independent nodes on shared storage in tests.
  #[doc(hidden)]
  pub danger_bypass_file_lock_for_multi_node_simulation: bool,
}

impl Default for SingleFileOpenOptions {
  fn default() -> Self {
    Self {
      read_only: false,
      create_if_missing: true,
      mvcc: false,
      mvcc_gc_interval_ms: None,
      mvcc_retention_ms: None,
      mvcc_max_chain_depth: None,
      page_size: DEFAULT_PAGE_SIZE,
      wal_size: None,
      auto_checkpoint: true,
      checkpoint_threshold: 0.5,
      background_checkpoint: true,
      cache: None,
      checkpoint_compression: Some(CompressionOptions {
        enabled: true,
        ..Default::default()
      }),
      sync_mode: SyncMode::Full,
      group_commit_enabled: false,
      group_commit_window_ms: 2,
      snapshot_parse_mode: SnapshotParseMode::Strict,
      replication_role: ReplicationRole::Disabled,
      replication_sidecar_path: None,
      replication_source_db_path: None,
      replication_source_sidecar_path: None,
      replication_fail_after_append_for_testing: None,
      replication_crash_after_local_commit_for_testing: false,
      replication_segment_max_bytes: None,
      replication_retention_min_entries: None,
      replication_retention_min_ms: None,
      danger_bypass_file_lock_for_multi_node_simulation: false,
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

  /// Skip all main-file locking solely to simulate independent nodes on shared storage in tests.
  /// Never use this for normal database access.
  #[doc(hidden)]
  pub fn danger_bypass_file_lock_for_multi_node_simulation(mut self, value: bool) -> Self {
    self.danger_bypass_file_lock_for_multi_node_simulation = value;
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

  pub fn page_size(mut self, value: usize) -> Self {
    self.page_size = value;
    self
  }

  /// Require a WAL of `value` bytes: a new file is created with it, and an
  /// existing file whose WAL size differs is rejected. Leave unset to accept
  /// an existing file's WAL size.
  pub fn wal_size(mut self, value: usize) -> Self {
    self.wal_size = Some(value);
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

  pub fn checkpoint_compression(mut self, options: Option<CompressionOptions>) -> Self {
    self.checkpoint_compression = options;
    self
  }

  pub fn disable_checkpoint_compression(mut self) -> Self {
    self.checkpoint_compression = None;
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

  /// Enable or disable group commit (coalesce WAL flushes across commits)
  pub fn group_commit_enabled(mut self, value: bool) -> Self {
    self.group_commit_enabled = value;
    self
  }

  /// Set the group commit window in milliseconds
  pub fn group_commit_window_ms(mut self, value: u64) -> Self {
    self.group_commit_window_ms = value;
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

  /// Set snapshot parse mode (Strict or Salvage)
  pub fn snapshot_parse_mode(mut self, mode: SnapshotParseMode) -> Self {
    self.snapshot_parse_mode = mode;
    self
  }

  /// Set replication role (disabled | primary | replica)
  pub fn replication_role(mut self, role: ReplicationRole) -> Self {
    self.replication_role = role;
    self
  }

  /// Set replication sidecar path (for primary/replica modes)
  pub fn replication_sidecar_path<P: AsRef<Path>>(mut self, path: P) -> Self {
    self.replication_sidecar_path = Some(path.as_ref().to_path_buf());
    self
  }

  /// Set replication source db path (replica role only)
  pub fn replication_source_db_path<P: AsRef<Path>>(mut self, path: P) -> Self {
    self.replication_source_db_path = Some(path.as_ref().to_path_buf());
    self
  }

  /// Set replication source sidecar path (replica role only)
  pub fn replication_source_sidecar_path<P: AsRef<Path>>(mut self, path: P) -> Self {
    self.replication_source_sidecar_path = Some(path.as_ref().to_path_buf());
    self
  }

  /// Test-only fault injection for append failures.
  pub fn replication_fail_after_append_for_testing(mut self, value: u64) -> Self {
    self.replication_fail_after_append_for_testing = Some(value);
    self
  }

  /// Test-only fault injection at the local-durable/sidecar boundary.
  #[doc(hidden)]
  pub fn replication_crash_after_local_commit_for_testing(mut self, value: bool) -> Self {
    self.replication_crash_after_local_commit_for_testing = value;
    self
  }

  /// Set replication segment rotation threshold in bytes (primary role only)
  pub fn replication_segment_max_bytes(mut self, value: u64) -> Self {
    self.replication_segment_max_bytes = Some(value);
    self
  }

  /// Set retention minimum entries to keep when pruning (primary role only)
  pub fn replication_retention_min_entries(mut self, value: u64) -> Self {
    self.replication_retention_min_entries = Some(value);
    self
  }

  /// Set retention minimum time window in milliseconds (primary role only)
  pub fn replication_retention_min_ms(mut self, value: u64) -> Self {
    self.replication_retention_min_ms = Some(value);
    self
  }
}

/// Options for closing a single-file database.
#[derive(Debug, Clone, Copy, Default)]
pub struct SingleFileCloseOptions {
  /// If set, run a blocking checkpoint before close when WAL usage >= threshold.
  /// Threshold is clamped to [0.0, 1.0].
  pub checkpoint_if_wal_usage_at_least: Option<f64>,
}

impl SingleFileCloseOptions {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn checkpoint_if_wal_usage_at_least(mut self, threshold: f64) -> Self {
    self.checkpoint_if_wal_usage_at_least = Some(threshold);
    self
  }
}

struct SnapshotLoadState<'a> {
  header: &'a DbHeaderV1,
  pager: &'a mut FilePager,
  options: &'a SingleFileOpenOptions,
  label_names: &'a mut HashMap<String, LabelId>,
  label_ids: &'a mut HashMap<LabelId, String>,
  etype_names: &'a mut HashMap<String, ETypeId>,
  etype_ids: &'a mut HashMap<ETypeId, String>,
  propkey_names: &'a mut HashMap<String, PropKeyId>,
  propkey_ids: &'a mut HashMap<PropKeyId, String>,
  next_node_id: &'a mut NodeId,
  next_label_id: &'a mut LabelId,
  next_etype_id: &'a mut ETypeId,
  next_propkey_id: &'a mut PropKeyId,
  #[cfg(feature = "bench-profile")]
  profile: &'a mut OpenProfileCounters,
  #[cfg(feature = "bench-profile")]
  profile_enabled: bool,
  crc_chunk_size: Option<usize>,
  #[cfg(feature = "bench-profile")]
  snapshot_profile: Option<&'a mut SnapshotOpenProfile>,
}

pub(crate) fn map_snapshot_range(pager: &FilePager, header: &DbHeaderV1) -> Result<Arc<Mmap>> {
  let offset = header
    .snapshot_start_page
    .checked_mul(header.page_size as u64)
    .ok_or_else(|| KiteError::InvalidSnapshot("snapshot offset overflow".to_string()))?;
  let length = header
    .snapshot_page_count
    .checked_mul(header.page_size as u64)
    .ok_or_else(|| KiteError::InvalidSnapshot("snapshot length overflow".to_string()))?;
  let length = usize::try_from(length)
    .map_err(|_| KiteError::InvalidSnapshot("snapshot is too large to map".to_string()))?;

  Ok(Arc::new(map_file_range(pager.file(), offset, length)?))
}

#[cfg(feature = "bench-profile")]
#[derive(Debug, Default)]
struct OpenProfileCounters {
  snapshot_parse_ns: u64,
  snapshot_crc_ns: u64,
  snapshot_decode_ns: u64,
  schema_hydrate_ns: u64,
  wal_scan_ns: u64,
  wal_replay_ns: u64,
  vector_init_ns: u64,
}

#[cfg(feature = "bench-profile")]
fn elapsed_ns(started: Instant) -> u64 {
  started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64
}

#[cfg(feature = "bench-profile")]
#[derive(Debug, Clone, Default)]
struct SnapshotOpenProfile {
  parse_total_ns: u64,
  snapshot_crc_ns: u64,
  snapshot_crc_bytes: usize,
  snapshot_crc_chunk_size: usize,
  snapshot_crc_sections: Vec<crate::core::snapshot::reader::SnapshotCrcSectionProfile>,
}

fn snapshot_crc_chunk_size_from_env() -> Option<usize> {
  std::env::var("KITEDB_SNAPSHOT_CRC_CHUNK_BYTES")
    .ok()
    .and_then(|raw| raw.parse::<usize>().ok())
    .filter(|value| *value > 0)
}

#[cfg(feature = "bench-profile")]
fn open_profile_enabled() -> bool {
  if std::env::var_os("KITEDB_BENCH_PROFILE_OPEN").is_none() {
    return false;
  }
  std::env::var("KITEDB_BENCH_PROFILE_OPEN")
    .map(|value| {
      let value = value.to_lowercase();
      value == "1" || value == "true" || value == "yes"
    })
    .unwrap_or(true)
}

#[cfg(feature = "bench-profile")]
fn log_open_profile(path: &Path, profile: &SnapshotOpenProfile) {
  if profile.parse_total_ns == 0 {
    return;
  }

  let snapshot_decode_ns = profile
    .parse_total_ns
    .saturating_sub(profile.snapshot_crc_ns);
  println!(
    "[open_profile] path={} snapshot_crc_ns={} snapshot_decode_ns={} snapshot_crc_bytes={} snapshot_crc_chunk_bytes={}",
    path.display(),
    profile.snapshot_crc_ns,
    snapshot_decode_ns,
    profile.snapshot_crc_bytes,
    profile.snapshot_crc_chunk_size,
  );

  for section in &profile.snapshot_crc_sections {
    let section_name = section
      .section_id
      .map(|id| format!("{id:?}"))
      .unwrap_or_else(|| "__non_section__".to_string());
    println!(
      "[open_profile] path={} snapshot_crc_section={} bytes={} ns={}",
      path.display(),
      section_name,
      section.bytes,
      section.crc_ns
    );
  }
}

fn load_snapshot_and_schema(state: &mut SnapshotLoadState<'_>) -> Result<Option<SnapshotData>> {
  if state.header.snapshot_page_count == 0 {
    return Ok(None);
  }

  let mut parse_options = crate::core::snapshot::reader::ParseSnapshotOptions::default();
  if matches!(
    state.options.snapshot_parse_mode,
    SnapshotParseMode::Salvage
  ) {
    parse_options.skip_crc_validation = true;
  }
  parse_options.crc_chunk_size = state.crc_chunk_size;

  #[cfg(feature = "bench-profile")]
  {
    let mmap = map_snapshot_range(state.pager, state.header)?;

    let parse_started = Instant::now();
    let _ = SnapshotData::parse(mmap.clone(), &parse_options);
    let parse_total_ns = elapsed_ns(parse_started);
    state.profile.snapshot_parse_ns = state
      .profile
      .snapshot_parse_ns
      .saturating_add(parse_total_ns);

    // Deep split for profiling runs: decode-only + inferred CRC delta.
    if state.profile_enabled && !parse_options.skip_crc_validation {
      let mut decode_options = parse_options.clone();
      decode_options.skip_crc_validation = true;
      let decode_started = Instant::now();
      if SnapshotData::parse(mmap, &decode_options).is_ok() {
        let decode_ns = elapsed_ns(decode_started);
        state.profile.snapshot_decode_ns =
          state.profile.snapshot_decode_ns.saturating_add(decode_ns);
        state.profile.snapshot_crc_ns = state
          .profile
          .snapshot_crc_ns
          .saturating_add(parse_total_ns.saturating_sub(decode_ns));
      } else {
        state.profile.snapshot_decode_ns = state
          .profile
          .snapshot_decode_ns
          .saturating_add(parse_total_ns);
      }
    } else {
      state.profile.snapshot_decode_ns = state
        .profile
        .snapshot_decode_ns
        .saturating_add(parse_total_ns);
    }
  }
  #[cfg(feature = "bench-profile")]
  let profile_sink = if state.snapshot_profile.is_some() && !parse_options.skip_crc_validation {
    Some(Arc::new(std::sync::Mutex::new(None)))
  } else {
    None
  };
  #[cfg(feature = "bench-profile")]
  {
    parse_options.crc_profile_sink = profile_sink.clone();
  }

  #[cfg(feature = "bench-profile")]
  let parse_start = Instant::now();
  let parse_result = SnapshotData::parse(
    map_snapshot_range(state.pager, state.header)?,
    &parse_options,
  );
  #[cfg(feature = "bench-profile")]
  let parse_total_ns = parse_start.elapsed().as_nanos() as u64;

  match parse_result {
    Ok(snap) => {
      #[cfg(feature = "bench-profile")]
      let schema_started = Instant::now();
      #[cfg(feature = "bench-profile")]
      {
        if let Some(profile) = state.snapshot_profile.as_deref_mut() {
          profile.parse_total_ns = parse_total_ns;
          if let Some(sink) = profile_sink {
            if let Ok(mut guard) = sink.lock() {
              if let Some(crc_profile) = guard.take() {
                profile.snapshot_crc_ns = crc_profile.total_ns;
                profile.snapshot_crc_bytes = crc_profile.total_bytes;
                profile.snapshot_crc_chunk_size = crc_profile.chunk_size;
                profile.snapshot_crc_sections = crc_profile.sections;
              }
            }
          }
        }
      }

      // Load schema from snapshot
      for i in 1..=snap.header.num_labels as u32 {
        if let Some(name) = snap.label_name(i) {
          state.label_names.insert(name.to_string(), i);
          state.label_ids.insert(i, name.to_string());
        }
      }
      for i in 1..=snap.header.num_etypes as u32 {
        if let Some(name) = snap.etype_name(i) {
          state.etype_names.insert(name.to_string(), i);
          state.etype_ids.insert(i, name.to_string());
        }
      }
      for i in 1..=snap.header.num_propkeys as u32 {
        if let Some(name) = snap.propkey_name(i) {
          state.propkey_names.insert(name.to_string(), i);
          state.propkey_ids.insert(i, name.to_string());
        }
      }

      // Update ID allocators from snapshot. Its max_node_id counts live nodes
      // only; the header's may be higher (deleted IDs) and must not be reused.
      *state.next_node_id = (*state.next_node_id).max(snap.header.max_node_id.saturating_add(1));
      *state.next_label_id = snap.header.num_labels as u32 + 1;
      *state.next_etype_id = snap.header.num_etypes as u32 + 1;
      *state.next_propkey_id = snap.header.num_propkeys as u32 + 1;
      #[cfg(feature = "bench-profile")]
      {
        state.profile.schema_hydrate_ns = state
          .profile
          .schema_hydrate_ns
          .saturating_add(elapsed_ns(schema_started));
      }

      Ok(Some(snap))
    }
    Err(e) => match state.options.snapshot_parse_mode {
      SnapshotParseMode::Strict => Err(e),
      SnapshotParseMode::Salvage => {
        eprintln!("Warning: Failed to parse snapshot: {e}");
        Ok(None)
      }
    },
  }
}

fn init_mvcc_from_wal(
  options: &SingleFileOpenOptions,
  next_tx_id: TxId,
  next_commit_ts: u64,
  committed_in_order: &[(TxId, Vec<&crate::core::wal::record::ParsedWalRecord>)],
  delta: &DeltaState,
) -> Option<std::sync::Arc<MvccManager>> {
  if !options.mvcc {
    return None;
  }

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

  if !committed_in_order.is_empty() {
    use crate::core::wal::record::{
      parse_add_edge_payload, parse_add_edge_props_payload, parse_add_edges_batch_payload,
      parse_add_edges_props_batch_payload, parse_add_node_label_payload, parse_create_node_payload,
      parse_create_nodes_batch_payload, parse_del_edge_prop_payload, parse_del_node_prop_payload,
      parse_delete_edge_payload, parse_delete_node_payload, parse_remove_node_label_payload,
      parse_set_edge_prop_payload, parse_set_edge_props_payload, parse_set_node_prop_payload,
    };

    let mut commit_ts: u64 = 1;
    for (txid, records) in committed_in_order {
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
                    delta: node_delta.for_version(),
                  },
                  *txid,
                  commit_ts,
                );
              }
            }
          }
          WalRecordType::CreateNodesBatch => {
            if let Some(nodes) = parse_create_nodes_batch_payload(&record.payload) {
              for data in nodes {
                if let Some(node_delta) = delta.created_nodes.get(&data.node_id) {
                  let mut vc = mvcc.version_chain.lock();
                  vc.append_node_version(
                    data.node_id,
                    NodeVersionData {
                      node_id: data.node_id,
                      delta: node_delta.for_version(),
                    },
                    *txid,
                    commit_ts,
                  );
                }
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
          WalRecordType::AddEdgesBatch => {
            if let Some(edges) = parse_add_edges_batch_payload(&record.payload) {
              let mut vc = mvcc.version_chain.lock();
              for data in edges {
                vc.append_edge_version(data.src, data.etype, data.dst, true, *txid, commit_ts);
              }
            }
          }
          WalRecordType::AddEdgeProps => {
            if let Some(data) = parse_add_edge_props_payload(&record.payload) {
              let mut vc = mvcc.version_chain.lock();
              vc.append_edge_version(data.src, data.etype, data.dst, true, *txid, commit_ts);
              for (key_id, value) in data.props {
                vc.append_edge_prop_version(
                  data.src,
                  data.etype,
                  data.dst,
                  key_id,
                  Some(std::sync::Arc::new(value)),
                  *txid,
                  commit_ts,
                );
              }
            }
          }
          WalRecordType::AddEdgesPropsBatch => {
            if let Some(edges) = parse_add_edges_props_batch_payload(&record.payload) {
              let mut vc = mvcc.version_chain.lock();
              for data in edges {
                vc.append_edge_version(data.src, data.etype, data.dst, true, *txid, commit_ts);
                for (key_id, value) in data.props {
                  vc.append_edge_prop_version(
                    data.src,
                    data.etype,
                    data.dst,
                    key_id,
                    Some(std::sync::Arc::new(value)),
                    *txid,
                    commit_ts,
                  );
                }
              }
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
                Some(std::sync::Arc::new(data.value)),
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
                Some(std::sync::Arc::new(data.value)),
                *txid,
                commit_ts,
              );
            }
          }
          WalRecordType::SetEdgeProps => {
            if let Some(data) = parse_set_edge_props_payload(&record.payload) {
              let mut vc = mvcc.version_chain.lock();
              for (key_id, value) in data.props {
                vc.append_edge_prop_version(
                  data.src,
                  data.etype,
                  data.dst,
                  key_id,
                  Some(std::sync::Arc::new(value)),
                  *txid,
                  commit_ts,
                );
              }
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
          WalRecordType::AddNodeLabel => {
            if let Some(data) = parse_add_node_label_payload(&record.payload) {
              let mut vc = mvcc.version_chain.lock();
              vc.append_node_label_version(
                data.node_id,
                data.label_id,
                Some(true),
                *txid,
                commit_ts,
              );
            }
          }
          WalRecordType::RemoveNodeLabel => {
            if let Some(data) = parse_remove_node_label_payload(&record.payload) {
              let mut vc = mvcc.version_chain.lock();
              vc.append_node_label_version(data.node_id, data.label_id, None, *txid, commit_ts);
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
}

// ============================================================================
// Open / Close
// ============================================================================

/// Open a single-file database
pub fn open_single_file<P: AsRef<Path>>(
  path: P,
  options: SingleFileOpenOptions,
) -> Result<SingleFileDB> {
  let lock_file = !options.danger_bypass_file_lock_for_multi_node_simulation;
  open_single_file_internal(path.as_ref(), options, lock_file)
}

pub(crate) fn open_replication_source(path: &Path) -> Result<SingleFileDB> {
  open_single_file_internal(
    path,
    SingleFileOpenOptions::new()
      .read_only(true)
      .create_if_missing(false)
      .replication_role(ReplicationRole::Disabled),
    false,
  )
}

fn open_single_file_internal(
  path: &Path,
  options: SingleFileOpenOptions,
  lock_file: bool,
) -> Result<SingleFileDB> {
  #[cfg(feature = "bench-profile")]
  let open_started = Instant::now();
  #[cfg(feature = "bench-profile")]
  let mut open_profile = OpenProfileCounters::default();
  #[cfg(feature = "bench-profile")]
  let profile_enabled = open_profile_enabled();

  // Validate page size
  if !is_valid_page_size(options.page_size) {
    return Err(KiteError::Internal(format!(
      "Invalid page size: {}. Must be power of 2 between 4KB and 64KB",
      options.page_size
    )));
  }

  // Check if file exists
  let file_exists = path.exists();

  if !file_exists && !options.create_if_missing {
    return Err(KiteError::InvalidPath(format!(
      "Database does not exist at {}",
      path.display()
    )));
  }

  if !file_exists && options.read_only {
    return Err(KiteError::ReadOnly);
  }

  // Open or create pager
  let (mut pager, mut header, is_new, mut header_slot) = if file_exists {
    // Open existing database
    let mut pager = open_pager_with_locking(path, options.page_size, options.read_only, lock_file)?;
    pager.set_full_fsync(options.sync_mode == SyncMode::Full);

    // Read both independently checksummed header pages and select the newest
    // valid generation. A torn newest slot falls back to the other slot.
    let (header, header_slot) = read_header_slots(&mut pager)?;

    // Refuse a format this build cannot read, or (writable) cannot write,
    // before anything below rewrites the file.
    header.check_supported(!options.read_only)?;

    // Files created before the dual-page format have WAL at page one. Migrate
    // through a separately checkpointed file; an in-place shift would destroy
    // the old header's fallback before the new slot is durable.
    if header.wal_start_page < HEADER_SLOT_B as u64 + 1 && !options.read_only {
      drop(pager);
      migrate_legacy_single_header(path, &options, lock_file)?;
      return open_single_file_internal(path, options, lock_file);
    }

    // The WAL size is fixed at creation. Only an explicitly requested size is
    // checked; otherwise the header's size is used as-is.
    if let Some(wal_size) = options.wal_size {
      let expected_wal_pages = pages_to_store(wal_size, header.page_size as usize) as u64;
      if header.wal_page_count != expected_wal_pages {
        return Err(KiteError::InvalidSnapshot(format!(
          "WAL size mismatch: header has {} pages, options require {} pages",
          header.wal_page_count, expected_wal_pages
        )));
      }
    }

    (pager, header, false, header_slot)
  } else {
    // Create new database
    let mut pager = create_pager_with_locking(path, options.page_size, lock_file)?;
    pager.set_full_fsync(options.sync_mode == SyncMode::Full);

    // Calculate WAL page count
    let wal_size = options.wal_size.unwrap_or(WAL_DEFAULT_SIZE);
    let wal_page_count = pages_to_store(wal_size, options.page_size) as u64;

    // Create initial header
    let header = DbHeaderV1::new(options.page_size as u32, wal_page_count);

    // Write both initial header slots before allocating the WAL. This makes a
    // brand-new file recoverable even if the first open is interrupted.
    let header_bytes = header.serialize_to_page();
    pager.write_page(0, &header_bytes)?;
    pager.write_page(1, &header_bytes)?;

    // Allocate WAL pages
    pager.allocate_pages(wal_page_count as u32)?;

    // Sync to disk
    pager.sync()?;

    (pager, header, true, HEADER_SLOT_A)
  };

  // Initialize WAL buffer
  let mut wal_buffer = WalBuffer::from_header(&header);

  // A background checkpoint cut that no install finished. Replay reads both
  // regions in place, primary first, unless a writable open merges them.
  let mut replay_cut_in_place = header.checkpoint_in_progress != 0;
  if replay_cut_in_place
    && (wal_buffer.primary_head() > wal_buffer.primary_region_size()
      || wal_buffer.secondary_head() > wal_buffer.capacity())
  {
    return Err(KiteError::InvalidWal(
      "WAL region heads exceed their regions".to_string(),
    ));
  }
  if !options.read_only {
    // Records of a type this version does not know are a newer version's,
    // not torn: refuse rather than trim or compact them away below.
    wal_buffer.check_record_types(&mut pager)?;

    // A crash during a commit's sync can leave a durable header naming WAL
    // bytes that never landed. Replay stops at them, so drop them before
    // anything is appended after them, out of replay's reach. (A retired
    // primary region is compacted below, which keeps only readable records.)
    if !wal_buffer.is_primary_retired() && wal_buffer.trim_to_valid_records(&mut pager)? {
      wal_buffer.store_in_header(&mut header);
      install_recovered_header(&mut pager, &mut header, &mut header_slot)?;
    }

    // Finish an interrupted background checkpoint. Each branch leaves the WAL
    // bytes synced before a header naming them is installed in the other
    // slot, so the selected header stays the crash fallback.
    let rebuilt_wal = if replay_cut_in_place {
      // If the secondary region's records do not fit after the primary
      // region's, the cut stays: the first checkpoint finishes it.
      replay_cut_in_place = !wal_buffer.merge_cut_into_primary(&mut pager)?;
      !replay_cut_in_place
    } else if wal_buffer.is_primary_retired() {
      // The new snapshot was installed with the post-cut records still in the
      // secondary region, and the process stopped before compacting them into
      // the primary region. Read-only opens replay them in place instead.
      wal_buffer.compact_secondary_into_primary(&mut pager)?;
      true
    } else {
      false
    };
    if rebuilt_wal {
      wal_buffer.store_in_header(&mut header);
      header.checkpoint_in_progress = 0;
      install_recovered_header(&mut pager, &mut header, &mut header_slot)?;
    }
  }

  // Initialize ID allocators from header
  let mut next_node_id = INITIAL_NODE_ID;
  let mut next_label_id = INITIAL_LABEL_ID;
  let mut next_etype_id = INITIAL_ETYPE_ID;
  let mut next_propkey_id = INITIAL_PROPKEY_ID;
  let next_tx_id = header.next_tx_id;

  if header.max_node_id > 0 {
    next_node_id = header.max_node_id.saturating_add(1);
  }

  // Initialize delta
  let mut delta = DeltaState::new();
  let mut next_commit_ts: u64 = 1;
  let mut committed_in_order: Vec<(TxId, Vec<&crate::core::wal::record::ParsedWalRecord>)> =
    Vec::new();

  // Schema maps
  let mut label_names: HashMap<String, LabelId> = HashMap::new();
  let mut label_ids: HashMap<LabelId, String> = HashMap::new();
  let mut etype_names: HashMap<String, ETypeId> = HashMap::new();
  let mut etype_ids: HashMap<ETypeId, String> = HashMap::new();
  let mut propkey_names: HashMap<String, PropKeyId> = HashMap::new();
  let mut propkey_ids: HashMap<PropKeyId, String> = HashMap::new();
  let crc_chunk_size = snapshot_crc_chunk_size_from_env();
  #[cfg(feature = "bench-profile")]
  let mut snapshot_profile = if open_profile_enabled() {
    Some(SnapshotOpenProfile::default())
  } else {
    None
  };

  // Load snapshot if exists
  let mut snapshot_state = SnapshotLoadState {
    header: &header,
    pager: &mut pager,
    options: &options,
    label_names: &mut label_names,
    label_ids: &mut label_ids,
    etype_names: &mut etype_names,
    etype_ids: &mut etype_ids,
    propkey_names: &mut propkey_names,
    propkey_ids: &mut propkey_ids,
    next_node_id: &mut next_node_id,
    next_label_id: &mut next_label_id,
    next_etype_id: &mut next_etype_id,
    next_propkey_id: &mut next_propkey_id,
    #[cfg(feature = "bench-profile")]
    profile: &mut open_profile,
    #[cfg(feature = "bench-profile")]
    profile_enabled,
    crc_chunk_size,
    #[cfg(feature = "bench-profile")]
    snapshot_profile: snapshot_profile.as_mut(),
  };
  let snapshot = load_snapshot_and_schema(&mut snapshot_state)?;
  #[cfg(feature = "bench-profile")]
  if let Some(profile) = snapshot_profile.as_ref() {
    log_open_profile(path, profile);
  }

  // Replay WAL for recovery (if not a new database)
  let mut _wal_records_storage: Option<Vec<crate::core::wal::record::ParsedWalRecord>>;
  if !is_new && header.wal_head > 0 {
    #[cfg(feature = "bench-profile")]
    let wal_scan_started = Instant::now();
    _wal_records_storage = Some(if replay_cut_in_place {
      wal_buffer.records_for_recovery(&mut pager)?
    } else {
      scan_wal_records(&mut pager, &header)?
    });
    #[cfg(feature = "bench-profile")]
    {
      open_profile.wal_scan_ns = open_profile
        .wal_scan_ns
        .saturating_add(elapsed_ns(wal_scan_started));
    }
    if let Some(ref wal_records) = _wal_records_storage {
      committed_in_order = committed_transactions(wal_records);

      // Replay committed transactions
      #[cfg(feature = "bench-profile")]
      let wal_replay_started = Instant::now();
      for (_txid, records) in &committed_in_order {
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
        next_commit_ts += 1;
      }
      drop_vectors_of_missing_nodes(&mut delta, snapshot.as_ref());
      #[cfg(feature = "bench-profile")]
      {
        open_profile.wal_replay_ns = open_profile
          .wal_replay_ns
          .saturating_add(elapsed_ns(wal_replay_started));
      }
    }
  } else {
    _wal_records_storage = None;
  }

  // Load vector-store state from snapshot (if present).
  // Newer snapshots keep stores lazy until first access.
  #[cfg(feature = "bench-profile")]
  let vector_init_started = Instant::now();
  let (mut vector_stores, mut vector_store_lazy_entries) = if let Some(ref snapshot) = snapshot {
    if snapshot
      .header
      .flags
      .contains(SnapshotFlags::HAS_VECTOR_STORES)
      || snapshot.header.flags.contains(SnapshotFlags::HAS_VECTORS)
    {
      vector_store_state_from_snapshot(snapshot)?
    } else {
      (HashMap::new(), HashMap::new())
    }
  } else {
    (HashMap::new(), HashMap::new())
  };

  // Apply pending vector operations from WAL replay
  for ((node_id, prop_key_id), operation) in delta.pending_vectors.drain() {
    if let Some(ref snapshot) = snapshot {
      materialize_vector_store_from_lazy_entries(
        snapshot,
        &mut vector_stores,
        &mut vector_store_lazy_entries,
        prop_key_id,
      )?;
    }

    match operation {
      Some(vector) => {
        // Get or create vector store
        let store = vector_stores.entry(prop_key_id).or_insert_with(|| {
          let config = VectorStoreConfig::new(vector.len());
          create_vector_store(config)
        });
        vector_store_insert(store, node_id, vector.as_ref()).map_err(|e| {
          KiteError::InvalidWal(format!(
            "Failed to apply vector insert during WAL replay for node {node_id} (prop {prop_key_id}): {e}"
          ))
        })?;
      }
      None => {
        // Delete operation
        if let Some(store) = vector_stores.get_mut(&prop_key_id) {
          vector_store_delete(store, node_id);
        }
      }
    }
  }
  #[cfg(feature = "bench-profile")]
  {
    open_profile.vector_init_ns = open_profile
      .vector_init_ns
      .saturating_add(elapsed_ns(vector_init_started));
  }

  // Initialize cache if enabled
  let cache = options.cache.clone().map(CacheManager::new);

  // Initialize MVCC if enabled (after WAL replay)
  let mvcc = init_mvcc_from_wal(
    &options,
    next_tx_id,
    next_commit_ts,
    &committed_in_order,
    &delta,
  );

  if options.read_only && options.replication_role != ReplicationRole::Disabled {
    return Err(KiteError::ReadOnly);
  }

  let (primary_replication, replica_replication) = match options.replication_role {
    ReplicationRole::Disabled => (None, None),
    ReplicationRole::Primary => (
      Some(PrimaryReplication::open_with_recovery(
        path,
        options.replication_sidecar_path.clone(),
        options.replication_segment_max_bytes,
        options.replication_retention_min_entries,
        options.replication_retention_min_ms,
        options.sync_mode,
        options.replication_fail_after_append_for_testing,
        committed_in_order.last().map(|(txid, _)| *txid),
        options.replication_crash_after_local_commit_for_testing,
      )?),
      None,
    ),
    ReplicationRole::Replica => (
      None,
      Some(ReplicaReplication::open(
        path,
        options.replication_sidecar_path.clone(),
        options.replication_source_db_path.clone(),
        options.replication_source_sidecar_path.clone(),
      )?),
    ),
  };

  #[cfg(feature = "bench-profile")]
  {
    if profile_enabled {
      let total_ns = elapsed_ns(open_started);
      let wal_records = _wal_records_storage.as_ref().map(|r| r.len()).unwrap_or(0);
      eprintln!(
        "[bench-profile][open] path={} total_ns={} snapshot_parse_ns={} snapshot_crc_ns={} snapshot_decode_ns={} schema_hydrate_ns={} wal_scan_ns={} wal_replay_ns={} vector_init_ns={} snapshot_loaded={} wal_records={} wal_txs={} vector_stores={} vector_lazy_entries={}",
        path.display(),
        total_ns,
        open_profile.snapshot_parse_ns,
        open_profile.snapshot_crc_ns,
        open_profile.snapshot_decode_ns,
        open_profile.schema_hydrate_ns,
        open_profile.wal_scan_ns,
        open_profile.wal_replay_ns,
        open_profile.vector_init_ns,
        usize::from(snapshot.is_some()),
        wal_records,
        committed_in_order.len(),
        vector_stores.len(),
        vector_store_lazy_entries.len(),
      );
    }
  }

  Ok(SingleFileDB {
    path: path.to_path_buf(),
    read_only: options.read_only,
    pager: Mutex::new(pager),
    header: RwLock::new(header),
    header_slot: AtomicU32::new(header_slot),
    wal_buffer: Mutex::new(wal_buffer),
    snapshot: RwLock::new(snapshot),
    delta: RwLock::new(delta),
    next_node_id: AtomicU64::new(next_node_id),
    next_label_id: AtomicU32::new(next_label_id),
    next_etype_id: AtomicU32::new(next_etype_id),
    next_propkey_id: AtomicU32::new(next_propkey_id),
    next_tx_id: AtomicU64::new(next_tx_id),
    current_tx: Mutex::new(HashMap::new()),
    active_writers: AtomicUsize::new(0),
    active_transactions: AtomicUsize::new(0),
    open_write_txids: Mutex::new(HashSet::new()),
    checkpoint_gate: RwLock::new(()),
    checkpoint_wait: Mutex::new(()),
    checkpoint_cv: parking_lot::Condvar::new(),
    cut_wait: Mutex::new(()),
    cut_cv: parking_lot::Condvar::new(),
    commit_lock: Mutex::new(()),
    group_commit_state: Mutex::new(super::GroupCommitState::default()),
    group_commit_cv: parking_lot::Condvar::new(),
    mvcc,
    label_names: RwLock::new(label_names),
    label_ids: RwLock::new(label_ids),
    etype_names: RwLock::new(etype_names),
    etype_ids: RwLock::new(etype_ids),
    propkey_names: RwLock::new(propkey_names),
    propkey_ids: RwLock::new(propkey_ids),
    schema_reservations: Mutex::new(SchemaReservations::default()),
    auto_checkpoint: options.auto_checkpoint,
    checkpoint_threshold: options.checkpoint_threshold,
    background_checkpoint: options.background_checkpoint,
    checkpoint_state: Mutex::new(BackgroundCheckpointState::default()),
    checkpoint_progress: AtomicU64::new(0),
    checkpoint_steps_running: AtomicUsize::new(0),
    checkpoint_cancelled: AtomicBool::new(false),
    vector_stores: RwLock::new(vector_stores),
    vector_store_lazy_entries: RwLock::new(vector_store_lazy_entries),
    cache: RwLock::new(cache),
    checkpoint_compression: options.checkpoint_compression.clone(),
    sync_mode: options.sync_mode,
    group_commit_enabled: options.group_commit_enabled,
    group_commit_window_ms: options.group_commit_window_ms,
    primary_replication,
    replica_replication,
    #[cfg(feature = "bench-profile")]
    commit_lock_wait_ns: AtomicU64::new(0),
    #[cfg(feature = "bench-profile")]
    wal_flush_ns: AtomicU64::new(0),
  })
}

/// Install a header written while opening in the slot after `header_slot`,
/// durably, keeping the selected header as the fallback until it lands.
fn install_recovered_header(
  pager: &mut FilePager,
  header: &mut DbHeaderV1,
  header_slot: &mut u32,
) -> Result<()> {
  header.change_counter += 1;
  let next_header_slot = other_header_slot(*header_slot);
  write_header_slot(pager, header, next_header_slot)?;
  pager.sync()?;
  *header_slot = next_header_slot;
  Ok(())
}

fn legacy_migration_temp_path(path: &Path) -> Result<PathBuf> {
  let file_name = path.file_name().ok_or_else(|| {
    KiteError::InvalidPath(format!(
      "database path has no file name: {}",
      path.display()
    ))
  })?;
  Ok(path.with_file_name(format!("{}.migrate-tmp", file_name.to_string_lossy())))
}

fn migrate_legacy_single_header(
  path: &Path,
  options: &SingleFileOpenOptions,
  lock_file: bool,
) -> Result<()> {
  let temp_path = legacy_migration_temp_path(path)?;
  if temp_path.exists() {
    std::fs::remove_file(&temp_path)?;
  }

  // Internal migration opens must not inspect, create, or advance replication
  // sidecars. Replication cursors address sidecar segment/log positions, not
  // main-file WAL offsets; the sidecar is unchanged and the recovered logical
  // state is identical, so primary and replica cursors remain valid without a
  // forced reseed. They are reopened only after the replacement is installed.
  let mut recovery_options = options.clone();
  recovery_options.read_only = true;
  recovery_options.create_if_missing = false;
  recovery_options.replication_role = ReplicationRole::Disabled;
  recovery_options.replication_sidecar_path = None;
  recovery_options.replication_source_db_path = None;
  recovery_options.replication_source_sidecar_path = None;
  let recovered = open_single_file_internal(path, recovery_options, lock_file)?;
  let legacy_header = recovered.header.read().clone();

  let mut temp_options = options.clone();
  temp_options.read_only = false;
  temp_options.create_if_missing = true;
  temp_options.replication_role = ReplicationRole::Disabled;
  temp_options.replication_sidecar_path = None;
  temp_options.replication_source_db_path = None;
  temp_options.replication_source_sidecar_path = None;
  // Keep the legacy file's WAL size unless the caller required one (which the
  // recovery open above has already checked against the legacy header).
  if temp_options.wal_size.is_none() {
    temp_options.wal_size =
      Some(legacy_header.wal_page_count as usize * legacy_header.page_size as usize);
  }

  let temp = match open_single_file_internal(&temp_path, temp_options, lock_file) {
    Ok(db) => db,
    Err(error) => {
      let _ = std::fs::remove_file(&temp_path);
      return Err(error);
    }
  };

  // Move the state produced by the normal read-only snapshot/WAL recovery
  // path into the new database, then let the normal checkpoint API serialize
  // it. No record-level migration implementation is duplicated here.
  *temp.snapshot.write() = recovered.snapshot.write().take();
  *temp.delta.write() = std::mem::replace(&mut *recovered.delta.write(), DeltaState::new());
  *temp.label_names.write() = std::mem::take(&mut *recovered.label_names.write());
  *temp.label_ids.write() = std::mem::take(&mut *recovered.label_ids.write());
  *temp.etype_names.write() = std::mem::take(&mut *recovered.etype_names.write());
  *temp.etype_ids.write() = std::mem::take(&mut *recovered.etype_ids.write());
  *temp.propkey_names.write() = std::mem::take(&mut *recovered.propkey_names.write());
  *temp.propkey_ids.write() = std::mem::take(&mut *recovered.propkey_ids.write());
  *temp.vector_stores.write() = std::mem::take(&mut *recovered.vector_stores.write());
  *temp.vector_store_lazy_entries.write() =
    std::mem::take(&mut *recovered.vector_store_lazy_entries.write());
  temp.next_node_id.store(
    recovered.next_node_id.load(Ordering::SeqCst),
    Ordering::SeqCst,
  );
  temp.next_label_id.store(
    recovered.next_label_id.load(Ordering::SeqCst),
    Ordering::SeqCst,
  );
  temp.next_etype_id.store(
    recovered.next_etype_id.load(Ordering::SeqCst),
    Ordering::SeqCst,
  );
  temp.next_propkey_id.store(
    recovered.next_propkey_id.load(Ordering::SeqCst),
    Ordering::SeqCst,
  );
  temp.next_tx_id.store(
    recovered.next_tx_id.load(Ordering::SeqCst),
    Ordering::SeqCst,
  );

  {
    let mut header = temp.header.write();
    // Exact semantic-field audit. Layout fields (db/snapshot/WAL locations and
    // heads) intentionally belong to the fresh dual-header file, and so do the
    // format fields (version, min_reader_version, and the WAL salts): it is
    // written in the current format. Generation and change counters advance
    // once when checkpoint installs the snapshot.
    header.magic = legacy_header.magic;
    header.page_size = legacy_header.page_size;
    header.flags = legacy_header.flags;
    header.change_counter = legacy_header.change_counter;
    header.active_snapshot_gen = legacy_header.active_snapshot_gen;
    header.prev_snapshot_gen = legacy_header.prev_snapshot_gen;
    header.max_node_id = recovered
      .next_node_id
      .load(Ordering::SeqCst)
      .saturating_sub(1);
    header.next_tx_id = recovered.next_tx_id.load(Ordering::SeqCst);
    header.last_commit_ts = legacy_header.last_commit_ts;
    header.schema_cookie = legacy_header.schema_cookie;
    // db_size_pages, snapshot_start_page/count, wal_start_page/count,
    // wal_head/tail, wal_primary/secondary_head, active_wal_region,
    // checkpoint_in_progress, version, min_reader_version, and
    // wal_primary/secondary_salt are fresh-file state and remain initialized.
  }

  let migration_result = (|| {
    temp.checkpoint()?;
    drop(recovered);
    drop(temp);

    // Crash argument: before rename, the original inode is untouched and a
    // partial/complete temp is removed on the next migration attempt. The temp
    // file and its directory entry are synced before rename. After atomic
    // rename, the visible file already contains two valid headers, an empty
    // WAL, and a complete snapshot; the second directory sync persists the
    // replacement name.
    std::fs::File::open(&temp_path)?.sync_all()?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::File::open(parent)?.sync_all()?;
    std::fs::rename(&temp_path, path)?;
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
  })();

  if migration_result.is_err() {
    let _ = std::fs::remove_file(&temp_path);
  }
  migration_result
}

/// Close a single-file database using custom close options.
pub fn close_single_file_with_options(
  db: SingleFileDB,
  options: SingleFileCloseOptions,
) -> Result<()> {
  if let Some(threshold_raw) = options.checkpoint_if_wal_usage_at_least {
    if !threshold_raw.is_finite() {
      return Err(KiteError::Internal(format!(
        "invalid close checkpoint threshold: {threshold_raw}"
      )));
    }

    let threshold = threshold_raw.clamp(0.0, 1.0);
    if !db.read_only && db.should_checkpoint(threshold) {
      db.checkpoint()?;
    }
  }

  if let Some(ref mvcc) = db.mvcc {
    mvcc.stop();
  }

  if db.read_only {
    return Ok(());
  }

  // Flush WAL and sync to disk
  let mut pager = db.pager.lock();
  let mut wal_buffer = db.wal_buffer.lock();

  // Flush any pending WAL writes
  wal_buffer.flush(&mut pager)?;

  // Update header with current WAL state
  {
    let mut header = db.header.write();
    wal_buffer.store_in_header(&mut header);
    header.max_node_id = db.next_node_id.load(Ordering::SeqCst).saturating_sub(1);
    header.next_tx_id = db.next_tx_id.load(Ordering::SeqCst);

    // Install the updated header in the inactive slot. The final sync below
    // makes the WAL and header durable together.
    db.persist_header(&mut pager, &mut header, false)?;
  }

  // Final sync
  pager.sync()?;
  Ok(())
}

/// Close a single-file database with default close behavior.
pub fn close_single_file(db: SingleFileDB) -> Result<()> {
  close_single_file_with_options(db, SingleFileCloseOptions::default())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::core::header::HEADER_SLOT_COUNT;
  use crate::core::single_file::recovery::read_wal_area;
  use crate::core::single_file::{
    close_single_file, close_single_file_with_options, SingleFileCloseOptions,
  };
  use crate::core::wal::buffer::header_salt_at;
  use crate::core::wal::record::{apply_wal_salt, parse_wal_record_with_salt};
  use crate::util::binary::{align_up, read_u32};
  use std::io::Write;
  use tempfile::tempdir;

  fn legacy_migration_temp_path(path: &Path) -> PathBuf {
    let file_name = path
      .file_name()
      .expect("database file name")
      .to_string_lossy();
    path.with_file_name(format!("{file_name}.migrate-tmp"))
  }

  fn build_legacy_single_header_fixture(
    path: &Path,
  ) -> (
    SingleFileOpenOptions,
    NodeId,
    LabelId,
    PropKeyId,
    DbHeaderV1,
  ) {
    let source_path = path.with_extension("source.kitedb");
    let options = SingleFileOpenOptions::new()
      .wal_size(64 * 1024)
      .auto_checkpoint(false);
    let source = open_single_file(&source_path, options.clone()).expect("source database");

    source.begin(false).expect("begin fixture transaction");
    let label_id = source.define_label("LegacyLabel").expect("dynamic label");
    let propkey_id = source
      .define_propkey("legacy_value")
      .expect("dynamic property");
    let node_id = source
      .create_node(Some("legacy-node"))
      .expect("legacy node");
    source
      .add_node_label(node_id, label_id)
      .expect("legacy label assignment");
    source
      .set_node_prop(node_id, propkey_id, PropValue::I64(42))
      .expect("legacy property");
    source.commit().expect("commit fixture transaction");
    source.checkpoint().expect("checkpoint fixture source");

    // Keep a committed update only in the legacy WAL so migration exercises
    // the normal snapshot + WAL recovery path rather than snapshot copying.
    source.begin(false).expect("begin WAL-only transaction");
    source
      .set_node_prop(node_id, propkey_id, PropValue::I64(84))
      .expect("WAL-only property update");
    source.commit().expect("commit WAL-only transaction");

    let current_header = source.header.read().clone();
    let (mut wal_bytes, snapshot_bytes) = {
      let mut pager = source.pager.lock();
      let mut wal = Vec::new();
      for page in 0..current_header.wal_page_count as u32 {
        wal.extend_from_slice(
          &pager
            .read_page(current_header.wal_start_page as u32 + page)
            .expect("WAL page"),
        );
      }
      let mut snapshot = Vec::new();
      for page in 0..current_header.snapshot_page_count as u32 {
        snapshot.extend_from_slice(
          &pager
            .read_page(current_header.snapshot_start_page as u32 + page)
            .expect("snapshot page"),
        );
      }
      (wal, snapshot)
    };

    // Legacy single-header files are format 1: unsalted WAL records.
    let live_wal = current_header.wal_tail as usize..current_header.wal_head as usize;
    assert!(
      apply_wal_salt(&mut wal_bytes[live_wal], current_header.wal_primary_salt),
      "unsalt the live WAL records"
    );
    let mut legacy_header = current_header;
    legacy_header.version = 1;
    legacy_header.min_reader_version = 1;
    legacy_header.wal_primary_salt = 0;
    legacy_header.wal_secondary_salt = 0;
    legacy_header.wal_start_page = 1;
    legacy_header.snapshot_start_page = 1 + legacy_header.wal_page_count;
    legacy_header.db_size_pages =
      legacy_header.snapshot_start_page + legacy_header.snapshot_page_count;

    let mut fixture = std::fs::File::create(path).expect("legacy fixture file");
    fixture
      .write_all(&legacy_header.serialize_to_page())
      .expect("legacy header");
    fixture.write_all(&wal_bytes).expect("legacy WAL");
    fixture.write_all(&snapshot_bytes).expect("legacy snapshot");
    fixture.sync_all().expect("legacy fixture sync");
    drop(source);

    (options, node_id, label_id, propkey_id, legacy_header)
  }

  fn corrupt_last_wal_record(db: &SingleFileDB) {
    let mut pager = db.pager.lock();
    let header = db.header.read().clone();
    let wal_data = read_wal_area(&mut pager, &header).expect("expected value");
    let mut pos = header.wal_tail as usize;
    let head = header.wal_head as usize;
    let mut last_start = None;

    while pos < head {
      let rec_len = read_u32(&wal_data, pos) as usize;
      if rec_len == 0 {
        break;
      }
      if parse_wal_record_with_salt(&wal_data, pos, header_salt_at(&header, pos as u64)).is_none() {
        break;
      }
      last_start = Some(pos);
      let aligned_size = align_up(rec_len, WAL_RECORD_ALIGNMENT);
      pos += aligned_size;
    }

    let last_start = last_start.expect("wal record");
    let rec_len = read_u32(&wal_data, last_start) as usize;
    let crc_offset = last_start + rec_len - 4;

    let wal_start = header.wal_start_page as usize * header.page_size as usize;
    let file_offset = wal_start + crc_offset;
    let page_size = header.page_size as usize;
    let page_num = (file_offset / page_size) as u32;
    let page_offset = file_offset % page_size;

    if page_offset + 4 <= page_size {
      let mut page = pager.read_page(page_num).expect("expected value");
      page[page_offset..page_offset + 4].fill(0);
      pager.write_page(page_num, &page).expect("expected value");
    } else {
      let first_len = page_size - page_offset;
      let mut page = pager.read_page(page_num).expect("expected value");
      page[page_offset..].fill(0);
      pager.write_page(page_num, &page).expect("expected value");

      let mut next_page = pager.read_page(page_num + 1).expect("expected value");
      next_page[..(4 - first_len)].fill(0);
      pager
        .write_page(page_num + 1, &next_page)
        .expect("expected value");
    }

    pager.sync().expect("expected value");
  }

  #[test]
  fn test_recover_incomplete_background_checkpoint() {
    let temp_dir = tempdir().expect("expected value");
    let db_path = temp_dir.path().join("checkpoint-recover.kitedb");

    let db = open_single_file(&db_path, SingleFileOpenOptions::new()).expect("expected value");

    // Write a primary WAL record
    db.begin(false).expect("expected value");
    let _n1 = db.create_node(Some("n1")).expect("expected value");
    db.commit().expect("expected value");

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
      pager.write_page(0, &header_bytes).expect("expected value");
      pager.sync().expect("expected value");
    }

    // Write to secondary WAL region
    db.begin(false).expect("expected value");
    let _n2 = db.create_node(Some("n2")).expect("expected value");
    db.commit().expect("expected value");

    close_single_file(db).expect("expected value");

    // Reopen and ensure both records are recovered
    let db = open_single_file(&db_path, SingleFileOpenOptions::new()).expect("expected value");
    assert!(db.node_by_key("n1").is_some());
    assert!(db.node_by_key("n2").is_some());
    close_single_file(db).expect("expected value");
  }

  #[test]
  fn test_group_commit_flush_and_persist() {
    let temp_dir = tempdir().expect("expected value");
    let db_path = temp_dir.path().join("group-commit.kitedb");

    let db = open_single_file(
      &db_path,
      SingleFileOpenOptions::new()
        .sync_mode(SyncMode::Normal)
        .group_commit_enabled(true)
        .group_commit_window_ms(0),
    )
    .expect("expected value");

    db.begin(false).expect("expected value");
    let node_id = db.create_node(Some("n1")).expect("expected value");
    let key_id = db.define_propkey("value").expect("expected value");
    db.set_node_prop(node_id, key_id, crate::types::PropValue::I64(42))
      .expect("expected value");
    db.commit().expect("expected value");

    assert!(!db.wal_buffer.lock().has_pending_writes());

    close_single_file(db).expect("expected value");

    let reopened = open_single_file(
      &db_path,
      SingleFileOpenOptions::new()
        .sync_mode(SyncMode::Normal)
        .group_commit_enabled(true)
        .group_commit_window_ms(0),
    )
    .expect("expected value");

    let value = reopened.node_prop(node_id, key_id).expect("prop value");
    assert_eq!(value, crate::types::PropValue::I64(42));

    close_single_file(reopened).expect("expected value");
  }

  #[test]
  fn test_replication_source_opens_primaries_with_any_wal_size() {
    // Replica bootstrap opens the primary with default options; that must not
    // assume the default WAL size.
    let temp_dir = tempdir().expect("expected value");
    let db_path = temp_dir.path().join("replication-source.kitedb");
    let db = open_single_file(&db_path, SingleFileOpenOptions::new().wal_size(64 * 1024))
      .expect("expected value");
    close_single_file(db).expect("expected value");

    let source = open_replication_source(&db_path).expect("open replication source");
    drop(source);
  }

  #[test]
  fn test_open_rejects_wal_size_mismatch() {
    let temp_dir = tempdir().expect("expected value");
    let db_path = temp_dir.path().join("wal-size-mismatch.kitedb");

    let db = open_single_file(&db_path, SingleFileOpenOptions::new().wal_size(64 * 1024))
      .expect("expected value");
    close_single_file(db).expect("expected value");

    let reopen = open_single_file(
      &db_path,
      SingleFileOpenOptions::new().wal_size(64 * 1024 * 1024),
    );

    assert!(reopen.is_err(), "expected wal size mismatch to error");
  }

  #[test]
  fn writable_open_migrates_legacy_single_header_database() {
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("legacy.kitedb");
    let (options, node_id, label_id, propkey_id, legacy_header) =
      build_legacy_single_header_fixture(&db_path);

    let migrated = open_single_file(&db_path, options.clone()).expect("automatic migration");
    assert_eq!(migrated.node_by_key("legacy-node"), Some(node_id));
    assert_eq!(
      migrated.node_prop(node_id, propkey_id),
      Some(PropValue::I64(84))
    );
    assert!(migrated.node_labels(node_id).contains(&label_id));
    assert_eq!(migrated.label_id("LegacyLabel"), Some(label_id));
    assert_eq!(migrated.propkey_id("legacy_value"), Some(propkey_id));
    assert!(migrated.header.read().wal_start_page >= HEADER_SLOT_COUNT);
    assert_eq!(
      migrated.header.read().max_node_id,
      legacy_header.max_node_id
    );
    // The migrated file is written in the current format.
    assert_eq!(migrated.header.read().version, VERSION_SINGLE_FILE);
    assert_eq!(
      migrated.header.read().min_reader_version,
      MIN_READER_SINGLE_FILE
    );
    assert_ne!(migrated.header.read().wal_primary_salt, 0);
    assert!(migrated.header.read().next_tx_id >= legacy_header.next_tx_id);

    migrated.begin(false).expect("post-migration transaction");
    migrated
      .create_node(Some("after-migration"))
      .expect("post-migration node");
    migrated.commit().expect("post-migration commit");
    close_single_file(migrated).expect("close migrated database");

    let reopened = open_single_file(&db_path, options).expect("reopen migrated database");
    assert!(reopened.node_by_key("legacy-node").is_some());
    assert!(reopened.node_by_key("after-migration").is_some());
    close_single_file(reopened).expect("close reopened database");
  }

  #[test]
  fn legacy_migration_without_explicit_wal_size_keeps_the_files_wal_size() {
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("legacy-default-options.kitedb");
    let (_, node_id, _, _, legacy_header) = build_legacy_single_header_fixture(&db_path);

    let options = SingleFileOpenOptions::new().auto_checkpoint(false);
    let migrated = open_single_file(&db_path, options.clone()).expect("automatic migration");
    assert_eq!(migrated.node_by_key("legacy-node"), Some(node_id));
    assert_eq!(
      migrated.header.read().wal_page_count,
      legacy_header.wal_page_count
    );
    close_single_file(migrated).expect("close migrated database");

    let reopened = open_single_file(&db_path, options).expect("reopen migrated database");
    assert_eq!(
      reopened.header.read().wal_page_count,
      legacy_header.wal_page_count
    );
    close_single_file(reopened).expect("close reopened database");
  }

  #[test]
  fn stale_migration_temp_is_cleaned_before_legacy_retry() {
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("legacy-stale-temp.kitedb");
    let (options, node_id, _, _, _) = build_legacy_single_header_fixture(&db_path);
    let migration_temp = legacy_migration_temp_path(&db_path);
    std::fs::write(&migration_temp, b"incomplete migration").expect("stale migration temp");

    let migrated = open_single_file(&db_path, options).expect("migration retry");
    assert_eq!(migrated.node_by_key("legacy-node"), Some(node_id));
    assert!(!migration_temp.exists());
    close_single_file(migrated).expect("close migrated database");
  }

  #[test]
  fn test_recover_checkpoint_with_partial_header_update() {
    let temp_dir = tempdir().expect("expected value");
    let db_path = temp_dir
      .path()
      .join("checkpoint-recover-partial-header.kitedb");

    let db = open_single_file(&db_path, SingleFileOpenOptions::new()).expect("expected value");

    // Write a primary WAL record
    db.begin(false).expect("expected value");
    let _n1 = db.create_node(Some("n1")).expect("expected value");
    db.commit().expect("expected value");

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
      pager.write_page(0, &header_bytes).expect("expected value");
      pager.sync().expect("expected value");
    }

    // Write to secondary WAL region
    db.begin(false).expect("expected value");
    let _n2 = db.create_node(Some("n2")).expect("expected value");
    db.commit().expect("expected value");

    // Simulate an interrupted header update: wal_head advanced, secondary head missing
    {
      let mut pager = db.pager.lock();
      let mut wal = db.wal_buffer.lock();
      wal.flush(&mut pager).expect("expected value");
      let mut header = db.header.write();

      header.active_wal_region = 1;
      header.checkpoint_in_progress = 1;
      header.wal_primary_head = wal.primary_head();
      header.wal_head = wal.head();
      header.wal_tail = wal.tail();
      header.wal_secondary_head = wal.primary_region_size();
      header.change_counter += 1;

      let header_bytes = header.serialize_to_page();
      pager.write_page(0, &header_bytes).expect("expected value");
      pager.sync().expect("expected value");
    }

    // Simulate crash by dropping without close
    drop(db);

    // Reopen and ensure both records are recovered
    let db = open_single_file(&db_path, SingleFileOpenOptions::new()).expect("expected value");
    assert!(db.node_by_key("n1").is_some());
    assert!(db.node_by_key("n2").is_some());
    close_single_file(db).expect("expected value");
  }

  #[test]
  fn test_recover_checkpoint_with_missing_primary_head() {
    let temp_dir = tempdir().expect("expected value");
    let db_path = temp_dir
      .path()
      .join("checkpoint-recover-missing-primary-head.kitedb");

    let db = open_single_file(&db_path, SingleFileOpenOptions::new()).expect("expected value");

    // Write a primary WAL record
    db.begin(false).expect("expected value");
    let _n1 = db.create_node(Some("n1")).expect("expected value");
    db.commit().expect("expected value");

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
      pager.write_page(0, &header_bytes).expect("expected value");
      pager.sync().expect("expected value");
    }

    drop(db);

    let db = open_single_file(&db_path, SingleFileOpenOptions::new()).expect("expected value");
    assert!(db.node_by_key("n1").is_some());
    close_single_file(db).expect("expected value");
  }

  #[test]
  fn test_recover_wal_with_truncated_record() {
    let temp_dir = tempdir().expect("expected value");
    let db_path = temp_dir.path().join("wal-truncated.kitedb");

    let db = open_single_file(&db_path, SingleFileOpenOptions::new()).expect("expected value");

    db.begin(false).expect("expected value");
    let _n1 = db.create_node(Some("n1")).expect("expected value");
    db.commit().expect("expected value");

    db.begin(false).expect("expected value");
    let _n2 = db.create_node(Some("n2")).expect("expected value");
    db.commit().expect("expected value");

    corrupt_last_wal_record(&db);
    drop(db);

    let db = open_single_file(&db_path, SingleFileOpenOptions::new()).expect("expected value");
    assert!(db.node_by_key("n1").is_some());
    assert!(db.node_by_key("n2").is_none());
    close_single_file(db).expect("expected value");
  }

  #[test]
  fn test_recover_ignores_uncommitted_transaction() {
    let temp_dir = tempdir().expect("expected value");
    let db_path = temp_dir.path().join("wal-uncommitted.kitedb");

    let db = open_single_file(&db_path, SingleFileOpenOptions::new()).expect("expected value");

    db.begin(false).expect("expected value");
    let _n1 = db.create_node(Some("n1")).expect("expected value");

    // Persist WAL head without a commit record
    {
      let mut pager = db.pager.lock();
      let wal = db.wal_buffer.lock();
      let mut header = db.header.write();

      header.wal_head = wal.head();
      header.wal_tail = wal.tail();
      header.wal_primary_head = wal.primary_head();
      header.wal_secondary_head = wal.secondary_head();
      header.active_wal_region = wal.active_region();
      header.change_counter += 1;

      let header_bytes = header.serialize_to_page();
      pager.write_page(0, &header_bytes).expect("expected value");
      pager.sync().expect("expected value");
    }

    drop(db);

    let db = open_single_file(&db_path, SingleFileOpenOptions::new()).expect("expected value");
    assert!(db.node_by_key("n1").is_none());
    close_single_file(db).expect("expected value");
  }

  #[test]
  fn test_recovery_replays_commits_in_wal_order() {
    let temp_dir = tempdir().expect("expected value");
    let db_path = temp_dir.path().join("wal-commit-order.kitedb");
    let options = SingleFileOpenOptions::new().auto_checkpoint(false);

    let db = open_single_file(&db_path, options.clone()).expect("expected value");

    db.begin(false).expect("expected value");
    let node_id = db.create_node(None).expect("expected value");
    let key_id = db.define_propkey("value").expect("expected value");
    db.set_node_prop(node_id, key_id, crate::types::PropValue::I64(0))
      .expect("expected value");
    db.commit().expect("expected value");

    for value in 1..=64 {
      db.begin(false).expect("expected value");
      db.set_node_prop(node_id, key_id, crate::types::PropValue::I64(value))
        .expect("expected value");
      db.commit().expect("expected value");
    }

    // Simulate a crash: committed WAL is durable, but no checkpoint is run.
    drop(db);

    // Reopen repeatedly so a HashMap iteration that happens to be ordered once
    // cannot make this regression pass by luck.
    for _ in 0..8 {
      let reopened = open_single_file(&db_path, options.clone()).expect("expected value");
      assert_eq!(
        reopened.node_prop(node_id, key_id),
        Some(crate::types::PropValue::I64(64))
      );
      drop(reopened);
    }
  }

  #[test]
  fn test_checkpoint_replay_after_crash() {
    let temp_dir = tempdir().expect("expected value");
    let db_path = temp_dir.path().join("checkpoint-replay.kitedb");

    let db = open_single_file(&db_path, SingleFileOpenOptions::new()).expect("expected value");

    db.begin(false).expect("expected value");
    let _n1 = db.create_node(Some("n1")).expect("expected value");
    db.commit().expect("expected value");

    db.checkpoint().expect("expected value");

    db.begin(false).expect("expected value");
    let _n2 = db.create_node(Some("n2")).expect("expected value");
    db.commit().expect("expected value");

    drop(db);

    let db = open_single_file(&db_path, SingleFileOpenOptions::new()).expect("expected value");
    assert!(db.node_by_key("n1").is_some());
    assert!(db.node_by_key("n2").is_some());
    close_single_file(db).expect("expected value");
  }

  #[test]
  fn test_close_with_checkpoint_if_wal_over_clears_wal() {
    let temp_dir = tempdir().expect("expected value");
    let db_path = temp_dir.path().join("close-with-checkpoint.kitedb");

    let db = open_single_file(
      &db_path,
      SingleFileOpenOptions::new().auto_checkpoint(false),
    )
    .expect("expected value");

    db.begin(false).expect("expected value");
    let _ = db.create_node(Some("n1")).expect("expected value");
    db.commit().expect("expected value");
    assert!(db.should_checkpoint(0.0));

    close_single_file_with_options(
      db,
      SingleFileCloseOptions::new().checkpoint_if_wal_usage_at_least(0.0),
    )
    .expect("expected value");

    let reopened = open_single_file(
      &db_path,
      SingleFileOpenOptions::new().auto_checkpoint(false),
    )
    .expect("expected value");
    let header = reopened.header.read().clone();
    assert_eq!(header.wal_head, 0);
    assert_eq!(header.wal_tail, 0);
    close_single_file(reopened).expect("expected value");
  }

  #[test]
  fn test_close_with_high_threshold_keeps_wal() {
    let temp_dir = tempdir().expect("expected value");
    let db_path = temp_dir.path().join("close-without-checkpoint.kitedb");

    let db = open_single_file(
      &db_path,
      SingleFileOpenOptions::new().auto_checkpoint(false),
    )
    .expect("expected value");

    db.begin(false).expect("expected value");
    let _ = db.create_node(Some("n1")).expect("expected value");
    db.commit().expect("expected value");

    close_single_file_with_options(
      db,
      SingleFileCloseOptions::new().checkpoint_if_wal_usage_at_least(1.0),
    )
    .expect("expected value");

    let reopened = open_single_file(
      &db_path,
      SingleFileOpenOptions::new().auto_checkpoint(false),
    )
    .expect("expected value");
    let header = reopened.header.read().clone();
    assert!(header.wal_head > 0);
    close_single_file(reopened).expect("expected value");
  }
}
