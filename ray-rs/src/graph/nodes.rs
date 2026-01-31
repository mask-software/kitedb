//! Node CRUD operations
//!
//! Provides functions for creating, deleting, and querying nodes.

use crate::error::{RayError, Result};
use crate::mvcc::visibility::{get_visible_version, node_exists as mvcc_node_exists};
use crate::types::*;

use super::tx::TxHandle;

// ============================================================================
// Node Options
// ============================================================================

/// Options for creating a node
#[derive(Debug, Default, Clone)]
pub struct NodeOpts {
  /// Optional unique key for the node
  pub key: Option<String>,
  /// Initial labels for the node
  pub labels: Option<Vec<LabelId>>,
  /// Initial properties for the node
  pub props: Option<Vec<(PropKeyId, PropValue)>>,
}

impl NodeOpts {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn with_key(mut self, key: impl Into<String>) -> Self {
    self.key = Some(key.into());
    self
  }

  pub fn with_label(mut self, label: LabelId) -> Self {
    self.labels.get_or_insert_with(Vec::new).push(label);
    self
  }

  pub fn with_prop(mut self, key: PropKeyId, value: PropValue) -> Self {
    self.props.get_or_insert_with(Vec::new).push((key, value));
    self
  }
}

// ============================================================================
// Node Operations
// ============================================================================

/// Create a new node
pub fn create_node(handle: &mut TxHandle, opts: NodeOpts) -> Result<NodeId> {
  if handle.tx.read_only {
    return Err(RayError::ReadOnly);
  }

  let node_id = handle.db.alloc_node_id();

  let mut node_delta = NodeDelta {
    key: opts.key.clone(),
    labels: None,
    labels_deleted: None,
    props: None,
  };

  if let Some(labels) = opts.labels {
    let mut set = std::collections::HashSet::new();
    for label_id in labels {
      set.insert(label_id);
    }
    node_delta.labels = Some(set);
  }

  handle.tx.pending_created_nodes.insert(node_id, node_delta);

  if let Some(key) = opts.key {
    handle.tx.pending_key_updates.insert(key, node_id);
  }

  if let Some(props) = opts.props {
    let mut map = std::collections::HashMap::new();
    for (key_id, value) in props {
      map.insert(key_id, Some(value));
    }
    handle.tx.pending_node_props.insert(node_id, map);
  }

  if let Some(mvcc) = handle.db.mvcc.as_ref() {
    let mut tx_mgr = mvcc.tx_manager.lock();
    tx_mgr.record_write(handle.tx.txid, format!("node:{node_id}"));
  }

  Ok(node_id)
}

/// Delete a node
pub fn delete_node(handle: &mut TxHandle, node_id: NodeId) -> Result<bool> {
  if handle.tx.read_only {
    return Err(RayError::ReadOnly);
  }

  // If created in this transaction, remove it entirely
  if let Some(node_delta) = handle.tx.pending_created_nodes.remove(&node_id) {
    if let Some(key) = node_delta.key {
      handle.tx.pending_key_updates.remove(&key);
    }
    handle.tx.pending_node_props.remove(&node_id);
    handle.tx.pending_out_add.remove(&node_id);
    handle.tx.pending_out_del.remove(&node_id);
    handle.tx.pending_in_add.remove(&node_id);
    handle.tx.pending_in_del.remove(&node_id);
    handle.tx.pending_node_labels_add.remove(&node_id);
    handle.tx.pending_node_labels_del.remove(&node_id);
    handle
      .tx
      .pending_vector_sets
      .retain(|(n, _), _| *n != node_id);
    handle
      .tx
      .pending_vector_deletes
      .retain(|(n, _)| *n != node_id);
    return Ok(true);
  }

  if !node_exists_internal(handle.db, node_id) {
    return Ok(false);
  }

  handle.tx.pending_deleted_nodes.insert(node_id);

  // Cascade delete vectors for this node
  {
    let stores = handle.db.vector_stores.read();
    for (prop_key_id, store) in stores.iter() {
      if store.node_to_vector.contains_key(&node_id) {
        handle
          .tx
          .pending_vector_deletes
          .insert((node_id, *prop_key_id));
      }
    }
  }

  if let Some(mvcc) = handle.db.mvcc.as_ref() {
    let mut tx_mgr = mvcc.tx_manager.lock();
    tx_mgr.record_write(handle.tx.txid, format!("node:{node_id}"));
  }

  Ok(true)
}

