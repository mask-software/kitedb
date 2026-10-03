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
//!    then the header naming the records, and in Full mode one sync for both
//!    -> each member's sidecar frame, in order`. Until that sync returns a
//!    failure leaves no trace of the group: its COMMIT records become
//!    ROLLBACK records, a header naming only the commits before it replaces
//!    its header, MVCC unstages its members, and each fails.
//! 2. Under the publish lock, taken before the commit lock is released, so
//!    groups publish in order (`publish_commits`): `for each member: schema
//!    publish -> one publish section, holding the delta (upgradable, written
//!    from the first merge on), with each member's MVCC commit timestamp,
//!    version chains, vectors and delta merge, in order`. Every step runs:
//!    the group is durable. Meanwhile the lead has passed on, and the next
//!    group is written.
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
use parking_lot::{Mutex, RwLockUpgradableReadGuard, RwLockWriteGuard};
use std::collections::{HashMap, HashSet, VecDeque};
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::thread::ThreadId;
#[cfg(feature = "bench-profile")]
use std::time::Instant;

use super::commit_profile::{self as prof, Stage};
use super::mvcc_history::{record_commit, HistoryPlan};
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
  pub(crate) static AFTER_NEXT_COMMIT_TIMESTAMP: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
    std::cell::RefCell::new(None);
}

fn after_commit_timestamp_test_hook() {
  #[cfg(test)]
  if let Some(hook) = AFTER_NEXT_COMMIT_TIMESTAMP.with(|hook| hook.borrow_mut().take()) {
    hook();
  }
}

