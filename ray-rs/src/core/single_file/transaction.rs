//! Transaction management for SingleFileDB
//!
//! Handles begin, commit, and rollback operations.
//!
//! A write transaction keeps its WAL records to itself (see
//! `SingleFileTxState::wal_deferred_from`): a small one hands them to its
//! commit whole, BEGIN record first, so it takes no WAL lock of its own.
//!
//! Commits go through a commit queue (see `commit_queued`). A committer
//! queues its prepared commit; the first to find no leader leads, and writes
//! everything queued as one group (`write_commits`), in two stages:
//!
//! 1. Under the commit lock (`write_commit_round`): `epoch fence check
//!    (primaries) -> for each member in order: vector check, MVCC conflict
//!    check against the committed transactions and those staged before it
//!    (a conflict aborts that member alone; the others are staged) -> under
//!    the WAL lock, each member's records while they fit (waiting for a
//!    background install if needed) -> one WAL write, without the WAL lock,
//!    and one fsync in Full mode -> one durable header -> each member's
//!    sidecar frame, in order`. Until the header is durable a failure leaves
//!    no trace of the group: its COMMIT records become ROLLBACK records,
//!    MVCC unstages its members, and each fails.
//! 2. Under the publish lock, taken before the commit lock is released, so
//!    groups publish in order (`publish_commits`): `for each member: schema
//!    publish -> one delta.write() section with each member's MVCC commit
//!    timestamp, version chains, vectors and delta merge, in order`. Every
//!    step runs: the group is durable. Meanwhile the lead has passed on, and
//!    the next group is written.
//!
//! An MVCC transaction never begins inside a publish section (see
//! `begin_with_mode`), so a snapshot holds each commit entirely or not at
//! all. The other members wait for their outcome without the commit lock;
//! the commits that arrive while a group is written form the next one.
//! Code that needs the delta and the WAL to agree takes both locks
//! (`lock_commits`).
//!
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
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::thread::ThreadId;
#[cfg(feature = "bench-profile")]
use std::time::Instant;

use super::open::SyncMode;
use super::writer_slot::WriterMode;
use super::{SchemaStaging, SingleFileDB, SingleFileTxState};
use crate::core::pager::FilePager;
use crate::core::wal::buffer::{SealedWrites, WalBuffer};

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
  /// Run on the thread that writes this thread's next commit to the file
  /// (its committer, or the leader of its group), right before the write.
  pub(crate) static DURING_NEXT_COMMIT_IO: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
    std::cell::RefCell::new(None);
}

fn during_commit_io_test_hook() {
  #[cfg(test)]
  if let Some(hook) = DURING_NEXT_COMMIT_IO.with(|hook| hook.borrow_mut().take()) {
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
/// committer, or the leader of its group.
pub(crate) struct CommitRequest {
  txid: TxId,
  bulk_load: bool,
  /// What it read and wrote, for its MVCC conflict check (none: nothing).
  mvcc_keys: Option<MvccKeys>,
  /// The records it kept back (its BEGIN record first, unless written
  /// already), and its COMMIT record last.
  records: Vec<u8>,
  /// The size of its COMMIT record, the last of `records`.
  commit_record_len: usize,
  pending: DeltaState,
  /// Its data records, for the replication sidecar (empty without one).
  pending_wal: Vec<u8>,
  staged_schema: SchemaStaging,
  /// The committing thread; its test hooks fire only there.
  committer: ThreadId,
}

/// A transaction's MVCC reads and writes, and their key groups, handed to
/// the transaction manager in its commit group, under the lock its conflict
/// check takes anyway.
struct MvccKeys {
  reads: TxKeySet,
  writes: TxKeySet,
  groups: Option<TxKeyGroups>,
}

/// How far a commit got, so its committer can settle its guards.
pub(crate) struct CommitOutcome {
  /// The header naming its COMMIT record is durable, so it is committed in
  /// MVCC and merged, whatever `result` says.
  durable: bool,
  /// Its staged schema names are published.
  schema_published: bool,
  result: Result<Option<CommitToken>>,
  /// What is left of its request (the emptied delta, record buffers) and
  /// key sets MVCC released, for its committer to free: off the commit
  /// path, and mostly on the thread that allocated it.
  _leftovers: Option<Box<CommitRequest>>,
  _released_keys: Vec<TxKeySet>,
}

impl CommitOutcome {
  fn failed(error: KiteError) -> Self {
    Self {
      durable: false,
      schema_published: false,
      result: Err(error),
      _leftovers: None,
      _released_keys: Vec::new(),
    }
  }
}

/// Most commits one group writes; the rest form the next group.
const MAX_GROUP_COMMITS: usize = 256;

/// The commit queue: commits wait here for the leader to write them.
#[derive(Default)]
pub(crate) struct CommitQueue {
  pub(crate) state: Mutex<CommitQueueState>,
}

#[derive(Default)]
pub(crate) struct CommitQueueState {
  /// Commits waiting for a group, oldest first.
  pub(crate) queued: VecDeque<QueuedCommit>,
  /// A committer leads: it writes the queued commits, or was handed the lead
  /// and is about to.
  pub(crate) leading: bool,
}

pub(crate) struct QueuedCommit {
  request: Box<CommitRequest>,
  ticket: Arc<CommitTicket>,
}

/// How long a queued committer spins (yielding its CPU) for its outcome
/// before it parks. A group takes microseconds in `SyncMode::Normal`, and
/// waking a parked thread costs its leader a system call per member, so a
/// committer that waits out a group or two without parking frees the
/// leader for the next group sooner.
const COMMIT_WAIT_SPIN: std::time::Duration = std::time::Duration::from_micros(100);

/// A queued commit's slot: its committer waits here until the leader
/// delivers its outcome or hands it the lead.
struct CommitTicket {
  /// `TICKET_*`.
  state: AtomicU8,
  outcome: Mutex<Option<CommitOutcome>>,
  /// The committer, to unpark once it parked.
  committer: std::thread::Thread,
}

/// Queued, its committer spinning.
const TICKET_QUEUED: u8 = 0;
/// Queued, its committer parked (or about to park).
const TICKET_PARKED: u8 = 1;
/// Its outcome is delivered.
const TICKET_DONE: u8 = 2;
/// Its committer is handed the lead.
const TICKET_LEAD: u8 = 3;

impl CommitTicket {
  fn new() -> Self {
    Self {
      state: AtomicU8::new(TICKET_QUEUED),
      outcome: Mutex::new(None),
      committer: std::thread::current(),
    }
  }

  /// Deliver `outcome`.
  fn deliver(&self, outcome: CommitOutcome) {
    *self.outcome.lock() = Some(outcome);
    self.set(TICKET_DONE);
  }

  /// Hand the committer the lead.
  fn hand_lead(&self) {
    self.set(TICKET_LEAD);
  }

  fn set(&self, state: u8) {
    if self.state.swap(state, Ordering::AcqRel) == TICKET_PARKED {
      self.committer.unpark();
    }
  }

  /// Wait until the commit is written (its outcome) or this committer is
  /// handed the lead (`None`). Spins for up to `COMMIT_WAIT_SPIN`, then
  /// parks; `std::thread::park` may return spuriously, so it rechecks.
  fn wait(&self) -> Option<CommitOutcome> {
    let spin_until = std::time::Instant::now() + COMMIT_WAIT_SPIN;
    let mut spins = 0u32;
    loop {
      match self.state.load(Ordering::Acquire) {
        TICKET_DONE => {
          let outcome = self.outcome.lock().take();
          return Some(outcome.unwrap_or_else(|| {
            CommitOutcome::failed(KiteError::Internal(
              "commit outcome delivered twice".to_string(),
            ))
          }));
        }
        TICKET_LEAD => return None,
        TICKET_PARKED => std::thread::park(),
        _ => {
          spins += 1;
          if spins < 64 {
            std::hint::spin_loop();
          } else if std::time::Instant::now() < spin_until {
            std::thread::yield_now();
          } else {
            // Fails if the leader got here first; the loop sees its state.
            let _ = self.state.compare_exchange(
              TICKET_QUEUED,
              TICKET_PARKED,
              Ordering::AcqRel,
              Ordering::Acquire,
            );
          }
        }
      }
    }
  }
}

/// The lead of the commit queue, held while a committer writes groups.
/// Released (`release`, or when dropped), it passes to the oldest queued
/// committer, if any, which then leads. A leader that unwinds also fails the
/// commits of its group it has no outcome for, so their committers stop
/// waiting.
struct CommitLeader<'db> {
  db: &'db SingleFileDB,
  /// The group being written, in order: the queued commits' tickets, and
  /// `None` for the leader's own commit.
  group: Vec<Option<Arc<CommitTicket>>>,
  released: bool,
}

impl CommitLeader<'_> {
  /// Deliver `outcomes`, in group order; returns the leader's own: that of
  /// its own commit, or of the queued one with ticket `own`.
  fn deliver(
    &mut self,
    own: Option<&Arc<CommitTicket>>,
    outcomes: Vec<CommitOutcome>,
  ) -> Option<CommitOutcome> {
    let mut own_outcome = None;
    for (ticket, outcome) in self.group.drain(..).zip(outcomes) {
      match ticket {
        Some(ticket) if !own.is_some_and(|own| Arc::ptr_eq(&ticket, own)) => {
          ticket.deliver(outcome)
        }
        _ => own_outcome = Some(outcome),
      }
    }
    own_outcome
  }

  /// Give up the lead, unless given up already (see `release`).
  fn release_lead(&mut self) {
    if !self.released {
      let db = self.db;
      self.release(&mut db.commit_queue.state.lock());
    }
  }

  /// Give up the lead: to the oldest queued committer, or to whoever queues
  /// next. `state` is the commit queue's, locked.
  fn release(&mut self, state: &mut CommitQueueState) {
    match state.queued.front() {
      Some(next) => next.ticket.hand_lead(),
      None => state.leading = false,
    }
    self.released = true;
  }
}

