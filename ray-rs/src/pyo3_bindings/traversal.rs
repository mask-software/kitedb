//! Python bindings for Traversal and Pathfinding result types

use pyo3::prelude::*;

// ============================================================================
// Traversal Result
// ============================================================================

/// A single result from a traversal
#[pyclass(name = "TraversalResult")]
#[derive(Debug, Clone)]
pub struct PyTraversalResult {
  /// The node ID that was reached
  #[pyo3(get)]
  pub node_id: i64,
  /// The depth (number of hops) from the start
  #[pyo3(get)]
  pub depth: u32,
  /// Source node of the edge used (if any)
  #[pyo3(get)]
  pub edge_src: Option<i64>,
  /// Destination node of the edge used (if any)
  #[pyo3(get)]
  pub edge_dst: Option<i64>,
  /// Edge type used (if any)
  #[pyo3(get)]
  pub edge_type: Option<u32>,
}

#[pymethods]
impl PyTraversalResult {
  fn __repr__(&self) -> String {
    format!(
      "TraversalResult(node_id={}, depth={})",
      self.node_id, self.depth
    )
  }
}

// ============================================================================
// Path Result
// ============================================================================

/// Result of a pathfinding query
#[pyclass(name = "PathResult")]
#[derive(Debug, Clone)]
pub struct PyPathResult {
  /// Nodes in order from source to target
  #[pyo3(get)]
  pub path: Vec<i64>,
  /// Edges as PathEdge objects
  #[pyo3(get)]
  pub edges: Vec<PyPathEdge>,
  /// Sum of edge weights along the path
  #[pyo3(get)]
  pub total_weight: f64,
  /// Whether a path was found
  #[pyo3(get)]
  pub found: bool,
}

#[pymethods]
impl PyPathResult {
  fn __repr__(&self) -> String {
    if self.found {
      format!(
        "PathResult(path={:?}, total_weight={:.2})",
        self.path, self.total_weight
      )
    } else {
      "PathResult(found=False)".to_string()
    }
  }

  fn __len__(&self) -> usize {
    self.path.len()
  }

  fn __bool__(&self) -> bool {
    self.found
  }
}

/// An edge in a path result
#[pyclass(name = "PathEdge")]
#[derive(Debug, Clone)]
pub struct PyPathEdge {
  #[pyo3(get)]
  pub src: i64,
  #[pyo3(get)]
  pub etype: u32,
  #[pyo3(get)]
  pub dst: i64,
}

#[pymethods]
impl PyPathEdge {
  fn __repr__(&self) -> String {
    format!("PathEdge({} --[{}]--> {})", self.src, self.etype, self.dst)
  }
}
