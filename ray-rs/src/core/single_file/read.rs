//! Read operations for SingleFileDB
//!
//! Handles all query operations: get properties, get edges, key lookups,
//! label checks, and neighbor traversal.
//!
//! # Lock order
//!
//! `delta` -> `snapshot` -> `mvcc.tx_manager` -> `mvcc.version_chain` -> `mvcc.gc`
//!
//! Every path that holds more than one of these takes them in this order: commit holds
//! `delta.write()` -> `snapshot.read()` -> `version_chain`, checkpoint installs hold
//! `delta.write()` -> `snapshot.write()` (then the vector stores) to replace both in one
//! step (`install_loaded_snapshot`), and GC holds `tx_manager` ->
//! `version_chain` -> `gc` (`mvcc/manager.rs`). Readers compute their MVCC timestamp first,
//! take `version_chain` last, and drop it before `record_read`. `delta` and `snapshot` are
//! task-fair RwLocks: a queued writer blocks new readers, so even a read guard must never be
//! requested while a later lock is held. The calling thread's own tx state mutex is private
//! to that thread and sits outside this order, but it is not reentrant: never lock it twice.

use std::collections::HashMap;

use crate::mvcc::visibility::{
  edge_exists as mvcc_edge_exists, node_exists as mvcc_node_exists, visible_version,
};
use crate::types::*;

use super::{SingleFileDB, SingleFileTxState};

/// Which layers' state of a node a reader sees: its transaction's pending
/// delta over the committed delta over the snapshot. A layer's delete masks
/// the node's copies below it (props, labels, key, edges); a recreated node
/// holds a fresh copy in the layer that recreated it. `mvcc` is the node's
/// MVCC visibility (`None` without a version chain), which takes precedence
/// over the committed delta's node state.
#[derive(Clone, Copy)]
pub(super) struct NodeLayers<'a> {
  pub(super) pending: Option<&'a DeltaState>,
  pub(super) delta: &'a DeltaState,
}

impl NodeLayers<'_> {
  /// The snapshot's copy of the node: its props, labels, key and edges. A
  /// node the committed delta recreated never shows its old copy, even when
  /// its version chain says it is visible.
  pub(super) fn sees_snapshot(&self, node_id: NodeId, mvcc: Option<bool>) -> bool {
    !self.pending_masks(node_id)
      && match mvcc {
        Some(visible) => visible && !self.delta.is_node_created(node_id),
        None => !self.delta.is_node_deleted(node_id),
      }
  }

  /// The committed delta's state of the node: its copy (if created there)
  /// and edge patches.
  pub(super) fn sees_delta(&self, node_id: NodeId, mvcc: Option<bool>) -> bool {
    !self.pending_masks(node_id)
      && match mvcc {
        Some(visible) => visible,
        None => !self.delta.is_node_removed(node_id),
      }
  }

  /// The transaction's own state of the node: its edge patches.
  pub(super) fn sees_pending(&self, node_id: NodeId, mvcc: Option<bool>) -> bool {
    self.pending.is_some_and(|p| p.is_node_created(node_id)) || self.sees_delta(node_id, mvcc)
  }

  /// Whether the transaction deleted (or recreated) the node, masking its
  /// committed copy.
  pub(super) fn pending_masks(&self, node_id: NodeId) -> bool {
    self.pending.is_some_and(|p| p.is_node_deleted(node_id))
  }
}

impl SingleFileDB {
  /// MVCC visibility context `(txid, snapshot_ts)`: the transaction's snapshot inside a
  /// transaction, the latest commit outside one, and `(0, 0)` with MVCC disabled.
  pub(super) fn mvcc_read_ts(&self, tx: Option<&SingleFileTxState>) -> (TxId, Timestamp) {
    match (self.mvcc.as_ref(), tx) {
      (None, _) => (0, 0),
      (Some(_), Some(tx)) => (tx.txid, tx.snapshot_ts),
      (Some(mvcc), None) => (0, mvcc.tx_manager.lock().next_commit_ts()),
    }
  }

  /// MVCC visibility of a node, or `None` when its version chain has no entry.
  /// Holds `version_chain` only for the lookup.
  fn mvcc_node_visible(
    &self,
    node_id: NodeId,
    tx_snapshot_ts: Timestamp,
    txid: TxId,
  ) -> Option<bool> {
    let mvcc = self.mvcc.as_ref()?;
    let vc = mvcc.version_chain.lock();
    vc.node_version(node_id)
      .map(|version| mvcc_node_exists(Some(version), tx_snapshot_ts, txid))
  }

  // ========================================================================
  // Node Property Reads
  // ========================================================================

