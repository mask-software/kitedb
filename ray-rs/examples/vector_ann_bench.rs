//! ANN algorithm benchmark (IVF vs IVF-PQ)
//!
//! Prints build time, then search p50/p95 latency and mean recall@k against
//! exact search (one block per re-rank factor when sweeping).
//!
//! Usage:
//!   cargo run --release --example vector_ann_bench --no-default-features -- [options]
//!
//! Options:
//!   --algorithm ivf|ivf_pq             Algorithm to benchmark (default: ivf_pq)
//!   --vectors N                        Number of vectors (default: 20000)
//!   --dimensions D                     Vector dimensions (default: 384)
//!   --queries N                        Query count (default: 200)
//!   --k N                              Top-k (default: 10)
//!   --n-clusters N                     IVF clusters (default: sqrt(vectors) clamped to [16,1024])
//!   --n-probe N                        Probe count (default: 10)
//!   --pq-subspaces N                   PQ subspaces for IVF-PQ (default: 48)
//!   --pq-centroids N                   PQ centroids per subspace (default: 256)
//!   --residuals true|false             Use residual encoding for IVF-PQ (default: false)
//!   --metric cosine|euclidean|dot      Distance metric (default: cosine)
//!   --dataset uniform|clustered        uniform: components in [-1, 1); clustered: Gaussian
//!                                      blobs (centers N(0, 4^2), points N(center, 1))
//!                                      lowrank: like clustered, but each blob spreads along
//!                                      16 random directions (same mean squared spread)
//!                                      plus N(0, 0.1^2) noise, like embeddings' low
//!                                      intrinsic dimension (default: uniform)
//!   --blobs N                          Blob count for clustered/lowrank (default: 100)
//!   --rerank-factors F[,F...]          IVF-PQ exact re-rank over-fetch factors to sweep on one
//!                                      index; 0 disables the re-rank, `default` is the library
//!                                      default (default: default)
//!   --rounds N                         Timed passes over the queries, interleaving the
//!                                      re-rank factors (default: 1)
//!   --seed N                           RNG seed (default: 42)

use kitedb::types::NodeId;
use kitedb::vector::{
  create_vector_store, normalize, vector_store_all_vectors, vector_store_insert,
  vector_store_vector_by_id, DistanceMetric, IvfConfig, IvfIndex, IvfPqConfig, IvfPqIndex,
  IvfPqSearchOptions, SearchOptions, VectorManifest, VectorSearchResult, VectorStoreConfig,
};
use rand::{rngs::StdRng, Rng, SeedableRng};
use std::collections::HashSet;
use std::env;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Algorithm {
  Ivf,
  IvfPq,
}

impl Algorithm {
  fn parse(raw: &str) -> Option<Self> {
    match raw.trim().to_lowercase().as_str() {
      "ivf" => Some(Self::Ivf),
      "ivf_pq" => Some(Self::IvfPq),
      _ => None,
    }
  }

