//! K-means clustering for IVF index training
//!
//! Implements k-means++ initialization and Lloyd's algorithm. The parallel
//! entry point uses rayon for the initialization passes, the assignment step
//! and the centroid update.
//!
//! Ported from src/vector/ivf-index.ts (training portion)

use std::borrow::Cow;

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
  /// Final inertia (sum of distances to the assigned centroids)
  pub inertia: f32,
  /// Number of iterations performed
  pub iterations: usize,
  /// Whether converged (inertia change < tolerance)
  pub converged: bool,
}

// ============================================================================
// Training Input
// ============================================================================

/// k-means quality stops improving at around this many training points per
/// cluster (FAISS uses the same cap); more points only cost build time.
pub(crate) const MAX_TRAINING_POINTS_PER_CLUSTER: usize = 256;

const TRAINING_SAMPLE_SEED: u64 = 0x6b6d_6561_6e73_5f31;

/// At most `max_points` of the `n` vectors in `vectors`, drawn without
/// replacement and kept in their original order. The seed is fixed, so a
/// rebuild over the same data trains on the same sample. Borrows `vectors`
/// when no sampling is needed.
pub(crate) fn training_sample(
  vectors: &[f32],
  n: usize,
  dimensions: usize,
  max_points: usize,
) -> (Cow<'_, [f32]>, usize) {
  if n <= max_points {
    return (Cow::Borrowed(&vectors[..n * dimensions]), n);
  }
  let mut rng = StdRng::seed_from_u64(TRAINING_SAMPLE_SEED ^ n as u64);
  let mut picked = rand::seq::index::sample(&mut rng, n, max_points).into_vec();
  picked.sort_unstable();
  let mut sample = Vec::with_capacity(max_points * dimensions);
  for index in picked {
    let offset = index * dimensions;
    sample.extend_from_slice(&vectors[offset..offset + dimensions]);
  }
  (Cow::Owned(sample), max_points)
}

// ============================================================================
// K-Means Algorithm
// ============================================================================

/// Minimum vectors per thread for parallelization to be beneficial
const MIN_VECTORS_PER_THREAD: usize = 1000;

/// Run k-means clustering on vectors
///
/// # Arguments
/// * `vectors` - Contiguous vector data (n * dimensions)
/// * `n` - Number of vectors
/// * `dimensions` - Number of dimensions per vector
/// * `config` - K-means configuration
/// * `distance_fn` - Distance function to use. Passing a function item (for
///   example `squared_euclidean`) rather than a `fn` pointer lets the hot
///   loops inline it.
///
/// # Returns
/// K-means result with centroids and assignments
///
/// # Errors
/// Returns an error if `n_clusters` or `dimensions` is zero, if there are
/// fewer vectors than clusters, or if `vectors` is not `n * dimensions` long.
pub fn kmeans<D>(
  vectors: &[f32],
  n: usize,
  dimensions: usize,
  config: &KMeansConfig,
  distance_fn: D,
) -> Result<KMeansResult, KMeansError>
where
  D: Fn(&[f32], &[f32]) -> f32 + Sync,
{
  run_kmeans(vectors, n, dimensions, config, &distance_fn, false)
}

/// Run parallel k-means clustering on vectors
///
/// Uses rayon for the k-means++ passes, the assignment step and the centroid
/// update. Runs sequentially for small datasets, where parallelization
/// overhead would outweigh the benefits. Arguments, result and errors are as
/// for [`kmeans`].
pub fn kmeans_parallel<D>(
  vectors: &[f32],
  n: usize,
  dimensions: usize,
  config: &KMeansConfig,
  distance_fn: D,
) -> Result<KMeansResult, KMeansError>
where
  D: Fn(&[f32], &[f32]) -> f32 + Sync,
{
  let parallel = n >= MIN_VECTORS_PER_THREAD * 2;
  run_kmeans(vectors, n, dimensions, config, &distance_fn, parallel)
}

