//! Product Quantization (PQ) for vector compression and fast distance computation
//!
//! PQ divides vectors into M subspaces and quantizes each subspace independently
//! using K centroids (typically 256 for uint8 codes).
//!
//! Benefits:
//! - Memory: 384D float32 (1536 bytes) -> 48 bytes (32x compression with M=48)
//! - Speed: Pre-compute distance tables for O(M) distance lookups instead of O(D)
//!
//! Algorithm:
//! 1. Training: Run k-means on each subspace to find subspace centroids
//! 2. Encoding: Assign each subvector to nearest centroid, store code
//! 3. Search: Build distance table (query to all centroids), sum table lookups
//!
//! Ported from src/vector/pq.ts

use crate::vector::types::PqConfig;

// ============================================================================
// PQ Index
// ============================================================================

/// Product Quantization index
#[derive(Debug)]
pub struct PqIndex {
  /// Configuration
  pub config: PqConfig,
  /// Original vector dimensions
  pub dimensions: usize,
  /// Dimensions per subspace (dimensions / num_subspaces)
  pub subspace_dims: usize,
  /// Centroids for each subspace: M arrays of K * subspace_dims floats
  pub centroids: Vec<Vec<f32>>,
  /// Encoded vectors: each vector is M uint8 codes
  pub codes: Option<Vec<u8>>,
  /// Number of encoded vectors
  pub num_vectors: usize,
  /// Whether the index has been trained
  pub trained: bool,
}

impl PqIndex {
  /// Create a new PQ index
  pub fn new(dimensions: usize, config: PqConfig) -> Result<Self, PqError> {
    if dimensions % config.num_subspaces != 0 {
      return Err(PqError::DimensionNotDivisible {
        dimensions,
        num_subspaces: config.num_subspaces,
      });
    }

    let subspace_dims = dimensions / config.num_subspaces;

    // Initialize empty centroids for each subspace
    let centroids: Vec<Vec<f32>> = (0..config.num_subspaces)
      .map(|_| vec![0.0; config.num_centroids * subspace_dims])
      .collect();

    Ok(Self {
      config,
      dimensions,
      subspace_dims,
      centroids,
      codes: None,
      num_vectors: 0,
      trained: false,
    })
  }

  /// Create with default configuration
  pub fn with_defaults(dimensions: usize) -> Result<Self, PqError> {
    Self::new(dimensions, PqConfig::default())
  }

  /// Train the PQ index on a set of vectors
  pub fn train(&mut self, vectors: &[f32], num_vectors: usize) -> Result<(), PqError> {
    if self.trained {
      return Err(PqError::AlreadyTrained);
    }

    let expected_len = num_vectors * self.dimensions;
    if vectors.len() < expected_len {
      return Err(PqError::DimensionMismatch {
        expected: expected_len,
        got: vectors.len(),
      });
    }

    if num_vectors < self.config.num_centroids {
      return Err(PqError::NotEnoughTrainingVectors {
        n: num_vectors,
        k: self.config.num_centroids,
      });
    }

    // Train each subspace independently
    for m in 0..self.config.num_subspaces {
      // Extract subvectors for this subspace
      let mut subvectors = Vec::with_capacity(num_vectors * self.subspace_dims);
      let sub_offset = m * self.subspace_dims;

      for i in 0..num_vectors {
        let vec_offset = i * self.dimensions + sub_offset;
        subvectors.extend_from_slice(&vectors[vec_offset..vec_offset + self.subspace_dims]);
      }

      // Run k-means on subvectors
      train_subspace(
        &mut self.centroids[m],
        &subvectors,
        num_vectors,
        self.subspace_dims,
        self.config.num_centroids,
        self.config.max_iterations,
      );
    }

    self.trained = true;
    Ok(())
  }

  /// Encode vectors into PQ codes
  pub fn encode(&mut self, vectors: &[f32], num_vectors: usize) -> Result<(), PqError> {
    if !self.trained {
      return Err(PqError::NotTrained);
    }

    let expected_len = num_vectors * self.dimensions;
    if vectors.len() < expected_len {
      return Err(PqError::DimensionMismatch {
        expected: expected_len,
        got: vectors.len(),
      });
    }

    // Allocate codes array
    let mut codes = vec![0u8; num_vectors * self.config.num_subspaces];

    // Encode each vector
    for i in 0..num_vectors {
      let vec_offset = i * self.dimensions;
      let code_offset = i * self.config.num_subspaces;

      for m in 0..self.config.num_subspaces {
        let sub_offset = m * self.subspace_dims;
        let subvec =
          &vectors[vec_offset + sub_offset..vec_offset + sub_offset + self.subspace_dims];

        // Find nearest centroid for this subspace
        let code = find_nearest_centroid(
          &self.centroids[m],
          subvec,
          self.subspace_dims,
          self.config.num_centroids,
        );

        codes[code_offset + m] = code;
      }
    }

    self.codes = Some(codes);
    self.num_vectors = num_vectors;

    Ok(())
  }

