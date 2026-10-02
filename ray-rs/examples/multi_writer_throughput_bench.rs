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
//!   --full-fsync              With --sync-mode full, sync with F_FULLFSYNC on
//!                             macOS, which flushes the drive's write cache
//!                             (plain fsync there does not)
//!   --group-commit-enabled    Enable group commit (default: false)
//!   --group-commit-window-ms  Group commit window in ms (default: 2)
//!   --mvcc | --no-mvcc        MVCC mode (default: the library default; without
//!                             MVCC, write transactions run one at a time)
//!   --keep-db                 Keep the database file after benchmark
//!
//! Unknown options are an error.

use std::env;
use std::path::PathBuf;
use std::process::exit;
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
  full_fsync: bool,
  group_commit_enabled: bool,
  group_commit_window_ms: u64,
  /// None: the library default.
  mvcc: Option<bool>,
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
      full_fsync: false,
      group_commit_enabled: false,
      group_commit_window_ms: 2,
      mvcc: None,
      keep_db: false,
    }
  }
}

fn usage_error(message: &str) -> ! {
  eprintln!("error: {message}");
  eprintln!("see the header of examples/multi_writer_throughput_bench.rs for the options");
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
      "--threads" => config.threads = value(&args, &mut i, flag),
      "--tx-per-thread" => config.tx_per_thread = value(&args, &mut i, flag),
      "--batch-size" => config.batch_size = value(&args, &mut i, flag),
      "--edges-per-node" => config.edges_per_node = value(&args, &mut i, flag),
      "--edge-types" => config.edge_types = value(&args, &mut i, flag),
      "--edge-props" => config.edge_props = value(&args, &mut i, flag),
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
      "--full-fsync" => config.full_fsync = true,
      "--group-commit-enabled" => config.group_commit_enabled = true,
      "--group-commit-window-ms" => config.group_commit_window_ms = value(&args, &mut i, flag),
      "--mvcc" => config.mvcc = Some(true),
      "--no-mvcc" => config.mvcc = Some(false),
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
  println!("Full fsync: {}", config.full_fsync);
  println!(
    "Group commit: {} (window {}ms)",
    config.group_commit_enabled, config.group_commit_window_ms
  );
  println!("MVCC: {}", mvcc_label(config.mvcc));
  println!("==================================================================");

  let temp_dir = tempdir().expect("temp dir");
  let db_path: PathBuf = temp_dir.path().join("multi-writer-throughput.kitedb");

  let mut open_opts = SingleFileOpenOptions::new()
    .wal_size(config.wal_size)
    .sync_mode(config.sync_mode)
    .full_fsync(config.full_fsync)
    .group_commit_enabled(config.group_commit_enabled)
    .group_commit_window_ms(config.group_commit_window_ms)
    .auto_checkpoint(false);
  if let Some(mvcc) = config.mvcc {
    open_opts = open_opts.mvcc(mvcc);
  }

  let db = open_single_file(&db_path, open_opts).expect("open db");
  let db = Arc::new(db);

  let mut etypes = Vec::with_capacity(config.edge_types);
  let mut edge_prop_keys = Vec::with_capacity(config.edge_props);
  db.begin(false).expect("expected value");
  for i in 0..config.edge_types {
    let etype = db
      .define_etype(&format!("edge_type_{i}"))
      .expect("expected value");
    etypes.push(etype);
  }
  for i in 0..config.edge_props {
    let key = db
      .define_propkey(&format!("edge_prop_{i}"))
      .expect("expected value");
    edge_prop_keys.push(key);
  }
  db.commit().expect("expected value");

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
        db.begin(false).expect("expected value");
        let mut keys = Vec::with_capacity(config.batch_size);
        for _ in 0..config.batch_size {
          let idx = node_counter.fetch_add(1, Ordering::Relaxed);
          let key = format!("t{tid}-n{idx}");
          keys.push(key);
        }
        let key_refs: Vec<Option<&str>> = keys.iter().map(|k| Some(k.as_str())).collect();
        let batch_nodes = db.create_nodes_batch(&key_refs).expect("expected value");
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
            db.add_edges_batch(&edges).expect("expected value");
          } else {
            db.add_edges_with_props_batch(edges_with_props)
              .expect("expected value");
          }
        }

        db.commit().expect("expected value");
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
    let _ = temp_dir.keep();
  }
}
