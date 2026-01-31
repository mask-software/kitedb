//! Edge CRUD operations
//!
//! Provides functions for adding, deleting, and querying edges.

use crate::error::{RayError, Result};
use crate::mvcc::visibility::{edge_exists as mvcc_edge_exists, get_visible_version};
use crate::types::*;

use super::tx::TxHandle;

// ============================================================================
// Edge Operations
// ============================================================================

/// Add an edge between two nodes
pub fn add_edge(handle: &mut TxHandle, src: NodeId, etype: ETypeId, dst: NodeId) -> Result<()> {
  if handle.tx.read_only {
    return Err(RayError::ReadOnly);
  }

  let patch = EdgePatch { etype, other: dst };

  if let Some(del_set) = handle.tx.pending_out_del.get_mut(&src) {
    if del_set.remove(&patch) {
      if del_set.is_empty() {
        handle.tx.pending_out_del.remove(&src);
      }
      if let Some(in_del) = handle.tx.pending_in_del.get_mut(&dst) {
        in_del.remove(&EdgePatch { etype, other: src });
        if in_del.is_empty() {
          handle.tx.pending_in_del.remove(&dst);
        }
      }
      return Ok(());
    }
  }

  handle
    .tx
    .pending_out_add
    .entry(src)
    .or_default()
    .insert(patch);
  handle
    .tx
    .pending_in_add
    .entry(dst)
    .or_default()
    .insert(EdgePatch { etype, other: src });

  if let Some(mvcc) = handle.db.mvcc.as_ref() {
    let mut tx_mgr = mvcc.tx_manager.lock();
    tx_mgr.record_write(handle.tx.txid, format!("edge:{src}:{etype}:{dst}"));
  }

  Ok(())
}

/// Delete an edge between two nodes
pub fn delete_edge(
  handle: &mut TxHandle,
  src: NodeId,
  etype: ETypeId,
  dst: NodeId,
) -> Result<bool> {
  if handle.tx.read_only {
    return Err(RayError::ReadOnly);
  }

  if !edge_exists_internal(handle.db, src, etype, dst) {
    return Ok(false);
  }

  let patch = EdgePatch { etype, other: dst };
  if let Some(add_set) = handle.tx.pending_out_add.get_mut(&src) {
    if add_set.remove(&patch) {
      if add_set.is_empty() {
        handle.tx.pending_out_add.remove(&src);
      }
      if let Some(in_add) = handle.tx.pending_in_add.get_mut(&dst) {
        in_add.remove(&EdgePatch { etype, other: src });
        if in_add.is_empty() {
          handle.tx.pending_in_add.remove(&dst);
        }
      }
      return Ok(true);
    }
  }
  handle
    .tx
    .pending_out_del
    .entry(src)
    .or_default()
    .insert(patch);
  handle
    .tx
    .pending_in_del
    .entry(dst)
    .or_default()
    .insert(EdgePatch { etype, other: src });

  if let Some(mvcc) = handle.db.mvcc.as_ref() {
    let mut tx_mgr = mvcc.tx_manager.lock();
    tx_mgr.record_write(handle.tx.txid, format!("edge:{src}:{etype}:{dst}"));
  }

  Ok(true)
}

/// Check if an edge exists
pub fn edge_exists(handle: &TxHandle, src: NodeId, etype: ETypeId, dst: NodeId) -> bool {
  // Check pending transaction changes first
  if let Some(add_set) = handle.tx.pending_out_add.get(&src) {
    if add_set.contains(&EdgePatch { etype, other: dst }) {
      return true;
    }
  }
  if let Some(del_set) = handle.tx.pending_out_del.get(&src) {
    if del_set.contains(&EdgePatch { etype, other: dst }) {
      return false;
    }
  }

  if let Some(mvcc) = handle.db.mvcc.as_ref() {
    let tx_snapshot_ts = handle.tx.snapshot_ts;
    let txid = handle.tx.txid;
    {
      let mut tx_mgr = mvcc.tx_manager.lock();
      tx_mgr.record_read(txid, format!("edge:{src}:{etype}:{dst}"));
    }
    let vc = mvcc.version_chain.lock();
    if let Some(version) = vc.get_edge_version(src, etype, dst) {
      return mvcc_edge_exists(Some(version), tx_snapshot_ts, txid);
    }
  }

  edge_exists_internal(handle.db, src, etype, dst)
}

