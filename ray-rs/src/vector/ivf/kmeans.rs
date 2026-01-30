//! K-means clustering for IVF index training
//!
//! Implements k-means++ initialization and Lloyd's algorithm.
//! Includes parallel versions using rayon for multi-core speedup.
//!
//! Ported from src/vector/ivf-index.ts (training portion)

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
#[cfg(not(target_arch = "wasm32"))]
use rayon::prelude::*;

#[cfg(test)]
use crate::vector::distance::squared_euclidean;

// ============================================================================
// K-Means Configuration
// ============================================================================

/// Configuration for k-means clustering
#[derive(Debug, Clone)]
pub struct KMeansConfig {
  /// Number of clusters (k)
  pub n_clusters: usize,
  /// Maximum iterations
  pub max_iterations: usize,
  /// Convergence tolerance (relative inertia change)
  pub tolerance: f32,
  /// Random seed (None for random)
  pub seed: Option<u64>,
}

impl Default for KMeansConfig {
  fn default() -> Self {
    Self {
      n_clusters: 100,
      max_iterations: 25,
      tolerance: 1e-4,
      seed: None,
    }
  }
}

impl KMeansConfig {
  pub fn new(n_clusters: usize) -> Self {
    Self {
      n_clusters,
      ..Default::default()
    }
  }

  pub fn with_max_iterations(mut self, max_iterations: usize) -> Self {
    self.max_iterations = max_iterations;
    self
  }

  pub fn with_tolerance(mut self, tolerance: f32) -> Self {
    self.tolerance = tolerance;
    self
  }

  pub fn with_seed(mut self, seed: u64) -> Self {
    self.seed = Some(seed);
    self
  }
}

// ============================================================================
// K-Means Result
// ============================================================================

/// Result of k-means clustering
#[derive(Debug, Clone)]
pub struct KMeansResult {
  /// Centroids (k * dimensions)
  pub centroids: Vec<f32>,
  /// Cluster assignments for each vector
  pub assignments: Vec<u32>,
  /// Final inertia (sum of squared distances to centroids)
  pub inertia: f32,
  /// Number of iterations performed
  pub iterations: usize,
  /// Whether converged (inertia change < tolerance)
  pub converged: bool,
}

// ============================================================================
// K-Means Algorithm
// ============================================================================

/// Run k-means clustering on vectors
///
/// # Arguments
/// * `vectors` - Contiguous vector data (n * dimensions)
/// * `n` - Number of vectors
/// * `dimensions` - Number of dimensions per vector
/// * `config` - K-means configuration
/// * `distance_fn` - Distance function to use
///
/// # Returns
/// K-means result with centroids and assignments
pub fn kmeans(
  vectors: &[f32],
  n: usize,
  dimensions: usize,
  config: &KMeansConfig,
  distance_fn: fn(&[f32], &[f32]) -> f32,
) -> Result<KMeansResult, KMeansError> {
  if n < config.n_clusters {
    return Err(KMeansError::NotEnoughVectors {
      n,
      k: config.n_clusters,
    });
  }

  if vectors.len() != n * dimensions {
    return Err(KMeansError::DimensionMismatch {
      expected: n * dimensions,
      got: vectors.len(),
    });
  }

  let k = config.n_clusters;

  // Initialize centroids using k-means++
  let mut centroids = kmeans_plus_plus_init(vectors, n, dimensions, k, distance_fn, config.seed);

  // Run Lloyd's algorithm
  let mut assignments = vec![0u32; n];
  let mut prev_inertia = f32::INFINITY;
  let mut iterations = 0;
  let mut converged = false;

  for iter in 0..config.max_iterations {
    iterations = iter + 1;

    // Assign vectors to nearest centroids
    let inertia = assign_to_centroids(
      vectors,
      n,
      dimensions,
      &centroids,
      k,
      &mut assignments,
      distance_fn,
    );

    // Check for convergence
    let inertia_change = (prev_inertia - inertia).abs() / inertia.max(1.0);
    if inertia_change < config.tolerance {
      converged = true;
      break;
    }
    prev_inertia = inertia;

    // Update centroids
    update_centroids(vectors, n, dimensions, &assignments, k, &mut centroids);
  }

  // Final assignment pass
  let inertia = assign_to_centroids(
    vectors,
    n,
    dimensions,
    &centroids,
    k,
    &mut assignments,
    distance_fn,
  );

  Ok(KMeansResult {
    centroids,
    assignments,
    inertia,
    iterations,
    converged,
  })
}

