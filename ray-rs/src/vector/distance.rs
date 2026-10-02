//! Distance functions with SIMD acceleration
//!
//! Provides optimized distance calculations for vector similarity search.
//! Each public function checks its inputs, then calls a kernel:
//!
//! - aarch64: NEON with four fused multiply-add accumulators (NEON is part of
//!   the aarch64 baseline);
//! - x86_64: AVX+FMA or AVX with four accumulators, picked at runtime;
//! - other targets, and x86_64 without AVX: portable 8-lane code that LLVM
//!   vectorizes.
//!
//! Ported from src/vector/distance.ts

use crate::vector::types::DistanceMetric;

#[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
use neon as kernel;
#[cfg(not(any(
  all(target_arch = "aarch64", target_feature = "neon"),
  target_arch = "x86_64"
)))]
use portable as kernel;
#[cfg(target_arch = "x86_64")]
use x86 as kernel;

// ============================================================================
// Distances
// ============================================================================

/// Dot product of two vectors
///
/// # Panics
/// Panics if the vectors have different lengths.
#[inline]
pub fn dot_product(a: &[f32], b: &[f32]) -> f32 {
  assert_eq!(a.len(), b.len(), "vector length mismatch");
  kernel::dot(a, b)
}

/// Squared Euclidean distance between two vectors
///
/// # Panics
/// Panics if the vectors have different lengths.
#[inline]
pub fn squared_euclidean(a: &[f32], b: &[f32]) -> f32 {
  assert_eq!(a.len(), b.len(), "vector length mismatch");
  kernel::squared_l2(a, b)
}

/// Euclidean distance (L2)
#[inline]
pub fn euclidean_distance(a: &[f32], b: &[f32]) -> f32 {
  squared_euclidean(a, b).sqrt()
}

/// Cosine similarity (assumes normalized vectors for efficiency)
#[inline]
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
  dot_product(a, b)
}

/// Cosine distance (1 - cosine_similarity)
#[inline]
pub fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
  1.0 - cosine_similarity(a, b)
}

/// Negated dot product: the sortable distance of `DistanceMetric::DotProduct`.
#[inline]
pub(crate) fn negative_dot_product(a: &[f32], b: &[f32]) -> f32 {
  -dot_product(a, b)
}

/// Cosine distance between a unit-length `query` and a vector of any length.
///
/// For stores that keep raw vectors: dividing by the stored vector's norm
/// keeps long vectors from looking more similar than they are.
#[inline]
pub(crate) fn cosine_distance_unit_query(query: &[f32], vector: &[f32]) -> f32 {
  let norm = l2_norm(vector);
  if norm > 0.0 {
    1.0 - dot_product(query, vector) / norm
  } else {
    1.0
  }
}

/// Evaluates `$body` with `$dist` bound to the metric's distance function as
/// a function item rather than a `fn` pointer, so the generic hot loops in
/// `$body` (k-means, list scans) monomorphize and inline the kernel.
///
/// The `stored_normalized` form picks the distance from a prepared query
/// (unit length for cosine) to a stored vector: `distance_fn` assumes cosine
/// operands are unit length, so for stores that skip normalization on insert
/// it divides out the stored vector's norm.
macro_rules! with_metric_distance {
  ($metric:expr, |$dist:ident| $body:expr) => {
    match $metric {
      $crate::vector::types::DistanceMetric::Cosine => {
        let $dist = $crate::vector::distance::cosine_distance;
        $body
      }
      $crate::vector::types::DistanceMetric::Euclidean => {
        let $dist = $crate::vector::distance::euclidean_distance;
        $body
      }
      $crate::vector::types::DistanceMetric::DotProduct => {
        let $dist = $crate::vector::distance::negative_dot_product;
        $body
      }
    }
  };
  ($metric:expr, stored_normalized = $normalized:expr, |$dist:ident| $body:expr) => {
    match $metric {
      $crate::vector::types::DistanceMetric::Cosine if !$normalized => {
        let $dist = $crate::vector::distance::cosine_distance_unit_query;
        $body
      }
      metric => $crate::vector::distance::with_metric_distance!(metric, |$dist| $body),
    }
  };
}
pub(crate) use with_metric_distance;

/// Distance function for `metric` as a `fn` pointer. Hot loops should use
/// `with_metric_distance!` instead.
pub(crate) fn metric_distance_fn(metric: DistanceMetric) -> fn(&[f32], &[f32]) -> f32 {
  with_metric_distance!(metric, |dist| dist)
}

