//! Vector Index Benchmark (Rust)
//!
//! Benchmarks vector index operations via the Rust API.
//! Intended for comparison with NAPI and Python vector benchmarks.
//!
//! Usage:
//!   cargo run --release --example vector_bench --no-default-features -- [options]
//!
//! Options:
//!   --vectors N        Number of vectors (default: 10000)
//!   --dimensions D     Vector dimensions (default: 768)
//!   --iterations I     Iterations for latency benchmarks (default: 1000)
//!   --k N              Number of nearest neighbors (default: 10)
//!   --n-probe N         IVF nProbe (default: 10)
//!   --output FILE      Output file path (default: auto-generated)
//!   --no-output        Disable file output

use rand::{rngs::StdRng, Rng, SeedableRng};
use kitedb::api::vector_search::{SimilarOptions, VectorIndex, VectorIndexOptions};
use kitedb::types::NodeId;
use kitedb::vector::DistanceMetric;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
struct BenchConfig {
  vectors: usize,
  dimensions: usize,
  iterations: usize,
  k: usize,
  n_probe: usize,
  output_file: Option<PathBuf>,
}

impl Default for BenchConfig {
  fn default() -> Self {
    Self {
      vectors: 10_000,
      dimensions: 768,
      iterations: 1000,
      k: 10,
      n_probe: 10,
      output_file: None,
    }
  }
}

fn parse_args() -> BenchConfig {
  let mut config = BenchConfig::default();
  let mut no_output = false;
  let args: Vec<String> = env::args().collect();

  let mut i = 1;
  while i < args.len() {
    match args[i].as_str() {
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
      "--iterations" => {
        if let Some(value) = args.get(i + 1) {
          config.iterations = value.parse().unwrap_or(config.iterations);
          i += 1;
        }
      }
      "--k" => {
        if let Some(value) = args.get(i + 1) {
          config.k = value.parse().unwrap_or(config.k);
          i += 1;
        }
      }
      "--n-probe" => {
        if let Some(value) = args.get(i + 1) {
          config.n_probe = value.parse().unwrap_or(config.n_probe);
          i += 1;
        }
      }
      "--output" => {
        if let Some(value) = args.get(i + 1) {
          config.output_file = Some(PathBuf::from(value));
          i += 1;
        }
      }
      "--no-output" => {
        no_output = true;
      }
      _ => {}
    }
    i += 1;
  }

  if !no_output && config.output_file.is_none() {
    let timestamp = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .unwrap_or_default()
      .as_secs();
    let default_path = Path::new("..")
      .join("bench")
      .join("results")
      .join(format!("benchmark-vector-rust-{timestamp}.txt"));
    config.output_file = Some(default_path);
  }

  if no_output {
    config.output_file = None;
  }

  config
}

struct Logger {
  output_file: Option<PathBuf>,
  buffer: Vec<String>,
}

impl Logger {
  fn new(output_file: Option<PathBuf>) -> Self {
    Self {
      output_file,
      buffer: Vec::new(),
    }
  }

  fn log(&mut self, message: &str) {
    println!("{message}");
    self.buffer.push(message.to_string());
  }

  fn flush(&self) {
    if let Some(path) = &self.output_file {
      if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
      }
      let _ = fs::write(path, format!("{}\n", self.buffer.join("\n")));
    }
  }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
struct LatencyStats {
  count: usize,
  min: u128,
  max: u128,
  sum: u128,
  p50: u128,
  p95: u128,
  p99: u128,
}

fn latency_stats(samples: &[u128]) -> LatencyStats {
  if samples.is_empty() {
    return LatencyStats {
      count: 0,
      min: 0,
      max: 0,
      sum: 0,
      p50: 0,
      p95: 0,
      p99: 0,
    };
  }

  let mut sorted = samples.to_vec();
  sorted.sort_unstable();
  let count = sorted.len();
  let sum = sorted.iter().sum();
  let idx = |pct: f64| -> usize { ((count as f64) * pct).floor().min((count - 1) as f64) as usize };

  LatencyStats {
    count,
    min: sorted[0],
    max: sorted[count - 1],
    sum,
    p50: sorted[idx(0.50)],
    p95: sorted[idx(0.95)],
    p99: sorted[idx(0.99)],
  }
}

fn format_latency(ns: u128) -> String {
  if ns < 1_000 {
    format!("{ns}ns")
  } else if ns < 1_000_000 {
    format!("{:.2}us", ns as f64 / 1_000.0)
  } else {
    format!("{:.2}ms", ns as f64 / 1_000_000.0)
  }
}

fn format_number(n: usize) -> String {
  let mut s = n.to_string();
  let mut i = s.len() as isize - 3;
  while i > 0 {
    s.insert(i as usize, ',');
    i -= 3;
  }
  s
}