  fn as_str(&self) -> &'static str {
    match self {
      Self::Ivf => "ivf",
      Self::IvfPq => "ivf_pq",
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dataset {
  Uniform,
  Clustered,
  LowRank,
}

impl Dataset {
  fn parse(raw: &str) -> Option<Self> {
    match raw.trim().to_lowercase().as_str() {
      "uniform" => Some(Self::Uniform),
      "clustered" => Some(Self::Clustered),
      "lowrank" => Some(Self::LowRank),
      _ => None,
    }
  }

  fn as_str(&self) -> &'static str {
    match self {
      Self::Uniform => "uniform",
      Self::Clustered => "clustered",
      Self::LowRank => "lowrank",
    }
  }
}

fn parse_metric(raw: &str) -> Option<DistanceMetric> {
  match raw.trim().to_lowercase().as_str() {
    "cosine" => Some(DistanceMetric::Cosine),
    "euclidean" | "l2" => Some(DistanceMetric::Euclidean),
    "dot" | "dot_product" => Some(DistanceMetric::DotProduct),
    _ => None,
  }
}

#[derive(Debug, Clone)]
struct BenchConfig {
  algorithm: Algorithm,
  vectors: usize,
  dimensions: usize,
  queries: usize,
  k: usize,
  n_clusters: Option<usize>,
  n_probe: usize,
  pq_subspaces: usize,
  pq_centroids: usize,
  residuals: bool,
  metric: DistanceMetric,
  dataset: Dataset,
  blobs: usize,
  /// None: the library default.
  rerank_factors: Vec<Option<usize>>,
  rounds: usize,
  seed: u64,
}

impl Default for BenchConfig {
  fn default() -> Self {
    Self {
      algorithm: Algorithm::IvfPq,
      vectors: 20_000,
      dimensions: 384,
      queries: 200,
      k: 10,
      n_clusters: None,
      n_probe: 10,
      pq_subspaces: 48,
      pq_centroids: 256,
      residuals: false,
      metric: DistanceMetric::Cosine,
      dataset: Dataset::Uniform,
      blobs: 100,
      rerank_factors: Vec::new(),
      rounds: 1,
      seed: 42,
    }
  }
}

fn parse_args() -> BenchConfig {
  let mut config = BenchConfig::default();
  let args: Vec<String> = env::args().collect();
  let mut i = 1usize;

  while i < args.len() {
    match args[i].as_str() {
      "--algorithm" => {
        if let Some(value) = args.get(i + 1) {
          if let Some(parsed) = Algorithm::parse(value) {
            config.algorithm = parsed;
          }
          i += 1;
        }
      }
      "--vectors" => {
        if let Some(value) = args.get(i + 1) {
          config.vectors = value.parse().unwrap_or(config.vectors);
          i += 1;
        }
      }
      "--dimensions" => {
        if let Some(value) = args.get(i + 1) {
          config.dimensions = value.parse().unwrap_or(config.dimensions);
          i += 1;
        }
      }
      "--queries" => {
        if let Some(value) = args.get(i + 1) {
          config.queries = value.parse().unwrap_or(config.queries);
          i += 1;
        }
      }
      "--k" => {
        if let Some(value) = args.get(i + 1) {
          config.k = value.parse().unwrap_or(config.k);
          i += 1;
        }
      }
      "--n-clusters" => {
        if let Some(value) = args.get(i + 1) {
          config.n_clusters = value.parse::<usize>().ok();
          i += 1;
        }
      }
      "--n-probe" => {
        if let Some(value) = args.get(i + 1) {
          config.n_probe = value.parse().unwrap_or(config.n_probe);
          i += 1;
        }
      }
      "--pq-subspaces" => {
        if let Some(value) = args.get(i + 1) {
          config.pq_subspaces = value.parse().unwrap_or(config.pq_subspaces);
          i += 1;
        }
      }
      "--pq-centroids" => {
        if let Some(value) = args.get(i + 1) {
          config.pq_centroids = value.parse().unwrap_or(config.pq_centroids);
          i += 1;
        }
      }
      "--residuals" => {
        if let Some(value) = args.get(i + 1) {
          config.residuals = matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes"
          );
          i += 1;
        }
      }
      "--metric" => {
        if let Some(value) = args.get(i + 1) {
          if let Some(parsed) = parse_metric(value) {
            config.metric = parsed;
          }
          i += 1;
        }
      }
      "--dataset" => {
        if let Some(value) = args.get(i + 1) {
          if let Some(parsed) = Dataset::parse(value) {
            config.dataset = parsed;
          }
          i += 1;
        }
      }
      "--blobs" => {
        if let Some(value) = args.get(i + 1) {
          config.blobs = value.parse().unwrap_or(config.blobs);
          i += 1;
        }
      }
      "--rerank-factor" | "--rerank-factors" => {
        if let Some(value) = args.get(i + 1) {
          config.rerank_factors = value
            .split(',')
            .filter_map(|factor| match factor.trim() {
              "default" => Some(None),
              factor => factor.parse().ok().map(Some),
            })
            .collect();
          i += 1;
        }
      }
      "--rounds" => {
        if let Some(value) = args.get(i + 1) {
          config.rounds = value.parse().unwrap_or(config.rounds);
          i += 1;
        }
      }
      "--seed" => {
        if let Some(value) = args.get(i + 1) {
          config.seed = value.parse().unwrap_or(config.seed);
          i += 1;
        }
      }
      _ => {}
    }
    i += 1;
  }

  config.vectors = config.vectors.max(1);
  config.dimensions = config.dimensions.max(1);
  config.queries = config.queries.max(1);
  config.k = config.k.max(1).min(config.vectors);
  config.n_probe = config.n_probe.max(1);
  config.pq_subspaces = config.pq_subspaces.max(1);
  config.pq_centroids = config.pq_centroids.max(2);
  config.blobs = config.blobs.max(1);
  config.rounds = config.rounds.max(1);
  config
}

fn random_vector(rng: &mut StdRng, dimensions: usize) -> Vec<f32> {
  let mut vector = vec![0.0f32; dimensions];
  for value in &mut vector {
    *value = rng.gen_range(-1.0f32..1.0f32);
  }
  vector
}