/// K-means++ initialization for better starting positions
fn kmeans_plus_plus_init(
  vectors: &[f32],
  n: usize,
  dimensions: usize,
  k: usize,
  distance_fn: fn(&[f32], &[f32]) -> f32,
  seed: Option<u64>,
) -> Vec<f32> {
  let mut rng: StdRng = match seed {
    Some(s) => StdRng::seed_from_u64(s),
    None => StdRng::from_entropy(),
  };

  let mut centroids = Vec::with_capacity(k * dimensions);

  // First centroid: random vector
  let first_idx = rng.gen_range(0..n);
  let first_offset = first_idx * dimensions;
  centroids.extend_from_slice(&vectors[first_offset..first_offset + dimensions]);

  // Remaining centroids: weighted by distance squared
  let mut min_dists = vec![f32::INFINITY; n];

  for c in 1..k {
    // Update min distances to nearest centroid
    let prev_cent_offset = (c - 1) * dimensions;
    let prev_centroid = &centroids[prev_cent_offset..prev_cent_offset + dimensions];

    let mut total_dist = 0.0;
    for (i, min_dist) in min_dists.iter_mut().enumerate().take(n) {
      let vec_offset = i * dimensions;
      let vec = &vectors[vec_offset..vec_offset + dimensions];
      let dist = distance_fn(vec, prev_centroid);
      // Use abs(dist)^2 for k-means++ (handles negative distances like dot product)
      let abs_dist = dist.abs();
      *min_dist = (*min_dist).min(abs_dist * abs_dist);
      total_dist += *min_dist;
    }

    // Weighted random selection
    let mut r = rng.gen::<f32>() * total_dist;
    let mut selected_idx = 0;

    for (i, dist) in min_dists.iter().enumerate().take(n) {
      r -= *dist;
      if r <= 0.0 {
        selected_idx = i;
        break;
      }
    }

    // Copy selected vector to centroids
    let selected_offset = selected_idx * dimensions;
    centroids.extend_from_slice(&vectors[selected_offset..selected_offset + dimensions]);
  }

  centroids
}

/// Assign vectors to nearest centroids
/// Returns total inertia (sum of squared distances)
fn assign_to_centroids(
  vectors: &[f32],
  n: usize,
  dimensions: usize,
  centroids: &[f32],
  k: usize,
  assignments: &mut [u32],
  distance_fn: fn(&[f32], &[f32]) -> f32,
) -> f32 {
  let mut inertia = 0.0;

  for (i, assignment) in assignments.iter_mut().enumerate().take(n) {
    let vec_offset = i * dimensions;
    let vec = &vectors[vec_offset..vec_offset + dimensions];

    let mut best_cluster = 0;
    let mut best_dist = f32::INFINITY;

    for c in 0..k {
      let cent_offset = c * dimensions;
      let centroid = &centroids[cent_offset..cent_offset + dimensions];
      let dist = distance_fn(vec, centroid);

      if dist < best_dist {
        best_dist = dist;
        best_cluster = c;
      }
    }

    *assignment = best_cluster as u32;
    inertia += best_dist;
  }

  inertia
}

