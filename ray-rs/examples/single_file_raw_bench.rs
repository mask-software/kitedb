//! Single-file raw benchmark for KiteDB core (Rust)
//!
//! Usage:
//!   cargo run --release --example single_file_raw_bench --no-default-features -- [options]
//!
//! Options:
//!   --nodes N                 Number of nodes (default: 10000)
//!   --edges M                 Number of edges (default: 50000)
//!   --iterations I            Iterations for latency benchmarks (default: 10000)
//!   --node-batches N          Batches in the 100-node write benchmark (default: iterations/100 rounded up, at most 50)
//!   --wal-size BYTES          WAL size in bytes (default: 67108864)
//!   --sync-mode MODE          Sync mode: full|normal|off (default: normal)
//!   --group-commit-enabled, --group-commit-window-ms N
//!                             Accepted for old command lines; no effect (every
//!                             commit is group-committed)
//!   --edge-types N            Number of edge types (default: 3)
//!   --edge-props N            Number of props per edge (default: 10)
//!   --checkpoint-log-ratio R  Checkpoint once the log reaches R x the snapshot (default: 0.5)
//!   --no-auto-checkpoint      Disable auto-checkpoint
//!   --vector-dims N            Vector dimensions (default: 128)
//!   --vector-count N           Number of vectors to set (default: 1000)
//!   --replication-primary      Enable primary replication sidecar on open options
//!   --replication-segment-max-bytes BYTES  Primary segment rotation threshold when replication is enabled
//!   --mvcc | --no-mvcc        MVCC mode (default: the library default)
//!   --skip-checkpoint         Skip the checkpoint after the graph build
//!   --reopen-readonly         Re-open the database read-only after the checkpoint
//!                             (skips the write benchmarks)
//!   --seed N                  Seed for the graph, vectors and the keys and nodes
//!                             each read benchmark picks (default: 42)
//!   --keep-db                 Keep the database file after benchmark
//!
//! Unknown options are an error.

use rand::{rngs::StdRng, Rng, SeedableRng};
use std::env;
use std::process::exit;
use std::time::Instant;
use tempfile::tempdir;

use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileOpenOptions, SyncMode,
};
use kitedb::replication::types::ReplicationRole;
use kitedb::types::PropValue;

#[derive(Debug, Clone)]
struct BenchConfig {
  nodes: usize,
  edges: usize,
  edge_types: usize,
  edge_props: usize,
  iterations: usize,
  node_batches: Option<usize>,
  wal_size: usize,
  sync_mode: SyncMode,
  /// None: the library default.
  checkpoint_log_ratio: Option<f64>,
  auto_checkpoint: bool,
  vector_dims: usize,
  vector_count: usize,
  replication_primary: bool,
  replication_segment_max_bytes: Option<u64>,
  /// None: the library default.
  mvcc: Option<bool>,
  seed: u64,
  keep_db: bool,
  skip_checkpoint: bool,
  reopen_readonly: bool,
}

impl Default for BenchConfig {
  fn default() -> Self {
    Self {
      nodes: 10_000,
      edges: 50_000,
      edge_types: 3,
      edge_props: 10,
      iterations: 10_000,
      node_batches: None,
      wal_size: 64 * 1024 * 1024,
      sync_mode: SyncMode::Normal,
      checkpoint_log_ratio: None,
      auto_checkpoint: true,
      vector_dims: 128,
      vector_count: 1000,
      replication_primary: false,
      replication_segment_max_bytes: None,
      mvcc: None,
      seed: 42,
      keep_db: false,
      skip_checkpoint: false,
      reopen_readonly: false,
    }
  }
}

fn usage_error(message: &str) -> ! {
  eprintln!("error: {message}");
  eprintln!("see the header of examples/single_file_raw_bench.rs for the options");
  exit(2);
}

fn value<T: std::str::FromStr>(args: &[String], i: &mut usize, flag: &str) -> T {
  *i += 1;
  let Some(raw) = args.get(*i) else {
    usage_error(&format!("{flag} needs a value"));
  };
  raw
    .parse()
    .unwrap_or_else(|_| usage_error(&format!("invalid value for {flag}: {raw}")))
}

