//! Streaming operations for Python bindings

use crate::core::single_file::SingleFileDB as RustSingleFileDB;
use crate::graph::db::GraphDB as RustGraphDB;
use crate::streaming;

use crate::pyo3_bindings::ops::edges::{count_edges_graph, count_edges_single};
use crate::pyo3_bindings::ops::nodes::{
  count_nodes_graph, count_nodes_single, get_node_key_graph, get_node_key_single,
};
use crate::pyo3_bindings::ops::properties::{
  get_edge_props_graph, get_edge_props_single, get_node_props_graph, get_node_props_single,
};
use crate::pyo3_bindings::types::{EdgePage, EdgeWithProps, FullEdge, NodePage, NodeWithProps};

/// Trait for streaming operations
pub trait StreamingOps {
  /// Stream nodes in batches
  fn stream_nodes_impl(&self, options: streaming::StreamOptions) -> Vec<Vec<i64>>;
  /// Stream edges in batches
  fn stream_edges_impl(&self, options: streaming::StreamOptions) -> Vec<Vec<FullEdge>>;
  /// Get a page of node IDs
  fn get_nodes_page_impl(&self, options: streaming::PaginationOptions) -> NodePage;
  /// Get a page of edges
  fn get_edges_page_impl(&self, options: streaming::PaginationOptions) -> EdgePage;
}

// ============================================================================
// Single-file database operations
// ============================================================================

pub fn stream_nodes_single(
  db: &RustSingleFileDB,
  options: streaming::StreamOptions,
) -> Vec<Vec<i64>> {
  streaming::stream_nodes_single(db, options)
    .into_iter()
    .map(|batch| batch.into_iter().map(|id| id as i64).collect())
    .collect()
}

pub fn stream_nodes_with_props_single(
  db: &RustSingleFileDB,
  options: streaming::StreamOptions,
) -> Vec<Vec<NodeWithProps>> {
  let batches = streaming::stream_nodes_single(db, options);
  batches
    .into_iter()
    .map(|batch| {
      batch
        .into_iter()
        .map(|node_id| {
          let key = get_node_key_single(db, node_id);
          let props = get_node_props_single(db, node_id).unwrap_or_default();
          NodeWithProps {
            id: node_id as i64,
            key,
            props,
          }
        })
        .collect()
    })
    .collect()
}

pub fn stream_edges_single(
  db: &RustSingleFileDB,
  options: streaming::StreamOptions,
) -> Vec<Vec<FullEdge>> {
  streaming::stream_edges_single(db, options)
    .into_iter()
    .map(|batch| {
      batch
        .into_iter()
        .map(|edge| FullEdge {
          src: edge.src as i64,
          etype: edge.etype,
          dst: edge.dst as i64,
        })
        .collect()
    })
    .collect()
}

pub fn stream_edges_with_props_single(
  db: &RustSingleFileDB,
  options: streaming::StreamOptions,
) -> Vec<Vec<EdgeWithProps>> {
  let batches = streaming::stream_edges_single(db, options);
  batches
    .into_iter()
    .map(|batch| {
      batch
        .into_iter()
        .map(|edge| {
          let props = get_edge_props_single(db, edge.src, edge.etype, edge.dst).unwrap_or_default();
          EdgeWithProps {
            src: edge.src as i64,
            etype: edge.etype,
            dst: edge.dst as i64,
            props,
          }
        })
        .collect()
    })
    .collect()
}

pub fn get_nodes_page_single(
  db: &RustSingleFileDB,
  options: streaming::PaginationOptions,
) -> NodePage {
  let page = streaming::get_nodes_page_single(db, options);
  NodePage {
    items: page.items.into_iter().map(|id| id as i64).collect(),
    next_cursor: page.next_cursor,
    has_more: page.has_more,
    total: Some(count_nodes_single(db)),
  }
}

pub fn get_edges_page_single(
  db: &RustSingleFileDB,
  options: streaming::PaginationOptions,
) -> EdgePage {
  let page = streaming::get_edges_page_single(db, options);
  EdgePage {
    items: page
      .items
      .into_iter()
      .map(|edge| FullEdge {
        src: edge.src as i64,
        etype: edge.etype,
        dst: edge.dst as i64,
      })
      .collect(),
    next_cursor: page.next_cursor,
    has_more: page.has_more,
    total: Some(count_edges_single(db)),
  }
}

// ============================================================================
// Graph database operations
// ============================================================================

pub fn stream_nodes_graph(db: &RustGraphDB, options: streaming::StreamOptions) -> Vec<Vec<i64>> {
  streaming::stream_nodes_graph(db, options)
    .into_iter()
    .map(|batch| batch.into_iter().map(|id| id as i64).collect())
    .collect()
}

pub fn stream_nodes_with_props_graph(
  db: &RustGraphDB,
  options: streaming::StreamOptions,
) -> Vec<Vec<NodeWithProps>> {
  let batches = streaming::stream_nodes_graph(db, options);
  batches
    .into_iter()
    .map(|batch| {
      batch
        .into_iter()
        .map(|node_id| {
          let key = get_node_key_graph(db, node_id);
          let props = get_node_props_graph(db, node_id).unwrap_or_default();
          NodeWithProps {
            id: node_id as i64,
            key,
            props,
          }
        })
        .collect()
    })
    .collect()
}

pub fn stream_edges_graph(
  db: &RustGraphDB,
  options: streaming::StreamOptions,
) -> Vec<Vec<FullEdge>> {
  streaming::stream_edges_graph(db, options)
    .into_iter()
    .map(|batch| {
      batch
        .into_iter()
        .map(|edge| FullEdge {
          src: edge.src as i64,
          etype: edge.etype,
          dst: edge.dst as i64,
        })
        .collect()
    })
    .collect()
}

pub fn stream_edges_with_props_graph(
  db: &RustGraphDB,
  options: streaming::StreamOptions,
) -> Vec<Vec<EdgeWithProps>> {
  let batches = streaming::stream_edges_graph(db, options);
  batches
    .into_iter()
    .map(|batch| {
      batch
        .into_iter()
        .map(|edge| {
          let props = get_edge_props_graph(db, edge.src, edge.etype, edge.dst).unwrap_or_default();
          EdgeWithProps {
            src: edge.src as i64,
            etype: edge.etype,
            dst: edge.dst as i64,
            props,
          }
        })
        .collect()
    })
    .collect()
}

pub fn get_nodes_page_graph(db: &RustGraphDB, options: streaming::PaginationOptions) -> NodePage {
  let page = streaming::get_nodes_page_graph(db, options);
  NodePage {
    items: page.items.into_iter().map(|id| id as i64).collect(),
    next_cursor: page.next_cursor,
    has_more: page.has_more,
    total: Some(count_nodes_graph(db)),
  }
}

pub fn get_edges_page_graph(db: &RustGraphDB, options: streaming::PaginationOptions) -> EdgePage {
  let page = streaming::get_edges_page_graph(db, options);
  EdgePage {
    items: page
      .items
      .into_iter()
      .map(|edge| FullEdge {
        src: edge.src as i64,
        etype: edge.etype,
        dst: edge.dst as i64,
      })
      .collect(),
    next_cursor: page.next_cursor,
    has_more: page.has_more,
    total: Some(count_edges_graph(db, None)),
  }
}
