//! Distance functions with SIMD acceleration
//!
//! Provides optimized distance calculations for vector similarity search.
//! Uses platform-specific SIMD intrinsics when available (x86_64 AVX/SSE),
//! with scalar fallbacks for other platforms.
//!
//! Ported from src/vector/distance.ts

// ============================================================================
// SIMD Support Detection
// ============================================================================

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

/// Check if AVX2 is available at runtime
#[cfg(target_arch = "x86_64")]
#[inline]
fn has_avx2() -> bool {
  is_x86_feature_detected!("avx2")
}

/// Check if AVX is available at runtime
#[cfg(target_arch = "x86_64")]
#[inline]
fn has_avx() -> bool {
  is_x86_feature_detected!("avx")
}

// ============================================================================
// Dot Product
// ============================================================================

/// Dot product of two vectors
///
/// Automatically uses the best available implementation:
/// - AVX2 (8-wide f32) if available
/// - AVX (8-wide f32) if available  
/// - Loop-unrolled scalar otherwise
#[inline]
pub fn dot_product(a: &[f32], b: &[f32]) -> f32 {
  assert_eq!(a.len(), b.len(), "vector length mismatch");

  #[cfg(target_arch = "x86_64")]
  {
    if has_avx() {
      // SAFETY: guarded by runtime AVX detection.
      return unsafe { dot_product_avx(a, b) };
    }
  }

  dot_product_unrolled(a, b)
}

/// Loop-unrolled dot product (8-way unrolling for auto-vectorization)
#[inline]
fn dot_product_unrolled(a: &[f32], b: &[f32]) -> f32 {
  let len = a.len();
  let chunks = len / 8;
  let remainder = len % 8;

  let mut sum0 = 0.0f32;
  let mut sum1 = 0.0f32;
  let mut sum2 = 0.0f32;
  let mut sum3 = 0.0f32;
  let mut sum4 = 0.0f32;
  let mut sum5 = 0.0f32;
  let mut sum6 = 0.0f32;
  let mut sum7 = 0.0f32;

  for i in 0..chunks {
    let base = i * 8;
    sum0 += a[base] * b[base];
    sum1 += a[base + 1] * b[base + 1];
    sum2 += a[base + 2] * b[base + 2];
    sum3 += a[base + 3] * b[base + 3];
    sum4 += a[base + 4] * b[base + 4];
    sum5 += a[base + 5] * b[base + 5];
    sum6 += a[base + 6] * b[base + 6];
    sum7 += a[base + 7] * b[base + 7];
  }

  // Handle remainder
  let base = chunks * 8;
  for i in 0..remainder {
    sum0 += a[base + i] * b[base + i];
  }

  sum0 + sum1 + sum2 + sum3 + sum4 + sum5 + sum6 + sum7
}

/// AVX-accelerated dot product (8-wide f32)
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx")]
unsafe fn dot_product_avx(a: &[f32], b: &[f32]) -> f32 {
  // SAFETY: caller guarantees AVX support via has_avx().
  let len = a.len();
  let chunks = len / 8;
  let remainder = len % 8;

  let mut sum = _mm256_setzero_ps();

  for i in 0..chunks {
    let base = i * 8;
    let va = _mm256_loadu_ps(a.as_ptr().add(base));
    let vb = _mm256_loadu_ps(b.as_ptr().add(base));
    sum = _mm256_add_ps(sum, _mm256_mul_ps(va, vb));
  }

  // Horizontal sum of AVX register
  let sum128_lo = _mm256_castps256_ps128(sum);
  let sum128_hi = _mm256_extractf128_ps(sum, 1);
  let sum128 = _mm_add_ps(sum128_lo, sum128_hi);
  let sum64 = _mm_add_ps(sum128, _mm_movehl_ps(sum128, sum128));
  let sum32 = _mm_add_ss(sum64, _mm_shuffle_ps(sum64, sum64, 1));
  let mut result = _mm_cvtss_f32(sum32);

  // Handle remainder
  let base = chunks * 8;
  for i in 0..remainder {
    result += a[base + i] * b[base + i];
  }

  result
}

// ============================================================================
// Squared Euclidean Distance
// ============================================================================

/// Squared Euclidean distance between two vectors
///
/// Automatically uses the best available implementation.
#[inline]
pub fn squared_euclidean(a: &[f32], b: &[f32]) -> f32 {
  assert_eq!(a.len(), b.len(), "vector length mismatch");

  #[cfg(target_arch = "x86_64")]
  {
    if has_avx() {
      // SAFETY: guarded by runtime AVX detection.
      return unsafe { squared_euclidean_avx(a, b) };
    }
  }

  squared_euclidean_unrolled(a, b)
}

