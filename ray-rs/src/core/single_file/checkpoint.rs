//! Checkpoint operations for SingleFileDB
//!
//! Handles merging snapshot + delta into a new snapshot, clearing WAL.

use std::collections::HashMap;
use std::sync::atomic::Ordering;

#[cfg(test)]
use std::cell::RefCell;
#[cfg(test)]
use std::sync::{Arc, Barrier, Mutex, OnceLock};

use crate::core::pager::{pages_to_store, FilePager};
use crate::core::snapshot::reader::SnapshotData;
use crate::core::snapshot::writer::{
  build_snapshot_to_memory, EdgeData, NodeData, SnapshotBuildInput,
};
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
  SnapshotPageWritten,
  SnapshotDurable,
  HeaderWritten,
  HeaderDurable,
}

#[cfg(test)]
type CheckpointTestBarrier = (CheckpointPhase, Arc<Barrier>);

#[cfg(test)]
thread_local! {
  static CHECKPOINT_TEST_FAULT: RefCell<Option<CheckpointPhase>> = const { RefCell::new(None) };
}
#[cfg(test)]
static CHECKPOINT_TEST_BARRIER: OnceLock<Mutex<Option<CheckpointTestBarrier>>> = OnceLock::new();
#[cfg(test)]
static CHECKPOINT_TEST_SERIAL: OnceLock<Mutex<()>> = OnceLock::new();

