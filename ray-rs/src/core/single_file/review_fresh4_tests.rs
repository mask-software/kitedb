//! Delta review of round 4 (89d395a..ead997f). Included from checkpoint.rs
//! for its private steps and test hooks.
use super::*;
use crate::core::single_file::{open_single_file, SingleFileOpenOptions, SyncMode};
use tempfile::tempdir;

fn key(prefix: &str, index: usize) -> String {
  format!("{prefix}-{index:05}-{}", "k".repeat(200))
}

/// Probe (N2's fix): with background checkpoints off and a persistent
/// checkpoint failure, writers refused at the segment limit keep asking for
/// the blocking checkpoint (`blocking_checkpoint_asked`). The back-off must
/// still hold: while it lasts, refused transactions that roll back run no
/// checkpoint (no snapshot is written).
#[test]
fn fresh4_refused_writers_do_not_defeat_the_backoff() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("asked-backoff.kitedb");
  let options = SingleFileOpenOptions::new()
    .wal_size(64 * 1024)
    .sync_mode(SyncMode::Normal)
    .background_checkpoint(false)
    .checkpoint_log_ratio(1000.0);
  let db = open_single_file(&path, options).expect("open");
  super::super::checkpoint_thread::set_checkpoint_test_backoff(
    &db,
    Duration::from_secs(30),
    Duration::from_secs(30),
  );
  db.begin(false).expect("begin");
  db.create_node(Some("seed")).expect("seed");
  db.commit().expect("commit");
  db.checkpoint().expect("seed checkpoint");
  set_wal_segment_test_limit(&db, 64 * 1024);
  set_checkpoint_test_db_fault(&db, CheckpointPhase::SnapshotWritten, true);
  let transaction = |start: usize| -> Result<()> {
    db.begin(false)?;
    for index in start..start + 100 {
      if let Err(error) = db.create_node(Some(&key("tx", index))) {
        db.rollback().expect("rollback");
        return Err(error);
      }
    }
    db.commit()
  };
  // Until the first failed checkpoint is recorded.
  let mut next = 0;
  while db.checkpoint_error().is_none() {
    let _ = transaction(next);
    next += 100;
    assert!(next < 100_000, "setup: no checkpoint failed");
  }
  let written = checkpoint_test_snapshot_bytes(&db);
  let mut refused = 0;
  for _ in 0..50 {
    if transaction(next).is_err() {
      refused += 1;
    }
    next += 100;
  }
  let during_backoff = checkpoint_test_snapshot_bytes(&db) - written;
  clear_checkpoint_test_db_faults(&db);
  assert!(refused > 0, "setup: no transaction was refused");
  assert_eq!(
    during_backoff, 0,
    "{refused} refused transactions during a 30 s back-off wrote {during_backoff} bytes of \
     snapshots: the back-off did not hold"
  );
}