// ============================================================================
// Norms and Normalization
// ============================================================================

/// Smallest sum of squares the fast norm path trusts. Below it, components
/// can underflow when squared (a vector of 1e-25s has a sum of squares of 0
/// in f32), so the norm is recomputed on a rescaled copy.
const MIN_FAST_SUM_OF_SQUARES: f32 = 1e-30;

/// L2 norm of a vector
///
/// Accurate across the whole f32 range: when the squares overflow or
/// underflow, it rescales by the largest component first. Returns infinity
/// only when the norm itself exceeds `f32::MAX`.
#[inline]
pub fn l2_norm(v: &[f32]) -> f32 {
  let sum_sq = kernel::sum_of_squares(v);
  if sum_sq.is_finite() && sum_sq >= MIN_FAST_SUM_OF_SQUARES {
    return sum_sq.sqrt();
  }
  let max_abs = max_abs(v);
  if max_abs == 0.0 || !max_abs.is_finite() || sum_sq.is_nan() {
    return sum_sq.sqrt();
  }
  let scaled: f32 = v.iter().map(|&x| (x / max_abs) * (x / max_abs)).sum();
  max_abs * scaled.sqrt()
}

/// Normalize a vector in-place
///
/// Any finite, nonzero vector becomes its unit direction, including vectors
/// whose squared components overflow or underflow f32. Zero vectors, and
/// vectors with NaN or infinite components, are left unchanged.
pub fn normalize_in_place(v: &mut [f32]) {
  let sum_sq = kernel::sum_of_squares(v);
  if sum_sq.is_finite() && sum_sq >= MIN_FAST_SUM_OF_SQUARES {
    scale_in_place(v, 1.0 / sum_sq.sqrt());
    return;
  }
  if sum_sq.is_nan() {
    return;
  }
  let max_abs = max_abs(v);
  if max_abs == 0.0 || !max_abs.is_finite() {
    return;
  }
  // Every component of v / max_abs is at most 1 in magnitude and the
  // largest is exactly 1, so the sum of squares lies in [1, len].
  for x in v.iter_mut() {
    *x /= max_abs;
  }
  let sum_sq = kernel::sum_of_squares(v);
  scale_in_place(v, 1.0 / sum_sq.sqrt());
}

/// Normalize a vector, returning a new vector
pub fn normalize(v: &[f32]) -> Vec<f32> {
  let mut result = v.to_vec();
  normalize_in_place(&mut result);
  result
}

/// Check if a vector is normalized (within tolerance)
pub fn is_normalized(v: &[f32], tolerance: f32) -> bool {
  let norm = l2_norm(v);
  (norm - 1.0).abs() < tolerance
}

fn max_abs(v: &[f32]) -> f32 {
  v.iter().fold(0.0f32, |max, &x| max.max(x.abs()))
}

fn scale_in_place(v: &mut [f32], factor: f32) {
  for x in v.iter_mut() {
    *x *= factor;
  }
}

// ============================================================================
// Kernels
// ============================================================================
//
// Every kernel takes slices of possibly different lengths and reads only the
// common prefix, so memory safety never depends on a caller's length check.

/// Portable kernels: eight independent accumulators let LLVM vectorize the
/// loops without reassociating a single running sum.
mod portable {
  const LANES: usize = 8;

  #[inline]
  fn reduce(acc: [f32; LANES]) -> f32 {
    ((acc[0] + acc[4]) + (acc[1] + acc[5])) + ((acc[2] + acc[6]) + (acc[3] + acc[7]))
  }

  #[inline]
  pub(super) fn dot(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let (a_blocks, a_tail) = a[..n].as_chunks::<LANES>();
    let (b_blocks, b_tail) = b[..n].as_chunks::<LANES>();
    let mut acc = [0.0f32; LANES];
    for (x, y) in a_blocks.iter().zip(b_blocks) {
      for ((sum, &x), &y) in acc.iter_mut().zip(x).zip(y) {
        *sum += x * y;
      }
    }
    let mut sum = reduce(acc);
    for (&x, &y) in a_tail.iter().zip(b_tail) {
      sum += x * y;
    }
    sum
  }

