//! Replication core benchmarks: snapshot bootstrap, Full-mode commit latency
//! with a primary sidecar, and snapshot transport export.
//!
//! Each scenario runs in its own process, so `peak_rss_mb` (getrusage) is
//! that scenario's peak.
//!
//! Usage:
//!   cargo run --release --no-default-features --example replication_core_bench -- <scenario> [options]
//!
//! Scenarios:
//!   build-source --path P --nodes N    Primary with N nodes (a name and a bio prop, a label, a
//!                                      chain of edges), checkpointed.
//!   bootstrap --source P [--replica-wal-mb M]
//!                                      Bootstrap a fresh replica from P (default WAL 4 MB).
//!   commit-latency [--commits N] [--full-fsync] [--no-primary]
//!                                      Full-mode single-node commits; per-commit latency.
//!   build-blob --path P --mb M         Primary of about M MB (1 KiB random props), checkpointed.
//!   export --path P [--format json|binary] [--repeat N]
//!                                      Snapshot transport export with data (binary: the
//!                                      SnapshotTransport struct, json: its JSON serializer).
//!
//! Every scenario also takes `--mvcc` / `--no-mvcc`: the MVCC mode of the
//! databases it opens (default: the library default). Options a scenario does
//! not take are an error.

use std::env;
use std::path::{Path, PathBuf};
use std::process::exit;
use std::str::FromStr;
use std::time::{Duration, Instant};

use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileOpenOptions, SyncMode,
};
use kitedb::replication::types::ReplicationRole;
use kitedb::types::PropValue;

const USAGE: &str =
  "usage: replication_core_bench <build-source|bootstrap|commit-latency|build-blob|export> ...";

fn usage_error(message: &str) -> ! {
  eprintln!("error: {message}");
  eprintln!("{USAGE}");
  eprintln!("see the header of examples/replication_core_bench.rs for each scenario's options");
  exit(2);
}

/// Rejects any argument after the scenario that is not one of `values`
/// (each followed by a value) or `flags` (`--mvcc` / `--no-mvcc` are always
/// allowed).
fn check_args(args: &[String], values: &[&str], flags: &[&str]) {
  let mut i = 2;
  while i < args.len() {
    let arg = args[i].as_str();
    if values.contains(&arg) {
      if args.get(i + 1).is_none() {
        usage_error(&format!("{arg} needs a value"));
      }
      i += 1;
    } else if !flags.contains(&arg) && arg != "--mvcc" && arg != "--no-mvcc" {
      usage_error(&format!("unknown option {arg} for {}", args[1]));
    }
    i += 1;
  }
}

fn arg_value(args: &[String], name: &str) -> Option<String> {
  args
    .iter()
    .position(|arg| arg == name)
    .and_then(|index| args.get(index + 1))
    .cloned()
}

/// The parsed value of `name`, if given; an invalid value is an error.
fn arg_parsed<T: FromStr>(args: &[String], name: &str) -> Option<T> {
  arg_value(args, name).map(|raw| {
    raw
      .parse()
      .unwrap_or_else(|_| usage_error(&format!("invalid value for {name}: {raw}")))
  })
}

fn arg_flag(args: &[String], name: &str) -> bool {
  args.iter().any(|arg| arg == name)
}

/// `--mvcc` / `--no-mvcc`, the last one given wins; None: the library default.
fn arg_mvcc(args: &[String]) -> Option<bool> {
  args.iter().rev().find_map(|arg| match arg.as_str() {
    "--mvcc" => Some(true),
    "--no-mvcc" => Some(false),
    _ => None,
  })
}

/// The MVCC mode the scenario runs in, for its output.
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

fn with_mvcc(options: SingleFileOpenOptions, mvcc: Option<bool>) -> SingleFileOpenOptions {
  match mvcc {
    Some(mvcc) => options.mvcc(mvcc),
    None => options,
  }
}

fn peak_rss_mb() -> f64 {
  let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
  // SAFETY: getrusage writes into the zeroed struct.
  unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
  let raw = usage.ru_maxrss as f64;
  if cfg!(target_os = "macos") {
    raw / (1024.0 * 1024.0)
  } else {
    raw / 1024.0
  }
}

fn ms(duration: Duration) -> f64 {
  duration.as_secs_f64() * 1000.0
}

fn primary_options(sync_mode: SyncMode, mvcc: Option<bool>) -> SingleFileOpenOptions {
  with_mvcc(
    SingleFileOpenOptions::new()
      .sync_mode(sync_mode)
      .auto_checkpoint(false)
      .wal_size(64 << 20)
      .replication_role(ReplicationRole::Primary),
    mvcc,
  )
}

