use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use std::{env, process::Command};

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileOpenOptions, SyncMode,
};
use kitedb::replication::types::ReplicationRole;

fn open_primary(
  path: &std::path::Path,
  sidecar: &std::path::Path,
  segment_max_bytes: u64,
  retention_min_entries: u64,
) -> kitedb::Result<kitedb::core::single_file::SingleFileDB> {
  open_primary_with_sync(
    path,
    sidecar,
    segment_max_bytes,
    retention_min_entries,
    SyncMode::Full,
  )
}

fn open_primary_with_sync(
  path: &std::path::Path,
  sidecar: &std::path::Path,
  segment_max_bytes: u64,
  retention_min_entries: u64,
  sync_mode: SyncMode,
) -> kitedb::Result<kitedb::core::single_file::SingleFileDB> {
  open_single_file(
    path,
    SingleFileOpenOptions::new()
      .sync_mode(sync_mode)
      .replication_role(ReplicationRole::Primary)
      .replication_sidecar_path(sidecar)
      .replication_segment_max_bytes(segment_max_bytes)
      .replication_retention_min_entries(retention_min_entries),
  )
}

fn open_primary_multi_node_simulation(
  path: &std::path::Path,
  sidecar: &std::path::Path,
  segment_max_bytes: u64,
  retention_min_entries: u64,
  sync_mode: SyncMode,
) -> kitedb::Result<kitedb::core::single_file::SingleFileDB> {
  open_single_file(
    path,
    SingleFileOpenOptions::new()
      .sync_mode(sync_mode)
      .replication_role(ReplicationRole::Primary)
      .replication_sidecar_path(sidecar)
      .replication_segment_max_bytes(segment_max_bytes)
      .replication_retention_min_entries(retention_min_entries)
      .danger_bypass_file_lock_for_multi_node_simulation(true),
  )
}

fn open_replica(
  replica_path: &std::path::Path,
  source_db_path: &std::path::Path,
  local_sidecar: &std::path::Path,
  source_sidecar: &std::path::Path,
) -> kitedb::Result<kitedb::core::single_file::SingleFileDB> {
  open_single_file(
    replica_path,
    SingleFileOpenOptions::new()
      .replication_role(ReplicationRole::Replica)
      .replication_sidecar_path(local_sidecar)
      .replication_source_db_path(source_db_path)
      .replication_source_sidecar_path(source_sidecar),
  )
}

const PRIMARY_LOCK_CHILD_ENV: &str = "RAYDB_PRIMARY_LOCK_CHILD";
const PRIMARY_LOCK_CHILD_DB_PATH_ENV: &str = "RAYDB_PRIMARY_LOCK_CHILD_DB_PATH";
const PRIMARY_LOCK_CHILD_SIDECAR_PATH_ENV: &str = "RAYDB_PRIMARY_LOCK_CHILD_SIDECAR_PATH";

#[test]
fn primary_lock_probe_child_process_helper() {
  if env::var_os(PRIMARY_LOCK_CHILD_ENV).is_none() {
    return;
  }

  let db_path =
    std::path::PathBuf::from(env::var(PRIMARY_LOCK_CHILD_DB_PATH_ENV).expect("child db path env"));
  let sidecar_path = std::path::PathBuf::from(
    env::var(PRIMARY_LOCK_CHILD_SIDECAR_PATH_ENV).expect("child sidecar path env"),
  );

  let exit_code = match open_primary(&db_path, &sidecar_path, 256, 8) {
    Ok(primary) => {
      let _ = close_single_file(primary);
      1
    }
    Err(_) => 0,
  };
  std::process::exit(exit_code);
}

#[test]
fn promotion_increments_epoch_and_fences_stale_primary_writes() {
  let dir = tempfile::tempdir().expect("tempdir");
  let db_path = dir.path().join("phase-d-promote.kitedb");
  let sidecar = dir.path().join("phase-d-promote.sidecar");

  // Simulate independent machines sharing storage where advisory locks are ineffective.
  let primary_a = open_primary_multi_node_simulation(&db_path, &sidecar, 256, 4, SyncMode::Full)
    .expect("open primary a");
  let primary_b = open_primary_multi_node_simulation(&db_path, &sidecar, 256, 4, SyncMode::Full)
    .expect("open primary b");

  primary_a.begin(false).expect("begin a");
  primary_a.create_node(Some("a0")).expect("create a0");
  let t0 = primary_a
    .commit_with_token()
    .expect("commit a0")
    .expect("token a0");
  assert_eq!(t0.epoch, 1);

  let new_epoch = primary_b.primary_promote_to_next_epoch().expect("promote");
  assert_eq!(new_epoch, 2);

  primary_b.begin(false).expect("begin b");
  primary_b.create_node(Some("b0")).expect("create b0");
  let t1 = primary_b
    .commit_with_token()
    .expect("commit b0")
    .expect("token b0");
  assert_eq!(t1.epoch, 2);

  primary_a.begin(false).expect("begin stale");
  primary_a.create_node(Some("stale")).expect("create stale");
  let err = primary_a
    .commit_with_token()
    .expect_err("stale primary commit must fail");
  assert!(
    err.to_string().contains("stale primary"),
    "unexpected stale commit error: {err}"
  );

  close_single_file(primary_b).expect("close b");
  close_single_file(primary_a).expect("close a");
}

