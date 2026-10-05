//! Third review pass of `fix/b4-checkpoint-segments` (760effa): tests for
//! gaps the review found. Included from checkpoint.rs for its private steps
//! and test hooks.
use super::*;
use crate::core::single_file::{
  close_single_file, open_single_file, SingleFileOpenOptions, SyncMode,
};
use std::sync::{Arc, Barrier};
use tempfile::tempdir;

/// The smallest WAL a database accepts: a 48 KiB primary region.
const SMALL_WAL: usize = 64 * 1024;

/// A key of about `len` bytes that does not compress: a snapshot holding
/// many of them takes about as many bytes as they do.
fn incompressible_key(prefix: &str, index: usize, len: usize) -> String {
  let mut state = (index as u64)
    .wrapping_mul(6364136223846793005)
    .wrapping_add(1442695040888963407);
  let mut key = format!("{prefix}-{index:06}-");
  while key.len() < len {
    state = state
      .wrapping_mul(6364136223846793005)
      .wrapping_add(1442695040888963407);
    key.push_str(&format!("{:016x}", state >> 1));
  }
  key.truncate(len);
  key
}

fn commit_key(db: &SingleFileDB, key: &str) -> Result<()> {
  db.begin(false)?;
  if let Err(error) = db.create_node(Some(key)) {
    let _ = db.rollback();
    return Err(error);
  }
  db.commit()
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
  let deadline = Instant::now() + Duration::from_secs(20);
  while !done() {
    assert!(Instant::now() < deadline, "gave up waiting for {what}");
    std::thread::sleep(Duration::from_millis(1));
  }
}

/// R11 (a test gap). An install that fails after its first header slot is
/// durable leaves that slot naming the new snapshot: until a newer header is
/// durable, a crash recovers from it. So the snapshot's pages stay out of
/// reuse (`defer_free_pages` in `install_snapshot`); freeing them at once
/// instead passes every other test.
///
/// Reachable: right after a vacuum the free list is empty, so the failed
/// install's snapshot pages are the only free range. The first thing the
/// database writes after the failure can be a spill, which writes no header
/// before its extent (here an open transaction's records that do not fit in
/// the WAL), and its extent takes the first free range that holds it. A
/// crash after the extent is written and synced, before the spill's header,
/// recovers from the failed install's slot: with the pages freed, over a
/// snapshot the spill overwrote.
#[test]
fn review3_a_failed_install_keeps_its_snapshot_pages_until_a_newer_header_is_durable() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("failed-install-pages.kitedb");
  // Extents of one and a half WALs (24 pages); MVCC: writers commit while
  // the checkpoint runs.
  let options = SingleFileOpenOptions::new()
    .wal_size(SMALL_WAL)
    .sync_mode(SyncMode::Normal)
    .auto_checkpoint(false)
    .wal_segment_size(1)
    .mvcc(true);
  let db = Arc::new(open_single_file(&path, options.clone()).expect("open"));

  // A snapshot of well over an extent, compacted next to the WAL by a vacuum,
  // which leaves no free page.
  let mut acked = Vec::new();
  for index in 0..400 {
    let key = incompressible_key("base", index, 1000);
    commit_key(&db, &key).expect("commit");
    acked.push(key);
  }
  db.vacuum_single_file(None).expect("vacuum");
  let snapshot_pages = db.header.read().snapshot_page_count;
  assert!(
    snapshot_pages >= 48,
    "setup: the snapshot takes {snapshot_pages} pages, not two extents"
  );
  assert!(
    db.pager.lock().free_page_list().is_empty(),
    "setup: free pages after the vacuum"
  );
  for index in 0..10 {
    let key = incompressible_key("pre", index, 200);
    commit_key(&db, &key).expect("commit");
    acked.push(key);
  }

  // A background checkpoint, held once its snapshot is durable, whose install
  // fails once its first header slot is durable.
  watch_checkpoint_phases(&db);
  let held = Arc::new(Barrier::new(2));
  set_checkpoint_test_barrier(&db, CheckpointPhase::SnapshotDurable, Arc::clone(&held));
  set_checkpoint_test_db_fault(&db, CheckpointPhase::HeaderDurable, false);
  let checkpoint = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || db.background_checkpoint())
  };
  wait_until("the held checkpoint", || {
    checkpoint_test_reached(&db)
      .iter()
      .any(|(phase, _, parked)| *phase == CheckpointPhase::SnapshotDurable && *parked)
  });
  // Commits after its cut fill the WAL (their headers come before the
  // install's).
  let mut index = 0;
  while db.wal_buffer.lock().free() > 12 * 1024 {
    let key = incompressible_key("during", index, 200);
    commit_key(&db, &key).expect("commit");
    acked.push(key);
    index += 1;
  }
  held.wait();
  let installed = checkpoint.join().expect("the checkpoint thread");
  assert!(
    installed.is_err(),
    "setup: the install did not fail: {installed:?}"
  );

  // The next write: a transaction's records too large for the WAL's room,
  // which spills it (no header first). Its extent's bytes are written and
  // synced; then it fails before its header, as a crash there would leave
  // it.
  set_checkpoint_test_db_fault(&db, CheckpointPhase::SpillSegmentWritten, false);
  db.begin(false).expect("begin");
  let mut spilled = false;
  for n in 0..40 {
    if db
      .create_node(Some(&incompressible_key("late", n, 1000)))
      .is_err()
    {
      spilled = true;
      break;
    }
  }
  let _ = db.rollback();
  clear_checkpoint_test_db_faults(&db);
  assert!(spilled, "setup: the transaction's records never spilled");

  let copy = dir.path().join("failed-install-pages-crash.kitedb");
  std::fs::copy(&path, &copy).expect("crash copy");
  let crashed = open_single_file(&copy, options.clone());
  let outcome = match &crashed {
    Ok(db) => {
      let lost = acked
        .iter()
        .filter(|key| db.node_by_key(key).is_none())
        .count();
      (
        lost == 0,
        format!("{lost} of {} acknowledged commits lost", acked.len()),
      )
    }
    Err(error) => (false, format!("the crash copy does not open: {error}")),
  };
  drop(crashed);
  let db = Arc::into_inner(db).expect("sole owner");
  close_single_file(db).expect("close");
  assert!(
    outcome.0,
    "a crash after a failed install and a spill: {}",
    outcome.1
  );
}
