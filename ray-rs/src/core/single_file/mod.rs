//! Single-file database format (.kitedb)
//!
//! Provides open/close/read/write operations for single-file databases.
//! Layout: [Header A] [Header B] [WAL (N pages)] [append-only snapshots]
//!
//! Ported from src/ray/graph-db/single-file.ts

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use parking_lot::{Condvar, Mutex, RwLock};

use self::vector::VectorStoreLazyEntry;
use crate::constants::*;
use crate::core::header::{other_header_slot, write_header_slot};
use crate::core::pager::FilePager;
use crate::core::snapshot::reader::SnapshotData;
use crate::core::wal::buffer::WalBuffer;
use crate::error::Result;
use crate::mvcc::MvccManager;
use crate::types::*;
use crate::util::compression::CompressionOptions;
use crate::vector::types::VectorManifest;

// Submodules
mod check;
mod checkpoint;
mod commit_profile;
mod compactor;
mod iter;
mod mvcc_history;
mod open;
mod read;
mod recovery;
mod replication;
mod schema;
mod transaction;
mod tx_registry;
mod vector;
mod write;
mod writer_slot;

#[cfg(test)]
mod stress;

// Re-export everything for backward compatibility
pub use compactor::{ResizeWalOptions, SingleFileOptimizeOptions, VacuumOptions};
pub use iter::*;
pub use open::{
  close_single_file, close_single_file_with_options, open_single_file, SingleFileCloseOptions,
  SingleFileOpenOptions, SnapshotParseMode, SyncMode,
};
pub(crate) use transaction::CommitQueue;
pub use transaction::{Savepoint, SingleFileTxGuard};

// Also re-export recovery items that are used externally
pub use recovery::replay_wal_record;

/// Largest node ID the database issues or accepts. IDs stay within `i64` so
/// every binding can represent them.
pub const MAX_NODE_ID: NodeId = i64::MAX as NodeId;

// ============================================================================
// Transaction State (for single-file DB)
// ============================================================================

/// Schema entries staged by one thread-affine transaction.
///
/// The forward and reverse maps stay in transaction state until commit. A
/// transaction can therefore use a freshly allocated ID immediately while
/// other transactions continue to see only committed schema.
#[derive(Debug, Clone, Default)]
pub(crate) struct SchemaStaging {
  pub(crate) label_names: HashMap<String, LabelId>,
  pub(crate) label_ids: HashMap<LabelId, String>,
  pub(crate) etype_names: HashMap<String, ETypeId>,
  pub(crate) etype_ids: HashMap<ETypeId, String>,
  pub(crate) propkey_names: HashMap<String, PropKeyId>,
  pub(crate) propkey_ids: HashMap<PropKeyId, String>,
}

impl SchemaStaging {
  /// Whether nothing is staged.
  pub(crate) fn is_empty(&self) -> bool {
    self.label_names.is_empty() && self.etype_names.is_empty() && self.propkey_names.is_empty()
  }

  pub(crate) fn label_id(&self, name: &str) -> Option<LabelId> {
    self.label_names.get(name).copied()
  }

  pub(crate) fn label_name(&self, id: LabelId) -> Option<String> {
    self.label_ids.get(&id).cloned()
  }

  pub(crate) fn etype_id(&self, name: &str) -> Option<ETypeId> {
    self.etype_names.get(name).copied()
  }

  pub(crate) fn etype_name(&self, id: ETypeId) -> Option<String> {
    self.etype_ids.get(&id).cloned()
  }

  pub(crate) fn propkey_id(&self, name: &str) -> Option<PropKeyId> {
    self.propkey_names.get(name).copied()
  }

  pub(crate) fn propkey_name(&self, id: PropKeyId) -> Option<String> {
    self.propkey_ids.get(&id).cloned()
  }

  pub(crate) fn define_label(&mut self, id: LabelId, name: &str) {
    self.label_names.insert(name.to_string(), id);
    self.label_ids.insert(id, name.to_string());
  }

  pub(crate) fn define_etype(&mut self, id: ETypeId, name: &str) {
    self.etype_names.insert(name.to_string(), id);
    self.etype_ids.insert(id, name.to_string());
  }

  pub(crate) fn define_propkey(&mut self, id: PropKeyId, name: &str) {
    self.propkey_names.insert(name.to_string(), id);
    self.propkey_ids.insert(id, name.to_string());
  }
}

#[derive(Debug)]
struct SchemaReservation<Id> {
  id: Id,
  owners: HashSet<TxId>,
}

