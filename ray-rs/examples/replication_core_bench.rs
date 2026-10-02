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

use std::env;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileOpenOptions, SyncMode,
};
use kitedb::replication::types::ReplicationRole;
use kitedb::types::PropValue;

fn arg_value(args: &[String], name: &str) -> Option<String> {
  args
    .iter()
    .position(|arg| arg == name)
    .and_then(|index| args.get(index + 1))
    .cloned()
}

fn arg_flag(args: &[String], name: &str) -> bool {
  args.iter().any(|arg| arg == name)
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

fn primary_options(sync_mode: SyncMode) -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .sync_mode(sync_mode)
    .auto_checkpoint(false)
    .wal_size(64 << 20)
    .replication_role(ReplicationRole::Primary)
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

fn build_source(path: &Path, nodes: usize) -> kitedb::Result<()> {
  let _ = std::fs::remove_file(path);
  let started = Instant::now();
  let db = open_single_file(path, primary_options(SyncMode::Normal))?;
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

fn bootstrap(source: &Path, replica_wal_mb: Option<usize>) -> kitedb::Result<()> {
  let dir = tempfile::tempdir()?;
  let replica_path = dir.path().join("bench-replica.kitedb");
  let mut options = SingleFileOpenOptions::new()
    .replication_role(ReplicationRole::Replica)
    .replication_source_db_path(source);
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

fn commit_latency(commits: usize, full_fsync: bool, with_primary: bool) -> kitedb::Result<()> {
  let dir = tempfile::tempdir()?;
  let path = dir.path().join("bench-commit.kitedb");
  let mut options = SingleFileOpenOptions::new()
    .sync_mode(SyncMode::Full)
    .full_fsync(full_fsync)
    .auto_checkpoint(false)
    .wal_size(64 << 20);
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

fn build_blob(path: &Path, mb: usize) -> kitedb::Result<()> {
  const WAL_MB: usize = 4;
  let _ = std::fs::remove_file(path);
  let db = open_single_file(
    path,
    primary_options(SyncMode::Normal).wal_size(WAL_MB << 20),
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

fn export(path: &Path, format: &str, repeat: usize) -> kitedb::Result<()> {
  // The file keeps the WAL size it was built with.
  let db = open_single_file(
    path,
    SingleFileOpenOptions::new()
      .sync_mode(SyncMode::Normal)
      .auto_checkpoint(false)
      .replication_role(ReplicationRole::Primary),
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
      other => {
        eprintln!("unknown format {other}");
        std::process::exit(2);
      }
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
  let path = arg_value(&args, "--path").map(PathBuf::from);
  match scenario.as_str() {
    "build-source" => build_source(
      &path.expect("--path"),
      arg_value(&args, "--nodes")
        .and_then(|value| value.parse().ok())
        .unwrap_or(200_000),
    ),
    "bootstrap" => bootstrap(
      &PathBuf::from(arg_value(&args, "--source").expect("--source")),
      arg_value(&args, "--replica-wal-mb").and_then(|value| value.parse().ok()),
    ),
    "commit-latency" => commit_latency(
      arg_value(&args, "--commits")
        .and_then(|value| value.parse().ok())
        .unwrap_or(200),
      arg_flag(&args, "--full-fsync"),
      !arg_flag(&args, "--no-primary"),
    ),
    "build-blob" => build_blob(
      &path.expect("--path"),
      arg_value(&args, "--mb")
        .and_then(|value| value.parse().ok())
        .unwrap_or(50),
    ),
    "export" => export(
      &path.expect("--path"),
      &arg_value(&args, "--format").unwrap_or_else(|| "json".to_string()),
      arg_value(&args, "--repeat")
        .and_then(|value| value.parse().ok())
        .unwrap_or(3),
    ),
    _ => {
      eprintln!(
        "usage: replication_core_bench <build-source|bootstrap|commit-latency|build-blob|export> ..."
      );
      std::process::exit(2);
    }
  }
}
