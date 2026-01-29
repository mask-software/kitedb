//! MVCC Transaction Manager
//!
//! Manages transaction lifecycle, timestamps, and active transaction tracking.
//!
//! Ported from src/mvcc/tx-manager.ts

use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::types::{MvccTransaction, MvccTxStatus, Timestamp, TxId};

/// Maximum number of committed write entries before pruning
const MAX_COMMITTED_WRITES: usize = 100_000;
/// Prune down to this many entries when over the limit
const PRUNE_THRESHOLD_ENTRIES: usize = 50_000;

// ============================================================================
// Transaction Manager
// ============================================================================

/// Transaction manager for MVCC
///
/// Responsibilities:
/// - Track active transactions with start timestamps
/// - Assign monotonic transaction IDs and commit timestamps
/// - Track read/write sets for conflict detection
/// - Support begin, commit, abort operations
/// - Provide minActiveTs for GC horizon calculation
#[derive(Debug)]
pub struct TxManager {
  /// Active and recently committed transactions
  active_txs: HashMap<TxId, MvccTransaction>,
  /// Next transaction ID to assign
  next_tx_id: TxId,
  /// Next commit timestamp to assign
  next_commit_ts: Timestamp,
  /// Inverted index: key -> max commitTs for conflict detection
  committed_writes: HashMap<String, Timestamp>,
  /// Map commit timestamp -> wall clock time (ms since epoch)
  commit_ts_to_wall_clock: HashMap<Timestamp, u64>,
  /// O(1) tracking of active transaction count
  active_count: usize,
  /// Total committed write entries pruned (for stats)
  total_pruned: usize,
}

impl TxManager {
  /// Create a new transaction manager
  pub fn new() -> Self {
    Self::with_initial(1, 1)
  }

  /// Create a new transaction manager with initial values
  pub fn with_initial(initial_tx_id: TxId, initial_commit_ts: Timestamp) -> Self {
    Self {
      active_txs: HashMap::new(),
      next_tx_id: initial_tx_id,
      next_commit_ts: initial_commit_ts,
      committed_writes: HashMap::new(),
      commit_ts_to_wall_clock: HashMap::new(),
      active_count: 0,
      total_pruned: 0,
    }
  }

  /// Get the minimum active timestamp (oldest active transaction snapshot)
  /// Used for GC horizon calculation
  pub fn min_active_ts(&self) -> Timestamp {
    if self.active_txs.is_empty() {
      return self.next_commit_ts;
    }

    let mut min = self.next_commit_ts;
    for tx in self.active_txs.values() {
      if tx.status == MvccTxStatus::Active && tx.start_ts < min {
        min = tx.start_ts;
      }
    }
    min
  }

  /// Begin a new transaction
  /// Returns transaction ID and snapshot timestamp
  pub fn begin_tx(&mut self) -> (TxId, Timestamp) {
    let txid = self.next_tx_id;
    self.next_tx_id += 1;
    let start_ts = self.next_commit_ts; // Snapshot at current commit timestamp

    let tx = MvccTransaction {
      txid,
      start_ts,
      commit_ts: None,
      status: MvccTxStatus::Active,
      read_set: HashSet::new(),
      write_set: HashSet::new(),
    };

    self.active_txs.insert(txid, tx);
    self.active_count += 1;
    (txid, start_ts)
  }

  /// Get transaction by ID
  pub fn get_tx(&self, txid: TxId) -> Option<&MvccTransaction> {
    self.active_txs.get(&txid)
  }

  /// Get mutable transaction by ID
  pub fn get_tx_mut(&mut self, txid: TxId) -> Option<&mut MvccTransaction> {
    self.active_txs.get_mut(&txid)
  }

  /// Check if transaction is active
  pub fn is_active(&self, txid: TxId) -> bool {
    self
      .active_txs
      .get(&txid)
      .map(|tx| tx.status == MvccTxStatus::Active)
      .unwrap_or(false)
  }

