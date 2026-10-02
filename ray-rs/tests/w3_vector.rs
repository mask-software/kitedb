// Wave-3 lane `vector`: failing reproductions for findings B1-B12.
//
// B1  plain IVF keeps raw-mean cosine centroids (probe biased by centroid norm)
// B2  IvfIndex search maps a missing node mapping to node 0 and skips the filter
// B3  a failed IvfIndex::train drops the training buffer but keeps its count
// B5  VectorIndex never retrains its ANN index as the corpus grows
// B7  brute-force sort is not a total order with NaN distances (panics)
// B9  normalization overflow/underflow; a NaN at the heap root freezes top-k
// B10 config validation: zero clusters, zero row groups, k=0, n_probe=0,
//     prime dimensions collapsing PQ to one subspace
// B11 VectorStoreConfig::with_metric keeps normalize_on_insert for every metric
// B12 compaction leaves a manifest that fails validation on reload
//
// Perf-only findings (B4, B6, B7 perf, B8) have no test here.

use std::collections::{HashMap, HashSet};
use std::panic::{self, AssertUnwindSafe};

use kitedb::api::vector_search::{AnnAlgorithm, SimilarOptions, VectorIndex, VectorIndexOptions};
use kitedb::vector::compaction::{
  clear_deleted_fragments, force_full_compaction, run_compaction_if_needed, CompactionStrategy,
};
use kitedb::vector::ivf::serialize::validate_manifest_for_serialization;
use kitedb::vector::ivf::{deserialize_manifest, kmeans, kmeans_parallel, serialize_manifest};
use kitedb::vector::{
  create_vector_store, normalize, vector_store_delete, vector_store_insert,
  vector_store_node_vector, DistanceMetric, IvfConfig, IvfError, IvfIndex, IvfPqConfig, IvfPqIndex,
  IvfPqSearchOptions, KMeansConfig, MultiQueryAggregation, PqConfig, PqIndex, SearchOptions,
  VectorManifest, VectorSearchResult, VectorStoreConfig,
};

// ============================================================================
// Helpers
// ============================================================================

/// SplitMix64: deterministic test data.
struct Rng(u64);

impl Rng {
  fn new(seed: u64) -> Self {
    Self(seed)
  }

  fn next_u64(&mut self) -> u64 {
    self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = self.0;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
  }

