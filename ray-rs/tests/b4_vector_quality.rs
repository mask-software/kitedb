// Lane b4 `vector-quality`: IVF-PQ recall.
//
// VQ1 The default ANN index (IVF-PQ, no residuals, 32 subspaces at 128
//     dimensions) ranks candidates by PQ (ADC) distance alone. On clustered
//     data, where the coarse probe finds every true neighbor (plain IVF
//     reaches recall 1.0), recall@10 is about 0.5 on this data (0.15 at
//     100K vectors): the PQ approximation reorders neighbors inside a
//     cluster. Nothing re-ranks the candidates by exact distance against the
//     stored vectors, and the returned distances are the approximate ones.
//
// The datasets are seeded. Index training draws its own k-means seeds, so the
// recall floors sit well below the measured recall.

use std::collections::HashSet;

use kitedb::api::vector_search::{SimilarOptions, VectorIndex, VectorIndexOptions};
use kitedb::vector::{
  create_vector_store, vector_store_insert, DistanceMetric, IvfPqConfig, IvfPqIndex,
  MultiQueryAggregation, VectorManifest, VectorSearchResult, VectorStoreConfig,
};

const DIMS: usize = 128;
const BLOBS: usize = 30;
const VECTORS: usize = 3000;
const QUERIES: usize = 40;
const K: usize = 10;
const MIN_RECALL: f64 = 0.9;

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
}

/// Gaussian blobs: centers N(0, 4^2) per component, points N(center, 1).
/// The coarse IVF probe finds every true neighbor on this data; ranking
/// inside a blob is what PQ gets wrong.
struct Blobs {
  centers: Vec<Vec<f32>>,
}

impl Blobs {
  fn new(rng: &mut Rng) -> Self {
    let centers = (0..BLOBS)
      .map(|_| (0..DIMS).map(|_| 4.0 * rng.gaussian()).collect())
      .collect();
    Self { centers }
  }

  fn sample(&self, rng: &mut Rng, blob: usize) -> Vec<f32> {
    self.centers[blob % BLOBS]
      .iter()
      .map(|&c| c + rng.gaussian())
      .collect()
  }
}

/// The data set (node id = position) and a source for queries drawn from the
/// same blobs, from a fixed seed.
fn dataset(seed: u64) -> (Vec<Vec<f32>>, Blobs, Rng) {
  let mut rng = Rng::new(seed);
  let blobs = Blobs::new(&mut rng);
  let vectors = (0..VECTORS).map(|i| blobs.sample(&mut rng, i)).collect();
  (vectors, blobs, rng)
}

fn queries(blobs: &Blobs, rng: &mut Rng) -> Vec<Vec<f32>> {
  (0..QUERIES).map(|q| blobs.sample(rng, q * 7)).collect()
}

/// Exact distance in the metric's native space, in f64: cosine `1 - cos`,
/// Euclidean L2, dot product `-dot`.
fn exact_distance(metric: DistanceMetric, query: &[f32], vector: &[f32]) -> f64 {
  let dot: f64 = query
    .iter()
    .zip(vector)
    .map(|(&a, &b)| a as f64 * b as f64)
    .sum();
  match metric {
    DistanceMetric::Cosine => {
      let norm = |v: &[f32]| v.iter().map(|&x| x as f64 * x as f64).sum::<f64>().sqrt();
      1.0 - dot / (norm(query) * norm(vector))
    }
    DistanceMetric::Euclidean => query
      .iter()
      .zip(vector)
      .map(|(&a, &b)| (a as f64 - b as f64).powi(2))
      .sum::<f64>()
      .sqrt(),
    DistanceMetric::DotProduct => -dot,
  }
}

/// Node ids (= positions in `vectors`) of the `k` smallest scores.
fn top_k_by(vectors: &[Vec<f32>], k: usize, score: impl Fn(&[f32]) -> f64) -> HashSet<u64> {
  let mut scored: Vec<(f64, u64)> = vectors
    .iter()
    .enumerate()
    .map(|(node, vector)| (score(vector), node as u64))
    .collect();
  scored.sort_by(|a, b| a.0.total_cmp(&b.0));
  scored.iter().take(k).map(|&(_, node)| node).collect()
}

