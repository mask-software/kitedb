//! Checkpoint operations for SingleFileDB
//!
//! Handles merging snapshot + delta into a new snapshot, clearing WAL.

use std::collections::HashMap;
use std::sync::atomic::Ordering;

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
use crate::error::{KiteError, Result};
use crate::types::*;
use crate::vector::types::VectorManifest;

use super::open::map_snapshot_range;
use super::recovery::{committed_transactions, replay_wal_record};
use super::vector::vector_store_state_from_snapshot;
use super::{CheckpointStatus, SingleFileDB};

type GraphData = (
  Vec<NodeData>,
  Vec<EdgeData>,
  HashMap<LabelId, String>,
  HashMap<ETypeId, String>,
  HashMap<PropKeyId, String>,
  HashMap<PropKeyId, VectorManifest>,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckpointPhase {
  GateAcquired,
  /// A background checkpoint's cut is durable and the gate is open again; its
  /// snapshot is not built yet.
  CutReleased,
  SnapshotPageWritten,
  SnapshotDurable,
  HeaderWritten,
  HeaderDurable,
  /// A background checkpoint's header naming the post-cut records in the
  /// secondary region is durable; they are not yet compacted into primary.
  PostCutWalRetained,
}

/// A barrier armed for one phase of checkpoints on the database at a path.
#[cfg(test)]
type CheckpointTestBarrier = (std::path::PathBuf, CheckpointPhase, Arc<Barrier>);

#[cfg(test)]
thread_local! {
  static CHECKPOINT_TEST_FAULT: RefCell<Option<CheckpointPhase>> = const { RefCell::new(None) };
}
#[cfg(test)]
static CHECKPOINT_TEST_BARRIERS: OnceLock<Mutex<Vec<CheckpointTestBarrier>>> = OnceLock::new();
#[cfg(test)]
static CHECKPOINT_TEST_SERIAL: OnceLock<Mutex<()>> = OnceLock::new();

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
  }

  let _ = (db_path, phase);
  Ok(())
}

