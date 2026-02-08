//! Replication long-run soak benchmark with lag churn, promotion fencing, and reseed recovery.
//!
//! Usage:
//!   cargo run --release --example replication_soak_bench --no-default-features -- [options]
//!
//! Options:
//!   --replicas N                 Replica count (default: 5)
//!   --cycles N                   Soak cycles (default: 18)
//!   --commits-per-cycle N        Primary commits per cycle (default: 120)
//!   --active-replicas N          Replicas actively catching up each cycle (default: 3)
//!   --churn-interval N           Cycles before rotating active replica window (default: 3)
//!   --promotion-interval N       Promote primary every N cycles; 0 disables (default: 6)
//!   --reseed-check-interval N    Probe lagging replicas for reseed every N cycles; 0 disables (default: 3)
//!   --max-frames N               Max frames per replica pull (default: 128)
//!   --recovery-max-loops N       Max catch-up loops when recovering lag (default: 80)
//!   --segment-max-bytes N        Sidecar segment rotation threshold (default: 1)
//!   --retention-min N            Primary retention min entries (default: 64)
//!   --sync-mode MODE             Sync mode: full|normal|off (default: normal)

use std::env;
use std::time::Instant;

use tempfile::tempdir;

use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions, SyncMode,
};
use kitedb::replication::types::ReplicationRole;

#[derive(Debug, Clone)]
struct SoakConfig {
  replicas: usize,
  cycles: usize,
  commits_per_cycle: usize,
  active_replicas_per_cycle: usize,
  churn_interval: usize,
  promotion_interval: usize,
  reseed_check_interval: usize,
  max_frames: usize,
  recovery_max_loops: usize,
  segment_max_bytes: u64,
  retention_min_entries: u64,
  sync_mode: SyncMode,
}

impl Default for SoakConfig {
  fn default() -> Self {
    Self {
      replicas: 5,
      cycles: 18,
      commits_per_cycle: 120,
      active_replicas_per_cycle: 3,
      churn_interval: 3,
      promotion_interval: 6,
      reseed_check_interval: 3,
      max_frames: 128,
      recovery_max_loops: 80,
      segment_max_bytes: 1,
      retention_min_entries: 64,
      sync_mode: SyncMode::Normal,
    }
  }
}

struct ReplicaSlot {
  id: String,
  db: SingleFileDB,
}

fn parse_args() -> SoakConfig {
  let mut config = SoakConfig::default();
  let args: Vec<String> = env::args().collect();

  let mut i = 1;
  while i < args.len() {
    match args[i].as_str() {
      "--replicas" => {
        if let Some(value) = args.get(i + 1) {
          config.replicas = value.parse().unwrap_or(config.replicas);
          i += 1;
        }
      }
      "--cycles" => {
        if let Some(value) = args.get(i + 1) {
          config.cycles = value.parse().unwrap_or(config.cycles);
          i += 1;
        }
      }
      "--commits-per-cycle" => {
        if let Some(value) = args.get(i + 1) {
          config.commits_per_cycle = value.parse().unwrap_or(config.commits_per_cycle);
          i += 1;
        }
      }
      "--active-replicas" => {
        if let Some(value) = args.get(i + 1) {
          config.active_replicas_per_cycle =
            value.parse().unwrap_or(config.active_replicas_per_cycle);
          i += 1;
        }
      }
      "--churn-interval" => {
        if let Some(value) = args.get(i + 1) {
          config.churn_interval = value.parse().unwrap_or(config.churn_interval);
          i += 1;
        }
      }
      "--promotion-interval" => {
        if let Some(value) = args.get(i + 1) {
          config.promotion_interval = value.parse().unwrap_or(config.promotion_interval);
          i += 1;
        }
      }
      "--reseed-check-interval" => {
        if let Some(value) = args.get(i + 1) {
          config.reseed_check_interval = value.parse().unwrap_or(config.reseed_check_interval);
          i += 1;
        }
      }
      "--max-frames" => {
        if let Some(value) = args.get(i + 1) {
          config.max_frames = value.parse().unwrap_or(config.max_frames);
          i += 1;
        }
      }
      "--recovery-max-loops" => {
        if let Some(value) = args.get(i + 1) {
          config.recovery_max_loops = value.parse().unwrap_or(config.recovery_max_loops);
          i += 1;
        }
      }
      "--segment-max-bytes" => {
        if let Some(value) = args.get(i + 1) {
          config.segment_max_bytes = value.parse().unwrap_or(config.segment_max_bytes);
          i += 1;
        }
      }
      "--retention-min" => {
        if let Some(value) = args.get(i + 1) {
          config.retention_min_entries = value.parse().unwrap_or(config.retention_min_entries);
          i += 1;
        }
      }
      "--sync-mode" => {
        if let Some(value) = args.get(i + 1) {
          config.sync_mode = match value.to_ascii_lowercase().as_str() {
            "full" => SyncMode::Full,
            "off" => SyncMode::Off,
            _ => SyncMode::Normal,
          };
          i += 1;
        }
      }
      _ => {}
    }
    i += 1;
  }

  config.replicas = config.replicas.max(1);
  config.cycles = config.cycles.max(1);
  config.commits_per_cycle = config.commits_per_cycle.max(1);
  config.active_replicas_per_cycle = config.active_replicas_per_cycle.max(1).min(config.replicas);
  config.churn_interval = config.churn_interval.max(1);
  config.max_frames = config.max_frames.max(1);
  config.recovery_max_loops = config.recovery_max_loops.max(1);
  config.segment_max_bytes = config.segment_max_bytes.max(1);
  config.retention_min_entries = config.retention_min_entries.max(1);
  config
}

