//! Checked numeric conversions for the N-API boundary.

use napi::bindgen_prelude::{BigInt, Error, Result};
use napi::Status;
use std::fmt::Display;

use crate::types::NodeId;

/// Keep binding-provided cache allocations bounded even on 64-bit hosts.
/// Upper bound for general result, batch, and traversal counts.
pub(crate) const MAX_COUNT: i64 = 1_000_000_000;
/// Upper bound for depth-like options.
pub(crate) const MAX_DEPTH: i64 = 1_000_000;
/// Upper bound for durations accepted at the binding boundary (100 years).
pub(crate) const MAX_DURATION_MS: i64 = 3_153_600_000_000;
/// Upper bound for byte-sized options that can cause file or buffer growth.
pub(crate) const MAX_BYTES: i64 = 1_i64 << 40;
/// Compression metadata is a u32 in the public binding, but keep the value
/// below the decompression safety ceiling used by the core.
pub(crate) const MAX_COMPRESSION_MIN_SIZE: i64 = 4 * 1024 * 1024 * 1024;
/// Page sizes are part of the single-file format contract.
pub(crate) const MIN_PAGE_SIZE: u32 = 4096;
pub(crate) const MAX_PAGE_SIZE: u32 = 65536;
/// The WAL must have enough pages for the format's minimum usable region.
pub(crate) const MIN_WAL_PAGES: u64 = 16;
/// Vector parameters are counts, but should not permit accidental huge work.
pub(crate) const MAX_VECTOR_PARAM: i64 = 1_000_000;
/// Vector dimensions are also used for direct allocations.
pub(crate) const MAX_VECTOR_DIMENSIONS: i64 = 1_000_000;
/// Largest integer a JS number represents exactly (`Number.MAX_SAFE_INTEGER`).
pub(crate) const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

fn invalid(field: &str, expectation: &str) -> Error {
  invalid_argument(format!("{field} {expectation}"))
}

pub(crate) fn invalid_argument(message: impl Into<String>) -> Error {
  Error::new(Status::InvalidArg, message.into())
}

pub(crate) fn non_negative_usize(field: &str, value: i64, max: i64) -> Result<usize> {
  if value < 0 {
    return Err(invalid(field, "must be non-negative"));
  }
  if value > max {
    return Err(invalid(field, &format!("must be <= {max}")));
  }
  usize::try_from(value).map_err(|_| invalid(field, "does not fit in a platform usize"))
}

pub(crate) fn positive_usize(field: &str, value: i64, max: i64) -> Result<usize> {
  if value <= 0 {
    return Err(invalid(field, "must be positive"));
  }
  non_negative_usize(field, value, max)
}

pub(crate) fn non_negative_u32(field: &str, value: i64, max: i64) -> Result<u32> {
  let value = non_negative_usize(field, value, max)?;
  u32::try_from(value).map_err(|_| invalid(field, "does not fit in a u32"))
}

pub(crate) fn positive_u32(field: &str, value: i64, max: i64) -> Result<u32> {
  if value <= 0 {
    return Err(invalid(field, "must be positive"));
  }
  non_negative_u32(field, value, max)
}

pub(crate) fn non_negative_u64(field: &str, value: i64, max: u64) -> Result<u64> {
  if value < 0 {
    return Err(invalid(field, "must be non-negative"));
  }
  let value = value as u64;
  if value > max {
    return Err(invalid(field, &format!("must be <= {max}")));
  }
  Ok(value)
}

pub(crate) fn positive_u64(field: &str, value: i64, max: u64) -> Result<u64> {
  if value <= 0 {
    return Err(invalid(field, "must be positive"));
  }
  non_negative_u64(field, value, max)
}

/// Render a JS number the way JS prints NaN and the infinities.
fn show_number(value: f64) -> String {
  if value.is_nan() {
    "NaN".to_string()
  } else if value.is_infinite() {
    if value > 0.0 { "Infinity" } else { "-Infinity" }.to_string()
  } else {
    value.to_string()
  }
}

/// `value` as an integer in `0..=max`, if it is one.
fn whole_in_range(value: f64, max: f64) -> Option<f64> {
  (value.is_finite() && value.fract() == 0.0 && (0.0..=max).contains(&value)).then_some(value)
}

/// Validate a node (or vector) ID passed in as a JS number.
///
/// IDs must be taken as `f64`: napi's `i64` conversion turns NaN and Infinity
/// into 0 and truncates fractions before the binding sees the value.
pub(crate) fn node_id(field: &str, value: f64) -> Result<NodeId> {
  match whole_in_range(value, MAX_SAFE_INTEGER) {
    Some(value) => Ok(value as NodeId),
    None => Err(invalid(
      field,
      &format!(
        "must be an integer between 0 and Number.MAX_SAFE_INTEGER, got {}",
        show_number(value)
      ),
    )),
  }
}

