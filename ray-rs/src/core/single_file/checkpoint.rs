//! Checkpoint operations for SingleFileDB
//!
//! Handles merging snapshot + delta into a new snapshot, clearing WAL.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use parking_lot::RwLockWriteGuard;

#[cfg(test)]
use std::cell::RefCell;
#[cfg(test)]
use std::sync::{Arc, Barrier, Mutex, OnceLock};

use crate::core::pager::{pages_to_store, FilePager};
use crate::core::snapshot::reader::SnapshotData;
use crate::core::snapshot::writer::{
  build_snapshot_to_memory, EdgeData, NodeData, SnapshotBuildInput,
};
use crate::core::wal::buffer::WalBuffer;
use crate::core::wal::record::ParsedWalRecord;
use crate::error::{KiteError, Result};
use crate::types::*;
use crate::vector::store::{create_vector_store, vector_store_delete, vector_store_insert};
use crate::vector::types::{VectorManifest, VectorStoreConfig};

use super::open::map_snapshot_range;
use super::recovery::{committed_transactions, replay_wal_record};
use super::{CheckpointStatus, SingleFileDB};
use crate::vector::ivf::serialize::validate_manifest_for_serialization;

type GraphData = (
  Vec<NodeData>,
  Vec<EdgeData>,
  HashMap<LabelId, String>,
  HashMap<ETypeId, String>,
  HashMap<PropKeyId, String>,
  HashMap<PropKeyId, VectorManifest>,
);

/// Bytes of a new snapshot written per positioned write.
const SNAPSHOT_WRITE_CHUNK: usize = 1 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckpointPhase {
  GateAcquired,
  /// A background checkpoint's cut is durable and the gate is open again; its
  /// snapshot is not built yet.
  CutReleased,
  /// A chunk of the new snapshot's pages is written.
  SnapshotPageWritten,
  /// Every page of the new snapshot is written; the sync comes next.
  SnapshotWritten,
  SnapshotDurable,
  HeaderWritten,
  HeaderDurable,
  /// A background checkpoint's header naming the post-cut records in the
  /// secondary region is durable; they are not yet compacted into primary.
  PostCutWalRetained,
  /// A checkpoint is about to map and parse the snapshot it wrote.
  SnapshotReload,
  /// A background checkpoint is about to replay its post-cut records into the
  /// delta that replaces the cut's.
  PostCutReplay,
}

/// A barrier armed for one phase of checkpoints on the database at a path.
#[cfg(test)]
type CheckpointTestBarrier = (std::path::PathBuf, CheckpointPhase, Arc<Barrier>);

#[cfg(test)]
thread_local! {
  static CHECKPOINT_TEST_FAULT: RefCell<Option<CheckpointPhase>> = const { RefCell::new(None) };
  static CHECKPOINT_TEST_PANIC: RefCell<Option<CheckpointPhase>> = const { RefCell::new(None) };
}
#[cfg(test)]
static CHECKPOINT_TEST_BARRIERS: OnceLock<Mutex<Vec<CheckpointTestBarrier>>> = OnceLock::new();
#[cfg(test)]
static CHECKPOINT_TEST_SERIAL: OnceLock<Mutex<()>> = OnceLock::new();
/// Background checkpoint cuts attempted, per database path.
#[cfg(test)]
static CHECKPOINT_TEST_CUTS: OnceLock<Mutex<HashMap<std::path::PathBuf, usize>>> = OnceLock::new();
/// A delay armed for one named step of checkpoints on the database at a path.
#[cfg(test)]
type CheckpointTestDelay = (std::path::PathBuf, &'static str, Duration);
#[cfg(test)]
static CHECKPOINT_TEST_DELAYS: OnceLock<Mutex<Vec<CheckpointTestDelay>>> = OnceLock::new();
/// Stall timeouts lowered for the database at a path.
#[cfg(test)]
static CHECKPOINT_TEST_STALL_TIMEOUTS: OnceLock<Mutex<HashMap<std::path::PathBuf, Duration>>> =
  OnceLock::new();

fn checkpoint_phase(db_path: &std::path::Path, phase: CheckpointPhase) -> Result<()> {
  #[cfg(test)]
  {
    let barrier = {
      let mut configured = CHECKPOINT_TEST_BARRIERS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .expect("checkpoint test barrier lock");
      configured
        .iter()
        .position(|(path, barrier_phase, _)| path == db_path && *barrier_phase == phase)
        .map(|index| configured.remove(index).2)
    };
    let faulted = CHECKPOINT_TEST_FAULT.with(|fault| {
      let mut fault = fault.borrow_mut();
      if *fault == Some(phase) {
        *fault = None;
        true
      } else {
        false
      }
    });
    if faulted {
      return Err(KiteError::Internal("injected checkpoint abort".to_string()));
    }
    if let Some(barrier) = barrier {
      barrier.wait();
    }
    let panics = CHECKPOINT_TEST_PANIC.with(|armed| {
      let mut armed = armed.borrow_mut();
      armed.take_if(|armed| *armed == phase).is_some()
    });
    if panics {
      panic!("injected checkpoint panic at {phase:?}");
    }
  }

  let _ = (db_path, phase);
  Ok(())
}

#[cfg(test)]
fn set_checkpoint_test_fault(phase: Option<CheckpointPhase>) {
  CHECKPOINT_TEST_FAULT.with(|fault| *fault.borrow_mut() = phase);
}

/// Panic on this thread when its next checkpoint reaches `phase` (after any
/// barrier armed for that phase).
#[cfg(test)]
fn set_checkpoint_test_panic(phase: Option<CheckpointPhase>) {
  CHECKPOINT_TEST_PANIC.with(|armed| *armed.borrow_mut() = phase);
}

#[cfg(test)]
fn count_checkpoint_test_cut(db_path: &std::path::Path) {
  *CHECKPOINT_TEST_CUTS
    .get_or_init(|| Mutex::new(HashMap::new()))
    .lock()
    .expect("checkpoint test cut counter lock")
    .entry(db_path.to_path_buf())
    .or_default() += 1;
}

/// Background checkpoint cuts attempted on `db` so far.
#[cfg(test)]
fn checkpoint_test_cuts(db: &SingleFileDB) -> usize {
  CHECKPOINT_TEST_CUTS
    .get_or_init(|| Mutex::new(HashMap::new()))
    .lock()
    .expect("checkpoint test cut counter lock")
    .get(db.path())
    .copied()
    .unwrap_or(0)
}

/// Park the next thread that reaches `phase` of a checkpoint on `db` on
/// `barrier`, replacing any barrier still armed for that phase. Barriers are
/// per database, so tests running in parallel never take each other's.
#[cfg(test)]
fn set_checkpoint_test_barrier(db: &SingleFileDB, phase: CheckpointPhase, barrier: Arc<Barrier>) {
  let mut configured = CHECKPOINT_TEST_BARRIERS
    .get_or_init(|| Mutex::new(Vec::new()))
    .lock()
    .expect("checkpoint test barrier lock");
  configured.retain(|(path, barrier_phase, _)| path != db.path() || *barrier_phase != phase);
  configured.push((db.path().to_path_buf(), phase, barrier));
}

/// Make the next `step` (see `SingleFileDB::checkpoint_step`) of a checkpoint
/// on `db` take `delay` longer, noting no progress meanwhile, like a slow
/// serialization or fsync of a large snapshot.
#[cfg(test)]
fn set_checkpoint_test_step_delay(db: &SingleFileDB, step: &'static str, delay: Duration) {
  CHECKPOINT_TEST_DELAYS
    .get_or_init(|| Mutex::new(Vec::new()))
    .lock()
    .expect("checkpoint test delay lock")
    .push((db.path().to_path_buf(), step, delay));
}

#[cfg(test)]
fn delay_checkpoint_test_step(db_path: &std::path::Path, step: &'static str) {
  let delay = {
    let mut armed = CHECKPOINT_TEST_DELAYS
      .get_or_init(|| Mutex::new(Vec::new()))
      .lock()
      .expect("checkpoint test delay lock");
    armed
      .iter()
      .position(|(path, armed_step, _)| path == db_path && *armed_step == step)
      .map(|index| armed.remove(index).2)
  };
  if let Some(delay) = delay {
    std::thread::sleep(delay);
  }
}

/// Use `timeout` instead of `CHECKPOINT_STALL_TIMEOUT` for `db`.
#[cfg(test)]
fn set_checkpoint_test_stall_timeout(db: &SingleFileDB, timeout: Duration) {
  CHECKPOINT_TEST_STALL_TIMEOUTS
    .get_or_init(|| Mutex::new(HashMap::new()))
    .lock()
    .expect("checkpoint test stall timeout lock")
    .insert(db.path().to_path_buf(), timeout);
}

/// How long the background checkpoint of the database at `db_path` may look
/// stalled before writers waiting for its install cancel its cut.
fn checkpoint_stall_timeout(db_path: &std::path::Path) -> Duration {
  #[cfg(test)]
  if let Some(timeout) = CHECKPOINT_TEST_STALL_TIMEOUTS
    .get_or_init(|| Mutex::new(HashMap::new()))
    .lock()
    .expect("checkpoint test stall timeout lock")
    .get(db_path)
  {
    return *timeout;
  }
  let _ = db_path;
  CHECKPOINT_STALL_TIMEOUT
}

#[cfg(test)]
fn checkpoint_test_serial() -> std::sync::MutexGuard<'static, ()> {
  CHECKPOINT_TEST_SERIAL
    .get_or_init(|| Mutex::new(()))
    .lock()
    .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A snapshot written and synced to pages that no valid header names yet.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WrittenSnapshot {
  pub(crate) generation: u64,
  pub(crate) start_page: u64,
  pub(crate) page_count: u64,
}

/// A snapshot mapped and parsed (see `SingleFileDB::load_snapshot`), with
/// its vector stores, ready to replace the in-memory one.
#[derive(Default)]
pub(crate) struct LoadedSnapshot {
  snapshot: Option<SnapshotData>,
  vector_stores: HashMap<PropKeyId, VectorManifest>,
}

impl LoadedSnapshot {
  /// Apply vector operations replayed from the WAL to these stores.
  fn apply_pending_vectors(
    &mut self,
    pending_vectors: &HashMap<(NodeId, PropKeyId), Option<VectorRef>>,
  ) -> Result<()> {
    for (&(node_id, prop_key_id), operation) in pending_vectors {
      match operation {
        Some(vector) => {
          let store = self
            .vector_stores
            .entry(prop_key_id)
            .or_insert_with(|| create_vector_store(VectorStoreConfig::new(vector.len())));
          vector_store_insert(store, node_id, vector.as_ref()).map_err(|error| {
            KiteError::Internal(format!(
              "Failed to apply vector insert for node {node_id} (prop {prop_key_id}): {error}"
            ))
          })?;
        }
        None => {
          if let Some(store) = self.vector_stores.get_mut(&prop_key_id) {
            vector_store_delete(store, node_id);
          }
        }
      }
    }
    Ok(())
  }
}

/// Check that every vector store a snapshot is about to hold decodes back as
/// itself (`validate_manifest_for_serialization`, which accepts what
/// `deserialize_manifest` accepts), and return a copy of them to install with
/// the snapshot. Copying is cheap next to decoding the snapshot's copy.
pub(super) fn snapshot_vector_stores(
  vector_stores: &HashMap<PropKeyId, VectorManifest>,
) -> Result<HashMap<PropKeyId, VectorManifest>> {
  for (prop_key_id, store) in vector_stores {
    validate_manifest_for_serialization(store).map_err(|error| {
      KiteError::InvalidSnapshot(format!(
        "vector store for prop key {prop_key_id} would not decode: {error}"
      ))
    })?;
  }
  Ok(vector_stores.clone())
}

/// A background checkpoint's replay of the transactions committed after its
/// cut, in rounds (see `SingleFileDB::replay_post_cut_records`).
#[derive(Default)]
struct PostCutReplay {
  /// The committed transactions replayed so far.
  delta: DeltaState,
  /// Records of the transactions not committed or rolled back by the last
  /// record seen, in order; the next round's records continue them.
  unfinished: Vec<ParsedWalRecord>,
  /// Every post-cut record read so far, as it lies in the secondary region,
  /// for the install's move-back to reuse.
  read: Vec<u8>,
}

/// The records of `records`' transactions that neither commit nor roll back
/// in them.
fn unfinished_transactions(records: Vec<ParsedWalRecord>) -> Vec<ParsedWalRecord> {
  let mut open = std::collections::HashSet::new();
  for record in &records {
    match record.record_type {
      WalRecordType::Begin => {
        open.insert(record.txid);
      }
      WalRecordType::Commit | WalRecordType::Rollback => {
        open.remove(&record.txid);
      }
      _ => {}
    }
  }
  records
    .into_iter()
    .filter(|record| open.contains(&record.txid))
    .collect()
}

/// Return `header` to `prior` after a failed header write, keeping the newer
/// change counter so the next write still outranks every slot on disk.
fn restore_header(header: &mut DbHeaderV1, prior: DbHeaderV1) {
  let change_counter = header.change_counter;
  *header = prior;
  header.change_counter = change_counter;
}

/// How long a background checkpoint may look stalled while writers wait for
/// its install before they cancel its cut; see `wait_for_cut_release`. It
/// looks stalled only outside every `checkpoint_step` and with no progress
/// noted (at every phase, snapshot page, and few thousand nodes collected),
/// which for a working checkpoint lasts microseconds: in practice only a
/// stopped thread (parked by a test or a debugger) looks stalled.
const CHECKPOINT_STALL_TIMEOUT: Duration = Duration::from_secs(5);
/// How often a writer waiting for a background install checks its progress.
const CUT_WAIT_POLL: Duration = Duration::from_millis(50);
/// Further passes a background checkpoint takes while writers keep waiting
/// for its installs.
const MAX_EXTRA_BACKGROUND_PASSES: usize = 4;

/// What a background checkpoint's cut left the run to do.
enum Cut {
  /// The run owns a cut; the committed delta as of the cut.
  Taken(Box<DeltaState>),
  /// A cut left in place that the run cannot resume; with no transaction
  /// open, a blocking checkpoint finishes it.
  FinishBlocking,
}

/// How a background checkpoint call ended, short of an error.
enum BackgroundCheckpointOutcome {
  /// It installed at least one snapshot.
  Installed,
  /// Another background checkpoint is running.
  AlreadyRunning,
  /// It did not start: the last cut declined, and every transaction it would
  /// have had to copy is still open (see `cut_still_declined`).
  StillDeclined,
  /// It did not start: a blocking checkpoint, optimize, vacuum or WAL resize
  /// is waiting for the checkpoint gate (see `exclusive_waiters`).
  ExclusiveWaiting,
}

/// A caller of `exclusive_checkpoint_gate`, registered in
/// `BackgroundCheckpointState::exclusive_waiters` until dropped.
struct ExclusiveWaiter<'db> {
  db: &'db SingleFileDB,
}

impl Drop for ExclusiveWaiter<'_> {
  fn drop(&mut self) {
    self.db.checkpoint_state.lock().exclusive_waiters -= 1;
  }
}

/// Whether a cut declined (see `cut_background_checkpoint`), changing
/// nothing, rather than failing.
fn is_declined_cut(error: &KiteError) -> bool {
  matches!(error, KiteError::CheckpointDeclined(_))
}

/// The error of a background checkpoint whose cut writers cancelled.
fn cancelled_checkpoint_error() -> KiteError {
  KiteError::Internal(
    "background checkpoint cancelled: it looked stalled while writers waited for it to free WAL \
     space"
      .to_string(),
  )
}

/// A checkpoint step that notes no progress until it ends, from
/// `SingleFileDB::checkpoint_step` until dropped.
struct CheckpointStep<'db> {
  db: &'db SingleFileDB,
}

impl Drop for CheckpointStep<'_> {
  fn drop(&mut self) {
    self
      .db
      .checkpoint_steps_running
      .fetch_sub(1, Ordering::AcqRel);
    self.db.checkpoint_progress.fetch_add(1, Ordering::Relaxed);
  }
}

/// A background checkpoint run, from its claim of the checkpoint status until
/// it ends. Dropping it (also when the run unwinds from a panic) returns the
/// status to idle, once and only if no later run has claimed it, and wakes
/// waiters.
struct BackgroundCheckpointRun<'db> {
  db: &'db SingleFileDB,
  run: u64,
}

impl Drop for BackgroundCheckpointRun<'_> {
  fn drop(&mut self) {
    {
      let mut state = self.db.checkpoint_state.lock();
      // The run released its cut already unless it panicked. Then the cut
      // stays in the WAL as an abandoned cut, durable and replayable, for the
      // next checkpoint to finish, rather than being rewritten from state the
      // panic may have left half-updated.
      if state.cut_owner == Some(self.run) {
        state.cut_owner = None;
      }
      if state.run == self.run {
        // Cleared before the status goes idle: a checkpoint that sees it idle
        // must not take this run's cancellation for its own.
        self.db.checkpoint_cancelled.store(false, Ordering::Release);
        state.status = CheckpointStatus::Idle;
      }
    }
    self.db.notify_cut_waiters();
    self.db.notify_checkpoint_waiters();
  }
}

impl SingleFileDB {
  // ========================================================================
  // Blocking Checkpoint
  // ========================================================================

  /// Perform a checkpoint - merge snapshot + delta into new snapshot
  ///
  /// This:
  /// 1. Collects all graph data from snapshot + delta
  /// 2. Builds a new snapshot in memory
  /// 3. Writes the new snapshot to disk (after WAL)
  /// 4. Updates header to point to new snapshot
  /// 5. Clears WAL and delta
  ///
  /// Crash protocol: a crash during snapshot writes or before the header slot
  /// flip leaves the old header, old snapshot, and WAL authoritative. A crash
  /// after the new slot is durable but before reclaim leaves the new snapshot
  /// authoritative and old pages orphaned. Open selects the newest valid
  /// checksummed slot in all three cases:
  ///
  /// * mid-snapshot-write: only unreachable EOF pages are partial;
  /// * pre-header-flip: the old slot still names the intact snapshot and WAL;
  /// * post-flip/pre-reclaim: the new slot names a synced snapshot, while old
  ///   pages remain untouched and can be reclaimed later by vacuum.
  pub fn checkpoint(&self) -> Result<()> {
    if self.read_only {
      return Err(KiteError::ReadOnly);
    }

    // A caller cannot checkpoint its own open transaction. Other transactions
    // that began before the gate are allowed to finish; the gate prevents any
    // new transaction from entering while the snapshot is cut and installed.
    if self.current_tx_handle().is_some() {
      return Err(KiteError::TransactionInProgress);
    }
    let _checkpoint_gate = self.exclusive_checkpoint_gate()?;
    self.checkpoint_holding_gate()
  }

  /// The blocking checkpoint, for a caller that holds the checkpoint gate
  /// while no transaction is open. Holding the gate on afterwards keeps the
  /// WAL empty for work that needs it so (see `resize_wal`).
  pub(crate) fn checkpoint_holding_gate(&self) -> Result<()> {
    let graph = self.collect_graph_data()?;
    let header = self.header.read().clone();
    let generation = header.active_snapshot_gen + 1;
    let (snapshot_buffer, vector_stores) = self.build_snapshot_buffer(generation, graph)?;
    let snapshot = self.write_new_snapshot(&header, generation, &snapshot_buffer)?;
    let loaded = self.load_unnamed_snapshot(snapshot, vector_stores)?;

    // The snapshot covers every WAL record, so the installed header names an
    // empty WAL.
    {
      let mut pager = self.pager.lock();
      let mut wal_buffer = self.wal_buffer.lock();
      let mut header = self.header.write();
      self.install_snapshot(
        &mut pager,
        &mut wal_buffer,
        &mut header,
        snapshot,
        WalBuffer::reset,
      )?;
    }

    // The installed snapshot holds everything the delta did.
    self.install_loaded_snapshot(loaded, DeltaState::new());
    self.truncate_orphaned_tail()?;

    Ok(())
  }

  /// `load_snapshot` for a snapshot just written; if it cannot be loaded, its
  /// pages (which no header names) are freed.
  pub(crate) fn load_unnamed_snapshot(
    &self,
    snapshot: WrittenSnapshot,
    vector_stores: HashMap<PropKeyId, VectorManifest>,
  ) -> Result<LoadedSnapshot> {
    self
      .load_snapshot(&snapshot, vector_stores)
      .inspect_err(|_| {
        self
          .pager
          .lock()
          .free_pages(snapshot.start_page as u32, snapshot.page_count as u32);
      })
  }