impl<Id> SchemaReservation<Id> {
  fn new(id: Id, txid: TxId) -> Self {
    let mut owners = HashSet::new();
    owners.insert(txid);
    Self { id, owners }
  }
}

/// In-process schema-name claims. A claim is made before a define WAL record
/// is emitted, so concurrent transactions defining one name share one ID.
/// The claim is memory-only: commit publishes the mapping and removes it;
/// rollback removes only that transaction's ownership. A process crash drops
/// all claims, and only committed WAL records recreate schema on reopen.
#[derive(Debug, Default)]
pub(crate) struct SchemaReservations {
  labels: HashMap<String, SchemaReservation<LabelId>>,
  etypes: HashMap<String, SchemaReservation<ETypeId>>,
  propkeys: HashMap<String, SchemaReservation<PropKeyId>>,
}

/// Transaction state for SingleFileDB
///
/// This is scoped to SingleFileDB and only tracks what single-file
/// transactions need.
#[derive(Debug, Clone)]
pub struct SingleFileTxState {
  pub txid: TxId,
  pub read_only: bool,
  pub snapshot_ts: u64,
  pub pending: DeltaState,
  pub(crate) schema: SchemaStaging,
  pub bulk_load: bool,
  /// The transaction's WAL records, unsalted: those not written to the WAL
  /// yet (see `wal_deferred_from`), and all of them for a primary's
  /// replication sidecar, which reads them at commit.
  pub pending_wal: Vec<u8>,
  /// A replica's replication apply; the only transactions in which a
  /// replica accepts data writes.
  pub(crate) replication_apply: bool,
  /// How it holds the writer slot (see `writer_slot`): every write
  /// transaction does, until it is settled.
  pub(crate) writer: Option<writer_slot::WriterMode>,
  /// With MVCC, what a write transaction read, kept here (thread-private)
  /// and handed to the transaction manager for its conflict check at commit.
  pub(crate) mvcc_reads: TxKeySet,
  /// What it wrote, kept and handed over the same way: writers never take
  /// the transaction manager's lock, which every commit takes.
  pub(crate) mvcc_writes: TxKeySet,
  /// With MVCC, where it is registered among the open transactions
  /// (`MvccManager::open`), until it ends: a commit hands it to its MVCC
  /// commit, which unregisters it.
  pub(crate) mvcc_slot: Option<crate::mvcc::OpenSlot>,
  /// Ids of its live savepoints, oldest first (see `SingleFileDB::savepoint`).
  pub(crate) savepoints: Vec<u64>,
  /// The id its next savepoint gets.
  pub(crate) next_savepoint_id: u64,
  /// The WAL records not written to the WAL yet: those in `pending_wal`
  /// from this offset on. A write transaction keeps its records here from
  /// its begin, its BEGIN record included (unwritten until `wal_begun`), so
  /// a small one reaches the WAL whole at commit, written by its commit
  /// group with no lock of its own. Once they outgrow `WAL_DEFER_BYTES`
  /// with no savepoint live, they are written (with the BEGIN record), and
  /// the transaction's later records go straight to the WAL. While a
  /// savepoint is live they always stay here, so rolling back to it drops
  /// them before they reach the WAL. A bulk load writes all its records at
  /// commit.
  pub(crate) wal_deferred_from: Option<usize>,
  /// Its BEGIN record is in the WAL (it is among `open_write_txids`, which
  /// a background checkpoint cut copies the records of).
  pub(crate) wal_begun: bool,
}

/// Bytes of WAL records a write transaction keeps to itself before it
/// writes them to the WAL (see `SingleFileTxState::wal_deferred_from`).
/// Below this the copy into the WAL under its commit group's locks is cheap;
/// above it, the transaction writes its records itself, as it makes them.
pub(crate) const WAL_DEFER_BYTES: usize = 16 * 1024;

impl SingleFileTxState {
  pub fn new(txid: TxId, read_only: bool, snapshot_ts: u64, bulk_load: bool) -> Self {
    Self {
      txid,
      read_only,
      snapshot_ts,
      pending: DeltaState::new(),
      schema: SchemaStaging::default(),
      bulk_load,
      pending_wal: Vec::new(),
      replication_apply: false,
      writer: None,
      mvcc_reads: TxKeySet::new(),
      mvcc_writes: TxKeySet::new(),
      mvcc_slot: None,
      savepoints: Vec::new(),
      next_savepoint_id: 0,
      wal_deferred_from: (!read_only && !bulk_load).then_some(0),
      wal_begun: false,
    }
  }