fn hits_in(truth: &HashSet<u64>, node_ids: impl Iterator<Item = u64>) -> usize {
  node_ids.filter(|node| truth.contains(node)).count()
}

/// Tolerance for comparing a returned f32 distance with the f64 exact one.
fn close(got: f32, expected: f64) -> bool {
  (got as f64 - expected).abs() <= 1e-3 * expected.abs().max(1.0)
}

/// An IVF-PQ index configured like `VectorIndex` builds it by default for
/// 128 dimensions and 3000 vectors: sqrt(n) clusters, 10 probes, 32 PQ
/// subspaces of 256 centroids, no residual encoding.
fn default_like_ivf_pq(
  metric: DistanceMetric,
  normalize_on_insert: bool,
  vectors: &[Vec<f32>],
) -> (IvfPqIndex, VectorManifest) {
  let n_clusters = (vectors.len() as f64).sqrt() as usize;
  let mut manifest = create_vector_store(
    VectorStoreConfig::new(DIMS)
      .with_metric(metric)
      .with_normalize(normalize_on_insert),
  );
  let mut ids = Vec::with_capacity(vectors.len());
  for (node, vector) in vectors.iter().enumerate() {
    ids.push(vector_store_insert(&mut manifest, node as u64, vector).expect("store insert"));
  }

  let mut index = IvfPqIndex::new(
    DIMS,
    IvfPqConfig::new()
      .with_n_clusters(n_clusters)
      .with_n_probe(10)
      .with_metric(metric)
      .with_num_subspaces(32)
      .with_num_centroids(256)
      .with_residuals(false),
  )
  .expect("valid config");
  index.build_from_store(&manifest).expect("build index");
  assert_eq!(index.stats().total_vectors, ids.len());
  (index, manifest)
}

/// Mean recall@K over the queries, and the first returned distance (with its
/// exact value) that is not the exact distance.
fn check_results(
  metric: DistanceMetric,
  vectors: &[Vec<f32>],
  queries: &[Vec<f32>],
  mut search: impl FnMut(&[f32]) -> Vec<VectorSearchResult>,
) -> (f64, Option<(f32, f64)>) {
  let mut found = 0usize;
  let mut wrong_distance = None;
  for query in queries {
    let truth = top_k_by(vectors, K, |vector| exact_distance(metric, query, vector));
    let hits = search(query);
    found += hits_in(&truth, hits.iter().map(|hit| hit.node_id));
    for hit in &hits {
      let expected = exact_distance(metric, query, &vectors[hit.node_id as usize]);
      if wrong_distance.is_none() && !close(hit.distance, expected) {
        wrong_distance = Some((hit.distance, expected));
      }
    }
  }
  (found as f64 / (queries.len() * K) as f64, wrong_distance)
}

// ============================================================================
// VQ1: IVF-PQ recall
// ============================================================================

/// Contract: the default `VectorIndex` (IVF-PQ) reaches recall@10 >= 0.9 on
/// clustered 128-d data whose neighbors the coarse probe always finds.
#[test]
fn b4_vq1_default_vector_index_recall_at_10_128d() {
  let (vectors, blobs, mut rng) = dataset(0x0B4_0001);
  let queries = queries(&blobs, &mut rng);
  let mut index = VectorIndex::new(VectorIndexOptions::new(DIMS));
  for (node, vector) in vectors.iter().enumerate() {
    index.set(node as u64, vector).expect("set");
  }
  index.build_index().expect("build index");
  assert!(
    index.stats().index_trained,
    "the default index should train"
  );

  let mut found = 0usize;
  for query in &queries {
    let truth = top_k_by(&vectors, K, |vector| {
      exact_distance(DistanceMetric::Cosine, query, vector)
    });
    let hits = index.search(query, SimilarOptions::new(K)).expect("search");
    assert_eq!(hits.len(), K);
    found += hits_in(&truth, hits.iter().map(|hit| hit.node_id));
  }
  let recall = found as f64 / (queries.len() * K) as f64;
  assert!(
    recall >= MIN_RECALL,
    "default VectorIndex (IVF-PQ) recall@10 at 128 dims is {recall:.3} (< {MIN_RECALL}): \
     candidates are ranked by PQ distance only, with no exact re-rank"
  );
}

