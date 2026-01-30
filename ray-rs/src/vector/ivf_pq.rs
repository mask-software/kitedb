//! IVF-PQ: Combined Inverted File Index with Product Quantization
//!
//! This combines IVF (for coarse clustering) with PQ (for fast distance computation).
//! It's the standard approach used by FAISS and other high-performance vector DBs.
//!
//! Architecture:
//! 1. IVF partitions vectors into clusters using coarse centroids
//! 2. PQ compresses residuals (vector - centroid) for each cluster
//! 3. Search: find nearest clusters, then use ADC on PQ codes
//!
//! This provides:
//! - Fast coarse search (IVF centroid comparison)
//! - Fast fine search (PQ table lookups instead of full distance)
//! - Memory efficiency (PQ codes instead of full vectors)
//!
//! Ported from src/vector/ivf-pq.ts

use std::collections::HashMap;

#[cfg(not(target_arch = "wasm32"))]
use rayon::prelude::*;

use crate::types::NodeId;
use crate::vector::distance::normalize;
use crate::vector::ivf::{kmeans_parallel, KMeansConfig};
use crate::vector::types::{
  DistanceMetric, IvfConfig, MultiQueryAggregation, PqConfig, VectorManifest, VectorSearchResult,
};

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for IVF-PQ combined index
#[derive(Debug, Clone)]
pub struct IvfPqConfig {
  /// IVF configuration (clustering)
  pub ivf: IvfConfig,
  /// PQ configuration (compression)
  pub pq: PqConfig,
  /// Whether to use residual encoding (recommended for better accuracy)
  pub use_residuals: bool,
}

impl Default for IvfPqConfig {
  fn default() -> Self {
    Self {
      ivf: IvfConfig::default(),
      pq: PqConfig::default(),
      use_residuals: true,
    }
  }
}

impl IvfPqConfig {
  /// Create a new config with default settings
  pub fn new() -> Self {
    Self::default()
  }

  /// Set the number of clusters
  pub fn with_n_clusters(mut self, n_clusters: usize) -> Self {
    self.ivf.n_clusters = n_clusters;
    self
  }

  /// Set the number of clusters to probe during search
  pub fn with_n_probe(mut self, n_probe: usize) -> Self {
    self.ivf.n_probe = n_probe;
    self
  }

  /// Set the distance metric
  pub fn with_metric(mut self, metric: DistanceMetric) -> Self {
    self.ivf.metric = metric;
    self
  }

  /// Set the number of PQ subspaces
  pub fn with_num_subspaces(mut self, num_subspaces: usize) -> Self {
    self.pq.num_subspaces = num_subspaces;
    self
  }

  /// Set the number of PQ centroids
  pub fn with_num_centroids(mut self, num_centroids: usize) -> Self {
    self.pq.num_centroids = num_centroids;
    self
  }

  /// Set whether to use residual encoding
  pub fn with_residuals(mut self, use_residuals: bool) -> Self {
    self.use_residuals = use_residuals;
    self
  }
}

// ============================================================================
// IVF-PQ Index
// ============================================================================

/// IVF-PQ combined index for approximate nearest neighbor search
#[derive(Debug)]
pub struct IvfPqIndex {
  /// Configuration
  pub config: IvfPqConfig,
  /// IVF centroids: n_clusters * dimensions
  pub ivf_centroids: Vec<f32>,
  /// Inverted lists: cluster -> vector IDs
  pub inverted_lists: HashMap<usize, Vec<u64>>,
  /// PQ codes for each vector: vectorId -> codes (M bytes)
  pub pq_codes: HashMap<u64, Vec<u8>>,
  /// PQ centroids for each subspace: M arrays of K * subspace_dims floats
  pub pq_centroids: Vec<Vec<f32>>,
  /// Pre-computed centroid-to-centroid distances (optional)
  pub centroid_distances: Option<Vec<f32>>,
  /// Number of dimensions
  pub dimensions: usize,
  /// Dimensions per PQ subspace
  pub subspace_dims: usize,
  /// Whether the index has been trained
  pub trained: bool,
  /// Training vectors buffer
  training_vectors: Option<Vec<f32>>,
  /// Number of training vectors
  training_count: usize,
}

impl IvfPqIndex {
  /// Create a new IVF-PQ index
  pub fn new(dimensions: usize, config: IvfPqConfig) -> Result<Self, IvfPqError> {
    if dimensions % config.pq.num_subspaces != 0 {
      return Err(IvfPqError::DimensionNotDivisible {
        dimensions,
        num_subspaces: config.pq.num_subspaces,
      });
    }

    let subspace_dims = dimensions / config.pq.num_subspaces;

    // Initialize empty PQ centroids for each subspace
    let pq_centroids: Vec<Vec<f32>> = (0..config.pq.num_subspaces)
      .map(|_| vec![0.0; config.pq.num_centroids * subspace_dims])
      .collect();

    Ok(Self {
      config,
      ivf_centroids: Vec::new(),
      inverted_lists: HashMap::new(),
      pq_codes: HashMap::new(),
      pq_centroids,
      centroid_distances: None,
      dimensions,
      subspace_dims,
      trained: false,
      training_vectors: Some(Vec::new()),
      training_count: 0,
    })
  }

  /// Create a new IVF-PQ index with default configuration
  pub fn with_defaults(dimensions: usize) -> Result<Self, IvfPqError> {
    Self::new(dimensions, IvfPqConfig::default())
  }

  /// Create from serialized data (for deserialization)
  #[allow(clippy::too_many_arguments)]
  pub fn from_serialized(
    config: IvfPqConfig,
    ivf_centroids: Vec<f32>,
    inverted_lists: HashMap<usize, Vec<u64>>,
    pq_codes: HashMap<u64, Vec<u8>>,
    pq_centroids: Vec<Vec<f32>>,
    centroid_distances: Option<Vec<f32>>,
    dimensions: usize,
    trained: bool,
  ) -> Result<Self, IvfPqError> {
    if dimensions % config.pq.num_subspaces != 0 {
      return Err(IvfPqError::DimensionNotDivisible {
        dimensions,
        num_subspaces: config.pq.num_subspaces,
      });
    }

    let subspace_dims = dimensions / config.pq.num_subspaces;

    Ok(Self {
      config,
      ivf_centroids,
      inverted_lists,
      pq_codes,
      pq_centroids,
      centroid_distances,
      dimensions,
      subspace_dims,
      trained,
      training_vectors: None,
      training_count: 0,
    })
  }

  /// Add vectors for training
  pub fn add_training_vectors(&mut self, vectors: &[f32], count: usize) -> Result<(), IvfPqError> {
    if self.trained {
      return Err(IvfPqError::AlreadyTrained);
    }

    let expected_len = count * self.dimensions;
    if vectors.len() < expected_len {
      return Err(IvfPqError::DimensionMismatch {
        expected: expected_len,
        got: vectors.len(),
      });
    }

    let training_buf = self.training_vectors.get_or_insert_with(Vec::new);
    training_buf.extend_from_slice(&vectors[..expected_len]);
    self.training_count += count;

    Ok(())
  }