/// Deterministic pseudo-random hex, so checkpoint compression cannot shrink it.
fn noise(seed: u64, len: usize) -> String {
  let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
  let mut out = String::with_capacity(len);
  while out.len() < len {
    state ^= state << 13;
    state ^= state >> 7;
    state ^= state << 17;
    out.push_str(&format!("{state:016x}"));
  }
  out.truncate(len);
  out
}

fn build_source(path: &Path, nodes: usize, mvcc: Option<bool>) -> kitedb::Result<()> {
  let _ = std::fs::remove_file(path);
  let started = Instant::now();
  let db = open_single_file(path, primary_options(SyncMode::Normal, mvcc))?;
  db.begin(false)?;
  let name = db.define_propkey("name")?;
  let bio = db.define_propkey("bio")?;
  let person = db.define_label("Person")?;
  let knows = db.define_etype("KNOWS")?;
  db.commit()?;
  let mut previous = None;
  for chunk_start in (0..nodes).step_by(10_000) {
    db.begin(false)?;
    for i in chunk_start..(chunk_start + 10_000).min(nodes) {
      let node = db.create_node(Some(&format!("person-{i}")))?;
      db.set_node_prop(node, name, PropValue::String(format!("person {i}")))?;
      db.set_node_prop(node, bio, PropValue::String(noise(i as u64, 48)))?;
      db.add_node_label(node, person)?;
      if let Some(previous) = previous {
        db.add_edge(previous, knows, node)?;
      }
      previous = Some(node);
    }
    db.commit()?;
    db.checkpoint()?;
  }
  close_single_file(db)?;
  println!("build_source_ms: {:.1}", ms(started.elapsed()));
  println!(
    "source_bytes: {}",
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
  );
  Ok(())
}

fn bootstrap(
  source: &Path,
  replica_wal_mb: Option<usize>,
  mvcc: Option<bool>,
) -> kitedb::Result<()> {
  let dir = tempfile::tempdir()?;
  let replica_path = dir.path().join("bench-replica.kitedb");
  let mut options = with_mvcc(
    SingleFileOpenOptions::new()
      .replication_role(ReplicationRole::Replica)
      .replication_source_db_path(source),
    mvcc,
  );
  if let Some(mb) = replica_wal_mb {
    options = options.wal_size(mb << 20);
  }
  let replica = open_single_file(&replica_path, options)?;
  let rss_before = peak_rss_mb();
  let started = Instant::now();
  let result = replica.replica_bootstrap_from_snapshot();
  let elapsed = started.elapsed();
  println!("bootstrap_ms: {:.1}", ms(elapsed));
  println!("bootstrap_result: {result:?}");
  println!("replica_nodes: {}", replica.count_nodes());
  println!("peak_rss_mb_before_bootstrap: {rss_before:.1}");
  println!("peak_rss_mb: {:.1}", peak_rss_mb());
  close_single_file(replica)?;
  Ok(())
}

fn commit_latency(
  commits: usize,
  full_fsync: bool,
  with_primary: bool,
  mvcc: Option<bool>,
) -> kitedb::Result<()> {
  let dir = tempfile::tempdir()?;
  let path = dir.path().join("bench-commit.kitedb");
  let mut options = with_mvcc(
    SingleFileOpenOptions::new()
      .sync_mode(SyncMode::Full)
      .full_fsync(full_fsync)
      .auto_checkpoint(false)
      .wal_size(64 << 20),
    mvcc,
  );
  if with_primary {
    options = options.replication_role(ReplicationRole::Primary);
  }
  let db = open_single_file(&path, options)?;
  let mut latencies = Vec::with_capacity(commits);
  for i in 0..commits {
    let started = Instant::now();
    db.begin(false)?;
    db.create_node(Some(&format!("n{i}")))?;
    db.commit_with_token()?;
    latencies.push(started.elapsed());
  }
  close_single_file(db)?;
  latencies.sort_unstable();
  let total: Duration = latencies.iter().sum();
  let pick = |q: f64| latencies[((latencies.len() as f64 - 1.0) * q).round() as usize];
  println!("commits: {commits} full_fsync: {full_fsync} primary: {with_primary}");
  println!("commit_avg_ms: {:.3}", ms(total) / commits as f64);
  println!("commit_p50_ms: {:.3}", ms(pick(0.5)));
  println!("commit_p99_ms: {:.3}", ms(pick(0.99)));
  Ok(())
}

