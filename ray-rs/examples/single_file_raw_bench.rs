//! Single-file raw benchmark for KiteDB core (Rust)
//!
//! Usage:
//!   cargo run --release --example single_file_raw_bench --no-default-features -- [options]
//!
//! Options:
//!   --nodes N                 Number of nodes (default: 10000)
//!   --edges M                 Number of edges (default: 50000)
//!   --iterations I            Iterations for latency benchmarks (default: 10000)
//!   --wal-size BYTES          WAL size in bytes (default: 67108864)
//!   --checkpoint-threshold P  Auto-checkpoint threshold (default: 0.8)
//!   --no-auto-checkpoint      Disable auto-checkpoint
//!   --vector-dims N            Vector dimensions (default: 128)
//!   --vector-count N           Number of vectors to set (default: 1000)
//!   --keep-db                 Keep the database file after benchmark

use rand::{rngs::StdRng, Rng, SeedableRng};
use std::env;
use std::time::Instant;
use tempfile::tempdir;

use kitedb::core::single_file::{close_single_file, open_single_file, SingleFileOpenOptions};

#[derive(Debug, Clone)]
struct BenchConfig {
  nodes: usize,
  edges: usize,
  iterations: usize,
  wal_size: usize,
  checkpoint_threshold: f64,
  auto_checkpoint: bool,
  vector_dims: usize,
  vector_count: usize,
  keep_db: bool,
  skip_checkpoint: bool,
  reopen_readonly: bool,
}

impl Default for BenchConfig {
  fn default() -> Self {
    Self {
      nodes: 10_000,
      edges: 50_000,
      iterations: 10_000,
      wal_size: 64 * 1024 * 1024,
      checkpoint_threshold: 0.8,
      auto_checkpoint: true,
      vector_dims: 128,
      vector_count: 1000,
      keep_db: false,
      skip_checkpoint: false,
      reopen_readonly: false,
    }
  }
}

fn parse_args() -> BenchConfig {
  let mut config = BenchConfig::default();
  let args: Vec<String> = env::args().collect();

  let mut i = 1;
  while i < args.len() {
    match args[i].as_str() {
      "--nodes" => {
        if let Some(value) = args.get(i + 1) {
          config.nodes = value.parse().unwrap_or(config.nodes);
          i += 1;
        }
      }
      "--edges" => {
        if let Some(value) = args.get(i + 1) {
          config.edges = value.parse().unwrap_or(config.edges);
          i += 1;
        }
      }
      "--iterations" => {
        if let Some(value) = args.get(i + 1) {
          config.iterations = value.parse().unwrap_or(config.iterations);
          i += 1;
        }
      }
      "--wal-size" => {
        if let Some(value) = args.get(i + 1) {
          config.wal_size = value.parse().unwrap_or(config.wal_size);
          i += 1;
        }
      }
      "--checkpoint-threshold" => {
        if let Some(value) = args.get(i + 1) {
          config.checkpoint_threshold = value.parse().unwrap_or(config.checkpoint_threshold);
          i += 1;
        }
      }
      "--no-auto-checkpoint" => {
        config.auto_checkpoint = false;
      }
      "--vector-dims" => {
        if let Some(value) = args.get(i + 1) {
          config.vector_dims = value.parse().unwrap_or(config.vector_dims);
          i += 1;
        }
      }
      "--vector-count" => {
        if let Some(value) = args.get(i + 1) {
          config.vector_count = value.parse().unwrap_or(config.vector_count);
          i += 1;
        }
      }
      "--skip-checkpoint" => {
        config.skip_checkpoint = true;
      }
      "--reopen-readonly" => {
        config.reopen_readonly = true;
      }
      "--keep-db" => {
        config.keep_db = true;
      }
      _ => {}
    }
    i += 1;
  }

  config
}

#[derive(Debug, Clone, Copy)]
struct LatencyStats {
  count: usize,
  max: u128,
  sum: u128,
  p50: u128,
  p95: u128,
  p99: u128,
}