  /// Take the checkpoint gate for work that replaces the snapshot or resets
  /// the WAL: no transaction is open, none can begin, and no background
  /// checkpoint is running. Running between a background checkpoint's cut and
  /// its install would erase its post-cut commits: the WAL holding them would
  /// be reset, and its install would then replace this snapshot with its
  /// older one.
  ///
  /// A running background checkpoint is waited for with the gate released,
  /// before waiting for open transactions: its install needs the gate, and a
  /// writer inside an open transaction may be waiting for that install (see
  /// `wait_for_cut_release`). While the gate is held, no background
  /// checkpoint can cut. A cut no run owns (a run stopped before its install)
  /// is just WAL to the caller: everything committed is in the delta, so a
  /// checkpoint replaces it, and compaction keeps it.
  ///
  /// The caller is registered as a waiter throughout, so no background
  /// checkpoint starts meanwhile: it waits for at most the run in progress.
  pub(crate) fn exclusive_checkpoint_gate(&self) -> Result<RwLockWriteGuard<'_, ()>> {
    self.checkpoint_state.lock().exclusive_waiters += 1;
    let _waiter = ExclusiveWaiter { db: self };
    loop {
      let checkpoint_gate = self.checkpoint_gate.write();
      // Only the test hook: a cancelled background run may still hold the
      // status (this waits for it below), so its cancellation is not ours.
      checkpoint_phase(&self.path, CheckpointPhase::GateAcquired)?;
      if !self.is_checkpoint_running() {
        self.wait_for_no_active_transactions();
        return Ok(checkpoint_gate);
      }
      drop(checkpoint_gate);
      self.wait_for_background_checkpoint();
    }
  }

  /// Map and parse `written`, a snapshot no header names yet, without
  /// installing it. Loading before the install means a failure leaves the
  /// database as it was; once the header is installed,
  /// `install_loaded_snapshot` cannot fail.
  ///
  /// Everything open would check or decode later is checked: the parse
  /// (footer CRC and structure) here, and the vector stores, which open
  /// leaves encoded until first use, before they were serialized
  /// (`snapshot_vector_stores`). A store that does not decode would otherwise
  /// be installed, then read as missing, and fail every later checkpoint.
  /// `vector_stores` are the stores the snapshot was built from, so they are
  /// installed as they are instead of decoding the snapshot's copy again.
  pub(crate) fn load_snapshot(
    &self,
    written: &WrittenSnapshot,
    vector_stores: HashMap<PropKeyId, VectorManifest>,
  ) -> Result<LoadedSnapshot> {
    self.reach_checkpoint_phase(CheckpointPhase::SnapshotReload)?;
    if written.page_count == 0 {
      return Ok(LoadedSnapshot::default());
    }
    let _step = self.checkpoint_step("load snapshot");

    // Map only the immutable snapshot range. Header and WAL pages remain
    // outside every live SnapshotData mapping.
    let mut range = self.header.read().clone();
    range.snapshot_start_page = written.start_page;
    range.snapshot_page_count = written.page_count;
    let mapped = map_snapshot_range(&self.pager.lock(), &range)?;
    let snapshot = SnapshotData::parse(
      mapped,
      &crate::core::snapshot::reader::ParseSnapshotOptions::default(),
    )?;
    Ok(LoadedSnapshot {
      snapshot: Some(snapshot),
      vector_stores,
    })
  }

  /// Make `loaded` the in-memory snapshot, with its vector stores, and
  /// `delta` the committed delta over it, in one critical section. Readers and
  /// transactions see the old pair or the new one, never a mix: the new
  /// snapshot under the old delta applies changes twice (a transaction would
  /// see an edge the delta had deleted, skip its own AddEdge record, and lose
  /// its add), and the old snapshot under the new delta drops commits.
  ///
  /// Takes `delta` before `snapshot`, the order every reader and commit uses
  /// (see read.rs). The replaced state is dropped after both are released.
  pub(crate) fn install_loaded_snapshot(&self, loaded: LoadedSnapshot, delta: DeltaState) {
    let _replaced = {
      let mut delta_guard = self.delta.write();
      let mut snapshot_guard = self.snapshot.write();
      (
        std::mem::replace(&mut *snapshot_guard, loaded.snapshot),
        std::mem::replace(&mut *self.vector_stores.write(), loaded.vector_stores),
        // Entries of the replaced snapshot; the new one's stores are decoded.
        std::mem::take(&mut *self.vector_store_lazy_entries.write()),
        std::mem::replace(&mut *delta_guard, delta),
      )
    };
  }

  // ========================================================================
  // Background Checkpoint (Non-Blocking)
  // ========================================================================

  /// Check if a background checkpoint is currently running
  pub fn is_checkpoint_running(&self) -> bool {
    matches!(
      self.checkpoint_status(),
      CheckpointStatus::Running | CheckpointStatus::Completing
    )
  }

  /// Get current checkpoint status
  pub fn checkpoint_status(&self) -> CheckpointStatus {
    self.checkpoint_state.lock().status
  }

  /// Block until no background checkpoint is running.
  fn wait_for_background_checkpoint(&self) {
    let mut wait = self.checkpoint_wait.lock();
    while self.is_checkpoint_running() {
      self.checkpoint_cv.wait(&mut wait);
    }
  }

  /// Claim the checkpoint status for a new background run, unless one is
  /// running or an exclusive operation waits for the gate.
  fn claim_background_checkpoint(
    &self,
  ) -> std::result::Result<BackgroundCheckpointRun<'_>, BackgroundCheckpointOutcome> {
    let mut state = self.checkpoint_state.lock();
    if state.status != CheckpointStatus::Idle {
      return Err(BackgroundCheckpointOutcome::AlreadyRunning);
    }
    if state.exclusive_waiters > 0 {
      return Err(BackgroundCheckpointOutcome::ExclusiveWaiting);
    }
    state.run += 1;
    state.status = CheckpointStatus::Running;
    Ok(BackgroundCheckpointRun {
      db: self,
      run: state.run,
    })
  }

  /// Whether a blocking checkpoint, optimize, vacuum or WAL resize waits for
  /// the checkpoint gate.
  fn exclusive_operation_waiting(&self) -> bool {
    self.checkpoint_state.lock().exclusive_waiters > 0
  }

  fn set_background_checkpoint_status(&self, run: u64, status: CheckpointStatus) {
    let mut state = self.checkpoint_state.lock();
    if state.run == run && state.status != CheckpointStatus::Idle {
      state.status = status;
    }
  }

  /// Note that a checkpoint made progress (see `wait_for_cut_release`), and
  /// stop a background run whose cut writers cancelled: no header will name
  /// the snapshot it is building, and the next checkpoint cannot start until
  /// it ends. Only the running background checkpoint can see the flag set: a
  /// blocking checkpoint or compaction waits for that run to end first.
  fn checkpoint_progressed(&self) -> Result<()> {
    self.checkpoint_progress.fetch_add(1, Ordering::Relaxed);
    if self.checkpoint_cancelled.load(Ordering::Acquire) {
      return Err(cancelled_checkpoint_error());
    }
    Ok(())
  }

  /// Reach `phase`: note the progress, then run any test hook armed for it.
  fn reach_checkpoint_phase(&self, phase: CheckpointPhase) -> Result<()> {
    self.checkpoint_progressed()?;
    checkpoint_phase(&self.path, phase)?;
    // A hook may have parked this thread until writers cancelled the cut.
    self.checkpoint_progressed()
  }

  /// Start `step`, a part of a checkpoint that notes no progress until it
  /// ends; it runs until the returned guard is dropped. Writers waiting for a
  /// background install never cancel a checkpoint inside one (see
  /// `wait_for_cut_release`), however long it takes. A step must not contain
  /// a phase: a thread parked at a phase hook looks stalled, as it should.
  fn checkpoint_step(&self, step: &'static str) -> CheckpointStep<'_> {
    self.checkpoint_steps_running.fetch_add(1, Ordering::AcqRel);
    #[cfg(test)]
    delay_checkpoint_test_step(&self.path, step);
    let _ = step;
    CheckpointStep { db: self }
  }

  /// Trigger a background checkpoint (non-blocking)
  ///
  /// This switches writes to secondary WAL region immediately and starts
  /// the checkpoint process. Writes can continue while checkpoint is running.
  ///
  /// It does not wait for open transactions to finish, so it starts however
  /// many are open (it only queues behind a blocking checkpoint or compaction
  /// that holds the gate). Each open transaction's records so far are copied
  /// into the secondary region at the cut and its later records go there too,
  /// so it may commit during or after the checkpoint. New transactions pause
  /// only while the cut and the install are written. The caller must not have
  /// its own open transaction (`TransactionInProgress`): a blocking checkpoint
  /// holding the gate could be waiting for it.
  ///
  /// It runs on the calling thread and returns once its snapshot is installed
  /// (or it failed). Writers that fill the secondary region before then wait
  /// for the install instead of failing; if any did, it takes another pass
  /// (up to `MAX_EXTRA_BACKGROUND_PASSES`), since the installed WAL still
  /// holds everything they wrote meanwhile.
  ///
  /// Fails with `CheckpointDeclined`, changing nothing, if the open
  /// transactions' records cannot be copied; it is not retried (this returns
  /// `CheckpointDeclined` at once) until one of them finishes. It also
  /// declines while a blocking checkpoint, optimize, vacuum or WAL resize
  /// waits for the checkpoint gate, which checkpoints anyway: starting ahead
  /// of it would make it wait again, and a loop of background checkpoints
  /// would starve it.
  ///
  /// Steps:
  /// 1. Switch writes to secondary WAL region, copying there the records of
  ///    transactions still open
  /// 2. Set checkpointInProgress flag (for crash recovery) and copy the
  ///    committed delta
  /// 3. Build new snapshot from the current snapshot + that copy
  /// 4. Write new snapshot to disk
  /// 5. Install a header for the new snapshot that drops every primary
  ///    record: the WAL is empty, or names only the post-cut records still in
  ///    the secondary region (this also clears checkpointInProgress)
  /// 6. If post-cut records exist, rewrite them at the start of the primary
  ///    region and install a second header naming them there
  ///
  /// If it fails after step 1, the WAL keeps every record: the secondary
  /// region's records are appended to the primary region if they fit;
  /// otherwise the cut stays in place (replay reads both regions) and the next
  /// checkpoint finishes it.
  pub fn background_checkpoint(&self) -> Result<()> {
    match self.run_background_checkpoint()? {
      BackgroundCheckpointOutcome::StillDeclined => Err(KiteError::CheckpointDeclined(
        "the last attempt could not copy the WAL records of open transactions into the \
         secondary WAL region, and all of them are still open; it starts once one finishes"
          .to_string(),
      )),
      BackgroundCheckpointOutcome::ExclusiveWaiting => Err(KiteError::CheckpointDeclined(
        "a blocking checkpoint, optimize, vacuum or WAL resize is waiting for the checkpoint \
         gate, and checkpoints once it has it"
          .to_string(),
      )),
      BackgroundCheckpointOutcome::Installed | BackgroundCheckpointOutcome::AlreadyRunning => {
        Ok(())
      }
    }
  }

  /// The auto-checkpoint after a commit: `background_checkpoint`, except that
  /// a cut still declined for the same open transactions is skipped quietly
  /// instead of being reported again on every commit.
  pub(crate) fn auto_background_checkpoint(&self) -> Result<()> {
    self.run_background_checkpoint().map(|_| ())
  }

  /// The auto-checkpoint, run by a thread that holds no lock and has no
  /// transaction open (its transaction just committed, failed, or rolled
  /// back, or its begin was refused): once WAL usage reaches the threshold,
  /// or `wal_refused` (the WAL just refused a record of this thread's),
  /// checkpoint unless a background checkpoint is running. It runs after
  /// failures too: when the WAL is full, no commit succeeds to trigger it.
  /// Returns whether a checkpoint ran without error.
  pub(crate) fn auto_checkpoint_if_needed(&self, wal_refused: bool) -> bool {
    if !self.auto_checkpoint
      || self.read_only
      || self.is_checkpoint_running()
      || !(wal_refused || self.should_checkpoint(self.checkpoint_threshold))
    {
      return false;
    }
    let result = if self.background_checkpoint {
      self.auto_background_checkpoint()
    } else {
      self.checkpoint()
    };
    // Reported, not returned: the caller's own outcome stands.
    if let Err(error) = &result {
      eprintln!("Warning: Auto-checkpoint failed: {error}");
    }
    result.is_ok()
  }

  fn run_background_checkpoint(&self) -> Result<BackgroundCheckpointOutcome> {
    if self.read_only {
      return Err(KiteError::ReadOnly);
    }
    if self.current_tx_handle().is_some() {
      return Err(KiteError::TransactionInProgress);
    }
    // An abandoned transaction would stay in the open set, copied by every
    // cut, until rolled back.
    self.reap_abandoned_transactions();
    if self.cut_still_declined() {
      return Ok(BackgroundCheckpointOutcome::StillDeclined);
    }

    // Claim the checkpoint before taking the gate, so commits that cross the
    // threshold meanwhile skip it instead of queueing behind it. Dropping
    // `run`, however this returns (or unwinds), returns the status to idle.
    let run = match self.claim_background_checkpoint() {
      Ok(run) => run,
      Err(outcome) => return Ok(outcome),
    };

    let mut extra_passes = 0;
    loop {
      if let Err(error) = self.background_checkpoint_pass(run.run) {
        self.release_cut(run.run);
        // An extra pass that declines leaves the WAL as the last install did.
        return if extra_passes > 0 && is_declined_cut(&error) {
          Ok(BackgroundCheckpointOutcome::Installed)
        } else {
          Err(error)
        };
      }
      // A waiting exclusive operation checkpoints anyway, and waits for
      // every pass this run takes.
      if !self.take_writers_waited()
        || extra_passes == MAX_EXTRA_BACKGROUND_PASSES
        || self.exclusive_operation_waiting()
      {
        return Ok(BackgroundCheckpointOutcome::Installed);
      }
      extra_passes += 1;
    }
  }

  /// One cut, snapshot, and install (steps 1-6 of `background_checkpoint`).
  fn background_checkpoint_pass(&self, run: u64) -> Result<()> {
    self.set_background_checkpoint_status(run, CheckpointStatus::Running);

    // Steps 1-2: establish a clean cut. The gate excludes blocking
    // checkpoints and compaction, and holds off a BEGIN record between the
    // switch and the set of open transactions being read.
    let cut_delta = {
      let _checkpoint_gate = self.checkpoint_gate.write();
      match self.cut_background_checkpoint(run)? {
        Cut::Taken(cut_delta) => *cut_delta,
        // No transaction is open, and the gate keeps new ones out.
        Cut::FinishBlocking => return self.checkpoint_holding_gate(),
      }
    };

    // Steps 3-6
    self.reach_checkpoint_phase(CheckpointPhase::CutReleased)?;
    let (snapshot, loaded) = self.build_and_write_snapshot(cut_delta)?;
    self.complete_background_checkpoint(run, snapshot, loaded)
  }

  /// Whether writers waited for the last install; clears the flag.
  fn take_writers_waited(&self) -> bool {
    std::mem::take(&mut self.checkpoint_state.lock().writers_waited)
  }

  /// Whether the last cut declined because the open transactions' records
  /// could not be copied, and every transaction open then still is: the
  /// copies a new cut needs can only have grown, so it would decline again
  /// after flushing, syncing, and scanning the WAL under the gate and the
  /// commit lock. (A cut declined with no write transaction open is retried:
  /// see `resume_abandoned_cut`.)
  fn cut_still_declined(&self) -> bool {
    let Some(declined) = self.checkpoint_state.lock().declined_carry.clone() else {
      return false;
    };
    let open = self.open_write_txids.lock();
    !declined.is_empty() && declined.iter().all(|txid| open.contains(txid))
  }

  /// Establish this run's cut: switch new WAL writes to the secondary region
  /// and durably mark the checkpoint in progress, or take over a cut an
  /// earlier run left in place. Returns the committed delta as of the cut,
  /// or that a cut left in place must be finished by a blocking checkpoint.
  ///
  /// Callers hold the checkpoint gate. Every transaction that commits after
  /// the cut has all its records in the secondary region: those still open
  /// are copied there. The snapshot must be built from exactly the pre-cut
  /// commits: completion replays every post-cut commit over it, and edge and
  /// label changes are not idempotent (a delete followed by a re-add would
  /// cancel out if applied twice).
  ///
  /// Declines (`CheckpointDeclined`) before changing anything if the open
  /// transactions' records cannot be copied; see `cut_still_declined`. Once
  /// the marker is durable this run owns the cut, so an error after that
  /// leaves it to `release_cut`.
  fn cut_background_checkpoint(&self, run: u64) -> Result<Cut> {
    #[cfg(test)]
    count_checkpoint_test_cut(&self.path);
    let _commit_guard = self.lock_commits();
    {
      let mut pager = self.pager.lock();
      let mut wal_buffer = self.wal_buffer.lock();
      let mut header = self.header.write();

      // This run holds the status, so no run owns a marker found here: an
      // earlier run stopped before its install and could not merge its cut
      // back into the primary region, or the database was reopened that way.
      if header.checkpoint_in_progress != 0 {
        return self.resume_abandoned_cut(run, &mut pager, &mut wal_buffer);
      }

      // An earlier completion installed its snapshot but failed to compact
      // the post-cut records it retained in the secondary region. Finish that
      // first, so this cut starts from a primary region holding every pre-cut
      // record and an empty secondary region.
      if wal_buffer.is_primary_retired() {
        self.compact_retained_wal(&mut pager, &mut wal_buffer, &mut header)?;
      }

      // Make all pre-cut WAL bytes durable before the checkpoint marker.
      wal_buffer.flush(&mut pager)?;
      pager.sync()?;

      // The commit lock holds off COMMIT records and the gate holds off BEGIN
      // records, so the open set is exact: a transaction that finishes after
      // it is read has written its COMMIT or ROLLBACK already, or never will.
      // Collecting the copies first declines a cut they would not fit, before
      // anything changes.
      let open = self.open_write_txids.lock().clone();
      let carried = match wal_buffer.open_transaction_records(&open, &mut pager) {
        Ok(carried) => carried,
        Err(error @ (KiteError::WalBufferFull | KiteError::InvalidWal(_))) => {
          let reason = match error {
            KiteError::WalBufferFull => format!(
              "the WAL records of the {} open write transactions do not fit in the secondary \
               WAL region",
              open.len()
            ),
            other => other.to_string(),
          };
          self.checkpoint_state.lock().declined_carry = Some(open);
          return Err(KiteError::CheckpointDeclined(format!(
            "{reason}; it starts once one of them finishes"
          )));
        }
        Err(error) => return Err(error),
      };

      let prior_header = header.clone();
      let prior_wal = wal_buffer.region_state();
      wal_buffer.switch_to_secondary();
      wal_buffer.store_in_header(&mut header);
      header.checkpoint_in_progress = 1;
      if let Err(error) = self.persist_header(&mut pager, &mut header, true) {
        // The marker may not be durable, so keep appending where the durable
        // header expects the next record.
        restore_header(&mut header, prior_header);
        wal_buffer.restore_region_state(prior_wal);
        return Err(error);
      }
      self.own_cut(run);

      // The durable marker names an empty secondary region, so the copies
      // cannot clobber records a crash fallback needs; the header of the next
      // commit names them.
      wal_buffer.carry_into_secondary(&carried, &mut pager)?;
    }

    // Commits merge into the delta under the commit lock, so this is exactly
    // the pre-cut state. Writers may already wait for this cut.
    let _step = self.checkpoint_step("copy delta");
    Ok(Cut::Taken(Box::new(self.delta.read().clone())))
  }

  /// Make `run` the owner of the cut it just made durable.
  fn own_cut(&self, run: u64) {
    let mut state = self.checkpoint_state.lock();
    state.cut += 1;
    state.cut_owner = Some(run);
    state.writers_waited = false;
    state.declined_carry = None;
    self.checkpoint_cancelled.store(false, Ordering::Release);
  }

  /// Take over a cut an earlier run left in the WAL. It is as that run made
  /// it: the primary region holds exactly the pre-cut records, and every
  /// transaction committed or open since lies wholly in the secondary region
  /// (copied there at the cut, or begun after it). So the pre-cut delta is
  /// the primary region replayed over the installed snapshot, and the rest of
  /// the checkpoint proceeds as after a fresh cut.
  ///
  /// Not if some transaction in the secondary region began before the cut
  /// (possible only if that run failed to copy the open transactions and then
  /// to merge the cut back): the install would drop its first records. Every
  /// committed transaction is in the delta, though, so with no transaction
  /// open (the caller holds the gate, so none can begin) a blocking checkpoint
  /// finishes the cut (`Cut::FinishBlocking`). Otherwise this declines until
  /// one of the open transactions finishes; it never waits for them, which
  /// would deadlock a binding that holds a global lock (Python's GIL) while
  /// it calls in.
  fn resume_abandoned_cut(
    &self,
    run: u64,
    pager: &mut FilePager,
    wal_buffer: &mut WalBuffer,
  ) -> Result<Cut> {
    wal_buffer.flush(pager)?;
    pager.sync()?;
    let open = self.open_write_txids.lock().clone();
    if !wal_buffer.secondary_holds_whole_transactions(&open, pager)? {
      if self.active_transactions.load(Ordering::Acquire) == 0 {
        return Ok(Cut::FinishBlocking);
      }
      self.checkpoint_state.lock().declined_carry = Some(open);
      return Err(KiteError::CheckpointDeclined(
        "a transaction in the secondary WAL region of an unfinished cut began before it; a \
         blocking checkpoint finishes the cut, and so does this one once no transaction is open"
          .to_string(),
      ));
    }
    let pre_cut_records = wal_buffer.scan_region(0, pager)?;
    let cut_delta = self.replay_into_new_delta(&pre_cut_records)?;
    self.own_cut(run);
    Ok(Cut::Taken(Box::new(cut_delta)))
  }

  /// Build, write, and load the snapshot (called during background
  /// checkpoint)
  fn build_and_write_snapshot(
    &self,
    cut_delta: DeltaState,
  ) -> Result<(WrittenSnapshot, LoadedSnapshot)> {
    let graph = self.collect_graph_data_from(&cut_delta)?;
    {
      let _step = self.checkpoint_step("drop delta copy");
      drop(cut_delta);
    }

    // Snapshot fields do not change while this checkpoint runs; commits only
    // move the WAL positions.
    let header = self.header.read().clone();
    let generation = header.active_snapshot_gen + 1;
    let (snapshot_buffer, vector_stores) = self.build_snapshot_buffer(generation, graph)?;
    let snapshot = self.write_new_snapshot(&header, generation, &snapshot_buffer)?;
    let loaded = self.load_unnamed_snapshot(snapshot, vector_stores)?;
    Ok((snapshot, loaded))
  }

  /// Install this run's snapshot (steps 5-6 of `background_checkpoint`).
  fn complete_background_checkpoint(
    &self,
    run: u64,
    snapshot: WrittenSnapshot,
    mut loaded: LoadedSnapshot,
  ) -> Result<()> {
    self.set_background_checkpoint_status(run, CheckpointStatus::Completing);
    let free_snapshot = || {
      self
        .pager
        .lock()
        .free_pages(snapshot.start_page as u32, snapshot.page_count as u32);
    };

    // The delta that replaces the cut's holds only the transactions
    // committed after the cut, whose records stay in the WAL. Replay them
    // over the new snapshot before installing it, so a failure leaves the
    // database as it was. Most are replayed here, without the commit lock,
    // so commits wait below only for the replay of those that land
    // meanwhile. The records read here stay as they are: the secondary
    // region only grows during a cut, and only cancelling the cut moves its
    // records, which the check under the commit lock below catches.
    let mut replay = PostCutReplay::default();
    let early = self
      .scan_post_cut_records(0)
      .and_then(|(records, read, end)| {
        replay.read = read;
        self.replay_post_cut_records(&mut replay, records, &mut loaded)?;
        Ok(end)
      })
      .inspect_err(|_| free_snapshot())?;

    // The gate excludes blocking checkpoints and compaction until the delta
    // is replaced below; the commit lock keeps the retained records and that
    // delta in step. Open transactions keep their records in the secondary
    // region, which is retained and compacted, and append after them. From
    // here writers cannot cancel the cut (that takes the commit lock): one
    // that tries waits until the install releases it.
    let _checkpoint_gate = self.checkpoint_gate.write();
    let _commit_guard = self.lock_commits();

    // Writers may have cancelled the cut while this run made no progress
    // (see `wait_for_cut_release`): the WAL is back in the primary region,
    // and no header names this snapshot.
    if self.checkpoint_state.lock().cut_owner != Some(run) {
      free_snapshot();
      return Err(cancelled_checkpoint_error());
    }
    // No commit lands from here (the commit lock), though open transactions
    // may append records, which the install retains.
    self
      .scan_post_cut_records(early)
      .and_then(|(records, read, _)| {
        replay.read.extend_from_slice(&read);
        self.replay_post_cut_records(&mut replay, records, &mut loaded)
      })
      .inspect_err(|_| free_snapshot())?;
    let post_cut_delta = replay.delta;

    let compaction_result;
    {
      let mut pager = self.pager.lock();
      let mut wal_buffer = self.wal_buffer.lock();
      let mut header = self.header.write();

      // The new snapshot covers every primary record, so the installed header
      // stops counting them: with no post-cut records the WAL is simply
      // empty; otherwise it names only the post-cut records, still in place
      // in the secondary region. Neither state writes WAL bytes, so the cut
      // header's WAL stays intact as the crash fallback until this header is
      // durable in both slots. If the install fails, the cut state is
      // restored (the cut header may still be the newest durable slot, and it
      // names the primary region), so the run releases the cut instead of
      // letting the next commit overwrite records that header still needs.
      let retain_post_cut = wal_buffer.has_secondary_records();
      self.install_snapshot(
        &mut pager,
        &mut wal_buffer,
        &mut header,
        snapshot,
        |wal_buffer| {
          if retain_post_cut {
            wal_buffer.retire_primary_region();
          } else {
            wal_buffer.reset();
          }
        },
      )?;
      // The cut is gone. Writers waiting for it retry once the WAL lock is
      // free, after the compaction below.
      self.checkpoint_state.lock().cut_owner = None;

      // Both slots now name the retained secondary records, so the primary
      // region is free to rewrite. A failure leaves the retained state, which
      // is consistent on disk and finished by the next background checkpoint
      // or open; the new snapshot is installed either way. The records read
      // and checked by the replay above are reused (the secondary region only
      // grew since), so only those open transactions appended since are read
      // here.
      compaction_result = if retain_post_cut {
        self.compact_retained_wal_reusing(&mut pager, &mut wal_buffer, &mut header, replay.read)
      } else {
        Ok(())
      };
    }
    self.notify_cut_waiters();

    self.install_loaded_snapshot(loaded, post_cut_delta);
    self.truncate_orphaned_tail()?;

    compaction_result
  }

  /// Move post-cut records retained in the secondary region to the start of
  /// the primary region, then install a header naming them there.
  ///
  /// Callers hold the commit lock and the WAL buffer, so no record is written
  /// meanwhile; open transactions append after the rewritten records. Every
  /// valid header slot names the retained secondary records rather than the
  /// primary region, so rewriting primary bytes cannot damage a crash
  /// fallback.
  ///
  /// If the rewrite fails, the buffer stays in the retained state the durable
  /// header names. If only the header install fails, the buffer keeps the
  /// compacted state: its bytes are already synced, the next header write
  /// records it, and until then the retained header stays a valid fallback
  /// because the secondary region is not written again before a new
  /// checkpoint marker is durable.
  pub(crate) fn compact_retained_wal(
    &self,
    pager: &mut FilePager,
    wal_buffer: &mut WalBuffer,
    header: &mut DbHeaderV1,
  ) -> Result<()> {
    self.compact_retained_wal_reusing(pager, wal_buffer, header, Vec::new())
  }

  /// `compact_retained_wal`, reusing `read`, the retained records' first
  /// bytes as they lie in the secondary region now (see
  /// `WalBuffer::compact_secondary_into_primary_reusing`).
  fn compact_retained_wal_reusing(
    &self,
    pager: &mut FilePager,
    wal_buffer: &mut WalBuffer,
    header: &mut DbHeaderV1,
    read: Vec<u8>,
  ) -> Result<()> {
    self.reach_checkpoint_phase(CheckpointPhase::PostCutWalRetained)?;
    {
      let _step = self.checkpoint_step("compact retained WAL");
      wal_buffer.compact_secondary_into_primary_reusing(read, pager)?;
    }
    wal_buffer.store_in_header(header);
    self.persist_checkpoint_header(pager, header)
  }

  /// Flush the WAL and read the post-cut records (in the secondary region)
  /// from offset `from` on; also their bytes as they lie there, and where
  /// they end, to read on from. The pager and WAL locks, which every commit
  /// takes, are held for the read only, not while the records are parsed.
  fn scan_post_cut_records(&self, from: u64) -> Result<(Vec<ParsedWalRecord>, Vec<u8>, u64)> {
    let read = {
      let mut pager = self.pager.lock();
      let mut wal_buffer = self.wal_buffer.lock();
      wal_buffer.flush(&mut pager)?;
      wal_buffer.read_region_from(1, from, &mut pager)?
    };
    Ok(read.parse())
  }

  /// Replay the transactions that commit in `records` (the post-cut records
  /// after those `replay` has seen) into `replay`'s delta over `loaded`, in
  /// commit order, and their vector operations into its stores. Schema they
  /// define joins this database's (it already has it, since they committed
  /// here), and the ID allocators never move backwards.
  fn replay_post_cut_records(
    &self,
    replay: &mut PostCutReplay,
    records: Vec<ParsedWalRecord>,
    loaded: &mut LoadedSnapshot,
  ) -> Result<()> {
    self.reach_checkpoint_phase(CheckpointPhase::PostCutReplay)?;
    let _step = self.checkpoint_step("replay post-cut records");
    let mut wal_records = std::mem::take(&mut replay.unfinished);
    wal_records.extend(records);
    let committed = committed_transactions(&wal_records);
    let delta = &mut replay.delta;
    let mut next_node_id = self.next_node_id.load(Ordering::Acquire);
    let mut next_label_id = self.next_label_id.load(Ordering::Acquire);
    let mut next_etype_id = self.next_etype_id.load(Ordering::Acquire);
    let mut next_propkey_id = self.next_propkey_id.load(Ordering::Acquire);
    // The schema they define, replayed into local maps and joined to this
    // database's after: holding its schema locks for the whole replay would
    // stall every commit (a commit publishes its schema under them).
    let mut label_names = HashMap::new();
    let mut label_ids = HashMap::new();
    let mut etype_names = HashMap::new();
    let mut etype_ids = HashMap::new();
    let mut propkey_names = HashMap::new();
    let mut propkey_ids = HashMap::new();
    for (_txid, records) in committed {
      for record in records {
        replay_wal_record(
          record,
          loaded.snapshot.as_ref(),
          delta,
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
      }
    }
    if !(label_ids.is_empty() && etype_ids.is_empty() && propkey_ids.is_empty()) {
      self.label_names.write().extend(label_names);
      self.label_ids.write().extend(label_ids);
      self.etype_names.write().extend(etype_names);
      self.etype_ids.write().extend(etype_ids);
      self.propkey_names.write().extend(propkey_names);
      self.propkey_ids.write().extend(propkey_ids);
    }

    // Open transactions may allocate IDs meanwhile, so only ever raise them.
    self.next_node_id.fetch_max(next_node_id, Ordering::AcqRel);
    self
      .next_label_id
      .fetch_max(next_label_id, Ordering::AcqRel);
    self
      .next_etype_id
      .fetch_max(next_etype_id, Ordering::AcqRel);
    self
      .next_propkey_id
      .fetch_max(next_propkey_id, Ordering::AcqRel);

    loaded.apply_pending_vectors(&delta.pending_vectors)?;
    delta.pending_vectors.clear();
    replay.unfinished = unfinished_transactions(wal_records);
    Ok(())
  }

  /// Replay the transactions committed in `records` into a new delta over the
  /// installed snapshot, leaving this database untouched: its allocators and
  /// schema already include them.
  fn replay_into_new_delta(
    &self,
    records: &[crate::core::wal::record::ParsedWalRecord],
  ) -> Result<DeltaState> {
    let mut delta = DeltaState::new();
    let mut next_node_id = self.next_node_id.load(Ordering::Acquire);
    let mut next_label_id = self.next_label_id.load(Ordering::Acquire);
    let mut next_etype_id = self.next_etype_id.load(Ordering::Acquire);
    let mut next_propkey_id = self.next_propkey_id.load(Ordering::Acquire);
    let mut label_names = HashMap::new();
    let mut label_ids = HashMap::new();
    let mut etype_names = HashMap::new();
    let mut etype_ids = HashMap::new();
    let mut propkey_names = HashMap::new();
    let mut propkey_ids = HashMap::new();
    let snapshot = self.snapshot.read();
    for (_txid, records) in committed_transactions(records) {
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
        )?;
      }
    }
    Ok(delta)
  }

  /// After a failed pass of background checkpoint `run`: if the run still
  /// owns its cut, leave it without losing a record (see `leave_cut`). If
  /// the cut cannot be left (its records do not fit in the primary region, or
  /// an I/O error), it stays in place as an abandoned cut: durable, replayed
  /// from both regions on open, and finished by the next checkpoint.
  fn release_cut(&self, run: u64) {
    {
      let _commit_guard = self.lock_commits();
      let mut pager = self.pager.lock();
      let mut wal_buffer = self.wal_buffer.lock();
      let mut header = self.header.write();
      // The cut owner changes only under the commit lock, except when a
      // run's thread unwinds, which cannot happen during this call.
      if self.checkpoint_state.lock().cut_owner != Some(run) {
        return;
      }
      match self.leave_cut(&mut pager, &mut wal_buffer, &mut header) {
        Ok(true) => {}
        Ok(false) => eprintln!(
          "Warning: kept the failed background checkpoint's cut: its WAL records do not fit in \
           the primary region; the next checkpoint finishes it"
        ),
        Err(error) => eprintln!(
          "Warning: kept the failed background checkpoint's cut ({error}); the next checkpoint \
           finishes it"
        ),
      }
      self.checkpoint_state.lock().cut_owner = None;
    }
    self.notify_cut_waiters();
  }

  /// Move the WAL out of a background checkpoint's cut: append the secondary
  /// region's records to the primary region and install a header without the
  /// checkpoint marker. Callers hold the commit lock and own the cut.
  ///
  /// Returns `false`, changing nothing, if the records do not fit (see
  /// `WalBuffer::merge_cut_into_primary`): rewriting part of them would leave
  /// a header naming a partial WAL. The records written are synced first, so
  /// until the header lands the cut header stays a valid fallback; if only
  /// the header write fails, the next one (every commit writes one) records
  /// the merge.
  fn leave_cut(
    &self,
    pager: &mut FilePager,
    wal_buffer: &mut WalBuffer,
    header: &mut DbHeaderV1,
  ) -> Result<bool> {
    if !wal_buffer.merge_cut_into_primary(pager)? {
      return Ok(false);
    }
    wal_buffer.store_in_header(header);
    header.checkpoint_in_progress = 0;
    if let Err(error) = self.persist_header(pager, header, true) {
      eprintln!("Warning: failed to write the header after leaving a checkpoint cut: {error}");
    }
    Ok(true)
  }

  /// The cut whose install a writer must wait for, given that `wal_buffer`
  /// just refused its record: a background checkpoint run owns a cut, so
  /// records go to the secondary region until its install frees the primary
  /// region. `None` means the WAL is full with nothing to wait for.
  ///
  /// Callers hold the WAL lock, so the answer matches the state that refused
  /// the record: cuts are taken and released under the WAL lock too (except
  /// by a run unwinding from a panic, which leaves the WAL as it is).
  pub(crate) fn cut_blocking_wal_writes(&self, wal_buffer: &WalBuffer) -> Option<u64> {
    if wal_buffer.active_region() != 1 {
      return None;
    }
    let state = self.checkpoint_state.lock();
    state.cut_owner.map(|_| state.cut)
  }

  /// Wait until `cut` is installed or released, so a writer that found the
  /// secondary region full can retry. The caller must hold no lock the
  /// checkpoint needs to finish: not the checkpoint gate, the commit lock,
  /// the pager, or the WAL.
  ///
  /// Deadlock-free: a run between its cut and its install waits for no
  /// transaction or writer (only for the gate, held briefly by `begin` or by
  /// blocking checkpoints, which release it while a run is in progress; see
  /// `exclusive_checkpoint_gate`). And a run never waits here itself: it
  /// writes no records, and its caller has no open transaction.
  ///
  /// Bounded: if its run looks stalled for `CHECKPOINT_STALL_TIMEOUT` (no
  /// progress noted and no `checkpoint_step` running: only a thread stopped
  /// outright, e.g. parked by a test or a debugger, does that), the writer
  /// cancels the cut by moving its records back into the primary region; the
  /// run stops at its next progress point. If the records do not fit, the
  /// writer gets `WalBufferFull`.
  pub(crate) fn wait_for_cut_release(&self, cut: u64) -> Result<()> {
    let mut progress = self.checkpoint_progress.load(Ordering::Relaxed);
    let mut progressed_at = Instant::now();
    let mut wait = self.cut_wait.lock();
    {
      let mut state = self.checkpoint_state.lock();
      if !state.holds_cut(cut) {
        return Ok(());
      }
      state.writers_waited = true;
    }
    loop {
      self.cut_cv.wait_for(&mut wait, CUT_WAIT_POLL);
      if !self.checkpoint_state.lock().holds_cut(cut) {
        return Ok(());
      }
      let current = self.checkpoint_progress.load(Ordering::Relaxed);
      let in_step = self.checkpoint_steps_running.load(Ordering::Acquire) > 0;
      if current != progress || in_step {
        progress = current;
        progressed_at = Instant::now();
      } else if progressed_at.elapsed() >= checkpoint_stall_timeout(&self.path) {
        drop(wait);
        return self.cancel_stalled_cut(cut);
      }
    }
  }

  /// Cancel `cut`, whose run stalled while writers wait for its install, by
  /// leaving it (see `leave_cut`).
  fn cancel_stalled_cut(&self, cut: u64) -> Result<()> {
    {
      let _commit_guard = self.lock_commits();
      let mut pager = self.pager.lock();
      let mut wal_buffer = self.wal_buffer.lock();
      let mut header = self.header.write();
      if !self.checkpoint_state.lock().holds_cut(cut) {
        return Ok(());
      }
      if !self.leave_cut(&mut pager, &mut wal_buffer, &mut header)? {
        return Err(KiteError::WalBufferFull);
      }
      // Only a run unwinding from a panic gives up its cut without the
      // commit lock; then there is no run left to stop.
      let mut state = self.checkpoint_state.lock();
      if state.holds_cut(cut) {
        state.cut_owner = None;
        // The run still holds the status; make it stop at its next progress
        // point instead of finishing a snapshot no header will name.
        self.checkpoint_cancelled.store(true, Ordering::Release);
      }
    }
    eprintln!(
      "Warning: cancelled a background checkpoint that looked stalled for {:?} while writers \
       waited for it to free WAL space",
      checkpoint_stall_timeout(&self.path)
    );
    self.notify_cut_waiters();
    Ok(())
  }

  /// Return an append-only page range for a new snapshot. The file may retain
  /// orphaned pages after a crash; using the physical end keeps those pages
  /// from being overwritten while the previous header can still reach them.
  pub(crate) fn snapshot_append_start_page(&self, header: &DbHeaderV1) -> Result<u64> {
    let pager = self.pager.lock();
    let page_size = header.page_size as u64;
    let file_pages = pager.file_size().div_ceil(page_size);
    let wal_end_page = header.wal_start_page + header.wal_page_count;
    let snapshot_end_page = header
      .snapshot_start_page
      .saturating_add(header.snapshot_page_count);

    Ok(
      file_pages
        .max(header.db_size_pages)
        .max(wal_end_page)
        .max(snapshot_end_page),
    )
  }

  /// Reuse only pages retired after both header slots durably pointed at a
  /// newer snapshot. Therefore no valid fallback header can name this range;
  /// a crash while rewriting it still opens either header on the current
  /// installed snapshot. If no retired range fits, append at physical EOF.
  fn checkpoint_snapshot_start_page(
    &self,
    header: &DbHeaderV1,
    snapshot_page_count: u64,
  ) -> Result<u64> {
    let mut pager = self.pager.lock();
    let wal_end_page = header.wal_start_page + header.wal_page_count;
    let snapshot_end_page = header
      .snapshot_start_page
      .saturating_add(header.snapshot_page_count);

    // The header, the WAL, and the installed snapshot are never free. Should
    // a bookkeeping mistake list any of them, withdraw them instead of
    // writing this snapshot over pages the installed header names.
    let live_pages_listed = pager.withdraw_free_pages(0, wal_end_page as u32)
      + pager.withdraw_free_pages(header.snapshot_start_page as u32, snapshot_end_page as u32);
    if live_pages_listed > 0 {
      eprintln!(
        "Warning: {live_pages_listed} pages of the header, WAL, or installed snapshot were \
         listed as free; withdrew them from reuse"
      );
    }

    if let Some(start_page) = pager.find_free_range(snapshot_page_count as u32) {
      pager.consume_free_range(start_page, snapshot_page_count as u32);
      return Ok(start_page as u64);
    }

    let page_size = header.page_size as u64;
    let file_pages = pager.file_size().div_ceil(page_size);
    Ok(
      file_pages
        .max(header.db_size_pages)
        .max(wal_end_page)
        .max(snapshot_end_page),
    )
  }

  /// Drop physical tail pages only after reload replaced the old snapshot
  /// mmap and both durable header slots point at the installed snapshot.
  fn truncate_orphaned_tail(&self) -> Result<()> {
    let header = self.header.read().clone();
    let keep_pages = header
      .snapshot_start_page
      .saturating_add(header.snapshot_page_count)
      .max(header.wal_start_page + header.wal_page_count);
    let mut pager = self.pager.lock();
    let file_pages = pager.file_size().div_ceil(header.page_size as u64);
    if keep_pages < file_pages
      && u32::try_from(keep_pages)
        .ok()
        .zip(u32::try_from(file_pages).ok())
        .is_some_and(|(start, end)| pager.is_range_free(start, end))
    {
      pager.truncate_pages(keep_pages as u32)?;
    }
    Ok(())
  }

  /// Install `header` durably in both slots.
  ///
  /// If the first slot never becomes durable, the next header write targets
  /// that slot again: it may hold a torn or unsynced copy of `header`, while
  /// the other slot holds the last durable header, which must stay the crash
  /// fallback until a newer header is durable.
  fn persist_checkpoint_header(
    &self,
    pager: &mut FilePager,
    header: &mut DbHeaderV1,
  ) -> Result<()> {
    let durable_slot = self.header_slot.load(Ordering::Acquire);
    let first_slot = self
      .persist_header(pager, header, false)
      .and_then(|()| self.reach_checkpoint_phase(CheckpointPhase::HeaderWritten))
      .and_then(|()| {
        let _step = self.checkpoint_step("sync header");
        pager.sync()
      });
    if let Err(error) = first_slot {
      self.header_slot.store(durable_slot, Ordering::Release);
      return Err(error);
    }
    self.reach_checkpoint_phase(CheckpointPhase::HeaderDurable)?;

    // Rotate the installed header into the other slot before retiring the old
    // snapshot. After this fsync both valid slots name `header`'s snapshot, so
    // no fallback can reach the region placed on the free list.
    let _step = self.checkpoint_step("sync header");
    self.persist_header(pager, header, true)
  }

  /// Install `snapshot`, with the WAL state `retire_wal` leaves, in both
  /// header slots.
  ///
  /// Until both slots are durable a crash may still select the previous
  /// header, so a failure returns the in-memory header and WAL to it: later
  /// commits append after the WAL records that header names instead of
  /// overwriting them. The failed install may have left `snapshot` named in a
  /// slot, so its pages stay out of reuse until a later install is durable in
  /// both slots; the previous snapshot's pages are not freed.
  pub(crate) fn install_snapshot(
    &self,
    pager: &mut FilePager,
    wal_buffer: &mut WalBuffer,
    header: &mut DbHeaderV1,
    snapshot: WrittenSnapshot,
    retire_wal: impl FnOnce(&mut WalBuffer),
  ) -> Result<()> {
    // `WalBuffer::reset` drops buffered bytes, which a failed install must
    // keep, so write them out first.
    wal_buffer.flush(pager)?;
    self.publish_replication_frames();
    let prior_header = header.clone();
    let prior_wal = wal_buffer.region_state();

    header.prev_snapshot_gen = header.active_snapshot_gen;
    header.active_snapshot_gen = snapshot.generation;
    header.snapshot_start_page = snapshot.start_page;
    header.snapshot_page_count = snapshot.page_count;
    header.db_size_pages = snapshot.start_page + snapshot.page_count;
    header.max_node_id = self.next_node_id.load(Ordering::SeqCst).saturating_sub(1);
    header.next_tx_id = self.next_tx_id.load(Ordering::SeqCst);

    // Record every region field: a stale primary head or active region would
    // make the next open append after, or re-merge, records this snapshot
    // already covers.
    retire_wal(wal_buffer);
    wal_buffer.store_in_header(header);
    header.checkpoint_in_progress = 0;

    if let Err(error) = self.persist_checkpoint_header(pager, header) {
      restore_header(header, prior_header);
      wal_buffer.restore_region_state(prior_wal);
      pager.defer_free_pages(snapshot.start_page as u32, snapshot.page_count as u32);
      return Err(error);
    }

    // Both slots name `snapshot`, so no fallback can reach the previous
    // snapshot or one left by an earlier failed install.
    if prior_header.snapshot_page_count > 0
      && prior_header.snapshot_start_page != snapshot.start_page
    {
      pager.free_pages(
        prior_header.snapshot_start_page as u32,
        prior_header.snapshot_page_count as u32,
      );
    }
    pager.release_deferred_free_pages();
    Ok(())
  }

  /// Publish the frames a primary's replication sidecar still buffers in
  /// memory (Normal and Off sync modes do, for up to 100 ms), durably and
  /// with a manifest naming them, before an install drops the WAL records of
  /// their commits: from then on the sidecar is the only copy replicas can
  /// get them from. Every commit's frame is appended by now: a blocking
  /// install runs with no transaction open, and a commit appends its frame
  /// before it stops counting as open; a background install holds the commit
  /// lock, under which commits append.
  ///
  /// A failure fences the sidecar for repair, as a failed append does, and
  /// the checkpoint goes on: local commits stay authoritative while
  /// replication is stale, and failing would leave the WAL to fill. A loss
  /// is never silent: the replication status reports the fence, and the
  /// `primary-unflushed` marker (written before the first buffered frame,
  /// removed only once none is) survives, so a reopen after a crash fences
  /// too. The sidecar locks are leaves, so this is safe under the pager, WAL,
  /// and header locks.
  fn publish_replication_frames(&self) {
    let Some(replication) = self.primary_replication.as_ref() else {
      return;
    };
    if let Err(error) = replication.publish_for_checkpoint() {
      eprintln!(
        "Warning: replication sidecar fenced for repair: publishing its buffered frames before \
         a checkpoint failed: {error}"
      );
    }
  }

  /// Serialize a checkpoint snapshot of `graph`. Returns it with a copy of
  /// its vector stores to install (`snapshot_vector_stores`).
  fn build_snapshot_buffer(
    &self,
    generation: u64,
    graph: GraphData,
  ) -> Result<(Vec<u8>, HashMap<PropKeyId, VectorManifest>)> {
    let _step = self.checkpoint_step("serialize snapshot");
    let (nodes, edges, labels, etypes, propkeys, vector_stores) = graph;
    let installed = snapshot_vector_stores(&vector_stores)?;
    let buffer = build_snapshot_to_memory(SnapshotBuildInput {
      generation,
      nodes,
      edges,
      labels,
      etypes,
      propkeys,
      vector_stores: Some(vector_stores),
      compression: self.checkpoint_compression.clone(),
    })?;
    Ok((buffer, installed))
  }

  /// Write `buffer` to pages no valid header names and sync it.
  fn write_new_snapshot(
    &self,
    header: &DbHeaderV1,
    generation: u64,
    buffer: &[u8],
  ) -> Result<WrittenSnapshot> {
    let page_size = header.page_size as usize;
    let page_count = pages_to_store(buffer.len(), page_size) as u64;
    // Never reuse the installed snapshot's pages. Orphaned pages from an
    // interrupted checkpoint are also skipped; vacuum owns reclamation.
    let start_page = self.checkpoint_snapshot_start_page(header, page_count)?;

    let written = self.write_unnamed_snapshot_pages(start_page as u32, buffer, page_size);
    if let Err(error) =
      written.and_then(|()| self.reach_checkpoint_phase(CheckpointPhase::SnapshotDurable))
    {
      // No header names these pages yet, so they are reusable right away.
      self
        .pager
        .lock()
        .free_pages(start_page as u32, page_count as u32);
      return Err(error);
    }

    Ok(WrittenSnapshot {
      generation,
      start_page,
      page_count,
    })
  }

  /// Write `buffer` as the pages from `start_page` on, which no header
  /// names, and sync them. The pager lock is held only to allocate them:
  /// commits take it to append to the WAL, and a slow write or fsync of the
  /// snapshot would stall every one of them. Where the file cannot be written
  /// without the pager (`FilePager::detached_writer`), it is written under
  /// the lock.
  fn write_unnamed_snapshot_pages(
    &self,
    start_page: u32,
    buffer: &[u8],
    page_size: usize,
  ) -> Result<()> {
    let writer = {
      let mut pager = self.pager.lock();
      self.allocate_snapshot_pages(&mut pager, start_page, buffer.len(), page_size)?;
      match pager.detached_writer() {
        Some(writer) => writer,
        None => return self.write_snapshot_pages(&mut pager, start_page, buffer, page_size),
      }
    };
    self.write_snapshot_chunks(start_page, buffer, page_size, |offset, data| {
      writer.write_range(offset, data)
    })?;
    self.reach_checkpoint_phase(CheckpointPhase::SnapshotWritten)?;
    let _step = self.checkpoint_step("sync snapshot");
    writer.sync()
  }

  /// Write snapshot buffer to file pages, and sync them.
  pub(crate) fn write_snapshot_pages(
    &self,
    pager: &mut FilePager,
    start_page: u32,
    buffer: &[u8],
    page_size: usize,
  ) -> Result<()> {
    self.allocate_snapshot_pages(pager, start_page, buffer.len(), page_size)?;
    self.write_snapshot_chunks(start_page, buffer, page_size, |offset, data| {
      pager.write_range(offset, data)
    })?;
    self.reach_checkpoint_phase(CheckpointPhase::SnapshotWritten)?;
    let _step = self.checkpoint_step("sync snapshot");
    pager.sync()
  }

  /// Extend the file to hold `bytes` from page `start_page` on.
  fn allocate_snapshot_pages(
    &self,
    pager: &mut FilePager,
    start_page: u32,
    bytes: usize,
    page_size: usize,
  ) -> Result<()> {
    let required_pages = start_page + pages_to_store(bytes, page_size);
    let current_pages = (pager.file_size() as usize).div_ceil(page_size);
    if required_pages as usize > current_pages {
      let _step = self.checkpoint_step("allocate snapshot pages");
      pager.allocate_pages(required_pages - current_pages as u32)?;
    }
    Ok(())
  }

  /// Write `buffer` with `write` (a file offset and bytes) as the pages from
  /// `start_page` on, the last padded with zeros, a chunk of
  /// `SNAPSHOT_WRITE_CHUNK` bytes at a time.
  fn write_snapshot_chunks(
    &self,
    start_page: u32,
    buffer: &[u8],
    page_size: usize,
    mut write: impl FnMut(u64, &[u8]) -> Result<()>,
  ) -> Result<()> {
    let padded_len = pages_to_store(buffer.len(), page_size) as usize * page_size;
    let chunk_len = (SNAPSHOT_WRITE_CHUNK / page_size).max(1) * page_size;
    let base = start_page as u64 * page_size as u64;
    let mut offset = 0;
    while offset < padded_len {
      let end = (offset + chunk_len).min(padded_len);
      if end <= buffer.len() {
        write(base + offset as u64, &buffer[offset..end])?;
      } else {
        let mut padded = vec![0u8; end - offset];
        padded[..buffer.len() - offset].copy_from_slice(&buffer[offset..]);
        write(base + offset as u64, &padded)?;
      }
      self.reach_checkpoint_phase(CheckpointPhase::SnapshotPageWritten)?;
      offset = end;
    }
    Ok(())
  }

  /// Collect all graph data from snapshot + delta
  pub(crate) fn collect_graph_data(&self) -> Result<GraphData> {
    let delta = self.delta.read();
    self.collect_graph_data_from(&delta)
  }

  /// Collect all graph data from snapshot + `delta`
  fn collect_graph_data_from(&self, delta: &DeltaState) -> Result<GraphData> {
    let _step = self.checkpoint_step("collect graph");
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut labels = HashMap::new();
    let mut etypes = HashMap::new();
    let mut propkeys = HashMap::new();

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
        if phys % 4096 == 0 {
          self.checkpoint_progressed()?;
        }
        let node_id = match snapshot.node_id(phys as u32) {
          Some(id) => id,
          None => continue,
        };

        // Skip deleted nodes
        if delta.is_node_deleted(node_id) {
          continue;
        }

        // Get key
        let key = snapshot.node_key(phys as u32);

        // Get properties from snapshot
        let mut props = HashMap::new();
        if let Some(snapshot_props) = snapshot.node_props(phys as u32) {
          for (key_id, value) in snapshot_props {
            props.insert(key_id, value);
          }
        }

        // Apply delta modifications
        if let Some(node_delta) = delta.node_delta(node_id) {
          if let Some(ref delta_props) = node_delta.props {
            for (&key_id, value) in delta_props {
              match value {
                Some(v) => {
                  props.insert(key_id, v.as_ref().clone());
                }
                None => {
                  props.remove(&key_id);
                }
              }
            }
          }
        }

        // Collect node labels (snapshot + delta)
        let mut node_labels: std::collections::HashSet<LabelId> = std::collections::HashSet::new();

        if let Some(snapshot_labels) = snapshot.node_labels(phys as u32) {
          node_labels.extend(snapshot_labels);
        }

        if let Some(node_delta) = delta.node_delta(node_id) {
          if let Some(ref labels) = node_delta.labels {
            node_labels.extend(labels.iter().copied());
          }
          if let Some(ref deleted) = node_delta.labels_deleted {
            for label_id in deleted {
              node_labels.remove(label_id);
            }
          }
        }

        let mut node_labels: Vec<LabelId> = node_labels.into_iter().collect();
        node_labels.sort_unstable();

        nodes.push(NodeData {
          node_id,
          key,
          labels: node_labels,
          props,
        });

        // Collect edges from this node
        for edge_info in snapshot.out_edges(phys as u32) {
          let dst_node_id = match snapshot.node_id(edge_info.dst) {
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

          // Get edge props from snapshot
          let mut edge_props = HashMap::new();
          if let Some(edge_idx) =
            snapshot.find_edge_index(phys as u32, edge_info.etype, edge_info.dst)
          {
            if let Some(snapshot_edge_props) = snapshot.edge_props(edge_idx) {
              edge_props = snapshot_edge_props;
            }
          }

          // Apply delta edge prop modifications
          let edge_key = (node_id, edge_info.etype, dst_node_id);
          if let Some(delta_edge_props) = delta.edge_props.get(&edge_key) {
            for (&key_id, value) in delta_edge_props {
              match value {
                Some(v) => {
                  edge_props.insert(key_id, v.as_ref().clone());
                }
                None => {
                  edge_props.remove(&key_id);
                }
              }
            }
          }

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
            props.insert(key_id, v.as_ref().clone());
          }
        }
      }

      let mut node_labels: Vec<LabelId> = node_delta
        .labels
        .as_ref()
        .map(|l| l.iter().copied().collect())
        .unwrap_or_default();
      node_labels.sort_unstable();

      nodes.push(NodeData {
        node_id,
        key: node_delta.key.clone(),
        labels: node_labels,
        props,
      });
    }

    // Add edges from delta
    let delta_edges_start = edges.len();
    for (&src, patches) in &delta.out_add {
      // Skip edges from deleted nodes (a recreated node keeps its new edges)
      if delta.is_node_removed(src) {
        continue;
      }

      for patch in patches {
        // Skip edges to deleted nodes
        if delta.is_node_removed(patch.other) {
          continue;
        }

        // Get edge props from delta
        let mut edge_props = HashMap::new();
        let edge_key = (src, patch.etype, patch.other);
        if let Some(delta_edge_props) = delta.edge_props.get(&edge_key) {
          for (&key_id, value) in delta_edge_props {
            if let Some(v) = value {
              edge_props.insert(key_id, v.as_ref().clone());
            }
          }
        }

        edges.push(EdgeData {
          src,
          etype: patch.etype,
          dst: patch.other,
          props: edge_props,
        });
      }
    }

    // Snapshot persistence now stores ANN vectors only in dedicated
    // vector-store sections. Remove duplicate vector payloads from node props.
    self.materialize_all_vector_stores()?;
    let mut vector_stores_for_snapshot: HashMap<PropKeyId, VectorManifest> =
      self.vector_stores.read().clone();
    self.drop_state_of_missing_nodes(
      delta,
      &mut edges,
      delta_edges_start,
      &mut vector_stores_for_snapshot,
    );
    // A delete only marks its vector deleted in its fragment. The snapshot is
    // rewritten anyway, so drop fully deleted fragments and copy the live
    // vectors of sparse ones into new fragments. Vector ids and the node
    // mappings stay, so the post-cut replay (by node and property) applies
    // as before. These are the copies `snapshot_vector_stores` validates, so
    // the compacted stores are the ones serialized and installed.
    for store in vector_stores_for_snapshot.values_mut() {
      if store.total_deleted == 0 {
        continue;
      }
      crate::vector::compaction::clear_deleted_fragments(store);
      // A round compacts a few fragments; each round that compacts removes
      // at least one, so this many rounds always finish.
      let strategy = crate::vector::compaction::CompactionStrategy::default();
      for _ in 0..store.fragments.len() {
        if !crate::vector::compaction::run_compaction_if_needed(store, &strategy) {
          break;
        }
      }
    }
    if !vector_stores_for_snapshot.is_empty() {
      for node in &mut nodes {
        node.props.retain(|prop_key_id, value| {
          !(vector_stores_for_snapshot.contains_key(prop_key_id)
            && matches!(value, PropValue::VectorF32(_)))
        });
      }
    }

    Ok((
      nodes,
      edges,
      labels,
      etypes,
      propkeys,
      vector_stores_for_snapshot,
    ))
  }

  /// Drop what the committed state holds for nodes that exist nowhere (see
  /// `DeltaState::node_exists_over`, also used by WAL replay): the delta
  /// edges `edges[delta_edges_start..]` with such an endpoint, props
  /// included, and the vectors of such nodes in `vector_stores`, copies of
  /// the live stores. The snapshot writer rejects a dangling edge, so one
  /// would fail this checkpoint and every later one while the WAL fills; a
  /// vector would be carried into every snapshot. Older versions left both
  /// behind (their node deletes logged no vector deletes), and so can
  /// non-MVCC write transactions racing a delete of the node, since nothing
  /// checks the node again at commit. The install replaces the delta and the
  /// stores, so they disappear live too. Snapshot edges need no check: both
  /// ends are snapshot nodes the delta did not delete.
  ///
  /// A background checkpoint's store copies may also hold vectors committed
  /// after its cut for nodes created after it. Dropping those is safe: the
  /// install replays every post-cut commit over the new snapshot, and so does
  /// open after a crash.
  fn drop_state_of_missing_nodes(
    &self,
    delta: &DeltaState,
    edges: &mut Vec<EdgeData>,
    delta_edges_start: usize,
    vector_stores: &mut HashMap<PropKeyId, VectorManifest>,
  ) {
    let snapshot = self.snapshot.read();
    let exists = |node_id| delta.node_exists_over(snapshot.as_ref(), node_id);

    let mut delta_edges = edges.split_off(delta_edges_start);
    let collected = delta_edges.len();
    delta_edges.retain(|edge| exists(edge.src) && exists(edge.dst));
    let dropped_edges = collected - delta_edges.len();
    edges.append(&mut delta_edges);

    let mut dropped_vectors = 0usize;
    for store in vector_stores.values_mut() {
      let missing: Vec<NodeId> = store
        .node_to_vector
        .keys()
        .copied()
        .filter(|&node_id| !exists(node_id))
        .collect();
      for node_id in missing {
        dropped_vectors += usize::from(vector_store_delete(store, node_id));
      }
    }

    if dropped_edges > 0 {
      eprintln!(
        "Warning: checkpoint dropped {dropped_edges} edges whose source or destination node \
         does not exist"
      );
    }
    if dropped_vectors > 0 {
      eprintln!("Warning: checkpoint dropped {dropped_vectors} vectors of nodes that do not exist");
    }
  }

  /// Check if checkpoint is recommended based on WAL usage
  pub fn should_checkpoint(&self, threshold: f64) -> bool {
    let usage = self.wal_buffer.lock().usage_ratio();
    usage >= threshold
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::core::header::other_header_slot;
  use crate::core::single_file::{open_single_file, SingleFileOpenOptions};
  use std::fs::OpenOptions as FsOpenOptions;
  use std::io::{Seek, SeekFrom, Write};
  use std::sync::{mpsc, Arc, Barrier};
  use std::time::Duration;
  use tempfile::tempdir;

  fn seeded_db(path: &std::path::Path) -> (SingleFileDB, SingleFileOpenOptions) {
    let options = SingleFileOpenOptions::new().auto_checkpoint(false);
    let db = open_single_file(path, options.clone()).expect("expected value");

    db.begin(false).expect("expected value");
    for index in 0..256 {
      db.create_node(Some(&format!("old-{index}")))
        .expect("expected value");
    }
    db.commit().expect("expected value");
    db.checkpoint().expect("expected value");

    db.begin(false).expect("expected value");
    db.create_node(Some("new-node")).expect("expected value");
    db.commit().expect("expected value");
    (db, options)
  }

  #[test]
  fn abort_during_snapshot_write_does_not_destroy_installed_snapshot() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("expected value");
    let db_path = temp_dir.path().join("checkpoint-mid-write.kitedb");
    let (db, options) = seeded_db(&db_path);

    set_checkpoint_test_fault(Some(CheckpointPhase::SnapshotPageWritten));
    assert!(db.checkpoint().is_err());
    drop(db);

    let reopened = open_single_file(&db_path, options).expect("expected value");
    assert!(reopened.node_by_key("old-0").is_some());
    assert!(reopened.node_by_key("new-node").is_some());
  }

  #[test]
  fn checkpoint_crash_phases_preserve_committed_state() {
    let _serial = checkpoint_test_serial();
    for (index, phase) in [
      CheckpointPhase::SnapshotPageWritten,
      CheckpointPhase::SnapshotDurable,
      CheckpointPhase::HeaderWritten,
      CheckpointPhase::HeaderDurable,
    ]
    .into_iter()
    .enumerate()
    {
      let temp_dir = tempdir().expect("expected value");
      let db_path = temp_dir
        .path()
        .join(format!("checkpoint-crash-{index}.kitedb"));
      let (db, options) = seeded_db(&db_path);
      set_checkpoint_test_fault(Some(phase));
      assert!(db.checkpoint().is_err(), "phase {phase:?} should abort");
      drop(db);

      let reopened = open_single_file(&db_path, options).expect("expected value");
      assert!(reopened.node_by_key("old-0").is_some());
      assert!(reopened.node_by_key("new-node").is_some());
    }
  }

  #[test]
  fn torn_newest_header_slot_falls_back_to_other_slot() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("expected value");
    let db_path = temp_dir.path().join("checkpoint-torn-header.kitedb");
    let options = SingleFileOpenOptions::new().auto_checkpoint(false);
    let db = open_single_file(&db_path, options.clone()).expect("expected value");
    db.begin(false).expect("expected value");
    db.create_node(Some("durable-node"))
      .expect("expected value");
    db.commit().expect("expected value");
    db.checkpoint().expect("expected value");
    let newest_slot = db.header_slot.load(Ordering::Acquire);
    let page_size = db.header.read().page_size as u64;
    drop(db);

    // Corrupt the newest slot's generation field so the other durable slot
    // must be selected.
    let mut file = FsOpenOptions::new()
      .read(true)
      .write(true)
      .open(&db_path)
      .expect("expected value");
    file
      .seek(SeekFrom::Start(newest_slot as u64 * page_size + 32))
      .expect("expected value");
    file.write_all(&[0xA5]).expect("expected value");
    file.sync_all().expect("expected value");

    let reopened = open_single_file(&db_path, options).expect("expected value");
    assert!(reopened.node_by_key("durable-node").is_some());
  }

  #[test]
  fn blocking_checkpoint_waits_for_pre_gate_commit() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("expected value");
    let db_path = temp_dir.path().join("checkpoint-concurrent.kitedb");
    let options = SingleFileOpenOptions::new().auto_checkpoint(false);
    let db = Arc::new(open_single_file(&db_path, options.clone()).expect("expected value"));
    db.begin(false).expect("expected value");
    db.create_node(Some("before")).expect("expected value");
    db.commit().expect("expected value");
    db.checkpoint().expect("expected value");

    let (ready_tx, ready_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel();
    let worker_db = Arc::clone(&db);
    let worker = std::thread::spawn(move || {
      worker_db.begin(false).expect("expected value");
      worker_db
        .create_node(Some("committed-during-checkpoint"))
        .expect("expected value");
      ready_tx.send(()).expect("expected value");
      go_rx.recv().expect("expected value");
      worker_db.commit().expect("expected value");
    });
    ready_rx.recv().expect("expected value");

    let gate_barrier = Arc::new(Barrier::new(2));
    set_checkpoint_test_barrier(
      &db,
      CheckpointPhase::GateAcquired,
      Arc::clone(&gate_barrier),
    );
    let checkpoint_db = Arc::clone(&db);
    let checkpoint_thread = std::thread::spawn(move || checkpoint_db.checkpoint());
    gate_barrier.wait();
    go_tx.send(()).expect("expected value");
    worker.join().expect("expected value");
    checkpoint_thread
      .join()
      .expect("expected value")
      .expect("expected value");

    assert!(db.node_by_key("committed-during-checkpoint").is_some());
    drop(db);
    let reopened = open_single_file(&db_path, options).expect("expected value");
    assert!(reopened
      .node_by_key("committed-during-checkpoint")
      .is_some());
  }

  #[test]
  fn background_checkpoint_preserves_post_cut_commit_live_and_after_reopen() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("expected value");
    let db_path = temp_dir
      .path()
      .join("checkpoint-background-post-cut.kitedb");
    let options = SingleFileOpenOptions::new().auto_checkpoint(false);
    let db = Arc::new(open_single_file(&db_path, options.clone()).expect("expected value"));
    db.begin(false).expect("expected value");
    db.create_node(Some("before-background"))
      .expect("expected value");
    db.commit().expect("expected value");

    let cut_barrier = Arc::new(Barrier::new(2));
    set_checkpoint_test_barrier(
      &db,
      CheckpointPhase::SnapshotDurable,
      Arc::clone(&cut_barrier),
    );
    let checkpoint_db = Arc::clone(&db);
    let checkpoint_thread = std::thread::spawn(move || checkpoint_db.background_checkpoint());
    cut_barrier.wait();

    db.begin(false).expect("expected value");
    db.create_node(Some("after-cut")).expect("expected value");
    db.commit().expect("expected value");

    checkpoint_thread
      .join()
      .expect("expected value")
      .expect("expected value");
    assert!(db.node_by_key("after-cut").is_some());

    drop(db);
    let reopened = open_single_file(&db_path, options).expect("expected value");
    assert!(reopened.node_by_key("after-cut").is_some());
  }

  /// Run a background checkpoint on another thread (with `fault` armed there)
  /// and run `post_cut` after its snapshot cut but before it completes.
  fn background_checkpoint_with_post_cut(
    db: &Arc<SingleFileDB>,
    fault: Option<CheckpointPhase>,
    post_cut: impl FnOnce(&SingleFileDB),
  ) -> Result<()> {
    let cut_barrier = Arc::new(Barrier::new(2));
    set_checkpoint_test_barrier(
      db,
      CheckpointPhase::SnapshotDurable,
      Arc::clone(&cut_barrier),
    );
    let checkpoint_db = Arc::clone(db);
    let checkpoint_thread = std::thread::spawn(move || {
      set_checkpoint_test_fault(fault);
      checkpoint_db.background_checkpoint()
    });

    // The marker is set under the header lock once the cut is installed; the
    // checkpoint thread then parks on the barrier until `post_cut` is done.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while db.header.read().checkpoint_in_progress == 0 {
      assert!(std::time::Instant::now() < deadline, "cut never happened");
      std::thread::yield_now();
    }
    post_cut(db);
    cut_barrier.wait();
    checkpoint_thread.join().expect("checkpoint thread")
  }

  fn commit_node(db: &SingleFileDB, key: &str) {
    db.begin(false).expect("begin");
    db.create_node(Some(key)).expect("create node");
    db.commit().expect("commit");
  }

  #[test]
  fn background_checkpoint_without_post_cut_commits_empties_wal() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("checkpoint-background-empty.kitedb");
    let options = SingleFileOpenOptions::new()
      .wal_size(64 * 1024)
      .auto_checkpoint(false);
    let db = open_single_file(&db_path, options.clone()).expect("open");
    for index in 0..50 {
      commit_node(&db, &format!("pre-{index}"));
    }
    assert!(db.wal_buffer.lock().usage_ratio() > 0.0);

    db.background_checkpoint().expect("background checkpoint");

    let stats = db.wal_stats();
    assert_eq!(
      (
        stats.active_region,
        stats.head,
        stats.tail,
        stats.primary_head
      ),
      (0, 0, 0, 0)
    );
    assert_eq!(db.wal_buffer.lock().usage_ratio(), 0.0);
    let header = db.header.read().clone();
    assert_eq!(
      (
        header.wal_head,
        header.wal_tail,
        header.wal_primary_head,
        header.active_wal_region,
        header.checkpoint_in_progress,
      ),
      (0, 0, 0, 0, 0)
    );

    drop(db);
    let reopened = open_single_file(&db_path, options).expect("reopen");
    assert_eq!(reopened.wal_stats().primary_head, 0);
    assert!(reopened.node_by_key("pre-0").is_some());
    assert!(reopened.node_by_key("pre-49").is_some());
  }

  #[test]
  fn background_checkpoint_moves_post_cut_records_to_primary_start() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("checkpoint-background-compact.kitedb");
    let options = SingleFileOpenOptions::new()
      .wal_size(64 * 1024)
      .auto_checkpoint(false);
    let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));
    db.begin(false).expect("begin");
    let node = db.create_node(Some("counter")).expect("node");
    let key = db.define_propkey("value").expect("propkey");
    db.set_node_prop(node, key, PropValue::I64(0))
      .expect("prop");
    db.commit().expect("commit");
    for value in 1..=40 {
      db.begin(false).expect("begin");
      db.set_node_prop(node, key, PropValue::I64(value))
        .expect("prop");
      db.commit().expect("commit");
    }
    let pre_cut_usage = db.wal_buffer.lock().usage_ratio();

    background_checkpoint_with_post_cut(&db, None, |db| {
      for value in [100, 101] {
        db.begin(false).expect("begin");
        db.set_node_prop(node, key, PropValue::I64(value))
          .expect("prop");
        db.commit().expect("commit");
      }
      commit_node(db, "after-cut");
    })
    .expect("background checkpoint");

    // Only the post-cut records remain, rewound to the primary start.
    let stats = db.wal_stats();
    assert_eq!((stats.active_region, stats.tail), (0, 0));
    assert_eq!(stats.head, stats.primary_head);
    assert!(!db.wal_buffer.lock().has_secondary_records());
    let usage = db.wal_buffer.lock().usage_ratio();
    assert!(
      usage > 0.0 && usage < pre_cut_usage / 4.0,
      "usage {usage} vs pre-cut {pre_cut_usage}"
    );
    let header = db.header.read().clone();
    assert_eq!(
      (
        header.wal_head,
        header.wal_tail,
        header.active_wal_region,
        header.checkpoint_in_progress,
      ),
      (stats.head, 0, 0, 0)
    );
    let retained = {
      let mut pager = db.pager.lock();
      db.wal_buffer
        .lock()
        .scan_records(&mut pager)
        .expect("scan WAL")
    };
    assert_eq!(committed_transactions(&retained).len(), 3);

    assert_eq!(db.node_prop(node, key), Some(PropValue::I64(101)));
    assert!(db.node_by_key("after-cut").is_some());

    drop(db);
    let reopened = open_single_file(&db_path, options).expect("reopen");
    assert_eq!(reopened.node_prop(node, key), Some(PropValue::I64(101)));
    assert!(reopened.node_by_key("after-cut").is_some());
    assert!(reopened.node_by_key("counter").is_some());
  }

  #[test]
  fn crash_before_post_cut_compaction_recovers_on_open() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir
      .path()
      .join("checkpoint-background-retained.kitedb");
    let options = SingleFileOpenOptions::new().auto_checkpoint(false);
    let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));
    commit_node(&db, "before-cut");

    let result =
      background_checkpoint_with_post_cut(&db, Some(CheckpointPhase::PostCutWalRetained), |db| {
        commit_node(db, "after-cut")
      });
    assert!(result.is_err());

    // The snapshot is installed, and the post-cut records are retained in
    // place in the secondary region.
    let header = db.header.read().clone();
    assert_eq!(
      (header.checkpoint_in_progress, header.active_wal_region),
      (0, 1)
    );
    assert_eq!(header.wal_tail, db.wal_buffer.lock().primary_region_size());
    assert!(db.wal_buffer.lock().is_primary_retired());
    assert!(db.node_by_key("before-cut").is_some());
    assert!(db.node_by_key("after-cut").is_some());

    // Crash: nothing else reaches the file.
    drop(db);

    // A read-only open replays the retained records in place.
    let bytes_before = std::fs::read(&db_path).expect("read file");
    let read_only =
      open_single_file(&db_path, options.clone().read_only(true)).expect("read-only open");
    assert!(read_only.node_by_key("before-cut").is_some());
    assert!(read_only.node_by_key("after-cut").is_some());
    drop(read_only);
    assert!(std::fs::read(&db_path).expect("read file") == bytes_before);

    let reopened = open_single_file(&db_path, options.clone()).expect("reopen");
    assert!(reopened.node_by_key("before-cut").is_some());
    assert!(reopened.node_by_key("after-cut").is_some());
    let stats = reopened.wal_stats();
    assert_eq!((stats.active_region, stats.tail), (0, 0));
    assert!(stats.primary_head > 0);
    assert_eq!(reopened.header.read().active_wal_region, 0);

    commit_node(&reopened, "after-reopen");
    crate::core::single_file::close_single_file(reopened).expect("close");
    let reopened = open_single_file(&db_path, options).expect("reopen again");
    for key in ["before-cut", "after-cut", "after-reopen"] {
      assert!(reopened.node_by_key(key).is_some(), "{key} missing");
    }
  }

  #[test]
  fn failed_post_cut_compaction_is_finished_by_next_background_checkpoint() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("checkpoint-background-resume.kitedb");
    let options = SingleFileOpenOptions::new().auto_checkpoint(false);
    let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));
    commit_node(&db, "before-cut");
    let result =
      background_checkpoint_with_post_cut(&db, Some(CheckpointPhase::PostCutWalRetained), |db| {
        commit_node(db, "after-cut")
      });
    assert!(result.is_err());

    // Commits keep landing in the retained secondary region.
    commit_node(&db, "while-retained");
    assert!(db.wal_buffer.lock().is_primary_retired());

    db.background_checkpoint()
      .expect("resumed background checkpoint");
    let stats = db.wal_stats();
    assert_eq!(
      (
        stats.active_region,
        stats.head,
        stats.tail,
        stats.primary_head
      ),
      (0, 0, 0, 0)
    );
    for key in ["before-cut", "after-cut", "while-retained"] {
      assert!(db.node_by_key(key).is_some(), "{key} missing live");
    }

    drop(db);
    let reopened = open_single_file(&db_path, options).expect("reopen");
    for key in ["before-cut", "after-cut", "while-retained"] {
      assert!(reopened.node_by_key(key).is_some(), "{key} missing");
    }
  }

  #[test]
  fn background_checkpoint_header_faults_preserve_post_cut_commits() {
    let _serial = checkpoint_test_serial();
    for (index, phase) in [
      CheckpointPhase::HeaderWritten,
      CheckpointPhase::HeaderDurable,
    ]
    .into_iter()
    .enumerate()
    {
      let temp_dir = tempdir().expect("temp dir");
      let db_path = temp_dir
        .path()
        .join(format!("checkpoint-background-header-{index}.kitedb"));
      let options = SingleFileOpenOptions::new().auto_checkpoint(false);
      let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));
      commit_node(&db, "before-cut");

      let result =
        background_checkpoint_with_post_cut(&db, Some(phase), |db| commit_node(db, "after-cut"));
      assert!(result.is_err(), "phase {phase:?} should abort");
      assert!(!db.is_checkpoint_running());
      assert!(db.node_by_key("before-cut").is_some());
      assert!(db.node_by_key("after-cut").is_some());

      // The process keeps working after the failed install.
      commit_node(&db, "after-failure");
      drop(db);

      let reopened = open_single_file(&db_path, options).expect("reopen");
      for key in ["before-cut", "after-cut", "after-failure"] {
        assert!(
          reopened.node_by_key(key).is_some(),
          "{key} missing after {phase:?}"
        );
      }
      reopened
        .background_checkpoint()
        .expect("checkpoint after recovery");
      assert!(reopened.node_by_key("after-cut").is_some());
    }
  }

  #[test]
  fn blocking_checkpoint_persists_reset_wal_regions() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("checkpoint-blocking-wal-state.kitedb");
    let options = SingleFileOpenOptions::new().auto_checkpoint(false);
    let db = open_single_file(&db_path, options.clone()).expect("open");
    db.begin(false).expect("begin");
    let a = db.create_node(Some("a")).expect("node a");
    let b = db.create_node(Some("b")).expect("node b");
    let knows = db.define_etype("knows").expect("etype");
    db.add_edge(a, knows, b).expect("edge");
    db.commit().expect("commit");
    db.checkpoint().expect("checkpoint");
    crate::core::single_file::close_single_file(db).expect("close");

    // The checkpoint emptied the WAL; a reopen must not resume appending
    // after the records the snapshot already covers.
    let db = open_single_file(&db_path, options.clone()).expect("reopen");
    assert_eq!(db.wal_stats().primary_head, 0);
    db.begin(false).expect("begin");
    db.create_node(Some("c")).expect("node c");
    db.commit().expect("commit");
    crate::core::single_file::close_single_file(db).expect("close");

    let db = open_single_file(&db_path, options).expect("reopen");
    assert_eq!(db.out_edges(a), vec![(knows, b)]);
    assert!(db.node_by_key("c").is_some());
  }

  #[test]
  fn repeated_checkpoints_reuse_orphaned_snapshot_space() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("checkpoint-bounded-growth.kitedb");
    let options = SingleFileOpenOptions::new()
      .wal_size(64 * 1024)
      .auto_checkpoint(false);
    let db = open_single_file(&db_path, options).expect("database");

    db.begin(false).expect("begin seed transaction");
    for index in 0..512 {
      db.create_node(Some(&format!("stable-{index}")))
        .expect("seed node");
    }
    db.commit().expect("commit seed transaction");
    db.checkpoint().expect("initial checkpoint");

    let header = db.header.read().clone();
    let snapshot_size = header.snapshot_page_count * header.page_size as u64;
    let initial_size = std::fs::metadata(&db_path).expect("initial metadata").len();

    for _ in 0..12 {
      db.checkpoint().expect("repeated checkpoint");
    }

    let final_size = std::fs::metadata(&db_path).expect("final metadata").len();
    let growth_budget = snapshot_size * 5 / 2 + header.page_size as u64;
    assert!(
      final_size < initial_size + growth_budget,
      "checkpoint file grew from {initial_size} to {final_size} with snapshot size {snapshot_size}"
    );
  }

  /// A blocking checkpoint whose header install fails must leave the WAL the
  /// old header names untouched. Regression: the WAL was reset in memory before
  /// the header write, so the next commit overwrote records the on-disk header
  /// still pointed at.
  #[test]
  fn failed_blocking_checkpoint_header_install_keeps_old_wal_intact() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("expected value");
    let db_path = temp_dir.path().join("failed-header-install.kitedb");
    // "new-node" exists only in the WAL; "old-*" are in the snapshot.
    let (db, options) = seeded_db(&db_path);

    let header_bytes = 2 * db.header.read().page_size as usize;
    let mut original_header_pages = vec![0u8; header_bytes];
    {
      use std::io::Read;
      let mut file = std::fs::File::open(&db_path).expect("open file");
      file
        .read_exact(&mut original_header_pages)
        .expect("read header pages");
    }

    // The header slot write never becomes durable.
    set_checkpoint_test_fault(Some(CheckpointPhase::HeaderWritten));
    assert!(db.checkpoint().is_err());

    // The process keeps running and commits again.
    db.begin(false).expect("expected value");
    db.create_node(Some("after-failed-checkpoint"))
      .expect("expected value");
    db.commit().expect("expected value");
    drop(db);

    // Crash model: neither the checkpoint's unsynced header write nor the
    // commit's header write reached the disk, but the commit's WAL write did.
    // The pre-checkpoint header is authoritative and must still find its WAL.
    {
      let mut file = FsOpenOptions::new()
        .write(true)
        .open(&db_path)
        .expect("open for write");
      file.seek(SeekFrom::Start(0)).expect("seek");
      file
        .write_all(&original_header_pages)
        .expect("restore header pages");
      file.sync_all().expect("sync");
    }

    let reopened = open_single_file(&db_path, options).expect("reopen");
    assert!(reopened.node_by_key("old-0").is_some());
    assert!(
      reopened.node_by_key("new-node").is_some(),
      "a commit made after a failed checkpoint overwrote WAL records the old header still names"
    );
  }

  fn read_header_pages(db_path: &std::path::Path, page_size: u32) -> Vec<u8> {
    let bytes = std::fs::read(db_path).expect("read database file");
    bytes[..2 * page_size as usize].to_vec()
  }

  fn write_header_pages(db_path: &std::path::Path, pages: &[u8]) {
    let mut file = FsOpenOptions::new()
      .write(true)
      .open(db_path)
      .expect("open for write");
    file.write_all(pages).expect("restore header pages");
    file.sync_all().expect("sync");
  }

  /// Open a copy of the file as it is now: the state a crash would leave.
  fn open_crash_copy(db_path: &std::path::Path, options: &SingleFileOpenOptions) -> SingleFileDB {
    let copy_path = db_path.with_extension("crash.kitedb");
    std::fs::copy(db_path, &copy_path).expect("copy database file");
    open_single_file(&copy_path, options.clone()).expect("open crash copy")
  }

  /// The slot a failed install wrote may hold a torn or unsynced header, so
  /// the next header write must go there instead of over the durable slot.
  #[test]
  fn failed_checkpoint_install_keeps_durable_header_slot_as_fallback() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("failed-install-slot.kitedb");
    let (db, _options) = seeded_db(&db_path);
    let durable_slot = db.header_slot.load(Ordering::Acquire);
    let page_size = db.header.read().page_size as usize;
    let slot_bytes = |slot: u32| {
      let bytes = std::fs::read(&db_path).expect("read database file");
      let start = slot as usize * page_size;
      bytes[start..start + page_size].to_vec()
    };
    let durable_bytes = slot_bytes(durable_slot);

    set_checkpoint_test_fault(Some(CheckpointPhase::HeaderWritten));
    assert!(db.checkpoint().is_err());
    assert_eq!(db.header_slot.load(Ordering::Acquire), durable_slot);

    commit_node(&db, "after-failed-checkpoint");
    assert_eq!(
      db.header_slot.load(Ordering::Acquire),
      other_header_slot(durable_slot)
    );
    assert!(
      slot_bytes(durable_slot) == durable_bytes,
      "a commit overwrote the last durable header slot"
    );
  }

  /// After a failed install, a header slot may still name the unused
  /// snapshot, so its pages stay out of reuse until the next checkpoint is
  /// installed in both slots; that checkpoint then reclaims them.
  #[test]
  fn checkpoint_after_failed_install_reclaims_unused_snapshot_pages() {
    let _serial = checkpoint_test_serial();
    for (index, phase) in [
      CheckpointPhase::HeaderWritten,
      CheckpointPhase::HeaderDurable,
    ]
    .into_iter()
    .enumerate()
    {
      let temp_dir = tempdir().expect("temp dir");
      let db_path = temp_dir
        .path()
        .join(format!("failed-install-reclaim-{index}.kitedb"));
      let (db, options) = seeded_db(&db_path);
      let installed = db.header.read().clone();

      set_checkpoint_test_fault(Some(phase));
      assert!(db.checkpoint().is_err(), "phase {phase:?} should abort");
      let header = db.header.read().clone();
      assert_eq!(
        (
          header.active_snapshot_gen,
          header.snapshot_start_page,
          header.wal_head
        ),
        (
          installed.active_snapshot_gen,
          installed.snapshot_start_page,
          installed.wal_head
        ),
        "memory must keep describing the installed snapshot and its WAL"
      );
      let unused = db.pager.lock().deferred_free_page_list();
      assert!(!unused.is_empty());

      // Commits keep appending after the WAL records the old header names.
      commit_node(&db, "after-failure");
      let crashed = open_crash_copy(&db_path, &options);
      for key in ["old-0", "new-node", "after-failure"] {
        assert!(
          crashed.node_by_key(key).is_some(),
          "{key} missing after a crash following {phase:?}"
        );
      }
      drop(crashed);

      db.checkpoint().expect("checkpoint after failed install");
      let header = db.header.read().clone();
      let installed_range =
        header.snapshot_start_page..header.snapshot_start_page + header.snapshot_page_count;
      assert!(
        unused
          .iter()
          .all(|page| !installed_range.contains(&(*page as u64))),
        "pages a header slot may name were reused"
      );
      {
        let pager = db.pager.lock();
        assert!(pager.deferred_free_page_list().is_empty());
        let file_pages = (pager.file_size() / header.page_size as u64) as u32;
        assert!(
          unused
            .iter()
            .all(|&page| page >= file_pages || pager.is_range_free(page, page + 1)),
          "unused snapshot pages leaked"
        );
      }
      for key in ["old-0", "new-node", "after-failure"] {
        assert!(db.node_by_key(key).is_some(), "{key} missing live");
      }

      drop(db);
      let reopened = open_single_file(&db_path, options).expect("reopen");
      for key in ["old-0", "new-node", "after-failure"] {
        assert!(
          reopened.node_by_key(key).is_some(),
          "{key} missing after {phase:?}"
        );
      }
    }
  }

  /// Optimize installs its snapshot the same way, so a failed install must
  /// also leave later commits appending after the old WAL.
  #[test]
  fn failed_optimize_header_install_keeps_old_wal_intact() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("failed-optimize-install.kitedb");
    let (db, options) = seeded_db(&db_path);
    let original_header_pages = read_header_pages(&db_path, db.header.read().page_size);

    set_checkpoint_test_fault(Some(CheckpointPhase::HeaderWritten));
    assert!(db.optimize_single_file(None).is_err());
    commit_node(&db, "after-failed-optimize");
    drop(db);

    // Neither header write reached the disk; the commit's WAL write did.
    write_header_pages(&db_path, &original_header_pages);
    let reopened = open_single_file(&db_path, options).expect("reopen");
    assert!(reopened.node_by_key("old-0").is_some());
    assert!(reopened.node_by_key("new-node").is_some());
  }

  /// A background checkpoint starts while another thread's write transaction
  /// is open, without waiting for it. The transaction commits between the cut
  /// and the install, and survives a crash at that point, the install, and a
  /// reopen, applied exactly once.
  #[test]
  fn background_checkpoint_carries_transaction_open_at_the_cut() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("checkpoint-background-open-tx.kitedb");
    let options = SingleFileOpenOptions::new().auto_checkpoint(false);
    let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));

    // The edge is in the snapshot and deleted in the delta, so applying the
    // open transaction's re-add twice would list it twice.
    db.begin(false).expect("begin");
    let a = db.create_node(Some("a")).expect("node a");
    let b = db.create_node(Some("b")).expect("node b");
    let knows = db.define_etype("knows").expect("etype");
    db.add_edge(a, knows, b).expect("edge");
    db.commit().expect("commit");
    db.checkpoint().expect("checkpoint");
    db.begin(false).expect("begin");
    db.delete_edge(a, knows, b).expect("delete edge");
    db.commit().expect("commit");

    let (opened_tx, opened_rx) = mpsc::channel();
    let (cut_tx, cut_rx) = mpsc::channel::<()>();
    let writer_db = Arc::clone(&db);
    let writer = std::thread::spawn(move || {
      writer_db.begin(false).expect("begin");
      writer_db.add_edge(a, knows, b).expect("re-add before cut");
      writer_db
        .create_node(Some("open-at-cut"))
        .expect("create before cut");
      opened_tx.send(()).expect("signal open");
      cut_rx.recv().expect("wait for cut");
      writer_db
        .create_node(Some("written-after-cut"))
        .expect("create after cut");
      writer_db.commit().expect("commit");
    });
    opened_rx.recv().expect("writer opened");

    let expected_keys = ["a", "b", "open-at-cut", "written-after-cut"];
    let crash_options = options.clone();
    background_checkpoint_with_post_cut(&db, None, move |db| {
      cut_tx.send(()).expect("release writer");
      writer.join().expect("writer thread");

      // Crash with the cut marker durable: open merges both WAL regions,
      // which hold the writer's pre-cut records twice.
      let crashed = open_crash_copy(db.path(), &crash_options);
      for key in expected_keys {
        assert!(
          crashed.node_by_key(key).is_some(),
          "{key} missing after crash"
        );
      }
      assert_eq!(crashed.out_edges(a), vec![(knows, b)]);
    })
    .expect("background checkpoint");

    assert!(!db.is_checkpoint_running());
    for key in expected_keys {
      assert!(db.node_by_key(key).is_some(), "{key} missing live");
    }
    assert_eq!(db.out_edges(a), vec![(knows, b)]);

    drop(db);
    let reopened = open_single_file(&db_path, options).expect("reopen");
    for key in expected_keys {
      assert!(reopened.node_by_key(key).is_some(), "{key} missing");
    }
    assert_eq!(reopened.out_edges(a), vec![(knows, b)]);
  }

  /// A transaction open for the whole background checkpoint, including the
  /// install, keeps working and commits afterwards.
  #[test]
  fn transaction_open_across_background_checkpoint_commits_after_install() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("checkpoint-background-long-tx.kitedb");
    // Write transactions open at once need MVCC (without it they run one at a time).
    let options = SingleFileOpenOptions::new()
      .mvcc(true)
      .mvcc_gc_interval_ms(10)
      .auto_checkpoint(false);
    let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));
    commit_node(&db, "before");

    let (opened_tx, opened_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let writer_db = Arc::clone(&db);
    let writer = std::thread::spawn(move || {
      writer_db.begin(false).expect("begin");
      writer_db.create_node(Some("long-1")).expect("create");
      opened_tx.send(()).expect("signal open");
      done_rx.recv().expect("wait for checkpoint");
      writer_db.create_node(Some("long-2")).expect("create");
      writer_db.commit().expect("commit");
    });
    opened_rx.recv().expect("writer opened");

    db.background_checkpoint().expect("background checkpoint");
    commit_node(&db, "after-install");
    done_tx.send(()).expect("release writer");
    writer.join().expect("writer thread");

    for key in ["before", "long-1", "long-2", "after-install"] {
      assert!(db.node_by_key(key).is_some(), "{key} missing live");
    }
    drop(db);
    let reopened = open_single_file(&db_path, options).expect("reopen");
    for key in ["before", "long-1", "long-2", "after-install"] {
      assert!(reopened.node_by_key(key).is_some(), "{key} missing");
    }
  }

  /// The background snapshot holds exactly the commits before the cut. Later
  /// commits are replayed over it at completion; had the snapshot already
  /// applied the delete, the re-add would cancel the replayed delete instead
  /// of restoring the edge.
  #[test]
  fn background_snapshot_holds_only_pre_cut_commits() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir
      .path()
      .join("checkpoint-background-cut-state.kitedb");
    let options = SingleFileOpenOptions::new().auto_checkpoint(false);
    let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));
    db.begin(false).expect("begin");
    let a = db.create_node(Some("a")).expect("node a");
    let b = db.create_node(Some("b")).expect("node b");
    let knows = db.define_etype("knows").expect("etype");
    db.add_edge(a, knows, b).expect("edge");
    db.commit().expect("commit");

    let released = Arc::new(Barrier::new(2));
    let written = Arc::new(Barrier::new(2));
    set_checkpoint_test_barrier(&db, CheckpointPhase::CutReleased, Arc::clone(&released));
    set_checkpoint_test_barrier(&db, CheckpointPhase::SnapshotDurable, Arc::clone(&written));
    let checkpoint_db = Arc::clone(&db);
    let checkpoint_thread = std::thread::spawn(move || checkpoint_db.background_checkpoint());

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while db.header.read().checkpoint_in_progress == 0 {
      assert!(std::time::Instant::now() < deadline, "cut never happened");
      std::thread::yield_now();
    }
    // After the cut, before the snapshot is collected.
    db.begin(false).expect("begin");
    db.delete_edge(a, knows, b).expect("delete edge");
    db.commit().expect("commit");
    released.wait();
    // After the snapshot is written.
    written.wait();
    db.begin(false).expect("begin");
    db.add_edge(a, knows, b).expect("re-add edge");
    db.commit().expect("commit");
    checkpoint_thread
      .join()
      .expect("checkpoint thread")
      .expect("background checkpoint");

    assert_eq!(db.out_edges(a), vec![(knows, b)]);
    drop(db);
    let reopened = open_single_file(&db_path, options).expect("reopen");
    assert_eq!(reopened.out_edges(a), vec![(knows, b)]);
  }

  /// A blocking checkpoint must not run between a background checkpoint's
  /// cut and its install: it would reset the WAL holding the post-cut
  /// commits, and the background install would then replace its snapshot.
  #[test]
  fn blocking_checkpoint_waits_for_running_background_checkpoint() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir
      .path()
      .join("checkpoint-blocking-during-background.kitedb");
    let options = SingleFileOpenOptions::new().auto_checkpoint(false);
    let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));
    commit_node(&db, "before-cut");

    let blocking_db = Arc::clone(&db);
    let mut blocking = None;
    background_checkpoint_with_post_cut(&db, None, |db| {
      commit_node(db, "after-cut");
      let handle = std::thread::spawn(move || blocking_db.checkpoint());
      std::thread::sleep(Duration::from_millis(100));
      assert!(
        !handle.is_finished(),
        "blocking checkpoint ran inside a background checkpoint"
      );
      blocking = Some(handle);
    })
    .expect("background checkpoint");
    blocking
      .expect("blocking checkpoint thread")
      .join()
      .expect("blocking checkpoint thread")
      .expect("blocking checkpoint");

    for key in ["before-cut", "after-cut"] {
      assert!(db.node_by_key(key).is_some(), "{key} missing live");
    }
    drop(db);
    let reopened = open_single_file(&db_path, options).expect("reopen");
    for key in ["before-cut", "after-cut"] {
      assert!(reopened.node_by_key(key).is_some(), "{key} missing");
    }
  }

  /// A blocking checkpoint holding the gate could be waiting for the
  /// caller's own transaction, so the caller is refused instead.
  #[test]
  fn background_checkpoint_refuses_callers_open_transaction() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("checkpoint-background-own-tx.kitedb");
    let db = open_single_file(
      &db_path,
      SingleFileOpenOptions::new().auto_checkpoint(false),
    )
    .expect("open");
    db.begin(false).expect("begin");
    db.create_node(Some("mine")).expect("create");
    assert!(matches!(
      db.background_checkpoint(),
      Err(KiteError::TransactionInProgress)
    ));
    assert_eq!(db.checkpoint_status(), CheckpointStatus::Idle);
    db.commit().expect("commit");
    db.background_checkpoint().expect("background checkpoint");
    assert!(db.node_by_key("mine").is_some());
  }

  /// A thread with an open transaction that calls `begin` again while a
  /// checkpoint holds the gate (waiting for that very transaction) gets an
  /// error instead of blocking on the gate forever.
  #[test]
  fn begin_inside_open_transaction_fails_while_checkpoint_holds_gate() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("checkpoint-nested-begin.kitedb");
    let db = Arc::new(
      open_single_file(
        &db_path,
        SingleFileOpenOptions::new().auto_checkpoint(false),
      )
      .expect("open"),
    );

    let (ready_tx, ready_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let (result_tx, result_rx) = mpsc::channel();
    let worker_db = Arc::clone(&db);
    let worker = std::thread::spawn(move || {
      worker_db.begin(false).expect("begin");
      worker_db.create_node(Some("worker")).expect("create");
      ready_tx.send(()).expect("signal ready");
      go_rx.recv().expect("wait for gate");
      let nested = worker_db.begin(false);
      result_tx
        .send(matches!(nested, Err(KiteError::TransactionInProgress)))
        .expect("send result");
      worker_db.commit().expect("commit");
    });
    ready_rx.recv().expect("worker ready");

    let gate_barrier = Arc::new(Barrier::new(2));
    set_checkpoint_test_barrier(
      &db,
      CheckpointPhase::GateAcquired,
      Arc::clone(&gate_barrier),
    );
    let checkpoint_db = Arc::clone(&db);
    let checkpoint_thread = std::thread::spawn(move || checkpoint_db.checkpoint());
    gate_barrier.wait();
    go_tx.send(()).expect("release worker");

    let refused = result_rx
      .recv_timeout(Duration::from_secs(10))
      .expect("nested begin blocked on the checkpoint gate");
    assert!(refused, "nested begin must fail with TransactionInProgress");
    worker.join().expect("worker thread");
    checkpoint_thread
      .join()
      .expect("checkpoint thread")
      .expect("checkpoint");
    assert!(db.node_by_key("worker").is_some());
  }

  /// An auto background checkpoint triggered by one thread's commit starts
  /// and finishes while another thread holds its transaction open, so it
  /// neither waits for that thread nor gives up.
  #[test]
  fn auto_background_checkpoint_runs_while_another_thread_holds_a_transaction() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("checkpoint-auto-open-tx.kitedb");
    // Write transactions open at once need MVCC (without it they run one at a time).
    let options = SingleFileOpenOptions::new()
      .mvcc(true)
      .mvcc_gc_interval_ms(10)
      .wal_size(64 * 1024)
      .auto_checkpoint(true)
      .checkpoint_threshold(0.5)
      .background_checkpoint(true);
    let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));

    let (opened_tx, opened_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let holder_db = Arc::clone(&db);
    let holder = std::thread::spawn(move || {
      holder_db.begin(false).expect("begin");
      holder_db.create_node(Some("held-open")).expect("create");
      opened_tx.send(()).expect("signal open");
      go_rx.recv().expect("wait");
      holder_db.commit().expect("commit");
    });
    opened_rx.recv().expect("holder opened");

    let start_gen = db.header.read().active_snapshot_gen;
    let (done_tx, done_rx) = mpsc::channel();
    let committer_db = Arc::clone(&db);
    let committer = std::thread::spawn(move || {
      let mut index = 0;
      while committer_db.header.read().active_snapshot_gen < start_gen + 2 {
        assert!(index < 5000, "auto checkpoints never ran");
        commit_node(&committer_db, &format!("c-{index}"));
        index += 1;
      }
      done_tx.send(index).expect("send count");
    });
    let commits = done_rx
      .recv_timeout(Duration::from_secs(60))
      .expect("committer stalled behind the open transaction");
    committer.join().expect("committer thread");

    go_tx.send(()).expect("release holder");
    holder.join().expect("holder thread");
    assert!(db.node_by_key("held-open").is_some());

    let db = Arc::try_unwrap(db).ok().expect("sole owner");
    crate::core::single_file::close_single_file(db).expect("close");
    let reopened = open_single_file(&db_path, options).expect("reopen");
    assert!(reopened.node_by_key("held-open").is_some());
    assert!(reopened.node_by_key("c-0").is_some());
    assert!(reopened
      .node_by_key(&format!("c-{}", commits - 1))
      .is_some());
  }

  /// When an open transaction's records do not fit in the secondary region,
  /// the background checkpoint declines before its cut, and the transaction
  /// commits in the primary region as usual.
  #[test]
  fn background_checkpoint_declines_when_open_transactions_do_not_fit() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir
      .path()
      .join("checkpoint-background-carry-full.kitedb");
    let options = SingleFileOpenOptions::new()
      .wal_size(64 * 1024)
      .auto_checkpoint(false);
    let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));
    commit_node(&db, "before");

    // About 20 KiB of records: more than the 16 KiB secondary region.
    let (opened_tx, opened_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let writer_db = Arc::clone(&db);
    let writer = std::thread::spawn(move || {
      writer_db.begin(false).expect("begin");
      for index in 0..10 {
        let key = format!("big-{index}-{}", "x".repeat(2000));
        writer_db.create_node(Some(&key)).expect("create");
      }
      opened_tx.send(()).expect("signal open");
      go_rx.recv().expect("wait");
      writer_db.commit().expect("commit");
    });
    opened_rx.recv().expect("writer opened");

    assert!(matches!(
      db.background_checkpoint(),
      Err(KiteError::CheckpointDeclined(_))
    ));
    assert_eq!(db.checkpoint_status(), CheckpointStatus::Idle);
    assert_eq!(db.header.read().checkpoint_in_progress, 0);
    assert_eq!(db.wal_stats().active_region, 0);

    go_tx.send(()).expect("release writer");
    writer.join().expect("writer thread");
    let big_key = format!("big-9-{}", "x".repeat(2000));
    assert!(db.node_by_key(&big_key).is_some());
    commit_node(&db, "after");

    drop(db);
    let reopened = open_single_file(&db_path, options).expect("reopen");
    for key in ["before", big_key.as_str(), "after"] {
      assert!(reopened.node_by_key(key).is_some(), "missing {key:.12}");
    }
  }

  /// A background checkpoint that fails after carrying an open transaction
  /// rebuilds the WAL from both regions, which then hold that transaction's
  /// first records twice. It still commits exactly once, live and on reopen.
  #[test]
  fn failed_background_checkpoint_keeps_carried_transaction_exactly_once() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir
      .path()
      .join("checkpoint-background-carry-failure.kitedb");
    let options = SingleFileOpenOptions::new().auto_checkpoint(false);
    let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));

    // The edge is in the snapshot and deleted in the delta, so applying the
    // open transaction's re-add twice would list it twice.
    db.begin(false).expect("begin");
    let a = db.create_node(Some("a")).expect("node a");
    let b = db.create_node(Some("b")).expect("node b");
    let knows = db.define_etype("knows").expect("etype");
    db.add_edge(a, knows, b).expect("edge");
    db.commit().expect("commit");
    db.checkpoint().expect("checkpoint");
    db.begin(false).expect("begin");
    db.delete_edge(a, knows, b).expect("delete edge");
    db.commit().expect("commit");

    let (opened_tx, opened_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let writer_db = Arc::clone(&db);
    let writer = std::thread::spawn(move || {
      writer_db.begin(false).expect("begin");
      writer_db.add_edge(a, knows, b).expect("re-add");
      writer_db.create_node(Some("carried")).expect("create");
      opened_tx.send(()).expect("signal open");
      go_rx.recv().expect("wait");
      writer_db
        .create_node(Some("after-failure"))
        .expect("create after failure");
      writer_db.commit().expect("commit");
    });
    opened_rx.recv().expect("writer opened");

    let checkpoint_db = Arc::clone(&db);
    let result = std::thread::spawn(move || {
      set_checkpoint_test_fault(Some(CheckpointPhase::CutReleased));
      checkpoint_db.background_checkpoint()
    })
    .join()
    .expect("checkpoint thread");
    assert!(result.is_err());
    assert_eq!(db.checkpoint_status(), CheckpointStatus::Idle);
    assert_eq!(db.header.read().checkpoint_in_progress, 0);

    go_tx.send(()).expect("release writer");
    writer.join().expect("writer thread");
    for key in ["a", "b", "carried", "after-failure"] {
      assert!(db.node_by_key(key).is_some(), "{key} missing live");
    }
    assert_eq!(db.out_edges(a), vec![(knows, b)]);

    drop(db);
    let reopened = open_single_file(&db_path, options).expect("reopen");
    for key in ["a", "b", "carried", "after-failure"] {
      assert!(reopened.node_by_key(key).is_some(), "{key} missing");
    }
    assert_eq!(reopened.out_edges(a), vec![(knows, b)]);
  }

  /// Vacuum relocates the snapshot onto pages an earlier checkpoint freed;
  /// those pages must leave the free list. Regression: they stayed free, so the
  /// next checkpoint wrote its new snapshot over the live one before its header
  /// install, breaking copy-on-write (a crash mid-write could leave the header
  /// naming half-overwritten pages).
  #[test]
  fn checkpoint_after_vacuum_never_overwrites_the_live_snapshot() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("expected value");
    let db_path = temp_dir.path().join("vacuum-free-list.kitedb");
    let options = SingleFileOpenOptions::new()
      .auto_checkpoint(false)
      .background_checkpoint(false);
    let db = open_single_file(&db_path, options.clone()).expect("open");

    // A large first snapshot.
    db.begin(false).expect("expected value");
    let mut ids = Vec::new();
    for index in 0..4000 {
      ids.push(
        db.create_node(Some(&format!("bulk-{index}")))
          .expect("expected value"),
      );
    }
    db.commit().expect("expected value");
    db.checkpoint().expect("expected value");

    // A small second snapshot; the large region goes on the free list.
    db.begin(false).expect("expected value");
    for id in &ids[10..] {
      db.delete_node(*id).expect("expected value");
    }
    db.commit().expect("expected value");
    db.checkpoint().expect("expected value");

    // Vacuum moves the small snapshot down onto the freed region. Keep the
    // WAL size so the file reopens with the same options.
    db.vacuum_single_file(Some(crate::core::single_file::VacuumOptions {
      shrink_wal: false,
      min_wal_size: None,
    }))
    .expect("vacuum");

    let (live_start, live_count) = {
      let header = db.header.read();
      (header.snapshot_start_page, header.snapshot_page_count)
    };

    db.begin(false).expect("expected value");
    db.create_node(Some("after-vacuum"))
      .expect("expected value");
    db.commit().expect("expected value");
    db.checkpoint().expect("checkpoint after vacuum");

    // Copy-on-write: the new snapshot must not have been written onto the
    // pages the installed snapshot occupied.
    let (new_start, new_count) = {
      let header = db.header.read();
      (header.snapshot_start_page, header.snapshot_page_count)
    };
    let overlaps = new_start < live_start + live_count && live_start < new_start + new_count;
    assert!(
      !overlaps,
      "checkpoint wrote its snapshot (pages {new_start}..{}) over the live snapshot (pages {live_start}..{})",
      new_start + new_count,
      live_start + live_count
    );

    drop(db);
    let reopened = open_single_file(&db_path, options).expect("reopen");
    assert!(reopened.node_by_key("bulk-0").is_some());
    assert!(reopened.node_by_key("bulk-500").is_none());
    assert!(reopened.node_by_key("after-vacuum").is_some());
  }

  /// A large snapshot replaced by a small one: the large region is on the free
  /// list, directly after the WAL, and the small snapshot follows it.
  fn db_with_freed_snapshot_region(
    db_path: &std::path::Path,
  ) -> (SingleFileDB, SingleFileOpenOptions) {
    let options = SingleFileOpenOptions::new()
      .auto_checkpoint(false)
      .background_checkpoint(false);
    let db = open_single_file(db_path, options.clone()).expect("open");

    db.begin(false).expect("begin");
    let mut ids = Vec::new();
    for index in 0..4000 {
      ids.push(
        db.create_node(Some(&format!("bulk-{index}")))
          .expect("create node"),
      );
    }
    db.commit().expect("commit");
    db.checkpoint().expect("checkpoint large snapshot");

    db.begin(false).expect("begin");
    for id in &ids[10..] {
      db.delete_node(*id).expect("delete node");
    }
    db.commit().expect("commit");
    db.checkpoint().expect("checkpoint small snapshot");

    let header = db.header.read().clone();
    let wal_end = header.wal_start_page + header.wal_page_count;
    let freed = db.pager.lock().free_page_list();
    assert!(
      freed.len() >= 2 && u64::from(freed[0]) == wal_end,
      "setup: the large snapshot's pages should be free directly after the WAL"
    );
    (db, options)
  }

  /// Every page on the free or deferred list must lie outside the header, the
  /// WAL, and the installed snapshot: a checkpoint may write its new snapshot
  /// onto a free page before its header install.
  fn assert_free_lists_exclude_live_pages(db: &SingleFileDB, context: &str) {
    let header = db.header.read().clone();
    let wal_end = header.wal_start_page + header.wal_page_count;
    let snapshot =
      header.snapshot_start_page..header.snapshot_start_page + header.snapshot_page_count;
    let pager = db.pager.lock();
    for (list, pages) in [
      ("free", pager.free_page_list()),
      ("deferred", pager.deferred_free_page_list()),
    ] {
      for page in pages.into_iter().map(u64::from) {
        assert!(
          page >= wal_end,
          "{context}: {list} page {page} lies in the header or WAL (pages 0..{wal_end})"
        );
        assert!(
          !snapshot.contains(&page),
          "{context}: {list} page {page} lies in the installed snapshot (pages {}..{})",
          snapshot.start,
          snapshot.end
        );
      }
    }
  }

  /// Checkpoint, asserting the new snapshot avoided every page the previously
  /// installed header named.
  fn checkpoint_avoiding_live_pages(db: &SingleFileDB, context: &str) {
    let before = db.header.read().clone();
    let wal_end = before.wal_start_page + before.wal_page_count;
    let live = before.snapshot_start_page..before.snapshot_start_page + before.snapshot_page_count;
    db.checkpoint().expect(context);

    let after = db.header.read().clone();
    let written = after.snapshot_start_page..after.snapshot_start_page + after.snapshot_page_count;
    assert!(
      written.start >= wal_end,
      "{context}: checkpoint wrote its snapshot (pages {}..{}) into the header or WAL (pages 0..{wal_end})",
      written.start,
      written.end
    );
    assert!(
      live.is_empty() || written.end <= live.start || live.end <= written.start,
      "{context}: checkpoint wrote its snapshot (pages {}..{}) over the live snapshot (pages {}..{})",
      written.start,
      written.end,
      live.start,
      live.end
    );
  }

  /// Growing the WAL moves the snapshot onto pages an earlier checkpoint freed
  /// and extends the WAL over others; none of them may stay free. Regression:
  /// they stayed on the free list, so the next checkpoint wrote its snapshot
  /// into the WAL, over records the installed header still named, before its
  /// header install.
  #[test]
  fn checkpoint_after_wal_resize_never_overwrites_the_wal_or_live_snapshot() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("resize-free-list.kitedb");
    let (db, options) = db_with_freed_snapshot_region(&db_path);

    // One more WAL page covers the first freed page; the snapshot moves onto
    // the second.
    let header = db.header.read().clone();
    let wal_size = (header.wal_page_count as usize + 1) * header.page_size as usize;
    db.resize_wal(
      wal_size,
      Some(crate::core::single_file::ResizeWalOptions {
        allow_shrink: false,
        checkpoint: false,
      }),
    )
    .expect("resize WAL");
    assert_free_lists_exclude_live_pages(&db, "after resize");

    commit_node(&db, "after-resize");
    checkpoint_avoiding_live_pages(&db, "checkpoint after resize");
    commit_node(&db, "after-checkpoint");

    drop(db);
    let reopened = open_single_file(&db_path, options.wal_size(wal_size)).expect("reopen");
    for key in ["bulk-0", "after-resize", "after-checkpoint"] {
      assert!(reopened.node_by_key(key).is_some(), "{key} missing");
    }
    assert!(reopened.node_by_key("bulk-500").is_none());
  }

  /// A failed install defers its snapshot's pages. Growing the WAL over them
  /// makes them WAL pages, so they must leave the deferred list rather than
  /// become free at the next install. Regression: they stayed deferred, the
  /// next install freed them, and the checkpoint after it wrote its snapshot
  /// into the WAL.
  #[test]
  fn wal_resize_over_deferred_pages_never_frees_wal_pages() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("resize-deferred.kitedb");
    let options = SingleFileOpenOptions::new()
      .auto_checkpoint(false)
      .background_checkpoint(false);
    let db = open_single_file(&db_path, options.clone()).expect("open");
    commit_node(&db, "before-resize");
    db.checkpoint().expect("checkpoint");

    // The install fails, deferring the unused snapshot's pages. The WAL stays
    // empty, as resize requires.
    set_checkpoint_test_fault(Some(CheckpointPhase::HeaderWritten));
    assert!(db.checkpoint().is_err());
    let deferred = db.pager.lock().deferred_free_page_list();
    let last_deferred = *deferred.last().expect("deferred pages");

    // Grow the WAL over every deferred page.
    let header = db.header.read().clone();
    let wal_pages = u64::from(last_deferred) + 1 - header.wal_start_page;
    let wal_size = wal_pages as usize * header.page_size as usize;
    db.resize_wal(
      wal_size,
      Some(crate::core::single_file::ResizeWalOptions {
        allow_shrink: false,
        checkpoint: false,
      }),
    )
    .expect("resize WAL");
    assert_free_lists_exclude_live_pages(&db, "after resize");

    commit_node(&db, "after-resize");
    checkpoint_avoiding_live_pages(&db, "first checkpoint after resize");
    commit_node(&db, "after-first-checkpoint");
    checkpoint_avoiding_live_pages(&db, "second checkpoint after resize");

    drop(db);
    let reopened = open_single_file(&db_path, options.wal_size(wal_size)).expect("reopen");
    for key in ["before-resize", "after-resize", "after-first-checkpoint"] {
      assert!(reopened.node_by_key(key).is_some(), "{key} missing");
    }
  }

  /// Vacuum may move the snapshot onto pages a failed install deferred; like
  /// freed pages it moves onto, they must leave their list.
  #[test]
  fn vacuum_leaves_no_live_pages_on_the_free_or_deferred_lists() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("vacuum-deferred.kitedb");
    let (db, options) = db_with_freed_snapshot_region(&db_path);

    // The failed install took the low end of the freed region, right after
    // the WAL, where vacuum will put the snapshot.
    set_checkpoint_test_fault(Some(CheckpointPhase::HeaderWritten));
    assert!(db.checkpoint().is_err());
    assert!(!db.pager.lock().deferred_free_page_list().is_empty());

    db.vacuum_single_file(Some(crate::core::single_file::VacuumOptions {
      shrink_wal: false,
      min_wal_size: None,
    }))
    .expect("vacuum");
    assert_free_lists_exclude_live_pages(&db, "after vacuum");

    commit_node(&db, "after-vacuum");
    checkpoint_avoiding_live_pages(&db, "checkpoint after vacuum");
    assert_free_lists_exclude_live_pages(&db, "after checkpoint");

    drop(db);
    let reopened = open_single_file(&db_path, options).expect("reopen");
    for key in ["bulk-0", "after-vacuum"] {
      assert!(reopened.node_by_key(key).is_some(), "{key} missing");
    }
    assert!(reopened.node_by_key("bulk-500").is_none());
  }

  /// Defense in depth: even if bookkeeping wrongly lists live pages as free, a
  /// checkpoint must not write its snapshot over the WAL or the installed
  /// snapshot.
  #[test]
  fn checkpoint_never_reuses_live_pages_listed_as_free() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("live-pages-listed-free.kitedb");
    // "new-node" exists only in the WAL.
    let (db, options) = seeded_db(&db_path);

    let header = db.header.read().clone();
    {
      let mut pager = db.pager.lock();
      pager.free_pages(header.wal_start_page as u32, header.wal_page_count as u32);
      pager.free_pages(
        header.snapshot_start_page as u32,
        header.snapshot_page_count as u32,
      );
    }
    checkpoint_avoiding_live_pages(&db, "checkpoint with live pages listed free");
    assert_free_lists_exclude_live_pages(&db, "after checkpoint");

    commit_node(&db, "after-checkpoint");
    drop(db);
    let reopened = open_single_file(&db_path, options).expect("reopen");
    for key in ["old-0", "new-node", "after-checkpoint"] {
      assert!(reopened.node_by_key(key).is_some(), "{key} missing");
    }
  }

  /// While a background checkpoint runs, writes go to the secondary WAL
  /// region. If that region fills before the checkpoint finishes (e.g. the
  /// checkpoint thread is starved of CPU), writers must wait for the
  /// checkpoint instead of failing. Regression: they got `WalBufferFull`
  /// immediately, so concurrent writers under load still filled the WAL.
  #[test]
  fn writers_wait_for_a_running_background_checkpoint_when_the_secondary_region_fills() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("checkpoint-backpressure.kitedb");
    // 64 KB WAL: the secondary region is 16 KB, far less than the writer needs.
    let options = SingleFileOpenOptions::new()
      .wal_size(64 * 1024)
      .auto_checkpoint(false)
      .sync_mode(crate::core::single_file::SyncMode::Normal);
    let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));
    commit_node(&db, "before-checkpoint");

    const COMMITS: usize = 600;
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let writer_db = Arc::clone(&db);
    let writer = std::thread::spawn(move || -> std::result::Result<(), String> {
      go_rx.recv().expect("wait for the cut");
      for index in 0..COMMITS {
        writer_db
          .begin(false)
          .map_err(|error| format!("begin #{index}: {error}"))?;
        writer_db
          .create_node(Some(&format!("during-{index}")))
          .map_err(|error| format!("create_node #{index}: {error}"))?;
        writer_db
          .commit()
          .map_err(|error| format!("commit #{index}: {error}"))?;
      }
      Ok(())
    });

    background_checkpoint_with_post_cut(&db, None, move |_db| {
      // The checkpoint is parked after its cut. Start overflowing the
      // secondary region, then keep the checkpoint parked long enough for a
      // writer that doesn't wait to hit the full region and fail.
      go_tx.send(()).expect("start writer");
      std::thread::sleep(std::time::Duration::from_millis(500));
    })
    .expect("background checkpoint");

    let outcome = writer.join().expect("writer thread");
    assert_eq!(
      outcome,
      Ok(()),
      "a writer failed instead of waiting for the checkpoint"
    );

    let db = Arc::try_unwrap(db).ok().expect("sole owner");
    drop(db);
    let reopened = open_single_file(&db_path, options).expect("reopen");
    assert!(reopened.node_by_key("before-checkpoint").is_some());
    assert!(reopened.node_by_key("during-0").is_some());
    assert!(reopened
      .node_by_key(&format!("during-{}", COMMITS - 1))
      .is_some());
  }

  /// Build a database whose small snapshot sits at the end of the file, so
  /// vacuum has to relocate it (via its temporary append-only copy).
  fn db_needing_snapshot_relocation(
    path: &std::path::Path,
  ) -> (SingleFileDB, SingleFileOpenOptions) {
    let options = SingleFileOpenOptions::new()
      .auto_checkpoint(false)
      .background_checkpoint(false);
    let db = open_single_file(path, options.clone()).expect("open");
    db.begin(false).expect("expected value");
    let mut ids = Vec::new();
    for index in 0..4000 {
      ids.push(
        db.create_node(Some(&format!("bulk-{index}")))
          .expect("expected value"),
      );
    }
    db.commit().expect("expected value");
    db.checkpoint().expect("expected value");
    db.begin(false).expect("expected value");
    for id in &ids[10..] {
      db.delete_node(*id).expect("expected value");
    }
    db.commit().expect("expected value");
    db.checkpoint().expect("expected value");
    (db, options)
  }

  /// After vacuum both header slots must name a valid layout. Regression:
  /// vacuum wrote its final header to one slot only, so the other still named
  /// the temporary snapshot copy vacuum had truncated; tearing the newest slot
  /// left the database unopenable.
  #[test]
  fn vacuum_survives_a_torn_newest_header_slot() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("expected value");
    let db_path = temp_dir.path().join("vacuum-torn-header.kitedb");
    let (db, options) = db_needing_snapshot_relocation(&db_path);
    db.vacuum_single_file(Some(crate::core::single_file::VacuumOptions {
      shrink_wal: false,
      min_wal_size: None,
    }))
    .expect("vacuum");
    let newest_slot = db.header_slot.load(Ordering::Acquire);
    let page_size = db.header.read().page_size as u64;
    drop(db);

    let mut file = FsOpenOptions::new()
      .read(true)
      .write(true)
      .open(&db_path)
      .expect("expected value");
    file
      .seek(SeekFrom::Start(newest_slot as u64 * page_size + 32))
      .expect("expected value");
    file.write_all(&[0xA5]).expect("expected value");
    file.sync_all().expect("expected value");

    let reopened =
      open_single_file(&db_path, options).expect("reopen with the newest header slot torn");
    assert!(reopened.node_by_key("bulk-0").is_some());
    assert!(reopened.node_by_key("bulk-500").is_none());
  }

  /// Vacuum must never shrink the WAL below the minimum, whatever
  /// `min_wal_size` says. Regression: `min_wal_size: Some(0)` produced a
  /// zero-page WAL.
  #[test]
  fn vacuum_never_shrinks_the_wal_below_the_minimum() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("expected value");
    let db_path = temp_dir.path().join("vacuum-min-wal.kitedb");
    let options = SingleFileOpenOptions::new().auto_checkpoint(false);
    let db = open_single_file(&db_path, options).expect("open");
    db.begin(false).expect("expected value");
    db.create_node(Some("node")).expect("expected value");
    db.commit().expect("expected value");
    db.checkpoint().expect("expected value");

    db.vacuum_single_file(Some(crate::core::single_file::VacuumOptions {
      shrink_wal: true,
      min_wal_size: Some(0),
    }))
    .expect("vacuum");

    // 16 pages is the compactor's MIN_WAL_PAGES.
    let wal_pages = db.header.read().wal_page_count;
    assert!(
      wal_pages >= 16,
      "vacuum shrank the WAL to {wal_pages} pages"
    );
    db.begin(false).expect("expected value");
    db.create_node(Some("after-vacuum"))
      .expect("write after vacuum");
    db.commit().expect("commit after vacuum");
  }

  /// Hold a write transaction open on another thread after it writes
  /// `records` nodes with ~2 KiB keys. Returns the sender that lets it commit.
  fn hold_big_transaction(
    db: &Arc<SingleFileDB>,
    records: usize,
  ) -> (mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    let (opened_tx, opened_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let writer_db = Arc::clone(db);
    let writer = std::thread::spawn(move || {
      writer_db.begin(false).expect("begin");
      for index in 0..records {
        let key = format!("big-{index}-{}", "x".repeat(2000));
        writer_db.create_node(Some(&key)).expect("create");
      }
      opened_tx.send(()).expect("signal open");
      go_rx.recv().expect("wait");
      writer_db.commit().expect("commit");
    });
    opened_rx.recv().expect("writer opened");
    (go_tx, writer)
  }

  /// A cut declined because an open transaction is too big to carry is not
  /// retried by every commit above the checkpoint threshold while that
  /// transaction stays open. Each retry flushed, synced, and scanned the
  /// whole primary region under the gate and the commit lock, only to decline
  /// again (3.9 ms per commit instead of 0.09 ms).
  #[test]
  fn declined_cut_is_not_retried_until_a_carried_transaction_finishes() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("checkpoint-declined-retry.kitedb");
    // Write transactions open at once need MVCC (without it they run one at a time).
    let options = SingleFileOpenOptions::new()
      .mvcc(true)
      .mvcc_gc_interval_ms(10)
      .wal_size(64 * 1024)
      .auto_checkpoint(true)
      .checkpoint_threshold(0.5)
      .background_checkpoint(true);
    let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));

    // About 20 KiB of records: more than the 16 KiB secondary region.
    let (go_tx, writer) = hold_big_transaction(&db, 10);
    let start_gen = db.header.read().active_snapshot_gen;

    let mut index = 0;
    let mut commits_above_threshold = 0;
    while commits_above_threshold < 20 {
      assert!(
        index < 1000,
        "the WAL never reached the checkpoint threshold"
      );
      commit_node(&db, &format!("c-{index}"));
      index += 1;
      if db.should_checkpoint(0.5) {
        commits_above_threshold += 1;
      }
    }
    assert_eq!(
      checkpoint_test_cuts(&db),
      1,
      "every commit above the threshold retried the declined cut"
    );
    assert_eq!(db.header.read().active_snapshot_gen, start_gen);

    // Once the big transaction finishes, the next commit above the threshold
    // checkpoints.
    go_tx.send(()).expect("release writer");
    writer.join().expect("writer thread");
    commit_node(&db, "after");
    assert_eq!(checkpoint_test_cuts(&db), 2);
    assert!(db.header.read().active_snapshot_gen > start_gen);

    let db = Arc::try_unwrap(db).ok().expect("sole owner");
    crate::core::single_file::close_single_file(db).expect("close");
    let reopened = open_single_file(&db_path, options).expect("reopen");
    let big_key = format!("big-9-{}", "x".repeat(2000));
    for key in ["c-0", big_key.as_str(), "after"] {
      assert!(reopened.node_by_key(key).is_some(), "missing {key:.12}");
    }
  }

  /// Park a background checkpoint right after its cut, commit `post_cut`,
  /// then let the checkpoint panic once its snapshot is written: between its
  /// cut and its install.
  fn background_checkpoint_panicking_after_cut(db: &Arc<SingleFileDB>, post_cut: &str) {
    let released = Arc::new(Barrier::new(2));
    set_checkpoint_test_barrier(db, CheckpointPhase::CutReleased, Arc::clone(&released));
    let checkpoint_db = Arc::clone(db);
    let checkpoint_thread = std::thread::spawn(move || {
      set_checkpoint_test_panic(Some(CheckpointPhase::SnapshotDurable));
      checkpoint_db.background_checkpoint()
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while db.header.read().checkpoint_in_progress == 0 {
      assert!(std::time::Instant::now() < deadline, "cut never happened");
      std::thread::yield_now();
    }
    commit_node(db, post_cut);
    released.wait();
    assert!(
      checkpoint_thread.join().is_err(),
      "the injected panic should reach the checkpoint thread"
    );
  }

  /// Run `operation` on another thread and fail if it does not finish in time.
  fn finishes_in_time<T: Send + 'static>(
    what: &str,
    operation: impl FnOnce() -> T + Send + 'static,
  ) -> T {
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
      let _ = done_tx.send(operation());
    });
    done_rx
      .recv_timeout(Duration::from_secs(20))
      .unwrap_or_else(|_| panic!("{what} hung"))
  }

  /// A panic between a background checkpoint's cut and its install must not
  /// leave it marked running: blocking checkpoint, optimize, vacuum, and
  /// resize wait for a running background checkpoint and hung forever. The
  /// cut stays durable, so no commit is lost.
  #[test]
  fn panic_between_cut_and_install_does_not_leave_the_checkpoint_running() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("checkpoint-background-panic.kitedb");
    let options = SingleFileOpenOptions::new().auto_checkpoint(false);
    let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));
    commit_node(&db, "before-cut");

    background_checkpoint_panicking_after_cut(&db, "after-cut");
    assert_eq!(db.checkpoint_status(), CheckpointStatus::Idle);

    // A crash now recovers both commits.
    let crashed = open_crash_copy(&db_path, &options);
    for key in ["before-cut", "after-cut"] {
      assert!(
        crashed.node_by_key(key).is_some(),
        "{key} missing after crash"
      );
    }
    drop(crashed);

    let checkpoint_db = Arc::clone(&db);
    finishes_in_time(
      "blocking checkpoint after a panicked background checkpoint",
      move || checkpoint_db.checkpoint(),
    )
    .expect("blocking checkpoint");
    for key in ["before-cut", "after-cut"] {
      assert!(db.node_by_key(key).is_some(), "{key} missing live");
    }
    assert_eq!(db.header.read().checkpoint_in_progress, 0);

    commit_node(&db, "after-checkpoint");
    drop(db);
    let reopened = open_single_file(&db_path, options).expect("reopen");
    for key in ["before-cut", "after-cut", "after-checkpoint"] {
      assert!(reopened.node_by_key(key).is_some(), "{key} missing");
    }
  }

  /// A cut left behind by a background checkpoint that stopped before its
  /// install (here a panic) is finished by the next background checkpoint,
  /// which never waits for open transactions, so it works even while one is
  /// open.
  #[test]
  fn abandoned_cut_is_finished_by_the_next_background_checkpoint() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir
      .path()
      .join("checkpoint-background-resume-cut.kitedb");
    let options = SingleFileOpenOptions::new().auto_checkpoint(false);
    let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));

    // The edge is in the snapshot and deleted in the delta, so a pre-cut
    // change applied twice would show.
    db.begin(false).expect("begin");
    let a = db.create_node(Some("a")).expect("node a");
    let b = db.create_node(Some("b")).expect("node b");
    let knows = db.define_etype("knows").expect("etype");
    db.add_edge(a, knows, b).expect("edge");
    db.commit().expect("commit");
    db.checkpoint().expect("checkpoint");
    db.begin(false).expect("begin");
    db.delete_edge(a, knows, b).expect("delete edge");
    db.commit().expect("commit");

    background_checkpoint_panicking_after_cut(&db, "after-cut");
    let start_gen = db.header.read().active_snapshot_gen;
    commit_node(&db, "while-abandoned");

    let (opened_tx, opened_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let writer_db = Arc::clone(&db);
    let writer = std::thread::spawn(move || {
      writer_db.begin(false).expect("begin");
      writer_db.add_edge(a, knows, b).expect("re-add edge");
      writer_db.create_node(Some("open-across")).expect("create");
      opened_tx.send(()).expect("signal open");
      go_rx.recv().expect("wait");
      writer_db.commit().expect("commit");
    });
    opened_rx.recv().expect("writer opened");

    let checkpoint_db = Arc::clone(&db);
    finishes_in_time(
      "background checkpoint finishing an abandoned cut",
      move || checkpoint_db.background_checkpoint(),
    )
    .expect("background checkpoint");
    assert!(db.header.read().active_snapshot_gen > start_gen);
    assert_eq!(db.header.read().checkpoint_in_progress, 0);
    go_tx.send(()).expect("release writer");
    writer.join().expect("writer thread");

    let keys = ["a", "b", "after-cut", "while-abandoned", "open-across"];
    for key in keys {
      assert!(db.node_by_key(key).is_some(), "{key} missing live");
    }
    assert_eq!(db.out_edges(a), vec![(knows, b)]);
    drop(db);
    let reopened = open_single_file(&db_path, options).expect("reopen");
    for key in keys {
      assert!(reopened.node_by_key(key).is_some(), "{key} missing");
    }
    assert_eq!(reopened.out_edges(a), vec![(knows, b)]);
  }

  /// A blocking checkpoint must not hold the gate waiting for a transaction
  /// whose writer is itself waiting for a background checkpoint to install:
  /// that install needs the gate. Everything finishes, and the background
  /// checkpoint installs instead of being cancelled as stalled.
  #[test]
  fn blocking_checkpoint_does_not_deadlock_with_a_writer_waiting_for_a_background_install() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("checkpoint-wait-vs-blocking.kitedb");
    let options = SingleFileOpenOptions::new()
      .wal_size(64 * 1024)
      .auto_checkpoint(false);
    let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));
    commit_node(&db, "before-cut");

    let snapshot_written = Arc::new(Barrier::new(2));
    set_checkpoint_test_barrier(
      &db,
      CheckpointPhase::SnapshotDurable,
      Arc::clone(&snapshot_written),
    );
    let background_db = Arc::clone(&db);
    let background = std::thread::spawn(move || background_db.background_checkpoint());
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while db.header.read().checkpoint_in_progress == 0 {
      assert!(std::time::Instant::now() < deadline, "cut never happened");
      std::thread::yield_now();
    }

    // One transaction writes more than the 16 KiB secondary region holds, so
    // it waits for the background install part-way (or, once its records
    // outgrow what it keeps back, all at once).
    let writer_db = Arc::clone(&db);
    let writer = std::thread::spawn(move || {
      writer_db.begin(false)?;
      for index in 0..24 {
        if let Err(error) = writer_db.create_node(Some(&format!("w-{index}-{}", "x".repeat(1000))))
        {
          let _ = writer_db.rollback();
          return Err(error);
        }
      }
      writer_db.commit()
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while db.wal_buffer.lock().free() > 2048 && !db.checkpoint_state.lock().writers_waited {
      assert!(
        std::time::Instant::now() < deadline,
        "writer never filled the secondary region"
      );
      std::thread::yield_now();
    }
    std::thread::sleep(Duration::from_millis(50));

    let blocking_db = Arc::clone(&db);
    let blocking = std::thread::spawn(move || blocking_db.checkpoint());
    std::thread::sleep(Duration::from_millis(100));
    snapshot_written.wait();

    let started = std::time::Instant::now();
    background
      .join()
      .expect("background checkpoint thread")
      .expect("background checkpoint installs");
    writer
      .join()
      .expect("writer thread")
      .expect("writer commits");
    blocking
      .join()
      .expect("blocking checkpoint thread")
      .expect("blocking checkpoint");
    assert!(
      started.elapsed() < Duration::from_secs(4),
      "took {:?}: the background checkpoint was cancelled as stalled",
      started.elapsed()
    );

    for key in ["before-cut", "w-0-", "w-23-"] {
      let key = if key.ends_with('-') {
        format!("{key}{}", "x".repeat(1000))
      } else {
        key.to_string()
      };
      assert!(db.node_by_key(&key).is_some(), "{key:.8} missing live");
    }
    drop(db);
    let reopened = open_single_file(&db_path, options).expect("reopen");
    assert!(reopened
      .node_by_key(&format!("w-23-{}", "x".repeat(1000)))
      .is_some());
  }

  /// A blocking checkpoint that fails to load the snapshot it wrote must
  /// leave the database as it was. Regression: the delta was cleared before
  /// the reload, so commits since the previous checkpoint vanished from reads,
  /// and the next checkpoint, built from the old snapshot and the empty delta,
  /// dropped them for good.
  #[test]
  fn failed_snapshot_reload_after_blocking_checkpoint_loses_nothing() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("failed-reload.kitedb");
    // "new-node" exists only in the WAL and the delta.
    let (db, options) = seeded_db(&db_path);

    set_checkpoint_test_fault(Some(CheckpointPhase::SnapshotReload));
    assert!(db.checkpoint().is_err());
    assert!(
      db.node_by_key("new-node").is_some(),
      "a commit vanished from reads after a failed snapshot reload"
    );

    db.checkpoint().expect("checkpoint after the failed one");
    commit_node(&db, "after");
    drop(db);
    let reopened = open_single_file(&db_path, options).expect("reopen");
    for key in ["old-0", "new-node", "after"] {
      assert!(reopened.node_by_key(key).is_some(), "{key} missing");
    }
  }

  /// Optimize installs its snapshot the same way, so a failed reload must
  /// not lose commits either.
  #[test]
  fn failed_snapshot_reload_after_optimize_loses_nothing() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("failed-optimize-reload.kitedb");
    let (db, options) = seeded_db(&db_path);

    set_checkpoint_test_fault(Some(CheckpointPhase::SnapshotReload));
    assert!(db.optimize_single_file(None).is_err());
    assert!(
      db.node_by_key("new-node").is_some(),
      "a commit vanished from reads after a failed snapshot reload"
    );

    db.checkpoint()
      .expect("checkpoint after the failed optimize");
    drop(db);
    let reopened = open_single_file(&db_path, options).expect("reopen");
    for key in ["old-0", "new-node"] {
      assert!(reopened.node_by_key(key).is_some(), "{key} missing");
    }
  }

  /// A background checkpoint that fails while replaying its post-cut records
  /// must not leave the new snapshot under the old delta. Regression: the
  /// snapshot was reloaded first, so the pre-cut commits in the delta applied
  /// twice (a node and an edge created before the cut were counted twice),
  /// and the next checkpoint wrote them twice.
  #[test]
  fn failed_post_cut_replay_does_not_apply_pre_cut_commits_twice() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("failed-post-cut-replay.kitedb");
    let options = SingleFileOpenOptions::new().auto_checkpoint(false);
    let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));
    db.begin(false).expect("begin");
    let a = db.create_node(Some("a")).expect("node a");
    let b = db.create_node(Some("b")).expect("node b");
    let knows = db.define_etype("knows").expect("etype");
    db.commit().expect("commit");
    db.checkpoint().expect("checkpoint");
    // Only in the delta when the background checkpoint cuts.
    db.begin(false).expect("begin");
    db.add_edge(a, knows, b).expect("edge");
    db.create_node(Some("before-cut")).expect("node");
    db.commit().expect("commit");

    let result =
      background_checkpoint_with_post_cut(&db, Some(CheckpointPhase::PostCutReplay), |db| {
        commit_node(db, "after-cut")
      });
    assert!(result.is_err());
    let expect_once = |db: &SingleFileDB, context: &str| {
      assert_eq!(
        db.count_nodes(),
        4,
        "{context}: nodes {:?}",
        db.list_nodes()
      );
      assert_eq!(db.count_edges(), 1, "{context}");
      assert_eq!(db.out_edges(a), vec![(knows, b)], "{context}");
      for key in ["a", "b", "before-cut", "after-cut"] {
        assert!(db.node_by_key(key).is_some(), "{context}: {key} missing");
      }
    };
    expect_once(&db, "after the failed checkpoint");

    db.checkpoint().expect("checkpoint after the failed one");
    expect_once(&db, "after the next checkpoint");
    drop(db);
    let reopened = open_single_file(&db_path, options).expect("reopen");
    expect_once(&reopened, "after reopen");
  }

  /// A read-only open of a file whose background checkpoint stopped between
  /// its cut and its install replays both WAL regions in place, without
  /// writing. (It used to refuse to open.)
  #[test]
  fn read_only_open_replays_an_unfinished_cut_in_place() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("read-only-unfinished-cut.kitedb");
    let options = SingleFileOpenOptions::new().auto_checkpoint(false);
    let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));
    commit_node(&db, "before-cut");
    background_checkpoint_panicking_after_cut(&db, "after-cut");
    drop(db);

    let bytes = std::fs::read(&db_path).expect("read file");
    let read_only = open_single_file(&db_path, options.read_only(true)).expect("read-only open");
    for key in ["before-cut", "after-cut"] {
      assert!(read_only.node_by_key(key).is_some(), "{key} missing");
    }
    drop(read_only);
    assert!(
      std::fs::read(&db_path).expect("read file") == bytes,
      "a read-only open wrote"
    );
  }

  /// A crash between a background checkpoint's cut and its install can leave
  /// more records in the two WAL regions than the primary region holds. Open
  /// then keeps the cut in place (replay reads both regions), and the next
  /// background checkpoint finishes it.
  #[test]
  fn unfinished_cut_too_big_to_merge_is_finished_after_reopen() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("unfinished-cut-too-big.kitedb");
    let options = SingleFileOpenOptions::new()
      .wal_size(64 * 1024)
      .auto_checkpoint(false);
    let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));
    // About 44 KiB of the 48 KiB primary region.
    let filler = |index: usize| format!("fill-{index}-{}", "f".repeat(1000));
    let mut fills = 0;
    while db.wal_stats().primary_head < 44 * 1024 {
      commit_node(&db, &filler(fills));
      fills += 1;
    }

    // About 6 KiB of post-cut commits: together they do not fit in primary.
    let post_cut = |index: usize| format!("post-{index}-{}", "p".repeat(1000));
    let snapshot_written = Arc::new(Barrier::new(2));
    set_checkpoint_test_barrier(
      &db,
      CheckpointPhase::SnapshotDurable,
      Arc::clone(&snapshot_written),
    );
    let checkpoint_db = Arc::clone(&db);
    let checkpoint = std::thread::spawn(move || checkpoint_db.background_checkpoint());
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while db.header.read().checkpoint_in_progress == 0 {
      assert!(std::time::Instant::now() < deadline, "cut never happened");
      std::thread::yield_now();
    }
    for index in 0..6 {
      commit_node(&db, &post_cut(index));
    }
    let crashed_path = db_path.with_extension("crash.kitedb");
    std::fs::copy(&db_path, &crashed_path).expect("copy");
    {
      let mut pager = db.pager.lock();
      let merged = db
        .wal_buffer
        .lock()
        .merged_cut_size(&mut pager)
        .expect("size");
      assert!(merged > db.wal_buffer.lock().primary_region_size());
    }
    snapshot_written.wait();
    checkpoint
      .join()
      .expect("checkpoint thread")
      .expect("checkpoint");
    drop(db);

    let expected: Vec<String> = (0..fills).map(filler).chain((0..6).map(post_cut)).collect();
    let reopened = open_single_file(&crashed_path, options.clone()).expect("reopen");
    assert_eq!(reopened.header.read().checkpoint_in_progress, 1);
    for key in &expected {
      assert!(reopened.node_by_key(key).is_some(), "{key:.12} missing");
    }
    reopened
      .background_checkpoint()
      .expect("background checkpoint finishing the cut");
    assert_eq!(reopened.header.read().checkpoint_in_progress, 0);
    // Only the post-cut commits stay in the WAL, rewound to the primary start.
    let stats = reopened.wal_stats();
    assert_eq!(stats.active_region, 0);
    assert!(
      stats.primary_head < 8 * 1024,
      "primary head {}",
      stats.primary_head
    );
    commit_node(&reopened, "after");
    drop(reopened);

    let reopened = open_single_file(&crashed_path, options).expect("reopen again");
    for key in expected.iter().map(String::as_str).chain(["after"]) {
      assert!(reopened.node_by_key(key).is_some(), "{key:.12} missing");
    }
  }
}