  #[inline]
  pub(super) fn squared_l2(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let (a_blocks, a_tail) = a[..n].as_chunks::<LANES>();
    let (b_blocks, b_tail) = b[..n].as_chunks::<LANES>();
    let mut acc = [0.0f32; LANES];
    for (x, y) in a_blocks.iter().zip(b_blocks) {
      for ((sum, &x), &y) in acc.iter_mut().zip(x).zip(y) {
        let d = x - y;
        *sum += d * d;
      }
    }
    let mut sum = reduce(acc);
    for (&x, &y) in a_tail.iter().zip(b_tail) {
      let d = x - y;
      sum += d * d;
    }
    sum
  }

  #[inline]
  pub(super) fn sum_of_squares(v: &[f32]) -> f32 {
    let (blocks, tail) = v.as_chunks::<LANES>();
    let mut acc = [0.0f32; LANES];
    for x in blocks {
      for (sum, &x) in acc.iter_mut().zip(x) {
        *sum += x * x;
      }
    }
    let mut sum = reduce(acc);
    for &x in tail {
      sum += x * x;
    }
    sum
  }
}

/// NEON kernels: 16 floats per iteration in four fused multiply-add
/// accumulators, then 4-float steps, then a scalar tail.
#[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
mod neon {
  use std::arch::aarch64::*;

  #[inline]
  pub(super) fn dot(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let (a_blocks, a_rest) = a[..n].as_chunks::<16>();
    let (b_blocks, b_rest) = b[..n].as_chunks::<16>();
    let (a_quads, a_tail) = a_rest.as_chunks::<4>();
    let (b_quads, b_tail) = b_rest.as_chunks::<4>();
    // SAFETY: NEON is enabled for this target (see the module cfg). Every
    // load reads 4 floats at offsets 0, 4, 8 or 12 of a 16-float block, or a
    // whole 4-float chunk.
    unsafe {
      let mut acc0 = vdupq_n_f32(0.0);
      let mut acc1 = vdupq_n_f32(0.0);
      let mut acc2 = vdupq_n_f32(0.0);
      let mut acc3 = vdupq_n_f32(0.0);
      for (x, y) in a_blocks.iter().zip(b_blocks) {
        let (x, y) = (x.as_ptr(), y.as_ptr());
        acc0 = vfmaq_f32(acc0, vld1q_f32(x), vld1q_f32(y));
        acc1 = vfmaq_f32(acc1, vld1q_f32(x.add(4)), vld1q_f32(y.add(4)));
        acc2 = vfmaq_f32(acc2, vld1q_f32(x.add(8)), vld1q_f32(y.add(8)));
        acc3 = vfmaq_f32(acc3, vld1q_f32(x.add(12)), vld1q_f32(y.add(12)));
      }
      for (x, y) in a_quads.iter().zip(b_quads) {
        acc0 = vfmaq_f32(acc0, vld1q_f32(x.as_ptr()), vld1q_f32(y.as_ptr()));
      }
      let mut sum = vaddvq_f32(vaddq_f32(vaddq_f32(acc0, acc1), vaddq_f32(acc2, acc3)));
      for (&x, &y) in a_tail.iter().zip(b_tail) {
        sum += x * y;
      }
      sum
    }
  }

  #[inline]
  pub(super) fn squared_l2(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let (a_blocks, a_rest) = a[..n].as_chunks::<16>();
    let (b_blocks, b_rest) = b[..n].as_chunks::<16>();
    let (a_quads, a_tail) = a_rest.as_chunks::<4>();
    let (b_quads, b_tail) = b_rest.as_chunks::<4>();
    // SAFETY: as in `dot`.
    unsafe {
      let mut acc0 = vdupq_n_f32(0.0);
      let mut acc1 = vdupq_n_f32(0.0);
      let mut acc2 = vdupq_n_f32(0.0);
      let mut acc3 = vdupq_n_f32(0.0);
      for (x, y) in a_blocks.iter().zip(b_blocks) {
        let (x, y) = (x.as_ptr(), y.as_ptr());
        let d0 = vsubq_f32(vld1q_f32(x), vld1q_f32(y));
        let d1 = vsubq_f32(vld1q_f32(x.add(4)), vld1q_f32(y.add(4)));
        let d2 = vsubq_f32(vld1q_f32(x.add(8)), vld1q_f32(y.add(8)));
        let d3 = vsubq_f32(vld1q_f32(x.add(12)), vld1q_f32(y.add(12)));
        acc0 = vfmaq_f32(acc0, d0, d0);
        acc1 = vfmaq_f32(acc1, d1, d1);
        acc2 = vfmaq_f32(acc2, d2, d2);
        acc3 = vfmaq_f32(acc3, d3, d3);
      }
      for (x, y) in a_quads.iter().zip(b_quads) {
        let d = vsubq_f32(vld1q_f32(x.as_ptr()), vld1q_f32(y.as_ptr()));
        acc0 = vfmaq_f32(acc0, d, d);
      }
      let mut sum = vaddvq_f32(vaddq_f32(vaddq_f32(acc0, acc1), vaddq_f32(acc2, acc3)));
      for (&x, &y) in a_tail.iter().zip(b_tail) {
        let d = x - y;
        sum += d * d;
      }
      sum
    }
  }

