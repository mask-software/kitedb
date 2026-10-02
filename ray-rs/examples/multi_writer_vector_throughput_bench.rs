//! Multi-writer throughput benchmark for vector writes in single-file KiteDB.
//!
//! Measures `set_node_vector()` throughput across writer threads.
//!
//! Usage:
//!   cargo run --release --example multi_writer_vector_throughput_bench --no-default-features -- [options]
//!
//! Options:
//!   --threads N               Writer threads (default: 4)
//!   --tx-per-thread N         Transactions per thread (default: 200)
//!   --batch-size N            Vector sets per transaction (default: 200)
//!   --vector-dims N           Vector dimensions (default: 128)
//!   --wal-size BYTES          WAL size in bytes (default: 268435456)
//!   --sync-mode MODE          Sync mode: full|normal|off (default: normal)
//!   --group-commit-enabled    Enable group commit (default: false)
//!   --group-commit-window-ms  Group commit window in ms (default: 2)
//!   --no-auto-checkpoint      Disable auto-checkpoint (default: enabled)
//!   --checkpoint-threshold P  Auto-checkpoint threshold (default: 0.7)
//!   --no-background-checkpoint Use blocking checkpoints (default: background)
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

#[derive(Debug, Clone)]
struct BenchConfig {
  threads: usize,
  tx_per_thread: usize,
  batch_size: usize,
  vector_dims: usize,
  wal_size: usize,
  sync_mode: SyncMode,
  group_commit_enabled: bool,
  group_commit_window_ms: u64,
  auto_checkpoint: bool,
  checkpoint_threshold: f64,
  background_checkpoint: bool,
  /// None: the library default.
  mvcc: Option<bool>,
  keep_db: bool,
}

impl Default for BenchConfig {
  fn default() -> Self {
    Self {
      threads: 4,
      tx_per_thread: 200,
      batch_size: 200,
      vector_dims: 128,
      wal_size: 256 * 1024 * 1024,
      sync_mode: SyncMode::Normal,
      group_commit_enabled: false,
      group_commit_window_ms: 2,
      auto_checkpoint: true,
      checkpoint_threshold: 0.7,
      background_checkpoint: true,
      mvcc: None,
      keep_db: false,
    }
  }
}