/// Validate a u32 value (edge type, prop key, label ID, depth, count) passed
/// in as a JS number.
///
/// These must be taken as `f64` too: napi's `u32` conversion wraps -1 to
/// 4294967295 and maps NaN to 0 and 1.5 to 1.
pub(crate) fn u32_value(field: &str, value: f64) -> Result<u32> {
  match whole_in_range(value, u32::MAX as f64) {
    Some(value) => Ok(value as u32),
    None => Err(invalid(
      field,
      &format!(
        "must be an integer between 0 and {}, got {}",
        u32::MAX,
        show_number(value)
      ),
    )),
  }
}

/// Validate an optional u32 value (see [`u32_value`]).
pub(crate) fn opt_u32_value(field: &str, value: Option<f64>) -> Result<Option<u32>> {
  value.map(|value| u32_value(field, value)).transpose()
}

/// Validate a count or depth passed in as a JS number (see [`u32_value`]).
pub(crate) fn count(field: &str, value: f64, max: i64) -> Result<usize> {
  non_negative_usize(field, u32_value(field, value)? as i64, max)
}

/// Validate every ID in a list (see [`node_id`]).
pub(crate) fn node_ids(field: &str, values: &[f64]) -> Result<Vec<NodeId>> {
  values.iter().map(|&value| node_id(field, value)).collect()
}

/// Validate every u32 value in a list (see [`u32_value`]).
pub(crate) fn u32_values(field: &str, values: &[f64]) -> Result<Vec<u32>> {
  values
    .iter()
    .map(|&value| u32_value(field, value))
    .collect()
}

/// Validate a JS number that must be an integer in the i64 range.
///
/// Every integral f64 in [-2^63, 2^63) converts exactly; fractions, NaN and
/// the infinities are rejected rather than truncated to an integer.
pub(crate) fn integral_i64(field: &str, value: f64) -> Result<i64> {
  // Exactly 2^63.
  const LIMIT: f64 = 9_223_372_036_854_775_808.0;
  if value.is_finite() && value.fract() == 0.0 && (-LIMIT..LIMIT).contains(&value) {
    return Ok(value as i64);
  }
  Err(invalid(
    field,
    &format!(
      "must be an integer in the 64-bit signed range, got {}",
      show_number(value)
    ),
  ))
}

/// Validate a JS BigInt that must fit an i64, instead of wrapping it.
pub(crate) fn bigint_i64(field: &str, value: &BigInt) -> Result<i64> {
  match value.get_i64() {
    (value, true) => Ok(value),
    _ => Err(invalid(
      field,
      "must fit a 64-bit signed integer (-2^63 to 2^63-1); this BigInt does not",
    )),
  }
}

/// Reject a vector whose length differs from the expected dimensions.
///
/// Core distance functions assert equal lengths, and a panic across the N-API
/// boundary aborts the process, so bindings check before calling into core.
pub(crate) fn vector_len(field: impl Display, len: usize, dimensions: usize) -> Result<()> {
  if len != dimensions {
    return Err(invalid_argument(format!(
      "Dimension mismatch: {field} has {len} dimensions, expected {dimensions}"
    )));
  }
  Ok(())
}

/// A byte count passed in as a JS number: an integer in `1..=MAX_BYTES`.
pub(crate) fn positive_bytes(field: &str, value: f64) -> Result<u64> {
  positive_u64(field, integral_i64(field, value)?, MAX_BYTES as u64)
}

/// A finite JS number, at least 0.
pub(crate) fn non_negative_number(field: &str, value: f64) -> Result<f64> {
  if !value.is_finite() || value < 0.0 {
    return Err(invalid(field, "must be a finite number, at least 0"));
  }
  Ok(value)
}

pub(crate) fn ratio(field: &str, value: f64) -> Result<f64> {
  if !value.is_finite() || !(0.0..=1.0).contains(&value) {
    return Err(invalid(field, "must be a finite number in [0.0, 1.0]"));
  }
  Ok(value)
}

pub(crate) fn page_size(value: u32) -> Result<usize> {
  if !(MIN_PAGE_SIZE..=MAX_PAGE_SIZE).contains(&value) || !value.is_power_of_two() {
    return Err(invalid(
      "pageSize",
      "must be a power of two between 4096 and 65536",
    ));
  }
  usize::try_from(value).map_err(|_| invalid("pageSize", "does not fit in a platform usize"))
}