impl Drop for CommitLeader<'_> {
  fn drop(&mut self) {
    for ticket in self.group.drain(..).flatten() {
      ticket.deliver(CommitOutcome::failed(KiteError::Internal(
        "the commit group writing this commit panicked".to_string(),
      )));
    }
    self.release_lead();
  }
}

/// A commit group's publish section (see `publish_commits`): makes
/// `SingleFileDB::publish_seq` odd until dropped, also on unwind.
struct PublishSection<'a>(&'a AtomicU64);

impl<'a> PublishSection<'a> {
  fn enter(seq: &'a AtomicU64) -> Self {
    seq.fetch_add(1, Ordering::SeqCst);
    Self(seq)
  }
}

impl Drop for PublishSection<'_> {
  fn drop(&mut self) {
    self.0.fetch_add(1, Ordering::SeqCst);
  }
}

/// One round of `SingleFileDB::write_commits`.
#[derive(Default)]
struct CommitRound {
  /// Commits made durable, in WAL order.
  durable: Vec<DurableCommit>,
  /// The next request found the WAL full until this background checkpoint
  /// cut is installed or released.
  wait_for_cut: Option<u64>,
  /// The next request found the WAL full, and no checkpoint in the way.
  wal_full: bool,
}

/// A commit made durable by a round of `SingleFileDB::write_commits`, to
/// publish.
struct DurableCommit {
  /// Its request's index among the commits written.
  index: usize,
  request: Box<CommitRequest>,
  /// Its replication commit token (a primary's sidecar took its frame).
  token: Option<CommitToken>,
}

/// Check `request`'s transaction for MVCC conflicts before its COMMIT record
/// is written, once its reads and writes join its sets: with the
/// transactions committed since it began, and those staged before it
/// (earlier members of its group, or of a group still publishing, committed
/// in MVCC only once durable). A conflict aborts it; otherwise it is staged
/// (`TxManager::stage_commit`), so later commits check against its writes.
/// Callers hold the commit lock, so nothing else stages before it commits in
/// MVCC (`SingleFileDB::commit_in_mvcc`).
fn check_and_stage_in_mvcc(
  mvcc: &crate::mvcc::MvccManager,
  tx_mgr: &mut crate::mvcc::TxManager,
  request: &mut CommitRequest,
) -> Result<()> {
  let txid = request.txid;
  if tx_mgr.tx(txid).is_none() {
    return Err(KiteError::Internal(format!(
      "transaction {txid} is not active in MVCC"
    )));
  }
  if let Some(keys) = request.mvcc_keys.take() {
    tx_mgr.record_reads_and_writes(txid, keys.reads, keys.writes, keys.groups);
  }
  if let Err(err) = mvcc.conflict_detector.validate_commit(tx_mgr, txid) {
    tx_mgr.abort_tx(txid);
    let mut keys = err.conflicting_keys;
    keys.sort_unstable();
    keys.dedup();
    return Err(KiteError::Conflict { txid, keys });
  }
  tx_mgr
    .stage_commit(txid)
    .map(|_| ())
    .map_err(|error| KiteError::Internal(error.to_string()))
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
  /// Its BEGIN record is in the WAL and its COMMIT or ROLLBACK is not, so it
  /// is among the open transactions (`open_write_txids`).
  wrote_begin: bool,
  /// How it holds the writer slot (write transactions), released here.
  writer: Option<WriterMode>,
}