/// Check if a node exists
pub fn node_exists(handle: &TxHandle, node_id: NodeId) -> bool {
  if handle.tx.pending_created_nodes.contains_key(&node_id) {
    return true;
  }
  if handle.tx.pending_deleted_nodes.contains(&node_id) {
    return false;
  }

  if let Some(mvcc) = handle.db.mvcc.as_ref() {
    let tx_snapshot_ts = handle.tx.snapshot_ts;
    let txid = handle.tx.txid;
    {
      let mut tx_mgr = mvcc.tx_manager.lock();
      tx_mgr.record_read(txid, format!("node:{node_id}"));
    }
    let vc = mvcc.version_chain.lock();
    if let Some(version) = vc.get_node_version(node_id) {
      return mvcc_node_exists(Some(version), tx_snapshot_ts, txid);
    }
  }

  node_exists_db(handle.db, node_id)
}

/// Internal node existence check (on GraphDB directly)
fn node_exists_internal(db: &super::db::GraphDB, node_id: NodeId) -> bool {
  node_exists_db(db, node_id)
}

// ============================================================================
// Direct Read Functions (No Transaction Required)
// ============================================================================
// These functions read directly from snapshot + delta without transaction
// overhead, matching the TypeScript implementation pattern.

/// Check if a node exists (direct read, no transaction)
pub fn node_exists_db(db: &super::db::GraphDB, node_id: NodeId) -> bool {
  if let Some(mvcc) = db.mvcc.as_ref() {
    let tx_snapshot_ts = mvcc.tx_manager.lock().get_next_commit_ts();
    let txid = 0;
    let vc = mvcc.version_chain.lock();
    if let Some(version) = vc.get_node_version(node_id) {
      return mvcc_node_exists(Some(version), tx_snapshot_ts, txid);
    }
  }

  let delta = db.delta.read();

  if delta.is_node_deleted(node_id) {
    return false;
  }

  if delta.is_node_created(node_id) {
    return true;
  }

  // Check snapshot
  if let Some(ref snapshot) = db.snapshot {
    return snapshot.has_node(node_id);
  }

  false
}

/// Get a node by its key (direct read, no transaction)
pub fn get_node_by_key_db(db: &super::db::GraphDB, key: &str) -> Option<NodeId> {
  let delta = db.delta.read();

  // Check if key was deleted in delta
  if delta.key_index_deleted.contains(key) {
    return None;
  }

  // Check if key exists in delta
  if let Some(node_id) = delta.get_node_by_key(key) {
    // Make sure the node isn't deleted
    if !delta.is_node_deleted(node_id) {
      return Some(node_id);
    }
  }

  // Check snapshot
  if let Some(ref snapshot) = db.snapshot {
    if let Some(node_id) = snapshot.lookup_by_key(key) {
      // Make sure node wasn't deleted in delta
      if !delta.is_node_deleted(node_id) {
        return Some(node_id);
      }
    }
  }

  None
}

/// Get a node property (direct read, no transaction)
pub fn get_node_prop_db(
  db: &super::db::GraphDB,
  node_id: NodeId,
  key_id: PropKeyId,
) -> Option<PropValue> {
  if let Some(mvcc) = db.mvcc.as_ref() {
    let tx_snapshot_ts = mvcc.tx_manager.lock().get_next_commit_ts();
    let txid = 0;
    let vc = mvcc.version_chain.lock();
    if let Some(prop_version) = vc.get_node_prop_version(node_id, key_id) {
      if let Some(visible) = get_visible_version(&prop_version, tx_snapshot_ts, txid) {
        return visible.data.clone();
      }
    }
  }

  get_node_prop_committed(db, node_id, key_id)
}

/// Get a node property from committed state only (snapshot + delta)
pub fn get_node_prop_committed(
  db: &super::db::GraphDB,
  node_id: NodeId,
  key_id: PropKeyId,
) -> Option<PropValue> {
  let delta = db.delta.read();

  // Check if node is deleted
  if delta.is_node_deleted(node_id) {
    return None;
  }

  // Check delta for property (Some(Some(v)) = set, Some(None) = deleted)
  if let Some(value_opt) = delta.get_node_prop(node_id, key_id) {
    return value_opt.cloned();
  }

  // Check snapshot
  if let Some(ref snapshot) = db.snapshot {
    if let Some(phys) = snapshot.get_phys_node(node_id) {
      return snapshot.get_node_prop(phys, key_id);
    }
  }

  None
}