/// Update centroids based on current assignments
fn update_centroids(
  vectors: &[f32],
  n: usize,
  dimensions: usize,
  assignments: &[u32],
  k: usize,
  centroids: &mut [f32],
) {
  // Compute cluster sums and counts
  let mut cluster_sums = vec![0.0f32; k * dimensions];
  let mut cluster_counts = vec![0u32; k];

  for (i, &cluster_id) in assignments.iter().enumerate().take(n) {
    let cluster = cluster_id as usize;
    let vec_offset = i * dimensions;
    let sum_offset = cluster * dimensions;

    for d in 0..dimensions {
      cluster_sums[sum_offset + d] += vectors[vec_offset + d];
    }
    cluster_counts[cluster] += 1;
  }

  // Update centroids
  for (c, &count) in cluster_counts.iter().enumerate().take(k) {
    if count == 0 {
      // Keep existing centroid (shouldn't happen with k-means++)
      continue;
    }

    let offset = c * dimensions;
    for d in 0..dimensions {
      centroids[offset + d] = cluster_sums[offset + d] / count as f32;
    }
  }
}

/// Reinitialize empty clusters with random vectors
#[allow(dead_code)]
fn reinitialize_empty_clusters(
  vectors: &[f32],
  n: usize,
  dimensions: usize,
  cluster_counts: &[u32],
  centroids: &mut [f32],
) {
  let mut rng = rand::thread_rng();

  for (c, &count) in cluster_counts.iter().enumerate() {
    if count == 0 {
      let rand_idx = rng.gen_range(0..n);
      let rand_offset = rand_idx * dimensions;
      let cent_offset = c * dimensions;

      centroids[cent_offset..cent_offset + dimensions]
        .copy_from_slice(&vectors[rand_offset..rand_offset + dimensions]);
    }
  }
}

// ============================================================================
// Parallel K-Means Algorithm
// ============================================================================

/// Minimum vectors per thread for parallelization to be beneficial
const MIN_VECTORS_PER_THREAD: usize = 1000;

/// Run parallel k-means clustering on vectors
///
/// Uses rayon for parallel assignment and centroid update steps.
/// Falls back to sequential for small datasets where parallelization overhead
/// would outweigh benefits.
///
/// # Arguments
/// * `vectors` - Contiguous vector data (n * dimensions)
/// * `n` - Number of vectors
/// * `dimensions` - Number of dimensions per vector
/// * `config` - K-means configuration
/// * `distance_fn` - Distance function to use
///
/// # Returns
/// K-means result with centroids and assignments
pub fn kmeans_parallel(
  vectors: &[f32],
  n: usize,
  dimensions: usize,
  config: &KMeansConfig,
  distance_fn: fn(&[f32], &[f32]) -> f32,
) -> Result<KMeansResult, KMeansError> {
  // Fall back to sequential for small datasets
  if n < MIN_VECTORS_PER_THREAD * 2 {
    return kmeans(vectors, n, dimensions, config, distance_fn);
  }

  if n < config.n_clusters {
    return Err(KMeansError::NotEnoughVectors {
      n,
      k: config.n_clusters,
    });
  }

  if vectors.len() != n * dimensions {
    return Err(KMeansError::DimensionMismatch {
      expected: n * dimensions,
      got: vectors.len(),
    });
  }

  let k = config.n_clusters;

  // Initialize centroids using k-means++ (sequential, relatively fast)
  let mut centroids = kmeans_plus_plus_init(vectors, n, dimensions, k, distance_fn, config.seed);

  // Run Lloyd's algorithm with parallel steps
  let mut assignments = vec![0u32; n];
  let mut prev_inertia = f32::INFINITY;
  let mut iterations = 0;
  let mut converged = false;

  for iter in 0..config.max_iterations {
    iterations = iter + 1;

    // Parallel assign vectors to nearest centroids
    let inertia = assign_to_centroids_parallel(
      vectors,
      n,
      dimensions,
      &centroids,
      k,
      &mut assignments,
      distance_fn,
    );

    // Check for convergence
    let inertia_change = (prev_inertia - inertia).abs() / inertia.max(1.0);
    if inertia_change < config.tolerance {
      converged = true;
      break;
    }
    prev_inertia = inertia;

    // Parallel update centroids
    update_centroids_parallel(vectors, n, dimensions, &assignments, k, &mut centroids);
  }

  // Final assignment pass
  let inertia = assign_to_centroids_parallel(
    vectors,
    n,
    dimensions,
    &centroids,
    k,
    &mut assignments,
    distance_fn,
  );

  Ok(KMeansResult {
    centroids,
    assignments,
    inertia,
    iterations,
    converged,
  })
}