  /// Uniform in [0, 1).
  fn unit(&mut self) -> f32 {
    (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
  }

  /// Standard normal (Box-Muller).
  fn gaussian(&mut self) -> f32 {
    let u1 = self.unit().max(f32::MIN_POSITIVE);
    let u2 = self.unit();
    (-2.0 * u1.ln()).sqrt() * (std::f32::consts::TAU * u2).cos()
  }

  fn gaussian_vec(&mut self, len: usize) -> Vec<f32> {
    (0..len).map(|_| self.gaussian()).collect()
  }
}

fn l2(v: &[f32]) -> f32 {
  v.iter().map(|x| x * x).sum::<f32>().sqrt()
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
  if let Some(msg) = payload.downcast_ref::<String>() {
    msg.clone()
  } else if let Some(msg) = payload.downcast_ref::<&str>() {
    (*msg).to_string()
  } else {
    "<non-string panic payload>".to_string()
  }
}

/// Runs `f`, turning a panic into a test failure that names the entry point.
fn no_panic<R>(what: &str, f: impl FnOnce() -> R) -> R {
  match panic::catch_unwind(AssertUnwindSafe(f)) {
    Ok(value) => value,
    Err(payload) => panic!(
      "{what}: panicked ({}) instead of returning a result",
      panic_message(payload.as_ref())
    ),
  }
}

/// `IvfIndex::new` returns `Self` today; a fix may make it fallible. Either
/// signature compiles here, and a constructor `Err` counts as a rejection.
trait IvfCtor {
  fn into_result(self) -> Result<IvfIndex, String>;
}

impl IvfCtor for IvfIndex {
  fn into_result(self) -> Result<IvfIndex, String> {
    Ok(self)
  }
}

impl<E: std::fmt::Debug> IvfCtor for Result<IvfIndex, E> {
  fn into_result(self) -> Result<IvfIndex, String> {
    self.map_err(|err| format!("{err:?}"))
  }
}

fn new_ivf(dims: usize, config: IvfConfig) -> Result<IvfIndex, String> {
  IvfIndex::new(dims, config).into_result()
}

fn euclidean_store(dims: usize) -> VectorManifest {
  create_vector_store(
    VectorStoreConfig::new(dims)
      .with_metric(DistanceMetric::Euclidean)
      .with_normalize(false),
  )
}

/// Inserts `vectors` for nodes `0..n` into `manifest` and an IVF index
/// trained on the same vectors.
fn trained_ivf(manifest: &mut VectorManifest, config: IvfConfig, vectors: &[Vec<f32>]) -> IvfIndex {
  let dims = manifest.config.dimensions;
  let mut index = new_ivf(dims, config).expect("fixture config is valid");
  let flat: Vec<f32> = vectors.iter().flatten().copied().collect();
  index
    .add_training_vectors(&flat, vectors.len())
    .expect("fixture training vectors");
  index.train().expect("fixture training");
  for (node, vector) in vectors.iter().enumerate() {
    let vector_id = vector_store_insert(manifest, node as u64, vector).expect("fixture insert");
    let stored = vector_store_node_vector(manifest, node as u64)
      .expect("fixture vector")
      .to_vec();
    index
      .insert(vector_id, &stored)
      .expect("fixture index insert");
  }
  index
}

fn node_ids(hits: &[VectorSearchResult]) -> Vec<u64> {
  hits.iter().map(|hit| hit.node_id).collect()
}

// ============================================================================
// B1: plain IVF never normalizes cosine centroids
// ============================================================================

fn unit_circle(degrees: f32) -> Vec<f32> {
  let radians = degrees.to_radians();
  vec![radians.cos(), radians.sin()]
}

/// Contract: like IVF-PQ, a trained cosine IVF index stores unit centroids,
/// because insert/search/delete compare unit vectors against them with
/// `1 - dot`.
#[test]
fn w3_b1_cosine_ivf_centroids_are_unit_length() {
  let dims = 8;
  let n = 512;
  let mut rng = Rng::new(0xB1);
  let data = rng.gaussian_vec(n * dims);

  let mut index =
    new_ivf(dims, IvfConfig::new(4).with_metric(DistanceMetric::Cosine)).expect("valid config");
  index.add_training_vectors(&data, n).expect("training data");
  index.train().expect("train");

  let norms: Vec<f32> = index.centroids.chunks_exact(dims).map(l2).collect();
  assert!(
    norms.iter().all(|norm| (norm - 1.0).abs() < 1e-3),
    "cosine IVF centroids must be unit length (IVF-PQ normalizes them); norms = {norms:?}"
  );
}

/// Contract: with n_probe=1, a cosine query probes the cluster whose
/// direction is nearest, regardless of how spread out that cluster is.
///
/// Cluster A: 100 directions within +-0.5 deg of 0 deg (raw-mean norm ~1.0).
/// Cluster B: 100 directions spread over 90..180 deg (raw-mean norm ~0.90 at
/// 135 deg). With unit centroids the probe boundary is 67.5 deg; with raw
/// means `1 - dot` favors the tighter cluster and moves it to ~68.75 deg. A
/// query at 68.1 deg is nearest to B's 90 deg edge (22 deg away, versus
/// 67.6 deg for anything in A), but the biased probe only searches A.
#[test]
fn w3_b1_cosine_ivf_probe_is_not_biased_by_cluster_spread() {
  let mut manifest = create_vector_store(VectorStoreConfig::new(2));
  let mut vectors = Vec::new();
  for i in 0..100 {
    vectors.push(unit_circle(-0.5 + i as f32 / 99.0));
  }
  for i in 0..100 {
    vectors.push(unit_circle(90.0 + 90.0 * i as f32 / 99.0));
  }
  let b_edge_node = 100; // the B vector at exactly 90 deg

  let index = trained_ivf(
    &mut manifest,
    IvfConfig::new(2)
      .with_n_probe(1)
      .with_metric(DistanceMetric::Cosine),
    &vectors,
  );

  let hits = index
    .search(&manifest, &unit_circle(68.1), 1, None)
    .expect("search");
  let centroid_norms: Vec<f32> = index
    .centroids
    .as_chunks::<2>()
    .0
    .iter()
    .map(|c| l2(c))
    .collect();
  assert_eq!(
    node_ids(&hits),
    vec![b_edge_node],
    "n_probe=1 must probe the cluster nearest in direction; got {hits:?} \
     (centroid norms {centroid_norms:?}, centroids {:?})",
    index.centroids
  );
}

// ============================================================================
// B2: IvfIndex search maps a missing node mapping to node 0
// ============================================================================

/// One indexed vector whose `vector_to_node` entry is missing while its data
/// is still reachable through `vector_locations`.
fn b2_fixture() -> (IvfIndex, VectorManifest) {
  let mut manifest = euclidean_store(2);
  let vector_id = vector_store_insert(&mut manifest, 7, &[1.0, 0.0]).expect("insert");
  manifest.vector_to_node.remove(&vector_id);

  let mut inverted_lists = HashMap::new();
  inverted_lists.insert(0, vec![vector_id]);
  let index = IvfIndex::from_serialized(
    IvfConfig::new(1)
      .with_n_probe(1)
      .with_metric(DistanceMetric::Euclidean),
    vec![0.0, 0.0],
    inverted_lists,
    2,
    true,
  );
  (index, manifest)
}

/// Contract (matches IVF-PQ): an unmappable vector is skipped. It never
/// becomes node 0 and never bypasses the filter.
#[test]
fn w3_b2_ivf_search_skips_vectors_without_node_mapping() {
  let (index, manifest) = b2_fixture();

  let filtered = index
    .search(
      &manifest,
      &[1.0, 0.0],
      5,
      Some(SearchOptions {
        filter: Some(Box::new(|_| false)),
        ..Default::default()
      }),
    )
    .expect("search");
  assert!(
    filtered.is_empty(),
    "a reject-all filter returned {filtered:?}: the filter is bypassed when the node mapping is missing"
  );

  let unfiltered = index
    .search(&manifest, &[1.0, 0.0], 5, None)
    .expect("search");
  assert!(
    unfiltered.is_empty(),
    "a vector without a node mapping was returned as {unfiltered:?} (node 0 via unwrap_or(0))"
  );
}

#[test]
fn w3_b2_ivf_search_multi_skips_vectors_without_node_mapping() {
  let (index, manifest) = b2_fixture();
  let query = [1.0f32, 0.0];
  let hits = index
    .search_multi(
      &manifest,
      &[&query[..], &query[..]],
      5,
      MultiQueryAggregation::Min,
      None,
    )
    .expect("search_multi");
  assert!(
    hits.is_empty(),
    "search_multi returned an unmappable vector as {hits:?} (node 0 via unwrap_or(0))"
  );
}

// ============================================================================
// B3: a failed IvfIndex::train loses the training buffer
// ============================================================================

/// Contract (matches IVF-PQ): a failed train leaves the buffered vectors in
/// place, so adding more and retrying works.
#[test]
fn w3_b3_failed_ivf_train_keeps_training_buffer() {
  let dims = 4;
  let mut rng = Rng::new(0xB3);
  let mut index = new_ivf(
    dims,
    IvfConfig::new(200).with_metric(DistanceMetric::Euclidean),
  )
  .expect("valid config");

  index
    .add_training_vectors(&rng.gaussian_vec(100 * dims), 100)
    .expect("first batch");
  let first = index.train();
  assert!(
    matches!(
      first,
      Err(IvfError::NotEnoughTrainingVectors { n: 100, k: 200 })
    ),
    "fixture: 100 vectors cannot train 200 clusters, got {first:?}"
  );

  index
    .add_training_vectors(&rng.gaussian_vec(200 * dims), 200)
    .expect("second batch");
  if let Err(err) = index.train() {
    panic!("retry with 300 buffered vectors for 200 clusters must train, got {err:?}");
  }
  assert!(index.trained);
}

// ============================================================================
// B5: the ANN index is never retrained as data grows
// ============================================================================

fn b5_check(algorithm: AnnAlgorithm) {
  let dims = 8;
  let mut rng = Rng::new(0xB5);
  let mut index = VectorIndex::new(
    VectorIndexOptions::new(dims)
      .with_metric(DistanceMetric::Euclidean)
      .with_ann_algorithm(algorithm)
      .with_training_threshold(256),
  );
  let query = rng.gaussian_vec(dims);

  for node in 0..256u64 {
    index.set(node, &rng.gaussian_vec(dims)).expect("set");
  }
  index
    .search(&query, SimilarOptions::new(5))
    .expect("first search");
  let first = index.stats().index_clusters;
  assert_eq!(
    first,
    Some(16),
    "{algorithm:?} fixture: the first build uses sqrt(256) = 16 clusters"
  );

  for node in 256..4096u64 {
    index.set(node, &rng.gaussian_vec(dims)).expect("set");
  }
  index
    .search(&query, SimilarOptions::new(5))
    .expect("second search");
  let after_growth = index.stats().index_clusters;
  assert!(
    after_growth.is_some_and(|clusters| clusters > 16),
    "{algorithm:?}: live vectors grew 16x (256 -> 4096) since training, but the ANN index \
     still has {after_growth:?} clusters (a retrain would use sqrt(4096) = 64)"
  );
}

/// Contract: once the live count has grown by a large factor since the last
/// training, the next search retrains (sqrt-rule clusters for the new size).
#[test]
fn w3_b5_ivf_index_retrains_after_growth() {
  b5_check(AnnAlgorithm::Ivf);
}

#[test]
fn w3_b5_ivf_pq_index_retrains_after_growth() {
  b5_check(AnnAlgorithm::IvfPq);
}

// ============================================================================
// B7: brute-force sort is not a total order with NaN distances
// ============================================================================

/// Contract: NaN distances neither panic the sort nor displace or reorder the
/// finite results.
///
/// Inputs are finite, but with the dot-product metric
/// `[1e20, 1e20] . [1e20, -1e20]` is `inf + -inf = NaN`. On Rust >= 1.81,
/// `sort_by(partial_cmp().unwrap_or(Equal))` detects the broken total order
/// and panics for this mix (~499/500 random orders in a scratch run); the
/// rest come back misordered.
#[test]
fn w3_b7_brute_force_nan_distances_do_not_panic_or_misorder() {
  let dims = 4;
  // Below the default training threshold, so search is brute force.
  let mut index =
    VectorIndex::new(VectorIndexOptions::new(dims).with_metric(DistanceMetric::DotProduct));
  // 48 finite scores: dot(q, v) = 1e20 * (1.5 + i), so higher i ranks first.
  for i in 0..48u64 {
    index
      .set(i, &[1.0 + i as f32, 0.5, 0.0, 0.0])
      .expect("finite vector");
  }
  // 16 vectors whose dot with the query overflows to NaN.
  for j in 0..16u64 {
    index
      .set(1000 + j, &[1e20, -1e20 * (1.0 + j as f32 * 0.01), 0.0, 0.0])
      .expect("finite vector");
  }

  let query = [1e20f32, 1e20, 0.0, 0.0];
  let hits = no_panic("brute-force VectorIndex::search with NaN distances", || {
    index.search(&query, SimilarOptions::new(10))
  })
  .expect("search");

  let got: Vec<u64> = hits.iter().map(|hit| hit.node_id).collect();
  let expected: Vec<u64> = (38..48u64).rev().collect();
  assert_eq!(
    got, expected,
    "top-10 must be the 10 best finite scores in order; NaN distances must not displace them"
  );
}

// ============================================================================
// B9: normalization overflow/underflow; NaN at the top-k heap root
// ============================================================================

fn assert_unit_direction(label: &str, got: &[f32], expected: &[f32]) {
  let ok = got.len() == expected.len()
    && got
      .iter()
      .zip(expected)
      .all(|(g, e)| g.is_finite() && (g - e).abs() < 1e-5);
  assert!(ok, "{label}: expected {expected:?}, got {got:?}");
}

/// Contract: normalizing a finite, nonzero vector yields its unit direction.
/// Today the sum of squares overflows to inf, inv_norm becomes 0, and the
/// vector silently becomes all zeros.
#[test]
fn w3_b9_normalize_huge_components_keeps_direction() {
  assert_unit_direction(
    "normalize([3e20, 4e20])",
    &normalize(&[3e20, 4e20]),
    &[0.6, 0.8],
  );
}

/// Today the squares underflow to 0 (and any norm below 1e-10 is skipped), so
/// the vector is returned unnormalized.
#[test]
fn w3_b9_normalize_tiny_components_reaches_unit_length() {
  assert_unit_direction(
    "normalize([3e-25, 4e-25])",
    &normalize(&[3e-25, 4e-25]),
    &[0.6, 0.8],
  );
}

/// Contract for a normalizing cosine index: a finite, nonzero vector is either
/// rejected or stored as its unit direction, never as a silently wrong one.
fn b9_cosine_index_check(label: &str, vector: [f32; 4]) {
  let mut index = VectorIndex::new(VectorIndexOptions::new(4));
  index.set(2, &[1.0, 1.0, 0.0, 0.0]).expect("set");
  if index.set(1, &vector).is_err() {
    return; // Rejecting the vector is an acceptable fix.
  }

  let stored = index.get(1).expect("stored vector");
  assert!(
    (l2(&stored) - 1.0).abs() < 1e-3,
    "{label}: {vector:?} was stored as {stored:?}, not as a unit vector"
  );
  let hits = index
    .search(&[1.0, 0.0, 0.0, 0.0], SimilarOptions::new(2))
    .expect("search");
  assert_eq!(
    hits.first().map(|hit| hit.node_id),
    Some(1),
    "{label}: node 1 points exactly along the query, got {hits:?}"
  );
}

#[test]
fn w3_b9_cosine_index_huge_vector_keeps_direction() {
  // 2e19^2 = 4e38 > f32::MAX.
  b9_cosine_index_check("huge", [2e19, 0.0, 0.0, 0.0]);
}

#[test]
fn w3_b9_cosine_index_tiny_vector_keeps_direction() {
  b9_cosine_index_check("tiny", [1e-30, 0.0, 0.0, 0.0]);
}

/// Contract: a NaN distance never freezes the top-k heap.
///
/// One cluster, insertion order fixed: the first candidate scores NaN
/// (`inf + -inf` in the dot product). Pushed first, it sits at the max-heap
/// root, and `dist < NaN` is always false, so no later candidate can enter.
#[test]
fn w3_b9_nan_distance_does_not_freeze_ivf_top_k() {
  let dims = 4;
  let mut manifest = create_vector_store(
    VectorStoreConfig::new(dims)
      .with_metric(DistanceMetric::DotProduct)
      .with_normalize(false),
  );
  let mut index = new_ivf(
    dims,
    IvfConfig::new(1)
      .with_n_probe(1)
      .with_metric(DistanceMetric::DotProduct),
  )
  .expect("valid config");
  let training: Vec<f32> = (1..=4).flat_map(|i| [i as f32, 0.0, 0.0, 0.0]).collect();
  index.add_training_vectors(&training, 4).expect("training");
  index.train().expect("train");

  let mut insert = |node: u64, vector: [f32; 4]| {
    let vector_id = vector_store_insert(&mut manifest, node, &vector).expect("insert");
    index.insert(vector_id, &vector).expect("index insert");
  };
  insert(100, [1e20, -1e20, 0.0, 0.0]); // NaN against the query
  for i in 1..=4u64 {
    insert(i, [i as f32, 0.0, 0.0, 0.0]); // score 1e20 * i
  }

  let hits = index
    .search(&manifest, &[1e20, 1e20, 0.0, 0.0], 2, None)
    .expect("search");
  assert_eq!(
    node_ids(&hits),
    vec![4, 3],
    "top-2 must be the two best finite scores; got {hits:?}"
  );
}

// ============================================================================
// B10: config validation
// ============================================================================

fn b10_ivf_zero_clusters(training_vectors: usize) {
  let dims = 4;
  let mut rng = Rng::new(0xB10);
  let data = rng.gaussian_vec(training_vectors * dims);
  let outcome = no_panic("IvfConfig::new(0) build + train", || {
    let mut index = new_ivf(dims, IvfConfig::new(0))?;
    index
      .add_training_vectors(&data, training_vectors)
      .map_err(|err| err.to_string())?;
    index.train().map_err(|err| err.to_string())
  });
  assert!(
    outcome.is_err(),
    "an IVF index with zero clusters must be rejected, got {outcome:?}"
  );
}

#[test]
fn w3_b10_ivf_zero_clusters_without_vectors_returns_err() {
  b10_ivf_zero_clusters(0);
}

#[test]
fn w3_b10_ivf_zero_clusters_with_vectors_returns_err() {
  b10_ivf_zero_clusters(10);
}

#[test]
fn w3_b10_kmeans_zero_clusters_returns_err() {
  let distance_fn = DistanceMetric::Euclidean.distance_fn();
  let data = [1.0f32; 40];
  for (label, vectors, n) in [("n=0", &data[..0], 0), ("n=10", &data[..], 10)] {
    let sequential = no_panic(&format!("kmeans(k=0, {label})"), || {
      kmeans(vectors, n, 4, &KMeansConfig::new(0), distance_fn)
    });
    assert!(sequential.is_err(), "kmeans(k=0, {label}) must return Err");
    let parallel = no_panic(&format!("kmeans_parallel(k=0, {label})"), || {
      kmeans_parallel(vectors, n, 4, &KMeansConfig::new(0), distance_fn)
    });
    assert!(
      parallel.is_err(),
      "kmeans_parallel(k=0, {label}) must return Err"
    );
  }
}

/// Contract: row_group_size=0 is rejected (at set) or made to work; reads
/// never divide by zero.
#[test]
fn w3_b10_row_group_size_zero_does_not_divide_by_zero() {
  let mut index = VectorIndex::new(VectorIndexOptions::new(4).with_row_group_size(0));
  let vector = [1.0f32, 2.0, 3.0, 4.0];
  let set = no_panic("VectorIndex::set with row_group_size=0", || {
    index.set(1, &vector)
  });
  if set.is_err() {
    return; // Rejecting the config is an acceptable fix.
  }
  let stored = no_panic("VectorIndex::get with row_group_size=0", || index.get(1));
  assert!(stored.is_some(), "the stored vector must be readable");
  let hits = no_panic("VectorIndex::search with row_group_size=0", || {
    index.search(&vector, SimilarOptions::new(1))
  })
  .expect("search");
  assert_eq!(hits.first().map(|hit| hit.node_id), Some(1));
}

/// Contract: k=0 returns no results (or an error); it never indexes into an
/// empty result list.
#[test]
fn w3_b10_pq_search_k_zero_does_not_panic() {
  let dims = 4;
  let n = 32;
  let mut rng = Rng::new(0xB10);
  let data = rng.gaussian_vec(n * dims);
  let mut pq = PqIndex::new(
    dims,
    PqConfig {
      num_subspaces: 2,
      num_centroids: 4,
      max_iterations: 5,
    },
  )
  .expect("valid config");
  pq.train(&data, n).expect("train");
  pq.encode(&data, n).expect("encode");

  let outcome = no_panic("PqIndex::search(k=0)", || pq.search(&data[..dims], 0, None));
  if let Ok(hits) = outcome {
    assert!(hits.is_empty(), "k=0 returned {} results", hits.len());
  }
}

fn b10_vectors(n: usize, dims: usize) -> Vec<Vec<f32>> {
  let mut rng = Rng::new(0xB10);
  (0..n).map(|_| rng.gaussian_vec(dims)).collect()
}

/// Contract: n_probe=0 is rejected or treated as at least one probe; it never
/// silently returns nothing from a populated index.
#[test]
fn w3_b10_ivf_search_n_probe_zero_is_not_silently_empty() {
  let vectors = b10_vectors(20, 4);
  let mut manifest = euclidean_store(4);
  let index = trained_ivf(
    &mut manifest,
    IvfConfig::new(2)
      .with_n_probe(1)
      .with_metric(DistanceMetric::Euclidean),
    &vectors,
  );

  let result = index.search(
    &manifest,
    &vectors[0],
    5,
    Some(SearchOptions {
      n_probe: Some(0),
      ..Default::default()
    }),
  );
  if let Ok(hits) = result {
    assert!(
      !hits.is_empty(),
      "IvfIndex::search with n_probe=0 silently returned no results"
    );
  }
}

/// Builds, trains, fills, and searches an IVF index configured with
/// n_probe=0. Any step may reject the config.
fn b10_search_with_config_n_probe_zero(
  vectors: &[Vec<f32>],
) -> Result<Vec<VectorSearchResult>, String> {
  let dims = vectors[0].len();
  let mut manifest = euclidean_store(dims);
  let mut index = new_ivf(
    dims,
    IvfConfig::new(2)
      .with_n_probe(0)
      .with_metric(DistanceMetric::Euclidean),
  )?;
  let flat: Vec<f32> = vectors.iter().flatten().copied().collect();
  index
    .add_training_vectors(&flat, vectors.len())
    .map_err(|err| err.to_string())?;
  index.train().map_err(|err| err.to_string())?;
  for (node, vector) in vectors.iter().enumerate() {
    let vector_id = vector_store_insert(&mut manifest, node as u64, vector).expect("insert");
    index
      .insert(vector_id, vector)
      .map_err(|err| err.to_string())?;
  }
  index
    .search(&manifest, &vectors[0], 5, None)
    .map_err(|err| err.to_string())
}

#[test]
fn w3_b10_ivf_config_n_probe_zero_is_not_silently_empty() {
  let vectors = b10_vectors(20, 4);
  if let Ok(hits) = b10_search_with_config_n_probe_zero(&vectors) {
    assert!(
      !hits.is_empty(),
      "an IVF index configured with n_probe=0 silently returned no results"
    );
  }
}

#[test]
fn w3_b10_ivf_pq_search_n_probe_zero_is_not_silently_empty() {
  let dims = 4;
  let vectors = b10_vectors(20, dims);
  let mut manifest = euclidean_store(dims);
  let mut index = IvfPqIndex::new(
    dims,
    IvfPqConfig::new()
      .with_n_clusters(2)
      .with_n_probe(1)
      .with_metric(DistanceMetric::Euclidean)
      .with_num_subspaces(2)
      .with_num_centroids(4)
      .with_residuals(false),
  )
  .expect("valid config");
  let flat: Vec<f32> = vectors.iter().flatten().copied().collect();
  index
    .add_training_vectors(&flat, vectors.len())
    .expect("training");
  index.train().expect("train");
  for (node, vector) in vectors.iter().enumerate() {
    let vector_id = vector_store_insert(&mut manifest, node as u64, vector).expect("insert");
    index.insert(vector_id, vector).expect("index insert");
  }

  let result = index.search(
    &manifest,
    &vectors[0],
    5,
    Some(IvfPqSearchOptions {
      n_probe: Some(0),
      ..Default::default()
    }),
  );
  if let Ok(hits) = result {
    assert!(
      !hits.is_empty(),
      "IvfPqIndex::search with n_probe=0 silently returned no results"
    );
  }
}

/// Mean recall@10 of the default (IVF-PQ) VectorIndex against exact search,
/// on clustered data.
fn b10_default_index_recall(dims: usize) -> f32 {
  const N: usize = 1200;
  const BLOBS: usize = 12;
  const QUERIES: usize = 40;
  const K: usize = 10;

  let mut rng = Rng::new(0xB10 + dims as u64);
  let centers: Vec<Vec<f32>> = (0..BLOBS)
    .map(|_| rng.gaussian_vec(dims).iter().map(|x| x * 4.0).collect())
    .collect();
  let sample = |rng: &mut Rng, blob: usize| -> Vec<f32> {
    centers[blob].iter().map(|c| c + rng.gaussian()).collect()
  };

  // IVF-PQ explicitly: the default (`Auto`) builds plain IVF at this size.
  let mut ann =
    VectorIndex::new(VectorIndexOptions::new(dims).with_ann_algorithm(AnnAlgorithm::IvfPq));
  let mut exact =
    VectorIndex::new(VectorIndexOptions::new(dims).with_training_threshold(usize::MAX));
  for node in 0..N {
    let vector = sample(&mut rng, node % BLOBS);
    ann.set(node as u64, &vector).expect("set");
    exact.set(node as u64, &vector).expect("set");
  }

  let mut found = 0usize;
  for q in 0..QUERIES {
    let query = sample(&mut rng, q % BLOBS);
    let truth: HashSet<u64> = exact
      .search(&query, SimilarOptions::new(K))
      .expect("exact search")
      .iter()
      .map(|hit| hit.node_id)
      .collect();
    let approx = ann
      .search(&query, SimilarOptions::new(K))
      .expect("ann search");
    found += approx
      .iter()
      .filter(|hit| truth.contains(&hit.node_id))
      .count();
  }
  found as f32 / (QUERIES * K) as f32
}

/// Contract: a dimension with no divisor in 2..=48 must not silently collapse
/// the default IVF-PQ index to one PQ subspace.
///
/// 97 is prime, so `resolve_pq_subspaces(48, 97)` returns 1: every vector in
/// a list is quantized to one of 256 whole-vector codewords, and ranking
/// inside a list is lost.
#[test]
fn w3_b10_prime_dimension_does_not_collapse_default_recall() {
  let recall = b10_default_index_recall(97);
  assert!(
    recall >= 0.5,
    "default VectorIndex recall@10 at 97 dims is {recall:.2}: the PQ subspace count \
     collapsed to 1 (prime dimension). Adjust it, or fall back to a non-PQ index"
  );
}

// ============================================================================
// B11: VectorStoreConfig::with_metric normalizes for every metric
// ============================================================================

/// Contract (matches VectorIndexOptions::with_metric from wave 1): choosing a
/// metric resets normalize_on_insert to that metric's default, which is true
/// only for cosine. An explicit with_normalize afterwards still wins.
#[test]
fn w3_b11_store_config_with_metric_resets_normalize() {
  for metric in [DistanceMetric::Euclidean, DistanceMetric::DotProduct] {
    let config = VectorStoreConfig::new(4).with_metric(metric);
    assert!(
      !config.normalize_on_insert,
      "VectorStoreConfig::new(4).with_metric({metric:?}) still normalizes on insert"
    );
  }
  assert!(
    VectorStoreConfig::new(4)
      .with_metric(DistanceMetric::Cosine)
      .normalize_on_insert
  );
  assert!(
    VectorStoreConfig::new(4)
      .with_metric(DistanceMetric::Euclidean)
      .with_normalize(true)
      .normalize_on_insert
  );
}

#[test]
fn w3_b11_euclidean_store_keeps_raw_vectors() {
  let mut manifest =
    create_vector_store(VectorStoreConfig::new(4).with_metric(DistanceMetric::Euclidean));
  vector_store_insert(&mut manifest, 1, &[3.0, 4.0, 0.0, 0.0]).expect("insert");
  assert_eq!(
    vector_store_node_vector(&manifest, 1),
    Some(&[3.0f32, 4.0, 0.0, 0.0][..]),
    "a Euclidean store must keep raw vectors"
  );
}

// ============================================================================
// B12: compaction breaks the manifest's reload validation
// ============================================================================

/// 120 vectors in three sealed fragments of 40 (ids 0..3), plus an empty
/// active fragment.
fn b12_manifest() -> VectorManifest {
  let mut manifest = create_vector_store(
    VectorStoreConfig::new(4)
      .with_metric(DistanceMetric::Euclidean)
      .with_normalize(false)
      .with_row_group_size(16)
      .with_fragment_target_size(40),
  );
  for node in 0..120u64 {
    let x = node as f32;
    vector_store_insert(&mut manifest, node, &[1.0 + x, 2.0, x * 0.5, 3.0]).expect("insert");
  }
  assert_round_trips("fixture before compaction", &manifest);
  manifest
}

/// Contract: a manifest produced by the compaction functions serializes,
/// deserializes, and passes validation, and every live vector survives. The
/// check a checkpoint runs before serializing a store agrees with the decode.
fn assert_round_trips(label: &str, manifest: &VectorManifest) {
  if let Err(err) = validate_manifest_for_serialization(manifest) {
    panic!("{label}: the pre-serialize check refuses a manifest that should reload: {err}");
  }
  let bytes = serialize_manifest(manifest);
  let restored = deserialize_manifest(&bytes).unwrap_or_else(|err| {
    let fragments: Vec<(usize, usize, usize, usize)> = manifest
      .fragments
      .iter()
      .map(|f| {
        (
          f.id,
          f.total_vectors,
          f.deleted_count,
          f.deletion_bitmap.len(),
        )
      })
      .collect();
    panic!(
      "{label}: manifest fails reload validation: {err}\n  total_vectors={} total_deleted={} \
       live={}\n  fragments (id, total, deleted, bitmap words) = {fragments:?}",
      manifest.total_vectors,
      manifest.total_deleted,
      manifest.live_count()
    )
  });
  for &node in manifest.node_to_vector.keys() {
    assert_eq!(
      vector_store_node_vector(&restored, node),
      vector_store_node_vector(manifest, node),
      "{label}: node {node} changed across the round trip"
    );
  }
}

#[test]
fn w3_b12_run_compaction_manifest_round_trips() {
  let mut manifest = b12_manifest();
  for node in 0..20u64 {
    assert!(vector_store_delete(&mut manifest, node));
  }
  let live_before = manifest.live_count();
  let strategy = CompactionStrategy {
    min_deletion_ratio: 0.3,
    max_fragments_per_compaction: 4,
    min_vectors_to_compact: 1,
  };
  assert!(run_compaction_if_needed(&mut manifest, &strategy));
  assert_eq!(manifest.live_count(), live_before);
  assert_round_trips("after run_compaction_if_needed", &manifest);
}

#[test]
fn w3_b12_force_full_compaction_manifest_round_trips() {
  let mut manifest = b12_manifest();
  for node in (0..120u64).step_by(3) {
    assert!(vector_store_delete(&mut manifest, node));
  }
  force_full_compaction(&mut manifest);
  assert_round_trips("after force_full_compaction", &manifest);
}

#[test]
fn w3_b12_clear_deleted_fragments_manifest_round_trips() {
  let mut manifest = b12_manifest();
  for node in 40..80u64 {
    assert!(vector_store_delete(&mut manifest, node));
  }
  assert_eq!(clear_deleted_fragments(&mut manifest), 1);
  assert_round_trips("after clear_deleted_fragments", &manifest);
}

/// Contract: a compacted store keeps working. Inserts that seal new
/// fragments, deletes, and a second compaction all leave a manifest that
/// reloads, with every live vector intact.
#[test]
fn w3_b12_compaction_lifecycle_round_trips() {
  let mut manifest = b12_manifest();
  for node in (0..120u64).filter(|node| node % 4 != 0) {
    assert!(vector_store_delete(&mut manifest, node));
  }
  assert_eq!(clear_deleted_fragments(&mut manifest), 0);
  force_full_compaction(&mut manifest);
  assert_round_trips("after the first compaction", &manifest);

  // Seal two more fragments, then delete across old and new data.
  for node in 200..290u64 {
    let x = node as f32;
    vector_store_insert(&mut manifest, node, &[x, 1.0, 2.0, 3.0]).expect("insert");
  }
  for node in (0..290u64).step_by(5) {
    vector_store_delete(&mut manifest, node);
  }
  assert_round_trips("after inserts and deletes", &manifest);

  let live_before = manifest.live_count();
  let strategy = CompactionStrategy {
    min_deletion_ratio: 0.1,
    max_fragments_per_compaction: 8,
    min_vectors_to_compact: 1,
  };
  assert!(run_compaction_if_needed(&mut manifest, &strategy));
  assert_eq!(manifest.live_count(), live_before);
  assert_round_trips("after the second compaction", &manifest);
}