  /// Encode a single vector and return the codes
  pub fn encode_one(&self, vector: &[f32]) -> Result<Vec<u8>, PqError> {
    if !self.trained {
      return Err(PqError::NotTrained);
    }

    if vector.len() != self.dimensions {
      return Err(PqError::DimensionMismatch {
        expected: self.dimensions,
        got: vector.len(),
      });
    }

    let mut codes = vec![0u8; self.config.num_subspaces];

    for (m, code) in codes.iter_mut().enumerate().take(self.config.num_subspaces) {
      let sub_offset = m * self.subspace_dims;
      let subvec = &vector[sub_offset..sub_offset + self.subspace_dims];

      *code = find_nearest_centroid(
        &self.centroids[m],
        subvec,
        self.subspace_dims,
        self.config.num_centroids,
      );
    }

    Ok(codes)
  }

  /// Build distance table for a query vector
  ///
  /// The table contains squared distances from query subvectors to all centroids.
  /// This allows O(M) distance computation instead of O(D).
  pub fn build_distance_table(&self, query: &[f32]) -> Result<Vec<f32>, PqError> {
    if !self.trained {
      return Err(PqError::NotTrained);
    }

    if query.len() != self.dimensions {
      return Err(PqError::DimensionMismatch {
        expected: self.dimensions,
        got: query.len(),
      });
    }

    let mut table = vec![0.0; self.config.num_subspaces * self.config.num_centroids];

    for m in 0..self.config.num_subspaces {
      let sub_offset = m * self.subspace_dims;
      let table_offset = m * self.config.num_centroids;
      let query_sub = &query[sub_offset..sub_offset + self.subspace_dims];

      for c in 0..self.config.num_centroids {
        let cent_offset = c * self.subspace_dims;
        let centroid = &self.centroids[m][cent_offset..cent_offset + self.subspace_dims];

        let mut dist = 0.0;
        for d in 0..self.subspace_dims {
          let diff = query_sub[d] - centroid[d];
          dist += diff * diff;
        }

        table[table_offset + c] = dist;
      }
    }

    Ok(table)
  }

  /// Compute approximate squared distance using distance table (ADC)
  pub fn distance_adc(&self, table: &[f32], code_offset: usize) -> f32 {
    let codes = match &self.codes {
      Some(c) => c,
      None => return f32::INFINITY,
    };

    let num_subspaces = self.config.num_subspaces;
    let num_centroids = self.config.num_centroids;

    let mut dist = 0.0;

    // Unroll for performance (process 4 subspaces at a time)
    let remainder = num_subspaces % 4;
    let main_len = num_subspaces - remainder;

    for m in (0..main_len).step_by(4) {
      dist += table[m * num_centroids + codes[code_offset + m] as usize]
        + table[(m + 1) * num_centroids + codes[code_offset + m + 1] as usize]
        + table[(m + 2) * num_centroids + codes[code_offset + m + 2] as usize]
        + table[(m + 3) * num_centroids + codes[code_offset + m + 3] as usize];
    }

    for m in main_len..num_subspaces {
      dist += table[m * num_centroids + codes[code_offset + m] as usize];
    }

    dist
  }

  /// Search for k nearest neighbors using ADC
  pub fn search(
    &self,
    query: &[f32],
    k: usize,
    vector_ids: Option<&[usize]>,
  ) -> Result<Vec<PqSearchResult>, PqError> {
    if !self.trained || self.codes.is_none() {
      return Err(PqError::NotTrained);
    }

    let table = self.build_distance_table(query)?;
    let num_subspaces = self.config.num_subspaces;

    let search_indices: Vec<usize> = match vector_ids {
      Some(ids) => ids.to_vec(),
      None => (0..self.num_vectors).collect(),
    };

    // Simple array for top-k (could optimize with heap for large k)
    let mut results: Vec<(usize, f32)> = Vec::new();
    let mut max_dist = f32::INFINITY;

    for &idx in &search_indices {
      let code_offset = idx * num_subspaces;
      let dist = self.distance_adc(&table, code_offset);

      if results.len() < k {
        results.push((idx, dist));
        if results.len() == k {
          results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
          max_dist = results[0].1;
        }
      } else if dist < max_dist {
        results[0] = (idx, dist);
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        max_dist = results[0].1;
      }
    }

    // Sort by distance ascending
    results.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());

