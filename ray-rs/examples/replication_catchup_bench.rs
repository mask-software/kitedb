//! Replication catch-up throughput benchmark.
//!
//! Usage:
//!   cargo run --release --example replication_catchup_bench --no-default-features -- [options]
//!
//! Options:
//!   --seed-commits N      Commits before replica bootstrap (default: 1000)
//!   --backlog-commits N   Commits generated after bootstrap, then caught up (default: 5000)
//!   --max-frames N        Max frames per catch-up pull (default: 256)
//!   --sync-mode MODE      Sync mode: full|normal|off (default: normal)
//!   --segment-max-bytes N Segment rotation threshold (default: 67108864)
//!   --retention-min N     Retention minimum entries (default: 20000)
//!   --mvcc | --no-mvcc    MVCC mode of primary and replica (default: the library default)
//!
//! Unknown options are an error.

use std::env;
use std::process::exit;
use std::time::{Duration, Instant};

use tempfile::tempdir;

use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions, SyncMode,
};
use kitedb::replication::types::ReplicationRole;

#[derive(Debug, Clone)]
struct BenchConfig {
  seed_commits: usize,
  backlog_commits: usize,
  max_frames: usize,
  sync_mode: SyncMode,
  segment_max_bytes: u64,
  retention_min_entries: u64,
  /// None: the library default.
  mvcc: Option<bool>,
}

impl Default for BenchConfig {
  fn default() -> Self {
    Self {
      seed_commits: 1000,
      backlog_commits: 5000,
      max_frames: 256,
      sync_mode: SyncMode::Normal,
      segment_max_bytes: 64 * 1024 * 1024,
      retention_min_entries: 20_000,
      mvcc: None,
    }
  }
}

fn usage_error(message: &str) -> ! {
  eprintln!("error: {message}");
  eprintln!("see the header of examples/replication_catchup_bench.rs for the options");
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
      "--seed-commits" => config.seed_commits = value(&args, &mut i, flag),
      "--backlog-commits" => config.backlog_commits = value(&args, &mut i, flag),
      "--max-frames" => config.max_frames = value(&args, &mut i, flag),
      "--sync-mode" => {
        let mode: String = value(&args, &mut i, flag);
        config.sync_mode = match mode.to_ascii_lowercase().as_str() {
          "full" => SyncMode::Full,
          "normal" => SyncMode::Normal,
          "off" => SyncMode::Off,
          other => usage_error(&format!(
            "--sync-mode must be full, normal or off, not {other}"
          )),
        };
      }
      "--segment-max-bytes" => config.segment_max_bytes = value(&args, &mut i, flag),
      "--retention-min" => config.retention_min_entries = value(&args, &mut i, flag),
      "--mvcc" => config.mvcc = Some(true),
      "--no-mvcc" => config.mvcc = Some(false),
      other => usage_error(&format!("unknown option {other}")),
    }
    i += 1;
  }

  if config.max_frames == 0 {
    config.max_frames = 1;
  }
  if config.backlog_commits == 0 {
    config.backlog_commits = 1;
  }
  config.retention_min_entries = config
    .retention_min_entries
    .max(config.backlog_commits as u64);
  config
}

fn sync_mode_label(mode: SyncMode) -> &'static str {
  match mode {
    SyncMode::Full => "full",
    SyncMode::Normal => "normal",
    SyncMode::Off => "off",
  }
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

/// Base open options shared by the primary and the replica.
fn base_options(config: &BenchConfig) -> SingleFileOpenOptions {
  let options = SingleFileOpenOptions::new()
    .sync_mode(config.sync_mode)
    .auto_checkpoint(false);
  match config.mvcc {
    Some(mvcc) => options.mvcc(mvcc),
    None => options,
  }
}

fn throughput(frames: usize, elapsed: Duration) -> f64 {
  if frames == 0 {
    return 0.0;
  }
  let secs = elapsed.as_secs_f64();
  if secs <= f64::EPSILON {
    frames as f64
  } else {
    frames as f64 / secs
  }
}

fn open_primary(
  path: &std::path::Path,
  sidecar: &std::path::Path,
  config: &BenchConfig,
) -> kitedb::Result<SingleFileDB> {
  open_single_file(
    path,
    base_options(config)
      .replication_role(ReplicationRole::Primary)
      .replication_sidecar_path(sidecar)
      .replication_segment_max_bytes(config.segment_max_bytes)
      .replication_retention_min_entries(config.retention_min_entries),
  )
}

