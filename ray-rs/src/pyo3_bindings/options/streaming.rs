//! Streaming and pagination options for Python bindings

use crate::streaming;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

/// Options for streaming node/edge batches
#[pyclass(name = "StreamOptions")]
#[derive(Debug, Clone, Default)]
pub struct StreamOptions {
  #[pyo3(get, set)]
  pub batch_size: Option<i64>,
}

#[pymethods]
impl StreamOptions {
  #[new]
  #[pyo3(signature = (batch_size=None))]
  fn new(batch_size: Option<i64>) -> Self {
    Self { batch_size }
  }

  fn __repr__(&self) -> String {
    format!("StreamOptions(batch_size={:?})", self.batch_size)
  }
}

impl StreamOptions {
  /// Convert to core streaming options
  pub fn to_rust(self) -> PyResult<streaming::StreamOptions> {
    let batch_size = self.batch_size.unwrap_or(0);
    if batch_size < 0 {
      return Err(PyRuntimeError::new_err("batch_size must be non-negative"));
    }
    Ok(streaming::StreamOptions {
      batch_size: batch_size as usize,
    })
  }
}

/// Options for cursor-based pagination
#[pyclass(name = "PaginationOptions")]
#[derive(Debug, Clone, Default)]
pub struct PaginationOptions {
  #[pyo3(get, set)]
  pub limit: Option<i64>,
  #[pyo3(get, set)]
  pub cursor: Option<String>,
}

#[pymethods]
impl PaginationOptions {
  #[new]
  #[pyo3(signature = (limit=None, cursor=None))]
  fn new(limit: Option<i64>, cursor: Option<String>) -> Self {
    Self { limit, cursor }
  }

  fn __repr__(&self) -> String {
    format!(
      "PaginationOptions(limit={:?}, cursor={:?})",
      self.limit, self.cursor
    )
  }
}

impl PaginationOptions {
  /// Convert to core pagination options
  pub fn to_rust(self) -> PyResult<streaming::PaginationOptions> {
    let limit = self.limit.unwrap_or(0);
    if limit < 0 {
      return Err(PyRuntimeError::new_err("limit must be non-negative"));
    }
    Ok(streaming::PaginationOptions {
      limit: limit as usize,
      cursor: self.cursor,
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_stream_options_default() {
    let opts = StreamOptions::default();
    let rust = opts.to_rust().unwrap();
    assert_eq!(rust.batch_size, 0);
  }

  #[test]
  fn test_stream_options_with_batch_size() {
    let opts = StreamOptions {
      batch_size: Some(100),
    };
    let rust = opts.to_rust().unwrap();
    assert_eq!(rust.batch_size, 100);
  }

  #[test]
  fn test_stream_options_negative_batch_size() {
    let opts = StreamOptions {
      batch_size: Some(-1),
    };
    assert!(opts.to_rust().is_err());
  }

  #[test]
  fn test_pagination_options_default() {
    let opts = PaginationOptions::default();
    let rust = opts.to_rust().unwrap();
    assert_eq!(rust.limit, 0);
    assert!(rust.cursor.is_none());
  }

  #[test]
  fn test_pagination_options_with_cursor() {
    let opts = PaginationOptions {
      limit: Some(50),
      cursor: Some("abc123".to_string()),
    };
    let rust = opts.to_rust().unwrap();
    assert_eq!(rust.limit, 50);
    assert_eq!(rust.cursor, Some("abc123".to_string()));
  }

  #[test]
  fn test_pagination_options_negative_limit() {
    let opts = PaginationOptions {
      limit: Some(-1),
      cursor: None,
    };
    assert!(opts.to_rust().is_err());
  }
}
