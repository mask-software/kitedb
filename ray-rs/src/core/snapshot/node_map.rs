//! NodeIdToPhys section layouts.
//!
//! Node IDs are user-chosen u64 values, so the NodeID -> physical node map
//! has two encodings:
//! - Dense: one i32 per NodeID in `0..=max_node_id`, -1 when absent. O(1)
//!   lookups. Used while IDs are packed closely enough that the array stays
//!   proportional to the node count, and the only layout before v5.
//! - Sparse (v5+, `SnapshotFlags::SPARSE_NODE_ID_MAP`): one
//!   `(node_id: u64, phys: u32)` entry per node, strictly ascending by
//!   node_id, found by binary search. Its size depends only on the node count.

use crate::error::{KiteError, Result};
use crate::types::{NodeId, PhysNode};
use crate::util::binary::{read_i32, read_u32, read_u64, write_i32};
use std::cmp::Ordering;

/// Bytes per dense entry (i32 phys, -1 = absent).
pub const DENSE_ENTRY_SIZE: usize = 4;
/// Bytes per sparse entry (u64 node_id, u32 phys).
pub const SPARSE_ENTRY_SIZE: usize = 12;

/// The writer keeps the dense layout while it needs at most this many slots
/// per node, plus `DENSE_SLACK_SLOTS`.
const DENSE_SLOTS_PER_NODE: u64 = 2;
const DENSE_SLACK_SLOTS: u64 = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeIdMapLayout {
  Dense,
  Sparse,
}

impl NodeIdMapLayout {
  /// Layout for `num_nodes` nodes whose largest ID is `max_node_id`.
  pub fn for_nodes(num_nodes: usize, max_node_id: NodeId) -> Self {
    // Dense entries are i32, so every physical index must fit.
    if i32::try_from(num_nodes).is_err() {
      return Self::Sparse;
    }
    let dense_slot_budget = (num_nodes as u64)
      .saturating_mul(DENSE_SLOTS_PER_NODE)
      .saturating_add(DENSE_SLACK_SLOTS);
    // `max_node_id + 1 <= budget`, without overflowing at u64::MAX.
    if max_node_id < dense_slot_budget {
      Self::Dense
    } else {
      Self::Sparse
    }
  }
}

/// Look up `node_id` in a dense map.
#[inline]
pub fn dense_lookup(map: &[u8], node_id: NodeId) -> Option<PhysNode> {
  let index = usize::try_from(node_id).ok()?;
  if index >= map.len() / DENSE_ENTRY_SIZE {
    return None;
  }
  PhysNode::try_from(read_i32(map, index * DENSE_ENTRY_SIZE)).ok()
}

/// Number of entries in a sparse map.
#[inline]
pub fn sparse_len(map: &[u8]) -> usize {
  map.len() / SPARSE_ENTRY_SIZE
}

/// Entry `index` of a sparse map. `index` must be below `sparse_len(map)`.
#[inline]
pub fn sparse_entry(map: &[u8], index: usize) -> (NodeId, PhysNode) {
  let offset = index * SPARSE_ENTRY_SIZE;
  (read_u64(map, offset), read_u32(map, offset + 8))
}

/// Look up `node_id` in a sparse map by binary search.
#[inline]
pub fn sparse_lookup(map: &[u8], node_id: NodeId) -> Option<PhysNode> {
  let mut lo = 0usize;
  let mut hi = sparse_len(map);
  while lo < hi {
    let mid = lo + (hi - lo) / 2;
    let (mid_id, phys) = sparse_entry(map, mid);
    match mid_id.cmp(&node_id) {
      Ordering::Less => lo = mid + 1,
      Ordering::Greater => hi = mid,
      Ordering::Equal => return Some(phys),
    }
  }
  None
}

fn map_error(message: String) -> KiteError {
  KiteError::InvalidSnapshot(format!("NodeIdToPhys: {message}"))
}

/// Encode the NodeIdToPhys section for nodes listed in physical order.
///
/// `phys_to_node_id` must be strictly ascending (the writer sorts nodes and
/// refuses duplicate IDs) and bounded by `max_node_id`: each ID maps to
/// exactly one physical node, as readers check at load.
pub fn encode(
  phys_to_node_id: &[NodeId],
  max_node_id: NodeId,
) -> Result<(NodeIdMapLayout, Vec<u8>)> {
  let layout = NodeIdMapLayout::for_nodes(phys_to_node_id.len(), max_node_id);
  let data = match layout {
    NodeIdMapLayout::Dense => encode_dense(phys_to_node_id, max_node_id)?,
    NodeIdMapLayout::Sparse => encode_sparse(phys_to_node_id)?,
  };
  Ok((layout, data))
}

