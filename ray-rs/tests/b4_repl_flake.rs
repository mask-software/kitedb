//! raydb-b4 `repl-flake` lane: a primary closed and reopened in a process that
//! spawns child processes meanwhile.
//!
//! A child spawned while the primary is open starts with a copy of the
//! sidecar's `primary.lock` descriptor and closes it only when its exec
//! completes. A close that released the lock only by closing its own
//! descriptor left it held by the child for that window, and the reopen failed
//! with "primary sidecar lock is held by another process". It made
//! `replication_phase_d::primary_reopen_does_not_reuse_log_indexes_when_manifest_lags_disk`
//! flaky, as the test binary's lock-probe test spawns a child.
#![cfg(unix)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileOpenOptions, SyncMode,
};
use kitedb::replication::types::ReplicationRole;

const SPAWNER_THREADS: usize = 4;
const REOPENS: usize = 1000;
const TIME_BUDGET: Duration = Duration::from_secs(4);

#[test]
fn primary_reopen_survives_concurrent_child_process_spawns() {
  let dir = tempfile::tempdir().expect("tempdir");
  let db_path = dir.path().join("reopen-while-spawning.kitedb");
  let sidecar = dir.path().join("reopen-while-spawning.sidecar");
  let options = SingleFileOpenOptions::new()
    .replication_role(ReplicationRole::Primary)
    .replication_sidecar_path(&sidecar)
    .sync_mode(SyncMode::Normal);

  let stop = Arc::new(AtomicBool::new(false));
  let spawners: Vec<_> = (0..SPAWNER_THREADS)
    .map(|_| {
      let stop = Arc::clone(&stop);
      std::thread::spawn(move || {
        let mut spawned = 0usize;
        while !stop.load(Ordering::Relaxed) {
          std::process::Command::new("true")
            .status()
            .expect("spawn child process");
          spawned += 1;
        }
        spawned
      })
    })
    .collect();

  let started = Instant::now();
  let mut reopens = 0usize;
  let mut failures = Vec::new();
  let mut last_log_index = 0u64;
  while reopens < REOPENS && started.elapsed() < TIME_BUDGET {
    reopens += 1;
    match open_single_file(&db_path, options.clone()) {
      Ok(db) => {
        db.begin(false).expect("begin");
        db.create_node(Some(&format!("n{reopens}")))
          .expect("create node");
        let token = db
          .commit_with_token()
          .expect("commit")
          .expect("commit token");
        assert!(
          token.log_index > last_log_index,
          "log indexes must keep increasing across reopens: {last_log_index} then {}",
          token.log_index
        );
        last_log_index = token.log_index;
        close_single_file(db).expect("close primary");
      }
      Err(error) => failures.push(format!("reopen {reopens}: {error}")),
    }
  }
  stop.store(true, Ordering::Relaxed);
  let spawned: usize = spawners
    .into_iter()
    .map(|spawner| spawner.join().expect("join spawner"))
    .sum();

  assert!(
    spawned > 0,
    "no child process was spawned during the reopens"
  );
  assert!(
    failures.is_empty(),
    "{} of {reopens} reopens failed while {spawned} child processes were spawned; first: {}",
    failures.len(),
    failures[0]
  );
}
