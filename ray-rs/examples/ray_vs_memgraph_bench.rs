//! RayDB vs Memgraph 1-hop traversal benchmark (Rust).
//!
//! Workload:
//! - Build the same graph in both engines
//! - Default: 10k nodes, 20k edges
//! - Query equivalent to `from(alice).out(KNOWS).toArray()`
//! - Alice fan-out defaults to 10 (configurable in 5-20 range)
//!
//! Usage:
//!   cargo run --release --example ray_vs_memgraph_bench --no-default-features -- \
//!     --nodes 10000 --edges 20000 --query-results 10 --iterations 5000
//!
//! `--help` lists the options (among them `--mvcc` / `--no-mvcc`, the MVCC
//! mode of the RayDB side; default: the library default). Unknown options are
//! an error.

use std::collections::HashSet;
use std::env;
use std::error::Error;
use std::time::Instant;

use neo4rs::{query, ConfigBuilder, Graph};
use rand::{rngs::StdRng, Rng, SeedableRng};
use tempfile::{tempdir, TempDir};

use kitedb::api::kite::{EdgeDef, Kite, KiteOptions, NodeDef};
use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileOpenOptions, SyncMode,
};
use kitedb::types::{ETypeId, NodeId};

#[derive(Debug, Clone)]
struct BenchConfig {
  nodes: usize,
  edges: usize,
  query_results: usize,
  iterations: usize,
  warmup: usize,
  seed: u64,
  batch_size: usize,
  memgraph_uri: String,
  memgraph_user: String,
  memgraph_password: String,
  /// None: the library default.
  mvcc: Option<bool>,
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
      memgraph_uri: "127.0.0.1:7687".to_string(),
      memgraph_user: String::new(),
      memgraph_password: String::new(),
      mvcc: None,
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
      "--memgraph-uri" => {
        i += 1;
        cfg.memgraph_uri = args
          .get(i)
          .ok_or("--memgraph-uri requires value")?
          .to_string();
      }
      "--memgraph-user" => {
        i += 1;
        cfg.memgraph_user = args
          .get(i)
          .ok_or("--memgraph-user requires value")?
          .to_string();
      }
      "--memgraph-password" => {
        i += 1;
        cfg.memgraph_password = args
          .get(i)
          .ok_or("--memgraph-password requires value")?
          .to_string();
      }
      "--mvcc" => {
        cfg.mvcc = Some(true);
      }
      "--no-mvcc" => {
        cfg.mvcc = Some(false);
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
  println!("RayDB vs Memgraph traversal benchmark");
  println!();
  println!("Options:");
  println!("  --nodes N              Number of nodes (default: 10000)");
  println!("  --edges N              Number of edges (default: 20000)");
  println!("  --query-results N      Alice outgoing neighbors (default: 10)");
  println!("  --iterations N         Timed query iterations (default: 5000)");
  println!("  --warmup N             Warmup iterations (default: 500)");
  println!("  --seed N               RNG seed (default: 42)");
  println!("  --batch-size N         Batch size for ingest (default: 1000)");
  println!("  --memgraph-uri URI     Memgraph Bolt URI (default: 127.0.0.1:7687)");
  println!("  --memgraph-user USER   Memgraph username (default: empty)");
  println!("  --memgraph-password P  Memgraph password (default: empty)");
  println!("  --mvcc | --no-mvcc     RayDB MVCC mode (default: the library default)");
  println!("  --keep-db              Keep local RayDB file");
}

/// The MVCC mode of the RayDB side, for the header.
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
  raydb_path: &std::path::Path,
  cfg: &BenchConfig,
  keys: &[String],
  edges: &[(usize, usize)],
) -> Result<(u128, ETypeId), Box<dyn Error>> {
  let started = Instant::now();
  let mut options = SingleFileOpenOptions::new()
    .sync_mode(SyncMode::Normal)
    .create_if_missing(true);
  if let Some(mvcc) = cfg.mvcc {
    options = options.mvcc(mvcc);
  }
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
  Ok((started.elapsed().as_millis(), knows))
}

