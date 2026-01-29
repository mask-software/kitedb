//! MVCC Visibility Rules
//!
//! Determines which versions are visible to a transaction based on snapshot isolation.
//!
//! Ported from src/mvcc/visibility.ts

use crate::types::{Timestamp, TxId};

// ============================================================================
// Versioned Record
// ============================================================================

/// A versioned record in an MVCC version chain
#[derive(Debug, Clone)]
pub struct VersionedRecord<T> {
  /// The data at this version
  pub data: T,
  /// Transaction ID that created this version
  pub txid: TxId,
  /// Commit timestamp (0 if uncommitted)
  pub commit_ts: Timestamp,
  /// Previous version in the chain
  pub prev: Option<Box<VersionedRecord<T>>>,
  /// Whether this record marks a deletion
  pub deleted: bool,
}

impl<T> VersionedRecord<T> {
  /// Create a new versioned record
  pub fn new(data: T, txid: TxId, commit_ts: Timestamp) -> Self {
    Self {
      data,
      txid,
      commit_ts,
      prev: None,
      deleted: false,
    }
  }

  /// Create a new versioned record with a previous version
  pub fn with_prev(
    data: T,
    txid: TxId,
    commit_ts: Timestamp,
    prev: Box<VersionedRecord<T>>,
  ) -> Self {
    Self {
      data,
      txid,
      commit_ts,
      prev: Some(prev),
      deleted: false,
    }
  }

  /// Create a deletion marker
  pub fn deletion(
    data: T,
    txid: TxId,
    commit_ts: Timestamp,
    prev: Option<Box<VersionedRecord<T>>>,
  ) -> Self {
    Self {
      data,
      txid,
      commit_ts,
      prev,
      deleted: true,
    }
  }

  /// Get the chain depth (number of versions)
  pub fn chain_depth(&self) -> usize {
    let mut depth = 1;
    let mut current = self.prev.as_ref();
    while let Some(prev) = current {
      depth += 1;
      current = prev.prev.as_ref();
    }
    depth
  }
}

// ============================================================================
// Visibility Functions
// ============================================================================

/// Check if a version is visible to a transaction
///
/// A version is visible if:
/// 1. It was committed before the transaction's snapshot timestamp (commit_ts < snapshot_ts)
/// 2. OR it was created by the transaction itself (own writes)
/// 3. AND it's not deleted (unless checking for deletion)
pub fn is_visible<T>(version: &VersionedRecord<T>, snapshot_ts: Timestamp, txid: TxId) -> bool {
  // Own writes are always visible (even if uncommitted)
  if version.txid == txid {
    return true;
  }

  // Uncommitted transactions (commit_ts = 0) are not visible to others
  if version.commit_ts == 0 {
    return false;
  }

  // Must be committed before snapshot
  version.commit_ts < snapshot_ts
}

/// Get the visible version from a version chain
///
/// Walks the chain to find the newest version visible to the transaction.
/// Returns None if no version is visible.
pub fn get_visible_version<T>(
  head: &VersionedRecord<T>,
  snapshot_ts: Timestamp,
  txid: TxId,
) -> Option<&VersionedRecord<T>> {
  // Fast path: single-version chain (most common case)
  if head.prev.is_none() {
    if is_visible(head, snapshot_ts, txid) {
      return Some(head);
    }
    return None;
  }

  // Slow path: multi-version chain - walk from newest to oldest
  let mut current = Some(head);

  while let Some(version) = current {
    if is_visible(version, snapshot_ts, txid) {
      return Some(version);
    }
    current = version.prev.as_deref();
  }

  None
}

/// Get a mutable reference to the visible version from a version chain
pub fn get_visible_version_mut<T>(
  head: &mut VersionedRecord<T>,
  snapshot_ts: Timestamp,
  txid: TxId,
) -> Option<&mut VersionedRecord<T>> {
  // Fast path: single-version chain
  if head.prev.is_none() {
    if is_visible(head, snapshot_ts, txid) {
      return Some(head);
    }
    return None;
  }

  // For mutable access, we need to check visibility first, then traverse
  // This is more complex due to borrow checker
  if is_visible(head, snapshot_ts, txid) {
    return Some(head);
  }

  // Check if any previous version is visible
  let mut current = head.prev.as_mut();
  while let Some(version) = current {
    if is_visible(version, snapshot_ts, txid) {
      return Some(version);
    }
    current = version.prev.as_mut();
  }

  None
}

/// Check if a node exists (is visible and not deleted)
pub fn node_exists<T>(
  version: Option<&VersionedRecord<T>>,
  snapshot_ts: Timestamp,
  txid: TxId,
) -> bool {
  let Some(head) = version else {
    return false;
  };

  let visible = get_visible_version(head, snapshot_ts, txid);
  match visible {
    Some(v) => !v.deleted,
    None => false,
  }
}