#[test]
fn promotion_fences_stale_primary_writes_in_normal_sync_mode() {
  let dir = tempfile::tempdir().expect("tempdir");
  let db_path = dir.path().join("phase-d-promote-normal-sync.kitedb");
  let sidecar = dir.path().join("phase-d-promote-normal-sync.sidecar");

  // Simulate independent machines sharing storage where advisory locks are ineffective.
  let primary_a = open_primary_multi_node_simulation(&db_path, &sidecar, 256, 4, SyncMode::Normal)
    .expect("open primary a");
  let primary_b = open_primary_multi_node_simulation(&db_path, &sidecar, 256, 4, SyncMode::Normal)
    .expect("open primary b");

  primary_a.begin(false).expect("begin a");
  primary_a.create_node(Some("a0")).expect("create a0");
  let t0 = primary_a
    .commit_with_token()
    .expect("commit a0")
    .expect("token a0");
  assert_eq!(t0.epoch, 1);

  let new_epoch = primary_b.primary_promote_to_next_epoch().expect("promote");
  assert_eq!(new_epoch, 2);

  primary_a.begin(false).expect("begin stale");
  primary_a.create_node(Some("stale")).expect("create stale");
  let err = primary_a
    .commit_with_token()
    .expect_err("stale primary commit must fail immediately in normal sync mode");
  assert!(
    err.to_string().contains("stale primary"),
    "unexpected stale commit error: {err}"
  );

  close_single_file(primary_b).expect("close b");
  close_single_file(primary_a).expect("close a");
}

#[test]
fn primary_open_rejects_sidecar_when_other_process_holds_primary_lock() {
  let dir = tempfile::tempdir().expect("tempdir");
  let db_path = dir.path().join("phase-d-process-lock.kitedb");
  let sidecar = dir.path().join("phase-d-process-lock.sidecar");
  let primary = open_primary(&db_path, &sidecar, 256, 8).expect("open parent primary");

  let status = Command::new(std::env::current_exe().expect("current test binary"))
    .arg("--test-threads=1")
    .arg("--exact")
    .arg("primary_lock_probe_child_process_helper")
    .arg("--nocapture")
    .env(PRIMARY_LOCK_CHILD_ENV, "1")
    .env(PRIMARY_LOCK_CHILD_DB_PATH_ENV, db_path.as_os_str())
    .env(PRIMARY_LOCK_CHILD_SIDECAR_PATH_ENV, sidecar.as_os_str())
    .status()
    .expect("spawn child probe");

  assert!(
    status.success(),
    "child process unexpectedly opened primary with same sidecar lock"
  );
  close_single_file(primary).expect("close parent primary");
}

#[test]
fn retention_respects_active_replica_cursor_and_minimum_window() {
  let dir = tempfile::tempdir().expect("tempdir");
  let db_path = dir.path().join("phase-d-retention.kitedb");
  let sidecar = dir.path().join("phase-d-retention.sidecar");

  let primary = open_primary(&db_path, &sidecar, 1, 2).expect("open primary");

  for i in 0..6 {
    primary.begin(false).expect("begin");
    primary
      .create_node(Some(&format!("n-{i}")))
      .expect("create");
    let _ = primary.commit_with_token().expect("commit").expect("token");
  }

  primary
    .primary_report_replica_progress("replica-a", 1, 2)
    .expect("report cursor");

  let prune = primary.primary_run_retention().expect("run retention");
  assert!(prune.pruned_segments > 0);

  let status = primary.primary_replication_status().expect("status");
  assert_eq!(status.retained_floor, 3);
  assert!(status
    .replica_lags
    .iter()
    .any(|lag| lag.replica_id == "replica-a" && lag.applied_log_index == 2));

  close_single_file(primary).expect("close primary");
}

#[test]
fn retention_uses_replica_progress_without_manual_report_calls() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("phase-d-auto-progress-primary.kitedb");
  let primary_sidecar = dir.path().join("phase-d-auto-progress-primary.sidecar");
  let replica_path = dir.path().join("phase-d-auto-progress-replica.kitedb");
  let replica_sidecar = dir.path().join("phase-d-auto-progress-replica.sidecar");

  let primary = open_primary(&primary_path, &primary_sidecar, 1, 1).expect("open primary");
  primary.begin(false).expect("begin base");
  primary.create_node(Some("base")).expect("create base");
  primary
    .commit_with_token()
    .expect("commit base")
    .expect("token base");

  let replica = open_replica(
    &replica_path,
    &primary_path,
    &replica_sidecar,
    &primary_sidecar,
  )
  .expect("open replica");
  replica
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap snapshot");

  for i in 0..3 {
    primary.begin(false).expect("begin warmup");
    primary
      .create_node(Some(&format!("warmup-{i}")))
      .expect("create warmup");
    primary.commit_with_token().expect("commit warmup");
  }

  let warmup_pulled = replica.replica_catch_up_once(64).expect("warmup catch-up");
  assert!(warmup_pulled > 0, "replica should apply warmup frames");

  for i in 0..4 {
    primary.begin(false).expect("begin backlog");
    primary
      .create_node(Some(&format!("backlog-{i}")))
      .expect("create backlog");
    primary.commit_with_token().expect("commit backlog");
  }

  let prune = primary.primary_run_retention().expect("run retention");
  assert!(prune.pruned_segments > 0, "test needs actual pruning");

  let backlog_pulled = replica
    .replica_catch_up_once(64)
    .expect("replica should catch up without reseed after retention");
  assert!(backlog_pulled > 0, "replica should pull backlog frames");
  assert_eq!(replica.count_nodes(), primary.count_nodes());
  assert!(
    !replica
      .replica_replication_status()
      .expect("replica status")
      .needs_reseed,
    "auto progress should prevent retention-induced reseed"
  );

  close_single_file(replica).expect("close replica");
  close_single_file(primary).expect("close primary");
}

