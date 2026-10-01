// Audit lane `vector`: repros for findings V1-V4.
//
// V1: VectorIndex normalizes stored vectors for non-cosine metrics, and cosine
//     without normalization ranks by raw dot product.
// V2: ANN entry points panic on wrong-length input or mismatched manifests
//     instead of returning Err.
// V3: search_multi aggregates over incomplete per-query candidate lists.
// V4: VectorIndex::set drops the old ANN entry before validating the new vector.

use std::collections::HashMap;
use std::panic::{self, AssertUnwindSafe};

use kitedb::api::vector_search::{AnnAlgorithm, SimilarOptions, VectorIndex, VectorIndexOptions};
use kitedb::vector::{
  create_vector_store, vector_store_insert, DistanceMetric, IvfConfig, IvfIndex, IvfPqConfig,
  IvfPqIndex, MultiQueryAggregation, VectorManifest, VectorSearchResult, VectorStoreConfig,
};

// ============================================================================
// Helpers
// ============================================================================

/// What an ANN entry point returned for invalid input.
///
/// Implemented for the current return types (`Vec`, `bool`) and for `Result`,
/// so these tests compile both before and after the entry points start
/// returning `Result`. Only an `Err` counts as a rejection.
trait EntryOutcome {
  fn rejected(&self) -> bool;
  fn describe(&self) -> String;
}

impl<T, E> EntryOutcome for Result<T, E> {
  fn rejected(&self) -> bool {
    self.is_err()
  }

  fn describe(&self) -> String {
    if self.is_ok() {
      "Ok(..)".to_string()
    } else {
      "Err(..)".to_string()
    }
  }
}

impl EntryOutcome for Vec<VectorSearchResult> {
  fn rejected(&self) -> bool {
    false
  }

  fn describe(&self) -> String {
    format!("Vec with {} results (no error channel)", self.len())
  }
}

impl EntryOutcome for bool {
  fn rejected(&self) -> bool {
    false
  }

  fn describe(&self) -> String {
    format!("bool {self} (no error channel)")
  }
}

/// Search output that compiles whether search returns `Vec` or `Result<Vec, _>`.
trait SearchOutput {
  fn into_hits(self, what: &str) -> Vec<VectorSearchResult>;
}

impl SearchOutput for Vec<VectorSearchResult> {
  fn into_hits(self, _what: &str) -> Vec<VectorSearchResult> {
    self
  }
}

