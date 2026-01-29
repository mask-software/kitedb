//! Transaction handling
//!
//! Provides begin, commit, and rollback operations for graph transactions.
//! All mutations happen within a transaction context.

use crate::core::wal::record::*;
use crate::error::{RayError, Result};
use crate::types::*;

use super::db::GraphDB;
use super::{edges, nodes};

// ============================================================================
// Transaction Handle
// ============================================================================

/// Handle for an active transaction
pub struct TxHandle<'a> {
  /// Reference to the database
  pub db: &'a GraphDB,
  /// Transaction state
  pub tx: TxState,
  /// Whether the transaction has been committed or rolled back
  finished: bool,
}

impl<'a> TxHandle<'a> {
  /// Create a new transaction handle
  pub fn new(db: &'a GraphDB, tx: TxState) -> Self {
    Self {
      db,
      tx,
      finished: false,
    }
  }

  /// Get the transaction ID
  pub fn txid(&self) -> TxId {
    self.tx.txid
  }

  /// Check if this is a read-only transaction
  pub fn is_read_only(&self) -> bool {
    self.tx.read_only
  }

  /// Get the snapshot timestamp for MVCC reads
  pub fn snapshot_ts(&self) -> u64 {
    self.tx.snapshot_ts
  }

  /// Check if the transaction is still active
  pub fn is_active(&self) -> bool {
    !self.finished
  }
}

// ============================================================================
// Transaction Operations
// ============================================================================

/// Begin a new transaction
pub fn begin_tx(db: &GraphDB) -> Result<TxHandle> {
  if db.read_only {
    return Err(RayError::ReadOnly);
  }

  if !db.mvcc_enabled() {
    let current = db.current_tx.lock();
    if current.is_some() {
      return Err(RayError::TransactionInProgress);
    }
  }

  let (txid, snapshot_ts) = if let Some(mvcc) = db.mvcc.as_ref() {
    let (txid, snapshot_ts) = {
      let mut tx_mgr = mvcc.tx_manager.lock();
      tx_mgr.begin_tx()
    };
    (txid, snapshot_ts)
  } else {
    (db.alloc_tx_id(), 0)
  };

  let tx = TxState::new(txid, false, snapshot_ts);

  if !db.mvcc_enabled() {
    let mut current = db.current_tx.lock();
    *current = Some(TxState::new(txid, false, snapshot_ts));
  }

  Ok(TxHandle::new(db, tx))
}

/// Begin a read-only transaction
pub fn begin_read_tx(db: &GraphDB) -> Result<TxHandle> {
  let (txid, snapshot_ts) = if let Some(mvcc) = db.mvcc.as_ref() {
    let (txid, snapshot_ts) = {
      let mut tx_mgr = mvcc.tx_manager.lock();
      tx_mgr.begin_tx()
    };
    (txid, snapshot_ts)
  } else {
    (db.alloc_tx_id(), 0)
  };

  let tx = TxState::new(txid, true, snapshot_ts);
  Ok(TxHandle::new(db, tx))
}

