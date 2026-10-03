//! RayDB vs Ladybug 1-hop traversal benchmark (Rust).
//!
//! Workload:
//! - Build the same graph in both engines
//! - Default: 10k nodes, 20k edges
//! - Query equivalent to `from(alice).out(KNOWS).toArray()`
//! - Alice fan-out defaults to 10 (configurable in 5-20 range)
//!
//! Usage:
//!   cargo run --release --example ray_vs_ladybug_bench --no-default-features --features bench-ladybug -- \
//!     --nodes 10000 --edges 20000 --query-results 10 --iterations 5000

use std::collections::HashSet;
use std::env;
use std::error::Error;
use std::path::Path;
use std::time::Instant;

use lbug::{Connection as LadybugConnection, Database as LadybugDatabase, SystemConfig};
use rand::{rngs::StdRng, Rng, SeedableRng};
use tempfile::{tempdir, TempDir};

use kitedb::api::kite::{EdgeDef, Kite, KiteOptions, NodeDef};
use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileOpenOptions, SyncMode,
};
use kitedb::types::NodeId;

#[derive(Debug, Clone)]
struct BenchConfig {
  nodes: usize,
  edges: usize,
  query_results: usize,
  iterations: usize,
  warmup: usize,
  seed: u64,
  batch_size: usize,
  keep_db: bool,
}

