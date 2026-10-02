//! Bulk-load throughput benchmark for KiteDB core (Rust)
//!
//! Loads keyed nodes with props, then edges with props, in transactions of
//! `--batch` items, and reports the throughput of each phase.
//!
//! Usage:
//!   cargo run --release --example bulk_load_bench --no-default-features -- [options]
//!
//! Options:
//!   --nodes N             Nodes to load (default: 200000)
//!   --edges M             Edges to load (default: 1000000)
//!   --batch B             Nodes or edges per transaction (default: 5000)
//!   --node-props P        Props per node (default: 2)
//!   --edge-props Q        Props per edge (default: 2)
//!   --tx bulk|normal      Transaction kind: begin_bulk or begin(false) (default: bulk)
//!   --mvcc | --no-mvcc    MVCC mode (default: the library default)
//!   --reader              Keep a read transaction open on another thread for the whole load
//!   --sync-mode MODE      full|normal|off (default: normal)
//!   --wal-size BYTES      WAL size in bytes (default: 67108864)
//!   --runs R              Repeat the load on fresh databases and report each run (default: 1)
//!
//! Unknown options are an error.

use std::env;
use std::process::exit;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions, SyncMode,
};
use kitedb::types::PropValue;
use tempfile::tempdir;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TxKind {
  Bulk,
  Normal,
}

#[derive(Debug, Clone)]
struct BenchConfig {
  nodes: usize,
  edges: usize,
  batch: usize,
  node_props: usize,
  edge_props: usize,
  tx: TxKind,
  mvcc: Option<bool>,
  reader: bool,
  sync_mode: SyncMode,
  wal_size: usize,
  runs: usize,
}

fn usage_error(message: &str) -> ! {
  eprintln!("error: {message}");
  eprintln!("see the header of examples/bulk_load_bench.rs for the options");
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
  let mut config = BenchConfig {
    nodes: 200_000,
    edges: 1_000_000,
    batch: 5_000,
    node_props: 2,
    edge_props: 2,
    tx: TxKind::Bulk,
    mvcc: None,
    reader: false,
    sync_mode: SyncMode::Normal,
    wal_size: 64 * 1024 * 1024,
    runs: 1,
  };
  let args: Vec<String> = env::args().collect();
  let mut i = 1;
  while i < args.len() {
    let flag = args[i].as_str();
    match flag {
      "--nodes" => config.nodes = value(&args, &mut i, flag),
      "--edges" => config.edges = value(&args, &mut i, flag),
      "--batch" => config.batch = value(&args, &mut i, flag),
      "--node-props" => config.node_props = value(&args, &mut i, flag),
      "--edge-props" => config.edge_props = value(&args, &mut i, flag),
      "--tx" => {
        let kind: String = value(&args, &mut i, flag);
        config.tx = match kind.as_str() {
          "bulk" => TxKind::Bulk,
          "normal" => TxKind::Normal,
          other => usage_error(&format!("--tx must be bulk or normal, not {other}")),
        };
      }
      "--mvcc" => config.mvcc = Some(true),
      "--no-mvcc" => config.mvcc = Some(false),
      "--reader" => config.reader = true,
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
      "--wal-size" => config.wal_size = value(&args, &mut i, flag),
      "--runs" => config.runs = value(&args, &mut i, flag),
      other => usage_error(&format!("unknown option {other}")),
    }
    i += 1;
  }
  if config.batch == 0 || config.runs == 0 {
    usage_error("--batch and --runs must be positive");
  }
  config
}

fn begin(db: &SingleFileDB, tx: TxKind) {
  let begun = match tx {
    TxKind::Bulk => db.begin_bulk(),
    TxKind::Normal => db.begin(false),
  };
  if let Err(error) = begun {
    eprintln!("error: could not begin a {tx:?} transaction: {error}");
    exit(1);
  }
}

struct RunResult {
  nodes: Duration,
  edges: Duration,
}

