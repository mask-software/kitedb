//! In-memory delta overlay for uncommitted changes
//!
//! Ported from src/core/delta.ts

use crate::core::snapshot::reader::SnapshotData;
use crate::types::*;
use std::collections::HashMap;

/// Whether `snapshot` holds the edge `src -[etype]-> dst`.
pub fn snapshot_has_edge(
  snapshot: Option<&SnapshotData>,
  src: NodeId,
  etype: ETypeId,
  dst: NodeId,
) -> bool {
  let Some(snap) = snapshot else {
    return false;
  };
  match (snap.phys_node(src), snap.phys_node(dst)) {
    (Some(src_phys), Some(dst_phys)) => snap.has_edge(src_phys, etype, dst_phys),
    _ => false,
  }
}

impl DeltaState {
  /// Create empty delta state
  pub fn new() -> Self {
    Self::default()
  }

  // ========================================================================
  // Layered Edge Operations
  // ========================================================================
  //
  // A delta overlays a base layer (the snapshot, or for a transaction the
  // committed state). `in_base` says whether the base holds the edge. These
  // keep `out_add` disjoint from the base and `out_del` inside it, so a
  // re-added base edge is never counted twice and deleting a base edge always
  // hides it.

  /// Whether the edge is visible through this delta.
  pub fn edge_visible(&self, src: NodeId, etype: ETypeId, dst: NodeId, in_base: bool) -> bool {
    self.is_edge_added(src, etype, dst) || (in_base && !self.is_edge_deleted(src, etype, dst))
  }

  /// Add an edge unless it is already visible.
  pub fn add_edge_over(&mut self, src: NodeId, etype: ETypeId, dst: NodeId, in_base: bool) {
    if self.edge_visible(src, etype, dst, in_base) {
      return;
    }
    if !in_base {
      // A tombstone for an edge the base lacks must not swallow the add.
      self.remove_edge_patch(src, etype, dst, false);
    }
    self.add_edge(src, etype, dst);
  }

  /// Delete an edge if it is visible. A base edge always gets a tombstone,
  /// even when the delta also held an add patch for it. The edge's props in
  /// this delta go with it.
  pub fn delete_edge_over(&mut self, src: NodeId, etype: ETypeId, dst: NodeId, in_base: bool) {
    if !self.edge_visible(src, etype, dst, in_base) {
      return;
    }
    self.edge_props.remove(&(src, etype, dst));
    self.remove_edge_patch(src, etype, dst, true);
    if in_base {
      self
        .out_del
        .entry(src)
        .or_default()
        .insert(EdgePatch { etype, other: dst });
      self
        .in_del
        .entry(dst)
        .or_default()
        .insert(EdgePatch { etype, other: src });
    }
  }

  /// Remove an add patch (`added`) or a tombstone (`!added`) in both directions.
  fn remove_edge_patch(&mut self, src: NodeId, etype: ETypeId, dst: NodeId, added: bool) {
    let (out_map, in_map) = if added {
      (&mut self.out_add, &mut self.in_add)
    } else {
      (&mut self.out_del, &mut self.in_del)
    };
    if let Some(set) = out_map.get_mut(&src) {
      if set.remove(&EdgePatch { etype, other: dst }) && set.is_empty() {
        out_map.remove(&src);
      }
    }
    if let Some(set) = in_map.get_mut(&dst) {
      if set.remove(&EdgePatch { etype, other: src }) && set.is_empty() {
        in_map.remove(&dst);
      }
    }
  }

  /// Whether `node_id` exists through this delta over `snapshot`.
  pub fn node_exists_over(&self, snapshot: Option<&SnapshotData>, node_id: NodeId) -> bool {
    if self.is_node_created(node_id) {
      return true;
    }
    !self.is_node_deleted(node_id) && snapshot.is_some_and(|snap| snap.has_node(node_id))
  }

  /// Whether `snapshot` holds the edge and this delta keeps the snapshot
  /// copies of both endpoints: a deleted or recreated endpoint masks the
  /// snapshot's edges. Edge tombstones are not applied here.
  pub fn snapshot_edge_over(
    &self,
    snapshot: Option<&SnapshotData>,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
  ) -> bool {
    !self.is_node_deleted(src)
      && !self.is_node_deleted(dst)
      && snapshot_has_edge(snapshot, src, etype, dst)
  }

  /// Whether the edge is visible through this delta over `snapshot`.
  pub fn edge_exists_over(
    &self,
    snapshot: Option<&SnapshotData>,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
  ) -> bool {
    if self.is_node_removed(src) || self.is_node_removed(dst) {
      return false;
    }
    if self.is_edge_added(src, etype, dst) {
      return true;
    }
    !self.is_edge_deleted(src, etype, dst) && self.snapshot_edge_over(snapshot, src, etype, dst)
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
  }