/// Draws vectors from the configured distribution. Clustered data picks a
/// blob uniformly per vector, so data and queries share the blobs.
struct VectorSource {
  dataset: Dataset,
  dimensions: usize,
  centers: Vec<Vec<f32>>,
  /// Per blob, `LOW_RANK_DIMS` spread directions (lowrank only).
  bases: Vec<Vec<Vec<f32>>>,
}

/// Intrinsic dimension of a lowrank blob.
const LOW_RANK_DIMS: usize = 16;

impl VectorSource {
  fn new(config: &BenchConfig, rng: &mut StdRng) -> Self {
    let gaussian_vec = |rng: &mut StdRng, scale: f32| -> Vec<f32> {
      (0..config.dimensions)
        .map(|_| scale * gaussian(rng))
        .collect()
    };
    let centers = match config.dataset {
      Dataset::Uniform => Vec::new(),
      Dataset::Clustered | Dataset::LowRank => {
        (0..config.blobs).map(|_| gaussian_vec(rng, 4.0)).collect()
      }
    };
    let bases = match config.dataset {
      Dataset::LowRank => (0..config.blobs)
        .map(|_| (0..LOW_RANK_DIMS).map(|_| gaussian_vec(rng, 1.0)).collect())
        .collect(),
      _ => Vec::new(),
    };
    Self {
      dataset: config.dataset,
      dimensions: config.dimensions,
      centers,
      bases,
    }
  }

  fn sample(&self, rng: &mut StdRng) -> Vec<f32> {
    match self.dataset {
      Dataset::Uniform => random_vector(rng, self.dimensions),
      Dataset::Clustered => {
        let center = &self.centers[rng.gen_range(0..self.centers.len())];
        center.iter().map(|&c| c + gaussian(rng)).collect()
      }
      Dataset::LowRank => {
        let blob = rng.gen_range(0..self.centers.len());
        let mut vector: Vec<f32> = self.centers[blob]
          .iter()
          .map(|&c| c + 0.1 * gaussian(rng))
          .collect();
        // Coefficients N(0, 1/r): the spread's mean squared norm matches the
        // isotropic blobs' (one per dimension).
        let scale = (1.0 / LOW_RANK_DIMS as f32).sqrt();
        for direction in &self.bases[blob] {
          let z = scale * gaussian(rng);
          for (value, &d) in vector.iter_mut().zip(direction) {
            *value += z * d;
          }
        }
        vector
      }
    }
  }
}

/// Standard normal sample (Box-Muller).
fn gaussian(rng: &mut StdRng) -> f32 {
  let u1: f32 = rng.gen_range(f32::MIN_POSITIVE..1.0);
  let u2: f32 = rng.gen();
  (-2.0 * u1.ln()).sqrt() * (std::f32::consts::TAU * u2).cos()
}

fn percentile(sorted: &[u128], ratio: f64) -> u128 {
  if sorted.is_empty() {
    return 0;
  }
  let idx = ((sorted.len() as f64) * ratio)
    .floor()
    .min((sorted.len() - 1) as f64) as usize;
  sorted[idx]
}

fn exact_top_k(
  manifest: &VectorManifest,
  query: &[f32],
  k: usize,
  metric: DistanceMetric,
) -> Vec<u64> {
  let query_prepared = if metric == DistanceMetric::Cosine {
    normalize(query)
  } else {
    query.to_vec()
  };
  let distance = metric.distance_fn();
  let mut candidates: Vec<(u64, f32)> = Vec::with_capacity(manifest.node_to_vector.len());

  for &vector_id in manifest.node_to_vector.values() {
    if let Some(vector) = vector_store_vector_by_id(manifest, vector_id) {
      candidates.push((vector_id, distance(&query_prepared, vector)));
    }
  }

  candidates.sort_by(|a, b| a.1.total_cmp(&b.1));
  candidates.into_iter().take(k).map(|(id, _)| id).collect()
}

fn recall_at_k(approx: &[VectorSearchResult], exact_ids: &[u64], k: usize) -> f64 {
  if k == 0 {
    return 1.0;
  }
  let exact: HashSet<u64> = exact_ids.iter().copied().collect();
  let hits = approx
    .iter()
    .take(k)
    .filter(|result| exact.contains(&result.vector_id))
    .count();
  hits as f64 / k as f64
}

fn choose_n_clusters(config: &BenchConfig) -> usize {
  config
    .n_clusters
    .unwrap_or_else(|| (config.vectors as f64).sqrt() as usize)
    .clamp(16, 1024)
}