/// Internal edge existence check
fn edge_exists_internal(db: &super::db::GraphDB, src: NodeId, etype: ETypeId, dst: NodeId) -> bool {
  edge_exists_db(db, src, etype, dst)
}

// ============================================================================
// Direct Read Functions (No Transaction Required)
// ============================================================================
// These functions read directly from snapshot + delta without transaction
// overhead, matching the TypeScript implementation pattern.

/// Check if an edge exists (direct read, no transaction)
pub fn edge_exists_db(db: &super::db::GraphDB, src: NodeId, etype: ETypeId, dst: NodeId) -> bool {
  if let Some(mvcc) = db.mvcc.as_ref() {
    // Fast-path: no active transactions and no versions
    let tx_snapshot_ts = mvcc.tx_manager.lock().get_next_commit_ts();
    let txid = 0;
    let vc = mvcc.version_chain.lock();
    if let Some(version) = vc.get_edge_version(src, etype, dst) {
      return mvcc_edge_exists(Some(version), tx_snapshot_ts, txid);
    }
  }

  let delta = db.delta.read();

  // Check if deleted in delta
  if delta.is_edge_deleted(src, etype, dst) {
    return false;
  }

  // Check if added in delta
  if delta.is_edge_added(src, etype, dst) {
    return true;
  }

  // Check snapshot
  if let Some(ref snapshot) = db.snapshot {
    if let (Some(src_phys), Some(dst_phys)) =
      (snapshot.get_phys_node(src), snapshot.get_phys_node(dst))
    {
      return snapshot.has_edge(src_phys, etype, dst_phys);
    }
  }

  false
}

/// Get outgoing neighbors for a node (direct read, no transaction)
pub fn get_neighbors_out_db(
  db: &super::db::GraphDB,
  src: NodeId,
  etype: Option<ETypeId>,
) -> Vec<NodeId> {
  let delta = db.delta.read();
  let mut neighbors = Vec::new();

  let deleted_set = delta.out_del.get(&src);

  // Get from snapshot first
  if let Some(ref snapshot) = db.snapshot {
    if let Some(src_phys) = snapshot.get_phys_node(src) {
      for (dst_phys, edge_etype) in snapshot.iter_out_edges(src_phys) {
        if etype.is_some() && etype != Some(edge_etype) {
          continue;
        }

        if let Some(dst_id) = snapshot.get_node_id(dst_phys) {
          let is_deleted = deleted_set
            .map(|set| {
              set.contains(&crate::types::EdgePatch {
                etype: edge_etype,
                other: dst_id,
              })
            })
            .unwrap_or(false);

          if !is_deleted {
            neighbors.push(dst_id);
          }
        }
      }
    }
  }

  // Get from delta additions
  if let Some(add_set) = delta.out_add.get(&src) {
    for patch in add_set {
      if (etype.is_none() || etype == Some(patch.etype)) && !neighbors.contains(&patch.other) {
        neighbors.push(patch.other);
      }
    }
  }

  neighbors
}

/// Get incoming neighbors for a node (direct read, no transaction)
pub fn get_neighbors_in_db(
  db: &super::db::GraphDB,
  dst: NodeId,
  etype: Option<ETypeId>,
) -> Vec<NodeId> {
  let delta = db.delta.read();
  let mut neighbors = Vec::new();

  let deleted_set = delta.in_del.get(&dst);

  // Get from snapshot first
  if let Some(ref snapshot) = db.snapshot {
    if let Some(dst_phys) = snapshot.get_phys_node(dst) {
      for (src_phys, edge_etype, _out_idx) in snapshot.iter_in_edges(dst_phys) {
        if etype.is_some() && etype != Some(edge_etype) {
          continue;
        }

        if let Some(src_id) = snapshot.get_node_id(src_phys) {
          let is_deleted = deleted_set
            .map(|set| {
              set.contains(&crate::types::EdgePatch {
                etype: edge_etype,
                other: src_id,
              })
            })
            .unwrap_or(false);

          if !is_deleted {
            neighbors.push(src_id);
          }
        }
      }
    }
  }

  // Get from delta additions
  if let Some(add_set) = delta.in_add.get(&dst) {
    for patch in add_set {
      if (etype.is_none() || etype == Some(patch.etype)) && !neighbors.contains(&patch.other) {
        neighbors.push(patch.other);
      }
    }
  }

  neighbors
}