fn build_blob(path: &Path, mb: usize, mvcc: Option<bool>) -> kitedb::Result<()> {
  const WAL_MB: usize = 4;
  let _ = std::fs::remove_file(path);
  let db = open_single_file(
    path,
    primary_options(SyncMode::Normal, mvcc).wal_size(WAL_MB << 20),
  )?;
  db.begin(false)?;
  let blob = db.define_propkey("blob")?;
  db.commit()?;
  // About 1 KiB per node; the file also holds the WAL.
  let nodes = mb.saturating_sub(WAL_MB).max(1) * 1024;
  for chunk_start in (0..nodes).step_by(1_000) {
    db.begin(false)?;
    for i in chunk_start..(chunk_start + 1_000).min(nodes) {
      let node = db.create_node(Some(&format!("blob-{i}")))?;
      db.set_node_prop(node, blob, PropValue::String(noise(i as u64, 1024)))?;
    }
    db.commit()?;
    db.checkpoint()?;
  }
  close_single_file(db)?;
  println!(
    "blob_bytes: {}",
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
  );
  Ok(())
}

fn export(path: &Path, format: &str, repeat: usize, mvcc: Option<bool>) -> kitedb::Result<()> {
  // The file keeps the WAL size it was built with.
  let db = open_single_file(
    path,
    with_mvcc(
      SingleFileOpenOptions::new()
        .sync_mode(SyncMode::Normal)
        .auto_checkpoint(false)
        .replication_role(ReplicationRole::Primary),
      mvcc,
    ),
  )?;
  let rss_before = peak_rss_mb();
  let mut best = Duration::MAX;
  for _ in 0..repeat.max(1) {
    let started = Instant::now();
    let bytes = match format {
      "json" => db
        .primary_export_snapshot_transport_json(true)
        .map(|json| json.len()),
      "binary" => db
        .primary_export_snapshot_transport(true)
        .map(|snapshot| snapshot.data.map_or(0, |data| data.len())),
      other => unreachable!("--format is checked when parsed: {other}"),
    };
    let elapsed = started.elapsed();
    best = best.min(elapsed);
    match bytes {
      Ok(bytes) => println!("export_{format}_bytes: {bytes} ms: {:.1}", ms(elapsed)),
      Err(error) => {
        println!("export_{format}_error: {error}");
        break;
      }
    }
  }
  println!("export_{format}_best_ms: {:.1}", ms(best));
  println!("peak_rss_mb_before_export: {rss_before:.1}");
  println!("peak_rss_mb: {:.1}", peak_rss_mb());
  close_single_file(db)?;
  Ok(())
}

fn main() -> kitedb::Result<()> {
  let args: Vec<String> = env::args().collect();
  let scenario = args.get(1).cloned().unwrap_or_default();
  match scenario.as_str() {
    "build-source" => check_args(&args, &["--path", "--nodes"], &[]),
    "bootstrap" => check_args(&args, &["--source", "--replica-wal-mb"], &[]),
    "commit-latency" => check_args(&args, &["--commits"], &["--full-fsync", "--no-primary"]),
    "build-blob" => check_args(&args, &["--path", "--mb"], &[]),
    "export" => check_args(&args, &["--path", "--format", "--repeat"], &[]),
    "" => usage_error("missing scenario"),
    other => usage_error(&format!("unknown scenario {other}")),
  }
  let path = arg_value(&args, "--path").map(PathBuf::from);
  let required_path = || {
    path
      .clone()
      .unwrap_or_else(|| usage_error("--path is required"))
  };
  let mvcc = arg_mvcc(&args);
  println!("mvcc: {}", mvcc_label(mvcc));
  match scenario.as_str() {
    "build-source" => build_source(
      &required_path(),
      arg_parsed(&args, "--nodes").unwrap_or(200_000),
      mvcc,
    ),
    "bootstrap" => bootstrap(
      &PathBuf::from(
        arg_value(&args, "--source").unwrap_or_else(|| usage_error("--source is required")),
      ),
      arg_parsed(&args, "--replica-wal-mb"),
      mvcc,
    ),
    "commit-latency" => commit_latency(
      arg_parsed(&args, "--commits").unwrap_or(200),
      arg_flag(&args, "--full-fsync"),
      !arg_flag(&args, "--no-primary"),
      mvcc,
    ),
    "build-blob" => build_blob(
      &required_path(),
      arg_parsed(&args, "--mb").unwrap_or(50),
      mvcc,
    ),
    "export" => {
      let format = arg_value(&args, "--format").unwrap_or_else(|| "json".to_string());
      if format != "json" && format != "binary" {
        usage_error(&format!("--format must be json or binary, not {format}"));
      }
      export(
        &required_path(),
        &format,
        arg_parsed(&args, "--repeat").unwrap_or(3),
        mvcc,
      )
    }
    _ => unreachable!("scenarios are checked above"),
  }
}