#[test]
fn missing_segment_marks_replica_needs_reseed() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("phase-d-missing-primary.kitedb");
  let primary_sidecar = dir.path().join("phase-d-missing-primary.sidecar");
  let replica_path = dir.path().join("phase-d-missing-replica.kitedb");
  let replica_sidecar = dir.path().join("phase-d-missing-replica.sidecar");

  let primary = open_primary(&primary_path, &primary_sidecar, 1, 2).expect("open primary");

  primary.begin(false).expect("begin base");
  primary.create_node(Some("base")).expect("create base");
  primary
    .commit_with_token()
    .expect("commit base")
    .expect("token base");

  let replica = open_replica(
    &replica_path,
    &primary_path,
    &replica_sidecar,
    &primary_sidecar,
  )
  .expect("open replica");
  replica
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap snapshot");

  for i in 0..4 {
    primary.begin(false).expect("begin");
    primary
      .create_node(Some(&format!("m-{i}")))
      .expect("create");
    primary.commit_with_token().expect("commit").expect("token");
  }

  let progress_path = primary_sidecar.join("replica-progress.json");
  if progress_path.exists() {
    std::fs::remove_file(&progress_path).expect("remove persisted replica progress");
  }
  let _ = primary.primary_run_retention().expect("run retention");

  let err = replica
    .replica_catch_up_once(32)
    .expect_err("replica should require reseed");
  assert!(err.to_string().contains("reseed"));

  let status = replica
    .replica_replication_status()
    .expect("replica status");
  assert!(status.needs_reseed);

  close_single_file(replica).expect("close replica");
  close_single_file(primary).expect("close primary");
}

#[test]
fn lagging_replica_reseed_recovers_after_retention_gap() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("phase-d-reseed-primary.kitedb");
  let primary_sidecar = dir.path().join("phase-d-reseed-primary.sidecar");
  let replica_path = dir.path().join("phase-d-reseed-replica.kitedb");
  let replica_sidecar = dir.path().join("phase-d-reseed-replica.sidecar");

  let primary = open_primary(&primary_path, &primary_sidecar, 1, 2).expect("open primary");

  primary.begin(false).expect("begin base");
  primary.create_node(Some("base")).expect("create base");
  primary
    .commit_with_token()
    .expect("commit base")
    .expect("token base");

  let replica = open_replica(
    &replica_path,
    &primary_path,
    &replica_sidecar,
    &primary_sidecar,
  )
  .expect("open replica");
  replica
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap snapshot");

  for i in 0..5 {
    primary.begin(false).expect("begin");
    primary
      .create_node(Some(&format!("r-{i}")))
      .expect("create");
    primary.commit_with_token().expect("commit").expect("token");
  }

  let progress_path = primary_sidecar.join("replica-progress.json");
  if progress_path.exists() {
    std::fs::remove_file(&progress_path).expect("remove persisted replica progress");
  }
  let _ = primary.primary_run_retention().expect("run retention");

  let _ = replica
    .replica_catch_up_once(32)
    .expect_err("must need reseed");
  assert!(
    replica
      .replica_replication_status()
      .expect("status")
      .needs_reseed
  );

  replica.replica_reseed_from_snapshot().expect("reseed");
  assert!(
    !replica
      .replica_replication_status()
      .expect("status post reseed")
      .needs_reseed
  );
  assert_eq!(replica.count_nodes(), primary.count_nodes());

  close_single_file(replica).expect("close replica");
  close_single_file(primary).expect("close primary");
}

