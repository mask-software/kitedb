//! Checked numeric conversions for the PyO3 boundary.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use crate::api::traversal::TraversalDirection;
use crate::types::NodeId;

pub(crate) const MAX_CACHE_ENTRIES: i64 = 10_000_000;
pub(crate) const MAX_COUNT: i64 = 1_000_000_000;
pub(crate) const MAX_DEPTH: i64 = 1_000_000;
pub(crate) const MAX_DURATION_MS: i64 = 3_153_600_000_000;
pub(crate) const MAX_BYTES: i64 = 1_i64 << 40;
pub(crate) const MAX_COMPRESSION_MIN_SIZE: i64 = 4 * 1024 * 1024 * 1024;
pub(crate) const MIN_PAGE_SIZE: u32 = 4096;
pub(crate) const MAX_PAGE_SIZE: u32 = 65536;
pub(crate) const MIN_WAL_PAGES: u64 = 16;
pub(crate) const MAX_VECTOR_PARAM: i64 = 1_000_000;
pub(crate) const MAX_VECTOR_DIMENSIONS: i64 = 1_000_000;

pub(crate) fn non_negative_usize(field: &str, value: i64, max: i64) -> PyResult<usize> {
  if value < 0 {
    return Err(PyValueError::new_err(format!(
      "{field} must be non-negative"
    )));
  }
  if value > max {
    return Err(PyValueError::new_err(format!("{field} must be <= {max}")));
  }
  usize::try_from(value)
    .map_err(|_| PyValueError::new_err(format!("{field} does not fit in a platform usize")))
}

pub(crate) fn positive_usize(field: &str, value: i64, max: i64) -> PyResult<usize> {
  if value <= 0 {
    return Err(PyValueError::new_err(format!("{field} must be positive")));
  }
  non_negative_usize(field, value, max)
}

pub(crate) fn non_negative_u32(field: &str, value: i64, max: i64) -> PyResult<u32> {
  let value = non_negative_usize(field, value, max)?;
  u32::try_from(value).map_err(|_| PyValueError::new_err(format!("{field} does not fit in a u32")))
}

pub(crate) fn positive_u32(field: &str, value: i64, max: i64) -> PyResult<u32> {
  if value <= 0 {
    return Err(PyValueError::new_err(format!("{field} must be positive")));
  }
  non_negative_u32(field, value, max)
}

pub(crate) fn non_negative_u64(field: &str, value: i64, max: u64) -> PyResult<u64> {
  if value < 0 {
    return Err(PyValueError::new_err(format!(
      "{field} must be non-negative"
    )));
  }
  let value = value as u64;
  if value > max {
    return Err(PyValueError::new_err(format!("{field} must be <= {max}")));
  }
  Ok(value)
}

pub(crate) fn positive_u64(field: &str, value: i64, max: u64) -> PyResult<u64> {
  if value <= 0 {
    return Err(PyValueError::new_err(format!("{field} must be positive")));
  }
  non_negative_u64(field, value, max)
}

/// Checks a node (or vector) id from Python. Core ids are u64, so a negative i64 cast with `as`
/// would wrap to a huge id and corrupt the allocator; every id argument must pass through here.
pub(crate) fn node_id(field: &str, value: i64) -> PyResult<NodeId> {
  non_negative_u64(field, value, i64::MAX as u64)
}

/// Checks that a vector has the expected length. Core distance kernels assert on mismatched
/// lengths, which would surface as a PanicException and poison any lock held by the caller.
pub(crate) fn vector_len(field: &str, vector: &[f64], dimensions: usize) -> PyResult<()> {
  if vector.len() != dimensions {
    return Err(PyValueError::new_err(format!(
      "{field} must have {dimensions} dimensions, got {}",
      vector.len()
    )));
  }
  Ok(())
}

pub(crate) fn ratio(field: &str, value: f64) -> PyResult<f64> {
  if !value.is_finite() || !(0.0..=1.0).contains(&value) {
    return Err(PyValueError::new_err(format!(
      "{field} must be a finite number in [0.0, 1.0]"
    )));
  }
  Ok(value)
}

pub(crate) fn page_size(value: i64) -> PyResult<usize> {
  if value < 0 {
    return Err(PyValueError::new_err("page_size must be non-negative"));
  }
  let value =
    u32::try_from(value).map_err(|_| PyValueError::new_err("page_size must fit in a u32"))?;
  if !(MIN_PAGE_SIZE..=MAX_PAGE_SIZE).contains(&value) || !value.is_power_of_two() {
    return Err(PyValueError::new_err(
      "page_size must be a power of two between 4096 and 65536",
    ));
  }
  usize::try_from(value)
    .map_err(|_| PyValueError::new_err("page_size does not fit in a platform usize"))
}

pub(crate) fn wal_size(value: i64, page_size: usize) -> PyResult<usize> {
  if value < 0 {
    return Err(PyValueError::new_err("wal_size must be non-negative"));
  }
  let value = value as u64;
  let min = MIN_WAL_PAGES * page_size as u64;
  if value < min {
    return Err(PyValueError::new_err(format!(
      "wal_size must be at least {min} bytes ({MIN_WAL_PAGES} pages)"
    )));
  }
  if value > MAX_BYTES as u64 {
    return Err(PyValueError::new_err(format!(
      "wal_size must be <= {MAX_BYTES} bytes"
    )));
  }
  usize::try_from(value)
    .map_err(|_| PyValueError::new_err("wal_size does not fit in a platform usize"))
}

pub(crate) fn compression_min_size(value: i64) -> PyResult<usize> {
  if value < 0 {
    return Err(PyValueError::new_err("min_size must be non-negative"));
  }
  if value > MAX_COMPRESSION_MIN_SIZE {
    return Err(PyValueError::new_err(format!(
      "min_size must be <= {MAX_COMPRESSION_MIN_SIZE}"
    )));
  }
  usize::try_from(value)
    .map_err(|_| PyValueError::new_err("min_size does not fit in a platform usize"))
}

pub(crate) fn compression_level(field: &str, value: i32, zstd: bool) -> PyResult<i32> {
  let (min, max) = if zstd { (1, 22) } else { (0, 9) };
  if !(min..=max).contains(&value) {
    return Err(PyValueError::new_err(format!(
      "{field} must be in [{min}, {max}]"
    )));
  }
  Ok(value)
}

/// Parses a traversal direction. `None` means the default, `out`; an unknown
/// name is a `ValueError` instead of a silent fallback.
pub(crate) fn direction(field: &str, value: Option<&str>) -> PyResult<TraversalDirection> {
  match value {
    None | Some("out") => Ok(TraversalDirection::Out),
    Some("in") => Ok(TraversalDirection::In),
    Some("both") => Ok(TraversalDirection::Both),
    Some(other) => Err(PyValueError::new_err(format!(
      "{field}: unknown direction {other:?}; expected one of: out, in, both"
    ))),
  }
}

/// Parses `traverse_multi` steps: `(direction, etype)` pairs.
pub(crate) fn traversal_steps(
  steps: Vec<(String, Option<u32>)>,
) -> PyResult<Vec<(TraversalDirection, Option<u32>)>> {
  steps
    .into_iter()
    .enumerate()
    .map(|(i, (dir, etype))| Ok((direction(&format!("steps[{i}]"), Some(&dir))?, etype)))
    .collect()
}