  /// Train the IVF-PQ index
  pub fn train(&mut self) -> Result<(), IvfPqError> {
    if self.trained {
      return Ok(());
    }

    let training_vectors = self
      .training_vectors
      .take()
      .ok_or(IvfPqError::NoTrainingVectors)?;

    let n = self.training_count;
    let n_clusters = self.config.ivf.n_clusters;

    if n < n_clusters {
      return Err(IvfPqError::NotEnoughTrainingVectors { n, k: n_clusters });
    }

    if n < self.config.pq.num_centroids {
      return Err(IvfPqError::NotEnoughTrainingVectors {
        n,
        k: self.config.pq.num_centroids,
      });
    }

    let distance_fn = self.config.ivf.metric.distance_fn();

    // Step 1: Train IVF centroids with parallel k-means
    let kmeans_config = KMeansConfig::new(n_clusters)
      .with_max_iterations(25)
      .with_tolerance(1e-4);

    let kmeans_result = kmeans_parallel(
      &training_vectors,
      n,
      self.dimensions,
      &kmeans_config,
      distance_fn,
    )
    .map_err(|e| IvfPqError::TrainingFailed(e.to_string()))?;

    self.ivf_centroids = kmeans_result.centroids;
    let assignments = kmeans_result.assignments;

    // Step 2: Compute residuals and train PQ
    let pq_training_data = if self.config.use_residuals {
      // Compute residuals: vector - assigned_centroid
      let mut residuals = vec![0.0f32; n * self.dimensions];
      for (i, &cluster_id) in assignments.iter().enumerate().take(n) {
        let cluster = cluster_id as usize;
        let vec_offset = i * self.dimensions;
        let cent_offset = cluster * self.dimensions;

        for d in 0..self.dimensions {
          residuals[vec_offset + d] =
            training_vectors[vec_offset + d] - self.ivf_centroids[cent_offset + d];
        }
      }
      residuals
    } else {
      training_vectors.clone()
    };

    // Train PQ on residuals
    self.train_pq(&pq_training_data, n)?;

    // Step 3: Pre-compute centroid distances for faster search
    let mut centroid_distances = vec![0.0f32; n_clusters * n_clusters];
    for i in 0..n_clusters {
      let ci = &self.ivf_centroids[i * self.dimensions..(i + 1) * self.dimensions];
      for j in i..n_clusters {
        let cj = &self.ivf_centroids[j * self.dimensions..(j + 1) * self.dimensions];
        let dist = distance_fn(ci, cj);
        centroid_distances[i * n_clusters + j] = dist;
        centroid_distances[j * n_clusters + i] = dist;
      }
    }
    self.centroid_distances = Some(centroid_distances);

    // Initialize inverted lists
    for c in 0..n_clusters {
      self.inverted_lists.insert(c, Vec::new());
    }

    self.trained = true;
    self.training_vectors = None;
    self.training_count = 0;

    Ok(())
  }

  /// Train PQ on the given data (parallel across subspaces)
  fn train_pq(&mut self, vectors: &[f32], num_vectors: usize) -> Result<(), IvfPqError> {
    let num_subspaces = self.config.pq.num_subspaces;
    let num_centroids = self.config.pq.num_centroids;
    let max_iterations = self.config.pq.max_iterations;
    let subspace_dims = self.subspace_dims;
    let dimensions = self.dimensions;

    // Train each subspace independently (parallel on native, sequential on wasm)
    let trained_centroids: Vec<Vec<f32>> = {
      #[cfg(not(target_arch = "wasm32"))]
      {
        (0..num_subspaces)
          .into_par_iter()
          .map(|m| {
            // Extract subvectors for this subspace
            let mut subvectors = Vec::with_capacity(num_vectors * subspace_dims);
            let sub_offset = m * subspace_dims;

            for i in 0..num_vectors {
              let vec_offset = i * dimensions + sub_offset;
              subvectors.extend_from_slice(&vectors[vec_offset..vec_offset + subspace_dims]);
            }

            // Run k-means on subvectors
            let mut centroids = vec![0.0f32; num_centroids * subspace_dims];
            train_pq_subspace(
              &mut centroids,
              &subvectors,
              num_vectors,
              subspace_dims,
              num_centroids,
              max_iterations,
            );
            centroids
          })
          .collect()
      }
      #[cfg(target_arch = "wasm32")]
      {
        (0..num_subspaces)
          .map(|m| {
            let mut subvectors = Vec::with_capacity(num_vectors * subspace_dims);
            let sub_offset = m * subspace_dims;

            for i in 0..num_vectors {
              let vec_offset = i * dimensions + sub_offset;
              subvectors.extend_from_slice(&vectors[vec_offset..vec_offset + subspace_dims]);
            }

            let mut centroids = vec![0.0f32; num_centroids * subspace_dims];
            train_pq_subspace(
              &mut centroids,
              &subvectors,
              num_vectors,
              subspace_dims,
              num_centroids,
              max_iterations,
            );
            centroids
          })
          .collect()
      }
    };

    // Copy results back
    for (m, centroids) in trained_centroids.into_iter().enumerate() {
      self.pq_centroids[m] = centroids;
    }

    Ok(())
  }

  /// Insert a vector into the index
  pub fn insert(&mut self, vector_id: u64, vector: &[f32]) -> Result<(), IvfPqError> {
    if !self.trained {
      return Err(IvfPqError::NotTrained);
    }

    if vector.len() != self.dimensions {
      return Err(IvfPqError::DimensionMismatch {
        expected: self.dimensions,
        got: vector.len(),
      });
    }

    let distance_fn = self.config.ivf.metric.distance_fn();

    // Prepare vector (normalize for cosine metric)
    let query_vec = if self.config.ivf.metric == DistanceMetric::Cosine {
      normalize(vector)
    } else {
      vector.to_vec()
    };

    // Find nearest centroid
    let mut best_cluster = 0;
    let mut best_dist = f32::INFINITY;

    for c in 0..self.config.ivf.n_clusters {
      let cent_offset = c * self.dimensions;
      let centroid = &self.ivf_centroids[cent_offset..cent_offset + self.dimensions];
      let dist = distance_fn(&query_vec, centroid);

      if dist < best_dist {
        best_dist = dist;
        best_cluster = c;
      }
    }

    // Compute residual or use raw vector
    let vector_to_encode = if self.config.use_residuals {
      let cent_offset = best_cluster * self.dimensions;
      query_vec
        .iter()
        .zip(&self.ivf_centroids[cent_offset..cent_offset + self.dimensions])
        .map(|(v, c)| v - c)
        .collect::<Vec<f32>>()
    } else {
      query_vec
    };

    // Encode with PQ
    let codes = self.encode_single_vector(&vector_to_encode);

    // Add to inverted list
    self
      .inverted_lists
      .entry(best_cluster)
      .or_default()
      .push(vector_id);

    // Store PQ codes
    self.pq_codes.insert(vector_id, codes);

    Ok(())
  }

  /// Encode a single vector to PQ codes
  fn encode_single_vector(&self, vector: &[f32]) -> Vec<u8> {
    let num_subspaces = self.config.pq.num_subspaces;
    let num_centroids = self.config.pq.num_centroids;

    let mut codes = vec![0u8; num_subspaces];

    for (m, code) in codes.iter_mut().enumerate().take(num_subspaces) {
      let sub_offset = m * self.subspace_dims;
      let subvec = &vector[sub_offset..sub_offset + self.subspace_dims];

      let mut best_centroid = 0;
      let mut best_dist = f32::INFINITY;

      for c in 0..num_centroids {
        let cent_offset = c * self.subspace_dims;
        let centroid = &self.pq_centroids[m][cent_offset..cent_offset + self.subspace_dims];

        let mut dist = 0.0;
        for d in 0..self.subspace_dims {
          let diff = subvec[d] - centroid[d];
          dist += diff * diff;
        }

        if dist < best_dist {
          best_dist = dist;
          best_centroid = c;
        }
      }

      *code = best_centroid as u8;
    }

    codes
  }

