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
// Cursor walks (SingleFileDB)
// =============================================================================

/// A walk over every node in ID order, in reads that each seek to where the
/// last one ended ([`SingleFileDB::nodes_after`]). It holds no database state
/// between reads: nodes created past the cursor are read, nodes deleted before
/// their read are not.
#[derive(Debug, Clone, Default)]
pub struct NodeCursor {
  after: Option<NodeId>,
  done: bool,
}

impl NodeCursor {
  pub fn new() -> Self {
    Self::default()
  }

  /// Whether the walk has read every node.
  pub fn is_done(&self) -> bool {
    self.done
  }

  /// The next nodes: at least `min` (fewer only at the end, none when done),
  /// and as many as the nodes created since the last checkpoint, which every
  /// read passes over once, so that part of the cost is paid once per that
  /// many nodes.
  pub fn next_chunk(&mut self, db: &SingleFileDB, min: usize) -> Vec<NodeId> {
    if self.done {
      return Vec::new();
    }
    let len = min.max(1).max(db.stats().delta_nodes_created);
    let nodes = db.nodes_after(self.after, len);
    self.done = nodes.len() < len;
    self.after = nodes.last().copied().or(self.after);
    nodes
  }
}

/// [`NodeCursor`] for edges, in `(src, etype, dst)` order
/// ([`SingleFileDB::edges_after`]).
#[derive(Debug, Clone, Default)]
pub struct EdgeCursor {
  after: Option<(NodeId, ETypeId, NodeId)>,
  done: bool,
}

impl EdgeCursor {
  pub fn new() -> Self {
    Self::default()
  }

  /// Whether the walk has read every edge.
  pub fn is_done(&self) -> bool {
    self.done
  }

  /// The next edges: at least `min` (fewer only at the end, none when done),
  /// and as many as the edges added since the last checkpoint (see
  /// [`NodeCursor::next_chunk`]).
  pub fn next_chunk(&mut self, db: &SingleFileDB, min: usize) -> Vec<Edge> {
    if self.done {
      return Vec::new();
    }
    let len = min.max(1).max(db.stats().delta_edges_added);
    let edges: Vec<Edge> = db
      .edges_after(self.after, len)
      .into_iter()
      .map(|edge| Edge {
        src: edge.src,
        etype: edge.etype,
        dst: edge.dst,
      })
      .collect();
    self.done = edges.len() < len;
    self.after = edges
      .last()
      .map(|edge| (edge.src, edge.etype, edge.dst))
      .or(self.after);
    edges
  }
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
// A page seeks to its cursor (`SingleFileDB::nodes_after` / `edges_after`):
// it costs what it returns, not the size of the graph.

const DEFAULT_PAGE_LIMIT: usize = 100;

pub fn nodes_page_single(db: &SingleFileDB, options: PaginationOptions) -> Page<NodeId> {
  let after = options.cursor.as_deref().and_then(parse_node_cursor);
  let limit = page_limit(options.limit);
  let nodes = db.nodes_after(after, limit.saturating_add(1));
  page_of(nodes, limit, |id| format!("n:{id}"))
}

pub fn edges_page_single(db: &SingleFileDB, options: PaginationOptions) -> Page<Edge> {
  let after = options.cursor.as_deref().and_then(parse_edge_cursor);
  let limit = page_limit(options.limit);
  let edges = db
    .edges_after(after, limit.saturating_add(1))
    .into_iter()
    .map(|edge| Edge {
      src: edge.src,
      etype: edge.etype,
      dst: edge.dst,
    })
    .collect();
  page_of(edges, limit, |edge| {
    format!("e:{}:{}:{}", edge.src, edge.etype, edge.dst)
  })
}

/// Page size: 0 keeps the default of 100.
fn page_limit(limit: usize) -> usize {
  if limit == 0 {
    DEFAULT_PAGE_LIMIT
  } else {
    limit
  }
}

/// The page of the first `limit` of `items` (up to `limit + 1` items), with a
/// cursor at its last item if more follow.
fn page_of<T>(mut items: Vec<T>, limit: usize, cursor_of: impl Fn(&T) -> String) -> Page<T> {
  let has_more = items.len() > limit;
  items.truncate(limit);
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