fn print_latency_table(logger: &mut Logger, name: &str, stats: &LatencyStats) {
  let ops_per_sec = if stats.sum > 0 {
    stats.count as f64 / (stats.sum as f64 / 1_000_000_000.0)
  } else {
    0.0
  };
  logger.log(&format!(
    "{:<40} p50={:>10} p95={:>10} p99={:>10} ({} ops/sec)",
    name,
    format_latency(stats.p50),
    format_latency(stats.p95),
    format_latency(stats.p99),
    format_number(ops_per_sec.round() as usize)
  ));
}

fn random_vector(rng: &mut StdRng, dims: usize) -> Vec<f32> {
  let mut vec = vec![0.0f32; dims];
  for v in &mut vec {
    *v = rng.gen_range(-1.0f32..1.0f32);
  }
  vec
}

fn main() {
  let config = parse_args();
  let mut logger = Logger::new(config.output_file.clone());

  logger.log(&"=".repeat(120));
  logger.log("Vector Index Benchmark (Rust)");
  logger.log(&"=".repeat(120));
  logger.log(&format!("Vectors: {}", format_number(config.vectors)));
  logger.log(&format!("Dimensions: {}", config.dimensions));
  logger.log(&format!("Iterations: {}", format_number(config.iterations)));
  logger.log(&format!("k: {}", config.k));
  logger.log(&format!("nProbe: {}", config.n_probe));
  logger.log(&"=".repeat(120));

  let mut rng = StdRng::seed_from_u64(42);
  let mut index = VectorIndex::new(
    VectorIndexOptions::new(config.dimensions)
      .with_metric(DistanceMetric::Cosine)
      .with_n_probe(config.n_probe),
  );

  let mut vectors: Vec<Vec<f32>> = Vec::with_capacity(config.vectors);
  for _ in 0..config.vectors {
    vectors.push(random_vector(&mut rng, config.dimensions));
  }

  logger.log("\n--- Vector Index Benchmarks (Rust) ---");

  logger.log("\n  Insert benchmarks:");
  let mut insert_samples = Vec::with_capacity(config.vectors);
  let insert_start = Instant::now();
  for (i, vector) in vectors.iter().enumerate() {
    let start = Instant::now();
    index.set(i as NodeId, vector).expect("set should succeed");
    insert_samples.push(start.elapsed().as_nanos());
  }
  let insert_total = insert_start.elapsed().as_nanos();
  let insert_stats = latency_stats(&insert_samples);
  print_latency_table(
    &mut logger,
    &format!("Set ({} vectors)", format_number(config.vectors)),
    &insert_stats,
  );
  logger.log(&format!(
    "  Total set time: {} ({} vectors/sec)",
    format_latency(insert_total),
    format_number(
      ((config.vectors as f64 * 1_000_000_000.0) / insert_total as f64).round() as usize
    )
  ));

  logger.log("\n  Index build:");
  let build_start = Instant::now();
  index.build_index().expect("build_index should succeed");
  let build_time = build_start.elapsed().as_nanos();
  logger.log(&format!("  build_index(): {}", format_latency(build_time)));

  logger.log("\n  Lookup benchmarks:");
  let mut lookup_samples = Vec::with_capacity(config.iterations);
  for _ in 0..config.iterations {
    let node_id = rng.gen_range(0..config.vectors) as NodeId;
    let start = Instant::now();
    let _ = index.get(node_id);
    lookup_samples.push(start.elapsed().as_nanos());
  }
  let lookup_stats = latency_stats(&lookup_samples);
  print_latency_table(&mut logger, "Random get", &lookup_stats);

  logger.log("\n  Search benchmarks:");
  let mut search_samples = Vec::with_capacity(config.iterations);
  for _ in 0..config.iterations {
    let query = random_vector(&mut rng, config.dimensions);
    let start = Instant::now();
    let _ = index
      .search(
        &query,
        SimilarOptions::new(config.k).with_n_probe(config.n_probe),
      )
      .expect("search should succeed");
    search_samples.push(start.elapsed().as_nanos());
  }
  let search_stats = latency_stats(&search_samples);
  print_latency_table(
    &mut logger,
    &format!("Search (k={}, nProbe={})", config.k, config.n_probe),
    &search_stats,
  );

  let stats = index.stats();
  logger.log("\n  Index stats:");
  logger.log(&format!(
    "    Total vectors: {}",
    format_number(stats.total_vectors)
  ));
  logger.log(&format!(
    "    Live vectors: {}",
    format_number(stats.live_vectors)
  ));
  logger.log(&format!("    Dimensions: {}", stats.dimensions));
  logger.log(&format!("    Metric: {:?}", stats.metric));
  logger.log(&format!("    Index trained: {}", stats.index_trained));
  if let Some(clusters) = stats.index_clusters {
    logger.log(&format!("    Index clusters: {clusters}"));
  }

  logger.log(&format!("\n{}", "=".repeat(120)));
  logger.log("Vector benchmark complete.");
  logger.log(&"=".repeat(120));

  logger.flush();
  if let Some(path) = &config.output_file {
    println!("\nResults saved to: {}", path.display());
  }
}
