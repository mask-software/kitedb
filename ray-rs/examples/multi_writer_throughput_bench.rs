//! Multi-writer throughput benchmark for single-file KiteDB.
//!
//! Usage:
//!   cargo run --release --example multi_writer_throughput_bench --no-default-features -- [options]
//!
//! Options:
//!   --threads N               Writer threads (default: 8)
//!   --tx-per-thread N         Transactions per thread (default: 200)
//!   --batch-size N            Nodes per transaction (default: 200)
//!   --edges-per-node N        Edges per node (default: 1)
//!   --edge-types N            Number of edge types (default: 3)
//!   --edge-props N            Number of props per edge (default: 10)
//!   --wal-size BYTES          WAL size in bytes (default: 268435456)
//!   --sync-mode MODE          Sync mode: full|normal|off (default: normal)
//!   --group-commit-enabled    Enable group commit (default: false)
//!   --group-commit-window-ms  Group commit window in ms (default: 2)
//!   --keep-db                 Keep the database file after benchmark

use std::env;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tempfile::tempdir;

use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileOpenOptions, SyncMode,
};
use kitedb::types::PropValue;

#[derive(Debug, Clone)]
struct BenchConfig {
  threads: usize,
  tx_per_thread: usize,
  batch_size: usize,
  edges_per_node: usize,
  edge_types: usize,
  edge_props: usize,
  wal_size: usize,
  sync_mode: SyncMode,
  group_commit_enabled: bool,
  group_commit_window_ms: u64,
  keep_db: bool,
}

impl Default for BenchConfig {
  fn default() -> Self {
    Self {
      threads: 8,
      tx_per_thread: 200,
      batch_size: 200,
      edges_per_node: 1,
      edge_types: 3,
      edge_props: 10,
      wal_size: 256 * 1024 * 1024,
      sync_mode: SyncMode::Normal,
      group_commit_enabled: false,
      group_commit_window_ms: 2,
      keep_db: false,
    }
  }
}

fn parse_args() -> BenchConfig {
  let mut config = BenchConfig::default();
  let args: Vec<String> = env::args().collect();

  let mut i = 1;
  while i < args.len() {
    match args[i].as_str() {
      "--threads" => {
        if let Some(value) = args.get(i + 1) {
          config.threads = value.parse().unwrap_or(config.threads);
          i += 1;
        }
      }
      "--tx-per-thread" => {
        if let Some(value) = args.get(i + 1) {
          config.tx_per_thread = value.parse().unwrap_or(config.tx_per_thread);
          i += 1;
        }
      }
      "--batch-size" => {
        if let Some(value) = args.get(i + 1) {
          config.batch_size = value.parse().unwrap_or(config.batch_size);
          i += 1;
        }
      }
      "--edges-per-node" => {
        if let Some(value) = args.get(i + 1) {
          config.edges_per_node = value.parse().unwrap_or(config.edges_per_node);
          i += 1;
        }
      }
      "--edge-types" => {
        if let Some(value) = args.get(i + 1) {
          config.edge_types = value.parse().unwrap_or(config.edge_types);
          i += 1;
        }
      }
      "--edge-props" => {
        if let Some(value) = args.get(i + 1) {
          config.edge_props = value.parse().unwrap_or(config.edge_props);
          i += 1;
        }
      }
      "--wal-size" => {
        if let Some(value) = args.get(i + 1) {
          config.wal_size = value.parse().unwrap_or(config.wal_size);
          i += 1;
        }
      }
      "--sync-mode" => {
        if let Some(value) = args.get(i + 1) {
          match value.to_lowercase().as_str() {
            "full" => config.sync_mode = SyncMode::Full,
            "off" => config.sync_mode = SyncMode::Off,
            _ => config.sync_mode = SyncMode::Normal,
          }
          i += 1;
        }
      }
      "--group-commit-enabled" => {
        config.group_commit_enabled = true;
      }
      "--group-commit-window-ms" => {
        if let Some(value) = args.get(i + 1) {
          config.group_commit_window_ms = value.parse().unwrap_or(config.group_commit_window_ms);
          i += 1;
        }
      }
      "--keep-db" => {
        config.keep_db = true;
      }
      _ => {}
    }
    i += 1;
  }

  if config.edge_types == 0 {
    config.edge_types = 1;
  }

  config
}

fn format_rate(count: u64, seconds: f64) -> String {
  if seconds <= 0.0 {
    return "n/a".to_string();
  }
  let rate = count as f64 / seconds;
  if rate >= 1_000_000.0 {
    return format!("{:.2}M/s", rate / 1_000_000.0);
  }
  if rate >= 1_000.0 {
    return format!("{:.2}K/s", rate / 1_000.0);
  }
  format!("{rate:.2}/s")
}