fn parse_args() -> BenchConfig {
  let mut config = BenchConfig::default();
  let args: Vec<String> = env::args().collect();

  let mut i = 1;
  while i < args.len() {
    let flag = args[i].as_str();
    match flag {
      "--nodes" => config.nodes = value(&args, &mut i, flag),
      "--edges" => config.edges = value(&args, &mut i, flag),
      "--edge-types" => config.edge_types = value(&args, &mut i, flag),
      "--edge-props" => config.edge_props = value(&args, &mut i, flag),
      "--iterations" => config.iterations = value(&args, &mut i, flag),
      "--node-batches" => config.node_batches = Some(value(&args, &mut i, flag)),
      "--wal-size" => config.wal_size = value(&args, &mut i, flag),
      "--sync-mode" => {
        let mode: String = value(&args, &mut i, flag);
        config.sync_mode = match mode.to_lowercase().as_str() {
          "full" => SyncMode::Full,
          "normal" => SyncMode::Normal,
          "off" => SyncMode::Off,
          other => usage_error(&format!(
            "--sync-mode must be full, normal or off, not {other}"
          )),
        };
      }
      // No effect since every commit is group-committed; accepted so old
      // command lines still run.
      "--group-commit-enabled" => {}
      "--group-commit-window-ms" => {
        let _: u64 = value(&args, &mut i, flag);
      }
      "--checkpoint-log-ratio" => config.checkpoint_log_ratio = Some(value(&args, &mut i, flag)),
      "--no-auto-checkpoint" => config.auto_checkpoint = false,
      "--vector-dims" => config.vector_dims = value(&args, &mut i, flag),
      "--vector-count" => config.vector_count = value(&args, &mut i, flag),
      "--replication-primary" => config.replication_primary = true,
      "--replication-segment-max-bytes" => {
        let bytes: u64 = value(&args, &mut i, flag);
        config.replication_segment_max_bytes = (bytes > 0).then_some(bytes);
      }
      "--mvcc" => config.mvcc = Some(true),
      "--no-mvcc" => config.mvcc = Some(false),
      "--seed" => config.seed = value(&args, &mut i, flag),
      "--skip-checkpoint" => config.skip_checkpoint = true,
      "--reopen-readonly" => config.reopen_readonly = true,
      "--keep-db" => config.keep_db = true,
      other => usage_error(&format!("unknown option {other}")),
    }
    i += 1;
  }

  if config.edge_types == 0 {
    config.edge_types = 1;
  }

  config
}

/// The MVCC mode the bench runs in, for its header.
fn mvcc_label(requested: Option<bool>) -> String {
  let on = requested.unwrap_or(SingleFileOpenOptions::new().mvcc);
  format!(
    "{}{}",
    if on { "on" } else { "off" },
    if requested.is_none() {
      " (library default)"
    } else {
      ""
    }
  )
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

fn format_sync_mode(mode: SyncMode) -> &'static str {
  match mode {
    SyncMode::Full => "Full",
    SyncMode::Normal => "Normal",
    SyncMode::Off => "Off",
  }
}

fn print_latency_table(name: &str, stats: LatencyStats) {
  println!("{}", latency_row(name, &stats));
}

fn latency_row(name: &str, stats: &LatencyStats) -> String {
  if stats.count == 0 {
    return format!("{name:<45} skipped (no samples)");
  }
  let ops_per_sec = if stats.sum > 0 {
    stats.count as f64 / (stats.sum as f64 / 1_000_000_000.0)
  } else {
    0.0
  };
  format!(
    "{:<45} p50={:>10} p95={:>10} p99={:>10} max={:>10} ({:.0} ops/sec)",
    name,
    format_latency(stats.p50),
    format_latency(stats.p95),
    format_latency(stats.p99),
    format_latency(stats.max),
    ops_per_sec
  )
}

/// Batches of `batch_size` operations for an `--iterations` budget: rounded
/// up, so any budget runs at least one batch, and at most `max_batches`.
fn batch_count(iterations: usize, batch_size: usize, max_batches: usize) -> usize {
  iterations.div_ceil(batch_size).min(max_batches)
}