impl<E: std::fmt::Debug> SearchOutput for Result<Vec<VectorSearchResult>, E> {
  fn into_hits(self, what: &str) -> Vec<VectorSearchResult> {
    self.unwrap_or_else(|err| panic!("{what}: unexpected error {err:?}"))
  }
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

/// Contract: invalid input returns `Err`, it never panics.
fn assert_err_not_panic<R: EntryOutcome>(what: &str, f: impl FnOnce() -> R) {
  match panic::catch_unwind(AssertUnwindSafe(f)) {
    Err(payload) => panic!(
      "{what}: panicked ({}) instead of returning Err",
      panic_message(payload.as_ref())
    ),
    Ok(outcome) => assert!(
      outcome.rejected(),
      "{what}: expected Err, got {}",
      outcome.describe()
    ),
  }
}

const DIMS: usize = 4;
const N_VECTORS: u64 = 16;

fn sample_vector(i: u64) -> Vec<f32> {
  let x = i as f32;
  vec![
    1.0 + x,
    (x * 0.7).sin() + 2.0,
    (x % 3.0) + 0.5,
    10.0 - x * 0.3,
  ]
}

fn euclidean_manifest(dims: usize) -> VectorManifest {
  create_vector_store(
    VectorStoreConfig::new(dims)
      .with_metric(DistanceMetric::Euclidean)
      .with_normalize(false),
  )
}

/// Manifest with nodes 1..=N_VECTORS; returns (manifest, [(vector_id, vector)]).
fn populated_manifest(dims: usize) -> (VectorManifest, Vec<(u64, Vec<f32>)>) {
  let mut manifest = euclidean_manifest(dims);
  let mut entries = Vec::new();
  for node_id in 1..=N_VECTORS {
    let mut vector = sample_vector(node_id);
    vector.resize(dims, 1.0);
    let vector_id = vector_store_insert(&mut manifest, node_id, &vector).expect("store insert");
    entries.push((vector_id, vector));
  }
  (manifest, entries)
}

fn trained_ivf() -> (IvfIndex, VectorManifest, Vec<(u64, Vec<f32>)>) {
  let (manifest, entries) = populated_manifest(DIMS);
  let mut index = IvfIndex::new(
    DIMS,
    IvfConfig::new(2)
      .with_n_probe(2)
      .with_metric(DistanceMetric::Euclidean),
  );
  let training: Vec<f32> = entries.iter().flat_map(|(_, v)| v.clone()).collect();
  index
    .add_training_vectors(&training, entries.len())
    .expect("add training vectors");
  index.train().expect("train ivf");
  for (vector_id, vector) in &entries {
    index.insert(*vector_id, vector).expect("ivf insert");
  }
  (index, manifest, entries)
}

fn trained_ivf_pq() -> (IvfPqIndex, VectorManifest, Vec<(u64, Vec<f32>)>) {
  let (manifest, entries) = populated_manifest(DIMS);
  let config = IvfPqConfig::new()
    .with_n_clusters(2)
    .with_n_probe(2)
    .with_metric(DistanceMetric::Euclidean)
    .with_num_subspaces(2)
    .with_num_centroids(4)
    .with_residuals(false);
  let mut index = IvfPqIndex::new(DIMS, config).expect("ivf-pq config");
  let training: Vec<f32> = entries.iter().flat_map(|(_, v)| v.clone()).collect();
  index
    .add_training_vectors(&training, entries.len())
    .expect("add training vectors");
  index.train().expect("train ivf-pq");
  for (vector_id, vector) in &entries {
    index.insert(*vector_id, vector).expect("ivf-pq insert");
  }
  (index, manifest, entries)
}

fn assert_close(what: &str, got: f32, expected: f32) {
  assert!(
    (got - expected).abs() < 1e-3,
    "{what}: expected {expected}, got {got}"
  );
}

// ============================================================================
// V1: normalization must follow the metric
// ============================================================================

#[test]
fn audit_v1_euclidean_default_keeps_raw_vectors() {
  let mut index =
    VectorIndex::new(VectorIndexOptions::new(3).with_metric(DistanceMetric::Euclidean));
  index.set(1, &[10.0, 0.0, 0.0]).expect("set 1");
  index.set(2, &[1.0, 0.0, 0.0]).expect("set 2");

  let hits = index
    .search(&[10.0, 0.0, 0.0], SimilarOptions::new(2))
    .expect("search");
  assert_eq!(hits.len(), 2);
  assert_eq!(hits[0].node_id, 1, "exact match must rank first: {hits:?}");
  assert_close("euclidean distance to exact match", hits[0].distance, 0.0);
  assert_close("euclidean distance to [1,0,0]", hits[1].distance, 9.0);
  assert_eq!(index.get(1), Some(vec![10.0, 0.0, 0.0]));
}

#[test]
fn audit_v1_dot_product_default_keeps_raw_vectors() {
  let mut index =
    VectorIndex::new(VectorIndexOptions::new(3).with_metric(DistanceMetric::DotProduct));
  index.set(1, &[10.0, 0.0, 0.0]).expect("set 1");
  index.set(2, &[1.0, 0.0, 0.0]).expect("set 2");

  let hits = index
    .search(&[1.0, 0.0, 0.0], SimilarOptions::new(2))
    .expect("search");
  assert_eq!(hits.len(), 2);
  assert_eq!(
    hits[0].node_id, 1,
    "larger inner product must rank first: {hits:?}"
  );
  assert_close("negated dot product for node 1", hits[0].distance, -10.0);
  assert_close("negated dot product for node 2", hits[1].distance, -1.0);
  assert_eq!(index.get(1), Some(vec![10.0, 0.0, 0.0]));
}

#[test]
fn audit_v1_euclidean_ivf_uses_raw_vectors() {
  let mut index = VectorIndex::new(
    VectorIndexOptions::new(3)
      .with_metric(DistanceMetric::Euclidean)
      .with_ann_algorithm(AnnAlgorithm::Ivf)
      .with_training_threshold(2)
      .with_n_clusters(1)
      .with_n_probe(1),
  );
  index.set(1, &[10.0, 0.0, 0.0]).expect("set 1");
  index.set(2, &[1.0, 0.0, 0.0]).expect("set 2");
  index.set(3, &[0.0, 5.0, 0.0]).expect("set 3");
  index.build_index().expect("build index");
  assert!(index.stats().index_trained);

  let hits = index
    .search(&[10.0, 0.0, 0.0], SimilarOptions::new(3))
    .expect("search");
  assert_eq!(hits[0].node_id, 1, "exact match must rank first: {hits:?}");
  assert_close(
    "ivf euclidean distance to exact match",
    hits[0].distance,
    0.0,
  );
  let node2 = hits
    .iter()
    .find(|h| h.node_id == 2)
    .expect("node 2 in results");
  assert_close("ivf euclidean distance to [1,0,0]", node2.distance, 9.0);
}

#[test]
fn audit_v1_cosine_without_normalize_uses_true_cosine() {
  let mut index = VectorIndex::new(VectorIndexOptions::new(3).with_normalize(false));
  index.set(1, &[1.0, 0.0, 0.0]).expect("set 1");
  index.set(2, &[10.0, 10.0, 0.0]).expect("set 2");

  let hits = index
    .search(&[1.0, 0.0, 0.0], SimilarOptions::new(2))
    .expect("search");
  assert_eq!(hits.len(), 2);
  assert_eq!(
    hits[0].node_id, 1,
    "same direction must rank first: {hits:?}"
  );
  assert_close("cosine distance, same direction", hits[0].distance, 0.0);
  assert_close(
    "cosine distance at 45 degrees",
    hits[1].distance,
    1.0 - std::f32::consts::FRAC_1_SQRT_2,
  );
}

#[test]
fn audit_v1_cosine_without_normalize_ivf_uses_true_cosine() {
  let mut index = VectorIndex::new(
    VectorIndexOptions::new(3)
      .with_normalize(false)
      .with_ann_algorithm(AnnAlgorithm::Ivf)
      .with_training_threshold(2)
      .with_n_clusters(1)
      .with_n_probe(1),
  );
  index.set(1, &[1.0, 0.0, 0.0]).expect("set 1");
  index.set(2, &[10.0, 10.0, 0.0]).expect("set 2");
  index.build_index().expect("build index");
  assert!(index.stats().index_trained);

  let hits = index
    .search(&[1.0, 0.0, 0.0], SimilarOptions::new(2))
    .expect("search");
  assert_eq!(hits.len(), 2);
  assert_eq!(
    hits[0].node_id, 1,
    "same direction must rank first: {hits:?}"
  );
  assert_close("ivf cosine distance, same direction", hits[0].distance, 0.0);
  assert_close(
    "ivf cosine distance at 45 degrees",
    hits[1].distance,
    1.0 - std::f32::consts::FRAC_1_SQRT_2,
  );
}

// ============================================================================
// V2: invalid input to ANN entry points returns Err, never panics
// ============================================================================

#[test]
fn audit_v2_ivf_search_wrong_query_len_returns_err() {
  let (index, manifest, _) = trained_ivf();
  for len in [DIMS - 1, DIMS + 1] {
    let query = vec![1.0; len];
    assert_err_not_panic(&format!("IvfIndex::search with {len}-dim query"), || {
      index.search(&manifest, &query, 3, None)
    });
  }
}

#[test]
fn audit_v2_ivf_search_multi_wrong_query_len_returns_err() {
  let (index, manifest, _) = trained_ivf();
  let good = vec![1.0; DIMS];
  let bad = vec![1.0; DIMS - 1];
  let queries: Vec<&[f32]> = vec![good.as_slice(), bad.as_slice()];
  assert_err_not_panic("IvfIndex::search_multi with a 3-dim query", || {
    index.search_multi(&manifest, &queries, 3, MultiQueryAggregation::Min, None)
  });
}

#[test]
fn audit_v2_ivf_delete_wrong_vector_len_returns_err() {
  let (mut index, _, entries) = trained_ivf();
  let (vector_id, _) = entries[0];
  let bad = vec![1.0; DIMS - 1];
  assert_err_not_panic("IvfIndex::delete with a 3-dim vector", || {
    index.delete(vector_id, &bad)
  });
}

#[test]
fn audit_v2_ivf_pq_search_wrong_query_len_returns_err() {
  let (index, manifest, _) = trained_ivf_pq();
  for len in [DIMS - 1, DIMS + 1] {
    let query = vec![1.0; len];
    assert_err_not_panic(&format!("IvfPqIndex::search with {len}-dim query"), || {
      index.search(&manifest, &query, 3, None)
    });
  }
}

#[test]
fn audit_v2_ivf_pq_search_multi_wrong_query_len_returns_err() {
  let (index, manifest, _) = trained_ivf_pq();
  let good = vec![1.0; DIMS];
  let bad = vec![1.0; DIMS - 1];
  let queries: Vec<&[f32]> = vec![good.as_slice(), bad.as_slice()];
  assert_err_not_panic("IvfPqIndex::search_multi with a 3-dim query", || {
    index.search_multi(&manifest, &queries, 3, MultiQueryAggregation::Min, None)
  });
}

#[test]
fn audit_v2_ivf_pq_delete_wrong_vector_len_returns_err() {
  let (mut index, _, entries) = trained_ivf_pq();
  let (vector_id, _) = entries[0];
  let bad = vec![1.0; DIMS - 1];
  assert_err_not_panic("IvfPqIndex::delete with a 3-dim vector", || {
    index.delete(vector_id, &bad)
  });
}

#[test]
fn audit_v2_ivf_search_manifest_dims_mismatch_returns_err() {
  let (index, _, _) = trained_ivf();
  // Same vector ids, but the manifest stores 8-dim vectors.
  let (wide_manifest, _) = populated_manifest(DIMS * 2);
  let query = vec![1.0; DIMS];
  assert_err_not_panic("IvfIndex::search with an 8-dim manifest", || {
    index.search(&wide_manifest, &query, 3, None)
  });
}

#[test]
fn audit_v2_ivf_search_manifest_short_row_group_returns_err() {
  let (index, mut manifest, _) = trained_ivf();
  // rg.data shorter than count * dims (as a hand-written JSON manifest could be).
  let row_group = &mut manifest.fragments[0].row_groups[0];
  assert_eq!(row_group.count, N_VECTORS as usize);
  row_group.data.truncate(DIMS * (N_VECTORS as usize / 2));
  let query = vec![1.0; DIMS];
  assert_err_not_panic("IvfIndex::search with a short row group", || {
    index.search(&manifest, &query, N_VECTORS as usize, None)
  });
}

#[test]
fn audit_v2_ivf_pq_search_manifest_dims_mismatch_returns_err() {
  let (index, _, _) = trained_ivf_pq();
  let (wide_manifest, _) = populated_manifest(DIMS * 2);
  let query = vec![1.0; DIMS];
  assert_err_not_panic("IvfPqIndex::search with an 8-dim manifest", || {
    index.search(&wide_manifest, &query, 3, None)
  });
}

// ============================================================================
// V3: multi-query aggregation must use every query's distance
// ============================================================================
//
// Euclidean, 2 dims, q1 = (0,0), q2 = (10,0), k = 1 (per-query over-fetch = 2):
//   A = (5, 0)    d = (5, 5)            sum 10.0   avg 5.0    max 5.0
//   B = (0, 4)    d = (4, 10.770)       sum 14.770 avg 7.385  max 10.770
//   C = (10, 4.5) d = (10.966, 4.5)     sum 15.466 avg 7.733  max 10.966
// q1's top-2 is {B, A}, q2's top-2 is {C, A}. A is the true winner for Sum, Avg
// and Max, but B (seen only by q1) and C (seen only by q2) get scored on one
// distance.

const V3_NODE_A: u64 = 1;
const V3_POINTS: [(u64, [f32; 2]); 3] = [(1, [5.0, 0.0]), (2, [0.0, 4.0]), (3, [10.0, 4.5])];
const V3_Q1: [f32; 2] = [0.0, 0.0];
const V3_Q2: [f32; 2] = [10.0, 0.0];

fn v3_manifest() -> (VectorManifest, HashMap<u64, u64>) {
  let mut manifest = euclidean_manifest(2);
  let mut node_to_vector = HashMap::new();
  for (node_id, point) in V3_POINTS {
    let vector_id = vector_store_insert(&mut manifest, node_id, &point).expect("store insert");
    node_to_vector.insert(node_id, vector_id);
  }
  (manifest, node_to_vector)
}

fn v3_expected(aggregation: MultiQueryAggregation) -> f32 {
  match aggregation {
    MultiQueryAggregation::Sum => 10.0,
    MultiQueryAggregation::Avg | MultiQueryAggregation::Max => 5.0,
    MultiQueryAggregation::Min => unreachable!("Min is not affected"),
  }
}

fn v3_check(label: &str, mut run: impl FnMut(MultiQueryAggregation) -> Vec<VectorSearchResult>) {
  let mut failures = Vec::new();
  for aggregation in [
    MultiQueryAggregation::Sum,
    MultiQueryAggregation::Avg,
    MultiQueryAggregation::Max,
  ] {
    let hits = run(aggregation);
    let expected = v3_expected(aggregation);
    let ok =
      hits.len() == 1 && hits[0].node_id == V3_NODE_A && (hits[0].distance - expected).abs() < 1e-3;
    if !ok {
      let got: Vec<(u64, f32)> = hits.iter().map(|h| (h.node_id, h.distance)).collect();
      failures.push(format!(
        "{aggregation:?}: expected [(node {V3_NODE_A}, {expected})], got {got:?}"
      ));
    }
  }
  assert!(
    failures.is_empty(),
    "{label} search_multi aggregated over incomplete candidate lists:\n  {}",
    failures.join("\n  ")
  );
}

#[test]
fn audit_v3_ivf_search_multi_aggregates_all_queries() {
  let (manifest, node_to_vector) = v3_manifest();
  // One cluster: every vector is probed, so per-query search is exact.
  let mut index = IvfIndex::new(
    2,
    IvfConfig::new(1)
      .with_n_probe(1)
      .with_metric(DistanceMetric::Euclidean),
  );
  let training: Vec<f32> = V3_POINTS.iter().flat_map(|(_, p)| p.to_vec()).collect();
  index
    .add_training_vectors(&training, V3_POINTS.len())
    .expect("add training vectors");
  index.train().expect("train ivf");
  for (node_id, point) in V3_POINTS {
    index
      .insert(node_to_vector[&node_id], &point)
      .expect("ivf insert");
  }

  let queries: Vec<&[f32]> = vec![V3_Q1.as_slice(), V3_Q2.as_slice()];
  v3_check("IvfIndex", |aggregation| {
    index
      .search_multi(&manifest, &queries, 1, aggregation, None)
      .into_hits("IvfIndex::search_multi")
  });
}

#[test]
fn audit_v3_ivf_pq_search_multi_aggregates_all_queries() {
  let (manifest, node_to_vector) = v3_manifest();
  // Hand-built codebooks that reproduce every point exactly, so ADC distances
  // equal exact Euclidean distances: subspace 0 is x, subspace 1 is y.
  let config = IvfPqConfig::new()
    .with_n_clusters(1)
    .with_n_probe(1)
    .with_metric(DistanceMetric::Euclidean)
    .with_num_subspaces(2)
    .with_num_centroids(3)
    .with_residuals(false);
  let pq_centroids = vec![vec![0.0, 5.0, 10.0], vec![0.0, 4.0, 4.5]];
  let codes: HashMap<u64, Vec<u8>> = HashMap::from([
    (1, vec![1, 0]), // A = (5, 0)
    (2, vec![0, 1]), // B = (0, 4)
    (3, vec![2, 2]), // C = (10, 4.5)
  ]);
  let pq_codes: HashMap<u64, Vec<u8>> = codes
    .into_iter()
    .map(|(node_id, code)| (node_to_vector[&node_id], code))
    .collect();
  let mut list: Vec<u64> = pq_codes.keys().copied().collect();
  list.sort_unstable();
  let index = IvfPqIndex::from_serialized(
    config,
    vec![0.0, 0.0],
    HashMap::from([(0, list)]),
    pq_codes,
    pq_centroids,
    None,
    2,
    true,
  )
  .expect("hand-built ivf-pq index");

  let queries: Vec<&[f32]> = vec![V3_Q1.as_slice(), V3_Q2.as_slice()];
  v3_check("IvfPqIndex", |aggregation| {
    index
      .search_multi(&manifest, &queries, 1, aggregation, None)
      .into_hits("IvfPqIndex::search_multi")
  });
}

// ============================================================================
// V4: a rejected set() must not drop the node from the ANN index
// ============================================================================

const V4_NODES: u64 = 8;

fn v4_vector(node_id: u64) -> Vec<f32> {
  let x = node_id as f32;
  vec![x, 1.0, (node_id % 3) as f32, 0.5]
}

fn v4_check(algorithm: AnnAlgorithm) {
  let invalid: [(&str, [f32; 4]); 2] = [("all-zero", [0.0; 4]), ("NaN", [f32::NAN, 1.0, 0.0, 0.0])];
  for (label, bad) in invalid {
    let mut index = VectorIndex::new(
      VectorIndexOptions::new(4)
        .with_ann_algorithm(algorithm)
        .with_training_threshold(2)
        .with_n_clusters(1)
        .with_n_probe(1)
        .with_pq_subspaces(2)
        .with_pq_centroids(4),
    );
    for node_id in 1..=V4_NODES {
      index.set(node_id, &v4_vector(node_id)).expect("set");
    }
    index.build_index().expect("build index");
    assert!(index.stats().index_trained);

    let before = index.get(1).expect("node 1 stored");
    assert!(
      index.set(1, &bad).is_err(),
      "{algorithm:?}: set(1, {label}) must be rejected"
    );
    assert_eq!(
      index.get(1),
      Some(before),
      "{algorithm:?}: rejected set(1, {label}) must keep the stored vector"
    );

    let hits = index
      .search(&v4_vector(1), SimilarOptions::new(V4_NODES as usize))
      .expect("search");
    let ids: Vec<u64> = hits.iter().map(|h| h.node_id).collect();
    assert!(
      ids.contains(&1),
      "{algorithm:?}: node 1 vanished from ANN results after rejected set(1, {label}); got {ids:?}"
    );
  }
}

#[test]
fn audit_v4_ivf_rejected_set_keeps_ann_entry() {
  v4_check(AnnAlgorithm::Ivf);
}

#[test]
fn audit_v4_ivf_pq_rejected_set_keeps_ann_entry() {
  v4_check(AnnAlgorithm::IvfPq);
}