  /// Whether the transaction's reads go to its MVCC conflict check. A
  /// read-only transaction never checks, and a bulk load has no other write
  /// transaction beside it to conflict with, so they note nothing.
  pub(crate) fn tracks_reads(&self) -> bool {
    !self.read_only && !self.bulk_load
  }

  /// Note that the transaction read `key`, for its MVCC conflict check at
  /// commit (see `tracks_reads`).
  pub(crate) fn record_read(&mut self, key: TxKey) {
    if self.tracks_reads() {
      self.mvcc_reads.insert(key);
    }
  }
}

// ============================================================================
// Single-File Database
// ============================================================================

/// Single-file database handle
pub struct SingleFileDB {
  /// Database file path
  pub(crate) path: PathBuf,
  /// Read-only mode
  pub(crate) read_only: bool,
  /// Set once `close_single_file` has persisted everything, so dropping the
  /// handle afterwards writes nothing.
  pub(crate) closed: AtomicBool,
  /// Page-based I/O
  pub(crate) pager: Mutex<FilePager>,
  /// Database header
  pub(crate) header: RwLock<DbHeaderV1>,
  /// Physical header slot containing the newest installed header.
  pub(crate) header_slot: AtomicU32,
  /// WAL buffer manager
  pub(crate) wal_buffer: Mutex<WalBuffer>,
  /// Memory-mapped snapshot data (if exists)
  pub(crate) snapshot: CacheAligned<RwLock<CacheAligned<Option<SnapshotData>>>>,
  /// Delta state (uncommitted changes)
  pub(crate) delta: CacheAligned<RwLock<CacheAligned<DeltaState>>>,

  // ID allocators
  pub(crate) next_node_id: AtomicU64,
  pub(crate) next_label_id: AtomicU32,
  pub(crate) next_etype_id: AtomicU32,
  pub(crate) next_propkey_id: AtomicU32,
  pub(crate) next_tx_id: AtomicU64,

  /// Shared with the thread-local entries of this database's transactions
  /// (see `tx_registry`): each thread keeps its own open transaction.
  pub(crate) tx_shared: std::sync::Arc<tx_registry::TxShared>,
  /// All transactions that have begun and have not finished commit/rollback.
  pub(crate) active_transactions: AtomicUsize,
  /// Write transactions that wrote a BEGIN record and have not finished
  /// commit/rollback. A background checkpoint cut copies the WAL records of
  /// those still unterminated into the secondary region.
  pub(crate) open_write_txids: Mutex<HashSet<TxId>>,

  /// Read permits cover transaction creation; the blocking checkpoint takes
  /// the write side for its complete snapshot/header critical section, and a
  /// background checkpoint takes it to establish its cut and to install.
  pub(crate) checkpoint_gate: RwLock<()>,
  /// Paired with `checkpoint_cv`. Notifiers take it before notifying, so a
  /// waiter that checked its condition under it cannot miss the wakeup.
  pub(crate) checkpoint_wait: Mutex<()>,
  /// Signaled when the last open transaction finishes and when a background
  /// checkpoint returns to idle.
  pub(crate) checkpoint_cv: Condvar,
  /// Paired with `cut_cv`, like `checkpoint_wait` with `checkpoint_cv`.
  pub(crate) cut_wait: Mutex<()>,
  /// Signaled when a background checkpoint's cut is installed or released;
  /// writers waiting for that in `wait_for_cut_release` park here.
  pub(crate) cut_cv: Condvar,

  /// Serializes writing commits: a commit group's checks, WAL records,
  /// header and replication frames (see `transaction::write_commit_round`).
  /// A group publishes (merges into the delta) after releasing it, under
  /// `publish_lock`; take both with `lock_commits` to hold off commits.
  pub(crate) commit_lock: Mutex<()>,
  /// Serializes publishing commit groups, in the order they took the commit
  /// lock (a group takes this before it releases that).
  pub(crate) publish_lock: Mutex<()>,
  /// Odd while a commit group publishes (gives its members MVCC timestamps
  /// and merges them, under `delta.write()`): an MVCC begin that sees it
  /// change begins again (see `begin_with_mode`).
  pub(crate) publish_seq: AtomicU64,

  /// Commits waiting to be written: one committer at a time leads, and
  /// writes everything queued as one group (one WAL write and one header)
  pub(crate) commit_queue: CommitQueue,

  /// MVCC manager (if enabled)
  pub(crate) mvcc: Option<std::sync::Arc<MvccManager>>,

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
  /// Pending name claims; never persisted in a snapshot or WAL.
  pub(crate) schema_reservations: Mutex<SchemaReservations>,