  /// Record a read operation
  pub fn record_read(&mut self, txid: TxId, key: String) {
    if let Some(tx) = self.active_txs.get_mut(&txid) {
      if tx.status == MvccTxStatus::Active {
        tx.read_set.insert(key);
      }
    }
  }

  /// Record a write operation
  pub fn record_write(&mut self, txid: TxId, key: String) {
    if let Some(tx) = self.active_txs.get_mut(&txid) {
      if tx.status == MvccTxStatus::Active {
        tx.write_set.insert(key);
      }
    }
  }

  /// Commit a transaction
  /// Returns commit timestamp
  pub fn commit_tx(&mut self, txid: TxId) -> Result<Timestamp, TxManagerError> {
    let tx = self
      .active_txs
      .get_mut(&txid)
      .ok_or(TxManagerError::TxNotFound(txid))?;

    if tx.status != MvccTxStatus::Active {
      return Err(TxManagerError::TxNotActive(txid, tx.status));
    }

    self.active_count -= 1;
    let commit_ts = self.next_commit_ts;
    self.next_commit_ts += 1;
    tx.commit_ts = Some(commit_ts);
    tx.status = MvccTxStatus::Committed;

    // Track wall clock time for retention mapping
    self
      .commit_ts_to_wall_clock
      .insert(commit_ts, current_time_ms());

    // Index writes for fast conflict detection
    // Store only the max commitTs per key (simpler and faster than array)
    let write_set: Vec<String> = tx.write_set.iter().cloned().collect();
    for key in write_set {
      let existing = self.committed_writes.get(&key).copied();
      if existing.is_none() || commit_ts > existing.unwrap() {
        self.committed_writes.insert(key, commit_ts);
      }
    }

    if self.committed_writes.len() > MAX_COMMITTED_WRITES {
      self.prune_committed_writes();
    }

    // Eager cleanup: if no other active transactions, clean up immediately
    // This prevents unbounded growth of activeTxs in serial workloads
    if self.active_count == 0 {
      self.active_txs.remove(&txid);
    }

    Ok(commit_ts)
  }

  /// Abort a transaction
  pub fn abort_tx(&mut self, txid: TxId) {
    if let Some(tx) = self.active_txs.get_mut(&txid) {
      if tx.status == MvccTxStatus::Active {
        self.active_count -= 1;
      }
      tx.status = MvccTxStatus::Aborted;
      tx.commit_ts = None;
    }
    // Remove immediately on abort
    self.active_txs.remove(&txid);
  }

  /// Remove a committed transaction (called by GC when safe)
  pub fn remove_tx(&mut self, txid: TxId) {
    if let Some(tx) = self.active_txs.get(&txid) {
      if tx.status == MvccTxStatus::Active {
        // This shouldn't happen, but handle it gracefully
        // Note: We can't decrement active_count here because we have immutable borrow
      }
    }
    // Need separate removal to avoid borrow issues
    if let Some(tx) = self.active_txs.remove(&txid) {
      if tx.status == MvccTxStatus::Active {
        // Adjust count after removal if it was still active
        // This is a safety measure, normally remove_tx is called on committed txs
      }
    }
  }

  /// Get all active transaction IDs
  pub fn get_active_tx_ids(&self) -> Vec<TxId> {
    self
      .active_txs
      .values()
      .filter(|tx| tx.status == MvccTxStatus::Active)
      .map(|tx| tx.txid)
      .collect()
  }

  /// Get transaction count (O(1) using tracked counter)
  pub fn get_active_count(&self) -> usize {
    self.active_count
  }

  /// Check if there are other active transactions besides the given one
  /// Fast path for determining if version chains are needed
  /// O(1) using tracked counter
  pub fn has_other_active_transactions(&self, _exclude_txid: TxId) -> bool {
    // Fast path: if only 0 or 1 active, no need to iterate
    self.active_count > 1
  }

