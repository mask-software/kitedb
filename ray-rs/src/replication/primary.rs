//! Primary-side replication orchestration.
//!
//! The main database commit is authoritative. Once its COMMIT record, WAL,
//! and header are durable, a replication-sidecar append is best effort: an
//! append error is recorded as `sidecar_needs_repair` and is never returned as
//! a failure of the local commit. The sidecar is fenced after that error, so
//! later commits cannot append over a missing frame. The stale/error state is
//! persisted in `primary-health.json`; reopen also compares the newest local
//! WAL commit txid with the newest sidecar frame and writes that marker if a
//! crash happened before the append attempt recorded the error.
//!
//! Normal/Off sync modes buffer frames in memory. A background publisher
//! writes them to the segment file within one tick and persists the manifest
//! once appends pause, so an idle primary never hides commits from replicas.
//! While frames are buffered, `primary-unflushed` exists; finding it on an
//! open whose WAL a checkpoint already emptied means commits may be missing
//! from the sidecar with no WAL record left to compare, so the sidecar is
//! fenced for repair.

use super::durability::SidecarSync;
use super::log_store::{ReplicationFrame, SegmentLogStore};
use super::manifest::{
  new_manifest_generation, ManifestStore, ReplicationManifest, SegmentMeta,
  MANIFEST_ENVELOPE_VERSION,
};
use super::progress::{
  clear_replica_progress_synced, load_replica_progress, remove_replica_progress_synced,
  upsert_replica_progress_synced, ReplicaProgress as ReplicaProgressEntry,
};
use super::transport::{build_commit_payload_header, decode_commit_frame_payload};
use super::types::{CommitToken, ReplicationCursor, ReplicationRole};
use crate::core::single_file::SyncMode;
use crate::error::{KiteError, Result};
use fs2::FileExt;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MANIFEST_FILE_NAME: &str = "manifest.json";
const PRIMARY_LOCK_FILE_NAME: &str = "primary.lock";
const DEFAULT_SEGMENT_MAX_BYTES: u64 = 64 * 1024 * 1024;
const DEFAULT_RETENTION_MIN_ENTRIES: u64 = 1024;
const DEFAULT_MANIFEST_REFRESH_APPEND_INTERVAL: u64 = 256;
const DEFAULT_APPEND_WRITE_BUFFER_BYTES: usize = 16 * 1024 * 1024;
const PRIMARY_HEALTH_FILE_NAME: &str = "primary-health.json";
const PRIMARY_HEALTH_VERSION: u32 = 1;
const PRIMARY_UNFLUSHED_MARKER_FILE_NAME: &str = "primary-unflushed";
/// Upper bound for buffered frames to reach the segment file.
const BUFFERED_FRAME_PUBLISH_INTERVAL: Duration = Duration::from_millis(100);

type SidecarOpLock = Arc<Mutex<()>>;
type SidecarPrimaryLock = Arc<PrimarySidecarProcessLock>;
type SidecarEpochFence = Arc<AtomicU64>;
type SharedSidecarHealth = Arc<Mutex<PrimarySidecarHealth>>;

static SIDECAR_LOCKS: OnceLock<StdMutex<HashMap<PathBuf, SidecarOpLock>>> = OnceLock::new();
static SIDECAR_PRIMARY_LOCKS: OnceLock<
  StdMutex<HashMap<PathBuf, Weak<PrimarySidecarProcessLock>>>,
> = OnceLock::new();
static SIDECAR_EPOCH_FENCES: OnceLock<StdMutex<HashMap<PathBuf, Weak<AtomicU64>>>> =
  OnceLock::new();
static SIDECAR_HEALTH: OnceLock<StdMutex<HashMap<PathBuf, Weak<Mutex<PrimarySidecarHealth>>>>> =
  OnceLock::new();

#[derive(Debug, Clone)]
pub struct PrimaryReplicationStatus {
  pub role: ReplicationRole,
  pub epoch: u64,
  pub head_log_index: u64,
  pub retained_floor: u64,
  pub replica_lags: Vec<ReplicaLagStatus>,
  pub sidecar_path: PathBuf,
  pub last_token: Option<CommitToken>,
  pub last_replication_error: Option<String>,
  pub sidecar_needs_repair: bool,
  pub append_attempts: u64,
  pub append_failures: u64,
  pub append_successes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicaLagStatus {
  pub replica_id: String,
  pub epoch: u64,
  pub applied_log_index: u64,
}

/// Result of a promotion request: the epoch this instance now observes, and
/// whether this instance advanced it (a stale instance only observes it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrimaryPromotion {
  pub epoch: u64,
  pub promoted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrimaryRetentionOutcome {
  pub pruned_segments: usize,
  pub retained_floor: u64,
}

/// The replication log position a snapshot of the primary holds: read under
/// the database's commit lock (commits append their frames under it), it
/// matches the database file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrimarySnapshotPosition {
  pub epoch: u64,
  pub head_log_index: u64,
  pub retained_floor: u64,
  /// The sidecar's log history (`ReplicationManifest::generation`).
  pub generation: u64,
  /// Right after the head frame: a log pull from here skips every frame the
  /// snapshot holds and returns the next one.
  pub start_cursor: ReplicationCursor,
}

#[derive(Debug)]
struct PrimarySidecarProcessLock {
  file: File,
}