  /// Enable auto-checkpoint when WAL usage exceeds threshold
  pub(crate) auto_checkpoint: bool,
  /// WAL usage threshold (0.0-1.0) to trigger auto-checkpoint
  pub(crate) checkpoint_threshold: f64,
  /// Use background (non-blocking) checkpoint instead of blocking
  pub(crate) background_checkpoint: bool,
  /// Which background checkpoint runs, and which one owns the current cut
  pub(crate) checkpoint_state: Mutex<BackgroundCheckpointState>,
  /// Bumped as a background checkpoint makes progress, so writers waiting
  /// for its install can tell a slow checkpoint from a stalled one.
  pub(crate) checkpoint_progress: AtomicU64,
  /// Checkpoint steps running that note no progress until they end (one
  /// serialization, parse, or fsync of a whole snapshot, which takes seconds
  /// on a large database). Writers waiting for an install treat a checkpoint
  /// inside one as working, however long it takes.
  pub(crate) checkpoint_steps_running: AtomicUsize,
  /// Set when writers cancel the running background checkpoint's cut, until
  /// that run ends: it stops at its next progress point instead of finishing
  /// a snapshot no header will name while it holds the checkpoint status.
  pub(crate) checkpoint_cancelled: AtomicBool,

  /// Vector stores keyed by property key ID
  /// Each property key can have its own vector store with different dimensions
  pub(crate) vector_stores: RwLock<HashMap<PropKeyId, VectorManifest>>,
  /// Lazy vector-store section index keyed by property key ID
  pub(crate) vector_store_lazy_entries: RwLock<HashMap<PropKeyId, VectorStoreLazyEntry>>,

  /// Compression options for checkpoint snapshots
  pub(crate) checkpoint_compression: Option<CompressionOptions>,

  /// Synchronization mode for WAL writes
  pub(crate) sync_mode: open::SyncMode,

  /// Primary replication runtime (enabled only when role=primary)
  pub(crate) primary_replication: Option<crate::replication::primary::PrimaryReplication>,
  /// Replica replication runtime (enabled only when role=replica)
  pub(crate) replica_replication: Option<crate::replication::replica::ReplicaReplication>,

  /// Committers whose commit is handed over and not written yet (test
  /// instrumentation).
  #[cfg(test)]
  pub(crate) commits_waiting: AtomicUsize,
  #[cfg(feature = "bench-profile")]
  pub(crate) commit_lock_wait_ns: AtomicU64,
  #[cfg(feature = "bench-profile")]
  pub(crate) wal_flush_ns: AtomicU64,
}

/// A value on cache lines of its own (128 bytes, Apple silicon's line; two
/// of x86-64's). Every read takes the delta's and the snapshot's read locks,
/// and each reader writes the lock word: the locks are wrapped in this, and
/// so is the data they guard (`RwLock<CacheAligned<T>>`, as parking_lot keeps
/// the data right after the lock word), so readers never miss the data, or
/// a neighboring field, because another reader took the lock.
#[repr(align(128))]
#[derive(Debug, Default)]
pub(crate) struct CacheAligned<T>(pub(crate) T);

impl<T> std::ops::Deref for CacheAligned<T> {
  type Target = T;
  fn deref(&self) -> &T {
    &self.0
  }
}