    Ok(
      results
        .into_iter()
        .map(|(index, distance)| PqSearchResult { index, distance })
        .collect(),
    )
  }

  /// Get PQ index statistics
  pub fn stats(&self) -> PqStats {
    let code_size_bytes = self.num_vectors * self.config.num_subspaces;
    let centroids_size_bytes =
      self.config.num_subspaces * self.config.num_centroids * self.subspace_dims * 4;
    let original_size_bytes = self.num_vectors * self.dimensions * 4;
    let compression_ratio = if original_size_bytes > 0 {
      original_size_bytes as f32 / (code_size_bytes + centroids_size_bytes) as f32
    } else {
      0.0
    };

    PqStats {
      trained: self.trained,
      dimensions: self.dimensions,
      num_subspaces: self.config.num_subspaces,
      subspace_dims: self.subspace_dims,
      num_centroids: self.config.num_centroids,
      num_vectors: self.num_vectors,
      code_size_bytes,
      centroids_size_bytes,
      compression_ratio,
    }
  }

  /// Clear encoded vectors (keeps trained centroids)
  pub fn clear_codes(&mut self) {
    self.codes = None;
    self.num_vectors = 0;
  }

  /// Reset the entire index
  pub fn reset(&mut self) {
    self.trained = false;
    self.codes = None;
    self.num_vectors = 0;
    for centroids in &mut self.centroids {
      centroids.fill(0.0);
    }
  }
}

// ============================================================================
// Training Helper
// ============================================================================

/// K-means training for a single subspace
fn train_subspace(
  centroids: &mut [f32],
  subvectors: &[f32],
  num_vectors: usize,
  subspace_dims: usize,
  num_centroids: usize,
  max_iterations: usize,
) {
  // Initialize centroids with k-means++
  initialize_centroids_kmeans_pp(
    centroids,
    subvectors,
    num_vectors,
    subspace_dims,
    num_centroids,
  );

  let mut assignments = vec![0u16; num_vectors];
  let mut cluster_sums = vec![0.0f32; num_centroids * subspace_dims];
  let mut cluster_counts = vec![0u32; num_centroids];

  for _ in 0..max_iterations {
    // Assign vectors to nearest centroids
    for (i, assignment) in assignments.iter_mut().enumerate().take(num_vectors) {
      let vec_offset = i * subspace_dims;
      let mut best_centroid = 0;
      let mut best_dist = f32::INFINITY;

      for c in 0..num_centroids {
        let cent_offset = c * subspace_dims;
        let mut dist = 0.0;
        for d in 0..subspace_dims {
          let diff = subvectors[vec_offset + d] - centroids[cent_offset + d];
          dist += diff * diff;
        }
        if dist < best_dist {
          best_dist = dist;
          best_centroid = c;
        }
      }
      *assignment = best_centroid as u16;
    }

    // Update centroids
    cluster_sums.fill(0.0);
    cluster_counts.fill(0);

    for (i, &cluster_id) in assignments.iter().enumerate().take(num_vectors) {
      let cluster = cluster_id as usize;
      let vec_offset = i * subspace_dims;
      let sum_offset = cluster * subspace_dims;

      for d in 0..subspace_dims {
        cluster_sums[sum_offset + d] += subvectors[vec_offset + d];
      }
      cluster_counts[cluster] += 1;
    }

    for (c, &count) in cluster_counts.iter().enumerate() {
      if count == 0 {
        continue;
      }

      let offset = c * subspace_dims;
      for d in 0..subspace_dims {
        centroids[offset + d] = cluster_sums[offset + d] / count as f32;
      }
    }
  }
}

/// K-means++ initialization
fn initialize_centroids_kmeans_pp(
  centroids: &mut [f32],
  vectors: &[f32],
  num_vectors: usize,
  dims: usize,
  k: usize,
) {
  use rand::Rng;
  let mut rng = rand::thread_rng();

  // First centroid: random vector
  let first_idx = rng.gen_range(0..num_vectors);
  for d in 0..dims {
    centroids[d] = vectors[first_idx * dims + d];
  }

  let mut min_dists = vec![f32::INFINITY; num_vectors];

  for c in 1..k {
    // Update min distances
    let prev_cent_offset = (c - 1) * dims;
    let mut total_dist = 0.0;

    for (i, min_dist) in min_dists.iter_mut().enumerate() {
      let vec_offset = i * dims;
      let mut dist = 0.0;
      for d in 0..dims {
        let diff = vectors[vec_offset + d] - centroids[prev_cent_offset + d];
        dist += diff * diff;
      }
      *min_dist = (*min_dist).min(dist);
      total_dist += *min_dist;
    }

    // Weighted random selection
    let mut r = rng.gen::<f32>() * total_dist;
    let mut selected_idx = 0;
    for (i, dist) in min_dists.iter().enumerate() {
      r -= *dist;
      if r <= 0.0 {
        selected_idx = i;
        break;
      }
    }

    // Copy selected vector to centroid
    let cent_offset = c * dims;
    for d in 0..dims {
      centroids[cent_offset + d] = vectors[selected_idx * dims + d];
    }
  }
}

