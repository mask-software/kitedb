//! ANN algorithm benchmark (IVF vs IVF-PQ)
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
//!   --residuals true|false             Use residual encoding for IVF-PQ (default: true)
//!   --seed N                           RNG seed (default: 42)

use kitedb::types::NodeId;
use kitedb::vector::{
  create_vector_store, normalize, vector_store_all_vectors, vector_store_insert,
  vector_store_vector_by_id, DistanceMetric, IvfConfig, IvfIndex, IvfPqConfig, IvfPqIndex,
  IvfPqSearchOptions, SearchOptions, VectorManifest, VectorSearchResult, VectorStoreConfig,
};
use rand::{rngs::StdRng, Rng, SeedableRng};
use std::cmp::Ordering;
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
  config
}

fn random_vector(rng: &mut StdRng, dimensions: usize) -> Vec<f32> {
  let mut vector = vec![0.0f32; dimensions];
  for value in &mut vector {
    *value = rng.gen_range(-1.0f32..1.0f32);
  }
  vector
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

  candidates.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal));
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

fn run_ivf_bench(
  config: &BenchConfig,
  manifest: &VectorManifest,
  vector_ids: &[u64],
  training_data: &[f32],
  queries: &[Vec<f32>],
) -> Result<(f64, u128, u128, f64), String> {
  let n_clusters = choose_n_clusters(config);
  let ivf_config = IvfConfig::new(n_clusters)
    .with_n_probe(config.n_probe)
    .with_metric(DistanceMetric::Cosine);
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

  let mut latency_ns: Vec<u128> = Vec::with_capacity(queries.len());
  let mut recall_sum = 0.0f64;
  for query in queries {
    let exact = exact_top_k(manifest, query, config.k, DistanceMetric::Cosine);
    let start = Instant::now();
    let approx = index.search(
      manifest,
      query,
      config.k,
      Some(SearchOptions {
        n_probe: Some(config.n_probe),
        filter: None,
        threshold: None,
      }),
    );
    latency_ns.push(start.elapsed().as_nanos());
    recall_sum += recall_at_k(&approx, &exact, config.k);
  }
  latency_ns.sort_unstable();
  let p50 = percentile(&latency_ns, 0.50);
  let p95 = percentile(&latency_ns, 0.95);
  let mean_recall = recall_sum / queries.len() as f64;

  Ok((build_elapsed_ms, p50, p95, mean_recall))
}

fn run_ivf_pq_bench(
  config: &BenchConfig,
  manifest: &VectorManifest,
  vector_ids: &[u64],
  training_data: &[f32],
  queries: &[Vec<f32>],
) -> Result<(f64, u128, u128, f64), String> {
  let n_clusters = choose_n_clusters(config);
  let ivf_pq_config = IvfPqConfig::new()
    .with_n_clusters(n_clusters)
    .with_n_probe(config.n_probe)
    .with_metric(DistanceMetric::Cosine)
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

  let mut latency_ns: Vec<u128> = Vec::with_capacity(queries.len());
  let mut recall_sum = 0.0f64;
  for query in queries {
    let exact = exact_top_k(manifest, query, config.k, DistanceMetric::Cosine);
    let start = Instant::now();
    let approx = index.search(
      manifest,
      query,
      config.k,
      Some(IvfPqSearchOptions {
        n_probe: Some(config.n_probe),
        filter: None,
        threshold: None,
      }),
    );
    latency_ns.push(start.elapsed().as_nanos());
    recall_sum += recall_at_k(&approx, &exact, config.k);
  }
  latency_ns.sort_unstable();
  let p50 = percentile(&latency_ns, 0.50);
  let p95 = percentile(&latency_ns, 0.95);
  let mean_recall = recall_sum / queries.len() as f64;

  Ok((build_elapsed_ms, p50, p95, mean_recall))
}

fn main() {
  let config = parse_args();
  let n_clusters = choose_n_clusters(&config);
  let mut rng = StdRng::seed_from_u64(config.seed);

  let store_config = VectorStoreConfig::new(config.dimensions)
    .with_metric(DistanceMetric::Cosine)
    .with_normalize(true);
  let mut manifest = create_vector_store(store_config);
  for node_id in 0..config.vectors {
    let vector = random_vector(&mut rng, config.dimensions);
    vector_store_insert(&mut manifest, node_id as NodeId, &vector).expect("insert failed");
  }

  let (training_data, _node_ids, vector_ids) = vector_store_all_vectors(&manifest);
  let mut query_rng = StdRng::seed_from_u64(config.seed ^ 0xA5A5_5A5A_55AA_AA55);
  let queries: Vec<Vec<f32>> = (0..config.queries)
    .map(|_| random_vector(&mut query_rng, config.dimensions))
    .collect();

  let result = match config.algorithm {
    Algorithm::Ivf => run_ivf_bench(&config, &manifest, &vector_ids, &training_data, &queries),
    Algorithm::IvfPq => run_ivf_pq_bench(&config, &manifest, &vector_ids, &training_data, &queries),
  };

  match result {
    Ok((build_ms, p50_ns, p95_ns, mean_recall)) => {
      println!("algorithm: {}", config.algorithm.as_str());
      println!("vectors: {}", config.vectors);
      println!("dimensions: {}", config.dimensions);
      println!("queries: {}", config.queries);
      println!("k: {}", config.k);
      println!("n_clusters: {}", n_clusters);
      println!("n_probe: {}", config.n_probe);
      if config.algorithm == Algorithm::IvfPq {
        println!("pq_subspaces: {}", config.pq_subspaces);
        println!("pq_centroids: {}", config.pq_centroids);
        println!("residuals: {}", config.residuals);
      }
      println!("build_elapsed_ms: {:.3}", build_ms);
      println!("search_p50_ms: {:.6}", p50_ns as f64 / 1_000_000.0);
      println!("search_p95_ms: {:.6}", p95_ns as f64 / 1_000_000.0);
      println!("mean_recall_at_k: {:.6}", mean_recall);
    }
    Err(err) => {
      eprintln!("benchmark_failed: {err}");
      std::process::exit(1);
    }
  }
}