fn compute_stats(samples: &mut [u128]) -> LatencyStats {
  if samples.is_empty() {
    return LatencyStats {
      count: 0,
      max: 0,
      sum: 0,
      p50: 0,
      p95: 0,
      p99: 0,
    };
  }

  samples.sort_unstable();
  let count = samples.len();
  let max = samples[count - 1];
  let sum: u128 = samples.iter().copied().sum();

  let p50 = samples[(count as f64 * 0.50).floor() as usize];
  let p95 = samples[(count as f64 * 0.95).floor() as usize];
  let p99 = samples[(count as f64 * 0.99).floor() as usize];

  LatencyStats {
    count,
    max,
    sum,
    p50,
    p95,
    p99,
  }
}

fn format_latency(ns: u128) -> String {
  if ns < 1_000 {
    return format!("{ns}ns");
  }
  if ns < 1_000_000 {
    return format!("{:.2}us", ns as f64 / 1_000.0);
  }
  format!("{:.2}ms", ns as f64 / 1_000_000.0)
}

fn format_number(n: usize) -> String {
  let s = n.to_string();
  let mut out = String::new();
  for (count, ch) in s.chars().rev().enumerate() {
    if count > 0 && count % 3 == 0 {
      out.push(',');
    }
    out.push(ch);
  }
  out.chars().rev().collect()
}

fn print_latency_table(name: &str, stats: LatencyStats) {
  let ops_per_sec = if stats.sum > 0 {
    stats.count as f64 / (stats.sum as f64 / 1_000_000_000.0)
  } else {
    0.0
  };
  println!(
    "{:<45} p50={:>10} p95={:>10} p99={:>10} max={:>10} ({:.0} ops/sec)",
    name,
    format_latency(stats.p50),
    format_latency(stats.p95),
    format_latency(stats.p99),
    format_latency(stats.max),
    ops_per_sec
  );
}

fn build_random_vector(rng: &mut StdRng, dimensions: usize) -> Vec<f32> {
  let mut values = Vec::with_capacity(dimensions);
  for _ in 0..dimensions {
    values.push(rng.gen());
  }
  values
}

struct GraphData {
  node_ids: Vec<u64>,
  node_keys: Vec<String>,
  etype_calls: u32,
}

fn build_graph(db: &kitedb::core::single_file::SingleFileDB, config: &BenchConfig) -> GraphData {
  let mut node_ids = Vec::with_capacity(config.nodes);
  let mut node_keys = Vec::with_capacity(config.nodes);
  let batch_size = 5_000usize;

  println!("  Creating nodes...");
  let mut etype_calls = 0u32;
  for batch_start in (0..config.nodes).step_by(batch_size) {
    let end = (batch_start + batch_size).min(config.nodes);
    db.begin(false).unwrap();

    if batch_start == 0 {
      etype_calls = db.define_etype("CALLS").unwrap();
    }

    for i in batch_start..end {
      let key = format!("pkg.module{}.Class{}", i / 100, i % 100);
      let node_id = db.create_node(Some(&key)).unwrap();
      node_ids.push(node_id);
      node_keys.push(key);
    }

    db.commit().unwrap();
    print!("\r  Created {} / {} nodes", end, config.nodes);
  }
  println!();

  println!("  Creating edges...");
  let mut edges_created = 0usize;
  let mut attempts = 0usize;
  let max_attempts = config.edges * 3;
  let mut rng = StdRng::from_entropy();

  while edges_created < config.edges && attempts < max_attempts {
    let batch_target = (edges_created + batch_size).min(config.edges);
    db.begin(false).unwrap();

    while edges_created < batch_target && attempts < max_attempts {
      attempts += 1;
      let src = node_ids[rng.gen_range(0..node_ids.len())];
      let dst = node_ids[rng.gen_range(0..node_ids.len())];
      if src != dst {
        db.add_edge(src, etype_calls, dst).unwrap();
        edges_created += 1;
      }
    }

    db.commit().unwrap();
    print!("\r  Created {} / {} edges", edges_created, config.edges);
  }
  println!();

  GraphData {
    node_ids,
    node_keys,
    etype_calls,
  }
}

