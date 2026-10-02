//! MVCC Transaction Manager
//!
//! Manages transaction lifecycle, timestamps, and active transaction tracking.
//!
//! Ported from src/mvcc/tx-manager.ts

use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

use hashbrown::HashMap;

use crate::types::{MvccTransaction, Timestamp, TxId, TxKey, TxKeySet};

/// Maximum number of committed write entries before pruning
pub(crate) const MAX_COMMITTED_WRITES: usize = 100_000;
/// Prune down to this many entries when over the limit
const PRUNE_THRESHOLD_ENTRIES: usize = 50_000;
/// Stale entries the commit-order log may hold beyond twice the live ones
/// before it is compacted.
const COMMIT_LOG_SLACK: usize = 1024;

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
///
/// Only active transactions are tracked: commit and abort drop the record. What
/// conflict checks need from a committed transaction lives on in
/// `committed_writes`.
#[derive(Debug)]
pub struct TxManager {
  /// Active transactions
  active_txs: HashMap<TxId, MvccTransaction>,
  /// Next transaction ID to assign
  next_tx_id: TxId,
  /// Next commit timestamp to assign
  next_commit_ts: Timestamp,
  /// Inverted index: key -> max commitTs for conflict detection
  committed_writes: HashMap<TxKey, Timestamp>,
  /// `committed_writes` updates in commit order, so pruning pops the oldest
  /// first. An entry is stale once its key has a newer commit.
  committed_writes_log: VecDeque<(Timestamp, TxKey)>,
  /// The newest commit whose writes are in `committed_writes` (0: none). A
  /// transaction that began after it cannot conflict with any.
  newest_indexed_ts: Timestamp,
  /// The first commit timestamp of each wall clock millisecond (since the
  /// epoch) that had a commit, oldest first, for the retention horizon. The
  /// times never decrease: a commit while the clock stepped back counts as
  /// the newest entry's millisecond.
  commit_wall_clock: VecDeque<(Timestamp, u64)>,
  /// Total committed write entries pruned (for stats)
  total_pruned: usize,
  /// Commit-log entries visited while pruning or compacting (test instrumentation)
  #[cfg(test)]
  pub(crate) prune_work: u64,
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
      committed_writes_log: VecDeque::new(),
      newest_indexed_ts: 0,
      commit_wall_clock: VecDeque::new(),
      total_pruned: 0,
      #[cfg(test)]
      prune_work: 0,
    }
  }

  /// Get the minimum active timestamp (oldest active transaction snapshot)
  /// Used for GC horizon calculation
  pub fn min_active_ts(&self) -> Timestamp {
    self
      .active_txs
      .values()
      .map(|tx| tx.start_ts)
      .fold(self.next_commit_ts, Timestamp::min)
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
      read_set: TxKeySet::new(),
      write_set: TxKeySet::new(),
    };

    self.active_txs.insert(txid, tx);
    (txid, start_ts)
  }

  /// Get an active transaction by ID
  pub fn tx(&self, txid: TxId) -> Option<&MvccTransaction> {
    self.active_txs.get(&txid)
  }

  /// Get a mutable active transaction by ID
  pub fn tx_mut(&mut self, txid: TxId) -> Option<&mut MvccTransaction> {
    self.active_txs.get_mut(&txid)
  }

  /// Check if transaction is active
  pub fn is_active(&self, txid: TxId) -> bool {
    self.active_txs.contains_key(&txid)
  }

  /// Record a read operation
  pub fn record_read(&mut self, txid: TxId, key: TxKey) {
    if let Some(tx) = self.active_txs.get_mut(&txid) {
      tx.read_set.insert(key);
    }
  }

  /// Record a write operation
  pub fn record_write(&mut self, txid: TxId, key: TxKey) {
    if let Some(tx) = self.active_txs.get_mut(&txid) {
      tx.write_set.insert(key);
    }
  }

  /// Record reads made earlier (the database buffers a write transaction's
  /// reads and hands them over at commit, before the conflict check)
  pub fn record_reads(&mut self, txid: TxId, keys: impl IntoIterator<Item = TxKey>) {
    if let Some(tx) = self.active_txs.get_mut(&txid) {
      tx.read_set.extend(keys);
    }
  }

  /// Commit a transaction and drop its record
  /// Returns commit timestamp
  pub fn commit_tx(&mut self, txid: TxId) -> Result<Timestamp, TxManagerError> {
    let tx = self
      .active_txs
      .remove(&txid)
      .ok_or(TxManagerError::TxNotFound(txid))?;

    let commit_ts = self.next_commit_ts;
    self.next_commit_ts += 1;

    // Track wall clock time for the retention horizon: the first commit of
    // each millisecond stands for the rest.
    let now = current_time_ms();
    if self
      .commit_wall_clock
      .back()
      .is_none_or(|&(_, last)| now > last)
    {
      self.commit_wall_clock.push_back((commit_ts, now));
    }

    // Index writes for fast conflict detection, storing only the max commitTs
    // per key. Only a transaction that began before a commit can conflict
    // with it: with none open, nothing indexed can conflict any more.
    if self.active_txs.is_empty() {
      self.total_pruned += self.committed_writes.len();
      self.committed_writes.clear();
      self.committed_writes_log.clear();
      return Ok(commit_ts);
    }
    if !tx.write_set.is_empty() {
      self.newest_indexed_ts = commit_ts;
    }
    for key in tx.write_set {
      let newer = self
        .committed_writes
        .get(&key)
        .is_none_or(|&existing_ts| commit_ts > existing_ts);
      if newer {
        self.committed_writes.insert(key.clone(), commit_ts);
        self.committed_writes_log.push_back((commit_ts, key));
      }
    }

    if self.committed_writes.len() > MAX_COMMITTED_WRITES {
      self.prune_committed_writes();
    }
    if self.committed_writes_log.len() > 2 * self.committed_writes.len() + COMMIT_LOG_SLACK {
      self.compact_committed_writes_log();
    }

    Ok(commit_ts)
  }

  /// Abort a transaction and drop its record
  pub fn abort_tx(&mut self, txid: TxId) {
    self.active_txs.remove(&txid);
  }

  /// Drop a transaction's record without committing it. Commit and abort
  /// already drop theirs.
  pub fn remove_tx(&mut self, txid: TxId) {
    self.active_txs.remove(&txid);
  }

  /// Get all active transaction IDs
  pub fn active_tx_ids(&self) -> Vec<TxId> {
    self.active_txs.keys().copied().collect()
  }

  /// Number of active transactions
  pub fn active_count(&self) -> usize {
    self.active_txs.len()
  }

  /// Check if there are other active transactions besides the given one
  /// Fast path for determining if version chains are needed
  pub fn has_other_active_transactions(&self, _exclude_txid: TxId) -> bool {
    self.active_txs.len() > 1
  }

  /// Get the next commit timestamp (for snapshot reads outside transactions)
  pub fn next_commit_ts(&self) -> Timestamp {
    self.next_commit_ts
  }

  /// Get all active transactions (for debugging/recovery)
  pub fn all_txs(&self) -> impl Iterator<Item = (&TxId, &MvccTransaction)> {
    self.active_txs.iter()
  }

  /// Get committed writes for a key (for conflict detection)
  /// Returns the max commitTs for the key if >= minCommitTs, otherwise None
  pub fn committed_write_ts(&self, key: &TxKey, min_commit_ts: Timestamp) -> Option<Timestamp> {
    self.committed_writes.get(key).and_then(|&max_ts| {
      if max_ts >= min_commit_ts {
        Some(max_ts)
      } else {
        None
      }
    })
  }

  /// Whether a transaction that began at `start_ts` can conflict with any
  /// indexed write (`has_conflicting_write` is false for every key otherwise)
  pub fn has_writes_since(&self, start_ts: Timestamp) -> bool {
    self.newest_indexed_ts >= start_ts
  }

  /// Check if there's a conflicting write for a key (fast path for conflict detection)
  /// Returns true if any transaction wrote this key with commitTs >= minCommitTs
  pub fn has_conflicting_write(&self, key: &TxKey, min_commit_ts: Timestamp) -> bool {
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
    self.committed_writes_log.clear();
    self.newest_indexed_ts = 0;
    self.commit_wall_clock.clear();
    self.total_pruned = 0;
  }

  /// Get the next transaction ID (useful for recovery)
  pub fn next_tx_id(&self) -> TxId {
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

  /// Get the oldest commit timestamp younger than the retention period (the
  /// next commit timestamp when none is)
  pub fn retention_horizon_ts(&self, retention_ms: u64) -> Timestamp {
    let cutoff_time = current_time_ms().saturating_sub(retention_ms);
    let first_retained = self
      .commit_wall_clock
      .partition_point(|&(_, wall_clock)| wall_clock <= cutoff_time);
    self
      .commit_wall_clock
      .get(first_retained)
      .map_or(self.next_commit_ts, |&(commit_ts, _)| commit_ts)
  }

  /// Prune old wall clock mappings older than the given horizon
  pub fn prune_wall_clock_mappings(&mut self, horizon_ts: Timestamp) {
    // An entry stands for the commits up to the next entry's: drop it once
    // all of them are older than the horizon.
    while self
      .commit_wall_clock
      .get(1)
      .is_some_and(|&(next_ts, _)| next_ts <= horizon_ts)
    {
      self.commit_wall_clock.pop_front();
    }
  }

  /// Get statistics about committed writes
  pub fn committed_writes_stats(&self) -> CommittedWritesStats {
    CommittedWritesStats {
      size: self.committed_writes.len(),
      pruned: self.total_pruned,
    }
  }

  /// Drop the oldest committed-write entries, down to the prune target. Only
  /// entries older than every active snapshot (`commit_ts < min_active_ts`) can
  /// go: no active or future transaction can conflict with them. Stops at the
  /// first entry a snapshot still needs, so with a long-lived reader it costs
  /// one look at the oldest entry.
  fn prune_committed_writes(&mut self) {
    let min_ts = self.min_active_ts();
    let target_size = MAX_COMMITTED_WRITES.saturating_sub(PRUNE_THRESHOLD_ENTRIES);
    let mut pruned = 0;

    while self.committed_writes.len() > target_size {
      #[cfg(test)]
      {
        self.prune_work += 1;
      }
      match self.committed_writes_log.front() {
        Some((commit_ts, _)) if *commit_ts < min_ts => {}
        _ => break,
      }
      let Some((commit_ts, key)) = self.committed_writes_log.pop_front() else {
        break;
      };
      // A stale entry's key was rewritten later; that entry still holds it.
      if self.committed_writes.get(&key) == Some(&commit_ts) {
        self.committed_writes.remove(&key);
        pruned += 1;
      }
    }

    self.total_pruned += pruned;
  }

  /// Drop stale log entries (keys a later commit rewrote). Runs once stale
  /// entries outnumber live ones, so its cost is amortized over the commits
  /// that made them.
  fn compact_committed_writes_log(&mut self) {
    #[cfg(test)]
    {
      self.prune_work += self.committed_writes_log.len() as u64;
    }
    let committed_writes = &self.committed_writes;
    self
      .committed_writes_log
      .retain(|(commit_ts, key)| committed_writes.get(key) == Some(commit_ts));
  }

  /// Wall clock entries kept for the retention horizon (test instrumentation)
  #[cfg(test)]
  pub(crate) fn wall_clock_len(&self) -> usize {
    self.commit_wall_clock.len()
  }

  /// Length of the commit-order log behind pruning (test instrumentation)
  #[cfg(test)]
  pub(crate) fn committed_writes_log_len(&self) -> usize {
    self.committed_writes_log.len()
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
  /// Transaction not found (never begun, or already committed or aborted)
  TxNotFound(TxId),
}

impl std::fmt::Display for TxManagerError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      TxManagerError::TxNotFound(txid) => write!(f, "Transaction {txid} not found"),
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

  fn key(name: &str) -> TxKey {
    TxKey::Key(std::sync::Arc::from(name))
  }

  #[test]
  fn test_new() {
    let tx_mgr = TxManager::new();
    assert_eq!(tx_mgr.active_count(), 0);
    assert_eq!(tx_mgr.next_tx_id(), 1);
    assert_eq!(tx_mgr.next_commit_ts(), 1);
  }

  #[test]
  fn test_with_initial() {
    let tx_mgr = TxManager::with_initial(100, 200);
    assert_eq!(tx_mgr.next_tx_id(), 100);
    assert_eq!(tx_mgr.next_commit_ts(), 200);
  }

  #[test]
  fn test_begin_tx() {
    let mut tx_mgr = TxManager::new();

    let (txid1, start_ts1) = tx_mgr.begin_tx();
    assert_eq!(txid1, 1);
    assert_eq!(start_ts1, 1);
    assert_eq!(tx_mgr.active_count(), 1);

    let (txid2, start_ts2) = tx_mgr.begin_tx();
    assert_eq!(txid2, 2);
    assert_eq!(start_ts2, 1); // Same snapshot
    assert_eq!(tx_mgr.active_count(), 2);
  }

  #[test]
  fn test_tx() {
    let mut tx_mgr = TxManager::new();
    let (txid, _) = tx_mgr.begin_tx();

    let tx = tx_mgr.tx(txid);
    assert!(tx.is_some());
    assert_eq!(tx.expect("expected value").txid, txid);

    assert!(tx_mgr.tx(999).is_none());
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

    tx_mgr.record_read(txid, key("key1"));
    tx_mgr.record_write(txid, key("key2"));

    let tx = tx_mgr.tx(txid).expect("expected value");
    assert!(tx.read_set.contains(&key("key1")));
    assert!(tx.write_set.contains(&key("key2")));
  }

  #[test]
  fn test_commit_tx() {
    let mut tx_mgr = TxManager::new();
    let (other, _) = tx_mgr.begin_tx();
    let (txid, _) = tx_mgr.begin_tx();

    tx_mgr.record_write(txid, key("key1"));

    let commit_ts = tx_mgr.commit_tx(txid).expect("expected value");
    assert_eq!(commit_ts, 1);
    assert_eq!(tx_mgr.active_count(), 1);

    // Indexed for the transaction still open, which began before it
    assert!(tx_mgr.has_conflicting_write(&key("key1"), 1));

    // With nothing open, nothing indexed can conflict any more
    tx_mgr.abort_tx(other);
    let (last, _) = tx_mgr.begin_tx();
    tx_mgr.commit_tx(last).expect("commit last");
    assert!(!tx_mgr.has_conflicting_write(&key("key1"), 0));
    assert_eq!(tx_mgr.committed_writes_stats().size, 0);
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
    tx_mgr.commit_tx(txid2).expect("expected value");

    // Try to commit already aborted (which was removed)
    let result = tx_mgr.commit_tx(txid);
    assert!(matches!(result, Err(TxManagerError::TxNotFound(_))));
  }

  #[test]
  fn test_abort_tx() {
    let mut tx_mgr = TxManager::new();
    let (txid, _) = tx_mgr.begin_tx();
    assert_eq!(tx_mgr.active_count(), 1);

    tx_mgr.abort_tx(txid);
    assert_eq!(tx_mgr.active_count(), 0);
    assert!(tx_mgr.tx(txid).is_none()); // Removed immediately
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
    tx_mgr.commit_tx(txid1).expect("expected value");

    // Start tx2 after commit
    let (_txid2, _) = tx_mgr.begin_tx();
    assert_eq!(tx_mgr.min_active_ts(), 2); // tx2's snapshot
  }

  #[test]
  fn test_active_tx_ids() {
    let mut tx_mgr = TxManager::new();

    let (txid1, _) = tx_mgr.begin_tx();
    let (txid2, _) = tx_mgr.begin_tx();

    let active_ids = tx_mgr.active_tx_ids();
    assert_eq!(active_ids.len(), 2);
    assert!(active_ids.contains(&txid1));
    assert!(active_ids.contains(&txid2));

    tx_mgr.commit_tx(txid1).expect("expected value");
    let active_ids = tx_mgr.active_tx_ids();
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
    assert!(!tx_mgr.has_conflicting_write(&key("key1"), 0));

    let (_open, _) = tx_mgr.begin_tx();
    let (txid, _) = tx_mgr.begin_tx();
    tx_mgr.record_write(txid, key("key1"));
    tx_mgr.commit_tx(txid).expect("expected value");

    // After commit at ts=1
    assert!(tx_mgr.has_conflicting_write(&key("key1"), 1));
    assert!(tx_mgr.has_conflicting_write(&key("key1"), 0));
    assert!(!tx_mgr.has_conflicting_write(&key("key1"), 2)); // min_commit_ts > actual
    assert!(!tx_mgr.has_conflicting_write(&key("key2"), 0)); // Different key
  }

  #[test]
  fn test_committed_write_ts() {
    let mut tx_mgr = TxManager::new();
    let (_open, _) = tx_mgr.begin_tx();
    let (txid, _) = tx_mgr.begin_tx();
    tx_mgr.record_write(txid, key("key1"));
    tx_mgr.commit_tx(txid).expect("expected value");

    assert_eq!(tx_mgr.committed_write_ts(&key("key1"), 0), Some(1));
    assert_eq!(tx_mgr.committed_write_ts(&key("key1"), 1), Some(1));
    assert_eq!(tx_mgr.committed_write_ts(&key("key1"), 2), None);
    assert_eq!(tx_mgr.committed_write_ts(&key("key2"), 0), None);
  }

  #[test]
  fn test_clear() {
    let mut tx_mgr = TxManager::new();
    let (txid, _) = tx_mgr.begin_tx();
    tx_mgr.record_write(txid, key("key1"));

    tx_mgr.clear();
    assert_eq!(tx_mgr.active_count(), 0);
    assert!(!tx_mgr.has_conflicting_write(&key("key1"), 0));
  }

  #[test]
  fn test_serial_workload_cleanup() {
    // Test that serial workloads (one tx at a time) clean up eagerly
    let mut tx_mgr = TxManager::new();

    for i in 0..10 {
      let (txid, _) = tx_mgr.begin_tx();
      tx_mgr.record_write(txid, TxKey::Key(format!("key{i}").into()));
      tx_mgr.commit_tx(txid).expect("expected value");
    }

    // After serial commits, no transactions should remain in active_txs
    assert_eq!(tx_mgr.active_count(), 0);
    assert_eq!(tx_mgr.active_tx_ids().len(), 0);
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
    tx_mgr.record_write(txid1, key("a"));
    tx_mgr.record_write(txid2, key("b"));
    tx_mgr.record_write(txid3, key("a")); // Same key as tx1

    // Commit tx1
    let commit_ts1 = tx_mgr.commit_tx(txid1).expect("expected value");
    assert_eq!(commit_ts1, 1);
    assert_eq!(tx_mgr.active_count(), 2);

    // tx3 now has conflict with tx1 on key "a"
    assert!(tx_mgr.has_conflicting_write(&key("a"), start_ts3));

    // Commit tx2 (no conflict)
    let commit_ts2 = tx_mgr.commit_tx(txid2).expect("expected value");
    assert_eq!(commit_ts2, 2);
    assert_eq!(tx_mgr.active_count(), 1);

    // Abort tx3
    tx_mgr.abort_tx(txid3);
    assert_eq!(tx_mgr.active_count(), 0);
  }

  #[test]
  fn test_remove_tx() {
    let mut tx_mgr = TxManager::new();

    let (txid1, _) = tx_mgr.begin_tx();
    let (txid2, _) = tx_mgr.begin_tx();

    // Commit drops the record even while another transaction is active
    tx_mgr.commit_tx(txid1).expect("expected value");
    assert!(tx_mgr.tx(txid1).is_none());
    assert_eq!(
      tx_mgr.commit_tx(txid1),
      Err(TxManagerError::TxNotFound(txid1))
    );

    tx_mgr.remove_tx(txid2);
    assert!(tx_mgr.tx(txid2).is_none());
    assert_eq!(tx_mgr.active_count(), 0);
  }

  #[test]
  fn test_error_display() {
    let err1 = TxManagerError::TxNotFound(42);
    assert_eq!(err1.to_string(), "Transaction 42 not found");
  }
}