fn sync_mode_label(mode: SyncMode) -> &'static str {
  match mode {
    SyncMode::Full => "full",
    SyncMode::Normal => "normal",
    SyncMode::Off => "off",
  }
}

fn open_primary(
  path: &std::path::Path,
  sidecar: &std::path::Path,
  config: &SoakConfig,
) -> kitedb::Result<SingleFileDB> {
  open_single_file(
    path,
    SingleFileOpenOptions::new()
      .sync_mode(config.sync_mode)
      .auto_checkpoint(false)
      .replication_role(ReplicationRole::Primary)
      .replication_sidecar_path(sidecar)
      .replication_segment_max_bytes(config.segment_max_bytes)
      .replication_retention_min_entries(config.retention_min_entries),
  )
}

fn open_replica(
  path: &std::path::Path,
  sidecar: &std::path::Path,
  source_db: &std::path::Path,
  source_sidecar: &std::path::Path,
  config: &SoakConfig,
) -> kitedb::Result<SingleFileDB> {
  open_single_file(
    path,
    SingleFileOpenOptions::new()
      .sync_mode(config.sync_mode)
      .auto_checkpoint(false)
      .replication_role(ReplicationRole::Replica)
      .replication_sidecar_path(sidecar)
      .replication_source_db_path(source_db)
      .replication_source_sidecar_path(source_sidecar),
  )
}

fn primary_status(
  db: &SingleFileDB,
) -> kitedb::Result<kitedb::replication::primary::PrimaryReplicationStatus> {
  db.primary_replication_status().ok_or_else(|| {
    kitedb::KiteError::InvalidReplication("missing primary replication status".to_string())
  })
}

fn replica_status(
  db: &SingleFileDB,
) -> kitedb::Result<kitedb::replication::replica::ReplicaReplicationStatus> {
  db.replica_replication_status().ok_or_else(|| {
    kitedb::KiteError::InvalidReplication("missing replica replication status".to_string())
  })
}

fn append_cycle_commits(
  db: &SingleFileDB,
  cycle: usize,
  count: usize,
  next_id: &mut usize,
  expected_keys: &mut Vec<String>,
) -> kitedb::Result<()> {
  for _ in 0..count {
    let key = format!("soak-{cycle}-{}", *next_id);
    db.begin(false)?;
    db.create_node(Some(&key))?;
    let _ = db.commit_with_token()?.ok_or_else(|| {
      kitedb::KiteError::InvalidReplication("primary commit token missing".to_string())
    })?;
    expected_keys.push(key);
    *next_id = next_id.saturating_add(1);
  }
  Ok(())
}