#[test]
fn lagging_replica_across_epoch_retention_gap_requires_reseed() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("phase-d-epoch-gap-primary.kitedb");
  let primary_sidecar = dir.path().join("phase-d-epoch-gap-primary.sidecar");
  let replica_path = dir.path().join("phase-d-epoch-gap-replica.kitedb");
  let replica_sidecar = dir.path().join("phase-d-epoch-gap-replica.sidecar");

  let primary = open_primary(&primary_path, &primary_sidecar, 1, 8).expect("open primary");

  primary.begin(false).expect("begin base");
  primary.create_node(Some("base")).expect("create base");
  primary
    .commit_with_token()
    .expect("commit base")
    .expect("token base");

  let replica = open_replica(
    &replica_path,
    &primary_path,
    &replica_sidecar,
    &primary_sidecar,
  )
  .expect("open replica");
  replica
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap snapshot");

  for i in 0..24 {
    primary.begin(false).expect("begin pre-promotion");
    primary
      .create_node(Some(&format!("pre-{i}")))
      .expect("create pre");
    primary
      .commit_with_token()
      .expect("commit pre")
      .expect("token pre");
  }

  let new_epoch = primary
    .primary_promote_to_next_epoch()
    .expect("promote to next epoch");
  assert_eq!(new_epoch, 2);

  for i in 0..24 {
    primary.begin(false).expect("begin post-promotion");
    primary
      .create_node(Some(&format!("post-{i}")))
      .expect("create post");
    primary
      .commit_with_token()
      .expect("commit post")
      .expect("token post");
  }

  let prune = primary.primary_run_retention().expect("run retention");
  assert!(
    prune.pruned_segments > 0,
    "test setup requires actual segment pruning"
  );

  let target_head = primary
    .primary_replication_status()
    .expect("primary replication status")
    .head_log_index;

  let mut needs_reseed = false;
  for _ in 0..16 {
    match replica.replica_catch_up_once(64) {
      Ok(_) => {
        let status = replica
          .replica_replication_status()
          .expect("replica status after catch-up");
        if status.needs_reseed {
          needs_reseed = true;
          break;
        }
        if status.applied_log_index >= target_head {
          break;
        }
      }
      Err(err) => {
        let status = replica
          .replica_replication_status()
          .expect("replica status after catch-up error");
        if status.needs_reseed || err.to_string().contains("reseed") {
          needs_reseed = true;
          break;
        }
        panic!("unexpected catch-up error before reseed: {err}");
      }
    }
  }

  let status = replica
    .replica_replication_status()
    .expect("final replica status");
  assert!(
    needs_reseed || status.needs_reseed,
    "lagging replica across epoch retention gap must require reseed; status={status:?}, replica_nodes={}, primary_nodes={}",
    replica.count_nodes(),
    primary.count_nodes()
  );

  close_single_file(replica).expect("close replica");
  close_single_file(primary).expect("close primary");
}

#[test]
fn transient_missing_segments_do_not_immediately_require_reseed() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("phase-d-transient-gap-primary.kitedb");
  let primary_sidecar = dir.path().join("phase-d-transient-gap-primary.sidecar");
  let replica_path = dir.path().join("phase-d-transient-gap-replica.kitedb");
  let replica_sidecar = dir.path().join("phase-d-transient-gap-replica.sidecar");

  let primary =
    open_primary(&primary_path, &primary_sidecar, 1024 * 1024, 8).expect("open primary");
  primary.begin(false).expect("begin base");
  primary.create_node(Some("base")).expect("create base");
  primary
    .commit_with_token()
    .expect("commit base")
    .expect("token base");

  let replica = open_replica(
    &replica_path,
    &primary_path,
    &replica_sidecar,
    &primary_sidecar,
  )
  .expect("open replica");
  replica
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap snapshot");

  primary.begin(false).expect("begin c1");
  primary.create_node(Some("c1")).expect("create c1");
  primary
    .commit_with_token()
    .expect("commit c1")
    .expect("token c1");

  let mut hidden_segments = Vec::new();
  for entry in std::fs::read_dir(&primary_sidecar).expect("read primary sidecar") {
    let path = entry.expect("read sidecar entry").path();
    let is_segment = path
      .file_name()
      .and_then(|name| name.to_str())
      .is_some_and(|name| name.starts_with("segment-") && name.ends_with(".rlog"));
    if !is_segment {
      continue;
    }
    let hidden = path.with_extension("rlog.hidden");
    std::fs::rename(&path, &hidden).expect("temporarily hide segment");
    hidden_segments.push((path, hidden));
  }
  assert!(
    !hidden_segments.is_empty(),
    "test setup failed: no segment files discovered"
  );

  let err = replica
    .replica_catch_up_once(64)
    .expect_err("transient segment unavailability must fail catch-up attempt");
  assert!(
    !replica
      .replica_replication_status()
      .expect("replica status after transient segment miss")
      .needs_reseed,
    "transient segment unavailability must not force immediate reseed: {err}"
  );

  for (segment, hidden) in hidden_segments {
    std::fs::rename(&hidden, &segment).expect("restore hidden segment");
  }

  let applied = replica
    .replica_catch_up_once(64)
    .expect("replica should recover after transient segment availability");
  assert!(applied > 0, "replica should apply pending frames");
  assert!(
    !replica
      .replica_replication_status()
      .expect("replica status after recovery")
      .needs_reseed,
    "successful recovery should keep reseed flag cleared"
  );

  close_single_file(replica).expect("close replica");
  close_single_file(primary).expect("close primary");
}

