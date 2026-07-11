//! Transaction management for SingleFileDB
//!
//! Handles begin, commit, and rollback operations.
//!
//! Commit ordering is:
//! `WAL COMMIT -> WAL flush -> durable header -> delta / vector / bookkeeping
//! merge -> sidecar attempt`. The sidecar attempt is deliberately
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
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
#[cfg(feature = "bench-profile")]
use std::time::Instant;

use super::open::SyncMode;
use super::{SingleFileDB, SingleFileTxState};

struct ActiveTransactionGuard<'db> {
  db: &'db SingleFileDB,
}

impl Drop for ActiveTransactionGuard<'_> {
  fn drop(&mut self) {
    self.db.transaction_finished();
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

    // A blocking checkpoint takes the write side. Holding this read permit
    // through insertion makes the gate atomic with transaction creation.
    let _checkpoint_gate = self.checkpoint_gate.read();

    let tid = std::thread::current().id();
    {
      let current_tx = self.current_tx.lock();
      if current_tx.contains_key(&tid) {
        return Err(KiteError::TransactionInProgress);
      }
    }

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

    // Write BEGIN record to WAL (for write transactions)
    if !read_only && !bulk_load {
      let record = WalRecord::new(WalRecordType::Begin, txid, build_begin_payload());
      let mut pager = self.pager.lock();
      let mut wal = self.wal_buffer.lock();
      wal.write_record(&record, &mut pager)?;
    }

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

    for (&node_id, node_delta) in &pending.created_nodes {
      vc.append_node_version(
        node_id,
        NodeVersionData {
          node_id,
          delta: node_delta.for_version(),
        },
        txid,
        commit_ts,
      );
    }

    for &node_id in &pending.deleted_nodes {
      vc.delete_node_version(node_id, txid, commit_ts);
    }

    for (&src, patches) in &pending.out_add {
      for patch in patches {
        vc.append_edge_version(src, patch.etype, patch.other, true, txid, commit_ts);
      }
    }

    for (&src, patches) in &pending.out_del {
      for patch in patches {
        vc.append_edge_version(src, patch.etype, patch.other, false, txid, commit_ts);
      }
    }

    let old_node_prop = |node_id: NodeId, key_id: PropKeyId| -> Option<PropValue> {
      if delta.is_node_deleted(node_id) {
        return None;
      }
      if let Some(value_opt) = delta.node_prop(node_id, key_id) {
        return value_opt.cloned();
      }
      if let Some(ref snap) = *snapshot {
        if let Some(phys) = snap.phys_node(node_id) {
          return snap.node_prop(phys, key_id);
        }
      }
      None
    };

    let old_edge_prop = |src: NodeId, etype: ETypeId, dst: NodeId, key_id: PropKeyId| {
      if delta.is_node_deleted(src) || delta.is_node_deleted(dst) {
        return None;
      }
      if delta.is_edge_deleted(src, etype, dst) {
        return None;
      }
      if let Some(value_opt) = delta.edge_prop(src, etype, dst, key_id) {
        return value_opt.cloned();
      }
      if let Some(ref snap) = *snapshot {
        if let (Some(src_phys), Some(dst_phys)) = (snap.phys_node(src), snap.phys_node(dst)) {
          if let Some(edge_idx) = snap.find_edge_index(src_phys, etype, dst_phys) {
            if let Some(snapshot_props) = snap.edge_props(edge_idx) {
              return snapshot_props.get(&key_id).cloned();
            }
          }
        }
      }
      None
    };

    let old_node_label = |node_id: NodeId, label_id: LabelId| -> bool {
      if delta.is_node_deleted(node_id) {
        return false;
      }
      if let Some(node_delta) = delta.node_delta(node_id) {
        if node_delta
          .labels_deleted
          .as_ref()
          .is_some_and(|labels| labels.contains(&label_id))
        {
          return false;
        }
        if node_delta
          .labels
          .as_ref()
          .is_some_and(|labels| labels.contains(&label_id))
        {
          return true;
        }
      }
      if let Some(ref snap) = *snapshot {
        if let Some(phys) = snap.phys_node(node_id) {
          if let Some(labels) = snap.node_labels(phys) {
            return labels.contains(&label_id);
          }
        }
      }
      false
    };

    for (node_id, node_delta) in pending
      .created_nodes
      .iter()
      .chain(pending.modified_nodes.iter())
    {
      if pending.deleted_nodes.contains(node_id) {
        continue;
      }

      if let Some(after_map) = node_delta.props.as_ref() {
        for (key_id, after_value) in after_map {
          let mut before_value = old_node_prop(*node_id, *key_id);
          if before_value.as_ref() == after_value.as_deref() {
            continue;
          }
          if vc.node_prop_version(*node_id, *key_id).is_none() {
            let before_shared = before_value.take().map(std::sync::Arc::new);
            vc.append_node_prop_version(*node_id, *key_id, before_shared, 0, 0);
          }
          vc.append_node_prop_version(*node_id, *key_id, after_value.clone(), txid, commit_ts);
        }
      }

      if let Some(added_labels) = node_delta.labels.as_ref() {
        for label_id in added_labels {
          let before_value = old_node_label(*node_id, *label_id);
          if before_value {
            continue;
          }
          if vc.node_label_version(*node_id, *label_id).is_none() {
            vc.append_node_label_version(
              *node_id,
              *label_id,
              if before_value { Some(true) } else { None },
              0,
              0,
            );
          }
          vc.append_node_label_version(*node_id, *label_id, Some(true), txid, commit_ts);
        }
      }

      if let Some(removed_labels) = node_delta.labels_deleted.as_ref() {
        for label_id in removed_labels {
          let before_value = old_node_label(*node_id, *label_id);
          if !before_value {
            continue;
          }
          if vc.node_label_version(*node_id, *label_id).is_none() {
            vc.append_node_label_version(
              *node_id,
              *label_id,
              if before_value { Some(true) } else { None },
              0,
              0,
            );
          }
          vc.append_node_label_version(*node_id, *label_id, None, txid, commit_ts);
        }
      }
    }

    for (edge_key, after_props) in &pending.edge_props {
      for (key_id, after_value) in after_props {
        let (src, etype, dst) = *edge_key;
        let mut before_value = old_edge_prop(src, etype, dst, *key_id);
        if before_value.as_ref() == after_value.as_deref() {
          continue;
        }
        if vc.edge_prop_version(src, etype, dst, *key_id).is_none() {
          let before_shared = before_value.take().map(std::sync::Arc::new);
          vc.append_edge_prop_version(src, etype, dst, *key_id, before_shared, 0, 0);
        }
        vc.append_edge_prop_version(
          src,
          etype,
          dst,
          *key_id,
          after_value.clone(),
          txid,
          commit_ts,
        );
      }
    }
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
    let active_transaction_guard = ActiveTransactionGuard { db: self };
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

    // Fencing must happen before MVCC marks the transaction committed or the
    // local WAL gets a COMMIT record. A repair fence is deliberately allowed
    // through; it affects only replication, not local commit authority.
    if let Some(replication) = self.primary_replication.as_ref() {
      replication.ensure_local_commit_allowed()?;
    }

    let mut commit_ts_for_mvcc = None;
    if let Some(mvcc) = self.mvcc.as_ref() {
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
      commit_ts_for_mvcc = Some((commit_ts, tx_mgr.active_count() > 0));
    }

    let replication_enabled = self.primary_replication.is_some();
    let group_commit_active =
      self.group_commit_enabled && self.sync_mode == SyncMode::Normal && !replication_enabled;
    let mut group_commit_seq = 0u64;
    let mut commit_token = None;

    // Serialize the WAL and delta portions together. The checkpoint cut uses
    // the same lock, so a commit is either completely before or completely
    // after a background snapshot cut.
    #[cfg(feature = "bench-profile")]
    let commit_lock_start = Instant::now();
    let _commit_guard = self.commit_lock.lock();
    #[cfg(feature = "bench-profile")]
    self.commit_lock_wait_ns.fetch_add(
      commit_lock_start.elapsed().as_nanos() as u64,
      Ordering::Relaxed,
    );

    {
      let mut pager = self.pager.lock();
      let mut wal = self.wal_buffer.lock();
      if bulk_load {
        let begin_record = WalRecord::new(WalRecordType::Begin, txid, build_begin_payload());
        wal.write_record(&begin_record, &mut pager)?;
        if !pending_wal.is_empty() {
          wal.write_record_bytes_batch(&pending_wal, &mut pager)?;
        }
        let commit_record = WalRecord::new(WalRecordType::Commit, txid, build_commit_payload());
        wal.write_record(&commit_record, &mut pager)?;
      } else {
        // Write COMMIT record to WAL
        let record = WalRecord::new(WalRecordType::Commit, txid, build_commit_payload());
        wal.write_record(&record, &mut pager)?;
      }

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
      header.wal_head = wal.head();
      header.wal_tail = wal.tail();
      header.wal_primary_head = wal.primary_head();
      header.wal_secondary_head = wal.secondary_head();
      header.active_wal_region = wal.active_region();
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
    }

    if group_commit_active {
      self.wait_for_group_commit(group_commit_seq)?;
    }

    // This is the schema visibility point. It occurs immediately after the
    // durable commit boundary, while commit_lock still serializes writers.
    // Publishing before any fallible post-commit work prevents a later error
    // from leaving a committed WAL definition hidden in this process.
    self.publish_staged_schema(&staged_schema)?;
    schema_reservation_guard.disarm();

    let mut delta = self.delta.write();

    self.apply_mvcc_commit(commit_ts_for_mvcc, txid, &pending, &delta);

    // Apply pending vector operations
    self.apply_pending_vectors(&pending.pending_vectors)?;

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
          eprintln!("Warning: Auto-checkpoint failed: {e}");
        }
      }
    }

    Ok(commit_token)
  }

  /// Rollback the current transaction
  pub fn rollback(&self) -> Result<()> {
    let tx_handle = {
      let tid = std::thread::current().id();
      let mut current_tx = self.current_tx.lock();
      current_tx.remove(&tid).ok_or(KiteError::NoTransaction)?
    };
    let (txid, read_only, bulk_load) = {
      let tx = tx_handle.lock();
      (tx.txid, tx.read_only, tx.bulk_load)
    };
    let _active_transaction_guard = ActiveTransactionGuard { db: self };
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
      let mut pager = self.pager.lock();
      let mut wal = self.wal_buffer.lock();
      wal.write_record(&record, &mut pager)?;
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
    let mut pager = self.pager.lock();
    let mut wal = self.wal_buffer.lock();
    wal.write_record(&record, &mut pager)?;
    Ok(())
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

  for node_id in pending.deleted_nodes.drain() {
    target.delete_node(node_id);
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
}
