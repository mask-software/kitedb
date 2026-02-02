//! Transaction management for SingleFileDB
//!
//! Handles begin, commit, and rollback operations.

use crate::core::wal::record::{
  build_begin_payload, build_commit_payload, build_rollback_payload, WalRecord,
};
use crate::error::{KiteError, Result};
use crate::types::*;

use super::open::SyncMode;
use super::{SingleFileDB, SingleFileTxState};

impl SingleFileDB {
  /// Begin a new transaction
  pub fn begin(&self, read_only: bool) -> Result<TxId> {
    if self.read_only && !read_only {
      return Err(KiteError::ReadOnly);
    }

    let mut current_tx = self.current_tx.lock();
    if current_tx.is_some() {
      return Err(KiteError::TransactionInProgress);
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

    let delta_snapshot = if read_only {
      None
    } else {
      Some(self.delta.read().clone())
    };

    *current_tx = Some(SingleFileTxState::new(
      txid,
      read_only,
      snapshot_ts,
      delta_snapshot,
    ));
    Ok(txid)
  }

  /// Commit the current transaction
  pub fn commit(&self) -> Result<()> {
    // Take the transaction and release the lock immediately
    let tx = {
      let mut current_tx = self.current_tx.lock();
      current_tx.take().ok_or(KiteError::NoTransaction)?
    };

    if tx.read_only {
      // Read-only transactions don't need WAL
      if let Some(mvcc) = self.mvcc.as_ref() {
        let mut tx_mgr = mvcc.tx_manager.lock();
        tx_mgr.abort_tx(tx.txid);
      }
      return Ok(());
    }

    let mut commit_ts_for_mvcc = None;
    if let Some(mvcc) = self.mvcc.as_ref() {
      let mut tx_mgr = mvcc.tx_manager.lock();
      if let Err(err) = mvcc.conflict_detector.validate_commit(&tx_mgr, tx.txid) {
        tx_mgr.abort_tx(tx.txid);
        if let Some(delta_snapshot) = tx.delta_snapshot {
          *self.delta.write() = delta_snapshot;
        }
        return Err(KiteError::Conflict {
          txid: err.txid,
          keys: err.conflicting_keys,
        });
      }

      let commit_ts = tx_mgr
        .commit_tx(tx.txid)
        .map_err(|e| KiteError::Internal(e.to_string()))?;
      commit_ts_for_mvcc = Some((commit_ts, tx_mgr.get_active_count() > 0));
    }

    if let (Some((commit_ts, has_active_readers)), Some(delta_snapshot)) =
      (commit_ts_for_mvcc, tx.delta_snapshot.as_ref())
    {
      if has_active_readers {
        let current_delta = self.delta.read();
        let snapshot = self.snapshot.read();
        let mvcc = self.mvcc.as_ref().unwrap();
        let mut vc = mvcc.version_chain.lock();

        let txid = tx.txid;

        for (&node_id, node_delta) in &current_delta.created_nodes {
          if !delta_snapshot.created_nodes.contains_key(&node_id) {
            vc.append_node_version(
              node_id,
              NodeVersionData {
                node_id,
                delta: node_delta.clone(),
              },
              txid,
              commit_ts,
            );
          }
        }

        for node_id in delta_snapshot.created_nodes.keys() {
          if !current_delta.created_nodes.contains_key(node_id) {
            vc.delete_node_version(*node_id, txid, commit_ts);
          }
        }

        for node_id in current_delta
          .deleted_nodes
          .difference(&delta_snapshot.deleted_nodes)
        {
          vc.delete_node_version(*node_id, txid, commit_ts);
        }

        let mut added_edges: Vec<(NodeId, ETypeId, NodeId)> = Vec::new();
        let mut deleted_edges: Vec<(NodeId, ETypeId, NodeId)> = Vec::new();

        for (&src, patches) in &current_delta.out_add {
          let before = delta_snapshot.out_add.get(&src);
          for patch in patches {
            if before.map(|set| set.contains(patch)).unwrap_or(false) {
              continue;
            }
            added_edges.push((src, patch.etype, patch.other));
          }
        }

        for (&src, patches) in &current_delta.out_del {
          let before = delta_snapshot.out_del.get(&src);
          for patch in patches {
            if before.map(|set| set.contains(patch)).unwrap_or(false) {
              continue;
            }
            deleted_edges.push((src, patch.etype, patch.other));
          }
        }

        for (&src, patches) in &delta_snapshot.out_add {
          let after = current_delta.out_add.get(&src);
          for patch in patches {
            if after.map(|set| set.contains(patch)).unwrap_or(false) {
              continue;
            }
            deleted_edges.push((src, patch.etype, patch.other));
          }
        }

        for (&src, patches) in &delta_snapshot.out_del {
          let after = current_delta.out_del.get(&src);
          for patch in patches {
            if after.map(|set| set.contains(patch)).unwrap_or(false) {
              continue;
            }
            added_edges.push((src, patch.etype, patch.other));
          }
        }

        for (src, etype, dst) in added_edges {
          vc.append_edge_version(src, etype, dst, true, txid, commit_ts);
        }
        for (src, etype, dst) in deleted_edges {
          vc.append_edge_version(src, etype, dst, false, txid, commit_ts);
        }

        let old_node_prop = |node_id: NodeId, key_id: PropKeyId| -> Option<PropValue> {
          let delta = delta_snapshot;
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
          let delta = delta_snapshot;
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

        for (node_id, node_delta) in current_delta
          .created_nodes
          .iter()
          .chain(current_delta.modified_nodes.iter())
        {
          if current_delta.deleted_nodes.contains(node_id) {
            continue;
          }
          let before_props = delta_snapshot
            .created_nodes
            .get(node_id)
            .or_else(|| delta_snapshot.modified_nodes.get(node_id))
            .and_then(|d| d.props.as_ref());
          let after_props = node_delta.props.as_ref();

          if let Some(after_map) = after_props {
            for (key_id, after_value) in after_map {
              let before_value = before_props.and_then(|m| m.get(key_id));
              if before_value == Some(after_value) {
                continue;
              }
              let is_new_node = !delta_snapshot.created_nodes.contains_key(node_id)
                && current_delta.created_nodes.contains_key(node_id);
              if !is_new_node && vc.get_node_prop_version(*node_id, *key_id).is_none() {
                let old_value = old_node_prop(*node_id, *key_id);
                vc.append_node_prop_version(*node_id, *key_id, old_value, 0, 0);
              }
              vc.append_node_prop_version(
                *node_id,
                *key_id,
                after_value.clone(),
                txid,
                commit_ts,
              );
            }
          }
        }

        for (edge_key, after_props) in &current_delta.edge_props {
          let before_props = delta_snapshot.edge_props.get(edge_key);
          for (key_id, after_value) in after_props {
            let before_value = before_props.and_then(|m| m.get(key_id));
            if before_value == Some(after_value) {
              continue;
            }
            let (src, etype, dst) = *edge_key;
            if vc
              .get_edge_prop_version(src, etype, dst, *key_id)
              .is_none()
            {
              vc.append_edge_prop_version(
                src,
                etype,
                dst,
                *key_id,
                old_edge_prop(src, etype, dst, *key_id),
                0,
                0,
              );
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

    // Write COMMIT record to WAL
    let record = WalRecord::new(WalRecordType::Commit, tx.txid, build_commit_payload());
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

    // Apply pending vector operations
    self.apply_pending_vectors();

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
    let mut current_tx = self.current_tx.lock();
    let tx = current_tx.take().ok_or(KiteError::NoTransaction)?;

    if tx.read_only {
      // Read-only transactions don't need WAL
      if let Some(mvcc) = self.mvcc.as_ref() {
        let mut tx_mgr = mvcc.tx_manager.lock();
        tx_mgr.abort_tx(tx.txid);
      }
      return Ok(());
    }

    if let Some(mvcc) = self.mvcc.as_ref() {
      let mut tx_mgr = mvcc.tx_manager.lock();
      tx_mgr.abort_tx(tx.txid);
    }

    // Write ROLLBACK record to WAL
    let record = WalRecord::new(WalRecordType::Rollback, tx.txid, build_rollback_payload());
    let mut pager = self.pager.lock();
    let mut wal = self.wal_buffer.lock();
    wal.write_record(&record, &mut pager)?;

    // Discard pending writes (rollback doesn't need to be durable)
    wal.discard_pending();

    if let Some(delta_snapshot) = tx.delta_snapshot {
      *self.delta.write() = delta_snapshot;
    }

    Ok(())
  }

  /// Check if there's an active transaction
  pub fn has_transaction(&self) -> bool {
    self.current_tx.lock().is_some()
  }

  /// Get the current transaction ID (if any)
  pub fn current_txid(&self) -> Option<TxId> {
    self.current_tx.lock().as_ref().map(|tx| tx.txid)
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
    let current_tx = self.current_tx.lock();
    match current_tx.as_ref() {
      Some(tx) if !tx.read_only => Ok(tx.txid),
      Some(_) => Err(KiteError::ReadOnly),
      None => Err(KiteError::NoTransaction),
    }
  }
}
