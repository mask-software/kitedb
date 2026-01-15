//! K-means clustering for IVF index training
//!
//! Implements k-means++ initialization and Lloyd's algorithm.
//!
//! Ported from src/vector/ivf-index.ts (training portion)

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

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
        for i in 0..n {
            let vec_offset = i * dimensions;
            let vec = &vectors[vec_offset..vec_offset + dimensions];
            let dist = distance_fn(vec, prev_centroid);
            // Use abs(dist)^2 for k-means++ (handles negative distances like dot product)
            let abs_dist = dist.abs();
            min_dists[i] = min_dists[i].min(abs_dist * abs_dist);
            total_dist += min_dists[i];
        }

        // Weighted random selection
        let mut r = rng.gen::<f32>() * total_dist;
        let mut selected_idx = 0;

        for i in 0..n {
            r -= min_dists[i];
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

    for i in 0..n {
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

        assignments[i] = best_cluster as u32;
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

    for i in 0..n {
        let cluster = assignments[i] as usize;
        let vec_offset = i * dimensions;
        let sum_offset = cluster * dimensions;

        for d in 0..dimensions {
            cluster_sums[sum_offset + d] += vectors[vec_offset + d];
        }
        cluster_counts[cluster] += 1;
    }

    // Update centroids
    for c in 0..k {
        let count = cluster_counts[c];
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
                write!(f, "Not enough vectors: {} < {} clusters", n, k)
            }
            KMeansError::DimensionMismatch { expected, got } => {
                write!(f, "Dimension mismatch: expected {}, got {}", expected, got)
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
}
