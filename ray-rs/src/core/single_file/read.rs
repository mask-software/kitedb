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
//!
//! # MVCC
//!
//! The committed delta and snapshot hold the latest committed state. The version chains only
//! hold history (see `mvcc::version_chain`): a read consults them first, and their `*_at`
//! lookups answer only for a reader whose snapshot predates a change, `None` otherwise. Reads
//! hold `delta` across those lookups, so a commit (which records its versions and merges into
//! the delta under `delta.write()`) lands completely before or after them. Enumerations add
//! what only the chains still hold: nodes, edges and keys deleted since the reader's snapshot.

use std::collections::HashMap;

use crate::types::*;

use super::{SingleFileDB, SingleFileTxState};

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

  /// Whether a node existed at the reader's snapshot, if it changed since (see the module
  /// docs); `None` when the delta and snapshot decide. Holds `version_chain` only for the
  /// lookup.
  fn mvcc_node_visible(
    &self,
    node_id: NodeId,
    tx_snapshot_ts: Timestamp,
    txid: TxId,
  ) -> Option<bool> {
    let mvcc = self.mvcc.as_ref()?;
    let vc = mvcc.version_chain.lock();
    vc.node_exists_at(node_id, tx_snapshot_ts, txid)
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

    if pending.is_some_and(|p| p.is_node_deleted(node_id)) {
      return None;
    }

    let (txid, tx_snapshot_ts) = self.mvcc_read_ts(tx_guard.as_deref());
    let delta = self.delta.read();

    let mut props = HashMap::new();
    let snapshot = self.snapshot.read();

    // Get properties from snapshot first
    if let Some(ref snap) = *snapshot {
      if let Some(phys) = snap.phys_node(node_id) {
        if let Some(snapshot_props) = snap.node_props(phys) {
          props = snapshot_props;
        }
      }
    }

    // Apply committed delta modifications
    if let Some(node_delta) = delta.node_delta(node_id) {
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

    let mut mvcc_node_visible = None;
    if let Some(mvcc) = self.mvcc.as_ref() {
      let vc = mvcc.version_chain.lock();
      mvcc_node_visible = vc.node_exists_at(node_id, tx_snapshot_ts, txid);
      for key_id in vc.node_prop_keys(node_id) {
        if let Some(value) = vc.node_prop_at(node_id, key_id, tx_snapshot_ts, txid) {
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
    } else if delta.is_node_deleted(node_id) {
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
      if tx.pending.is_node_deleted(node_id) {
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

    let delta = self.delta.read();

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
      mvcc_node_visible = vc.node_exists_at(node_id, tx_snapshot_ts, txid);
      if mvcc_node_visible != Some(false) {
        if let Some(value) = vc.node_prop_at(node_id, key_id, tx_snapshot_ts, txid) {
          return value.as_deref().cloned();
        }
      }
    }

    // Check if node is deleted (unless MVCC snapshot says otherwise)
    if mvcc_node_visible == Some(false) {
      return None;
    }
    if mvcc_node_visible.is_none() && delta.is_node_deleted(node_id) {
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

    // Fall back to snapshot
    let snapshot = self.snapshot.read();
    if let Some(ref snap) = *snapshot {
      if let Some(phys) = snap.phys_node(node_id) {
        return snap.node_prop(phys, key_id);
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
  pub fn edge_props(
    &self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
  ) -> Option<HashMap<PropKeyId, PropValue>> {
    let tx_handle = self.current_tx_handle();
    let tx_guard = tx_handle.as_ref().map(|tx| tx.lock());
    let pending = tx_guard.as_ref().map(|tx| &tx.pending);

    if pending.is_some_and(|p| p.is_node_deleted(src) || p.is_node_deleted(dst)) {
      return None;
    }
    if pending.is_some_and(|p| p.is_edge_deleted(src, etype, dst)) {
      return None;
    }

    let (txid, tx_snapshot_ts) = self.mvcc_read_ts(tx_guard.as_deref());
    let delta = self.delta.read();

    let mut mvcc_src_visible = None;
    let mut mvcc_dst_visible = None;
    let mut mvcc_edge_visible = None;

    let mut props = HashMap::new();
    let snapshot = self.snapshot.read();

    // First, determine if edge exists
    let edge_added_in_delta = delta.is_edge_added(src, etype, dst);
    let edge_added_in_pending = pending.is_some_and(|p| p.is_edge_added(src, etype, dst));
    let mut edge_exists_in_snapshot = false;

    // Check snapshot for edge existence and get base properties
    if let Some(ref snap) = *snapshot {
      if let Some(src_phys) = snap.phys_node(src) {
        if let Some(dst_phys) = snap.phys_node(dst) {
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

    // Apply committed delta modifications, then what changed since the reader's snapshot
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

    if let Some(mvcc) = self.mvcc.as_ref() {
      let vc = mvcc.version_chain.lock();
      mvcc_src_visible = vc.node_exists_at(src, tx_snapshot_ts, txid);
      mvcc_dst_visible = vc.node_exists_at(dst, tx_snapshot_ts, txid);
      mvcc_edge_visible = vc.edge_exists_at(src, etype, dst, tx_snapshot_ts, txid);
      for key_id in vc.edge_prop_keys(src, etype, dst) {
        if let Some(value) = vc.edge_prop_at(src, etype, dst, key_id, tx_snapshot_ts, txid) {
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

    if mvcc_src_visible == Some(false) || mvcc_dst_visible == Some(false) {
      return None;
    }
    if mvcc_src_visible.is_none() && delta.is_node_deleted(src) {
      return None;
    }
    if mvcc_dst_visible.is_none() && delta.is_node_deleted(dst) {
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
      if tx.pending.is_node_deleted(src) || tx.pending.is_node_deleted(dst) {
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
    }

    let delta = self.delta.read();

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
      mvcc_src_visible = vc.node_exists_at(src, tx_snapshot_ts, txid);
      mvcc_dst_visible = vc.node_exists_at(dst, tx_snapshot_ts, txid);
      mvcc_edge_visible = vc.edge_exists_at(src, etype, dst, tx_snapshot_ts, txid);
      let gone = [mvcc_src_visible, mvcc_dst_visible, mvcc_edge_visible].contains(&Some(false));
      if !gone {
        if let Some(value) = vc.edge_prop_at(src, etype, dst, key_id, tx_snapshot_ts, txid) {
          return value.as_deref().cloned();
        }
      }
    }

    if mvcc_src_visible == Some(false) || mvcc_dst_visible == Some(false) {
      return None;
    }

    // Check if either node is deleted
    if mvcc_src_visible.is_none() && delta.is_node_deleted(src) {
      return None;
    }
    if mvcc_dst_visible.is_none() && delta.is_node_deleted(dst) {
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
    let edge_exists_in_snapshot = if let Some(ref snap) = *snapshot {
      if let Some(src_phys) = snap.phys_node(src) {
        if let Some(dst_phys) = snap.phys_node(dst) {
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
    if let Some(ref snap) = *snapshot {
      if let Some(src_phys) = snap.phys_node(src) {
        if let Some(dst_phys) = snap.phys_node(dst) {
          if let Some(edge_idx) = snap.find_edge_index(src_phys, etype, dst_phys) {
            // Get property from snapshot
            if let Some(snapshot_props) = snap.edge_props(edge_idx) {
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
  pub fn out_edges(&self, node_id: NodeId) -> Vec<(ETypeId, NodeId)> {
    let tx_handle = self.current_tx_handle();
    let tx_guard = tx_handle.as_ref().map(|tx| tx.lock());
    let pending = tx_guard.as_ref().map(|tx| &tx.pending);

    // If node is deleted, no edges
    if pending.is_some_and(|p| p.is_node_deleted(node_id)) {
      return Vec::new();
    }

    let (txid, tx_snapshot_ts) = self.mvcc_read_ts(tx_guard.as_deref());
    let delta = self.delta.read();
    let snapshot = self.snapshot.read();
    let vc_guard = self.mvcc.as_ref().map(|mvcc| mvcc.version_chain.lock());

    // If node is deleted in committed state, no edges
    let node_visible = vc_guard
      .as_ref()
      .and_then(|vc| vc.node_exists_at(node_id, tx_snapshot_ts, txid));
    if node_visible == Some(false) || (node_visible.is_none() && delta.is_node_deleted(node_id)) {
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
      if let Some(phys) = snap.phys_node(node_id) {
        for (dst_phys, etype) in snap.iter_out_edges(phys) {
          // Convert physical dst to NodeId
          if let Some(dst_node_id) = snap.node_id(dst_phys) {
            // Skip edges to deleted nodes
            let dst_visible = vc_guard
              .as_ref()
              .and_then(|vc| vc.node_exists_at(dst_node_id, tx_snapshot_ts, txid));
            if dst_visible == Some(false)
              || pending.is_some_and(|p| p.is_node_deleted(dst_node_id))
              || (dst_visible.is_none() && delta.is_node_deleted(dst_node_id))
            {
              continue;
            }
            // Skip edges deleted in delta
            let edge_visible = vc_guard
              .as_ref()
              .and_then(|vc| vc.edge_exists_at(node_id, etype, dst_node_id, tx_snapshot_ts, txid));
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
    if let Some(added_edges) = delta.out_add.get(&node_id) {
      for edge_patch in added_edges {
        // Skip edges to deleted nodes
        let dst_visible = vc_guard
          .as_ref()
          .and_then(|vc| vc.node_exists_at(edge_patch.other, tx_snapshot_ts, txid));
        if dst_visible == Some(false)
          || pending.is_some_and(|p| p.is_node_deleted(edge_patch.other))
          || (dst_visible.is_none() && delta.is_node_deleted(edge_patch.other))
        {
          continue;
        }
        let edge_visible = vc_guard.as_ref().and_then(|vc| {
          vc.edge_exists_at(
            node_id,
            edge_patch.etype,
            edge_patch.other,
            tx_snapshot_ts,
            txid,
          )
        });
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
          .and_then(|vc| vc.node_exists_at(edge_patch.other, tx_snapshot_ts, txid));
        if dst_visible == Some(false)
          || pending.is_some_and(|p| p.is_node_deleted(edge_patch.other))
          || (dst_visible.is_none() && delta.is_node_deleted(edge_patch.other))
        {
          continue;
        }
        edges.push((edge_patch.etype, edge_patch.other));
      }
    }

    // Edges deleted since the reader's snapshot remain only in their version chains
    if let Some(vc) = vc_guard.as_ref() {
      for (src, etype, dst) in vc.node_edges_at(node_id, tx_snapshot_ts, txid) {
        if src != node_id
          || pending.is_some_and(|p| p.is_node_deleted(dst) || p.is_edge_deleted(src, etype, dst))
        {
          continue;
        }
        let dst_visible = vc.node_exists_at(dst, tx_snapshot_ts, txid);
        if dst_visible == Some(false) || (dst_visible.is_none() && delta.is_node_deleted(dst)) {
          continue;
        }
        edges.push((etype, dst));
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
    if pending.is_some_and(|p| p.is_node_deleted(node_id)) {
      return Vec::new();
    }

    let (txid, tx_snapshot_ts) = self.mvcc_read_ts(tx_guard.as_deref());
    let delta = self.delta.read();
    let snapshot = self.snapshot.read();
    let vc_guard = self.mvcc.as_ref().map(|mvcc| mvcc.version_chain.lock());

    // If node is deleted, no edges
    let node_visible = vc_guard
      .as_ref()
      .and_then(|vc| vc.node_exists_at(node_id, tx_snapshot_ts, txid));
    if node_visible == Some(false) || (node_visible.is_none() && delta.is_node_deleted(node_id)) {
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
      if let Some(phys) = snap.phys_node(node_id) {
        for (src_phys, etype, _out_index) in snap.iter_in_edges(phys) {
          // Convert physical src to NodeId
          if let Some(src_node_id) = snap.node_id(src_phys) {
            // Skip edges from deleted nodes
            let src_visible = vc_guard
              .as_ref()
              .and_then(|vc| vc.node_exists_at(src_node_id, tx_snapshot_ts, txid));
            if src_visible == Some(false)
              || pending.is_some_and(|p| p.is_node_deleted(src_node_id))
              || (src_visible.is_none() && delta.is_node_deleted(src_node_id))
            {
              continue;
            }
            // Skip edges deleted in delta
            let edge_visible = vc_guard
              .as_ref()
              .and_then(|vc| vc.edge_exists_at(src_node_id, etype, node_id, tx_snapshot_ts, txid));
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
    if let Some(added_edges) = delta.in_add.get(&node_id) {
      for edge_patch in added_edges {
        // Skip edges from deleted nodes
        let src_visible = vc_guard
          .as_ref()
          .and_then(|vc| vc.node_exists_at(edge_patch.other, tx_snapshot_ts, txid));
        if src_visible == Some(false)
          || pending.is_some_and(|p| p.is_node_deleted(edge_patch.other))
          || (src_visible.is_none() && delta.is_node_deleted(edge_patch.other))
        {
          continue;
        }
        let edge_visible = vc_guard.as_ref().and_then(|vc| {
          vc.edge_exists_at(
            edge_patch.other,
            edge_patch.etype,
            node_id,
            tx_snapshot_ts,
            txid,
          )
        });
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
          .and_then(|vc| vc.node_exists_at(edge_patch.other, tx_snapshot_ts, txid));
        if src_visible == Some(false)
          || pending.is_some_and(|p| p.is_node_deleted(edge_patch.other))
          || (src_visible.is_none() && delta.is_node_deleted(edge_patch.other))
        {
          continue;
        }
        edges.push((edge_patch.etype, edge_patch.other));
      }
    }

    // Edges deleted since the reader's snapshot remain only in their version chains
    if let Some(vc) = vc_guard.as_ref() {
      for (src, etype, dst) in vc.node_edges_at(node_id, tx_snapshot_ts, txid) {
        if dst != node_id
          || pending.is_some_and(|p| p.is_node_deleted(src) || p.is_edge_deleted(src, etype, dst))
        {
          continue;
        }
        let src_visible = vc.node_exists_at(src, tx_snapshot_ts, txid);
        if src_visible == Some(false) || (src_visible.is_none() && delta.is_node_deleted(src)) {
          continue;
        }
        edges.push((etype, src));
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

    if pending.is_some_and(|p| p.is_node_deleted(node_id)) {
      return false;
    }
    if pending.is_some_and(|p| p.is_label_removed(node_id, label_id)) {
      return false;
    }
    if pending.is_some_and(|p| p.is_label_added(node_id, label_id)) {
      return true;
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
      node_visible = vc.node_exists_at(node_id, tx_snapshot_ts, txid);
      if node_visible == Some(false) {
        return false;
      }
      if let Some(has_label) = vc.node_label_at(node_id, label_id, tx_snapshot_ts, txid) {
        return has_label;
      }
    }

    // Check if node is deleted
    if node_visible.is_none() && delta.is_node_deleted(node_id) {
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

    if pending.is_some_and(|p| p.is_node_deleted(node_id)) {
      return Vec::new();
    }

    let (txid, tx_snapshot_ts) = self.mvcc_read_ts(tx_guard.as_deref());
    let delta = self.delta.read();
    let snapshot = self.snapshot.read();
    let vc_guard = self.mvcc.as_ref().map(|mvcc| mvcc.version_chain.lock());

    // Check if node is deleted
    let node_visible = vc_guard
      .as_ref()
      .and_then(|vc| vc.node_exists_at(node_id, tx_snapshot_ts, txid));
    if node_visible == Some(false) || (node_visible.is_none() && delta.is_node_deleted(node_id)) {
      return Vec::new();
    }

    let mut labels = std::collections::HashSet::new();

    // Load labels from snapshot first (if present)
    if let Some(ref snapshot) = *snapshot {
      if let Some(phys) = snapshot.phys_node(node_id) {
        if let Some(snapshot_labels) = snapshot.node_labels(phys) {
          labels.extend(snapshot_labels);
        }
      }
    }

    // Add labels from committed delta
    if let Some(added) = delta.added_labels(node_id) {
      labels.extend(added.iter().copied());
    }

    // Remove labels deleted in committed delta
    if let Some(removed) = delta.removed_labels(node_id) {
      for &label_id in removed {
        labels.remove(&label_id);
      }
    }

    if let Some(vc) = vc_guard.as_ref() {
      for label_id in vc.node_label_keys(node_id) {
        match vc.node_label_at(node_id, label_id, tx_snapshot_ts, txid) {
          Some(true) => {
            labels.insert(label_id);
          }
          Some(false) => {
            labels.remove(&label_id);
          }
          None => {}
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
      if pending.is_some_and(|p| p.is_node_deleted(node_id)) {
        return None;
      }
      if self.mvcc_node_visible(node_id, tx_snapshot_ts, txid) == Some(false) {
        return None;
      }
      return Some(node_id);
    }

    // The key's owner at the reader's snapshot, if it changed since
    if let Some(mvcc) = self.mvcc.as_ref() {
      let owner = mvcc
        .version_chain
        .lock()
        .key_owner_at(key, tx_snapshot_ts, txid);
      if let Some(owner) = owner {
        return owner.filter(|&node_id| !pending.is_some_and(|p| p.is_node_deleted(node_id)));
      }
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
      if node_visible == Some(true) || !delta.is_node_deleted(node_id) {
        return Some(node_id);
      }
    }

    // Fall back to snapshot
    let snapshot = self.snapshot.read();
    if let Some(ref snap) = *snapshot {
      if let Some(node_id) = snap.lookup_by_key(key) {
        if pending.is_some_and(|p| p.is_node_deleted(node_id)) {
          return None;
        }
        let node_visible = self.mvcc_node_visible(node_id, tx_snapshot_ts, txid);
        if node_visible == Some(false) {
          return None;
        }
        if node_visible == Some(true) || !delta.is_node_deleted(node_id) {
          return Some(node_id);
        }
      }
    }

    None
  }

  /// Get the key for a node
  ///
  /// Returns the key string if the node has one, None otherwise.
  pub fn node_key(&self, node_id: NodeId) -> Option<String> {
    let tx_handle = self.current_tx_handle();
    let tx_guard = tx_handle.as_ref().map(|tx| tx.lock());
    let pending = tx_guard.as_ref().map(|tx| &tx.pending);

    if pending.is_some_and(|p| p.is_node_deleted(node_id)) {
      return None;
    }

    if let Some(node_delta) = pending.and_then(|p| p.created_nodes.get(&node_id)) {
      return node_delta.key.clone();
    }

    let (txid, tx_snapshot_ts) = self.mvcc_read_ts(tx_guard.as_deref());
    let delta = self.delta.read();

    // The node at the reader's snapshot, with its key, if it changed since
    if let Some(mvcc) = self.mvcc.as_ref() {
      let vc = mvcc.version_chain.lock();
      if let Some(node) = vc.node_at(node_id, tx_snapshot_ts, txid) {
        return node.and_then(|node| node.delta.key.clone());
      }
    }

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