/// Commit a transaction
pub fn commit(handle: &mut TxHandle) -> Result<()> {
  if handle.finished {
    return Err(RayError::NoTransaction);
  }

  if handle.tx.read_only {
    if let Some(mvcc) = handle.db.mvcc.as_ref() {
      let mut tx_mgr = mvcc.tx_manager.lock();
      tx_mgr.abort_tx(handle.tx.txid);
    }
    handle.finished = true;
    return Ok(());
  }

  // MVCC: conflict detection + commit timestamp
  if let Some(mvcc) = handle.db.mvcc.as_ref() {
    let mut tx_mgr = mvcc.tx_manager.lock();
    if let Err(err) = mvcc.conflict_detector.validate_commit(&tx_mgr, handle.tx.txid) {
      return Err(RayError::Conflict {
        txid: err.txid,
        keys: err.conflicting_keys,
      });
    }

    let commit_ts = tx_mgr
      .commit_tx(handle.tx.txid)
      .map_err(|e| RayError::Internal(e.to_string()))?;

    let has_active_readers = tx_mgr.get_active_count() > 0;
    drop(tx_mgr);

    if has_active_readers {
      let mut vc = mvcc.version_chain.lock();

      // Node creations
      for (node_id, node_delta) in &handle.tx.pending_created_nodes {
        vc.append_node_version(
          *node_id,
          NodeVersionData {
            node_id: *node_id,
            delta: node_delta.clone(),
          },
          handle.tx.txid,
          commit_ts,
        );
      }

      // Node deletions
      for node_id in &handle.tx.pending_deleted_nodes {
        vc.delete_node_version(*node_id, handle.tx.txid, commit_ts);
      }

      // Edge additions
      for (src, patches) in &handle.tx.pending_out_add {
        for patch in patches {
          vc.append_edge_version(*src, patch.etype, patch.other, true, handle.tx.txid, commit_ts);
        }
      }

      // Edge deletions
      for (src, patches) in &handle.tx.pending_out_del {
        for patch in patches {
          vc.append_edge_version(*src, patch.etype, patch.other, false, handle.tx.txid, commit_ts);
        }
      }

      // Node property changes
      for (node_id, props) in &handle.tx.pending_node_props {
        let is_new = handle.tx.pending_created_nodes.contains_key(node_id);
        for (key_id, value) in props {
          if !is_new && vc.get_node_prop_version(*node_id, *key_id).is_none() {
            if let Some(old_value) =
              nodes::get_node_prop_committed(handle.db, *node_id, *key_id)
            {
              vc.append_node_prop_version(*node_id, *key_id, Some(old_value), 0, 0);
            }
          }
          vc.append_node_prop_version(
            *node_id,
            *key_id,
            value.clone(),
            handle.tx.txid,
            commit_ts,
          );
        }
      }

      // Edge property changes
      for ((src, etype, dst), props) in &handle.tx.pending_edge_props {
        for (key_id, value) in props {
          if vc
            .get_edge_prop_version(*src, *etype, *dst, *key_id)
            .is_none()
          {
            if let Some(old_value) =
              edges::get_edge_prop_committed(handle.db, *src, *etype, *dst, *key_id)
            {
              vc.append_edge_prop_version(
                *src,
                *etype,
                *dst,
                *key_id,
                Some(old_value),
                0,
                0,
              );
            }
          }
          vc.append_edge_prop_version(
            *src,
            *etype,
            *dst,
            *key_id,
            value.clone(),
            handle.tx.txid,
            commit_ts,
          );
        }
      }
    }
  }

  // Build WAL records
  let mut records: Vec<WalRecord> = Vec::new();
  records.push(WalRecord::new(
    WalRecordType::Begin,
    handle.tx.txid,
    build_begin_payload(),
  ));

  // Definitions first
  for (label_id, name) in &handle.tx.pending_new_labels {
    records.push(WalRecord::new(
      WalRecordType::DefineLabel,
      handle.tx.txid,
      build_define_label_payload(*label_id, name),
    ));
  }
  for (etype_id, name) in &handle.tx.pending_new_etypes {
    records.push(WalRecord::new(
      WalRecordType::DefineEtype,
      handle.tx.txid,
      build_define_etype_payload(*etype_id, name),
    ));
  }
  for (propkey_id, name) in &handle.tx.pending_new_propkeys {
    records.push(WalRecord::new(
      WalRecordType::DefinePropkey,
      handle.tx.txid,
      build_define_propkey_payload(*propkey_id, name),
    ));
  }

  // Node creations
  for (node_id, node_delta) in &handle.tx.pending_created_nodes {
    records.push(WalRecord::new(
      WalRecordType::CreateNode,
      handle.tx.txid,
      build_create_node_payload(*node_id, node_delta.key.as_deref()),
    ));

    if let Some(labels) = &node_delta.labels {
      for label_id in labels {
        records.push(WalRecord::new(
          WalRecordType::AddNodeLabel,
          handle.tx.txid,
          build_add_node_label_payload(*node_id, *label_id),
        ));
      }
    }

    if let Some(props) = handle.tx.pending_node_props.get(node_id) {
      for (key_id, value) in props {
        if let Some(value) = value {
          records.push(WalRecord::new(
            WalRecordType::SetNodeProp,
            handle.tx.txid,
            build_set_node_prop_payload(*node_id, *key_id, value),
          ));
        }
      }
    }
  }

  // Node deletions
  for node_id in &handle.tx.pending_deleted_nodes {
    records.push(WalRecord::new(
      WalRecordType::DeleteNode,
      handle.tx.txid,
      build_delete_node_payload(*node_id),
    ));
  }

  // Edge additions
  for (src, patches) in &handle.tx.pending_out_add {
    for patch in patches {
      records.push(WalRecord::new(
        WalRecordType::AddEdge,
        handle.tx.txid,
        build_add_edge_payload(*src, patch.etype, patch.other),
      ));
    }
  }

  // Edge deletions
  for (src, patches) in &handle.tx.pending_out_del {
    for patch in patches {
      records.push(WalRecord::new(
        WalRecordType::DeleteEdge,
        handle.tx.txid,
        build_delete_edge_payload(*src, patch.etype, patch.other),
      ));
    }
  }

  // Node property changes for existing nodes
  for (node_id, props) in &handle.tx.pending_node_props {
    if !handle.tx.pending_created_nodes.contains_key(node_id) {
      for (key_id, value) in props {
        if let Some(value) = value {
          records.push(WalRecord::new(
            WalRecordType::SetNodeProp,
            handle.tx.txid,
            build_set_node_prop_payload(*node_id, *key_id, value),
          ));
        } else {
          records.push(WalRecord::new(
            WalRecordType::DelNodeProp,
            handle.tx.txid,
            build_del_node_prop_payload(*node_id, *key_id),
          ));
        }
      }
    }
  }

  // Node label changes
  for (node_id, labels) in &handle.tx.pending_node_labels_add {
    for label_id in labels {
      records.push(WalRecord::new(
        WalRecordType::AddNodeLabel,
        handle.tx.txid,
        build_add_node_label_payload(*node_id, *label_id),
      ));
    }
  }
  for (node_id, labels) in &handle.tx.pending_node_labels_del {
    for label_id in labels {
      records.push(WalRecord::new(
        WalRecordType::RemoveNodeLabel,
        handle.tx.txid,
        build_remove_node_label_payload(*node_id, *label_id),
      ));
    }
  }

  // Edge property changes
  for ((src, etype, dst), props) in &handle.tx.pending_edge_props {
    for (key_id, value) in props {
      if let Some(value) = value {
        records.push(WalRecord::new(
          WalRecordType::SetEdgeProp,
          handle.tx.txid,
          build_set_edge_prop_payload(*src, *etype, *dst, *key_id, value),
        ));
      } else {
        records.push(WalRecord::new(
          WalRecordType::DelEdgeProp,
          handle.tx.txid,
          build_del_edge_prop_payload(*src, *etype, *dst, *key_id),
        ));
      }
    }
  }

  // Vector embeddings - set operations
  for ((node_id, prop_key_id), vector) in &handle.tx.pending_vector_sets {
    records.push(WalRecord::new(
      WalRecordType::SetNodeVector,
      handle.tx.txid,
      build_set_node_vector_payload(*node_id, *prop_key_id, vector),
    ));
  }

  // Vector embeddings - delete operations
  for (node_id, prop_key_id) in &handle.tx.pending_vector_deletes {
    records.push(WalRecord::new(
      WalRecordType::DelNodeVector,
      handle.tx.txid,
      build_del_node_vector_payload(*node_id, *prop_key_id),
    ));
  }

  records.push(WalRecord::new(
    WalRecordType::Commit,
    handle.tx.txid,
    build_commit_payload(),
  ));

  handle.db.flush_wal(&records)?;

  {
    let mut delta = handle.db.delta.write();

    for (label_id, name) in &handle.tx.pending_new_labels {
      delta.define_label(*label_id, name);
    }
    for (etype_id, name) in &handle.tx.pending_new_etypes {
      delta.define_etype(*etype_id, name);
    }
    for (propkey_id, name) in &handle.tx.pending_new_propkeys {
      delta.define_propkey(*propkey_id, name);
    }

    for (node_id, node_delta) in &handle.tx.pending_created_nodes {
      delta.create_node(*node_id, node_delta.key.as_deref());
      if let Some(labels) = &node_delta.labels {
        for label_id in labels {
          delta.add_node_label(*node_id, *label_id);
        }
      }
    }

    for node_id in &handle.tx.pending_deleted_nodes {
      delta.delete_node(*node_id);
    }

    for (src, patches) in &handle.tx.pending_out_add {
      for patch in patches {
        delta.add_edge(*src, patch.etype, patch.other);
      }
    }
    for (src, patches) in &handle.tx.pending_out_del {
      for patch in patches {
        delta.delete_edge(*src, patch.etype, patch.other);
      }
    }

    for (node_id, props) in &handle.tx.pending_node_props {
      for (key_id, value) in props {
        if let Some(value) = value {
          delta.set_node_prop(*node_id, *key_id, value.clone());
        } else {
          delta.delete_node_prop(*node_id, *key_id);
        }
      }
    }

    for (node_id, labels) in &handle.tx.pending_node_labels_add {
      for label_id in labels {
        delta.add_node_label(*node_id, *label_id);
      }
    }
    for (node_id, labels) in &handle.tx.pending_node_labels_del {
      for label_id in labels {
        delta.remove_node_label(*node_id, *label_id);
      }
    }

    for ((src, etype, dst), props) in &handle.tx.pending_edge_props {
      for (key_id, value) in props {
        if let Some(value) = value {
          delta.set_edge_prop(*src, *etype, *dst, *key_id, value.clone());
        } else {
          delta.delete_edge_prop(*src, *etype, *dst, *key_id);
        }
      }
    }
  }

  for (label_id, name) in &handle.tx.pending_new_labels {
    handle.db.update_label_mapping(*label_id, name);
  }
  for (etype_id, name) in &handle.tx.pending_new_etypes {
    handle.db.update_etype_mapping(*etype_id, name);
  }
  for (propkey_id, name) in &handle.tx.pending_new_propkeys {
    handle.db.update_propkey_mapping(*propkey_id, name);
  }

  {
    let mut stores = handle.db.vector_stores.write();

    for ((node_id, prop_key_id), vector) in &handle.tx.pending_vector_sets {
      let store = stores.entry(*prop_key_id).or_insert_with(|| {
        let config = crate::vector::types::VectorStoreConfig::new(vector.len());
        crate::vector::store::create_vector_store(config)
      });
      let _ = crate::vector::store::vector_store_insert(store, *node_id, vector);
    }

    for (node_id, prop_key_id) in &handle.tx.pending_vector_deletes {
      if let Some(store) = stores.get_mut(prop_key_id) {
        crate::vector::store::vector_store_delete(store, *node_id);
      }
    }
  }

  if !handle.db.mvcc_enabled() {
    let mut current = handle.db.current_tx.lock();
    *current = None;
  }

  handle.finished = true;
  Ok(())
}