pub(crate) fn wal_size(value: u32, page_size: usize) -> Result<usize> {
  let value = value as u64;
  let min = MIN_WAL_PAGES * page_size as u64;
  if value < min {
    return Err(invalid(
      "walSize",
      &format!("must be at least {min} bytes ({MIN_WAL_PAGES} pages)"),
    ));
  }
  if value > MAX_BYTES as u64 {
    return Err(invalid("walSize", &format!("must be <= {MAX_BYTES} bytes")));
  }
  usize::try_from(value).map_err(|_| invalid("walSize", "does not fit in a platform usize"))
}

pub(crate) fn resize_wal_size(field: &str, value: i64) -> Result<usize> {
  positive_usize(field, value, MAX_BYTES)
}

pub(crate) fn compression_min_size(value: u32) -> Result<usize> {
  if value as i64 > MAX_COMPRESSION_MIN_SIZE {
    return Err(invalid(
      "minSize",
      &format!("must be <= {MAX_COMPRESSION_MIN_SIZE}"),
    ));
  }
  usize::try_from(value).map_err(|_| invalid("minSize", "does not fit in a platform usize"))
}

pub(crate) fn compression_level(field: &str, value: i32, zstd: bool) -> Result<i32> {
  let (min, max) = if zstd { (1, 22) } else { (0, 9) };
  if !(min..=max).contains(&value) {
    return Err(invalid(field, &format!("must be in [{min}, {max}]")));
  }
  Ok(value)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn node_id_accepts_only_safe_non_negative_integers() {
    for valid in [0.0, -0.0, 42.0, MAX_SAFE_INTEGER] {
      assert_eq!(node_id("nodeId", valid).unwrap(), valid as NodeId);
    }
    for invalid in [
      -1.0,
      1.5,
      f64::NAN,
      f64::INFINITY,
      f64::NEG_INFINITY,
      MAX_SAFE_INTEGER + 1.0,
    ] {
      let err = node_id("nodeId", invalid).unwrap_err();
      assert!(err.reason.starts_with("nodeId must be an integer"), "{err}");
    }
  }

  #[test]
  fn u32_value_rejects_values_napi_would_wrap() {
    for valid in [0.0, -0.0, 7.0, u32::MAX as f64] {
      assert_eq!(u32_value("etype", valid).unwrap(), valid as u32);
    }
    for invalid in [-1.0, 1.5, f64::NAN, f64::INFINITY, u32::MAX as f64 + 1.0] {
      let err = u32_value("etype", invalid).unwrap_err();
      assert!(err.reason.starts_with("etype must be an integer"), "{err}");
    }
    assert_eq!(opt_u32_value("etype", None).unwrap(), None);
    assert!(count("k", 5.0, 3).is_err());
  }

  #[test]
  fn integral_i64_accepts_only_whole_numbers_in_range() {
    assert_eq!(
      integral_i64("intValue", -MAX_SAFE_INTEGER).unwrap(),
      -9_007_199_254_740_991
    );
    assert_eq!(integral_i64("intValue", 2f64.powi(60)).unwrap(), 1 << 60);
    assert_eq!(
      integral_i64("intValue", -(2f64.powi(63))).unwrap(),
      i64::MIN
    );
    for invalid in [0.5, f64::NAN, f64::NEG_INFINITY, 2f64.powi(63)] {
      assert!(integral_i64("intValue", invalid).is_err());
    }
  }

  #[test]
  fn bigint_i64_rejects_values_outside_i64() {
    let big = |sign_bit: bool, words: Vec<u64>| BigInt { sign_bit, words };
    assert_eq!(
      bigint_i64("v", &big(true, vec![1 << 63])).unwrap(),
      i64::MIN
    );
    assert_eq!(
      bigint_i64("v", &big(false, vec![i64::MAX as u64])).unwrap(),
      i64::MAX
    );
    assert!(bigint_i64("v", &big(false, vec![1 << 63])).is_err());
    assert!(bigint_i64("v", &big(true, vec![(1 << 63) + 1])).is_err());
    assert!(bigint_i64("v", &big(false, vec![5, 1])).is_err());
  }

  #[test]
  fn vector_len_reports_dimension_mismatch() {
    assert!(vector_len("query", 3, 3).is_ok());
    let err = vector_len("query", 2, 3).unwrap_err();
    assert_eq!(
      err.reason,
      "Dimension mismatch: query has 2 dimensions, expected 3"
    );
  }
}
