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

use crate::constants::*;
use crate::core::header::{
  other_header_slot, read_header_slots_with_fallback, write_header_slot, HEADER_SLOT_A,
  HEADER_SLOT_B, HEADER_SLOT_COUNT,
};
use crate::core::pager::{
  create_pager_with_locking, is_valid_page_size, open_pager_with_locking, pages_to_store,
  FilePager, NewPager,
};
use crate::core::snapshot::reader::SnapshotData;
use crate::core::wal::buffer::WalBuffer;
use crate::error::{KiteError, Result};
use crate::mvcc::{GcConfig, MvccManager};
use crate::replication::durability::SidecarSync;
use crate::replication::primary::PrimaryReplication;
use crate::replication::replica::ReplicaReplication;
use crate::replication::types::ReplicationRole;
use crate::types::*;
use crate::util::compression::CompressionOptions;
use crate::util::fs::sync_parent_dir;
use crate::util::mmap::Mmap;

use super::recovery::{
  committed_transactions_after, drop_vectors_of_missing_nodes, replay_wal_record, scan_wal_records,
};
use super::segments::{finish_cut_into_segment, read_wal_segment_log, spilled_transactions_in_log};
use super::vector::{apply_replayed_vectors, vector_store_state_from_snapshot};
use super::{BackgroundCheckpointState, SchemaReservations, SingleFileDB, SingleFileInner};

// ============================================================================
// Open Options
// ============================================================================

