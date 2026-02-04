//! Internal helper functions for Python bindings

use crate::api::traversal::TraversalDirection;
use crate::core::single_file::SingleFileDB as RustSingleFileDB;
use crate::types::{ETypeId, Edge, NodeId};

/// Get neighbors from single-file database for traversal
pub fn get_neighbors_from_single_file(
  db: &RustSingleFileDB,
  node_id: NodeId,
  direction: TraversalDirection,
  etype: Option<ETypeId>,
) -> Vec<Edge> {
  let mut edges = Vec::new();
  match direction {
    TraversalDirection::Out => {
      for (e, dst) in db.out_edges(node_id) {
        if etype.is_none() || etype == Some(e) {
          edges.push(Edge {
            src: node_id,
            etype: e,
            dst,
          });
        }
      }
    }
    TraversalDirection::In => {
      for (e, src) in db.in_edges(node_id) {
        if etype.is_none() || etype == Some(e) {
          edges.push(Edge {
            src,
            etype: e,
            dst: node_id,
          });
        }
      }
    }
    TraversalDirection::Both => {
      edges.extend(get_neighbors_from_single_file(
        db,
        node_id,
        TraversalDirection::Out,
        etype,
      ));
      edges.extend(get_neighbors_from_single_file(
        db,
        node_id,
        TraversalDirection::In,
        etype,
      ));
    }
  }
  edges
}

#[cfg(test)]
mod tests {
  // Note: Most helper tests require database instances which are
  // better tested through integration tests
}