/// Get an edge property (direct read, no transaction)
pub fn get_edge_prop_db(
  db: &super::db::GraphDB,
  src: NodeId,
  etype: ETypeId,
  dst: NodeId,
  key_id: PropKeyId,
) -> Option<PropValue> {
  if let Some(mvcc) = db.mvcc.as_ref() {
    let tx_snapshot_ts = mvcc.tx_manager.lock().get_next_commit_ts();
    let txid = 0;
    let vc = mvcc.version_chain.lock();
    if let Some(prop_version) = vc.get_edge_prop_version(src, etype, dst, key_id) {
      if let Some(visible) = get_visible_version(&prop_version, tx_snapshot_ts, txid) {
        return visible.data.clone();
      }
    }
  }

  get_edge_prop_committed(db, src, etype, dst, key_id)
}

/// Get an edge property from committed state only (snapshot + delta)
pub fn get_edge_prop_committed(
  db: &super::db::GraphDB,
  src: NodeId,
  etype: ETypeId,
  dst: NodeId,
  key_id: PropKeyId,
) -> Option<PropValue> {
  let delta = db.delta.read();

  if delta.is_edge_deleted(src, etype, dst) {
    return None;
  }

  // Check delta first
  if let Some(delta_props) = delta.edge_props.get(&(src, etype, dst)) {
    if let Some(value) = delta_props.get(&key_id) {
      return value.clone();
    }
  }

  let edge_added_in_delta = delta.is_edge_added(src, etype, dst);

  // Fall back to snapshot
  if let Some(ref snapshot) = db.snapshot {
    if let Some(src_phys) = snapshot.get_phys_node(src) {
      if let Some(dst_phys) = snapshot.get_phys_node(dst) {
        if let Some(edge_idx) = snapshot.find_edge_index(src_phys, etype, dst_phys) {
          if let Some(snapshot_props) = snapshot.get_edge_props(edge_idx) {
            if let Some(value) = snapshot_props.get(&key_id) {
              return Some(value.clone());
            }
          }
        } else if !edge_added_in_delta {
          return None;
        }
      }
    }
  }

  None
}