fn checkpoint_phase(phase: CheckpointPhase) -> Result<()> {
  #[cfg(test)]
  {
    let barrier = {
      let mut configured = CHECKPOINT_TEST_BARRIER
        .get_or_init(|| Mutex::new(None))
        .lock()
        .expect("checkpoint test barrier lock");
      if configured
        .as_ref()
        .is_some_and(|(barrier_phase, _)| *barrier_phase == phase)
      {
        configured.take().map(|(_, barrier)| barrier)
      } else {
        None
      }
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

  let _ = phase;
  Ok(())
}

#[cfg(test)]
fn set_checkpoint_test_fault(phase: Option<CheckpointPhase>) {
  CHECKPOINT_TEST_FAULT.with(|fault| *fault.borrow_mut() = phase);
}

#[cfg(test)]
fn set_checkpoint_test_barrier(phase: CheckpointPhase, barrier: Arc<Barrier>) {
  let target = CHECKPOINT_TEST_BARRIER.get_or_init(|| Mutex::new(None));
  *target.lock().expect("checkpoint test barrier lock") = Some((phase, barrier));
}

#[cfg(test)]
fn checkpoint_test_serial() -> std::sync::MutexGuard<'static, ()> {
  CHECKPOINT_TEST_SERIAL
    .get_or_init(|| Mutex::new(()))
    .lock()
    .unwrap_or_else(|poisoned| poisoned.into_inner())
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
    let _checkpoint_gate = self.checkpoint_gate.write();
    checkpoint_phase(CheckpointPhase::GateAcquired)?;
    self.wait_for_no_active_transactions();

    // Collect all graph data
    let (nodes, edges, labels, etypes, propkeys, vector_stores) = self.collect_graph_data()?;

    // Get current header state
    let header = self.header.read().clone();
    let old_snapshot_start_page = header.snapshot_start_page;
    let old_snapshot_page_count = header.snapshot_page_count;
    let new_gen = header.active_snapshot_gen + 1;

    // Build new snapshot in memory
    let snapshot_buffer = build_snapshot_to_memory(SnapshotBuildInput {
      generation: new_gen,
      nodes,
      edges,
      labels,
      etypes,
      propkeys,
      vector_stores: Some(vector_stores),
      compression: self.checkpoint_compression.clone(),
    })?;

    let new_snapshot_page_count =
      pages_to_store(snapshot_buffer.len(), header.page_size as usize) as u64;
    // Never reuse the installed snapshot's pages. Orphaned pages from an
    // interrupted checkpoint are also skipped; vacuum owns reclamation.
    let new_snapshot_start_page =
      self.checkpoint_snapshot_start_page(&header, new_snapshot_page_count)?;

    // Write snapshot to file
    {
      let mut pager = self.pager.lock();
      self.write_snapshot_pages(
        &mut pager,
        new_snapshot_start_page as u32,
        &snapshot_buffer,
        header.page_size as usize,
      )?;
    }
    checkpoint_phase(CheckpointPhase::SnapshotDurable)?;

    // Update header
    {
      let mut pager = self.pager.lock();
      let mut wal_buffer = self.wal_buffer.lock();
      let mut header = self.header.write();
      // Update header fields
      header.prev_snapshot_gen = header.active_snapshot_gen;
      header.active_snapshot_gen = new_gen;
      header.snapshot_start_page = new_snapshot_start_page;
      header.snapshot_page_count = new_snapshot_page_count;
      header.db_size_pages = new_snapshot_start_page + new_snapshot_page_count;
      header.max_node_id = self.next_node_id.load(Ordering::SeqCst).saturating_sub(1);
      header.next_tx_id = self.next_tx_id.load(Ordering::SeqCst);

      // Reset WAL
      header.wal_head = 0;
      header.wal_tail = 0;
      wal_buffer.reset();

      // Snapshot pages were synced by write_snapshot_pages. Install the
      // header only after that durable write, using the inactive slot.
      self.persist_checkpoint_header(&mut pager, &mut header)?;

      if old_snapshot_page_count > 0 && old_snapshot_start_page != new_snapshot_start_page {
        pager.free_pages(
          old_snapshot_start_page as u32,
          old_snapshot_page_count as u32,
        );
      }
    }

    // Clear delta
    self.delta.write().clear();

    // Reload the new snapshot
    self.reload_snapshot()?;
    self.truncate_orphaned_tail()?;

    Ok(())
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

  /// Trigger a background checkpoint (non-blocking)
  ///
  /// This switches writes to secondary WAL region immediately and starts
  /// the checkpoint process. Writes can continue while checkpoint is running.
  ///
  /// Steps:
  /// 1. Switch writes to secondary WAL region
  /// 2. Set checkpointInProgress flag (for crash recovery)
  /// 3. Build new snapshot from primary WAL + current snapshot + delta
  /// 4. Write new snapshot to disk
  /// 5. Merge secondary into primary, update header
  /// 6. Clear checkpointInProgress flag
  pub fn background_checkpoint(&self) -> Result<()> {
    if self.read_only {
      return Err(KiteError::ReadOnly);
    }

    // Check if already running
    {
      let mut status = self.checkpoint_status.lock();
      match *status {
        CheckpointStatus::Running => {
          // Already running, just return
          return Ok(());
        }
        CheckpointStatus::Completing => {
          // Wait for completion by returning
          return Ok(());
        }
        CheckpointStatus::Idle => {
          *status = CheckpointStatus::Running;
        }
      }
    }

    // Step 1: establish a clean cut. Existing transactions are not erased;
    // this path declines to start if one is open, while the short write gate
    // prevents a new transaction from appearing between the check and switch.
    let checkpoint_gate = self.checkpoint_gate.write();
    if self.active_transactions.load(Ordering::Acquire) != 0 {
      drop(checkpoint_gate);
      *self.checkpoint_status.lock() = CheckpointStatus::Idle;
      return Err(KiteError::TransactionInProgress);
    }

    let start_result = {
      let _commit_guard = self.commit_lock.lock();
      let mut pager = self.pager.lock();
      let mut wal_buffer = self.wal_buffer.lock();
      let mut header = self.header.write();

      // Make all pre-cut WAL bytes durable before the checkpoint marker.
      wal_buffer.flush(&mut pager)?;
      pager.sync()?;

      wal_buffer.switch_to_secondary();
      header.active_wal_region = 1;
      header.checkpoint_in_progress = 1;
      header.wal_head = wal_buffer.head();
      header.wal_tail = wal_buffer.tail();
      header.wal_primary_head = wal_buffer.primary_head();
      header.wal_secondary_head = wal_buffer.secondary_head();
      self.persist_header(&mut pager, &mut header, true)
    };
    drop(checkpoint_gate);
    if let Err(error) = start_result {
      *self.checkpoint_status.lock() = CheckpointStatus::Idle;
      return Err(error);
    }

    // Step 2-4: Build and write snapshot, get the info
    let snapshot_info = match self.build_and_write_snapshot() {
      Ok(info) => info,
      Err(e) => {
        // On error, try to recover
        self.recover_from_checkpoint_error();
        return Err(e);
      }
    };

    // Step 5: Complete the checkpoint
    if let Err(error) = self.complete_background_checkpoint(snapshot_info) {
      if self.header.read().checkpoint_in_progress != 0 {
        self.recover_from_checkpoint_error();
      } else {
        *self.checkpoint_status.lock() = CheckpointStatus::Idle;
      }
      return Err(error);
    }

    Ok(())
  }

  /// Build and write the snapshot (called during background checkpoint)
  /// Returns (new_gen, new_snapshot_start_page, new_snapshot_page_count)
  fn build_and_write_snapshot(&self) -> Result<(u64, u64, u64)> {
    // Collect all graph data (reads from snapshot + delta)
    let (nodes, edges, labels, etypes, propkeys, vector_stores) = self.collect_graph_data()?;

    // Get current header state
    let header = self.header.read().clone();
    let new_gen = header.active_snapshot_gen + 1;

    // Build new snapshot in memory
    let snapshot_buffer = build_snapshot_to_memory(SnapshotBuildInput {
      generation: new_gen,
      nodes,
      edges,
      labels,
      etypes,
      propkeys,
      vector_stores: Some(vector_stores),
      compression: self.checkpoint_compression.clone(),
    })?;

    let new_snapshot_page_count =
      pages_to_store(snapshot_buffer.len(), header.page_size as usize) as u64;
    let new_snapshot_start_page =
      self.checkpoint_snapshot_start_page(&header, new_snapshot_page_count)?;

    // Write snapshot to file
    {
      let mut pager = self.pager.lock();
      self.write_snapshot_pages(
        &mut pager,
        new_snapshot_start_page as u32,
        &snapshot_buffer,
        header.page_size as usize,
      )?;
    }
    checkpoint_phase(CheckpointPhase::SnapshotDurable)?;

    Ok((new_gen, new_snapshot_start_page, new_snapshot_page_count))
  }

  /// Complete the background checkpoint
  fn complete_background_checkpoint(&self, snapshot_info: (u64, u64, u64)) -> Result<()> {
    let (new_gen, new_snapshot_start_page, new_snapshot_page_count) = snapshot_info;

    // Mark as completing (brief lock period)
    *self.checkpoint_status.lock() = CheckpointStatus::Completing;

    // Stop new transactions for the install window and let every transaction
    // that began after the cut finish before its secondary WAL is transferred.
    let _checkpoint_gate = self.checkpoint_gate.write();
    self.wait_for_no_active_transactions();
    let _commit_guard = self.commit_lock.lock();

    // Merge secondary records into primary and update header
    let post_cut_records;
    {
      let mut pager = self.pager.lock();
      let mut wal_buffer = self.wal_buffer.lock();
      let mut header = self.header.write();
      let old_snapshot_start_page = header.snapshot_start_page;
      let old_snapshot_page_count = header.snapshot_page_count;

      wal_buffer.flush(&mut pager)?;
      post_cut_records = if wal_buffer.has_secondary_records() {
        wal_buffer.scan_region(1, &mut pager)?
      } else {
        Vec::new()
      };

      // Update header with new snapshot location. The normal path appends the
      // secondary records after the old primary prefix so the old header can
      // still recover if the install is interrupted. If that prefix has no
      // room, install the new snapshot with the secondary region as the
      // temporary retained WAL, then compact that region after the first
      // header is durable.
      header.prev_snapshot_gen = header.active_snapshot_gen;
      header.active_snapshot_gen = new_gen;
      header.snapshot_start_page = new_snapshot_start_page;
      header.snapshot_page_count = new_snapshot_page_count;
      header.db_size_pages = new_snapshot_start_page + new_snapshot_page_count;
      header.max_node_id = self.next_node_id.load(Ordering::SeqCst).saturating_sub(1);
      header.next_tx_id = self.next_tx_id.load(Ordering::SeqCst);

      // Update WAL state
      header.wal_head = wal_buffer.head();
      header.wal_tail = wal_buffer.tail();
      header.wal_primary_head = wal_buffer.primary_head();
      header.wal_secondary_head = wal_buffer.secondary_head();

      let compact_after_install =
        match wal_buffer.merge_secondary_into_primary_preserving_old(&mut pager) {
          Ok(()) => false,
          Err(KiteError::WalBufferFull) => true,
          Err(error) => return Err(error),
        };

      if compact_after_install {
        header.wal_head = wal_buffer.secondary_head();
        header.wal_tail = wal_buffer.primary_region_size();
        header.wal_primary_head = wal_buffer.primary_head();
        header.wal_secondary_head = wal_buffer.secondary_head();
        header.active_wal_region = 1;
        header.checkpoint_in_progress = 0;
        self.persist_checkpoint_header(&mut pager, &mut header)?;

        // The first durable header makes the old primary unreachable. It is
        // now safe to rebuild a compact primary and install it with a second
        // header flip; a crash between flips recovers the secondary region.
        wal_buffer.merge_secondary_into_primary(&mut pager)?;
        wal_buffer.flush(&mut pager)?;
        pager.sync()?;
      } else {
        wal_buffer.flush(&mut pager)?;
        pager.sync()?;
      }

      header.wal_head = wal_buffer.head();
      header.wal_tail = wal_buffer.tail();
      header.wal_primary_head = wal_buffer.primary_head();
      header.wal_secondary_head = wal_buffer.secondary_head();
      header.active_wal_region = 0;
      header.checkpoint_in_progress = 0;
      // The new snapshot and retained WAL are durable before the inactive
      // header slot is installed.
      self.persist_checkpoint_header(&mut pager, &mut header)?;

      // Mark old snapshot pages as free (for future vacuum)
      if old_snapshot_page_count > 0 && old_snapshot_start_page != new_snapshot_start_page {
        pager.free_pages(
          old_snapshot_start_page as u32,
          old_snapshot_page_count as u32,
        );
      }
    }

    // Reload the new snapshot
    self.reload_snapshot()?;
    self.truncate_orphaned_tail()?;

    // Replace, rather than clear, the delta with only transactions written
    // after the snapshot cut. Those records are retained in the new primary
    // WAL range and must remain visible both now and after restart.
    let post_cut_delta = self.replay_records_into_delta(&post_cut_records)?;
    *self.delta.write() = post_cut_delta;

    // Mark as idle
    *self.checkpoint_status.lock() = CheckpointStatus::Idle;

    Ok(())
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

    header.active_wal_region = 0;
    header.checkpoint_in_progress = 0;
    header.wal_head = wal_buffer.head();
    header.wal_tail = wal_buffer.tail();
    header.wal_primary_head = wal_buffer.primary_head();
    header.wal_secondary_head = wal_buffer.secondary_head();
    if let Err(error) = self.persist_header(&mut pager, &mut header, false) {
      eprintln!("Warning: Failed to write checkpoint header during recovery: {error}");
    }
    if let Err(error) = pager.sync() {
      eprintln!("Warning: Failed to sync checkpoint header during recovery: {error}");
    }

    // Mark as idle
    *self.checkpoint_status.lock() = CheckpointStatus::Idle;
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
    if let Some(start_page) = pager.find_free_range(snapshot_page_count as u32) {
      pager.consume_free_range(start_page, snapshot_page_count as u32);
      return Ok(start_page as u64);
    }

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

  fn persist_checkpoint_header(
    &self,
    pager: &mut FilePager,
    header: &mut DbHeaderV1,
  ) -> Result<()> {
    self.persist_header(pager, header, false)?;
    checkpoint_phase(CheckpointPhase::HeaderWritten)?;
    pager.sync()?;
    checkpoint_phase(CheckpointPhase::HeaderDurable)?;

    // Rotate the installed header into the other slot before retiring the old
    // snapshot. After this fsync both valid slots name `header`'s snapshot, so
    // no fallback can reach the region placed on the free list.
    self.persist_header(pager, header, true)
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
      checkpoint_phase(CheckpointPhase::SnapshotPageWritten)?;
    }

    // Sync to disk
    pager.sync()?;

    Ok(())
  }

  /// Collect all graph data from snapshot + delta
  pub(crate) fn collect_graph_data(&self) -> Result<GraphData> {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut labels = HashMap::new();
    let mut etypes = HashMap::new();
    let mut propkeys = HashMap::new();

    let delta = self.delta.read();

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
  use crate::core::single_file::{open_single_file, SingleFileOpenOptions};
  use std::fs::OpenOptions as FsOpenOptions;
  use std::io::{Seek, SeekFrom, Write};
  use std::sync::{mpsc, Arc, Barrier};
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
    set_checkpoint_test_barrier(CheckpointPhase::GateAcquired, Arc::clone(&gate_barrier));
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
    set_checkpoint_test_barrier(CheckpointPhase::SnapshotDurable, Arc::clone(&cut_barrier));
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
}
