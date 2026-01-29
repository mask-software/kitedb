//! Streaming and pagination helpers

use crate::core::single_file::SingleFileDB;
use crate::graph::db::GraphDB;
use crate::graph::iterators::{
  list_edges as graph_list_edges, list_nodes as graph_list_nodes, ListEdgesOptions,
};
use crate::types::{Edge, NodeId};

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
// Streaming (GraphDB)
// =============================================================================

pub fn stream_nodes_graph(db: &GraphDB, options: StreamOptions) -> Vec<Vec<NodeId>> {
  let batch_size = if options.batch_size == 0 {
    1000
  } else {
    options.batch_size
  };
  let mut batches: Vec<Vec<NodeId>> = Vec::new();
  let mut current: Vec<NodeId> = Vec::with_capacity(batch_size);
  for node_id in graph_list_nodes(db) {
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

pub fn stream_edges_graph(db: &GraphDB, options: StreamOptions) -> Vec<Vec<Edge>> {
  let batch_size = if options.batch_size == 0 {
    1000
  } else {
    options.batch_size
  };
  let mut batches: Vec<Vec<Edge>> = Vec::new();
  let mut current: Vec<Edge> = Vec::with_capacity(batch_size);
  for edge in graph_list_edges(db, ListEdgesOptions::default()) {
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
// Pagination (GraphDB)
// =============================================================================

pub fn get_nodes_page_graph(db: &GraphDB, options: PaginationOptions) -> Page<NodeId> {
  let limit = if options.limit == 0 {
    100
  } else {
    options.limit
  };
  let mut start_after: Option<NodeId> = None;
  if let Some(cursor) = options.cursor.as_ref() {
    if let Some(stripped) = cursor.strip_prefix("n:") {
      if let Ok(id) = stripped.parse::<u64>() {
        start_after = Some(id);
      }
    }
  }

  let mut items = Vec::new();
  let mut found_start = start_after.is_none();

  for node_id in graph_list_nodes(db) {
    if !found_start {
      if Some(node_id) == start_after {
        found_start = true;
      }
      continue;
    }

    items.push(node_id);
    if items.len() > limit {
      break;
    }
  }

  let has_more = items.len() > limit;
  if has_more {
    items.pop();
  }

  let next_cursor = if has_more {
    items.last().map(|id| format!("n:{id}"))
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

pub fn get_edges_page_graph(db: &GraphDB, options: PaginationOptions) -> Page<Edge> {
  let limit = if options.limit == 0 {
    100
  } else {
    options.limit
  };
  let mut start_after: Option<(NodeId, u32, NodeId)> = None;
  if let Some(cursor) = options.cursor.as_ref() {
    if let Some(stripped) = cursor.strip_prefix("e:") {
      let parts: Vec<&str> = stripped.split(':').collect();
      if parts.len() == 3 {
        if let (Ok(src), Ok(etype), Ok(dst)) = (
          parts[0].parse::<u64>(),
          parts[1].parse::<u32>(),
          parts[2].parse::<u64>(),
        ) {
          start_after = Some((src, etype, dst));
        }
      }
    }
  }

  let mut items = Vec::new();
  let mut found_start = start_after.is_none();
  for edge in graph_list_edges(db, ListEdgesOptions::default()) {
    if !found_start {
      if let Some((src, etype, dst)) = start_after {
        if edge.src == src && edge.etype == etype && edge.dst == dst {
          found_start = true;
        }
      }
      continue;
    }

    items.push(Edge {
      src: edge.src,
      etype: edge.etype,
      dst: edge.dst,
    });
    if items.len() > limit {
      break;
    }
  }

  let has_more = items.len() > limit;
  if has_more {
    items.pop();
  }

  let next_cursor = if has_more {
    items
      .last()
      .map(|edge| format!("e:{}:{}:{}", edge.src, edge.etype, edge.dst))
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

// =============================================================================
// Pagination (SingleFileDB)
// =============================================================================

pub fn get_nodes_page_single(db: &SingleFileDB, options: PaginationOptions) -> Page<NodeId> {
  let limit = if options.limit == 0 {
    100
  } else {
    options.limit
  };
  let mut start_after: Option<NodeId> = None;
  if let Some(cursor) = options.cursor.as_ref() {
    if let Some(stripped) = cursor.strip_prefix("n:") {
      if let Ok(id) = stripped.parse::<u64>() {
        start_after = Some(id);
      }
    }
  }

  let mut items = Vec::new();
  let mut found_start = start_after.is_none();
  for node_id in db.list_nodes() {
    if !found_start {
      if Some(node_id) == start_after {
        found_start = true;
      }
      continue;
    }
    items.push(node_id);
    if items.len() > limit {
      break;
    }
  }

  let has_more = items.len() > limit;
  if has_more {
    items.pop();
  }

  let next_cursor = if has_more {
    items.last().map(|id| format!("n:{id}"))
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

pub fn get_edges_page_single(db: &SingleFileDB, options: PaginationOptions) -> Page<Edge> {
  let limit = if options.limit == 0 {
    100
  } else {
    options.limit
  };
  let mut start_after: Option<(NodeId, u32, NodeId)> = None;
  if let Some(cursor) = options.cursor.as_ref() {
    if let Some(stripped) = cursor.strip_prefix("e:") {
      let parts: Vec<&str> = stripped.split(':').collect();
      if parts.len() == 3 {
        if let (Ok(src), Ok(etype), Ok(dst)) = (
          parts[0].parse::<u64>(),
          parts[1].parse::<u32>(),
          parts[2].parse::<u64>(),
        ) {
          start_after = Some((src, etype, dst));
        }
      }
    }
  }

  let mut items = Vec::new();
  let mut found_start = start_after.is_none();
  for edge in db.list_edges(None) {
    if !found_start {
      if let Some((src, etype, dst)) = start_after {
        if edge.src == src && edge.etype == etype && edge.dst == dst {
          found_start = true;
        }
      }
      continue;
    }
    items.push(Edge {
      src: edge.src,
      etype: edge.etype,
      dst: edge.dst,
    });
    if items.len() > limit {
      break;
    }
  }

  let has_more = items.len() > limit;
  if has_more {
    items.pop();
  }

  let next_cursor = if has_more {
    items
      .last()
      .map(|edge| format!("e:{}:{}:{}", edge.src, edge.etype, edge.dst))
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
