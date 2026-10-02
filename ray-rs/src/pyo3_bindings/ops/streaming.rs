//! Streaming operations for Python bindings

use pyo3::prelude::*;
use pyo3::IntoPyObjectExt;

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
/// It lists node ids once when created and builds each batch on demand
/// (keys and properties included), so memory stays proportional to one batch.
/// A node deleted after the stream started is skipped by the `with_props`
/// stream.
#[pyclass(name = "NodeBatchIterator", module = "kitedb._kitedb")]
pub struct NodeBatchIterator {
  db: Py<PyDatabase>,
  ids: Vec<NodeId>,
  pos: usize,
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
    let ids = db.borrow(py).with_db_nogil(py, |db| Ok(db.list_nodes()))?;
    Ok(Self {
      db,
      ids,
      pos: 0,
      batch_size: batch_size(&options),
      with_props,
    })
  }
}

#[pymethods]
impl NodeBatchIterator {
  fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
    slf
  }

  fn __next__(&mut self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
    if self.pos >= self.ids.len() {
      return Ok(None);
    }
    let end = self.ids.len().min(self.pos + self.batch_size);
    let batch = &self.ids[self.pos..end];
    self.pos = end;
    if !self.with_props {
      let ids: Vec<i64> = batch.iter().map(|&id| id as i64).collect();
      return ids.into_py_any(py).map(Some);
    }
    let nodes = self.db.borrow(py).with_db_nogil(py, |db| {
      Ok(
        batch
          .iter()
          .filter_map(|&id| {
            // None: the node was deleted after the stream started.
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
/// It lists edges once when created and loads properties per batch on demand.
/// An edge deleted after the stream started is skipped by the `with_props`
/// stream.
#[pyclass(name = "EdgeBatchIterator", module = "kitedb._kitedb")]
pub struct EdgeBatchIterator {
  db: Py<PyDatabase>,
  edges: Vec<(NodeId, ETypeId, NodeId)>,
  pos: usize,
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
    let edges = db.borrow(py).with_db_nogil(py, |db| {
      Ok(
        db.list_edges(None)
          .into_iter()
          .map(|edge| (edge.src, edge.etype, edge.dst))
          .collect(),
      )
    })?;
    Ok(Self {
      db,
      edges,
      pos: 0,
      batch_size: batch_size(&options),
      with_props,
    })
  }
}

#[pymethods]
impl EdgeBatchIterator {
  fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
    slf
  }

  fn __next__(&mut self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
    if self.pos >= self.edges.len() {
      return Ok(None);
    }
    let end = self.edges.len().min(self.pos + self.batch_size);
    let batch = &self.edges[self.pos..end];
    self.pos = end;
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