fn catch_up_to_target(
  replica: &SingleFileDB,
  target_log_index: u64,
  max_frames: usize,
  max_loops: usize,
) -> kitedb::Result<usize> {
  let mut loops = 0usize;
  loop {
    let status = replica_status(replica)?;
    if status.needs_reseed {
      return Err(kitedb::KiteError::InvalidReplication(
        "replica needs reseed".to_string(),
      ));
    }
    if status.applied_log_index >= target_log_index {
      return Ok(loops);
    }
    if loops >= max_loops {
      return Err(kitedb::KiteError::InvalidReplication(format!(
        "replica catch-up exceeded max loops ({max_loops})"
      )));
    }

    let applied = match replica.replica_catch_up_once(max_frames) {
      Ok(applied) => applied,
      Err(err) => {
        let status = replica_status(replica)?;
        if status.needs_reseed || err.to_string().contains("reseed") {
          return Err(kitedb::KiteError::InvalidReplication(
            "replica needs reseed".to_string(),
          ));
        }
        return Err(err);
      }
    };

    loops = loops.saturating_add(1);
    if applied == 0 {
      let status = replica_status(replica)?;
      if status.applied_log_index >= target_log_index {
        return Ok(loops);
      }
      return Err(kitedb::KiteError::InvalidReplication(
        "replica catch-up stalled before target".to_string(),
      ));
    }
  }
}