  /// Delete a vector from the index
  pub fn delete(&mut self, vector_id: u64, vector: &[f32]) -> bool {
    if !self.trained {
      return false;
    }

    let distance_fn = self.config.ivf.metric.distance_fn();

    // Prepare vector (normalize for cosine metric)
    let query_vec = if self.config.ivf.metric == DistanceMetric::Cosine {
      normalize(vector)
    } else {
      vector.to_vec()
    };

    // Find which cluster it's in
    let mut best_cluster = 0;
    let mut best_dist = f32::INFINITY;

    for c in 0..self.config.ivf.n_clusters {
      let cent_offset = c * self.dimensions;
      let centroid = &self.ivf_centroids[cent_offset..cent_offset + self.dimensions];
      let dist = distance_fn(&query_vec, centroid);

      if dist < best_dist {
        best_dist = dist;
        best_cluster = c;
      }
    }

    // Remove from inverted list
    let removed_from_list = if let Some(list) = self.inverted_lists.get_mut(&best_cluster) {
      if let Some(idx) = list.iter().position(|&id| id == vector_id) {
        list.swap_remove(idx);
        true
      } else {
        false
      }
    } else {
      false
    };

    // Remove PQ codes
    let removed_codes = self.pq_codes.remove(&vector_id).is_some();

    removed_from_list || removed_codes
  }

  /// Search for k nearest neighbors
  pub fn search(
    &self,
    manifest: &VectorManifest,
    query: &[f32],
    k: usize,
    options: Option<IvfPqSearchOptions>,
  ) -> Vec<VectorSearchResult> {
    if !self.trained {
      return Vec::new();
    }

    let options = options.unwrap_or_default();
    let n_probe = options.n_probe.unwrap_or(self.config.ivf.n_probe);

    // Normalize query for cosine metric
    let query_for_search = if self.config.ivf.metric == DistanceMetric::Cosine {
      normalize(query)
    } else {
      query.to_vec()
    };

    // Find top n_probe nearest centroids
    let probe_clusters = self.find_nearest_centroids(&query_for_search, n_probe);

    // Use max-heap to track top-k candidates
    let mut heap = MaxHeap::new();

    // For non-residual mode, build the distance table ONCE
    let shared_dist_table = if !self.config.use_residuals {
      Some(self.build_distance_table(&query_for_search))
    } else {
      None
    };

    // Search within selected clusters
    for cluster in probe_clusters {
      let vector_ids = match self.inverted_lists.get(&cluster) {
        Some(list) if !list.is_empty() => list,
        _ => continue,
      };

      // Get distance table (shared or per-cluster for residuals)
      let dist_table = if self.config.use_residuals {
        // Query residual = query - centroid (requires per-cluster table)
        let cent_offset = cluster * self.dimensions;
        let query_residual: Vec<f32> = query_for_search
          .iter()
          .zip(&self.ivf_centroids[cent_offset..cent_offset + self.dimensions])
          .map(|(q, c)| q - c)
          .collect();
        self.build_distance_table(&query_residual)
      } else {
        shared_dist_table.clone().unwrap()
      };

      // Search vectors in this cluster using PQ ADC
      for &vector_id in vector_ids {
        // Apply filter early if provided
        if let Some(ref filter) = options.filter {
          if let Some(&node_id) = manifest.vector_to_node.get(&vector_id) {
            if !filter(node_id) {
              continue;
            }
          }
        }

        // Get PQ codes for this vector
        let codes = match self.pq_codes.get(&vector_id) {
          Some(c) => c,
          None => continue,
        };

        // Compute approximate distance using ADC
        let dist = self.distance_adc(&dist_table, codes);

        // Apply threshold filter
        if let Some(threshold) = options.threshold {
          let similarity = self.config.ivf.metric.distance_to_similarity(dist);
          if similarity < threshold {
            continue;
          }
        }

        // Add to heap
        if heap.len() < k {
          heap.push(vector_id, dist);
        } else if let Some(&(_, max_dist)) = heap.peek() {
          if dist < max_dist {
            heap.pop();
            heap.push(vector_id, dist);
          }
        }
      }
    }

    // Convert to results
    let results = heap.into_sorted_vec();

    results
      .into_iter()
      .map(|(vector_id, distance)| {
        let node_id = manifest
          .vector_to_node
          .get(&vector_id)
          .copied()
          .unwrap_or(0);
        VectorSearchResult {
          vector_id,
          node_id,
          distance,
          similarity: self.config.ivf.metric.distance_to_similarity(distance),
        }
      })
      .collect()
  }

  /// Build distance table for a query vector
  fn build_distance_table(&self, query: &[f32]) -> Vec<f32> {
    let num_subspaces = self.config.pq.num_subspaces;
    let num_centroids = self.config.pq.num_centroids;

    let mut table = vec![0.0; num_subspaces * num_centroids];

    for m in 0..num_subspaces {
      let sub_offset = m * self.subspace_dims;
      let table_offset = m * num_centroids;
      let query_sub = &query[sub_offset..sub_offset + self.subspace_dims];

      for c in 0..num_centroids {
        let cent_offset = c * self.subspace_dims;
        let centroid = &self.pq_centroids[m][cent_offset..cent_offset + self.subspace_dims];

        let mut dist = 0.0;
        for d in 0..self.subspace_dims {
          let diff = query_sub[d] - centroid[d];
          dist += diff * diff;
        }

        table[table_offset + c] = dist;
      }
    }

    table
  }

  /// Compute approximate distance using ADC
  fn distance_adc(&self, table: &[f32], codes: &[u8]) -> f32 {
    let num_subspaces = self.config.pq.num_subspaces;
    let num_centroids = self.config.pq.num_centroids;

    let mut dist = 0.0;

    // Unroll for performance (8x like TypeScript version)
    let remainder = num_subspaces % 8;
    let main_len = num_subspaces - remainder;

    for m in (0..main_len).step_by(8) {
      dist += table[m * num_centroids + codes[m] as usize]
        + table[(m + 1) * num_centroids + codes[m + 1] as usize]
        + table[(m + 2) * num_centroids + codes[m + 2] as usize]
        + table[(m + 3) * num_centroids + codes[m + 3] as usize]
        + table[(m + 4) * num_centroids + codes[m + 4] as usize]
        + table[(m + 5) * num_centroids + codes[m + 5] as usize]
        + table[(m + 6) * num_centroids + codes[m + 6] as usize]
        + table[(m + 7) * num_centroids + codes[m + 7] as usize];
    }

    for m in main_len..num_subspaces {
      dist += table[m * num_centroids + codes[m] as usize];
    }

    dist
  }

  /// Find the top n nearest centroids
  fn find_nearest_centroids(&self, query: &[f32], n: usize) -> Vec<usize> {
    let distance_fn = self.config.ivf.metric.distance_fn();
    let n_clusters = self.config.ivf.n_clusters;

    let mut centroid_dists: Vec<(usize, f32)> = (0..n_clusters)
      .map(|c| {
        let cent_offset = c * self.dimensions;
        let centroid = &self.ivf_centroids[cent_offset..cent_offset + self.dimensions];
        let dist = distance_fn(query, centroid);
        (c, dist)
      })
      .collect();

    centroid_dists.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

    centroid_dists.into_iter().take(n).map(|(c, _)| c).collect()
  }