  /// Get all properties for a node
  ///
  /// Returns None if the node doesn't exist or is deleted.
  /// Merges properties from snapshot with delta modifications.
  pub fn node_props(&self, node_id: NodeId) -> Option<HashMap<PropKeyId, PropValue>> {
    let tx_handle = self.current_tx_handle();
    let tx_guard = tx_handle.as_ref().map(|tx| tx.lock());
    let pending = tx_guard.as_ref().map(|tx| &tx.pending);

    if pending.is_some_and(|p| p.is_node_removed(node_id)) {
      return None;
    }

    let (txid, tx_snapshot_ts) = self.mvcc_read_ts(tx_guard.as_deref());
    let delta = self.delta.read();
    let layers = NodeLayers {
      pending,
      delta: &delta,
    };

    let mut props = HashMap::new();
    let snapshot = self.snapshot.read();
    let vc_guard = self.mvcc.as_ref().map(|mvcc| mvcc.version_chain.lock());
    let mvcc_node_visible = vc_guard
      .as_ref()
      .and_then(|vc| vc.node_version(node_id))
      .map(|version| mvcc_node_exists(Some(version), tx_snapshot_ts, txid));

    // Get properties from snapshot first
    if let Some(ref snap) = *snapshot {
      if let Some(phys) = snap
        .phys_node(node_id)
        .filter(|_| layers.sees_snapshot(node_id, mvcc_node_visible))
      {
        if let Some(snapshot_props) = snap.node_props(phys) {
          props = snapshot_props;
        }
      }
    }

    // Apply committed delta modifications
    if let Some(node_delta) = delta
      .node_delta(node_id)
      .filter(|_| !layers.pending_masks(node_id))
    {
      if let Some(ref delta_props) = node_delta.props {
        props.reserve(delta_props.len());
        for (&key_id, value) in delta_props {
          match value {
            Some(v) => {
              props.insert(key_id, v.as_ref().clone());
            }
            None => {
              props.remove(&key_id);
            }
          }
        }
      }
    }

    if let Some(vc) = vc_guard.as_ref().filter(|_| !layers.pending_masks(node_id)) {
      for key_id in vc.node_prop_keys(node_id) {
        if let Some(prop_version) = vc.node_prop_version(node_id, key_id) {
          if let Some(visible) = visible_version(&prop_version, tx_snapshot_ts, txid) {
            match &visible.data {
              Some(v) => {
                props.insert(key_id, v.as_ref().clone());
              }
              None => {
                props.remove(&key_id);
              }
            }
          }
        }
      }
    }
    drop(vc_guard);

    // Apply pending modifications (overlay)
    if let Some(pending_delta) = pending {
      if let Some(node_delta) = pending_delta.node_delta(node_id) {
        if let Some(ref delta_props) = node_delta.props {
          props.reserve(delta_props.len());
          for (&key_id, value) in delta_props {
            match value {
              Some(v) => {
                props.insert(key_id, v.as_ref().clone());
              }
              None => {
                props.remove(&key_id);
              }
            }
          }
        }
      }
    }

    // Check if node exists at all
    let node_exists_in_pending =
      pending.is_some_and(|p| p.is_node_created(node_id) || p.node_delta(node_id).is_some());
    let node_exists = if node_exists_in_pending {
      true
    } else if let Some(visible) = mvcc_node_visible {
      visible
    } else if delta.is_node_removed(node_id) {
      false
    } else {
      let node_exists_in_delta =
        delta.is_node_created(node_id) || delta.node_delta(node_id).is_some();
      if node_exists_in_delta {
        true
      } else if let Some(ref snap) = *snapshot {
        snap.phys_node(node_id).is_some()
      } else {
        false
      }
    };

    if !node_exists {
      return None;
    }

    if let Some(mvcc) = self.mvcc.as_ref() {
      if txid != 0 {
        let mut tx_mgr = mvcc.tx_manager.lock();
        for key_id in props.keys() {
          tx_mgr.record_read(
            txid,
            TxKey::NodeProp {
              node_id,
              key_id: *key_id,
            },
          );
        }
      }
    }

    Some(props)
  }