fn main() -> kitedb::Result<()> {
  let config = parse_args();
  println!("replication_soak_bench");
  println!("sync_mode: {}", sync_mode_label(config.sync_mode));
  println!("replicas: {}", config.replicas);
  println!("cycles: {}", config.cycles);
  println!("commits_per_cycle: {}", config.commits_per_cycle);
  println!(
    "active_replicas_per_cycle: {}",
    config.active_replicas_per_cycle
  );
  println!("churn_interval: {}", config.churn_interval);
  println!("promotion_interval: {}", config.promotion_interval);
  println!("reseed_check_interval: {}", config.reseed_check_interval);
  println!("max_frames: {}", config.max_frames);
  println!("recovery_max_loops: {}", config.recovery_max_loops);

  let started = Instant::now();
  let dir = tempdir().expect("tempdir");
  let primary_db_path = dir.path().join("soak-primary.kitedb");
  let primary_sidecar = dir.path().join("soak-primary.sidecar");

  let primary = open_primary(&primary_db_path, &primary_sidecar, &config)?;
  let mut stale_probe = open_primary(&primary_db_path, &primary_sidecar, &config)?;

  let mut replicas: Vec<ReplicaSlot> = Vec::with_capacity(config.replicas);
  for idx in 0..config.replicas {
    let replica_db_path = dir.path().join(format!("soak-replica-{idx}.kitedb"));
    let replica_sidecar = dir.path().join(format!("soak-replica-{idx}.sidecar"));
    let replica = open_replica(
      &replica_db_path,
      &replica_sidecar,
      &primary_db_path,
      &primary_sidecar,
      &config,
    )?;
    replica.replica_bootstrap_from_snapshot()?;
    replicas.push(ReplicaSlot {
      id: format!("replica-{idx}"),
      db: replica,
    });
  }

  let mut expected_keys =
    Vec::with_capacity(config.cycles.saturating_mul(config.commits_per_cycle));
  let mut next_id = 0usize;

  let mut writes_committed = 0usize;
  let mut promotion_count = 0usize;
  let mut stale_fence_rejections = 0usize;
  let mut reseed_count = 0usize;
  let mut reseed_recovery_successes = 0usize;
  let mut max_recovery_loops_seen = 0usize;
  let mut max_observed_lag = 0u64;
  let divergence_violations = 0usize;

  for cycle in 0..config.cycles {
    append_cycle_commits(
      &primary,
      cycle,
      config.commits_per_cycle,
      &mut next_id,
      &mut expected_keys,
    )?;
    writes_committed = writes_committed.saturating_add(config.commits_per_cycle);

    let head = primary_status(&primary)?;

    let active_start = (cycle / config.churn_interval) % replicas.len();
    let mut active = vec![false; replicas.len()];
    for offset in 0..config.active_replicas_per_cycle {
      active[(active_start + offset) % replicas.len()] = true;
    }

    for (idx, slot) in replicas.iter_mut().enumerate() {
      if !active[idx] {
        continue;
      }

      let loops = match catch_up_to_target(
        &slot.db,
        head.head_log_index,
        config.max_frames,
        config.recovery_max_loops,
      ) {
        Ok(loops) => loops,
        Err(err) => {
          let status = replica_status(&slot.db)?;
          if status.needs_reseed || err.to_string().contains("reseed") {
            reseed_count = reseed_count.saturating_add(1);
            primary.checkpoint()?;
            slot.db.replica_reseed_from_snapshot()?;
            reseed_recovery_successes = reseed_recovery_successes.saturating_add(1);
            catch_up_to_target(
              &slot.db,
              head.head_log_index,
              config.max_frames,
              config.recovery_max_loops,
            )?
          } else {
            return Err(err);
          }
        }
      };
      max_recovery_loops_seen = max_recovery_loops_seen.max(loops);

      let status = replica_status(&slot.db)?;
      primary.primary_report_replica_progress(
        &slot.id,
        status.applied_epoch,
        status.applied_log_index,
      )?;
    }

    let _ = primary.primary_run_retention()?;

    let should_probe_reseed =
      config.reseed_check_interval > 0 && (cycle + 1) % config.reseed_check_interval == 0;
    if should_probe_reseed {
      let head = primary_status(&primary)?;
      for (idx, slot) in replicas.iter_mut().enumerate() {
        if active[idx] {
          continue;
        }

        match slot.db.replica_catch_up_once(config.max_frames) {
          Ok(_) => {}
          Err(err) => {
            let status = replica_status(&slot.db)?;
            if status.needs_reseed || err.to_string().contains("reseed") {
              reseed_count = reseed_count.saturating_add(1);
              primary.checkpoint()?;
              slot.db.replica_reseed_from_snapshot()?;
              reseed_recovery_successes = reseed_recovery_successes.saturating_add(1);
              let loops = catch_up_to_target(
                &slot.db,
                head.head_log_index,
                config.max_frames,
                config.recovery_max_loops,
              )?;
              max_recovery_loops_seen = max_recovery_loops_seen.max(loops);
              let status = replica_status(&slot.db)?;
              primary.primary_report_replica_progress(
                &slot.id,
                status.applied_epoch,
                status.applied_log_index,
              )?;
            } else {
              return Err(err);
            }
          }
        }
      }
    }

    let head = primary_status(&primary)?;
    for slot in &replicas {
      let status = replica_status(&slot.db)?;
      let lag = head.head_log_index.saturating_sub(status.applied_log_index);
      max_observed_lag = max_observed_lag.max(lag);
    }

    if config.promotion_interval > 0 && (cycle + 1) % config.promotion_interval == 0 {
      // Before epoch change, force all replicas to converge to current head.
      // This keeps promotion checks deterministic under churn and retention pressure.
      let head_before_promotion = primary_status(&primary)?;
      for slot in &mut replicas {
        let loops = match catch_up_to_target(
          &slot.db,
          head_before_promotion.head_log_index,
          config.max_frames,
          config.recovery_max_loops,
        ) {
          Ok(loops) => loops,
          Err(err) => {
            let status = replica_status(&slot.db)?;
            if status.needs_reseed || err.to_string().contains("reseed") {
              reseed_count = reseed_count.saturating_add(1);
              primary.checkpoint()?;
              slot.db.replica_reseed_from_snapshot()?;
              reseed_recovery_successes = reseed_recovery_successes.saturating_add(1);
              catch_up_to_target(
                &slot.db,
                head_before_promotion.head_log_index,
                config.max_frames,
                config.recovery_max_loops,
              )?
            } else {
              return Err(err);
            }
          }
        };
        max_recovery_loops_seen = max_recovery_loops_seen.max(loops);

        let status = replica_status(&slot.db)?;
        primary.primary_report_replica_progress(
          &slot.id,
          status.applied_epoch,
          status.applied_log_index,
        )?;
      }

      let _ = primary.primary_promote_to_next_epoch()?;
      promotion_count = promotion_count.saturating_add(1);

      // Force stale handle manifest refresh before write probe so fencing is deterministic.
      let _ = stale_probe.primary_run_retention();

      stale_probe.begin(false)?;
      stale_probe.create_node(Some(&format!("stale-probe-{cycle}")))?;
      match stale_probe.commit_with_token() {
        Ok(_) => {
          return Err(kitedb::KiteError::InvalidReplication(
            "stale writer unexpectedly committed after promotion".to_string(),
          ));
        }
        Err(err) => {
          if err.to_string().contains("stale primary") {
            stale_fence_rejections = stale_fence_rejections.saturating_add(1);
          } else {
            return Err(err);
          }
        }
      }

      let _ = stale_probe.rollback();
      close_single_file(stale_probe)?;
      stale_probe = open_primary(&primary_db_path, &primary_sidecar, &config)?;
    }

    if cycle % 3 == 0 || cycle + 1 == config.cycles {
      println!(
        "progress_cycle: {} primary_epoch: {} primary_head_log_index: {} reseeds: {} promotions: {}",
        cycle + 1,
        head.epoch,
        head.head_log_index,
        reseed_count,
        promotion_count
      );
    }
  }

  let final_head = primary_status(&primary)?;
  for slot in &mut replicas {
    let loops = match catch_up_to_target(
      &slot.db,
      final_head.head_log_index,
      config.max_frames,
      config.recovery_max_loops,
    ) {
      Ok(loops) => loops,
      Err(err) => {
        let status = replica_status(&slot.db)?;
        if status.needs_reseed || err.to_string().contains("reseed") {
          reseed_count = reseed_count.saturating_add(1);
          primary.checkpoint()?;
          slot.db.replica_reseed_from_snapshot()?;
          reseed_recovery_successes = reseed_recovery_successes.saturating_add(1);
          catch_up_to_target(
            &slot.db,
            final_head.head_log_index,
            config.max_frames,
            config.recovery_max_loops,
          )?
        } else {
          return Err(err);
        }
      }
    };
    max_recovery_loops_seen = max_recovery_loops_seen.max(loops);

    if slot.db.count_nodes() != primary.count_nodes() {
      return Err(kitedb::KiteError::InvalidReplication(format!(
        "node-count divergence on {}: replica={} primary={}",
        slot.id,
        slot.db.count_nodes(),
        primary.count_nodes()
      )));
    }

    for key in &expected_keys {
      if slot.db.node_by_key(key).is_none() {
        return Err(kitedb::KiteError::InvalidReplication(format!(
          "missing key on {}: {key}",
          slot.id
        )));
      }
    }
  }

  let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
  let final_head = primary_status(&primary)?;

  println!("writes_committed: {}", writes_committed);
  println!("promotion_count: {}", promotion_count);
  println!("stale_fence_rejections: {}", stale_fence_rejections);
  println!("reseed_count: {}", reseed_count);
  println!("reseed_recovery_successes: {}", reseed_recovery_successes);
  println!("max_recovery_loops: {}", max_recovery_loops_seen);
  println!("max_observed_lag: {}", max_observed_lag);
  println!("divergence_violations: {}", divergence_violations);
  println!("final_primary_epoch: {}", final_head.epoch);
  println!(
    "final_primary_head_log_index: {}",
    final_head.head_log_index
  );
  println!("final_primary_nodes: {}", primary.count_nodes());
  println!("elapsed_ms: {:.3}", elapsed_ms);

  for slot in replicas {
    close_single_file(slot.db)?;
  }
  close_single_file(stale_probe)?;
  close_single_file(primary)?;
  Ok(())
}