fn run_kmeans<D>(
  vectors: &[f32],
  n: usize,
  dimensions: usize,
  config: &KMeansConfig,
  distance_fn: &D,
  parallel: bool,
) -> Result<KMeansResult, KMeansError>
where
  D: Fn(&[f32], &[f32]) -> f32 + Sync,
{
  if config.n_clusters == 0 {
    return Err(KMeansError::InvalidInput(
      "n_clusters must be nonzero".into(),
    ));
  }
  if dimensions == 0 {
    return Err(KMeansError::InvalidInput(
      "dimensions must be nonzero".into(),
    ));
  }
  if n < config.n_clusters {
    return Err(KMeansError::NotEnoughVectors {
      n,
      k: config.n_clusters,
    });
  }
  let expected = n.checked_mul(dimensions);
  if expected != Some(vectors.len()) {
    return Err(KMeansError::DimensionMismatch {
      expected: expected.unwrap_or(usize::MAX),
      got: vectors.len(),
    });
  }

  let k = config.n_clusters;
  let mut centroids = kmeans_plus_plus_init(
    vectors,
    n,
    dimensions,
    k,
    distance_fn,
    config.seed,
    parallel,
  );

  // Lloyd's algorithm
  let mut assignments = vec![0u32; n];
  let mut prev_inertia = f32::INFINITY;
  let mut iterations = 0;
  let mut converged = false;

  for iter in 0..config.max_iterations {
    iterations = iter + 1;

    let inertia = assign_to_centroids(
      vectors,
      dimensions,
      &centroids,
      &mut assignments,
      distance_fn,
      parallel,
    );

    let inertia_change = (prev_inertia - inertia).abs() / inertia.max(1.0);
    if inertia_change < config.tolerance {
      converged = true;
      break;
    }
    prev_inertia = inertia;

    update_centroids(
      vectors,
      dimensions,
      &assignments,
      k,
      &mut centroids,
      parallel,
    );
  }

  // Final assignment pass
  let inertia = assign_to_centroids(
    vectors,
    dimensions,
    &centroids,
    &mut assignments,
    distance_fn,
    parallel,
  );

  Ok(KMeansResult {
    centroids,
    assignments,
    inertia,
    iterations,
    converged,
  })
}

/// Index and distance of the centroid nearest to `vector`. Ties keep the
/// lowest index; NaN distances never win.
#[inline]
pub(crate) fn nearest_centroid<D>(
  vector: &[f32],
  centroids: &[f32],
  dimensions: usize,
  distance_fn: &D,
) -> (usize, f32)
where
  D: Fn(&[f32], &[f32]) -> f32,
{
  let mut best_cluster = 0;
  let mut best_dist = f32::INFINITY;
  for (cluster, centroid) in centroids.chunks_exact(dimensions).enumerate() {
    let dist = distance_fn(vector, centroid);
    if dist < best_dist {
      best_dist = dist;
      best_cluster = cluster;
    }
  }
  (best_cluster, best_dist)
}

/// K-means++ initialization: each next centroid is a vector drawn with
/// probability proportional to its squared distance to the nearest centroid
/// so far. The distance passes run in parallel when `parallel` is set; the
/// draw stays sequential, so a seeded run picks the same centroids either
/// way.
fn kmeans_plus_plus_init<D>(
  vectors: &[f32],
  n: usize,
  dimensions: usize,
  k: usize,
  distance_fn: &D,
  seed: Option<u64>,
  parallel: bool,
) -> Vec<f32>
where
  D: Fn(&[f32], &[f32]) -> f32 + Sync,
{
  let mut rng: StdRng = match seed {
    Some(s) => StdRng::seed_from_u64(s),
    None => StdRng::from_entropy(),
  };

  let mut centroids = Vec::with_capacity(k * dimensions);
  let first_offset = rng.gen_range(0..n) * dimensions;
  centroids.extend_from_slice(&vectors[first_offset..first_offset + dimensions]);

  let mut min_dists = vec![f32::INFINITY; n];
  for c in 1..k {
    let latest = &centroids[(c - 1) * dimensions..c * dimensions];
    update_min_distances(
      vectors,
      dimensions,
      latest,
      &mut min_dists,
      distance_fn,
      parallel,
    );
    let selected_offset = weighted_pick(&min_dists, &mut rng) * dimensions;
    centroids.extend_from_slice(&vectors[selected_offset..selected_offset + dimensions]);
  }

  centroids
}

/// Lowers each `min_dists[i]` to the squared distance from vector `i` to
/// `centroid`. Uses `|distance|^2`, so negative distances (dot product) work.
fn update_min_distances<D>(
  vectors: &[f32],
  dimensions: usize,
  centroid: &[f32],
  min_dists: &mut [f32],
  distance_fn: &D,
  parallel: bool,
) where
  D: Fn(&[f32], &[f32]) -> f32 + Sync,
{
  let update = |(vector, min_dist): (&[f32], &mut f32)| {
    let dist = distance_fn(vector, centroid).abs();
    let squared = dist * dist;
    if squared < *min_dist {
      *min_dist = squared;
    }
  };
  #[cfg(not(target_arch = "wasm32"))]
  if parallel {
    vectors
      .par_chunks_exact(dimensions)
      .zip(min_dists.par_iter_mut())
      .with_min_len(MIN_VECTORS_PER_THREAD)
      .for_each(update);
    return;
  }
  let _ = parallel;
  vectors
    .chunks_exact(dimensions)
    .zip(min_dists.iter_mut())
    .for_each(update);
}

