//! Internal helper functions for Python bindings

use crate::api::traversal::{DbNeighbors, TraversalDirection};
use crate::core::single_file::SingleFileDB as RustSingleFileDB;
use crate::types::{ETypeId, Edge, NodeId};

/// The edges a hop expands, for traversal and pathfinding: the shared
/// implementation `Kite` and the Node bindings use too (`Both` lists a
/// self-loop once).
pub fn neighbors_from_single_file(
  db: &RustSingleFileDB,
  node_id: NodeId,
  direction: TraversalDirection,
  etype: Option<ETypeId>,
) -> Vec<Edge> {
  DbNeighbors::new(db).neighbors(node_id, direction, etype)
}

#[cfg(test)]
mod tests {
  // Note: Most helper tests require database instances which are
  // better tested through integration tests
}
