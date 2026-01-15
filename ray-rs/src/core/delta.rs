//! In-memory delta overlay for uncommitted changes
//!
//! Ported from src/core/delta.ts

use crate::types::*;

impl DeltaState {
    /// Create empty delta state
    pub fn new() -> Self {
        Self::default()
    }

    /// Add edge with cancellation logic
    pub fn add_edge(&mut self, src: NodeId, etype: ETypeId, dst: NodeId) {
        let patch = EdgePatch { etype, other: dst };

        // Check if cancels a pending delete
        if let Some(del_set) = self.out_del.get_mut(&src) {
            if del_set.remove(&patch) {
                if del_set.is_empty() {
                    self.out_del.remove(&src);
                }
            } else {
                self.out_add.entry(src).or_default().insert(patch);
            }
        } else {
            self.out_add.entry(src).or_default().insert(patch);
        }

        // Same for in-edges
        let in_patch = EdgePatch { etype, other: src };
        if let Some(del_set) = self.in_del.get_mut(&dst) {
            if del_set.remove(&in_patch) {
                if del_set.is_empty() {
                    self.in_del.remove(&dst);
                }
            } else {
                self.in_add.entry(dst).or_default().insert(in_patch);
            }
        } else {
            self.in_add.entry(dst).or_default().insert(in_patch);
        }

        // Track reverse index for O(k) cleanup on node deletion
        self.incoming_edge_sources
            .entry(dst)
            .or_default()
            .insert(src);
    }

    /// Delete edge with cancellation logic
    pub fn delete_edge(&mut self, src: NodeId, etype: ETypeId, dst: NodeId) {
        let patch = EdgePatch { etype, other: dst };

        // Check if cancels a pending add
        if let Some(add_set) = self.out_add.get_mut(&src) {
            if add_set.remove(&patch) {
                if add_set.is_empty() {
                    self.out_add.remove(&src);
                }
                // Also remove from in_add
                if let Some(in_add_set) = self.in_add.get_mut(&dst) {
                    let in_patch = EdgePatch { etype, other: src };
                    in_add_set.remove(&in_patch);
                    if in_add_set.is_empty() {
                        self.in_add.remove(&dst);
                    }
                }
                return;
            }
        }

        // Add to delete sets
        self.out_del.entry(src).or_default().insert(patch);
        let in_patch = EdgePatch { etype, other: src };
        self.in_del.entry(dst).or_default().insert(in_patch);
    }

    /// Check if edge is deleted in delta
    pub fn is_edge_deleted(&self, src: NodeId, etype: ETypeId, dst: NodeId) -> bool {
        self.out_del
            .get(&src)
            .map(|s| s.contains(&EdgePatch { etype, other: dst }))
            .unwrap_or(false)
    }

    /// Check if edge is added in delta
    pub fn is_edge_added(&self, src: NodeId, etype: ETypeId, dst: NodeId) -> bool {
        self.out_add
            .get(&src)
            .map(|s| s.contains(&EdgePatch { etype, other: dst }))
            .unwrap_or(false)
    }

    /// Clear all delta state
    pub fn clear(&mut self) {
        self.created_nodes.clear();
        self.deleted_nodes.clear();
        self.modified_nodes.clear();
        self.out_add.clear();
        self.out_del.clear();
        self.in_add.clear();
        self.in_del.clear();
        self.edge_props.clear();
        self.new_labels.clear();
        self.new_etypes.clear();
        self.new_propkeys.clear();
        self.key_index.clear();
        self.key_index_deleted.clear();
        self.incoming_edge_sources.clear();
    }

    /// Get count of edges added for a source node
    pub fn edges_added_count(&self, src: NodeId) -> usize {
        self.out_add.get(&src).map(|s| s.len()).unwrap_or(0)
    }

    /// Get count of edges deleted for a source node
    pub fn edges_deleted_count(&self, src: NodeId) -> usize {
        self.out_del.get(&src).map(|s| s.len()).unwrap_or(0)
    }

    /// Total edges added across all nodes
    pub fn total_edges_added(&self) -> usize {
        self.out_add.values().map(|s| s.len()).sum()
    }

    /// Total edges deleted across all nodes
    pub fn total_edges_deleted(&self) -> usize {
        self.out_del.values().map(|s| s.len()).sum()
    }

    // ========================================================================
    // Node Operations
    // ========================================================================

    /// Create a new node
    pub fn create_node(&mut self, node_id: NodeId, key: Option<&str>) {
        let node_delta = NodeDelta {
            key: key.map(|s| s.to_string()),
            labels: None,
            labels_deleted: None,
            props: None,
        };
        self.created_nodes.insert(node_id, node_delta);
        
        // Add to key index if key provided
        if let Some(k) = key {
            self.key_index.insert(k.to_string(), node_id);
        }
    }