impl Drop for PrimarySidecarProcessLock {
  fn drop(&mut self) {
    // The lock belongs to the open file description, which a child process
    // spawned meanwhile shares through its inherited descriptor. Closing only
    // this descriptor would leave the lock held until the child closes its
    // copy, so release it explicitly.
    let _ = FileExt::unlock(&self.file);
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ManifestDiskStamp {
  len: u64,
  modified_unix_nanos: Option<u128>,
}

#[derive(Debug)]
struct PrimaryReplicationState {
  manifest: ReplicationManifest,
  manifest_disk_stamp: ManifestDiskStamp,
  log_store: SegmentLogStore,
  active_segment_size_bytes: u64,
  last_token: Option<CommitToken>,
  last_replication_error: Option<String>,
  sidecar_needs_repair: bool,
  replica_progress: HashMap<String, ReplicaProgressEntry>,
  write_fenced: bool,
  /// The persisted manifest moved to another epoch: another instance was
  /// promoted. Unlike `write_fenced`, never set by the repair fence.
  superseded: bool,
  appends_since_manifest_refresh: u64,
  /// Frames were appended since the publisher's last tick.
  appended_since_publish: bool,
  /// The in-memory manifest is ahead of the persisted one.
  manifest_dirty: bool,
  /// `primary-unflushed` exists on disk.
  unflushed_marker: bool,
}

/// Primary replication runtime. Buffered sync modes run a background
/// publisher that is stopped, followed by a final publish, on drop.
#[derive(Debug)]
pub struct PrimaryReplication {
  inner: Arc<PrimaryReplicationInner>,
  publisher: Option<BufferedFramePublisher>,
}

#[derive(Debug)]
struct PrimaryReplicationInner {
  sidecar_path: PathBuf,
  manifest_store: ManifestStore,
  state: Mutex<PrimaryReplicationState>,
  append_attempts: AtomicU64,
  append_failures: AtomicU64,
  append_successes: AtomicU64,
  segment_max_bytes: u64,
  retention_min_entries: u64,
  retention_min_duration: Option<Duration>,
  sync: SidecarSync,
  durable_append: bool,
  checksum_payload: bool,
  persist_manifest_each_append: bool,
  manifest_refresh_append_interval: u64,
  append_write_buffer_bytes: usize,
  fail_after_append_for_testing: Option<u64>,
  crash_after_local_commit_for_testing: bool,
  health_store: PrimarySidecarHealthStore,
  /// `primary-health.json` as last written in this process.
  health: SharedSidecarHealth,
  unflushed_marker_path: PathBuf,
  sidecar_op_lock: SidecarOpLock,
  _sidecar_primary_lock: SidecarPrimaryLock,
  epoch_fence: SidecarEpochFence,
}

/// Thread that publishes buffered frames every `interval` until dropped.
#[derive(Debug)]
struct BufferedFramePublisher {
  shutdown: Arc<PublisherShutdown>,
  thread: Option<JoinHandle<()>>,
}

#[derive(Debug, Default)]
struct PublisherShutdown {
  requested: Mutex<bool>,
  wake: parking_lot::Condvar,
}

impl BufferedFramePublisher {
  fn spawn(primary: Weak<PrimaryReplicationInner>, interval: Duration) -> Result<Self> {
    let shutdown = Arc::new(PublisherShutdown::default());
    let thread_shutdown = Arc::clone(&shutdown);
    let thread = std::thread::Builder::new()
      .name("kitedb-replication-publisher".to_string())
      .spawn(move || loop {
        {
          let mut requested = thread_shutdown.requested.lock();
          if !*requested {
            thread_shutdown.wake.wait_for(&mut requested, interval);
          }
          if *requested {
            return;
          }
        }
        match primary.upgrade() {
          Some(primary) => primary.publish_buffered_frames(),
          None => return,
        }
      })?;
    Ok(Self {
      shutdown,
      thread: Some(thread),
    })
  }
}

impl Drop for BufferedFramePublisher {
  fn drop(&mut self) {
    *self.shutdown.requested.lock() = true;
    self.shutdown.wake.notify_all();
    if let Some(thread) = self.thread.take() {
      let _ = thread.join();
    }
  }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct PrimarySidecarHealth {
  version: u32,
  last_replication_error: Option<String>,
  sidecar_needs_repair: bool,
}

#[derive(Debug, Clone)]
struct PrimarySidecarHealthStore {
  path: PathBuf,
  sync: SidecarSync,
}

impl PrimarySidecarHealthStore {
  fn new(sidecar_path: &Path, sync: SidecarSync) -> Self {
    Self {
      path: sidecar_path.join(PRIMARY_HEALTH_FILE_NAME),
      sync,
    }
  }

  fn read(&self) -> Result<Option<PrimarySidecarHealth>> {
    let bytes = match std::fs::read(&self.path) {
      Ok(bytes) => bytes,
      Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
      Err(error) => return Err(error.into()),
    };

    let health: PrimarySidecarHealth = serde_json::from_slice(&bytes).map_err(|error| {
      KiteError::Serialization(format!("decode primary replication health: {error}"))
    })?;
    if health.version != PRIMARY_HEALTH_VERSION {
      return Err(KiteError::VersionMismatch {
        required: health.version,
        current: PRIMARY_HEALTH_VERSION,
      });
    }
    Ok(Some(health))
  }

  fn write(&self, health: &PrimarySidecarHealth) -> Result<()> {
    if let Some(parent) = self.path.parent() {
      std::fs::create_dir_all(parent)?;
    }

    let mut persisted = health.clone();
    persisted.version = PRIMARY_HEALTH_VERSION;
    let bytes = serde_json::to_vec(&persisted).map_err(|error| {
      KiteError::Serialization(format!("encode primary replication health: {error}"))
    })?;
    let temp_path = self.path.with_extension("json.tmp");
    let mut temp_file = OpenOptions::new()
      .create(true)
      .truncate(true)
      .write(true)
      .open(&temp_path)?;
    temp_file.write_all(&bytes)?;
    self.sync.sync_file(&temp_file)?;
    drop(temp_file);

    std::fs::rename(&temp_path, &self.path)?;
    self.sync.sync_parent_dir(&self.path)?;
    Ok(())
  }
}

impl PrimaryReplication {
  /// Open a primary replication sidecar using the stable runtime options.
  pub fn open(
    db_path: &Path,
    sidecar_path: Option<PathBuf>,
    segment_max_bytes: Option<u64>,
    retention_min_entries: Option<u64>,
    retention_min_ms: Option<u64>,
    sync_mode: SyncMode,
    fail_after_append_for_testing: Option<u64>,
  ) -> Result<Self> {
    Self::open_with_recovery(
      db_path,
      sidecar_path,
      segment_max_bytes,
      retention_min_entries,
      retention_min_ms,
      SidecarSync::new(sync_mode, false),
      fail_after_append_for_testing,
      None,
      false,
    )
  }

  /// Open a primary and reconcile a local WAL commit boundary during recovery.
  /// The sidecar follows the database's sync policy, `sync`.
  #[allow(clippy::too_many_arguments)]
  pub fn open_with_recovery(
    db_path: &Path,
    sidecar_path: Option<PathBuf>,
    segment_max_bytes: Option<u64>,
    retention_min_entries: Option<u64>,
    retention_min_ms: Option<u64>,
    sync: SidecarSync,
    fail_after_append_for_testing: Option<u64>,
    local_latest_committed_txid: Option<u64>,
    crash_after_local_commit_for_testing: bool,
  ) -> Result<Self> {
    let inner = Arc::new(PrimaryReplicationInner::open_with_recovery(
      db_path,
      sidecar_path,
      segment_max_bytes,
      retention_min_entries,
      retention_min_ms,
      sync,
      fail_after_append_for_testing,
      local_latest_committed_txid,
      crash_after_local_commit_for_testing,
    )?);
    let publisher = if inner.durable_append {
      None
    } else {
      Some(BufferedFramePublisher::spawn(
        Arc::downgrade(&inner),
        BUFFERED_FRAME_PUBLISH_INTERVAL,
      )?)
    };
    Ok(Self { inner, publisher })
  }

  pub fn append_commit_frame(&self, payload: Vec<u8>) -> Result<CommitToken> {
    self.inner.append_commit_frame(payload)
  }

  pub fn append_commit_wal_frame(&self, txid: u64, wal_bytes: Vec<u8>) -> Result<CommitToken> {
    self.inner.append_commit_wal_frame(txid, wal_bytes)
  }

  pub fn crash_after_local_commit_for_testing(&self) -> bool {
    self.inner.crash_after_local_commit_for_testing()
  }

  /// Reject a local commit before its WAL COMMIT record when this instance is
  /// fenced by a newer primary epoch. A sidecar-repair fence is intentionally
  /// excluded: local commits remain authoritative while replication is stale.
  ///
  /// The database runs it under its commit lock, right before it writes the
  /// commit, so a promotion that lands while the commit waits for the lock
  /// fences it. One that lands after the check, while the commit is written,
  /// fences the commit's sidecar append instead.
  pub fn ensure_local_commit_allowed(&self) -> Result<()> {
    self.inner.ensure_local_commit_allowed()
  }

  pub fn promote_to_next_epoch(&self) -> Result<u64> {
    self.promote().map(|promotion| promotion.epoch)
  }

  pub fn promote(&self) -> Result<PrimaryPromotion> {
    self.inner.promote()
  }

  pub fn report_replica_progress(
    &self,
    replica_id: &str,
    epoch: u64,
    applied_log_index: u64,
  ) -> Result<()> {
    self
      .inner
      .report_replica_progress(replica_id, epoch, applied_log_index)
  }

  /// Forget a replica's progress, so a decommissioned replica stops holding
  /// back retention. Returns whether it had progress recorded. A replica
  /// that reports progress again is tracked again.
  pub fn remove_replica_progress(&self, replica_id: &str) -> Result<bool> {
    self.inner.remove_replica_progress(replica_id)
  }

  pub fn run_retention(&self) -> Result<PrimaryRetentionOutcome> {
    self.inner.run_retention()
  }

  pub fn last_token(&self) -> Option<CommitToken> {
    self.inner.last_token()
  }

  /// The log position a snapshot taken now holds. Call it under the
  /// database's commit lock, so no commit appends a frame meanwhile.
  pub fn snapshot_position(&self) -> PrimarySnapshotPosition {
    self.inner.snapshot_position()
  }

  pub fn status(&self) -> PrimaryReplicationStatus {
    self.inner.status()
  }

  pub fn flush_for_transport_export(&self) -> Result<()> {
    self.inner.flush_for_transport_export()
  }

  /// Make every frame appended so far durable, with a manifest naming it,
  /// before a checkpoint drops the WAL records of their commits. On failure
  /// the sidecar is fenced for repair, as after a failed append, and the
  /// error is returned for the caller to report.
  pub fn publish_for_checkpoint(&self) -> Result<()> {
    self.inner.publish_for_checkpoint()
  }

  /// Stop the background publisher without publishing, so frames appended
  /// from now on stay buffered in memory, as they would be at a crash before
  /// its next tick.
  #[cfg(test)]
  pub(crate) fn stop_publisher_for_testing(&mut self) {
    drop(self.publisher.take());
  }
}

impl Drop for PrimaryReplication {
  fn drop(&mut self) {
    // Stop the publisher first so the final publish is the last sidecar write.
    drop(self.publisher.take());
    self.inner.publish_on_close();
  }
}

impl PrimaryReplicationInner {
  #[allow(clippy::too_many_arguments)]
  fn open_with_recovery(
    db_path: &Path,
    sidecar_path: Option<PathBuf>,
    segment_max_bytes: Option<u64>,
    retention_min_entries: Option<u64>,
    retention_min_ms: Option<u64>,
    sync: SidecarSync,
    fail_after_append_for_testing: Option<u64>,
    local_latest_committed_txid: Option<u64>,
    crash_after_local_commit_for_testing: bool,
  ) -> Result<Self> {
    let sidecar_path = sidecar_path.unwrap_or_else(|| default_replication_sidecar_path(db_path));
    std::fs::create_dir_all(&sidecar_path)?;
    let (sidecar_primary_lock, held_by_live_instance) =
      acquire_sidecar_primary_lock(&sidecar_path)?;

    let manifest_store = ManifestStore::with_sync(sidecar_path.join(MANIFEST_FILE_NAME), sync);
    let health_store = PrimarySidecarHealthStore::new(&sidecar_path, sync);
    let health = shared_sidecar_health(&sidecar_path, &health_store)?;
    let persisted_health = Some(health.lock().clone());
    let unflushed_marker_path = sidecar_path.join(PRIMARY_UNFLUSHED_MARKER_FILE_NAME);
    // The marker says a primary stopped with frames in memory. While another
    // instance in this process holds the sidecar, any marker is that live
    // instance's: its frames are still buffered, not lost, and it removes
    // the marker itself once they are published. (The first instance to
    // take the lock already handled a predecessor's marker.)
    let stopped_with_buffered_frames = !held_by_live_instance && unflushed_marker_path.exists();

    let mut manifest = if manifest_store.path().exists() {
      manifest_store.read()?
    } else {
      let initial = ReplicationManifest {
        version: MANIFEST_ENVELOPE_VERSION,
        epoch: 1,
        head_log_index: 0,
        retained_floor: 0,
        active_segment_id: 1,
        segments: vec![SegmentMeta {
          id: 1,
          start_log_index: 1,
          end_log_index: 0,
          size_bytes: 0,
        }],
        generation: new_manifest_generation(),
      };
      manifest_store.write(&initial)?;
      initial
    };

    ensure_active_segment_metadata(&mut manifest);
    let mut last_replication_error = persisted_health
      .as_ref()
      .and_then(|health| health.last_replication_error.clone());
    let mut sidecar_needs_repair = persisted_health
      .as_ref()
      .map(|health| health.sidecar_needs_repair)
      .unwrap_or(false);

    if !sidecar_needs_repair {
      match reconcile_manifest_head_from_active_segment(&sidecar_path, &mut manifest) {
        Ok(true) => {
          // Recover append state when manifest head lagged a flushed segment tail.
          if let Err(error) = manifest_store.write(&manifest) {
            sidecar_needs_repair = true;
            last_replication_error = Some(format!(
              "replication sidecar manifest recovery failed: {error}"
            ));
          }
        }
        Ok(false) => {}
        Err(error) => {
          sidecar_needs_repair = true;
          last_replication_error = Some(format!(
            "replication sidecar recovery failed; repair/resync required: {error}"
          ));
        }
      }
    }

    if !sidecar_needs_repair {
      if let Some(local_txid) = local_latest_committed_txid {
        match sidecar_last_txid(&sidecar_path, &manifest) {
          Ok(Some(sidecar_txid)) if sidecar_txid == local_txid => {}
          Ok(Some(sidecar_txid)) => {
            sidecar_needs_repair = true;
            last_replication_error = Some(format!(
              "local WAL commit txid {local_txid} is newer than sidecar txid {sidecar_txid}; repair/resync required"
            ));
          }
          Ok(None) => {
            sidecar_needs_repair = true;
            last_replication_error = Some(format!(
              "local WAL commit txid {local_txid} has no matching replication sidecar frame; repair/resync required"
            ));
          }
          Err(error) => {
            sidecar_needs_repair = true;
            last_replication_error = Some(format!(
              "replication sidecar frame inspection failed; repair/resync required: {error}"
            ));
          }
        }
      } else if stopped_with_buffered_frames {
        // No WAL commit is left to compare: a checkpoint folded the commits
        // whose frames were still buffered when the process stopped.
        sidecar_needs_repair = true;
        last_replication_error = Some(
          "primary stopped with replication frames buffered in memory and the WAL holding their commits was checkpointed; repair/resync required"
            .to_string(),
        );
      }
    }

    if sidecar_needs_repair {
      let repair = PrimarySidecarHealth {
        version: PRIMARY_HEALTH_VERSION,
        last_replication_error: last_replication_error.clone(),
        sidecar_needs_repair: true,
      };
      persist_health_best_effort(&health_store, &repair);
      *health.lock() = repair;
    }
    if stopped_with_buffered_frames {
      // The decision above (or the persisted health) now carries the marker.
      remove_unflushed_marker(&unflushed_marker_path)?;
    }

    let segment_path = sidecar_path.join(segment_file_name(manifest.active_segment_id));
    let active_segment_size_bytes = segment_file_len(&segment_path)?;
    let durable_append = sync.durable_append();
    let append_write_buffer_bytes = if durable_append {
      0
    } else {
      DEFAULT_APPEND_WRITE_BUFFER_BYTES
    };
    let log_store = SegmentLogStore::open_append(&segment_path, append_write_buffer_bytes, sync)?;
    let manifest_disk_stamp = read_manifest_disk_stamp(manifest_store.path())?;
    let replica_progress = load_replica_progress(&sidecar_path)?;

    let sidecar_op_lock = sidecar_operation_lock(&sidecar_path);
    let epoch_fence = sidecar_epoch_fence(&sidecar_path, manifest.epoch);

    Ok(Self {
      sidecar_path,
      manifest_store,
      state: Mutex::new(PrimaryReplicationState {
        manifest,
        manifest_disk_stamp,
        log_store,
        active_segment_size_bytes,
        last_token: None,
        last_replication_error,
        sidecar_needs_repair,
        replica_progress,
        write_fenced: false,
        superseded: false,
        appends_since_manifest_refresh: 0,
        appended_since_publish: false,
        manifest_dirty: false,
        unflushed_marker: false,
      }),
      append_attempts: AtomicU64::new(0),
      append_failures: AtomicU64::new(0),
      append_successes: AtomicU64::new(0),
      segment_max_bytes: segment_max_bytes
        .unwrap_or(DEFAULT_SEGMENT_MAX_BYTES)
        .max(1),
      retention_min_entries: retention_min_entries.unwrap_or(DEFAULT_RETENTION_MIN_ENTRIES),
      retention_min_duration: retention_min_ms.map(Duration::from_millis),
      sync,
      durable_append,
      checksum_payload: durable_append,
      persist_manifest_each_append: durable_append,
      manifest_refresh_append_interval: if durable_append {
        1
      } else {
        DEFAULT_MANIFEST_REFRESH_APPEND_INTERVAL
      },
      append_write_buffer_bytes,
      fail_after_append_for_testing,
      crash_after_local_commit_for_testing,
      health_store,
      health,
      unflushed_marker_path,
      sidecar_op_lock,
      _sidecar_primary_lock: sidecar_primary_lock,
      epoch_fence,
    })
  }

  pub fn append_commit_frame(&self, payload: Vec<u8>) -> Result<CommitToken> {
    self.append_frame(|log_store, epoch, log_index, checksum| {
      log_store.append_payload_segments_with_crc(epoch, log_index, &[payload.as_slice()], checksum)
    })
  }

  pub fn append_commit_wal_frame(&self, txid: u64, wal_bytes: Vec<u8>) -> Result<CommitToken> {
    let header = match build_commit_payload_header(txid, wal_bytes.len()) {
      Ok(header) => header,
      Err(error) => {
        self.mark_append_failure(&error);
        return Err(error);
      }
    };
    self.append_frame(move |log_store, epoch, log_index, checksum| {
      log_store.append_payload_owned_segments_with_crc(
        epoch,
        log_index,
        vec![header.to_vec(), wal_bytes],
        checksum,
      )
    })
  }

  pub fn crash_after_local_commit_for_testing(&self) -> bool {
    self.crash_after_local_commit_for_testing
  }

  /// Reject a local commit before its WAL COMMIT record when this instance is
  /// fenced by a newer primary epoch. A sidecar-repair fence is intentionally
  /// excluded: local commits remain authoritative while replication is stale.
  /// The epoch check still runs then, since a repair says nothing about
  /// whether another instance was promoted.
  pub fn ensure_local_commit_allowed(&self) -> Result<()> {
    let _sidecar_guard = self.sidecar_op_lock.lock();
    let mut state = self.state.lock();
    self.refresh_health_locked(&mut state);

    if self.epoch_fence.load(Ordering::Acquire) > state.manifest.epoch {
      state.write_fenced = true;
      return Err(stale_primary_error());
    }

    if state.sidecar_needs_repair {
      // The repair fence sets `write_fenced` too, so only a newer epoch
      // counts. A sidecar too broken to read must not block local commits.
      let _ = self.refresh_manifest_locked(&mut state);
      return if state.superseded {
        Err(stale_primary_error())
      } else {
        Ok(())
      };
    }

    let epoch_changed = self.refresh_manifest_locked(&mut state)?;
    if epoch_changed || state.write_fenced {
      return Err(stale_primary_error());
    }
    Ok(())
  }

  /// Append one frame, written by `write_frame(log_store, epoch, log_index,
  /// checksum_payload)`, which returns its size. Any error fences the
  /// sidecar for repair.
  fn append_frame(
    &self,
    write_frame: impl FnOnce(&mut SegmentLogStore, u64, u64, bool) -> Result<u64>,
  ) -> Result<CommitToken> {
    self.append_attempts.fetch_add(1, Ordering::Relaxed);

    if let Some(limit) = self.fail_after_append_for_testing {
      let successes = self.append_successes.load(Ordering::Relaxed);
      if successes >= limit {
        let error = KiteError::InvalidReplication(
          "replication append failure injected for testing".to_string(),
        );
        self.append_failures.fetch_add(1, Ordering::Relaxed);
        self.mark_append_failure(&error);
        return Err(error);
      }
    }

    let result = self.append_frame_inner(write_frame);
    if let Err(error) = &result {
      self.mark_append_failure(error);
    }
    result
  }

  fn append_frame_inner(
    &self,
    write_frame: impl FnOnce(&mut SegmentLogStore, u64, u64, bool) -> Result<u64>,
  ) -> Result<CommitToken> {
    let _sidecar_guard = self.sidecar_op_lock.lock();
    let mut state = self.state.lock();
    self.refresh_health_locked(&mut state);
    if state.sidecar_needs_repair {
      self.append_failures.fetch_add(1, Ordering::Relaxed);
      return Err(sidecar_repair_error());
    }
    let fenced_epoch = self.epoch_fence.load(Ordering::Acquire);
    if fenced_epoch > state.manifest.epoch {
      state.write_fenced = true;
      self.append_failures.fetch_add(1, Ordering::Relaxed);
      return Err(stale_primary_error());
    }
    if state.write_fenced {
      self.append_failures.fetch_add(1, Ordering::Relaxed);
      return Err(stale_primary_error());
    }
    let should_refresh = state.appends_since_manifest_refresh
      >= self.manifest_refresh_append_interval.saturating_sub(1);
    if should_refresh {
      let epoch_changed = self.refresh_manifest_locked(&mut state)?;
      state.appends_since_manifest_refresh = 0;
      if epoch_changed || state.write_fenced {
        self.append_failures.fetch_add(1, Ordering::Relaxed);
        return Err(stale_primary_error());
      }
    }

    if let Err(error) = self.note_frame_buffering_locked(&mut state) {
      self.append_failures.fetch_add(1, Ordering::Relaxed);
      return Err(error);
    }

    let epoch = state.manifest.epoch;
    let next_log_index = state.manifest.head_log_index.saturating_add(1);

    let frame_size = match write_frame(
      &mut state.log_store,
      epoch,
      next_log_index,
      self.checksum_payload,
    ) {
      Ok(size) => size,
      Err(error) => {
        self.append_failures.fetch_add(1, Ordering::Relaxed);
        return Err(error);
      }
    };

    if self.durable_append {
      if let Err(error) = state.log_store.sync() {
        self.append_failures.fetch_add(1, Ordering::Relaxed);
        return Err(error);
      }
    }

    let mut next_manifest = state.manifest.clone();
    next_manifest.head_log_index = next_log_index;

    ensure_active_segment_metadata(&mut next_manifest);
    state.active_segment_size_bytes = state.active_segment_size_bytes.saturating_add(frame_size);
    let size_bytes = state.active_segment_size_bytes;

    if let Some(meta) = next_manifest
      .segments
      .iter_mut()
      .find(|entry| entry.id == next_manifest.active_segment_id)
    {
      if meta.end_log_index < meta.start_log_index {
        meta.start_log_index = next_log_index;
      }
      meta.end_log_index = next_log_index;
      meta.size_bytes = size_bytes;
    }

    let mut rotated = false;
    if size_bytes >= self.segment_max_bytes {
      rotated = true;
      next_manifest.active_segment_id = next_manifest.active_segment_id.saturating_add(1);
      let start = next_log_index.saturating_add(1);
      next_manifest.segments.push(SegmentMeta {
        id: next_manifest.active_segment_id,
        start_log_index: start,
        end_log_index: start.saturating_sub(1),
        size_bytes: 0,
      });
    }

    let persist_manifest = self.persist_manifest_each_append || rotated || should_refresh;
    if persist_manifest {
      if let Err(error) = state.log_store.flush() {
        self.append_failures.fetch_add(1, Ordering::Relaxed);
        return Err(error);
      }
      // Without a rotation only the head moved, and a head that a crash
      // reverts is recovered from the active segment on open: the rename
      // need not be durable, only the manifest's content.
      let written = if rotated {
        self.manifest_store.write(&next_manifest)
      } else {
        self.manifest_store.write_head(&next_manifest)
      };
      if let Err(error) = written {
        self.append_failures.fetch_add(1, Ordering::Relaxed);
        return Err(error);
      }
      state.manifest_disk_stamp = read_manifest_disk_stamp(self.manifest_store.path())?;
    }

    let token = CommitToken::new(epoch, next_log_index);
    if rotated {
      state.log_store = SegmentLogStore::open_append(
        self
          .sidecar_path
          .join(segment_file_name(next_manifest.active_segment_id)),
        self.append_write_buffer_bytes,
        self.sync,
      )?;
      state.active_segment_size_bytes = 0;
    }
    state.manifest = next_manifest;
    state.manifest_dirty = !persist_manifest;
    state.appended_since_publish = true;
    state.last_token = Some(token);
    state.appends_since_manifest_refresh = state.appends_since_manifest_refresh.saturating_add(1);
    self.append_successes.fetch_add(1, Ordering::Relaxed);
    self
      .epoch_fence
      .store(state.manifest.epoch, Ordering::Release);

    Ok(token)
  }

  pub fn promote(&self) -> Result<PrimaryPromotion> {
    let _sidecar_guard = self.sidecar_op_lock.lock();
    let mut state = self.state.lock();
    if state.sidecar_needs_repair {
      return Err(sidecar_repair_error());
    }
    let epoch_changed = self.refresh_manifest_locked(&mut state)?;
    if epoch_changed || state.write_fenced {
      return Ok(PrimaryPromotion {
        epoch: state.manifest.epoch,
        promoted: false,
      });
    }

    let mut next_manifest = state.manifest.clone();
    next_manifest.epoch = next_manifest.epoch.saturating_add(1);
    next_manifest.active_segment_id = next_manifest.active_segment_id.saturating_add(1);
    next_manifest.segments.push(SegmentMeta {
      id: next_manifest.active_segment_id,
      start_log_index: next_manifest.head_log_index.saturating_add(1),
      end_log_index: next_manifest.head_log_index,
      size_bytes: 0,
    });
    ensure_active_segment_metadata(&mut next_manifest);
    self.manifest_store.write(&next_manifest)?;
    state.manifest_disk_stamp = read_manifest_disk_stamp(self.manifest_store.path())?;

    state.log_store = SegmentLogStore::open_append(
      self
        .sidecar_path
        .join(segment_file_name(next_manifest.active_segment_id)),
      self.append_write_buffer_bytes,
      self.sync,
    )?;
    state.active_segment_size_bytes = 0;
    state.manifest = next_manifest;
    state.manifest_dirty = false;
    state.last_token = None;
    state.replica_progress.clear();
    clear_replica_progress_synced(&self.sidecar_path, self.sync)?;
    state.write_fenced = false;
    state.appends_since_manifest_refresh = 0;
    self
      .epoch_fence
      .store(state.manifest.epoch, Ordering::Release);
    Ok(PrimaryPromotion {
      epoch: state.manifest.epoch,
      promoted: true,
    })
  }

  pub fn report_replica_progress(
    &self,
    replica_id: &str,
    epoch: u64,
    applied_log_index: u64,
  ) -> Result<()> {
    let _sidecar_guard = self.sidecar_op_lock.lock();
    let mut state = self.state.lock();
    if state.sidecar_needs_repair {
      return Err(sidecar_repair_error());
    }
    let epoch_changed = self.refresh_manifest_locked(&mut state)?;
    if epoch_changed || state.write_fenced {
      return Err(stale_primary_error());
    }
    if epoch != state.manifest.epoch {
      return Err(KiteError::InvalidReplication(format!(
        "replica progress epoch mismatch: reported {epoch}, primary epoch {}",
        state.manifest.epoch
      )));
    }

    upsert_replica_progress_synced(
      &self.sidecar_path,
      replica_id,
      epoch,
      applied_log_index,
      self.sync,
    )?;
    state.replica_progress.insert(
      replica_id.to_string(),
      ReplicaProgressEntry {
        epoch,
        applied_log_index,
      },
    );
    Ok(())
  }

  pub fn remove_replica_progress(&self, replica_id: &str) -> Result<bool> {
    let _sidecar_guard = self.sidecar_op_lock.lock();
    let mut state = self.state.lock();
    if state.sidecar_needs_repair {
      return Err(sidecar_repair_error());
    }
    let epoch_changed = self.refresh_manifest_locked(&mut state)?;
    if epoch_changed || state.write_fenced {
      return Err(stale_primary_error());
    }

    let removed = remove_replica_progress_synced(&self.sidecar_path, replica_id, self.sync)?;
    state.replica_progress.remove(replica_id);
    Ok(removed)
  }

  pub fn run_retention(&self) -> Result<PrimaryRetentionOutcome> {
    let _sidecar_guard = self.sidecar_op_lock.lock();
    let mut state = self.state.lock();
    if state.sidecar_needs_repair {
      return Err(sidecar_repair_error());
    }
    let epoch_changed = self.refresh_manifest_locked(&mut state)?;
    if epoch_changed || state.write_fenced {
      return Err(stale_primary_error());
    }
    self.refresh_replica_progress_locked(&mut state)?;
    state.log_store.flush()?;

    let head = state.manifest.head_log_index;
    let window_floor = head.saturating_sub(self.retention_min_entries);
    let replica_floor = state
      .replica_progress
      .values()
      .filter(|progress| progress.epoch == state.manifest.epoch)
      .map(|progress| progress.applied_log_index.saturating_add(1))
      .min();
    let target_floor = window_floor
      .min(replica_floor.unwrap_or(window_floor))
      .max(state.manifest.retained_floor);

    let mut next_manifest = state.manifest.clone();
    next_manifest.retained_floor = target_floor;
    let retention_cutoff = self
      .retention_min_duration
      .and_then(|duration| SystemTime::now().checked_sub(duration));

    let active_segment_id = next_manifest.active_segment_id;
    let mut pruned_ids = Vec::new();
    let mut retained_segments = Vec::with_capacity(next_manifest.segments.len());
    for segment in &next_manifest.segments {
      if segment.id == active_segment_id {
        retained_segments.push(segment.clone());
        continue;
      }

      let prune_by_index = segment.end_log_index > 0 && segment.end_log_index < target_floor;
      if !prune_by_index {
        retained_segments.push(segment.clone());
        continue;
      }

      if !self.segment_old_enough_for_prune(segment.id, retention_cutoff)? {
        retained_segments.push(segment.clone());
        continue;
      }

      pruned_ids.push(segment.id);
    }
    next_manifest.segments = retained_segments;
    ensure_active_segment_metadata(&mut next_manifest);

    self.manifest_store.write(&next_manifest)?;
    state.manifest_disk_stamp = read_manifest_disk_stamp(self.manifest_store.path())?;
    state.manifest = next_manifest;
    state.manifest_dirty = false;
    state.appends_since_manifest_refresh = 0;

    for id in &pruned_ids {
      let segment_path = self.sidecar_path.join(segment_file_name(*id));
      if segment_path.exists() {
        std::fs::remove_file(&segment_path)?;
      }
    }

    Ok(PrimaryRetentionOutcome {
      pruned_segments: pruned_ids.len(),
      retained_floor: target_floor,
    })
  }

  pub fn last_token(&self) -> Option<CommitToken> {
    self.state.lock().last_token
  }

  /// The head frame ends the active segment, or, when that segment is still
  /// empty, a segment with a lower id; either way a cursor at the active
  /// segment's end with the head's log index follows it. Frames after the
  /// head have higher log indexes, and a promoted epoch's frames the new
  /// epoch, so the cursor precedes all of them. The end is the larger of
  /// this instance's count (which includes frames still buffered) and the
  /// file's length (which includes frames another instance on this sidecar
  /// appended).
  pub fn snapshot_position(&self) -> PrimarySnapshotPosition {
    let state = self.state.lock();
    let manifest = &state.manifest;
    let active_segment_path = self
      .sidecar_path
      .join(segment_file_name(manifest.active_segment_id));
    let active_segment_end = segment_file_len(&active_segment_path)
      .unwrap_or(0)
      .max(state.active_segment_size_bytes);
    PrimarySnapshotPosition {
      epoch: manifest.epoch,
      head_log_index: manifest.head_log_index,
      retained_floor: manifest.retained_floor,
      generation: manifest.generation,
      start_cursor: ReplicationCursor::new(
        manifest.epoch,
        manifest.active_segment_id,
        active_segment_end,
        manifest.head_log_index,
      ),
    }
  }

  pub fn status(&self) -> PrimaryReplicationStatus {
    let state = self.state.lock();
    let mut replica_lags: Vec<ReplicaLagStatus> = state
      .replica_progress
      .iter()
      .map(|(replica_id, progress)| ReplicaLagStatus {
        replica_id: replica_id.clone(),
        epoch: progress.epoch,
        applied_log_index: progress.applied_log_index,
      })
      .collect();
    replica_lags.sort_by(|left, right| left.replica_id.cmp(&right.replica_id));

    PrimaryReplicationStatus {
      role: ReplicationRole::Primary,
      epoch: state.manifest.epoch,
      head_log_index: state.manifest.head_log_index,
      retained_floor: state.manifest.retained_floor,
      replica_lags,
      sidecar_path: self.sidecar_path.clone(),
      last_token: state.last_token,
      last_replication_error: state.last_replication_error.clone(),
      sidecar_needs_repair: state.sidecar_needs_repair,
      append_attempts: self.append_attempts.load(Ordering::Relaxed),
      append_failures: self.append_failures.load(Ordering::Relaxed),
      append_successes: self.append_successes.load(Ordering::Relaxed),
    }
  }

  fn mark_append_failure(&self, error: &KiteError) {
    let _sidecar_guard = self.sidecar_op_lock.lock();
    let health = {
      let mut state = self.state.lock();
      if !state.sidecar_needs_repair || state.last_replication_error.is_none() {
        state.last_replication_error = Some(error.to_string());
      }
      state.sidecar_needs_repair = true;
      state.write_fenced = true;
      let health = PrimarySidecarHealth {
        version: PRIMARY_HEALTH_VERSION,
        last_replication_error: state.last_replication_error.clone(),
        sidecar_needs_repair: true,
      };
      *self.health.lock() = health.clone();
      health
    };
    persist_health_best_effort(&self.health_store, &health);
  }

  /// Adopt a repair fence another instance on this sidecar recorded.
  fn refresh_health_locked(&self, state: &mut PrimaryReplicationState) {
    let health = self.health.lock();
    if !health.sidecar_needs_repair {
      return;
    }

    state.sidecar_needs_repair = true;
    state.write_fenced = true;
    if state.last_replication_error.is_none() {
      state.last_replication_error = health.last_replication_error.clone();
    }
  }

  pub fn flush_for_transport_export(&self) -> Result<()> {
    let _sidecar_guard = self.sidecar_op_lock.lock();
    let mut state = self.state.lock();
    self.publish_locked(&mut state, false)
  }

  /// See `PrimaryReplication::publish_for_checkpoint`. Full sync appends are
  /// durable, with their manifest, as they return, so only buffered modes
  /// have anything to do. The frames are synced before `publish_locked`
  /// persists the manifest and removes the `primary-unflushed` marker.
  pub fn publish_for_checkpoint(&self) -> Result<()> {
    if self.durable_append {
      return Ok(());
    }
    let result = {
      let _sidecar_guard = self.sidecar_op_lock.lock();
      let mut state = self.state.lock();
      state
        .log_store
        .sync()
        .and_then(|()| self.publish_locked(&mut state, true))
    };
    if let Err(error) = &result {
      self.mark_append_failure(error);
    }
    result
  }

  /// Record on disk that frames are about to sit in memory, before the first
  /// buffered append after a publish. Full sync appends never buffer.
  fn note_frame_buffering_locked(&self, state: &mut PrimaryReplicationState) -> Result<()> {
    if self.durable_append || state.unflushed_marker {
      return Ok(());
    }
    OpenOptions::new()
      .create(true)
      .truncate(true)
      .write(true)
      .open(&self.unflushed_marker_path)?;
    state.unflushed_marker = true;
    Ok(())
  }

  /// Publisher tick: write buffered frames to the segment file, and persist
  /// the manifest once a full tick passed without appends (a busy primary
  /// still persists it every `DEFAULT_MANIFEST_REFRESH_APPEND_INTERVAL`).
  fn publish_buffered_frames(&self) {
    {
      let state = self.state.lock();
      if !state.appended_since_publish && !state.manifest_dirty && !state.unflushed_marker {
        return;
      }
    }

    let result = {
      let _sidecar_guard = self.sidecar_op_lock.lock();
      let mut state = self.state.lock();
      let idle = !std::mem::take(&mut state.appended_since_publish);
      self.publish_locked(&mut state, idle)
    };
    if let Err(error) = result {
      self.mark_append_failure(&error);
    }
  }

  /// Final publish when the primary closes.
  fn publish_on_close(&self) {
    let result = {
      let _sidecar_guard = self.sidecar_op_lock.lock();
      let mut state = self.state.lock();
      self.publish_locked(&mut state, true)
    };
    if let Err(error) = result {
      self.mark_append_failure(&error);
    }
  }

  fn publish_locked(
    &self,
    state: &mut PrimaryReplicationState,
    persist_manifest: bool,
  ) -> Result<()> {
    state.log_store.flush()?;

    if persist_manifest && state.manifest_dirty {
      // A fenced sidecar keeps its manifest for repair, and a manifest that
      // another primary instance advanced is never overwritten.
      if !state.sidecar_needs_repair && !self.refresh_manifest_locked(state)? && !state.write_fenced
      {
        self.manifest_store.write(&state.manifest)?;
        state.manifest_disk_stamp = read_manifest_disk_stamp(self.manifest_store.path())?;
        state.appends_since_manifest_refresh = 0;
      }
      state.manifest_dirty = false;
    }

    if state.unflushed_marker && !state.log_store.has_buffered_frames() {
      remove_unflushed_marker(&self.unflushed_marker_path)?;
      state.unflushed_marker = false;
    }
    Ok(())
  }

  fn refresh_manifest_locked(&self, state: &mut PrimaryReplicationState) -> Result<bool> {
    let disk_stamp = read_manifest_disk_stamp(self.manifest_store.path())?;
    if disk_stamp == state.manifest_disk_stamp {
      return Ok(false);
    }

    let mut persisted = self.manifest_store.read()?;
    ensure_active_segment_metadata(&mut persisted);

    let epoch_changed = persisted.epoch != state.manifest.epoch;
    let active_changed = persisted.active_segment_id != state.manifest.active_segment_id;
    state.manifest_disk_stamp = disk_stamp;

    if epoch_changed {
      state.write_fenced = true;
      state.superseded = true;
      state.manifest = persisted;
      self
        .epoch_fence
        .store(state.manifest.epoch, Ordering::Release);
      if active_changed {
        state.log_store = SegmentLogStore::open_append(
          self
            .sidecar_path
            .join(segment_file_name(state.manifest.active_segment_id)),
          self.append_write_buffer_bytes,
          self.sync,
        )?;
        state.active_segment_size_bytes = segment_file_len(
          &self
            .sidecar_path
            .join(segment_file_name(state.manifest.active_segment_id)),
        )?;
      }
      return Ok(true);
    }

    if self.persist_manifest_each_append {
      state.manifest = persisted;
      if active_changed {
        state.log_store = SegmentLogStore::open_append(
          self
            .sidecar_path
            .join(segment_file_name(state.manifest.active_segment_id)),
          self.append_write_buffer_bytes,
          self.sync,
        )?;
        state.active_segment_size_bytes = segment_file_len(
          &self
            .sidecar_path
            .join(segment_file_name(state.manifest.active_segment_id)),
        )?;
      }
      return Ok(false);
    }

    if active_changed {
      state.write_fenced = true;
      state.manifest = persisted;
      self
        .epoch_fence
        .store(state.manifest.epoch, Ordering::Release);
      state.log_store = SegmentLogStore::open_append(
        self
          .sidecar_path
          .join(segment_file_name(state.manifest.active_segment_id)),
        self.append_write_buffer_bytes,
        self.sync,
      )?;
      state.active_segment_size_bytes = segment_file_len(
        &self
          .sidecar_path
          .join(segment_file_name(state.manifest.active_segment_id)),
      )?;
      return Ok(false);
    }

    if persisted.retained_floor > state.manifest.retained_floor {
      state.manifest.retained_floor = persisted.retained_floor;
    }

    Ok(false)
  }

  fn refresh_replica_progress_locked(&self, state: &mut PrimaryReplicationState) -> Result<()> {
    state.replica_progress = load_replica_progress(&self.sidecar_path)?;
    Ok(())
  }

  fn segment_old_enough_for_prune(
    &self,
    segment_id: u64,
    retention_cutoff: Option<SystemTime>,
  ) -> Result<bool> {
    let Some(cutoff) = retention_cutoff else {
      return Ok(true);
    };

    let segment_path = self.sidecar_path.join(segment_file_name(segment_id));
    let metadata = match std::fs::metadata(&segment_path) {
      Ok(metadata) => metadata,
      Err(error) if error.kind() == ErrorKind::NotFound => return Ok(true),
      Err(error) => return Err(error.into()),
    };

    let modified = match metadata.modified() {
      Ok(modified) => modified,
      Err(_) => return Ok(false),
    };

    Ok(modified <= cutoff)
  }
}

pub fn default_replication_sidecar_path(db_path: &Path) -> PathBuf {
  let file_name = db_path
    .file_name()
    .map(|name| format!("{}.replication", name.to_string_lossy()))
    .unwrap_or_else(|| "replication-sidecar".to_string());

  match db_path.parent() {
    Some(parent) => parent.join(file_name),
    None => PathBuf::from(file_name),
  }
}

fn ensure_active_segment_metadata(manifest: &mut ReplicationManifest) {
  let active_id = manifest.active_segment_id;
  if manifest.segments.iter().any(|entry| entry.id == active_id) {
    return;
  }

  let start = manifest.head_log_index.saturating_add(1);
  manifest.segments.push(SegmentMeta {
    id: active_id,
    start_log_index: start,
    end_log_index: start.saturating_sub(1),
    size_bytes: 0,
  });
}

fn segment_file_name(id: u64) -> String {
  format!("segment-{id:020}.rlog")
}

fn sidecar_last_txid(sidecar_path: &Path, manifest: &ReplicationManifest) -> Result<Option<u64>> {
  let Some(frame) = sidecar_last_frame(sidecar_path, manifest)? else {
    return Ok(None);
  };
  if frame.log_index != manifest.head_log_index {
    return Err(KiteError::InvalidReplication(format!(
      "replication sidecar manifest head {} does not match last frame {}",
      manifest.head_log_index, frame.log_index
    )));
  }
  Ok(Some(decode_commit_frame_payload(&frame.payload)?.txid))
}

fn sidecar_last_frame(
  sidecar_path: &Path,
  manifest: &ReplicationManifest,
) -> Result<Option<ReplicationFrame>> {
  let Some(segment) = manifest
    .segments
    .iter()
    .filter(|segment| segment.end_log_index > 0)
    .max_by_key(|segment| (segment.end_log_index, segment.id))
  else {
    return Ok(None);
  };

  let segment_path = sidecar_path.join(segment_file_name(segment.id));
  if !segment_path.exists() {
    return Ok(None);
  }
  let frames = SegmentLogStore::open(&segment_path)?.read_all()?;
  Ok(frames.into_iter().last())
}

fn reconcile_manifest_head_from_active_segment(
  sidecar_path: &Path,
  manifest: &mut ReplicationManifest,
) -> Result<bool> {
  let segment_path = sidecar_path.join(segment_file_name(manifest.active_segment_id));
  if !segment_path.exists() {
    return Ok(false);
  }

  let (_, _, last_seen) =
    SegmentLogStore::open(&segment_path)?.read_filtered_from_offset(0, |_| false, 0)?;
  let Some((segment_epoch, segment_head_log_index)) = last_seen else {
    return Ok(false);
  };

  if segment_epoch != manifest.epoch || segment_head_log_index <= manifest.head_log_index {
    return Ok(false);
  }

  manifest.head_log_index = segment_head_log_index;
  if let Some(active_segment) = manifest
    .segments
    .iter_mut()
    .find(|entry| entry.id == manifest.active_segment_id)
  {
    if active_segment.end_log_index < segment_head_log_index {
      active_segment.end_log_index = segment_head_log_index;
    }
    if active_segment.start_log_index > active_segment.end_log_index {
      active_segment.start_log_index = active_segment.end_log_index;
    }
    active_segment.size_bytes = segment_file_len(&segment_path)?;
  }

  ensure_active_segment_metadata(manifest);
  Ok(true)
}

fn remove_unflushed_marker(path: &Path) -> Result<()> {
  match std::fs::remove_file(path) {
    Ok(()) => Ok(()),
    Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
    Err(error) => Err(error.into()),
  }
}

/// Whether a primary sidecar is fenced for repair/resync (`primary-health.json`).
pub fn primary_sidecar_needs_repair(sidecar_path: &Path) -> Result<bool> {
  Ok(
    PrimarySidecarHealthStore::new(sidecar_path, SidecarSync::default())
      .read()?
      .is_some_and(|health| health.sidecar_needs_repair),
  )
}

fn stale_primary_error() -> KiteError {
  KiteError::InvalidReplication("stale primary is fenced for writes".to_string())
}

fn sidecar_repair_error() -> KiteError {
  KiteError::InvalidReplication(
    "primary replication sidecar needs repair/resync; later appends are fenced".to_string(),
  )
}

fn persist_health_best_effort(store: &PrimarySidecarHealthStore, health: &PrimarySidecarHealth) {
  if let Err(error) = store.write(health) {
    eprintln!("Warning: failed to persist primary replication sidecar health state: {error}");
  }
}

fn read_manifest_disk_stamp(path: &Path) -> Result<ManifestDiskStamp> {
  let metadata = std::fs::metadata(path)?;
  let modified_unix_nanos = metadata
    .modified()
    .ok()
    .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
    .map(|value| value.as_nanos());

  Ok(ManifestDiskStamp {
    len: metadata.len(),
    modified_unix_nanos,
  })
}

fn segment_file_len(path: &Path) -> Result<u64> {
  match std::fs::metadata(path) {
    Ok(metadata) => Ok(metadata.len()),
    Err(error) if error.kind() == ErrorKind::NotFound => Ok(0),
    Err(error) => Err(error.into()),
  }
}

fn sidecar_operation_lock(sidecar_path: &Path) -> SidecarOpLock {
  let registry = SIDECAR_LOCKS.get_or_init(|| StdMutex::new(HashMap::new()));
  let mut registry = registry.lock().expect("sidecar lock registry poisoned");
  registry
    .entry(sidecar_path.to_path_buf())
    .or_insert_with(|| Arc::new(Mutex::new(())))
    .clone()
}

/// Take the sidecar's primary lock, or share it with the instance in this
/// process that holds it. The flag is true when the lock was shared: a live
/// primary instance in this process owns the sidecar.
fn acquire_sidecar_primary_lock(sidecar_path: &Path) -> Result<(SidecarPrimaryLock, bool)> {
  let key = normalize_sidecar_path(sidecar_path);
  let registry = SIDECAR_PRIMARY_LOCKS.get_or_init(|| StdMutex::new(HashMap::new()));
  let mut registry = registry
    .lock()
    .map_err(|_| KiteError::LockFailed("primary sidecar lock registry poisoned".to_string()))?;

  if let Some(existing) = registry.get(&key).and_then(Weak::upgrade) {
    return Ok((existing, true));
  }

  let lock_path = key.join(PRIMARY_LOCK_FILE_NAME);
  let lock_file = OpenOptions::new()
    .create(true)
    .truncate(false)
    .read(true)
    .write(true)
    .open(&lock_path)?;
  lock_file.try_lock_exclusive().map_err(|error| {
    KiteError::LockFailed(format!(
      "primary sidecar lock is held by another process: {} ({error})",
      lock_path.display()
    ))
  })?;

  let lock = Arc::new(PrimarySidecarProcessLock { file: lock_file });
  registry.insert(key, Arc::downgrade(&lock));
  Ok((lock, false))
}

/// The in-process copy of the sidecar's `primary-health.json`, shared by
/// every instance on it; loaded from the file by the first. Only this process
/// writes the file while it holds the sidecar's primary lock, so appends and
/// commits read the copy instead of the file.
fn shared_sidecar_health(
  sidecar_path: &Path,
  store: &PrimarySidecarHealthStore,
) -> Result<SharedSidecarHealth> {
  let key = normalize_sidecar_path(sidecar_path);
  let registry = SIDECAR_HEALTH.get_or_init(|| StdMutex::new(HashMap::new()));
  let mut registry = registry
    .lock()
    .map_err(|_| KiteError::LockFailed("sidecar health registry poisoned".to_string()))?;
  if let Some(existing) = registry.get(&key).and_then(Weak::upgrade) {
    return Ok(existing);
  }
  let health = Arc::new(Mutex::new(store.read()?.unwrap_or_default()));
  registry.insert(key, Arc::downgrade(&health));
  Ok(health)
}

fn sidecar_epoch_fence(sidecar_path: &Path, initial_epoch: u64) -> SidecarEpochFence {
  let key = normalize_sidecar_path(sidecar_path);
  let registry = SIDECAR_EPOCH_FENCES.get_or_init(|| StdMutex::new(HashMap::new()));
  let mut registry = registry
    .lock()
    .expect("sidecar epoch fence registry poisoned");
  let entry = registry
    .entry(key)
    .or_insert_with(|| Arc::downgrade(&Arc::new(AtomicU64::new(initial_epoch))));
  let fence = if let Some(existing) = entry.upgrade() {
    existing
  } else {
    let created = Arc::new(AtomicU64::new(initial_epoch));
    *entry = Arc::downgrade(&created);
    created
  };
  fence.fetch_max(initial_epoch, Ordering::AcqRel);
  fence
}

fn normalize_sidecar_path(path: &Path) -> PathBuf {
  std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// raydb-b4 repl-flake: closing a primary releases `primary.lock` while a
/// spawned child still holds a copy of its descriptor.
#[cfg(all(test, unix))]
#[path = "b4_repl_flake_tests.rs"]
mod b4_repl_flake_tests;
