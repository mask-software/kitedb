//! Streaming operations for Python bindings

use pyo3::prelude::*;
use pyo3::IntoPyObjectExt;
use std::collections::VecDeque;

use crate::core::single_file::SingleFileDB as RustSingleFileDB;
use crate::pyo3_bindings::database::PyDatabase;
use crate::streaming;
use crate::types::{ETypeId, NodeId};

use crate::pyo3_bindings::ops::edges::count_edges_single;
use crate::pyo3_bindings::ops::nodes::{count_nodes_single, node_key_single};
use crate::pyo3_bindings::ops::properties::{edge_props_single, node_props_single};
use crate::pyo3_bindings::types::{EdgePage, EdgeWithProps, FullEdge, NodePage, NodeWithProps};

/// Trait for streaming operations
pub trait StreamingOps {
  /// Stream nodes in batches
  fn stream_nodes_impl(&self, options: streaming::StreamOptions) -> Vec<Vec<i64>>;
  /// Stream edges in batches
  fn stream_edges_impl(&self, options: streaming::StreamOptions) -> Vec<Vec<FullEdge>>;
  /// Get a page of node IDs
  fn nodes_page_impl(&self, options: streaming::PaginationOptions) -> NodePage;
  /// Get a page of edges
  fn edges_page_impl(&self, options: streaming::PaginationOptions) -> EdgePage;
}

// ============================================================================
// Lazy batch iterators
// ============================================================================

/// Batch size used when `StreamOptions.batch_size` is unset or 0.
const DEFAULT_BATCH_SIZE: usize = 1000;

fn batch_size(options: &streaming::StreamOptions) -> usize {
  if options.batch_size == 0 {
    DEFAULT_BATCH_SIZE
  } else {
    options.batch_size
  }
}

/// Iterator over node batches, returned by `Database.stream_nodes` and
/// `Database.stream_nodes_with_props`.
///
/// It walks the nodes in ID order with a cursor (`streaming::NodeCursor`),
/// reading ahead at most a batch or the nodes created since the last
/// checkpoint, and builds each batch on demand (keys and properties
/// included), so memory stays proportional to a batch. A node created past
/// the cursor during the stream is listed; one deleted before it is read is
/// not, and the `with_props` stream skips one deleted after.
#[pyclass(name = "NodeBatchIterator", module = "kitedb._kitedb")]
pub struct NodeBatchIterator {
  db: Py<PyDatabase>,
  cursor: streaming::NodeCursor,
  /// Nodes read and not yielded yet
  buffer: VecDeque<NodeId>,
  batch_size: usize,
  with_props: bool,
}

impl NodeBatchIterator {
  pub(crate) fn new(
    py: Python<'_>,
    db: Py<PyDatabase>,
    options: streaming::StreamOptions,
    with_props: bool,
  ) -> PyResult<Self> {
    let mut stream = Self {
      db,
      cursor: streaming::NodeCursor::new(),
      buffer: VecDeque::new(),
      batch_size: batch_size(&options),
      with_props,
    };
    stream.read_ahead(py)?;
    Ok(stream)
  }

  /// Read until a batch is buffered or the walk ends.
  fn read_ahead(&mut self, py: Python<'_>) -> PyResult<()> {
    if self.buffer.len() >= self.batch_size || self.cursor.is_done() {
      return Ok(());
    }
    let missing = self.batch_size - self.buffer.len();
    let cursor = &mut self.cursor;
    let nodes = self
      .db
      .borrow(py)
      .with_db_nogil(py, |db| Ok(cursor.next_chunk(db, missing)))?;
    self.buffer.extend(nodes);
    Ok(())
  }
}