/// Contract: for every metric, IVF-PQ search reaches recall@10 >= 0.9 and
/// returns the exact distance in the metric's native space (cosine
/// `1 - cos`, Euclidean L2, dot product `-dot`), whether or not the store
/// normalized the vectors.
#[test]
fn b4_vq1_ivf_pq_search_recall_and_exact_distances_per_metric() {
  let (vectors, blobs, mut rng) = dataset(0x0B4_0002);
  let queries = queries(&blobs, &mut rng);
  let cases = [
    ("cosine, normalized store", DistanceMetric::Cosine, true),
    ("cosine, raw store", DistanceMetric::Cosine, false),
    ("euclidean", DistanceMetric::Euclidean, false),
    ("dot product", DistanceMetric::DotProduct, false),
  ];
  let mut failures = Vec::new();
  for (label, metric, normalize) in cases {
    let (index, manifest) = default_like_ivf_pq(metric, normalize, &vectors);
    let (recall, wrong_distance) = check_results(metric, &vectors, &queries, |query| {
      index.search(&manifest, query, K, None).expect("search")
    });
    if recall < MIN_RECALL {
      failures.push(format!("{label}: recall@10 {recall:.3} < {MIN_RECALL}"));
    }
    if let Some((got, expected)) = wrong_distance {
      failures.push(format!(
        "{label}: returned distance {got} is not the exact distance {expected:.5}"
      ));
    }
  }
  assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Contract: multi-query IVF-PQ search ranks by the exact aggregated distance,
/// with recall@10 >= 0.9 against an exact aggregated search.
#[test]
fn b4_vq1_ivf_pq_search_multi_recall_and_exact_distances() {
  let (vectors, blobs, mut rng) = dataset(0x0B4_0003);
  let metric = DistanceMetric::Euclidean;
  let (index, manifest) = default_like_ivf_pq(metric, false, &vectors);

  // Two queries from the same blob per search.
  let groups: Vec<[Vec<f32>; 2]> = (0..QUERIES / 2)
    .map(|q| [blobs.sample(&mut rng, q * 7), blobs.sample(&mut rng, q * 7)])
    .collect();
  let mut found = 0usize;
  let mut wrong_distance = None;
  for group in &groups {
    let group: Vec<&[f32]> = group.iter().map(Vec::as_slice).collect();
    let avg = |vector: &[f32]| {
      group
        .iter()
        .map(|query| exact_distance(metric, query, vector))
        .sum::<f64>()
        / group.len() as f64
    };
    let truth = top_k_by(&vectors, K, avg);
    let hits = index
      .search_multi(&manifest, &group, K, MultiQueryAggregation::Avg, None)
      .expect("search_multi");
    found += hits_in(&truth, hits.iter().map(|hit| hit.node_id));
    for hit in &hits {
      let expected = avg(&vectors[hit.node_id as usize]);
      if wrong_distance.is_none() && !close(hit.distance, expected) {
        wrong_distance = Some((hit.distance, expected));
      }
    }
  }
  let recall = found as f64 / (groups.len() * K) as f64;
  let mut failures = Vec::new();
  if recall < MIN_RECALL {
    failures.push(format!("search_multi recall@10 {recall:.3} < {MIN_RECALL}"));
  }
  if let Some((got, expected)) = wrong_distance {
    failures.push(format!(
      "search_multi returned distance {got}, not the exact aggregate {expected:.5}"
    ));
  }
  assert!(failures.is_empty(), "{}", failures.join("\n"));
}
