//! Final review pass of `fix/b4-checkpoint-segments` at 760effa. Included
//! from checkpoint.rs for its private steps and test hooks.
use super::*;
use crate::core::single_file::{
  close_single_file, open_single_file, SingleFileOpenOptions, SyncMode,
};
use std::sync::mpsc;
use std::sync::Arc;
use tempfile::tempdir;

/// The smallest WAL a database accepts: a 48 KiB primary region.
const SMALL_WAL: usize = 64 * 1024;

fn key(prefix: &str, index: usize) -> String {
  format!("{prefix}-{index:05}-{}", "k".repeat(200))
}

fn commit_key(db: &SingleFileDB, key: &str) -> Result<()> {
  db.begin(false)?;
  if let Err(error) = db.create_node(Some(key)) {
    let _ = db.rollback();
    return Err(error);
  }
  db.commit()
}

fn options() -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .wal_size(SMALL_WAL)
    .sync_mode(SyncMode::Normal)
    .auto_checkpoint(false)
    .mvcc(true)
}

fn held_keys() -> Vec<String> {
  (0..20)
    .map(|n| format!("held-{n}-{}", "h".repeat(1000)))
    .collect()
}

fn missing(db: &SingleFileDB, keys: &[String]) -> usize {
  keys
    .iter()
    .filter(|key| db.node_by_key(key).is_none())
    .count()
}

/// R7's state, left as a process that ends without closing leaves it (the
/// database dropped): a transaction whose records spilled while it was open
/// (a cut spilled them; the install kept that covered segment) and that
/// committed after the cut. Since decision Q3 a clean close checkpoints the
/// segments away, so `review_seg2_tests::commit_across_a_cut`, which closes,
/// no longer leaves this state, and its two R7 tests pass with the rebuild
/// at open removed; a dropped database (or a crash) still leaves it.
fn commit_across_a_cut_and_drop(path: &std::path::Path) -> Vec<String> {
  let db = Arc::new(open_single_file(path, options()).expect("open"));
  let (held_tx, held_rx) = mpsc::channel();
  let (go_tx, go_rx) = mpsc::channel::<()>();
  let holder = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || {
      db.begin(false).expect("begin");
      for key in held_keys() {
        db.create_node(Some(&key)).expect("node");
      }
      held_tx.send(()).expect("signal");
      go_rx.recv().expect("wait");
      db.commit()
    })
  };
  held_rx.recv().expect("the holder wrote");
  commit_key(&db, &key("pre", 0)).expect("commit");
  db.background_checkpoint().expect("checkpoint");
  let stats = wal_segment_test_stats(&db);
  assert!(
    stats.live > 0 && stats.covered > 0,
    "setup: the install kept no covered segment for the open transaction: {stats:?}"
  );
  go_tx.send(()).expect("release the holder");
  holder
    .join()
    .expect("the holder thread")
    .expect("the holder's commit, after the cut");
  drop(Arc::into_inner(db).expect("sole owner"));
  held_keys()
}

/// T1 (test gap). R7's fix, the rebuild of the spilled transactions at open
/// (`spilled_transactions_in_log`, cf68104), is pinned by no test at
/// 760effa: with it removed (open starts `spilled_txids` empty again), every
/// single_file test, the compat and replication tests, and the model test
/// at 24 and at 200 seeds pass. R7's own tests close the database, which now
/// checkpoints the segments away (Q3). This one drops it: it passes at
/// 760effa and fails with the rebuild removed.
#[test]
fn fresh2_r7_rebuild_is_pinned_after_a_drop() {
  let dir = tempdir().expect("tempdir");

  // The first checkpoint after the reopen.
  let path = dir.path().join("dropped-checkpoint.kitedb");
  let held = commit_across_a_cut_and_drop(&path);
  let db = open_single_file(&path, options()).expect("reopen");
  assert!(
    !db.header.read().wal_segments.is_empty(),
    "setup: the reopened file names no WAL segment"
  );
  assert_eq!(missing(&db, &held), 0, "setup: the reopen lost the commit");
  db.background_checkpoint()
    .expect("the first checkpoint after the reopen");
  let lost_live = missing(&db, &held);
  drop(db);
  let db = open_single_file(&path, options()).expect("reopen");
  let lost_reopened = missing(&db, &held);
  drop(db);
  assert!(
    lost_live == 0 && lost_reopened == 0,
    "the first checkpoint after a reopen dropped a committed transaction: {lost_live} of its \
     {} nodes gone live, {lost_reopened} after another reopen",
    held.len()
  );

  // The first spill after the reopen, then a crash.
  let path = dir.path().join("dropped-spill.kitedb");
  let held = commit_across_a_cut_and_drop(&path);
  let db = open_single_file(&path, options()).expect("reopen");
  assert_eq!(missing(&db, &held), 0, "setup: the reopen lost the commit");
  let spills = wal_segment_test_stats(&db).next_seq;
  let mut index = 0;
  while wal_segment_test_stats(&db).next_seq == spills && index < 1_000 {
    commit_key(&db, &key("after", index)).expect("commit");
    index += 1;
  }
  let copy = dir.path().join("dropped-spill-crash.kitedb");
  std::fs::copy(&path, &copy).expect("crash copy");
  drop(db);
  let crashed = open_single_file(&copy, options()).expect("open the crash copy");
  let lost = missing(&crashed, &held);
  drop(crashed);
  assert_eq!(
    lost,
    0,
    "a spill after a reopen dropped the segment holding a committed transaction's first \
     records: a crash lost {lost} of its {} nodes",
    held.len()
  );
  let _ = close_single_file;
}

