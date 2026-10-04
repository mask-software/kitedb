//! Second review pass of `fix/b4-checkpoint-segments` (the fixes in
//! 8f89b44). Each test reproduces a bug the fixes introduced, and fails on
//! that commit. Included from checkpoint.rs for its private steps and test
//! hooks.
use super::*;
use crate::core::single_file::{close_single_file, open_single_file, SingleFileOpenOptions, SyncMode};
use std::sync::mpsc;
use std::sync::Arc;
use tempfile::tempdir;

/// The smallest WAL a database accepts: a 48 KiB primary region.
const SMALL_WAL: usize = 64 * 1024;

/// The key of the `index`th node of `prefix`; its commit takes about 300
/// bytes of WAL.
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

/// The keys of the transaction `commit_across_a_cut` commits.
fn held_keys() -> Vec<String> {
  (0..20)
    .map(|n| format!("held-{n}-{}", "h".repeat(1000)))
    .collect()
}

/// Leave the database at `path` as a crash or a plain close can: a write
/// transaction whose records spilled into a WAL segment while it was open
/// (a checkpoint's cut spilled them; its install kept that segment, which
/// the snapshot covers), and which committed after that cut (its COMMIT
/// record in the WAL). Every one of its records is still in the log, and a
/// reopen replays it. Returns the transaction's keys.
fn commit_across_a_cut(path: &std::path::Path, options: &SingleFileOpenOptions) -> Vec<String> {
  let db = Arc::new(open_single_file(path, options.clone()).expect("open"));
  let (held_tx, held_rx) = mpsc::channel();
  let (go_tx, go_rx) = mpsc::channel::<()>();
  let holder = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || {
      db.begin(false).expect("begin");
      // More than the transaction keeps back: its records go to the WAL.
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
  let db = Arc::into_inner(db).expect("sole owner");
  close_single_file(db).expect("close");
  held_keys()
}

fn missing(db: &SingleFileDB, keys: &[String]) -> usize {
  keys
    .iter()
    .filter(|key| db.node_by_key(key).is_none())
    .count()
}

fn options() -> SingleFileOpenOptions {
  // MVCC: the holder and the other writes are open at once.
  SingleFileOpenOptions::new()
    .wal_size(SMALL_WAL)
    .sync_mode(SyncMode::Normal)
    .auto_checkpoint(false)
    .mvcc(true)
}

/// R7a. `spilled_txids` starts empty at open: nothing rebuilds it from the
/// log. After a reopen, a transaction that committed after a cut, with its
/// first records in a segment that cut covered (kept by its install), is
/// unknown to the checkpoint: its cut drops that segment as unneeded
/// (`unneeded_wal_segments`, `needed_from` = `u64::MAX`), and its replay of
/// the commits up to the cut starts after it (`read_from` = covered + 1).
/// The transaction's COMMIT record is replayed without its BEGIN record,
/// so the new snapshot lacks it, the install replaces the delta that had
/// it, and it is gone, live and after a reopen.
#[test]
fn review2_a_transaction_committed_across_a_cut_survives_a_reopen_and_the_next_checkpoint() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("reopen-checkpoint.kitedb");
  let held = commit_across_a_cut(&path, &options());

  let db = open_single_file(&path, options()).expect("reopen");
  assert_eq!(missing(&db, &held), 0, "setup: the reopen lost the commit");
  db.background_checkpoint()
    .expect("the first checkpoint after the reopen");
  let lost_live = missing(&db, &held);
  close_single_file(db).expect("close");
  let db = open_single_file(&path, options()).expect("reopen");
  let lost_reopened = missing(&db, &held);
  assert!(
    lost_live == 0 && lost_reopened == 0,
    "the first checkpoint after a reopen dropped a committed transaction: {lost_live} of its \
     {} nodes are gone live, {lost_reopened} after another reopen",
    held.len()
  );
}

/// R7b. The same without a checkpoint: after the reopen, the first spill a
/// writer makes drops the segment holding the transaction's first records
/// (`spill_wal` drops `unneeded_wal_segments`), durably in both header
/// slots. A crash then loses the transaction: its COMMIT record has no
/// BEGIN record left in the log.
#[test]
fn review2_a_transaction_committed_across_a_cut_survives_a_reopen_a_spill_and_a_crash() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("reopen-spill.kitedb");
  let held = commit_across_a_cut(&path, &options());

  let db = open_single_file(&path, options()).expect("reopen");
  assert_eq!(missing(&db, &held), 0, "setup: the reopen lost the commit");
  let spills = wal_segment_test_stats(&db).next_seq;
  let mut index = 0;
  while wal_segment_test_stats(&db).next_seq == spills && index < 1_000 {
    commit_key(&db, &key("after", index)).expect("commit");
    index += 1;
  }
  assert!(
    wal_segment_test_stats(&db).next_seq > spills,
    "setup: the WAL never spilled"
  );
  // A crash now: a copy of the file as it is.
  let copy = dir.path().join("reopen-spill-crash.kitedb");
  std::fs::copy(&path, &copy).expect("crash copy");
  drop(db);
  let crashed = open_single_file(&copy, options()).expect("open the crash copy");
  let lost = missing(&crashed, &held);
  assert_eq!(
    lost,
    0,
    "a spill after a reopen dropped the segment holding a committed transaction's first \
     records: a crash lost {lost} of its {} nodes",
    held.len()
  );
}