  /// Get a specific property for a node
  ///
  /// Returns None if the node doesn't exist, is deleted, or doesn't have the property.
  pub fn node_prop(&self, node_id: NodeId, key_id: PropKeyId) -> Option<PropValue> {
    let tx_handle = self.current_tx_handle();
    if let Some(handle) = tx_handle.as_ref() {
      let tx = handle.lock();
      if tx.pending.is_node_removed(node_id) {
        return None;
      }
      if let Some(node_delta) = tx.pending.node_delta(node_id) {
        if let Some(ref delta_props) = node_delta.props {
          if let Some(value) = delta_props.get(&key_id) {
            return value.as_deref().cloned();
          }
        }
      }
      if tx.pending.is_node_created(node_id) {
        return None;
      }
    }

    let mut mvcc_node_visible = None;
    if let Some(mvcc) = self.mvcc.as_ref() {
      let (txid, tx_snapshot_ts) = if let Some(handle) = tx_handle.as_ref() {
        let tx = handle.lock();
        (tx.txid, tx.snapshot_ts)
      } else {
        (0, mvcc.tx_manager.lock().next_commit_ts())
      };
      if txid != 0 {
        let mut tx_mgr = mvcc.tx_manager.lock();
        tx_mgr.record_read(txid, TxKey::NodeProp { node_id, key_id });
      }
      let vc = mvcc.version_chain.lock();
      if let Some(version) = vc.node_version(node_id) {
        mvcc_node_visible = Some(mvcc_node_exists(Some(version), tx_snapshot_ts, txid));
      }
      if let Some(prop_version) = vc.node_prop_version(node_id, key_id) {
        if let Some(visible) = visible_version(&prop_version, tx_snapshot_ts, txid) {
          return visible.data.as_deref().cloned();
        }
      }
    }

    let delta = self.delta.read();

    // Check if node is deleted (unless MVCC snapshot says otherwise)
    if mvcc_node_visible == Some(false) {
      return None;
    }
    if mvcc_node_visible.is_none() && delta.is_node_removed(node_id) {
      return None;
    }

    // Check delta first (for modifications)
    if let Some(node_delta) = delta.node_delta(node_id) {
      if let Some(ref delta_props) = node_delta.props {
        if let Some(value) = delta_props.get(&key_id) {
          // None means explicitly deleted
          return value.as_deref().cloned();
        }
      }
    }

    // A node created (or recreated) in the delta has no snapshot props.
    if delta.is_node_created(node_id) {
      return None;
    }

    // Fall back to snapshot
    let snapshot = self.snapshot.read();
    if let Some(ref snap) = *snapshot {
      if let Some(phys) = snap.phys_node(node_id) {
        return snap.node_prop(phys, key_id);
      }
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
  pub fn edge_props(
    &self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
  ) -> Option<HashMap<PropKeyId, PropValue>> {
    let tx_handle = self.current_tx_handle();
    let tx_guard = tx_handle.as_ref().map(|tx| tx.lock());
    let pending = tx_guard.as_ref().map(|tx| &tx.pending);

    if pending.is_some_and(|p| p.is_node_removed(src) || p.is_node_removed(dst)) {
      return None;
    }
    if pending.is_some_and(|p| p.is_edge_deleted(src, etype, dst)) {
      return None;
    }

    let (txid, tx_snapshot_ts) = self.mvcc_read_ts(tx_guard.as_deref());
    let delta = self.delta.read();
    let layers = NodeLayers {
      pending,
      delta: &delta,
    };
    // An endpoint the transaction deleted or recreated masks the committed
    // edge: only the transaction's own edge and props remain.
    let committed_masked = layers.pending_masks(src) || layers.pending_masks(dst);

    let mut mvcc_edge_visible = None;

    let mut props = HashMap::new();
    let snapshot = self.snapshot.read();
    let vc_guard = self.mvcc.as_ref().map(|mvcc| mvcc.version_chain.lock());
    let node_visible = |node_id| {
      vc_guard
        .as_ref()
        .and_then(|vc| vc.node_version(node_id))
        .map(|version| mvcc_node_exists(Some(version), tx_snapshot_ts, txid))
    };
    let mvcc_src_visible = node_visible(src);
    let mvcc_dst_visible = node_visible(dst);

    // First, determine if edge exists
    let edge_added_in_delta = delta.is_edge_added(src, etype, dst);
    let edge_added_in_pending = pending.is_some_and(|p| p.is_edge_added(src, etype, dst));
    let mut edge_exists_in_snapshot = false;

    // Check snapshot for edge existence and get base properties
    if let Some(ref snap) = *snapshot {
      if let (Some(src_phys), Some(dst_phys)) = (snap.phys_node(src), snap.phys_node(dst)) {
        if layers.sees_snapshot(src, mvcc_src_visible)
          && layers.sees_snapshot(dst, mvcc_dst_visible)
        {
          if let Some(edge_idx) = snap.find_edge_index(src_phys, etype, dst_phys) {
            edge_exists_in_snapshot = true;
            // Get properties from snapshot
            if let Some(snapshot_props) = snap.edge_props(edge_idx) {
              props = snapshot_props;
            }
          }
        }
      }
    }

    if let Some(vc) = vc_guard.as_ref().filter(|_| !committed_masked) {
      if let Some(version) = vc.edge_version(src, etype, dst) {
        mvcc_edge_visible = Some(mvcc_edge_exists(Some(version), tx_snapshot_ts, txid));
      }
      for key_id in vc.edge_prop_keys(src, etype, dst) {
        if let Some(prop_version) = vc.edge_prop_version(src, etype, dst, key_id) {
          if let Some(visible) = visible_version(&prop_version, tx_snapshot_ts, txid) {
            match &visible.data {
              Some(v) => {
                props.insert(key_id, v.as_ref().clone());
              }
              None => {
                props.remove(&key_id);
              }
            }
          }
        }
      }
    }
    drop(vc_guard);

    if committed_masked {
      if !edge_added_in_pending {
        return None;
      }
    } else {
      if mvcc_src_visible == Some(false) || mvcc_dst_visible == Some(false) {
        return None;
      }
      if mvcc_src_visible.is_none() && delta.is_node_removed(src) {
        return None;
      }
      if mvcc_dst_visible.is_none() && delta.is_node_removed(dst) {
        return None;
      }
      if mvcc_edge_visible == Some(false) {
        return None;
      }
      if mvcc_edge_visible.is_none() && delta.is_edge_deleted(src, etype, dst) {
        return None;
      }

      // Edge must exist either in delta or snapshot (unless MVCC says visible)
      if mvcc_edge_visible != Some(true)
        && !edge_added_in_delta
        && !edge_added_in_pending
        && !edge_exists_in_snapshot
      {
        return None;
      }

      // Apply committed delta modifications (only if edge exists)
      if let Some(delta_props) = delta.edge_props_delta(src, etype, dst) {
        props.reserve(delta_props.len());
        for (&key_id, value) in delta_props {
          match value {
            Some(v) => {
              props.insert(key_id, v.as_ref().clone());
            }
            None => {
              props.remove(&key_id);
            }
          }
        }
      }
    }

    // Apply pending modifications
    if let Some(pending_delta) = pending {
      if let Some(delta_props) = pending_delta.edge_props_delta(src, etype, dst) {
        props.reserve(delta_props.len());
        for (&key_id, value) in delta_props {
          match value {
            Some(v) => {
              props.insert(key_id, v.as_ref().clone());
            }
            None => {
              props.remove(&key_id);
            }
          }
        }
      }
    }

    if let Some(mvcc) = self.mvcc.as_ref() {
      if txid != 0 {
        let mut tx_mgr = mvcc.tx_manager.lock();
        for key_id in props.keys() {
          tx_mgr.record_read(
            txid,
            TxKey::EdgeProp {
              src,
              etype,
              dst,
              key_id: *key_id,
            },
          );
        }
      }
    }

    Some(props)
  }

  /// Get a specific property for an edge
  ///
  /// Returns None if the edge doesn't exist or doesn't have the property.
  pub fn edge_prop(
    &self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    key_id: PropKeyId,
  ) -> Option<PropValue> {
    let tx_handle = self.current_tx_handle();
    if let Some(handle) = tx_handle.as_ref() {
      let tx = handle.lock();
      if tx.pending.is_node_removed(src) || tx.pending.is_node_removed(dst) {
        return None;
      }
      if tx.pending.is_edge_deleted(src, etype, dst) {
        return None;
      }
      if let Some(delta_props) = tx.pending.edge_props_delta(src, etype, dst) {
        if let Some(value) = delta_props.get(&key_id) {
          return value.as_deref().cloned();
        }
      }
      // An endpoint the transaction recreated masks the committed edge.
      if tx.pending.is_node_deleted(src) || tx.pending.is_node_deleted(dst) {
        return None;
      }
    }

    let mut mvcc_src_visible = None;
    let mut mvcc_dst_visible = None;
    let mut mvcc_edge_visible = None;
    if let Some(mvcc) = self.mvcc.as_ref() {
      let (txid, tx_snapshot_ts) = if let Some(handle) = tx_handle.as_ref() {
        let tx = handle.lock();
        (tx.txid, tx.snapshot_ts)
      } else {
        (0, mvcc.tx_manager.lock().next_commit_ts())
      };
      if txid != 0 {
        let mut tx_mgr = mvcc.tx_manager.lock();
        tx_mgr.record_read(
          txid,
          TxKey::EdgeProp {
            src,
            etype,
            dst,
            key_id,
          },
        );
      }
      let vc = mvcc.version_chain.lock();
      if let Some(version) = vc.node_version(src) {
        mvcc_src_visible = Some(mvcc_node_exists(Some(version), tx_snapshot_ts, txid));
      }
      if let Some(version) = vc.node_version(dst) {
        mvcc_dst_visible = Some(mvcc_node_exists(Some(version), tx_snapshot_ts, txid));
      }
      if let Some(version) = vc.edge_version(src, etype, dst) {
        mvcc_edge_visible = Some(mvcc_edge_exists(Some(version), tx_snapshot_ts, txid));
      }
      if let Some(prop_version) = vc.edge_prop_version(src, etype, dst, key_id) {
        if let Some(visible) = visible_version(&prop_version, tx_snapshot_ts, txid) {
          return visible.data.as_deref().cloned();
        }
      }
    }

    let delta = self.delta.read();

    if mvcc_src_visible == Some(false) || mvcc_dst_visible == Some(false) {
      return None;
    }

    // Check if either node is deleted
    if mvcc_src_visible.is_none() && delta.is_node_removed(src) {
      return None;
    }
    if mvcc_dst_visible.is_none() && delta.is_node_removed(dst) {
      return None;
    }

    // Check if edge is deleted in delta
    if mvcc_edge_visible == Some(false) {
      return None;
    }
    if mvcc_edge_visible.is_none() && delta.is_edge_deleted(src, etype, dst) {
      return None;
    }

    // First, determine if edge exists at all
    let edge_added_in_delta = delta.is_edge_added(src, etype, dst);
    let edge_added_in_pending = tx_handle
      .as_ref()
      .map(|handle| handle.lock().pending.is_edge_added(src, etype, dst))
      .unwrap_or(false);
    let snapshot = self.snapshot.read();
    // The transaction's masks returned above.
    let layers = NodeLayers {
      pending: None,
      delta: &delta,
    };
    let snapshot_edge = snapshot
      .as_ref()
      .filter(|_| {
        layers.sees_snapshot(src, mvcc_src_visible) && layers.sees_snapshot(dst, mvcc_dst_visible)
      })
      .and_then(|snap| {
        let (src_phys, dst_phys) = (snap.phys_node(src)?, snap.phys_node(dst)?);
        Some((snap, snap.find_edge_index(src_phys, etype, dst_phys)?))
      });
    let edge_exists_in_snapshot = snapshot_edge.is_some();

    // Edge must exist either in delta or snapshot
    if mvcc_edge_visible != Some(true)
      && !edge_added_in_delta
      && !edge_added_in_pending
      && !edge_exists_in_snapshot
    {
      return None;
    }

    // Check delta first (for modifications)
    if let Some(delta_props) = delta.edge_props_delta(src, etype, dst) {
      if let Some(value) = delta_props.get(&key_id) {
        // Some(None) means explicitly deleted
        return value.as_deref().cloned();
      }
    }

    // Fall back to snapshot
    let (snap, edge_idx) = snapshot_edge?;
    snap.edge_props(edge_idx)?.remove(&key_id)
  }

  // ========================================================================
  // Edge Traversal
  // ========================================================================

  /// Get outgoing edges for a node
  ///
  /// Returns edges as (edge_type_id, destination_node_id) pairs.
  /// Merges edges from snapshot with delta additions/deletions.
  /// Filters out edges to deleted nodes.
  pub fn out_edges(&self, node_id: NodeId) -> Vec<(ETypeId, NodeId)> {
    let tx_handle = self.current_tx_handle();
    let tx_guard = tx_handle.as_ref().map(|tx| tx.lock());
    let pending = tx_guard.as_ref().map(|tx| &tx.pending);

    // If node is deleted, no edges
    if pending.is_some_and(|p| p.is_node_removed(node_id)) {
      return Vec::new();
    }

    let (txid, tx_snapshot_ts) = self.mvcc_read_ts(tx_guard.as_deref());
    let delta = self.delta.read();
    let snapshot = self.snapshot.read();
    let vc_guard = self.mvcc.as_ref().map(|mvcc| mvcc.version_chain.lock());
    let layers = NodeLayers {
      pending,
      delta: &delta,
    };

    // If node is deleted in committed state, no edges
    let node_visible = vc_guard
      .as_ref()
      .and_then(|vc| vc.node_version(node_id))
      .map(|version| mvcc_node_exists(Some(version), tx_snapshot_ts, txid));
    if !layers.sees_pending(node_id, node_visible) {
      return Vec::new();
    }

    let mut capacity = 0usize;
    if let Some(ref snap) = *snapshot {
      if let Some(phys) = snap.phys_node(node_id) {
        capacity = capacity.saturating_add(snap.out_degree(phys).unwrap_or(0));
      }
    }
    if let Some(added_edges) = delta.out_add.get(&node_id) {
      capacity = capacity.saturating_add(added_edges.len());
    }
    if let Some(added_edges) = pending.and_then(|p| p.out_add.get(&node_id)) {
      capacity = capacity.saturating_add(added_edges.len());
    }
    let mut edges = Vec::with_capacity(capacity);

    // Get edges from snapshot
    if let Some(ref snap) = *snapshot {
      if let Some(phys) = snap
        .phys_node(node_id)
        .filter(|_| layers.sees_snapshot(node_id, node_visible))
      {
        for (dst_phys, etype) in snap.iter_out_edges(phys) {
          // Convert physical dst to NodeId
          if let Some(dst_node_id) = snap.node_id(dst_phys) {
            // Skip edges to deleted nodes
            let dst_visible = vc_guard
              .as_ref()
              .and_then(|vc| vc.node_version(dst_node_id))
              .map(|version| mvcc_node_exists(Some(version), tx_snapshot_ts, txid));
            if !layers.sees_snapshot(dst_node_id, dst_visible) {
              continue;
            }
            // Skip edges deleted in delta
            let edge_visible = vc_guard
              .as_ref()
              .and_then(|vc| vc.edge_version(node_id, etype, dst_node_id))
              .map(|version| mvcc_edge_exists(Some(version), tx_snapshot_ts, txid));
            if edge_visible == Some(false)
              || pending.is_some_and(|p| p.is_edge_deleted(node_id, etype, dst_node_id))
              || (edge_visible.is_none() && delta.is_edge_deleted(node_id, etype, dst_node_id))
            {
              continue;
            }
            edges.push((etype, dst_node_id));
          }
        }
      }
    }

    // Add edges from delta
    if let Some(added_edges) = delta
      .out_add
      .get(&node_id)
      .filter(|_| layers.sees_delta(node_id, node_visible))
    {
      for edge_patch in added_edges {
        // Skip edges to deleted nodes
        let dst_visible = vc_guard
          .as_ref()
          .and_then(|vc| vc.node_version(edge_patch.other))
          .map(|version| mvcc_node_exists(Some(version), tx_snapshot_ts, txid));
        if !layers.sees_delta(edge_patch.other, dst_visible) {
          continue;
        }
        let edge_visible = vc_guard
          .as_ref()
          .and_then(|vc| vc.edge_version(node_id, edge_patch.etype, edge_patch.other))
          .map(|version| mvcc_edge_exists(Some(version), tx_snapshot_ts, txid));
        if edge_visible == Some(false)
          || pending.is_some_and(|p| p.is_edge_deleted(node_id, edge_patch.etype, edge_patch.other))
        {
          continue;
        }
        edges.push((edge_patch.etype, edge_patch.other));
      }
    }

    if let Some(added_edges) = pending.and_then(|p| p.out_add.get(&node_id)) {
      for edge_patch in added_edges {
        let dst_visible = vc_guard
          .as_ref()
          .and_then(|vc| vc.node_version(edge_patch.other))
          .map(|version| mvcc_node_exists(Some(version), tx_snapshot_ts, txid));
        if !layers.sees_pending(edge_patch.other, dst_visible) {
          continue;
        }
        edges.push((edge_patch.etype, edge_patch.other));
      }
    }

    // Sort by (etype, dst) for consistent ordering
    edges.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    edges.dedup();

    drop(vc_guard);
    if let Some(mvcc) = self.mvcc.as_ref() {
      if txid != 0 {
        let mut tx_mgr = mvcc.tx_manager.lock();
        tx_mgr.record_read(
          txid,
          TxKey::NeighborsOut {
            node_id,
            etype: None,
          },
        );
      }
    }

    edges
  }

  /// Get incoming edges for a node
  ///
  /// Returns edges as (edge_type_id, source_node_id) pairs.
  /// Merges edges from snapshot with delta additions/deletions.
  /// Filters out edges from deleted nodes.
  pub fn in_edges(&self, node_id: NodeId) -> Vec<(ETypeId, NodeId)> {
    let tx_handle = self.current_tx_handle();
    let tx_guard = tx_handle.as_ref().map(|tx| tx.lock());
    let pending = tx_guard.as_ref().map(|tx| &tx.pending);

    // If node is deleted, no edges
    if pending.is_some_and(|p| p.is_node_removed(node_id)) {
      return Vec::new();
    }

    let (txid, tx_snapshot_ts) = self.mvcc_read_ts(tx_guard.as_deref());
    let delta = self.delta.read();
    let snapshot = self.snapshot.read();
    let vc_guard = self.mvcc.as_ref().map(|mvcc| mvcc.version_chain.lock());
    let layers = NodeLayers {
      pending,
      delta: &delta,
    };

    // If node is deleted, no edges
    let node_visible = vc_guard
      .as_ref()
      .and_then(|vc| vc.node_version(node_id))
      .map(|version| mvcc_node_exists(Some(version), tx_snapshot_ts, txid));
    if !layers.sees_pending(node_id, node_visible) {
      return Vec::new();
    }

    let mut capacity = 0usize;
    if let Some(ref snap) = *snapshot {
      if let Some(phys) = snap.phys_node(node_id) {
        capacity = capacity.saturating_add(snap.in_degree(phys).unwrap_or(0));
      }
    }
    if let Some(added_edges) = delta.in_add.get(&node_id) {
      capacity = capacity.saturating_add(added_edges.len());
    }
    if let Some(added_edges) = pending.and_then(|p| p.in_add.get(&node_id)) {
      capacity = capacity.saturating_add(added_edges.len());
    }
    let mut edges = Vec::with_capacity(capacity);

    // Get edges from snapshot
    if let Some(ref snap) = *snapshot {
      if let Some(phys) = snap
        .phys_node(node_id)
        .filter(|_| layers.sees_snapshot(node_id, node_visible))
      {
        for (src_phys, etype, _out_index) in snap.iter_in_edges(phys) {
          // Convert physical src to NodeId
          if let Some(src_node_id) = snap.node_id(src_phys) {
            // Skip edges from deleted nodes
            let src_visible = vc_guard
              .as_ref()
              .and_then(|vc| vc.node_version(src_node_id))
              .map(|version| mvcc_node_exists(Some(version), tx_snapshot_ts, txid));
            if !layers.sees_snapshot(src_node_id, src_visible) {
              continue;
            }
            // Skip edges deleted in delta
            let edge_visible = vc_guard
              .as_ref()
              .and_then(|vc| vc.edge_version(src_node_id, etype, node_id))
              .map(|version| mvcc_edge_exists(Some(version), tx_snapshot_ts, txid));
            if edge_visible == Some(false)
              || pending.is_some_and(|p| p.is_edge_deleted(src_node_id, etype, node_id))
              || (edge_visible.is_none() && delta.is_edge_deleted(src_node_id, etype, node_id))
            {
              continue;
            }
            edges.push((etype, src_node_id));
          }
        }
      }
    }

    // Add edges from delta (in_add stores patches where other=src)
    if let Some(added_edges) = delta
      .in_add
      .get(&node_id)
      .filter(|_| layers.sees_delta(node_id, node_visible))
    {
      for edge_patch in added_edges {
        // Skip edges from deleted nodes
        let src_visible = vc_guard
          .as_ref()
          .and_then(|vc| vc.node_version(edge_patch.other))
          .map(|version| mvcc_node_exists(Some(version), tx_snapshot_ts, txid));
        if !layers.sees_delta(edge_patch.other, src_visible) {
          continue;
        }
        let edge_visible = vc_guard
          .as_ref()
          .and_then(|vc| vc.edge_version(edge_patch.other, edge_patch.etype, node_id))
          .map(|version| mvcc_edge_exists(Some(version), tx_snapshot_ts, txid));
        if edge_visible == Some(false)
          || pending.is_some_and(|p| p.is_edge_deleted(edge_patch.other, edge_patch.etype, node_id))
        {
          continue;
        }
        edges.push((edge_patch.etype, edge_patch.other));
      }
    }

    if let Some(added_edges) = pending.and_then(|p| p.in_add.get(&node_id)) {
      for edge_patch in added_edges {
        let src_visible = vc_guard
          .as_ref()
          .and_then(|vc| vc.node_version(edge_patch.other))
          .map(|version| mvcc_node_exists(Some(version), tx_snapshot_ts, txid));
        if !layers.sees_pending(edge_patch.other, src_visible) {
          continue;
        }
        edges.push((edge_patch.etype, edge_patch.other));
      }
    }

    // Sort by (etype, src) for consistent ordering
    edges.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    edges.dedup();

    drop(vc_guard);
    if let Some(mvcc) = self.mvcc.as_ref() {
      if txid != 0 {
        let mut tx_mgr = mvcc.tx_manager.lock();
        tx_mgr.record_read(
          txid,
          TxKey::NeighborsIn {
            node_id,
            etype: None,
          },
        );
      }
    }

    edges
  }

  /// Get out-degree (number of outgoing edges) for a node
  pub fn out_degree(&self, node_id: NodeId) -> usize {
    self.out_edges(node_id).len()
  }

  /// Get in-degree (number of incoming edges) for a node
  pub fn in_degree(&self, node_id: NodeId) -> usize {
    self.in_edges(node_id).len()
  }

  /// Get neighbors via outgoing edges of a specific type
  ///
  /// Returns destination node IDs for edges of the given type.
  pub fn out_neighbors(&self, node_id: NodeId, etype: ETypeId) -> Vec<NodeId> {
    let neighbors: Vec<NodeId> = self
      .out_edges(node_id)
      .into_iter()
      .filter(|(e, _)| *e == etype)
      .map(|(_, dst)| dst)
      .collect();
    if let Some(mvcc) = self.mvcc.as_ref() {
      let tx_handle = self.current_tx_handle();
      if let Some(handle) = tx_handle.as_ref() {
        let tx = handle.lock();
        let mut tx_mgr = mvcc.tx_manager.lock();
        tx_mgr.record_read(
          tx.txid,
          TxKey::NeighborsOut {
            node_id,
            etype: Some(etype),
          },
        );
      }
    }
    neighbors
  }

  /// Get neighbors via incoming edges of a specific type
  ///
  /// Returns source node IDs for edges of the given type.
  pub fn in_neighbors(&self, node_id: NodeId, etype: ETypeId) -> Vec<NodeId> {
    let neighbors: Vec<NodeId> = self
      .in_edges(node_id)
      .into_iter()
      .filter(|(e, _)| *e == etype)
      .map(|(_, src)| src)
      .collect();
    if let Some(mvcc) = self.mvcc.as_ref() {
      let tx_handle = self.current_tx_handle();
      if let Some(handle) = tx_handle.as_ref() {
        let tx = handle.lock();
        let mut tx_mgr = mvcc.tx_manager.lock();
        tx_mgr.record_read(
          tx.txid,
          TxKey::NeighborsIn {
            node_id,
            etype: Some(etype),
          },
        );
      }
    }
    neighbors
  }

  /// Check if there are any outgoing edges of a specific type
  pub fn has_out_edges(&self, node_id: NodeId, etype: ETypeId) -> bool {
    self.out_edges(node_id).iter().any(|(e, _)| *e == etype)
  }

  /// Check if there are any incoming edges of a specific type
  pub fn has_in_edges(&self, node_id: NodeId, etype: ETypeId) -> bool {
    self.in_edges(node_id).iter().any(|(e, _)| *e == etype)
  }

  // ========================================================================
  // Node Label Reads
  // ========================================================================

  /// Check if a node has a specific label
  pub fn node_has_label(&self, node_id: NodeId, label_id: LabelId) -> bool {
    let tx_handle = self.current_tx_handle();
    let tx_guard = tx_handle.as_ref().map(|tx| tx.lock());
    let pending = tx_guard.as_ref().map(|tx| &tx.pending);

    if pending.is_some_and(|p| p.is_node_removed(node_id)) {
      return false;
    }
    if pending.is_some_and(|p| p.is_label_removed(node_id, label_id)) {
      return false;
    }
    if pending.is_some_and(|p| p.is_label_added(node_id, label_id)) {
      return true;
    }
    // A node the transaction created (or recreated) has only the labels it added.
    if pending.is_some_and(|p| p.is_node_created(node_id)) {
      return false;
    }

    let (txid, tx_snapshot_ts) = self.mvcc_read_ts(tx_guard.as_deref());
    let has_label = self.committed_node_has_label(node_id, label_id, tx_snapshot_ts, txid);

    if let Some(mvcc) = self.mvcc.as_ref() {
      if txid != 0 {
        let mut tx_mgr = mvcc.tx_manager.lock();
        tx_mgr.record_read(txid, TxKey::NodeLabels(node_id));
        tx_mgr.record_read(txid, TxKey::NodeLabel { node_id, label_id });
      }
    }
    has_label
  }

  /// Label membership in committed state (MVCC versions, delta, snapshot), ignoring the
  /// current transaction's pending changes.
  fn committed_node_has_label(
    &self,
    node_id: NodeId,
    label_id: LabelId,
    tx_snapshot_ts: Timestamp,
    txid: TxId,
  ) -> bool {
    let delta = self.delta.read();

    let mut node_visible = None;
    if let Some(mvcc) = self.mvcc.as_ref() {
      let vc = mvcc.version_chain.lock();
      node_visible = vc
        .node_version(node_id)
        .map(|version| mvcc_node_exists(Some(version), tx_snapshot_ts, txid));
      if node_visible == Some(false) {
        return false;
      }
      if let Some(label_version) = vc.node_label_version(node_id, label_id) {
        if let Some(visible) = visible_version(&label_version, tx_snapshot_ts, txid) {
          return visible.data.unwrap_or(false);
        }
      }
    }

    // Check if node is deleted
    if node_visible.is_none() && delta.is_node_removed(node_id) {
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

    // A node created (or recreated) in the delta has no snapshot labels.
    if delta.is_node_created(node_id) {
      return false;
    }

    // Check snapshot for label (if present)
    if let Some(ref snapshot) = *self.snapshot.read() {
      if let Some(phys) = snapshot.phys_node(node_id) {
        if let Some(labels) = snapshot.node_labels(phys) {
          return labels.contains(&label_id);
        }
      }
    }

    false
  }

  /// Get all labels for a node
  pub fn node_labels(&self, node_id: NodeId) -> Vec<LabelId> {
    let tx_handle = self.current_tx_handle();
    let tx_guard = tx_handle.as_ref().map(|tx| tx.lock());
    let pending = tx_guard.as_ref().map(|tx| &tx.pending);

    if pending.is_some_and(|p| p.is_node_removed(node_id)) {
      return Vec::new();
    }

    let (txid, tx_snapshot_ts) = self.mvcc_read_ts(tx_guard.as_deref());
    let delta = self.delta.read();
    let snapshot = self.snapshot.read();
    let vc_guard = self.mvcc.as_ref().map(|mvcc| mvcc.version_chain.lock());
    let layers = NodeLayers {
      pending,
      delta: &delta,
    };

    // Check if node is deleted
    let node_visible = vc_guard
      .as_ref()
      .and_then(|vc| vc.node_version(node_id))
      .map(|version| mvcc_node_exists(Some(version), tx_snapshot_ts, txid));
    if !layers.sees_pending(node_id, node_visible) {
      return Vec::new();
    }

    let mut labels = std::collections::HashSet::new();

    // Load labels from snapshot first (if present)
    if let Some(ref snapshot) = *snapshot {
      if let Some(phys) = snapshot
        .phys_node(node_id)
        .filter(|_| layers.sees_snapshot(node_id, node_visible))
      {
        if let Some(snapshot_labels) = snapshot.node_labels(phys) {
          labels.extend(snapshot_labels);
        }
      }
    }

    // The committed delta and MVCC labels, unless the transaction recreated
    // the node.
    let committed = !layers.pending_masks(node_id);

    // Add labels from committed delta
    if let Some(added) = delta.added_labels(node_id).filter(|_| committed) {
      labels.extend(added.iter().copied());
    }

    // Remove labels deleted in committed delta
    if let Some(removed) = delta.removed_labels(node_id).filter(|_| committed) {
      for &label_id in removed {
        labels.remove(&label_id);
      }
    }

    if let Some(vc) = vc_guard.as_ref().filter(|_| committed) {
      for label_id in vc.node_label_keys(node_id) {
        if let Some(label_version) = vc.node_label_version(node_id, label_id) {
          if let Some(visible) = visible_version(&label_version, tx_snapshot_ts, txid) {
            match visible.data {
              Some(true) => {
                labels.insert(label_id);
              }
              _ => {
                labels.remove(&label_id);
              }
            }
          }
        }
      }
    }
    drop(vc_guard);

    // Apply pending label changes
    if let Some(pending_delta) = pending {
      if let Some(added) = pending_delta.added_labels(node_id) {
        labels.extend(added.iter().copied());
      }
      if let Some(removed) = pending_delta.removed_labels(node_id) {
        for &label_id in removed {
          labels.remove(&label_id);
        }
      }
    }

    let mut result: Vec<_> = labels.into_iter().collect();
    result.sort_unstable();
    if let Some(mvcc) = self.mvcc.as_ref() {
      if txid != 0 {
        let mut tx_mgr = mvcc.tx_manager.lock();
        tx_mgr.record_read(txid, TxKey::NodeLabels(node_id));
        for label_id in &result {
          tx_mgr.record_read(
            txid,
            TxKey::NodeLabel {
              node_id,
              label_id: *label_id,
            },
          );
        }
      }
    }
    result
  }

  // ========================================================================
  // Key Lookups
  // ========================================================================

  /// Look up a node by its key
  ///
  /// Returns the NodeId if found, None otherwise.
  /// Checks delta key index first, then falls back to snapshot.
  pub fn node_by_key(&self, key: &str) -> Option<NodeId> {
    let tx_handle = self.current_tx_handle();
    let tx_guard = tx_handle.as_ref().map(|tx| tx.lock());
    let pending = tx_guard.as_ref().map(|tx| &tx.pending);
    let (txid, tx_snapshot_ts) = self.mvcc_read_ts(tx_guard.as_deref());

    if let Some(mvcc) = self.mvcc.as_ref() {
      if txid != 0 {
        let mut tx_mgr = mvcc.tx_manager.lock();
        tx_mgr.record_read(txid, TxKey::Key(key.into()));
      }
    }

    let delta = self.delta.read();

    // Check pending key index first
    if pending.is_some_and(|p| p.key_index_deleted.contains(key)) {
      return None;
    }

    if let Some(&node_id) = pending.and_then(|p| p.key_index.get(key)) {
      if pending.is_some_and(|p| p.is_node_removed(node_id)) {
        return None;
      }
      // A node the transaction created (or recreated) is its own.
      if !pending.is_some_and(|p| p.is_node_created(node_id))
        && self.mvcc_node_visible(node_id, tx_snapshot_ts, txid) == Some(false)
      {
        return None;
      }
      return Some(node_id);
    }

    // Check committed delta key index
    if delta.key_index_deleted.contains(key) {
      return None;
    }

    if let Some(&node_id) = delta.key_index.get(key) {
      // Verify node isn't deleted
      if pending.is_some_and(|p| p.is_node_deleted(node_id)) {
        return None;
      }
      let node_visible = self.mvcc_node_visible(node_id, tx_snapshot_ts, txid);
      if node_visible == Some(false) {
        return None;
      }
      if node_visible == Some(true) || !delta.is_node_removed(node_id) {
        return Some(node_id);
      }
    }

    // Fall back to snapshot: a deleted or recreated node's snapshot key is
    // masked with the rest of its snapshot copy.
    let snapshot = self.snapshot.read();
    let layers = NodeLayers {
      pending,
      delta: &delta,
    };
    snapshot
      .as_ref()
      .and_then(|snap| snap.lookup_by_key(key))
      .filter(|&node_id| {
        let node_visible = self.mvcc_node_visible(node_id, tx_snapshot_ts, txid);
        layers.sees_snapshot(node_id, node_visible)
      })
  }

  /// Get the key for a node
  ///
  /// Returns the key string if the node has one, None otherwise.
  pub fn node_key(&self, node_id: NodeId) -> Option<String> {
    let tx_handle = self.current_tx_handle();
    let tx_guard = tx_handle.as_ref().map(|tx| tx.lock());
    let pending = tx_guard.as_ref().map(|tx| &tx.pending);

    if pending.is_some_and(|p| p.is_node_removed(node_id)) {
      return None;
    }

    if let Some(node_delta) = pending.and_then(|p| p.created_nodes.get(&node_id)) {
      return node_delta.key.clone();
    }

    let (txid, tx_snapshot_ts) = self.mvcc_read_ts(tx_guard.as_deref());
    let delta = self.delta.read();
    let node_visible = self.mvcc_node_visible(node_id, tx_snapshot_ts, txid);

    // Check if node is deleted
    if node_visible == Some(false) || (node_visible.is_none() && delta.is_node_removed(node_id)) {
      return None;
    }

    // Check created nodes in delta first
    if let Some(node_delta) = delta.created_nodes.get(&node_id) {
      return node_delta.key.clone();
    }

    // Fall back to snapshot
    let snapshot = self.snapshot.read();
    if let Some(ref snap) = *snapshot {
      if let Some(phys) = snap.phys_node(node_id) {
        return snap.node_key(phys);
      }
    }

    None
  }
}

#[cfg(test)]
mod tests {
  use crate::core::single_file::open::{
    close_single_file, open_single_file, SingleFileOpenOptions,
  };
  use crate::error::KiteError;
  use std::sync::{mpsc, Arc};
  use std::thread;
  use tempfile::tempdir;

  #[test]
  fn test_mvcc_label_visibility_across_transactions() {
    let temp_dir = tempdir().expect("expected value");
    let db_path = temp_dir.path().join("test-db");
    let db = Arc::new(
      open_single_file(db_path, SingleFileOpenOptions::new().mvcc(true)).expect("expected value"),
    );

    db.begin(false).expect("expected value");
    let node_id = db.create_node(Some("n1")).expect("expected value");
    let label_id = db.define_label("Tag").expect("expected value");
    db.commit().expect("expected value");

    let (ready_tx, ready_rx) = mpsc::channel();
    let (cont_tx, cont_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let db_reader = Arc::clone(&db);
    let handle = thread::spawn(move || {
      db_reader.begin(true).expect("expected value");
      assert!(!db_reader.node_has_label(node_id, label_id));
      assert!(db_reader.node_labels(node_id).is_empty());
      ready_tx.send(()).expect("expected value");
      cont_rx.recv().expect("expected value");
      assert!(!db_reader.node_has_label(node_id, label_id));
      assert!(db_reader.node_labels(node_id).is_empty());
      db_reader.commit().expect("expected value");
      done_tx.send(()).expect("expected value");
    });

    ready_rx.recv().expect("expected value");
    db.begin(false).expect("expected value");
    db.add_node_label(node_id, label_id)
      .expect("expected value");
    db.commit().expect("expected value");
    cont_tx.send(()).expect("expected value");
    done_rx.recv().expect("expected value");
    handle.join().expect("expected value");

    db.begin(true).expect("expected value");
    assert!(db.node_has_label(node_id, label_id));
    let labels = db.node_labels(node_id);
    assert!(labels.contains(&label_id));
    db.commit().expect("expected value");

    let (ready_tx2, ready_rx2) = mpsc::channel();
    let (cont_tx2, cont_rx2) = mpsc::channel();
    let (done_tx2, done_rx2) = mpsc::channel();
    let db_reader2 = Arc::clone(&db);
    let handle2 = thread::spawn(move || {
      db_reader2.begin(true).expect("expected value");
      assert!(db_reader2.node_has_label(node_id, label_id));
      assert!(db_reader2.node_labels(node_id).contains(&label_id));
      ready_tx2.send(()).expect("expected value");
      cont_rx2.recv().expect("expected value");
      assert!(db_reader2.node_has_label(node_id, label_id));
      assert!(db_reader2.node_labels(node_id).contains(&label_id));
      db_reader2.commit().expect("expected value");
      done_tx2.send(()).expect("expected value");
    });

    ready_rx2.recv().expect("expected value");
    db.begin(false).expect("expected value");
    db.remove_node_label(node_id, label_id)
      .expect("expected value");
    db.commit().expect("expected value");
    cont_tx2.send(()).expect("expected value");
    done_rx2.recv().expect("expected value");
    handle2.join().expect("expected value");

    db.begin(true).expect("expected value");
    assert!(!db.node_has_label(node_id, label_id));
    assert!(!db.node_labels(node_id).contains(&label_id));
    db.commit().expect("expected value");

    let db = match Arc::try_unwrap(db) {
      Ok(db) => db,
      Err(_) => panic!("single owner"),
    };
    close_single_file(db).expect("expected value");
  }

  #[test]
  fn test_mvcc_neighbor_read_conflicts_with_edge_write() {
    let temp_dir = tempdir().expect("expected value");
    let db_path = temp_dir.path().join("test-db");
    let db = Arc::new(
      open_single_file(db_path, SingleFileOpenOptions::new().mvcc(true)).expect("expected value"),
    );

    db.begin(false).expect("expected value");
    let src = db.create_node(Some("src")).expect("expected value");
    let dst = db.create_node(Some("dst")).expect("expected value");
    db.commit().expect("expected value");

    let (ready_tx, ready_rx) = mpsc::channel();
    let (cont_tx, cont_rx) = mpsc::channel();
    let db_reader = Arc::clone(&db);
    let handle = thread::spawn(move || {
      db_reader.begin(false).expect("expected value");
      let edges = db_reader.out_edges(src);
      assert!(edges.is_empty());
      ready_tx.send(()).expect("expected value");
      cont_rx.recv().expect("expected value");
      let result = db_reader.commit();
      match result {
        Err(KiteError::Conflict { keys, .. }) => {
          assert!(keys
            .iter()
            .any(|key| key == &format!("neighbors_out:{src}:*")));
        }
        other => panic!("expected conflict, got {other:?}"),
      }
    });

    ready_rx.recv().expect("expected value");
    db.begin(false).expect("expected value");
    db.add_edge_by_name(src, "Rel", dst)
      .expect("expected value");
    db.commit().expect("expected value");
    cont_tx.send(()).expect("expected value");
    handle.join().expect("expected value");

    let db = match Arc::try_unwrap(db) {
      Ok(db) => db,
      Err(_) => panic!("single owner"),
    };
    close_single_file(db).expect("expected value");
  }
}
