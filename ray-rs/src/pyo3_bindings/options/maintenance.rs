//! Database maintenance options for Python bindings

use crate::core::single_file::{
  SingleFileOptimizeOptions as RustSingleFileOptimizeOptions, VacuumOptions as RustVacuumOptions,
};
use crate::util::compression::{CompressionOptions as CoreCompressionOptions, CompressionType};
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

/// Compression options for database optimization
#[pyclass(name = "CompressionOptions")]
#[derive(Debug, Clone, Default)]
pub struct CompressionOptions {
  #[pyo3(get, set)]
  pub enabled: Option<bool>,
  #[pyo3(get, set)]
  pub compression_type: Option<String>,
  #[pyo3(get, set)]
  pub min_size: Option<u32>,
  #[pyo3(get, set)]
  pub level: Option<i32>,
}

#[pymethods]
impl CompressionOptions {
  #[new]
  #[pyo3(signature = (enabled=None, compression_type=None, min_size=None, level=None))]
  fn new(
    enabled: Option<bool>,
    compression_type: Option<String>,
    min_size: Option<u32>,
    level: Option<i32>,
  ) -> Self {
    Self {
      enabled,
      compression_type,
      min_size,
      level,
    }
  }

  fn __repr__(&self) -> String {
    format!(
      "CompressionOptions(enabled={:?}, type={:?}, level={:?})",
      self.enabled, self.compression_type, self.level
    )
  }
}

impl CompressionOptions {
  /// Convert to core compression options
  pub fn to_core(&self) -> PyResult<CoreCompressionOptions> {
    let mut out = CoreCompressionOptions::default();
    if let Some(enabled) = self.enabled {
      out.enabled = enabled;
    }
    if let Some(ref name) = self.compression_type {
      let lower = name.to_lowercase();
      out.compression_type = match lower.as_str() {
        "none" => CompressionType::None,
        "zstd" => CompressionType::Zstd,
        "gzip" => CompressionType::Gzip,
        "deflate" => CompressionType::Deflate,
        _ => {
          return Err(PyRuntimeError::new_err(format!(
            "Unknown compression_type: {name}"
          )))
        }
      };
    }
    if let Some(min_size) = self.min_size {
      out.min_size = min_size as usize;
    }
    if let Some(level) = self.level {
      out.level = level;
    }
    Ok(out)
  }
}

/// Options for optimizing a single-file database
#[pyclass(name = "SingleFileOptimizeOptions")]
#[derive(Debug, Clone, Default)]
pub struct SingleFileOptimizeOptions {
  #[pyo3(get, set)]
  pub compression: Option<CompressionOptions>,
}

#[pymethods]
impl SingleFileOptimizeOptions {
  #[new]
  #[pyo3(signature = (compression=None))]
  fn new(compression: Option<CompressionOptions>) -> Self {
    Self { compression }
  }

  fn __repr__(&self) -> String {
    format!(
      "SingleFileOptimizeOptions(compression={:?})",
      self.compression.is_some()
    )
  }
}

impl SingleFileOptimizeOptions {
  /// Convert to core optimize options
  pub fn to_core(&self) -> PyResult<RustSingleFileOptimizeOptions> {
    let compression = match self.compression.as_ref() {
      Some(opts) => Some(opts.to_core()?),
      None => None,
    };
    Ok(RustSingleFileOptimizeOptions { compression })
  }
}

/// Options for vacuuming a single-file database
#[pyclass(name = "VacuumOptions")]
#[derive(Debug, Clone, Default)]
pub struct VacuumOptions {
  #[pyo3(get, set)]
  pub shrink_wal: Option<bool>,
  #[pyo3(get, set)]
  pub min_wal_size: Option<u64>,
}

#[pymethods]
impl VacuumOptions {
  #[new]
  #[pyo3(signature = (shrink_wal=None, min_wal_size=None))]
  fn new(shrink_wal: Option<bool>, min_wal_size: Option<u64>) -> Self {
    Self {
      shrink_wal,
      min_wal_size,
    }
  }

  fn __repr__(&self) -> String {
    format!(
      "VacuumOptions(shrink_wal={:?}, min_wal_size={:?})",
      self.shrink_wal, self.min_wal_size
    )
  }
}

impl VacuumOptions {
  /// Convert to core vacuum options
  pub fn to_core(&self) -> RustVacuumOptions {
    RustVacuumOptions {
      shrink_wal: self.shrink_wal.unwrap_or(true),
      min_wal_size: self.min_wal_size,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_compression_options_default() {
    let opts = CompressionOptions::default();
    assert!(opts.enabled.is_none());
  }

  #[test]
  fn test_compression_options_to_core() {
    let opts = CompressionOptions {
      enabled: Some(true),
      compression_type: Some("zstd".to_string()),
      min_size: Some(1024),
      level: Some(3),
    };
    let core = opts.to_core().unwrap();
    assert!(core.enabled);
    assert_eq!(core.compression_type, CompressionType::Zstd);
    assert_eq!(core.min_size, 1024);
    assert_eq!(core.level, 3);
  }

  #[test]
  fn test_compression_options_invalid_type() {
    let opts = CompressionOptions {
      compression_type: Some("invalid".to_string()),
      ..Default::default()
    };
    assert!(opts.to_core().is_err());
  }

  #[test]
  fn test_vacuum_options_default() {
    let opts = VacuumOptions::default();
    let core = opts.to_core();
    assert!(core.shrink_wal); // defaults to true
  }

  #[test]
  fn test_vacuum_options_custom() {
    let opts = VacuumOptions {
      shrink_wal: Some(false),
      min_wal_size: Some(1024 * 1024),
    };
    let core = opts.to_core();
    assert!(!core.shrink_wal);
    assert_eq!(core.min_wal_size, Some(1024 * 1024));
  }
}