#[cfg(test)]
thread_local! {
  /// Commit groups this thread took and wrote as the commit queue's leader.
  pub(crate) static GROUPS_LED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn group_led_test_hook() {
  #[cfg(test)]
  GROUPS_LED.with(|groups| groups.set(groups.get() + 1));
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
  /// Its MVCC history plan, if worked out before it queued (see
  /// `SingleFileDB::plan_history`).
  history: Option<HistoryPlan>,
  /// The MVCC commit timestamp its conflict check staged it at.
  commit_ts: Option<Timestamp>,
  /// Its data records, for the replication sidecar (empty without one).
  pending_wal: Vec<u8>,
  staged_schema: SchemaStaging,
  /// The committing thread; its test hooks fire only there.
  committer: ThreadId,
  /// Key sets MVCC released at its commit's group
  /// (`TxManager::commit_tx_releasing`), handed back with the request: its
  /// committer's thread keeps a few to reuse as its next transactions' read
  /// and write sets, and frees the rest (`TxSpares::keep_request`). A group
  /// shares its released sets out among its members, so each committer gets
  /// about as many as its commits release. Empty, with its capacity kept,
  /// between commits.
  released_keys: Vec<TxKeySet>,
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
  /// What is left of its request (the emptied delta, record buffers), for
  /// its committer to keep for its next transaction (`TxSpares`), on the
  /// thread that allocated it.
  leftovers: Option<Box<CommitRequest>>,
}

impl CommitOutcome {
  fn failed(error: KiteError) -> Self {
    Self {
      durable: false,
      schema_published: false,
      result: Err(error),
      leftovers: None,
    }
  }
}

/// A commit group copies a member's records up to this size into the WAL's
/// own buffers; it takes larger ones whole (`WalBuffer::write_owned_record_bytes`).
const COPIED_RECORDS_MAX: usize = 64 * 1024;

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

/// How long a queued committer spins without yielding its CPU first, while
/// fewer transactions are open than the machine has CPUs (see
/// `SingleFileDB::commit_queued`). A yield returns a microsecond or so
/// later, so a committer that yields sees its outcome (or the lead) that
/// much late, and a group waits for its next leader as long; most outcomes
/// come within this. With more transactions open than CPUs, a committer
/// that kept its CPU would hold back the very leader it waits for, so it
/// yields from the start.
const COMMIT_WAIT_HOLD: std::time::Duration = std::time::Duration::from_micros(30);

/// The CPUs a process may run threads on, for `COMMIT_WAIT_HOLD`.
fn available_cpus() -> usize {
  static CPUS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
  *CPUS.get_or_init(|| std::thread::available_parallelism().map_or(1, usize::from))
}

/// A queued commit's slot: its committer waits here until the leader
/// delivers its outcome or hands it the lead.
struct CommitTicket {
  /// `TICKET_*`.
  state: AtomicU8,
  outcome: Mutex<Option<CommitOutcome>>,
  /// The committer, to unpark once it parked.
  committer: std::thread::Thread,
  /// When its state last changed (`commit_profile::stamp`).
  #[cfg(feature = "bench-profile")]
  stamp: AtomicU64,
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
      #[cfg(feature = "bench-profile")]
      stamp: AtomicU64::new(0),
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
    #[cfg(feature = "bench-profile")]
    self.stamp.store(prof::stamp(), Ordering::Relaxed);
    if self.state.swap(state, Ordering::AcqRel) == TICKET_PARKED {
      self.committer.unpark();
    }
  }

  /// Wait until the commit is written (its outcome) or this committer is
  /// handed the lead (`None`). Spins for up to `COMMIT_WAIT_SPIN`, then
  /// parks; `std::thread::park` may return spuriously, so it rechecks. With
  /// `hold_cpu` it keeps its CPU for the first `COMMIT_WAIT_HOLD` of that,
  /// spinning without yielding, so it sees its outcome or the lead at once.
  fn wait(&self, hold_cpu: bool) -> Option<CommitOutcome> {
    let mark = prof::start();
    let now = std::time::Instant::now();
    let spin_until = now + COMMIT_WAIT_SPIN;
    let hold_until = now + COMMIT_WAIT_HOLD;
    let mut holding = hold_cpu;
    let mut spins = 0u32;
    loop {
      match self.state.load(Ordering::Acquire) {
        TICKET_DONE => {
          #[cfg(feature = "bench-profile")]
          prof::since_stamp(Stage::DeliverLatency, self.stamp.load(Ordering::Relaxed));
          prof::end(Stage::FollowerWait, mark);
          let outcome = self.outcome.lock().take();
          return Some(outcome.unwrap_or_else(|| {
            CommitOutcome::failed(KiteError::Internal(
              "commit outcome delivered twice".to_string(),
            ))
          }));
        }
        TICKET_LEAD => {
          #[cfg(feature = "bench-profile")]
          prof::since_stamp(Stage::HandoffLatency, self.stamp.load(Ordering::Relaxed));
          prof::end(Stage::FollowerWait, mark);
          prof::count(Stage::FollowerLead, 1);
          return None;
        }
        TICKET_PARKED => std::thread::park(),
        _ => {
          spins += 1;
          // Keeping the CPU, the clock is read every 64 spins.
          if holding && spins.is_multiple_of(64) {
            holding = std::time::Instant::now() < hold_until;
          }
          if spins < 64 || holding {
            std::hint::spin_loop();
          } else if std::time::Instant::now() < spin_until {
            std::thread::yield_now();
          } else {
            // Fails if the leader got here first; the loop sees its state.
            if self
              .state
              .compare_exchange(
                TICKET_QUEUED,
                TICKET_PARKED,
                Ordering::AcqRel,
                Ordering::Acquire,
              )
              .is_ok()
            {
              prof::count(Stage::FollowerParked, 1);
            }
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
    outcomes: impl IntoIterator<Item = CommitOutcome>,
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

/// The version chains (and, for commits that record more than created
/// nodes, the snapshot) a publish holds to record its commits' history (see
/// `SingleFileDB::record_mvcc_history`): taken by the first commit that
/// records, released before the publish first writes the delta (a reader
/// waits for the chains holding the delta), then kept for the rest.
#[derive(Default)]
struct PublishHistory<'a> {
  snapshot: Option<
    parking_lot::RwLockReadGuard<
      'a,
      super::CacheAligned<Option<crate::core::snapshot::reader::SnapshotData>>,
    >,
  >,
  chains: Option<crate::mvcc::HistoryWriter<'a>>,
}

/// A publish's hold of the committed delta (see `publish_commits`):
/// upgradable, so reads go on, until its first merge, then written.
enum PublishDelta<'a> {
  Reading(RwLockUpgradableReadGuard<'a, super::CacheAligned<DeltaState>>),
  Merging(RwLockWriteGuard<'a, super::CacheAligned<DeltaState>>),
}

impl<'a> PublishDelta<'a> {
  fn state(&self) -> &DeltaState {
    match self {
      Self::Reading(delta) => delta,
      Self::Merging(delta) => delta,
    }
  }

  fn is_reading(&self) -> bool {
    matches!(self, Self::Reading(_))
  }

  /// The delta, written: once its readers are done, the first time.
  fn merging(self) -> RwLockWriteGuard<'a, super::CacheAligned<DeltaState>> {
    match self {
      Self::Reading(delta) => RwLockUpgradableReadGuard::upgrade(delta),
      Self::Merging(delta) => delta,
    }
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
  /// Its schema publish's result, then the first failure of its publish.
  published: Result<()>,
  schema_published: bool,
  /// Its MVCC commit timestamp, and whether a reader needs version chains.
  mvcc_commit: Option<(u64, bool)>,
}

/// What a thread keeps of its last settled transaction for its next one:
/// the transaction's state, its pending delta and record buffers (emptied,
/// their capacity kept), its commit request and its queue ticket, and the
/// MVCC key sets its commits got back (emptied). Each but the key sets comes
/// back to the thread that allocated it (a commit's request returns with its
/// outcome), so a small transaction allocates little, and its committer
/// frees nothing a leader on another thread would otherwise free.
#[derive(Default)]
struct TxSpares {
  state: Option<Arc<Mutex<SingleFileTxState>>>,
  pending: Option<DeltaState>,
  pending_wal: Vec<u8>,
  records: Vec<u8>,
  request: Option<Box<CommitRequest>>,
  ticket: Option<Arc<CommitTicket>>,
  /// Empty MVCC key sets with room for keys, for a write transaction's
  /// reads and writes (at most `SPARE_KEY_SETS`).
  key_sets: Vec<TxKeySet>,
}

/// Record buffers above this capacity, and pending deltas with room for more
/// entries than this in a table, are not kept: a large transaction's are
/// better freed than held by its thread.
const SPARE_BUFFER_MAX: usize = 64 * 1024;
const SPARE_DELTA_MAX: usize = 256;
/// MVCC key sets a thread keeps for its next transaction (its reads and its
/// writes), each with room for at most `SPARE_KEY_SET_MAX` keys.
const SPARE_KEY_SETS: usize = 2;
const SPARE_KEY_SET_MAX: usize = 256;

thread_local! {
  static TX_SPARES: std::cell::RefCell<TxSpares> = std::cell::RefCell::new(TxSpares::default());
}

impl TxSpares {
  /// Run `f` on this thread's spares; `None` once the thread is exiting.
  fn with<R>(f: impl FnOnce(&mut Self) -> R) -> Option<R> {
    TX_SPARES
      .try_with(|spares| {
        spares
          .try_borrow_mut()
          .ok()
          .map(|mut spares| f(&mut spares))
      })
      .ok()
      .flatten()
  }

  /// A buffer kept for reuse: emptied, if not too large.
  fn keep_buffer(slot: &mut Vec<u8>, mut buffer: Vec<u8>) {
    if buffer.capacity() <= SPARE_BUFFER_MAX && buffer.capacity() > slot.capacity() {
      buffer.clear();
      *slot = buffer;
    }
  }

  /// Keep what is left of a settled commit's request for the thread's next
  /// transactions, and some of the key sets its group released (see
  /// `keep_key_sets`); the others are freed.
  fn keep_request(mut request: Box<CommitRequest>) {
    let mut pending = std::mem::take(&mut request.pending);
    let pending_wal = std::mem::take(&mut request.pending_wal);
    let records = std::mem::take(&mut request.records);
    request.staged_schema = SchemaStaging::default();
    request.history = None;
    request.mvcc_keys = None;
    let keep_pending = pending.clear_for_reuse(SPARE_DELTA_MAX);
    Self::with(|spares| {
      if keep_pending {
        spares.pending = Some(pending);
      }
      Self::keep_buffer(&mut spares.pending_wal, pending_wal);
      Self::keep_buffer(&mut spares.records, records);
      spares.keep_key_sets_here(request.released_keys.drain(..));
      request.released_keys.clear();
      spares.request = Some(request);
    });
  }

  /// Keep a settled transaction's state, unless something still shares it.
  fn keep_state(mut state: Arc<Mutex<SingleFileTxState>>) {
    if Arc::get_mut(&mut state).is_some() {
      Self::with(|spares| spares.state = Some(state));
    }
  }

  /// Keep `sets` for this thread's next transactions' MVCC reads and writes,
  /// emptied, while it keeps fewer than `SPARE_KEY_SETS`; the others, and
  /// sets with room for more than `SPARE_KEY_SET_MAX` keys, are freed.
  fn keep_key_sets(sets: impl IntoIterator<Item = TxKeySet>) {
    let mut sets = sets.into_iter();
    Self::with(|spares| spares.keep_key_sets_here(sets.by_ref()));
  }

  /// `keep_key_sets` on these spares; the sets it does not keep are left
  /// in `sets`, for the caller to free outside them.
  fn keep_key_sets_here(&mut self, sets: impl Iterator<Item = TxKeySet>) {
    for mut set in sets {
      if self.key_sets.len() >= SPARE_KEY_SETS {
        break;
      }
      if set.capacity() > 0 && set.capacity() <= SPARE_KEY_SET_MAX {
        set.clear();
        self.key_sets.push(set);
      }
    }
  }

  /// A kept MVCC key set, empty, or a new one.
  fn key_set(&mut self) -> TxKeySet {
    self.key_sets.pop().unwrap_or_default()
  }

  /// A queue ticket for a commit of this thread: the kept one if no leader
  /// still holds it.
  fn ticket() -> Arc<CommitTicket> {
    let kept = Self::with(|spares| spares.ticket.take()).flatten();
    match kept {
      Some(mut ticket) => match Arc::get_mut(&mut ticket) {
        Some(unshared) => {
          *unshared.state.get_mut() = TICKET_QUEUED;
          *unshared.outcome.get_mut() = None;
          ticket
        }
        None => Arc::new(CommitTicket::new()),
      },
      None => Arc::new(CommitTicket::new()),
    }
  }
}

/// The buffers a commit leader works a group with, kept from group to group
/// by each thread that leads (`LeaderScratch::take`): a group would
/// otherwise allocate and free each of them.
#[derive(Default)]
struct LeaderScratch {
  group: Vec<Option<Arc<CommitTicket>>>,
  queue: VecDeque<(usize, Box<CommitRequest>)>,
  passed: VecDeque<(usize, Box<CommitRequest>)>,
  outcomes: Vec<Option<CommitOutcome>>,
  staged: Vec<(usize, Box<CommitRequest>)>,
  commit_records: Vec<(u64, TxId)>,
  durable: Vec<DurableCommit>,
  /// Key sets a round's MVCC commits released, until shared out among its
  /// members' requests (`CommitRequest::released_keys`).
  released: Vec<TxKeySet>,
}

thread_local! {
  static LEADER_SCRATCH: std::cell::RefCell<LeaderScratch> =
    std::cell::RefCell::new(LeaderScratch::default());
}

impl LeaderScratch {
  /// This thread's buffers (empty ones if it has none, or is exiting).
  fn take() -> Self {
    LEADER_SCRATCH
      .try_with(|scratch| std::mem::take(&mut *scratch.borrow_mut()))
      .unwrap_or_default()
  }

  /// Keep the buffers, emptied, for this thread's next lead.
  fn keep(mut self) {
    self.group.clear();
    self.queue.clear();
    self.passed.clear();
    self.outcomes.clear();
    self.staged.clear();
    self.commit_records.clear();
    self.durable.clear();
    self.released.clear();
    let _ = LEADER_SCRATCH.try_with(|scratch| *scratch.borrow_mut() = self);
  }
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
  let commit_ts = tx_mgr
    .stage_commit(txid)
    .map_err(|error| KiteError::Internal(error.to_string()))?;
  request.commit_ts = Some(commit_ts);
  Ok(())
}

/// Lock the transaction manager, spinning (without giving up the CPU) for a
/// while before blocking: commits and begins hold it for a microsecond or
/// two at a time, and a thread that yields or parks to wait that long pays
/// more than it waits.
fn lock_tx_manager(
  mvcc: &crate::mvcc::MvccManager,
) -> parking_lot::MutexGuard<'_, crate::mvcc::TxManager> {
  let mutex = &*mvcc.tx_manager;
  if let Some(guard) = mutex.try_lock() {
    return guard;
  }
  let until = std::time::Instant::now() + TX_MANAGER_SPIN;
  let mut spins = 0u32;
  loop {
    std::hint::spin_loop();
    spins = spins.wrapping_add(1);
    if !mutex.is_locked() {
      if let Some(guard) = mutex.try_lock() {
        return guard;
      }
    }
    if spins.is_multiple_of(128) && std::time::Instant::now() >= until {
      return mutex.lock();
    }
  }
}

/// How long `lock_tx_manager` spins before it blocks.
const TX_MANAGER_SPIN: std::time::Duration = std::time::Duration::from_micros(20);

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
    let begin_mark = prof::start();
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
      // chains and merges them into the delta in one publish section (see
      // `publish_commits`), so this snapshot holds each entirely or not at
      // all, and a commit that has not taken its timestamp yet sees this
      // transaction as a reader that needs version chains. A begin does not
      // complete inside such a section: it waits for the section, and
      // begins again if one started meanwhile (`publish_seq` is odd during
      // one). Outside them it takes no delta lock.
      let (txid, snapshot_ts) = loop {
        let seq = self.publish_seq.load(Ordering::SeqCst);
        if seq % 2 == 1 {
          let wait_mark = prof::start();
          self.wait_for_publish_section(seq);
          prof::end(Stage::BeginPublishWait, wait_mark);
          continue;
        }
        let begun = lock_tx_manager(mvcc).begin_tx();
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
    // An MVCC write transaction notes what it reads and writes in key sets
    // its thread's earlier commits got back (`CommitRequest::released_keys`).
    let key_sets = self.mvcc.is_some() && tx_state.tracks_reads();
    // This thread's last transaction's state and buffers, if it kept them.
    let kept = TxSpares::with(|spares| {
      let buffers = (!read_only).then(|| {
        let keys = key_sets.then(|| (spares.key_set(), spares.key_set()));
        (
          spares.pending.take(),
          std::mem::take(&mut spares.pending_wal),
          keys,
        )
      });
      (spares.state.take(), buffers)
    });
    let (kept_state, kept_buffers) = kept.unwrap_or_default();
    if let Some((pending, pending_wal, keys)) = kept_buffers {
      if let Some(pending) = pending {
        tx_state.pending = pending;
      }
      tx_state.pending_wal = pending_wal;
      if let Some((reads, writes)) = keys {
        tx_state.mvcc_reads = reads;
        tx_state.mvcc_writes = writes;
      }
    }
    let tx_state = match kept_state {
      Some(mut kept) => match Arc::get_mut(&mut kept) {
        Some(unshared) => {
          *unshared.get_mut() = tx_state;
          kept
        }
        None => Arc::new(Mutex::new(tx_state)),
      },
      None => Arc::new(Mutex::new(tx_state)),
    };

    self.register_thread_transaction(tx_state);
    if let Some(claim) = writer_claim {
      claim.keep();
    }
    self.active_transactions.fetch_add(1, Ordering::Release);
    if !read_only {
      self.active_writers.fetch_add(1, Ordering::SeqCst);
      prof::end(Stage::Begin, begin_mark);
    }
    Ok(txid)
  }

  /// Wait for the publish section `seq` (odd: see `publish_commits`) to end:
  /// spin, then yield (most sections take microseconds, and a parked thread
  /// costs the publish a wakeup), then also wait for its merges
  /// (`delta.write()`).
  fn wait_for_publish_section(&self, seq: u64) {
    let mut spins = 0u32;
    while self.publish_seq.load(Ordering::SeqCst) == seq {
      spins += 1;
      if spins <= 64 {
        std::hint::spin_loop();
        continue;
      }
      if spins > 128 {
        drop(self.delta.read());
      }
      std::thread::yield_now();
    }
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

  /// Record the changes of `request`'s commit in the MVCC version chains, for the
  /// transactions still open (see `mvcc_history`), against the committed state `delta` (its
  /// group's earlier commits merged), under `hold`; `horizon` is the history's
  /// (`MvccManager::history_horizon`). With none open, no reader can need the state the commit
  /// replaces: every later read sees the commit, in the delta.
  fn record_mvcc_history<'a>(
    &'a self,
    hold: &mut PublishHistory<'a>,
    horizon: Timestamp,
    commit_ts_for_mvcc: Option<(u64, bool)>,
    request: &mut CommitRequest,
    delta: &DeltaState,
  ) {
    let (Some((commit_ts, true)), Some(mvcc)) = (commit_ts_for_mvcc, self.mvcc.as_ref()) else {
      return;
    };
    let pending = &request.pending;
    // A commit that creates no node needs no chains to plan (`HistoryPlan::of`).
    let mut plan = request.history.take().or_else(|| {
      pending
        .created_nodes
        .is_empty()
        .then(|| HistoryPlan::of(pending, None))
    });
    // Lock order (see read.rs): the snapshot before the chains.
    let take_snapshot = |hold: &mut PublishHistory<'a>| {
      if hold.snapshot.is_none() {
        hold.chains = None;
        hold.snapshot = Some(self.snapshot.read());
      }
    };
    if plan
      .as_ref()
      .is_some_and(|plan| plan.reads_committed_state(pending))
    {
      take_snapshot(hold);
    }
    let chains = hold.chains.get_or_insert_with(|| mvcc.history_writer());
    let plan = match plan.take() {
      Some(plan) if plan.holds_for(chains.chains()) => plan,
      _ => HistoryPlan::of(pending, Some(chains.chains())),
    };
    if plan.reads_committed_state(pending) {
      take_snapshot(hold);
    }
    let snapshot = hold
      .snapshot
      .as_ref()
      .and_then(|snapshot| snapshot.as_ref());
    let chains = hold.chains.get_or_insert_with(|| mvcc.history_writer());
    chains.record(commit_ts, |vc| {
      record_commit(
        vc,
        delta,
        snapshot,
        pending,
        &plan,
        request.txid,
        commit_ts,
        horizon,
      );
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
    let commit_mark = prof::start();
    let result = self.commit_transaction(&tx_handle);
    if !read_only {
      // Every lock is released and this thread's transaction is finished, so
      // the checkpoint may wait for other threads' open transactions without
      // ever waiting on its own. A failed commit checkpoints too: when the
      // WAL refused its COMMIT record, every later commit would fail the same
      // way, and nothing else would ever checkpoint.
      self.auto_checkpoint_if_needed(matches!(result, Err(KiteError::WalBufferFull)));
      prof::end(Stage::CommitTotal, commit_mark);
    }
    TxSpares::keep_state(tx_handle);
    result
  }

  /// Commit the transaction `tx_handle`, already taken from its thread.
  fn commit_transaction(
    &self,
    tx_handle: &Arc<Mutex<SingleFileTxState>>,
  ) -> Result<Option<CommitToken>> {
    let prep_mark = prof::start();
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
      let mvcc_keys = if self.mvcc.is_none() || (reads.is_empty() && writes.is_empty()) {
        // Nothing to check: the sets go back to the thread's spares.
        if reads.capacity() > 0 || writes.capacity() > 0 {
          TxSpares::keep_key_sets([reads, writes]);
        }
        None
      } else {
        // Grouped here, without any lock: with other transactions open, the
        // check and the commit, which every other commit waits for, then
        // look up and note groups instead of keys. Alone, it commits with no
        // check and nothing to note.
        let groups = (reads.len() + writes.len() >= crate::mvcc::KEY_GROUPS_MIN_KEYS
          && self.active_transactions.load(Ordering::Acquire) > 1)
          .then(|| TxKeyGroups::of(&reads, &writes));
        Some(MvccKeys {
          reads,
          writes,
          groups,
        })
      };
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
    // Built into one buffer of the size they take.
    let commit = WalRecord::new(WalRecordType::Commit, txid, build_commit_payload());
    let commit_record_len = commit.estimated_size();
    let records = match (bulk_load, deferred) {
      (false, None) => commit.build(),
      (_, deferred) => {
        let from = if bulk_load { 0 } else { deferred.unwrap_or(0) };
        let begin =
          (!wal_begun).then(|| WalRecord::new(WalRecordType::Begin, txid, build_begin_payload()));
        let mut records =
          TxSpares::with(|spares| std::mem::take(&mut spares.records)).unwrap_or_default();
        records.reserve(
          begin.as_ref().map_or(0, WalRecord::estimated_size) + pending_wal.len() - from
            + commit_record_len,
        );
        if let Some(begin) = begin {
          begin.build_into(&mut records);
        }
        records.extend_from_slice(&pending_wal[from..]);
        commit.build_into(&mut records);
        records
      }
    };
    let history = self.plan_history(&pending);
    let request = CommitRequest {
      txid,
      bulk_load,
      mvcc_keys,
      records,
      commit_record_len,
      pending,
      history,
      commit_ts: None,
      pending_wal,
      staged_schema,
      committer: std::thread::current().id(),
      released_keys: Vec::new(),
    };
    let request = match TxSpares::with(|spares| spares.request.take()).flatten() {
      Some(mut kept) => {
        let released_keys = std::mem::take(&mut kept.released_keys);
        *kept = CommitRequest {
          released_keys,
          ..request
        };
        kept
      }
      None => Box::new(request),
    };

    prof::end(Stage::CommitPrep, prep_mark);
    #[cfg(test)]
    self.commits_waiting.fetch_add(1, Ordering::SeqCst);
    let wait_mark = prof::start();
    let mut outcome = self.commit_queued(request);
    prof::end(Stage::CommitWait, wait_mark);
    if let Some(leftovers) = outcome.leftovers.take() {
      TxSpares::keep_request(leftovers);
    }
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
    let ticket = TxSpares::ticket();
    state.queued.push_back(QueuedCommit {
      request,
      ticket: Arc::clone(&ticket),
    });
    drop(state);
    // Spinning without yielding pays only while every open transaction's
    // thread can have a CPU of its own.
    let hold_cpu = self.active_transactions.load(Ordering::Relaxed) < available_cpus();
    let outcome = match ticket.wait(hold_cpu) {
      Some(outcome) => outcome,
      // Handed the lead: this commit is the oldest queued.
      None => self.lead_commits(None, Some(&ticket)),
    };
    TxSpares::with(|spares| spares.ticket = Some(ticket));
    outcome
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
    let mut scratch = LeaderScratch::take();
    let mut leader = CommitLeader {
      db: self,
      group: std::mem::take(&mut scratch.group),
      released: false,
    };
    before_commit_lock_test_hook();
    let mut queue = std::mem::take(&mut scratch.queue);
    if let Some(request) = own_request {
      queue.push_back((0, request));
      leader.group.push(None);
    }
    let take_mark = prof::start();
    {
      let mut state = self.commit_queue.state.lock();
      let take = state.queued.len().min(MAX_GROUP_COMMITS - queue.len());
      for queued in state.queued.drain(..take) {
        leader.group.push(Some(queued.ticket));
        queue.push_back((queue.len(), queued.request));
      }
    }
    prof::end(Stage::LeadTake, take_mark);
    group_led_test_hook();
    self.write_commits(&mut queue, &mut leader, &mut scratch);
    let deliver_mark = prof::start();
    let outcomes = scratch.outcomes.drain(..).map(|outcome| {
      outcome.unwrap_or_else(|| {
        CommitOutcome::failed(KiteError::Internal("commit was not written".to_string()))
      })
    });
    let own = leader.deliver(own_ticket, outcomes).unwrap_or_else(|| {
      CommitOutcome::failed(KiteError::Internal("commit was not written".to_string()))
    });
    prof::end(Stage::Deliver, deliver_mark);
    scratch.queue = queue;
    scratch.group = std::mem::take(&mut leader.group);
    scratch.keep();
    own
  }

  /// Write the commits of `queue` (each with its index in the group), in
  /// order, and leave their outcomes in `scratch.outcomes`, by index. Each round, under the commit lock, writes the COMMIT records
  /// of those that fit and makes them durable with one WAL write and one
  /// header (`write_commit_round`); then, under the publish lock, which it
  /// takes before it releases the commit lock (so rounds publish in commit
  /// order), publishes them (`publish_commits`). Once a round leaves nothing
  /// of `queue` to write, `leader` passes the lead on before the publish.
  /// Callers hold no lock.
  fn write_commits(
    &self,
    queue: &mut VecDeque<(usize, Box<CommitRequest>)>,
    leader: &mut CommitLeader<'_>,
    scratch: &mut LeaderScratch,
  ) {
    let mut outcomes = std::mem::take(&mut scratch.outcomes);
    outcomes.clear();
    outcomes.resize_with(queue.len(), || None);
    let mut checkpointed_for_room = false;
    while !queue.is_empty() {
      #[cfg(feature = "bench-profile")]
      let commit_lock_start = Instant::now();
      let lock_mark = prof::start();
      let commit_guard = self.commit_lock.lock();
      prof::end(Stage::CommitLockWait, lock_mark);
      #[cfg(feature = "bench-profile")]
      self.commit_lock_wait_ns.fetch_add(
        commit_lock_start.elapsed().as_nanos() as u64,
        Ordering::Relaxed,
      );

      let mut round = self.write_commit_round(queue, &mut outcomes, scratch);
      let publish_lock_mark = prof::start();
      let publish_guard = self.publish_lock.lock();
      prof::end(Stage::PublishLockWait, publish_lock_mark);
      drop(commit_guard);
      let release_mark = prof::start();
      if queue.is_empty() {
        leader.release_lead();
      }
      prof::end(Stage::ReleaseLead, release_mark);
      let publish_mark = prof::start();
      self.publish_commits(&mut round.durable, &mut outcomes, &mut scratch.released);
      drop(publish_guard);
      scratch.durable = std::mem::take(&mut round.durable);
      prof::end(Stage::Publish, publish_mark);

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
    scratch.outcomes = outcomes;
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
  /// becomes durable later, and MVCC unstages its members. In Full mode a
  /// header naming only the commits before the round is written over the
  /// one written with it (whose sync may have failed after it reached the
  /// disk), and synced with the rollback.
  fn write_commit_round(
    &self,
    queue: &mut VecDeque<(usize, Box<CommitRequest>)>,
    outcomes: &mut [Option<CommitOutcome>],
    scratch: &mut LeaderScratch,
  ) -> CommitRound {
    let mut round = CommitRound {
      durable: std::mem::take(&mut scratch.durable),
      ..CommitRound::default()
    };
    let checks_mark = prof::start();
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
    let mut loaded = std::mem::take(&mut scratch.passed);
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
    std::mem::swap(queue, &mut loaded);
    let mut checked = loaded;
    prof::end(Stage::PreChecks, checks_mark);
    let mvcc_mark = prof::start();

    // Vector and MVCC checks, in order, without the WAL lock: each commit
    // that passes is staged, so the ones after it check against its writes.
    // The transaction manager's lock is taken once for the commits without
    // vectors (their check takes the vector stores' lock).
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
          let tx_mgr = tx_mgr.get_or_insert_with(|| lock_tx_manager(mvcc));
          if let Err(error) = check_and_stage_in_mvcc(mvcc, tx_mgr, &mut request) {
            outcomes[index] = Some(CommitOutcome::failed(error));
            continue;
          }
        }
        claimed_dimensions.extend(new_dimensions);
        checked.push_back((index, request));
      }
    }

    prof::end(Stage::MvccCheck, mvcc_mark);
    // Their records, in order, while they fit. Those that do not wait for
    // the next round, unstaged (they are the newest staged), and are checked
    // again then.
    let pager_mark = prof::start();
    let mut pager = self.pager.lock();
    prof::end(Stage::PagerLockWait, pager_mark);
    let append_mark = prof::start();
    let mut staged = std::mem::take(&mut scratch.staged);
    // The WAL position of each staged commit's COMMIT record.
    let mut commit_records = std::mem::take(&mut scratch.commit_records);
    let sealed = {
      let mut wal = self.wal_buffer.lock();
      while let Some((index, mut request)) = checked.pop_front() {
        if wal.can_fit(request.records.len()) {
          // A small member's records are copied into buffers the WAL
          // reuses, and its own buffer goes back to its committer with its
          // outcome: the leader neither allocates nor frees one per member.
          let written = if request.records.len() <= COPIED_RECORDS_MAX {
            wal.write_record_bytes_batch(&request.records)
          } else {
            wal.write_owned_record_bytes(&mut request.records)
          };
          if let Err(error) = written {
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
        scratch.passed = checked;
        scratch.staged = staged;
        scratch.commit_records = commit_records;
        return round;
      }
      // `SyncMode::Off` leaves the records buffered (checkpoints and close
      // write them) and names them in the in-memory header only. A
      // Full-mode round syncs once its writes are done, so it tops up the
      // zeros ahead of the head in them.
      (self.sync_mode != SyncMode::Off).then(|| wal.seal(self.sync_mode == SyncMode::Full))
    };
    prof::end(Stage::WalAppendSeal, append_mark);
    prof::count(Stage::Groups, 1);
    prof::count(Stage::GroupCommits, staged.len() as u64);

    scratch.passed = checked;
    let last_commit_ts = staged.last().and_then(|(_, request)| request.commit_ts);
    match self.persist_commit_round(&mut pager, sealed.as_ref(), last_commit_ts, staged.len()) {
      Ok(()) => {
        drop(pager);
        if let Some(sealed) = sealed {
          self.wal_buffer.lock().recycle_sealed(sealed);
        }
        let settle_mark = prof::start();
        self.settle_durable_commits(&mut staged, &mut round.durable);
        prof::end(Stage::Settle, settle_mark);
        commit_records.clear();
        scratch.commit_records = commit_records;
      }
      Err(error) => {
        if let Some(sealed) = sealed {
          let mut wal = self.wal_buffer.lock();
          let scrubbed = wal.restore_sealed(sealed, commit_records).and_then(|()| {
            // In Full mode the round's header was written before its sync,
            // which may have failed after the header reached the disk: write
            // one naming only the commits before the round, into the slot
            // that one went to, so the rollback's sync below covers both.
            if self.sync_mode == SyncMode::Full {
              let mut header = self.header.write();
              if let Err(retract) = self.persist_header(&mut pager, &mut header, false) {
                eprintln!("Warning: could not rewrite the header after a failed commit: {retract}");
              }
            }
            wal.flush(&mut pager)
          });
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
        for ((index, _), error) in staged.drain(..).zip(errors) {
          outcomes[index] = Some(CommitOutcome::failed(error));
        }
      }
    }
    scratch.staged = staged;
    round
  }

  /// Make a round's commits durable as the sync mode asks: write the sealed
  /// WAL bytes, then the header naming them, and in Full mode sync once,
  /// making both durable together. `commits` is the number of commits the
  /// round staged. On error the in-memory header is as it was, but for its
  /// newer change counter (the next header must outrank every slot on disk).
  ///
  /// A crash during that sync can leave the header on disk without some of
  /// the WAL pages it names (in Normal mode, which never syncs here, any
  /// time). Recovery stops at the first record that does not parse, and
  /// those pages read as the zeros `SealedWrites` wrote and synced there
  /// before (see `WalBuffer::seal`), never as records an earlier crash or
  /// WAL cycle left: so it recovers a prefix of the WAL, every commit
  /// acknowledged before (each round's sync returned before the next round
  /// wrote) and at most a prefix of this round's, none acknowledged.
  ///
  /// `sealed` is `None` in `SyncMode::Off`, which writes nothing.
  /// `last_commit_ts` is the MVCC commit timestamp the round's last commit
  /// was staged at (`check_and_stage_in_mvcc`), the newest staged.
  fn persist_commit_round(
    &self,
    pager: &mut FilePager,
    sealed: Option<&SealedWrites>,
    last_commit_ts: Option<Timestamp>,
    commits: usize,
  ) -> Result<()> {
    during_commit_io_test_hook();
    let write_mark = prof::start();
    if let Some(sealed) = sealed {
      #[cfg(feature = "bench-profile")]
      let flush_start = Instant::now();
      let written = sealed.write(pager).and_then(|()| {
        self.wal_buffer.lock().note_sealed_written(sealed);
        // A failed round's rewritten records may not be durable yet, and
        // this header names bytes past them: make them durable first.
        if sealed.needs_sync() {
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
    prof::end(Stage::WalWrite, write_mark);
    let header_mark = prof::start();

    // MVCC commits the staged commits in order, this round's last.
    let last_commit_ts = match (self.mvcc.as_ref(), last_commit_ts) {
      (Some(_), Some(last_commit_ts)) => last_commit_ts,
      (Some(mvcc), None) => {
        let tx_mgr = mvcc.tx_manager.lock();
        tx_mgr
          .newest_staged_ts()
          .unwrap_or_else(|| tx_mgr.next_commit_ts() + (commits as u64).saturating_sub(1))
      }
      (None, _) => std::time::SystemTime::now()
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
    if let Some(sealed) = sealed {
      #[cfg(feature = "bench-profile")]
      let sync_start = Instant::now();
      let full = self.sync_mode == SyncMode::Full;
      let persisted = self.persist_header(pager, &mut header, full);
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
      drop(header);
      if full {
        self.wal_buffer.lock().note_sealed_synced(sealed);
      }
    }
    prof::end(Stage::HeaderWrite, header_mark);
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
  fn settle_durable_commits(
    &self,
    staged: &mut Vec<(usize, Box<CommitRequest>)>,
    durable: &mut Vec<DurableCommit>,
  ) {
    {
      let mut open = self.open_write_txids.lock();
      for (_, request) in staged.iter() {
        open.remove(&request.txid);
      }
    }
    durable.extend(staged.drain(..).map(|(index, mut request)| {
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
        published: Ok(()),
        schema_published: false,
        mvcc_commit: None,
      }
    }));
  }

  /// Make a round's durable commits visible, in WAL order, under the publish
  /// lock (rounds publish in the order they took the commit lock): publish
  /// their schema; then, in one publish section (`publish_seq` odd), commit
  /// them in MVCC (their timestamps), and for each in order add its version
  /// chains, apply its vectors and merge it into the delta. No transaction
  /// begins inside the section, and each one begun before counts as a reader
  /// that needs version chains.
  ///
  /// The section holds the delta upgradable, so reads go on, until its first
  /// merge, then written. A reader sees no commit of the group until its
  /// merge: a reader whose snapshot predates the group reads the delta and,
  /// where the chains answer for it, their versions from before; both hold the
  /// state before each commit until the commit merges, which waits for the
  /// reader to release the delta. The chains are held only to record a
  /// commit, never across the wait for readers: a reader waits for them
  /// holding the delta.
  ///
  /// Every step runs for every commit even if an earlier one fails: stopping
  /// early would leave a durable transaction out of the delta, invisible
  /// until a reopen replays it, and the next checkpoint (a snapshot of the
  /// delta) would drop it. Each commit reports its first failure.
  fn publish_commits(
    &self,
    round: &mut Vec<DurableCommit>,
    outcomes: &mut [Option<CommitOutcome>],
    released: &mut Vec<TxKeySet>,
  ) {
    if round.is_empty() {
      return;
    }
    let this_thread = std::thread::current().id();

    // This is the schema visibility point, right after the durable commit
    // boundary. Publishing before any fallible post-commit work keeps a
    // later error from leaving a committed WAL definition hidden in this
    // process.
    let schema_mark = prof::start();
    for commit in round.iter_mut() {
      commit.published = self.publish_staged_schema(&commit.request.staged_schema);
      commit.schema_published = commit.published.is_ok();
    }
    prof::end(Stage::PublishSchema, schema_mark);
    for commit in round.iter() {
      if commit.request.committer == this_thread {
        before_merge_test_hook();
      }
    }

    // Upgradable: reads (and transactions, which read the delta as they
    // write) go on until the first merge.
    let delta_mark = prof::start();
    let mut delta = PublishDelta::Reading(self.delta.upgradable_read());
    prof::end(Stage::PublishDeltaWait, delta_mark);
    let _publishing = PublishSection::enter(&self.publish_seq);
    let mvcc_mark = prof::start();
    let horizon = self.commit_in_mvcc(round, released);
    prof::end(Stage::PublishMvcc, mvcc_mark);
    let mut history = PublishHistory::default();
    for commit in round.iter_mut() {
      let request = &mut commit.request;
      let on_committer_thread = request.committer == this_thread;
      if on_committer_thread {
        after_commit_timestamp_test_hook();
      }
      let history_mark = prof::start();
      self.record_mvcc_history(
        &mut history,
        horizon,
        commit.mvcc_commit,
        request,
        delta.state(),
      );
      prof::end(Stage::PublishHistory, history_mark);
      if delta.is_reading() {
        history = PublishHistory::default();
      }
      let merge_wait_mark = prof::start();
      let mut merged = delta.merging();
      prof::end(Stage::PublishMergeWait, merge_wait_mark);
      let merge_mark = prof::start();

      // The stores are loaded and the dimensions checked (`write_commit_round`).
      let vector_fault = if on_committer_thread {
        post_durable_test_fault()
      } else {
        Ok(())
      };
      let vector_result =
        vector_fault.and_then(|()| self.apply_pending_vectors(&request.pending.pending_vectors));

      merged.merge_from(&mut request.pending);
      prof::end(Stage::PublishMerge, merge_mark);
      delta = PublishDelta::Merging(merged);
      if commit.published.is_ok() {
        commit.published = vector_result;
      }
    }
    drop(history);
    drop(_publishing);
    drop(delta);

    // Each member takes back as many of the released key sets as a small
    // commit releases (its read and write sets), the last all the rest.
    let members = round.len();
    for (position, mut commit) in round.drain(..).enumerate() {
      let share = if position + 1 == members {
        released.len()
      } else {
        SPARE_KEY_SETS.min(released.len())
      };
      let request = &mut commit.request;
      request
        .released_keys
        .extend(released.drain(released.len() - share..));
      outcomes[commit.index] = Some(CommitOutcome {
        durable: true,
        schema_published: commit.schema_published,
        result: commit.published.map(|()| commit.token),
        leftovers: Some(commit.request),
      });
    }
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

  /// Commit `round`'s transactions in MVCC, in order, if enabled, at their
  /// durable point: each gets its commit timestamp, and whether a
  /// transaction that may still read was active once it committed (which
  /// then needs version chains); a failure becomes its result. Adds the key
  /// sets the commits released to `released`, to reuse or free without the
  /// locks, and returns the history horizon after them
  /// (`MvccManager::history_horizon`). Callers hold the delta in a publish
  /// section (see `publish_commits`) and staged them
  /// (`check_and_stage_in_mvcc`).
  fn commit_in_mvcc(&self, round: &mut [DurableCommit], released: &mut Vec<TxKeySet>) -> Timestamp {
    let Some(mvcc) = self.mvcc.as_ref() else {
      return 0;
    };
    let mut tx_mgr = lock_tx_manager(mvcc);
    for commit in round.iter_mut() {
      match tx_mgr.commit_tx_releasing(commit.request.txid, released) {
        Ok(commit_ts) => commit.mvcc_commit = Some((commit_ts, tx_mgr.has_open_readers())),
        Err(error) => {
          if commit.published.is_ok() {
            commit.published = Err(KiteError::Internal(error.to_string()));
          }
        }
      }
    }
    // Only commits that record history need it.
    let records_history = round
      .iter()
      .any(|commit| matches!(commit.mvcc_commit, Some((_, true))));
    if records_history {
      mvcc.history_horizon(&tx_mgr)
    } else {
      0
    }
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
    TxSpares::keep_state(tx_handle);
    result
  }

  /// Roll back the transaction `tx_handle`, already taken from its thread
  /// (by `rollback`, or abandoned by a thread that ended with it open).
  pub(super) fn rollback_transaction(
    &self,
    tx_handle: &Arc<Mutex<SingleFileTxState>>,
  ) -> Result<()> {
    let (txid, read_only, wal_begun, writer, key_sets) = {
      let mut tx = tx_handle.lock();
      let key_sets = [
        std::mem::take(&mut tx.mvcc_reads),
        std::mem::take(&mut tx.mvcc_writes),
      ];
      (
        tx.txid,
        tx.read_only,
        tx.wal_begun,
        tx.writer.take(),
        key_sets,
      )
    };
    if key_sets.iter().any(|set| set.capacity() > 0) {
      TxSpares::keep_key_sets(key_sets);
    }
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
    let mut tx = tx_handle.lock();
    // A record kept back is built straight into the transaction's buffer.
    if let (false, Some(from)) = (tx.bulk_load, tx.wal_deferred_from) {
      let kept = tx.pending_wal.len() - from + record.estimated_size();
      if !tx.savepoints.is_empty() || kept <= super::WAL_DEFER_BYTES {
        record.build_into(&mut tx.pending_wal);
        return Ok(());
      }
      drop(tx);
      return self.write_deferred_records(tx_handle, &record.build());
    }
    if tx.bulk_load {
      record.build_into(&mut tx.pending_wal);
      return Ok(());
    }
    drop(tx);
    let mut record_bytes = record.build();
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
/// raydb-b4 `write-costs` lane: what each commit and begin costs with
/// several writers.
#[cfg(test)]
#[path = "b4_write_costs_tests.rs"]
mod b4_write_costs_tests;
/// raydb-b4 `write-scaling` lane: the commit queue.
#[cfg(test)]
#[path = "b4_write_scaling_tests.rs"]
mod b4_write_scaling_tests;
