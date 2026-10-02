//! Transaction management for SingleFileDB
//!
//! Handles begin, commit, and rollback operations.
//!
//! Commit ordering is:
//! `room for the COMMIT record (waiting for a background install if needed)
//! -> MVCC commit timestamp -> WAL COMMIT -> WAL flush -> durable header ->
//! delta / vector / bookkeeping merge -> sidecar attempt`. The sidecar attempt is deliberately
//! non-authoritative after the local durability boundary: an error records
//! primary replication lag and fences future sidecar appends, while this
//! commit still completes locally and returns success.

use crate::core::wal::record::{
  build_begin_payload, build_commit_payload, build_rollback_payload, WalRecord,
};
use crate::error::{KiteError, Result};
use crate::replication::primary::PrimaryReplicationStatus;
use crate::replication::types::CommitToken;
use crate::types::*;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
#[cfg(feature = "bench-profile")]
use std::time::Instant;

use super::open::SyncMode;
use super::{SingleFileDB, SingleFileTxState};
use crate::core::pager::FilePager;
use crate::core::wal::buffer::WalBuffer;

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
}

impl Drop for ActiveTransactionGuard<'_> {
  fn drop(&mut self) {
    self.db.transaction_finished(self.txid, self.wrote_begin);
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

    // Only this thread inserts its own entry, so checking before the gate is
    // race-free. It must come first: a blocking checkpoint holding the gate
    // may be waiting for this thread's open transaction.
    let tid = std::thread::current().id();
    if self.current_tx.lock().contains_key(&tid) {
      return Err(KiteError::TransactionInProgress);
    }

    // A checkpoint takes the write side. Holding this read permit through
    // insertion makes the gate atomic with transaction creation.
    let mut checkpointed_for_room = false;
    let (_checkpoint_gate, txid, snapshot_ts) = loop {
      let checkpoint_gate = self.checkpoint_gate.read();
      let (txid, snapshot_ts) = if let Some(mvcc) = self.mvcc.as_ref() {
        let (txid, snapshot_ts) = {
          let mut tx_mgr = mvcc.tx_manager.lock();
          tx_mgr.begin_tx()
        };
        self
          .next_tx_id
          .store(txid.saturating_add(1), std::sync::atomic::Ordering::SeqCst);
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

    let tx_state = Arc::new(Mutex::new(SingleFileTxState::new(
      txid,
      read_only,
      snapshot_ts,
      bulk_load,
    )));

    self.current_tx.lock().insert(tid, tx_state);
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

  /// `try_write_wal`, waiting and retrying for as long as a background
  /// checkpoint holds the WAL in a full secondary region, and compacting a
  /// retained WAL that fills it. Callers hold no lock that checkpoint needs.
  fn write_wal_waiting(&self, record: &WalRecord) -> Result<()> {
    loop {
      match self.try_write_wal(|wal, pager| wal.write_record(record, pager))? {
        WalWrite::Written(_) => return Ok(()),
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

  pub(crate) fn current_tx_handle(&self) -> Option<Arc<Mutex<SingleFileTxState>>> {
    let tid = std::thread::current().id();
    let current_tx = self.current_tx.lock();
    current_tx.get(&tid).cloned()
  }

  pub(crate) fn require_write_tx_handle(&self) -> Result<(TxId, Arc<Mutex<SingleFileTxState>>)> {
    let handle = self.current_tx_handle().ok_or(KiteError::NoTransaction)?;
    let txid = {
      let tx = handle.lock();
      if tx.read_only {
        return Err(KiteError::ReadOnly);
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
    let mut vc = mvcc.version_chain.lock();
    super::mvcc_history::record_commit(&mut vc, delta, snapshot.as_ref(), pending, txid, commit_ts);
  }

  /// Refuse, before MVCC or a COMMIT record records it, a commit whose
  /// vectors cannot be applied: another transaction fixed the store's
  /// dimensions after this one set its vectors (`set_node_vector` checks only
  /// the store as it was then). Callers hold the commit lock, so no store
  /// changes meanwhile.
  fn check_pending_vectors(
    &self,
    pending_vectors: &HashMap<(NodeId, PropKeyId), Option<VectorRef>>,
  ) -> Result<()> {
    for (&(_node_id, prop_key_id), operation) in pending_vectors {
      let Some(vector) = operation else {
        continue;
      };
      self.ensure_vector_store_loaded(prop_key_id)?;
      if let Some(store) = self.vector_stores.read().get(&prop_key_id) {
        if store.config.dimensions != vector.len() {
          return Err(KiteError::VectorDimensionMismatch {
            expected: store.config.dimensions,
            got: vector.len(),
          });
        }
      }
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

    let tx_handle = {
      let tid = std::thread::current().id();
      let mut current_tx = self.current_tx.lock();
      current_tx.remove(&tid).ok_or(KiteError::NoTransaction)?
    };
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

  /// Commit the transaction `tx_handle`, already removed from `current_tx`.
  fn commit_transaction(
    &self,
    tx_handle: &Arc<Mutex<SingleFileTxState>>,
  ) -> Result<Option<CommitToken>> {
    let (txid, read_only, bulk_load, pending, pending_wal, staged_schema) = {
      let mut tx = tx_handle.lock();
      let pending = std::mem::take(&mut tx.pending);
      let staged_schema = std::mem::take(&mut tx.schema);
      let pending_wal = std::mem::take(&mut tx.pending_wal);
      (
        tx.txid,
        tx.read_only,
        tx.bulk_load,
        pending,
        pending_wal,
        staged_schema,
      )
    };
    let active_transaction_guard = ActiveTransactionGuard {
      db: self,
      txid,
      wrote_begin: !read_only && !bulk_load,
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
    // Until MVCC commits it (right before its COMMIT record), every failure
    // aborts it there.
    let mut mvcc_abort = MvccAbortGuard {
      db: self,
      txid,
      armed: true,
    };

    // Fencing must happen before MVCC marks the transaction committed or the
    // local WAL gets a COMMIT record. A repair fence is deliberately allowed
    // through; it affects only replication, not local commit authority.
    if let Some(replication) = self.primary_replication.as_ref() {
      replication.ensure_local_commit_allowed()?;
    }

    let replication_enabled = self.primary_replication.is_some();
    let group_commit_active =
      self.group_commit_enabled && self.sync_mode == SyncMode::Normal && !replication_enabled;
    let mut group_commit_seq = 0u64;
    let mut commit_token = None;

    // A bulk load writes its whole transaction now, in one batch, so a WAL
    // that refuses it is left without a partial copy.
    let commit_records = {
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

    // Serialize the WAL and delta portions together. The checkpoint cut uses
    // the same lock, so a commit is either completely before or completely
    // after a background snapshot cut.
    let (_commit_guard, commit_ts_for_mvcc) = loop {
      #[cfg(feature = "bench-profile")]
      let commit_lock_start = Instant::now();
      let commit_guard = self.commit_lock.lock();
      #[cfg(feature = "bench-profile")]
      self.commit_lock_wait_ns.fetch_add(
        commit_lock_start.elapsed().as_nanos() as u64,
        Ordering::Relaxed,
      );

      // Refused here, nothing records the commit: not MVCC, not the WAL.
      self.check_pending_vectors(&pending.pending_vectors)?;
      let mut pager = self.pager.lock();
      let mut wal = self.wal_buffer.lock();
      if !wal.can_fit(commit_records.len()) {
        // Nothing of this commit is recorded yet, so make room and retry.
        if wal.is_primary_retired() {
          let mut header = self.header.write();
          self.compact_retained_wal(&mut pager, &mut wal, &mut header)?;
          continue;
        }
        let Some(cut) = self.cut_blocking_wal_writes(&wal) else {
          return Err(KiteError::WalBufferFull);
        };
        // The background checkpoint takes the commit lock to install.
        drop(wal);
        drop(pager);
        drop(commit_guard);
        self.wait_for_cut_release(cut)?;
        continue;
      }

      // MVCC commits here, after any wait for WAL space and right before the
      // COMMIT record, under the commit lock: commit timestamps follow WAL
      // and delta order, and a transaction that began while this commit
      // waited does not see it.
      let commit_ts_for_mvcc = self.commit_in_mvcc(txid)?;
      mvcc_abort.armed = false;
      wal.write_record_bytes_batch(&commit_records, &mut pager)?;

      // Flush WAL to disk based on sync mode
      let should_flush = matches!(self.sync_mode, SyncMode::Full | SyncMode::Normal);
      if should_flush && !group_commit_active {
        #[cfg(feature = "bench-profile")]
        let flush_start = Instant::now();
        wal.flush(&mut pager)?;
        #[cfg(feature = "bench-profile")]
        self
          .wal_flush_ns
          .fetch_add(flush_start.elapsed().as_nanos() as u64, Ordering::Relaxed);
      }

      // Update header with current WAL state and commit metadata
      let mut header = self.header.write();
      wal.store_in_header(&mut header);
      header.max_node_id = self
        .next_node_id
        .load(std::sync::atomic::Ordering::SeqCst)
        .saturating_sub(1);
      header.next_tx_id = self.next_tx_id.load(std::sync::atomic::Ordering::SeqCst);
      header.last_commit_ts = if let Some((commit_ts, _)) = commit_ts_for_mvcc {
        commit_ts
      } else {
        std::time::SystemTime::now()
          .duration_since(std::time::UNIX_EPOCH)
          .map(|d| d.as_millis() as u64)
          .unwrap_or(0)
      };
      // Persist header based on sync mode
      if self.sync_mode != SyncMode::Off {
        #[cfg(feature = "bench-profile")]
        let sync_start = Instant::now();
        self.persist_header(&mut pager, &mut header, self.sync_mode == SyncMode::Full)?;
        #[cfg(feature = "bench-profile")]
        self
          .wal_flush_ns
          .fetch_add(sync_start.elapsed().as_nanos() as u64, Ordering::Relaxed);
      }

      if group_commit_active {
        let mut state = self.group_commit_state.lock();
        state.next_seq = state.next_seq.saturating_add(1);
        group_commit_seq = state.next_seq;
      }
      break (commit_guard, commit_ts_for_mvcc);
    };

    // The commit is durable from here on (with group commit, once its flush
    // lands). Every step below runs even if an earlier one fails: returning
    // early would leave a durable transaction out of the delta, invisible
    // until a reopen replays it, and the next checkpoint (a snapshot of the
    // delta) would drop it. The first failure is reported at the end.
    let group_commit_result = if group_commit_active {
      self.wait_for_group_commit(group_commit_seq)
    } else {
      Ok(())
    };

    // This is the schema visibility point. It occurs immediately after the
    // durable commit boundary, while commit_lock still serializes writers.
    // Publishing before any fallible post-commit work prevents a later error
    // from leaving a committed WAL definition hidden in this process.
    let schema_result = self.publish_staged_schema(&staged_schema);
    if schema_result.is_ok() {
      schema_reservation_guard.disarm();
    }

    let mut delta = self.delta.write();

    self.apply_mvcc_commit(commit_ts_for_mvcc, txid, &pending, &delta);

    // Apply pending vector operations
    let vector_result =
      post_durable_test_fault().and_then(|()| self.apply_pending_vectors(&pending.pending_vectors));

    merge_pending_delta(&mut delta, pending);
    if bulk_load {
      self.cache_clear();
    }
    drop(delta);

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

    drop(_commit_guard);
    drop(active_transaction_guard);
    group_commit_result.and(schema_result).and(vector_result)?;
    Ok(commit_token)
  }

  /// Validate and commit `txid` in MVCC, if enabled: its commit timestamp,
  /// and whether any transaction is still active (which then needs version
  /// chains). A conflict aborts it.
  fn commit_in_mvcc(&self, txid: TxId) -> Result<Option<(u64, bool)>> {
    let Some(mvcc) = self.mvcc.as_ref() else {
      return Ok(None);
    };
    let mut tx_mgr = mvcc.tx_manager.lock();
    if let Err(err) = mvcc.conflict_detector.validate_commit(&tx_mgr, txid) {
      tx_mgr.abort_tx(txid);
      return Err(KiteError::Conflict {
        txid: err.txid,
        keys: err.conflicting_keys,
      });
    }
    let commit_ts = tx_mgr
      .commit_tx(txid)
      .map_err(|e| KiteError::Internal(e.to_string()))?;
    Ok(Some((commit_ts, tx_mgr.active_count() > 0)))
  }

  /// Rollback the current transaction
  pub fn rollback(&self) -> Result<()> {
    let tx_handle = {
      let tid = std::thread::current().id();
      let mut current_tx = self.current_tx.lock();
      current_tx.remove(&tid).ok_or(KiteError::NoTransaction)?
    };
    let read_only = tx_handle.lock().read_only;
    let result = self.rollback_transaction(&tx_handle);
    if !read_only {
      // As after a commit: a rollback often follows a write the full WAL
      // refused, and nothing else may checkpoint.
      self.auto_checkpoint_if_needed(false);
    }
    result
  }

  /// Roll back the transaction `tx_handle`, already removed from
  /// `current_tx`.
  fn rollback_transaction(&self, tx_handle: &Arc<Mutex<SingleFileTxState>>) -> Result<()> {
    let (txid, read_only, bulk_load) = {
      let tx = tx_handle.lock();
      (tx.txid, tx.read_only, tx.bulk_load)
    };
    let _active_transaction_guard = ActiveTransactionGuard {
      db: self,
      txid,
      wrote_begin: !read_only && !bulk_load,
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
      // Write ROLLBACK record to WAL
      let record = WalRecord::new(WalRecordType::Rollback, txid, build_rollback_payload());
      self.write_wal_waiting(&record)?;
    }

    Ok(())
  }

  /// Check if there's an active transaction
  pub fn has_transaction(&self) -> bool {
    self.current_tx_handle().is_some()
  }

  pub(crate) fn has_any_transaction(&self) -> bool {
    !self.current_tx.lock().is_empty()
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

  /// Write a WAL record (internal helper)
  pub(crate) fn write_wal(&self, record: WalRecord) -> Result<()> {
    self.write_wal_waiting(&record)
  }

  pub(crate) fn write_wal_tx(
    &self,
    tx_handle: &Arc<Mutex<SingleFileTxState>>,
    record: WalRecord,
  ) -> Result<()> {
    let mut tx = tx_handle.lock();
    let record_bytes = record.build();
    if tx.bulk_load {
      tx.pending_wal.extend_from_slice(&record_bytes);
      Ok(())
    } else {
      drop(tx);
      self.write_wal(record)?;
      let mut tx = tx_handle.lock();
      tx.pending_wal.extend_from_slice(&record_bytes);
      Ok(())
    }
  }

  fn wait_for_group_commit(&self, seq: u64) -> Result<()> {
    let window_ms = self.group_commit_window_ms;

    {
      let mut state = self.group_commit_state.lock();
      if state.flushing {
        while state.flushed_seq < seq && state.last_error_seq < seq {
          self.group_commit_cv.wait(&mut state);
        }
        if state.last_error_seq >= seq {
          let message = state
            .last_error
            .as_deref()
            .unwrap_or("group commit flush failed");
          return Err(KiteError::Internal(message.to_string()));
        }
        return Ok(());
      }
      state.flushing = true;
    }

    if window_ms > 0 && self.active_writers.load(Ordering::SeqCst) > 0 {
      std::thread::sleep(Duration::from_millis(window_ms));
    }

    #[cfg(feature = "bench-profile")]
    let flush_start = Instant::now();
    let flush_result = {
      let mut pager = self.pager.lock();
      let mut wal = self.wal_buffer.lock();
      wal.flush(&mut pager)
    };
    #[cfg(feature = "bench-profile")]
    self
      .wal_flush_ns
      .fetch_add(flush_start.elapsed().as_nanos() as u64, Ordering::Relaxed);

    let mut state = self.group_commit_state.lock();
    state.flushed_seq = state.next_seq;
    state.flushing = false;
    match &flush_result {
      Ok(_) => {
        state.last_error_seq = 0;
        state.last_error = None;
      }
      Err(err) => {
        state.last_error_seq = state.next_seq;
        state.last_error = Some(err.to_string());
      }
    }
    self.group_commit_cv.notify_all();

    flush_result
  }

  /// Get current transaction ID or error
  pub(crate) fn require_write_tx(&self) -> Result<TxId> {
    let (txid, _) = self.require_write_tx_handle()?;
    Ok(txid)
  }
}

fn merge_pending_delta(target: &mut DeltaState, mut pending: DeltaState) {
  target.new_labels.extend(pending.new_labels.drain());
  target.new_etypes.extend(pending.new_etypes.drain());
  target.new_propkeys.extend(pending.new_propkeys.drain());

  // Deletes first: a node the transaction deleted and created again is a
  // recreate, whose new copy replaces the committed one.
  for node_id in pending.deleted_nodes.drain() {
    target.delete_node(node_id);
  }

  for (node_id, mut node_delta) in pending.created_nodes.drain() {
    target.create_node(node_id, node_delta.key.as_deref());

    if let Some(labels) = node_delta.labels.take() {
      for label_id in labels {
        target.add_node_label(node_id, label_id);
      }
    }
    if let Some(labels_deleted) = node_delta.labels_deleted.take() {
      for label_id in labels_deleted {
        target.remove_node_label(node_id, label_id);
      }
    }
    if let Some(props) = node_delta.props.take() {
      for (key_id, value) in props {
        match value {
          Some(value) => target.set_node_prop_ref(node_id, key_id, value),
          None => target.delete_node_prop(node_id, key_id),
        }
      }
    }
  }

  for (node_id, mut node_delta) in pending.modified_nodes.drain() {
    if let Some(labels) = node_delta.labels.take() {
      for label_id in labels {
        target.add_node_label(node_id, label_id);
      }
    }
    if let Some(labels_deleted) = node_delta.labels_deleted.take() {
      for label_id in labels_deleted {
        target.remove_node_label(node_id, label_id);
      }
    }
    if let Some(props) = node_delta.props.take() {
      for (key_id, value) in props {
        match value {
          Some(value) => target.set_node_prop_ref(node_id, key_id, value),
          None => target.delete_node_prop(node_id, key_id),
        }
      }
    }
  }

  for (src, patches) in pending.out_add.drain() {
    for patch in patches {
      target.add_edge(src, patch.etype, patch.other);
    }
  }

  for (src, patches) in pending.out_del.drain() {
    for patch in patches {
      target.delete_edge(src, patch.etype, patch.other);
    }
  }

  for ((src, etype, dst), props) in pending.edge_props.drain() {
    for (key_id, value) in props {
      match value {
        Some(value) => target.set_edge_prop_ref(src, etype, dst, key_id, value),
        None => target.delete_edge_prop(src, etype, dst, key_id),
      }
    }
  }

  target.key_index.extend(pending.key_index.drain());
  target
    .key_index_deleted
    .extend(pending.key_index_deleted.drain());
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
    let options = SingleFileOpenOptions::new().auto_checkpoint(false);
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
}
