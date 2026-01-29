//! Edge types for Python bindings

use pyo3::prelude::*;

/// Edge representation (neighbor style - used for traversal results)
#[pyclass(name = "Edge")]
#[derive(Debug, Clone)]
pub struct Edge {
  #[pyo3(get)]
  pub etype: u32,
  #[pyo3(get)]
  pub node_id: i64,
}

#[pymethods]
impl Edge {
  #[new]
  fn new(etype: u32, node_id: i64) -> Self {
    Edge { etype, node_id }
  }

  fn __repr__(&self) -> String {
    format!("Edge(etype={}, node_id={})", self.etype, self.node_id)
  }

  fn __eq__(&self, other: &Self) -> bool {
    self.etype == other.etype && self.node_id == other.node_id
  }
}

/// Full edge representation (src, etype, dst)
#[pyclass(name = "FullEdge")]
#[derive(Debug, Clone)]
pub struct FullEdge {
  #[pyo3(get)]
  pub src: i64,
  #[pyo3(get)]
  pub etype: u32,
  #[pyo3(get)]
  pub dst: i64,
}

#[pymethods]
impl FullEdge {
  #[new]
  fn new(src: i64, etype: u32, dst: i64) -> Self {
    FullEdge { src, etype, dst }
  }

  fn __repr__(&self) -> String {
    format!(
      "FullEdge(src={}, etype={}, dst={})",
      self.src, self.etype, self.dst
    )
  }

  fn __eq__(&self, other: &Self) -> bool {
    self.src == other.src && self.etype == other.etype && self.dst == other.dst
  }
}

// Public constructor for use within the crate
impl FullEdge {
  /// Create a new full edge (pub for crate use)
  pub fn create(src: i64, etype: u32, dst: i64) -> Self {
    Self::new(src, etype, dst)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_edge_creation() {
    let edge = Edge::new(1, 42);
    assert_eq!(edge.etype, 1);
    assert_eq!(edge.node_id, 42);
  }

  #[test]
  fn test_full_edge_creation() {
    let edge = FullEdge::new(1, 2, 3);
    assert_eq!(edge.src, 1);
    assert_eq!(edge.etype, 2);
    assert_eq!(edge.dst, 3);
  }

  #[test]
  fn test_edge_equality() {
    let e1 = Edge::new(1, 42);
    let e2 = Edge::new(1, 42);
    let e3 = Edge::new(2, 42);
    assert!(e1.__eq__(&e2));
    assert!(!e1.__eq__(&e3));
  }

  #[test]
  fn test_full_edge_equality() {
    let e1 = FullEdge::new(1, 2, 3);
    let e2 = FullEdge::new(1, 2, 3);
    let e3 = FullEdge::new(1, 2, 4);
    assert!(e1.__eq__(&e2));
    assert!(!e1.__eq__(&e3));
  }
}