/// Parallel assign vectors to nearest centroids
/// Returns total inertia (sum of squared distances)
#[cfg(not(target_arch = "wasm32"))]
fn assign_to_centroids_parallel(
  vectors: &[f32],
  n: usize,
  dimensions: usize,
  centroids: &[f32],
  k: usize,
  assignments: &mut [u32],
  distance_fn: fn(&[f32], &[f32]) -> f32,
) -> f32 {
  // Parallel compute assignments and distances
  let results: Vec<(u32, f32)> = (0..n)
    .into_par_iter()
    .map(|i| {
      let vec_offset = i * dimensions;
      let vec = &vectors[vec_offset..vec_offset + dimensions];

      let mut best_cluster = 0u32;
      let mut best_dist = f32::INFINITY;

      for c in 0..k {
        let cent_offset = c * dimensions;
        let centroid = &centroids[cent_offset..cent_offset + dimensions];
        let dist = distance_fn(vec, centroid);

        if dist < best_dist {
          best_dist = dist;
          best_cluster = c as u32;
        }
      }

      (best_cluster, best_dist)
    })
    .collect();

  // Update assignments and compute total inertia
  let mut inertia = 0.0f32;
  for (i, (cluster, dist)) in results.into_iter().enumerate() {
    assignments[i] = cluster;
    inertia += dist;
  }

  inertia
}

#[cfg(target_arch = "wasm32")]
fn assign_to_centroids_parallel(
  vectors: &[f32],
  n: usize,
  dimensions: usize,
  centroids: &[f32],
  k: usize,
  assignments: &mut [u32],
  distance_fn: fn(&[f32], &[f32]) -> f32,
) -> f32 {
  let mut inertia = 0.0f32;
  for i in 0..n {
    let vec_offset = i * dimensions;
    let vec = &vectors[vec_offset..vec_offset + dimensions];

    let mut best_cluster = 0u32;
    let mut best_dist = f32::INFINITY;

    for c in 0..k {
      let cent_offset = c * dimensions;
      let centroid = &centroids[cent_offset..cent_offset + dimensions];
      let dist = distance_fn(vec, centroid);

      if dist < best_dist {
        best_dist = dist;
        best_cluster = c as u32;
      }
    }

    assignments[i] = best_cluster;
    inertia += best_dist;
  }

  inertia
}

/// Parallel update centroids based on current assignments
#[cfg(not(target_arch = "wasm32"))]
fn update_centroids_parallel(
  vectors: &[f32],
  n: usize,
  dimensions: usize,
  assignments: &[u32],
  k: usize,
  centroids: &mut [f32],
) {
  // Parallel compute cluster sums using thread-local accumulators
  // Then reduce to final sums
  let (cluster_sums, cluster_counts) = (0..n)
    .into_par_iter()
    .fold(
      || (vec![0.0f32; k * dimensions], vec![0u32; k]),
      |(mut sums, mut counts), i| {
        let cluster = assignments[i] as usize;
        let vec_offset = i * dimensions;
        let sum_offset = cluster * dimensions;

        for d in 0..dimensions {
          sums[sum_offset + d] += vectors[vec_offset + d];
        }
        counts[cluster] += 1;

        (sums, counts)
      },
    )
    .reduce(
      || (vec![0.0f32; k * dimensions], vec![0u32; k]),
      |(mut sums1, mut counts1), (sums2, counts2)| {
        for i in 0..sums1.len() {
          sums1[i] += sums2[i];
        }
        for i in 0..counts1.len() {
          counts1[i] += counts2[i];
        }
        (sums1, counts1)
      },
    );

  // Update centroids (sequential, small work)
  for (c, &count) in cluster_counts.iter().enumerate() {
    if count == 0 {
      continue;
    }

    let offset = c * dimensions;
    for d in 0..dimensions {
      centroids[offset + d] = cluster_sums[offset + d] / count as f32;
    }
  }
}