impl<T> std::ops::DerefMut for CacheAligned<T> {
  fn deref_mut(&mut self) -> &mut T {
    &mut self.0
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

/// Ownership of background checkpoints.
///
/// One background checkpoint runs at a time. A run claims `status` under
/// this lock and only that run (identified by `run`) returns it to idle, once,
/// when it ends, so a finished run can never reset a later run's status.
#[derive(Debug)]
pub(crate) struct BackgroundCheckpointState {
  pub(crate) status: CheckpointStatus,
  /// The latest run to claim `status`.
  pub(crate) run: u64,
  /// The run whose cut is durable but neither installed nor released. It is
  /// set by that run's cut under the checkpoint gate and the commit lock, and
  /// cleared under the commit lock (and the WAL lock) by its install, by its
  /// release of the cut after a failure, or by a writer cancelling it as
  /// stalled; or, last resort, when the run ends. Writers that find the
  /// secondary region full wait only while some run owns the cut.
  pub(crate) cut_owner: Option<u64>,
  /// Counts cuts taken. Writers wait for one cut by its number, so a run's
  /// next pass cannot keep them waiting after the cut they hit is installed.
  pub(crate) cut: u64,
  /// A writer waited for the current cut's install, so its run takes another
  /// pass once it installs.
  pub(crate) writers_waited: bool,
  /// Open transactions whose records did not fit in the secondary region at
  /// the last declined cut. A cut is not retried before one of them
  /// finishes: until then the copies only grow.
  pub(crate) declined_carry: Option<HashSet<TxId>>,
  /// Blocking checkpoints, optimizes, vacuums and WAL resizes waiting in
  /// `exclusive_checkpoint_gate`. While any wait, new background checkpoints
  /// decline instead of claiming `status` ahead of them: a waiter that finds
  /// a run in progress whenever it gets the gate would wait forever behind a
  /// background checkpoint loop.
  pub(crate) exclusive_waiters: usize,
}

impl Default for BackgroundCheckpointState {
  fn default() -> Self {
    Self {
      status: CheckpointStatus::Idle,
      run: 0,
      cut_owner: None,
      cut: 0,
      writers_waited: false,
      declined_carry: None,
      exclusive_waiters: 0,
    }
  }
}

impl BackgroundCheckpointState {
  /// Whether cut number `cut` is still durable and neither installed nor
  /// released.
  pub(crate) fn holds_cut(&self, cut: u64) -> bool {
    self.cut_owner.is_some() && self.cut == cut
  }
}

/// A database dropped without `close_single_file` still persists what close
/// would, best effort: without it, `SyncMode::Off` loses every commit since
/// the last checkpoint. It never panics; a failure is reported on stderr.
/// After a successful close it does nothing.
impl Drop for SingleFileDB {
  fn drop(&mut self) {
    if self.read_only || self.closed.load(Ordering::Acquire) {
      return;
    }
    let persisted =
      std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.persist_for_close()));
    let failure = match persisted {
      Ok(Ok(())) => return,
      Ok(Err(error)) => error.to_string(),
      Err(_) => "it panicked".to_string(),
    };
    eprintln!(
      "Warning: {} was dropped without close, and persisting its commits failed: {failure}",
      self.path.display()
    );
  }
}

// ============================================================================
// SingleFileDB Implementation - ID Allocators
// ============================================================================

impl SingleFileDB {
  /// Install a header in the inactive slot, then optionally make that slot
  /// durable. The old slot remains untouched until this method succeeds.
  pub(crate) fn persist_header(
    &self,
    pager: &mut FilePager,
    header: &mut DbHeaderV1,
    sync: bool,
  ) -> Result<()> {
    let current_slot = self.header_slot.load(Ordering::Acquire);
    let next_slot = other_header_slot(current_slot);
    header.change_counter = header
      .change_counter
      .checked_add(1)
      .ok_or_else(|| crate::error::KiteError::Internal("header generation overflow".to_string()))?;

    write_header_slot(pager, header, next_slot)?;
    if sync {
      // A header slot lies inside the file, so a data sync makes it durable
      // (a full sync if the file's length changed since the last one).
      pager.sync_data()?;
    }
    self.header_slot.store(next_slot, Ordering::Release);
    Ok(())
  }

  /// Flush the WAL buffer, install a header naming every commit, and sync:
  /// what closing persists. In `SyncMode::Off` nothing else writes commits
  /// since the last checkpoint to disk.
  pub(crate) fn persist_for_close(&self) -> Result<()> {
    let mut pager = self.pager.lock();
    let mut wal_buffer = self.wal_buffer.lock();
    // A failed commit's records may still be readable on disk; the header
    // below must not name bytes past them before their overwrite is durable.
    if wal_buffer.needs_sync() {
      wal_buffer.sync(&mut pager)?;
    } else {
      wal_buffer.flush(&mut pager)?;
    }
    {
      let mut header = self.header.write();
      wal_buffer.store_in_header(&mut header);
      header.max_node_id = self.next_node_id.load(Ordering::SeqCst).saturating_sub(1);
      header.next_tx_id = self.next_tx_id.load(Ordering::SeqCst);

      // Install the updated header in the inactive slot. The sync below makes
      // the WAL and header durable together.
      self.persist_header(&mut pager, &mut header, false)?;
    }
    pager.sync()
  }

  pub(crate) fn transaction_finished(&self, txid: TxId, wrote_begin: bool) {
    if wrote_begin {
      self.open_write_txids.lock().remove(&txid);
    }
    let previous = self.active_transactions.fetch_sub(1, Ordering::AcqRel);
    debug_assert!(previous > 0, "active transaction count underflow");
    if previous == 1 {
      self.notify_checkpoint_waiters();
    }
  }