/// The random stream of one benchmark phase: `--seed` mixed with the phase,
/// so each phase sees the same values whichever phases ran before it.
fn phase_rng(seed: u64, phase: u64) -> StdRng {
  StdRng::seed_from_u64(seed ^ phase.wrapping_mul(0x9E37_79B9_7F4A_7C15))
}

const PHASE_EDGES: u64 = 1;
const PHASE_KEY_LOOKUPS: u64 = 2;
const PHASE_TRAVERSALS: u64 = 3;
const PHASE_EDGE_EXISTS: u64 = 4;
const PHASE_VECTORS: u64 = 5;
const PHASE_VECTOR_READS: u64 = 6;

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
  edge_types: Vec<u32>,
  edge_prop_keys: Vec<u32>,
}

fn build_graph(db: &kitedb::core::single_file::SingleFileDB, config: &BenchConfig) -> GraphData {
  let mut node_ids = Vec::with_capacity(config.nodes);
  let mut node_keys = Vec::with_capacity(config.nodes);
  let batch_size = 5_000usize;

  println!("  Creating nodes...");
  let mut edge_types: Vec<u32> = Vec::new();
  let mut edge_prop_keys: Vec<u32> = Vec::new();
  for batch_start in (0..config.nodes).step_by(batch_size) {
    let end = (batch_start + batch_size).min(config.nodes);
    db.begin_bulk().expect("expected value");

    if batch_start == 0 {
      for idx in 0..config.edge_types {
        let name = format!("CALLS_{idx}");
        edge_types.push(db.define_etype(&name).expect("expected value"));
      }
      for idx in 0..config.edge_props {
        let name = format!("edge_prop_{idx}");
        edge_prop_keys.push(db.define_propkey(&name).expect("expected value"));
      }
    }

    let mut keys = Vec::with_capacity(end - batch_start);
    for i in batch_start..end {
      let key = format!("pkg.module{}.Class{}", i / 100, i % 100);
      keys.push(key);
    }
    let key_refs: Vec<Option<&str>> = keys.iter().map(|k| Some(k.as_str())).collect();
    let batch_ids = db.create_nodes_batch(&key_refs).expect("expected value");
    node_ids.extend(batch_ids);
    node_keys.extend(keys);

    db.commit().expect("expected value");
    print!("\r  Created {} / {} nodes", end, config.nodes);
  }
  println!();

  println!("  Creating edges...");
  let mut edges_created = 0usize;
  let mut attempts = 0usize;
  let max_attempts = config.edges * 3;
  let mut rng = phase_rng(config.seed, PHASE_EDGES);

  while edges_created < config.edges && attempts < max_attempts {
    let batch_target = (edges_created + batch_size).min(config.edges);
    db.begin_bulk().expect("expected value");

    let mut edges = Vec::new();
    let mut edges_with_props = Vec::new();
    if edge_prop_keys.is_empty() {
      edges.reserve(batch_target.saturating_sub(edges_created));
    } else {
      edges_with_props.reserve(batch_target.saturating_sub(edges_created));
    }

    while edges_created < batch_target && attempts < max_attempts {
      attempts += 1;
      let src = node_ids[rng.gen_range(0..node_ids.len())];
      let dst = node_ids[rng.gen_range(0..node_ids.len())];
      if src != dst {
        let etype = edge_types[rng.gen_range(0..edge_types.len())];
        if edge_prop_keys.is_empty() {
          edges.push((src, etype, dst));
        } else {
          let mut props = Vec::with_capacity(edge_prop_keys.len());
          for (idx, key_id) in edge_prop_keys.iter().enumerate() {
            let value = PropValue::I64(edges_created.saturating_add(idx) as i64);
            props.push((*key_id, value));
          }
          edges_with_props.push((src, etype, dst, props));
        }
        edges_created += 1;
      }
    }

    if edge_prop_keys.is_empty() {
      db.add_edges_batch(&edges).expect("expected value");
    } else {
      db.add_edges_with_props_batch(edges_with_props)
        .expect("expected value");
    }

    db.commit().expect("expected value");
    print!("\r  Created {} / {} edges", edges_created, config.edges);
  }
  println!();

  GraphData {
    node_ids,
    node_keys,
    edge_types,
    edge_prop_keys,
  }
}