#[cfg(target_arch = "wasm32")]
fn update_centroids_parallel(
  vectors: &[f32],
  n: usize,
  dimensions: usize,
  assignments: &[u32],
  k: usize,
  centroids: &mut [f32],
) {
  let mut cluster_sums = vec![0.0f32; k * dimensions];
  let mut cluster_counts = vec![0u32; k];

  for i in 0..n {
    let cluster = assignments[i] as usize;
    let vec_offset = i * dimensions;
    let sum_offset = cluster * dimensions;

    for d in 0..dimensions {
      cluster_sums[sum_offset + d] += vectors[vec_offset + d];
    }
    cluster_counts[cluster] += 1;
  }

  for (c, &count) in cluster_counts.iter().enumerate() {
    if count == 0 {
      continue;
    }

    let offset = c * dimensions;
    for d in 0..dimensions {
      centroids[offset + d] = cluster_sums[offset + d] / count as f32;
    }
  }
}

/// Parallel k-means++ initialization
///
/// Note: This is partially parallel - the distance updates are parallel,
/// but the weighted selection is sequential (inherently serial).
#[allow(dead_code)]
#[cfg(not(target_arch = "wasm32"))]
fn kmeans_plus_plus_init_parallel(
  vectors: &[f32],
  n: usize,
  dimensions: usize,
  k: usize,
  distance_fn: fn(&[f32], &[f32]) -> f32,
  seed: Option<u64>,
) -> Vec<f32> {
  let mut rng: StdRng = match seed {
    Some(s) => StdRng::seed_from_u64(s),
    None => StdRng::from_entropy(),
  };

  let mut centroids = Vec::with_capacity(k * dimensions);

  // First centroid: random vector
  let first_idx = rng.gen_range(0..n);
  let first_offset = first_idx * dimensions;
  centroids.extend_from_slice(&vectors[first_offset..first_offset + dimensions]);

  let mut min_dists = vec![f32::INFINITY; n];

  for c in 1..k {
    let prev_cent_offset = (c - 1) * dimensions;
    let prev_centroid = &centroids[prev_cent_offset..prev_cent_offset + dimensions];

    // Parallel update min distances
    let new_dists: Vec<f32> = (0..n)
      .into_par_iter()
      .map(|i| {
        let vec_offset = i * dimensions;
        let vec = &vectors[vec_offset..vec_offset + dimensions];
        let dist = distance_fn(vec, prev_centroid);
        let abs_dist = dist.abs();
        min_dists[i].min(abs_dist * abs_dist)
      })
      .collect();

    // Update min_dists and compute total
    let mut total_dist = 0.0f32;
    for (i, dist) in new_dists.into_iter().enumerate() {
      min_dists[i] = dist;
      total_dist += dist;
    }

    // Weighted random selection (sequential)
    let mut r = rng.gen::<f32>() * total_dist;
    let mut selected_idx = 0;

    for (i, dist) in min_dists.iter().enumerate().take(n) {
      r -= *dist;
      if r <= 0.0 {
        selected_idx = i;
        break;
      }
    }

    let selected_offset = selected_idx * dimensions;
    centroids.extend_from_slice(&vectors[selected_offset..selected_offset + dimensions]);
  }

  centroids
}

