//! Export and import options for Python bindings

use crate::export as ray_export;
use pyo3::prelude::*;

/// Options for exporting a database
#[pyclass(name = "ExportOptions")]
#[derive(Debug, Clone, Default)]
pub struct ExportOptions {
  #[pyo3(get, set)]
  pub include_nodes: Option<bool>,
  #[pyo3(get, set)]
  pub include_edges: Option<bool>,
  #[pyo3(get, set)]
  pub include_schema: Option<bool>,
  #[pyo3(get, set)]
  pub pretty: Option<bool>,
}

#[pymethods]
impl ExportOptions {
  #[new]
  #[pyo3(signature = (include_nodes=None, include_edges=None, include_schema=None, pretty=None))]
  fn new(
    include_nodes: Option<bool>,
    include_edges: Option<bool>,
    include_schema: Option<bool>,
    pretty: Option<bool>,
  ) -> Self {
    Self {
      include_nodes,
      include_edges,
      include_schema,
      pretty,
    }
  }

  fn __repr__(&self) -> String {
    format!(
      "ExportOptions(include_nodes={:?}, include_edges={:?}, pretty={:?})",
      self.include_nodes, self.include_edges, self.pretty
    )
  }
}

impl ExportOptions {
  /// Convert to core export options
  pub fn to_rust(self) -> ray_export::ExportOptions {
    let mut opts = ray_export::ExportOptions::default();
    if let Some(v) = self.include_nodes {
      opts.include_nodes = v;
    }
    if let Some(v) = self.include_edges {
      opts.include_edges = v;
    }
    if let Some(v) = self.include_schema {
      opts.include_schema = v;
    }
    if let Some(v) = self.pretty {
      opts.pretty = v;
    }
    opts
  }
}

/// Options for importing into a database
#[pyclass(name = "ImportOptions")]
#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
  #[pyo3(get, set)]
  pub skip_existing: Option<bool>,
  #[pyo3(get, set)]
  pub batch_size: Option<i64>,
}

#[pymethods]
impl ImportOptions {
  #[new]
  #[pyo3(signature = (skip_existing=None, batch_size=None))]
  fn new(skip_existing: Option<bool>, batch_size: Option<i64>) -> Self {
    Self {
      skip_existing,
      batch_size,
    }
  }

  fn __repr__(&self) -> String {
    format!(
      "ImportOptions(skip_existing={:?}, batch_size={:?})",
      self.skip_existing, self.batch_size
    )
  }
}

impl ImportOptions {
  /// Convert to core import options
  pub fn to_rust(self) -> ray_export::ImportOptions {
    let mut opts = ray_export::ImportOptions::default();
    if let Some(v) = self.skip_existing {
      opts.skip_existing = v;
    }
    if let Some(v) = self.batch_size {
      if v > 0 {
        opts.batch_size = v as usize;
      }
    }
    opts
  }
}

/// Export result information
#[pyclass(name = "ExportResult")]
#[derive(Debug, Clone)]
pub struct ExportResult {
  #[pyo3(get)]
  pub node_count: i64,
  #[pyo3(get)]
  pub edge_count: i64,
}

#[pymethods]
impl ExportResult {
  #[new]
  fn new(node_count: i64, edge_count: i64) -> Self {
    Self {
      node_count,
      edge_count,
    }
  }

  fn __repr__(&self) -> String {
    format!(
      "ExportResult(node_count={}, edge_count={})",
      self.node_count, self.edge_count
    )
  }
}

/// Import result information
#[pyclass(name = "ImportResult")]
#[derive(Debug, Clone)]
pub struct ImportResult {
  #[pyo3(get)]
  pub node_count: i64,
  #[pyo3(get)]
  pub edge_count: i64,
  #[pyo3(get)]
  pub skipped: i64,
}

#[pymethods]
impl ImportResult {
  #[new]
  fn new(node_count: i64, edge_count: i64, skipped: i64) -> Self {
    Self {
      node_count,
      edge_count,
      skipped,
    }
  }

  fn __repr__(&self) -> String {
    format!(
      "ImportResult(node_count={}, edge_count={}, skipped={})",
      self.node_count, self.edge_count, self.skipped
    )
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_export_options_default() {
    let opts = ExportOptions::default();
    let rust = opts.to_rust();
    // Default rust options should include nodes and edges
    assert!(rust.include_nodes);
    assert!(rust.include_edges);
  }

  #[test]
  fn test_export_options_custom() {
    let opts = ExportOptions {
      include_nodes: Some(true),
      include_edges: Some(false),
      include_schema: Some(true),
      pretty: Some(true),
    };
    let rust = opts.to_rust();
    assert!(rust.include_nodes);
    assert!(!rust.include_edges);
    assert!(rust.include_schema);
    assert!(rust.pretty);
  }

  #[test]
  fn test_import_options_default() {
    let opts = ImportOptions::default();
    let rust = opts.to_rust();
    assert!(!rust.skip_existing);
  }

  #[test]
  fn test_import_options_custom() {
    let opts = ImportOptions {
      skip_existing: Some(true),
      batch_size: Some(500),
    };
    let rust = opts.to_rust();
    assert!(rust.skip_existing);
    assert_eq!(rust.batch_size, 500);
  }

  #[test]
  fn test_export_result() {
    let result = ExportResult::new(100, 200);
    assert_eq!(result.node_count, 100);
    assert_eq!(result.edge_count, 200);
  }

  #[test]
  fn test_import_result() {
    let result = ImportResult::new(100, 200, 5);
    assert_eq!(result.node_count, 100);
    assert_eq!(result.edge_count, 200);
    assert_eq!(result.skipped, 5);
  }
}