  #[inline]
  pub(super) fn sum_of_squares(v: &[f32]) -> f32 {
    let (blocks, rest) = v.as_chunks::<16>();
    let (quads, tail) = rest.as_chunks::<4>();
    // SAFETY: as in `dot`.
    unsafe {
      let mut acc0 = vdupq_n_f32(0.0);
      let mut acc1 = vdupq_n_f32(0.0);
      let mut acc2 = vdupq_n_f32(0.0);
      let mut acc3 = vdupq_n_f32(0.0);
      for x in blocks {
        let x = x.as_ptr();
        let x0 = vld1q_f32(x);
        let x1 = vld1q_f32(x.add(4));
        let x2 = vld1q_f32(x.add(8));
        let x3 = vld1q_f32(x.add(12));
        acc0 = vfmaq_f32(acc0, x0, x0);
        acc1 = vfmaq_f32(acc1, x1, x1);
        acc2 = vfmaq_f32(acc2, x2, x2);
        acc3 = vfmaq_f32(acc3, x3, x3);
      }
      for x in quads {
        let x = vld1q_f32(x.as_ptr());
        acc0 = vfmaq_f32(acc0, x, x);
      }
      let mut sum = vaddvq_f32(vaddq_f32(vaddq_f32(acc0, acc1), vaddq_f32(acc2, acc3)));
      for &x in tail {
        sum += x * x;
      }
      sum
    }
  }
}

/// x86_64 kernels: AVX+FMA when the CPU has both, else AVX, else portable.
/// 32 floats per iteration in four accumulators, then 8-float steps, then a
/// scalar tail.
#[cfg(target_arch = "x86_64")]
mod x86 {
  use std::arch::x86_64::*;

  use super::portable;

  #[inline]
  fn has_fma() -> bool {
    is_x86_feature_detected!("avx") && is_x86_feature_detected!("fma")
  }

  #[inline]
  fn has_avx() -> bool {
    is_x86_feature_detected!("avx")
  }

  #[inline]
  pub(super) fn dot(a: &[f32], b: &[f32]) -> f32 {
    if has_fma() {
      // SAFETY: AVX and FMA were detected at runtime.
      unsafe { dot_fma(a, b) }
    } else if has_avx() {
      // SAFETY: AVX was detected at runtime.
      unsafe { dot_avx(a, b) }
    } else {
      portable::dot(a, b)
    }
  }

  #[inline]
  pub(super) fn squared_l2(a: &[f32], b: &[f32]) -> f32 {
    if has_fma() {
      // SAFETY: AVX and FMA were detected at runtime.
      unsafe { squared_l2_fma(a, b) }
    } else if has_avx() {
      // SAFETY: AVX was detected at runtime.
      unsafe { squared_l2_avx(a, b) }
    } else {
      portable::squared_l2(a, b)
    }
  }

  #[inline]
  pub(super) fn sum_of_squares(v: &[f32]) -> f32 {
    if has_fma() {
      // SAFETY: AVX and FMA were detected at runtime.
      unsafe { sum_of_squares_fma(v) }
    } else if has_avx() {
      // SAFETY: AVX was detected at runtime.
      unsafe { sum_of_squares_avx(v) }
    } else {
      portable::sum_of_squares(v)
    }
  }

  /// Horizontal sum of the four accumulators' 32 lanes.
  #[target_feature(enable = "avx")]
  #[inline]
  unsafe fn reduce(acc: [__m256; 4]) -> f32 {
    let v = _mm256_add_ps(_mm256_add_ps(acc[0], acc[1]), _mm256_add_ps(acc[2], acc[3]));
    let s = _mm_add_ps(_mm256_castps256_ps128(v), _mm256_extractf128_ps(v, 1));
    let s = _mm_add_ps(s, _mm_movehl_ps(s, s));
    let s = _mm_add_ss(s, _mm_shuffle_ps(s, s, 1));
    _mm_cvtss_f32(s)
  }