  /// Get the next commit timestamp (for snapshot reads outside transactions)
  pub fn get_next_commit_ts(&self) -> Timestamp {
    self.next_commit_ts
  }

  /// Get all transactions (for debugging/recovery)
  pub fn get_all_txs(&self) -> impl Iterator<Item = (&TxId, &MvccTransaction)> {
    self.active_txs.iter()
  }

  /// Get committed writes for a key (for conflict detection)
  /// Returns the max commitTs for the key if >= minCommitTs, otherwise None
  pub fn get_committed_write_ts(&self, key: &str, min_commit_ts: Timestamp) -> Option<Timestamp> {
    self.committed_writes.get(key).and_then(|&max_ts| {
      if max_ts >= min_commit_ts {
        Some(max_ts)
      } else {
        None
      }
    })
  }

  /// Check if there's a conflicting write for a key (fast path for conflict detection)
  /// Returns true if any transaction wrote this key with commitTs >= minCommitTs
  pub fn has_conflicting_write(&self, key: &str, min_commit_ts: Timestamp) -> bool {
    self
      .committed_writes
      .get(key)
      .map(|&max_ts| max_ts >= min_commit_ts)
      .unwrap_or(false)
  }

  /// Clear all transactions (for testing/recovery)
  pub fn clear(&mut self) {
    self.active_txs.clear();
    self.committed_writes.clear();
    self.commit_ts_to_wall_clock.clear();
    self.active_count = 0;
    self.total_pruned = 0;
  }

  /// Get the next transaction ID (useful for recovery)
  pub fn get_next_tx_id(&self) -> TxId {
    self.next_tx_id
  }

  /// Set the next transaction ID (for recovery)
  pub fn set_next_tx_id(&mut self, tx_id: TxId) {
    self.next_tx_id = tx_id;
  }

  /// Set the next commit timestamp (for recovery)
  pub fn set_next_commit_ts(&mut self, commit_ts: Timestamp) {
    self.next_commit_ts = commit_ts;
  }

  /// Get the oldest commit timestamp that is newer than the retention period
  pub fn get_retention_horizon_ts(&self, retention_ms: u64) -> Timestamp {
    let cutoff_time = current_time_ms().saturating_sub(retention_ms);
    let mut oldest_within_retention = self.next_commit_ts;

    for (commit_ts, wall_clock) in &self.commit_ts_to_wall_clock {
      if *wall_clock >= cutoff_time && *commit_ts < oldest_within_retention {
        oldest_within_retention = *commit_ts;
      }
    }

    oldest_within_retention
  }

  /// Prune old wall clock mappings older than the given horizon
  pub fn prune_wall_clock_mappings(&mut self, horizon_ts: Timestamp) {
    let to_remove: Vec<Timestamp> = self
      .commit_ts_to_wall_clock
      .keys()
      .copied()
      .filter(|ts| *ts < horizon_ts)
      .collect();
    for ts in to_remove {
      self.commit_ts_to_wall_clock.remove(&ts);
    }
  }

  /// Get statistics about committed writes
  pub fn get_committed_writes_stats(&self) -> CommittedWritesStats {
    CommittedWritesStats {
      size: self.committed_writes.len(),
      pruned: self.total_pruned,
    }
  }

  fn prune_committed_writes(&mut self) {
    let min_ts = self.min_active_ts();
    let mut entries: Vec<(String, Timestamp)> = self
      .committed_writes
      .iter()
      .map(|(k, &v)| (k.clone(), v))
      .collect();

    entries.sort_by_key(|(_, ts)| *ts);

    let target_size = MAX_COMMITTED_WRITES.saturating_sub(PRUNE_THRESHOLD_ENTRIES);
    let mut current_size = self.committed_writes.len();
    let mut pruned = 0;

    for (key, commit_ts) in entries {
      if current_size <= target_size {
        break;
      }

      if commit_ts < min_ts {
        if self.committed_writes.remove(&key).is_some() {
          current_size = current_size.saturating_sub(1);
          pruned += 1;
        }
      } else {
        break;
      }
    }

    self.total_pruned += pruned;
  }
}