#[test]
fn bootstrap_handles_concurrent_primary_writes_safely() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("phase-d-bootstrap-race-primary.kitedb");
  let primary_sidecar = dir.path().join("phase-d-bootstrap-race-primary.sidecar");
  let replica_path = dir.path().join("phase-d-bootstrap-race-replica.kitedb");
  let replica_sidecar = dir.path().join("phase-d-bootstrap-race-replica.sidecar");

  let primary =
    Arc::new(open_primary(&primary_path, &primary_sidecar, 1024 * 1024, 8).expect("open primary"));

  primary.begin(false).expect("begin seed");
  for i in 0..5_000 {
    primary
      .create_node(Some(&format!("seed-{i}")))
      .expect("create seed");
  }
  primary.commit_with_token().expect("commit seed");

  let replica = open_replica(
    &replica_path,
    &primary_path,
    &replica_sidecar,
    &primary_sidecar,
  )
  .expect("open replica");

  let stop = Arc::new(AtomicBool::new(false));
  let wrote = Arc::new(AtomicUsize::new(0));

  let writer_primary = Arc::clone(&primary);
  let writer_stop = Arc::clone(&stop);
  let writer_wrote = Arc::clone(&wrote);
  let writer = std::thread::spawn(move || {
    let mut i = 0usize;
    let mut local_commits = 0usize;
    while !writer_stop.load(Ordering::Relaxed) {
      if writer_primary.begin(false).is_ok() {
        let _ = writer_primary.create_node(Some(&format!("race-{i}")));
        match writer_primary.commit_with_token() {
          Ok(_) => {
            writer_wrote.fetch_add(1, Ordering::Relaxed);
            local_commits = local_commits.saturating_add(1);
            if local_commits.is_multiple_of(64) {
              let _ = writer_primary.checkpoint();
            }
          }
          Err(_) => {
            let _ = writer_primary.rollback();
            let _ = writer_primary.checkpoint();
          }
        }
      }
      i = i.saturating_add(1);
    }
  });

  let bootstrap = replica.replica_bootstrap_from_snapshot();
  stop.store(true, Ordering::Relaxed);
  writer.join().expect("join writer");

  let wrote_commits = wrote.load(Ordering::Relaxed);
  assert!(
    wrote_commits > 0,
    "test setup failed: expected concurrent primary commits during bootstrap"
  );

  match bootstrap {
    Ok(()) => {
      primary
        .checkpoint()
        .expect("checkpoint primary after contention");
      let target_head = primary
        .primary_replication_status()
        .expect("primary status after contention")
        .head_log_index;
      let mut stalled = 0usize;
      for _ in 0..128 {
        let status = replica
          .replica_replication_status()
          .expect("replica status during contention catch-up");
        if status.applied_log_index >= target_head {
          break;
        }
        let applied = replica
          .replica_catch_up_once(256)
          .expect("catch-up after bootstrap under contention");
        if applied == 0 {
          stalled = stalled.saturating_add(1);
          if stalled >= 20 {
            break;
          }
          std::thread::sleep(Duration::from_millis(5));
        } else {
          stalled = 0;
        }
      }
      let final_status = replica
        .replica_replication_status()
        .expect("final replica status after contention catch-up");
      assert!(
        final_status.applied_log_index >= target_head,
        "replica did not catch up after bootstrap under contention: applied={}, target={}",
        final_status.applied_log_index,
        target_head
      );
    }
    Err(err) => {
      let message = err.to_string();
      assert!(
        message.contains("quiesce")
          || message.contains("WAL buffer full")
          || message.contains("WalBufferFull"),
        "unexpected bootstrap error: {err}"
      );
    }
  }

  close_single_file(replica).expect("close replica");
  let primary = Arc::into_inner(primary).expect("primary unique");
  close_single_file(primary).expect("close primary");
}

#[test]
fn bootstrap_retries_until_source_quiesces() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("phase-d-bootstrap-retry-primary.kitedb");
  let primary_sidecar = dir.path().join("phase-d-bootstrap-retry-primary.sidecar");
  let replica_path = dir.path().join("phase-d-bootstrap-retry-replica.kitedb");
  let replica_sidecar = dir.path().join("phase-d-bootstrap-retry-replica.sidecar");

  let primary =
    Arc::new(open_primary(&primary_path, &primary_sidecar, 1024 * 1024, 8).expect("open primary"));

  primary.begin(false).expect("begin seed");
  for i in 0..5_000 {
    primary
      .create_node(Some(&format!("seed-{i}")))
      .expect("create seed");
  }
  primary.commit_with_token().expect("commit seed");

  let replica = open_replica(
    &replica_path,
    &primary_path,
    &replica_sidecar,
    &primary_sidecar,
  )
  .expect("open replica");

  let writer_primary = Arc::clone(&primary);
  let writer = std::thread::spawn(move || {
    for i in 0..80 {
      if writer_primary.begin(false).is_ok() {
        let _ = writer_primary.create_node(Some(&format!("retry-race-{i}")));
        let _ = writer_primary.commit_with_token();
      }
      std::thread::sleep(Duration::from_millis(2));
    }
  });

  let bootstrap = replica.replica_bootstrap_from_snapshot();
  writer.join().expect("join writer");

  bootstrap.expect(
    "bootstrap should retry while writes are active and eventually succeed once source quiesces",
  );
  primary
    .checkpoint()
    .expect("checkpoint primary after writer");
  let target_head = primary
    .primary_replication_status()
    .expect("primary status after writer")
    .head_log_index;

  let mut stalled = 0usize;
  for _ in 0..128 {
    let status = replica
      .replica_replication_status()
      .expect("replica status during catch-up");
    if status.applied_log_index >= target_head {
      break;
    }
    let applied = replica
      .replica_catch_up_once(256)
      .expect("catch-up after bootstrap");
    if applied == 0 {
      stalled = stalled.saturating_add(1);
      if stalled >= 20 {
        break;
      }
      std::thread::sleep(Duration::from_millis(5));
    } else {
      stalled = 0;
    }
  }
  let final_status = replica
    .replica_replication_status()
    .expect("final replica status after catch-up");
  assert!(
    final_status.applied_log_index >= target_head,
    "replica did not catch up after bootstrap retry: applied={}, target={}",
    final_status.applied_log_index,
    target_head
  );

  close_single_file(replica).expect("close replica");
  let primary = Arc::into_inner(primary).expect("primary unique");
  close_single_file(primary).expect("close primary");
}

