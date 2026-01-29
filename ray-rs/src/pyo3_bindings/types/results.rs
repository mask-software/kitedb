//! Pagination result types for Python bindings

use super::edges::FullEdge;
use pyo3::prelude::*;

/// Page of node IDs (cursor-based pagination)
#[pyclass(name = "NodePage")]
#[derive(Debug, Clone)]
pub struct NodePage {
  #[pyo3(get)]
  pub items: Vec<i64>,
  #[pyo3(get)]
  pub next_cursor: Option<String>,
  #[pyo3(get)]
  pub has_more: bool,
  #[pyo3(get)]
  pub total: Option<i64>,
}

#[pymethods]
impl NodePage {
  #[new]
  #[pyo3(signature = (items, next_cursor=None, has_more=false, total=None))]
  fn new(items: Vec<i64>, next_cursor: Option<String>, has_more: bool, total: Option<i64>) -> Self {
    NodePage {
      items,
      next_cursor,
      has_more,
      total,
    }
  }

  fn __repr__(&self) -> String {
    format!(
      "NodePage(count={}, has_more={}, total={:?})",
      self.items.len(),
      self.has_more,
      self.total
    )
  }

  fn __len__(&self) -> usize {
    self.items.len()
  }

  fn __iter__(slf: PyRef<'_, Self>) -> PyResult<Py<NodePageIterator>> {
    let iter = NodePageIterator {
      items: slf.items.clone(),
      index: 0,
    };
    Py::new(slf.py(), iter)
  }
}

/// Iterator for NodePage
#[pyclass]
pub struct NodePageIterator {
  items: Vec<i64>,
  index: usize,
}

#[pymethods]
impl NodePageIterator {
  fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
    slf
  }

  fn __next__(mut slf: PyRefMut<'_, Self>) -> Option<i64> {
    if slf.index < slf.items.len() {
      let item = slf.items[slf.index];
      slf.index += 1;
      Some(item)
    } else {
      None
    }
  }
}

/// Page of edges (cursor-based pagination)
#[pyclass(name = "EdgePage")]
#[derive(Debug, Clone)]
pub struct EdgePage {
  #[pyo3(get)]
  pub items: Vec<FullEdge>,
  #[pyo3(get)]
  pub next_cursor: Option<String>,
  #[pyo3(get)]
  pub has_more: bool,
  #[pyo3(get)]
  pub total: Option<i64>,
}

#[pymethods]
impl EdgePage {
  #[new]
  #[pyo3(signature = (items, next_cursor=None, has_more=false, total=None))]
  fn new(
    items: Vec<FullEdge>,
    next_cursor: Option<String>,
    has_more: bool,
    total: Option<i64>,
  ) -> Self {
    EdgePage {
      items,
      next_cursor,
      has_more,
      total,
    }
  }

  fn __repr__(&self) -> String {
    format!(
      "EdgePage(count={}, has_more={}, total={:?})",
      self.items.len(),
      self.has_more,
      self.total
    )
  }

  fn __len__(&self) -> usize {
    self.items.len()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_node_page_creation() {
    let page = NodePage::new(vec![1, 2, 3], Some("cursor".to_string()), true, Some(100));
    assert_eq!(page.items.len(), 3);
    assert_eq!(page.next_cursor, Some("cursor".to_string()));
    assert!(page.has_more);
    assert_eq!(page.total, Some(100));
  }

  #[test]
  fn test_edge_page_creation() {
    let edges = vec![FullEdge::create(1, 2, 3)];
    let page = EdgePage::new(edges, None, false, Some(1));
    assert_eq!(page.items.len(), 1);
    assert!(!page.has_more);
  }

  #[test]
  fn test_node_page_len() {
    let page = NodePage::new(vec![1, 2, 3, 4, 5], None, false, None);
    assert_eq!(page.__len__(), 5);
  }
}
