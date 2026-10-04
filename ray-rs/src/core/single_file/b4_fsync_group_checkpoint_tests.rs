//! raydb-b4 `fsync-group` lane: crash images of Full-mode commit groups
//! written right after a background checkpoint's cut spilled the WAL into a
//! WAL segment and started it over under a fresh salt (see
//! `b4_fsync_group_tests.rs`). Included from checkpoint.rs for its phase
//! hooks.
use super::*;
use crate::core::pager::io_hooks::IoEvent;
use crate::core::single_file::recovery::b4_fsync_group_tests::{
  commit_node, mixed_keys, options, record_commits, Landing, Recorded,
};
use crate::core::single_file::{open_single_file, SyncMode};

const PAGE: u64 = 4096;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};
use tempfile::tempdir;

/// Commits before a background checkpoint's cut, then commits in the WAL
/// the cut emptied while the checkpoint is parked right after it, then
/// (once it installed) more: every crash image keeps the commits
/// acknowledged by then, and the ones recorded are a prefix of their commit
/// order. (The install's own crash images are
/// `b4_wal_perf_checkpoint_tests`'.)
#[test]
fn fg_crash_after_a_cut_keeps_acknowledged_commits() {
  let _serial = checkpoint_test_serial();
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("region-switch.kitedb");
  let db = Arc::new(open_single_file(&path, options(SyncMode::Full)).expect("open"));
  let before = mixed_keys("pre", 4);
  for key in &before {
    commit_node(&db, key).expect("commit");
  }

  let parked = Arc::new(Barrier::new(2));
  set_checkpoint_test_barrier(&db, CheckpointPhase::CutReleased, Arc::clone(&parked));
  let checkpointer = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || db.background_checkpoint())
  };
  let deadline = Instant::now() + Duration::from_secs(10);
  while checkpoint_test_cuts(&db) == 0 {
    assert!(Instant::now() < deadline, "the cut never happened");
    std::thread::yield_now();
  }
  // The cut's header is installed (durably, in both slots) under the header
  // lock.
  let after_cut = std::fs::read(&path).expect("image after the cut");
  assert!(
    wal_segment_test_stats(&db).live > 0,
    "setup: the cut spilled the WAL into a segment"
  );
  let mut recorded = Recorded::new(after_cut);
  recorded.durable_before(&before);
  record_commits(&db, &mut recorded, &mixed_keys("post", 4));

  parked.wait();
  checkpointer
    .join()
    .expect("checkpoint thread")
    .expect("background checkpoint");
  let stats = db.wal_stats();
  assert_eq!(
    (stats.active_region, stats.tail),
    (0, 0),
    "setup: the post-cut commits are in the primary region"
  );

  // The cut started the WAL over under a fresh salt, which no record on
  // disk has: commits after it need no zeros ahead.
  let mut after_install = Recorded::new(std::fs::read(&path).expect("image after the install"));
  after_install.durable_before(&before);
  record_commits(&db, &mut after_install, &mixed_keys("later", 4));
  let zeroed: usize = after_install
    .events
    .iter()
    .filter_map(|event| match event {
      IoEvent::Write { offset, data }
        if *offset >= 2 * PAGE && data.iter().all(|byte| *byte == 0) =>
      {
        Some(data.len())
      }
      _ => None,
    })
    .sum();
  assert_eq!(
    zeroed, 0,
    "commits in a WAL started over by a cut zeroed bytes"
  );
  drop(db);

  for recorded in [&recorded, &after_install] {
    recorded.check_images(
      dir.path(),
      SyncMode::Full,
      &[Landing::Whole, Landing::Lost, Landing::FirstPageLost],
      &[],
    );
  }
}