/// Loop-unrolled squared Euclidean distance
#[inline]
fn squared_euclidean_unrolled(a: &[f32], b: &[f32]) -> f32 {
  let len = a.len();
  let chunks = len / 8;
  let remainder = len % 8;

  let mut sum0 = 0.0f32;
  let mut sum1 = 0.0f32;
  let mut sum2 = 0.0f32;
  let mut sum3 = 0.0f32;
  let mut sum4 = 0.0f32;
  let mut sum5 = 0.0f32;
  let mut sum6 = 0.0f32;
  let mut sum7 = 0.0f32;

  for i in 0..chunks {
    let base = i * 8;
    let d0 = a[base] - b[base];
    let d1 = a[base + 1] - b[base + 1];
    let d2 = a[base + 2] - b[base + 2];
    let d3 = a[base + 3] - b[base + 3];
    let d4 = a[base + 4] - b[base + 4];
    let d5 = a[base + 5] - b[base + 5];
    let d6 = a[base + 6] - b[base + 6];
    let d7 = a[base + 7] - b[base + 7];

    sum0 += d0 * d0;
    sum1 += d1 * d1;
    sum2 += d2 * d2;
    sum3 += d3 * d3;
    sum4 += d4 * d4;
    sum5 += d5 * d5;
    sum6 += d6 * d6;
    sum7 += d7 * d7;
  }

  // Handle remainder
  let base = chunks * 8;
  for i in 0..remainder {
    let d = a[base + i] - b[base + i];
    sum0 += d * d;
  }

  sum0 + sum1 + sum2 + sum3 + sum4 + sum5 + sum6 + sum7
}

/// AVX-accelerated squared Euclidean distance
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx")]
unsafe fn squared_euclidean_avx(a: &[f32], b: &[f32]) -> f32 {
  // SAFETY: caller guarantees AVX support via has_avx().
  let len = a.len();
  let chunks = len / 8;
  let remainder = len % 8;

  let mut sum = _mm256_setzero_ps();

  for i in 0..chunks {
    let base = i * 8;
    let va = _mm256_loadu_ps(a.as_ptr().add(base));
    let vb = _mm256_loadu_ps(b.as_ptr().add(base));
    let diff = _mm256_sub_ps(va, vb);
    sum = _mm256_add_ps(sum, _mm256_mul_ps(diff, diff));
  }

  // Horizontal sum
  let sum128_lo = _mm256_castps256_ps128(sum);
  let sum128_hi = _mm256_extractf128_ps(sum, 1);
  let sum128 = _mm_add_ps(sum128_lo, sum128_hi);
  let sum64 = _mm_add_ps(sum128, _mm_movehl_ps(sum128, sum128));
  let sum32 = _mm_add_ss(sum64, _mm_shuffle_ps(sum64, sum64, 1));
  let mut result = _mm_cvtss_f32(sum32);

  // Handle remainder
  let base = chunks * 8;
  for i in 0..remainder {
    let d = a[base + i] - b[base + i];
    result += d * d;
  }

  result
}

// ============================================================================
// Other Distance Functions
// ============================================================================

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

/// L2 norm of a vector
#[inline]
pub fn l2_norm(v: &[f32]) -> f32 {
  #[cfg(target_arch = "x86_64")]
  {
    if has_avx() {
      // SAFETY: guarded by runtime AVX detection.
      return unsafe { l2_norm_avx(v) };
    }
  }

  l2_norm_unrolled(v)
}

/// Loop-unrolled L2 norm
#[inline]
fn l2_norm_unrolled(v: &[f32]) -> f32 {
  let len = v.len();
  let chunks = len / 8;
  let remainder = len % 8;

  let mut sum0 = 0.0f32;
  let mut sum1 = 0.0f32;
  let mut sum2 = 0.0f32;
  let mut sum3 = 0.0f32;
  let mut sum4 = 0.0f32;
  let mut sum5 = 0.0f32;
  let mut sum6 = 0.0f32;
  let mut sum7 = 0.0f32;

  for i in 0..chunks {
    let base = i * 8;
    sum0 += v[base] * v[base];
    sum1 += v[base + 1] * v[base + 1];
    sum2 += v[base + 2] * v[base + 2];
    sum3 += v[base + 3] * v[base + 3];
    sum4 += v[base + 4] * v[base + 4];
    sum5 += v[base + 5] * v[base + 5];
    sum6 += v[base + 6] * v[base + 6];
    sum7 += v[base + 7] * v[base + 7];
  }

  let base = chunks * 8;
  for i in 0..remainder {
    sum0 += v[base + i] * v[base + i];
  }

  (sum0 + sum1 + sum2 + sum3 + sum4 + sum5 + sum6 + sum7).sqrt()
}