/// Index drawn with probability proportional to `weights[i]`. Falls back to
/// a uniform draw when the weights carry no information (all zero, or not
/// finite).
fn weighted_pick(weights: &[f32], rng: &mut StdRng) -> usize {
  let total: f64 = weights.iter().map(|&w| f64::from(w)).sum();
  if !total.is_finite() || total <= 0.0 {
    return rng.gen_range(0..weights.len());
  }
  let mut r = rng.gen::<f64>() * total;
  for (i, &weight) in weights.iter().enumerate() {
    r -= f64::from(weight);
    if r <= 0.0 {
      return i;
    }
  }
  // Rounding left a sliver of `r`: take the last vector that has weight.
  weights.iter().rposition(|&w| w > 0.0).unwrap_or(0)
}

/// Vectors per partial inertia sum. Fixed, so the sum adds the same partials
/// in the same order however many threads computed them.
const INERTIA_CHUNK: usize = MIN_VECTORS_PER_THREAD / 4;

/// Assigns every vector to its nearest centroid and returns the inertia (the
/// sum of the distances to the assigned centroids).
///
/// The inertia adds per-chunk partial sums in chunk order, so it is the same
/// with or without `parallel` and on any thread count.
pub(crate) fn assign_to_centroids<D>(
  vectors: &[f32],
  dimensions: usize,
  centroids: &[f32],
  assignments: &mut [u32],
  distance_fn: &D,
  parallel: bool,
) -> f32
where
  D: Fn(&[f32], &[f32]) -> f32 + Sync,
{
  let assign_chunk = |(chunk, chunk_assignments): (&[f32], &mut [u32])| {
    chunk
      .chunks_exact(dimensions)
      .zip(chunk_assignments.iter_mut())
      .map(|(vector, assignment)| {
        let (cluster, dist) = nearest_centroid(vector, centroids, dimensions, distance_fn);
        *assignment = cluster as u32;
        f64::from(dist)
      })
      .sum::<f64>()
  };
  let chunk_len = INERTIA_CHUNK * dimensions;
  #[cfg(not(target_arch = "wasm32"))]
  if parallel {
    let partials: Vec<f64> = vectors
      .par_chunks(chunk_len)
      .zip(assignments.par_chunks_mut(INERTIA_CHUNK))
      .map(assign_chunk)
      .collect();
    return partials.iter().sum::<f64>() as f32;
  }
  let _ = parallel;
  vectors
    .chunks(chunk_len)
    .zip(assignments.chunks_mut(INERTIA_CHUNK))
    .map(assign_chunk)
    .sum::<f64>() as f32
}

/// Moves each centroid to the mean of its assigned vectors. A centroid with
/// no vectors keeps its position.
///
/// Each centroid adds its vectors in index order (in parallel across
/// centroids when `parallel` is set), so the result is the same with or
/// without `parallel` and on any thread count.
fn update_centroids(
  vectors: &[f32],
  dimensions: usize,
  assignments: &[u32],
  k: usize,
  centroids: &mut [f32],
  parallel: bool,
) {
  // Vector indices grouped by cluster, each group in index order (a
  // counting sort): cluster c's vectors are members[starts[c]..starts[c + 1]].
  let mut starts = vec![0usize; k + 1];
  for &cluster in assignments {
    starts[cluster as usize + 1] += 1;
  }
  for c in 0..k {
    starts[c + 1] += starts[c];
  }
  let mut next = starts[..k].to_vec();
  let mut members = vec![0usize; assignments.len()];
  for (index, &cluster) in assignments.iter().enumerate() {
    let slot = &mut next[cluster as usize];
    members[*slot] = index;
    *slot += 1;
  }

  let update = |(c, centroid): (usize, &mut [f32])| {
    let group = &members[starts[c]..starts[c + 1]];
    if group.is_empty() {
      return;
    }
    centroid.fill(0.0);
    for &index in group {
      let vector = &vectors[index * dimensions..(index + 1) * dimensions];
      for (sum, &x) in centroid.iter_mut().zip(vector) {
        *sum += x;
      }
    }
    let count = group.len() as f32;
    for sum in centroid.iter_mut() {
      *sum /= count;
    }
  };
  #[cfg(not(target_arch = "wasm32"))]
  if parallel {
    centroids
      .par_chunks_mut(dimensions)
      .enumerate()
      .for_each(update);
    return;
  }
  let _ = parallel;
  centroids
    .chunks_mut(dimensions)
    .enumerate()
    .for_each(update);
}