  // SAFETY (all kernels below): the caller detected the target features.
  // Every load reads 8 floats at `i` with `i + 8 <= n`, and `n` is at most
  // the length of each slice.

  #[target_feature(enable = "avx,fma")]
  unsafe fn dot_fma(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let (pa, pb) = (a.as_ptr(), b.as_ptr());
    let mut acc = [_mm256_setzero_ps(); 4];
    let mut i = 0;
    while i + 32 <= n {
      for (lane, sum) in acc.iter_mut().enumerate() {
        let at = i + lane * 8;
        *sum = _mm256_fmadd_ps(
          _mm256_loadu_ps(pa.add(at)),
          _mm256_loadu_ps(pb.add(at)),
          *sum,
        );
      }
      i += 32;
    }
    while i + 8 <= n {
      acc[0] = _mm256_fmadd_ps(
        _mm256_loadu_ps(pa.add(i)),
        _mm256_loadu_ps(pb.add(i)),
        acc[0],
      );
      i += 8;
    }
    let mut sum = reduce(acc);
    for (&x, &y) in a[i..n].iter().zip(&b[i..n]) {
      sum += x * y;
    }
    sum
  }

  #[target_feature(enable = "avx")]
  unsafe fn dot_avx(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let (pa, pb) = (a.as_ptr(), b.as_ptr());
    let mut acc = [_mm256_setzero_ps(); 4];
    let mut i = 0;
    while i + 32 <= n {
      for (lane, sum) in acc.iter_mut().enumerate() {
        let at = i + lane * 8;
        let product = _mm256_mul_ps(_mm256_loadu_ps(pa.add(at)), _mm256_loadu_ps(pb.add(at)));
        *sum = _mm256_add_ps(*sum, product);
      }
      i += 32;
    }
    while i + 8 <= n {
      let product = _mm256_mul_ps(_mm256_loadu_ps(pa.add(i)), _mm256_loadu_ps(pb.add(i)));
      acc[0] = _mm256_add_ps(acc[0], product);
      i += 8;
    }
    let mut sum = reduce(acc);
    for (&x, &y) in a[i..n].iter().zip(&b[i..n]) {
      sum += x * y;
    }
    sum
  }

  #[target_feature(enable = "avx,fma")]
  unsafe fn squared_l2_fma(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let (pa, pb) = (a.as_ptr(), b.as_ptr());
    let mut acc = [_mm256_setzero_ps(); 4];
    let mut i = 0;
    while i + 32 <= n {
      for (lane, sum) in acc.iter_mut().enumerate() {
        let at = i + lane * 8;
        let d = _mm256_sub_ps(_mm256_loadu_ps(pa.add(at)), _mm256_loadu_ps(pb.add(at)));
        *sum = _mm256_fmadd_ps(d, d, *sum);
      }
      i += 32;
    }
    while i + 8 <= n {
      let d = _mm256_sub_ps(_mm256_loadu_ps(pa.add(i)), _mm256_loadu_ps(pb.add(i)));
      acc[0] = _mm256_fmadd_ps(d, d, acc[0]);
      i += 8;
    }
    let mut sum = reduce(acc);
    for (&x, &y) in a[i..n].iter().zip(&b[i..n]) {
      let d = x - y;
      sum += d * d;
    }
    sum
  }

  #[target_feature(enable = "avx")]
  unsafe fn squared_l2_avx(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let (pa, pb) = (a.as_ptr(), b.as_ptr());
    let mut acc = [_mm256_setzero_ps(); 4];
    let mut i = 0;
    while i + 32 <= n {
      for (lane, sum) in acc.iter_mut().enumerate() {
        let at = i + lane * 8;
        let d = _mm256_sub_ps(_mm256_loadu_ps(pa.add(at)), _mm256_loadu_ps(pb.add(at)));
        *sum = _mm256_add_ps(*sum, _mm256_mul_ps(d, d));
      }
      i += 32;
    }
    while i + 8 <= n {
      let d = _mm256_sub_ps(_mm256_loadu_ps(pa.add(i)), _mm256_loadu_ps(pb.add(i)));
      acc[0] = _mm256_add_ps(acc[0], _mm256_mul_ps(d, d));
      i += 8;
    }
    let mut sum = reduce(acc);
    for (&x, &y) in a[i..n].iter().zip(&b[i..n]) {
      let d = x - y;
      sum += d * d;
    }
    sum
  }

