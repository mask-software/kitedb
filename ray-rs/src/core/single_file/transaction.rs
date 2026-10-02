//! Transaction management for SingleFileDB
//!
//! Handles begin, commit, and rollback operations.
//!
//! Commit ordering is:
//! `epoch fence check (primaries) -> room for the COMMIT record (waiting for
//! a background install if needed) -> MVCC conflict check -> WAL COMMIT ->
//! WAL flush (and fsync, in Full mode) -> durable header -> schema publish
//! -> MVCC commit timestamp, version chains, vector and delta merge ->
//! sidecar attempt`, all under the commit lock. Until the header is durable
//! a failure leaves no trace of the commit: its COMMIT record is forgotten,
//! and MVCC aborts it. From there on
//! every step runs. The MVCC timestamp, version chains and delta merge share
//! one `delta.write()` critical section, and MVCC transactions begin under
//! `delta.read()`, so a snapshot holds a commit entirely or not at all.
//!
//! With group commit, commits queue and one committer at a time (the leader)
//! runs these steps for everything queued: one WAL flush and one header for
//! the batch, then each commit's publish, in order.
//!
//! The sidecar attempt is deliberately non-authoritative after the local
//! durability boundary: an error records primary replication lag and fences
//! future sidecar appends, while this commit still completes locally and
//! returns success.

use crate::core::wal::record::{
  build_begin_payload, build_commit_payload, build_rollback_payload, WalRecord,
};
use crate::error::{KiteError, Result};
use crate::mvcc::TxKeyGroups;
use crate::replication::primary::PrimaryReplicationStatus;
use crate::replication::types::CommitToken;
use crate::types::*;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet, VecDeque};
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::ThreadId;
#[cfg(feature = "bench-profile")]
use std::time::Instant;

use super::open::SyncMode;
use super::{SchemaStaging, SingleFileDB, SingleFileTxState};
use crate::core::pager::FilePager;
use crate::core::wal::buffer::{WalBuffer, WalRegionState};

#[cfg(test)]
thread_local! {
  /// Fail this thread's next commit right after its durable point, the way a
  /// failing vector apply does.
  static FAIL_NEXT_COMMIT_AFTER_DURABLE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn post_durable_test_fault() -> Result<()> {
  #[cfg(test)]
  if FAIL_NEXT_COMMIT_AFTER_DURABLE.with(|fail| fail.replace(false)) {
    return Err(KiteError::Internal(
      "injected failure after the commit's durable point".to_string(),
    ));
  }
  Ok(())
}

#[cfg(test)]
thread_local! {
  /// Run on this thread's next commit once it is durable, right before its
  /// changes merge into the delta (wave-2 D3 reproduction).
  pub(crate) static BEFORE_NEXT_COMMIT_MERGE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
    std::cell::RefCell::new(None);
}

#[cfg(test)]
thread_local! {
  /// Run on this thread's next commit right before it takes the commit lock.
  pub(crate) static BEFORE_NEXT_COMMIT_LOCK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
    std::cell::RefCell::new(None);
}

fn before_commit_lock_test_hook() {
  #[cfg(test)]
  if let Some(hook) = BEFORE_NEXT_COMMIT_LOCK.with(|hook| hook.borrow_mut().take()) {
    hook();
  }
}

fn before_merge_test_hook() {
  #[cfg(test)]
  if let Some(hook) = BEFORE_NEXT_COMMIT_MERGE.with(|hook| hook.borrow_mut().take()) {
    hook();
  }
}

#[cfg(test)]
thread_local! {
  /// Run on this thread's next commit right after MVCC gives it its commit
  /// timestamp, before its version chains and delta merge.
  static AFTER_NEXT_COMMIT_TIMESTAMP: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
    std::cell::RefCell::new(None);
}

fn after_commit_timestamp_test_hook() {
  #[cfg(test)]
  if let Some(hook) = AFTER_NEXT_COMMIT_TIMESTAMP.with(|hook| hook.borrow_mut().take()) {
    hook();
  }
}

/// A transaction's commit, as handed to the thread that writes it: its
/// committer, or the group-commit leader.
pub(crate) struct CommitRequest {
  txid: TxId,
  bulk_load: bool,
  /// Its COMMIT record, or a bulk load's whole transaction.
  records: Vec<u8>,
  pending: DeltaState,
  /// Its data records, for the replication sidecar (empty without one).
  pending_wal: Vec<u8>,
  staged_schema: SchemaStaging,
  /// The committing thread; its test hooks fire only there.
  committer: ThreadId,
}

/// How far a commit got, so its committer can settle its guards.
pub(crate) struct CommitOutcome {
  /// The header naming its COMMIT record is durable, so it is committed in
  /// MVCC and merged, whatever `result` says.
  durable: bool,
  /// Its staged schema names are published.
  schema_published: bool,
  result: Result<Option<CommitToken>>,
}

impl CommitOutcome {
  fn failed(error: KiteError) -> Self {
    Self {
      durable: false,
      schema_published: false,
      result: Err(error),
    }
  }
}

/// Group-commit queue. Commits queue here; the first committer to find no
/// leader becomes it, takes everything queued, and writes it as one batch.
#[derive(Default)]
pub(crate) struct GroupCommitState {
  queue: VecDeque<(u64, CommitRequest)>,
  /// Outcomes of written commits, by ticket, until their committers take them.
  outcomes: HashMap<u64, CommitOutcome>,
  next_ticket: u64,
  leader_active: bool,
}

/// The lead of a group commit: once dropped, its batch's outcomes are
/// delivered and the next leader may start. A leader that unwinds fails the
/// commits it has no outcome for, so their committers stop waiting.
struct GroupCommitLeader<'db> {
  db: &'db SingleFileDB,
  tickets: Vec<u64>,
  /// Outcomes of the batch, in ticket order.
  outcomes: Vec<CommitOutcome>,
}

impl Drop for GroupCommitLeader<'_> {
  fn drop(&mut self) {
    let mut state = self.db.group_commit_state.lock();
    let mut outcomes = std::mem::take(&mut self.outcomes).into_iter();
    for &ticket in &self.tickets {
      let outcome = outcomes.next().unwrap_or_else(|| {
        CommitOutcome::failed(KiteError::Internal(
          "the group commit writing this commit panicked".to_string(),
        ))
      });
      state.outcomes.insert(ticket, outcome);
    }
    state.leader_active = false;
    self.db.group_commit_cv.notify_all();
  }
}

/// One round of `SingleFileDB::write_commits`.
#[derive(Default)]
struct CommitRound {
  /// Requests whose commits are durable, in WAL order, with their indexes.
  durable: Vec<(usize, CommitRequest)>,
  /// The next request found the WAL full until this background checkpoint
  /// cut is installed or released.
  wait_for_cut: Option<u64>,
}

/// What the commits written earlier in a round claim, which the ones after
/// them must agree with.
#[derive(Default)]
struct RoundClaims {
  /// Dimensions given to vector properties that have no store yet.
  vector_dimensions: HashMap<PropKeyId, usize>,
  /// MVCC keys written: a later commit of the round that read or wrote one
  /// conflicts, as it would once the earlier one had committed.
  mvcc_writes: TxKeySet,
}

/// A copy of `error` for every further commit of a round it failed.
fn round_error(error: &KiteError) -> KiteError {
  match error {
    KiteError::Io(io) => KiteError::Io(std::io::Error::new(io.kind(), io.to_string())),
    other => KiteError::Internal(other.to_string()),
  }
}

/// Outcome of `SingleFileDB::try_write_wal`.
pub(crate) enum WalWrite<T> {
  Written(T),
  /// The WAL refused the record until background checkpoint cut `.0` is
  /// installed or released.
  BlockedOn(u64),
  /// The WAL refused the record because it lives in the secondary region
  /// after a background install whose compaction failed, with the primary
  /// region empty; `compact_retired_wal` makes room.
  NeedsCompaction,
}

/// Marks a transaction finished once commit or rollback is done with it,
/// including on error paths. A successful commit drops it only after its
/// COMMIT record is written.
struct ActiveTransactionGuard<'db> {
  db: &'db SingleFileDB,
  txid: TxId,
  /// The transaction wrote a BEGIN record (a non-bulk write transaction).
  wrote_begin: bool,
  /// It holds non-MVCC mode's writer slot, released here.
  holds_writer: bool,
}

impl Drop for ActiveTransactionGuard<'_> {
  fn drop(&mut self) {
    self.db.transaction_finished(self.txid, self.wrote_begin);
    if self.holds_writer {
      self.db.tx_shared.writer.release();
    }
  }
}

/// Aborts a transaction in MVCC when its commit fails before MVCC commits it.
struct MvccAbortGuard<'db> {
  db: &'db SingleFileDB,
  txid: TxId,
  armed: bool,
}