fn benchmark_key_lookups(
  db: &kitedb::core::single_file::SingleFileDB,
  graph: &GraphData,
  iterations: usize,
) {
  println!("\n--- Key Lookups (get_node_by_key) ---");
  let mut rng = StdRng::from_entropy();
  let mut samples = Vec::with_capacity(iterations);

  for _ in 0..iterations {
    let key = &graph.node_keys[rng.gen_range(0..graph.node_keys.len())];
    let start = Instant::now();
    let _ = db.get_node_by_key(key);
    samples.push(start.elapsed().as_nanos());
  }

  let stats = compute_stats(&mut samples);
  print_latency_table("Random existing keys", stats);
}

fn benchmark_traversals(
  db: &kitedb::core::single_file::SingleFileDB,
  graph: &GraphData,
  iterations: usize,
) {
  println!("\n--- 1-Hop Traversals (out) ---");
  let mut rng = StdRng::from_entropy();
  let mut samples = Vec::with_capacity(iterations);

  for _ in 0..iterations {
    let node = graph.node_ids[rng.gen_range(0..graph.node_ids.len())];
    let start = Instant::now();
    let edges = db.get_out_edges(node);
    let _count = edges.len();
    samples.push(start.elapsed().as_nanos());
  }

  let stats = compute_stats(&mut samples);
  print_latency_table("Random nodes", stats);
}

fn benchmark_edge_exists(
  db: &kitedb::core::single_file::SingleFileDB,
  graph: &GraphData,
  iterations: usize,
) {
  println!("\n--- Edge Exists ---");
  let mut rng = StdRng::from_entropy();
  let mut samples = Vec::with_capacity(iterations);

  for _ in 0..iterations {
    let src = graph.node_ids[rng.gen_range(0..graph.node_ids.len())];
    let dst = graph.node_ids[rng.gen_range(0..graph.node_ids.len())];
    let start = Instant::now();
    let _ = db.edge_exists(src, graph.etype_calls, dst);
    samples.push(start.elapsed().as_nanos());
  }

  let stats = compute_stats(&mut samples);
  print_latency_table("Random edge exists", stats);
}

fn benchmark_vectors(
  db: &kitedb::core::single_file::SingleFileDB,
  graph: &GraphData,
  config: &BenchConfig,
) -> Option<(u32, Vec<u64>)> {
  if config.vector_count == 0 || config.vector_dims == 0 {
    println!("\n--- Vector Operations ---");
    println!("  Skipped (vector_count/vector_dims == 0)");
    return None;
  }

  println!("\n--- Vector Operations ---");
  let vector_count = config.vector_count.min(graph.node_ids.len());
  let vector_nodes = graph.node_ids[..vector_count].to_vec();

  db.begin(false).unwrap();
  let prop_key_id = db.define_propkey("embedding").unwrap();
  db.commit().unwrap();

  let mut rng = StdRng::from_entropy();
  let vectors: Vec<Vec<f32>> = (0..vector_count)
    .map(|_| build_random_vector(&mut rng, config.vector_dims))
    .collect();

  let batch_size = 100usize;
  let mut samples = Vec::new();

  let mut i = 0;
  while i < vector_nodes.len() {
    let end = (i + batch_size).min(vector_nodes.len());
    let start = Instant::now();
    db.begin(false).unwrap();
    for j in i..end {
      db.set_node_vector(vector_nodes[j], prop_key_id, &vectors[j])
        .unwrap();
    }
    db.commit().unwrap();
    samples.push(start.elapsed().as_nanos());
    i = end;
  }

  let stats = compute_stats(&mut samples);
  print_latency_table(&format!("Set vectors (batch {batch_size})"), stats);

  Some((prop_key_id, vector_nodes))
}

fn benchmark_vector_reads(
  db: &kitedb::core::single_file::SingleFileDB,
  vector_nodes: &[u64],
  prop_key_id: u32,
  iterations: usize,
) {
  let mut rng = StdRng::from_entropy();
  let mut samples = Vec::with_capacity(iterations);
  for _ in 0..iterations {
    let node = vector_nodes[rng.gen_range(0..vector_nodes.len())];
    let start = Instant::now();
    let _ = db.get_node_vector(node, prop_key_id);
    samples.push(start.elapsed().as_nanos());
  }
  let stats = compute_stats(&mut samples);
  print_latency_table("get_node_vector() random", stats);

  let mut samples = Vec::with_capacity(iterations);
  for _ in 0..iterations {
    let node = vector_nodes[rng.gen_range(0..vector_nodes.len())];
    let start = Instant::now();
    let _ = db.has_node_vector(node, prop_key_id);
    samples.push(start.elapsed().as_nanos());
  }
  let stats = compute_stats(&mut samples);
  print_latency_table("has_node_vector() random", stats);
}