  /// Delete edge with cancellation logic. The edge's props in this delta go
  /// with it: a later add of the same triple is a new edge.
  pub fn delete_edge(&mut self, src: NodeId, etype: ETypeId, dst: NodeId) {
    self.edge_props.remove(&(src, etype, dst));
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
    self
      .out_del
      .get(&src)
      .map(|s| s.contains(&EdgePatch { etype, other: dst }))
      .unwrap_or(false)
  }

  /// Check if edge is added in delta
  pub fn is_edge_added(&self, src: NodeId, etype: ETypeId, dst: NodeId) -> bool {
    self
      .out_add
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
    self.pending_vectors.clear();
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

  /// Create a new node.
  ///
  /// Over a deleted id this recreates the node: the delete stays and keeps
  /// masking the base copy (its props, labels, key and edges), and the node
  /// starts fresh, without the edge patches or edge props this delta held
  /// for the old copy. Props of the old copy's base edges have no patch to
  /// find them by, so this scans `edge_props`; recreating an id is rare.
  pub fn create_node(&mut self, node_id: NodeId, key: Option<&str>) {
    if self.is_node_deleted(node_id) {
      self.modified_nodes.remove(&node_id);
      self.drop_edge_patches(node_id);
      self
        .edge_props
        .retain(|&(src, _, dst), _| src != node_id && dst != node_id);
    }
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
    // If it was created in this delta, remove it instead. A recreated node
    // keeps the delete that masks its base copy.
    if let Some(removed) = self.created_nodes.remove(&node_id) {
      // Remove from key index
      if let Some(key) = &removed.key {
        self.key_index.remove(key);
      }

      // State written to the node after it was created here (older versions
      // let writes to a deleted node through), and its edges, found through
      // its own patch sets: O(its degree).
      self.modified_nodes.remove(&node_id);
      self.drop_edge_patches(node_id);

      return;
    }

    // Mark as deleted
    self.deleted_nodes.insert(node_id);

    // Remove any modified state, and this delta's patches of its edges: the
    // delete masks the base copies, and an add patch would outlive the node.
    self.modified_nodes.remove(&node_id);
    self.drop_edge_patches(node_id);
  }

  /// Whether this delta holds its own copy of the node: created here, or
  /// recreated over a deleted base copy. Its state is the delta's alone.
  pub fn is_node_created(&self, node_id: NodeId) -> bool {
    self.created_nodes.contains_key(&node_id)
  }

  /// Whether this delta deleted the node's base copy, masking its props,
  /// labels, key and edges. True for a recreated node as well: check
  /// [`Self::is_node_removed`] for whether the node is gone.
  pub fn is_node_deleted(&self, node_id: NodeId) -> bool {
    self.deleted_nodes.contains(&node_id)
  }

  /// Whether the node is gone through this delta: deleted, and not created
  /// again.
  pub fn is_node_removed(&self, node_id: NodeId) -> bool {
    self.is_node_deleted(node_id) && !self.is_node_created(node_id)
  }

  /// Drop every edge patch (add or tombstone, both directions) incident to
  /// `node_id`, with the props of those edges. Patches are kept in both
  /// directions, so the node's own `out_*` and `in_*` sets name every one of
  /// them.
  fn drop_edge_patches(&mut self, node_id: NodeId) {
    for added in [true, false] {
      let (out_map, in_map) = if added {
        (&self.out_add, &self.in_add)
      } else {
        (&self.out_del, &self.in_del)
      };
      let out_edges = out_map
        .get(&node_id)
        .into_iter()
        .flatten()
        .map(|patch| (node_id, patch.etype, patch.other));
      let in_edges = in_map
        .get(&node_id)
        .into_iter()
        .flatten()
        .map(|patch| (patch.other, patch.etype, node_id));
      let edges: Vec<_> = out_edges.chain(in_edges).collect();
      for (src, etype, dst) in edges {
        self.remove_edge_patch(src, etype, dst, added);
        self.edge_props.remove(&(src, etype, dst));
      }
    }
  }

  /// Get node delta (for created or modified nodes)
  pub fn node_delta(&self, node_id: NodeId) -> Option<&NodeDelta> {
    self
      .created_nodes
      .get(&node_id)
      .or_else(|| self.modified_nodes.get(&node_id))
  }

  // ========================================================================
  // Node Property Operations
  // ========================================================================

  /// Set a node property
  pub fn set_node_prop(&mut self, node_id: NodeId, key_id: PropKeyId, value: PropValue) {
    self.set_node_prop_ref(node_id, key_id, std::sync::Arc::new(value));
  }

  /// Set a node property using a shared value
  pub fn set_node_prop_ref(&mut self, node_id: NodeId, key_id: PropKeyId, value: PropValueRef) {
    // Get or create the node delta
    let node_delta = if let Some(node_delta) = self.created_nodes.get_mut(&node_id) {
      node_delta
    } else {
      self
        .modified_nodes
        .entry(node_id)
        .or_insert_with(|| NodeDelta {
          key: None,
          labels: None,
          labels_deleted: None,
          props: None,
        })
    };

    let props = node_delta
      .props
      .get_or_insert_with(std::collections::HashMap::new);
    props.insert(key_id, Some(value));
  }

  /// Delete a node property
  pub fn delete_node_prop(&mut self, node_id: NodeId, key_id: PropKeyId) {
    let node_delta = if let Some(node_delta) = self.created_nodes.get_mut(&node_id) {
      node_delta
    } else {
      self
        .modified_nodes
        .entry(node_id)
        .or_insert_with(|| NodeDelta {
          key: None,
          labels: None,
          labels_deleted: None,
          props: None,
        })
    };

    let props = node_delta
      .props
      .get_or_insert_with(std::collections::HashMap::new);
    // None value means deleted
    props.insert(key_id, None);
  }

  /// Get a node property from delta
  pub fn node_prop(&self, node_id: NodeId, key_id: PropKeyId) -> Option<Option<&PropValue>> {
    let node_delta = self
      .created_nodes
      .get(&node_id)
      .or_else(|| self.modified_nodes.get(&node_id))?;

    let props = node_delta.props.as_ref()?;
    props.get(&key_id).map(|v| v.as_deref())
  }

  // ========================================================================
  // Node Label Operations
  // ========================================================================

  /// Add a label to a node
  pub fn add_node_label(&mut self, node_id: NodeId, label_id: LabelId) {
    let node_delta = if let Some(node_delta) = self.created_nodes.get_mut(&node_id) {
      node_delta
    } else {
      self
        .modified_nodes
        .entry(node_id)
        .or_insert_with(|| NodeDelta {
          key: None,
          labels: None,
          labels_deleted: None,
          props: None,
        })
    };

    // Remove from deleted set if present
    if let Some(ref mut deleted) = node_delta.labels_deleted {
      deleted.remove(&label_id);
    }

    // Add to labels set
    let labels = node_delta
      .labels
      .get_or_insert_with(std::collections::HashSet::new);
    labels.insert(label_id);
  }

  /// Remove a label from a node
  pub fn remove_node_label(&mut self, node_id: NodeId, label_id: LabelId) {
    let is_created = self.created_nodes.contains_key(&node_id);

    let node_delta = if let Some(node_delta) = self.created_nodes.get_mut(&node_id) {
      node_delta
    } else {
      self
        .modified_nodes
        .entry(node_id)
        .or_insert_with(|| NodeDelta {
          key: None,
          labels: None,
          labels_deleted: None,
          props: None,
        })
    };

    // Remove from added labels if present
    if let Some(ref mut labels) = node_delta.labels {
      labels.remove(&label_id);
    }

    // If not a new node, mark as deleted
    if !is_created {
      let deleted = node_delta
        .labels_deleted
        .get_or_insert_with(std::collections::HashSet::new);
      deleted.insert(label_id);
    }
  }

  /// Check if a label was added to a node in delta
  pub fn is_label_added(&self, node_id: NodeId, label_id: LabelId) -> bool {
    if let Some(node_delta) = self
      .created_nodes
      .get(&node_id)
      .or_else(|| self.modified_nodes.get(&node_id))
    {
      if let Some(ref labels) = node_delta.labels {
        return labels.contains(&label_id);
      }
    }
    false
  }

  /// Check if a label was removed from a node in delta
  pub fn is_label_removed(&self, node_id: NodeId, label_id: LabelId) -> bool {
    if let Some(node_delta) = self.modified_nodes.get(&node_id) {
      if let Some(ref deleted) = node_delta.labels_deleted {
        return deleted.contains(&label_id);
      }
    }
    false
  }

  /// Get labels added in delta for a node
  pub fn added_labels(&self, node_id: NodeId) -> Option<&std::collections::HashSet<LabelId>> {
    self
      .created_nodes
      .get(&node_id)
      .or_else(|| self.modified_nodes.get(&node_id))
      .and_then(|d| d.labels.as_ref())
  }

  /// Get labels removed in delta for a node
  pub fn removed_labels(&self, node_id: NodeId) -> Option<&std::collections::HashSet<LabelId>> {
    self
      .modified_nodes
      .get(&node_id)
      .and_then(|d| d.labels_deleted.as_ref())
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
  // Edge Property Operations
  // ========================================================================

  /// Set an edge property
  pub fn set_edge_prop(
    &mut self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    key_id: PropKeyId,
    value: PropValue,
  ) {
    self.set_edge_prop_ref(src, etype, dst, key_id, std::sync::Arc::new(value));
  }

  /// Set an edge property using a shared value
  pub fn set_edge_prop_ref(
    &mut self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    key_id: PropKeyId,
    value: PropValueRef,
  ) {
    let edge_key = (src, etype, dst);
    let props = self.edge_props.entry(edge_key).or_default();
    props.insert(key_id, Some(value));
  }

  /// Delete an edge property
  pub fn delete_edge_prop(&mut self, src: NodeId, etype: ETypeId, dst: NodeId, key_id: PropKeyId) {
    let edge_key = (src, etype, dst);
    let props = self.edge_props.entry(edge_key).or_default();
    props.insert(key_id, None);
  }

  /// Get an edge property from delta
  /// Returns Some(Some(value)) if set, Some(None) if deleted, None if not in delta
  pub fn edge_prop(
    &self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    key_id: PropKeyId,
  ) -> Option<Option<&PropValue>> {
    let edge_key = (src, etype, dst);
    self
      .edge_props
      .get(&edge_key)
      .and_then(|props| props.get(&key_id))
      .map(|v| v.as_deref())
  }

  /// Get all edge property modifications in delta
  pub fn edge_props_delta(
    &self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
  ) -> Option<&HashMap<PropKeyId, Option<PropValueRef>>> {
    self.edge_props.get(&(src, etype, dst))
  }

  // ========================================================================
  // Key Index Operations
  // ========================================================================

  /// Lookup node by key in delta
  pub fn node_by_key(&self, key: &str) -> Option<NodeId> {
    // Check if key was deleted
    if self.key_index_deleted.contains(key) {
      return None;
    }
    self.key_index.get(key).copied()
  }

  /// Live node holding `key` through this delta over `snapshot`.
  pub fn key_owner_over(&self, snapshot: Option<&SnapshotData>, key: &str) -> Option<NodeId> {
    if self.key_index_deleted.contains(key) {
      return None;
    }
    if let Some(&node_id) = self.key_index.get(key) {
      if !self.is_node_removed(node_id) {
        return Some(node_id);
      }
    }
    // A deleted or recreated node's snapshot key is masked with its copy.
    snapshot
      .and_then(|snap| snap.lookup_by_key(key))
      .filter(|&node_id| !self.is_node_deleted(node_id))
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

  #[test]
  fn test_add_over_base_edge_is_noop() {
    let mut delta = DeltaState::new();
    delta.add_edge_over(1, 10, 2, true);
    assert!(!delta.is_edge_added(1, 10, 2));
    assert_eq!(delta.total_edges_added(), 0);
    assert!(delta.edge_visible(1, 10, 2, true));
  }

  #[test]
  fn test_delete_over_base_edge_always_tombstones() {
    let mut delta = DeltaState::new();
    // A stray add patch for a base edge (written by older versions).
    delta.add_edge(1, 10, 2);
    delta.delete_edge_over(1, 10, 2, true);
    assert!(!delta.is_edge_added(1, 10, 2));
    assert!(delta.is_edge_deleted(1, 10, 2));
    assert!(!delta.edge_visible(1, 10, 2, true));

    // Re-adding lifts the tombstone without an add patch.
    delta.add_edge_over(1, 10, 2, true);
    assert!(!delta.is_edge_deleted(1, 10, 2));
    assert!(!delta.is_edge_added(1, 10, 2));
  }

  #[test]
  fn test_recreate_keeps_base_masked_and_starts_fresh() {
    let (n, a, b, c) = (1, 2, 3, 4);
    let mut delta = DeltaState::new();
    // State the delta held for the base node n before its delete.
    delta.add_edge(n, 10, a);
    delta.add_edge(b, 10, n);
    delta.delete_edge(n, 10, c); // tombstone of a base edge
    delta.add_edge(a, 10, b); // unrelated
    delta.add_node_label(n, 7);
    delta.delete_node(n);
    assert!(delta.is_node_removed(n));

    delta.create_node(n, Some("new"));
    assert!(delta.is_node_created(n));
    assert!(delta.is_node_deleted(n), "the base copy stays masked");
    assert!(!delta.is_node_removed(n));
    assert!(delta.node_exists_over(None, n));
    assert_eq!(delta.key_owner_over(None, "new"), Some(n));
    assert!(delta.modified_nodes.is_empty());
    for map in [&delta.out_add, &delta.in_add, &delta.out_del, &delta.in_del] {
      assert!(
        map
          .iter()
          .all(|(&node, patches)| node != n && patches.iter().all(|p| p.other != n)),
        "an old edge patch of n survived the recreate: {map:?}"
      );
    }
    assert!(delta.is_edge_added(a, 10, b), "unrelated patches stay");

    // Deleting the recreated node removes it again, base copy still masked.
    delta.delete_node(n);
    assert!(delta.is_node_removed(n));
    assert_eq!(delta.key_owner_over(None, "new"), None);
  }

  #[test]
  fn test_delete_created_node_drops_only_its_edges() {
    let mut delta = DeltaState::new();
    for node in 1..=4 {
      delta.create_node(node, None);
    }
    delta.add_edge(1, 10, 2);
    delta.add_edge(3, 10, 1);
    delta.add_edge(1, 10, 1);
    delta.add_edge(3, 10, 4);
    delta.add_edge(2, 11, 3);
    delta.delete_node(1);
    for map in [&delta.out_add, &delta.in_add] {
      assert!(
        map
          .iter()
          .all(|(&node, patches)| node != 1 && patches.iter().all(|p| p.other != 1)),
        "an edge patch of the deleted node survived: {map:?}"
      );
    }
    assert!(delta.is_edge_added(3, 10, 4) && delta.is_edge_added(2, 11, 3));
    assert_eq!(delta.total_edges_added(), 2);
    assert_eq!(delta.in_add.values().map(|s| s.len()).sum::<usize>(), 2);
  }

  #[test]
  fn test_edge_props_go_with_the_edge() {
    let mut delta = DeltaState::new();
    // A delta edge: deleting cancels the add patch and drops its props.
    delta.add_edge(1, 10, 2);
    delta.set_edge_prop(1, 10, 2, 7, PropValue::I64(5));
    delta.delete_edge(1, 10, 2);
    assert!(delta.edge_props_delta(1, 10, 2).is_none());

    // A base edge: the tombstone drops the props the delta held for it.
    delta.set_edge_prop(3, 10, 4, 7, PropValue::I64(6));
    delta.delete_edge_over(3, 10, 4, true);
    assert!(delta.is_edge_deleted(3, 10, 4));
    assert!(delta.edge_props_delta(3, 10, 4).is_none());

    // A deleted node's delta edges lose their props.
    delta.create_node(5, None);
    delta.create_node(6, None);
    delta.add_edge(5, 10, 6);
    delta.add_edge(6, 10, 5);
    delta.set_edge_prop(5, 10, 6, 7, PropValue::I64(7));
    delta.set_edge_prop(6, 10, 5, 7, PropValue::I64(8));
    delta.delete_node(6);
    assert!(delta.edge_props.is_empty(), "{:?}", delta.edge_props);
  }

  #[test]
  fn test_recreate_drops_props_of_the_old_copys_base_edges() {
    let mut delta = DeltaState::new();
    // Props the delta held for base edges of node 1, then its delete.
    delta.set_edge_prop(1, 10, 2, 7, PropValue::I64(5));
    delta.set_edge_prop(3, 10, 1, 7, PropValue::I64(6));
    delta.set_edge_prop(2, 10, 3, 7, PropValue::I64(7));
    delta.delete_node(1);
    delta.create_node(1, None);
    assert_eq!(
      delta.edge_props.keys().copied().collect::<Vec<_>>(),
      vec![(2, 10, 3)],
      "only the unrelated edge keeps its props"
    );
  }

  #[test]
  fn test_layered_ops_on_edge_missing_from_base() {
    let mut delta = DeltaState::new();
    delta.delete_edge_over(1, 10, 2, false);
    assert!(
      !delta.is_edge_deleted(1, 10, 2),
      "no tombstone for a missing edge"
    );

    // A stray tombstone must not swallow the add.
    delta.delete_edge(1, 10, 2);
    delta.add_edge_over(1, 10, 2, false);
    assert!(delta.is_edge_added(1, 10, 2));
    assert!(!delta.is_edge_deleted(1, 10, 2));

    delta.delete_edge_over(1, 10, 2, false);
    assert!(!delta.is_edge_added(1, 10, 2));
    assert!(!delta.is_edge_deleted(1, 10, 2));
    assert!(delta.in_add.is_empty() && delta.in_del.is_empty());
  }
}