/// T2 (test gap). F3's second half, "a full table still cuts" (the cut
/// covers the segments without spilling the WAL when the spill would need
/// an entry there is none of; 4242e69), is pinned by no test at 760effa:
/// with it reverted (such a cut declines, as before), every checkpoint test,
/// both F3 tests and the model test at 200 seeds pass. F3's changed setup
/// ends with the newest segment unsealed by `release_cut`, so the cut there
/// spills into it and never needs the new path. Here the newest segment
/// stays sealed: the runs fail in their install (after one header slot is
/// durable), which keeps the seal on purpose, so each run's cut spills into
/// a new entry until the table is full. Once the failures stop, background
/// checkpoints must go on. Passes at 760effa; fails with the second half
/// reverted.
#[test]
fn fresh2_a_full_table_whose_newest_segment_is_sealed_still_cuts() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("full-sealed.kitedb");
  let options = SingleFileOpenOptions::new()
    .wal_size(SMALL_WAL)
    .sync_mode(SyncMode::Normal)
    .auto_checkpoint(false);
  let db = open_single_file(&path, options.clone()).expect("open");
  set_wal_segment_test_limit(&db, u64::MAX / 4);
  set_checkpoint_test_db_fault(&db, CheckpointPhase::HeaderDurable, true);
  let mut acked = Vec::new();
  let mut index = 0;
  while wal_segment_test_stats(&db).live < crate::constants::MAX_WAL_SEGMENTS && index < 200 {
    let key = key("pre", index);
    commit_key(&db, &key).expect("commit");
    acked.push(key);
    assert!(
      db.background_checkpoint().is_err(),
      "the fault did not fire"
    );
    index += 1;
  }
  clear_checkpoint_test_db_faults(&db);
  let newest_sealed = db
    .header
    .read()
    .wal_segments
    .entries
    .last()
    .is_some_and(|last| last.sealed);
  assert_eq!(
    wal_segment_test_stats(&db).live,
    crate::constants::MAX_WAL_SEGMENTS,
    "setup: the failed installs did not fill the table"
  );
  assert!(newest_sealed, "setup: the newest segment is not sealed");

  // The failures are over. Commits (the WAL holds records), then a
  // background checkpoint: it must cover the segments.
  for more in 0..10 {
    let key = key("after", more);
    commit_key(&db, &key).expect("commit into the WAL");
    acked.push(key);
  }
  let run = db.background_checkpoint();
  let stats = wal_segment_test_stats(&db);
  assert!(
    run.is_ok() && stats.live < crate::constants::MAX_WAL_SEGMENTS,
    "with the failures over, a background checkpoint could not cut a full table whose newest \
     segment is sealed: {run:?}, {stats:?}"
  );
  for round in 0..10 {
    for write in 0..50 {
      let key = key(&format!("post{round}"), write);
      commit_key(&db, &key).expect("commit after the checkpoint");
      acked.push(key);
    }
    db.background_checkpoint().expect("background checkpoint");
  }
  close_single_file(db).expect("close");
  let reopened = open_single_file(&path, options).expect("reopen");
  assert_eq!(missing(&reopened, &acked), 0, "acknowledged commits lost");
}
