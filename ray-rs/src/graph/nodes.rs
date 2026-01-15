//! Node CRUD operations
//!
//! Provides functions for creating, deleting, and querying nodes.

use crate::core::wal::record::*;
use crate::error::{RayError, Result};
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
    let node_id = handle.db.alloc_node_id();

    // Build CREATE_NODE WAL record
    let payload = build_create_node_payload(node_id, opts.key.as_deref());
    handle.add_record(WalRecord::new(
        WalRecordType::CreateNode,
        handle.txid(),
        payload,
    ))?;

    // Add property records if any
    if let Some(props) = opts.props {
        for (key_id, value) in props {
            let payload = build_set_node_prop_payload(node_id, key_id, &value);
            handle.add_record(WalRecord::new(
                WalRecordType::SetNodeProp,
                handle.txid(),
                payload,
            ))?;
        }
    }

    Ok(node_id)
}

/// Delete a node
pub fn delete_node(handle: &mut TxHandle, node_id: NodeId) -> Result<bool> {
    // Check if node exists
    if !node_exists_internal(handle.db, node_id) {
        return Ok(false);
    }

    // Build DELETE_NODE WAL record
    let payload = build_delete_node_payload(node_id);
    handle.add_record(WalRecord::new(
        WalRecordType::DeleteNode,
        handle.txid(),
        payload,
    ))?;

    Ok(true)
}

/// Check if a node exists
pub fn node_exists(handle: &TxHandle, node_id: NodeId) -> bool {
    // Check delta first
    let delta = handle.db.delta.read();
    
    if delta.is_node_deleted(node_id) {
        return false;
    }
    
    if delta.is_node_created(node_id) {
        return true;
    }

    // Check snapshot
    if let Some(ref snapshot) = handle.db.snapshot {
        return snapshot.has_node(node_id);
    }

    false
}

/// Internal node existence check (on GraphDB directly)
fn node_exists_internal(db: &super::db::GraphDB, node_id: NodeId) -> bool {
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

/// Set a node property
pub fn set_node_prop(
    handle: &mut TxHandle,
    node_id: NodeId,
    key_id: PropKeyId,
    value: PropValue,
) -> Result<()> {
    let payload = build_set_node_prop_payload(node_id, key_id, &value);
    handle.add_record(WalRecord::new(
        WalRecordType::SetNodeProp,
        handle.txid(),
        payload,
    ))
}

/// Delete a node property
pub fn del_node_prop(
    handle: &mut TxHandle,
    node_id: NodeId,
    key_id: PropKeyId,
) -> Result<()> {
    let payload = build_del_node_prop_payload(node_id, key_id);
    handle.add_record(WalRecord::new(
        WalRecordType::DelNodeProp,
        handle.txid(),
        payload,
    ))
}

/// Get a node property
pub fn get_node_prop(handle: &TxHandle, node_id: NodeId, key_id: PropKeyId) -> Option<PropValue> {
    // Check delta first
    let delta = handle.db.delta.read();
    
    // Check if node is deleted
    if delta.is_node_deleted(node_id) {
        return None;
    }

    // Check delta for property (Some(Some(v)) = set, Some(None) = deleted)
    if let Some(value_opt) = delta.get_node_prop(node_id, key_id) {
        // If explicitly set or deleted in delta, return that
        return value_opt.cloned();
    }

    // Check snapshot
    if let Some(ref snapshot) = handle.db.snapshot {
        // Get physical node index
        if let Some(phys) = snapshot.get_phys_node(node_id) {
            return snapshot.get_node_prop(phys, key_id);
        }
    }

    None
}

/// Get a node by its key
pub fn get_node_by_key(handle: &TxHandle, key: &str) -> Option<NodeId> {
    // Check delta first
    let delta = handle.db.delta.read();
    
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
    if let Some(ref snapshot) = handle.db.snapshot {
        if let Some(node_id) = snapshot.lookup_by_key(key) {
            // Make sure node wasn't deleted in delta
            if !delta.is_node_deleted(node_id) {
                return Some(node_id);
            }
        }
    }

    None
}

/// Count total nodes in the database
pub fn count_nodes(handle: &TxHandle) -> u64 {
    let delta = handle.db.delta.read();
    
    // Start with snapshot count
    let snapshot_count = handle.db.snapshot
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

    snapshot_count + created - deleted_from_snapshot
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::db::{open_graph_db, close_graph_db, OpenOptions};
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
        let node_id = create_node(
            &mut tx, 
            NodeOpts::new().with_key("alice")
        ).unwrap();
        
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
