//! WAL performance benchmark for KiteDB core (Rust).
//!
//! Usage:
//!   cargo run --release --example wal_perf_bench --no-default-features -- [SECTION...] [options]
//!
//! Sections (default: all):
//!   commits      small write transactions (one keyed node each) per second,
//!                in SyncMode::Full and SyncMode::Normal
//!   open         time to open (writable and read-only) a database whose WAL
//!                is 64 MiB or 256 MiB, with a small live range and with the
//!                primary region nearly full
//!   checkpoint   a background checkpoint of a 100k-node graph while one
//!                thread commits nonstop: its duration, and the longest
//!                commit wait meanwhile
//!
//! Options:
//!   --commits N     transactions per sync mode in `commits` (default: 2000)
//!   --runs R        repetitions of each measurement (default: 3)
//!   --nodes N       graph size for `checkpoint` (default: 100000)
//!   --dir PATH      where to create the databases (default: a temp dir)
//!   --mvcc | --no-mvcc  MVCC mode (default: the library default)
//!
//! Unknown options are an error.
//!
//! Every number depends on the machine's load and its file system (macOS
//! fsync and Linux fdatasync cost very differently): compare runs taken on
//! the same machine, back to back. Open times are with a warm page cache.

use std::env;
use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions, SyncMode,
};
use kitedb::types::{ETypeId, NodeId, PropValue};

const MIB: usize = 1024 * 1024;

struct Config {
  sections: Vec<String>,
  commits: usize,
  runs: usize,
  nodes: usize,
  dir: Option<PathBuf>,
  /// None: the library default.
  mvcc: Option<bool>,
}

fn usage_error(message: &str) -> ! {
  eprintln!("error: {message}");
  eprintln!("see the header of examples/wal_perf_bench.rs for the sections and options");
  exit(2);
}