fn usage_error(message: &str) -> ! {
  eprintln!("error: {message}");
  eprintln!("see the header of examples/multi_writer_vector_throughput_bench.rs for the options");
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
      "--vector-dims" => config.vector_dims = value(&args, &mut i, flag),
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
      "--group-commit-enabled" => config.group_commit_enabled = true,
      "--group-commit-window-ms" => config.group_commit_window_ms = value(&args, &mut i, flag),
      "--no-auto-checkpoint" => config.auto_checkpoint = false,
      "--checkpoint-threshold" => {
        let threshold: f64 = value(&args, &mut i, flag);
        config.checkpoint_threshold = threshold.clamp(0.0, 1.0);
      }
      "--no-background-checkpoint" => config.background_checkpoint = false,
      "--mvcc" => config.mvcc = Some(true),
      "--no-mvcc" => config.mvcc = Some(false),
      "--keep-db" => config.keep_db = true,
      other => usage_error(&format!("unknown option {other}")),
    }
    i += 1;
  }

  if config.threads == 0 {
    config.threads = 1;
  }
  if config.tx_per_thread == 0 {
    config.tx_per_thread = 1;
  }
  if config.batch_size == 0 {
    config.batch_size = 1;
  }
  if config.vector_dims == 0 {
    config.vector_dims = 1;
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

fn create_nodes(db: &kitedb::core::single_file::SingleFileDB, count: usize) -> Vec<u64> {
  let mut ids = Vec::with_capacity(count);
  let batch_size = 10_000usize;
  for start in (0..count).step_by(batch_size) {
    let end = (start + batch_size).min(count);
    db.begin_bulk().expect("expected value");
    let mut keys = Vec::with_capacity(end - start);
    for i in start..end {
      keys.push(format!("vec-node:{i}"));
    }
    let key_refs: Vec<Option<&str>> = keys.iter().map(|k| Some(k.as_str())).collect();
    let batch_ids = db.create_nodes_batch(&key_refs).expect("expected value");
    ids.extend(batch_ids);
    db.commit().expect("expected value");
  }
  ids
}

fn main() {
  let config = parse_args();

  let total_vectors = config.threads * config.tx_per_thread * config.batch_size;
  println!("==================================================================");
  println!("Multi-writer Vector Throughput Benchmark (Rust)");
  println!("==================================================================");
  println!("Threads: {}", config.threads);
  println!("Tx per thread: {}", config.tx_per_thread);
  println!("Batch size: {}", config.batch_size);
  println!("Vector dims: {}", config.vector_dims);
  println!("Total vectors: {total_vectors}");
  println!("WAL size: {} bytes", config.wal_size);
  println!("Sync mode: {:?}", config.sync_mode);
  println!(
    "Group commit: {} (window {}ms)",
    config.group_commit_enabled, config.group_commit_window_ms
  );
  println!(
    "Auto-checkpoint: {} (threshold {}, background {})",
    config.auto_checkpoint, config.checkpoint_threshold, config.background_checkpoint
  );
  println!("MVCC: {}", mvcc_label(config.mvcc));
  println!("==================================================================");

  let temp_dir = tempdir().expect("temp dir");
  let db_path: PathBuf = temp_dir
    .path()
    .join("multi-writer-vector-throughput.kitedb");

  let mut open_opts = SingleFileOpenOptions::new()
    .wal_size(config.wal_size)
    .sync_mode(config.sync_mode)
    .group_commit_enabled(config.group_commit_enabled)
    .group_commit_window_ms(config.group_commit_window_ms)
    .auto_checkpoint(config.auto_checkpoint)
    .checkpoint_threshold(config.checkpoint_threshold)
    .background_checkpoint(config.background_checkpoint);
  if let Some(mvcc) = config.mvcc {
    open_opts = open_opts.mvcc(mvcc);
  }

  let db = open_single_file(&db_path, open_opts).expect("open db");
  let db = Arc::new(db);

  // Schema setup (single tx).
  db.begin(false).expect("expected value");
  let prop_key_id = db.define_propkey("embedding").expect("expected value");
  db.commit().expect("expected value");

  // Pre-create nodes so the benchmark measures vector writes (not node creation).
  let node_ids = create_nodes(&db, total_vectors);
  let node_ids = Arc::new(node_ids);

  // Pre-create vector store to avoid any "first set" races / store init cost.
  db.vector_store_or_create(prop_key_id, config.vector_dims)
    .expect("expected value");

  let start_idx = Arc::new(AtomicU64::new(0));
  let start = Instant::now();

  let mut handles = Vec::with_capacity(config.threads);
  for _ in 0..config.threads {
    let db = Arc::clone(&db);
    let node_ids = Arc::clone(&node_ids);
    let start_idx = Arc::clone(&start_idx);
    let config = config.clone();

    let handle = std::thread::spawn(move || {
      let mut total_sets = 0u64;
      let vector = vec![0.1234f32; config.vector_dims];

      for _ in 0..config.tx_per_thread {
        db.begin(false).expect("expected value");
        let base = start_idx.fetch_add(config.batch_size as u64, Ordering::Relaxed) as usize;
        for offset in 0..config.batch_size {
          let node_id = node_ids[base + offset];
          db.set_node_vector(node_id, prop_key_id, &vector)
            .expect("expected value");
          total_sets += 1;
        }
        db.commit().expect("expected value");
      }

      total_sets
    });
    handles.push(handle);
  }

  let mut vectors_written = 0u64;
  for handle in handles {
    vectors_written += handle.join().expect("writer thread");
  }

  let elapsed = start.elapsed().as_secs_f64();
  let tx_total = (config.threads * config.tx_per_thread) as u64;

  println!("\n--- Throughput ---");
  println!("Elapsed: {elapsed:.3}s");
  println!("Transactions: {tx_total}");
  println!("Vectors written: {vectors_written}");
  println!("Tx rate: {}", format_rate(tx_total, elapsed));
  println!("Vector rate: {}", format_rate(vectors_written, elapsed));

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