fn run_once(config: &BenchConfig, options: &SingleFileOpenOptions) -> RunResult {
  let dir = tempdir().expect("temp dir");
  let db =
    Arc::new(open_single_file(dir.path().join("bulk.kitedb"), options.clone()).expect("open"));

  db.begin(false).expect("begin schema");
  let etype = db.define_etype("LINKS").expect("etype");
  let node_keys: Vec<u32> = (0..config.node_props)
    .map(|i| {
      db.define_propkey(&format!("node_prop_{i}"))
        .expect("propkey")
    })
    .collect();
  let edge_keys: Vec<u32> = (0..config.edge_props)
    .map(|i| {
      db.define_propkey(&format!("edge_prop_{i}"))
        .expect("propkey")
    })
    .collect();
  db.commit().expect("commit schema");

  // A read transaction open across the load: every commit must keep what it
  // replaces for that reader's snapshot.
  let reader = config.reader.then(|| {
    let (opened_tx, opened_rx) = mpsc::channel();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let reader_db = Arc::clone(&db);
    let handle = std::thread::spawn(move || {
      reader_db.begin(true).expect("reader begin");
      opened_tx.send(()).expect("reader opened");
      let _ = stop_rx.recv();
      reader_db.rollback().expect("reader end");
    });
    opened_rx.recv().expect("reader open");
    (stop_tx, handle)
  });

  let mut node_ids = Vec::with_capacity(config.nodes);
  let start = Instant::now();
  for batch_start in (0..config.nodes).step_by(config.batch) {
    let end = (batch_start + config.batch).min(config.nodes);
    begin(&db, config.tx);
    let keys: Vec<String> = (batch_start..end).map(|i| format!("node:{i}")).collect();
    let key_refs: Vec<Option<&str>> = keys.iter().map(|key| Some(key.as_str())).collect();
    let ids = db.create_nodes_batch(&key_refs).expect("create nodes");
    for (offset, &node_id) in ids.iter().enumerate() {
      for (p, &key_id) in node_keys.iter().enumerate() {
        db.set_node_prop(
          node_id,
          key_id,
          PropValue::I64((batch_start + offset + p) as i64),
        )
        .expect("set node prop");
      }
    }
    db.commit().expect("commit nodes");
    node_ids.extend(ids);
  }
  let nodes = start.elapsed();

  let start = Instant::now();
  let mut loaded = 0usize;
  // A deterministic spread of distinct (src, dst) pairs.
  let n = node_ids.len().max(1);
  while loaded < config.edges && !node_ids.is_empty() {
    let end = (loaded + config.batch).min(config.edges);
    begin(&db, config.tx);
    let batch: Vec<_> = (loaded..end)
      .map(|i| {
        let src = node_ids[i % n];
        let dst = node_ids[(i / n + 1 + i % n) % n];
        let props = edge_keys
          .iter()
          .enumerate()
          .map(|(p, &key_id)| (key_id, PropValue::I64((i + p) as i64)))
          .collect();
        (src, etype, dst, props)
      })
      .collect();
    db.add_edges_with_props_batch(batch).expect("add edges");
    db.commit().expect("commit edges");
    loaded = end;
  }
  let edges = start.elapsed();

  if let Some((stop, handle)) = reader {
    let _ = stop.send(());
    handle.join().expect("reader thread");
  }
  let db = Arc::try_unwrap(db).ok().expect("sole owner");
  close_single_file(db).expect("close");
  RunResult { nodes, edges }
}

fn per_sec(count: usize, elapsed: Duration) -> f64 {
  count as f64 / elapsed.as_secs_f64().max(1e-9)
}

fn main() {
  let config = parse_args();
  let mut options = SingleFileOpenOptions::new()
    .sync_mode(config.sync_mode)
    .wal_size(config.wal_size);
  if let Some(mvcc) = config.mvcc {
    options = options.mvcc(mvcc);
  }

  println!("{}", "=".repeat(80));
  println!("Bulk-load benchmark (Rust)");
  println!("{}", "=".repeat(80));
  println!(
    "MVCC: {}{}",
    if options.mvcc { "on" } else { "off" },
    if config.mvcc.is_none() {
      " (library default)"
    } else {
      ""
    }
  );
  println!(
    "Transactions: {}",
    match config.tx {
      TxKind::Bulk => "begin_bulk",
      TxKind::Normal => "begin(false)",
    }
  );
  println!("Open read transaction during the load: {}", config.reader);
  println!(
    "Nodes: {} ({} props), edges: {} ({} props), batch: {}",
    config.nodes, config.node_props, config.edges, config.edge_props, config.batch
  );
  println!(
    "Sync mode: {:?}, WAL size: {} bytes, runs: {}",
    config.sync_mode, config.wal_size, config.runs
  );
  println!("{}", "=".repeat(80));

  for run in 1..=config.runs {
    let result = run_once(&config, &options);
    println!(
      "run {run}: nodes {:>9.0}/s ({:>7.1} ms)  edges {:>9.0}/s ({:>7.1} ms)",
      per_sec(config.nodes, result.nodes),
      result.nodes.as_secs_f64() * 1000.0,
      per_sec(config.edges, result.edges),
      result.edges.as_secs_f64() * 1000.0,
    );
  }
}