#[test]
fn promotion_race_rejects_split_brain_writes() {
  let dir = tempfile::tempdir().expect("tempdir");
  let db_path = dir.path().join("phase-d-race.kitedb");
  let sidecar = dir.path().join("phase-d-race.sidecar");

  // Simulate independent machines sharing storage where advisory locks are ineffective.
  let left = Arc::new(
    open_primary_multi_node_simulation(&db_path, &sidecar, 128, 8, SyncMode::Full)
      .expect("open left"),
  );
  let right = Arc::new(
    open_primary_multi_node_simulation(&db_path, &sidecar, 128, 8, SyncMode::Full)
      .expect("open right"),
  );

  let l = Arc::clone(&left);
  let h1 = std::thread::spawn(move || {
    let promote = l.primary_promote_to_next_epoch();
    l.begin(false).expect("left begin");
    l.create_node(Some("left")).expect("left create");
    let commit = l.commit_with_token();
    (promote, commit)
  });

  let r = Arc::clone(&right);
  let h2 = std::thread::spawn(move || {
    let promote = r.primary_promote_to_next_epoch();
    r.begin(false).expect("right begin");
    r.create_node(Some("right")).expect("right create");
    let commit = r.commit_with_token();
    (promote, commit)
  });

  let (left_promote, left_result) = h1.join().expect("left join");
  let (right_promote, right_result) = h2.join().expect("right join");
  assert!(left_promote.is_ok());
  assert!(right_promote.is_ok());

  let left_ok = left_result.as_ref().is_ok_and(|token| token.is_some());
  let right_ok = right_result.as_ref().is_ok_and(|token| token.is_some());
  assert!(
    left_ok ^ right_ok,
    "exactly one writer should succeed after race"
  );

  let left = Arc::into_inner(left).expect("left unique");
  let right = Arc::into_inner(right).expect("right unique");
  close_single_file(left).expect("close left");
  close_single_file(right).expect("close right");
}

#[test]
fn retention_time_window_keeps_recent_segments() {
  let dir = tempfile::tempdir().expect("tempdir");
  let db_path = dir.path().join("phase-d-retention-window.kitedb");
  let sidecar = dir.path().join("phase-d-retention-window.sidecar");

  let primary = open_single_file(
    &db_path,
    SingleFileOpenOptions::new()
      .replication_role(ReplicationRole::Primary)
      .replication_sidecar_path(&sidecar)
      .replication_segment_max_bytes(1)
      .replication_retention_min_entries(0)
      .replication_retention_min_ms(60_000),
  )
  .expect("open primary");

  for i in 0..6 {
    primary.begin(false).expect("begin");
    primary
      .create_node(Some(&format!("w-{i}")))
      .expect("create");
    primary.commit_with_token().expect("commit").expect("token");
  }

  let segments_before = std::fs::read_dir(&sidecar)
    .expect("list sidecar")
    .filter_map(|entry| entry.ok())
    .filter(|entry| entry.file_name().to_string_lossy().starts_with("segment-"))
    .count();
  assert!(
    segments_before > 1,
    "expected multiple segments for retention"
  );

  let prune = primary.primary_run_retention().expect("run retention");
  assert_eq!(prune.pruned_segments, 0);

  // Ensure no filesystem-timestamp race with segment creation.
  std::thread::sleep(Duration::from_millis(5));

  let segments_after = std::fs::read_dir(&sidecar)
    .expect("list sidecar after retention")
    .filter_map(|entry| entry.ok())
    .filter(|entry| entry.file_name().to_string_lossy().starts_with("segment-"))
    .count();
  assert_eq!(segments_after, segments_before);

  close_single_file(primary).expect("close primary");
}

#[test]
fn replica_open_requires_source_db_path() {
  let dir = tempfile::tempdir().expect("tempdir");
  let replica_path = dir.path().join("phase-d-misconfig-no-source.kitedb");
  let replica_sidecar = dir.path().join("phase-d-misconfig-no-source.sidecar");

  let err = open_single_file(
    &replica_path,
    SingleFileOpenOptions::new()
      .replication_role(ReplicationRole::Replica)
      .replication_sidecar_path(&replica_sidecar),
  )
  .err()
  .expect("replica open without source db path must fail");

  assert!(
    err.to_string().contains("source db path"),
    "unexpected error: {err}"
  );
}

