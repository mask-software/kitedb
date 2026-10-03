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
/// Recent commits, and the keys they wrote, kept whole before the oldest are
/// folded into `committed_writes` (only commits an open transaction still
/// pins stay that long).
pub(crate) const RECENT_COMMITS_MAX: usize = 1024;
pub(crate) const RECENT_KEYS_MAX: usize = 256 * 1024;
/// `group_writes` entries before the first prune.
const GROUP_WRITES_PRUNE_MIN: usize = 64 * 1024;

/// When `TxManager` folds, prunes and compacts what it keeps for conflict
/// checks (the constants above; tests lower them).
#[derive(Debug, Clone, Copy)]
struct KeepLimits {
  recent_commits: usize,
  recent_keys: usize,
  group_writes_min: usize,
  committed_writes: usize,
}

impl Default for KeepLimits {
  fn default() -> Self {
    Self {
      recent_commits: RECENT_COMMITS_MAX,
      recent_keys: RECENT_KEYS_MAX,
      group_writes_min: GROUP_WRITES_PRUNE_MIN,
      committed_writes: MAX_COMMITTED_WRITES,
    }
  }
}
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
/// `recent_commits`, then `committed_writes`.
///
/// Commits run one at a time (the database commits under one lock), so a
/// commit moves its write set in whole, with no per-key indexing: a large
/// transaction would otherwise hold every concurrent commit up while it
/// indexed each key. Conflict checks find the keys through `group_writes`.
///
/// A commit group checks its members in order before any of them is durable:
/// each that passes is staged (`stage_commit`), so the members after it
/// conflict with its writes as if it had committed, and commits
/// (`commit_tx`) in staging order once the group is durable, or is unstaged
/// with the rest of its group (`unstage_commits`) if the group fails.
#[derive(Debug)]
pub struct TxManager {
  /// Active transactions
  active_txs: HashMap<TxId, MvccTransaction>,
  /// Next transaction ID to assign
  next_tx_id: TxId,
  /// Next commit timestamp to assign
  next_commit_ts: Timestamp,
  /// The write sets of recent commits an open transaction may conflict
  /// with, oldest first, as they committed. Each commit's timestamp is newer
  /// than every one in `committed_writes`.
  recent_commits: VecDeque<CommitWrites>,
  /// Keys in `recent_commits`.
  recent_keys: usize,
  /// For each key group (`key_group`), the newest commit, in
  /// `recent_commits` or `committed_writes`, that wrote a key of it: a
  /// conflict check skips the exact lookup of a key whose group no commit
  /// since its snapshot wrote.
  group_writes: HashMap<u64, Timestamp>,
  /// Pruning drops `group_writes` entries older than this, so checks for
  /// older snapshots skip the filter.
  group_floor: Timestamp,
  /// `group_writes` is pruned once it holds more entries than this.
  group_writes_prune_at: usize,
  limits: KeepLimits,
  /// The key groups of open transactions that handed their reads and writes
  /// over with them (`record_reads_and_writes`) and recorded nothing since.
  tx_key_groups: HashMap<TxId, TxKeyGroups>,
  /// Older commits' writes, folded out of `recent_commits` while an open
  /// transaction still pinned them: key -> newest commit timestamp.
  committed_writes: HashMap<TxKey, Timestamp>,
  /// `committed_writes` updates in commit order, so pruning pops the oldest
  /// first. An entry is stale once its key has a newer commit.
  committed_writes_log: VecDeque<(Timestamp, TxKey)>,
  /// The newest commit whose writes are kept (0: none). A transaction that
  /// began after it cannot conflict with any.
  newest_indexed_ts: Timestamp,
  /// The first commit timestamp of each wall clock millisecond (since the
  /// epoch) that had a commit, oldest first, for the retention horizon. The
  /// times never decrease: a commit while the clock stepped back counts as
  /// the newest entry's millisecond. Kept only while `track_wall_clock`.
  commit_wall_clock: VecDeque<(Timestamp, u64)>,
  /// Commits note their time in `commit_wall_clock`. Off when the retention
  /// period is 0 (`MvccManager::new`), whose horizon needs no times: a
  /// commit then reads no clock.
  track_wall_clock: bool,
  /// Some recent commit is `unindexed`.
  has_unindexed: bool,
  /// Staged transactions (`stage_commit`), in staging order, with the
  /// commit timestamps `commit_tx` gives them: consecutive from
  /// `next_commit_ts`. They are still active, and their writes already in
  /// `recent_commits`.
  staged: VecDeque<(TxId, Timestamp)>,
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
      recent_commits: VecDeque::new(),
      recent_keys: 0,
      group_writes: HashMap::new(),
      group_floor: 0,
      group_writes_prune_at: GROUP_WRITES_PRUNE_MIN,
      limits: KeepLimits::default(),
      tx_key_groups: HashMap::new(),
      committed_writes: HashMap::new(),
      committed_writes_log: VecDeque::new(),
      newest_indexed_ts: 0,
      commit_wall_clock: VecDeque::new(),
      track_wall_clock: true,
      has_unindexed: false,
      staged: VecDeque::new(),
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
    // It may check against the staged writes once they commit.
    self.index_staged_writes();
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
      self.tx_key_groups.remove(&txid);
    }
  }

  /// Record a write operation
  pub fn record_write(&mut self, txid: TxId, key: TxKey) {
    if let Some(tx) = self.active_txs.get_mut(&txid) {
      tx.write_set.insert(key);
      self.tx_key_groups.remove(&txid);
    }
  }

  /// Record reads made earlier (the database buffers a write transaction's
  /// reads and hands them over at commit, before the conflict check)
  pub fn record_reads(&mut self, txid: TxId, keys: impl IntoIterator<Item = TxKey>) {
    if let Some(tx) = self.active_txs.get_mut(&txid) {
      tx.read_set.extend(keys);
      self.tx_key_groups.remove(&txid);
    }
  }

  /// Record reads and writes made earlier (the database keeps a write
  /// transaction's sets with it and hands them over at commit, before the
  /// conflict check). A set moves in whole when the transaction has none yet.
  /// `groups`, if given, are their key groups (`TxKeyGroups::of`), which the
  /// conflict check and the commit then use instead of every key; they are
  /// kept only while they cover everything the transaction recorded.
  pub fn record_reads_and_writes(
    &mut self,
    txid: TxId,
    reads: TxKeySet,
    writes: TxKeySet,
    groups: Option<TxKeyGroups>,
  ) {
    let Some(tx) = self.active_txs.get_mut(&txid) else {
      return;
    };
    let complete = tx.read_set.is_empty() && tx.write_set.is_empty();
    absorb(&mut tx.read_set, reads);
    absorb(&mut tx.write_set, writes);
    match groups.filter(|_| complete) {
      Some(groups) => {
        self.tx_key_groups.insert(txid, groups);
      }
      None => {
        self.tx_key_groups.remove(&txid);
      }
    }
  }

  /// The key groups of transaction `txid`'s reads and writes that a commit
  /// since `start_ts` wrote: only its keys in them can conflict. `None` if
  /// that is not known (its groups were not handed over, or `start_ts` is
  /// older than what `group_writes` keeps), and every key must be checked.
  pub fn groups_written_since(
    &self,
    txid: TxId,
    start_ts: Timestamp,
  ) -> Option<hashbrown::HashSet<u64>> {
    if start_ts < self.group_floor {
      return None;
    }
    let groups = self.tx_key_groups.get(&txid)?;
    Some(
      groups
        .reads_and_writes()
        .iter()
        .copied()
        .filter(|group| {
          self
            .group_writes
            .get(group)
            .is_some_and(|&commit_ts| commit_ts >= start_ts)
        })
        .collect(),
    )
  }

  /// Stage `txid` for commit, after its conflict check passed: from now on
  /// conflict checks treat its writes as committed at the timestamp this
  /// returns (the next one not yet given or staged), which `commit_tx` gives
  /// it. Transactions are committed in the order they are staged.
  pub fn stage_commit(&mut self, txid: TxId) -> Result<Timestamp, TxManagerError> {
    let commit_ts = self.next_commit_ts + self.staged.len() as Timestamp;
    let tx = self
      .active_txs
      .get_mut(&txid)
      .ok_or(TxManagerError::TxNotFound(txid))?;
    if self.staged.iter().any(|&(staged, _)| staged == txid) {
      return Err(TxManagerError::InvalidState(format!(
        "transaction {txid} is staged already"
      )));
    }
    // An empty write set (with room for keys) stays with the transaction,
    // and goes back with its read set when it commits.
    let writes = if tx.write_set.is_empty() {
      TxKeySet::new()
    } else {
      std::mem::take(&mut tx.write_set)
    };
    let key_groups = self.tx_key_groups.remove(&txid);
    self.staged.push_back((txid, commit_ts));
    // With no transaction open but the staged ones (and this), none checks
    // against these writes unless one begins before they commit (`begin_tx`
    // notes them then); if none does, the commit drops them unnoted. A
    // writer alone thus never indexes its keys.
    if self.active_txs.len() == self.staged.len() && !writes.is_empty() {
      self.recent_keys += writes.len();
      self.recent_commits.push_back(CommitWrites {
        commit_ts,
        keys: writes,
        unindexed: Some(key_groups),
      });
      self.has_unindexed = true;
    } else {
      self.index_recent_commit(commit_ts, writes, key_groups);
    }
    Ok(commit_ts)
  }

  /// Unstage every staged transaction, newest first (see `unstage_last`).
  pub fn unstage_commits(&mut self) {
    while !self.staged.is_empty() {
      self.unstage_last();
    }
  }

  /// Unstage the transaction staged last: its commit group failed. It gets
  /// its writes back from the recent commits, so they cause no conflict,
  /// and is active as before it was staged; the ones staged before it stay
  /// staged. `newest_indexed_ts` and a few `group_writes` entries may keep
  /// naming its timestamp, which only costs later checks an exact lookup.
  pub fn unstage_last(&mut self) {
    let Some((txid, commit_ts)) = self.staged.pop_back() else {
      return;
    };
    if self
      .recent_commits
      .back()
      .is_some_and(|commit| commit.commit_ts == commit_ts)
    {
      if let Some(commit) = self.recent_commits.pop_back() {
        self.recent_keys -= commit.keys.len();
        if let Some(tx) = self.active_txs.get_mut(&txid) {
          tx.write_set = commit.keys;
        }
      }
    }
  }

  /// The commit timestamp of the transaction staged last, if any.
  pub fn newest_staged_ts(&self) -> Option<Timestamp> {
    self.staged.back().map(|&(_, commit_ts)| commit_ts)
  }

  /// Whether a transaction other than the staged ones is active: a reader
  /// that may still read the state a commit replaces. Staged transactions
  /// read nothing more; they only wait to commit.
  pub fn has_open_readers(&self) -> bool {
    self.active_txs.len() > self.staged.len()
  }

  /// Commit a transaction and drop its record
  /// Returns commit timestamp
  pub fn commit_tx(&mut self, txid: TxId) -> Result<Timestamp, TxManagerError> {
    self.commit_tx_releasing(txid, &mut Vec::new())
  }

  /// `commit_tx`, handing the key sets the commit no longer needs to
  /// `released`, for the caller to free once it releases its locks (a large
  /// transaction's take a while).
  pub fn commit_tx_releasing(
    &mut self,
    txid: TxId,
    released: &mut Vec<TxKeySet>,
  ) -> Result<Timestamp, TxManagerError> {
    let staged = match self.staged.front() {
      Some(&(first, commit_ts)) if first == txid => {
        debug_assert_eq!(
          commit_ts, self.next_commit_ts,
          "staged timestamps are consecutive"
        );
        self.staged.pop_front();
        true
      }
      _ if self.staged.iter().any(|&(staged, _)| staged == txid) => {
        return Err(TxManagerError::InvalidState(format!(
          "transaction {txid} commits before the transactions staged ahead of it"
        )));
      }
      _ => false,
    };
    let tx = self
      .active_txs
      .remove(&txid)
      .ok_or(TxManagerError::TxNotFound(txid))?;
    let key_groups = self.tx_key_groups.remove(&txid);

    let commit_ts = self.next_commit_ts;
    self.next_commit_ts += 1;

    // Track wall clock time for the retention horizon: the first commit of
    // each millisecond stands for the rest.
    if self.track_wall_clock {
      let now = current_time_ms();
      if self
        .commit_wall_clock
        .back()
        .is_none_or(|&(_, last)| now > last)
      {
        self.commit_wall_clock.push_back((commit_ts, now));
      }
    }

    // Only a transaction that began before a commit can conflict with it:
    // with none open, nothing kept can conflict any more. Its read set goes
    // back, and so does a staged one's write set if it was empty (sets with
    // room for keys can be reused).
    if tx.read_set.capacity() > 0 {
      released.push(tx.read_set);
    }
    let mut writes = tx.write_set;
    if staged && writes.capacity() > 0 {
      released.push(std::mem::take(&mut writes));
    }
    if self.active_txs.is_empty() {
      // Staged transactions are active, so none is left.
      self.total_pruned += self.committed_writes.len() + self.recent_keys;
      released.extend(self.recent_commits.drain(..).map(|commit| commit.keys));
      self.has_unindexed = false;
      self.recent_keys = 0;
      self.group_writes.clear();
      self.committed_writes.clear();
      self.committed_writes_log.clear();
      return Ok(commit_ts);
    }
    if !staged {
      self.index_recent_commit(commit_ts, writes, key_groups);
    }
    self.prune_recent_commits(released);
    Ok(commit_ts)
  }

  /// Keep `writes`, a commit's at `commit_ts` (newer than every kept one),
  /// for the conflict checks of the transactions open beside it.
  fn index_recent_commit(
    &mut self,
    commit_ts: Timestamp,
    writes: TxKeySet,
    key_groups: Option<TxKeyGroups>,
  ) {
    if writes.is_empty() {
      return;
    }
    note_group_writes(&mut self.group_writes, commit_ts, &writes, key_groups);
    self.newest_indexed_ts = commit_ts;
    self.recent_keys += writes.len();
    self.recent_commits.push_back(CommitWrites {
      commit_ts,
      keys: writes,
      unindexed: None,
    });
  }

  /// Note in `group_writes` the staged writes `stage_commit` left unnoted,
  /// before a transaction that may check against them begins. Those of
  /// transactions committed since need no note: a transaction that begins
  /// now cannot conflict with them.
  fn index_staged_writes(&mut self) {
    if !self.has_unindexed {
      return;
    }
    self.has_unindexed = false;
    let next_commit_ts = self.next_commit_ts;
    for commit in self.recent_commits.iter_mut().rev() {
      if commit.commit_ts < next_commit_ts {
        break;
      }
      let Some(key_groups) = commit.unindexed.take() else {
        continue;
      };
      note_group_writes(
        &mut self.group_writes,
        commit.commit_ts,
        &commit.keys,
        key_groups,
      );
      self.newest_indexed_ts = self.newest_indexed_ts.max(commit.commit_ts);
    }
  }

  /// Drop the recent commits no open transaction can conflict with (older
  /// than every snapshot), handing their key sets to `released`, then fold
  /// the oldest of the rest into `committed_writes` once they are too many:
  /// an open transaction has pinned them for long, and folding keeps one
  /// entry per key however often they rewrite it. Prunes `committed_writes`
  /// as before, and `group_writes` once it outgrows twice what the last prune
  /// left (amortized constant work per commit).
  fn prune_recent_commits(&mut self, released: &mut Vec<TxKeySet>) {
    let min_ts = self.min_active_ts();
    while self
      .recent_commits
      .front()
      .is_some_and(|commit| commit.commit_ts < min_ts)
    {
      if let Some(commit) = self.recent_commits.pop_front() {
        self.recent_keys -= commit.keys.len();
        self.total_pruned += commit.keys.len();
        released.push(commit.keys);
      }
    }

    let limits = self.limits;
    // Staged writes stay whole, so unstaging can take them back.
    let first_staged = self.staged.front().map(|&(_, commit_ts)| commit_ts);
    if self.recent_commits.len() > limits.recent_commits || self.recent_keys > limits.recent_keys {
      while self.recent_commits.len() > limits.recent_commits / 2
        || self.recent_keys > limits.recent_keys / 2
      {
        if self.recent_commits.front().is_some_and(|commit| {
          first_staged.is_some_and(|first_staged| commit.commit_ts >= first_staged)
        }) {
          break;
        }
        let Some(commit) = self.recent_commits.pop_front() else {
          break;
        };
        self.recent_keys -= commit.keys.len();
        self.index_committed_writes(commit);
      }
    }
    // Folded writes go once every snapshot is past them (constant work while
    // none is: see `prune_committed_writes`).
    if self.committed_writes.len() > self.limits.committed_writes {
      self.prune_committed_writes();
    }

    if self.group_writes.len() > self.group_writes_prune_at {
      #[cfg(test)]
      {
        self.prune_work += self.group_writes.len() as u64;
      }
      self
        .group_writes
        .retain(|_, commit_ts| *commit_ts >= min_ts);
      self.group_floor = self.group_floor.max(min_ts);
      self.group_writes_prune_at = limits.group_writes_min.max(2 * self.group_writes.len());
    }
  }

  /// Index `commit`'s writes in `committed_writes`, keeping the newest
  /// commit timestamp per key.
  fn index_committed_writes(&mut self, commit: CommitWrites) {
    let commit_ts = commit.commit_ts;
    for key in commit.keys {
      let newer = self
        .committed_writes
        .get(&key)
        .is_none_or(|&existing_ts| commit_ts > existing_ts);
      if newer {
        self.committed_writes.insert(key.clone(), commit_ts);
        self.committed_writes_log.push_back((commit_ts, key));
      }
    }

    if self.committed_writes.len() > self.limits.committed_writes {
      self.prune_committed_writes();
    }
    if self.committed_writes_log.len() > 2 * self.committed_writes.len() + COMMIT_LOG_SLACK {
      self.compact_committed_writes_log();
    }
  }

  /// Abort a transaction and drop its record. A staged transaction's
  /// group is unstaged first (its commit path unstages it before it aborts,
  /// so this is a fallback).
  pub fn abort_tx(&mut self, txid: TxId) {
    if self.staged.iter().any(|&(staged, _)| staged == txid) {
      debug_assert!(
        false,
        "staged transaction {txid} aborted before its group was unstaged"
      );
      self.unstage_commits();
    }
    self.active_txs.remove(&txid);
    self.tx_key_groups.remove(&txid);
  }

  /// Drop a transaction's record without committing it. Commit and abort
  /// already drop theirs.
  pub fn remove_tx(&mut self, txid: TxId) {
    self.abort_tx(txid);
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
    if min_commit_ts >= self.group_floor
      && self
        .group_writes
        .get(&key_group(key))
        .is_none_or(|&commit_ts| commit_ts < min_commit_ts)
    {
      return None;
    }
    // Newest first; every recent commit is newer than the folded ones.
    for commit in self.recent_commits.iter().rev() {
      if commit.commit_ts < min_commit_ts {
        return None;
      }
      if commit.keys.contains(key) {
        return Some(commit.commit_ts);
      }
    }
    self
      .committed_writes
      .get(key)
      .copied()
      .filter(|&max_ts| max_ts >= min_commit_ts)
  }

  /// Whether a transaction that began at `start_ts` can conflict with any
  /// indexed write (`has_conflicting_write` is false for every key otherwise)
  pub fn has_writes_since(&self, start_ts: Timestamp) -> bool {
    self.newest_indexed_ts >= start_ts
  }

  /// Check if there's a conflicting write for a key (fast path for conflict detection)
  /// Returns true if any transaction wrote this key with commitTs >= minCommitTs
  pub fn has_conflicting_write(&self, key: &TxKey, min_commit_ts: Timestamp) -> bool {
    self.committed_write_ts(key, min_commit_ts).is_some()
  }

  /// Clear all transactions (for testing/recovery)
  pub fn clear(&mut self) {
    self.active_txs.clear();
    self.tx_key_groups.clear();
    self.recent_commits.clear();
    self.recent_keys = 0;
    self.group_writes.clear();
    self.committed_writes.clear();
    self.committed_writes_log.clear();
    self.newest_indexed_ts = 0;
    self.commit_wall_clock.clear();
    self.has_unindexed = false;
    self.staged.clear();
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

  /// Whether commits note their wall clock time, which
  /// `retention_horizon_ts` needs for a retention period above 0 (on by
  /// default). Turning it off forgets the times noted.
  pub fn set_wall_clock_tracking(&mut self, track: bool) {
    self.track_wall_clock = track;
    if !track {
      self.commit_wall_clock.clear();
    }
  }

  /// Get the oldest commit timestamp younger than the retention period (the
  /// next commit timestamp when none is, or when commits note no wall clock
  /// time: see `set_wall_clock_tracking`)
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
      size: self.committed_writes.len() + self.recent_keys,
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
    let target_size = self.limits.committed_writes.saturating_sub(
      PRUNE_THRESHOLD_ENTRIES * self.limits.committed_writes / MAX_COMMITTED_WRITES,
    );
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

  /// Fold, prune and compact at these sizes instead (test instrumentation):
  /// `recent_commits` and `recent_keys` recent commits and keys kept whole,
  /// `group_writes` entries before the first prune, `committed_writes`
  /// entries before pruning (down to half).
  #[cfg(test)]
  pub(crate) fn set_keep_limits_for_test(
    &mut self,
    recent_commits: usize,
    recent_keys: usize,
    group_writes: usize,
    committed_writes: usize,
  ) {
    self.limits = KeepLimits {
      recent_commits,
      recent_keys,
      group_writes_min: group_writes,
      committed_writes,
    };
    self.group_writes_prune_at = group_writes;
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

/// One commit's writes, kept whole in `TxManager::recent_commits`.
#[derive(Debug)]
struct CommitWrites {
  commit_ts: Timestamp,
  keys: TxKeySet,
  /// Not noted in `group_writes` yet (with the groups to note, if known):
  /// staged while no transaction could check against them (see
  /// `TxManager::stage_commit`).
  unindexed: Option<Option<TxKeyGroups>>,
}

/// The key groups (see `key_group`) of a transaction's reads and writes,
/// each once. Its conflict check and its commit then look up and note a few
/// groups instead of every key. The database computes them before it takes
/// any lock.
///
/// They pay off for transactions of more than a few keys: for a small one,
/// looking its keys up costs less than working out and handing over its
/// groups (see `KEY_GROUPS_MIN_KEYS`).
#[derive(Debug, Clone, Default)]
pub struct TxKeyGroups {
  /// The groups of the writes, then those of the reads no write shares.
  groups: Vec<u64>,
  /// How many of `groups` are the writes'.
  writes: usize,
}

/// Transactions with fewer reads and writes than this hand over no key
/// groups (see `TxKeyGroups`).
pub const KEY_GROUPS_MIN_KEYS: usize = 16;

impl TxKeyGroups {
  /// The groups of `reads` and `writes`.
  pub fn of(reads: &TxKeySet, writes: &TxKeySet) -> Self {
    let mut seen = hashbrown::HashSet::with_capacity(writes.len() / 4 + reads.len());
    let mut groups: Vec<u64> = writes
      .iter()
      .map(key_group)
      .filter(|&group| seen.insert(group))
      .collect();
    let writes = groups.len();
    groups.extend(
      reads
        .iter()
        .map(key_group)
        .filter(|&group| seen.insert(group)),
    );
    Self { groups, writes }
  }

  /// The groups of the reads and the writes, each once.
  fn reads_and_writes(&self) -> &[u64] {
    &self.groups
  }

  /// The groups of the writes.
  fn writes(&self) -> &[u64] {
    &self.groups[..self.writes]
  }
}

/// The group of `key` in `TxManager::group_writes`: the node it concerns (an
/// edge's source), so most of a transaction's keys share few groups, or a
/// hash of a node key string. Groups only filter: two keys sharing one cost a
/// lookup, never a missed conflict.
pub fn key_group(key: &TxKey) -> u64 {
  match key {
    TxKey::Node(node_id) | TxKey::NodeLabels(node_id) => *node_id,
    TxKey::NodeProp { node_id, .. }
    | TxKey::NeighborsOut { node_id, .. }
    | TxKey::NeighborsIn { node_id, .. }
    | TxKey::NodeLabel { node_id, .. } => *node_id,
    TxKey::Edge { src, .. } | TxKey::EdgeProp { src, .. } => *src,
    // Node ids leave the top bit clear (they stay within i64).
    TxKey::Key(key) => xxhash_rust::xxh64::xxh64(key.as_bytes(), 0) | (1 << 63),
  }
}

/// Note in `group_writes` that the commit at `commit_ts` wrote `writes`
/// (whose groups are `key_groups`, if known). Commit timestamps only grow,
/// so this is each group's newest.
fn note_group_writes(
  group_writes: &mut HashMap<u64, Timestamp>,
  commit_ts: Timestamp,
  writes: &TxKeySet,
  key_groups: Option<TxKeyGroups>,
) {
  match key_groups {
    Some(groups) => {
      for &group in groups.writes() {
        group_writes.insert(group, commit_ts);
      }
    }
    None => {
      for key in writes {
        group_writes.insert(key_group(key), commit_ts);
      }
    }
  }
}

/// Add `keys` to `set`, moving them in whole when `set` is empty.
fn absorb(set: &mut TxKeySet, keys: TxKeySet) {
  if set.is_empty() {
    *set = keys;
  } else {
    set.extend(keys);
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
  /// Staged out of order (see `TxManager::stage_commit`)
  InvalidState(String),
}

impl std::fmt::Display for TxManagerError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      TxManagerError::TxNotFound(txid) => write!(f, "Transaction {txid} not found"),
      TxManagerError::InvalidState(message) => f.write_str(message),
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

  /// A commit drops the recent commits no open transaction can conflict with any more. Their
  /// key sets go to `released`, for the caller to free outside its locks: the publish commits
  /// in MVCC under the delta write lock, where every reader and writer waits, and a 200-node
  /// transaction writes thousands of keys. Regression: they were freed there.
  #[test]
  fn pruned_commits_hand_their_keys_to_released() {
    let mut tx_mgr = TxManager::new();
    let (reader, _) = tx_mgr.begin_tx();
    let (writer, _) = tx_mgr.begin_tx();
    for i in 0..100 {
      tx_mgr.record_write(writer, TxKey::Node(i));
    }
    tx_mgr
      .commit_tx_releasing(writer, &mut Vec::new())
      .expect("commit");

    // A transaction that began after it, so the writer's keys go once the reader ends.
    let (later, _) = tx_mgr.begin_tx();
    tx_mgr.abort_tx(reader);
    let (next, _) = tx_mgr.begin_tx();
    tx_mgr.record_write(next, TxKey::Node(1000));
    let mut released = Vec::new();
    tx_mgr
      .commit_tx_releasing(next, &mut released)
      .expect("commit");
    assert!(
      released.iter().any(|keys| keys.len() == 100),
      "the pruned commit's keys were freed under the lock: released {:?}",
      released.iter().map(|keys| keys.len()).collect::<Vec<_>>()
    );
    tx_mgr.abort_tx(later);
  }
}
