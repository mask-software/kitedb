//! IVF-PQ: Combined Inverted File Index with Product Quantization
//!
//! This combines IVF (for coarse clustering) with PQ (for fast distance computation).
//! It's the standard approach used by FAISS and other high-performance vector DBs.
//!
//! Architecture:
//! 1. IVF partitions vectors into clusters using coarse centroids
//! 2. PQ compresses residuals (vector - centroid) for each cluster
//! 3. Search: find nearest clusters, then use ADC on PQ codes
//! 4. Re-rank: order the best ADC candidates by exact distance to their
//!    stored vectors (see [`IvfPqSearchOptions::rerank_factor`])
//!
//! This provides:
//! - Fast coarse search (IVF centroid comparison)
//! - Fast fine search (PQ table lookups instead of full distance)
//! - Memory efficiency (PQ codes instead of full vectors)
//!
//! Ported from src/vector/ivf-pq.ts

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

#[cfg(not(target_arch = "wasm32"))]
use rayon::prelude::*;

use crate::types::NodeId;
use crate::vector::distance::{normalize, normalize_in_place, with_metric_distance};
use crate::vector::ivf::index::nearest_first;
use crate::vector::ivf::kmeans::{
  assign_to_centroids, nearest_centroid, training_sample, MAX_TRAINING_POINTS_PER_CLUSTER,
};
use crate::vector::ivf::{kmeans_parallel, KMeansConfig};
use crate::vector::store::{validate_manifest_layout, FragmentLookup};
use crate::vector::top_k::TopK;
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
    let subspace_dims = validate_ivf_pq_config(dimensions, &config)?;
    let pq_centroid_len = config
      .pq
      .num_centroids
      .checked_mul(subspace_dims)
      .ok_or_else(|| IvfPqError::SizeOverflow("IVF-PQ centroid allocation".into()))?;

    // Initialize empty PQ centroids for each subspace
    let pq_centroids: Vec<Vec<f32>> = (0..config.pq.num_subspaces)
      .map(|_| vec![0.0; pq_centroid_len])
      .collect();

    Ok(Self {
      config,
      ivf_centroids: Vec::new(),
      inverted_lists: HashMap::new(),
      pq_codes: HashMap::new(),
      pq_centroids,
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
    _centroid_distances: Option<Vec<f32>>,
    dimensions: usize,
    trained: bool,
  ) -> Result<Self, IvfPqError> {
    let subspace_dims = validate_ivf_pq_config(dimensions, &config)?;
    validate_ivf_pq_parts(&SerializedIvfPqParts {
      config: &config,
      ivf_centroids: &ivf_centroids,
      inverted_lists: &inverted_lists,
      pq_codes: &pq_codes,
      pq_centroids: &pq_centroids,
      dimensions,
      trained,
    })?;

    Ok(Self {
      config,
      ivf_centroids,
      inverted_lists,
      pq_codes,
      pq_centroids,
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

    let expected_len = count
      .checked_mul(self.dimensions)
      .ok_or_else(|| IvfPqError::SizeOverflow("IVF-PQ training input".into()))?;
    if vectors.len() < expected_len {
      return Err(IvfPqError::DimensionMismatch {
        expected: expected_len,
        got: vectors.len(),
      });
    }

    let training_buf = self.training_vectors.get_or_insert_with(Vec::new);
    training_buf.extend_from_slice(&vectors[..expected_len]);
    self.training_count = self
      .training_count
      .checked_add(count)
      .ok_or_else(|| IvfPqError::SizeOverflow("IVF-PQ training count".into()))?;

    Ok(())
  }

  /// Train the IVF-PQ index
  ///
  /// The coarse clusters train on at most 256 vectors per cluster and the PQ
  /// codebooks on at most 256 per centroid, sampled from the buffer.
  ///
  /// # Errors
  /// A failed train keeps the buffered vectors, so adding more and retrying
  /// works.
  pub fn train(&mut self) -> Result<(), IvfPqError> {
    if self.trained {
      return Ok(());
    }

    // Validate before taking the buffer, so a rejected train keeps it.
    validate_ivf_pq_config(self.dimensions, &self.config)?;
    if self.training_vectors.is_none() {
      return Err(IvfPqError::NoTrainingVectors);
    }
    self.check_training_count(self.training_count)?;
    let Some(mut vectors) = self.training_vectors.take() else {
      return Err(IvfPqError::NoTrainingVectors);
    };

    // Normalizing the buffer in place keeps it valid for a retry.
    if self.config.ivf.metric == DistanceMetric::Cosine {
      for vector in vectors.chunks_exact_mut(self.dimensions) {
        normalize_in_place(vector);
      }
    }

    match self.train_on(&vectors, self.training_count, false) {
      Ok(()) => {
        self.training_count = 0;
        Ok(())
      }
      Err(err) => {
        self.training_vectors = Some(vectors);
        Err(err)
      }
    }
  }

  /// Train on `n` vectors the caller keeps, without copying them into the
  /// training buffer. `unit_vectors` says they are already unit length (a
  /// normalizing store), which saves a normalized copy for cosine.
  pub(crate) fn train_from(
    &mut self,
    vectors: &[f32],
    n: usize,
    unit_vectors: bool,
  ) -> Result<(), IvfPqError> {
    if self.trained {
      return Err(IvfPqError::AlreadyTrained);
    }
    validate_ivf_pq_config(self.dimensions, &self.config)?;
    self.check_training_count(n)?;
    let expected_len = n
      .checked_mul(self.dimensions)
      .ok_or_else(|| IvfPqError::SizeOverflow("IVF-PQ training input".into()))?;
    if vectors.len() < expected_len {
      return Err(IvfPqError::DimensionMismatch {
        expected: expected_len,
        got: vectors.len(),
      });
    }
    let normalize = self.config.ivf.metric == DistanceMetric::Cosine && !unit_vectors;
    self.train_on(&vectors[..expected_len], n, normalize)
  }

  fn check_training_count(&self, n: usize) -> Result<(), IvfPqError> {
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
    Ok(())
  }

  /// Coarse k-means over a sample of `vectors` (normalized first when
  /// `normalize` is set), then PQ codebooks over a smaller sample.
  fn train_on(&mut self, vectors: &[f32], n: usize, normalize: bool) -> Result<(), IvfPqError> {
    let dimensions = self.dimensions;
    let n_clusters = self.config.ivf.n_clusters;
    let metric = self.config.ivf.metric;

    let (mut sample, sample_n) = training_sample(
      vectors,
      n,
      dimensions,
      n_clusters.saturating_mul(MAX_TRAINING_POINTS_PER_CLUSTER),
    );
    if normalize {
      for vector in sample.to_mut().chunks_exact_mut(dimensions) {
        normalize_in_place(vector);
      }
    }

    // Step 1: Train IVF centroids with parallel k-means
    let kmeans_config = KMeansConfig::new(n_clusters)
      .with_max_iterations(25)
      .with_tolerance(1e-4);
    let kmeans_result = with_metric_distance!(metric, |dist| kmeans_parallel(
      &sample,
      sample_n,
      dimensions,
      &kmeans_config,
      dist
    ))
    .map_err(|e| IvfPqError::TrainingFailed(e.to_string()))?;

    let mut ivf_centroids = kmeans_result.centroids;
    if metric == DistanceMetric::Cosine {
      // k-means updates centroids with arithmetic means.  Normalize those
      // means before using them for insert/search/delete, which all compare
      // against normalized cosine vectors.
      for centroid in ivf_centroids.chunks_exact_mut(dimensions) {
        normalize_in_place(centroid);
      }
    }

    // Step 2: Train PQ on residuals or raw vectors. The codebooks need far
    // fewer points than the coarse clusters (at most 256 per centroid), and
    // the sample is drawn from the coarse one.
    let (pq_sample, pq_n) = training_sample(
      &sample,
      sample_n,
      dimensions,
      self
        .config
        .pq
        .num_centroids
        .saturating_mul(MAX_TRAINING_POINTS_PER_CLUSTER),
    );
    if self.config.use_residuals {
      // Assign against the final centroids: for cosine they were normalized
      // after the k-means updates.
      let mut assignments = vec![0u32; pq_n];
      with_metric_distance!(metric, |dist| assign_to_centroids(
        &pq_sample,
        dimensions,
        &ivf_centroids,
        &mut assignments,
        &dist,
        true
      ));
      let mut residuals = pq_sample.into_owned();
      for (vector, &cluster) in residuals.chunks_exact_mut(dimensions).zip(&assignments) {
        let offset = cluster as usize * dimensions;
        for (value, &centroid) in vector
          .iter_mut()
          .zip(&ivf_centroids[offset..offset + dimensions])
        {
          *value -= centroid;
        }
      }
      self.train_pq(&residuals, pq_n)?;
    } else {
      self.train_pq(&pq_sample, pq_n)?;
    }

    self.ivf_centroids = ivf_centroids;
    self.inverted_lists = (0..n_clusters).map(|c| (c, Vec::new())).collect();
    self.trained = true;
    self.training_vectors = None;

    Ok(())
  }

  /// Train PQ on the given data (parallel across subspaces)
  fn train_pq(&mut self, vectors: &[f32], num_vectors: usize) -> Result<(), IvfPqError> {
    let num_subspaces = self.config.pq.num_subspaces;
    let num_centroids = self.config.pq.num_centroids;
    let max_iterations = self.config.pq.max_iterations;
    let subspace_dims = self.subspace_dims;
    let dimensions = self.dimensions;
    let subvector_capacity = num_vectors
      .checked_mul(subspace_dims)
      .ok_or_else(|| IvfPqError::SizeOverflow("IVF-PQ subvector allocation".into()))?;

    // Train each subspace independently (parallel on native, sequential on wasm)
    let trained_centroids: Vec<Vec<f32>> = {
      #[cfg(not(target_arch = "wasm32"))]
      {
        (0..num_subspaces)
          .into_par_iter()
          .map(|m| {
            // Extract subvectors for this subspace
            let mut subvectors = Vec::with_capacity(subvector_capacity);
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
            let mut subvectors = Vec::with_capacity(subvector_capacity);
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
    self.check_dimensions(vector)?;

    // Prepare vector (normalize for cosine metric)
    let query_vec = self.prepare_query(vector);
    let query_slice = query_vec.as_ref();
    let best_cluster = self.find_nearest_centroid(query_slice);

    // Compute residual or use raw vector
    // Encode with PQ (avoid allocation for non-residual paths)
    let codes = if self.config.use_residuals {
      let cent_offset = best_cluster * self.dimensions;
      let residuals: Vec<f32> = query_slice
        .iter()
        .zip(&self.ivf_centroids[cent_offset..cent_offset + self.dimensions])
        .map(|(v, c)| v - c)
        .collect();
      self.encode_single_vector(&residuals)
    } else {
      self.encode_single_vector(query_slice)
    };

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
  ///
  /// # Errors
  /// Returns an error if the vector length does not match the index dimensions.
  pub fn delete(&mut self, vector_id: u64, vector: &[f32]) -> Result<bool, IvfPqError> {
    self.check_dimensions(vector)?;
    if !self.trained {
      return Ok(false);
    }

    // Prepare vector (normalize for cosine metric)
    let query_vec = self.prepare_query(vector);
    let best_cluster = self.find_nearest_centroid(query_vec.as_ref());

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

    Ok(removed_from_list || removed_codes)
  }

  /// Search for k nearest neighbors
  ///
  /// Ranks the probed clusters' vectors by PQ (ADC) distance, then re-ranks
  /// the best of them by exact distance to their vectors in `manifest`
  /// (see [`IvfPqSearchOptions::rerank_factor`]).
  ///
  /// # Errors
  /// Returns an error if the query or the manifest does not match the index
  /// dimensions, or if the manifest's row groups are malformed.
  pub fn search(
    &self,
    manifest: &VectorManifest,
    query: &[f32],
    k: usize,
    options: Option<IvfPqSearchOptions>,
  ) -> Result<Vec<VectorSearchResult>, IvfPqError> {
    self.check_dimensions(query)?;
    self.check_manifest(manifest)?;
    if k == 0 {
      return Ok(Vec::new());
    }
    let options = options.unwrap_or_default();
    let query = self.prepare_query(query);
    let hits = match rerank_candidates(k, options.rerank_factor) {
      // The threshold applies to the exact distance, after the re-rank.
      Some(candidates) => self.rerank(
        manifest,
        &query,
        k,
        self.collect_candidates(manifest, &query, candidates, &options, false),
        options.threshold,
      ),
      None => self.collect_candidates(manifest, &query, k, &options, true),
    };
    Ok(
      hits
        .into_iter()
        .map(|(candidate, distance)| self.to_result(candidate, distance))
        .collect(),
    )
  }

  /// The `k` closest of the ADC `candidates` by exact distance from the
  /// prepared `query` to their vectors in `manifest`, closest first. A
  /// candidate whose vector the manifest does not hold (one that carries
  /// only node mappings) keeps its ADC distance.
  fn rerank(
    &self,
    manifest: &VectorManifest,
    query: &[f32],
    k: usize,
    candidates: Vec<(Candidate, f32)>,
    threshold: Option<f32>,
  ) -> Vec<(Candidate, f32)> {
    let metric = self.config.ivf.metric;
    let fragments = FragmentLookup::new(manifest);
    let mut top = TopK::new(k);
    with_metric_distance!(
      metric,
      stored_normalized = manifest.config.normalize_on_insert,
      |dist| {
        for (candidate, adc_distance) in candidates {
          let distance = match fragments.vector_by_id(manifest, candidate.vector_id) {
            Some(vector) => dist(query, vector),
            None => adc_distance,
          };
          if passes_threshold(metric, threshold, distance) {
            top.push(candidate, distance);
          }
        }
      }
    );
    top.into_sorted_vec()
  }

  /// One ADC table per prepared query (for `cluster`'s residuals, if any).
  fn distance_tables(&self, queries: &[Cow<'_, [f32]>], cluster: Option<usize>) -> Vec<AdcTable> {
    queries
      .iter()
      .map(|query| self.build_distance_table(query, cluster))
      .collect()
  }

  /// Top-k candidates for a prepared (cosine-normalized) query, best first.
  /// Multi-query search disables `apply_threshold` and thresholds the
  /// aggregated distance instead.
  fn collect_candidates(
    &self,
    manifest: &VectorManifest,
    query: &[f32],
    k: usize,
    options: &IvfPqSearchOptions,
    apply_threshold: bool,
  ) -> Vec<(Candidate, f32)> {
    if !self.trained || k == 0 {
      return Vec::new();
    }

    // An n_probe of zero searches one cluster rather than none.
    let n_probe = options.n_probe.unwrap_or(self.config.ivf.n_probe).max(1);

    // Find top n_probe nearest centroids
    let probe_clusters = self.find_nearest_centroids(query, n_probe);

    // Track the top-k candidates (NaN distances never enter)
    let mut top = TopK::new(k);

    // For non-residual mode, build the distance table ONCE
    let shared_table = (!self.config.use_residuals).then(|| self.build_distance_table(query, None));

    // Search within selected clusters
    for cluster in probe_clusters {
      let vector_ids = match self.inverted_lists.get(&cluster) {
        Some(list) if !list.is_empty() => list,
        _ => continue,
      };

      let residual_table;
      let dist_table = match &shared_table {
        Some(table) => table,
        None => {
          residual_table = self.build_distance_table(query, Some(cluster));
          &residual_table
        }
      };

      // Search vectors in this cluster using PQ ADC
      for &vector_id in vector_ids {
        // A missing mapping is not a valid result. Do this check before the
        // filter so an unmappable vector can never bypass it.
        let node_id = match manifest.vector_to_node.get(&vector_id) {
          Some(&node_id) => node_id,
          None => continue,
        };

        if let Some(ref filter) = options.filter {
          if !filter(node_id) {
            continue;
          }
        }

        // Get PQ codes for this vector
        let codes = match self.pq_codes.get(&vector_id) {
          Some(c) => c,
          None => continue,
        };

        // Compute approximate distance using ADC
        let dist = self.distance_adc(dist_table, codes);

        if apply_threshold && !passes_threshold(self.config.ivf.metric, options.threshold, dist) {
          continue;
        }

        top.push(
          Candidate {
            vector_id,
            node_id,
            cluster,
          },
          dist,
        );
      }
    }

    top.into_sorted_vec()
  }

  fn to_result(&self, candidate: Candidate, distance: f32) -> VectorSearchResult {
    VectorSearchResult {
      vector_id: candidate.vector_id,
      node_id: candidate.node_id,
      distance,
      similarity: self.config.ivf.metric.distance_to_similarity(distance),
    }
  }

  fn prepare_query<'a>(&self, query: &'a [f32]) -> Cow<'a, [f32]> {
    if self.config.ivf.metric == DistanceMetric::Cosine {
      Cow::Owned(normalize(query))
    } else {
      Cow::Borrowed(query)
    }
  }

  fn check_dimensions(&self, vector: &[f32]) -> Result<(), IvfPqError> {
    if vector.len() != self.dimensions {
      return Err(IvfPqError::DimensionMismatch {
        expected: self.dimensions,
        got: vector.len(),
      });
    }
    Ok(())
  }

  /// Search reads only the node mappings, but a manifest of a different
  /// width cannot belong to this index.
  fn check_manifest(&self, manifest: &VectorManifest) -> Result<(), IvfPqError> {
    validate_manifest_layout(manifest, self.dimensions)
      .map_err(|e| IvfPqError::InvalidManifest(e.to_string()))
  }

  /// Build a metric-aware ADC table.
  ///
  /// The table is evaluated in the same native distance space as
  /// `DistanceMetric::distance_fn`: Euclidean returns L2 (not squared L2),
  /// dot product returns negative inner product, and cosine returns
  /// `1 - normalized_inner_product`.
  fn build_distance_table(&self, query: &[f32], cluster: Option<usize>) -> AdcTable {
    let num_subspaces = self.config.pq.num_subspaces;
    let num_centroids = self.config.pq.num_centroids;
    let mut values = vec![0.0; num_subspaces * num_centroids];
    let mut norm_sq = if self.config.ivf.metric == DistanceMetric::Cosine {
      Some(vec![0.0; num_subspaces * num_centroids])
    } else {
      None
    };

    for m in 0..num_subspaces {
      let sub_offset = m * self.subspace_dims;
      let table_offset = m * num_centroids;
      let query_sub = &query[sub_offset..sub_offset + self.subspace_dims];
      let centroid_sub = cluster.map(|cluster| {
        let offset = cluster * self.dimensions + sub_offset;
        &self.ivf_centroids[offset..offset + self.subspace_dims]
      });

      for c in 0..num_centroids {
        let pq_offset = c * self.subspace_dims;
        let pq_centroid = &self.pq_centroids[m][pq_offset..pq_offset + self.subspace_dims];
        let table_index = table_offset + c;

        match self.config.ivf.metric {
          DistanceMetric::Euclidean => {
            let mut squared_distance = 0.0;
            for d in 0..self.subspace_dims {
              let reconstructed = pq_centroid[d] + centroid_sub.map_or(0.0, |centroid| centroid[d]);
              let diff = query_sub[d] - reconstructed;
              squared_distance += diff * diff;
            }
            values[table_index] = squared_distance;
          }
          DistanceMetric::DotProduct => {
            let mut inner_product = 0.0;
            for d in 0..self.subspace_dims {
              let reconstructed = pq_centroid[d] + centroid_sub.map_or(0.0, |centroid| centroid[d]);
              inner_product += query_sub[d] * reconstructed;
            }
            // The exact path uses -dot_product as its sortable distance.
            values[table_index] = -inner_product;
          }
          DistanceMetric::Cosine => {
            let mut inner_product = 0.0;
            let mut reconstructed_norm_sq = 0.0;
            for d in 0..self.subspace_dims {
              let reconstructed = pq_centroid[d] + centroid_sub.map_or(0.0, |centroid| centroid[d]);
              inner_product += query_sub[d] * reconstructed;
              reconstructed_norm_sq += reconstructed * reconstructed;
            }
            values[table_index] = inner_product;
            norm_sq.as_mut().expect("cosine norm table")[table_index] = reconstructed_norm_sq;
          }
        }
      }
    }

    AdcTable { values, norm_sq }
  }

  /// Compute a native metric distance using ADC.
  fn distance_adc(&self, table: &AdcTable, codes: &[u8]) -> f32 {
    let num_subspaces = self.config.pq.num_subspaces;
    let num_centroids = self.config.pq.num_centroids;
    let mut value = 0.0;
    let mut reconstructed_norm_sq = 0.0;

    // Unroll for performance (8x like the TypeScript version).
    let remainder = num_subspaces % 8;
    let main_len = num_subspaces - remainder;

    for m in (0..main_len).step_by(8) {
      let indices = [
        m * num_centroids + codes[m] as usize,
        (m + 1) * num_centroids + codes[m + 1] as usize,
        (m + 2) * num_centroids + codes[m + 2] as usize,
        (m + 3) * num_centroids + codes[m + 3] as usize,
        (m + 4) * num_centroids + codes[m + 4] as usize,
        (m + 5) * num_centroids + codes[m + 5] as usize,
        (m + 6) * num_centroids + codes[m + 6] as usize,
        (m + 7) * num_centroids + codes[m + 7] as usize,
      ];
      for &index in &indices {
        value += table.values[index];
        if let Some(norm_sq) = &table.norm_sq {
          reconstructed_norm_sq += norm_sq[index];
        }
      }
    }

    for (m, code) in codes.iter().enumerate().skip(main_len).take(num_subspaces) {
      let index = m * num_centroids + *code as usize;
      value += table.values[index];
      if let Some(norm_sq) = &table.norm_sq {
        reconstructed_norm_sq += norm_sq[index];
      }
    }

    match self.config.ivf.metric {
      DistanceMetric::Euclidean => value.max(0.0).sqrt(),
      DistanceMetric::DotProduct => value,
      DistanceMetric::Cosine => {
        let similarity = if reconstructed_norm_sq > 1e-20 {
          value / reconstructed_norm_sq.sqrt()
        } else {
          0.0
        };
        1.0 - similarity
      }
    }
  }

  /// Nearest coarse centroid to a prepared (cosine-normalized) vector.
  fn find_nearest_centroid(&self, vector: &[f32]) -> usize {
    with_metric_distance!(self.config.ivf.metric, |dist| nearest_centroid(
      vector,
      &self.ivf_centroids,
      self.dimensions,
      &dist
    )
    .0)
  }

  /// Find the `n` nearest centroids, closest first.
  fn find_nearest_centroids(&self, query: &[f32], n: usize) -> Vec<usize> {
    let mut centroid_dists: Vec<(usize, f32)> =
      with_metric_distance!(self.config.ivf.metric, |dist| {
        self
          .ivf_centroids
          .chunks_exact(self.dimensions)
          .map(|centroid| dist(query, centroid))
          .enumerate()
          .collect()
      });
    nearest_first(&mut centroid_dists, n)
  }

  /// Search with multiple query vectors
  ///
  /// This is more efficient than running multiple separate searches because it:
  /// 1. Collects all candidate vectors across all queries
  /// 2. Aggregates distances per node using the specified aggregation method
  ///    (exact distances unless the re-rank is off, as in [`Self::search`])
  /// 3. Returns the top-k results based on aggregated distances
  ///
  /// # Arguments
  /// * `manifest` - The vector store manifest
  /// * `queries` - Array of query vectors (all must have same dimensions)
  /// * `k` - Number of results to return
  /// * `aggregation` - How to aggregate distances from multiple queries
  /// * `options` - Search options (n_probe, filter, threshold, rerank_factor)
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
  ) -> Result<Vec<VectorSearchResult>, IvfPqError> {
    for query in queries {
      self.check_dimensions(query)?;
    }
    self.check_manifest(manifest)?;
    if !self.trained || queries.is_empty() || k == 0 {
      return Ok(Vec::new());
    }

    let options = options.unwrap_or_default();

    // Filter while collecting candidates.  Over-fetch grows geometrically up
    // to the number of indexed codes, so a filtered search can recover k
    // survivors without an unbounded scan/retry loop.  `n_probe` is passed
    // through on every pass; it is never replaced by the config default.
    let max_candidates = self.pq_codes.len();
    if max_candidates == 0 {
      return Ok(Vec::new());
    }
    // With the exact re-rank, each query contributes as many ADC candidates
    // as a single-query search would re-rank.
    let rerank = rerank_candidates(k, options.rerank_factor);
    let mut expanded_k = k
      .saturating_mul(2)
      .max(rerank.unwrap_or(k))
      .min(max_candidates);

    let prepared: Vec<Cow<[f32]>> = queries
      .iter()
      .map(|query| self.prepare_query(query))
      .collect();
    let fragments = rerank.is_some().then(|| FragmentLookup::new(manifest));
    // ADC tables, built on first use. Without residuals they depend only on
    // the queries, so they are shared across clusters and passes.
    let mut shared_tables: Option<Vec<AdcTable>> = None;
    let mut distances = Vec::with_capacity(prepared.len());

    loop {
      // Candidates: union of each query's top-expanded_k.
      let mut seen = HashSet::new();
      let mut by_cluster: HashMap<usize, Vec<Candidate>> = HashMap::new();
      let mut all_short = true;
      for query in &prepared {
        let found = self.collect_candidates(manifest, query, expanded_k, &options, false);
        all_short &= found.len() < expanded_k;
        for (candidate, _) in found {
          if seen.insert(candidate.vector_id) {
            by_cluster
              .entry(candidate.cluster)
              .or_default()
              .push(candidate);
          }
        }
      }
      let exhausted = expanded_k >= max_candidates || all_short;

      // Score every candidate against every query. A candidate found by only
      // one query must not be aggregated over that query's distance alone.
      // With the re-rank, a candidate's distances are exact when the manifest
      // holds its vector, and ADC otherwise.
      let metric = self.config.ivf.metric;
      let mut top = TopK::new(k);
      with_metric_distance!(
        metric,
        stored_normalized = manifest.config.normalize_on_insert,
        |dist| {
          for (cluster, candidates) in by_cluster {
            // Residual tables depend on the cluster.
            let mut cluster_tables: Option<Vec<AdcTable>> = None;
            for candidate in candidates {
              distances.clear();
              let stored = fragments
                .as_ref()
                .and_then(|fragments| fragments.vector_by_id(manifest, candidate.vector_id));
              if let Some(vector) = stored {
                distances.extend(prepared.iter().map(|query| dist(query, vector)));
              } else {
                let Some(codes) = self.pq_codes.get(&candidate.vector_id) else {
                  continue;
                };
                let tables = if self.config.use_residuals {
                  cluster_tables
                    .get_or_insert_with(|| self.distance_tables(&prepared, Some(cluster)))
                } else {
                  shared_tables.get_or_insert_with(|| self.distance_tables(&prepared, None))
                };
                distances.extend(tables.iter().map(|table| self.distance_adc(table, codes)));
              }
              let distance = aggregation.aggregate(&distances);
              if passes_threshold(metric, options.threshold, distance) {
                top.push(candidate, distance);
              }
            }
          }
        }
      );

      // Stop as soon as enough filtered and threshold-qualified nodes are
      // available, or when every query exhausted its selected-cluster
      // candidates.
      if top.len() >= k || exhausted {
        return Ok(
          top
            .into_sorted_vec()
            .into_iter()
            .map(|(candidate, distance)| self.to_result(candidate, distance))
            .collect(),
        );
      }

      let next_k = expanded_k.saturating_mul(2).min(max_candidates);
      if next_k == expanded_k {
        return Ok(Vec::new());
      }
      expanded_k = next_k;
    }
  }

  /// Build index from all vectors in the store
  pub fn build_from_store(&mut self, manifest: &VectorManifest) -> Result<(), IvfPqError> {
    self.check_manifest(manifest)?;
    if self.trained {
      return Err(IvfPqError::AlreadyTrained);
    }

    // Train on the live vectors only: deleted slots still hold data.
    let fragments = FragmentLookup::new(manifest);
    let live: Vec<(u64, &[f32])> = manifest
      .vector_locations
      .iter()
      .filter_map(|(&vector_id, location)| {
        fragments
          .vector(&manifest.config, location)
          .map(|vector| (vector_id, vector))
      })
      .collect();
    self
      .training_vectors
      .get_or_insert_with(Vec::new)
      .reserve(live.len().saturating_mul(self.dimensions));
    for (_, vector) in &live {
      self.add_training_vectors(vector, 1)?;
    }
    self.train()?;

    for (vector_id, vector) in live {
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

/// Metric-aware ADC lookup tables.
///
/// `values` stores squared component distances for Euclidean, negative
/// component inner products for DotProduct, and component inner products for
/// Cosine.  Cosine additionally stores the reconstructed component norms so
/// the final ADC score is a normalized inner product.
#[derive(Debug, Clone)]
struct AdcTable {
  values: Vec<f32>,
  norm_sq: Option<Vec<f32>>,
}

/// Default exact re-rank over-fetch factor for IVF-PQ search (see
/// [`IvfPqSearchOptions::rerank_factor`]).
///
/// With [`MIN_RERANK_CANDIDATES`], a default search re-ranks the best
/// `max(k * 4, 80)` ADC candidates. On clustered 128-d data (32 subspaces,
/// no residuals, 10K-100K vectors) that lifted recall@10 from 0.15-0.68 to
/// 0.53-1.0 for 10-15% more search time. At k >= 50, 4k candidates already
/// reached recall@k 0.98; 8k cost a third more time for little gain.
pub const DEFAULT_RERANK_FACTOR: usize = 4;

/// Fewest candidates an IVF-PQ search re-ranks by exact distance. The PQ
/// ranking error does not shrink with `k`: recall@1 needs about as many
/// candidates as recall@10.
pub const MIN_RERANK_CANDIDATES: usize = 80;

/// Options for IVF-PQ search
#[derive(Default)]
pub struct IvfPqSearchOptions {
  /// Number of clusters to probe (overrides config)
  pub n_probe: Option<usize>,
  /// Filter function (return true to include)
  pub filter: Option<Box<dyn Fn(NodeId) -> bool>>,
  /// Minimum similarity threshold, applied to the returned distance
  pub threshold: Option<f32>,
  /// Exact re-rank over-fetch factor.
  ///
  /// The PQ (ADC) scan keeps the `k * rerank_factor` closest candidates (at
  /// least [`MIN_RERANK_CANDIDATES`]), which are then ranked by exact
  /// distance to their vectors in the search's manifest, so results carry
  /// exact distances and the threshold applies to them. A candidate whose
  /// vector the manifest does not hold keeps its ADC distance. `None` uses
  /// [`DEFAULT_RERANK_FACTOR`]; `Some(0)` skips the re-rank and returns the
  /// approximate ADC ranking and distances.
  pub rerank_factor: Option<usize>,
}

impl std::fmt::Debug for IvfPqSearchOptions {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("IvfPqSearchOptions")
      .field("n_probe", &self.n_probe)
      .field("filter", &self.filter.as_ref().map(|_| "<fn>"))
      .field("threshold", &self.threshold)
      .field("rerank_factor", &self.rerank_factor)
      .finish()
  }
}

/// How many ADC candidates to re-rank for a top-`k` search, or None when the
/// re-rank is off (`rerank_factor` of 0).
fn rerank_candidates(k: usize, rerank_factor: Option<usize>) -> Option<usize> {
  match rerank_factor.unwrap_or(DEFAULT_RERANK_FACTOR) {
    0 => None,
    factor => Some(k.saturating_mul(factor).max(MIN_RERANK_CANDIDATES)),
  }
}

fn passes_threshold(metric: DistanceMetric, threshold: Option<f32>, distance: f32) -> bool {
  threshold.is_none_or(|threshold| metric.distance_to_similarity(distance) >= threshold)
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
// Candidates
// ============================================================================

/// A search hit before it becomes a `VectorSearchResult`. Multi-query search
/// needs the cluster to rebuild residual ADC tables for the other queries.
#[derive(Debug, Clone, Copy)]
struct Candidate {
  vector_id: u64,
  node_id: NodeId,
  cluster: usize,
}

// ============================================================================
// Training Helpers
// ============================================================================

/// K-means training for a single PQ subspace
///
/// Stops early once an iteration changes no assignment: the centroids would
/// not move again.
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

  let mut assignments = vec![u16::MAX; num_vectors];
  let mut cluster_sums = vec![0.0f32; num_centroids * subspace_dims];
  let mut cluster_counts = vec![0u32; num_centroids];
  // Centroids by dimension (`[d][c]`), so the distances from one subvector
  // to all centroids accumulate in a loop over contiguous centroids, which
  // vectorizes, instead of one short loop per centroid.
  let mut by_dimension = vec![0.0f32; num_centroids * subspace_dims];
  let mut distances = vec![0.0f32; num_centroids];

  for _ in 0..max_iterations {
    for (c, centroid) in centroids.chunks_exact(subspace_dims).enumerate() {
      for (d, &value) in centroid.iter().enumerate() {
        by_dimension[d * num_centroids + c] = value;
      }
    }

    // Assign vectors to nearest centroids
    let mut changed = false;
    for (subvector, assignment) in subvectors
      .chunks_exact(subspace_dims)
      .zip(assignments.iter_mut())
      .take(num_vectors)
    {
      distances.fill(0.0);
      for (&x, column) in subvector
        .iter()
        .zip(by_dimension.chunks_exact(num_centroids))
      {
        for (dist, &c) in distances.iter_mut().zip(column) {
          let diff = x - c;
          *dist += diff * diff;
        }
      }
      // First centroid at the minimum distance, as a strict `<` scan picks.
      let min = distances.iter().copied().fold(f32::INFINITY, f32::min);
      let best = distances.iter().position(|&d| d == min).unwrap_or(0) as u16;
      if *assignment != best {
        *assignment = best;
        changed = true;
      }
    }
    if !changed {
      break;
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
  InvalidConfiguration(String),
  InvalidStructure(String),
  SizeOverflow(String),
  InvalidManifest(String),
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
      IvfPqError::InvalidConfiguration(msg) => {
        write!(f, "Invalid IVF-PQ configuration: {msg}")
      }
      IvfPqError::InvalidStructure(msg) => write!(f, "Invalid IVF-PQ structure: {msg}"),
      IvfPqError::SizeOverflow(context) => write!(f, "IVF-PQ size overflow: {context}"),
      IvfPqError::InvalidManifest(msg) => write!(f, "Invalid vector manifest: {msg}"),
    }
  }
}

impl std::error::Error for IvfPqError {}

fn validate_ivf_pq_config(dimensions: usize, config: &IvfPqConfig) -> Result<usize, IvfPqError> {
  if dimensions == 0 {
    return Err(IvfPqError::InvalidConfiguration(
      "dimensions must be nonzero".into(),
    ));
  }
  if config.ivf.n_clusters == 0 {
    return Err(IvfPqError::InvalidConfiguration(
      "n_clusters must be nonzero".into(),
    ));
  }
  if config.ivf.n_probe == 0 {
    return Err(IvfPqError::InvalidConfiguration(
      "n_probe must be nonzero".into(),
    ));
  }
  if config.pq.num_subspaces == 0 {
    return Err(IvfPqError::InvalidConfiguration(
      "num_subspaces must be nonzero".into(),
    ));
  }
  if !(1..=MAX_PQ_CENTROIDS).contains(&config.pq.num_centroids) {
    return Err(IvfPqError::InvalidConfiguration(format!(
      "num_centroids must be in 1..={} (got {})",
      MAX_PQ_CENTROIDS, config.pq.num_centroids
    )));
  }
  if config.pq.max_iterations == 0 {
    return Err(IvfPqError::InvalidConfiguration(
      "max_iterations must be nonzero".into(),
    ));
  }
  if !dimensions.is_multiple_of(config.pq.num_subspaces) {
    return Err(IvfPqError::DimensionNotDivisible {
      dimensions,
      num_subspaces: config.pq.num_subspaces,
    });
  }

  let subspace_dims = dimensions / config.pq.num_subspaces;
  config
    .pq
    .num_centroids
    .checked_mul(subspace_dims)
    .ok_or_else(|| IvfPqError::SizeOverflow("IVF-PQ centroid allocation".into()))?;
  Ok(subspace_dims)
}

struct SerializedIvfPqParts<'a> {
  config: &'a IvfPqConfig,
  ivf_centroids: &'a [f32],
  inverted_lists: &'a HashMap<usize, Vec<u64>>,
  pq_codes: &'a HashMap<u64, Vec<u8>>,
  pq_centroids: &'a [Vec<f32>],
  dimensions: usize,
  trained: bool,
}

fn validate_ivf_pq_parts(parts: &SerializedIvfPqParts<'_>) -> Result<(), IvfPqError> {
  let config = parts.config;
  let ivf_centroids = parts.ivf_centroids;
  let inverted_lists = parts.inverted_lists;
  let pq_codes = parts.pq_codes;
  let pq_centroids = parts.pq_centroids;
  let dimensions = parts.dimensions;
  let trained = parts.trained;
  let subspace_dims = validate_ivf_pq_config(dimensions, config)?;
  let expected_ivf_centroids = if trained {
    config
      .ivf
      .n_clusters
      .checked_mul(dimensions)
      .ok_or_else(|| IvfPqError::SizeOverflow("IVF centroid shape".into()))?
  } else {
    0
  };
  if ivf_centroids.len() != expected_ivf_centroids {
    return Err(IvfPqError::InvalidStructure(format!(
      "IVF centroid count {} does not match expected {}",
      ivf_centroids.len(),
      expected_ivf_centroids
    )));
  }

  if inverted_lists.len() > config.ivf.n_clusters {
    return Err(IvfPqError::InvalidStructure(format!(
      "inverted list count {} exceeds n_clusters {}",
      inverted_lists.len(),
      config.ivf.n_clusters
    )));
  }
  let mut list_ids = HashSet::new();
  for (&cluster, list) in inverted_lists {
    if cluster >= config.ivf.n_clusters {
      return Err(IvfPqError::InvalidStructure(format!(
        "inverted list cluster {} is outside n_clusters {}",
        cluster, config.ivf.n_clusters
      )));
    }
    for &vector_id in list {
      if !list_ids.insert(vector_id) {
        return Err(IvfPqError::InvalidStructure(format!(
          "duplicate vector id {vector_id} in inverted lists"
        )));
      }
    }
  }

  if pq_centroids.len() != config.pq.num_subspaces {
    return Err(IvfPqError::InvalidStructure(format!(
      "PQ subspace count {} does not match configured {}",
      pq_centroids.len(),
      config.pq.num_subspaces
    )));
  }
  let expected_pq_centroids = config
    .pq
    .num_centroids
    .checked_mul(subspace_dims)
    .ok_or_else(|| IvfPqError::SizeOverflow("PQ centroid shape".into()))?;
  for (subspace, centroids) in pq_centroids.iter().enumerate() {
    if centroids.len() != expected_pq_centroids {
      return Err(IvfPqError::InvalidStructure(format!(
        "PQ subspace {} centroid count {} does not match expected {}",
        subspace,
        centroids.len(),
        expected_pq_centroids
      )));
    }
  }

  for (&vector_id, codes) in pq_codes {
    if codes.len() != config.pq.num_subspaces {
      return Err(IvfPqError::InvalidStructure(format!(
        "PQ code for vector {} has length {}, expected {}",
        vector_id,
        codes.len(),
        config.pq.num_subspaces
      )));
    }
    if codes
      .iter()
      .any(|&code| usize::from(code) >= config.pq.num_centroids)
    {
      return Err(IvfPqError::InvalidStructure(format!(
        "PQ code for vector {} contains a centroid outside codebook size {}",
        vector_id, config.pq.num_centroids
      )));
    }
  }

  if list_ids.len() != pq_codes.len() || list_ids.iter().any(|id| !pq_codes.contains_key(id)) {
    return Err(IvfPqError::InvalidStructure(
      "inverted-list IDs and PQ-code IDs do not match".into(),
    ));
  }

  if !trained && (!inverted_lists.is_empty() || !pq_codes.is_empty()) {
    return Err(IvfPqError::InvalidStructure(
      "untrained index contains vectors".into(),
    ));
  }

  Ok(())
}

// ============================================================================
// Serialization
// ============================================================================

/// Magic number for IVF-PQ index: "IVPQ"
const IVFPQ_MAGIC: u32 = 0x49565051;
/// Header size for IVF-PQ index
const IVFPQ_HEADER_SIZE: usize = 48;
/// PQ codes are serialized as uint8 centroid indexes.
const MAX_PQ_CENTROIDS: usize = u8::MAX as usize + 1;
/// Header flag used by the current writer to omit the obsolete centroid
/// distance payload. A zero flag identifies the legacy format, whose trailing
/// centroid distances are still read and discarded.
const IVFPQ_FORMAT_FLAG_NO_CENTROID_DISTANCES: u8 = 1;

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
  /// Structurally inconsistent input
  InvalidStructure(String),
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
      SerializeError::InvalidStructure(msg) => write!(f, "Invalid IVF-PQ structure: {msg}"),
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
  let end = offset
    .checked_add(needed)
    .ok_or_else(|| SerializeError::InvalidStructure(format!("{context} size overflow")))?;
  if end > buf_len {
    return Err(SerializeError::BufferUnderflow {
      context: context.to_string(),
      offset,
      needed,
      available: buf_len.saturating_sub(offset),
    });
  }
  Ok(())
}

fn ensure_count_bytes(
  buf_len: usize,
  offset: usize,
  count: usize,
  minimum_size: usize,
  context: &str,
) -> Result<(), SerializeError> {
  let needed = count
    .checked_mul(minimum_size)
    .ok_or_else(|| SerializeError::InvalidStructure(format!("{context} count size overflow")))?;
  ensure_bytes(buf_len, offset, needed, context)
}

fn read_u8(buffer: &[u8], offset: &mut usize, context: &str) -> Result<u8, SerializeError> {
  let end = offset
    .checked_add(1)
    .ok_or_else(|| SerializeError::InvalidStructure(format!("{context} offset overflow")))?;
  let slice = buffer
    .get(*offset..end)
    .ok_or_else(|| SerializeError::BufferUnderflow {
      context: context.to_string(),
      offset: *offset,
      needed: 1,
      available: buffer.len().saturating_sub(*offset),
    })?;
  *offset = end;
  Ok(slice[0])
}

fn read_u32_le(buffer: &[u8], offset: &mut usize, context: &str) -> Result<u32, SerializeError> {
  let end = offset
    .checked_add(4)
    .ok_or_else(|| SerializeError::InvalidStructure(format!("{context} offset overflow")))?;
  let slice = buffer
    .get(*offset..end)
    .ok_or_else(|| SerializeError::BufferUnderflow {
      context: context.to_string(),
      offset: *offset,
      needed: 4,
      available: buffer.len().saturating_sub(*offset),
    })?;
  let mut bytes = [0u8; 4];
  bytes.copy_from_slice(slice);
  *offset = end;
  Ok(u32::from_le_bytes(bytes))
}

fn read_u64_le(buffer: &[u8], offset: &mut usize, context: &str) -> Result<u64, SerializeError> {
  let end = offset
    .checked_add(8)
    .ok_or_else(|| SerializeError::InvalidStructure(format!("{context} offset overflow")))?;
  let slice = buffer
    .get(*offset..end)
    .ok_or_else(|| SerializeError::BufferUnderflow {
      context: context.to_string(),
      offset: *offset,
      needed: 8,
      available: buffer.len().saturating_sub(*offset),
    })?;
  let mut bytes = [0u8; 8];
  bytes.copy_from_slice(slice);
  *offset = end;
  Ok(u64::from_le_bytes(bytes))
}

fn read_f32_le(buffer: &[u8], offset: &mut usize, context: &str) -> Result<f32, SerializeError> {
  let end = offset
    .checked_add(4)
    .ok_or_else(|| SerializeError::InvalidStructure(format!("{context} offset overflow")))?;
  let slice = buffer
    .get(*offset..end)
    .ok_or_else(|| SerializeError::BufferUnderflow {
      context: context.to_string(),
      offset: *offset,
      needed: 4,
      available: buffer.len().saturating_sub(*offset),
    })?;
  let mut bytes = [0u8; 4];
  bytes.copy_from_slice(slice);
  *offset = end;
  Ok(f32::from_le_bytes(bytes))
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
///   - format flags (1): bit 0 means the obsolete centroid-distance payload
///     is omitted; zero means the legacy payload follows the PQ codes
///   - reserved (16)
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
///
/// Legacy files may additionally contain a centroid-distance payload after the
/// PQ codes. It is read for compatibility and discarded.
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
  buffer.push(IVFPQ_FORMAT_FLAG_NO_CENTROID_DISTANCES);
  buffer.extend_from_slice(&[0u8; 16]); // reserved

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

  buffer
}

/// Deserialize IVF-PQ index from binary
pub fn deserialize_ivf_pq(buffer: &[u8]) -> Result<IvfPqIndex, SerializeError> {
  let buf_len = buffer.len();
  ensure_bytes(buf_len, 0, IVFPQ_HEADER_SIZE, "IVF-PQ header")?;

  let mut offset = 0;

  // Header
  let magic = read_u32_le(buffer, &mut offset, "IVF-PQ magic")?;
  if magic != IVFPQ_MAGIC {
    return Err(SerializeError::InvalidMagic {
      expected: IVFPQ_MAGIC,
      got: magic,
    });
  }

  let dimensions = read_u32_le(buffer, &mut offset, "IVF-PQ dimensions")? as usize;
  let n_clusters = read_u32_le(buffer, &mut offset, "IVF-PQ n_clusters")? as usize;
  let n_probe = read_u32_le(buffer, &mut offset, "IVF-PQ n_probe")? as usize;
  let num_subspaces = read_u32_le(buffer, &mut offset, "IVF-PQ num_subspaces")? as usize;
  let num_centroids = read_u32_le(buffer, &mut offset, "IVF-PQ num_centroids")? as usize;
  let max_iterations = read_u32_le(buffer, &mut offset, "IVF-PQ max_iterations")? as usize;
  let metric = u8_to_metric(read_u8(buffer, &mut offset, "IVF-PQ metric")?)?;
  let trained = match read_u8(buffer, &mut offset, "IVF-PQ trained")? {
    0 => false,
    1 => true,
    value => {
      return Err(SerializeError::InvalidStructure(format!(
        "IVF-PQ trained flag {value} is invalid"
      )));
    }
  };
  let use_residuals = match read_u8(buffer, &mut offset, "IVF-PQ use_residuals")? {
    0 => false,
    1 => true,
    value => {
      return Err(SerializeError::InvalidStructure(format!(
        "IVF-PQ use_residuals flag {value} is invalid"
      )));
    }
  };
  let format_flags = read_u8(buffer, &mut offset, "IVF-PQ format flags")?;
  if format_flags & !IVFPQ_FORMAT_FLAG_NO_CENTROID_DISTANCES != 0 {
    return Err(SerializeError::InvalidStructure(format!(
      "IVF-PQ format flags {format_flags:#04x} contain unknown bits"
    )));
  }
  ensure_bytes(buf_len, offset, 16, "IVF-PQ header reserved")?;
  offset += 16; // reserved

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
  validate_ivf_pq_config(dimensions, &config)
    .map_err(|error| SerializeError::InvalidStructure(error.to_string()))?;

  // IVF centroids
  let ivf_centroid_count = read_u32_le(buffer, &mut offset, "IVF-PQ centroid count")? as usize;
  ensure_count_bytes(
    buf_len,
    offset,
    ivf_centroid_count,
    4,
    "IVF-PQ IVF centroids",
  )?;
  let expected_ivf_centroid_count = if trained {
    n_clusters
      .checked_mul(dimensions)
      .ok_or_else(|| SerializeError::InvalidStructure("IVF centroid shape overflow".into()))?
  } else {
    0
  };
  if ivf_centroid_count != expected_ivf_centroid_count {
    return Err(SerializeError::InvalidStructure(format!(
      "IVF centroid count {ivf_centroid_count} does not match expected {expected_ivf_centroid_count}"
    )));
  }
  let mut ivf_centroids = Vec::with_capacity(ivf_centroid_count);
  for _ in 0..ivf_centroid_count {
    let val = read_f32_le(buffer, &mut offset, "IVF-PQ IVF centroid")?;
    ivf_centroids.push(val);
  }

  // Inverted lists
  let num_lists = read_u32_le(buffer, &mut offset, "IVF-PQ inverted list count")? as usize;
  if num_lists > n_clusters {
    return Err(SerializeError::InvalidStructure(format!(
      "inverted list count {num_lists} exceeds n_clusters {n_clusters}"
    )));
  }
  ensure_count_bytes(
    buf_len,
    offset,
    num_lists,
    8,
    "IVF-PQ inverted list headers",
  )?;

  let mut inverted_lists: HashMap<usize, Vec<u64>> = HashMap::with_capacity(num_lists);
  let mut list_ids = HashSet::new();
  for i in 0..num_lists {
    let cluster = read_u32_le(
      buffer,
      &mut offset,
      &format!("IVF-PQ inverted list {i} cluster"),
    )? as usize;
    if cluster >= n_clusters {
      return Err(SerializeError::InvalidStructure(format!(
        "IVF-PQ inverted list {i} cluster {cluster} is outside n_clusters {n_clusters}"
      )));
    }
    if inverted_lists.contains_key(&cluster) {
      return Err(SerializeError::InvalidStructure(format!(
        "duplicate IVF-PQ inverted list cluster {cluster}"
      )));
    }
    let list_length = read_u32_le(
      buffer,
      &mut offset,
      &format!("IVF-PQ inverted list {i} length"),
    )? as usize;

    ensure_count_bytes(
      buf_len,
      offset,
      list_length,
      8,
      &format!("IVF-PQ inverted list {i} data"),
    )?;
    let mut list = Vec::with_capacity(list_length);
    for _ in 0..list_length {
      let vector_id = read_u64_le(buffer, &mut offset, "IVF-PQ inverted list vector_id")?;
      if !list_ids.insert(vector_id) {
        return Err(SerializeError::InvalidStructure(format!(
          "duplicate IVF-PQ vector id {vector_id} in inverted lists"
        )));
      }
      list.push(vector_id);
    }
    inverted_lists.insert(cluster, list);
  }

  // PQ centroids
  let num_pq_subspaces = read_u32_le(buffer, &mut offset, "IVF-PQ PQ subspace count")? as usize;
  ensure_count_bytes(
    buf_len,
    offset,
    num_pq_subspaces,
    4,
    "IVF-PQ PQ subspace headers",
  )?;
  if num_pq_subspaces != num_subspaces {
    return Err(SerializeError::InvalidStructure(format!(
      "PQ subspace count {num_pq_subspaces} does not match configured {num_subspaces}"
    )));
  }

  let mut pq_centroids = Vec::with_capacity(num_pq_subspaces);
  for i in 0..num_pq_subspaces {
    let centroid_count = read_u32_le(
      buffer,
      &mut offset,
      &format!("IVF-PQ PQ subspace {i} centroid count"),
    )? as usize;

    ensure_count_bytes(
      buf_len,
      offset,
      centroid_count,
      4,
      &format!("IVF-PQ PQ subspace {i} centroids"),
    )?;
    let expected_centroid_count = num_centroids
      .checked_mul(dimensions / num_subspaces)
      .ok_or_else(|| SerializeError::InvalidStructure("PQ centroid shape overflow".into()))?;
    if centroid_count != expected_centroid_count {
      return Err(SerializeError::InvalidStructure(format!(
        "PQ subspace {i} centroid count {centroid_count} does not match expected {expected_centroid_count}"
      )));
    }
    let mut centroids = Vec::with_capacity(centroid_count);
    for _ in 0..centroid_count {
      let val = read_f32_le(buffer, &mut offset, "IVF-PQ PQ centroid")?;
      centroids.push(val);
    }
    pq_centroids.push(centroids);
  }

  // PQ codes
  let num_pq_codes = read_u32_le(buffer, &mut offset, "IVF-PQ PQ codes count")? as usize;
  ensure_count_bytes(buf_len, offset, num_pq_codes, 12, "IVF-PQ PQ code headers")?;

  let mut pq_codes: HashMap<u64, Vec<u8>> = HashMap::with_capacity(num_pq_codes);
  for i in 0..num_pq_codes {
    let vector_id = read_u64_le(
      buffer,
      &mut offset,
      &format!("IVF-PQ PQ code {i} vector_id"),
    )?;
    let code_len =
      read_u32_le(buffer, &mut offset, &format!("IVF-PQ PQ code {i} length"))? as usize;

    if code_len != num_subspaces {
      return Err(SerializeError::InvalidStructure(format!(
        "PQ code {i} length {code_len} does not match num_subspaces {num_subspaces}"
      )));
    }

    ensure_bytes(
      buf_len,
      offset,
      code_len,
      &format!("IVF-PQ PQ code {i} data"),
    )?;
    let codes = buffer[offset..offset + code_len].to_vec();
    offset += code_len;
    if codes.iter().any(|&code| usize::from(code) >= num_centroids) {
      return Err(SerializeError::InvalidStructure(format!(
        "PQ code {i} contains a centroid outside codebook size {num_centroids}"
      )));
    }
    if pq_codes.insert(vector_id, codes).is_some() {
      return Err(SerializeError::InvalidStructure(format!(
        "duplicate IVF-PQ code vector id {vector_id}"
      )));
    }
  }

  // Legacy writers appended centroid distances. Read and discard them when
  // the format flag is absent; current files omit this unused payload.
  let centroid_distances = if format_flags & IVFPQ_FORMAT_FLAG_NO_CENTROID_DISTANCES == 0 {
    let has_centroid_distances =
      match read_u8(buffer, &mut offset, "IVF-PQ centroid distances flag")? {
        0 => false,
        1 => true,
        value => {
          return Err(SerializeError::InvalidStructure(format!(
            "centroid distances flag {value} is invalid"
          )));
        }
      };

    if has_centroid_distances {
      let distance_count =
        read_u32_le(buffer, &mut offset, "IVF-PQ centroid distances count")? as usize;
      ensure_count_bytes(
        buf_len,
        offset,
        distance_count,
        4,
        "IVF-PQ centroid distances",
      )?;
      let expected_distance_count = n_clusters.checked_mul(n_clusters).ok_or_else(|| {
        SerializeError::InvalidStructure("centroid distance shape overflow".into())
      })?;
      if distance_count != expected_distance_count {
        return Err(SerializeError::InvalidStructure(format!(
          "centroid distance count {distance_count} does not match expected {expected_distance_count}"
        )));
      }
      for _ in 0..distance_count {
        let _ = read_f32_le(buffer, &mut offset, "IVF-PQ centroid distance")?;
      }
    }
    Some(Vec::new())
  } else {
    None
  };

  if offset != buf_len {
    return Err(SerializeError::InvalidStructure(format!(
      "IVF-PQ payload has {} trailing bytes",
      buf_len - offset
    )));
  }

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
  .map_err(|e| SerializeError::InvalidStructure(format!("IVF-PQ index construction: {e}")))
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
  use crate::vector::types::VectorStoreConfig;

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

  fn manual_metric_fixture(
    metric: DistanceMetric,
  ) -> (IvfPqIndex, VectorManifest, Vec<Vec<f32>>, Vec<f32>) {
    let config = IvfPqConfig {
      ivf: IvfConfig {
        n_clusters: 1,
        n_probe: 1,
        metric,
      },
      pq: PqConfig {
        num_subspaces: 4,
        num_centroids: 5,
        max_iterations: 1,
      },
      use_residuals: false,
    };
    let mut index = IvfPqIndex::new(4, config).expect("fixture config");
    index.ivf_centroids = vec![0.0; 4];
    index.pq_centroids = vec![
      vec![1.0, 0.8, 0.6, 0.0, -1.0],
      vec![0.0, 0.6, 0.8, 1.0, 0.0],
      vec![0.0, 0.0, 0.0, 0.0, 1.0],
      vec![0.0; 5],
    ];

    let vectors = vec![
      vec![1.0, 0.0, 0.0, 0.0],
      vec![0.8, 0.6, 0.0, 0.0],
      vec![0.6, 0.8, 0.0, 0.0],
      vec![-1.0, 0.0, 0.0, 0.0],
      vec![0.0, 0.0, 1.0, 0.0],
    ];
    let codes = [
      [0, 0, 0, 0],
      [1, 1, 0, 0],
      [2, 2, 0, 0],
      [4, 0, 0, 0],
      [3, 0, 1, 0],
    ];
    index.inverted_lists.insert(0, (1..=5).collect());
    for (offset, code) in codes.into_iter().enumerate() {
      index.pq_codes.insert((offset + 1) as u64, code.to_vec());
    }
    index.trained = true;

    let mut manifest = VectorManifest::new(VectorStoreConfig::new(4).with_metric(metric));
    for vector_id in 1..=5 {
      manifest.vector_to_node.insert(vector_id, vector_id);
      manifest.node_to_vector.insert(vector_id, vector_id);
    }

    (index, manifest, vectors, vec![1.0, 0.0, 0.0, 0.0])
  }

  fn exact_fixture_distances(
    metric: DistanceMetric,
    vectors: &[Vec<f32>],
    query: &[f32],
  ) -> Vec<(u64, f32)> {
    let query_normalized = if metric == DistanceMetric::Cosine {
      normalize(query)
    } else {
      query.to_vec()
    };
    let distance_fn = metric.distance_fn();
    let mut distances: Vec<(u64, f32)> = vectors
      .iter()
      .enumerate()
      .map(|(offset, vector)| {
        let vector_normalized = if metric == DistanceMetric::Cosine {
          normalize(vector)
        } else {
          vector.clone()
        };
        (
          (offset + 1) as u64,
          distance_fn(&query_normalized, &vector_normalized),
        )
      })
      .collect();
    distances.sort_by(|a, b| a.1.total_cmp(&b.1));
    distances
  }

  #[test]
  fn test_ivf_pq_adc_matches_exact_metric_space() {
    for metric in [
      DistanceMetric::Euclidean,
      DistanceMetric::Cosine,
      DistanceMetric::DotProduct,
    ] {
      let (index, manifest, vectors, query) = manual_metric_fixture(metric);
      let exact = exact_fixture_distances(metric, &vectors, &query);
      let expected_ids: HashSet<u64> = exact.iter().take(3).map(|(id, _)| *id).collect();
      let results = index.search(&manifest, &query, 3, None).expect("search");
      let actual_ids: HashSet<u64> = results.iter().map(|result| result.vector_id).collect();

      assert_eq!(
        expected_ids.intersection(&actual_ids).count(),
        3,
        "{metric:?}"
      );
      for result in &results {
        let (_, expected_distance) = exact
          .iter()
          .find(|(id, _)| *id == result.vector_id)
          .expect("exact result");
        assert!(
          (result.distance - expected_distance).abs() < 1e-5,
          "{metric:?}: ADC distance {} != exact {}",
          result.distance,
          expected_distance
        );
        assert!(
          (result.similarity - metric.distance_to_similarity(result.distance)).abs() < 1e-5,
          "{metric:?}: similarity is not derived from native distance"
        );
      }

      for pair in results.windows(2) {
        assert!(pair[0].distance <= pair[1].distance + 1e-5, "{metric:?}");
        assert!(
          pair[0].similarity + 1e-5 >= pair[1].similarity,
          "{metric:?}"
        );
      }

      match metric {
        DistanceMetric::Euclidean => {
          assert!(results.iter().all(|result| result.distance >= 0.0));
          assert!(results.iter().all(|result| result.similarity <= 1.0));
        }
        DistanceMetric::Cosine => {
          assert!(results.iter().all(|result| result.distance >= -1e-5));
          assert!(results
            .iter()
            .all(|result| result.similarity >= -1.0 - 1e-5));
          assert!(results.iter().all(|result| result.similarity <= 1.0 + 1e-5));
        }
        DistanceMetric::DotProduct => {
          assert!(results[0].distance < 0.0);
          assert!(results[0].similarity > 0.0);
          assert!(results
            .iter()
            .all(|result| { (result.distance + result.similarity).abs() < 1e-5 }));
        }
      }
    }
  }

  /// One cluster and one 2-centroid PQ subspace, so the first three vectors
  /// share a code and tie at ADC distance 1.0 from (1, 0). Their exact
  /// Euclidean distances are 0.1, 0.9 and 0.0. Node ids are 0..4. With
  /// `store_vectors` false the manifest carries only node mappings.
  fn rerank_fixture(store_vectors: bool) -> (IvfPqIndex, VectorManifest) {
    let config = IvfPqConfig {
      ivf: IvfConfig {
        n_clusters: 1,
        n_probe: 1,
        metric: DistanceMetric::Euclidean,
      },
      pq: PqConfig {
        num_subspaces: 1,
        num_centroids: 2,
        max_iterations: 1,
      },
      use_residuals: false,
    };
    let mut index = IvfPqIndex::new(2, config).expect("fixture config");
    index.ivf_centroids = vec![0.0, 0.0];
    index.pq_centroids = vec![vec![0.0, 0.0, 5.0, 5.0]];
    index.inverted_lists.insert(0, Vec::new());
    index.trained = true;

    let store_config = VectorStoreConfig::new(2).with_metric(DistanceMetric::Euclidean);
    let mut stored = VectorManifest::new(store_config.clone());
    let mut mappings_only = VectorManifest::new(store_config);
    for (node, vector) in [[0.9, 0.0], [0.1, 0.0], [1.0, 0.0], [5.0, 5.0]]
      .iter()
      .enumerate()
    {
      let vector_id =
        crate::vector::store::vector_store_insert(&mut stored, node as NodeId, vector)
          .expect("store insert");
      index.insert(vector_id, vector).expect("index insert");
      mappings_only
        .vector_to_node
        .insert(vector_id, node as NodeId);
      mappings_only
        .node_to_vector
        .insert(node as NodeId, vector_id);
    }
    (index, if store_vectors { stored } else { mappings_only })
  }

  fn hits(results: &[VectorSearchResult]) -> Vec<(NodeId, f32)> {
    results.iter().map(|r| (r.node_id, r.distance)).collect()
  }

  fn assert_hits(results: &[VectorSearchResult], expected: &[(NodeId, f32)]) {
    let got = hits(results);
    assert_eq!(got.len(), expected.len(), "{got:?}");
    for ((node, distance), (expected_node, expected_distance)) in got.iter().zip(expected) {
      assert_eq!(node, expected_node, "{got:?}");
      assert!((distance - expected_distance).abs() < 1e-5, "{got:?}");
    }
  }

  fn rerank_options(rerank_factor: Option<usize>, threshold: Option<f32>) -> IvfPqSearchOptions {
    IvfPqSearchOptions {
      threshold,
      rerank_factor,
      ..Default::default()
    }
  }

  #[test]
  fn test_ivf_pq_rerank_candidate_count() {
    assert_eq!(DEFAULT_RERANK_FACTOR, 4);
    assert_eq!(MIN_RERANK_CANDIDATES, 80);
    assert_eq!(rerank_candidates(10, None), Some(80));
    assert_eq!(rerank_candidates(1, None), Some(80));
    assert_eq!(rerank_candidates(50, None), Some(200));
    assert_eq!(rerank_candidates(50, Some(1)), Some(80));
    assert_eq!(rerank_candidates(100, Some(3)), Some(300));
    assert_eq!(rerank_candidates(10, Some(0)), None);
    assert_eq!(rerank_candidates(usize::MAX, Some(4)), Some(usize::MAX));
  }

  #[test]
  fn test_ivf_pq_search_reranks_by_exact_distance() {
    let (index, manifest) = rerank_fixture(true);
    let query = [1.0, 0.0];

    let exact = index.search(&manifest, &query, 2, None).expect("search");
    assert_hits(&exact, &[(2, 0.0), (0, 0.1)]);
    assert!((exact[0].similarity - 1.0).abs() < 1e-6);

    // Re-rank off: the ADC ranking, where the first three vectors tie.
    let adc = index
      .search(&manifest, &query, 2, Some(rerank_options(Some(0), None)))
      .expect("search");
    assert_eq!(adc.len(), 2);
    assert!(adc
      .iter()
      .all(|r| r.node_id <= 2 && (r.distance - 1.0).abs() < 1e-5));
  }

  #[test]
  fn test_ivf_pq_threshold_applies_to_reranked_distance() {
    let (index, manifest) = rerank_fixture(true);
    let query = [1.0, 0.0];
    // Euclidean similarity is 1 / (1 + d): only the exact match (d = 0)
    // clears 0.95; every ADC distance is 1.0 (similarity 0.5) or more.
    let exact = index
      .search(&manifest, &query, 3, Some(rerank_options(None, Some(0.95))))
      .expect("search");
    assert_hits(&exact, &[(2, 0.0)]);
    let adc = index
      .search(
        &manifest,
        &query,
        3,
        Some(rerank_options(Some(0), Some(0.95))),
      )
      .expect("search");
    assert!(adc.is_empty(), "{:?}", hits(&adc));
  }

  #[test]
  fn test_ivf_pq_search_multi_reranks_by_exact_distance() {
    let (index, manifest) = rerank_fixture(true);
    let (q1, q2) = ([1.0, 0.0], [1.1, 0.0]);
    let exact = index
      .search_multi(&manifest, &[&q1, &q2], 2, MultiQueryAggregation::Avg, None)
      .expect("search_multi");
    assert_hits(&exact, &[(2, 0.05), (0, 0.15)]);

    let adc = index
      .search_multi(
        &manifest,
        &[&q1, &q2],
        2,
        MultiQueryAggregation::Avg,
        Some(rerank_options(Some(0), None)),
      )
      .expect("search_multi");
    assert_eq!(adc.len(), 2);
    assert!(
      adc.iter().all(|r| (r.distance - 1.05).abs() < 1e-5),
      "{:?}",
      hits(&adc)
    );
  }

  #[test]
  fn test_ivf_pq_rerank_keeps_adc_distance_without_stored_vectors() {
    // A manifest with node mappings only (all IVF-PQ search needed before
    // the re-rank) still returns hits, at their ADC distances.
    let (index, manifest) = rerank_fixture(false);
    let query = [1.0, 0.0];
    let results = index.search(&manifest, &query, 2, None).expect("search");
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| (r.distance - 1.0).abs() < 1e-5));
    let multi = index
      .search_multi(&manifest, &[&query], 2, MultiQueryAggregation::Min, None)
      .expect("search_multi");
    assert_eq!(multi.len(), 2);
    assert!(multi.iter().all(|r| (r.distance - 1.0).abs() < 1e-5));
  }

  #[test]
  fn test_ivf_pq_cosine_training_uses_normalized_vectors() {
    let config = IvfPqConfig {
      ivf: IvfConfig {
        n_clusters: 1,
        n_probe: 1,
        metric: DistanceMetric::Cosine,
      },
      pq: PqConfig {
        num_subspaces: 1,
        num_centroids: 2,
        max_iterations: 5,
      },
      use_residuals: false,
    };
    let mut index = IvfPqIndex::new(2, config).expect("fixture config");
    index
      .add_training_vectors(&[10.0, 0.0, 0.0, 1.0], 2)
      .expect("training vectors");
    index.train().expect("cosine training");

    let centroid = &index.ivf_centroids;
    let centroid_norm = (centroid.iter().map(|value| value * value).sum::<f32>()).sqrt();
    assert!((centroid_norm - 1.0).abs() < 1e-5);
    assert!(index
      .pq_centroids
      .iter()
      .flatten()
      .all(|value| value.abs() <= 1.0 + 1e-5));
  }

  fn manual_multi_query_fixture() -> (IvfPqIndex, VectorManifest) {
    let config = IvfPqConfig {
      ivf: IvfConfig {
        n_clusters: 2,
        n_probe: 1,
        metric: DistanceMetric::Euclidean,
      },
      pq: PqConfig {
        num_subspaces: 1,
        num_centroids: 2,
        max_iterations: 1,
      },
      use_residuals: false,
    };
    let mut index = IvfPqIndex::new(2, config).expect("fixture config");
    index.ivf_centroids = vec![0.0, 0.0, 100.0, 0.0];
    index.pq_centroids = vec![vec![0.0, 0.0, 10.0, 0.0]];
    index.inverted_lists.insert(0, vec![1, 2]);
    index.inverted_lists.insert(1, vec![3, 4]);
    index.pq_codes.insert(1, vec![0]);
    index.pq_codes.insert(2, vec![0]);
    index.pq_codes.insert(3, vec![1]);
    index.pq_codes.insert(4, vec![1]);
    index.trained = true;

    let mut manifest = VectorManifest::new(VectorStoreConfig::new(2));
    for vector_id in 1..=4 {
      manifest.vector_to_node.insert(vector_id, vector_id);
      manifest.node_to_vector.insert(vector_id, vector_id);
    }
    (index, manifest)
  }

  #[test]
  fn test_ivf_pq_search_multi_honors_probe_and_filters_during_collection() {
    let (index, manifest) = manual_multi_query_fixture();
    let query = [0.0, 0.0];
    let filtered = index
      .search_multi(
        &manifest,
        &[&query],
        2,
        MultiQueryAggregation::Min,
        Some(IvfPqSearchOptions {
          n_probe: Some(2),
          filter: Some(Box::new(|node_id| node_id >= 3)),
          ..Default::default()
        }),
      )
      .expect("search_multi");
    assert_eq!(filtered.len(), 2);
    assert!(filtered.iter().all(|result| result.node_id >= 3));

    let not_probed = index
      .search_multi(
        &manifest,
        &[&query],
        2,
        MultiQueryAggregation::Min,
        Some(IvfPqSearchOptions {
          filter: Some(Box::new(|node_id| node_id >= 3)),
          ..Default::default()
        }),
      )
      .expect("search_multi");
    assert!(not_probed.is_empty());
  }

  #[test]
  fn test_ivf_pq_search_multi_scores_residual_candidates_against_their_cluster() {
    // Residual codebooks that reproduce A = (1, 1) in cluster 0 and
    // B = (9, 3) in cluster 1 exactly. With n_probe = 1, q1 only sees A and q2
    // only sees B, so each candidate's distance to the other query must use
    // its own cluster's residual table.
    let config = IvfPqConfig {
      ivf: IvfConfig {
        n_clusters: 2,
        n_probe: 1,
        metric: DistanceMetric::Euclidean,
      },
      pq: PqConfig {
        num_subspaces: 2,
        num_centroids: 2,
        max_iterations: 1,
      },
      use_residuals: true,
    };
    let index = IvfPqIndex::from_serialized(
      config,
      vec![0.0, 0.0, 10.0, 0.0],
      HashMap::from([(0, vec![1]), (1, vec![2])]),
      HashMap::from([(1, vec![0, 0]), (2, vec![1, 1])]),
      vec![vec![1.0, -1.0], vec![1.0, 3.0]],
      None,
      2,
      true,
    )
    .expect("hand-built residual index");
    let mut manifest = VectorManifest::new(VectorStoreConfig::new(2));
    for vector_id in [1, 2] {
      manifest.vector_to_node.insert(vector_id, vector_id);
      manifest.node_to_vector.insert(vector_id, vector_id);
    }

    let q1 = [0.0, 0.0];
    let q2 = [10.0, 0.0];
    let a = (2.0f32.sqrt(), 82.0f32.sqrt());
    let b = (90.0f32.sqrt(), 10.0f32.sqrt());
    for (aggregation, expected) in [
      (MultiQueryAggregation::Sum, [(1, a.0 + a.1), (2, b.0 + b.1)]),
      (MultiQueryAggregation::Max, [(1, a.1), (2, b.0)]),
    ] {
      let results = index
        .search_multi(&manifest, &[&q1, &q2], 2, aggregation, None)
        .expect("search_multi");
      let got: Vec<(u64, f32)> = results.iter().map(|r| (r.node_id, r.distance)).collect();
      assert_eq!(got.len(), 2, "{aggregation:?}: {got:?}");
      for ((node_id, distance), (expected_id, expected_distance)) in got.iter().zip(expected) {
        assert_eq!(*node_id, expected_id, "{aggregation:?}: {got:?}");
        assert!(
          (distance - expected_distance).abs() < 1e-4,
          "{aggregation:?}: {got:?}"
        );
      }
    }
  }

  #[test]
  fn test_ivf_pq_build_from_store_rejects_mismatched_manifest_before_training() {
    let mut manifest = VectorManifest::new(VectorStoreConfig::new(8).with_normalize(false));
    for node_id in 1..=4u64 {
      let vector: Vec<f32> = (0..8).map(|d| (node_id * 8 + d) as f32).collect();
      crate::vector::store::vector_store_insert(&mut manifest, node_id, &vector)
        .expect("store insert");
    }
    let config = IvfPqConfig::new()
      .with_n_clusters(1)
      .with_num_subspaces(2)
      .with_num_centroids(2);
    let mut index = IvfPqIndex::new(4, config).expect("config");

    assert!(matches!(
      index.build_from_store(&manifest),
      Err(IvfPqError::InvalidManifest(_))
    ));
    assert!(!index.trained);
  }

  #[test]
  fn test_ivf_pq_failed_train_preserves_training_buffer() {
    let config = IvfPqConfig {
      ivf: IvfConfig {
        n_clusters: 2,
        n_probe: 1,
        metric: DistanceMetric::Euclidean,
      },
      pq: PqConfig {
        num_subspaces: 2,
        num_centroids: 2,
        max_iterations: 3,
      },
      use_residuals: false,
    };
    let mut index = IvfPqIndex::new(2, config).expect("fixture config");
    index
      .add_training_vectors(&[1.0, 0.0], 1)
      .expect("first vector");

    assert!(matches!(
      index.train(),
      Err(IvfPqError::NotEnoughTrainingVectors { .. })
    ));
    assert_eq!(index.training_count, 1);
    assert_eq!(index.training_vectors.as_ref().expect("buffer").len(), 2);

    index
      .add_training_vectors(&[0.0, 1.0], 1)
      .expect("second vector");
    index.train().expect("retry training");
    assert!(index.trained);
  }

  #[test]
  fn test_ivf_pq_num_centroids_must_fit_uint8_codes() {
    for invalid in [0, 257] {
      let mut config = test_config();
      config.pq.num_centroids = invalid;
      let error = IvfPqIndex::new(16, config).expect_err("invalid centroid count");
      assert!(error.to_string().contains("1..=256"));
      assert!(error.to_string().contains(&invalid.to_string()));
    }
  }

  #[test]
  fn test_ivf_pq_missing_mapping_never_becomes_node_zero() {
    let config = IvfPqConfig {
      ivf: IvfConfig {
        n_clusters: 1,
        n_probe: 1,
        metric: DistanceMetric::Euclidean,
      },
      pq: PqConfig {
        num_subspaces: 1,
        num_centroids: 1,
        max_iterations: 1,
      },
      use_residuals: false,
    };
    let mut index = IvfPqIndex::new(2, config).expect("fixture config");
    index.ivf_centroids = vec![0.0, 0.0];
    index.pq_centroids = vec![vec![0.0, 0.0]];
    index.inverted_lists.insert(0, vec![99]);
    index.pq_codes.insert(99, vec![0]);
    index.trained = true;

    let manifest = VectorManifest::new(VectorStoreConfig::new(2));
    let results = index
      .search(
        &manifest,
        &[0.0, 0.0],
        1,
        Some(IvfPqSearchOptions {
          filter: Some(Box::new(|node_id| node_id == 0)),
          ..Default::default()
        }),
      )
      .expect("search");
    assert!(results.is_empty());
    assert!(results.iter().all(|result| result.node_id != 0));
  }

  #[test]
  fn test_ivf_pq_new() {
    let index = IvfPqIndex::new(16, test_config()).expect("expected value");
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
    let mut index = IvfPqIndex::new(16, test_config()).expect("expected value");

    let vectors = vec![0.0f32; 50 * 16];
    index
      .add_training_vectors(&vectors, 50)
      .expect("expected value");

    assert_eq!(index.training_count, 50);
  }

  #[test]
  fn test_ivf_pq_train() {
    let mut index = IvfPqIndex::new(16, test_config()).expect("expected value");

    // Create training vectors
    let mut vectors = Vec::new();
    for i in 0..500 {
      for d in 0..16 {
        vectors.push((i * 16 + d) as f32 / 8000.0);
      }
    }
    index
      .add_training_vectors(&vectors, 500)
      .expect("expected value");

    index.train().expect("expected value");

    assert!(index.trained);
    assert_eq!(index.ivf_centroids.len(), 4 * 16); // n_clusters * dimensions
  }

  #[test]
  fn test_ivf_pq_train_not_enough_vectors() {
    let mut index = IvfPqIndex::new(16, test_config()).expect("expected value");

    let vectors = vec![0.0f32; 2 * 16]; // Only 2 vectors, need at least 4 clusters
    index
      .add_training_vectors(&vectors, 2)
      .expect("expected value");

    let result = index.train();
    assert!(matches!(
      result,
      Err(IvfPqError::NotEnoughTrainingVectors { .. })
    ));
  }

  #[test]
  fn test_ivf_pq_insert() {
    let mut index = IvfPqIndex::new(16, test_config()).expect("expected value");

    // Train first
    let mut vectors = Vec::new();
    for i in 0..500 {
      for d in 0..16 {
        vectors.push((i * 16 + d) as f32 / 8000.0);
      }
    }
    index
      .add_training_vectors(&vectors, 500)
      .expect("expected value");
    index.train().expect("expected value");

    // Insert
    let vector = vec![0.5f32; 16];
    index.insert(0, &vector).expect("expected value");

    let stats = index.stats();
    assert_eq!(stats.total_vectors, 1);
    assert!(index.pq_codes.contains_key(&0));
  }

  #[test]
  fn test_ivf_pq_insert_not_trained() {
    let mut index = IvfPqIndex::new(16, test_config()).expect("expected value");

    let vector = vec![0.5f32; 16];
    let result = index.insert(0, &vector);

    assert!(matches!(result, Err(IvfPqError::NotTrained)));
  }

  #[test]
  fn test_ivf_pq_delete() {
    let mut index = IvfPqIndex::new(16, test_config()).expect("expected value");

    // Train
    let mut vectors = Vec::new();
    for i in 0..500 {
      for d in 0..16 {
        vectors.push((i * 16 + d) as f32 / 8000.0);
      }
    }
    index
      .add_training_vectors(&vectors, 500)
      .expect("expected value");
    index.train().expect("expected value");

    // Insert and delete
    let vector = vec![0.5f32; 16];
    index.insert(0, &vector).expect("expected value");
    assert!(index.delete(0, &vector).expect("delete"));
    assert!(!index.delete(0, &vector).expect("delete")); // Already deleted

    let stats = index.stats();
    assert_eq!(stats.total_vectors, 0);
  }

  #[test]
  fn test_ivf_pq_stats() {
    let mut index = IvfPqIndex::new(16, test_config()).expect("expected value");

    // Train
    let mut vectors = Vec::new();
    for i in 0..500 {
      for d in 0..16 {
        vectors.push((i * 16 + d) as f32 / 8000.0);
      }
    }
    index
      .add_training_vectors(&vectors, 500)
      .expect("expected value");
    index.train().expect("expected value");

    // Insert some vectors
    for i in 0..10 {
      let vector: Vec<f32> = (0..16).map(|d| (i * 16 + d) as f32 / 160.0).collect();
      index.insert(i as u64, &vector).expect("expected value");
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
    let mut index = IvfPqIndex::new(16, test_config()).expect("expected value");

    // Train
    let mut vectors = Vec::new();
    for i in 0..500 {
      for d in 0..16 {
        vectors.push((i * 16 + d) as f32 / 8000.0);
      }
    }
    index
      .add_training_vectors(&vectors, 500)
      .expect("expected value");
    index.train().expect("expected value");

    // Insert
    let vector = vec![0.5f32; 16];
    index.insert(0, &vector).expect("expected value");

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

    let mut index = IvfPqIndex::new(16, config).expect("expected value");

    // Train
    let mut vectors = Vec::new();
    for i in 0..500 {
      for d in 0..16 {
        vectors.push((i * 16 + d) as f32 / 8000.0);
      }
    }
    index
      .add_training_vectors(&vectors, 500)
      .expect("expected value");
    index.train().expect("expected value");

    assert!(index.trained);
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
    let index = IvfPqIndex::new(16, test_config()).expect("expected value");

    let serialized = serialize_ivf_pq(&index);
    let deserialized = deserialize_ivf_pq(&serialized).expect("expected value");

    assert_eq!(deserialized.dimensions, 16);
    assert_eq!(deserialized.config.ivf.n_clusters, 4);
    assert_eq!(deserialized.config.pq.num_subspaces, 4);
    assert!(!deserialized.trained);
  }

  #[test]
  fn test_ivf_pq_serialize_round_trip() {
    let mut index = IvfPqIndex::new(16, test_config()).expect("expected value");

    // Train
    let mut vectors = Vec::new();
    for i in 0..500 {
      for d in 0..16 {
        vectors.push((i * 16 + d) as f32 / 8000.0);
      }
    }
    index
      .add_training_vectors(&vectors, 500)
      .expect("expected value");
    index.train().expect("expected value");

    // Insert some vectors
    for i in 0..10 {
      let vector: Vec<f32> = (0..16).map(|d| (i * 16 + d) as f32 / 160.0).collect();
      index.insert(i as u64, &vector).expect("expected value");
    }

    let serialized = serialize_ivf_pq(&index);
    let deserialized = deserialize_ivf_pq(&serialized).expect("expected value");

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
    assert_eq!(serialized[31], IVFPQ_FORMAT_FLAG_NO_CENTROID_DISTANCES);

    // Check stats match
    let orig_stats = index.stats();
    let deser_stats = deserialized.stats();
    assert_eq!(orig_stats.total_vectors, deser_stats.total_vectors);
  }

  #[test]
  fn test_ivf_pq_reads_legacy_centroid_distance_payload() {
    let config = IvfPqConfig {
      ivf: IvfConfig {
        n_clusters: 1,
        n_probe: 1,
        metric: DistanceMetric::Euclidean,
      },
      pq: PqConfig {
        num_subspaces: 1,
        num_centroids: 1,
        max_iterations: 1,
      },
      use_residuals: false,
    };
    let mut index = IvfPqIndex::new(2, config).expect("fixture config");
    index.ivf_centroids = vec![0.0, 0.0];
    index.inverted_lists.insert(0, vec![7]);
    index.pq_codes.insert(7, vec![0]);
    index.trained = true;

    let current = serialize_ivf_pq(&index);
    assert_eq!(current[31], IVFPQ_FORMAT_FLAG_NO_CENTROID_DISTANCES);

    // Convert the current payload to the format written before the flag was
    // introduced: clear the flag and append the obsolete legacy payload.
    let mut legacy = current;
    legacy[31] = 0;
    legacy.push(1); // has_centroid_distances
    legacy.extend_from_slice(&1u32.to_le_bytes());
    legacy.extend_from_slice(&0.0f32.to_le_bytes());

    let restored = deserialize_ivf_pq(&legacy).expect("legacy payload");
    assert!(restored.trained);
    assert_eq!(restored.pq_codes.len(), 1);
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
  #[allow(clippy::type_complexity)]
  fn test_ivf_pq_corruption_matrix_returns_errors_without_panicking() {
    let index = IvfPqIndex::new(16, test_config()).expect("index");
    let valid = serialize_ivf_pq(&index);
    let mutations: [(&str, Box<dyn Fn(&mut Vec<u8>)>); 4] = [
      (
        "zero PQ subspaces in header",
        Box::new(|bytes| bytes[16..20].copy_from_slice(&0u32.to_le_bytes())),
      ),
      (
        "too many PQ centroids for uint8 codes",
        Box::new(|bytes| bytes[20..24].copy_from_slice(&257u32.to_le_bytes())),
      ),
      (
        "zero IVF clusters",
        Box::new(|bytes| bytes[8..12].copy_from_slice(&0u32.to_le_bytes())),
      ),
      (
        "PQ subspace count mismatch",
        Box::new(|bytes| bytes[56..60].copy_from_slice(&3u32.to_le_bytes())),
      ),
    ];

    for (name, mutate) in mutations {
      let mut corrupted = valid.clone();
      mutate(&mut corrupted);
      let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        deserialize_ivf_pq(&corrupted)
      }));
      assert!(result.is_ok(), "{name} panicked");
      assert!(result.expect("panic checked").is_err(), "{name} accepted");
    }

    for (name, field_offset) in [
      ("IVF centroid count", 48usize),
      ("inverted list count", 52usize),
      ("PQ subspace count", 56usize),
      ("PQ centroid count", 60usize),
    ] {
      let mut corrupted = valid.clone();
      corrupted[field_offset..field_offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
      let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        deserialize_ivf_pq(&corrupted)
      }));
      assert!(result.is_ok(), "{name} u32::MAX panicked");
      assert!(
        result.expect("panic checked").is_err(),
        "{name} u32::MAX accepted"
      );
    }

    for length in 0..valid.len() {
      let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        deserialize_ivf_pq(&valid[..length])
      }));
      assert!(result.is_ok(), "truncation at {length} panicked");
      assert!(
        result.expect("panic checked").is_err(),
        "truncation at {length} accepted"
      );
    }

    let mut trailing = valid.clone();
    trailing.push(0);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      deserialize_ivf_pq(&trailing)
    }));
    assert!(result.is_ok(), "trailing bytes panicked");
    assert!(
      result.expect("panic checked").is_err(),
      "trailing bytes accepted"
    );

    let mut populated_config = test_config();
    populated_config.ivf.n_clusters = 2;
    populated_config.ivf.n_probe = 1;
    populated_config.pq.num_subspaces = 2;
    populated_config.pq.num_centroids = 2;
    let mut populated = IvfPqIndex::new(4, populated_config).expect("index");
    populated.ivf_centroids = vec![1.0; 8];
    populated.inverted_lists.insert(0, vec![10]);
    populated.inverted_lists.insert(1, vec![20]);
    populated.pq_codes.insert(10, vec![0, 1]);
    populated.pq_codes.insert(20, vec![1, 0]);
    populated.trained = true;
    let valid = serialize_ivf_pq(&populated);
    let lists_offset = IVFPQ_HEADER_SIZE + 4 + populated.ivf_centroids.len() * 4 + 4;

    let mut duplicate_cluster = valid.clone();
    let first_cluster = u32::from_le_bytes(
      valid[lists_offset..lists_offset + 4]
        .try_into()
        .expect("cluster"),
    );
    duplicate_cluster[lists_offset + 16..lists_offset + 20]
      .copy_from_slice(&first_cluster.to_le_bytes());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      deserialize_ivf_pq(&duplicate_cluster)
    }));
    assert!(result.is_ok(), "duplicate cluster panicked");
    assert!(
      result.expect("panic checked").is_err(),
      "duplicate cluster accepted"
    );

    let mut duplicate_id = valid.clone();
    let first_id = u64::from_le_bytes(
      valid[lists_offset + 8..lists_offset + 16]
        .try_into()
        .expect("id"),
    );
    duplicate_id[lists_offset + 24..lists_offset + 32].copy_from_slice(&first_id.to_le_bytes());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      deserialize_ivf_pq(&duplicate_id)
    }));
    assert!(result.is_ok(), "duplicate vector ID panicked");
    assert!(
      result.expect("panic checked").is_err(),
      "duplicate vector ID accepted"
    );

    let mut code_length_mismatch = valid.clone();
    let pq_codes_offset = lists_offset + 2 * 16 + 4 + 2 * (4 + 4 * 4);
    code_length_mismatch[pq_codes_offset + 4 + 8..pq_codes_offset + 4 + 12]
      .copy_from_slice(&0u32.to_le_bytes());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      deserialize_ivf_pq(&code_length_mismatch)
    }));
    assert!(result.is_ok(), "code length mismatch panicked");
    assert!(
      result.expect("panic checked").is_err(),
      "code length mismatch accepted"
    );

    let mut list_code_mismatch = valid.clone();
    list_code_mismatch[lists_offset + 8..lists_offset + 16].copy_from_slice(&99u64.to_le_bytes());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      deserialize_ivf_pq(&list_code_mismatch)
    }));
    assert!(result.is_ok(), "list/code mismatch panicked");
    assert!(
      result.expect("panic checked").is_err(),
      "list/code mismatch accepted"
    );
  }

  #[test]
  fn test_ivf_pq_serialized_size() {
    let mut index = IvfPqIndex::new(16, test_config()).expect("expected value");

    // Train
    let mut vectors = Vec::new();
    for i in 0..500 {
      for d in 0..16 {
        vectors.push((i * 16 + d) as f32 / 8000.0);
      }
    }
    index
      .add_training_vectors(&vectors, 500)
      .expect("expected value");
    index.train().expect("expected value");

    // Insert some vectors
    for i in 0..5 {
      let vector: Vec<f32> = (0..16).map(|d| (i * 16 + d) as f32 / 80.0).collect();
      index.insert(i as u64, &vector).expect("expected value");
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
    let mut index = IvfPqIndex::new(16, test_config()).expect("expected value");

    // Train
    let mut vectors = Vec::new();
    for i in 0..500 {
      for d in 0..16 {
        vectors.push((i * 16 + d) as f32 / 8000.0);
      }
    }
    index
      .add_training_vectors(&vectors, 500)
      .expect("expected value");
    index.train().expect("expected value");

    // Create a minimal manifest
    let config = crate::vector::types::VectorStoreConfig::new(16);
    let manifest = crate::vector::types::VectorManifest::new(config);

    // Empty queries should return empty results
    let results = index
      .search_multi(&manifest, &[], 5, MultiQueryAggregation::Min, None)
      .expect("search_multi");
    assert!(results.is_empty());
  }

  #[test]
  fn test_ivf_pq_search_multi_not_trained() {
    let index = IvfPqIndex::new(16, test_config()).expect("expected value");
    let config = crate::vector::types::VectorStoreConfig::new(16);
    let manifest = crate::vector::types::VectorManifest::new(config);

    let query = vec![0.5f32; 16];
    let results = index
      .search_multi(&manifest, &[&query], 5, MultiQueryAggregation::Min, None)
      .expect("search_multi");
    assert!(results.is_empty());
  }
}