fn benchmark_writes(db: &kitedb::core::single_file::SingleFileDB, iterations: usize) {
  println!("\n--- Batch Writes (100 nodes) ---");
  let batch_size = 100usize;
  let batches = (iterations / batch_size).min(50);
  let mut samples = Vec::with_capacity(batches);

  for b in 0..batches {
    let start = Instant::now();
    db.begin(false).unwrap();
    for i in 0..batch_size {
      let key = format!("bench:raw:{b}:{i}");
      let _ = db.create_node(Some(&key)).unwrap();
    }
    db.commit().unwrap();
    samples.push(start.elapsed().as_nanos());
  }

  let stats = compute_stats(&mut samples);
  print_latency_table("Batch of 100 nodes", stats);
}

fn main() {
  let config = parse_args();

  println!("{}", "=".repeat(120));
  println!("Single-file Raw Benchmark (Rust)");
  println!("{}", "=".repeat(120));
  println!("Nodes: {}", format_number(config.nodes));
  println!("Edges: {}", format_number(config.edges));
  println!("Iterations: {}", format_number(config.iterations));
  println!("WAL size: {} bytes", format_number(config.wal_size));
  println!("Auto-checkpoint: {}", config.auto_checkpoint);
  println!("Checkpoint threshold: {}", config.checkpoint_threshold);
  println!("Vector dims: {}", format_number(config.vector_dims));
  println!("Vector count: {}", format_number(config.vector_count));
  println!("Skip checkpoint: {}", config.skip_checkpoint);
  println!("Reopen read-only: {}", config.reopen_readonly);
  println!("{}", "=".repeat(120));

  let temp = tempdir().expect("failed to create temp dir");
  let db_path = temp.path().join("ray-bench-raw.kitedb");

  let options = SingleFileOpenOptions::new()
    .wal_size(config.wal_size)
    .auto_checkpoint(config.auto_checkpoint)
    .checkpoint_threshold(config.checkpoint_threshold)
    .sync_normal();

  let mut db = open_single_file(&db_path, options).expect("failed to open single-file db");

  println!("\n[1/6] Building graph...");
  let start_build = Instant::now();
  let graph = build_graph(&db, &config);
  println!("  Built in {}ms", start_build.elapsed().as_millis());

  println!("\n[2/6] Vector setup...");
  let vector_setup = benchmark_vectors(&db, &graph, &config);

  println!("\n[3/6] Checkpointing...");
  if config.skip_checkpoint {
    println!("  Skipped checkpoint");
  } else {
    let start_cp = Instant::now();
    db.checkpoint().expect("checkpoint failed");
    println!("  Checkpointed in {}ms", start_cp.elapsed().as_millis());
  }

  if config.reopen_readonly {
    close_single_file(db).expect("failed to close db before reopen");
    let read_options = SingleFileOpenOptions::new()
      .read_only(true)
      .create_if_missing(false);
    db = open_single_file(&db_path, read_options).expect("failed to reopen db");
    println!("  Re-opened database in read-only mode");
  }

  println!("\n[4/6] Key lookup benchmarks...");
  benchmark_key_lookups(&db, &graph, config.iterations);

  println!("\n[5/6] Traversal and edge benchmarks...");
  benchmark_traversals(&db, &graph, config.iterations);
  benchmark_edge_exists(&db, &graph, config.iterations);

  if let Some((prop_key_id, vector_nodes)) = vector_setup {
    if !vector_nodes.is_empty() {
      benchmark_vector_reads(&db, &vector_nodes, prop_key_id, config.iterations);
    }
  }

  println!("\n[6/6] Write benchmarks...");
  if config.reopen_readonly {
    println!("  Skipped write benchmarks (read-only)");
  } else {
    benchmark_writes(&db, config.iterations);
  }

  close_single_file(db).expect("failed to close db");

  if config.keep_db {
    println!("\nDatabase preserved at: {}", db_path.display());
  }
}