impl Drop for MvccAbortGuard<'_> {
  fn drop(&mut self) {
    if let (true, Some(mvcc)) = (self.armed, self.db.mvcc.as_ref()) {
      mvcc.tx_manager.lock().abort_tx(self.txid);
    }
  }
}

struct SchemaReservationGuard<'db> {
  db: &'db SingleFileDB,
  txid: TxId,
  active: bool,
}

impl<'db> SchemaReservationGuard<'db> {
  fn new(db: &'db SingleFileDB, txid: TxId) -> Self {
    Self {
      db,
      txid,
      active: true,
    }
  }

  fn disarm(&mut self) {
    self.active = false;
  }
}

impl Drop for SchemaReservationGuard<'_> {
  fn drop(&mut self) {
    if self.active {
      self.db.release_schema_reservations(self.txid);
    }
  }
}

/// RAII transaction guard for SingleFileDB.
/// Rolls back the transaction on drop unless committed or rolled back.
pub struct SingleFileTxGuard<'db> {
  db: &'db SingleFileDB,
  txid: TxId,
  active: bool,
  _nosend: PhantomData<Rc<()>>,
}

impl<'db> SingleFileTxGuard<'db> {
  fn new(db: &'db SingleFileDB, txid: TxId) -> Self {
    Self {
      db,
      txid,
      active: true,
      _nosend: PhantomData,
    }
  }

  pub fn txid(&self) -> TxId {
    self.txid
  }

  pub fn commit(mut self) -> Result<()> {
    self.active = false;
    self.db.commit()
  }

  pub fn rollback(mut self) -> Result<()> {
    self.active = false;
    self.db.rollback()
  }
}

impl Drop for SingleFileTxGuard<'_> {
  fn drop(&mut self) {
    if !self.active {
      return;
    }
    self.active = false;
    if self.db.current_txid() != Some(self.txid) {
      return;
    }
    let _ = self.db.rollback();
  }
}

impl SingleFileDB {
  fn begin_with_mode(&self, read_only: bool, bulk_load: bool) -> Result<TxId> {
    if self.read_only && !read_only {
      return Err(KiteError::ReadOnly);
    }
    if bulk_load && read_only {
      return Err(KiteError::ReadOnly);
    }
    if bulk_load && self.mvcc.is_some() {
      return Err(KiteError::Internal(
        "bulk load requires MVCC disabled".to_string(),
      ));
    }

    // Only this thread registers its own transaction, so checking before the
    // gate is race-free. It must come first: a blocking checkpoint holding
    // the gate may be waiting for this thread's open transaction.
    if self.current_tx_handle().is_some() {
      return Err(KiteError::TransactionInProgress);
    }
    self.reap_abandoned_transactions();
    // Without MVCC, write transactions run one at a time (see
    // `writer_slot`). Taken before the checkpoint gate, holding nothing; a
    // failed begin releases it.
    let writer_claim = (self.mvcc.is_none() && !read_only).then(|| self.tx_shared.writer.claim());

    // A checkpoint takes the write side. Holding this read permit through
    // insertion makes the gate atomic with transaction creation.
    let mut checkpointed_for_room = false;
    let (_checkpoint_gate, txid, snapshot_ts) = loop {
      let checkpoint_gate = self.checkpoint_gate.read();
      let (txid, snapshot_ts) = if let Some(mvcc) = self.mvcc.as_ref() {
        let (txid, snapshot_ts) = {
          // A commit takes its timestamp, adds its version chains and
          // merges into the delta under `delta.write()` (see
          // `publish_commit`), so this snapshot holds it entirely or not at
          // all, and a commit that has not taken its timestamp yet sees this
          // transaction as a reader that needs version chains.
          let _delta = self.delta.read();
          let mut tx_mgr = mvcc.tx_manager.lock();
          tx_mgr.begin_tx()
        };
        // Only ever raise it: a concurrent begin that took a later txid may
        // have stored already, and the header persists this value, so a lower
        // one would issue a used txid again after reopen.
        self
          .next_tx_id
          .fetch_max(txid.saturating_add(1), Ordering::SeqCst);
        (txid, snapshot_ts)
      } else {
        (self.alloc_tx_id(), 0)
      };

      // Write BEGIN record to WAL (for write transactions). Bulk loads write
      // all their records at commit instead.
      if read_only || bulk_load {
        break (checkpoint_gate, txid, snapshot_ts);
      }
      let record = WalRecord::new(WalRecordType::Begin, txid, build_begin_payload());
      let written = self.try_write_wal(|wal, pager| wal.write_record(&record, pager));
      match written {
        Ok(WalWrite::Written(_)) => {
          self.open_write_txids.lock().insert(txid);
          break (checkpoint_gate, txid, snapshot_ts);
        }
        // The background checkpoint must take the gate to install, so wait
        // without the permit, then begin afresh.
        Ok(WalWrite::BlockedOn(cut)) => {
          self.abort_unregistered_transaction(txid);
          drop(checkpoint_gate);
          self.wait_for_cut_release(cut)?;
        }
        // Compacting under the permit keeps blocking checkpoints and
        // compaction out, as an open transaction would.
        Ok(WalWrite::NeedsCompaction) => {
          self.abort_unregistered_transaction(txid);
          let compacted = self.compact_retired_wal();
          drop(checkpoint_gate);
          compacted?;
        }
        // The WAL is full and no checkpoint is in the way. This thread has no
        // transaction open, so it can checkpoint now, as the next commit
        // would: if every thread only began transactions, none would.
        Err(KiteError::WalBufferFull) if !checkpointed_for_room => {
          self.abort_unregistered_transaction(txid);
          drop(checkpoint_gate);
          if !self.auto_checkpoint_if_needed(true) {
            return Err(KiteError::WalBufferFull);
          }
          checkpointed_for_room = true;
        }
        Err(error) => {
          self.abort_unregistered_transaction(txid);
          return Err(error);
        }
      }
    };

    let mut tx_state = SingleFileTxState::new(txid, read_only, snapshot_ts, bulk_load);
    tx_state.holds_writer = writer_claim.is_some();
    let tx_state = Arc::new(Mutex::new(tx_state));

    self.register_thread_transaction(tx_state);
    if let Some(claim) = writer_claim {
      claim.keep();
    }
    self.active_transactions.fetch_add(1, Ordering::Release);
    if !read_only {
      self.active_writers.fetch_add(1, Ordering::SeqCst);
    }
    Ok(txid)
  }

  /// Drop the MVCC state of a transaction whose begin failed before it was
  /// registered.
  fn abort_unregistered_transaction(&self, txid: TxId) {
    if let Some(mvcc) = self.mvcc.as_ref() {
      mvcc.tx_manager.lock().abort_tx(txid);
    }
  }

  /// Run `write` under the pager and WAL locks. If the WAL refuses the record
  /// because the secondary region filled during a background checkpoint,
  /// returns the cut to wait for: the caller releases every lock that
  /// checkpoint needs (the checkpoint gate, the commit lock), waits with
  /// `wait_for_cut_release`, and retries. Refused records are not written.
  pub(crate) fn try_write_wal<T>(
    &self,
    write: impl FnOnce(&mut WalBuffer, &mut FilePager) -> Result<T>,
  ) -> Result<WalWrite<T>> {
    let mut pager = self.pager.lock();
    let mut wal = self.wal_buffer.lock();
    match write(&mut wal, &mut pager) {
      Ok(value) => Ok(WalWrite::Written(value)),
      Err(KiteError::WalBufferFull) => {
        if let Some(cut) = self.cut_blocking_wal_writes(&wal) {
          Ok(WalWrite::BlockedOn(cut))
        } else if wal.is_primary_retired() {
          Ok(WalWrite::NeedsCompaction)
        } else {
          Err(KiteError::WalBufferFull)
        }
      }
      Err(error) => Err(error),
    }
  }

  /// Write `record` with `try_write_wal`, waiting and retrying for as long
  /// as a background checkpoint holds the WAL in a full secondary region, and
  /// compacting a retained WAL that fills it, then run `then` under the WAL
  /// lock right after the record is written. Callers hold no lock that
  /// checkpoint needs.
  fn write_wal_waiting_then(&self, record: &WalRecord, then: impl Fn()) -> Result<()> {
    self.write_built_wal_waiting_then(&mut record.build(), then)
  }

  /// `write_wal_waiting_then` for a record already built (unsalted, as
  /// `WalRecord::build` returns it); `record` is unsalted again on return.
  fn write_built_wal_waiting_then(&self, record: &mut [u8], then: impl Fn()) -> Result<()> {
    loop {
      let written = self.try_write_wal(|wal, pager| {
        wal.write_built_record(record, pager)?;
        then();
        Ok(())
      })?;
      match written {
        WalWrite::Written(()) => return Ok(()),
        WalWrite::BlockedOn(cut) => self.wait_for_cut_release(cut)?,
        WalWrite::NeedsCompaction => self.compact_retired_wal()?,
      }
    }
  }