  #[target_feature(enable = "avx,fma")]
  unsafe fn sum_of_squares_fma(v: &[f32]) -> f32 {
    let n = v.len();
    let p = v.as_ptr();
    let mut acc = [_mm256_setzero_ps(); 4];
    let mut i = 0;
    while i + 32 <= n {
      for (lane, sum) in acc.iter_mut().enumerate() {
        let x = _mm256_loadu_ps(p.add(i + lane * 8));
        *sum = _mm256_fmadd_ps(x, x, *sum);
      }
      i += 32;
    }
    while i + 8 <= n {
      let x = _mm256_loadu_ps(p.add(i));
      acc[0] = _mm256_fmadd_ps(x, x, acc[0]);
      i += 8;
    }
    let mut sum = reduce(acc);
    for &x in &v[i..] {
      sum += x * x;
    }
    sum
  }

  #[target_feature(enable = "avx")]
  unsafe fn sum_of_squares_avx(v: &[f32]) -> f32 {
    let n = v.len();
    let p = v.as_ptr();
    let mut acc = [_mm256_setzero_ps(); 4];
    let mut i = 0;
    while i + 32 <= n {
      for (lane, sum) in acc.iter_mut().enumerate() {
        let x = _mm256_loadu_ps(p.add(i + lane * 8));
        *sum = _mm256_add_ps(*sum, _mm256_mul_ps(x, x));
      }
      i += 32;
    }
    while i + 8 <= n {
      let x = _mm256_loadu_ps(p.add(i));
      acc[0] = _mm256_add_ps(acc[0], _mm256_mul_ps(x, x));
      i += 8;
    }
    let mut sum = reduce(acc);
    for &x in &v[i..] {
      sum += x * x;
    }
    sum
  }
}

// ============================================================================
// Batch Distance Functions
// ============================================================================

/// Compute cosine distances from a query to multiple vectors in a row group
///
/// This is optimized for the case where vectors are stored contiguously.
/// Returns distances for vectors from `start_idx` to `start_idx + count - 1`.
pub fn batch_cosine_distance(
  query: &[f32],
  row_group_data: &[f32],
  dimensions: usize,
  start_idx: usize,
  count: usize,
) -> Vec<f32> {
  let mut results = Vec::with_capacity(count);

  for i in 0..count {
    let offset = (start_idx + i) * dimensions;
    let vector = &row_group_data[offset..offset + dimensions];
    results.push(cosine_distance(query, vector));
  }

  results
}

/// Compute squared Euclidean distances from a query to multiple vectors
pub fn batch_squared_euclidean(
  query: &[f32],
  row_group_data: &[f32],
  dimensions: usize,
  start_idx: usize,
  count: usize,
) -> Vec<f32> {
  let mut results = Vec::with_capacity(count);

  for i in 0..count {
    let offset = (start_idx + i) * dimensions;
    let vector = &row_group_data[offset..offset + dimensions];
    results.push(squared_euclidean(query, vector));
  }

  results
}

/// Compute dot product distances from a query to multiple vectors
/// (for inner product search, negate to get distance)
pub fn batch_dot_product_distance(
  query: &[f32],
  row_group_data: &[f32],
  dimensions: usize,
  start_idx: usize,
  count: usize,
) -> Vec<f32> {
  let mut results = Vec::with_capacity(count);

  for i in 0..count {
    let offset = (start_idx + i) * dimensions;
    let vector = &row_group_data[offset..offset + dimensions];
    results.push(-dot_product(query, vector)); // Negate for distance
  }

  results
}

/// Compute dot product at a specific index in row group data
/// This avoids allocating a slice when we know the exact offset.
#[inline]
pub fn dot_product_at(
  query: &[f32],
  row_group_data: &[f32],
  dimensions: usize,
  index: usize,
) -> f32 {
  let offset = index * dimensions;
  dot_product(query, &row_group_data[offset..offset + dimensions])
}

