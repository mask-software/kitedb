//! Read operations for SingleFileDB
//!
//! Handles all query operations: get properties, get edges, key lookups,
//! label checks, and neighbor traversal.

use std::collections::HashMap;

use crate::types::*;

use super::SingleFileDB;

impl SingleFileDB {
  // ========================================================================
  // Node Property Reads
  // ========================================================================

  /// Get all properties for a node
  ///
  /// Returns None if the node doesn't exist or is deleted.
  /// Merges properties from snapshot with delta modifications.
  pub fn get_node_props(&self, node_id: NodeId) -> Option<HashMap<PropKeyId, PropValue>> {
    let delta = self.delta.read();

    // Check if node is deleted
    if delta.is_node_deleted(node_id) {
      return None;
    }

    let mut props = HashMap::new();
    let snapshot = self.snapshot.read();

    // Get properties from snapshot first
    if let Some(ref snap) = *snapshot {
      if let Some(phys) = snap.get_phys_node(node_id) {
        if let Some(snapshot_props) = snap.get_node_props(phys) {
          props = snapshot_props;
        }
      }
    }

    // Apply delta modifications
    if let Some(node_delta) = delta.get_node_delta(node_id) {
      if let Some(ref delta_props) = node_delta.props {
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

    // Check if node exists at all
    let node_exists_in_delta =
      delta.is_node_created(node_id) || delta.get_node_delta(node_id).is_some();

    if !node_exists_in_delta {
      if let Some(ref snap) = *snapshot {
        snap.get_phys_node(node_id)?;
      } else {
        // No snapshot and node not in delta
        return None;
      }
    }

    Some(props)
  }

  /// Get a specific property for a node
  ///
  /// Returns None if the node doesn't exist, is deleted, or doesn't have the property.
  pub fn get_node_prop(&self, node_id: NodeId, key_id: PropKeyId) -> Option<PropValue> {
    let delta = self.delta.read();

    // Check if node is deleted
    if delta.is_node_deleted(node_id) {
      return None;
    }

    // Check delta first (for modifications)
    if let Some(node_delta) = delta.get_node_delta(node_id) {
      if let Some(ref delta_props) = node_delta.props {
        if let Some(value) = delta_props.get(&key_id) {
          // None means explicitly deleted
          return value.clone();
        }
      }
    }

    // Fall back to snapshot
    let snapshot = self.snapshot.read();
    if let Some(ref snap) = *snapshot {
      if let Some(phys) = snap.get_phys_node(node_id) {
        return snap.get_node_prop(phys, key_id);
      }
    }

    // Check if node exists at all (in delta as created)
    if delta.is_node_created(node_id) {
      // Node exists but doesn't have this property
      return None;
    }

    None
  }

  // ========================================================================
  // Edge Property Reads
  // ========================================================================

  /// Get all properties for an edge
  ///
  /// Returns None if the edge doesn't exist.
  /// Merges properties from snapshot with delta modifications.
  pub fn get_edge_props(
    &self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
  ) -> Option<HashMap<PropKeyId, PropValue>> {
    let delta = self.delta.read();

    // Check if either node is deleted
    if delta.is_node_deleted(src) || delta.is_node_deleted(dst) {
      return None;
    }

    // Check if edge is deleted in delta
    if delta.is_edge_deleted(src, etype, dst) {
      return None;
    }

    let mut props = HashMap::new();
    let snapshot = self.snapshot.read();

    // First, determine if edge exists
    let edge_added_in_delta = delta.is_edge_added(src, etype, dst);
    let mut edge_exists_in_snapshot = false;

    // Check snapshot for edge existence and get base properties
    if let Some(ref snap) = *snapshot {
      if let Some(src_phys) = snap.get_phys_node(src) {
        if let Some(dst_phys) = snap.get_phys_node(dst) {
          if let Some(edge_idx) = snap.find_edge_index(src_phys, etype, dst_phys) {
            edge_exists_in_snapshot = true;
            // Get properties from snapshot
            if let Some(snapshot_props) = snap.get_edge_props(edge_idx) {
              props = snapshot_props;
            }
          }
        }
      }
    }

    // Edge must exist either in delta or snapshot
    if !edge_added_in_delta && !edge_exists_in_snapshot {
      return None;
    }

    // Apply delta modifications (only if edge exists)
    if let Some(delta_props) = delta.get_edge_props_delta(src, etype, dst) {
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

    Some(props)
  }

  /// Get a specific property for an edge
  ///
  /// Returns None if the edge doesn't exist or doesn't have the property.
  pub fn get_edge_prop(
    &self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    key_id: PropKeyId,
  ) -> Option<PropValue> {
    let delta = self.delta.read();

    // Check if either node is deleted
    if delta.is_node_deleted(src) || delta.is_node_deleted(dst) {
      return None;
    }

    // Check if edge is deleted in delta
    if delta.is_edge_deleted(src, etype, dst) {
      return None;
    }

    // First, determine if edge exists at all
    let edge_added_in_delta = delta.is_edge_added(src, etype, dst);
    let snapshot = self.snapshot.read();
    let edge_exists_in_snapshot = if let Some(ref snap) = *snapshot {
      if let Some(src_phys) = snap.get_phys_node(src) {
        if let Some(dst_phys) = snap.get_phys_node(dst) {
          snap.find_edge_index(src_phys, etype, dst_phys).is_some()
        } else {
          false
        }
      } else {
        false
      }
    } else {
      false
    };

    // Edge must exist either in delta or snapshot
    if !edge_added_in_delta && !edge_exists_in_snapshot {
      return None;
    }

    // Check delta first (for modifications)
    if let Some(delta_props) = delta.get_edge_props_delta(src, etype, dst) {
      if let Some(value) = delta_props.get(&key_id) {
        // Some(None) means explicitly deleted
        return value.clone();
      }
    }

    // Fall back to snapshot
    if let Some(ref snap) = *snapshot {
      if let Some(src_phys) = snap.get_phys_node(src) {
        if let Some(dst_phys) = snap.get_phys_node(dst) {
          if let Some(edge_idx) = snap.find_edge_index(src_phys, etype, dst_phys) {
            // Get property from snapshot
            if let Some(snapshot_props) = snap.get_edge_props(edge_idx) {
              if let Some(value) = snapshot_props.get(&key_id) {
                return Some(value.clone());
              }
            }
          }
        }
      }
    }

    None
  }

  // ========================================================================
  // Edge Traversal
  // ========================================================================

  /// Get outgoing edges for a node
  ///
  /// Returns edges as (edge_type_id, destination_node_id) pairs.
  /// Merges edges from snapshot with delta additions/deletions.
  /// Filters out edges to deleted nodes.
  pub fn get_out_edges(&self, node_id: NodeId) -> Vec<(ETypeId, NodeId)> {
    let delta = self.delta.read();

    // If node is deleted, no edges
    if delta.is_node_deleted(node_id) {
      return Vec::new();
    }

    let mut edges = Vec::new();
    let snapshot = self.snapshot.read();

    // Get edges from snapshot
    if let Some(ref snap) = *snapshot {
      if let Some(phys) = snap.get_phys_node(node_id) {
        for (dst_phys, etype) in snap.iter_out_edges(phys) {
          // Convert physical dst to NodeId
          if let Some(dst_node_id) = snap.get_node_id(dst_phys) {
            // Skip edges to deleted nodes
            if delta.is_node_deleted(dst_node_id) {
              continue;
            }
            // Skip edges deleted in delta
            if delta.is_edge_deleted(node_id, etype, dst_node_id) {
              continue;
            }
            edges.push((etype, dst_node_id));
          }
        }
      }
    }

    // Add edges from delta
    if let Some(added_edges) = delta.out_add.get(&node_id) {
      for edge_patch in added_edges {
        // Skip edges to deleted nodes
        if delta.is_node_deleted(edge_patch.other) {
          continue;
        }
        edges.push((edge_patch.etype, edge_patch.other));
      }
    }

    // Sort by (etype, dst) for consistent ordering
    edges.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    edges
  }

  /// Get incoming edges for a node
  ///
  /// Returns edges as (edge_type_id, source_node_id) pairs.
  /// Merges edges from snapshot with delta additions/deletions.
  /// Filters out edges from deleted nodes.
  pub fn get_in_edges(&self, node_id: NodeId) -> Vec<(ETypeId, NodeId)> {
    let delta = self.delta.read();

    // If node is deleted, no edges
    if delta.is_node_deleted(node_id) {
      return Vec::new();
    }

    let mut edges = Vec::new();
    let snapshot = self.snapshot.read();

    // Get edges from snapshot
    if let Some(ref snap) = *snapshot {
      if let Some(phys) = snap.get_phys_node(node_id) {
        for (src_phys, etype, _out_index) in snap.iter_in_edges(phys) {
          // Convert physical src to NodeId
          if let Some(src_node_id) = snap.get_node_id(src_phys) {
            // Skip edges from deleted nodes
            if delta.is_node_deleted(src_node_id) {
              continue;
            }
            // Skip edges deleted in delta
            if delta.is_edge_deleted(src_node_id, etype, node_id) {
              continue;
            }
            edges.push((etype, src_node_id));
          }
        }
      }
    }

    // Add edges from delta (in_add stores patches where other=src)
    if let Some(added_edges) = delta.in_add.get(&node_id) {
      for edge_patch in added_edges {
        // Skip edges from deleted nodes
        if delta.is_node_deleted(edge_patch.other) {
          continue;
        }
        edges.push((edge_patch.etype, edge_patch.other));
      }
    }

    // Sort by (etype, src) for consistent ordering
    edges.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    edges
  }

  /// Get out-degree (number of outgoing edges) for a node
  pub fn get_out_degree(&self, node_id: NodeId) -> usize {
    self.get_out_edges(node_id).len()
  }

  /// Get in-degree (number of incoming edges) for a node
  pub fn get_in_degree(&self, node_id: NodeId) -> usize {
    self.get_in_edges(node_id).len()
  }

  /// Get neighbors via outgoing edges of a specific type
  ///
  /// Returns destination node IDs for edges of the given type.
  pub fn get_out_neighbors(&self, node_id: NodeId, etype: ETypeId) -> Vec<NodeId> {
    self
      .get_out_edges(node_id)
      .into_iter()
      .filter(|(e, _)| *e == etype)
      .map(|(_, dst)| dst)
      .collect()
  }

  /// Get neighbors via incoming edges of a specific type
  ///
  /// Returns source node IDs for edges of the given type.
  pub fn get_in_neighbors(&self, node_id: NodeId, etype: ETypeId) -> Vec<NodeId> {
    self
      .get_in_edges(node_id)
      .into_iter()
      .filter(|(e, _)| *e == etype)
      .map(|(_, src)| src)
      .collect()
  }

  /// Check if there are any outgoing edges of a specific type
  pub fn has_out_edges(&self, node_id: NodeId, etype: ETypeId) -> bool {
    self.get_out_edges(node_id).iter().any(|(e, _)| *e == etype)
  }

  /// Check if there are any incoming edges of a specific type
  pub fn has_in_edges(&self, node_id: NodeId, etype: ETypeId) -> bool {
    self.get_in_edges(node_id).iter().any(|(e, _)| *e == etype)
  }

  // ========================================================================
  // Node Label Reads
  // ========================================================================

  /// Check if a node has a specific label
  pub fn node_has_label(&self, node_id: NodeId, label_id: LabelId) -> bool {
    let delta = self.delta.read();

    // Check if node is deleted
    if delta.is_node_deleted(node_id) {
      return false;
    }

    // Check if label was removed in delta
    if delta.is_label_removed(node_id, label_id) {
      return false;
    }

    // Check if label was added in delta
    if delta.is_label_added(node_id, label_id) {
      return true;
    }

    // Check snapshot for label (if present)
    if let Some(ref snapshot) = *self.snapshot.read() {
      if let Some(phys) = snapshot.get_phys_node(node_id) {
        if let Some(labels) = snapshot.get_node_labels(phys) {
          return labels.contains(&label_id);
        }
      }
    }

    false
  }

  /// Get all labels for a node
  pub fn get_node_labels(&self, node_id: NodeId) -> Vec<LabelId> {
    let delta = self.delta.read();

    // Check if node is deleted
    if delta.is_node_deleted(node_id) {
      return Vec::new();
    }

    let mut labels = std::collections::HashSet::new();

    // Load labels from snapshot first (if present)
    if let Some(ref snapshot) = *self.snapshot.read() {
      if let Some(phys) = snapshot.get_phys_node(node_id) {
        if let Some(snapshot_labels) = snapshot.get_node_labels(phys) {
          labels.extend(snapshot_labels);
        }
      }
    }

    // Add labels from delta
    if let Some(added) = delta.get_added_labels(node_id) {
      labels.extend(added.iter().copied());
    }

    // Remove labels deleted in delta
    if let Some(removed) = delta.get_removed_labels(node_id) {
      for &label_id in removed {
        labels.remove(&label_id);
      }
    }

    let mut result: Vec<_> = labels.into_iter().collect();
    result.sort_unstable();
    result
  }

  // ========================================================================
  // Key Lookups
  // ========================================================================

  /// Look up a node by its key
  ///
  /// Returns the NodeId if found, None otherwise.
  /// Checks delta key index first, then falls back to snapshot.
  pub fn get_node_by_key(&self, key: &str) -> Option<NodeId> {
    let delta = self.delta.read();

    // Check delta key index first
    if delta.key_index_deleted.contains(key) {
      return None;
    }

    if let Some(&node_id) = delta.key_index.get(key) {
      // Verify node isn't deleted
      if !delta.is_node_deleted(node_id) {
        return Some(node_id);
      }
    }

    // Fall back to snapshot
    let snapshot = self.snapshot.read();
    if let Some(ref snap) = *snapshot {
      if let Some(node_id) = snap.lookup_by_key(key) {
        // Verify node isn't deleted in delta
        if !delta.is_node_deleted(node_id) {
          return Some(node_id);
        }
      }
    }

    None
  }

  /// Get the key for a node
  ///
  /// Returns the key string if the node has one, None otherwise.
  pub fn get_node_key(&self, node_id: NodeId) -> Option<String> {
    let delta = self.delta.read();

    // Check if node is deleted
    if delta.is_node_deleted(node_id) {
      return None;
    }

    // Check created nodes in delta first
    if let Some(node_delta) = delta.created_nodes.get(&node_id) {
      return node_delta.key.clone();
    }

    // Fall back to snapshot
    let snapshot = self.snapshot.read();
    if let Some(ref snap) = *snapshot {
      if let Some(phys) = snap.get_phys_node(node_id) {
        return snap.get_node_key(phys);
      }
    }

    None
  }
}