// ============================================================================
// Errors
// ============================================================================

#[derive(Debug, Clone)]
pub enum KMeansError {
  NotEnoughVectors {
    n: usize,
    k: usize,
  },
  DimensionMismatch {
    expected: usize,
    got: usize,
  },
  /// A zero cluster count or zero dimensions.
  InvalidInput(String),
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
      KMeansError::InvalidInput(msg) => write!(f, "Invalid k-means input: {msg}"),
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
    let result = kmeans(&vectors, 100, 3, &config, squared_euclidean).expect("expected value");

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
    let result = kmeans(&vectors, 200, 2, &config, squared_euclidean).expect("expected value");

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
    let result = kmeans(&vectors, 4, 2, &config, squared_euclidean).expect("expected value");

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

  #[test]
  fn test_kmeans_rejects_zero_dimensions() {
    let config = KMeansConfig::new(1);
    assert!(matches!(
      kmeans(&[], 3, 0, &config, squared_euclidean),
      Err(KMeansError::InvalidInput(_))
    ));
  }

  #[test]
  fn test_training_sample_borrows_small_inputs_and_samples_large_ones() {
    let vectors: Vec<f32> = (0..40).map(|i| i as f32).collect(); // 20 x 2
    let (all, n) = training_sample(&vectors, 20, 2, 32);
    assert!(matches!(all, Cow::Borrowed(_)));
    assert_eq!((all.len(), n), (40, 20));

    let (sample, n) = training_sample(&vectors, 20, 2, 8);
    assert_eq!((sample.len(), n), (16, 8));
    // Whole vectors, in their original order, without repeats.
    let firsts: Vec<f32> = sample.as_chunks::<2>().0.iter().map(|v| v[0]).collect();
    assert!(sample.as_chunks::<2>().0.iter().all(|v| v[1] == v[0] + 1.0));
    assert!(firsts.windows(2).all(|w| w[0] < w[1]));
    // The seed is fixed: the same input gives the same sample.
    assert_eq!(training_sample(&vectors, 20, 2, 8).0, sample);
  }

  #[test]
  fn test_weighted_pick_never_picks_zero_weight() {
    let mut rng = StdRng::seed_from_u64(9);
    let weights = [0.0, 3.0, 0.0, 1.0, 0.0];
    for _ in 0..200 {
      let picked = weighted_pick(&weights, &mut rng);
      assert!(picked == 1 || picked == 3, "picked {picked}");
    }
    // No information: any index, never out of range.
    assert!(weighted_pick(&[0.0; 4], &mut rng) < 4);
    assert!(weighted_pick(&[f32::INFINITY, 1.0], &mut rng) < 2);
  }

  #[test]
  fn test_seeded_init_is_identical_sequential_and_parallel() {
    let n = 3000;
    let dims = 4;
    let vectors: Vec<f32> = (0..n * dims).map(|i| ((i * 7919) % 1000) as f32).collect();
    let sequential =
      kmeans_plus_plus_init(&vectors, n, dims, 8, &squared_euclidean, Some(5), false);
    let parallel = kmeans_plus_plus_init(&vectors, n, dims, 8, &squared_euclidean, Some(5), true);
    assert_eq!(sequential, parallel);
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
    let result =
      kmeans_parallel(&vectors, 4, 2, &config, squared_euclidean).expect("expected value");

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
    let result =
      kmeans_parallel(&vectors, n, dims, &config, squared_euclidean).expect("expected value");

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

    let result_par =
      kmeans_parallel(&vectors, n, dims, &config, squared_euclidean).expect("expected value");
    let result_seq = kmeans(&vectors, n, dims, &config, squared_euclidean).expect("expected value");

    // Both should produce valid results
    assert_eq!(result_par.centroids.len(), result_seq.centroids.len());
    assert_eq!(result_par.assignments.len(), result_seq.assignments.len());

    // Inertia should be similar (within 10% typically)
    let ratio = result_par.inertia / result_seq.inertia;
    assert!(ratio > 0.5 && ratio < 2.0, "Inertia ratio: {ratio}");
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
    let result =
      kmeans_parallel(&vectors, n, dims, &config, squared_euclidean).expect("expected value");

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
        "Cluster has too few vectors: {count}"
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
    let result =
      kmeans_parallel(&vectors, n, dims, &config, squared_euclidean).expect("expected value");

    // With perfectly separated clusters, should converge
    assert!(
      result.converged || result.iterations < 20,
      "Should converge quickly with perfect clusters, iterations: {}",
      result.iterations
    );
  }
}