  /// Search with multiple query vectors
  ///
  /// This is more efficient than running multiple separate searches because it:
  /// 1. Collects all candidate vectors across all queries
  /// 2. Aggregates distances per node using the specified aggregation method
  /// 3. Returns the top-k results based on aggregated distances
  ///
  /// # Arguments
  /// * `manifest` - The vector store manifest
  /// * `queries` - Array of query vectors (all must have same dimensions)
  /// * `k` - Number of results to return
  /// * `aggregation` - How to aggregate distances from multiple queries
  /// * `options` - Search options (n_probe, filter, threshold)
  ///
  /// # Returns
  /// Vector of search results sorted by aggregated distance
  pub fn search_multi(
    &self,
    manifest: &VectorManifest,
    queries: &[&[f32]],
    k: usize,
    aggregation: MultiQueryAggregation,
    options: Option<IvfPqSearchOptions>,
  ) -> Vec<VectorSearchResult> {
    if !self.trained || queries.is_empty() {
      return Vec::new();
    }

    let options = options.unwrap_or_default();

    // Run individual searches with higher k to ensure we have enough candidates
    let expanded_k = k * 2;
    let all_results: Vec<Vec<VectorSearchResult>> = queries
      .iter()
      .map(|query| self.search(manifest, query, expanded_k, None))
      .collect();

    // Aggregate by node_id
    let mut aggregated: std::collections::HashMap<NodeId, (Vec<f32>, u64)> =
      std::collections::HashMap::new();

    for results in &all_results {
      for result in results {
        let entry = aggregated
          .entry(result.node_id)
          .or_insert_with(|| (Vec::new(), result.vector_id));
        entry.0.push(result.distance);
      }
    }

    // Apply filter if provided
    let aggregated: std::collections::HashMap<NodeId, (Vec<f32>, u64)> =
      if let Some(ref filter) = options.filter {
        aggregated
          .into_iter()
          .filter(|(node_id, _)| filter(*node_id))
          .collect()
      } else {
        aggregated
      };

    // Compute aggregated scores and build results
    let mut scored: Vec<VectorSearchResult> = aggregated
      .into_iter()
      .map(|(node_id, (distances, vector_id))| {
        let distance = aggregation.aggregate(&distances);
        let similarity = self.config.ivf.metric.distance_to_similarity(distance);
        VectorSearchResult {
          vector_id,
          node_id,
          distance,
          similarity,
        }
      })
      .collect();

    // Apply threshold filter
    if let Some(threshold) = options.threshold {
      scored.retain(|r| r.similarity >= threshold);
    }

    // Sort by distance and return top k
    scored.sort_by(|a, b| {
      a.distance
        .partial_cmp(&b.distance)
        .unwrap_or(std::cmp::Ordering::Equal)
    });
    scored.truncate(k);

    scored
  }

  /// Build index from all vectors in the store
  pub fn build_from_store(&mut self, manifest: &VectorManifest) -> Result<(), IvfPqError> {
    // Collect training vectors
    for fragment in &manifest.fragments {
      for row_group in &fragment.row_groups {
        self.add_training_vectors(&row_group.data, row_group.count)?;
      }
    }

    // Train the index
    self.train()?;

    // Build fragment lookup map for O(1) access
    let fragment_map: std::collections::HashMap<usize, &_> =
      manifest.fragments.iter().map(|f| (f.id, f)).collect();

    // Insert all vectors
    for (&vector_id, location) in &manifest.vector_locations {
      // Get fragment with O(1) lookup
      let fragment = match fragment_map.get(&location.fragment_id) {
        Some(f) => *f,
        None => continue,
      };

      if fragment.is_deleted(location.local_index) {
        continue;
      }

      let row_group_idx = location.local_index / manifest.config.row_group_size;
      let local_row_idx = location.local_index % manifest.config.row_group_size;
      let row_group = match fragment.row_groups.get(row_group_idx) {
        Some(rg) => rg,
        None => continue,
      };

      let offset = local_row_idx * manifest.config.dimensions;
      let vector = &row_group.data[offset..offset + manifest.config.dimensions];

      self.insert(vector_id, vector)?;
    }

    Ok(())
  }

  /// Get index statistics
  pub fn stats(&self) -> IvfPqStats {
    let mut total_vectors = 0;
    let mut empty_clusters = 0;
    let mut min_cluster_size = usize::MAX;
    let mut max_cluster_size = 0;

    for list in self.inverted_lists.values() {
      total_vectors += list.len();
      if list.is_empty() {
        empty_clusters += 1;
      }
      min_cluster_size = min_cluster_size.min(list.len());
      max_cluster_size = max_cluster_size.max(list.len());
    }

    if self.inverted_lists.is_empty() {
      min_cluster_size = 0;
    }

    // Memory calculation
    let original_bytes = total_vectors * self.dimensions * 4; // float32
    let pq_code_bytes = total_vectors * self.config.pq.num_subspaces; // uint8 codes
    let pq_centroid_bytes =
      self.config.pq.num_subspaces * self.config.pq.num_centroids * self.subspace_dims * 4;
    let ivf_centroid_bytes = self.config.ivf.n_clusters * self.dimensions * 4;

    let compressed_bytes = pq_code_bytes + pq_centroid_bytes + ivf_centroid_bytes;
    let memory_savings_ratio = if original_bytes > 0 {
      original_bytes as f32 / compressed_bytes as f32
    } else {
      0.0
    };

    IvfPqStats {
      trained: self.trained,
      n_clusters: self.config.ivf.n_clusters,
      total_vectors,
      avg_vectors_per_cluster: if self.config.ivf.n_clusters > 0 {
        total_vectors as f32 / self.config.ivf.n_clusters as f32
      } else {
        0.0
      },
      empty_cluster_count: empty_clusters,
      min_cluster_size,
      max_cluster_size,
      pq_num_subspaces: self.config.pq.num_subspaces,
      pq_num_centroids: self.config.pq.num_centroids,
      memory_savings_ratio,
    }
  }

  /// Clear the index (but keep configuration)
  pub fn clear(&mut self) {
    self.ivf_centroids.clear();
    self.inverted_lists.clear();
    self.pq_codes.clear();
    self.centroid_distances = None;
    self.trained = false;
    self.training_vectors = Some(Vec::new());
    self.training_count = 0;

    // Reset PQ centroids
    for centroids in &mut self.pq_centroids {
      centroids.fill(0.0);
    }
  }
}

// ============================================================================
// Search Options
// ============================================================================

/// Options for IVF-PQ search
#[derive(Default)]
pub struct IvfPqSearchOptions {
  /// Number of clusters to probe (overrides config)
  pub n_probe: Option<usize>,
  /// Filter function (return true to include)
  pub filter: Option<Box<dyn Fn(NodeId) -> bool>>,
  /// Minimum similarity threshold
  pub threshold: Option<f32>,
}

impl std::fmt::Debug for IvfPqSearchOptions {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("IvfPqSearchOptions")
      .field("n_probe", &self.n_probe)
      .field("filter", &self.filter.as_ref().map(|_| "<fn>"))
      .field("threshold", &self.threshold)
      .finish()
  }
}

// ============================================================================
// Statistics
// ============================================================================

/// IVF-PQ index statistics
#[derive(Debug, Clone)]
pub struct IvfPqStats {
  pub trained: bool,
  pub n_clusters: usize,
  pub total_vectors: usize,
  pub avg_vectors_per_cluster: f32,
  pub empty_cluster_count: usize,
  pub min_cluster_size: usize,
  pub max_cluster_size: usize,
  pub pq_num_subspaces: usize,
  pub pq_num_centroids: usize,
  pub memory_savings_ratio: f32,
}

// ============================================================================
// Max Heap for Top-K
// ============================================================================

/// Simple max-heap for top-k selection
struct MaxHeap {
  items: Vec<(u64, f32)>, // (vector_id, distance)
}

impl MaxHeap {
  fn new() -> Self {
    Self { items: Vec::new() }
  }

  fn len(&self) -> usize {
    self.items.len()
  }

  fn push(&mut self, id: u64, dist: f32) {
    self.items.push((id, dist));
    self.sift_up(self.items.len() - 1);
  }

  fn pop(&mut self) -> Option<(u64, f32)> {
    if self.items.is_empty() {
      return None;
    }
    let len = self.items.len();
    self.items.swap(0, len - 1);
    let result = self.items.pop();
    if !self.items.is_empty() {
      self.sift_down(0);
    }
    result
  }

  fn peek(&self) -> Option<&(u64, f32)> {
    self.items.first()
  }

  fn sift_up(&mut self, mut idx: usize) {
    while idx > 0 {
      let parent = (idx - 1) / 2;
      if self.items[idx].1 > self.items[parent].1 {
        self.items.swap(idx, parent);
        idx = parent;
      } else {
        break;
      }
    }
  }