  /// Wake threads in `wait_for_no_active_transactions` or
  /// `wait_for_background_checkpoint`. Taking the wait mutex orders this
  /// notification after a waiter's condition check, so it cannot be lost
  /// between that check and the waiter parking.
  pub(crate) fn notify_checkpoint_waiters(&self) {
    let _wait = self.checkpoint_wait.lock();
    self.checkpoint_cv.notify_all();
  }

  /// Wake writers in `wait_for_cut_release`, the same way.
  pub(crate) fn notify_cut_waiters(&self) {
    let _wait = self.cut_wait.lock();
    self.cut_cv.notify_all();
  }

  /// Database file path
  pub fn path(&self) -> &Path {
    &self.path
  }

  /// Read-only mode
  pub fn is_read_only(&self) -> bool {
    self.read_only
  }

  /// Allocate a new node ID. Fails once every ID up to [`MAX_NODE_ID`] is taken.
  // `fetch_update` is renamed `try_update` in newer Rust; keep the old name so
  // older toolchains still build.
  #[allow(deprecated)]
  pub fn alloc_node_id(&self) -> Result<NodeId> {
    self
      .next_node_id
      .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |next| {
        (next <= MAX_NODE_ID).then(|| next + 1)
      })
      .map_err(|_| {
        crate::error::KiteError::Internal(format!("node ID space exhausted (max {MAX_NODE_ID})"))
      })
  }

  /// Ensure the next node ID is greater than the provided value
  /// (callers keep `node_id <= MAX_NODE_ID`).
  pub fn reserve_node_id(&self, node_id: NodeId) {
    let desired = node_id.saturating_add(1);
    loop {
      let current = self.next_node_id.load(Ordering::SeqCst);
      if current >= desired {
        break;
      }
      if self
        .next_node_id
        .compare_exchange(current, desired, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
      {
        break;
      }
    }
  }

  /// Claim a label name for a transaction, sharing an existing in-flight ID.
  pub(crate) fn claim_label_reservation(&self, name: &str, txid: TxId) -> LabelId {
    let mut reservations = self.schema_reservations.lock();
    if let Some(id) = self.label_names.read().get(name).copied() {
      reservations.labels.remove(name);
      return id;
    }
    if let Some(reservation) = reservations.labels.get_mut(name) {
      reservation.owners.insert(txid);
      return reservation.id;
    }

    let id = self.alloc_unclaimed_label_id();
    reservations
      .labels
      .insert(name.to_string(), SchemaReservation::new(id, txid));
    id
  }

  /// Claim an edge type name for a transaction, sharing an existing in-flight ID.
  pub(crate) fn claim_etype_reservation(&self, name: &str, txid: TxId) -> ETypeId {
    let mut reservations = self.schema_reservations.lock();
    if let Some(id) = self.etype_names.read().get(name).copied() {
      reservations.etypes.remove(name);
      return id;
    }
    if let Some(reservation) = reservations.etypes.get_mut(name) {
      reservation.owners.insert(txid);
      return reservation.id;
    }

    let id = self.alloc_unclaimed_etype_id();
    reservations
      .etypes
      .insert(name.to_string(), SchemaReservation::new(id, txid));
    id
  }

  /// Claim a property key name for a transaction, sharing an existing in-flight ID.
  pub(crate) fn claim_propkey_reservation(&self, name: &str, txid: TxId) -> PropKeyId {
    let mut reservations = self.schema_reservations.lock();
    if let Some(id) = self.propkey_names.read().get(name).copied() {
      reservations.propkeys.remove(name);
      return id;
    }
    if let Some(reservation) = reservations.propkeys.get_mut(name) {
      reservation.owners.insert(txid);
      return reservation.id;
    }

    let id = self.alloc_unclaimed_propkey_id();
    reservations
      .propkeys
      .insert(name.to_string(), SchemaReservation::new(id, txid));
    id
  }

  pub(crate) fn release_label_reservation(&self, name: &str, txid: TxId) {
    release_schema_reservation(&mut self.schema_reservations.lock().labels, name, txid);
  }

  pub(crate) fn release_etype_reservation(&self, name: &str, txid: TxId) {
    release_schema_reservation(&mut self.schema_reservations.lock().etypes, name, txid);
  }

  pub(crate) fn release_propkey_reservation(&self, name: &str, txid: TxId) {
    release_schema_reservation(&mut self.schema_reservations.lock().propkeys, name, txid);
  }

  /// Release every claim owned by a transaction that did not commit.
  pub(crate) fn release_schema_reservations(&self, txid: TxId) {
    let mut reservations = self.schema_reservations.lock();
    release_schema_reservation_owner(&mut reservations.labels, txid);
    release_schema_reservation_owner(&mut reservations.etypes, txid);
    release_schema_reservation_owner(&mut reservations.propkeys, txid);
  }

  /// Publish staged schema after the transaction's durable commit and data
  /// delta merge. The caller holds `commit_lock`, which gives schema and data
  /// one serialized commit order.
  pub(crate) fn publish_staged_schema(&self, staged: &SchemaStaging) -> Result<()> {
    // Most commits define nothing: skip the seven locks every reader of a
    // name or id takes.
    if staged.is_empty() {
      return Ok(());
    }
    let mut reservations = self.schema_reservations.lock();
    let mut label_names = self.label_names.write();
    let mut label_ids = self.label_ids.write();
    let mut etype_names = self.etype_names.write();
    let mut etype_ids = self.etype_ids.write();
    let mut propkey_names = self.propkey_names.write();
    let mut propkey_ids = self.propkey_ids.write();

    publish_schema_entries(
      &mut reservations.labels,
      &mut label_names,
      &mut label_ids,
      &staged.label_names,
      "label",
    )?;
    publish_schema_entries(
      &mut reservations.etypes,
      &mut etype_names,
      &mut etype_ids,
      &staged.etype_names,
      "edge type",
    )?;
    publish_schema_entries(
      &mut reservations.propkeys,
      &mut propkey_names,
      &mut propkey_ids,
      &staged.propkey_names,
      "property key",
    )?;
    Ok(())
  }

  fn alloc_unclaimed_label_id(&self) -> LabelId {
    loop {
      let id = self.alloc_label_id();
      if !self.label_ids.read().contains_key(&id) {
        return id;
      }
    }
  }

  fn alloc_unclaimed_etype_id(&self) -> ETypeId {
    loop {
      let id = self.alloc_etype_id();
      if !self.etype_ids.read().contains_key(&id) {
        return id;
      }
    }
  }

  fn alloc_unclaimed_propkey_id(&self) -> PropKeyId {
    loop {
      let id = self.alloc_propkey_id();
      if !self.propkey_ids.read().contains_key(&id) {
        return id;
      }
    }
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

  /// Where commits' time went, stage by stage, since the last reset.
  #[cfg(feature = "bench-profile")]
  pub fn commit_profile_report() -> String {
    commit_profile::report()
  }

  /// Clear the commit profile's totals.
  #[cfg(feature = "bench-profile")]
  pub fn commit_profile_reset() {
    commit_profile::reset()
  }

  #[cfg(feature = "bench-profile")]
  pub fn take_profile_snapshot(&self) -> (u64, u64) {
    (
      self.commit_lock_wait_ns.swap(0, Ordering::Relaxed),
      self.wal_flush_ns.swap(0, Ordering::Relaxed),
    )
  }

  /// Check if a node exists
  pub fn node_exists(&self, node_id: NodeId) -> bool {
    let tx_handle = self.current_tx_handle();
    if let Some(handle) = tx_handle.as_ref() {
      let tx = handle.lock();
      if tx.pending.is_node_created(node_id) {
        return true;
      }
      if tx.pending.is_node_deleted(node_id) {
        return false;
      }
    }

    // Read-locked across the MVCC lookup: a commit lands completely before or after this
    // read (lock order: see read.rs).
    let delta = self.delta.read();

    let (txid, tx_snapshot_ts) = match tx_handle.as_ref() {
      Some(handle) => {
        let mut tx = handle.lock();
        self.record_reads(Some(&mut tx), [TxKey::Node(node_id)]);
        self.mvcc_read_ts(Some(&tx))
      }
      None => self.mvcc_read_ts(None),
    };
    if let Some(vc) = self.mvcc_history(tx_snapshot_ts) {
      if let Some(exists) = vc.node_exists_at(node_id, tx_snapshot_ts, txid) {
        return exists;
      }
    }

    let snapshot = self.snapshot.read();
    delta.node_exists_over(snapshot.as_ref(), node_id)
  }

  /// Check if an edge exists
  pub fn edge_exists(&self, src: NodeId, etype: ETypeId, dst: NodeId) -> bool {
    let tx_handle = self.current_tx_handle();
    if let Some(handle) = tx_handle.as_ref() {
      let tx = handle.lock();
      if tx.pending.is_node_removed(src) || tx.pending.is_node_removed(dst) {
        return false;
      }
      if tx.pending.is_edge_deleted(src, etype, dst) {
        return false;
      }
      if tx.pending.is_edge_added(src, etype, dst) {
        return true;
      }
      // A node this transaction deleted or recreated masks its committed edges.
      if tx.pending.is_node_deleted(src) || tx.pending.is_node_deleted(dst) {
        return false;
      }
    }

    // Read-locked across the MVCC lookup: a commit lands completely before or after this
    // read (lock order: see read.rs).
    let delta = self.delta.read();

    let (txid, tx_snapshot_ts) = match tx_handle.as_ref() {
      Some(handle) => {
        let mut tx = handle.lock();
        self.record_reads(Some(&mut tx), [TxKey::Edge { src, etype, dst }]);
        self.mvcc_read_ts(Some(&tx))
      }
      None => self.mvcc_read_ts(None),
    };
    if let Some(vc) = self.mvcc_history(tx_snapshot_ts) {
      // An endpoint created after the reader's snapshot hides the edge: its
      // commit records no history for the edge (see `mvcc_history`).
      let endpoint_gone = [src, dst]
        .into_iter()
        .any(|node_id| vc.node_exists_at(node_id, tx_snapshot_ts, txid) == Some(false));
      if endpoint_gone {
        return false;
      }
      if let Some(exists) = vc.edge_exists_at(src, etype, dst, tx_snapshot_ts, txid) {
        return exists;
      }
    }

    let snapshot = self.snapshot.read();
    delta.edge_exists_over(snapshot.as_ref(), src, etype, dst)
  }

  /// Check if MVCC is enabled
  pub fn mvcc_enabled(&self) -> bool {
    self.mvcc.is_some()
  }
}

fn release_schema_reservation<Id>(
  reservations: &mut HashMap<String, SchemaReservation<Id>>,
  name: &str,
  txid: TxId,
) {
  let remove = reservations
    .get_mut(name)
    .map(|reservation| {
      reservation.owners.remove(&txid);
      reservation.owners.is_empty()
    })
    .unwrap_or(false);
  if remove {
    reservations.remove(name);
  }
}

fn release_schema_reservation_owner<Id>(
  reservations: &mut HashMap<String, SchemaReservation<Id>>,
  txid: TxId,
) {
  reservations.retain(|_, reservation| {
    reservation.owners.remove(&txid);
    !reservation.owners.is_empty()
  });
}

fn publish_schema_entries<Id>(
  reservations: &mut HashMap<String, SchemaReservation<Id>>,
  global_names: &mut HashMap<String, Id>,
  global_ids: &mut HashMap<Id, String>,
  staged: &HashMap<String, Id>,
  kind: &str,
) -> Result<()>
where
  Id: Copy + Eq + Hash,
{
  // Validate the whole batch before mutating either global map. The normal
  // path has one allocator-owned ID per name; these checks make a corrupted
  // reservation or an allocator regression fail closed instead of replacing
  // an existing mapping.
  let mut staged_ids = HashMap::with_capacity(staged.len());
  for (name, &id) in staged {
    if let Some(reservation) = reservations.get(name) {
      if reservation.id != id {
        return Err(crate::error::KiteError::Internal(format!(
          "{kind} reservation ID changed before commit"
        )));
      }
    }
    if let Some(existing) = global_names.get(name) {
      if *existing != id {
        return Err(crate::error::KiteError::Internal(format!(
          "{kind} name maps to two IDs during commit"
        )));
      }
    }
    if let Some(existing) = global_ids.get(&id) {
      if existing != name {
        return Err(crate::error::KiteError::Internal(format!(
          "{kind} ID maps to two names during commit"
        )));
      }
    }
    if let Some(existing) = staged_ids.insert(id, name) {
      if existing != name {
        return Err(crate::error::KiteError::Internal(format!(
          "{kind} transaction stages one ID for two names"
        )));
      }
    }
  }

  for (name, &id) in staged {
    global_names.entry(name.clone()).or_insert(id);
    global_ids.entry(id).or_insert_with(|| name.clone());
    // Once the mapping is committed, all transactions that shared this claim
    // can use the committed entry; no pending reservation must survive it.
    reservations.remove(name);
  }
  Ok(())
}

// ============================================================================
// Utility Functions
// ============================================================================

/// Check if a path is a single-file database
pub fn is_single_file_path<P: AsRef<Path>>(path: P) -> bool {
  path
    .as_ref()
    .extension()
    .map(|ext| ext == "kitedb")
    .unwrap_or(false)
}

/// Get the single-file extension
pub fn single_file_extension() -> &'static str {
  EXT_KITEDB
}

/// raydb-b4 `pipeline` lane: the read locks' cache lines.
#[cfg(test)]
#[path = "b4_pipeline_tests.rs"]
mod b4_pipeline_tests;