/// Synchronization mode for WAL writes
///
/// Controls the durability vs performance trade-off for commits.
/// Similar to SQLite's PRAGMA synchronous setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SyncMode {
  /// Fsync on every commit (durable to OS, slowest).
  /// On macOS, fsync leaves writes in the drive's volatile cache, so without
  /// [`SingleFileOpenOptions::full_fsync`] this mode does not survive power
  /// loss there (the same as SQLite's default).
  #[default]
  Full,

  /// Fsync only on checkpoint (balanced)
  /// WAL writes are buffered in OS cache. Data may be lost if OS crashes,
  /// but not if application crashes. ~1000x faster than Full.
  Normal,

  /// No fsync, and no WAL write per commit (fastest, least safe).
  ///
  /// **Commits stay in memory until a checkpoint, `close_single_file`, or
  /// dropping the handle writes them.** A crash of the process, not just of
  /// the OS, loses every commit since the last checkpoint. Only for tests
  /// and data you can rebuild.
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
  /// MVCC: snapshot-isolated transactions and conflict detection between
  /// concurrent write transactions (default: true). It is runtime state
  /// only: the file format is the same either way.
  ///
  /// `false` is deprecated and will be removed in a later release. Without
  /// MVCC, write transactions run one at a time and transactions read the
  /// latest committed state instead of a snapshot.
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
  /// Checkpoint automatically once the log (the WAL and its WAL segments)
  /// reaches the checkpoint trigger (default true; see
  /// `checkpoint_log_ratio`). Without, the WAL spills into WAL segments until
  /// `wal_segment_limit`, and then writes fail with `WalBufferFull` until a
  /// checkpoint.
  pub auto_checkpoint: bool,
  /// Has no effect: automatic checkpoints follow the log, not the WAL's
  /// usage (see `checkpoint_log_ratio` and `checkpoint_log_budget`). Still
  /// accepted so existing callers keep compiling.
  #[deprecated(
    note = "has no effect: automatic checkpoints follow the log; see checkpoint_log_ratio and \
            checkpoint_log_budget"
  )]
  pub checkpoint_threshold: f64,
  /// Automatic checkpoints run while writes go on (default true). Without,
  /// they are blocking: one runs after the commit that crosses the trigger
  /// (waiting for open transactions), and a writer at `wal_segment_limit`
  /// fails instead of waiting, with `WalBufferFull` (`CheckpointFailed`
  /// while the last automatic checkpoint failed); one runs once its
  /// transaction ends, by commit or rollback (after the back-off, if the
  /// last one failed).
  pub background_checkpoint: bool,
  /// Run automatic background checkpoints on a thread of the database's
  /// own, so the commit that crosses the trigger returns at once (default
  /// true). Without it they run on the committing thread. Ignored by
  /// read-only opens, without `background_checkpoint`, and on wasm32.
  pub checkpoint_thread: bool,
  /// An automatic checkpoint starts once the log the snapshot does not cover
  /// (WAL segments and WAL) reaches this fraction of the snapshot's size
  /// (default 0.5), within limits: at least three eighths of the WAL (half
  /// its region: where earlier releases checkpointed by default, so a small
  /// database holds no more log than it did), at most
  /// `checkpoint_log_budget`.
  pub checkpoint_log_ratio: f64,
  /// The most log, in bytes, an automatic checkpoint waits for (default
  /// 128 MiB; it caps the trigger's floor too). The delta that holds the
  /// log's commits in memory takes about ten times the log's size, so this
  /// bounds that memory while checkpoints keep up. Writers that outrun them
  /// grow the log up to `wal_segment_limit` (by default twice the checkpoint
  /// trigger, at least 16 WALs, at most four times this), and wait for a
  /// checkpoint only there.
  pub checkpoint_log_budget: u64,
  /// Bytes of a WAL segment extent: spills of the WAL fill one before the
  /// next is allocated (default: a sixteenth of the segment limit, from two
  /// WALs to the larger of 32 MiB and two WALs; at least one and a half
  /// WALs and the records it is made for). An open transaction
  /// whose records spilled keeps the extent it began in, records written
  /// before it included, until a checkpoint covers its commit, so extents
  /// small next to the limit keep such pins small.
  pub wal_segment_size: Option<u64>,
  /// The most bytes of WAL segments: past it the WAL spills no more, and
  /// writers wait for a checkpoint (or fail: with `WalBufferFull` without
  /// automatic checkpoints, and with blocking ones as `background_checkpoint`
  /// says). Default: twice the
  /// checkpoint trigger, at least 16 WALs, at most four times
  /// `checkpoint_log_budget`. The segment table caps the segments at 63
  /// extents too: with default extents, about four times a limit up to
  /// 512 MiB, and 2 GiB beyond; with `wal_segment_size` set, 63 of it.
  pub wal_segment_limit: Option<u64>,
  /// Has no effect. The cache layer was removed: no read ever consulted it,
  /// and reads are served from the snapshot and delta. Still accepted so
  /// existing callers keep compiling.
  #[deprecated(note = "has no effect: the cache layer was removed")]
  #[allow(deprecated)]
  pub cache: Option<CacheOptions>,
  /// Compression options for checkpoint snapshots
  pub checkpoint_compression: Option<CompressionOptions>,
  /// Synchronization mode for WAL writes (default: Full)
  pub sync_mode: SyncMode,
  /// macOS only: with `SyncMode::Full`, sync with `F_FULLFSYNC` so commits
  /// survive power loss (default false). See [`Self::full_fsync`].
  pub full_fsync: bool,
  /// Has no effect, kept for compatibility: every commit is group-committed.
  /// Commits that arrive while others are written are written together, in
  /// every sync mode: one WAL write, one fsync in `SyncMode::Full`, and one
  /// header write for the group. A single writer pays nothing for it.
  pub group_commit_enabled: bool,
  /// Has no effect, kept for compatibility: no commit waits for others to
  /// join its group; those arriving while a group is written form the next
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
  #[allow(deprecated)]
  fn default() -> Self {
    Self {
      read_only: false,
      create_if_missing: true,
      mvcc: true,
      mvcc_gc_interval_ms: None,
      mvcc_retention_ms: None,
      mvcc_max_chain_depth: None,
      page_size: DEFAULT_PAGE_SIZE,
      wal_size: None,
      auto_checkpoint: true,
      checkpoint_threshold: 0.5,
      background_checkpoint: true,
      checkpoint_thread: true,
      checkpoint_log_ratio: CHECKPOINT_LOG_RATIO_DEFAULT,
      checkpoint_log_budget: CHECKPOINT_LOG_BUDGET_DEFAULT,
      wal_segment_size: None,
      wal_segment_limit: None,
      cache: None,
      checkpoint_compression: Some(CompressionOptions {
        enabled: true,
        ..Default::default()
      }),
      sync_mode: SyncMode::Full,
      full_fsync: false,
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

  /// Enable or disable MVCC (default: enabled; see the `mvcc` field).
  ///
  /// `mvcc(false)` is deprecated and will be removed in a later release. It
  /// is not marked `#[deprecated]` because the setter also takes `true`, and
  /// a warning on every call would break `-D warnings` builds.
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

  /// Has no effect; see [`SingleFileOpenOptions::checkpoint_threshold`].
  #[deprecated(
    note = "has no effect: automatic checkpoints follow the log; see checkpoint_log_ratio and \
            checkpoint_log_budget"
  )]
  #[allow(deprecated)]
  pub fn checkpoint_threshold(mut self, value: f64) -> Self {
    self.checkpoint_threshold = value.clamp(0.0, 1.0);
    self
  }

  pub fn background_checkpoint(mut self, value: bool) -> Self {
    self.background_checkpoint = value;
    self
  }

  /// Run automatic background checkpoints on the database's checkpoint
  /// thread (default true); see the `checkpoint_thread` field.
  pub fn checkpoint_thread(mut self, value: bool) -> Self {
    self.checkpoint_thread = value;
    self
  }

  /// Checkpoint once the uncovered log reaches `value` times the snapshot's
  /// size (default 0.5; a finite number, at least 0); see the
  /// `checkpoint_log_ratio` field.
  pub fn checkpoint_log_ratio(mut self, value: f64) -> Self {
    self.checkpoint_log_ratio = value;
    self
  }

  /// The most log, in bytes, an automatic checkpoint waits for (default
  /// 128 MiB, more than 0); see the `checkpoint_log_budget` field.
  pub fn checkpoint_log_budget(mut self, bytes: u64) -> Self {
    self.checkpoint_log_budget = bytes;
    self
  }

  /// Bytes of a WAL segment extent (more than 0); see the
  /// `wal_segment_size` field.
  pub fn wal_segment_size(mut self, bytes: u64) -> Self {
    self.wal_segment_size = Some(bytes);
    self
  }

  /// The most bytes of WAL segments (more than 0); see the
  /// `wal_segment_limit` field.
  pub fn wal_segment_limit(mut self, bytes: u64) -> Self {
    self.wal_segment_limit = Some(bytes);
    self
  }

  /// Has no effect; see [`SingleFileOpenOptions::cache`].
  #[deprecated(note = "has no effect: the cache layer was removed")]
  #[allow(deprecated)]
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

  /// Has no effect; see [`SingleFileOpenOptions::cache`].
  #[deprecated(note = "has no effect: the cache layer was removed")]
  #[allow(deprecated)]
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

  /// macOS only: make `SyncMode::Full` durable against power loss (default
  /// false), like SQLite's `PRAGMA fullfsync`.
  ///
  /// On macOS, fsync(2) hands writes to the drive, whose volatile cache can
  /// lose them, or persist a header before the pages it names, if power
  /// fails. With this option every sync in `SyncMode::Full` uses
  /// `F_FULLFSYNC`, which flushes that cache too (falling back to fsync on
  /// file systems without it). It is much slower: milliseconds per commit
  /// instead of tens of microseconds. Without it, Full mode on macOS survives
  /// application and OS crashes but not power loss, the same as SQLite's
  /// default. Other modes, and other platforms, are unaffected.
  pub fn full_fsync(mut self, value: bool) -> Self {
    self.full_fsync = value;
    self
  }

  /// Has no effect (see the `group_commit_enabled` field)
  pub fn group_commit_enabled(mut self, value: bool) -> Self {
    self.group_commit_enabled = value;
    self
  }

  /// Has no effect (see the `group_commit_window_ms` field)
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

  /// Set sync mode to Off: no fsync, and commits stay in memory until a
  /// checkpoint, close, or drop writes them, so a process crash loses every
  /// commit since the last checkpoint. Only for testing or ephemeral data.
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
  /// If set, run a blocking checkpoint before close when the log the
  /// snapshot does not cover (WAL segments and WAL) is at least this
  /// fraction of the checkpoint trigger (see `SingleFileDB::should_checkpoint`),
  /// so the next open replays less. Threshold is clamped to [0.0, 1.0]. (A
  /// clean close checkpoints WAL segments away whatever this says; this
  /// covers a log that is only in the WAL.)
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