fn open_replica(
  path: &std::path::Path,
  sidecar: &std::path::Path,
  source_db_path: &std::path::Path,
  source_sidecar: &std::path::Path,
  config: &BenchConfig,
) -> kitedb::Result<SingleFileDB> {
  open_single_file(
    path,
    base_options(config)
      .replication_role(ReplicationRole::Replica)
      .replication_sidecar_path(sidecar)
      .replication_source_db_path(source_db_path)
      .replication_source_sidecar_path(source_sidecar),
  )
}

fn append_commits(
  db: &SingleFileDB,
  label: &str,
  count: usize,
  offset: usize,
) -> kitedb::Result<()> {
  for i in 0..count {
    db.begin(false)?;
    db.create_node(Some(&format!("{label}:{}", offset + i)))?;
    let _ = db.commit_with_token()?;
  }
  Ok(())
}

fn main() -> kitedb::Result<()> {
  let config = parse_args();
  println!("replication_catchup_bench");
  println!("sync_mode: {}", sync_mode_label(config.sync_mode));
  println!("mvcc: {}", mvcc_label(config.mvcc));
  println!("seed_commits: {}", config.seed_commits);
  println!("backlog_commits: {}", config.backlog_commits);
  println!("max_frames: {}", config.max_frames);

  let dir = tempdir().expect("tempdir");
  let primary_db_path = dir.path().join("bench-primary.kitedb");
  let primary_sidecar = dir.path().join("bench-primary.sidecar");
  let replica_db_path = dir.path().join("bench-replica.kitedb");
  let replica_sidecar = dir.path().join("bench-replica.sidecar");

  let primary = open_primary(&primary_db_path, &primary_sidecar, &config)?;
  append_commits(&primary, "seed", config.seed_commits, 0)?;

  let replica = open_replica(
    &replica_db_path,
    &replica_sidecar,
    &primary_db_path,
    &primary_sidecar,
    &config,
  )?;
  replica.replica_bootstrap_from_snapshot()?;

  let produce_start = Instant::now();
  append_commits(
    &primary,
    "backlog",
    config.backlog_commits,
    config.seed_commits,
  )?;
  let produce_elapsed = produce_start.elapsed();
  let _ = primary.primary_run_retention()?;

  let catchup_start = Instant::now();
  let mut catchup_loops = 0usize;
  let mut applied_frames = 0usize;
  loop {
    let applied = replica.replica_catch_up_once(config.max_frames)?;
    if applied == 0 {
      break;
    }
    applied_frames = applied_frames.saturating_add(applied);
    catchup_loops = catchup_loops.saturating_add(1);
  }
  let catchup_elapsed = catchup_start.elapsed();

  let primary_status = primary
    .primary_replication_status()
    .ok_or_else(|| kitedb::KiteError::InvalidReplication("missing primary status".to_string()))?;
  let replica_status = replica
    .replica_replication_status()
    .ok_or_else(|| kitedb::KiteError::InvalidReplication("missing replica status".to_string()))?;

  if replica_status.applied_epoch != primary_status.epoch
    || replica_status.applied_log_index != primary_status.head_log_index
  {
    return Err(kitedb::KiteError::InvalidReplication(format!(
      "catch-up mismatch: replica at {}:{}, primary at {}:{}",
      replica_status.applied_epoch,
      replica_status.applied_log_index,
      primary_status.epoch,
      primary_status.head_log_index
    )));
  }

  if replica.count_nodes() != primary.count_nodes() {
    return Err(kitedb::KiteError::InvalidReplication(
      "replica node count mismatch after catch-up".to_string(),
    ));
  }

  let primary_fps = throughput(config.backlog_commits, produce_elapsed);
  let catchup_fps = throughput(applied_frames, catchup_elapsed);
  let throughput_ratio = if primary_fps <= f64::EPSILON {
    0.0
  } else {
    catchup_fps / primary_fps
  };

  println!("applied_frames: {}", applied_frames);
  println!("catchup_loops: {}", catchup_loops);
  println!(
    "produce_elapsed_ms: {:.3}",
    produce_elapsed.as_secs_f64() * 1000.0
  );
  println!(
    "catchup_elapsed_ms: {:.3}",
    catchup_elapsed.as_secs_f64() * 1000.0
  );
  println!("primary_frames_per_sec: {:.2}", primary_fps);
  println!("catchup_frames_per_sec: {:.2}", catchup_fps);
  println!("throughput_ratio: {:.4}", throughput_ratio);
  println!("primary_head_log_index: {}", primary_status.head_log_index);
  println!(
    "replica_applied: {}:{}",
    replica_status.applied_epoch, replica_status.applied_log_index
  );

  close_single_file(replica)?;
  close_single_file(primary)?;
  Ok(())
}