/// Get all node properties (direct read, no transaction)
pub fn get_node_props_db(
  db: &super::db::GraphDB,
  node_id: NodeId,
) -> Option<std::collections::HashMap<PropKeyId, PropValue>> {
  use std::collections::HashMap;

  let delta = db.delta.read();
  if delta.is_node_deleted(node_id) {
    return None;
  }

  let mut props = HashMap::new();
  let node_created_in_delta = delta.is_node_created(node_id);
  let mut node_exists_in_snapshot = false;

  if let Some(ref snapshot) = db.snapshot {
    if let Some(phys) = snapshot.get_phys_node(node_id) {
      node_exists_in_snapshot = true;
      if let Some(snapshot_props) = snapshot.get_node_props(phys) {
        props = snapshot_props;
      }
    }
  }

  if !node_created_in_delta && !node_exists_in_snapshot {
    return None;
  }

  if let Some(node_delta) = delta
    .created_nodes
    .get(&node_id)
    .or_else(|| delta.modified_nodes.get(&node_id))
  {
    if let Some(delta_props) = node_delta.props.as_ref() {
      for (&key_id, value) in delta_props {
        match value {
          Some(v) => {
            props.insert(key_id, v.clone());
          }
          None => {
            props.remove(&key_id);
          }
        }
      }
    }
  }

  Some(props)
}

// ============================================================================
// Node Label Operations
// ============================================================================

/// Add a label to a node
pub fn add_node_label(handle: &mut TxHandle, node_id: NodeId, label_id: LabelId) -> Result<()> {
  if handle.tx.read_only {
    return Err(RayError::ReadOnly);
  }

  if let Some(node_delta) = handle.tx.pending_created_nodes.get_mut(&node_id) {
    let labels = node_delta
      .labels
      .get_or_insert_with(std::collections::HashSet::new);
    labels.insert(label_id);
  } else {
    if let Some(removed) = handle.tx.pending_node_labels_del.get_mut(&node_id) {
      removed.remove(&label_id);
      if removed.is_empty() {
        handle.tx.pending_node_labels_del.remove(&node_id);
      }
    }
    handle
      .tx
      .pending_node_labels_add
      .entry(node_id)
      .or_default()
      .insert(label_id);
  }

  if let Some(mvcc) = handle.db.mvcc.as_ref() {
    let mut tx_mgr = mvcc.tx_manager.lock();
    tx_mgr.record_write(handle.tx.txid, format!("node:{node_id}"));
  }

  Ok(())
}

/// Remove a label from a node
pub fn remove_node_label(handle: &mut TxHandle, node_id: NodeId, label_id: LabelId) -> Result<()> {
  if handle.tx.read_only {
    return Err(RayError::ReadOnly);
  }

  if let Some(node_delta) = handle.tx.pending_created_nodes.get_mut(&node_id) {
    if let Some(labels) = node_delta.labels.as_mut() {
      labels.remove(&label_id);
    }
  } else {
    if let Some(added) = handle.tx.pending_node_labels_add.get_mut(&node_id) {
      added.remove(&label_id);
      if added.is_empty() {
        handle.tx.pending_node_labels_add.remove(&node_id);
      }
    }
    handle
      .tx
      .pending_node_labels_del
      .entry(node_id)
      .or_default()
      .insert(label_id);
  }

  if let Some(mvcc) = handle.db.mvcc.as_ref() {
    let mut tx_mgr = mvcc.tx_manager.lock();
    tx_mgr.record_write(handle.tx.txid, format!("node:{node_id}"));
  }

  Ok(())
}

/// Check if a node has a label (direct read)
pub fn node_has_label_db(db: &super::db::GraphDB, node_id: NodeId, label_id: LabelId) -> bool {
  let delta = db.delta.read();
  if delta.is_node_deleted(node_id) {
    return false;
  }

  if delta.is_label_removed(node_id, label_id) {
    return false;
  }

  if delta.is_label_added(node_id, label_id) {
    return true;
  }

  if let Some(ref snapshot) = db.snapshot {
    if let Some(phys) = snapshot.get_phys_node(node_id) {
      if let Some(labels) = snapshot.get_node_labels(phys) {
        return labels.contains(&label_id);
      }
    }
  }

  false
}