/// Find nearest centroid for a subvector
fn find_nearest_centroid(
  centroids: &[f32],
  subvec: &[f32],
  subspace_dims: usize,
  num_centroids: usize,
) -> u8 {
  let mut best_centroid = 0;
  let mut best_dist = f32::INFINITY;

  for c in 0..num_centroids {
    let cent_offset = c * subspace_dims;
    let mut dist = 0.0;

    for d in 0..subspace_dims {
      let diff = subvec[d] - centroids[cent_offset + d];
      dist += diff * diff;
    }

    if dist < best_dist {
      best_dist = dist;
      best_centroid = c;
    }
  }

  best_centroid as u8
}

// ============================================================================
// Results and Statistics
// ============================================================================

/// PQ search result
#[derive(Debug, Clone)]
pub struct PqSearchResult {
  pub index: usize,
  pub distance: f32,
}

/// PQ index statistics
#[derive(Debug, Clone)]
pub struct PqStats {
  pub trained: bool,
  pub dimensions: usize,
  pub num_subspaces: usize,
  pub subspace_dims: usize,
  pub num_centroids: usize,
  pub num_vectors: usize,
  pub code_size_bytes: usize,
  pub centroids_size_bytes: usize,
  pub compression_ratio: f32,
}

// ============================================================================
// Errors
// ============================================================================

#[derive(Debug, Clone)]
pub enum PqError {
  DimensionNotDivisible {
    dimensions: usize,
    num_subspaces: usize,
  },
  DimensionMismatch {
    expected: usize,
    got: usize,
  },
  AlreadyTrained,
  NotTrained,
  NotEnoughTrainingVectors {
    n: usize,
    k: usize,
  },
}

impl std::fmt::Display for PqError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      PqError::DimensionNotDivisible {
        dimensions,
        num_subspaces,
      } => write!(
        f,
        "Dimensions ({dimensions}) must be divisible by num_subspaces ({num_subspaces})"
      ),
      PqError::DimensionMismatch { expected, got } => {
        write!(f, "Dimension mismatch: expected {expected}, got {got}")
      }
      PqError::AlreadyTrained => write!(f, "Index already trained"),
      PqError::NotTrained => write!(f, "Index must be trained before use"),
      PqError::NotEnoughTrainingVectors { n, k } => {
        write!(f, "Need at least {k} training vectors, got {n}")
      }
    }
  }
}