fn benchmark_raydb_query(
  raydb_path: &std::path::Path,
  cfg: &BenchConfig,
) -> Result<(LatencyStats, usize), Box<dyn Error>> {
  let user = NodeDef::new("User", "user:");
  let knows = EdgeDef::new("KNOWS");
  let mut options = KiteOptions::new()
    .node(user)
    .edge(knows)
    .sync_mode(SyncMode::Normal);
  if let Some(mvcc) = cfg.mvcc {
    options = options.mvcc(mvcc);
  }
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

fn normalize_memgraph_uri(uri: &str) -> String {
  uri
    .trim_start_matches("bolt://")
    .trim_start_matches("neo4j://")
    .to_string()
}

fn cypher_quote(value: &str) -> String {
  value.replace('\\', "\\\\").replace('\'', "\\'")
}

async fn memgraph_connect(cfg: &BenchConfig) -> Result<Graph, Box<dyn Error>> {
  let config = ConfigBuilder::default()
    .uri(&normalize_memgraph_uri(&cfg.memgraph_uri))
    .user(&cfg.memgraph_user)
    .password(&cfg.memgraph_password)
    .db("memgraph")
    .fetch_size(1000)
    .max_connections(8)
    .build()?;
  Ok(Graph::connect(config).await?)
}

async fn memgraph_run(graph: &Graph, q: &str) -> Result<(), Box<dyn Error>> {
  graph.run(query(q)).await?;
  Ok(())
}

async fn memgraph_count_rows(graph: &Graph, q: &str) -> Result<usize, Box<dyn Error>> {
  let mut rows = graph.execute(query(q)).await?;
  let mut count = 0usize;
  loop {
    match rows.next().await {
      Ok(Some(_)) => count += 1,
      Ok(None) => break,
      Err(err) => return Err(Box::new(err)),
    }
  }
  Ok(count)
}

async fn ingest_memgraph(
  graph: &Graph,
  cfg: &BenchConfig,
  keys: &[String],
  edges: &[(usize, usize)],
) -> Result<u128, Box<dyn Error>> {
  let started = Instant::now();

  memgraph_run(graph, "MATCH (n) DETACH DELETE n").await?;
  let _ = memgraph_run(graph, "CREATE INDEX ON :User(key)").await;

  for start in (0..keys.len()).step_by(cfg.batch_size) {
    let end = (start + cfg.batch_size).min(keys.len());
    let list = keys[start..end]
      .iter()
      .map(|k| format!("'{}'", cypher_quote(k)))
      .collect::<Vec<_>>()
      .join(", ");
    let q = format!("UNWIND [{list}] AS key CREATE (:User {{key: key}})");
    memgraph_run(graph, &q).await?;
  }

  for start in (0..edges.len()).step_by(cfg.batch_size) {
    let end = (start + cfg.batch_size).min(edges.len());
    let pairs = edges[start..end]
      .iter()
      .map(|(src, dst)| {
        format!(
          "['{}','{}']",
          cypher_quote(&keys[*src]),
          cypher_quote(&keys[*dst])
        )
      })
      .collect::<Vec<_>>()
      .join(", ");

    let q = format!(
      "UNWIND [{pairs}] AS pair \
       MATCH (s:User {{key: pair[0]}}) \
       MATCH (d:User {{key: pair[1]}}) \
       CREATE (s)-[:KNOWS]->(d)"
    );
    memgraph_run(graph, &q).await?;
  }

  Ok(started.elapsed().as_millis())
}

async fn benchmark_memgraph_query(
  graph: &Graph,
  cfg: &BenchConfig,
) -> Result<(LatencyStats, usize), Box<dyn Error>> {
  let q = "MATCH (a:User {key: 'user:alice'})-[:KNOWS]->(b) RETURN b.key AS key";

  for _ in 0..cfg.warmup {
    let _ = memgraph_count_rows(graph, q).await?;
  }

  let mut samples = Vec::with_capacity(cfg.iterations);
  let mut result_count = 0usize;
  for _ in 0..cfg.iterations {
    let start = Instant::now();
    result_count = memgraph_count_rows(graph, q).await?;
    samples.push(start.elapsed().as_nanos());
  }

  Ok((compute_stats(&mut samples), result_count))
}

async fn async_main(cfg: BenchConfig) -> Result<(), Box<dyn Error>> {
  let (keys, edges) = build_workload(&cfg);

  let temp = tempdir()?;
  let raydb_path = temp.path().join("ray-vs-memgraph.kitedb");

  println!("RayDB vs Memgraph: 1-hop traversal");
  println!("Nodes: {}", format_number(cfg.nodes));
  println!("Edges: {}", format_number(cfg.edges));
  println!("Alice expected results: {}", cfg.query_results);
  println!(
    "Iterations: {} (warmup {})",
    format_number(cfg.iterations),
    format_number(cfg.warmup)
  );
  println!("RayDB MVCC: {}", mvcc_label(cfg.mvcc));
  println!();

  let (ray_ingest_ms, _knows_id) = ingest_raydb(&raydb_path, &cfg, &keys, &edges)?;
  let graph = memgraph_connect(&cfg).await?;
  let memgraph_ingest_ms = ingest_memgraph(&graph, &cfg, &keys, &edges).await?;

  let (ray_stats, ray_count) = benchmark_raydb_query(&raydb_path, &cfg)?;
  let (mem_stats, mem_count) = benchmark_memgraph_query(&graph, &cfg).await?;

  if ray_count != cfg.query_results {
    return Err(
      format!(
        "RayDB result mismatch: got {}, expected {}",
        ray_count, cfg.query_results
      )
      .into(),
    );
  }
  if mem_count != cfg.query_results {
    return Err(
      format!(
        "Memgraph result mismatch: got {}, expected {}",
        mem_count, cfg.query_results
      )
      .into(),
    );
  }

  println!("Setup times (not included in query latency):");
  println!("  RayDB ingest:    {:.2}ms", ray_ingest_ms as f64);
  println!("  Memgraph ingest: {:.2}ms", memgraph_ingest_ms as f64);
  println!();
  println!("Query latency (from(alice).out(KNOWS).toArray equivalent):");
  print_stats("RayDB", ray_stats);
  print_stats("Memgraph", mem_stats);

  if ray_stats.p50 > 0 && ray_stats.p95 > 0 {
    println!();
    println!(
      "Memgraph/RayDB ratio: p50={:.2}x p95={:.2}x",
      mem_stats.p50 as f64 / ray_stats.p50 as f64,
      mem_stats.p95 as f64 / ray_stats.p95 as f64
    );
  }

  if cfg.keep_db {
    persist_temp(temp, &raydb_path)?;
  }

  Ok(())
}

fn persist_temp(temp: TempDir, raydb_path: &std::path::Path) -> Result<(), Box<dyn Error>> {
  let keep_dir = temp.keep();
  println!();
  println!("RayDB dataset kept at: {}", raydb_path.display());
  println!("Temp dir: {}", keep_dir.display());
  Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
  let cfg = parse_args().unwrap_or_else(|error| {
    eprintln!("error: {error}");
    eprintln!("run with --help, or see the header of examples/ray_vs_memgraph_bench.rs");
    std::process::exit(2);
  });
  let rt = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()?;
  rt.block_on(async_main(cfg))
}