  fn sift_down(&mut self, mut idx: usize) {
    let len = self.items.len();
    loop {
      let left = 2 * idx + 1;
      let right = 2 * idx + 2;
      let mut largest = idx;

      if left < len && self.items[left].1 > self.items[largest].1 {
        largest = left;
      }
      if right < len && self.items[right].1 > self.items[largest].1 {
        largest = right;
      }

      if largest != idx {
        self.items.swap(idx, largest);
        idx = largest;
      } else {
        break;
      }
    }
  }

  fn into_sorted_vec(mut self) -> Vec<(u64, f32)> {
    let mut result = Vec::with_capacity(self.items.len());
    while let Some(item) = self.pop() {
      result.push(item);
    }
    result.reverse();
    result
  }
}

// ============================================================================
// Training Helpers
// ============================================================================

/// K-means training for a single PQ subspace
fn train_pq_subspace(
  centroids: &mut [f32],
  subvectors: &[f32],
  num_vectors: usize,
  subspace_dims: usize,
  num_centroids: usize,
  max_iterations: usize,
) {
  // Initialize centroids with k-means++
  initialize_pq_centroids_kmeans_pp(
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

/// K-means++ initialization for PQ subspace centroids
fn initialize_pq_centroids_kmeans_pp(
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

    for (i, min_dist) in min_dists.iter_mut().enumerate().take(num_vectors) {
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
    for (i, dist) in min_dists.iter().enumerate().take(num_vectors) {
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

// ============================================================================
// Errors
// ============================================================================

#[derive(Debug, Clone)]
pub enum IvfPqError {
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
  NoTrainingVectors,
  NotEnoughTrainingVectors {
    n: usize,
    k: usize,
  },
  TrainingFailed(String),
}

impl std::fmt::Display for IvfPqError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      IvfPqError::DimensionNotDivisible {
        dimensions,
        num_subspaces,
      } => write!(
        f,
        "Dimensions ({dimensions}) must be divisible by num_subspaces ({num_subspaces})"
      ),
      IvfPqError::DimensionMismatch { expected, got } => {
        write!(f, "Dimension mismatch: expected {expected}, got {got}")
      }
      IvfPqError::AlreadyTrained => write!(f, "Index already trained"),
      IvfPqError::NotTrained => write!(f, "Index not trained"),
      IvfPqError::NoTrainingVectors => write!(f, "No training vectors provided"),
      IvfPqError::NotEnoughTrainingVectors { n, k } => {
        write!(f, "Not enough training vectors: {n} < {k} required")
      }
      IvfPqError::TrainingFailed(msg) => write!(f, "Training failed: {msg}"),
    }
  }
}

impl std::error::Error for IvfPqError {}

// ============================================================================
// Serialization
// ============================================================================

/// Magic number for IVF-PQ index: "IVPQ"
const IVFPQ_MAGIC: u32 = 0x49565051;
/// Header size for IVF-PQ index
const IVFPQ_HEADER_SIZE: usize = 48;

/// Serialization error
#[derive(Debug, Clone)]
pub enum SerializeError {
  /// Invalid magic number
  InvalidMagic { expected: u32, got: u32 },
  /// Buffer underflow
  BufferUnderflow {
    context: String,
    offset: usize,
    needed: usize,
    available: usize,
  },
  /// Invalid metric value
  InvalidMetric(u32),
}

impl std::fmt::Display for SerializeError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      SerializeError::InvalidMagic { expected, got } => {
        write!(
          f,
          "Invalid magic: expected 0x{expected:08X}, got 0x{got:08X}"
        )
      }
      SerializeError::BufferUnderflow {
        context,
        offset,
        needed,
        available,
      } => {
        write!(
          f,
          "Buffer underflow in {context}: need {needed} bytes at offset {offset}, but only {available} available"
        )
      }
      SerializeError::InvalidMetric(n) => {
        write!(
          f,
          "Invalid metric value: {n}. Expected 0 (cosine), 1 (euclidean), or 2 (dot)"
        )
      }
    }
  }
}

impl std::error::Error for SerializeError {}

/// Convert DistanceMetric to u8
fn metric_to_u8(metric: DistanceMetric) -> u8 {
  match metric {
    DistanceMetric::Cosine => 0,
    DistanceMetric::Euclidean => 1,
    DistanceMetric::DotProduct => 2,
  }
}

/// Convert u8 to DistanceMetric
fn u8_to_metric(n: u8) -> Result<DistanceMetric, SerializeError> {
  match n {
    0 => Ok(DistanceMetric::Cosine),
    1 => Ok(DistanceMetric::Euclidean),
    2 => Ok(DistanceMetric::DotProduct),
    _ => Err(SerializeError::InvalidMetric(n as u32)),
  }
}

/// Ensure buffer has enough bytes remaining
fn ensure_bytes(
  buf_len: usize,
  offset: usize,
  needed: usize,
  context: &str,
) -> Result<(), SerializeError> {
  if offset + needed > buf_len {
    return Err(SerializeError::BufferUnderflow {
      context: context.to_string(),
      offset,
      needed,
      available: buf_len.saturating_sub(offset),
    });
  }
  Ok(())
}

/// Calculate serialized size of IVF-PQ index
pub fn ivf_pq_serialized_size(index: &IvfPqIndex) -> usize {
  let mut size = IVFPQ_HEADER_SIZE;

  // IVF centroids
  size += 4 + index.ivf_centroids.len() * 4;

  // Number of inverted lists
  size += 4;

  // Inverted lists
  for list in index.inverted_lists.values() {
    size += 4 + 4 + list.len() * 8; // cluster ID + list length + vector IDs (u64)
  }

  // PQ centroids
  size += 4; // num_subspaces
  for centroids in &index.pq_centroids {
    size += 4 + centroids.len() * 4; // centroid count + centroids
  }

  // PQ codes
  size += 4; // count
  for codes in index.pq_codes.values() {
    size += 8 + 4 + codes.len(); // vector_id (u64) + code_len (u32) + codes
  }

  // Centroid distances (optional)
  size += 1; // has_centroid_distances flag
  if let Some(ref dists) = index.centroid_distances {
    size += 4 + dists.len() * 4; // count + distances
  }

  size
}