impl std::error::Error for PqError {}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;

  fn test_config() -> PqConfig {
    PqConfig {
      num_subspaces: 4,
      num_centroids: 8,
      max_iterations: 10,
    }
  }

  #[test]
  fn test_pq_new() {
    let index = PqIndex::new(16, test_config()).unwrap();
    assert_eq!(index.dimensions, 16);
    assert_eq!(index.subspace_dims, 4);
    assert!(!index.trained);
  }

  #[test]
  fn test_pq_new_not_divisible() {
    let result = PqIndex::new(15, test_config());
    assert!(matches!(result, Err(PqError::DimensionNotDivisible { .. })));
  }

  #[test]
  fn test_pq_train() {
    let mut index = PqIndex::new(16, test_config()).unwrap();

    // Create training vectors
    let mut vectors = Vec::new();
    for i in 0..100 {
      for d in 0..16 {
        vectors.push((i * 16 + d) as f32 / 1600.0);
      }
    }

    index.train(&vectors, 100).unwrap();
    assert!(index.trained);
  }

  #[test]
  fn test_pq_train_not_enough_vectors() {
    let mut index = PqIndex::new(16, test_config()).unwrap();

    let vectors = vec![0.0; 5 * 16]; // Only 5 vectors, need 8
    let result = index.train(&vectors, 5);

    assert!(matches!(
      result,
      Err(PqError::NotEnoughTrainingVectors { .. })
    ));
  }

  #[test]
  fn test_pq_encode() {
    let mut index = PqIndex::new(16, test_config()).unwrap();

    // Train
    let mut vectors = Vec::new();
    for i in 0..100 {
      for d in 0..16 {
        vectors.push((i * 16 + d) as f32 / 1600.0);
      }
    }
    index.train(&vectors, 100).unwrap();

    // Encode
    index.encode(&vectors, 100).unwrap();

    assert!(index.codes.is_some());
    assert_eq!(index.num_vectors, 100);
    assert_eq!(index.codes.as_ref().unwrap().len(), 100 * 4);
  }

  #[test]
  fn test_pq_encode_one() {
    let mut index = PqIndex::new(16, test_config()).unwrap();

    // Train
    let mut vectors = Vec::new();
    for i in 0..100 {
      for d in 0..16 {
        vectors.push((i * 16 + d) as f32 / 1600.0);
      }
    }
    index.train(&vectors, 100).unwrap();

    // Encode single vector
    let vector = vec![0.5; 16];
    let codes = index.encode_one(&vector).unwrap();

    assert_eq!(codes.len(), 4);
  }

  #[test]
  fn test_pq_distance_table() {
    let mut index = PqIndex::new(16, test_config()).unwrap();

    // Train
    let mut vectors = Vec::new();
    for i in 0..100 {
      for d in 0..16 {
        vectors.push((i * 16 + d) as f32 / 1600.0);
      }
    }
    index.train(&vectors, 100).unwrap();

    // Build distance table
    let query = vec![0.5; 16];
    let table = index.build_distance_table(&query).unwrap();

    assert_eq!(table.len(), 4 * 8); // num_subspaces * num_centroids
  }

  #[test]
  fn test_pq_search() {
    let mut index = PqIndex::new(16, test_config()).unwrap();

    // Train
    let mut vectors = Vec::new();
    for i in 0..100 {
      for d in 0..16 {
        vectors.push((i * 16 + d) as f32 / 1600.0);
      }
    }
    index.train(&vectors, 100).unwrap();
    index.encode(&vectors, 100).unwrap();

    // Search
    let query = vec![0.5; 16];
    let results = index.search(&query, 5, None).unwrap();

    assert_eq!(results.len(), 5);
    // Results should be sorted by distance
    for i in 1..results.len() {
      assert!(results[i - 1].distance <= results[i].distance);
    }
  }

  #[test]
  fn test_pq_stats() {
    let mut index = PqIndex::new(16, test_config()).unwrap();

    // Train
    let mut vectors = Vec::new();
    for i in 0..100 {
      for d in 0..16 {
        vectors.push((i * 16 + d) as f32 / 1600.0);
      }
    }
    index.train(&vectors, 100).unwrap();
    index.encode(&vectors, 100).unwrap();

    let stats = index.stats();
    assert!(stats.trained);
    assert_eq!(stats.dimensions, 16);
    assert_eq!(stats.num_subspaces, 4);
    assert_eq!(stats.num_vectors, 100);
    assert!(stats.compression_ratio > 0.0);
  }

  #[test]
  fn test_pq_clear_codes() {
    let mut index = PqIndex::new(16, test_config()).unwrap();

    let mut vectors = Vec::new();
    for i in 0..100 {
      for d in 0..16 {
        vectors.push((i * 16 + d) as f32 / 1600.0);
      }
    }
    index.train(&vectors, 100).unwrap();
    index.encode(&vectors, 100).unwrap();

    index.clear_codes();

    assert!(index.trained); // Still trained
    assert!(index.codes.is_none());
    assert_eq!(index.num_vectors, 0);
  }

  #[test]
  fn test_pq_reset() {
    let mut index = PqIndex::new(16, test_config()).unwrap();

    let mut vectors = Vec::new();
    for i in 0..100 {
      for d in 0..16 {
        vectors.push((i * 16 + d) as f32 / 1600.0);
      }
    }
    index.train(&vectors, 100).unwrap();
    index.encode(&vectors, 100).unwrap();

    index.reset();

    assert!(!index.trained);
    assert!(index.codes.is_none());
  }

  #[test]
  fn test_error_display() {
    let err1 = PqError::DimensionNotDivisible {
      dimensions: 15,
      num_subspaces: 4,
    };
    assert!(err1.to_string().contains("15"));
    assert!(err1.to_string().contains("4"));

    let err2 = PqError::AlreadyTrained;
    assert!(err2.to_string().contains("already"));

    let err3 = PqError::NotTrained;
    assert!(err3.to_string().contains("trained"));
  }
}