/// Compute squared Euclidean at a specific index in row group data
#[inline]
pub fn squared_euclidean_at(
  query: &[f32],
  row_group_data: &[f32],
  dimensions: usize,
  index: usize,
) -> f32 {
  let offset = index * dimensions;
  squared_euclidean(query, &row_group_data[offset..offset + dimensions])
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_dot_product() {
    let a = [1.0, 2.0, 3.0];
    let b = [4.0, 5.0, 6.0];
    assert_eq!(dot_product(&a, &b), 32.0);
  }

  #[test]
  fn test_dot_product_large() {
    // Test with > 8 elements to exercise SIMD path
    let a: Vec<f32> = (0..384).map(|i| i as f32 * 0.01).collect();
    let b: Vec<f32> = (0..384).map(|i| (384 - i) as f32 * 0.01).collect();

    let result = dot_product(&a, &b);
    let expected: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();

    assert!(
      (result - expected).abs() < 1e-3,
      "result: {result}, expected: {expected}"
    );
  }

  #[test]
  fn test_squared_euclidean() {
    let a = [1.0, 0.0, 0.0];
    let b = [0.0, 1.0, 0.0];
    assert_eq!(squared_euclidean(&a, &b), 2.0);
  }

  #[test]
  fn test_squared_euclidean_large() {
    // Test with > 8 elements
    let a: Vec<f32> = (0..384).map(|i| i as f32 * 0.01).collect();
    let b: Vec<f32> = (0..384).map(|i| (i + 1) as f32 * 0.01).collect();

    let result = squared_euclidean(&a, &b);
    let expected: f32 = a
      .iter()
      .zip(b.iter())
      .map(|(x, y)| {
        let d = x - y;
        d * d
      })
      .sum();

    assert!(
      (result - expected).abs() < 1e-3,
      "result: {result}, expected: {expected}"
    );
  }

  #[test]
  fn test_l2_norm() {
    let v = [3.0, 4.0];
    assert_eq!(l2_norm(&v), 5.0);
  }

  #[test]
  fn test_l2_norm_large() {
    let v: Vec<f32> = (0..384).map(|i| i as f32 * 0.01).collect();

    let result = l2_norm(&v);
    let expected: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();

    assert!(
      (result - expected).abs() < 1e-3,
      "result: {result}, expected: {expected}"
    );
  }

  #[test]
  fn test_normalize() {
    let v = [3.0, 4.0];
    let n = normalize(&v);
    assert!((n[0] - 0.6).abs() < 1e-6);
    assert!((n[1] - 0.8).abs() < 1e-6);
    assert!(is_normalized(&n, 1e-6));
  }

  #[test]
  fn test_normalize_large() {
    let v: Vec<f32> = (0..384).map(|i| (i + 1) as f32).collect();
    let n = normalize(&v);
    assert!(is_normalized(&n, 1e-5));
  }

  #[test]
  fn test_batch_cosine_distance() {
    let query = [1.0, 0.0, 0.0];
    let row_group = [
      1.0, 0.0, 0.0, // Vector 0: identical
      0.0, 1.0, 0.0, // Vector 1: orthogonal
      -1.0, 0.0, 0.0, // Vector 2: opposite
    ];

    let distances = batch_cosine_distance(&query, &row_group, 3, 0, 3);

    assert!((distances[0] - 0.0).abs() < 1e-6); // Identical
    assert!((distances[1] - 1.0).abs() < 1e-6); // Orthogonal
    assert!((distances[2] - 2.0).abs() < 1e-6); // Opposite
  }

  #[test]
  fn test_batch_squared_euclidean() {
    let query = [0.0, 0.0, 0.0];
    let row_group = [
      1.0, 0.0, 0.0, // Vector 0: distance 1
      0.0, 2.0, 0.0, // Vector 1: distance 4
      0.0, 0.0, 3.0, // Vector 2: distance 9
    ];

    let distances = batch_squared_euclidean(&query, &row_group, 3, 0, 3);

    assert!((distances[0] - 1.0).abs() < 1e-6);
    assert!((distances[1] - 4.0).abs() < 1e-6);
    assert!((distances[2] - 9.0).abs() < 1e-6);
  }

  #[test]
  fn test_dot_product_at() {
    let query = [1.0, 2.0, 3.0];
    let row_group = [
      4.0, 5.0, 6.0, // Index 0
      7.0, 8.0, 9.0, // Index 1
    ];

    assert_eq!(dot_product_at(&query, &row_group, 3, 0), 32.0);
    assert_eq!(dot_product_at(&query, &row_group, 3, 1), 50.0);
  }

  /// Deterministic inputs in [-1, 1).
  fn kernel_inputs(len: usize, seed: u32) -> Vec<f32> {
    (0..len)
      .map(|i| {
        let x = (i as u32).wrapping_mul(2_654_435_761).wrapping_add(seed);
        (x >> 8) as f32 / (1u32 << 23) as f32 - 1.0
      })
      .collect()
  }

  fn assert_close(label: &str, len: usize, got: f32, expected: f64) {
    let tolerance = 1e-5 * (1.0 + expected.abs()) * (len.max(1) as f64).sqrt();
    assert!(
      (got as f64 - expected).abs() <= tolerance,
      "{label} len {len}: got {got}, expected {expected}"
    );
  }

  /// Every kernel (the selected SIMD one and the portable one) matches an
  /// f64 reference for lengths that exercise the block, step and tail paths.
  #[test]
  fn test_kernels_match_reference_for_all_lengths() {
    let lengths = (0..=70).chain([127, 128, 129, 384, 1000, 1536]);
    for len in lengths {
      let a = kernel_inputs(len, 1);
      let b = kernel_inputs(len, 2);
      let dot: f64 = a.iter().zip(&b).map(|(&x, &y)| x as f64 * y as f64).sum();
      let sq: f64 = a
        .iter()
        .zip(&b)
        .map(|(&x, &y)| (x as f64 - y as f64).powi(2))
        .sum();
      let sum_sq: f64 = a.iter().map(|&x| (x as f64).powi(2)).sum();

      assert_close("dot", len, kernel::dot(&a, &b), dot);
      assert_close("portable dot", len, portable::dot(&a, &b), dot);
      assert_close("squared_l2", len, kernel::squared_l2(&a, &b), sq);
      assert_close("portable squared_l2", len, portable::squared_l2(&a, &b), sq);
      assert_close("sum_of_squares", len, kernel::sum_of_squares(&a), sum_sq);
      assert_close(
        "portable sum_of_squares",
        len,
        portable::sum_of_squares(&a),
        sum_sq,
      );
    }
  }

  /// Kernels read only the common prefix of unequal slices.
  #[test]
  fn test_kernels_read_only_the_common_prefix() {
    let a = kernel_inputs(37, 3);
    let b = kernel_inputs(64, 4);
    assert_eq!(kernel::dot(&a, &b), kernel::dot(&a, &b[..37]));
    assert_eq!(kernel::squared_l2(&b, &a), kernel::squared_l2(&b[..37], &a));
  }

  #[test]
  fn test_l2_norm_survives_overflow_and_underflow() {
    assert!((l2_norm(&[3e20, 4e20]) / 5e20 - 1.0).abs() < 1e-6);
    assert!((l2_norm(&[3e-25, 4e-25]) / 5e-25 - 1.0).abs() < 1e-6);
    assert_eq!(l2_norm(&[0.0; 5]), 0.0);
    assert!(l2_norm(&[f32::MAX, f32::MAX]).is_infinite());
    assert!(l2_norm(&[1.0, f32::NAN]).is_nan());
  }

  #[test]
  fn test_normalize_reaches_unit_length_across_the_f32_range() {
    for v in [
      vec![3e20f32, 4e20],
      vec![3e-25, 4e-25],
      vec![1e-45, 0.0, 0.0],
      vec![f32::MAX, -f32::MAX, f32::MAX],
      vec![1e30, 1e-30, 0.0],
    ] {
      let n = normalize(&v);
      assert!(
        n.iter().all(|x| x.is_finite()) && is_normalized(&n, 1e-5),
        "normalize({v:?}) = {n:?}"
      );
    }
    let n = normalize(&[3e20, 4e20]);
    assert!((n[0] - 0.6).abs() < 1e-6 && (n[1] - 0.8).abs() < 1e-6);

    // Vectors without a direction are left as they are.
    assert_eq!(normalize(&[0.0, 0.0]), vec![0.0, 0.0]);
    assert_eq!(normalize(&[f32::INFINITY, 1.0]), vec![f32::INFINITY, 1.0]);
    assert!(normalize(&[f32::NAN, 1.0])[0].is_nan());
  }

  #[test]
  fn test_static_dispatch_matches_metric_distance_fn() {
    let a = kernel_inputs(19, 5);
    let b = kernel_inputs(19, 6);
    for metric in [
      DistanceMetric::Cosine,
      DistanceMetric::Euclidean,
      DistanceMetric::DotProduct,
    ] {
      let expected = metric.distance_fn()(&a, &b);
      assert_eq!(with_metric_distance!(metric, |dist| dist(&a, &b)), expected);
      assert_eq!(metric_distance_fn(metric)(&a, &b), expected);
      assert_eq!(
        with_metric_distance!(metric, stored_normalized = true, |dist| dist(&a, &b)),
        expected
      );
    }
    assert_eq!(
      with_metric_distance!(DistanceMetric::Cosine, stored_normalized = false, |dist| {
        dist(&a, &b)
      }),
      cosine_distance_unit_query(&a, &b)
    );
  }
}