/// Get all edge properties (direct read, no transaction)
pub fn get_edge_props_db(
  db: &super::db::GraphDB,
  src: NodeId,
  etype: ETypeId,
  dst: NodeId,
) -> Option<std::collections::HashMap<PropKeyId, PropValue>> {
  use std::collections::HashMap;

  let delta = db.delta.read();

  if delta.is_edge_deleted(src, etype, dst) {
    return None;
  }

  let mut props = HashMap::new();
  let edge_added_in_delta = delta.is_edge_added(src, etype, dst);
  let mut edge_exists_in_snapshot = false;

  // Check snapshot
  if let Some(ref snapshot) = db.snapshot {
    if let Some(src_phys) = snapshot.get_phys_node(src) {
      if let Some(dst_phys) = snapshot.get_phys_node(dst) {
        if let Some(edge_idx) = snapshot.find_edge_index(src_phys, etype, dst_phys) {
          edge_exists_in_snapshot = true;
          if let Some(snapshot_props) = snapshot.get_edge_props(edge_idx) {
            props = snapshot_props;
          }
        }
      }
    }
  }

  if !edge_added_in_delta && !edge_exists_in_snapshot {
    return None;
  }

  // Apply delta modifications
  if let Some(delta_props) = delta.edge_props.get(&(src, etype, dst)) {
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

/// Get outgoing neighbors for a node
pub fn get_neighbors_out(handle: &TxHandle, src: NodeId, etype: Option<ETypeId>) -> Vec<NodeId> {
  let mut neighbors = get_neighbors_out_db(handle.db, src, etype);

  if let Some(del_set) = handle.tx.pending_out_del.get(&src) {
    neighbors.retain(|dst| {
      if let Some(filter_etype) = etype {
        !del_set.contains(&EdgePatch {
          etype: filter_etype,
          other: *dst,
        })
      } else {
        !del_set.iter().any(|patch| patch.other == *dst)
      }
    });
  }

  if let Some(add_set) = handle.tx.pending_out_add.get(&src) {
    for patch in add_set {
      if (etype.is_none() || etype == Some(patch.etype)) && !neighbors.contains(&patch.other) {
        neighbors.push(patch.other);
      }
    }
  }

  if let Some(mvcc) = handle.db.mvcc.as_ref() {
    let mut tx_mgr = mvcc.tx_manager.lock();
    let etype_key = etype
      .map(|e| e.to_string())
      .unwrap_or_else(|| "*".to_string());
    tx_mgr.record_read(handle.tx.txid, format!("neighbors_out:{src}:{etype_key}"));
  }

  neighbors
}

/// Get incoming neighbors for a node
pub fn get_neighbors_in(handle: &TxHandle, dst: NodeId, etype: Option<ETypeId>) -> Vec<NodeId> {
  let mut neighbors = get_neighbors_in_db(handle.db, dst, etype);

  if let Some(del_set) = handle.tx.pending_in_del.get(&dst) {
    neighbors.retain(|src_id| {
      if let Some(filter_etype) = etype {
        !del_set.contains(&EdgePatch {
          etype: filter_etype,
          other: *src_id,
        })
      } else {
        !del_set.iter().any(|patch| patch.other == *src_id)
      }
    });
  }

  if let Some(add_set) = handle.tx.pending_in_add.get(&dst) {
    for patch in add_set {
      if (etype.is_none() || etype == Some(patch.etype)) && !neighbors.contains(&patch.other) {
        neighbors.push(patch.other);
      }
    }
  }

  if let Some(mvcc) = handle.db.mvcc.as_ref() {
    let mut tx_mgr = mvcc.tx_manager.lock();
    let etype_key = etype
      .map(|e| e.to_string())
      .unwrap_or_else(|| "*".to_string());
    tx_mgr.record_read(handle.tx.txid, format!("neighbors_in:{dst}:{etype_key}"));
  }

  neighbors
}

// ============================================================================
// Edge Property Operations
// ============================================================================

/// Set a property on an edge
pub fn set_edge_prop(
  handle: &mut TxHandle,
  src: NodeId,
  etype: ETypeId,
  dst: NodeId,
  key_id: PropKeyId,
  value: PropValue,
) -> Result<()> {
  if handle.tx.read_only {
    return Err(RayError::ReadOnly);
  }

  let props = handle
    .tx
    .pending_edge_props
    .entry((src, etype, dst))
    .or_default();
  props.insert(key_id, Some(value));

  if let Some(mvcc) = handle.db.mvcc.as_ref() {
    let mut tx_mgr = mvcc.tx_manager.lock();
    tx_mgr.record_write(
      handle.tx.txid,
      format!("edgeprop:{src}:{etype}:{dst}:{key_id}"),
    );
  }

  Ok(())
}

/// Delete a property from an edge
pub fn del_edge_prop(
  handle: &mut TxHandle,
  src: NodeId,
  etype: ETypeId,
  dst: NodeId,
  key_id: PropKeyId,
) -> Result<()> {
  if handle.tx.read_only {
    return Err(RayError::ReadOnly);
  }

  let props = handle
    .tx
    .pending_edge_props
    .entry((src, etype, dst))
    .or_default();
  props.insert(key_id, None);

  if let Some(mvcc) = handle.db.mvcc.as_ref() {
    let mut tx_mgr = mvcc.tx_manager.lock();
    tx_mgr.record_write(
      handle.tx.txid,
      format!("edgeprop:{src}:{etype}:{dst}:{key_id}"),
    );
  }

  Ok(())
}

/// Get a property from an edge
pub fn get_edge_prop(
  handle: &TxHandle,
  src: NodeId,
  etype: ETypeId,
  dst: NodeId,
  key_id: PropKeyId,
) -> Option<PropValue> {
  if let Some(pending) = handle.tx.pending_edge_props.get(&(src, etype, dst)) {
    if let Some(value) = pending.get(&key_id) {
      return value.clone();
    }
  }

  if let Some(mvcc) = handle.db.mvcc.as_ref() {
    let tx_snapshot_ts = handle.tx.snapshot_ts;
    let txid = handle.tx.txid;
    {
      let mut tx_mgr = mvcc.tx_manager.lock();
      tx_mgr.record_read(txid, format!("edgeprop:{src}:{etype}:{dst}:{key_id}"));
    }
    let vc = mvcc.version_chain.lock();
    if let Some(prop_version) = vc.get_edge_prop_version(src, etype, dst, key_id) {
      if let Some(visible) = get_visible_version(&prop_version, tx_snapshot_ts, txid) {
        return visible.data.clone();
      }
    }
  }

  let delta = handle.db.delta.read();

  if delta.is_edge_deleted(src, etype, dst) {
    return None;
  }

  if let Some(delta_props) = delta.edge_props.get(&(src, etype, dst)) {
    if let Some(value) = delta_props.get(&key_id) {
      return value.clone();
    }
  }

  let edge_added_in_delta = delta.is_edge_added(src, etype, dst);

  if let Some(ref snapshot) = handle.db.snapshot {
    if let Some(src_phys) = snapshot.get_phys_node(src) {
      if let Some(dst_phys) = snapshot.get_phys_node(dst) {
        if let Some(edge_idx) = snapshot.find_edge_index(src_phys, etype, dst_phys) {
          if let Some(snapshot_props) = snapshot.get_edge_props(edge_idx) {
            if let Some(value) = snapshot_props.get(&key_id) {
              return Some(value.clone());
            }
          }
        } else if !edge_added_in_delta {
          return None;
        }
      }
    }
  }

  None
}

/// Get all properties from an edge
pub fn get_edge_props(
  handle: &TxHandle,
  src: NodeId,
  etype: ETypeId,
  dst: NodeId,
) -> Option<std::collections::HashMap<PropKeyId, PropValue>> {
  use std::collections::HashMap;

  let delta = handle.db.delta.read();

  if delta.is_edge_deleted(src, etype, dst) {
    return None;
  }

  if let Some(del_set) = handle.tx.pending_out_del.get(&src) {
    if del_set.contains(&EdgePatch { etype, other: dst }) {
      return None;
    }
  }

  let mut props = HashMap::new();
  let edge_added_in_delta = delta.is_edge_added(src, etype, dst);
  let edge_added_in_tx = handle
    .tx
    .pending_out_add
    .get(&src)
    .map(|set| set.contains(&EdgePatch { etype, other: dst }))
    .unwrap_or(false);
  let mut edge_exists_in_snapshot = false;

  // Check snapshot for edge existence and get base properties
  if let Some(ref snapshot) = handle.db.snapshot {
    if let Some(src_phys) = snapshot.get_phys_node(src) {
      if let Some(dst_phys) = snapshot.get_phys_node(dst) {
        if let Some(edge_idx) = snapshot.find_edge_index(src_phys, etype, dst_phys) {
          edge_exists_in_snapshot = true;
          if let Some(snapshot_props) = snapshot.get_edge_props(edge_idx) {
            props = snapshot_props;
          }
        }
      }
    }
  }

  // Edge must exist either in delta or snapshot
  if !edge_added_in_delta && !edge_exists_in_snapshot && !edge_added_in_tx {
    return None;
  }

  // Apply delta modifications
  if let Some(delta_props) = delta.edge_props.get(&(src, etype, dst)) {
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

  // Apply pending transaction modifications
  if let Some(pending_props) = handle.tx.pending_edge_props.get(&(src, etype, dst)) {
    for (key_id, value) in pending_props {
      match value {
        Some(v) => {
          props.insert(*key_id, v.clone());
        }
        None => {
          props.remove(key_id);
        }
      }
    }
  }

  Some(props)
}

// ============================================================================
// Edge Counting
// ============================================================================

/// Count edges for a source node
pub fn count_edges_out(handle: &TxHandle, src: NodeId, etype: Option<ETypeId>) -> usize {
  let delta = handle.db.delta.read();

  let added = delta
    .out_add
    .get(&src)
    .map(|set| {
      if let Some(et) = etype {
        set.iter().filter(|p| p.etype == et).count()
      } else {
        set.len()
      }
    })
    .unwrap_or(0);

  let deleted_set = delta.out_del.get(&src);
  let deleted = deleted_set
    .map(|set| {
      if let Some(et) = etype {
        set.iter().filter(|p| p.etype == et).count()
      } else {
        set.len()
      }
    })
    .unwrap_or(0);

  // Get snapshot count
  let snapshot_count = if let Some(ref snapshot) = handle.db.snapshot {
    if let Some(src_phys) = snapshot.get_phys_node(src) {
      if let Some(et) = etype {
        // Count only edges with matching etype
        snapshot
          .iter_out_edges(src_phys)
          .filter(|(_dst, e)| *e == et)
          .count()
      } else {
        snapshot.get_out_degree(src_phys).unwrap_or(0)
      }
    } else {
      0
    }
  } else {
    0
  };

  let mut count = snapshot_count.saturating_sub(deleted) + added;

  let pending_added = handle
    .tx
    .pending_out_add
    .get(&src)
    .map(|set| {
      if let Some(et) = etype {
        set.iter().filter(|p| p.etype == et).count()
      } else {
        set.len()
      }
    })
    .unwrap_or(0);

  let pending_deleted = handle
    .tx
    .pending_out_del
    .get(&src)
    .map(|set| {
      if let Some(et) = etype {
        set.iter().filter(|p| p.etype == et).count()
      } else {
        set.len()
      }
    })
    .unwrap_or(0);

  count = count.saturating_add(pending_added);
  count = count.saturating_sub(pending_deleted);

  count
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;
  use crate::graph::db::{close_graph_db, open_graph_db, OpenOptions};
  use crate::graph::nodes::{create_node, NodeOpts};
  use crate::graph::tx::{begin_tx, commit};
  use tempfile::tempdir;

  #[test]
  fn test_add_edge() {
    let temp_dir = tempdir().unwrap();
    let db = open_graph_db(temp_dir.path(), OpenOptions::new()).unwrap();

    let mut tx = begin_tx(&db).unwrap();

    let node1 = create_node(&mut tx, NodeOpts::new()).unwrap();
    let node2 = create_node(&mut tx, NodeOpts::new()).unwrap();

    add_edge(&mut tx, node1, 1, node2).unwrap();

    commit(&mut tx).unwrap();
    close_graph_db(db).unwrap();
  }

  #[test]
  fn test_add_multiple_edges() {
    let temp_dir = tempdir().unwrap();
    let db = open_graph_db(temp_dir.path(), OpenOptions::new()).unwrap();

    let mut tx = begin_tx(&db).unwrap();

    let alice = create_node(&mut tx, NodeOpts::new().with_key("alice")).unwrap();
    let bob = create_node(&mut tx, NodeOpts::new().with_key("bob")).unwrap();
    let charlie = create_node(&mut tx, NodeOpts::new().with_key("charlie")).unwrap();

    let knows = 1; // Edge type ID for "knows"
    let follows = 2; // Edge type ID for "follows"

    add_edge(&mut tx, alice, knows, bob).unwrap();
    add_edge(&mut tx, alice, knows, charlie).unwrap();
    add_edge(&mut tx, bob, follows, alice).unwrap();

    commit(&mut tx).unwrap();
    close_graph_db(db).unwrap();
  }

  #[test]
  fn test_edge_exists_after_add() {
    let temp_dir = tempdir().unwrap();
    let db = open_graph_db(temp_dir.path(), OpenOptions::new()).unwrap();

    let mut tx = begin_tx(&db).unwrap();

    let node1 = create_node(&mut tx, NodeOpts::new()).unwrap();
    let node2 = create_node(&mut tx, NodeOpts::new()).unwrap();

    add_edge(&mut tx, node1, 1, node2).unwrap();

    commit(&mut tx).unwrap();

    // After commit, check if edge exists
    let tx2 = crate::graph::tx::begin_read_tx(&db).unwrap();
    assert!(edge_exists(&tx2, node1, 1, node2));

    close_graph_db(db).unwrap();
  }

  #[test]
  fn test_get_neighbors() {
    let temp_dir = tempdir().unwrap();
    let db = open_graph_db(temp_dir.path(), OpenOptions::new()).unwrap();

    let mut tx = begin_tx(&db).unwrap();

    let alice = create_node(&mut tx, NodeOpts::new()).unwrap();
    let bob = create_node(&mut tx, NodeOpts::new()).unwrap();
    let charlie = create_node(&mut tx, NodeOpts::new()).unwrap();

    add_edge(&mut tx, alice, 1, bob).unwrap();
    add_edge(&mut tx, alice, 1, charlie).unwrap();

    commit(&mut tx).unwrap();

    // Check neighbors
    let tx2 = crate::graph::tx::begin_read_tx(&db).unwrap();
    let neighbors = get_neighbors_out(&tx2, alice, None);
    assert_eq!(neighbors.len(), 2);
    assert!(neighbors.contains(&bob));
    assert!(neighbors.contains(&charlie));

    close_graph_db(db).unwrap();
  }
}
