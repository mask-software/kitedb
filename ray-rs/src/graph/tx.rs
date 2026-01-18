//! Transaction handling
//!
//! Provides begin, commit, and rollback operations for graph transactions.
//! All mutations happen within a transaction context.

use crate::core::wal::record::*;
use crate::error::{RayError, Result};
use crate::types::*;

use super::db::{GraphDB, TxState};

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

  /// Add a WAL record to this transaction
  pub fn add_record(&mut self, record: WalRecord) -> Result<()> {
    if self.tx.read_only {
      return Err(RayError::ReadOnly);
    }
    self.tx.wal_records.push(record);
    Ok(())
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

  // Check for existing transaction (single-writer model for now)
  {
    let current = db.current_tx.lock();
    if current.is_some() {
      return Err(RayError::TransactionInProgress);
    }
  }

  // Allocate transaction ID
  let txid = db.alloc_tx_id();
  let snapshot_ts = 0; // TODO: Get from MVCC manager if enabled

  let tx = TxState::new(txid, false, snapshot_ts);

  // Set as current transaction
  {
    let mut current = db.current_tx.lock();
    *current = Some(TxState::new(txid, false, snapshot_ts));
  }

  Ok(TxHandle::new(db, tx))
}

/// Begin a read-only transaction
pub fn begin_read_tx(db: &GraphDB) -> Result<TxHandle> {
  let txid = db.alloc_tx_id();
  let snapshot_ts = 0; // TODO: Get from MVCC manager

  let tx = TxState::new(txid, true, snapshot_ts);
  Ok(TxHandle::new(db, tx))
}

/// Commit a transaction
pub fn commit(handle: &mut TxHandle) -> Result<()> {
  if handle.finished {
    return Err(RayError::NoTransaction);
  }

  if handle.tx.read_only {
    // Read-only transactions just need to clean up
    handle.finished = true;
    return Ok(());
  }

  // Build BEGIN record
  let begin_record = WalRecord::new(WalRecordType::Begin, handle.tx.txid, build_begin_payload());

  // Build COMMIT record
  let commit_record = WalRecord::new(
    WalRecordType::Commit,
    handle.tx.txid,
    build_commit_payload(),
  );

  // Collect all WAL records
  let mut all_records = Vec::with_capacity(handle.tx.wal_records.len() + 2);
  all_records.push(begin_record);
  all_records.append(&mut handle.tx.wal_records);
  all_records.push(commit_record);

  // Flush to WAL
  handle.db.flush_wal(&all_records)?;

  // Apply changes to delta
  // This happens by processing the WAL records we just wrote
  apply_records_to_delta(handle.db, &all_records)?;

  // Clear current transaction
  {
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

  // Clear WAL records - nothing was written yet
  handle.tx.wal_records.clear();

  // Clear current transaction
  if !handle.tx.read_only {
    let mut current = handle.db.current_tx.lock();
    *current = None;
  }

  handle.finished = true;
  Ok(())
}

/// Apply WAL records to the delta state
fn apply_records_to_delta(db: &GraphDB, records: &[WalRecord]) -> Result<()> {
  let mut delta = db.delta.write();

  for record in records {
    match record.record_type {
      WalRecordType::Begin | WalRecordType::Commit | WalRecordType::Rollback => {
        // Control records don't affect delta
      }
      WalRecordType::CreateNode => {
        if let Some(data) = parse_create_node_payload(&record.payload) {
          delta.create_node(data.node_id, data.key.as_deref());
        }
      }
      WalRecordType::DeleteNode => {
        if let Some(data) = parse_delete_node_payload(&record.payload) {
          delta.delete_node(data.node_id);
        }
      }
      WalRecordType::AddEdge => {
        if let Some(data) = parse_add_edge_payload(&record.payload) {
          delta.add_edge(data.src, data.etype, data.dst);
        }
      }
      WalRecordType::DeleteEdge => {
        if let Some(data) = parse_delete_edge_payload(&record.payload) {
          delta.delete_edge(data.src, data.etype, data.dst);
        }
      }
      WalRecordType::SetNodeProp => {
        if let Some(data) = parse_set_node_prop_payload(&record.payload) {
          delta.set_node_prop(data.node_id, data.key_id, data.value);
        }
      }
      WalRecordType::DelNodeProp => {
        if let Some(data) = parse_del_node_prop_payload(&record.payload) {
          delta.delete_node_prop(data.node_id, data.key_id);
        }
      }
      WalRecordType::DefineLabel => {
        if let Some(data) = parse_define_label_payload(&record.payload) {
          delta.define_label(data.label_id, &data.name);
        }
      }
      WalRecordType::DefineEtype => {
        if let Some(data) = parse_define_etype_payload(&record.payload) {
          delta.define_etype(data.label_id, &data.name);
        }
      }
      WalRecordType::DefinePropkey => {
        if let Some(data) = parse_define_propkey_payload(&record.payload) {
          delta.define_propkey(data.label_id, &data.name);
        }
      }
      WalRecordType::SetEdgeProp => {
        if let Some(data) = parse_set_edge_prop_payload(&record.payload) {
          delta.set_edge_prop(data.src, data.etype, data.dst, data.key_id, data.value);
        }
      }
      WalRecordType::DelEdgeProp => {
        if let Some(data) = parse_del_edge_prop_payload(&record.payload) {
          delta.delete_edge_prop(data.src, data.etype, data.dst, data.key_id);
        }
      }
      _ => {
        // Other record types (vectors, etc.) - skip for now
      }
    }
  }

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
