//! Key lookup index
//!
//! Provides functions for looking up nodes by their unique keys.
//! The key index merges snapshot and delta state.

use crate::core::snapshot::reader::SnapshotData;
use crate::types::*;

/// Key lookup result from delta
pub enum DeltaKeyResult {
  /// Key found, returns node ID
  Found(NodeId),
  /// Key was explicitly deleted
  Deleted,
  /// Key not found in delta
  NotFound,
}

/// Look up a key in the delta state
pub fn lookup_key_in_delta(delta: &DeltaState, key: &str) -> DeltaKeyResult {
  // Check if key was deleted
  if delta.key_index_deleted.contains(key) {
    return DeltaKeyResult::Deleted;
  }

  // Check key index for new/updated keys
  if let Some(&node_id) = delta.key_index.get(key) {
    return DeltaKeyResult::Found(node_id);
  }

  // Check created nodes for key
  for (&node_id, node_delta) in &delta.created_nodes {
    if node_delta.key.as_deref() == Some(key) {
      return DeltaKeyResult::Found(node_id);
    }
  }

  DeltaKeyResult::NotFound
}

/// Look up a node by key across snapshot and delta
/// Delta takes precedence over snapshot
pub fn lookup_by_key(
  snapshot: Option<&SnapshotData>,
  delta: &DeltaState,
  key: &str,
) -> Option<NodeId> {
  // First check delta
  match lookup_key_in_delta(delta, key) {
    DeltaKeyResult::Found(node_id) => {
      // Make sure node isn't deleted
      if !delta.deleted_nodes.contains(&node_id) {
        return Some(node_id);
      }
      return None;
    }
    DeltaKeyResult::Deleted => {
      return None;
    }
    DeltaKeyResult::NotFound => {}
  }

  // Fall back to snapshot
  if let Some(snap) = snapshot {
    if let Some(node_id) = snap.lookup_by_key(key) {
      // Check if the node was deleted in delta
      if !delta.deleted_nodes.contains(&node_id) {
        return Some(node_id);
      }
    }
  }

  None
}

/// Check if a key exists
pub fn has_key(snapshot: Option<&SnapshotData>, delta: &DeltaState, key: &str) -> bool {
  lookup_by_key(snapshot, delta, key).is_some()
}

/// Get the key for a node if it has one
pub fn get_node_key(
  snapshot: Option<&SnapshotData>,
  delta: &DeltaState,
  node_id: NodeId,
) -> Option<String> {
  // Check if node is deleted
  if delta.deleted_nodes.contains(&node_id) {
    return None;
  }

  // Check delta for created nodes
  if let Some(node_delta) = delta.created_nodes.get(&node_id) {
    return node_delta.key.clone();
  }

  // Check delta for modified nodes
  if let Some(node_delta) = delta.modified_nodes.get(&node_id) {
    if node_delta.key.is_some() {
      return node_delta.key.clone();
    }
  }

  // Fall back to snapshot
  if let Some(snap) = snapshot {
    if let Some(phys) = snap.get_phys_node(node_id) {
      return snap.get_node_key(phys);
    }
  }

  None
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_lookup_in_empty_delta() {
    let delta = DeltaState::default();

    match lookup_key_in_delta(&delta, "test_key") {
      DeltaKeyResult::NotFound => {}
      _ => panic!("Expected NotFound"),
    }
  }

  #[test]
  fn test_lookup_created_node() {
    let mut delta = DeltaState::default();
    delta.create_node(42, Some("alice"));

    match lookup_key_in_delta(&delta, "alice") {
      DeltaKeyResult::Found(id) => assert_eq!(id, 42),
      _ => panic!("Expected Found"),
    }
  }

  #[test]
  fn test_lookup_deleted_key() {
    let mut delta = DeltaState::default();
    delta.key_index_deleted.insert("deleted_key".to_string());

    match lookup_key_in_delta(&delta, "deleted_key") {
      DeltaKeyResult::Deleted => {}
      _ => panic!("Expected Deleted"),
    }
  }

  #[test]
  fn test_lookup_by_key_with_delta() {
    let mut delta = DeltaState::default();
    delta.create_node(100, Some("bob"));

    let result = lookup_by_key(None, &delta, "bob");
    assert_eq!(result, Some(100));
  }

  #[test]
  fn test_has_key() {
    let mut delta = DeltaState::default();
    delta.create_node(1, Some("exists"));

    assert!(has_key(None, &delta, "exists"));
    assert!(!has_key(None, &delta, "not_exists"));
  }

  #[test]
  fn test_get_node_key() {
    let mut delta = DeltaState::default();
    delta.create_node(50, Some("charlie"));

    assert_eq!(get_node_key(None, &delta, 50), Some("charlie".to_string()));
    assert_eq!(get_node_key(None, &delta, 99), None);
  }
}