/// The snapshot range `header` names, mapped or copied as
/// [`FilePager::map_immutable_range`] decides.
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

  Ok(Arc::new(pager.map_immutable_range(offset, length)?))
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

fn init_mvcc(
  options: &SingleFileOpenOptions,
  next_tx_id: TxId,
  next_commit_ts: u64,
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

  // The replayed commits need no version history: no transaction is open
  // yet, and every one that begins sees them all.
  let mvcc = std::sync::Arc::new(MvccManager::new(next_tx_id, next_commit_ts, gc_config));
  // A read-only handle commits nothing, so it never has version history or
  // commit times for GC to collect: no GC thread to start on open and join
  // on close.
  if !options.read_only {
    mvcc.start();
  }
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
  validate_checkpoint_options(&options)?;

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
  let (mut pager, mut header, is_new, mut header_slot, fallback_header, in_current_magic) =
    if file_exists {
      // Open existing database
      let mut pager =
        open_pager_with_locking(path, options.page_size, options.read_only, lock_file)?;
      pager.set_full_fsync(options.full_fsync && options.sync_mode == SyncMode::Full);

      // Read both independently checksummed header pages and select the newest
      // valid generation. A torn newest slot falls back to the other slot.
      let slots = read_header_slots_with_fallback(&mut pager)?;
      let (header, header_slot, fallback_header) = (slots.header, slots.slot, slots.fallback);

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

      (
        pager,
        header,
        false,
        header_slot,
        fallback_header,
        slots.in_current_magic,
      )
    } else {
      // Create new database. If another opener created one here since the
      // existence check above, open that one instead.
      let mut pager = match create_pager_with_locking(path, options.page_size, lock_file)? {
        NewPager::Created(pager) => pager,
        NewPager::Exists => return open_single_file_internal(path, options, lock_file),
      };
      pager.set_full_fsync(options.full_fsync && options.sync_mode == SyncMode::Full);

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

      (pager, header, true, HEADER_SLOT_A, None, true)
    };

  // Initialize WAL buffer
  // Fails if the header's WAL positions lie outside their regions.
  let mut wal_buffer = WalBuffer::from_header(&header)?;
  if is_new {
    wal_buffer.note_created_zeroed();
  }

  // A version 2 background checkpoint's cut that no install finished (this
  // version never cuts into the secondary region). Replay reads both regions
  // in place, primary first, unless a writable open merges them.
  let mut replay_cut_in_place = header.checkpoint_in_progress != 0;
  if !options.read_only {
    // One pass over each region does both checks. Records of a type this
    // version does not know are a newer version's, not torn: refuse rather
    // than trim or compact them away below. And a crash during a commit's
    // sync can leave a durable header naming WAL bytes that never landed.
    // Replay stops at them, so drop them before anything is appended after
    // them, out of replay's reach. (A retired primary region is compacted
    // below, which keeps only readable records.)
    if wal_buffer.check_and_trim(&mut pager)? {
      wal_buffer.store_in_header(&mut header);
      install_recovered_header(&mut pager, &mut header, &mut header_slot)?;
    }

    // Finish an interrupted background checkpoint. Each branch leaves the WAL
    // bytes synced before a header naming them is installed in the other
    // slot, so the selected header stays the crash fallback.
    let rebuilt_wal = if replay_cut_in_place {
      // If the secondary region's records do not fit after the primary
      // region's, the cut goes to a WAL segment instead.
      if !wal_buffer.merge_cut_into_primary(&mut pager)? {
        finish_cut_into_segment(&mut pager, &mut wal_buffer, &mut header, &mut header_slot)?;
      }
      replay_cut_in_place = false;
      true
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

    if !is_new {
      reclaim_unnamed_pages(&mut pager, &header, fallback_header.as_ref());
    }
  }

  // Initialize ID allocators from header
  let mut next_node_id = INITIAL_NODE_ID;
  let mut next_label_id = INITIAL_LABEL_ID;
  let mut next_etype_id = INITIAL_ETYPE_ID;
  let mut next_propkey_id = INITIAL_PROPKEY_ID;
  // Not raised by replay: every header write that names log records stores
  // the next transaction id then, at least every id in them (a commit
  // round's, a spill's, an install's, a close's; a write that only drops
  // segments keeps the last one's).
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

  // The transactions that commit after the covered segments with records in
  // a segment: the segments they hold stay needed (see `spilled_txids`).
  let mut spilled_txids = HashMap::new();

  // Replay WAL for recovery (if not a new database)
  let mut _wal_records_storage: Option<Vec<crate::core::wal::record::ParsedWalRecord>>;
  if !is_new && (header.wal_head > 0 || !header.wal_segments.is_empty()) {
    #[cfg(feature = "bench-profile")]
    let wal_scan_started = Instant::now();
    // The log: the WAL segments' records, then the WAL's.
    let segments = read_wal_segment_log(&pager, &header)?;
    let covered_records = segments.covered_records;
    let mut log = segments.records;
    if header.wal_head > 0 {
      log.extend(if replay_cut_in_place {
        wal_buffer.records_for_recovery(&mut pager)?
      } else {
        scan_wal_records(&mut pager, &header)?
      });
    }
    _wal_records_storage = Some(log);
    #[cfg(feature = "bench-profile")]
    {
      open_profile.wal_scan_ns = open_profile
        .wal_scan_ns
        .saturating_add(elapsed_ns(wal_scan_started));
    }
    if let Some(ref wal_records) = _wal_records_storage {
      spilled_txids =
        spilled_transactions_in_log(wal_records, &segments.segment_ends, covered_records);
      // Transactions committed in segments the snapshot covers are in it.
      committed_in_order = committed_transactions_after(wal_records, covered_records);

      // Replay committed transactions
      #[cfg(feature = "bench-profile")]
      let wal_replay_started = Instant::now();
      let mut skipped = 0usize;
      for (_txid, records) in &committed_in_order {
        for record in records {
          let applied = replay_wal_record(
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
          )?;
          skipped += usize::from(!applied);
        }
        next_commit_ts += 1;
      }
      if skipped > 0 {
        eprintln!(
          "Warning: WAL replay of {} skipped {skipped} vector maintenance records \
           (BatchVectors, SealFragment, CompactFragments), which no version applies",
          path.display()
        );
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
  apply_replayed_vectors(
    std::mem::take(&mut delta.pending_vectors),
    &committed_in_order,
    snapshot.as_ref(),
    &mut vector_stores,
    &mut vector_store_lazy_entries,
  )?;
  #[cfg(feature = "bench-profile")]
  {
    open_profile.vector_init_ns = open_profile
      .vector_init_ns
      .saturating_add(elapsed_ns(vector_init_started));
  }

  // Initialize MVCC if enabled (after WAL replay)
  let mvcc = init_mvcc(&options, next_tx_id, next_commit_ts);

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
        SidecarSync::new(options.sync_mode, options.full_fsync),
        options.replication_fail_after_append_for_testing,
        committed_in_order.last().map(|(txid, _)| *txid),
        options.replication_crash_after_local_commit_for_testing,
      )?),
      None,
    ),
    ReplicationRole::Replica => (
      None,
      Some(
        ReplicaReplication::open(
          path,
          options.replication_sidecar_path.clone(),
          options.replication_source_db_path.clone(),
          options.replication_source_sidecar_path.clone(),
        )?
        .with_sync(SidecarSync::new(options.sync_mode, options.full_fsync)),
      ),
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

  // A file in the older magic (`MAGIC_KITEDB_V1`, which releases up to
  // v0.2.18 accept without checking the format version): rewrite both header
  // slots in the current one, so they refuse the file from here on. Last,
  // once every check that may refuse the open passed (the header, the WAL,
  // the snapshot, the log's records and their replay, the vector stores, the
  // replication sidecar): a refused open leaves the file in its magic, with
  // its fallback slot. Only the recovery writes above may have written
  // before (headers in the current magic). A crash between the two writes
  // leaves one slot in each magic, which the next writable open finishes.
  if !options.read_only && !in_current_magic {
    install_recovered_header(&mut pager, &mut header, &mut header_slot)?;
    install_recovered_header(&mut pager, &mut header, &mut header_slot)?;
    header.magic = MAGIC_KITEDB;
  }

  let wal_segment_size = options.wal_segment_size;
  Ok(SingleFileDB::owning(SingleFileInner {
    path: path.to_path_buf(),
    read_only: options.read_only,
    closed: AtomicBool::new(false),
    pager: Mutex::new(pager),
    header: RwLock::new(header),
    header_slot: AtomicU32::new(header_slot),
    wal_buffer: Mutex::new(wal_buffer),
    snapshot: super::CacheAligned(RwLock::new(super::CacheAligned(snapshot))),
    delta: super::CacheAligned(RwLock::new(super::CacheAligned(delta))),
    next_node_id: AtomicU64::new(next_node_id),
    next_label_id: AtomicU32::new(next_label_id),
    next_etype_id: AtomicU32::new(next_etype_id),
    next_propkey_id: AtomicU32::new(next_propkey_id),
    next_tx_id: AtomicU64::new(next_tx_id),
    tx_shared: std::sync::Arc::new(super::tx_registry::TxShared::default()),
    active_transactions: AtomicUsize::new(0),
    open_write_txids: Mutex::new(HashSet::new()),
    checkpoint_gate: RwLock::new(()),
    checkpoint_wait: Mutex::new(()),
    checkpoint_cv: parking_lot::Condvar::new(),
    segment_space_wait: Mutex::new(()),
    segment_space_cv: parking_lot::Condvar::new(),
    segment_waiters: AtomicUsize::new(0),
    blocking_checkpoint_asked: AtomicBool::new(false),
    commit_lock: Mutex::new(()),
    publish_lock: Mutex::new(()),
    publish_seq: AtomicU64::new(0),
    commit_queue: super::CommitQueue::default(),
    mvcc,
    label_names: RwLock::new(label_names),
    label_ids: RwLock::new(label_ids),
    etype_names: RwLock::new(etype_names),
    etype_ids: RwLock::new(etype_ids),
    propkey_names: RwLock::new(propkey_names),
    propkey_ids: RwLock::new(propkey_ids),
    schema_reservations: Mutex::new(SchemaReservations::default()),
    auto_checkpoint: options.auto_checkpoint,
    background_checkpoint: options.background_checkpoint,
    checkpoint_state: Mutex::new(BackgroundCheckpointState::default()),
    vector_stores: RwLock::new(vector_stores),
    vector_store_lazy_entries: RwLock::new(vector_store_lazy_entries),
    checkpoint_compression: options.checkpoint_compression.clone(),
    checkpoint_thread_enabled: options.checkpoint_thread,
    checkpoint_thread: Mutex::new(None),
    checkpoint_thread_stopped: AtomicBool::new(false),
    checkpoint_abandoned: AtomicBool::new(false),
    checkpoint_installing: AtomicBool::new(false),
    auto_checkpoint_failure: Mutex::new(Default::default()),
    writes_refused: std::sync::OnceLock::new(),
    wal_segment_size,
    checkpoint_log_ratio: options.checkpoint_log_ratio,
    checkpoint_log_budget: options.checkpoint_log_budget,
    wal_segment_limit_bytes: AtomicU64::new(options.wal_segment_limit.unwrap_or(0)),
    wal_spills: AtomicU64::new(0),
    spilled_txids: Mutex::new(spilled_txids),
    wal_segment_frees: AtomicU64::new(0),
    sync_mode: options.sync_mode,
    primary_replication,
    replica_replication,
    #[cfg(test)]
    wal_segment_test_capacity: AtomicUsize::new(0),
    #[cfg(test)]
    commits_waiting: AtomicUsize::new(0),
    #[cfg(feature = "bench-profile")]
    commit_lock_wait_ns: AtomicU64::new(0),
    #[cfg(feature = "bench-profile")]
    wal_flush_ns: AtomicU64::new(0),
  }))
}

/// Refuse checkpoint options no database can use: a log ratio that is not a
/// finite number at least 0, sizes of 0, and a WAL segment size past
/// `WAL_SEGMENT_MAX_SIZE`. (A log budget or segment limit too large to
/// compute with means no cap: the arithmetic on them saturates.)
fn validate_checkpoint_options(options: &SingleFileOpenOptions) -> Result<()> {
  if !options.checkpoint_log_ratio.is_finite() || options.checkpoint_log_ratio < 0.0 {
    return Err(KiteError::Internal(format!(
      "invalid checkpoint_log_ratio {}: a finite number, at least 0",
      options.checkpoint_log_ratio
    )));
  }
  for (name, value) in [
    ("checkpoint_log_budget", Some(options.checkpoint_log_budget)),
    ("wal_segment_size", options.wal_segment_size),
    ("wal_segment_limit", options.wal_segment_limit),
  ] {
    if value == Some(0) {
      return Err(KiteError::Internal(format!(
        "invalid {name} 0: more than 0 bytes"
      )));
    }
  }
  if let Some(size) = options
    .wal_segment_size
    .filter(|&size| size > WAL_SEGMENT_MAX_SIZE)
  {
    return Err(KiteError::Internal(format!(
      "invalid wal_segment_size {size}: at most {WAL_SEGMENT_MAX_SIZE} bytes"
    )));
  }
  Ok(())
}

/// Put the pages of the file no header slot names on the pager's free list,
/// for checkpoints to reuse and to truncate at the end of the file. The list
/// lives in memory, so without this the pages an earlier process freed (old
/// snapshots in front of the last one, or a snapshot an interrupted
/// checkpoint wrote) stayed in the file for good.
///
/// The pages `fallback` (the other valid header slot, as open found it)
/// names are held back until an install is durable in both slots, as a
/// running database holds back its own: until then a crash may still fall
/// back to that slot. (A header that recovery installed since names the
/// pages `header` names.)
fn reclaim_unnamed_pages(
  pager: &mut FilePager,
  header: &DbHeaderV1,
  fallback: Option<&DbHeaderV1>,
) {
  fn named(header: &DbHeaderV1) -> Vec<(u64, u64)> {
    let mut ranges = vec![
      (
        header.wal_start_page,
        header.wal_start_page + header.wal_page_count,
      ),
      (
        header.snapshot_start_page,
        header
          .snapshot_start_page
          .saturating_add(header.snapshot_page_count),
      ),
    ];
    ranges.extend(
      header
        .wal_segments
        .entries
        .iter()
        .map(|segment| (segment.start_page, segment.end_page())),
    );
    ranges
  }
  let fallback = fallback.map_or_else(Vec::new, named);
  let mut live = named(header);
  live.sort_unstable();
  let Ok(file_pages) = u32::try_from(pager.file_size().div_ceil(header.page_size as u64)) else {
    return;
  };
  let file_pages = u64::from(file_pages);

  // Every page between the header pages, the live ranges and the end of
  // the file.
  let mut gap_start = HEADER_SLOT_COUNT;
  for (start, end) in live.into_iter().chain([(file_pages, file_pages)]) {
    for page in gap_start..start.min(file_pages) {
      let held_back = fallback
        .iter()
        .any(|&(start, end)| start <= page && page < end);
      if held_back {
        pager.defer_free_pages(page as u32, 1);
      } else {
        pager.free_pages(page as u32, 1);
      }
    }
    gap_start = gap_start.max(end);
  }
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
  **temp.snapshot.write() = recovered.snapshot.write().take();
  **temp.delta.write() = std::mem::replace(&mut **recovered.delta.write(), DeltaState::new());
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
    // written in the current format (and magic). Generation and change
    // counters advance once when checkpoint installs the snapshot.
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
    sync_parent_dir(path)?;
    std::fs::rename(&temp_path, path)?;
    sync_parent_dir(path)?;
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
  // First: a run still building its snapshot is abandoned, one installing
  // finishes, and the thread ends (it holds a handle to the database).
  db.stop_checkpoint_thread();
  if !db.read_only {
    if let Err(error) = db.ensure_writes_allowed() {
      // Memory may not match disk: persist nothing (and neither does drop).
      // A reopen recovers every acknowledged commit from disk.
      if let Some(ref mvcc) = db.mvcc {
        mvcc.stop();
      }
      db.closed.store(true, Ordering::Release);
      return Err(error);
    }
  }
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

  // A clean close leaves no WAL segments: a checkpoint covers them (the
  // header is format version 2 again), and the compaction below cuts off
  // their pages. With a transaction open they stay, for the next open to
  // replay; and if the checkpoint fails, so do they.
  if !db.read_only
    && !db.header.read().wal_segments.is_empty()
    && db.active_transactions.load(Ordering::Acquire) == 0
  {
    if let Err(error) = db.checkpoint() {
      eprintln!(
        "Warning: closing {} keeps its WAL segments (the next open replays them): the \
         checkpoint covering them failed: {error}",
        db.path.display()
      );
    }
  }

  if let Some(ref mvcc) = db.mvcc {
    mvcc.stop();
  }

  if db.read_only {
    return Ok(());
  }

  db.compact_for_close()?;
  // On failure, dropping `db` tries once more.
  db.persist_for_close()?;
  db.closed.store(true, Ordering::Release);
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

  /// `page`, a serialized header, in the magic legacy files have
  /// (`MAGIC_KITEDB_V1`), its checksums made over it again.
  fn with_v1_magic(mut page: Vec<u8>) -> Vec<u8> {
    page[0..16].copy_from_slice(&MAGIC_KITEDB_V1);
    let header_crc = crate::util::crc::crc32(&page[..176]);
    page[176..180].copy_from_slice(&header_crc.to_le_bytes());
    let footer = page.len() - 4;
    let footer_crc = crate::util::crc::crc32(&page[..footer]);
    page[footer..].copy_from_slice(&footer_crc.to_le_bytes());
    page
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
      .write_all(&with_v1_magic(legacy_header.serialize_to_page()))
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
    assert_eq!(
      std::fs::read(&db_path).expect("read")[0..16],
      MAGIC_KITEDB_V1,
      "setup: the legacy file is not in v0.2.18's magic"
    );

    let migrated = open_single_file(&db_path, options.clone()).expect("automatic migration");
    // Both header slots in the current magic: v0.2.18 refuses the file now.
    let bytes = std::fs::read(&db_path).expect("read");
    let page_size = migrated.header.read().page_size as usize;
    assert_eq!(bytes[0..16], MAGIC_KITEDB);
    assert_eq!(bytes[page_size..page_size + 16], MAGIC_KITEDB);
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
    assert_eq!(migrated.header.read().version, VERSION_SALTED_WAL);
    assert_eq!(
      migrated.header.read().min_reader_version,
      MIN_READER_SALTED_WAL
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

/// raydb-b4 sigbus: the replica's lockless view of a live primary file.
#[cfg(test)]
#[path = "b4_sigbus_tests.rs"]
mod b4_sigbus_tests;