fn main() {
  let config = parse_args();

  println!("==================================================================");
  println!("Multi-writer Throughput Benchmark (Rust)");
  println!("==================================================================");
  println!("Threads: {}", config.threads);
  println!("Tx per thread: {}", config.tx_per_thread);
  println!("Batch size: {}", config.batch_size);
  println!("Edges per node: {}", config.edges_per_node);
  println!("Edge types: {}", config.edge_types);
  println!("Edge props: {}", config.edge_props);
  println!("WAL size: {} bytes", config.wal_size);
  println!("Sync mode: {:?}", config.sync_mode);
  println!(
    "Group commit: {} (window {}ms)",
    config.group_commit_enabled, config.group_commit_window_ms
  );
  println!("==================================================================");

  let temp_dir = tempdir().expect("temp dir");
  let db_path: PathBuf = temp_dir.path().join("multi-writer-throughput.kitedb");

  let open_opts = SingleFileOpenOptions::new()
    .wal_size(config.wal_size)
    .sync_mode(config.sync_mode)
    .group_commit_enabled(config.group_commit_enabled)
    .group_commit_window_ms(config.group_commit_window_ms)
    .auto_checkpoint(false);

  let db = open_single_file(&db_path, open_opts).expect("open db");
  let db = Arc::new(db);

  let mut etypes = Vec::with_capacity(config.edge_types);
  let mut edge_prop_keys = Vec::with_capacity(config.edge_props);
  db.begin(false).unwrap();
  for i in 0..config.edge_types {
    let etype = db.define_etype(&format!("edge_type_{i}")).unwrap();
    etypes.push(etype);
  }
  for i in 0..config.edge_props {
    let key = db.define_propkey(&format!("edge_prop_{i}")).unwrap();
    edge_prop_keys.push(key);
  }
  db.commit().unwrap();

  let node_counter = Arc::new(AtomicU64::new(0));
  let start = Instant::now();

  let mut handles = Vec::with_capacity(config.threads);
  for tid in 0..config.threads {
    let db = Arc::clone(&db);
    let etypes = etypes.clone();
    let edge_prop_keys = edge_prop_keys.clone();
    let node_counter = Arc::clone(&node_counter);
    let config = config.clone();

    let handle = std::thread::spawn(move || {
      let mut total_nodes = 0u64;
      let mut total_edges = 0u64;
      for _ in 0..config.tx_per_thread {
        db.begin(false).unwrap();
        let mut keys = Vec::with_capacity(config.batch_size);
        for _ in 0..config.batch_size {
          let idx = node_counter.fetch_add(1, Ordering::Relaxed);
          let key = format!("t{tid}-n{idx}");
          keys.push(key);
        }
        let key_refs: Vec<Option<&str>> = keys.iter().map(|k| Some(k.as_str())).collect();
        let batch_nodes = db.create_nodes_batch(&key_refs).unwrap();
        total_nodes += batch_nodes.len() as u64;

        if !etypes.is_empty() && !batch_nodes.is_empty() && config.edges_per_node > 0 {
          let etype = etypes[tid % etypes.len()];
          let last = batch_nodes.len();
          let mut edges = Vec::new();
          let mut edges_with_props = Vec::new();
          if edge_prop_keys.is_empty() {
            edges.reserve(last * config.edges_per_node);
          } else {
            edges_with_props.reserve(last * config.edges_per_node);
          }
          for (i, &src) in batch_nodes.iter().enumerate() {
            for e in 0..config.edges_per_node {
              let dst = batch_nodes[(i + 1 + e) % last];
              if src == dst {
                continue;
              }
              if edge_prop_keys.is_empty() {
                edges.push((src, etype, dst));
              } else {
                let mut props = Vec::with_capacity(edge_prop_keys.len());
                for (idx, key_id) in edge_prop_keys.iter().enumerate() {
                  let value = PropValue::I64((idx as i64) + 1);
                  props.push((*key_id, value));
                }
                edges_with_props.push((src, etype, dst, props));
              }
              total_edges += 1;
            }
          }
          if edge_prop_keys.is_empty() {
            db.add_edges_batch(&edges).unwrap();
          } else {
            db.add_edges_with_props_batch(edges_with_props).unwrap();
          }
        }

        db.commit().unwrap();
      }
      (total_nodes, total_edges)
    });
    handles.push(handle);
  }

  let mut nodes_written = 0u64;
  let mut edges_written = 0u64;
  for handle in handles {
    let (n, e) = handle.join().expect("writer thread");
    nodes_written += n;
    edges_written += e;
  }

  let elapsed = start.elapsed().as_secs_f64();
  let tx_total = (config.threads * config.tx_per_thread) as u64;

  println!("\n--- Throughput ---");
  println!("Elapsed: {elapsed:.3}s");
  println!("Transactions: {tx_total}");
  println!("Nodes written: {nodes_written}");
  println!("Edges written: {edges_written}");
  println!("Tx rate: {}", format_rate(tx_total, elapsed));
  println!("Node rate: {}", format_rate(nodes_written, elapsed));
  println!("Edge rate: {}", format_rate(edges_written, elapsed));

  match Arc::try_unwrap(db) {
    Ok(db) => {
      close_single_file(db).expect("close db");
    }
    Err(_) => {
      println!("Warning: failed to unwrap DB Arc; skipping explicit close");
    }
  }
  if config.keep_db {
    println!("DB kept at: {}", db_path.display());
    std::mem::forget(temp_dir);
  }
}