#[test]
fn replica_open_rejects_source_sidecar_equal_local_sidecar() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("phase-d-misconfig-primary.kitedb");
  let primary_sidecar = dir.path().join("phase-d-misconfig-primary.sidecar");
  let replica_path = dir.path().join("phase-d-misconfig-replica.kitedb");

  let primary = open_primary(&primary_path, &primary_sidecar, 128, 8).expect("open primary");
  primary.begin(false).expect("begin primary");
  primary.create_node(Some("seed")).expect("create seed");
  primary.commit_with_token().expect("commit primary");

  let err = open_single_file(
    &replica_path,
    SingleFileOpenOptions::new()
      .replication_role(ReplicationRole::Replica)
      .replication_sidecar_path(&primary_sidecar)
      .replication_source_db_path(&primary_path)
      .replication_source_sidecar_path(&primary_sidecar),
  )
  .err()
  .expect("replica local/source sidecar collision must fail");

  assert!(
    err.to_string().contains("source sidecar path must differ"),
    "unexpected error: {err}"
  );

  close_single_file(primary).expect("close primary");
}

#[test]
fn primary_snapshot_transport_export_includes_metadata_and_optional_data() {
  let dir = tempfile::tempdir().expect("tempdir");
  let db_path = dir.path().join("phase-d-transport-snapshot.kitedb");
  let sidecar = dir.path().join("phase-d-transport-snapshot.sidecar");
  let primary = open_primary(&db_path, &sidecar, 128, 8).expect("open primary");

  primary.begin(false).expect("begin");
  primary.create_node(Some("snap-1")).expect("create");
  primary.commit_with_token().expect("commit");

  let without_data = primary
    .primary_export_snapshot_transport_json(false)
    .expect("snapshot transport export");
  let without_data_json: serde_json::Value =
    serde_json::from_str(&without_data).expect("parse snapshot export");
  assert_eq!(without_data_json["format"], "single-file-db-copy");
  assert_eq!(without_data_json["epoch"], 1);
  assert_eq!(without_data_json["data_base64"], serde_json::Value::Null);
  assert!(without_data_json["checksum_crc32c"]
    .as_str()
    .map(|value| !value.is_empty())
    .unwrap_or(false));

  let with_data = primary
    .primary_export_snapshot_transport_json(true)
    .expect("snapshot export with data");
  let with_data_json: serde_json::Value =
    serde_json::from_str(&with_data).expect("parse snapshot export with data");
  let encoded = with_data_json["data_base64"]
    .as_str()
    .expect("data_base64 must be present");
  let decoded = BASE64_STANDARD
    .decode(encoded)
    .expect("decode snapshot base64");
  assert_eq!(
    decoded.len() as u64,
    with_data_json["byte_length"]
      .as_u64()
      .expect("byte_length must be u64")
  );

  close_single_file(primary).expect("close primary");
}

#[test]
fn primary_log_transport_export_pages_by_cursor() {
  let dir = tempfile::tempdir().expect("tempdir");
  let db_path = dir.path().join("phase-d-transport-log.kitedb");
  let sidecar = dir.path().join("phase-d-transport-log.sidecar");
  let primary = open_primary(&db_path, &sidecar, 1, 2).expect("open primary");

  for i in 0..5 {
    primary.begin(false).expect("begin");
    primary
      .create_node(Some(&format!("transport-{i}")))
      .expect("create");
    primary.commit_with_token().expect("commit");
  }

  let first = primary
    .primary_export_log_transport_json(None, 2, 1024 * 1024, true)
    .expect("first log export");
  let first_json: serde_json::Value = serde_json::from_str(&first).expect("parse first page");
  assert_eq!(first_json["frame_count"], 2);
  assert_eq!(first_json["eof"], false);
  assert!(first_json["frames"]
    .as_array()
    .expect("frames array")
    .iter()
    .all(|frame| frame["payload_base64"].as_str().is_some()));

  let cursor = first_json["next_cursor"]
    .as_str()
    .expect("next_cursor")
    .to_string();
  let second = primary
    .primary_export_log_transport_json(Some(&cursor), 4, 1024 * 1024, false)
    .expect("second log export");
  let second_json: serde_json::Value = serde_json::from_str(&second).expect("parse second page");
  assert!(second_json["frame_count"].as_u64().unwrap_or_default() > 0);
  assert!(second_json["frames"]
    .as_array()
    .expect("frames array")
    .iter()
    .all(|frame| frame["payload_base64"].is_null()));

  close_single_file(primary).expect("close primary");
}

