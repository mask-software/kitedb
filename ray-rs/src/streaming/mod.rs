//! Streaming and pagination helpers

use crate::core::single_file::SingleFileDB;
use crate::types::{ETypeId, Edge, NodeId};

#[derive(Debug, Clone, Default)]
pub struct StreamOptions {
  pub batch_size: usize,
}

#[derive(Debug, Clone, Default)]
pub struct PaginationOptions {
  pub limit: usize,
  pub cursor: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Page<T> {
  pub items: Vec<T>,
  pub next_cursor: Option<String>,
  pub has_more: bool,
  pub total: Option<usize>,
}

// =============================================================================
// Streaming (SingleFileDB)
// =============================================================================

pub fn stream_nodes_single(db: &SingleFileDB, options: StreamOptions) -> Vec<Vec<NodeId>> {
  let batch_size = if options.batch_size == 0 {
    1000
  } else {
    options.batch_size
  };
  let mut batches: Vec<Vec<NodeId>> = Vec::new();
  let mut current: Vec<NodeId> = Vec::with_capacity(batch_size);
  for node_id in db.list_nodes() {
    current.push(node_id);
    if current.len() >= batch_size {
      batches.push(current);
      current = Vec::with_capacity(batch_size);
    }
  }
  if !current.is_empty() {
    batches.push(current);
  }
  batches
}

pub fn stream_edges_single(db: &SingleFileDB, options: StreamOptions) -> Vec<Vec<Edge>> {
  let batch_size = if options.batch_size == 0 {
    1000
  } else {
    options.batch_size
  };
  let mut batches: Vec<Vec<Edge>> = Vec::new();
  let mut current: Vec<Edge> = Vec::with_capacity(batch_size);
  for edge in db.list_edges(None) {
    current.push(Edge {
      src: edge.src,
      etype: edge.etype,
      dst: edge.dst,
    });
    if current.len() >= batch_size {
      batches.push(current);
      current = Vec::with_capacity(batch_size);
    }
  }
  if !current.is_empty() {
    batches.push(current);
  }
  batches
}

// =============================================================================
// Pagination (SingleFileDB)
// =============================================================================
//
// A cursor names the last item of the previous page; the next page starts at
// the first item ordered after it. A page resumes in place even if the
// cursor's own node or edge was deleted, and adding or deleting other items
// never makes a page repeat or skip one (items added before the cursor are
// not returned). Nodes are ordered by id, edges by (src, etype, dst).
//
// Each page lists and sorts every node or edge: O(N log N) per page, until
// the core offers iterators that seek to a key.

const DEFAULT_PAGE_LIMIT: usize = 100;

pub fn nodes_page_single(db: &SingleFileDB, options: PaginationOptions) -> Page<NodeId> {
  let start_after = options.cursor.as_deref().and_then(parse_node_cursor);
  // `list_nodes` is sorted by id.
  let nodes = db.list_nodes();
  let start = start_after.map_or(0, |cursor| nodes.partition_point(|&id| id <= cursor));
  page_of(&nodes[start..], options.limit, |id| format!("n:{id}"))
}

pub fn edges_page_single(db: &SingleFileDB, options: PaginationOptions) -> Page<Edge> {
  let start_after = options.cursor.as_deref().and_then(parse_edge_cursor);
  let mut edges: Vec<Edge> = db
    .list_edges(None)
    .into_iter()
    .map(|edge| Edge {
      src: edge.src,
      etype: edge.etype,
      dst: edge.dst,
    })
    .collect();
  edges.sort_unstable_by_key(edge_order);
  let start = start_after.map_or(0, |cursor| {
    edges.partition_point(|edge| edge_order(edge) <= cursor)
  });
  page_of(&edges[start..], options.limit, |edge| {
    format!("e:{}:{}:{}", edge.src, edge.etype, edge.dst)
  })
}

/// The first `limit` items (100 for 0) of `rest`, with a cursor at the last
/// one if more follow.
fn page_of<T: Copy>(rest: &[T], limit: usize, cursor_of: impl Fn(&T) -> String) -> Page<T> {
  let limit = if limit == 0 {
    DEFAULT_PAGE_LIMIT
  } else {
    limit
  };
  let has_more = rest.len() > limit;
  let items = rest[..rest.len().min(limit)].to_vec();
  let next_cursor = if has_more {
    items.last().map(cursor_of)
  } else {
    None
  };
  Page {
    items,
    next_cursor,
    has_more,
    total: None,
  }
}

fn edge_order(edge: &Edge) -> (NodeId, ETypeId, NodeId) {
  (edge.src, edge.etype, edge.dst)
}

/// `n:<id>`. An unparsable cursor starts from the beginning.
fn parse_node_cursor(cursor: &str) -> Option<NodeId> {
  cursor.strip_prefix("n:")?.parse().ok()
}

/// `e:<src>:<etype>:<dst>`. An unparsable cursor starts from the beginning.
fn parse_edge_cursor(cursor: &str) -> Option<(NodeId, ETypeId, NodeId)> {
  let mut parts = cursor.strip_prefix("e:")?.split(':');
  let src = parts.next()?.parse().ok()?;
  let etype = parts.next()?.parse().ok()?;
  let dst = parts.next()?.parse().ok()?;
  parts.next().is_none().then_some((src, etype, dst))
}
