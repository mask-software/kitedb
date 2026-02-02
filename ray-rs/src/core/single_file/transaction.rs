//! Transaction management for SingleFileDB
//!
//! Handles begin, commit, and rollback operations.

use crate::core::wal::record::{
  build_begin_payload, build_commit_payload, build_rollback_payload, WalRecord,
};
use crate::error::{KiteError, Result};
use crate::types::*;
use parking_lot::Mutex;
use std::sync::Arc;

use super::open::SyncMode;
use super::{SingleFileDB, SingleFileTxState};

impl SingleFileDB {
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
    if self.read_only && !read_only {
      return Err(KiteError::ReadOnly);
    }

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
    if !read_only {
      let record = WalRecord::new(WalRecordType::Begin, txid, build_begin_payload());
      let mut pager = self.pager.lock();
      let mut wal = self.wal_buffer.lock();
      wal.write_record(&record, &mut pager)?;
    }

    let tx_state = Arc::new(Mutex::new(SingleFileTxState::new(
      txid,
      read_only,
      snapshot_ts,
    )));

    self.current_tx.lock().insert(tid, tx_state);
    Ok(txid)
  }

  /// Commit the current transaction
  pub fn commit(&self) -> Result<()> {
    let tx_handle = {
      let tid = std::thread::current().id();
      let mut current_tx = self.current_tx.lock();
      current_tx.remove(&tid).ok_or(KiteError::NoTransaction)?
    };

    let (txid, read_only, pending) = {
      let mut tx = tx_handle.lock();
      let pending = std::mem::take(&mut tx.pending);
      (tx.txid, tx.read_only, pending)
    };

    if read_only {
      // Read-only transactions don't need WAL
      if let Some(mvcc) = self.mvcc.as_ref() {
        let mut tx_mgr = mvcc.tx_manager.lock();
        tx_mgr.abort_tx(txid);
      }
      return Ok(());
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
      commit_ts_for_mvcc = Some((commit_ts, tx_mgr.get_active_count() > 0));
    }

    // Serialize commit to preserve WAL ordering without holding the delta lock during I/O.
    let _commit_guard = self.commit_lock.lock();

    // Write COMMIT record to WAL
    let record = WalRecord::new(WalRecordType::Commit, txid, build_commit_payload());
    {
      let mut pager = self.pager.lock();
      let mut wal = self.wal_buffer.lock();
      wal.write_record(&record, &mut pager)?;

      // Flush WAL to disk based on sync mode
      let should_flush = matches!(self.sync_mode, SyncMode::Full | SyncMode::Normal);
      if should_flush {
        wal.flush(&mut pager)?;
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
      header.change_counter += 1;

      // Persist header based on sync mode
      if self.sync_mode != SyncMode::Off {
        let header_bytes = header.serialize_to_page();
        pager.write_page(0, &header_bytes)?;

        if self.sync_mode == SyncMode::Full {
          // Full durability: fsync after WAL + header updates
          pager.sync()?;
        }
      }
    }

    let mut delta = self.delta.write();

    if let (Some((commit_ts, has_active_readers)), Some(mvcc)) =
      (commit_ts_for_mvcc, self.mvcc.as_ref())
    {
      if has_active_readers {
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
          if let Some(value_opt) = delta.get_node_prop(node_id, key_id) {
            return value_opt.cloned();
          }
          if let Some(ref snap) = *snapshot {
            if let Some(phys) = snap.get_phys_node(node_id) {
              return snap.get_node_prop(phys, key_id);
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
          if let Some(value_opt) = delta.get_edge_prop(src, etype, dst, key_id) {
            return value_opt.cloned();
          }
          if let Some(ref snap) = *snapshot {
            if let (Some(src_phys), Some(dst_phys)) =
              (snap.get_phys_node(src), snap.get_phys_node(dst))
            {
              if let Some(edge_idx) = snap.find_edge_index(src_phys, etype, dst_phys) {
                if let Some(snapshot_props) = snap.get_edge_props(edge_idx) {
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
          if let Some(node_delta) = delta.get_node_delta(node_id) {
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
            if let Some(phys) = snap.get_phys_node(node_id) {
              if let Some(labels) = snap.get_node_labels(phys) {
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
              let before_value = old_node_prop(*node_id, *key_id);
              if before_value == *after_value {
                continue;
              }
              if vc.get_node_prop_version(*node_id, *key_id).is_none() {
                vc.append_node_prop_version(*node_id, *key_id, before_value.clone(), 0, 0);
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
              if vc.get_node_label_version(*node_id, *label_id).is_none() {
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
              if vc.get_node_label_version(*node_id, *label_id).is_none() {
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
            let before_value = old_edge_prop(src, etype, dst, *key_id);
            if before_value == *after_value {
              continue;
            }
            if vc.get_edge_prop_version(src, etype, dst, *key_id).is_none() {
              vc.append_edge_prop_version(src, etype, dst, *key_id, before_value.clone(), 0, 0);
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
    }

    // Apply pending vector operations
    self.apply_pending_vectors(&pending.pending_vectors);

    merge_pending_delta(&mut delta, pending);
    drop(delta);

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

    Ok(())
  }

  /// Rollback the current transaction
  pub fn rollback(&self) -> Result<()> {
    let tx_handle = {
      let tid = std::thread::current().id();
      let mut current_tx = self.current_tx.lock();
      current_tx.remove(&tid).ok_or(KiteError::NoTransaction)?
    };
    let (txid, read_only) = {
      let tx = tx_handle.lock();
      (tx.txid, tx.read_only)
    };

    if read_only {
      // Read-only transactions don't need WAL
      if let Some(mvcc) = self.mvcc.as_ref() {
        let mut tx_mgr = mvcc.tx_manager.lock();
        tx_mgr.abort_tx(txid);
      }
      return Ok(());
    }

    if let Some(mvcc) = self.mvcc.as_ref() {
      let mut tx_mgr = mvcc.tx_manager.lock();
      tx_mgr.abort_tx(txid);
    }

    // Write ROLLBACK record to WAL
    let record = WalRecord::new(WalRecordType::Rollback, txid, build_rollback_payload());
    let mut pager = self.pager.lock();
    let mut wal = self.wal_buffer.lock();
    wal.write_record(&record, &mut pager)?;

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

  /// Write a WAL record (internal helper)
  pub(crate) fn write_wal(&self, record: WalRecord) -> Result<()> {
    let mut pager = self.pager.lock();
    let mut wal = self.wal_buffer.lock();
    wal.write_record(&record, &mut pager)?;
    Ok(())
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
          Some(value) => target.set_node_prop(node_id, key_id, value),
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
          Some(value) => target.set_node_prop(node_id, key_id, value),
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
        Some(value) => target.set_edge_prop(src, etype, dst, key_id, value),
        None => target.delete_edge_prop(src, etype, dst, key_id),
      }
    }
  }

  target.key_index.extend(pending.key_index.drain());
  target
    .key_index_deleted
    .extend(pending.key_index_deleted.drain());
}