  /// Compact WAL records retained in the secondary region by a background
  /// install whose own compaction failed (see `compact_retained_wal`), so
  /// writers that fill that region get the empty primary region instead of
  /// `WalBufferFull`. Callers hold no lock.
  ///
  /// The commit lock excludes background cuts and installs (a cut finishes
  /// the same compaction first). The caller keeps blocking checkpoints and
  /// compaction out with its open transaction or a checkpoint gate permit.
  pub(crate) fn compact_retired_wal(&self) -> Result<()> {
    let _commit_guard = self.commit_lock.lock();
    let mut pager = self.pager.lock();
    let mut wal = self.wal_buffer.lock();
    let mut header = self.header.write();
    if wal.is_primary_retired() {
      self.compact_retained_wal(&mut pager, &mut wal, &mut header)?;
    }
    Ok(())
  }

  /// The current write transaction, for a data write (nodes, edges,
  /// properties, labels, vectors). A replica refuses data writes except from
  /// its own replication apply (`begin_replication_apply`): its data mirrors
  /// the primary's.
  pub(crate) fn require_write_tx_handle(&self) -> Result<(TxId, Arc<Mutex<SingleFileTxState>>)> {
    self.write_tx_handle(true)
  }

  /// The current write transaction, for a schema definition. Replicas accept
  /// these: an application (Kite) defines the names it uses before the first
  /// pull, and replicas translate the primary's ids by name.
  pub(crate) fn require_schema_tx_handle(&self) -> Result<(TxId, Arc<Mutex<SingleFileTxState>>)> {
    self.write_tx_handle(false)
  }

  fn write_tx_handle(&self, data_write: bool) -> Result<(TxId, Arc<Mutex<SingleFileTxState>>)> {
    let handle = self.current_tx_handle().ok_or(KiteError::NoTransaction)?;
    let txid = {
      let tx = handle.lock();
      if tx.read_only {
        return Err(KiteError::ReadOnly);
      }
      if data_write && self.replica_replication.is_some() && !tx.replication_apply {
        return Err(KiteError::InvalidReplication(
          "database is opened in replica role: local data writes are rejected (write to the \
           primary; schema definitions are allowed)"
            .to_string(),
        ));
      }
      tx.txid
    };
    Ok((txid, handle))
  }

  /// Begin a new transaction
  pub fn begin(&self, read_only: bool) -> Result<TxId> {
    self.begin_with_mode(read_only, false)
  }

