//! Per-commit cost with a long-lived MVCC reader and many committed write keys.
//!
//! A reader that stays open pins every committed-write entry (each may still
//! conflict with it), so the committed-write index cannot be pruned. This
//! measures what each further commit costs in that state, and how many
//! committed transaction records the manager keeps.
//!
//! Two levels:
//!   tx_manager  TxManager alone: one bulk commit of `--keys` write keys, then
//!               `--commits` commits of one key each
//!   db          SingleFileDB with MVCC: one commit creating `--keys / 2` keyed
//!               nodes (two write keys each), then `--commits` single-prop commits
//!
//! Usage:
//!   cargo run --release --example mvcc_commit_prune_bench --no-default-features -- [options]
//!
//! Options:
//!   --keys N      Committed write keys pinned by the reader (default: 120000)
//!   --commits N   Measured commits (default: 2000)

use std::env;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::tempdir;

use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions, SyncMode,
};
use kitedb::mvcc::TxManager;
use kitedb::types::{NodeId, PropValue, TxKey};

struct BenchConfig {
  keys: usize,
  commits: usize,
}

fn parse_args() -> BenchConfig {
  let mut config = BenchConfig {
    keys: 120_000,
    commits: 2_000,
  };
  let args: Vec<String> = env::args().collect();
  let mut i = 1;
  while i < args.len() {
    let value = args.get(i + 1).map(String::as_str);
    match (args[i].as_str(), value) {
      ("--keys", Some(v)) => config.keys = v.parse().expect("--keys"),
      ("--commits", Some(v)) => config.commits = v.parse().expect("--commits"),
      (flag, _) => panic!("unknown or incomplete option: {flag}"),
    }
    i += 2;
  }
  config.commits = config.commits.max(1);
  config
}

fn report(level: &str, mut samples: Vec<Duration>, committed_writes: usize, extra: &str) {
  samples.sort();
  let total: Duration = samples.iter().sum();
  let pct = |p: usize| samples[(samples.len() - 1) * p / 100];
  println!(
    "{level:<10} commits={} mean={:?} p50={:?} p99={:?} max={:?} committed_writes={committed_writes}{extra}",
    samples.len(),
    total / samples.len() as u32,
    pct(50),
    pct(99),
    samples[samples.len() - 1],
  );
}

fn bench_tx_manager(config: &BenchConfig) {
  let mut tx_mgr = TxManager::new();
  let (reader, _) = tx_mgr.begin_tx();

  let (bulk, _) = tx_mgr.begin_tx();
  for i in 0..config.keys {
    tx_mgr.record_write(bulk, TxKey::Node(i as NodeId));
  }
  tx_mgr.commit_tx(bulk).expect("commit bulk");

  let mut samples = Vec::with_capacity(config.commits);
  for i in 0..config.commits {
    let (txid, _) = tx_mgr.begin_tx();
    tx_mgr.record_read(txid, TxKey::Node(i as NodeId));
    tx_mgr.record_write(txid, TxKey::Node((config.keys + i) as NodeId));
    let started = Instant::now();
    tx_mgr.commit_tx(txid).expect("commit");
    samples.push(started.elapsed());
  }

  let (retained_txs, retained_keys) = tx_mgr
    .all_txs()
    .filter(|(&txid, _)| txid != reader)
    .fold((0, 0), |(txs, keys), (_, tx)| {
      (txs + 1, keys + tx.read_set.len() + tx.write_set.len())
    });
  report(
    "tx_manager",
    samples,
    tx_mgr.committed_writes_stats().size,
    &format!(" retained_committed_txs={retained_txs} retained_set_keys={retained_keys}"),
  );
}

fn bench_db(config: &BenchConfig) {
  let dir = tempdir().expect("tempdir");
  let options = SingleFileOpenOptions::new()
    .mvcc(true)
    .auto_checkpoint(false)
    .background_checkpoint(false)
    .sync_mode(SyncMode::Off)
    .wal_size(512 * 1024 * 1024);
  let db = Arc::new(open_single_file(dir.path().join("prune.kitedb"), options).expect("open"));

  db.begin(false).expect("begin schema");
  let prop = db.define_propkey("counter").expect("define propkey");
  let hot = db.create_node(Some("hot")).expect("create hot node");
  db.set_node_prop(hot, prop, PropValue::I64(0))
    .expect("seed prop");
  db.commit().expect("commit schema");

  let (ready_tx, ready_rx) = mpsc::channel();
  let (release_tx, release_rx) = mpsc::channel::<()>();
  let reader = {
    let db: Arc<SingleFileDB> = Arc::clone(&db);
    thread::spawn(move || {
      db.begin(true).expect("begin reader");
      ready_tx.send(()).expect("reader ready");
      let _ = release_rx.recv();
      db.commit().expect("end reader");
    })
  };
  ready_rx.recv().expect("reader began");

  db.begin(false).expect("begin bulk");
  for i in 0..config.keys / 2 {
    db.create_node(Some(&format!("k{i}"))).expect("create node");
  }
  db.commit().expect("commit bulk");

  let mut samples = Vec::with_capacity(config.commits);
  for i in 0..config.commits {
    db.begin(false).expect("begin");
    db.set_node_prop(hot, prop, PropValue::I64(i as i64))
      .expect("set prop");
    let started = Instant::now();
    db.commit().expect("commit");
    samples.push(started.elapsed());
  }
  let committed_writes = db
    .stats()
    .mvcc_stats
    .map_or(0, |stats| stats.committed_writes_size);
  report("db", samples, committed_writes, "");

  drop(release_tx);
  reader.join().expect("reader thread");
  let db = Arc::try_unwrap(db).unwrap_or_else(|_| panic!("db still shared"));
  close_single_file(db).expect("close");
}

/// Asks macOS to keep the calling thread on performance cores. Hybrid Apple
/// chips otherwise move bench threads between performance and efficiency
/// cores, which swings results by 2-5x between runs.
fn prefer_performance_cores() {
  #[cfg(target_os = "macos")]
  // SAFETY: sets the calling thread's QoS class; takes no pointers.
  unsafe {
    libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0);
  }
}

fn main() {
  prefer_performance_cores();
  let config = parse_args();
  println!(
    "mvcc_commit_prune_bench: keys={} commits={} (one reader open throughout)",
    config.keys, config.commits
  );
  bench_tx_manager(&config);
  bench_db(&config);
}