#[allow(dead_code)]
#[cfg(target_arch = "wasm32")]
fn kmeans_plus_plus_init_parallel(
  vectors: &[f32],
  n: usize,
  dimensions: usize,
  k: usize,
  distance_fn: fn(&[f32], &[f32]) -> f32,
  seed: Option<u64>,
) -> Vec<f32> {
  let mut rng: StdRng = match seed {
    Some(s) => StdRng::seed_from_u64(s),
    None => StdRng::from_entropy(),
  };

  let mut centroids = Vec::with_capacity(k * dimensions);

  let first_idx = rng.gen_range(0..n);
  let first_offset = first_idx * dimensions;
  centroids.extend_from_slice(&vectors[first_offset..first_offset + dimensions]);

  let mut min_dists = vec![f32::INFINITY; n];

  for c in 1..k {
    let prev_cent_offset = (c - 1) * dimensions;
    let prev_centroid = &centroids[prev_cent_offset..prev_cent_offset + dimensions];

    for i in 0..n {
      let vec_offset = i * dimensions;
      let vec = &vectors[vec_offset..vec_offset + dimensions];
      let dist = distance_fn(vec, prev_centroid);
      let abs_dist = dist.abs();
      let candidate = abs_dist * abs_dist;
      if candidate < min_dists[i] {
        min_dists[i] = candidate;
      }
    }

    let mut total_dist = 0.0f32;
    for dist in min_dists.iter().take(n) {
      total_dist += *dist;
    }

    let mut r = rng.gen::<f32>() * total_dist;
    let mut selected_idx = 0;

    for (i, dist) in min_dists.iter().enumerate().take(n) {
      r -= *dist;
      if r <= 0.0 {
        selected_idx = i;
        break;
      }
    }

    let selected_offset = selected_idx * dimensions;
    centroids.extend_from_slice(&vectors[selected_offset..selected_offset + dimensions]);
  }

  centroids
}

// ============================================================================
// Errors
// ============================================================================

#[derive(Debug, Clone)]
pub enum KMeansError {
  NotEnoughVectors { n: usize, k: usize },
  DimensionMismatch { expected: usize, got: usize },
}

impl std::fmt::Display for KMeansError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      KMeansError::NotEnoughVectors { n, k } => {
        write!(f, "Not enough vectors: {n} < {k} clusters")
      }
      KMeansError::DimensionMismatch { expected, got } => {
        write!(f, "Dimension mismatch: expected {expected}, got {got}")
      }
    }
  }
}