/// One search configuration's latency percentiles and mean recall@k.
struct Measurement {
  label: Option<String>,
  p50_ns: u128,
  p95_ns: u128,
  mean_recall: f64,
}

/// Latencies and recall of one search configuration across rounds.
#[derive(Default)]
struct Samples {
  latency_ns: Vec<u128>,
  recall_sum: f64,
  searches: usize,
}

impl Samples {
  /// Times `search` over every query and scores it against the exact top-k.
  fn record(
    &mut self,
    config: &BenchConfig,
    queries: &[Vec<f32>],
    exact: &[Vec<u64>],
    mut search: impl FnMut(&[f32]) -> Result<Vec<VectorSearchResult>, String>,
  ) -> Result<(), String> {
    for (query, exact) in queries.iter().zip(exact) {
      let start = Instant::now();
      let approx = search(query)?;
      self.latency_ns.push(start.elapsed().as_nanos());
      self.recall_sum += recall_at_k(&approx, exact, config.k);
      self.searches += 1;
    }
    Ok(())
  }

  fn finish(mut self, label: Option<String>) -> Measurement {
    self.latency_ns.sort_unstable();
    Measurement {
      label,
      p50_ns: percentile(&self.latency_ns, 0.50),
      p95_ns: percentile(&self.latency_ns, 0.95),
      mean_recall: self.recall_sum / self.searches.max(1) as f64,
    }
  }
}

/// Runs every configuration's queries once untimed, then `config.rounds`
/// timed rounds that interleave the configurations, so load on the machine
/// hits them alike.
fn measure_all<C>(
  config: &BenchConfig,
  queries: &[Vec<f32>],
  exact: &[Vec<u64>],
  configurations: &[C],
  mut search: impl FnMut(&C, &[f32]) -> Result<Vec<VectorSearchResult>, String>,
) -> Result<Vec<Samples>, String> {
  for configuration in configurations {
    for query in queries {
      search(configuration, query)?;
    }
  }
  let mut samples: Vec<Samples> = configurations.iter().map(|_| Samples::default()).collect();
  for _ in 0..config.rounds {
    for (configuration, samples) in configurations.iter().zip(&mut samples) {
      samples.record(config, queries, exact, |query| search(configuration, query))?;
    }
  }
  Ok(samples)
}

fn run_ivf_bench(
  config: &BenchConfig,
  manifest: &VectorManifest,
  vector_ids: &[u64],
  training_data: &[f32],
  queries: &[Vec<f32>],
  exact: &[Vec<u64>],
) -> Result<(f64, Vec<Measurement>), String> {
  let n_clusters = choose_n_clusters(config);
  let ivf_config = IvfConfig::new(n_clusters)
    .with_n_probe(config.n_probe)
    .with_metric(config.metric);
  let mut index = IvfIndex::new(config.dimensions, ivf_config);

  let build_start = Instant::now();
  index
    .add_training_vectors(training_data, vector_ids.len())
    .map_err(|err| err.to_string())?;
  index.train().map_err(|err| err.to_string())?;
  for &vector_id in vector_ids {
    let vector = vector_store_vector_by_id(manifest, vector_id)
      .ok_or_else(|| format!("missing vector {vector_id}"))?;
    index
      .insert(vector_id, vector)
      .map_err(|err| err.to_string())?;
  }
  let build_elapsed_ms = build_start.elapsed().as_millis() as f64;

  let samples = measure_all(config, queries, exact, &[()], |_, query| {
    index
      .search(
        manifest,
        query,
        config.k,
        Some(SearchOptions {
          n_probe: Some(config.n_probe),
          ..Default::default()
        }),
      )
      .map_err(|err| err.to_string())
  })?;
  Ok((
    build_elapsed_ms,
    samples.into_iter().map(|s| s.finish(None)).collect(),
  ))
}

