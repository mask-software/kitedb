//! Delta review of rounds 5-6 (9fc68b3): the header's cached log state.
//! Included from checkpoint.rs.
use super::*;
use crate::core::single_file::{
  close_single_file, open_single_file, ResizeWalOptions, SingleFileOpenOptions, SyncMode,
  VacuumOptions,
};
use std::sync::{mpsc, Arc};
use tempfile::tempdir;

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

/// The cached log state (`HeaderCell`) agrees with the header it was cached
/// from, and the lock-free trigger check with the one that reads the header.
fn assert_cached(db: &SingleFileDB, what: &str) {
  let (uncovered, trigger, wal_size) = {
    let header = db.header.read();
    let table = &header.wal_segments;
    (
      table
        .entries
        .iter()
        .filter(|segment| segment.seq > table.covered)
        .map(|segment| segment.byte_len)
        .sum::<u64>(),
      db.checkpoint_log_trigger(&header),
      header.wal_page_count * header.page_size as u64,
    )
  };
  let cached = db.header.log_state();
  assert_eq!(
    (cached.uncovered_segments, cached.trigger, cached.wal_size),
    (uncovered, trigger, wal_size),
    "{what}: the cached log state is stale"
  );
  assert_eq!(
    db.log_reached_trigger(),
    db.log_usage_ratio() >= 1.0,
    "{what}: the lock-free trigger check disagrees with the header's"
  );
}

/// Commit keys until the WAL spills (or `limit` commits).
fn commit_until_spill(db: &SingleFileDB, prefix: &str, limit: usize) {
  let spills = wal_segment_test_stats(db).next_seq;
  for index in 0..limit {
    commit_key(db, &key(prefix, index)).expect("commit");
    if wal_segment_test_stats(db).next_seq != spills {
      return;
    }
  }
  panic!("setup: {prefix}: the WAL never spilled");
}

/// Guard for focus item 1: every path that changes the WAL, the segments,
/// the snapshot or the WAL's size keeps the cached headroom and log state
/// in step with the header: open, spills (a writer's, an open transaction's,
/// a commit larger than the WAL), a background checkpoint's cut and install
/// (also one that keeps a segment for an open transaction), a blocking
/// checkpoint, optimize, a WAL resize, vacuum, and a reopen with segments.
#[test]
fn review6_the_cached_log_state_follows_every_header_change() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("cached-log-state.kitedb");
  let options = SingleFileOpenOptions::new()
    .wal_size(SMALL_WAL)
    .sync_mode(SyncMode::Normal)
    .auto_checkpoint(false)
    .mvcc(true);
  let db = Arc::new(open_single_file(&path, options.clone()).expect("open"));
  assert_cached(&db, "open");
  commit_until_spill(&db, "a", 2_000);
  assert_cached(&db, "a writer's spill");

  // An open transaction's records spill, and a checkpoint keeps them.
  let (wrote_tx, wrote_rx) = mpsc::channel();
  let (go_tx, go_rx) = mpsc::channel::<()>();
  let holder = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || {
      db.begin(false).expect("begin");
      for n in 0..60 {
        db.create_node(Some(&format!("held-{n}-{}", "h".repeat(1000))))
          .expect("node");
      }
      wrote_tx.send(()).expect("signal");
      go_rx.recv().expect("wait");
      db.commit()
    })
  };
  wrote_rx.recv().expect("the holder wrote");
  assert_cached(&db, "an open transaction's spill");
  db.background_checkpoint().expect("checkpoint");
  assert_cached(&db, "an install keeping a segment");
  go_tx.send(()).expect("release");
  holder.join().expect("holder").expect("holder commit");
  assert_cached(&db, "the holder's commit");

  // A commit larger than the WAL, straight into a segment.
  let keys: Vec<String> = (0..300).map(|index| key("bulk", index)).collect();
  let refs: Vec<Option<&str>> = keys.iter().map(|key| Some(key.as_str())).collect();
  db.begin_bulk().expect("begin bulk");
  db.create_nodes_batch(&refs).expect("nodes");
  db.commit().expect("bulk commit");
  assert_cached(&db, "a commit larger than the WAL");

  commit_until_spill(&db, "b", 2_000);
  db.background_checkpoint().expect("checkpoint");
  assert_cached(&db, "a background install");
  commit_until_spill(&db, "c", 2_000);
  db.checkpoint().expect("blocking checkpoint");
  assert_cached(&db, "a blocking checkpoint");
  commit_until_spill(&db, "d", 2_000);
  db.optimize_single_file(None).expect("optimize");
  assert_cached(&db, "optimize");
  db.resize_wal(
    2 * SMALL_WAL,
    Some(ResizeWalOptions {
      allow_shrink: false,
      checkpoint: true,
    }),
  )
  .expect("resize");
  assert_cached(&db, "a WAL resize");
  commit_until_spill(&db, "e", 4_000);
  db.vacuum_single_file(Some(VacuumOptions {
    shrink_wal: true,
    min_wal_size: None,
  }))
  .expect("vacuum");
  assert_cached(&db, "vacuum");
  commit_until_spill(&db, "f", 4_000);
  assert_cached(&db, "a spill before reopening");
  let db = Arc::into_inner(db).expect("sole owner");
  drop(db); // Drop keeps the segments (close would checkpoint them away).
  let db = open_single_file(&path, options).expect("reopen");
  assert!(
    wal_segment_test_stats(&db).live > 0,
    "setup: the reopened file has no segment"
  );
  assert_cached(&db, "a reopen with segments");
  close_single_file(db).expect("close");
}