#[test]
fn primary_log_transport_export_rejects_crc_corruption() {
  let dir = tempfile::tempdir().expect("tempdir");
  let db_path = dir.path().join("phase-d-transport-crc-corrupt.kitedb");
  let sidecar = dir.path().join("phase-d-transport-crc-corrupt.sidecar");
  let primary = open_primary(&db_path, &sidecar, 128, 8).expect("open primary");

  primary.begin(false).expect("begin");
  primary.create_node(Some("crc-corrupt")).expect("create");
  primary.commit_with_token().expect("commit");

  let mut segments: Vec<_> = std::fs::read_dir(&sidecar)
    .expect("read sidecar")
    .filter_map(|entry| entry.ok())
    .map(|entry| entry.path())
    .filter(|path| {
      path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("segment-") && name.ends_with(".rlog"))
    })
    .collect();
  segments.sort();
  let segment_path = segments.first().expect("segment path");

  let mut bytes = std::fs::read(segment_path).expect("read segment");
  assert!(
    bytes.len() > 32,
    "test setup failed: expected segment with payload bytes"
  );
  bytes[32] ^= 0xFF;
  std::fs::write(segment_path, &bytes).expect("write corrupted segment");

  let err = primary
    .primary_export_log_transport_json(None, 128, 1024 * 1024, true)
    .expect_err("transport export should fail on corrupted frame crc");
  assert!(
    err.to_string().contains("CrcMismatch") || err.to_string().to_lowercase().contains("crc"),
    "unexpected transport corruption error: {err}"
  );

  close_single_file(primary).expect("close primary");
}

#[test]
fn primary_reopen_does_not_reuse_log_indexes_when_manifest_lags_disk() {
  let dir = tempfile::tempdir().expect("tempdir");
  let db_path = dir.path().join("phase-d-manifest-lag.kitedb");
  let sidecar = dir.path().join("phase-d-manifest-lag.sidecar");

  let primary_first = open_single_file(
    &db_path,
    SingleFileOpenOptions::new()
      .replication_role(ReplicationRole::Primary)
      .replication_sidecar_path(&sidecar)
      .replication_segment_max_bytes(1024 * 1024)
      .replication_retention_min_entries(8)
      .sync_mode(SyncMode::Normal),
  )
  .expect("open primary");
  primary_first.begin(false).expect("begin first");
  primary_first
    .create_node(Some("first"))
    .expect("create first");
  primary_first
    .commit_with_token()
    .expect("commit first")
    .expect("token first");
  close_single_file(primary_first).expect("close first primary");

  let primary_second = open_single_file(
    &db_path,
    SingleFileOpenOptions::new()
      .replication_role(ReplicationRole::Primary)
      .replication_sidecar_path(&sidecar)
      .replication_segment_max_bytes(1024 * 1024)
      .replication_retention_min_entries(8)
      .sync_mode(SyncMode::Normal),
  )
  .expect("reopen primary");
  primary_second.begin(false).expect("begin second");
  primary_second
    .create_node(Some("second"))
    .expect("create second");
  primary_second
    .commit_with_token()
    .expect("commit second")
    .expect("token second");

  let exported = primary_second
    .primary_export_log_transport_json(None, 16, 1024 * 1024, false)
    .expect("export log transport");
  let exported_json: serde_json::Value = serde_json::from_str(&exported).expect("parse json");
  let frames = exported_json["frames"].as_array().expect("frames array");
  assert!(
    frames.len() >= 2,
    "expected at least two frames after reopen test"
  );
  let first_idx = frames[0]["log_index"].as_u64().expect("first log index");
  let second_idx = frames[1]["log_index"].as_u64().expect("second log index");
  assert!(
    second_idx > first_idx,
    "log indexes must remain strictly increasing across reopen: first={first_idx} second={second_idx}"
  );

  close_single_file(primary_second).expect("close second primary");
}

#[test]
fn primary_snapshot_transport_rejects_oversized_inline_payloads() {
  let dir = tempfile::tempdir().expect("tempdir");
  let db_path = dir
    .path()
    .join("phase-d-transport-snapshot-too-large.kitedb");
  let sidecar = dir
    .path()
    .join("phase-d-transport-snapshot-too-large.sidecar");
  let primary = open_primary(&db_path, &sidecar, 128, 8).expect("open primary");

  let oversized = 33 * 1024 * 1024u64;
  let db_file = std::fs::OpenOptions::new()
    .write(true)
    .open(&db_path)
    .expect("open db file for resize");
  db_file.set_len(oversized).expect("set db file length");

  let err = primary
    .primary_export_snapshot_transport_json(true)
    .expect_err("oversized inline snapshot export must fail");
  assert!(
    err.to_string().to_lowercase().contains("snapshot")
      && err.to_string().to_lowercase().contains("size"),
    "unexpected oversized snapshot error: {err}"
  );

  close_single_file(primary).expect("close primary");
}

#[test]
fn primary_log_transport_enforces_byte_budget_even_for_first_frame() {
  let dir = tempfile::tempdir().expect("tempdir");
  let db_path = dir.path().join("phase-d-transport-log-budget.kitedb");
  let sidecar = dir.path().join("phase-d-transport-log-budget.sidecar");
  let primary = open_primary(&db_path, &sidecar, 1024 * 1024, 8).expect("open primary");

  primary.begin(false).expect("begin");
  for i in 0..300 {
    primary
      .create_node(Some(&format!("budget-{i:03}-{}", "x".repeat(40))))
      .expect("create");
  }
  primary.commit_with_token().expect("commit");

  let err = primary
    .primary_export_log_transport_json(None, 16, 1024, true)
    .expect_err("oversized frame should not bypass max_bytes budget");
  assert!(
    err.to_string().contains("max_bytes"),
    "unexpected max-bytes error: {err}"
  );

  close_single_file(primary).expect("close primary");
}