fn benchmark_key_lookups(
  db: &kitedb::core::single_file::SingleFileDB,
  graph: &GraphData,
  iterations: usize,
  seed: u64,
) {
  println!("\n--- Key Lookups (node_by_key) ---");
  let mut rng = phase_rng(seed, PHASE_KEY_LOOKUPS);
  let mut samples = Vec::with_capacity(iterations);

  for _ in 0..iterations {
    let key = &graph.node_keys[rng.gen_range(0..graph.node_keys.len())];
    let start = Instant::now();
    let _ = db.node_by_key(key);
    samples.push(start.elapsed().as_nanos());
  }

  let stats = compute_stats(&mut samples);
  print_latency_table("Random existing keys", stats);
}

fn benchmark_traversals(
  db: &kitedb::core::single_file::SingleFileDB,
  graph: &GraphData,
  iterations: usize,
  seed: u64,
) {
  println!("\n--- 1-Hop Traversals (out) ---");
  let mut rng = phase_rng(seed, PHASE_TRAVERSALS);
  let mut samples = Vec::with_capacity(iterations);

  for _ in 0..iterations {
    let node = graph.node_ids[rng.gen_range(0..graph.node_ids.len())];
    let start = Instant::now();
    let edges = db.out_edges(node);
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
  seed: u64,
) {
  println!("\n--- Edge Exists ---");
  let mut rng = phase_rng(seed, PHASE_EDGE_EXISTS);
  let mut samples = Vec::with_capacity(iterations);

  for _ in 0..iterations {
    let src = graph.node_ids[rng.gen_range(0..graph.node_ids.len())];
    let dst = graph.node_ids[rng.gen_range(0..graph.node_ids.len())];
    let etype = graph.edge_types[rng.gen_range(0..graph.edge_types.len())];
    let start = Instant::now();
    let _ = db.edge_exists(src, etype, dst);
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

  db.begin(false).expect("expected value");
  let prop_key_id = db.define_propkey("embedding").expect("expected value");
  db.commit().expect("expected value");

  let mut rng = phase_rng(config.seed, PHASE_VECTORS);
  let vectors: Vec<Vec<f32>> = (0..vector_count)
    .map(|_| build_random_vector(&mut rng, config.vector_dims))
    .collect();

  let batch_size = 100usize;
  let mut samples = Vec::new();

  let mut i = 0;
  while i < vector_nodes.len() {
    let end = (i + batch_size).min(vector_nodes.len());
    let start = Instant::now();
    db.begin(false).expect("expected value");
    for j in i..end {
      db.set_node_vector(vector_nodes[j], prop_key_id, &vectors[j])
        .expect("expected value");
    }
    db.commit().expect("expected value");
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
  seed: u64,
) {
  let mut rng = phase_rng(seed, PHASE_VECTOR_READS);
  let mut samples = Vec::with_capacity(iterations);
  for _ in 0..iterations {
    let node = vector_nodes[rng.gen_range(0..vector_nodes.len())];
    let start = Instant::now();
    let _ = db.node_vector(node, prop_key_id);
    samples.push(start.elapsed().as_nanos());
  }
  let stats = compute_stats(&mut samples);
  print_latency_table("node_vector() random", stats);

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

fn create_bench_nodes(
  db: &kitedb::core::single_file::SingleFileDB,
  label: &str,
  count: usize,
) -> Vec<u64> {
  if count == 0 {
    return Vec::new();
  }
  db.begin_bulk().expect("expected value");
  let mut keys = Vec::with_capacity(count);
  for idx in 0..count {
    keys.push(format!("{label}:{idx}"));
  }
  let key_refs: Vec<Option<&str>> = keys.iter().map(|k| Some(k.as_str())).collect();
  let node_ids = db.create_nodes_batch(&key_refs).expect("expected value");
  db.commit().expect("expected value");
  node_ids
}

fn benchmark_writes(
  db: &kitedb::core::single_file::SingleFileDB,
  graph: &GraphData,
  iterations: usize,
  node_batches: Option<usize>,
) {
  println!("\n--- Batch Writes (100 nodes) ---");
  let batch_size = 100usize;
  let batches = node_batches.unwrap_or_else(|| batch_count(iterations, batch_size, 50));
  let mut samples = Vec::with_capacity(batches);

  for b in 0..batches {
    let start = Instant::now();
    db.begin_bulk().expect("expected value");
    let mut keys = Vec::with_capacity(batch_size);
    for i in 0..batch_size {
      keys.push(format!("bench:raw:{b}:{i}"));
    }
    let key_refs: Vec<Option<&str>> = keys.iter().map(|k| Some(k.as_str())).collect();
    let _ = db.create_nodes_batch(&key_refs).expect("expected value");
    db.commit().expect("expected value");
    samples.push(start.elapsed().as_nanos());
  }

  let stats = compute_stats(&mut samples);
  print_latency_table("Batch of 100 nodes", stats);

  if batches == 0 {
    return;
  }

  let edge_batch_size = 100usize;
  let edge_batches = batch_count(iterations, edge_batch_size, 50);
  if edge_batches == 0 {
    return;
  }

  let edge_etype = graph.edge_types.first().copied().unwrap_or_else(|| {
    db.begin_bulk().expect("expected value");
    let etype = db.define_etype("BENCH_EDGE").expect("expected value");
    db.commit().expect("expected value");
    etype
  });

  println!("\n--- Batch Writes (100 edges) ---");
  let total_edges = edge_batches * edge_batch_size;
  let edge_nodes = create_bench_nodes(db, "bench:edge", total_edges * 2);
  let mut edge_samples = Vec::with_capacity(edge_batches);
  for b in 0..edge_batches {
    let start = Instant::now();
    db.begin_bulk().expect("expected value");
    let base = b * edge_batch_size * 2;
    let mut edges = Vec::with_capacity(edge_batch_size);
    for i in 0..edge_batch_size {
      let src = edge_nodes[base + i * 2];
      let dst = edge_nodes[base + i * 2 + 1];
      edges.push((src, edge_etype, dst));
    }
    db.add_edges_batch(&edges).expect("expected value");
    db.commit().expect("expected value");
    edge_samples.push(start.elapsed().as_nanos());
  }
  let edge_stats = compute_stats(&mut edge_samples);
  print_latency_table("Batch of 100 edges", edge_stats);

  if graph.edge_prop_keys.is_empty() {
    return;
  }

  println!("\n--- Batch Writes (100 edges + props) ---");
  let edge_prop_nodes = create_bench_nodes(db, "bench:edge-props", total_edges * 2);
  let mut edge_prop_samples = Vec::with_capacity(edge_batches);
  for b in 0..edge_batches {
    let start = Instant::now();
    db.begin_bulk().expect("expected value");
    let base = b * edge_batch_size * 2;
    let mut edges = Vec::with_capacity(edge_batch_size);
    for i in 0..edge_batch_size {
      let src = edge_prop_nodes[base + i * 2];
      let dst = edge_prop_nodes[base + i * 2 + 1];
      let mut props = Vec::with_capacity(graph.edge_prop_keys.len());
      for (idx, key_id) in graph.edge_prop_keys.iter().enumerate() {
        let value = PropValue::I64((b * edge_batch_size + i + idx) as i64);
        props.push((*key_id, value));
      }
      edges.push((src, edge_etype, dst, props));
    }
    db.add_edges_with_props_batch(edges)
      .expect("expected value");
    db.commit().expect("expected value");
    edge_prop_samples.push(start.elapsed().as_nanos());
  }
  let edge_prop_stats = compute_stats(&mut edge_prop_samples);
  print_latency_table("Batch of 100 edges + props", edge_prop_stats);
}

fn main() {
  let config = parse_args();

  println!("{}", "=".repeat(120));
  println!("Single-file Raw Benchmark (Rust)");
  println!("{}", "=".repeat(120));
  println!("Nodes: {}", format_number(config.nodes));
  println!("Edges: {}", format_number(config.edges));
  println!("Edge types: {}", format_number(config.edge_types));
  println!("Edge props: {}", format_number(config.edge_props));
  println!("Iterations: {}", format_number(config.iterations));
  if let Some(batches) = config.node_batches {
    println!("Node write batches: {}", format_number(batches));
  }
  println!("WAL size: {} bytes", format_number(config.wal_size));
  println!("Sync mode: {}", format_sync_mode(config.sync_mode));
  println!("MVCC: {}", mvcc_label(config.mvcc));
  println!("Group commit: every commit");
  println!("Seed: {}", config.seed);
  println!("Auto-checkpoint: {}", config.auto_checkpoint);
  println!("Checkpoint log ratio: {:?}", config.checkpoint_log_ratio);
  println!("Vector dims: {}", format_number(config.vector_dims));
  println!("Vector count: {}", format_number(config.vector_count));
  println!("Replication primary: {}", config.replication_primary);
  if let Some(bytes) = config.replication_segment_max_bytes {
    println!(
      "Replication segment max bytes: {}",
      format_number(bytes as usize)
    );
  }
  println!("Skip checkpoint: {}", config.skip_checkpoint);
  println!("Reopen read-only: {}", config.reopen_readonly);
  println!("{}", "=".repeat(120));

  let temp = tempdir().expect("failed to create temp dir");
  let db_path = temp.path().join("ray-bench-raw.kitedb");

  let mut options = SingleFileOpenOptions::new()
    .wal_size(config.wal_size)
    .auto_checkpoint(config.auto_checkpoint)
    .sync_mode(config.sync_mode);
  if let Some(ratio) = config.checkpoint_log_ratio {
    options = options.checkpoint_log_ratio(ratio);
  }

  if let Some(mvcc) = config.mvcc {
    options = options.mvcc(mvcc);
  }
  if config.replication_primary {
    options = options.replication_role(ReplicationRole::Primary);
    if let Some(max_bytes) = config.replication_segment_max_bytes {
      options = options.replication_segment_max_bytes(max_bytes);
    }
  }

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
    let mut read_options = SingleFileOpenOptions::new()
      .read_only(true)
      .create_if_missing(false);
    if let Some(mvcc) = config.mvcc {
      read_options = read_options.mvcc(mvcc);
    }
    db = open_single_file(&db_path, read_options).expect("failed to reopen db");
    println!("  Re-opened database in read-only mode");
  }

  println!("\n[4/6] Key lookup benchmarks...");
  benchmark_key_lookups(&db, &graph, config.iterations, config.seed);

  println!("\n[5/6] Traversal and edge benchmarks...");
  benchmark_traversals(&db, &graph, config.iterations, config.seed);
  benchmark_edge_exists(&db, &graph, config.iterations, config.seed);

  if let Some((prop_key_id, vector_nodes)) = vector_setup {
    if !vector_nodes.is_empty() {
      benchmark_vector_reads(
        &db,
        &vector_nodes,
        prop_key_id,
        config.iterations,
        config.seed,
      );
    }
  }

  println!("\n[6/6] Write benchmarks...");
  if config.reopen_readonly {
    println!("  Skipped write benchmarks (read-only)");
  } else {
    benchmark_writes(&db, &graph, config.iterations, config.node_batches);
  }

  close_single_file(db).expect("failed to close db");

  if config.keep_db {
    // Dropping the TempDir would delete the database with it.
    let _ = temp.keep();
    println!("\nDatabase preserved at: {}", db_path.display());
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn small_iteration_counts_still_run_a_batch() {
    assert_eq!(batch_count(1, 100, 50), 1);
    assert_eq!(batch_count(50, 100, 50), 1);
    assert_eq!(batch_count(100, 100, 50), 1);
    assert_eq!(batch_count(150, 100, 50), 2);
    assert_eq!(batch_count(10_000, 100, 50), 50);
    assert_eq!(batch_count(0, 100, 50), 0);
  }

  #[test]
  fn no_samples_print_as_skipped_not_as_zero_latency() {
    let row = latency_row("Batch of 100 nodes", &compute_stats(&mut []));
    assert!(row.contains("skipped"), "{row}");
    assert!(!row.contains("0 ops/sec"), "{row}");
  }
}