  /// Begin a new transaction guard (rolls back on drop)
  pub fn begin_guard(&self, read_only: bool) -> Result<SingleFileTxGuard<'_>> {
    let txid = self.begin_with_mode(read_only, false)?;
    Ok(SingleFileTxGuard::new(self, txid))
  }

  /// Begin a write transaction guard for a replica's replication apply
  /// (bootstrap, reseed, catch-up): the only transactions in which a replica
  /// accepts data writes.
  pub(crate) fn begin_replication_apply(&self) -> Result<SingleFileTxGuard<'_>> {
    let guard = self.begin_guard(false)?;
    if let Some(tx) = self.current_tx_handle() {
      tx.lock().replication_apply = true;
    }
    Ok(guard)
  }

  /// Begin a bulk-load transaction (fast path, MVCC disabled)
  pub fn begin_bulk(&self) -> Result<TxId> {
    self.begin_with_mode(false, true)
  }

  /// Begin a bulk-load transaction guard (rolls back on drop)
  pub fn begin_bulk_guard(&self) -> Result<SingleFileTxGuard<'_>> {
    let txid = self.begin_with_mode(false, true)?;
    Ok(SingleFileTxGuard::new(self, txid))
  }

  /// Record the changes of a commit in the MVCC version chains, for the transactions still
  /// open (see `mvcc_history`). With none open, no reader can need the state the commit
  /// replaces: every later read sees the commit, in the delta.
  fn apply_mvcc_commit(
    &self,
    commit_ts_for_mvcc: Option<(u64, bool)>,
    txid: TxId,
    pending: &DeltaState,
    delta: &DeltaState,
  ) {
    let Some((commit_ts, has_active_readers)) = commit_ts_for_mvcc else {
      return;
    };
    let Some(mvcc) = self.mvcc.as_ref() else {
      return;
    };
    if !has_active_readers {
      return;
    }

    let snapshot = self.snapshot.read();
    mvcc.record_history(commit_ts, |vc| {
      super::mvcc_history::record_commit(vc, delta, snapshot.as_ref(), pending, txid, commit_ts);
    });
  }

  /// Load the vector stores `pending_vectors` touches, so the commit's
  /// vector check sees them and applying its vectors after its durable point
  /// does no I/O. They stay loaded while the caller holds the commit lock:
  /// only a checkpoint install, which takes it, replaces them.
  fn load_vector_stores(
    &self,
    pending_vectors: &HashMap<(NodeId, PropKeyId), Option<VectorRef>>,
  ) -> Result<()> {
    let prop_keys: HashSet<PropKeyId> = pending_vectors
      .keys()
      .map(|&(_node_id, prop_key_id)| prop_key_id)
      .collect();
    for prop_key_id in prop_keys {
      self.ensure_vector_store_loaded(prop_key_id)?;
    }
    Ok(())
  }

  /// Refuse, before MVCC or a COMMIT record records it, a commit that writes
  /// to a node or edge that no longer exists: each write checked it, but
  /// another transaction deleted it before this one commits (a node in
  /// `deleted_in_round` is deleted by a commit earlier in this round, not
  /// merged yet). The writes would otherwise land as props, labels, edges or
  /// vectors of a missing node. Writes to a node the transaction deleted
  /// itself go with it at merge. Callers hold the commit lock, so the
  /// committed state does not change before the merge.
  fn check_commit_targets(
    &self,
    pending: &DeltaState,
    deleted_in_round: &HashSet<NodeId>,
  ) -> Result<()> {
    let delta = self.delta.read();
    let snapshot = self.snapshot.read();
    let snapshot = snapshot.as_ref();
    // The transaction's own copy, or one it deleted: nothing to re-check.
    let own =
      |node_id: NodeId| pending.is_node_deleted(node_id) || pending.is_node_created(node_id);
    let committed = |node_id: NodeId| {
      !deleted_in_round.contains(&node_id) && delta.node_exists_over(snapshot, node_id)
    };
    let edge_endpoints = pending
      .out_add
      .iter()
      .flat_map(|(&src, patches)| patches.iter().flat_map(move |patch| [src, patch.other]));
    let vector_nodes = pending
      .pending_vectors
      .iter()
      .filter(|(_, operation)| operation.is_some())
      .map(|(&(node_id, _), _)| node_id);
    let touched = pending
      .modified_nodes
      .keys()
      .copied()
      .chain(edge_endpoints)
      .chain(vector_nodes);
    for node_id in touched {
      if !own(node_id) && !committed(node_id) {
        return Err(KiteError::NodeNotFound(node_id));
      }
    }
    for &(src, etype, dst) in pending.edge_props.keys() {
      if pending.is_node_removed(src) || pending.is_node_removed(dst) {
        continue;
      }
      let in_base = committed(src)
        && committed(dst)
        && !pending.is_node_deleted(src)
        && !pending.is_node_deleted(dst)
        && delta.edge_exists_over(snapshot, src, etype, dst);
      if !pending.edge_visible(src, etype, dst, in_base) {
        return Err(KiteError::EdgeNotFound { src, etype, dst });
      }
    }
    Ok(())
  }

  /// Refuse, before MVCC or a COMMIT record records it, a commit whose
  /// vectors cannot be applied: their dimensions disagree with their
  /// property's store, or with the dimensions `claimed` by commits earlier in
  /// its round for a property without one (`set_node_vector` checks only the
  /// store as it was then, and the transaction's own vectors). Returns the
  /// dimensions this commit gives properties without a store. Callers hold
  /// the commit lock, so no store changes meanwhile, and loaded the stores
  /// (`load_vector_stores`).
  fn check_commit_vectors(
    &self,
    pending_vectors: &HashMap<(NodeId, PropKeyId), Option<VectorRef>>,
    claimed: &HashMap<PropKeyId, usize>,
  ) -> Result<HashMap<PropKeyId, usize>> {
    let stores = self.vector_stores.read();
    let mut new_dimensions = HashMap::new();
    for (&(_node_id, prop_key_id), operation) in pending_vectors {
      let Some(vector) = operation else {
        continue;
      };
      let expected = stores
        .get(&prop_key_id)
        .map(|store| store.config.dimensions)
        .or_else(|| claimed.get(&prop_key_id).copied())
        .or_else(|| new_dimensions.get(&prop_key_id).copied());
      match expected {
        Some(expected) if expected != vector.len() => {
          return Err(KiteError::VectorDimensionMismatch {
            expected,
            got: vector.len(),
          });
        }
        Some(_) => {}
        None => {
          new_dimensions.insert(prop_key_id, vector.len());
        }
      }
    }
    Ok(new_dimensions)
  }

  /// Commit the current transaction
  pub fn commit(&self) -> Result<()> {
    self.commit_with_token().map(|_| ())
  }

  /// Commit the current transaction and return replication commit token if enabled.
  pub fn commit_with_token(&self) -> Result<Option<CommitToken>> {
    if self.read_only && self.current_tx_handle().is_none() {
      return Err(KiteError::ReadOnly);
    }

    let tx_handle = self
      .take_thread_transaction()
      .ok_or(KiteError::NoTransaction)?;
    let read_only = tx_handle.lock().read_only;
    let result = self.commit_transaction(&tx_handle);
    if !read_only {
      // Every lock is released and this thread's transaction is finished, so
      // the checkpoint may wait for other threads' open transactions without
      // ever waiting on its own. A failed commit checkpoints too: when the
      // WAL refused its COMMIT record, every later commit would fail the same
      // way, and nothing else would ever checkpoint.
      self.auto_checkpoint_if_needed(matches!(result, Err(KiteError::WalBufferFull)));
    }
    result
  }

  /// Commit the transaction `tx_handle`, already taken from its thread.
  fn commit_transaction(
    &self,
    tx_handle: &Arc<Mutex<SingleFileTxState>>,
  ) -> Result<Option<CommitToken>> {
    let (txid, read_only, bulk_load, holds_writer, pending, pending_wal, staged_schema) = {
      let mut tx = tx_handle.lock();
      let pending = std::mem::take(&mut tx.pending);
      let staged_schema = std::mem::take(&mut tx.schema);
      let pending_wal = std::mem::take(&mut tx.pending_wal);
      // Its reads and writes join its MVCC sets before the conflict check.
      let reads = std::mem::take(&mut tx.mvcc_reads);
      let writes = std::mem::take(&mut tx.mvcc_writes);
      if let (Some(mvcc), false) = (self.mvcc.as_ref(), reads.is_empty() && writes.is_empty()) {
        // Grouped here, without any lock: with other transactions open, the
        // check and the commit, which every other commit waits for, then
        // look up and note groups instead of keys. Alone, it commits with no
        // check and nothing to note.
        let groups = (self.active_transactions.load(Ordering::Acquire) > 1)
          .then(|| TxKeyGroups::of(&reads, &writes));
        mvcc
          .tx_manager
          .lock()
          .record_reads_and_writes(tx.txid, reads, writes, groups);
      }
      (
        tx.txid,
        tx.read_only,
        tx.bulk_load,
        std::mem::take(&mut tx.holds_writer),
        pending,
        pending_wal,
        staged_schema,
      )
    };
    // Dropped last: the transaction counts as active (blocking checkpoints
    // wait for it) until its commit is settled. For background cuts it stops
    // counting as open once its COMMIT is durable (`publish_commit`).
    let _active_transaction_guard = ActiveTransactionGuard {
      db: self,
      txid,
      wrote_begin: !read_only && !bulk_load,
      holds_writer,
    };
    let mut schema_reservation_guard = SchemaReservationGuard::new(self, txid);

    if read_only {
      // Read-only transactions don't need WAL
      if let Some(mvcc) = self.mvcc.as_ref() {
        let mut tx_mgr = mvcc.tx_manager.lock();
        tx_mgr.abort_tx(txid);
      }
      return Ok(None);
    }
    let prev_writers = self.active_writers.fetch_sub(1, Ordering::SeqCst);
    debug_assert!(prev_writers > 0, "active_writers underflow in commit");
    // Until the commit is durable (MVCC commits it there), every failure
    // aborts it in MVCC.
    let mut mvcc_abort = MvccAbortGuard {
      db: self,
      txid,
      armed: true,
    };

    let replication_enabled = self.primary_replication.is_some();
    let group_commit_active =
      self.group_commit_enabled && self.sync_mode == SyncMode::Normal && !replication_enabled;

    // A bulk load writes its whole transaction now, in one batch, so a WAL
    // that refuses it is left without a partial copy.
    let records = {
      let commit = WalRecord::new(WalRecordType::Commit, txid, build_commit_payload()).build();
      if bulk_load {
        let mut records = WalRecord::new(WalRecordType::Begin, txid, build_begin_payload()).build();
        records.extend_from_slice(&pending_wal);
        records.extend_from_slice(&commit);
        records
      } else {
        commit
      }
    };
    let request = CommitRequest {
      txid,
      bulk_load,
      records,
      pending,
      pending_wal,
      staged_schema,
      committer: std::thread::current().id(),
    };

    let outcome = if group_commit_active {
      self.commit_in_group(request)
    } else {
      self
        .write_commits(vec![request])
        .into_iter()
        .next()
        .unwrap_or_else(|| {
          CommitOutcome::failed(KiteError::Internal("commit was not written".to_string()))
        })
    };
    mvcc_abort.armed = !outcome.durable;
    if outcome.schema_published {
      schema_reservation_guard.disarm();
    }
    outcome.result
  }

  /// Group commit. Queue `request`; the first committer to find no leader
  /// leads: it takes everything queued and writes it as one batch
  /// (`write_commits`: one WAL flush and one header for all). The others
  /// wait for their outcome without the commit lock, so the commits that
  /// arrive while a batch is written form the next one. Nobody sleeps to
  /// wait for more.
  fn commit_in_group(&self, request: CommitRequest) -> CommitOutcome {
    let mut state = self.group_commit_state.lock();
    let ticket = state.next_ticket;
    state.next_ticket += 1;
    state.queue.push_back((ticket, request));
    loop {
      if let Some(outcome) = state.outcomes.remove(&ticket) {
        return outcome;
      }
      if state.leader_active {
        self.group_commit_cv.wait(&mut state);
        continue;
      }
      state.leader_active = true;
      let (tickets, requests): (Vec<u64>, Vec<CommitRequest>) = state.queue.drain(..).unzip();
      drop(state);
      let mut leader = GroupCommitLeader {
        db: self,
        tickets,
        outcomes: Vec::new(),
      };
      leader.outcomes = self.write_commits(requests);
      drop(leader);
      state = self.group_commit_state.lock();
    }
  }

  /// Write `requests`' commits in order, and return their outcomes in the
  /// same order. Each round, under the commit lock, writes the COMMIT records
  /// of those that fit and makes them durable with one WAL flush and one
  /// header (`write_commit_round`), then publishes each (`publish_commit`).
  /// Callers hold no lock.
  fn write_commits(&self, requests: Vec<CommitRequest>) -> Vec<CommitOutcome> {
    let mut outcomes: Vec<Option<CommitOutcome>> = requests.iter().map(|_| None).collect();
    let mut queue: VecDeque<(usize, CommitRequest)> = requests.into_iter().enumerate().collect();
    while !queue.is_empty() {
      before_commit_lock_test_hook();
      #[cfg(feature = "bench-profile")]
      let commit_lock_start = Instant::now();
      let commit_guard = self.commit_lock.lock();
      #[cfg(feature = "bench-profile")]
      self.commit_lock_wait_ns.fetch_add(
        commit_lock_start.elapsed().as_nanos() as u64,
        Ordering::Relaxed,
      );

      let round = self.write_commit_round(&mut queue, &mut outcomes);
      for (index, request) in round.durable {
        outcomes[index] = Some(self.publish_commit(request));
      }
      drop(commit_guard);

      // The background checkpoint takes the commit lock to install.
      if let Some(cut) = round.wait_for_cut {
        if let Err(error) = self.wait_for_cut_release(cut) {
          if let Some((index, _)) = queue.pop_front() {
            outcomes[index] = Some(CommitOutcome::failed(error));
          }
        }
      }
    }
    outcomes
      .into_iter()
      .map(|outcome| {
        outcome.unwrap_or_else(|| {
          CommitOutcome::failed(KiteError::Internal("commit was not written".to_string()))
        })
      })
      .collect()
  }

  /// One round of `write_commits`, under the commit lock: write the COMMIT
  /// records of `queue`'s requests, in order, while they fit, then make them
  /// durable (`persist_commit_header`). Returns the durable ones, to publish;
  /// a request refused before that gets its outcome in `outcomes`, with
  /// nothing of it recorded.
  ///
  /// The pager and WAL locks are held from the first record to the header,
  /// so the round's records follow every record written before them and
  /// none follows them. If the round fails before its header is durable,
  /// rewinding the WAL head forgets exactly its records: later records
  /// overwrite their bytes before any header names them, so a failed commit
  /// never becomes durable later.
  fn write_commit_round(
    &self,
    queue: &mut VecDeque<(usize, CommitRequest)>,
    outcomes: &mut [Option<CommitOutcome>],
  ) -> CommitRound {
    let mut round = CommitRound::default();
    // Loading a store and checking targets take the snapshot lock, so before
    // the pager lock.
    let mut loaded = VecDeque::with_capacity(queue.len());
    let mut deleted_in_round = HashSet::new();
    for (index, request) in queue.drain(..) {
      // With MVCC, conflict detection refuses these commits (each write
      // recorded a read of its node or edge), except bulk loads, which record
      // nothing.
      let check_targets = self.mvcc.is_none() || request.bulk_load;
      // Epoch fencing, under the commit lock so a promotion that landed while
      // this commit waited for it is seen, and before MVCC or the WAL records
      // the commit. A repair fence is let through: it affects replication,
      // not local commit authority.
      let checked = self
        .primary_replication
        .as_ref()
        .map_or(Ok(()), |replication| {
          replication.ensure_local_commit_allowed()
        })
        .and_then(|()| self.load_vector_stores(&request.pending.pending_vectors))
        .and_then(|()| {
          if check_targets {
            self.check_commit_targets(&request.pending, &deleted_in_round)
          } else {
            Ok(())
          }
        });
      match checked {
        Ok(()) => {
          // Conservative: if this commit is refused later in the round, a
          // later one writing to these nodes is refused too.
          deleted_in_round.extend(request.pending.deleted_nodes.iter().copied());
          loaded.push_back((index, request));
        }
        Err(error) => outcomes[index] = Some(CommitOutcome::failed(error)),
      }
    }
    *queue = loaded;

    let mut pager = self.pager.lock();
    let mut wal = self.wal_buffer.lock();
    let mut staged = Vec::new();
    let mut wal_before_round: Option<WalRegionState> = None;
    let mut claims = RoundClaims::default();
    while let Some((index, request)) = queue.pop_front() {
      let new_dimensions = match self
        .check_commit_vectors(&request.pending.pending_vectors, &claims.vector_dimensions)
      {
        Ok(new_dimensions) => new_dimensions,
        Err(error) => {
          outcomes[index] = Some(CommitOutcome::failed(error));
          continue;
        }
      };
      if !wal.can_fit(request.records.len()) {
        // Make what fits durable first; this one waits for the next round.
        if !staged.is_empty() {
          queue.push_front((index, request));
          break;
        }
        // Nothing of this commit is recorded yet, so make room and retry.
        if wal.is_primary_retired() {
          let mut header = self.header.write();
          match self.compact_retained_wal(&mut pager, &mut wal, &mut header) {
            Ok(()) => queue.push_front((index, request)),
            Err(error) => outcomes[index] = Some(CommitOutcome::failed(error)),
          }
          continue;
        }
        match self.cut_blocking_wal_writes(&wal) {
          Some(cut) => {
            queue.push_front((index, request));
            round.wait_for_cut = Some(cut);
            break;
          }
          None => outcomes[index] = Some(CommitOutcome::failed(KiteError::WalBufferFull)),
        }
        continue;
      }
      // Only a later commit of the round needs this one's writes.
      let claim_writes = !queue.is_empty();
      let mvcc_writes =
        match self.check_commit_in_mvcc(request.txid, &claims.mvcc_writes, claim_writes) {
          Ok(mvcc_writes) => mvcc_writes,
          Err(error) => {
            outcomes[index] = Some(CommitOutcome::failed(error));
            continue;
          }
        };
      let before = wal.region_state();
      if let Err(error) = wal.write_record_bytes_batch(&request.records, &mut pager) {
        outcomes[index] = Some(CommitOutcome::failed(error));
        continue;
      }
      wal_before_round.get_or_insert(before);
      claims.vector_dimensions.extend(new_dimensions);
      claims.mvcc_writes.extend(mvcc_writes);
      staged.push((index, request));
    }
    let Some(wal_before_round) = wal_before_round else {
      return round;
    };

    match self.persist_commit_header(&mut pager, &mut wal, staged.len()) {
      Ok(()) => round.durable = staged,
      Err(error) => {
        if let Err(scrub) = wal.discard_since(wal_before_round, &mut pager) {
          eprintln!(
            "Warning: could not sync the discarded records of a failed commit; the next commit \
             syncs them before its header: {scrub}"
          );
        }
        let mut errors: Vec<KiteError> =
          staged.iter().skip(1).map(|_| round_error(&error)).collect();
        errors.insert(0, error);
        for ((index, _), error) in staged.into_iter().zip(errors) {
          outcomes[index] = Some(CommitOutcome::failed(error));
        }
      }
    }
    round
  }

  /// Make the WAL records written so far durable as the sync mode asks:
  /// flush them (and in Full mode fsync them) before a header names them,
  /// then install that header. `commits` is the number of commits the round
  /// wrote. A header written before its WAL bytes names bytes a crash can
  /// leave unwritten, where recovery reads stale records of an earlier WAL
  /// cycle. On error the in-memory header is as it was, but for its newer
  /// change counter (the next header must outrank every slot on disk).
  fn persist_commit_header(
    &self,
    pager: &mut FilePager,
    wal: &mut WalBuffer,
    commits: usize,
  ) -> Result<()> {
    #[cfg(feature = "bench-profile")]
    let flush_start = Instant::now();
    let flushed = match self.sync_mode {
      SyncMode::Full => wal.sync(pager),
      // A failed round's records may still be readable on disk, and this
      // header names bytes past them: make their overwrite durable first.
      SyncMode::Normal if wal.needs_sync() => wal.sync(pager),
      SyncMode::Normal => wal.flush(pager),
      SyncMode::Off => Ok(()),
    };
    #[cfg(feature = "bench-profile")]
    self
      .wal_flush_ns
      .fetch_add(flush_start.elapsed().as_nanos() as u64, Ordering::Relaxed);
    flushed?;

    // MVCC commits the round's commits in order from its next timestamp.
    let last_commit_ts = match self.mvcc.as_ref() {
      Some(mvcc) => mvcc.tx_manager.lock().next_commit_ts() + (commits as u64).saturating_sub(1),
      None => std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0),
    };
    let mut header = self.header.write();
    let prior = header.clone();
    wal.store_in_header(&mut header);
    header.max_node_id = self
      .next_node_id
      .load(std::sync::atomic::Ordering::SeqCst)
      .saturating_sub(1);
    header.next_tx_id = self.next_tx_id.load(std::sync::atomic::Ordering::SeqCst);
    header.last_commit_ts = last_commit_ts;
    if self.sync_mode != SyncMode::Off {
      #[cfg(feature = "bench-profile")]
      let sync_start = Instant::now();
      let persisted = self.persist_header(pager, &mut header, self.sync_mode == SyncMode::Full);
      #[cfg(feature = "bench-profile")]
      self
        .wal_flush_ns
        .fetch_add(sync_start.elapsed().as_nanos() as u64, Ordering::Relaxed);
      if let Err(error) = persisted {
        let change_counter = header.change_counter;
        *header = prior;
        header.change_counter = change_counter;
        return Err(error);
      }
    }
    Ok(())
  }

  /// Make a durable commit visible, under the commit lock and in WAL order:
  /// publish its schema; then, in one `delta.write()` critical section,
  /// commit it in MVCC (its timestamp), add its version chains, apply its
  /// vectors and merge it into the delta; then hand it to the replication
  /// sidecar. Transactions begin under `delta.read()`, so none begins between
  /// the timestamp and the merge, and each one begun before counts as a
  /// reader that needs version chains.
  ///
  /// Every step runs even if an earlier one fails: stopping early would leave
  /// a durable transaction out of the delta, invisible until a reopen replays
  /// it, and the next checkpoint (a snapshot of the delta) would drop it. The
  /// first failure is reported.
  fn publish_commit(&self, request: CommitRequest) -> CommitOutcome {
    let CommitRequest {
      txid,
      mut pending,
      pending_wal,
      staged_schema,
      committer,
      ..
    } = request;
    let on_committer_thread = committer == std::thread::current().id();

    // A background cut (which takes the commit lock) no longer counts it as
    // open: its records end with a durable COMMIT. Its committer may only
    // learn that later (a group-commit follower wakes after the batch is
    // delivered), and a cut taken meanwhile would skip its records, its
    // install drop them, and the next cut find an open transaction with no
    // BEGIN record and decline.
    self.open_write_txids.lock().remove(&txid);

    // This is the schema visibility point, right after the durable commit
    // boundary. Publishing before any fallible post-commit work keeps a
    // later error from leaving a committed WAL definition hidden in this
    // process.
    let schema_result = self.publish_staged_schema(&staged_schema);

    if on_committer_thread {
      before_merge_test_hook();
    }
    let mut delta = self.delta.write();
    let mvcc_commit = self.commit_in_mvcc(txid);
    if on_committer_thread {
      after_commit_timestamp_test_hook();
    }
    let commit_ts_for_mvcc = mvcc_commit.as_ref().ok().copied().flatten();
    self.apply_mvcc_commit(commit_ts_for_mvcc, txid, &pending, &delta);

    // The stores are loaded and the dimensions checked (`write_commit_round`).
    let vector_fault = if on_committer_thread {
      post_durable_test_fault()
    } else {
      Ok(())
    };
    let vector_result =
      vector_fault.and_then(|()| self.apply_pending_vectors(&pending.pending_vectors));

    delta.merge_from(&mut pending);
    drop(delta);
    // Its emptied maps are freed without the lock.
    drop(pending);

    let mut commit_token = None;
    if let Some(replication) = self.primary_replication.as_ref() {
      if replication.crash_after_local_commit_for_testing() {
        // Test-only abrupt-stop hook for the exact local-durable/sidecar
        // boundary. The main WAL, header, and in-memory state are complete.
        std::process::abort();
      }
      match replication.append_commit_wal_frame(txid, pending_wal) {
        Ok(token) => commit_token = Some(token),
        Err(error) => {
          eprintln!("Warning: local commit durable but replication sidecar append failed: {error}")
        }
      }
    }

    CommitOutcome {
      durable: true,
      schema_published: schema_result.is_ok(),
      result: schema_result
        .and(mvcc_commit.map(|_| ()))
        .and(vector_result)
        .map(|()| commit_token),
    }
  }

  /// Check `txid` for MVCC conflicts before its COMMIT record is written:
  /// with the transactions committed since it began, and with `claimed`, the
  /// keys written by commits earlier in its round (committed in MVCC only
  /// once the round is durable). Returns the keys it writes if
  /// `claim_writes` (later commits of the round check against them). A
  /// conflict aborts it. Callers hold the commit lock, so nothing commits between
  /// this check and its MVCC commit (`commit_in_mvcc`).
  fn check_commit_in_mvcc(
    &self,
    txid: TxId,
    claimed: &TxKeySet,
    claim_writes: bool,
  ) -> Result<Vec<TxKey>> {
    let Some(mvcc) = self.mvcc.as_ref() else {
      return Ok(Vec::new());
    };
    let mut tx_mgr = mvcc.tx_manager.lock();
    let (mut conflicts, writes) = match tx_mgr.tx(txid) {
      Some(tx) => (
        if claimed.is_empty() {
          Vec::new()
        } else {
          tx.read_set
            .union(&tx.write_set)
            .filter(|key| claimed.contains(*key))
            .map(|key| key.to_string())
            .collect::<Vec<_>>()
        },
        if claim_writes {
          tx.write_set.iter().cloned().collect::<Vec<_>>()
        } else {
          Vec::new()
        },
      ),
      None => {
        return Err(KiteError::Internal(format!(
          "transaction {txid} is not active in MVCC"
        )))
      }
    };
    if let Err(err) = mvcc.conflict_detector.validate_commit(&tx_mgr, txid) {
      conflicts.extend(err.conflicting_keys);
    }
    if !conflicts.is_empty() {
      tx_mgr.abort_tx(txid);
      conflicts.sort_unstable();
      conflicts.dedup();
      return Err(KiteError::Conflict {
        txid,
        keys: conflicts,
      });
    }
    Ok(writes)
  }

  /// Commit `txid` in MVCC, if enabled, at its durable point: its commit
  /// timestamp, and whether any transaction is still active (which then
  /// needs version chains). Callers hold `delta.write()` (see
  /// `publish_commit`) and checked its conflicts (`check_commit_in_mvcc`).
  fn commit_in_mvcc(&self, txid: TxId) -> Result<Option<(u64, bool)>> {
    let Some(mvcc) = self.mvcc.as_ref() else {
      return Ok(None);
    };
    let mut tx_mgr = mvcc.tx_manager.lock();
    let commit_ts = tx_mgr
      .commit_tx(txid)
      .map_err(|e| KiteError::Internal(e.to_string()))?;
    Ok(Some((commit_ts, tx_mgr.active_count() > 0)))
  }

  /// Rollback the current transaction
  pub fn rollback(&self) -> Result<()> {
    let tx_handle = self
      .take_thread_transaction()
      .ok_or(KiteError::NoTransaction)?;
    let read_only = tx_handle.lock().read_only;
    let result = self.rollback_transaction(&tx_handle);
    if !read_only {
      // As after a commit: a rollback often follows a write the full WAL
      // refused, and nothing else may checkpoint.
      self.auto_checkpoint_if_needed(false);
    }
    result
  }

  /// Roll back the transaction `tx_handle`, already taken from its thread
  /// (by `rollback`, or abandoned by a thread that ended with it open).
  pub(super) fn rollback_transaction(
    &self,
    tx_handle: &Arc<Mutex<SingleFileTxState>>,
  ) -> Result<()> {
    let (txid, read_only, bulk_load, holds_writer) = {
      let mut tx = tx_handle.lock();
      (
        tx.txid,
        tx.read_only,
        tx.bulk_load,
        std::mem::take(&mut tx.holds_writer),
      )
    };
    let _active_transaction_guard = ActiveTransactionGuard {
      db: self,
      txid,
      wrote_begin: !read_only && !bulk_load,
      holds_writer,
    };
    let _schema_reservation_guard = SchemaReservationGuard::new(self, txid);

    if read_only {
      // Read-only transactions don't need WAL
      if let Some(mvcc) = self.mvcc.as_ref() {
        let mut tx_mgr = mvcc.tx_manager.lock();
        tx_mgr.abort_tx(txid);
      }
      return Ok(());
    }
    let prev_writers = self.active_writers.fetch_sub(1, Ordering::SeqCst);
    debug_assert!(prev_writers > 0, "active_writers underflow in rollback");

    if let Some(mvcc) = self.mvcc.as_ref() {
      let mut tx_mgr = mvcc.tx_manager.lock();
      tx_mgr.abort_tx(txid);
    }

    if !bulk_load {
      // Write the ROLLBACK record, and stop counting the transaction as open
      // under the same WAL lock, which a background cut holds while it reads
      // the open set (see `publish_commit` for COMMIT records).
      let record = WalRecord::new(WalRecordType::Rollback, txid, build_rollback_payload());
      self.write_wal_waiting_then(&record, || {
        self.open_write_txids.lock().remove(&txid);
      })?;
    }

    Ok(())
  }

  /// Check if there's an active transaction
  pub fn has_transaction(&self) -> bool {
    self.current_tx_handle().is_some()
  }

  /// Get the current transaction ID (if any)
  pub fn current_txid(&self) -> Option<TxId> {
    self.current_tx_handle().as_ref().map(|tx| tx.lock().txid)
  }

  /// Get the most recently emitted commit token from primary replication.
  pub fn last_commit_token(&self) -> Option<CommitToken> {
    self
      .primary_replication
      .as_ref()
      .and_then(|replication| replication.last_token())
  }

  /// Get primary replication status when replication role is `primary`.
  pub fn primary_replication_status(&self) -> Option<PrimaryReplicationStatus> {
    self
      .primary_replication
      .as_ref()
      .map(|replication| replication.status())
  }

  /// Log `record` for the transaction `tx_handle`: to the WAL now, or, for a
  /// bulk load, at its commit. It is encoded once. The transaction keeps a
  /// copy only when something reads it later: a bulk load's commit writes
  /// its records then, and a primary's commit hands them to the replication
  /// sidecar (`publish_commit`).
  pub(crate) fn write_wal_tx(
    &self,
    tx_handle: &Arc<Mutex<SingleFileTxState>>,
    record: WalRecord,
  ) -> Result<()> {
    let mut record_bytes = record.build();
    let mut tx = tx_handle.lock();
    if tx.bulk_load {
      tx.pending_wal.extend_from_slice(&record_bytes);
      return Ok(());
    }
    drop(tx);
    self.write_built_wal_waiting_then(&mut record_bytes, || {})?;
    if self.primary_replication.is_some() {
      tx_handle
        .lock()
        .pending_wal
        .extend_from_slice(&record_bytes);
    }
    Ok(())
  }

  /// Get current transaction ID or error
  pub(crate) fn require_write_tx(&self) -> Result<TxId> {
    let (txid, _) = self.require_write_tx_handle()?;
    Ok(txid)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::core::single_file::{close_single_file, open_single_file, SingleFileOpenOptions};
  use std::panic::{catch_unwind, AssertUnwindSafe};
  use tempfile::tempdir;

  #[test]
  fn tx_guard_rolls_back_on_drop() -> Result<()> {
    let temp_dir = tempdir()?;
    let db_path = temp_dir.path().join("tx-guard.kitedb");
    let db = open_single_file(&db_path, SingleFileOpenOptions::new())?;

    let result = catch_unwind(AssertUnwindSafe(|| {
      let _tx = db.begin_guard(false).expect("expected value");
      db.create_node(Some("guarded")).expect("expected value");
      panic!("boom");
    }));

    assert!(result.is_err());
    assert!(!db.has_transaction());

    db.begin(false)?;
    db.commit()?;
    close_single_file(db)?;

    Ok(())
  }

  /// A commit whose COMMIT record is durable is committed even if a later
  /// step fails: it must be visible, and survive the next checkpoint (which
  /// replaces the WAL with a snapshot of the delta). Regression: the error
  /// returned before the transaction merged into the delta, so the next
  /// checkpoint dropped it.
  #[test]
  fn commit_failing_after_its_durable_point_is_kept() {
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("post-durable-failure.kitedb");
    let options = SingleFileOpenOptions::new().auto_checkpoint(false);
    let db = open_single_file(&db_path, options.clone()).expect("open");
    db.begin(false).expect("begin");
    db.create_node(Some("kept")).expect("create");
    FAIL_NEXT_COMMIT_AFTER_DURABLE.with(|fail| fail.set(true));
    assert!(db.commit().is_err());
    assert!(!db.has_transaction());
    assert!(
      db.node_by_key("kept").is_some(),
      "a durable commit is missing from reads"
    );

    db.checkpoint().expect("checkpoint");
    drop(db);
    let reopened = open_single_file(&db_path, options).expect("reopen");
    assert!(
      reopened.node_by_key("kept").is_some(),
      "the checkpoint dropped a durable commit"
    );
  }

  /// Two transactions that give a new vector property different dimensions
  /// cannot both commit; the second is refused before its COMMIT record is
  /// written. Regression: that COMMIT became durable before its vector failed
  /// to apply, so the commit reported an error and its changes stayed
  /// invisible, yet a reopen replayed it and failed on the mismatch: the
  /// database could not be opened until a checkpoint dropped it.
  #[test]
  fn vector_dimension_conflict_is_refused_before_the_commit_is_durable() {
    use std::sync::{mpsc, Arc};
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("vector-dimension-race.kitedb");
    // Write transactions open at once need MVCC (without it they run one at a time).
    let options = SingleFileOpenOptions::new()
      .mvcc(true)
      .mvcc_gc_interval_ms(10)
      .auto_checkpoint(false);
    let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));
    db.begin(false).expect("begin");
    let embedding = db.define_propkey("embedding").expect("propkey");
    db.commit().expect("commit");

    // Each transaction stages a vector before either commits, so neither
    // sees the other's dimensions when it sets its vector.
    let stage = |key: &'static str, dimensions: usize| {
      let (staged_tx, staged_rx) = mpsc::channel();
      let (go_tx, go_rx) = mpsc::channel::<()>();
      let writer_db = Arc::clone(&db);
      let writer = std::thread::spawn(move || {
        writer_db.begin(false).expect("begin");
        let node = writer_db.create_node(Some(key)).expect("create");
        writer_db
          .set_node_vector(node, embedding, &vec![0.5; dimensions])
          .expect("stage vector");
        staged_tx.send(()).expect("signal staged");
        go_rx.recv().expect("wait");
        writer_db.commit()
      });
      staged_rx.recv().expect("staged");
      (go_tx, writer)
    };
    let (first_go, first) = stage("three", 3);
    let (second_go, second) = stage("four", 4);
    first_go.send(()).expect("release first");
    first.join().expect("first thread").expect("first commit");
    second_go.send(()).expect("release second");
    let second_result = second.join().expect("second thread");

    assert!(second_result.is_err(), "both dimensions committed");
    assert!(db.node_by_key("three").is_some());
    assert!(db.node_by_key("four").is_none());

    // Crash, then reopen.
    let copy_path = db_path.with_extension("crash.kitedb");
    std::fs::copy(&db_path, &copy_path).expect("copy");
    let crashed = open_single_file(&copy_path, options).expect("reopen after the refused commit");
    assert!(crashed.node_by_key("three").is_some());
    assert!(crashed.node_by_key("four").is_none());
  }

  /// MVCC: the batches these tests build need several write transactions
  /// open at once, and without MVCC they run one at a time.
  fn group_commit_options() -> SingleFileOpenOptions {
    SingleFileOpenOptions::new()
      .mvcc(true)
      .mvcc_gc_interval_ms(10)
      .auto_checkpoint(false)
      .sync_mode(SyncMode::Normal)
      .group_commit_enabled(true)
  }

  fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !condition() {
      assert!(
        std::time::Instant::now() < deadline,
        "timed out waiting for {what}"
      );
      std::thread::sleep(std::time::Duration::from_millis(1));
    }
  }

  /// Hold the commit lock while a group-commit leader starts (with a batch of
  /// its own, waiting for the lock), so the commits that `queue_commits`
  /// starts next queue behind it as one batch. Returns the leader's thread
  /// and the lock; dropping the lock writes the leader's batch, then theirs.
  fn hold_group_commit_leader(
    db: &Arc<SingleFileDB>,
  ) -> (
    std::thread::JoinHandle<Result<()>>,
    parking_lot::MutexGuard<'_, ()>,
  ) {
    let commit_lock = db.commit_lock.lock();
    let leader_db = Arc::clone(db);
    let leader = std::thread::spawn(move || {
      leader_db.begin(false)?;
      leader_db.create_node(Some("leader"))?;
      leader_db.commit()
    });
    wait_until("the group-commit leader", || {
      let state = db.group_commit_state.lock();
      state.leader_active && state.queue.is_empty()
    });
    (leader, commit_lock)
  }

  fn wait_for_queued_commits(db: &SingleFileDB, count: usize) {
    wait_until("queued commits", || {
      db.group_commit_state.lock().queue.len() == count
    });
  }

  /// Commits that queue while a group commit is written are written as one
  /// batch: one header for all of them, and each is visible and durable.
  #[test]
  fn group_commit_writes_queued_commits_as_one_batch() {
    const QUEUED: usize = 5;
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("group-batch.kitedb");
    let db = Arc::new(open_single_file(&db_path, group_commit_options()).expect("open"));
    let generation = db.header.read().change_counter;

    let (leader, commit_lock) = hold_group_commit_leader(&db);
    let writers: Vec<_> = (0..QUEUED)
      .map(|i| {
        let db = Arc::clone(&db);
        std::thread::spawn(move || {
          db.begin(false)?;
          db.create_node(Some(&format!("queued-{i}")))?;
          db.commit()
        })
      })
      .collect();
    wait_for_queued_commits(&db, QUEUED);
    drop(commit_lock);
    leader.join().expect("leader").expect("leader commit");
    for writer in writers {
      writer.join().expect("writer").expect("queued commit");
    }

    assert_eq!(
      db.header.read().change_counter - generation,
      2,
      "the leader's batch and the queued batch each write one header"
    );
    let image = db_path.with_extension("image.kitedb");
    std::fs::copy(&db_path, &image).expect("copy");
    let crashed = open_single_file(&image, group_commit_options()).expect("open image");
    for key in
      std::iter::once("leader".to_string()).chain((0..QUEUED).map(|i| format!("queued-{i}")))
    {
      assert!(db.node_by_key(&key).is_some(), "{key} is not visible");
      assert!(crashed.node_by_key(&key).is_some(), "{key} is not durable");
    }
  }

  /// Two transactions that read and increment one counter, committed in the
  /// same group-commit batch: the second conflicts with the first, as it
  /// would had the first committed before it was checked. Neither is
  /// committed in MVCC before the batch is durable, so the MVCC check alone
  /// would pass both and lose an update.
  #[test]
  fn group_commit_refuses_a_conflicting_commit_in_the_same_batch() {
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("group-conflict.kitedb");
    let options = group_commit_options().mvcc(true).mvcc_gc_interval_ms(10);
    let db = Arc::new(open_single_file(&db_path, options).expect("open"));
    db.begin(false).expect("begin");
    let node = db.create_node(Some("counter")).expect("node");
    let count = db.define_propkey("count").expect("propkey");
    db.set_node_prop(node, count, PropValue::I64(0))
      .expect("set count");
    db.commit().expect("commit");

    let (leader, commit_lock) = hold_group_commit_leader(&db);
    let both_read = Arc::new(std::sync::Barrier::new(2));
    let incrementers: Vec<_> = (0..2)
      .map(|_| {
        let db = Arc::clone(&db);
        let both_read = Arc::clone(&both_read);
        std::thread::spawn(move || {
          db.begin(false)?;
          let value = match db.node_prop(node, count) {
            Some(PropValue::I64(value)) => value,
            other => panic!("unexpected count {other:?}"),
          };
          both_read.wait();
          db.set_node_prop(node, count, PropValue::I64(value + 1))?;
          db.commit()
        })
      })
      .collect();
    wait_for_queued_commits(&db, 2);
    drop(commit_lock);
    leader.join().expect("leader").expect("leader commit");
    let results: Vec<Result<()>> = incrementers
      .into_iter()
      .map(|incrementer| incrementer.join().expect("incrementer"))
      .collect();

    assert_eq!(
      results.iter().filter(|result| result.is_ok()).count(),
      1,
      "exactly one increment commits: {results:?}"
    );
    assert!(
      results
        .iter()
        .any(|result| matches!(result, Err(KiteError::Conflict { .. }))),
      "the other conflicts: {results:?}"
    );
    assert_eq!(db.node_prop(node, count), Some(PropValue::I64(1)));
  }

  /// Two transactions that give a new vector property different dimensions,
  /// committed in the same group-commit batch: the store does not exist until
  /// the batch is durable, so the second is checked against the first's
  /// dimensions and refused before its COMMIT record.
  #[test]
  fn group_commit_refuses_conflicting_vector_dimensions_in_the_same_batch() {
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("group-vectors.kitedb");
    let db = Arc::new(open_single_file(&db_path, group_commit_options()).expect("open"));
    db.begin(false).expect("begin");
    let embedding = db.define_propkey("embedding").expect("propkey");
    db.commit().expect("commit");

    let (leader, commit_lock) = hold_group_commit_leader(&db);
    let writers: Vec<_> = [("three", 3usize), ("four", 4usize)]
      .into_iter()
      .map(|(key, dimensions)| {
        let db = Arc::clone(&db);
        std::thread::spawn(move || {
          db.begin(false)?;
          let node = db.create_node(Some(key))?;
          db.set_node_vector(node, embedding, &vec![0.5; dimensions])?;
          db.commit()
        })
      })
      .collect();
    wait_for_queued_commits(&db, 2);
    drop(commit_lock);
    leader.join().expect("leader").expect("leader commit");
    let results: Vec<Result<()>> = writers
      .into_iter()
      .map(|writer| writer.join().expect("writer"))
      .collect();

    let winners: Vec<&str> = ["three", "four"]
      .into_iter()
      .zip(&results)
      .filter(|(_, result)| result.is_ok())
      .map(|(key, _)| key)
      .collect();
    assert_eq!(winners.len(), 1, "exactly one dimension wins: {results:?}");
    assert!(
      results
        .iter()
        .any(|result| matches!(result, Err(KiteError::VectorDimensionMismatch { .. }))),
      "the other is refused: {results:?}"
    );
    let image = db_path.with_extension("image.kitedb");
    std::fs::copy(&db_path, &image).expect("copy");
    let crashed = open_single_file(&image, group_commit_options()).expect("open image");
    for opened in [&*db, &crashed] {
      for key in ["three", "four"] {
        assert_eq!(opened.node_by_key(key).is_some(), winners.contains(&key));
      }
    }
  }

  /// A transaction cannot begin between a commit's MVCC timestamp and its
  /// version chains and delta merge. Begun there, its snapshot would include
  /// the commit while an existing version chain lacks it, and the commit
  /// would appear later in the same snapshot.
  #[test]
  fn transaction_cannot_begin_between_a_commit_timestamp_and_its_merge() {
    use std::sync::mpsc;
    use std::time::Duration;
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("begin-during-publish.kitedb");
    let options = SingleFileOpenOptions::new()
      .auto_checkpoint(false)
      .mvcc(true)
      .mvcc_gc_interval_ms(10);
    let db = Arc::new(open_single_file(&db_path, options).expect("open"));
    db.begin(false).expect("begin");
    let node = db.create_node(Some("counter")).expect("node");
    let count = db.define_propkey("count").expect("propkey");
    db.set_node_prop(node, count, PropValue::I64(0))
      .expect("set count");
    db.commit().expect("commit");
    let read_count = move |db: &SingleFileDB| match db.node_prop(node, count) {
      Some(PropValue::I64(value)) => value,
      other => panic!("unexpected count {other:?}"),
    };

    // A reader open across the next two commits makes both add version
    // chains: count gets one, then the second commit appends to it.
    let (reader_open_tx, reader_open_rx) = mpsc::channel();
    let (release_reader_tx, release_reader_rx) = mpsc::channel::<()>();
    let reader_db = Arc::clone(&db);
    let reader = std::thread::spawn(move || {
      reader_db.begin(true).expect("reader begin");
      reader_open_tx.send(()).expect("signal reader");
      let _ = release_reader_rx.recv();
      reader_db.rollback().expect("reader end");
    });
    reader_open_rx.recv().expect("reader open");
    db.begin(false).expect("begin");
    db.set_node_prop(node, count, PropValue::I64(1))
      .expect("set count");
    db.commit().expect("commit");

    // Right after the next commit takes its timestamp, another thread begins
    // and reads; the commit waits a while for that first read.
    let (first_read_tx, first_read_rx) = mpsc::channel();
    let (second_read_tx, second_read_rx) = mpsc::channel::<()>();
    let late_handle = std::rc::Rc::new(std::cell::RefCell::new(None));
    let late_slot = std::rc::Rc::clone(&late_handle);
    let late_db = Arc::clone(&db);
    db.begin(false).expect("begin");
    db.set_node_prop(node, count, PropValue::I64(2))
      .expect("set count");
    AFTER_NEXT_COMMIT_TIMESTAMP.with(|hook| {
      *hook.borrow_mut() = Some(Box::new(move || {
        let late = std::thread::spawn(move || {
          late_db.begin(true).expect("late begin");
          let first = read_count(&late_db);
          let _ = first_read_tx.send(());
          let _ = second_read_rx.recv();
          let second = read_count(&late_db);
          late_db.rollback().expect("late end");
          (first, second)
        });
        let _ = first_read_rx.recv_timeout(Duration::from_millis(300));
        *late_slot.borrow_mut() = Some(late);
      }));
    });
    db.commit().expect("commit");
    second_read_tx.send(()).expect("second read");
    let late = late_handle.borrow_mut().take().expect("hook ran");
    let (first, second) = late.join().expect("late thread");
    release_reader_tx.send(()).expect("release reader");
    reader.join().expect("reader thread");

    assert_eq!(
      (first, second),
      (2, 2),
      "a transaction begun during a commit's publish saw it appear mid-snapshot"
    );
  }

  /// A group-commit batch whose header cannot be written fails every commit
  /// in it, and none of them comes back once a later commit's header covers
  /// the WAL they were written to.
  #[test]
  fn failed_group_commit_batch_never_becomes_durable() {
    const QUEUED: usize = 3;
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("group-failed-batch.kitedb");
    let db = Arc::new(open_single_file(&db_path, group_commit_options()).expect("open"));

    let (leader, commit_lock) = hold_group_commit_leader(&db);
    let writers: Vec<_> = (0..QUEUED)
      .map(|i| {
        let db = Arc::clone(&db);
        std::thread::spawn(move || {
          db.begin(false)?;
          db.create_node(Some(&format!("failed-{i}")))?;
          db.commit()
        })
      })
      .collect();
    wait_for_queued_commits(&db, QUEUED);
    // The header generation overflows: no header can be written.
    let generation = db.header.read().change_counter;
    db.header.write().change_counter = u64::MAX;
    drop(commit_lock);
    let leader_result = leader.join().expect("leader");
    let results: Vec<Result<()>> = writers
      .into_iter()
      .map(|writer| writer.join().expect("writer"))
      .collect();
    db.header.write().change_counter = generation;
    assert!(leader_result.is_err(), "the leader's batch failed");
    assert!(
      results.iter().all(|result| result.is_err()),
      "every commit of the batch failed: {results:?}"
    );

    db.begin(false).expect("begin");
    db.create_node(Some("after")).expect("create");
    db.commit().expect("commit after the failed batches");
    let failed_keys: Vec<String> = std::iter::once("leader".to_string())
      .chain((0..QUEUED).map(|i| format!("failed-{i}")))
      .collect();
    for key in &failed_keys {
      assert!(db.node_by_key(key).is_none(), "{key} is visible");
    }
    let db = Arc::try_unwrap(db).ok().expect("sole owner");
    close_single_file(db).expect("close");
    let reopened = open_single_file(&db_path, group_commit_options()).expect("reopen");
    assert!(reopened.node_by_key("after").is_some());
    for key in &failed_keys {
      assert!(reopened.node_by_key(key).is_none(), "{key} came back");
    }
  }
}

/// Wave-2 commit-durability reproductions (D1-D4), failing until fixed.
#[cfg(test)]
#[path = "w2_commit_durability_tests.rs"]
mod w2_tests;

/// raydb-b4 commit-pipeline: concurrent MVCC commits.
#[cfg(test)]
#[path = "b4_commit_pipeline_tests.rs"]
mod b4_commit_pipeline_tests;
/// raydb-b4 `mvcc` lane, finding 5: transactions that begin during a commit's
/// publish.
#[cfg(test)]
#[path = "b4_mvcc_commit_tests.rs"]
mod b4_mvcc_commit_tests;
/// raydb-b4 engine-concurrency: group commit and background cuts.
#[cfg(test)]
#[path = "b4_commit_tests.rs"]
mod b4_tests;