impl Drop for ActiveTransactionGuard<'_> {
  fn drop(&mut self) {
    self.db.transaction_finished(self.txid, self.wrote_begin);
    if let Some(mode) = self.writer {
      self.db.tx_shared.writer.release(mode);
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

/// A point in a write transaction to roll back to: what the transaction had
/// changed when [`SingleFileDB::savepoint`] took it. Rolling back to it
/// ([`SingleFileDB::rollback_to`]) undoes the transaction's changes since and
/// keeps it; releasing it ([`SingleFileDB::release_savepoint`]) keeps them.
/// Savepoints nest: rolling back to or releasing one ends every savepoint
/// taken after it. A savepoint belongs to the transaction that took it.
#[derive(Debug)]
pub struct Savepoint {
  txid: TxId,
  id: u64,
  pending: DeltaState,
  schema: SchemaStaging,
  mvcc_writes: TxKeySet,
  pending_wal_len: usize,
}

impl Savepoint {
  /// The transaction it belongs to.
  pub fn txid(&self) -> TxId {
    self.txid
  }
}

/// Where `savepoint` is among `tx`'s live savepoints.
fn live_savepoint(tx: &SingleFileTxState, savepoint: &Savepoint) -> Result<usize> {
  if tx.txid != savepoint.txid {
    return Err(KiteError::InvalidSavepoint(format!(
      "it belongs to transaction {}, not {}",
      savepoint.txid, tx.txid
    )));
  }
  tx.savepoints
    .iter()
    .position(|&id| id == savepoint.id)
    .ok_or_else(|| {
      KiteError::InvalidSavepoint(
        "it was released, or the transaction rolled back to or released an earlier one".into(),
      )
    })
}

/// Names `now` stages that `then` did not, per kind (labels, edge types,
/// property keys).
fn names_staged_since(now: &SchemaStaging, then: &SchemaStaging) -> [Vec<String>; 3] {
  fn since<Id>(now: &HashMap<String, Id>, then: &HashMap<String, Id>) -> Vec<String> {
    now
      .keys()
      .filter(|name| !then.contains_key(*name))
      .cloned()
      .collect()
  }
  [
    since(&now.label_names, &then.label_names),
    since(&now.etype_names, &then.etype_names),
    since(&now.propkey_names, &then.propkey_names),
  ]
}

impl SingleFileDB {
  fn begin_with_mode(&self, read_only: bool, bulk_load: bool) -> Result<TxId> {
    if self.read_only && !read_only {
      return Err(KiteError::ReadOnly);
    }
    if bulk_load && read_only {
      return Err(KiteError::ReadOnly);
    }

    // Only this thread registers its own transaction, so checking before the
    // gate is race-free. It must come first: a blocking checkpoint holding
    // the gate may be waiting for this thread's open transaction.
    if self.current_tx_handle().is_some() {
      return Err(KiteError::TransactionInProgress);
    }
    self.reap_abandoned_transactions();
    // Write transactions claim the writer slot (see `writer_slot`): MVCC ones
    // together, a bulk load or a write transaction without MVCC alone. Taken
    // before the checkpoint gate, holding nothing; a failed begin releases
    // it.
    let writer_claim = (!read_only).then(|| {
      let mode = if bulk_load || self.mvcc.is_none() {
        WriterMode::Exclusive
      } else {
        WriterMode::Shared
      };
      self.tx_shared.writer.claim(mode)
    });

    // A checkpoint takes the write side. Holding this read permit through
    // registration makes the gate atomic with transaction creation. No WAL
    // record is written here: a write transaction's BEGIN record goes with
    // its other records (see `SingleFileTxState::wal_deferred_from`).
    let _checkpoint_gate = self.checkpoint_gate.read();
    let (txid, snapshot_ts) = if let Some(mvcc) = self.mvcc.as_ref() {
      // A commit group takes its members' timestamps, adds their version
      // chains and merges them into the delta in one `delta.write()` section
      // (see `publish_commits`), so this snapshot holds each entirely or not
      // at all, and a commit that has not taken its timestamp yet sees this
      // transaction as a reader that needs version chains. A begin does not
      // complete inside such a section: it waits for the section, and
      // begins again if one started meanwhile (`publish_seq` is odd during
      // one). Outside them it takes no delta lock.
      let (txid, snapshot_ts) = loop {
        let seq = self.publish_seq.load(Ordering::SeqCst);
        if seq % 2 == 1 {
          drop(self.delta.read());
          continue;
        }
        let begun = mvcc.tx_manager.lock().begin_tx();
        if self.publish_seq.load(Ordering::SeqCst) == seq {
          break begun;
        }
        mvcc.tx_manager.lock().abort_tx(begun.0);
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

    let mut tx_state = SingleFileTxState::new(txid, read_only, snapshot_ts, bulk_load);
    tx_state.writer = writer_claim.as_ref().map(|claim| claim.mode());
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

  /// Run `write` under the WAL lock. If the WAL refuses the record because
  /// the secondary region filled during a background checkpoint, returns the
  /// cut to wait for: the caller releases every lock that checkpoint needs
  /// (the checkpoint gate, the commit lock), waits with
  /// `wait_for_cut_release`, and retries. Refused records are not written.
  ///
  /// Records are only buffered here, so this never waits for file I/O: a
  /// commit group writes the buffered records without the WAL lock (see
  /// `WalBuffer::seal`).
  pub(crate) fn try_write_wal<T>(
    &self,
    write: impl FnOnce(&mut WalBuffer) -> Result<T>,
  ) -> Result<WalWrite<T>> {
    let mut wal = self.wal_buffer.lock();
    match write(&mut wal) {
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
      let written = self.try_write_wal(|wal| {
        wal.write_built_record(record)?;
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

  /// Write the records `tx_handle`'s transaction kept back (see
  /// `SingleFileTxState::wal_deferred_from`) to the WAL, after its BEGIN
  /// record if that is not there yet, and then `record` (a record it logs
  /// now, or none); from then on it writes its records as it makes them.
  /// Callers checked that no savepoint is live. On error nothing is written,
  /// and the records stay kept back.
  fn write_deferred_records(
    &self,
    tx_handle: &Arc<Mutex<SingleFileTxState>>,
    record: &[u8],
  ) -> Result<()> {
    let (txid, from, mut records, writes_begin) = {
      let tx = tx_handle.lock();
      let Some(from) = tx.wal_deferred_from else {
        return Ok(());
      };
      let kept = tx.pending_wal.len() - from;
      let mut records = if tx.wal_begun {
        Vec::with_capacity(kept + record.len())
      } else {
        WalRecord::new(WalRecordType::Begin, tx.txid, build_begin_payload()).build()
      };
      records.extend_from_slice(&tx.pending_wal[from..]);
      records.extend_from_slice(record);
      (tx.txid, from, records, !tx.wal_begun)
    };
    // No lock is held here: a background checkpoint may need to install
    // before the WAL takes the records. A transaction whose BEGIN record is
    // written joins the open set under the WAL lock, which a background cut
    // holds while it reads that set.
    loop {
      let written = self.try_write_wal(|wal| {
        wal.write_owned_record_bytes(&mut records)?;
        if writes_begin {
          self.open_write_txids.lock().insert(txid);
        }
        Ok(())
      })?;
      match written {
        WalWrite::Written(()) => break,
        WalWrite::BlockedOn(cut) => self.wait_for_cut_release(cut)?,
        WalWrite::NeedsCompaction => self.compact_retired_wal()?,
      }
    }
    let mut tx = tx_handle.lock();
    tx.wal_begun = true;
    tx.wal_deferred_from = None;
    // Only a primary's commit reads the copy (for the replication sidecar).
    if self.primary_replication.is_none() {
      tx.pending_wal.truncate(from);
    } else {
      tx.pending_wal.extend_from_slice(record);
    }
    Ok(())
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
    let _commit_guard = self.lock_commits();
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

  /// Begin a bulk-load transaction: the fast path for loading data, with or
  /// without MVCC.
  ///
  /// It runs alone among writers: it waits for the open write transactions
  /// to finish, and write transactions that begin while it is open, or while
  /// it waits, wait for it. Its WAL records are written at commit, in one
  /// batch, and it records nothing for MVCC conflict checks (no write
  /// transaction runs beside it). Readers never wait for it: reads outside a
  /// transaction see it whole once it commits, and a read transaction that
  /// began before its commit never sees it (snapshot isolation; its commit
  /// keeps the version history such a reader needs, as any commit does).
  pub fn begin_bulk(&self) -> Result<TxId> {
    self.begin_with_mode(false, true)
  }

  /// Begin a bulk-load transaction guard (rolls back on drop); see
  /// [`Self::begin_bulk`].
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

  /// Take a savepoint in the calling thread's write transaction (see
  /// [`Savepoint`]).
  ///
  /// It copies the transaction's pending changes, so it costs time and memory
  /// in proportion to what the transaction changed so far. While a savepoint
  /// is live, the transaction's WAL records stay in memory (see
  /// `SingleFileTxState::wal_deferred_from`), so rolling back never leaves
  /// one in the WAL.
  pub fn savepoint(&self) -> Result<Savepoint> {
    let (_, handle) = self.require_schema_tx_handle()?;
    let mut tx = handle.lock();
    let id = tx.next_savepoint_id;
    tx.next_savepoint_id += 1;
    if !tx.bulk_load && tx.wal_deferred_from.is_none() {
      tx.wal_deferred_from = Some(tx.pending_wal.len());
    }
    tx.savepoints.push(id);
    Ok(Savepoint {
      txid: tx.txid,
      id,
      pending: tx.pending.clone(),
      schema: tx.schema.clone(),
      mvcc_writes: tx.mvcc_writes.clone(),
      pending_wal_len: tx.pending_wal.len(),
    })
  }

  /// Undo what the calling thread's transaction changed since `savepoint`:
  /// its writes and their WAL records, the schema names it defined (and its
  /// claims on them), and the MVCC writes it recorded, so they cause no
  /// conflict. Its reads stay recorded: what it does next may depend on
  /// them. `savepoint` stays live; savepoints taken after it end.
  pub fn rollback_to(&self, savepoint: &Savepoint) -> Result<()> {
    let handle = self.current_tx_handle().ok_or(KiteError::NoTransaction)?;
    let (txid, [labels, etypes, propkeys]) = {
      let mut tx = handle.lock();
      let position = live_savepoint(&tx, savepoint)?;
      tx.savepoints.truncate(position + 1);
      let staged_since = names_staged_since(&tx.schema, &savepoint.schema);
      tx.pending = savepoint.pending.clone();
      tx.schema = savepoint.schema.clone();
      tx.mvcc_writes = savepoint.mvcc_writes.clone();
      // A live savepoint keeps every record since it in memory.
      tx.pending_wal.truncate(savepoint.pending_wal_len);
      (tx.txid, staged_since)
    };
    for name in &labels {
      self.release_label_reservation(name, txid);
    }
    for name in &etypes {
      self.release_etype_reservation(name, txid);
    }
    for name in &propkeys {
      self.release_propkey_reservation(name, txid);
    }
    Ok(())
  }

  /// Release `savepoint`, keeping what the calling thread's transaction
  /// changed since; savepoints taken after it end too. Once no savepoint is
  /// live, the WAL records kept in memory are written if the transaction
  /// writes its records itself (see `SingleFileTxState::wal_deferred_from`),
  /// or else by its commit, as are those the WAL cannot take now.
  pub fn release_savepoint(&self, savepoint: Savepoint) -> Result<()> {
    let handle = self.current_tx_handle().ok_or(KiteError::NoTransaction)?;
    let write_now = {
      let mut tx = handle.lock();
      let position = live_savepoint(&tx, &savepoint)?;
      tx.savepoints.truncate(position);
      tx.savepoints.is_empty()
        && !tx.bulk_load
        && tx
          .wal_deferred_from
          .is_some_and(|from| tx.wal_begun || tx.pending_wal.len() - from > super::WAL_DEFER_BYTES)
    };
    if write_now {
      // A failure leaves them to the commit.
      let _ = self.write_deferred_records(&handle, &[]);
    }
    Ok(())
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
    let (
      txid,
      read_only,
      bulk_load,
      writer,
      pending,
      pending_wal,
      staged_schema,
      deferred,
      wal_begun,
      mvcc_keys,
    ) = {
      let mut tx = tx_handle.lock();
      let pending = std::mem::take(&mut tx.pending);
      let staged_schema = std::mem::take(&mut tx.schema);
      let pending_wal = std::mem::take(&mut tx.pending_wal);
      // Its reads and writes join its MVCC sets right before its conflict
      // check, in its group's (`write_commit_round`).
      let reads = std::mem::take(&mut tx.mvcc_reads);
      let writes = std::mem::take(&mut tx.mvcc_writes);
      let mvcc_keys =
        (self.mvcc.is_some() && !(reads.is_empty() && writes.is_empty())).then(|| {
          // Grouped here, without any lock: with other transactions open, the
          // check and the commit, which every other commit waits for, then
          // look up and note groups instead of keys. Alone, it commits with no
          // check and nothing to note.
          let groups = (self.active_transactions.load(Ordering::Acquire) > 1)
            .then(|| TxKeyGroups::of(&reads, &writes));
          MvccKeys {
            reads,
            writes,
            groups,
          }
        });
      (
        tx.txid,
        tx.read_only,
        tx.bulk_load,
        tx.writer.take(),
        pending,
        pending_wal,
        staged_schema,
        tx.wal_deferred_from,
        tx.wal_begun,
        mvcc_keys,
      )
    };
    // Dropped last: the transaction counts as active (blocking checkpoints
    // wait for it) until its commit is settled. For background cuts it stops
    // counting as open once its COMMIT is durable (`publish_commits`).
    let mut active_transaction_guard = ActiveTransactionGuard {
      db: self,
      txid,
      wrote_begin: wal_begun,
      writer,
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

    // The records it kept back go with its COMMIT record, in one batch, so a
    // WAL that refuses them is left without a partial copy: a bulk load's
    // whole transaction, and a write transaction's records since its BEGIN
    // record (with it, if not written yet) or since a savepoint.
    let commit = WalRecord::new(WalRecordType::Commit, txid, build_commit_payload()).build();
    let commit_record_len = commit.len();
    let records = match (bulk_load, deferred) {
      (false, None) => commit,
      (_, deferred) => {
        let from = if bulk_load { 0 } else { deferred.unwrap_or(0) };
        let mut records = if wal_begun {
          Vec::with_capacity(pending_wal.len() - from + commit.len())
        } else {
          WalRecord::new(WalRecordType::Begin, txid, build_begin_payload()).build()
        };
        records.extend_from_slice(&pending_wal[from..]);
        records.extend_from_slice(&commit);
        records
      }
    };
    let request = Box::new(CommitRequest {
      txid,
      bulk_load,
      mvcc_keys,
      records,
      commit_record_len,
      pending,
      pending_wal,
      staged_schema,
      committer: std::thread::current().id(),
    });

    #[cfg(test)]
    self.commits_waiting.fetch_add(1, Ordering::SeqCst);
    let outcome = self.commit_queued(request);
    #[cfg(test)]
    self.commits_waiting.fetch_sub(1, Ordering::SeqCst);
    mvcc_abort.armed = !outcome.durable;
    // Its publish took it out of the open set already.
    active_transaction_guard.wrote_begin &= !outcome.durable;
    if outcome.schema_published {
      schema_reservation_guard.disarm();
    }
    outcome.result
  }

  /// Queue `request` and return its outcome once it is written. The first
  /// committer to find no leader leads: it takes everything queued (at most
  /// `MAX_GROUP_COMMITS`) and writes it as one group (`write_commits`: one
  /// WAL write, one sync if the mode asks for one, and one header for all).
  /// The others wait without the commit lock, so the commits that arrive
  /// while a group is written form the next one, which the oldest of them
  /// leads once the group is durable. Nobody sleeps to wait for more
  /// commits.
  fn commit_queued(&self, request: Box<CommitRequest>) -> CommitOutcome {
    let mut state = self.commit_queue.state.lock();
    if !state.leading {
      state.leading = true;
      drop(state);
      return self.lead_commits(Some(request), None);
    }
    let ticket = Arc::new(CommitTicket::new());
    state.queued.push_back(QueuedCommit {
      request,
      ticket: Arc::clone(&ticket),
    });
    drop(state);
    match ticket.wait() {
      Some(outcome) => outcome,
      // Handed the lead: this commit is the oldest queued.
      None => self.lead_commits(None, Some(&ticket)),
    }
  }

  /// Lead the commit queue (see `commit_queued`): write one group, this
  /// committer's commit (`own_request`, or the queued one of `own_ticket`)
  /// and the commits queued before the leader took them, at most
  /// `MAX_GROUP_COMMITS`. Once the group is durable the lead passes on, so
  /// the next group is written while this one publishes. Returns this
  /// committer's outcome.
  fn lead_commits(
    &self,
    own_request: Option<Box<CommitRequest>>,
    own_ticket: Option<&Arc<CommitTicket>>,
  ) -> CommitOutcome {
    let mut leader = CommitLeader {
      db: self,
      group: Vec::new(),
      released: false,
    };
    before_commit_lock_test_hook();
    let mut requests = Vec::new();
    if let Some(request) = own_request {
      requests.push(request);
      leader.group.push(None);
    }
    {
      let mut state = self.commit_queue.state.lock();
      let take = state.queued.len().min(MAX_GROUP_COMMITS - requests.len());
      for queued in state.queued.drain(..take) {
        leader.group.push(Some(queued.ticket));
        requests.push(queued.request);
      }
    }
    let outcomes = self.write_commits(requests, &mut leader);
    leader.deliver(own_ticket, outcomes).unwrap_or_else(|| {
      CommitOutcome::failed(KiteError::Internal("commit was not written".to_string()))
    })
  }

  /// Write `requests`' commits in order, and return their outcomes in the
  /// same order. Each round, under the commit lock, writes the COMMIT records
  /// of those that fit and makes them durable with one WAL write and one
  /// header (`write_commit_round`); then, under the publish lock, which it
  /// takes before it releases the commit lock (so rounds publish in commit
  /// order), publishes them (`publish_commits`). Once a round leaves nothing
  /// of `requests` to write, `leader` passes the lead on before the publish.
  /// Callers hold no lock.
  // A commit is boxed once, and moves by pointer from its committer through
  // the queue and a group's stages.
  #[allow(clippy::vec_box)]
  fn write_commits(
    &self,
    requests: Vec<Box<CommitRequest>>,
    leader: &mut CommitLeader<'_>,
  ) -> Vec<CommitOutcome> {
    let mut outcomes: Vec<Option<CommitOutcome>> = requests.iter().map(|_| None).collect();
    let mut queue: VecDeque<(usize, Box<CommitRequest>)> =
      requests.into_iter().enumerate().collect();
    let mut checkpointed_for_room = false;
    while !queue.is_empty() {
      #[cfg(feature = "bench-profile")]
      let commit_lock_start = Instant::now();
      let commit_guard = self.commit_lock.lock();
      #[cfg(feature = "bench-profile")]
      self.commit_lock_wait_ns.fetch_add(
        commit_lock_start.elapsed().as_nanos() as u64,
        Ordering::Relaxed,
      );

      let round = self.write_commit_round(&mut queue, &mut outcomes);
      let publish_guard = self.publish_lock.lock();
      drop(commit_guard);
      if queue.is_empty() {
        leader.release_lead();
      }
      for (index, outcome) in self.publish_commits(round.durable) {
        outcomes[index] = Some(outcome);
      }
      drop(publish_guard);

      // The background checkpoint takes the commit lock to install.
      if let Some(cut) = round.wait_for_cut {
        if let Err(error) = self.wait_for_cut_release(cut) {
          if let Some((index, _)) = queue.pop_front() {
            outcomes[index] = Some(CommitOutcome::failed(error));
          }
        }
      }
      // A background checkpoint makes room without waiting for the open
      // transactions (these among them); a blocking one would wait for them
      // forever. Once one ran, a commit that still does not fit fails.
      if round.wal_full {
        let made_room = !checkpointed_for_room
          && self.background_checkpoint
          && self.auto_checkpoint_if_needed(true);
        checkpointed_for_room |= made_room;
        if !made_room {
          if let Some((index, _)) = queue.pop_front() {
            outcomes[index] = Some(CommitOutcome::failed(KiteError::WalBufferFull));
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

  /// Hold off commits: take the commit lock, and wait until every commit
  /// that took it before has published (merged into the delta). Callers
  /// that need the delta, the vector stores and the WAL to agree
  /// (checkpoints, exports, backups, vector store creation) take this instead
  /// of the bare commit lock: a commit publishes after it releases it.
  pub(crate) fn lock_commits(&self) -> parking_lot::MutexGuard<'_, ()> {
    let commit_guard = self.commit_lock.lock();
    drop(self.publish_lock.lock());
    commit_guard
  }

  /// One round of `write_commits`, under the commit lock: append the COMMIT
  /// records of `queue`'s requests, in order, while they fit, each after its
  /// MVCC conflict check stages it, then make them durable
  /// (`persist_commit_round`). Returns the durable ones, to publish; a
  /// request refused before that gets its outcome in `outcomes`, with nothing
  /// of it recorded.
  ///
  /// The pager lock is held from the first record to the header, so no one
  /// reads, flushes or names the WAL past the round's records meanwhile. The
  /// WAL lock is held only to append and seal them: transactions keep
  /// appending while the round's file I/O runs, after its records. If the
  /// round fails before its header is durable, its COMMIT records become
  /// ROLLBACK records (`WalBuffer::restore_sealed`), so a failed commit never
  /// becomes durable later, and MVCC unstages its members.
  fn write_commit_round(
    &self,
    queue: &mut VecDeque<(usize, Box<CommitRequest>)>,
    outcomes: &mut [Option<CommitOutcome>],
  ) -> CommitRound {
    let mut round = CommitRound::default();
    // Vector checks read the stores, which a group still publishing may be
    // changing (creating one, or adding to it): let it finish first.
    if queue
      .iter()
      .any(|(_, request)| !request.pending.pending_vectors.is_empty())
    {
      drop(self.publish_lock.lock());
    }
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

    // Vector and MVCC checks, in order, without the WAL lock: each commit
    // that passes is staged, so the ones after it check against its writes.
    // The transaction manager's lock is taken once for the commits without
    // vectors (their check takes the vector stores' lock).
    let mut checked = VecDeque::with_capacity(queue.len());
    {
      // Dimensions given by commits earlier in the round to vector
      // properties that have no store yet.
      let mut claimed_dimensions = HashMap::new();
      let mut tx_mgr: Option<parking_lot::MutexGuard<'_, crate::mvcc::TxManager>> = None;
      for (index, mut request) in queue.drain(..) {
        let new_dimensions = if request.pending.pending_vectors.is_empty() {
          HashMap::new()
        } else {
          tx_mgr = None;
          match self.check_commit_vectors(&request.pending.pending_vectors, &claimed_dimensions) {
            Ok(new_dimensions) => new_dimensions,
            Err(error) => {
              outcomes[index] = Some(CommitOutcome::failed(error));
              continue;
            }
          }
        };
        if let Some(mvcc) = self.mvcc.as_ref() {
          let tx_mgr = tx_mgr.get_or_insert_with(|| mvcc.tx_manager.lock());
          if let Err(error) = check_and_stage_in_mvcc(mvcc, tx_mgr, &mut request) {
            outcomes[index] = Some(CommitOutcome::failed(error));
            continue;
          }
        }
        claimed_dimensions.extend(new_dimensions);
        checked.push_back((index, request));
      }
    }

    // Their records, in order, while they fit. Those that do not wait for
    // the next round, unstaged (they are the newest staged), and are checked
    // again then.
    let mut pager = self.pager.lock();
    let mut staged = Vec::new();
    // The WAL position of each staged commit's COMMIT record.
    let mut commit_records = Vec::new();
    let sealed = {
      let mut wal = self.wal_buffer.lock();
      while let Some((index, mut request)) = checked.pop_front() {
        if wal.can_fit(request.records.len()) {
          if let Err(error) = wal.write_owned_record_bytes(&mut request.records) {
            self.unstage_newest_in_mvcc(checked.len() + 1);
            outcomes[index] = Some(CommitOutcome::failed(error));
            queue.extend(checked.drain(..));
            break;
          }
          commit_records.push((wal.head() - request.commit_record_len as u64, request.txid));
          staged.push((index, request));
          continue;
        }
        self.unstage_newest_in_mvcc(checked.len() + 1);
        // Make what fits durable first; this one waits for the next round.
        if !staged.is_empty() {
          queue.push_back((index, request));
          queue.extend(checked.drain(..));
          break;
        }
        // Nothing of this round is recorded yet, so make room and retry.
        if wal.is_primary_retired() {
          let mut header = self.header.write();
          match self.compact_retained_wal(&mut pager, &mut wal, &mut header) {
            Ok(()) => queue.push_back((index, request)),
            Err(error) => outcomes[index] = Some(CommitOutcome::failed(error)),
          }
        } else if let Some(cut) = self.cut_blocking_wal_writes(&wal) {
          queue.push_back((index, request));
          round.wait_for_cut = Some(cut);
        } else {
          queue.push_back((index, request));
          round.wal_full = true;
        }
        queue.extend(checked.drain(..));
        break;
      }
      if staged.is_empty() {
        return round;
      }
      // `SyncMode::Off` leaves the records buffered (checkpoints and close
      // write them) and names them in the in-memory header only.
      (self.sync_mode != SyncMode::Off).then(|| wal.seal())
    };

    match self.persist_commit_round(&mut pager, sealed.as_ref(), staged.len()) {
      Ok(()) => {
        drop(pager);
        round.durable = self.settle_durable_commits(staged);
      }
      Err(error) => {
        if let Some(sealed) = sealed {
          let mut wal = self.wal_buffer.lock();
          let scrubbed = wal
            .restore_sealed(sealed, commit_records)
            .and_then(|()| wal.flush(&mut pager));
          if let Err(scrub) = scrubbed {
            eprintln!(
              "Warning: could not make the failed commits' rollback durable; the next header \
               syncs it first: {scrub}"
            );
          }
        }
        // This round's commits are the newest staged; those of a round
        // still publishing stay staged.
        self.unstage_newest_in_mvcc(staged.len());
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

  /// Make a round's commits durable as the sync mode asks: write the sealed
  /// WAL bytes (and in Full mode fsync them) before a header names them,
  /// then install that header. `commits` is the number of commits the round
  /// staged. A header written before its WAL bytes names bytes a crash can
  /// leave unwritten, where recovery reads stale records of an earlier WAL
  /// cycle. On error the in-memory header is as it was, but for its newer
  /// change counter (the next header must outrank every slot on disk).
  ///
  /// `sealed` is `None` in `SyncMode::Off`, which writes nothing.
  fn persist_commit_round(
    &self,
    pager: &mut FilePager,
    sealed: Option<&SealedWrites>,
    commits: usize,
  ) -> Result<()> {
    during_commit_io_test_hook();
    if let Some(sealed) = sealed {
      #[cfg(feature = "bench-profile")]
      let flush_start = Instant::now();
      let written = sealed.write(pager).and_then(|()| {
        // A failed round's rewritten records may not be durable yet, and
        // this header names bytes past them: make them durable first.
        if self.sync_mode == SyncMode::Full || sealed.needs_sync() {
          pager.sync_data()?;
          self.wal_buffer.lock().note_sealed_synced(sealed);
        }
        Ok(())
      });
      #[cfg(feature = "bench-profile")]
      self
        .wal_flush_ns
        .fetch_add(flush_start.elapsed().as_nanos() as u64, Ordering::Relaxed);
      written?;
    }

    // MVCC commits the staged commits in order, this round's last.
    let last_commit_ts = match self.mvcc.as_ref() {
      Some(mvcc) => {
        let tx_mgr = mvcc.tx_manager.lock();
        tx_mgr
          .newest_staged_ts()
          .unwrap_or_else(|| tx_mgr.next_commit_ts() + (commits as u64).saturating_sub(1))
      }
      None => std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0),
    };
    let wal_state = match sealed {
      Some(sealed) => *sealed.state(),
      None => self.wal_buffer.lock().region_state(),
    };
    let mut header = self.header.write();
    let prior = header.clone();
    wal_state.store_in_header(&mut header);
    header.max_node_id = self
      .next_node_id
      .load(std::sync::atomic::Ordering::SeqCst)
      .saturating_sub(1);
    header.next_tx_id = self.next_tx_id.load(std::sync::atomic::Ordering::SeqCst);
    header.last_commit_ts = last_commit_ts;
    if sealed.is_some() {
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

  /// The steps of a round's durable commits that stay under the commit
  /// lock, in WAL order: they stop counting as open, and a primary's
  /// replication sidecar takes their frames. Returns them, to publish.
  ///
  /// A background cut (which takes the commit lock) must no longer count
  /// them as open: their records end with a durable COMMIT. Their committers
  /// learn that only later, and a cut taken meanwhile would skip their
  /// records, its install drop them, and the next cut find an open
  /// transaction with no BEGIN record and decline. The sidecar frames are
  /// appended under the commit lock, in order, with the epoch fence checked
  /// under it (`write_commit_round`), so a copy of the database taken under
  /// the commit lock never holds a commit its frame position misses.
  fn settle_durable_commits(&self, staged: Vec<(usize, Box<CommitRequest>)>) -> Vec<DurableCommit> {
    {
      let mut open = self.open_write_txids.lock();
      for (_, request) in &staged {
        open.remove(&request.txid);
      }
    }
    staged
      .into_iter()
      .map(|(index, mut request)| {
        let mut token = None;
        if let Some(replication) = self.primary_replication.as_ref() {
          if replication.crash_after_local_commit_for_testing() {
            // Test-only abrupt-stop hook for the exact local-durable/sidecar
            // boundary: the main WAL and header are complete.
            std::process::abort();
          }
          match replication
            .append_commit_wal_frame(request.txid, std::mem::take(&mut request.pending_wal))
          {
            Ok(commit_token) => token = Some(commit_token),
            Err(error) => {
              eprintln!(
                "Warning: local commit durable but replication sidecar append failed: {error}"
              )
            }
          }
        }
        DurableCommit {
          index,
          request,
          token,
        }
      })
      .collect()
  }

  /// Make a round's durable commits visible, in WAL order, under the publish
  /// lock (rounds publish in the order they took the commit lock): publish
  /// their schema; then, in one `delta.write()` critical section, for each
  /// in order, commit it in MVCC (its timestamp), add its version chains,
  /// apply its vectors and merge it into the delta. Transactions begin under
  /// `delta.read()`, so none begins inside the section, and each one begun
  /// before counts as a reader that needs version chains.
  ///
  /// Every step runs for every commit even if an earlier one fails: stopping
  /// early would leave a durable transaction out of the delta, invisible
  /// until a reopen replays it, and the next checkpoint (a snapshot of the
  /// delta) would drop it. Each commit reports its first failure.
  fn publish_commits(&self, round: Vec<DurableCommit>) -> Vec<(usize, CommitOutcome)> {
    if round.is_empty() {
      return Vec::new();
    }
    let this_thread = std::thread::current().id();

    // This is the schema visibility point, right after the durable commit
    // boundary. Publishing before any fallible post-commit work keeps a
    // later error from leaving a committed WAL definition hidden in this
    // process.
    let mut published: Vec<(DurableCommit, Result<()>)> = round
      .into_iter()
      .map(|commit| {
        let schema = self.publish_staged_schema(&commit.request.staged_schema);
        (commit, schema)
      })
      .collect();
    for (commit, _) in &published {
      if commit.request.committer == this_thread {
        before_merge_test_hook();
      }
    }

    let mut results = Vec::with_capacity(published.len());
    let mut delta = self.delta.write();
    let _publishing = PublishSection::enter(&self.publish_seq);
    let (mvcc_commits, mut released_keys) =
      self.commit_in_mvcc(published.iter().map(|(commit, _)| commit.request.txid));
    for ((commit, _), mvcc_commit) in published.iter_mut().zip(mvcc_commits) {
      let request = &mut commit.request;
      let on_committer_thread = request.committer == this_thread;
      if on_committer_thread {
        after_commit_timestamp_test_hook();
      }
      let commit_ts_for_mvcc = mvcc_commit.as_ref().ok().copied().flatten();
      self.apply_mvcc_commit(commit_ts_for_mvcc, request.txid, &request.pending, &delta);

      // The stores are loaded and the dimensions checked (`write_commit_round`).
      let vector_fault = if on_committer_thread {
        post_durable_test_fault()
      } else {
        Ok(())
      };
      let vector_result =
        vector_fault.and_then(|()| self.apply_pending_vectors(&request.pending.pending_vectors));

      delta.merge_from(&mut request.pending);
      results.push(mvcc_commit.map(|_| ()).and(vector_result));
    }
    drop(_publishing);
    drop(delta);

    published
      .into_iter()
      .zip(results)
      .map(|((commit, schema_result), result)| {
        let outcome = CommitOutcome {
          durable: true,
          schema_published: schema_result.is_ok(),
          result: schema_result.and(result).map(|()| commit.token),
          _leftovers: Some(commit.request),
          _released_keys: std::mem::take(&mut released_keys),
        };
        (commit.index, outcome)
      })
      .collect()
  }

  /// Unstage the `count` commits `check_and_stage_in_mvcc` staged last,
  /// whose records the WAL did not take this round.
  fn unstage_newest_in_mvcc(&self, count: usize) {
    if let Some(mvcc) = self.mvcc.as_ref() {
      let mut tx_mgr = mvcc.tx_manager.lock();
      for _ in 0..count {
        tx_mgr.unstage_last();
      }
    }
  }

  /// Commit `txids` in MVCC, in order, if enabled, at their durable point:
  /// for each, its commit timestamp, and whether a transaction that may still
  /// read was active once it committed (which then needs version chains).
  /// Callers hold `delta.write()` (see `publish_commits`) and staged them
  /// (`check_and_stage_in_mvcc`).
  /// Also returns the key sets the commits released, to free without the
  /// locks.
  #[allow(clippy::type_complexity)]
  fn commit_in_mvcc(
    &self,
    txids: impl Iterator<Item = TxId>,
  ) -> (Vec<Result<Option<(u64, bool)>>>, Vec<TxKeySet>) {
    let Some(mvcc) = self.mvcc.as_ref() else {
      return (txids.map(|_| Ok(None)).collect(), Vec::new());
    };
    let mut tx_mgr = mvcc.tx_manager.lock();
    let mut released = Vec::new();
    let committed = txids
      .map(|txid| {
        let commit_ts = tx_mgr
          .commit_tx_releasing(txid, &mut released)
          .map_err(|e| KiteError::Internal(e.to_string()))?;
        Ok(Some((commit_ts, tx_mgr.has_open_readers())))
      })
      .collect();
    (committed, released)
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
    let (txid, read_only, wal_begun, writer) = {
      let mut tx = tx_handle.lock();
      (tx.txid, tx.read_only, tx.wal_begun, tx.writer.take())
    };
    let _active_transaction_guard = ActiveTransactionGuard {
      db: self,
      txid,
      wrote_begin: wal_begun,
      writer,
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

    // A transaction with no record in the WAL leaves nothing to roll back
    // there: its records were kept back (a bulk load's, and a write
    // transaction's until it writes its BEGIN record).
    if wal_begun {
      // Write the ROLLBACK record, and stop counting the transaction as open
      // under the same WAL lock, which a background cut holds while it reads
      // the open set (see `settle_durable_commits` for COMMIT records).
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

  /// Log `record` for the transaction `tx_handle`: keep it back with the
  /// transaction's other records (see `SingleFileTxState::wal_deferred_from`;
  /// they are written once they outgrow `WAL_DEFER_BYTES`, or by the
  /// commit), or write it to the WAL now. It is encoded once. The
  /// transaction keeps a copy of a record it wrote only for a primary's
  /// commit, which hands them all to the replication sidecar.
  ///
  /// On error the record is dropped: callers apply only a logged write.
  pub(crate) fn write_wal_tx(
    &self,
    tx_handle: &Arc<Mutex<SingleFileTxState>>,
    record: WalRecord,
  ) -> Result<()> {
    let mut record_bytes = record.build();
    let mut tx = tx_handle.lock();
    if let (false, Some(from)) = (tx.bulk_load, tx.wal_deferred_from) {
      let kept = tx.pending_wal.len() - from + record_bytes.len();
      if !tx.savepoints.is_empty() || kept <= super::WAL_DEFER_BYTES {
        tx.pending_wal.extend_from_slice(&record_bytes);
        return Ok(());
      }
      drop(tx);
      return self.write_deferred_records(tx_handle, &record_bytes);
    }
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
      let state = db.commit_queue.state.lock();
      state.leading && state.queued.is_empty()
    });
    (leader, commit_lock)
  }

  fn wait_for_queued_commits(db: &SingleFileDB, count: usize) {
    wait_until("queued commits", || {
      db.commit_queue.state.lock().queued.len() == count
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
/// raydb-b4 `mvcc-default` lane: MVCC as the default, bulk loads under MVCC.
#[cfg(test)]
#[path = "b4_mvcc_default_tests.rs"]
mod b4_mvcc_default_tests;
/// raydb-b4 engine-concurrency: group commit and background cuts.
#[cfg(test)]
#[path = "b4_commit_tests.rs"]
mod b4_tests;
/// raydb-b4 `write-scaling` lane: the commit queue.
#[cfg(test)]
#[path = "b4_write_scaling_tests.rs"]
mod b4_write_scaling_tests;