/// Check if an edge exists (is visible and was added, not deleted)
///
/// For edges, we store EdgeVersionData with an `added` field.
/// - added=true means the edge exists
/// - added=false means the edge was deleted
pub fn edge_exists<T: EdgeLike>(
  version: Option<&VersionedRecord<T>>,
  snapshot_ts: Timestamp,
  txid: TxId,
) -> bool {
  let Some(head) = version else {
    return false;
  };

  let visible = get_visible_version(head, snapshot_ts, txid);
  match visible {
    Some(v) => v.data.is_added(),
    None => false,
  }
}

/// Trait for edge-like data that has an `added` flag
pub trait EdgeLike {
  fn is_added(&self) -> bool;
}

impl EdgeLike for crate::types::EdgeVersionData {
  fn is_added(&self) -> bool {
    self.added
  }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_is_visible_own_write() {
    let version = VersionedRecord::new(42, 1, 0); // Uncommitted

    // Own transaction can see uncommitted version
    assert!(is_visible(&version, 100, 1));

    // Other transactions cannot
    assert!(!is_visible(&version, 100, 2));
  }

  #[test]
  fn test_is_visible_committed() {
    let version = VersionedRecord::new(42, 1, 10);

    // Snapshot after commit can see it
    assert!(is_visible(&version, 20, 2));

    // Snapshot at commit time cannot see it
    assert!(!is_visible(&version, 10, 2));

    // Snapshot before commit cannot see it
    assert!(!is_visible(&version, 5, 2));
  }

  #[test]
  fn test_get_visible_version_single() {
    let version = VersionedRecord::new(42, 1, 10);

    let visible = get_visible_version(&version, 20, 2);
    assert!(visible.is_some());
    assert_eq!(visible.unwrap().data, 42);

    let not_visible = get_visible_version(&version, 5, 2);
    assert!(not_visible.is_none());
  }

  #[test]
  fn test_get_visible_version_chain() {
    // Create a version chain: v3 -> v2 -> v1
    let v1 = VersionedRecord::new(1, 1, 10);
    let v2 = VersionedRecord::with_prev(2, 2, 20, Box::new(v1));
    let v3 = VersionedRecord::with_prev(3, 3, 30, Box::new(v2));

    // Snapshot at 35 sees v3
    let visible = get_visible_version(&v3, 35, 100);
    assert_eq!(visible.unwrap().data, 3);

    // Snapshot at 25 sees v2
    let visible = get_visible_version(&v3, 25, 100);
    assert_eq!(visible.unwrap().data, 2);

    // Snapshot at 15 sees v1
    let visible = get_visible_version(&v3, 15, 100);
    assert_eq!(visible.unwrap().data, 1);

    // Snapshot at 5 sees nothing
    let visible = get_visible_version(&v3, 5, 100);
    assert!(visible.is_none());
  }

  #[test]
  fn test_node_exists_deleted() {
    // Create a node that was deleted
    let v1 = VersionedRecord::new("created", 1, 10);
    let v2 = VersionedRecord::deletion("deleted", 2, 20, Some(Box::new(v1)));

    // Before deletion, node exists
    assert!(node_exists(Some(&v2), 15, 100));

    // After deletion, node doesn't exist
    assert!(!node_exists(Some(&v2), 25, 100));
  }

  #[test]
  fn test_chain_depth() {
    let v1 = VersionedRecord::new(1, 1, 10);
    assert_eq!(v1.chain_depth(), 1);

    let v2 = VersionedRecord::with_prev(2, 2, 20, Box::new(v1));
    assert_eq!(v2.chain_depth(), 2);

    let v3 = VersionedRecord::with_prev(3, 3, 30, Box::new(v2));
    assert_eq!(v3.chain_depth(), 3);
  }

  // Test edge existence
  #[derive(Debug, Clone)]
  struct TestEdge {
    added: bool,
  }

  impl EdgeLike for TestEdge {
    fn is_added(&self) -> bool {
      self.added
    }
  }

  #[test]
  fn test_edge_exists() {
    let v1 = VersionedRecord::new(TestEdge { added: true }, 1, 10);

    assert!(edge_exists(Some(&v1), 15, 100));

    let v2 = VersionedRecord::with_prev(TestEdge { added: false }, 2, 20, Box::new(v1));

    // Before deletion
    assert!(edge_exists(Some(&v2), 15, 100));

    // After deletion
    assert!(!edge_exists(Some(&v2), 25, 100));
  }

  #[test]
  fn test_no_version() {
    assert!(!node_exists::<i32>(None, 100, 1));

    struct DummyEdge;
    impl EdgeLike for DummyEdge {
      fn is_added(&self) -> bool {
        false
      }
    }
    assert!(!edge_exists::<DummyEdge>(None, 100, 1));
  }
}