#[cfg(test)]
fn set_checkpoint_test_fault(phase: Option<CheckpointPhase>) {
  CHECKPOINT_TEST_FAULT.with(|fault| *fault.borrow_mut() = phase);
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

/// Return `header` to `prior` after a failed header write, keeping the newer
/// change counter so the next write still outranks every slot on disk.
fn restore_header(header: &mut DbHeaderV1, prior: DbHeaderV1) {
  let change_counter = header.change_counter;
  *header = prior;
  header.change_counter = change_counter;
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

    let graph = self.collect_graph_data()?;
    let header = self.header.read().clone();
    let generation = header.active_snapshot_gen + 1;
    let snapshot_buffer = self.build_snapshot_buffer(generation, graph)?;
    let snapshot = self.write_new_snapshot(&header, generation, &snapshot_buffer)?;

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

    // Clear delta
    self.delta.write().clear();

    // Reload the new snapshot
    self.reload_snapshot()?;
    self.truncate_orphaned_tail()?;

    Ok(())
  }

  /// Take the checkpoint gate for work that replaces the snapshot or resets
  /// the WAL: no transaction is open, none can begin, and no background
  /// checkpoint is between its cut and its install. Running inside that window
  /// would erase the background checkpoint's post-cut commits: the WAL holding
  /// them would be reset, and its install would then replace this snapshot
  /// with its older one.
  pub(crate) fn exclusive_checkpoint_gate(&self) -> Result<RwLockWriteGuard<'_, ()>> {
    loop {
      let checkpoint_gate = self.checkpoint_gate.write();
      checkpoint_phase(&self.path, CheckpointPhase::GateAcquired)?;
      self.wait_for_no_active_transactions();
      // The marker is set by a background cut and cleared by its install
      // (both under this gate) or by the WAL rebuild after it fails. Either
      // way the background checkpoint then returns to idle.
      if self.header.read().checkpoint_in_progress == 0 {
        return Ok(checkpoint_gate);
      }
      drop(checkpoint_gate);
      self.wait_for_background_checkpoint();
    }
  }

  /// Reload snapshot from disk after checkpoint
  pub(crate) fn reload_snapshot(&self) -> Result<()> {
    let header = self.header.read();

    if header.snapshot_page_count == 0 {
      // No snapshot to load
      *self.snapshot.write() = None;
      self.vector_stores.write().clear();
      self.vector_store_lazy_entries.write().clear();
      return Ok(());
    }

    // Map only the immutable snapshot range. Header and WAL pages remain
    // outside every live SnapshotData mapping.
    let pager = self.pager.lock();
    let new_snapshot = SnapshotData::parse(
      map_snapshot_range(&pager, &header)?,
      &crate::core::snapshot::reader::ParseSnapshotOptions::default(),
    )?;

    // Update the snapshot
    *self.snapshot.write() = Some(new_snapshot);

    // Rebuild vector stores from the new snapshot
    if let Some(ref snapshot) = *self.snapshot.read() {
      let (stores, lazy_entries) = vector_store_state_from_snapshot(snapshot)?;
      *self.vector_stores.write() = stores;
      *self.vector_store_lazy_entries.write() = lazy_entries;
    }

    Ok(())
  }

  // ========================================================================
  // Background Checkpoint (Non-Blocking)
  // ========================================================================

  /// Check if a background checkpoint is currently running
  pub fn is_checkpoint_running(&self) -> bool {
    let status = *self.checkpoint_status.lock();
    matches!(
      status,
      CheckpointStatus::Running | CheckpointStatus::Completing
    )
  }

  /// Get current checkpoint status
  pub fn checkpoint_status(&self) -> CheckpointStatus {
    *self.checkpoint_status.lock()
  }

  /// Block until no background checkpoint is running.
  fn wait_for_background_checkpoint(&self) {
    let mut wait = self.checkpoint_wait.lock();
    while self.is_checkpoint_running() {
      self.checkpoint_cv.wait(&mut wait);
    }
  }

  /// End a background checkpoint and wake threads waiting for it. Callers
  /// leave the in-memory checkpoint marker clear.
  fn set_checkpoint_idle(&self) {
    *self.checkpoint_status.lock() = CheckpointStatus::Idle;
    self.notify_checkpoint_waiters();
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
  pub fn background_checkpoint(&self) -> Result<()> {
    if self.read_only {
      return Err(KiteError::ReadOnly);
    }
    if self.current_tx_handle().is_some() {
      return Err(KiteError::TransactionInProgress);
    }

    // Claim the checkpoint before taking the gate, so commits that cross the
    // threshold meanwhile skip it instead of queueing behind it.
    {
      let mut status = self.checkpoint_status.lock();
      if *status != CheckpointStatus::Idle {
        // Already running or completing
        return Ok(());
      }
      *status = CheckpointStatus::Running;
    }

    // Steps 1-2: establish a clean cut. The gate excludes blocking
    // checkpoints and compaction, and holds off a BEGIN record between the
    // switch and the set of open transactions being read.
    let checkpoint_gate = self.checkpoint_gate.write();
    let cut_result = self.cut_background_checkpoint();
    drop(checkpoint_gate);

    // Steps 3-6
    let result = cut_result.and_then(|cut_delta| {
      checkpoint_phase(&self.path, CheckpointPhase::CutReleased)?;
      let snapshot = self.build_and_write_snapshot(cut_delta)?;
      self.complete_background_checkpoint(snapshot)
    });
    if result.is_err() {
      self.abandon_background_checkpoint();
    }
    result
  }

  /// Return to idle after a failed background checkpoint. Once the cut
  /// marker is set, the WAL is first rebuilt in the primary region from both
  /// regions, so the marker can be cleared without losing a record.
  fn abandon_background_checkpoint(&self) {
    if self.header.read().checkpoint_in_progress != 0 {
      self.recover_from_checkpoint_error();
    } else {
      self.set_checkpoint_idle();
    }
  }

  /// Switch new WAL writes to the secondary region and durably mark the
  /// checkpoint in progress. Returns the committed delta as of the cut.
  ///
  /// Callers hold the checkpoint gate. Every transaction that commits after
  /// the cut has all its records in the secondary region: those still open
  /// are copied there. The snapshot must be built from exactly the pre-cut
  /// commits: completion replays every post-cut commit over it, and edge and
  /// label changes are not idempotent (a delete followed by a re-add would
  /// cancel out if applied twice).
  ///
  /// An error after the marker is set leaves it set, for the caller to
  /// recover from.
  fn cut_background_checkpoint(&self) -> Result<DeltaState> {
    let _commit_guard = self.commit_lock.lock();
    {
      let mut pager = self.pager.lock();
      let mut wal_buffer = self.wal_buffer.lock();
      let mut header = self.header.write();

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
      let carried = wal_buffer.open_transaction_records(&open, &mut pager)?;

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

      // The durable marker names an empty secondary region, so the copies
      // cannot clobber records a crash fallback needs; the header of the next
      // commit names them.
      wal_buffer.carry_into_secondary(&carried, &mut pager)?;
    }

    // Commits merge into the delta under the commit lock, so this is exactly
    // the pre-cut state.
    Ok(self.delta.read().clone())
  }

  /// Build and write the snapshot (called during background checkpoint)
  fn build_and_write_snapshot(&self, cut_delta: DeltaState) -> Result<WrittenSnapshot> {
    let graph = self.collect_graph_data_from(&cut_delta)?;
    drop(cut_delta);

    // Snapshot fields do not change while this checkpoint runs; commits only
    // move the WAL positions.
    let header = self.header.read().clone();
    let generation = header.active_snapshot_gen + 1;
    let snapshot_buffer = self.build_snapshot_buffer(generation, graph)?;
    self.write_new_snapshot(&header, generation, &snapshot_buffer)
  }

  /// Complete the background checkpoint
  fn complete_background_checkpoint(&self, snapshot: WrittenSnapshot) -> Result<()> {
    // Mark as completing (brief lock period)
    *self.checkpoint_status.lock() = CheckpointStatus::Completing;

    // The gate excludes blocking checkpoints and compaction until the delta
    // is replaced below; the commit lock keeps the retained records and that
    // delta in step. Open transactions keep their records in the secondary
    // region, which is retained and compacted, and append after them.
    let _checkpoint_gate = self.checkpoint_gate.write();
    let _commit_guard = self.commit_lock.lock();

    let post_cut_records;
    let compaction_result;
    {
      let mut pager = self.pager.lock();
      let mut wal_buffer = self.wal_buffer.lock();
      let mut header = self.header.write();

      wal_buffer.flush(&mut pager)?;
      let retain_post_cut = wal_buffer.has_secondary_records();
      post_cut_records = if retain_post_cut {
        wal_buffer.scan_region(1, &mut pager)?
      } else {
        Vec::new()
      };

      // The new snapshot covers every primary record, so the installed header
      // stops counting them: with no post-cut records the WAL is simply
      // empty; otherwise it names only the post-cut records, still in place
      // in the secondary region. Neither state writes WAL bytes, so the cut
      // header's WAL stays intact as the crash fallback until this header is
      // durable in both slots. If the install fails, the cut state is
      // restored (the cut header may still be the newest durable slot, and it
      // names the primary region), so the caller rebuilds the WAL instead of
      // letting the next commit overwrite records that header still needs.
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

      // Both slots now name the retained secondary records, so the primary
      // region is free to rewrite. A failure leaves the retained state, which
      // is consistent on disk and finished by the next background checkpoint
      // or open; the new snapshot is installed either way.
      compaction_result = if retain_post_cut {
        self.compact_retained_wal(&mut pager, &mut wal_buffer, &mut header)
      } else {
        Ok(())
      };
    }

    // Reload the new snapshot
    self.reload_snapshot()?;
    self.truncate_orphaned_tail()?;

    // Replace, rather than clear, the delta with only transactions written
    // after the snapshot cut. Those records are retained in the WAL and must
    // remain visible both now and after restart.
    let post_cut_delta = self.replay_records_into_delta(&post_cut_records)?;
    *self.delta.write() = post_cut_delta;

    self.set_checkpoint_idle();

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
  fn compact_retained_wal(
    &self,
    pager: &mut FilePager,
    wal_buffer: &mut WalBuffer,
    header: &mut DbHeaderV1,
  ) -> Result<()> {
    checkpoint_phase(&self.path, CheckpointPhase::PostCutWalRetained)?;
    wal_buffer.compact_secondary_into_primary(pager)?;
    wal_buffer.store_in_header(header);
    self.persist_checkpoint_header(pager, header)
  }

  fn replay_records_into_delta(
    &self,
    wal_records: &[crate::core::wal::record::ParsedWalRecord],
  ) -> Result<DeltaState> {
    let committed = committed_transactions(wal_records);
    let mut delta = DeltaState::new();
    let mut next_node_id = self.next_node_id.load(Ordering::Acquire);
    let mut next_label_id = self.next_label_id.load(Ordering::Acquire);
    let mut next_etype_id = self.next_etype_id.load(Ordering::Acquire);
    let mut next_propkey_id = self.next_propkey_id.load(Ordering::Acquire);
    let snapshot = self.snapshot.read();
    let mut label_names = self.label_names.write();
    let mut label_ids = self.label_ids.write();
    let mut etype_names = self.etype_names.write();
    let mut etype_ids = self.etype_ids.write();
    let mut propkey_names = self.propkey_names.write();
    let mut propkey_ids = self.propkey_ids.write();

    for (_txid, records) in committed {
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
    }

    drop(propkey_ids);
    drop(propkey_names);
    drop(etype_ids);
    drop(etype_names);
    drop(label_ids);
    drop(label_names);
    drop(snapshot);

    self.next_node_id.store(next_node_id, Ordering::Release);
    self.next_label_id.store(next_label_id, Ordering::Release);
    self.next_etype_id.store(next_etype_id, Ordering::Release);
    self
      .next_propkey_id
      .store(next_propkey_id, Ordering::Release);

    let pending_vectors = delta.pending_vectors.clone();
    self.apply_pending_vectors(&pending_vectors)?;
    delta.pending_vectors.clear();
    Ok(delta)
  }

  /// Recover from a checkpoint error
  fn recover_from_checkpoint_error(&self) {
    // Rebuild a clean primary WAL before clearing the marker. This preserves
    // both pre-cut and post-cut committed records if the snapshot build fails.
    {
      let _commit_guard = self.commit_lock.lock();
      let mut pager = self.pager.lock();
      let mut wal_buffer = self.wal_buffer.lock();
      let mut header = self.header.write();

      if let Err(error) = wal_buffer
        .recover_incomplete_checkpoint(&mut pager)
        .and_then(|_| wal_buffer.flush(&mut pager))
        .and_then(|_| pager.sync())
      {
        eprintln!("Warning: Failed to rebuild checkpoint WAL during recovery: {error}");
      }

      wal_buffer.store_in_header(&mut header);
      header.checkpoint_in_progress = 0;
      if let Err(error) = self.persist_header(&mut pager, &mut header, false) {
        eprintln!("Warning: Failed to write checkpoint header during recovery: {error}");
      }
      if let Err(error) = pager.sync() {
        eprintln!("Warning: Failed to sync checkpoint header during recovery: {error}");
      }
    }

    self.set_checkpoint_idle();
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
      .and_then(|()| checkpoint_phase(&self.path, CheckpointPhase::HeaderWritten))
      .and_then(|()| pager.sync());
    if let Err(error) = first_slot {
      self.header_slot.store(durable_slot, Ordering::Release);
      return Err(error);
    }
    checkpoint_phase(&self.path, CheckpointPhase::HeaderDurable)?;

    // Rotate the installed header into the other slot before retiring the old
    // snapshot. After this fsync both valid slots name `header`'s snapshot, so
    // no fallback can reach the region placed on the free list.
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

  /// Serialize a checkpoint snapshot of `graph`.
  fn build_snapshot_buffer(&self, generation: u64, graph: GraphData) -> Result<Vec<u8>> {
    let (nodes, edges, labels, etypes, propkeys, vector_stores) = graph;
    build_snapshot_to_memory(SnapshotBuildInput {
      generation,
      nodes,
      edges,
      labels,
      etypes,
      propkeys,
      vector_stores: Some(vector_stores),
      compression: self.checkpoint_compression.clone(),
    })
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

    let written = {
      let mut pager = self.pager.lock();
      self.write_snapshot_pages(&mut pager, start_page as u32, buffer, page_size)
    };
    if let Err(error) =
      written.and_then(|()| checkpoint_phase(&self.path, CheckpointPhase::SnapshotDurable))
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

  /// Write snapshot buffer to file pages
  pub(crate) fn write_snapshot_pages(
    &self,
    pager: &mut FilePager,
    start_page: u32,
    buffer: &[u8],
    page_size: usize,
  ) -> Result<()> {
    let num_pages = pages_to_store(buffer.len(), page_size);

    // Ensure file is large enough
    let required_pages = start_page + num_pages;
    let current_pages = (pager.file_size() as usize).div_ceil(page_size);

    if required_pages as usize > current_pages {
      pager.allocate_pages(required_pages - current_pages as u32)?;
    }

    // Write pages
    for i in 0..num_pages {
      let mut page_data = vec![0u8; page_size];
      let src_offset = i as usize * page_size;
      let src_end = std::cmp::min(src_offset + page_size, buffer.len());
      page_data[..src_end - src_offset].copy_from_slice(&buffer[src_offset..src_end]);
      pager.write_page(start_page + i, &page_data)?;
      checkpoint_phase(&self.path, CheckpointPhase::SnapshotPageWritten)?;
    }

    // Sync to disk
    pager.sync()?;

    Ok(())
  }

  /// Collect all graph data from snapshot + delta
  pub(crate) fn collect_graph_data(&self) -> Result<GraphData> {
    let delta = self.delta.read();
    self.collect_graph_data_from(&delta)
  }

  /// Collect all graph data from snapshot + `delta`
  fn collect_graph_data_from(&self, delta: &DeltaState) -> Result<GraphData> {
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
          node_labels.extend(snapshot_labels.into_iter());
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
    for (&src, patches) in &delta.out_add {
      // Skip edges from deleted nodes
      if delta.is_node_deleted(src) {
        continue;
      }

      for patch in patches {
        // Skip edges to deleted nodes
        if delta.is_node_deleted(patch.other) {
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
    let vector_stores_for_snapshot: HashMap<PropKeyId, VectorManifest> =
      self.vector_stores.read().clone();
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
    let options = SingleFileOpenOptions::new().auto_checkpoint(false);
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
    let options = SingleFileOpenOptions::new()
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
      Err(KiteError::WalBufferFull)
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
    db.create_node(Some("after-vacuum")).expect("expected value");
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
    assert!(wal_pages >= 16, "vacuum shrank the WAL to {wal_pages} pages");
    db.begin(false).expect("expected value");
    db.create_node(Some("after-vacuum")).expect("write after vacuum");
    db.commit().expect("commit after vacuum");
  }
}