fn parse_args() -> Config {
  let mut config = Config {
    sections: Vec::new(),
    commits: 2000,
    runs: 3,
    nodes: 100_000,
    dir: None,
    mvcc: None,
  };
  let mut args = env::args().skip(1);
  while let Some(arg) = args.next() {
    let mut value = |name: &str| {
      args
        .next()
        .unwrap_or_else(|| usage_error(&format!("{name} needs a value")))
    };
    let number = |name: &str, raw: String| -> usize {
      raw
        .parse()
        .unwrap_or_else(|_| usage_error(&format!("invalid value for {name}: {raw}")))
    };
    match arg.as_str() {
      "--commits" => config.commits = number("--commits", value("--commits")),
      "--runs" => config.runs = number("--runs", value("--runs")),
      "--nodes" => config.nodes = number("--nodes", value("--nodes")),
      "--dir" => config.dir = Some(PathBuf::from(value("--dir"))),
      "--mvcc" => config.mvcc = Some(true),
      "--no-mvcc" => config.mvcc = Some(false),
      "commits" | "open" | "checkpoint" | "all" => config.sections.push(arg),
      other => usage_error(&format!("unknown argument {other}")),
    }
  }
  if config.sections.is_empty() || config.sections.iter().any(|s| s == "all") {
    config.sections = vec!["commits".into(), "open".into(), "checkpoint".into()];
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

fn options(sync_mode: SyncMode, wal_size: usize, mvcc: Option<bool>) -> SingleFileOpenOptions {
  let options = SingleFileOpenOptions::new()
    .sync_mode(sync_mode)
    .wal_size(wal_size)
    .auto_checkpoint(false)
    .background_checkpoint(false);
  match mvcc {
    Some(mvcc) => options.mvcc(mvcc),
    None => options,
  }
}

fn median(mut values: Vec<Duration>) -> Duration {
  values.sort_unstable();
  values[values.len() / 2]
}

fn ms(duration: Duration) -> f64 {
  duration.as_secs_f64() * 1000.0
}

fn commit_node(db: &SingleFileDB, key: &str) {
  db.begin(false).expect("begin");
  db.create_node(Some(key)).expect("create node");
  db.commit().expect("commit");
}

// ============================================================================
// commits
// ============================================================================

fn bench_commits(config: &Config, dir: &Path) {
  println!(
    "## commits/s, one keyed node per transaction ({} per run)",
    config.commits
  );
  for (name, sync_mode) in [("Full", SyncMode::Full), ("Normal", SyncMode::Normal)] {
    let mut rates = Vec::new();
    for run in 0..config.runs {
      let path = dir.join(format!("commits-{name}-{run}.kitedb"));
      let db = open_single_file(&path, options(sync_mode, 64 * MIB, config.mvcc)).expect("open");
      // Warm up: the first commits pay for page-cache misses.
      for index in 0..50 {
        commit_node(&db, &format!("warm-{index}"));
      }
      let started = Instant::now();
      for index in 0..config.commits {
        commit_node(&db, &format!("n-{index}"));
      }
      let elapsed = started.elapsed();
      rates.push(config.commits as f64 / elapsed.as_secs_f64());
      close_single_file(db).expect("close");
      let _ = std::fs::remove_file(&path);
    }
    rates.sort_by(|a, b| a.partial_cmp(b).expect("rate"));
    println!(
      "  {name:<6} median {:>9.0} commits/s   (runs: {})",
      rates[rates.len() / 2],
      rates
        .iter()
        .map(|rate| format!("{rate:.0}"))
        .collect::<Vec<_>>()
        .join(", ")
    );
  }
}

// ============================================================================
// open
// ============================================================================

/// Fill a database's WAL: `fill` false commits a few small transactions;
/// true commits 4 KiB property writes (cycling over 64 nodes, so replay is
/// cheap and the open time is mostly WAL I/O) until the primary region is
/// ~95% full.
fn build_wal(path: &Path, wal_size: usize, fill: bool, mvcc: Option<bool>) -> (u64, usize) {
  let db = open_single_file(path, options(SyncMode::Normal, wal_size, mvcc)).expect("open");
  db.begin(false).expect("begin");
  let blob = db.define_propkey("blob").expect("propkey");
  let nodes: Vec<NodeId> = (0..64)
    .map(|index| db.create_node(Some(&format!("b-{index}"))).expect("node"))
    .collect();
  db.commit().expect("commit");
  let mut commits = 1;
  if fill {
    let value = PropValue::String("x".repeat(4000));
    let mut index = 0usize;
    while db.wal_stats().used < (db.wal_stats().capacity as f64 * 0.75 * 0.95) as u64 {
      db.begin(false).expect("begin");
      for _ in 0..8 {
        db.set_node_prop(nodes[index % nodes.len()], blob, value.clone())
          .expect("prop");
        index += 1;
      }
      db.commit().expect("commit");
      commits += 1;
    }
  } else {
    for index in 0..16 {
      commit_node(&db, &format!("small-{index}"));
      commits += 1;
    }
  }
  let used = db.wal_stats().used;
  close_single_file(db).expect("close");
  (used, commits)
}

fn time_open(
  path: &Path,
  wal_size: usize,
  read_only: bool,
  runs: usize,
  mvcc: Option<bool>,
) -> Duration {
  let mut times = Vec::new();
  for _ in 0..runs.max(3) {
    let started = Instant::now();
    let db = open_single_file(
      path,
      options(SyncMode::Normal, wal_size, mvcc).read_only(read_only),
    )
    .expect("open");
    times.push(started.elapsed());
    if read_only {
      drop(db);
    } else {
      close_single_file(db).expect("close");
    }
  }
  median(times)
}

fn bench_open(config: &Config, dir: &Path) {
  println!(
    "## open time vs WAL size (median of {} opens, warm page cache)",
    config.runs.max(3)
  );
  for wal_mib in [64usize, 256] {
    for fill in [false, true] {
      let path = dir.join(format!("open-{wal_mib}-{fill}.kitedb"));
      let (used, commits) = build_wal(&path, wal_mib * MIB, fill, config.mvcc);
      let writable = time_open(&path, wal_mib * MIB, false, config.runs, config.mvcc);
      let read_only = time_open(&path, wal_mib * MIB, true, config.runs, config.mvcc);
      println!(
        "  WAL {wal_mib:>3} MiB, live {:>7.2} MiB ({commits:>6} commits): writable {:>8.2} ms, \
         read-only {:>8.2} ms",
        used as f64 / MIB as f64,
        ms(writable),
        ms(read_only)
      );
      let _ = std::fs::remove_file(&path);
    }
  }
}

// ============================================================================
// checkpoint
// ============================================================================

fn checkpoint_graph(path: &Path, nodes: usize, mvcc: Option<bool>) -> Arc<SingleFileDB> {
  let db =
    Arc::new(open_single_file(path, options(SyncMode::Normal, 64 * MIB, mvcc)).expect("open"));
  let keys: Vec<String> = (0..nodes).map(|index| format!("n{index}")).collect();
  for chunk in keys.chunks(10_000) {
    db.begin(false).expect("begin");
    let chunk: Vec<Option<&str>> = chunk.iter().map(|key| Some(key.as_str())).collect();
    db.create_nodes_batch(&chunk).expect("create batch");
    db.commit().expect("commit");
  }
  db.begin(false).expect("begin");
  let knows = db.define_etype("knows").expect("etype");
  let weight = db.define_propkey("weight").expect("propkey");
  db.commit().expect("commit");
  let ids: Vec<NodeId> = db.list_nodes();
  for chunk in ids.chunks(10_000) {
    db.begin(false).expect("begin");
    let edges: Vec<(NodeId, ETypeId, NodeId)> =
      chunk.windows(2).map(|w| (w[0], knows, w[1])).collect();
    db.add_edges_batch(&edges).expect("edges");
    for &node in chunk.iter().step_by(10) {
      db.set_node_prop(node, weight, PropValue::I64(node as i64))
        .expect("prop");
    }
    db.commit().expect("commit");
  }
  db.checkpoint().expect("checkpoint");
  for index in 0..200 {
    commit_node(&db, &format!("pre-{index}"));
  }
  db
}

fn bench_checkpoint(config: &Config, dir: &Path) {
  println!(
    "## background checkpoint of {} nodes with one unpaced writer",
    config.nodes
  );
  for run in 0..config.runs {
    let path = dir.join(format!("checkpoint-{run}.kitedb"));
    let db = checkpoint_graph(&path, config.nodes, config.mvcc);
    let running = Arc::new(AtomicBool::new(true));
    let checkpointer = {
      let (db, running) = (Arc::clone(&db), Arc::clone(&running));
      thread::spawn(move || {
        let started = Instant::now();
        let result = db.background_checkpoint();
        let elapsed = started.elapsed();
        running.store(false, Ordering::Release);
        (result, elapsed)
      })
    };
    let mut latencies = Vec::new();
    let mut index = 0usize;
    while running.load(Ordering::Acquire) {
      let started = Instant::now();
      commit_node(&db, &format!("during-{index}"));
      latencies.push(started.elapsed());
      index += 1;
    }
    let (result, elapsed) = checkpointer.join().expect("checkpointer");
    result.expect("background checkpoint");
    let max = latencies.iter().copied().max().unwrap_or_default();
    let median = median(latencies.clone());
    println!(
      "  run {run}: checkpoint {:>8.2} ms; {:>6} commits meanwhile, median {:>6.3} ms, max \
       {:>7.3} ms",
      ms(elapsed),
      latencies.len(),
      ms(median),
      ms(max)
    );
    let db = Arc::try_unwrap(db).ok().expect("sole owner");
    close_single_file(db).expect("close");
    let _ = std::fs::remove_file(&path);
  }
}

fn main() {
  let config = parse_args();
  println!("MVCC: {}", mvcc_label(config.mvcc));
  let temp = tempfile::tempdir().expect("tempdir");
  let dir = config
    .dir
    .clone()
    .unwrap_or_else(|| temp.path().to_path_buf());
  std::fs::create_dir_all(&dir).expect("bench dir");
  for section in &config.sections {
    match section.as_str() {
      "commits" => bench_commits(&config, &dir),
      "open" => bench_open(&config, &dir),
      "checkpoint" => bench_checkpoint(&config, &dir),
      _ => unreachable!(),
    }
  }
}