impl std::error::Error for KMeansError {}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_kmeans_config_default() {
    let config = KMeansConfig::default();
    assert_eq!(config.n_clusters, 100);
    assert_eq!(config.max_iterations, 25);
  }

  #[test]
  fn test_kmeans_config_builder() {
    let config = KMeansConfig::new(50)
      .with_max_iterations(10)
      .with_tolerance(1e-3)
      .with_seed(42);

    assert_eq!(config.n_clusters, 50);
    assert_eq!(config.max_iterations, 10);
    assert_eq!(config.seed, Some(42));
  }

  #[test]
  fn test_kmeans_simple() {
    // Create 2 clear clusters
    let mut vectors = Vec::new();

    // Cluster 1: around (1, 0, 0)
    for _ in 0..50 {
      vectors.extend_from_slice(&[1.0 + rand::random::<f32>() * 0.1, 0.0, 0.0]);
    }

    // Cluster 2: around (0, 1, 0)
    for _ in 0..50 {
      vectors.extend_from_slice(&[0.0, 1.0 + rand::random::<f32>() * 0.1, 0.0]);
    }

    let config = KMeansConfig::new(2).with_seed(42);
    let result = kmeans(&vectors, 100, 3, &config, squared_euclidean).unwrap();

    assert_eq!(result.centroids.len(), 2 * 3);
    assert_eq!(result.assignments.len(), 100);
    assert!(result.iterations <= config.max_iterations);
  }

  #[test]
  fn test_kmeans_not_enough_vectors() {
    let vectors = vec![1.0, 2.0, 3.0]; // Only 1 vector

    let config = KMeansConfig::new(2);
    let result = kmeans(&vectors, 1, 3, &config, squared_euclidean);

    assert!(matches!(result, Err(KMeansError::NotEnoughVectors { .. })));
  }

  #[test]
  fn test_kmeans_dimension_mismatch() {
    let vectors = vec![1.0, 2.0, 3.0, 4.0]; // 4 elements

    let config = KMeansConfig::new(1);
    let result = kmeans(&vectors, 2, 3, &config, squared_euclidean); // Expects 6 elements

    assert!(matches!(result, Err(KMeansError::DimensionMismatch { .. })));
  }

  #[test]
  fn test_kmeans_convergence() {
    // Simple well-separated clusters
    let mut vectors = Vec::new();

    for _ in 0..100 {
      vectors.extend_from_slice(&[0.0, 0.0]);
    }
    for _ in 0..100 {
      vectors.extend_from_slice(&[10.0, 10.0]);
    }

    let config = KMeansConfig::new(2).with_seed(42).with_tolerance(1e-6);
    let result = kmeans(&vectors, 200, 2, &config, squared_euclidean).unwrap();

    // Should converge quickly with well-separated clusters
    assert!(result.converged || result.iterations <= 10);
  }

  #[test]
  fn test_kmeans_assignments() {
    // Two very distinct clusters
    let vectors = vec![
      0.0, 0.0, // Point near origin
      0.1, 0.1, // Point near origin
      10.0, 10.0, // Point far away
      10.1, 10.1, // Point far away
    ];

    let config = KMeansConfig::new(2).with_seed(42);
    let result = kmeans(&vectors, 4, 2, &config, squared_euclidean).unwrap();

    // Points 0,1 should be in same cluster, points 2,3 in another
    assert_eq!(result.assignments[0], result.assignments[1]);
    assert_eq!(result.assignments[2], result.assignments[3]);
    assert_ne!(result.assignments[0], result.assignments[2]);
  }

  #[test]
  fn test_error_display() {
    let err1 = KMeansError::NotEnoughVectors { n: 5, k: 10 };
    assert!(err1.to_string().contains("5"));
    assert!(err1.to_string().contains("10"));

    let err2 = KMeansError::DimensionMismatch {
      expected: 100,
      got: 50,
    };
    assert!(err2.to_string().contains("100"));
    assert!(err2.to_string().contains("50"));
  }

  // ========================================================================
  // Parallel K-Means Tests
  // ========================================================================

  #[test]
  fn test_kmeans_parallel_fallback_small_dataset() {
    // Small dataset should fall back to sequential
    let vectors = vec![
      0.0, 0.0, // Point near origin
      0.1, 0.1, // Point near origin
      10.0, 10.0, // Point far away
      10.1, 10.1, // Point far away
    ];

    let config = KMeansConfig::new(2).with_seed(42);
    let result = kmeans_parallel(&vectors, 4, 2, &config, squared_euclidean).unwrap();

    // Points 0,1 should be in same cluster, points 2,3 in another
    assert_eq!(result.assignments[0], result.assignments[1]);
    assert_eq!(result.assignments[2], result.assignments[3]);
    assert_ne!(result.assignments[0], result.assignments[2]);
  }

  #[test]
  fn test_kmeans_parallel_large_dataset() {
    // Create a larger dataset to trigger parallel execution
    let n = 5000; // Above MIN_VECTORS_PER_THREAD * 2 threshold
    let dims = 16;
    let k = 10;

    let mut vectors = Vec::with_capacity(n * dims);
    for i in 0..n {
      for d in 0..dims {
        // Create vectors clustered around different points
        let cluster_center = (i % k) as f32 * 10.0;
        vectors.push(cluster_center + (d as f32) * 0.01 + (i as f32) * 0.0001);
      }
    }

    let config = KMeansConfig::new(k).with_seed(42).with_max_iterations(15);
    let result = kmeans_parallel(&vectors, n, dims, &config, squared_euclidean).unwrap();

    assert_eq!(result.centroids.len(), k * dims);
    assert_eq!(result.assignments.len(), n);
    // Verify inertia is reasonable (not infinite or NaN)
    assert!(result.inertia.is_finite());
    assert!(result.inertia >= 0.0);
  }

  #[test]
  fn test_kmeans_parallel_vs_sequential_consistency() {
    // Verify parallel produces consistent results with sequential
    // (not identical due to floating point ordering, but similar quality)
    let n = 3000;
    let dims = 8;
    let k = 5;

    // Create well-separated clusters
    let mut vectors = Vec::with_capacity(n * dims);
    for i in 0..n {
      let cluster = i % k;
      for d in 0..dims {
        vectors.push((cluster * 100 + d) as f32 + rand::random::<f32>() * 0.1);
      }
    }

    let config = KMeansConfig::new(k).with_seed(123).with_max_iterations(20);

    let result_par = kmeans_parallel(&vectors, n, dims, &config, squared_euclidean).unwrap();
    let result_seq = kmeans(&vectors, n, dims, &config, squared_euclidean).unwrap();

    // Both should produce valid results
    assert_eq!(result_par.centroids.len(), result_seq.centroids.len());
    assert_eq!(result_par.assignments.len(), result_seq.assignments.len());

    // Inertia should be similar (within 10% typically)
    let ratio = result_par.inertia / result_seq.inertia;
    assert!(ratio > 0.5 && ratio < 2.0, "Inertia ratio: {}", ratio);
  }

  #[test]
  fn test_kmeans_parallel_well_separated_clusters() {
    // Test with very distinct clusters to verify correctness
    let n = 4000;
    let dims = 4;
    let k = 4;
    let vectors_per_cluster = n / k;

    let mut vectors = Vec::with_capacity(n * dims);
    for cluster in 0..k {
      let center = cluster as f32 * 100.0;
      for _ in 0..vectors_per_cluster {
        for d in 0..dims {
          vectors.push(center + (d as f32) * 0.1 + rand::random::<f32>() * 0.5);
        }
      }
    }

    let config = KMeansConfig::new(k).with_seed(456).with_max_iterations(25);
    let result = kmeans_parallel(&vectors, n, dims, &config, squared_euclidean).unwrap();

    // Count vectors per assignment
    let mut cluster_counts = vec![0usize; k];
    for &assignment in &result.assignments {
      cluster_counts[assignment as usize] += 1;
    }

    // Each cluster should have roughly vectors_per_cluster assignments
    // (allow some variation due to random initialization)
    for count in &cluster_counts {
      assert!(
        *count > vectors_per_cluster / 2,
        "Cluster has too few vectors: {}",
        count
      );
    }
  }

  #[test]
  fn test_kmeans_parallel_convergence() {
    // Test that parallel version converges properly
    let n = 2500;
    let dims = 6;
    let k = 3;

    // Create perfectly separated clusters
    let mut vectors = Vec::with_capacity(n * dims);
    for i in 0..n {
      let cluster = i % k;
      for _ in 0..dims {
        vectors.push(cluster as f32 * 1000.0);
      }
    }

    let config = KMeansConfig::new(k)
      .with_seed(789)
      .with_max_iterations(50)
      .with_tolerance(1e-6);
    let result = kmeans_parallel(&vectors, n, dims, &config, squared_euclidean).unwrap();

    // With perfectly separated clusters, should converge
    assert!(
      result.converged || result.iterations < 20,
      "Should converge quickly with perfect clusters, iterations: {}",
      result.iterations
    );
  }
}