/// AVX-accelerated L2 norm
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx")]
unsafe fn l2_norm_avx(v: &[f32]) -> f32 {
  // SAFETY: caller guarantees AVX support via has_avx().
  let len = v.len();
  let chunks = len / 8;
  let remainder = len % 8;

  let mut sum = _mm256_setzero_ps();

  for i in 0..chunks {
    let base = i * 8;
    let va = _mm256_loadu_ps(v.as_ptr().add(base));
    sum = _mm256_add_ps(sum, _mm256_mul_ps(va, va));
  }

  // Horizontal sum
  let sum128_lo = _mm256_castps256_ps128(sum);
  let sum128_hi = _mm256_extractf128_ps(sum, 1);
  let sum128 = _mm_add_ps(sum128_lo, sum128_hi);
  let sum64 = _mm_add_ps(sum128, _mm_movehl_ps(sum128, sum128));
  let sum32 = _mm_add_ss(sum64, _mm_shuffle_ps(sum64, sum64, 1));
  let mut result = _mm_cvtss_f32(sum32);

  // Handle remainder
  let base = chunks * 8;
  for i in 0..remainder {
    result += v[base + i] * v[base + i];
  }

  result.sqrt()
}

// ============================================================================
// Normalization
// ============================================================================

/// Normalize a vector in-place
pub fn normalize_in_place(v: &mut [f32]) {
  let norm = l2_norm(v);
  if norm > 1e-10 {
    let inv_norm = 1.0 / norm;

    #[cfg(target_arch = "x86_64")]
    {
      if has_avx() {
        // SAFETY: guarded by runtime AVX detection.
        unsafe {
          normalize_in_place_avx(v, inv_norm);
        }
        return;
      }
    }

    // Scalar fallback with unrolling
    let len = v.len();
    let chunks = len / 8;
    let remainder = len % 8;

    for i in 0..chunks {
      let base = i * 8;
      v[base] *= inv_norm;
      v[base + 1] *= inv_norm;
      v[base + 2] *= inv_norm;
      v[base + 3] *= inv_norm;
      v[base + 4] *= inv_norm;
      v[base + 5] *= inv_norm;
      v[base + 6] *= inv_norm;
      v[base + 7] *= inv_norm;
    }

    let base = chunks * 8;
    for i in 0..remainder {
      v[base + i] *= inv_norm;
    }
  }
}

/// AVX-accelerated in-place normalization
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx")]
unsafe fn normalize_in_place_avx(v: &mut [f32], inv_norm: f32) {
  // SAFETY: caller guarantees AVX support via has_avx().
  let len = v.len();
  let chunks = len / 8;
  let remainder = len % 8;

  let inv_norm_vec = _mm256_set1_ps(inv_norm);

  for i in 0..chunks {
    let base = i * 8;
    let va = _mm256_loadu_ps(v.as_ptr().add(base));
    let result = _mm256_mul_ps(va, inv_norm_vec);
    _mm256_storeu_ps(v.as_mut_ptr().add(base), result);
  }

  // Handle remainder
  let base = chunks * 8;
  for i in 0..remainder {
    v[base + i] *= inv_norm;
  }
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

  #[test]
  fn test_unrolled_matches_simple() {
    // Verify unrolled versions match simple iterator-based versions
    let a: Vec<f32> = (0..100).map(|i| i as f32 * 0.1).collect();
    let b: Vec<f32> = (0..100).map(|i| (100 - i) as f32 * 0.1).collect();

    // Simple implementations for comparison
    let simple_dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let simple_sq_eu: f32 = a.iter().zip(b.iter()).map(|(x, y)| (x - y).powi(2)).sum();

    let unrolled_dot = dot_product_unrolled(&a, &b);
    let unrolled_sq_eu = squared_euclidean_unrolled(&a, &b);

    assert!((unrolled_dot - simple_dot).abs() < 1e-3);
    assert!((unrolled_sq_eu - simple_sq_eu).abs() < 1e-3);
  }
}