/// Serialize IVF-PQ index to binary
///
/// # Format
/// - Header (48 bytes)
///   - magic (4): "IVPQ" = 0x49565051
///   - dimensions (4)
///   - n_clusters (4)
///   - n_probe (4)
///   - num_subspaces (4)
///   - num_centroids (4)
///   - max_iterations (4)
///   - metric (1): 0=cosine, 1=euclidean, 2=dot
///   - trained (1)
///   - use_residuals (1)
///   - reserved (17)
/// - ivf_centroid_count (4)
/// - IVF centroids (ivf_centroid_count * 4 bytes)
/// - num_inverted_lists (4)
/// - For each inverted list:
///   - cluster ID (4)
///   - list length (4)
///   - vector IDs (length * 8)
/// - num_pq_subspaces (4)
/// - For each PQ subspace:
///   - centroid_count (4)
///   - centroids (centroid_count * 4)
/// - num_pq_codes (4)
/// - For each PQ code entry:
///   - vector_id (8)
///   - code_len (4)
///   - codes (code_len bytes)
/// - has_centroid_distances (1)
/// - If has_centroid_distances:
///   - distance_count (4)
///   - distances (distance_count * 4)
pub fn serialize_ivf_pq(index: &IvfPqIndex) -> Vec<u8> {
  let size = ivf_pq_serialized_size(index);
  let mut buffer = Vec::with_capacity(size);

  // Header
  buffer.extend_from_slice(&IVFPQ_MAGIC.to_le_bytes());
  buffer.extend_from_slice(&(index.dimensions as u32).to_le_bytes());
  buffer.extend_from_slice(&(index.config.ivf.n_clusters as u32).to_le_bytes());
  buffer.extend_from_slice(&(index.config.ivf.n_probe as u32).to_le_bytes());
  buffer.extend_from_slice(&(index.config.pq.num_subspaces as u32).to_le_bytes());
  buffer.extend_from_slice(&(index.config.pq.num_centroids as u32).to_le_bytes());
  buffer.extend_from_slice(&(index.config.pq.max_iterations as u32).to_le_bytes());
  buffer.push(metric_to_u8(index.config.ivf.metric));
  buffer.push(if index.trained { 1 } else { 0 });
  buffer.push(if index.config.use_residuals { 1 } else { 0 });
  buffer.extend_from_slice(&[0u8; 17]); // reserved

  // IVF centroids
  buffer.extend_from_slice(&(index.ivf_centroids.len() as u32).to_le_bytes());
  for &val in &index.ivf_centroids {
    buffer.extend_from_slice(&val.to_le_bytes());
  }

  // Inverted lists
  buffer.extend_from_slice(&(index.inverted_lists.len() as u32).to_le_bytes());
  for (&cluster, list) in &index.inverted_lists {
    buffer.extend_from_slice(&(cluster as u32).to_le_bytes());
    buffer.extend_from_slice(&(list.len() as u32).to_le_bytes());
    for &vector_id in list {
      buffer.extend_from_slice(&vector_id.to_le_bytes());
    }
  }

  // PQ centroids
  buffer.extend_from_slice(&(index.pq_centroids.len() as u32).to_le_bytes());
  for centroids in &index.pq_centroids {
    buffer.extend_from_slice(&(centroids.len() as u32).to_le_bytes());
    for &val in centroids {
      buffer.extend_from_slice(&val.to_le_bytes());
    }
  }

  // PQ codes
  buffer.extend_from_slice(&(index.pq_codes.len() as u32).to_le_bytes());
  for (&vector_id, codes) in &index.pq_codes {
    buffer.extend_from_slice(&vector_id.to_le_bytes());
    buffer.extend_from_slice(&(codes.len() as u32).to_le_bytes());
    buffer.extend_from_slice(codes);
  }

  // Centroid distances
  if let Some(ref dists) = index.centroid_distances {
    buffer.push(1);
    buffer.extend_from_slice(&(dists.len() as u32).to_le_bytes());
    for &val in dists {
      buffer.extend_from_slice(&val.to_le_bytes());
    }
  } else {
    buffer.push(0);
  }

  buffer
}

/// Deserialize IVF-PQ index from binary
pub fn deserialize_ivf_pq(buffer: &[u8]) -> Result<IvfPqIndex, SerializeError> {
  let buf_len = buffer.len();
  ensure_bytes(buf_len, 0, IVFPQ_HEADER_SIZE, "IVF-PQ header")?;

  let mut offset = 0;

  // Header
  let magic = u32::from_le_bytes(buffer[offset..offset + 4].try_into().unwrap());
  offset += 4;
  if magic != IVFPQ_MAGIC {
    return Err(SerializeError::InvalidMagic {
      expected: IVFPQ_MAGIC,
      got: magic,
    });
  }

  let dimensions = u32::from_le_bytes(buffer[offset..offset + 4].try_into().unwrap()) as usize;
  offset += 4;
  let n_clusters = u32::from_le_bytes(buffer[offset..offset + 4].try_into().unwrap()) as usize;
  offset += 4;
  let n_probe = u32::from_le_bytes(buffer[offset..offset + 4].try_into().unwrap()) as usize;
  offset += 4;
  let num_subspaces = u32::from_le_bytes(buffer[offset..offset + 4].try_into().unwrap()) as usize;
  offset += 4;
  let num_centroids = u32::from_le_bytes(buffer[offset..offset + 4].try_into().unwrap()) as usize;
  offset += 4;
  let max_iterations = u32::from_le_bytes(buffer[offset..offset + 4].try_into().unwrap()) as usize;
  offset += 4;
  let metric = u8_to_metric(buffer[offset])?;
  offset += 1;
  let trained = buffer[offset] == 1;
  offset += 1;
  let use_residuals = buffer[offset] == 1;
  offset += 1;
  offset += 17; // reserved

  let config = IvfPqConfig {
    ivf: IvfConfig {
      n_clusters,
      n_probe,
      metric,
    },
    pq: PqConfig {
      num_subspaces,
      num_centroids,
      max_iterations,
    },
    use_residuals,
  };

  // IVF centroids
  ensure_bytes(buf_len, offset, 4, "IVF-PQ centroid count")?;
  let ivf_centroid_count =
    u32::from_le_bytes(buffer[offset..offset + 4].try_into().unwrap()) as usize;
  offset += 4;

  ensure_bytes(
    buf_len,
    offset,
    ivf_centroid_count * 4,
    "IVF-PQ IVF centroids",
  )?;
  let mut ivf_centroids = Vec::with_capacity(ivf_centroid_count);
  for _ in 0..ivf_centroid_count {
    let val = f32::from_le_bytes(buffer[offset..offset + 4].try_into().unwrap());
    ivf_centroids.push(val);
    offset += 4;
  }

  // Inverted lists
  ensure_bytes(buf_len, offset, 4, "IVF-PQ inverted list count")?;
  let num_lists = u32::from_le_bytes(buffer[offset..offset + 4].try_into().unwrap()) as usize;
  offset += 4;

  let mut inverted_lists: HashMap<usize, Vec<u64>> = HashMap::new();
  for i in 0..num_lists {
    ensure_bytes(
      buf_len,
      offset,
      8,
      &format!("IVF-PQ inverted list {i} header"),
    )?;
    let cluster = u32::from_le_bytes(buffer[offset..offset + 4].try_into().unwrap()) as usize;
    offset += 4;
    let list_length = u32::from_le_bytes(buffer[offset..offset + 4].try_into().unwrap()) as usize;
    offset += 4;

    ensure_bytes(
      buf_len,
      offset,
      list_length * 8,
      &format!("IVF-PQ inverted list {i} data"),
    )?;
    let mut list = Vec::with_capacity(list_length);
    for _ in 0..list_length {
      let vector_id = u64::from_le_bytes(buffer[offset..offset + 8].try_into().unwrap());
      list.push(vector_id);
      offset += 8;
    }
    inverted_lists.insert(cluster, list);
  }

  // PQ centroids
  ensure_bytes(buf_len, offset, 4, "IVF-PQ PQ subspace count")?;
  let num_pq_subspaces =
    u32::from_le_bytes(buffer[offset..offset + 4].try_into().unwrap()) as usize;
  offset += 4;

  let mut pq_centroids = Vec::with_capacity(num_pq_subspaces);
  for i in 0..num_pq_subspaces {
    ensure_bytes(
      buf_len,
      offset,
      4,
      &format!("IVF-PQ PQ subspace {i} centroid count"),
    )?;
    let centroid_count =
      u32::from_le_bytes(buffer[offset..offset + 4].try_into().unwrap()) as usize;
    offset += 4;

    ensure_bytes(
      buf_len,
      offset,
      centroid_count * 4,
      &format!("IVF-PQ PQ subspace {i} centroids"),
    )?;
    let mut centroids = Vec::with_capacity(centroid_count);
    for _ in 0..centroid_count {
      let val = f32::from_le_bytes(buffer[offset..offset + 4].try_into().unwrap());
      centroids.push(val);
      offset += 4;
    }
    pq_centroids.push(centroids);
  }

  // PQ codes
  ensure_bytes(buf_len, offset, 4, "IVF-PQ PQ codes count")?;
  let num_pq_codes = u32::from_le_bytes(buffer[offset..offset + 4].try_into().unwrap()) as usize;
  offset += 4;

  let mut pq_codes: HashMap<u64, Vec<u8>> = HashMap::new();
  for i in 0..num_pq_codes {
    ensure_bytes(buf_len, offset, 12, &format!("IVF-PQ PQ code {i} header"))?;
    let vector_id = u64::from_le_bytes(buffer[offset..offset + 8].try_into().unwrap());
    offset += 8;
    let code_len = u32::from_le_bytes(buffer[offset..offset + 4].try_into().unwrap()) as usize;
    offset += 4;

    ensure_bytes(
      buf_len,
      offset,
      code_len,
      &format!("IVF-PQ PQ code {i} data"),
    )?;
    let codes = buffer[offset..offset + code_len].to_vec();
    offset += code_len;
    pq_codes.insert(vector_id, codes);
  }

  // Centroid distances
  ensure_bytes(buf_len, offset, 1, "IVF-PQ centroid distances flag")?;
  let has_centroid_distances = buffer[offset] == 1;
  offset += 1;

  let centroid_distances = if has_centroid_distances {
    ensure_bytes(buf_len, offset, 4, "IVF-PQ centroid distances count")?;
    let distance_count =
      u32::from_le_bytes(buffer[offset..offset + 4].try_into().unwrap()) as usize;
    offset += 4;

    ensure_bytes(
      buf_len,
      offset,
      distance_count * 4,
      "IVF-PQ centroid distances",
    )?;
    let mut dists = Vec::with_capacity(distance_count);
    for _ in 0..distance_count {
      let val = f32::from_le_bytes(buffer[offset..offset + 4].try_into().unwrap());
      dists.push(val);
      offset += 4;
    }
    Some(dists)
  } else {
    None
  };

  IvfPqIndex::from_serialized(
    config,
    ivf_centroids,
    inverted_lists,
    pq_codes,
    pq_centroids,
    centroid_distances,
    dimensions,
    trained,
  )
  .map_err(|e| SerializeError::BufferUnderflow {
    context: format!("IVF-PQ index construction: {e}"),
    offset: 0,
    needed: 0,
    available: 0,
  })
}