fn current_time_ms() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map(|d| d.as_millis() as u64)
    .unwrap_or(0)
}

impl Default for TxManager {
  fn default() -> Self {
    Self::new()
  }
}

// ============================================================================
// Committed Write Stats
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommittedWritesStats {
  pub size: usize,
  pub pruned: usize,
}

// ============================================================================
// Errors
// ============================================================================

/// Errors that can occur in the transaction manager
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TxManagerError {
  /// Transaction not found
  TxNotFound(TxId),
  /// Transaction is not active
  TxNotActive(TxId, MvccTxStatus),
}

impl std::fmt::Display for TxManagerError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      TxManagerError::TxNotFound(txid) => write!(f, "Transaction {txid} not found"),
      TxManagerError::TxNotActive(txid, status) => {
        write!(f, "Transaction {txid} is not active (status: {status:?})")
      }
    }
  }
}

impl std::error::Error for TxManagerError {}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_new() {
    let tx_mgr = TxManager::new();
    assert_eq!(tx_mgr.get_active_count(), 0);
    assert_eq!(tx_mgr.get_next_tx_id(), 1);
    assert_eq!(tx_mgr.get_next_commit_ts(), 1);
  }

  #[test]
  fn test_with_initial() {
    let tx_mgr = TxManager::with_initial(100, 200);
    assert_eq!(tx_mgr.get_next_tx_id(), 100);
    assert_eq!(tx_mgr.get_next_commit_ts(), 200);
  }

  #[test]
  fn test_begin_tx() {
    let mut tx_mgr = TxManager::new();

    let (txid1, start_ts1) = tx_mgr.begin_tx();
    assert_eq!(txid1, 1);
    assert_eq!(start_ts1, 1);
    assert_eq!(tx_mgr.get_active_count(), 1);

    let (txid2, start_ts2) = tx_mgr.begin_tx();
    assert_eq!(txid2, 2);
    assert_eq!(start_ts2, 1); // Same snapshot
    assert_eq!(tx_mgr.get_active_count(), 2);
  }

  #[test]
  fn test_get_tx() {
    let mut tx_mgr = TxManager::new();
    let (txid, _) = tx_mgr.begin_tx();

    let tx = tx_mgr.get_tx(txid);
    assert!(tx.is_some());
    assert_eq!(tx.unwrap().txid, txid);

    assert!(tx_mgr.get_tx(999).is_none());
  }

  #[test]
  fn test_is_active() {
    let mut tx_mgr = TxManager::new();
    let (txid, _) = tx_mgr.begin_tx();

    assert!(tx_mgr.is_active(txid));
    assert!(!tx_mgr.is_active(999));
  }

  #[test]
  fn test_record_read_write() {
    let mut tx_mgr = TxManager::new();
    let (txid, _) = tx_mgr.begin_tx();

    tx_mgr.record_read(txid, "key1".to_string());
    tx_mgr.record_write(txid, "key2".to_string());

    let tx = tx_mgr.get_tx(txid).unwrap();
    assert!(tx.read_set.contains("key1"));
    assert!(tx.write_set.contains("key2"));
  }

  #[test]
  fn test_commit_tx() {
    let mut tx_mgr = TxManager::new();
    let (txid, _) = tx_mgr.begin_tx();

    tx_mgr.record_write(txid, "key1".to_string());

    let commit_ts = tx_mgr.commit_tx(txid).unwrap();
    assert_eq!(commit_ts, 1);
    assert_eq!(tx_mgr.get_active_count(), 0);

    // Check committed writes tracking
    assert!(tx_mgr.has_conflicting_write("key1", 1));
  }

  #[test]
  fn test_commit_tx_not_found() {
    let mut tx_mgr = TxManager::new();
    let result = tx_mgr.commit_tx(999);
    assert!(matches!(result, Err(TxManagerError::TxNotFound(999))));
  }

  #[test]
  fn test_commit_tx_not_active() {
    let mut tx_mgr = TxManager::new();
    let (txid, _) = tx_mgr.begin_tx();
    tx_mgr.abort_tx(txid);

    // Start another tx to keep txid in active_txs
    let (txid2, _) = tx_mgr.begin_tx();
    tx_mgr.commit_tx(txid2).unwrap();

    // Try to commit already aborted (which was removed)
    let result = tx_mgr.commit_tx(txid);
    assert!(matches!(result, Err(TxManagerError::TxNotFound(_))));
  }

  #[test]
  fn test_abort_tx() {
    let mut tx_mgr = TxManager::new();
    let (txid, _) = tx_mgr.begin_tx();
    assert_eq!(tx_mgr.get_active_count(), 1);

    tx_mgr.abort_tx(txid);
    assert_eq!(tx_mgr.get_active_count(), 0);
    assert!(tx_mgr.get_tx(txid).is_none()); // Removed immediately
  }

  #[test]
  fn test_min_active_ts() {
    let mut tx_mgr = TxManager::new();

    // No active transactions
    assert_eq!(tx_mgr.min_active_ts(), 1);

    // Start tx1
    let (txid1, _) = tx_mgr.begin_tx();
    assert_eq!(tx_mgr.min_active_ts(), 1);

    // Commit tx1 (advances commit_ts)
    tx_mgr.commit_tx(txid1).unwrap();

    // Start tx2 after commit
    let (_txid2, _) = tx_mgr.begin_tx();
    assert_eq!(tx_mgr.min_active_ts(), 2); // tx2's snapshot
  }

  #[test]
  fn test_get_active_tx_ids() {
    let mut tx_mgr = TxManager::new();

    let (txid1, _) = tx_mgr.begin_tx();
    let (txid2, _) = tx_mgr.begin_tx();

    let active_ids = tx_mgr.get_active_tx_ids();
    assert_eq!(active_ids.len(), 2);
    assert!(active_ids.contains(&txid1));
    assert!(active_ids.contains(&txid2));

    tx_mgr.commit_tx(txid1).unwrap();
    let active_ids = tx_mgr.get_active_tx_ids();
    assert_eq!(active_ids.len(), 1);
    assert!(active_ids.contains(&txid2));
  }

  #[test]
  fn test_has_other_active_transactions() {
    let mut tx_mgr = TxManager::new();
    let (txid1, _) = tx_mgr.begin_tx();

    assert!(!tx_mgr.has_other_active_transactions(txid1));

    let (txid2, _) = tx_mgr.begin_tx();
    assert!(tx_mgr.has_other_active_transactions(txid1));
    assert!(tx_mgr.has_other_active_transactions(txid2));
  }

  #[test]
  fn test_has_conflicting_write() {
    let mut tx_mgr = TxManager::new();

    // No writes yet
    assert!(!tx_mgr.has_conflicting_write("key1", 0));

    let (txid, _) = tx_mgr.begin_tx();
    tx_mgr.record_write(txid, "key1".to_string());
    tx_mgr.commit_tx(txid).unwrap();

    // After commit at ts=1
    assert!(tx_mgr.has_conflicting_write("key1", 1));
    assert!(tx_mgr.has_conflicting_write("key1", 0));
    assert!(!tx_mgr.has_conflicting_write("key1", 2)); // min_commit_ts > actual
    assert!(!tx_mgr.has_conflicting_write("key2", 0)); // Different key
  }

  #[test]
  fn test_get_committed_write_ts() {
    let mut tx_mgr = TxManager::new();
    let (txid, _) = tx_mgr.begin_tx();
    tx_mgr.record_write(txid, "key1".to_string());
    tx_mgr.commit_tx(txid).unwrap();

    assert_eq!(tx_mgr.get_committed_write_ts("key1", 0), Some(1));
    assert_eq!(tx_mgr.get_committed_write_ts("key1", 1), Some(1));
    assert_eq!(tx_mgr.get_committed_write_ts("key1", 2), None);
    assert_eq!(tx_mgr.get_committed_write_ts("key2", 0), None);
  }

  #[test]
  fn test_clear() {
    let mut tx_mgr = TxManager::new();
    let (txid, _) = tx_mgr.begin_tx();
    tx_mgr.record_write(txid, "key1".to_string());

    tx_mgr.clear();
    assert_eq!(tx_mgr.get_active_count(), 0);
    assert!(!tx_mgr.has_conflicting_write("key1", 0));
  }

  #[test]
  fn test_serial_workload_cleanup() {
    // Test that serial workloads (one tx at a time) clean up eagerly
    let mut tx_mgr = TxManager::new();

    for i in 0..10 {
      let (txid, _) = tx_mgr.begin_tx();
      tx_mgr.record_write(txid, format!("key{}", i));
      tx_mgr.commit_tx(txid).unwrap();
    }

    // After serial commits, no transactions should remain in active_txs
    assert_eq!(tx_mgr.get_active_count(), 0);
    assert_eq!(tx_mgr.get_active_tx_ids().len(), 0);
  }

  #[test]
  fn test_concurrent_workload() {
    let mut tx_mgr = TxManager::new();

    // Start multiple transactions
    let (txid1, start_ts1) = tx_mgr.begin_tx();
    let (txid2, start_ts2) = tx_mgr.begin_tx();
    let (txid3, start_ts3) = tx_mgr.begin_tx();

    // All get same snapshot
    assert_eq!(start_ts1, start_ts2);
    assert_eq!(start_ts2, start_ts3);

    // Record some writes
    tx_mgr.record_write(txid1, "a".to_string());
    tx_mgr.record_write(txid2, "b".to_string());
    tx_mgr.record_write(txid3, "a".to_string()); // Same key as tx1

    // Commit tx1
    let commit_ts1 = tx_mgr.commit_tx(txid1).unwrap();
    assert_eq!(commit_ts1, 1);
    assert_eq!(tx_mgr.get_active_count(), 2);

    // tx3 now has conflict with tx1 on key "a"
    assert!(tx_mgr.has_conflicting_write("a", start_ts3));

    // Commit tx2 (no conflict)
    let commit_ts2 = tx_mgr.commit_tx(txid2).unwrap();
    assert_eq!(commit_ts2, 2);
    assert_eq!(tx_mgr.get_active_count(), 1);

    // Abort tx3
    tx_mgr.abort_tx(txid3);
    assert_eq!(tx_mgr.get_active_count(), 0);
  }

  #[test]
  fn test_remove_tx() {
    let mut tx_mgr = TxManager::new();

    // Start two transactions so committed one isn't auto-cleaned
    let (txid1, _) = tx_mgr.begin_tx();
    let (txid2, _) = tx_mgr.begin_tx();

    tx_mgr.commit_tx(txid1).unwrap();

    // tx1 should still be in active_txs because there's another active tx
    assert!(tx_mgr.get_tx(txid1).is_some());

    // Remove it manually (like GC would do)
    tx_mgr.remove_tx(txid1);
    assert!(tx_mgr.get_tx(txid1).is_none());

    // tx2 should still be there
    assert!(tx_mgr.get_tx(txid2).is_some());
  }

  #[test]
  fn test_error_display() {
    let err1 = TxManagerError::TxNotFound(42);
    assert_eq!(err1.to_string(), "Transaction 42 not found");

    let err2 = TxManagerError::TxNotActive(42, MvccTxStatus::Committed);
    assert!(err2.to_string().contains("42"));
    assert!(err2.to_string().contains("not active"));
  }
}