#[pymethods]
impl NodeBatchIterator {
  fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
    slf
  }

  fn __next__(&mut self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
    self.read_ahead(py)?;
    if self.buffer.is_empty() {
      return Ok(None);
    }
    let end = self.buffer.len().min(self.batch_size);
    let batch: Vec<NodeId> = self.buffer.drain(..end).collect();
    if !self.with_props {
      let ids: Vec<i64> = batch.iter().map(|&id| id as i64).collect();
      return ids.into_py_any(py).map(Some);
    }
    let nodes = self.db.borrow(py).with_db_nogil(py, |db| {
      Ok(
        batch
          .iter()
          .filter_map(|&id| {
            // None: the node was deleted after it was read.
            node_props_single(db, id).map(|props| NodeWithProps {
              id: id as i64,
              key: node_key_single(db, id),
              props,
            })
          })
          .collect::<Vec<_>>(),
      )
    })?;
    nodes.into_py_any(py).map(Some)
  }
}

/// Iterator over edge batches, returned by `Database.stream_edges` and
/// `Database.stream_edges_with_props`.
///
/// It walks the edges in `(src, etype, dst)` order with a cursor
/// (`streaming::EdgeCursor`), like `NodeBatchIterator`, and loads properties
/// per batch on demand. The `with_props` stream skips an edge deleted after it
/// was read.
#[pyclass(name = "EdgeBatchIterator", module = "kitedb._kitedb")]
pub struct EdgeBatchIterator {
  db: Py<PyDatabase>,
  cursor: streaming::EdgeCursor,
  /// Edges read and not yielded yet
  buffer: VecDeque<(NodeId, ETypeId, NodeId)>,
  batch_size: usize,
  with_props: bool,
}

impl EdgeBatchIterator {
  pub(crate) fn new(
    py: Python<'_>,
    db: Py<PyDatabase>,
    options: streaming::StreamOptions,
    with_props: bool,
  ) -> PyResult<Self> {
    let mut stream = Self {
      db,
      cursor: streaming::EdgeCursor::new(),
      buffer: VecDeque::new(),
      batch_size: batch_size(&options),
      with_props,
    };
    stream.read_ahead(py)?;
    Ok(stream)
  }

  /// Read until a batch is buffered or the walk ends.
  fn read_ahead(&mut self, py: Python<'_>) -> PyResult<()> {
    if self.buffer.len() >= self.batch_size || self.cursor.is_done() {
      return Ok(());
    }
    let missing = self.batch_size - self.buffer.len();
    let cursor = &mut self.cursor;
    let edges = self
      .db
      .borrow(py)
      .with_db_nogil(py, |db| Ok(cursor.next_chunk(db, missing)))?;
    self.buffer.extend(
      edges
        .into_iter()
        .map(|edge| (edge.src, edge.etype, edge.dst)),
    );
    Ok(())
  }
}

#[pymethods]
impl EdgeBatchIterator {
  fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
    slf
  }

  fn __next__(&mut self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
    self.read_ahead(py)?;
    if self.buffer.is_empty() {
      return Ok(None);
    }
    let end = self.buffer.len().min(self.batch_size);
    let batch: Vec<(NodeId, ETypeId, NodeId)> = self.buffer.drain(..end).collect();
    if !self.with_props {
      let edges: Vec<FullEdge> = batch
        .iter()
        .map(|&(src, etype, dst)| FullEdge::create(src as i64, etype, dst as i64))
        .collect();
      return edges.into_py_any(py).map(Some);
    }
    let edges = self.db.borrow(py).with_db_nogil(py, |db| {
      Ok(
        batch
          .iter()
          .filter_map(|&(src, etype, dst)| {
            edge_props_single(db, src, etype, dst).map(|props| EdgeWithProps {
              src: src as i64,
              etype,
              dst: dst as i64,
              props,
            })
          })
          .collect::<Vec<_>>(),
      )
    })?;
    edges.into_py_any(py).map(Some)
  }
}

pub fn nodes_page_single(db: &RustSingleFileDB, options: streaming::PaginationOptions) -> NodePage {
  let page = streaming::nodes_page_single(db, options);
  NodePage {
    items: page.items.into_iter().map(|id| id as i64).collect(),
    next_cursor: page.next_cursor,
    has_more: page.has_more,
    total: Some(count_nodes_single(db)),
  }
}

pub fn edges_page_single(db: &RustSingleFileDB, options: streaming::PaginationOptions) -> EdgePage {
  let page = streaming::edges_page_single(db, options);
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