/// Write IVF-PQ index to a writer
pub fn write_ivf_pq<W: std::io::Write>(
  index: &IvfPqIndex,
  writer: &mut W,
) -> std::io::Result<usize> {
  let data = serialize_ivf_pq(index);
  writer.write_all(&data)?;
  Ok(data.len())
}

/// Read IVF-PQ index from a reader
pub fn read_ivf_pq<R: std::io::Read>(reader: &mut R) -> Result<IvfPqIndex, SerializeError> {
  let mut buffer = Vec::new();
  reader
    .read_to_end(&mut buffer)
    .map_err(|e| SerializeError::BufferUnderflow {
      context: format!("IO error: {e}"),
      offset: 0,
      needed: 0,
      available: 0,
    })?;
  deserialize_ivf_pq(&buffer)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;

  fn test_config() -> IvfPqConfig {
    IvfPqConfig {
      ivf: IvfConfig {
        n_clusters: 4,
        n_probe: 2,
        metric: DistanceMetric::Euclidean,
      },
      pq: PqConfig {
        num_subspaces: 4,
        num_centroids: 8,
        max_iterations: 10,
      },
      use_residuals: true,
    }
  }

  #[test]
  fn test_ivf_pq_new() {
    let index = IvfPqIndex::new(16, test_config()).unwrap();
    assert_eq!(index.dimensions, 16);
    assert_eq!(index.subspace_dims, 4);
    assert!(!index.trained);
  }

  #[test]
  fn test_ivf_pq_new_not_divisible() {
    let result = IvfPqIndex::new(15, test_config());
    assert!(matches!(
      result,
      Err(IvfPqError::DimensionNotDivisible { .. })
    ));
  }

  #[test]
  fn test_ivf_pq_add_training_vectors() {
    let mut index = IvfPqIndex::new(16, test_config()).unwrap();

    let vectors = vec![0.0f32; 50 * 16];
    index.add_training_vectors(&vectors, 50).unwrap();

    assert_eq!(index.training_count, 50);
  }

  #[test]
  fn test_ivf_pq_train() {
    let mut index = IvfPqIndex::new(16, test_config()).unwrap();

    // Create training vectors
    let mut vectors = Vec::new();
    for i in 0..500 {
      for d in 0..16 {
        vectors.push((i * 16 + d) as f32 / 8000.0);
      }
    }
    index.add_training_vectors(&vectors, 500).unwrap();

    index.train().unwrap();

    assert!(index.trained);
    assert_eq!(index.ivf_centroids.len(), 4 * 16); // n_clusters * dimensions
    assert!(index.centroid_distances.is_some());
  }

  #[test]
  fn test_ivf_pq_train_not_enough_vectors() {
    let mut index = IvfPqIndex::new(16, test_config()).unwrap();

    let vectors = vec![0.0f32; 2 * 16]; // Only 2 vectors, need at least 4 clusters
    index.add_training_vectors(&vectors, 2).unwrap();

    let result = index.train();
    assert!(matches!(
      result,
      Err(IvfPqError::NotEnoughTrainingVectors { .. })
    ));
  }

  #[test]
  fn test_ivf_pq_insert() {
    let mut index = IvfPqIndex::new(16, test_config()).unwrap();

    // Train first
    let mut vectors = Vec::new();
    for i in 0..500 {
      for d in 0..16 {
        vectors.push((i * 16 + d) as f32 / 8000.0);
      }
    }
    index.add_training_vectors(&vectors, 500).unwrap();
    index.train().unwrap();

    // Insert
    let vector = vec![0.5f32; 16];
    index.insert(0, &vector).unwrap();

    let stats = index.stats();
    assert_eq!(stats.total_vectors, 1);
    assert!(index.pq_codes.contains_key(&0));
  }

  #[test]
  fn test_ivf_pq_insert_not_trained() {
    let mut index = IvfPqIndex::new(16, test_config()).unwrap();

    let vector = vec![0.5f32; 16];
    let result = index.insert(0, &vector);

    assert!(matches!(result, Err(IvfPqError::NotTrained)));
  }

  #[test]
  fn test_ivf_pq_delete() {
    let mut index = IvfPqIndex::new(16, test_config()).unwrap();

    // Train
    let mut vectors = Vec::new();
    for i in 0..500 {
      for d in 0..16 {
        vectors.push((i * 16 + d) as f32 / 8000.0);
      }
    }
    index.add_training_vectors(&vectors, 500).unwrap();
    index.train().unwrap();

    // Insert and delete
    let vector = vec![0.5f32; 16];
    index.insert(0, &vector).unwrap();
    assert!(index.delete(0, &vector));
    assert!(!index.delete(0, &vector)); // Already deleted

    let stats = index.stats();
    assert_eq!(stats.total_vectors, 0);
  }

  #[test]
  fn test_ivf_pq_stats() {
    let mut index = IvfPqIndex::new(16, test_config()).unwrap();

    // Train
    let mut vectors = Vec::new();
    for i in 0..500 {
      for d in 0..16 {
        vectors.push((i * 16 + d) as f32 / 8000.0);
      }
    }
    index.add_training_vectors(&vectors, 500).unwrap();
    index.train().unwrap();

    // Insert some vectors
    for i in 0..10 {
      let vector: Vec<f32> = (0..16).map(|d| (i * 16 + d) as f32 / 160.0).collect();
      index.insert(i as u64, &vector).unwrap();
    }

    let stats = index.stats();
    assert!(stats.trained);
    assert_eq!(stats.n_clusters, 4);
    assert_eq!(stats.total_vectors, 10);
    assert_eq!(stats.pq_num_subspaces, 4);
    assert_eq!(stats.pq_num_centroids, 8);
    assert!(stats.memory_savings_ratio > 0.0);
  }

  #[test]
  fn test_ivf_pq_clear() {
    let mut index = IvfPqIndex::new(16, test_config()).unwrap();

    // Train
    let mut vectors = Vec::new();
    for i in 0..500 {
      for d in 0..16 {
        vectors.push((i * 16 + d) as f32 / 8000.0);
      }
    }
    index.add_training_vectors(&vectors, 500).unwrap();
    index.train().unwrap();

    // Insert
    let vector = vec![0.5f32; 16];
    index.insert(0, &vector).unwrap();

    index.clear();

    assert!(!index.trained);
    assert!(index.ivf_centroids.is_empty());
    assert!(index.inverted_lists.is_empty());
    assert!(index.pq_codes.is_empty());
  }

  #[test]
  fn test_ivf_pq_config_builder() {
    let config = IvfPqConfig::new()
      .with_n_clusters(50)
      .with_n_probe(5)
      .with_metric(DistanceMetric::Euclidean)
      .with_num_subspaces(32)
      .with_num_centroids(128)
      .with_residuals(false);

    assert_eq!(config.ivf.n_clusters, 50);
    assert_eq!(config.ivf.n_probe, 5);
    assert_eq!(config.ivf.metric, DistanceMetric::Euclidean);
    assert_eq!(config.pq.num_subspaces, 32);
    assert_eq!(config.pq.num_centroids, 128);
    assert!(!config.use_residuals);
  }

  #[test]
  fn test_ivf_pq_without_residuals() {
    let config = IvfPqConfig {
      ivf: IvfConfig {
        n_clusters: 4,
        n_probe: 2,
        metric: DistanceMetric::Euclidean,
      },
      pq: PqConfig {
        num_subspaces: 4,
        num_centroids: 8,
        max_iterations: 10,
      },
      use_residuals: false, // No residual encoding
    };

    let mut index = IvfPqIndex::new(16, config).unwrap();

    // Train
    let mut vectors = Vec::new();
    for i in 0..500 {
      for d in 0..16 {
        vectors.push((i * 16 + d) as f32 / 8000.0);
      }
    }
    index.add_training_vectors(&vectors, 500).unwrap();
    index.train().unwrap();

    assert!(index.trained);
  }

  #[test]
  fn test_max_heap() {
    let mut heap = MaxHeap::new();

    heap.push(1, 0.5);
    heap.push(2, 0.3);
    heap.push(3, 0.8);
    heap.push(4, 0.1);

    assert_eq!(heap.len(), 4);

    // Max should be 3 (distance 0.8)
    let (id, dist) = *heap.peek().unwrap();
    assert_eq!(id, 3);
    assert_eq!(dist, 0.8);

    let sorted = heap.into_sorted_vec();
    assert_eq!(sorted.len(), 4);
    // Should be sorted by distance ascending
    assert!(sorted[0].1 <= sorted[1].1);
    assert!(sorted[1].1 <= sorted[2].1);
    assert!(sorted[2].1 <= sorted[3].1);
  }

  #[test]
  fn test_error_display() {
    let err1 = IvfPqError::DimensionNotDivisible {
      dimensions: 15,
      num_subspaces: 4,
    };
    assert!(err1.to_string().contains("15"));
    assert!(err1.to_string().contains("4"));

    let err2 = IvfPqError::AlreadyTrained;
    assert!(err2.to_string().contains("already"));

    let err3 = IvfPqError::NotTrained;
    assert!(err3.to_string().contains("not trained"));
  }

  #[test]
  fn test_ivf_pq_serialize_empty() {
    let index = IvfPqIndex::new(16, test_config()).unwrap();

    let serialized = serialize_ivf_pq(&index);
    let deserialized = deserialize_ivf_pq(&serialized).unwrap();

    assert_eq!(deserialized.dimensions, 16);
    assert_eq!(deserialized.config.ivf.n_clusters, 4);
    assert_eq!(deserialized.config.pq.num_subspaces, 4);
    assert!(!deserialized.trained);
  }

  #[test]
  fn test_ivf_pq_serialize_round_trip() {
    let mut index = IvfPqIndex::new(16, test_config()).unwrap();

    // Train
    let mut vectors = Vec::new();
    for i in 0..500 {
      for d in 0..16 {
        vectors.push((i * 16 + d) as f32 / 8000.0);
      }
    }
    index.add_training_vectors(&vectors, 500).unwrap();
    index.train().unwrap();

    // Insert some vectors
    for i in 0..10 {
      let vector: Vec<f32> = (0..16).map(|d| (i * 16 + d) as f32 / 160.0).collect();
      index.insert(i as u64, &vector).unwrap();
    }

    let serialized = serialize_ivf_pq(&index);
    let deserialized = deserialize_ivf_pq(&serialized).unwrap();

    assert_eq!(deserialized.dimensions, index.dimensions);
    assert_eq!(
      deserialized.config.ivf.n_clusters,
      index.config.ivf.n_clusters
    );
    assert_eq!(
      deserialized.config.pq.num_subspaces,
      index.config.pq.num_subspaces
    );
    assert_eq!(
      deserialized.config.use_residuals,
      index.config.use_residuals
    );
    assert!(deserialized.trained);
    assert_eq!(deserialized.pq_codes.len(), 10);
    assert!(deserialized.centroid_distances.is_some());

    // Check stats match
    let orig_stats = index.stats();
    let deser_stats = deserialized.stats();
    assert_eq!(orig_stats.total_vectors, deser_stats.total_vectors);
  }

  #[test]
  fn test_ivf_pq_serialize_invalid_magic() {
    let mut buffer = vec![0u8; IVFPQ_HEADER_SIZE];
    buffer[0..4].copy_from_slice(&0x00000000u32.to_le_bytes()); // Wrong magic

    let result = deserialize_ivf_pq(&buffer);
    assert!(matches!(result, Err(SerializeError::InvalidMagic { .. })));
  }

  #[test]
  fn test_ivf_pq_serialize_buffer_underflow() {
    let buffer = vec![]; // Empty buffer
    let result = deserialize_ivf_pq(&buffer);
    assert!(matches!(
      result,
      Err(SerializeError::BufferUnderflow { .. })
    ));
  }

  #[test]
  fn test_ivf_pq_serialized_size() {
    let mut index = IvfPqIndex::new(16, test_config()).unwrap();

    // Train
    let mut vectors = Vec::new();
    for i in 0..500 {
      for d in 0..16 {
        vectors.push((i * 16 + d) as f32 / 8000.0);
      }
    }
    index.add_training_vectors(&vectors, 500).unwrap();
    index.train().unwrap();

    // Insert some vectors
    for i in 0..5 {
      let vector: Vec<f32> = (0..16).map(|d| (i * 16 + d) as f32 / 80.0).collect();
      index.insert(i as u64, &vector).unwrap();
    }

    let size = ivf_pq_serialized_size(&index);
    let serialized = serialize_ivf_pq(&index);

    assert_eq!(size, serialized.len());
  }

  // ========================================================================
  // Multi-Query Search Tests
  // ========================================================================

  #[test]
  fn test_ivf_pq_search_multi_empty_queries() {
    let mut index = IvfPqIndex::new(16, test_config()).unwrap();

    // Train
    let mut vectors = Vec::new();
    for i in 0..500 {
      for d in 0..16 {
        vectors.push((i * 16 + d) as f32 / 8000.0);
      }
    }
    index.add_training_vectors(&vectors, 500).unwrap();
    index.train().unwrap();

    // Create a minimal manifest
    let config = crate::vector::types::VectorStoreConfig::new(16);
    let manifest = crate::vector::types::VectorManifest::new(config);

    // Empty queries should return empty results
    let results = index.search_multi(&manifest, &[], 5, MultiQueryAggregation::Min, None);
    assert!(results.is_empty());
  }

  #[test]
  fn test_ivf_pq_search_multi_not_trained() {
    let index = IvfPqIndex::new(16, test_config()).unwrap();
    let config = crate::vector::types::VectorStoreConfig::new(16);
    let manifest = crate::vector::types::VectorManifest::new(config);

    let query = vec![0.5f32; 16];
    let results = index.search_multi(&manifest, &[&query], 5, MultiQueryAggregation::Min, None);
    assert!(results.is_empty());
  }
}