fn encode_dense(phys_to_node_id: &[NodeId], max_node_id: NodeId) -> Result<Vec<u8>> {
  let len = usize::try_from(max_node_id)
    .ok()
    .and_then(|max| max.checked_add(1))
    .and_then(|slots| slots.checked_mul(DENSE_ENTRY_SIZE))
    .ok_or_else(|| map_error(format!("dense map for max node ID {max_node_id} overflows")))?;
  // 0xFF bytes encode -1 (absent) in every slot.
  let mut data = vec![0xFFu8; len];
  for (phys, &node_id) in phys_to_node_id.iter().enumerate() {
    if node_id > max_node_id {
      return Err(map_error(format!(
        "node ID {node_id} exceeds max node ID {max_node_id}"
      )));
    }
    let phys = i32::try_from(phys)
      .map_err(|_| map_error(format!("physical node {phys} does not fit a dense entry")))?;
    // node_id <= max_node_id, which fits usize (checked above).
    let slot = node_id as usize * DENSE_ENTRY_SIZE;
    if read_i32(&data, slot) != -1 {
      return Err(map_error(format!("node ID {node_id} is repeated")));
    }
    write_i32(&mut data, slot, phys);
  }
  Ok(data)
}

fn encode_sparse(phys_to_node_id: &[NodeId]) -> Result<Vec<u8>> {
  let capacity = phys_to_node_id
    .len()
    .checked_mul(SPARSE_ENTRY_SIZE)
    .ok_or_else(|| map_error("sparse map size overflows".to_string()))?;
  let mut data = Vec::with_capacity(capacity);
  let mut previous: Option<NodeId> = None;
  for (phys, &node_id) in phys_to_node_id.iter().enumerate() {
    if previous.is_some_and(|previous| previous >= node_id) {
      return Err(map_error(format!(
        "node IDs are not strictly ascending at physical node {phys}"
      )));
    }
    previous = Some(node_id);
    let phys = PhysNode::try_from(phys)
      .map_err(|_| map_error(format!("physical node {phys} does not fit u32")))?;
    data.extend_from_slice(&node_id.to_le_bytes());
    data.extend_from_slice(&phys.to_le_bytes());
  }
  Ok(data)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn layout_stays_dense_for_packed_ids() {
    assert_eq!(NodeIdMapLayout::for_nodes(0, 0), NodeIdMapLayout::Dense);
    assert_eq!(NodeIdMapLayout::for_nodes(3, 1023), NodeIdMapLayout::Dense);
    assert_eq!(
      NodeIdMapLayout::for_nodes(1_000_000, 1_999_999),
      NodeIdMapLayout::Dense
    );
  }

  #[test]
  fn layout_goes_sparse_for_spread_ids() {
    assert_eq!(NodeIdMapLayout::for_nodes(0, 1024), NodeIdMapLayout::Sparse);
    assert_eq!(
      NodeIdMapLayout::for_nodes(1_000_000, 2_001_024),
      NodeIdMapLayout::Sparse
    );
    assert_eq!(
      NodeIdMapLayout::for_nodes(2, u64::MAX),
      NodeIdMapLayout::Sparse
    );
    // Physical indices past i32::MAX cannot be stored densely.
    assert_eq!(
      NodeIdMapLayout::for_nodes(i32::MAX as usize + 1, 0),
      NodeIdMapLayout::Sparse
    );
  }

  #[test]
  fn dense_round_trip_and_out_of_range_ids_miss() {
    let (layout, map) = encode(&[1, 2, 5], 5).expect("encode");
    assert_eq!(layout, NodeIdMapLayout::Dense);
    assert_eq!(map.len(), 6 * DENSE_ENTRY_SIZE);
    for (node_id, phys) in [(1, Some(0)), (2, Some(1)), (5, Some(2))] {
      assert_eq!(dense_lookup(&map, node_id), phys);
    }
    for node_id in [0, 3, 4, 6, 1 << 62, (1 << 62) + 1, u64::MAX] {
      assert_eq!(dense_lookup(&map, node_id), None, "dense_lookup({node_id})");
    }
  }

  #[test]
  fn sparse_round_trip_and_neighbours_miss() {
    let ids = [1, 3_000_000_000, 1 << 40, u64::MAX - 1, u64::MAX];
    let (layout, map) = encode(&ids, u64::MAX).expect("encode");
    assert_eq!(layout, NodeIdMapLayout::Sparse);
    assert_eq!(map.len(), ids.len() * SPARSE_ENTRY_SIZE);
    for (phys, &node_id) in ids.iter().enumerate() {
      assert_eq!(sparse_lookup(&map, node_id), Some(phys as PhysNode));
    }
    for node_id in [0, 2, 2_999_999_999, 3_000_000_001, (1 << 40) + 1] {
      assert_eq!(
        sparse_lookup(&map, node_id),
        None,
        "sparse_lookup({node_id})"
      );
    }
    assert_eq!(sparse_lookup(&[], 1), None);
  }

  /// A repeated ID would leave a physical node no ID maps to.
  #[test]
  fn duplicate_ids_are_rejected() {
    assert!(encode(&[1, 2, 2], 2).is_err(), "dense");
    assert!(encode(&[1, 1 << 40, 1 << 40], 1 << 40).is_err(), "sparse");
  }

  #[test]
  fn encode_rejects_inconsistent_input_instead_of_panicking() {
    assert!(encode(&[7], 5).is_err());
    assert!(encode(&[1 << 40, 1], 1 << 40).is_err());
  }
}