fn run_ivf_pq_bench(
  config: &BenchConfig,
  manifest: &VectorManifest,
  vector_ids: &[u64],
  training_data: &[f32],
  queries: &[Vec<f32>],
  exact: &[Vec<u64>],
) -> Result<(f64, Vec<Measurement>), String> {
  let n_clusters = choose_n_clusters(config);
  let ivf_pq_config = IvfPqConfig::new()
    .with_n_clusters(n_clusters)
    .with_n_probe(config.n_probe)
    .with_metric(config.metric)
    .with_num_subspaces(config.pq_subspaces)
    .with_num_centroids(config.pq_centroids)
    .with_residuals(config.residuals);
  let mut index =
    IvfPqIndex::new(config.dimensions, ivf_pq_config).map_err(|err| err.to_string())?;

  let build_start = Instant::now();
  index
    .add_training_vectors(training_data, vector_ids.len())
    .map_err(|err| err.to_string())?;
  index.train().map_err(|err| err.to_string())?;
  for &vector_id in vector_ids {
    let vector = vector_store_vector_by_id(manifest, vector_id)
      .ok_or_else(|| format!("missing vector {vector_id}"))?;
    index
      .insert(vector_id, vector)
      .map_err(|err| err.to_string())?;
  }
  let build_elapsed_ms = build_start.elapsed().as_millis() as f64;

  // One index, every requested re-rank factor (None: the library default).
  let factors: Vec<Option<usize>> = if config.rerank_factors.is_empty() {
    vec![None]
  } else {
    config.rerank_factors.clone()
  };
  let samples = measure_all(config, queries, exact, &factors, |&rerank_factor, query| {
    index
      .search(
        manifest,
        query,
        config.k,
        Some(IvfPqSearchOptions {
          n_probe: Some(config.n_probe),
          rerank_factor,
          ..Default::default()
        }),
      )
      .map_err(|err| err.to_string())
  })?;
  let measurements = factors
    .iter()
    .zip(samples)
    .map(|(rerank_factor, samples)| {
      let label = (config.rerank_factors.len() > 1).then(|| match rerank_factor {
        Some(factor) => factor.to_string(),
        None => "default".to_string(),
      });
      samples.finish(label)
    })
    .collect();
  Ok((build_elapsed_ms, measurements))
}

fn main() {
  let config = parse_args();
  let n_clusters = choose_n_clusters(&config);
  let mut rng = StdRng::seed_from_u64(config.seed);

  let source = VectorSource::new(&config, &mut rng);

  // The store normalizes on insert for cosine only, like VectorIndex.
  let store_config = VectorStoreConfig::new(config.dimensions).with_metric(config.metric);
  let mut manifest = create_vector_store(store_config);
  for node_id in 0..config.vectors {
    let vector = source.sample(&mut rng);
    vector_store_insert(&mut manifest, node_id as NodeId, &vector).expect("insert failed");
  }

  let (training_data, _node_ids, vector_ids) = vector_store_all_vectors(&manifest);
  let mut query_rng = StdRng::seed_from_u64(config.seed ^ 0xA5A5_5A5A_55AA_AA55);
  let queries: Vec<Vec<f32>> = (0..config.queries)
    .map(|_| source.sample(&mut query_rng))
    .collect();
  let exact: Vec<Vec<u64>> = queries
    .iter()
    .map(|query| exact_top_k(&manifest, query, config.k, config.metric))
    .collect();

  let result = match config.algorithm {
    Algorithm::Ivf => run_ivf_bench(
      &config,
      &manifest,
      &vector_ids,
      &training_data,
      &queries,
      &exact,
    ),
    Algorithm::IvfPq => run_ivf_pq_bench(
      &config,
      &manifest,
      &vector_ids,
      &training_data,
      &queries,
      &exact,
    ),
  };

  match result {
    Ok((build_ms, measurements)) => {
      println!("algorithm: {}", config.algorithm.as_str());
      println!("vectors: {}", config.vectors);
      println!("dimensions: {}", config.dimensions);
      println!("metric: {:?}", config.metric);
      println!("dataset: {}", config.dataset.as_str());
      if config.dataset != Dataset::Uniform {
        println!("blobs: {}", config.blobs);
      }
      println!("queries: {}", config.queries);
      println!("k: {}", config.k);
      println!("n_clusters: {}", n_clusters);
      println!("n_probe: {}", config.n_probe);
      if config.algorithm == Algorithm::IvfPq {
        println!("pq_subspaces: {}", config.pq_subspaces);
        println!("pq_centroids: {}", config.pq_centroids);
        println!("residuals: {}", config.residuals);
        if let [Some(factor)] = config.rerank_factors.as_slice() {
          println!("rerank_factor: {factor}");
        }
      }
      println!("build_elapsed_ms: {:.3}", build_ms);
      for measurement in measurements {
        if let Some(label) = &measurement.label {
          println!("rerank_factor: {label}");
        }
        println!(
          "search_p50_ms: {:.6}",
          measurement.p50_ns as f64 / 1_000_000.0
        );
        println!(
          "search_p95_ms: {:.6}",
          measurement.p95_ns as f64 / 1_000_000.0
        );
        println!("mean_recall_at_k: {:.6}", measurement.mean_recall);
      }
    }
    Err(err) => {
      eprintln!("benchmark_failed: {err}");
      std::process::exit(1);
    }
  }
}