    /// Delete a node
    pub fn delete_node(&mut self, node_id: NodeId) {
        // If it was just created in this delta, remove it instead
        if let Some(removed) = self.created_nodes.remove(&node_id) {
            // Remove from key index
            if let Some(key) = &removed.key {
                self.key_index.remove(key);
            }
            
            // Clean up outgoing edges from this node
            self.out_add.remove(&node_id);
            
            // Clean up incoming edges to this node
            // We need to remove edges where this node is the destination
            if let Some(sources) = self.incoming_edge_sources.remove(&node_id) {
                for src in sources {
                    if let Some(patches) = self.out_add.get_mut(&src) {
                        patches.retain(|p| p.other != node_id);
                        if patches.is_empty() {
                            self.out_add.remove(&src);
                        }
                    }
                }
            }
            
            // Clean up in_add entries
            self.in_add.remove(&node_id);
            for (_, patches) in self.in_add.iter_mut() {
                patches.retain(|p| p.other != node_id);
            }
            self.in_add.retain(|_, patches| !patches.is_empty());
            
            return;
        }
        
        // Mark as deleted
        self.deleted_nodes.insert(node_id);
        
        // Remove any modified state
        self.modified_nodes.remove(&node_id);
    }

    /// Check if node was created in delta
    pub fn is_node_created(&self, node_id: NodeId) -> bool {
        self.created_nodes.contains_key(&node_id)
    }

    /// Check if node was deleted in delta
    pub fn is_node_deleted(&self, node_id: NodeId) -> bool {
        self.deleted_nodes.contains(&node_id)
    }

    /// Get node delta (for created or modified nodes)
    pub fn get_node_delta(&self, node_id: NodeId) -> Option<&NodeDelta> {
        self.created_nodes.get(&node_id)
            .or_else(|| self.modified_nodes.get(&node_id))
    }

    // ========================================================================
    // Node Property Operations
    // ========================================================================

    /// Set a node property
    pub fn set_node_prop(&mut self, node_id: NodeId, key_id: PropKeyId, value: PropValue) {
        // Get or create the node delta
        let node_delta = if self.created_nodes.contains_key(&node_id) {
            self.created_nodes.get_mut(&node_id).unwrap()
        } else {
            self.modified_nodes.entry(node_id).or_insert_with(|| NodeDelta {
                key: None,
                labels: None,
                labels_deleted: None,
                props: None,
            })
        };

        // Initialize props map if needed
        if node_delta.props.is_none() {
            node_delta.props = Some(std::collections::HashMap::new());
        }

        node_delta.props.as_mut().unwrap().insert(key_id, Some(value));
    }

    /// Delete a node property
    pub fn delete_node_prop(&mut self, node_id: NodeId, key_id: PropKeyId) {
        let node_delta = if self.created_nodes.contains_key(&node_id) {
            self.created_nodes.get_mut(&node_id).unwrap()
        } else {
            self.modified_nodes.entry(node_id).or_insert_with(|| NodeDelta {
                key: None,
                labels: None,
                labels_deleted: None,
                props: None,
            })
        };

        if node_delta.props.is_none() {
            node_delta.props = Some(std::collections::HashMap::new());
        }

        // None value means deleted
        node_delta.props.as_mut().unwrap().insert(key_id, None);
    }

    /// Get a node property from delta
    pub fn get_node_prop(&self, node_id: NodeId, key_id: PropKeyId) -> Option<Option<&PropValue>> {
        let node_delta = self.created_nodes.get(&node_id)
            .or_else(|| self.modified_nodes.get(&node_id))?;
        
        let props = node_delta.props.as_ref()?;
        props.get(&key_id).map(|v| v.as_ref())
    }

    // ========================================================================
    // Definition Operations
    // ========================================================================

    /// Define a new label
    pub fn define_label(&mut self, label_id: LabelId, name: &str) {
        self.new_labels.insert(label_id, name.to_string());
    }

    /// Define a new edge type
    pub fn define_etype(&mut self, etype_id: ETypeId, name: &str) {
        self.new_etypes.insert(etype_id, name.to_string());
    }

    /// Define a new property key
    pub fn define_propkey(&mut self, propkey_id: PropKeyId, name: &str) {
        self.new_propkeys.insert(propkey_id, name.to_string());
    }

    // ========================================================================
    // Key Index Operations
    // ========================================================================

    /// Lookup node by key in delta
    pub fn get_node_by_key(&self, key: &str) -> Option<NodeId> {
        // Check if key was deleted
        if self.key_index_deleted.contains(key) {
            return None;
        }
        self.key_index.get(key).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_edge() {
        let mut delta = DeltaState::new();
        delta.add_edge(1, 10, 2);

        assert!(delta.is_edge_added(1, 10, 2));
        assert!(!delta.is_edge_deleted(1, 10, 2));
    }

    #[test]
    fn test_delete_edge() {
        let mut delta = DeltaState::new();
        delta.delete_edge(1, 10, 2);

        assert!(delta.is_edge_deleted(1, 10, 2));
        assert!(!delta.is_edge_added(1, 10, 2));
    }

    #[test]
    fn test_add_cancels_delete() {
        let mut delta = DeltaState::new();
        delta.delete_edge(1, 10, 2);
        assert!(delta.is_edge_deleted(1, 10, 2));

        delta.add_edge(1, 10, 2);
        assert!(!delta.is_edge_deleted(1, 10, 2));
        assert!(!delta.is_edge_added(1, 10, 2)); // Cancellation
    }

    #[test]
    fn test_delete_cancels_add() {
        let mut delta = DeltaState::new();
        delta.add_edge(1, 10, 2);
        assert!(delta.is_edge_added(1, 10, 2));

        delta.delete_edge(1, 10, 2);
        assert!(!delta.is_edge_added(1, 10, 2));
        assert!(!delta.is_edge_deleted(1, 10, 2)); // Cancellation
    }
}
