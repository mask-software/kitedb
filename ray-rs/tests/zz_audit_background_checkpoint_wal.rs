//! Auto-checkpoints must keep the log (the WAL and its WAL segments) from
//! growing without bound under a steady stream of small commits, in both
//! blocking and background mode.
//!
//! Regression: after the first background checkpoint, the primary WAL region
//! head was never reset, so `usage_ratio()` stayed above the threshold. Every
//! later commit then started another full checkpoint, and with a small WAL the
//! writes eventually failed with "WAL buffer full".

use kitedb::core::single_file::{close_single_file, open_single_file, SingleFileOpenOptions};
use std::time::{Duration, Instant};

const WAL_SIZE: usize = 64 * 1024;
/// About 3 MiB of log: many times the checkpoint trigger.
const COMMITS: usize = 10_000;

fn options(background: bool) -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .wal_size(WAL_SIZE)
    .auto_checkpoint(true)
    .background_checkpoint(background)
}

fn commit_many_small_transactions(background: bool) {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("auto-checkpoint.kitedb");
  let db = open_single_file(&path, options(background)).expect("open");

  for index in 0..COMMITS {
    db.begin(false).expect("begin");
    db.create_node(Some(&format!("n:{index}")))
      .unwrap_or_else(|error| panic!("create_node #{index} failed: {error}"));
    db.commit()
      .unwrap_or_else(|error| panic!("commit #{index} failed: {error}"));
  }

  // Once the checkpoint in flight (if any) installs, the log must be back
  // below the checkpoint trigger: checkpoints reclaim it instead of
  // re-triggering forever.
  let deadline = Instant::now() + Duration::from_secs(20);
  while db.should_checkpoint(1.0) || db.is_checkpoint_running() {
    assert!(
      Instant::now() < deadline,
      "the log stayed at the checkpoint trigger after {COMMITS} commits: {:?}",
      db.wal_stats()
    );
    std::thread::sleep(Duration::from_millis(5));
  }

  close_single_file(db).expect("close");
  let reopened = open_single_file(&path, options(background)).expect("reopen");
  assert!(reopened.node_by_key("n:0").is_some());
  assert!(reopened
    .node_by_key(&format!("n:{}", COMMITS - 1))
    .is_some());
  close_single_file(reopened).expect("close reopened");
}

#[test]
fn blocking_auto_checkpoints_keep_up_with_small_commits() {
  commit_many_small_transactions(false);
}

#[test]
fn background_auto_checkpoints_keep_up_with_small_commits() {
  commit_many_small_transactions(true);
}