/// Rollback a transaction
pub fn rollback(handle: &mut TxHandle) -> Result<()> {
  if handle.finished {
    return Err(RayError::NoTransaction);
  }

  if let Some(mvcc) = handle.db.mvcc.as_ref() {
    let mut tx_mgr = mvcc.tx_manager.lock();
    tx_mgr.abort_tx(handle.tx.txid);
  }

  if !handle.db.mvcc_enabled() && !handle.tx.read_only {
    let mut current = handle.db.current_tx.lock();
    *current = None;
  }

  handle.finished = true;
  Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;
  use crate::graph::db::{close_graph_db, open_graph_db, OpenOptions};
  use tempfile::tempdir;

  #[test]
  fn test_begin_tx() {
    let temp_dir = tempdir().unwrap();
    let db = open_graph_db(temp_dir.path(), OpenOptions::new()).unwrap();

    let tx = begin_tx(&db).unwrap();
    assert!(!tx.is_read_only());
    assert!(tx.is_active());

    // Should fail - transaction already in progress
    assert!(begin_tx(&db).is_err());

    close_graph_db(db).unwrap();
  }

  #[test]
  fn test_begin_read_tx() {
    let temp_dir = tempdir().unwrap();
    let db = open_graph_db(temp_dir.path(), OpenOptions::new()).unwrap();

    // Multiple read transactions should be allowed
    let tx1 = begin_read_tx(&db).unwrap();
    let tx2 = begin_read_tx(&db).unwrap();

    assert!(tx1.is_read_only());
    assert!(tx2.is_read_only());

    close_graph_db(db).unwrap();
  }

  #[test]
  fn test_commit_empty_tx() {
    let temp_dir = tempdir().unwrap();
    let db = open_graph_db(temp_dir.path(), OpenOptions::new()).unwrap();

    let mut tx = begin_tx(&db).unwrap();
    commit(&mut tx).unwrap();

    assert!(!tx.is_active());

    close_graph_db(db).unwrap();
  }

  #[test]
  fn test_rollback() {
    let temp_dir = tempdir().unwrap();
    let db = open_graph_db(temp_dir.path(), OpenOptions::new()).unwrap();

    let mut tx = begin_tx(&db).unwrap();
    rollback(&mut tx).unwrap();

    assert!(!tx.is_active());

    // Should be able to start new transaction after rollback
    let tx2 = begin_tx(&db).unwrap();
    assert!(tx2.is_active());

    close_graph_db(db).unwrap();
  }
}