impl Default for BenchConfig {
  fn default() -> Self {
    Self {
      nodes: 10_000,
      edges: 20_000,
      query_results: 10,
      iterations: 5_000,
      warmup: 500,
      seed: 42,
      batch_size: 1_000,
      keep_db: false,
    }
  }
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

fn parse_args() -> Result<BenchConfig, String> {
  let mut cfg = BenchConfig::default();
  let args: Vec<String> = env::args().collect();

  let mut i = 1;
  while i < args.len() {
    match args[i].as_str() {
      "--nodes" => {
        i += 1;
        cfg.nodes = args
          .get(i)
          .ok_or("--nodes requires value")?
          .parse()
          .map_err(|_| "invalid --nodes")?;
      }
      "--edges" => {
        i += 1;
        cfg.edges = args
          .get(i)
          .ok_or("--edges requires value")?
          .parse()
          .map_err(|_| "invalid --edges")?;
      }
      "--query-results" => {
        i += 1;
        cfg.query_results = args
          .get(i)
          .ok_or("--query-results requires value")?
          .parse()
          .map_err(|_| "invalid --query-results")?;
      }
      "--iterations" => {
        i += 1;
        cfg.iterations = args
          .get(i)
          .ok_or("--iterations requires value")?
          .parse()
          .map_err(|_| "invalid --iterations")?;
      }
      "--warmup" => {
        i += 1;
        cfg.warmup = args
          .get(i)
          .ok_or("--warmup requires value")?
          .parse()
          .map_err(|_| "invalid --warmup")?;
      }
      "--seed" => {
        i += 1;
        cfg.seed = args
          .get(i)
          .ok_or("--seed requires value")?
          .parse()
          .map_err(|_| "invalid --seed")?;
      }
      "--batch-size" => {
        i += 1;
        cfg.batch_size = args
          .get(i)
          .ok_or("--batch-size requires value")?
          .parse()
          .map_err(|_| "invalid --batch-size")?;
      }
      "--keep-db" => {
        cfg.keep_db = true;
      }
      "--help" | "-h" => {
        print_help();
        std::process::exit(0);
      }
      other => return Err(format!("unknown argument: {other}")),
    }
    i += 1;
  }

  if cfg.nodes < 2 {
    return Err("--nodes must be >= 2".to_string());
  }
  if cfg.edges < 1 {
    return Err("--edges must be >= 1".to_string());
  }
  if cfg.query_results < 1 {
    return Err("--query-results must be >= 1".to_string());
  }
  if cfg.query_results >= cfg.nodes {
    return Err("--query-results must be < --nodes".to_string());
  }
  if cfg.query_results > cfg.edges {
    return Err("--query-results must be <= --edges".to_string());
  }
  if cfg.iterations < 1 {
    return Err("--iterations must be >= 1".to_string());
  }
  if cfg.batch_size < 1 {
    return Err("--batch-size must be >= 1".to_string());
  }

  Ok(cfg)
}

fn print_help() {
  println!("RayDB vs Ladybug traversal benchmark");
  println!();
  println!("Options:");
  println!("  --nodes N              Number of nodes (default: 10000)");
  println!("  --edges N              Number of edges (default: 20000)");
  println!("  --query-results N      Alice outgoing neighbors (default: 10)");
  println!("  --iterations N         Timed query iterations (default: 5000)");
  println!("  --warmup N             Warmup iterations (default: 500)");
  println!("  --seed N               RNG seed (default: 42)");
  println!("  --batch-size N         Batch size for ingest (default: 1000)");
  println!("  --keep-db              Keep local RayDB + Ladybug datasets");
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

fn print_stats(name: &str, stats: LatencyStats) {
  let ops = if stats.sum > 0 {
    stats.count as f64 / (stats.sum as f64 / 1_000_000_000.0)
  } else {
    0.0
  };
  println!(
    "{:<10} p50={:>10} p95={:>10} p99={:>10} max={:>10} ({:.0} ops/sec)",
    name,
    format_latency(stats.p50),
    format_latency(stats.p95),
    format_latency(stats.p99),
    format_latency(stats.max),
    ops
  );
}

fn build_workload(cfg: &BenchConfig) -> (Vec<String>, Vec<(usize, usize)>) {
  let mut keys = Vec::with_capacity(cfg.nodes);
  keys.push("user:alice".to_string());
  for i in 1..cfg.nodes {
    keys.push(format!("user:u{i}"));
  }

  let mut edges: HashSet<(usize, usize)> = HashSet::with_capacity(cfg.edges * 2);
  for dst in 1..=cfg.query_results {
    edges.insert((0, dst));
  }

  let mut rng = StdRng::seed_from_u64(cfg.seed);
  while edges.len() < cfg.edges {
    let src = rng.gen_range(1..cfg.nodes); // keep alice fan-out fixed
    let dst = rng.gen_range(0..cfg.nodes);
    if src != dst {
      edges.insert((src, dst));
    }
  }

  (keys, edges.into_iter().collect())
}

fn ingest_raydb(
  raydb_path: &Path,
  cfg: &BenchConfig,
  keys: &[String],
  edges: &[(usize, usize)],
) -> Result<u128, Box<dyn Error>> {
  let started = Instant::now();
  let options = SingleFileOpenOptions::new()
    .sync_mode(SyncMode::Normal)
    .create_if_missing(true);
  let db = open_single_file(raydb_path, options)?;

  db.begin_bulk()?;
  let knows = db.define_etype("KNOWS")?;
  db.commit()?;
  let mut node_ids: Vec<NodeId> = Vec::with_capacity(keys.len());

  for start in (0..keys.len()).step_by(cfg.batch_size) {
    let end = (start + cfg.batch_size).min(keys.len());
    db.begin_bulk()?;
    let key_refs: Vec<Option<&str>> = keys[start..end].iter().map(|k| Some(k.as_str())).collect();
    let batch_ids = db.create_nodes_batch(&key_refs)?;
    node_ids.extend(batch_ids);
    db.commit()?;
  }

  for start in (0..edges.len()).step_by(cfg.batch_size) {
    let end = (start + cfg.batch_size).min(edges.len());
    let mut batch = Vec::with_capacity(end - start);
    for (src_index, dst_index) in &edges[start..end] {
      batch.push((node_ids[*src_index], knows, node_ids[*dst_index]));
    }
    db.begin_bulk()?;
    db.add_edges_batch(&batch)?;
    db.commit()?;
  }

  close_single_file(db)?;
  Ok(started.elapsed().as_millis())
}

fn benchmark_raydb_query(
  raydb_path: &Path,
  cfg: &BenchConfig,
) -> Result<(LatencyStats, usize), Box<dyn Error>> {
  let user = NodeDef::new("User", "user:");
  let knows = EdgeDef::new("KNOWS");
  let options = KiteOptions::new()
    .node(user)
    .edge(knows)
    .sync_mode(SyncMode::Normal);
  let kite = Kite::open(raydb_path, options)?;
  let alice = kite
    .raw()
    .node_by_key("user:alice")
    .ok_or("missing alice in RayDB")?;

  for _ in 0..cfg.warmup {
    let _ = kite.from(alice).out(Some("KNOWS"))?.to_vec();
  }

  let mut samples = Vec::with_capacity(cfg.iterations);
  let mut result_count = 0usize;

  for _ in 0..cfg.iterations {
    let start = Instant::now();
    let rows = kite.from(alice).out(Some("KNOWS"))?.to_vec();
    samples.push(start.elapsed().as_nanos());
    result_count = rows.len();
  }

  kite.close()?;
  Ok((compute_stats(&mut samples), result_count))
}

fn cypher_quote(value: &str) -> String {
  value.replace('\\', "\\\\").replace('\'', "\\'")
}

fn ingest_ladybug(
  conn: &LadybugConnection<'_>,
  cfg: &BenchConfig,
  keys: &[String],
  edges: &[(usize, usize)],
) -> Result<u128, Box<dyn Error>> {
  let started = Instant::now();

  conn.query("CREATE NODE TABLE User(key STRING, PRIMARY KEY(key));")?;
  conn.query("CREATE REL TABLE KNOWS(FROM User TO User);")?;

  for start in (0..keys.len()).step_by(cfg.batch_size) {
    let end = (start + cfg.batch_size).min(keys.len());
    let mut q = String::with_capacity((end - start) * 48);
    for key in &keys[start..end] {
      q.push_str("CREATE (:User {key: '");
      q.push_str(&cypher_quote(key));
      q.push_str("'});");
    }
    conn.query(&q)?;
  }

  for start in (0..edges.len()).step_by(cfg.batch_size) {
    let end = (start + cfg.batch_size).min(edges.len());
    let mut q = String::with_capacity((end - start) * 128);
    for (src_index, dst_index) in &edges[start..end] {
      q.push_str("MATCH (s:User {key: '");
      q.push_str(&cypher_quote(&keys[*src_index]));
      q.push_str("'}), (d:User {key: '");
      q.push_str(&cypher_quote(&keys[*dst_index]));
      q.push_str("'}) CREATE (s)-[:KNOWS]->(d);");
    }
    conn.query(&q)?;
  }

  Ok(started.elapsed().as_millis())
}

fn benchmark_ladybug_query(
  conn: &LadybugConnection<'_>,
  cfg: &BenchConfig,
) -> Result<(LatencyStats, usize), Box<dyn Error>> {
  let q = "MATCH (a:User {key: 'user:alice'})-[:KNOWS]->(b) RETURN b.key AS key";

  for _ in 0..cfg.warmup {
    let _ = conn.query(q)?.count();
  }

  let mut samples = Vec::with_capacity(cfg.iterations);
  let mut result_count = 0usize;

  for _ in 0..cfg.iterations {
    let start = Instant::now();
    result_count = conn.query(q)?.count();
    samples.push(start.elapsed().as_nanos());
  }

  Ok((compute_stats(&mut samples), result_count))
}

fn persist_temp(
  temp: TempDir,
  raydb_path: &Path,
  ladybug_path: &Path,
) -> Result<(), Box<dyn Error>> {
  let keep_dir = temp.keep();
  println!();
  println!("RayDB dataset kept at: {}", raydb_path.display());
  println!("Ladybug dataset kept at: {}", ladybug_path.display());
  println!("Temp dir: {}", keep_dir.display());
  Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
  let cfg = parse_args().map_err(|e| format!("argument error: {e}"))?;
  let (keys, edges) = build_workload(&cfg);

  let temp = tempdir()?;
  let raydb_path = temp.path().join("ray-vs-ladybug.kitedb");
  let ladybug_path = temp.path().join("ray-vs-ladybug.lbug");

  println!("RayDB vs Ladybug: 1-hop traversal");
  println!("Nodes: {}", format_number(cfg.nodes));
  println!("Edges: {}", format_number(cfg.edges));
  println!("Alice expected results: {}", cfg.query_results);
  println!(
    "Iterations: {} (warmup {})",
    format_number(cfg.iterations),
    format_number(cfg.warmup)
  );
  println!();

  let ray_ingest_ms = ingest_raydb(&raydb_path, &cfg, &keys, &edges)?;

  let ladybug_db = LadybugDatabase::new(&ladybug_path, SystemConfig::default())?;
  let ladybug_conn = LadybugConnection::new(&ladybug_db)?;
  let ladybug_ingest_ms = ingest_ladybug(&ladybug_conn, &cfg, &keys, &edges)?;

  let (ray_stats, ray_count) = benchmark_raydb_query(&raydb_path, &cfg)?;
  let (ladybug_stats, ladybug_count) = benchmark_ladybug_query(&ladybug_conn, &cfg)?;

  if ray_count != cfg.query_results {
    return Err(
      format!(
        "RayDB result mismatch: got {}, expected {}",
        ray_count, cfg.query_results
      )
      .into(),
    );
  }
  if ladybug_count != cfg.query_results {
    return Err(
      format!(
        "Ladybug result mismatch: got {}, expected {}",
        ladybug_count, cfg.query_results
      )
      .into(),
    );
  }

  println!("Setup times (not included in query latency):");
  println!("  RayDB ingest:    {:.2}ms", ray_ingest_ms as f64);
  println!("  Ladybug ingest:  {:.2}ms", ladybug_ingest_ms as f64);
  println!();
  println!("Query latency (from(alice).out(KNOWS).toArray equivalent):");
  print_stats("RayDB", ray_stats);
  print_stats("Ladybug", ladybug_stats);

  if ray_stats.p50 > 0 && ray_stats.p95 > 0 {
    println!();
    println!(
      "Ladybug/RayDB ratio: p50={:.2}x p95={:.2}x",
      ladybug_stats.p50 as f64 / ray_stats.p50 as f64,
      ladybug_stats.p95 as f64 / ray_stats.p95 as f64
    );
  }

  if cfg.keep_db {
    persist_temp(temp, &raydb_path, &ladybug_path)?;
  }

  Ok(())
}