#[cfg(test)]
/// Regression tests from the independent review of 0c19303.
mod review_regressions {
  use super::*;
  use crate::core::single_file::{open_single_file, SingleFileOpenOptions};
  use std::sync::{Arc, Barrier};
  use std::time::{Duration, Instant};
  use tempfile::tempdir;

  fn commit_node(db: &SingleFileDB, key: &str) {
    db.begin(false).expect("begin");
    db.create_node(Some(key)).expect("create node");
    db.commit().expect("commit");
  }

  fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cond() {
      assert!(Instant::now() < deadline, "timed out waiting for {what}");
      std::thread::yield_now();
    }
  }

  /// REVIEW: complete_background_checkpoint calls set_checkpoint_idle() and
  /// then returns the compaction error; background_checkpoint() then calls
  /// abandon_background_checkpoint() a second time. If another background
  /// checkpoint B claims the status in between, A's abandon either stomps B's
  /// status to Idle or (seeing B's marker) rebuilds B's WAL out from under it.
  /// B then installs with WalBuffer::reset and drops a committed transaction.
  #[test]
  fn review_double_idle_after_failed_compaction_loses_post_cut_commit() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("review-double-idle.kitedb");
    let options = SingleFileOpenOptions::new().auto_checkpoint(false);
    let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));
    commit_node(&db, "before-a");

    // Checkpoint A: post-cut commit (so it retains + compacts), compaction faulted.
    let a_snapshot = Arc::new(Barrier::new(2));
    set_checkpoint_test_barrier(
      &db,
      CheckpointPhase::SnapshotDurable,
      Arc::clone(&a_snapshot),
    );
    let a_db = Arc::clone(&db);
    let a = std::thread::spawn(move || {
      set_checkpoint_test_fault(Some(CheckpointPhase::PostCutWalRetained));
      a_db.background_checkpoint()
    });
    wait_until("A's cut", || db.header.read().checkpoint_in_progress != 0);
    commit_node(&db, "after-a-cut");

    // Park A inside set_checkpoint_idle (status already Idle, notify blocked).
    let wait_guard = db.checkpoint_wait.lock();
    a_snapshot.wait();
    wait_until("A to go idle", || {
      db.checkpoint_status() == CheckpointStatus::Idle
    });

    // Checkpoint B claims the now-idle status, then queues on the gate A holds.
    let b_cut = Arc::new(Barrier::new(2));
    let b_snapshot = Arc::new(Barrier::new(2));
    set_checkpoint_test_barrier(&db, CheckpointPhase::CutReleased, Arc::clone(&b_cut));
    set_checkpoint_test_barrier(
      &db,
      CheckpointPhase::SnapshotDurable,
      Arc::clone(&b_snapshot),
    );
    let b_db = Arc::clone(&db);
    let b = std::thread::spawn(move || b_db.background_checkpoint());
    wait_until("B to claim", || {
      db.checkpoint_status() == CheckpointStatus::Running
    });
    drop(wait_guard);

    // B cuts; A finishes with its compaction error and runs abandon.
    b_cut.wait();
    let a_result = a.join().expect("A thread");
    assert!(
      a_result.is_err(),
      "A should report the injected compaction fault"
    );
    eprintln!(
      "after A's abandon: status={:?} marker={} (B is still between cut and install)",
      db.checkpoint_status(),
      db.header.read().checkpoint_in_progress
    );

    // A committed transaction lands after B's cut.
    commit_node(&db, "after-b-cut");
    if db.header.read().checkpoint_in_progress != 0 {
      // Status was stomped to Idle while B runs: a third checkpoint can start.
      let c = db.background_checkpoint();
      eprintln!("concurrent checkpoint C while B runs: {c:?}");
    }

    b_snapshot.wait();
    let b_result = b.join().expect("B thread");
    eprintln!("B result: {b_result:?}");

    for key in ["before-a", "after-a-cut"] {
      assert!(db.node_by_key(key).is_some(), "{key} missing live");
    }
    let live = db.node_by_key("after-b-cut").is_some();
    drop(db);
    let reopened = open_single_file(&db_path, options).expect("reopen");
    let durable = reopened.node_by_key("after-b-cut").is_some();
    assert!(
      live && durable,
      "committed 'after-b-cut' lost: live={live} after-reopen={durable}"
    );
  }

  /// Fill the primary region with committed ~1 KiB records until it reaches
  /// `target` bytes, then open a transaction on another thread that writes
  /// `open_records` more and waits. Returns (sender to release it, handle).
  fn fill_and_hold(
    db: &Arc<SingleFileDB>,
    target: u64,
    open_records: usize,
  ) -> (std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    let mut index = 0;
    while db.wal_stats().primary_head < target {
      commit_node(db, &format!("fill-{index}-{}", "f".repeat(1000)));
      index += 1;
    }
    let (opened_tx, opened_rx) = std::sync::mpsc::channel();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let writer_db = Arc::clone(db);
    let writer = std::thread::spawn(move || {
      writer_db.begin(false).expect("begin");
      for i in 0..open_records {
        writer_db
          .create_node(Some(&format!("open-{i}-{}", "o".repeat(1000))))
          .expect("create in open tx");
      }
      opened_tx.send(()).expect("signal");
      let _ = go_rx.recv();
      let _ = writer_db.commit();
    });
    opened_rx.recv().expect("writer opened");
    (go_tx, writer)
  }

  /// REVIEW: crash-recovery merges primary + secondary into the 75% primary
  /// region. The carry duplicates open transactions' records into the
  /// secondary, so a crash between the cut and the install can leave a WAL
  /// whose merge no longer fits: open() fails with WalBufferFull.
  #[test]
  fn review_crash_during_background_checkpoint_with_carry_is_unopenable() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("review-carry-overflow.kitedb");
    // Write transactions open at once need MVCC (without it they run one at a time).
    let options = SingleFileOpenOptions::new()
      .mvcc(true)
      .mvcc_gc_interval_ms(10)
      .wal_size(64 * 1024)
      .auto_checkpoint(false);
    let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));
    // primary region = 48 KiB, secondary = 16 KiB
    let (go, writer) = fill_and_hold(&db, 38 * 1024, 6);
    let before_cut = db.wal_stats();
    eprintln!(
      "before cut: primary_head={} (primary size {})",
      before_cut.primary_head,
      db.wal_buffer.lock().primary_region_size()
    );

    let snap = Arc::new(Barrier::new(2));
    set_checkpoint_test_barrier(&db, CheckpointPhase::SnapshotDurable, Arc::clone(&snap));
    let cp_db = Arc::clone(&db);
    let cp = std::thread::spawn(move || cp_db.background_checkpoint());
    wait_until("cut", || db.header.read().checkpoint_in_progress != 0);
    commit_node(&db, "post-cut");
    let stats = db.wal_stats();
    eprintln!(
      "after cut + 1 commit: primary_head={} secondary used={}",
      stats.primary_head,
      stats.secondary_head - db.wal_buffer.lock().secondary_region_size() * 3
    );

    // Crash here: copy the file as it is.
    let copy_path = db_path.with_extension("crash.kitedb");
    std::fs::copy(&db_path, &copy_path).expect("copy");
    let crashed = open_single_file(&copy_path, options.clone());

    snap.wait();
    let _ = cp.join();
    let _ = go.send(());
    let _ = writer.join();

    match crashed {
      Ok(crashed) => {
        assert!(crashed.node_by_key("post-cut").is_some(), "post-cut lost");
      }
      Err(error) => panic!("database unopenable after crash mid background checkpoint: {error:?}"),
    }
  }

  /// REVIEW: same WAL shape, but the checkpoint fails (instead of the process
  /// crashing). recover_from_checkpoint_error swallows the rebuild error and
  /// then persists a header naming the partially rebuilt WAL with the marker
  /// cleared, so committed post-cut records are dropped on the next open.
  #[test]
  fn review_failed_background_checkpoint_rebuild_overflow_drops_commits() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("review-carry-overflow-live.kitedb");
    // Write transactions open at once need MVCC (without it they run one at a time).
    let options = SingleFileOpenOptions::new()
      .mvcc(true)
      .mvcc_gc_interval_ms(10)
      .wal_size(64 * 1024)
      .auto_checkpoint(false);
    let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));
    let (go, writer) = fill_and_hold(&db, 38 * 1024, 6);

    let snap = Arc::new(Barrier::new(2));
    set_checkpoint_test_barrier(&db, CheckpointPhase::SnapshotDurable, Arc::clone(&snap));
    let cp_db = Arc::clone(&db);
    let cp = std::thread::spawn(move || {
      // e.g. EIO while installing the header (the new failed-install path)
      set_checkpoint_test_fault(Some(CheckpointPhase::HeaderWritten));
      cp_db.background_checkpoint()
    });
    wait_until("cut", || db.header.read().checkpoint_in_progress != 0);
    commit_node(&db, "post-cut");
    snap.wait();
    let result = cp.join().expect("cp thread");
    eprintln!("background checkpoint result: {result:?}");
    let live = db.node_by_key("post-cut").is_some();
    let _ = go.send(());
    let _ = writer.join();
    drop(db);

    let reopened = open_single_file(&db_path, options).expect("reopen");
    let durable = reopened.node_by_key("post-cut").is_some();
    let committed_open = reopened
      .node_by_key(&format!("open-5-{}", "o".repeat(1000)))
      .is_some();
    assert!(
      live && durable,
      "committed 'post-cut' lost after failed checkpoint: live={live} reopen={durable} (open tx committed later present: {committed_open})"
    );
  }

  fn crash_mid_background_checkpoint(
    name: &str,
    open_records: usize,
    post_cut_commits: usize,
  ) -> Result<SingleFileDB> {
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join(name);
    // Write transactions open at once need MVCC (without it they run one at a time).
    let options = SingleFileOpenOptions::new()
      .mvcc(true)
      .mvcc_gc_interval_ms(10)
      .wal_size(64 * 1024)
      .auto_checkpoint(false);
    let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));
    let target = 45768 - (open_records as u64) * 1080;
    let (go, writer) = fill_and_hold(&db, target, open_records);
    let snap = Arc::new(Barrier::new(2));
    set_checkpoint_test_barrier(&db, CheckpointPhase::SnapshotDurable, Arc::clone(&snap));
    let cp_db = Arc::clone(&db);
    let cp = std::thread::spawn(move || cp_db.background_checkpoint());
    wait_until("cut", || db.header.read().checkpoint_in_progress != 0);
    for i in 0..post_cut_commits {
      commit_node(
        &db,
        &format!(
          "post-cut-{i}-{}",
          if post_cut_commits > 1 {
            "p".repeat(1000)
          } else {
            String::new()
          }
        ),
      );
    }
    let stats = db.wal_stats();
    eprintln!(
      "{name}: primary_head={} secondary_used={}",
      stats.primary_head,
      stats.secondary_head - 49152
    );
    let copy_path = temp_dir.path().join("crash-copy.kitedb");
    std::fs::copy(&db_path, &copy_path).expect("copy");
    let crashed = open_single_file(&copy_path, options);
    snap.wait();
    let _ = cp.join();
    let _ = go.send(());
    let _ = writer.join();
    std::mem::forget(temp_dir);
    crashed
  }

  #[test]
  fn review_control_no_carry_one_post_cut_commit_opens() {
    let _serial = checkpoint_test_serial();
    let r = crash_mid_background_checkpoint("ctl1.kitedb", 0, 1);
    assert!(r.is_ok(), "control failed: {:?}", r.err());
  }

  #[test]
  fn review_control_no_carry_many_post_cut_commits() {
    let _serial = checkpoint_test_serial();
    let r = crash_mid_background_checkpoint("ctl2.kitedb", 0, 6);
    assert!(
      r.is_ok(),
      "pre-existing overflow without carry: {:?}",
      r.err()
    );
  }

  /// REVIEW: the cut accepts copies up to the whole secondary region. While
  /// the checkpoint runs, every other write then fails with WalBufferFull,
  /// including the carried transaction's own next write / commit, though the
  /// primary region has plenty of room (the old code declined the cut).
  #[test]
  fn review_carry_filling_secondary_fails_concurrent_writes() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("review-carry-fills-secondary.kitedb");
    // Write transactions open at once need MVCC (without it they run one at a time).
    let options = SingleFileOpenOptions::new()
      .mvcc(true)
      .mvcc_gc_interval_ms(10)
      .wal_size(64 * 1024)
      .auto_checkpoint(false);
    let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));
    commit_node(&db, "before");
    // ~15 KiB open transaction, secondary is 16 KiB, primary 48 KiB (mostly free).
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let (opened_tx, opened_rx) = std::sync::mpsc::channel();
    let (res_tx, res_rx) = std::sync::mpsc::channel();
    let writer_db = Arc::clone(&db);
    let writer = std::thread::spawn(move || {
      writer_db.begin(false).expect("begin");
      for i in 0..14 {
        writer_db
          .create_node(Some(&format!("t-{i}-{}", "x".repeat(1000))))
          .expect("create");
      }
      opened_tx.send(()).unwrap();
      go_rx.recv().unwrap();
      let more = writer_db.create_node(Some(&format!("t-more-{}", "x".repeat(1000))));
      let commit = writer_db.commit();
      res_tx.send((more.map(|_| ()), commit)).unwrap();
    });
    opened_rx.recv().unwrap();
    eprintln!(
      "primary usage before cut: {:.2}",
      db.wal_buffer.lock().usage_ratio()
    );

    let snap = Arc::new(Barrier::new(2));
    set_checkpoint_test_barrier(&db, CheckpointPhase::SnapshotDurable, Arc::clone(&snap));
    let cp_db = Arc::clone(&db);
    let cp = std::thread::spawn(move || cp_db.background_checkpoint());
    wait_until("cut", || db.header.read().checkpoint_in_progress != 0);
    eprintln!(
      "secondary usage right after cut: {:.2}",
      db.wal_buffer.lock().usage_ratio()
    );

    // Another thread's ordinary commit during the checkpoint.
    db.begin(false).expect("begin");
    let other = db.create_node(Some(&format!("other-{}", "y".repeat(1000))));
    eprintln!(
      "other writer's create during checkpoint: {:?}",
      other.as_ref().map(|_| ())
    );
    if other.is_ok() {
      db.commit().expect("commit");
    } else {
      let _ = db.rollback();
    }

    go_tx.send(()).unwrap();
    let (more, commit) = res_rx.recv().unwrap();
    eprintln!("carried tx: next write {more:?}, commit {commit:?}");
    writer.join().unwrap();
    snap.wait();
    let _ = cp.join();
    assert!(
      other.is_ok() && more.is_ok() && commit.is_ok(),
      "writes failed during background checkpoint"
    );
  }
  /// REVIEW (expected to pass): a carried transaction that rolls back after
  /// the cut leaves nothing behind, live, after a crash, and after reopen.
  #[test]
  fn review_carried_tx_rolled_back_leaves_nothing() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("review-carry-rollback.kitedb");
    let options = SingleFileOpenOptions::new().auto_checkpoint(false);
    let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));
    commit_node(&db, "before");
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let (opened_tx, opened_rx) = std::sync::mpsc::channel();
    let wdb = Arc::clone(&db);
    let writer = std::thread::spawn(move || {
      wdb.begin(false).expect("begin");
      wdb.create_node(Some("rolled-back")).expect("create");
      opened_tx.send(()).unwrap();
      go_rx.recv().unwrap();
      wdb.create_node(Some("rolled-back-2")).expect("create");
      wdb.rollback().expect("rollback");
    });
    opened_rx.recv().unwrap();
    let snap = Arc::new(Barrier::new(2));
    set_checkpoint_test_barrier(&db, CheckpointPhase::SnapshotDurable, Arc::clone(&snap));
    let cp_db = Arc::clone(&db);
    let cp = std::thread::spawn(move || cp_db.background_checkpoint());
    wait_until("cut", || db.header.read().checkpoint_in_progress != 0);
    go_tx.send(()).unwrap();
    writer.join().unwrap();
    commit_node(&db, "after");
    let copy_path = db_path.with_extension("crash.kitedb");
    std::fs::copy(&db_path, &copy_path).expect("copy");
    let crashed = open_single_file(&copy_path, options.clone()).expect("open crash copy");
    assert!(crashed.node_by_key("rolled-back").is_none());
    assert!(crashed.node_by_key("after").is_some());
    drop(crashed);
    snap.wait();
    cp.join().unwrap().expect("checkpoint");
    assert!(db.node_by_key("rolled-back").is_none());
    drop(db);
    let reopened = open_single_file(&db_path, options).expect("reopen");
    assert!(reopened.node_by_key("rolled-back").is_none());
    assert!(reopened.node_by_key("rolled-back-2").is_none());
    assert!(reopened.node_by_key("after").is_some());
  }

  /// REVIEW (expected to pass): a transaction carried across two consecutive
  /// background checkpoints, committing between the second cut and install,
  /// applies exactly once after a crash there and after reopen.
  #[test]
  fn review_tx_carried_across_two_checkpoints_applies_once() {
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("review-two-carries.kitedb");
    let options = SingleFileOpenOptions::new().auto_checkpoint(false);
    let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));
    commit_node(&db, "before");
    let (step_tx, step_rx) = std::sync::mpsc::channel::<()>();
    let (ack_tx, ack_rx) = std::sync::mpsc::channel::<TxId>();
    let wdb = Arc::clone(&db);
    let writer = std::thread::spawn(move || {
      let txid = wdb.begin(false).expect("begin");
      wdb.create_node(Some("t-1")).expect("t-1");
      ack_tx.send(txid).unwrap();
      step_rx.recv().unwrap();
      wdb.create_node(Some("t-2")).expect("t-2");
      ack_tx.send(txid).unwrap();
      step_rx.recv().unwrap();
      wdb.create_node(Some("t-3")).expect("t-3");
      wdb.commit().expect("commit");
      ack_tx.send(txid).unwrap();
    });
    let txid = ack_rx.recv().unwrap();
    db.background_checkpoint().expect("checkpoint 1");
    step_tx.send(()).unwrap();
    ack_rx.recv().unwrap();

    let snap = Arc::new(Barrier::new(2));
    set_checkpoint_test_barrier(&db, CheckpointPhase::SnapshotDurable, Arc::clone(&snap));
    let cp_db = Arc::clone(&db);
    let cp = std::thread::spawn(move || cp_db.background_checkpoint());
    wait_until("cut 2", || db.header.read().checkpoint_in_progress != 0);
    step_tx.send(()).unwrap();
    ack_rx.recv().unwrap();
    writer.join().unwrap();

    let copy_path = db_path.with_extension("crash.kitedb");
    std::fs::copy(&db_path, &copy_path).expect("copy");
    let crashed = open_single_file(&copy_path, options.clone()).expect("open crash copy");
    let records = {
      let mut pager = crashed.pager.lock();
      crashed
        .wal_buffer
        .lock()
        .scan_records(&mut pager)
        .expect("scan")
    };
    let committed = committed_transactions(&records);
    let t: Vec<_> = committed.iter().filter(|(id, _)| *id == txid).collect();
    assert_eq!(t.len(), 1, "carried tx committed {} times", t.len());
    assert_eq!(
      t[0].1.len(),
      3,
      "carried tx replays {} data records, expected 3",
      t[0].1.len()
    );
    for key in ["before", "t-1", "t-2", "t-3"] {
      assert!(
        crashed.node_by_key(key).is_some(),
        "{key} missing after crash"
      );
    }
    drop(crashed);
    snap.wait();
    cp.join().unwrap().expect("checkpoint 2");
    drop(db);
    let reopened = open_single_file(&db_path, options).expect("reopen");
    for key in ["before", "t-1", "t-2", "t-3"] {
      assert!(
        reopened.node_by_key(key).is_some(),
        "{key} missing after reopen"
      );
    }
  }
  /// REVIEW: a torn WAL record (header durable, WAL page not, e.g. a crash
  /// inside the commit's single fsync) leaves a hole open() never trims, so
  /// later records are appended after it. scan_region stops at the hole, so
  /// the cut does not see an open transaction written after it: nothing is
  /// carried, its post-cut COMMIT lands in secondary without a BEGIN, and the
  /// completion drops the acknowledged commit, live and on disk.
  #[test]
  fn review_carry_misses_open_tx_after_wal_hole() {
    use std::io::{Seek, SeekFrom, Write};
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("review-wal-hole.kitedb");
    let options = SingleFileOpenOptions::new().auto_checkpoint(false);
    let db = open_single_file(&db_path, options.clone()).expect("open");
    commit_node(&db, "before-hole");
    let h0 = db.wal_stats().primary_head;
    commit_node(&db, "torn");
    let (base, page_size) = {
      let h = db.header.read();
      (h.wal_start_page * h.page_size as u64, h.page_size)
    };
    let _ = page_size;
    drop(db); // crash
              // The torn transaction's first record never reached the disk intact.
    {
      let mut f = std::fs::OpenOptions::new()
        .write(true)
        .open(&db_path)
        .unwrap();
      f.seek(SeekFrom::Start(base + h0 + 9)).unwrap();
      f.write_all(&[0xEE]).unwrap();
      f.sync_all().unwrap();
    }
    let db = Arc::new(open_single_file(&db_path, options.clone()).expect("reopen"));
    assert!(db.node_by_key("before-hole").is_some());
    eprintln!(
      "after reopen: primary_head={} (hole at {h0})",
      db.wal_stats().primary_head
    );

    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let (opened_tx, opened_rx) = std::sync::mpsc::channel();
    let wdb = Arc::clone(&db);
    let writer = std::thread::spawn(move || {
      wdb.begin(false).expect("begin");
      wdb.create_node(Some("open-across-cut")).expect("create");
      opened_tx.send(()).unwrap();
      go_rx.recv().unwrap();
      wdb.commit().expect("commit");
    });
    opened_rx.recv().unwrap();
    let snap = Arc::new(Barrier::new(2));
    set_checkpoint_test_barrier(&db, CheckpointPhase::SnapshotDurable, Arc::clone(&snap));
    let cp_db = Arc::clone(&db);
    let cp = std::thread::spawn(move || cp_db.background_checkpoint());
    wait_until("cut", || db.header.read().checkpoint_in_progress != 0);
    go_tx.send(()).unwrap();
    writer.join().unwrap();
    let live_before = db.node_by_key("open-across-cut").is_some();
    snap.wait();
    cp.join().unwrap().expect("checkpoint");
    let live_after = db.node_by_key("open-across-cut").is_some();
    let db = Arc::try_unwrap(db).ok().unwrap();
    drop(db);
    let reopened = open_single_file(&db_path, options).expect("reopen");
    let durable = reopened.node_by_key("open-across-cut").is_some();
    assert!(
      live_before && live_after && durable,
      "acknowledged commit lost: live before install={live_before}, after install={live_after}, after reopen={durable}"
    );
  }

  #[test]
  fn review_control_commit_after_wal_hole_survives_crash() {
    use std::io::{Seek, SeekFrom, Write};
    let _serial = checkpoint_test_serial();
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("review-wal-hole-ctl.kitedb");
    let options = SingleFileOpenOptions::new().auto_checkpoint(false);
    let db = open_single_file(&db_path, options.clone()).expect("open");
    commit_node(&db, "before-hole");
    let h0 = db.wal_stats().primary_head;
    commit_node(&db, "torn");
    let base = {
      let h = db.header.read();
      h.wal_start_page * h.page_size as u64
    };
    drop(db);
    {
      let mut f = std::fs::OpenOptions::new()
        .write(true)
        .open(&db_path)
        .unwrap();
      f.seek(SeekFrom::Start(base + h0 + 9)).unwrap();
      f.write_all(&[0xEE]).unwrap();
      f.sync_all().unwrap();
    }
    let db = open_single_file(&db_path, options.clone()).expect("reopen");
    commit_node(&db, "acked-after-hole"); // SyncMode::Full: durable on return
    drop(db); // crash
    let reopened = open_single_file(&db_path, options).expect("reopen 2");
    assert!(
      reopened.node_by_key("acked-after-hole").is_some(),
      "pre-existing: acked commit after a WAL hole lost on crash"
    );
  }
}

/// Regression tests from the final review of the background-checkpoint batch.
#[cfg(test)]
#[path = "final_review_regressions.rs"]
mod final_review_regressions;

/// Wave-2 checkpoint reproductions (K1-K5).
#[cfg(test)]
#[path = "w2_checkpoint_tests.rs"]
mod w2_tests;

/// raydb-b4 engine-concurrency: checkpoint-gate fairness (F3).
#[cfg(test)]
#[path = "b4_checkpoint_tests.rs"]
mod b4_tests;

/// raydb-b4 wal-perf: crash images of the install's post-cut move-back (F5).
#[cfg(test)]
#[path = "b4_wal_perf_checkpoint_tests.rs"]
mod b4_wal_perf_tests;

/// raydb-b4 core-misc: vector-store compaction at checkpoint (B12).
#[cfg(test)]
#[path = "b4_core_misc_checkpoint_tests.rs"]
mod b4_core_misc_tests;

/// raydb-b4 commit-pipeline: commits during a background checkpoint, held at
/// a phase hook.
#[cfg(test)]
#[path = "b4_commit_pipeline_checkpoint_tests.rs"]
mod b4_commit_pipeline_tests;