/// Get all labels for a node (direct read)
pub fn get_node_labels_db(db: &super::db::GraphDB, node_id: NodeId) -> Vec<LabelId> {
  let delta = db.delta.read();
  if delta.is_node_deleted(node_id) {
    return Vec::new();
  }

  let mut labels: std::collections::HashSet<LabelId> = std::collections::HashSet::new();

  if let Some(ref snapshot) = db.snapshot {
    if let Some(phys) = snapshot.get_phys_node(node_id) {
      if let Some(snapshot_labels) = snapshot.get_node_labels(phys) {
        labels.extend(snapshot_labels);
      }
    }
  }

  if let Some(added) = delta.get_added_labels(node_id) {
    labels.extend(added.iter().copied());
  }

  if let Some(removed) = delta.get_removed_labels(node_id) {
    for label_id in removed {
      labels.remove(label_id);
    }
  }

  labels.into_iter().collect()
}

/// Count total nodes in the database (direct read, no transaction)
pub fn count_nodes_db(db: &super::db::GraphDB) -> u64 {
  let delta = db.delta.read();

  // Start with snapshot count
  let snapshot_count = db
    .snapshot
    .as_ref()
    .map(|s| s.header.num_nodes)
    .unwrap_or(0);

  // Count nodes created in delta
  let created = delta.created_nodes.len() as u64;

  // Count nodes deleted in delta that existed in snapshot
  let mut deleted_from_snapshot = 0u64;
  if let Some(ref snapshot) = db.snapshot {
    for &node_id in &delta.deleted_nodes {
      if !delta.created_nodes.contains_key(&node_id) && snapshot.has_node(node_id) {
        deleted_from_snapshot += 1;
      }
    }
  }

  snapshot_count + created - deleted_from_snapshot
}

/// Set a node property
pub fn set_node_prop(
  handle: &mut TxHandle,
  node_id: NodeId,
  key_id: PropKeyId,
  value: PropValue,
) -> Result<()> {
  if handle.tx.read_only {
    return Err(RayError::ReadOnly);
  }

  let props = handle.tx.pending_node_props.entry(node_id).or_default();
  props.insert(key_id, Some(value));

  if let Some(mvcc) = handle.db.mvcc.as_ref() {
    let mut tx_mgr = mvcc.tx_manager.lock();
    tx_mgr.record_write(handle.tx.txid, format!("nodeprop:{node_id}:{key_id}"));
  }

  Ok(())
}

/// Delete a node property
pub fn del_node_prop(handle: &mut TxHandle, node_id: NodeId, key_id: PropKeyId) -> Result<()> {
  if handle.tx.read_only {
    return Err(RayError::ReadOnly);
  }

  let props = handle.tx.pending_node_props.entry(node_id).or_default();
  props.insert(key_id, None);

  if let Some(mvcc) = handle.db.mvcc.as_ref() {
    let mut tx_mgr = mvcc.tx_manager.lock();
    tx_mgr.record_write(handle.tx.txid, format!("nodeprop:{node_id}:{key_id}"));
  }

  Ok(())
}

/// Get a node property
pub fn get_node_prop(handle: &TxHandle, node_id: NodeId, key_id: PropKeyId) -> Option<PropValue> {
  if handle.tx.pending_deleted_nodes.contains(&node_id) {
    return None;
  }

  if let Some(pending_props) = handle.tx.pending_node_props.get(&node_id) {
    if let Some(value) = pending_props.get(&key_id) {
      return value.clone();
    }
  }

  if let Some(mvcc) = handle.db.mvcc.as_ref() {
    let tx_snapshot_ts = handle.tx.snapshot_ts;
    let txid = handle.tx.txid;
    {
      let mut tx_mgr = mvcc.tx_manager.lock();
      tx_mgr.record_read(txid, format!("nodeprop:{node_id}:{key_id}"));
    }
    let vc = mvcc.version_chain.lock();
    if let Some(prop_version) = vc.get_node_prop_version(node_id, key_id) {
      if let Some(visible) = get_visible_version(&prop_version, tx_snapshot_ts, txid) {
        return visible.data.clone();
      }
    }
  }

  let delta = handle.db.delta.read();

  if delta.is_node_deleted(node_id) {
    return None;
  }

  if let Some(value_opt) = delta.get_node_prop(node_id, key_id) {
    return value_opt.cloned();
  }

  if let Some(ref snapshot) = handle.db.snapshot {
    if let Some(phys) = snapshot.get_phys_node(node_id) {
      return snapshot.get_node_prop(phys, key_id);
    }
  }

  None
}

/// Get a node by its key
pub fn get_node_by_key(handle: &TxHandle, key: &str) -> Option<NodeId> {
  if let Some(node_id) = handle.tx.pending_key_updates.get(key) {
    return Some(*node_id);
  }
  if handle.tx.pending_key_deletes.contains(key) {
    return None;
  }

  if let Some(mvcc) = handle.db.mvcc.as_ref() {
    let mut tx_mgr = mvcc.tx_manager.lock();
    tx_mgr.record_read(handle.tx.txid, format!("key:{key}"));
  }

  get_node_by_key_db(handle.db, key)
}

/// Count total nodes in the database
pub fn count_nodes(handle: &TxHandle) -> u64 {
  let delta = handle.db.delta.read();

  // Start with snapshot count
  let snapshot_count = handle
    .db
    .snapshot
    .as_ref()
    .map(|s| s.header.num_nodes)
    .unwrap_or(0);

  // Count nodes created in delta
  let created = delta.created_nodes.len() as u64;

  // Count nodes deleted in delta that existed in snapshot
  // (only snapshot nodes should be subtracted, not delta-created-then-deleted)
  let mut deleted_from_snapshot = 0u64;
  if let Some(ref snapshot) = handle.db.snapshot {
    for &node_id in &delta.deleted_nodes {
      // Only count if it was actually in snapshot (not a delta-created node)
      if !delta.created_nodes.contains_key(&node_id) && snapshot.has_node(node_id) {
        deleted_from_snapshot += 1;
      }
    }
  }

  let mut count = snapshot_count + created - deleted_from_snapshot;

  // Apply pending deletions
  for &node_id in &handle.tx.pending_deleted_nodes {
    let in_snapshot = handle
      .db
      .snapshot
      .as_ref()
      .map(|s| s.has_node(node_id))
      .unwrap_or(false);
    let in_delta_created = delta.created_nodes.contains_key(&node_id);
    let deleted_in_delta = delta.deleted_nodes.contains(&node_id);
    if (in_snapshot || in_delta_created) && !deleted_in_delta {
      count = count.saturating_sub(1);
    }
  }

  // Add pending creations
  count + handle.tx.pending_created_nodes.len() as u64
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;
  use crate::graph::db::{close_graph_db, open_graph_db, OpenOptions};
  use crate::graph::tx::{begin_tx, commit, rollback};
  use tempfile::tempdir;

  #[test]
  fn test_create_node() {
    let temp_dir = tempdir().unwrap();
    let db = open_graph_db(temp_dir.path(), OpenOptions::new()).unwrap();

    let mut tx = begin_tx(&db).unwrap();
    let node_id = create_node(&mut tx, NodeOpts::new()).unwrap();

    assert!(node_id >= 1);

    commit(&mut tx).unwrap();
    close_graph_db(db).unwrap();
  }

  #[test]
  fn test_create_node_with_key() {
    let temp_dir = tempdir().unwrap();
    let db = open_graph_db(temp_dir.path(), OpenOptions::new()).unwrap();

    let mut tx = begin_tx(&db).unwrap();
    let node_id = create_node(&mut tx, NodeOpts::new().with_key("alice")).unwrap();

    assert!(node_id >= 1);

    commit(&mut tx).unwrap();
    close_graph_db(db).unwrap();
  }

  #[test]
  fn test_create_multiple_nodes() {
    let temp_dir = tempdir().unwrap();
    let db = open_graph_db(temp_dir.path(), OpenOptions::new()).unwrap();

    let mut tx = begin_tx(&db).unwrap();

    let node1 = create_node(&mut tx, NodeOpts::new()).unwrap();
    let node2 = create_node(&mut tx, NodeOpts::new()).unwrap();
    let node3 = create_node(&mut tx, NodeOpts::new()).unwrap();

    // Node IDs should be sequential
    assert_eq!(node2, node1 + 1);
    assert_eq!(node3, node2 + 1);

    commit(&mut tx).unwrap();
    close_graph_db(db).unwrap();
  }

  #[test]
  fn test_rollback_node_creation() {
    let temp_dir = tempdir().unwrap();
    let db = open_graph_db(temp_dir.path(), OpenOptions::new()).unwrap();

    let initial_next_id = db.peek_next_node_id();

    {
      let mut tx = begin_tx(&db).unwrap();
      let _node = create_node(&mut tx, NodeOpts::new()).unwrap();
      rollback(&mut tx).unwrap();
    }

    // After rollback, next ID should have still been consumed
    // (we don't reclaim IDs on rollback)
    assert!(db.peek_next_node_id() > initial_next_id);

    close_graph_db(db).unwrap();
  }
}
