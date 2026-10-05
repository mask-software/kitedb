//! raydb-b4 `checkpoint-segments`, round 5: the commit path's own cost with
//! several writers. Included from checkpoint.rs for its private steps.
use super::*;
use crate::core::single_file::{open_single_file, SingleFileOpenOptions};
use std::sync::mpsc;
use tempfile::tempdir;

/// The automatic checkpoint's check after every commit and rollback
/// (`auto_checkpoint_if_needed`) waited for the header's lock to measure the
/// log against the checkpoint trigger. A commit group's leader holds that
/// lock for writing across its header write and, in Full mode, the group's
/// sync: with several writers, each commit's check waited for the next
/// group's sync, and the leader waited for the checks. The 8-writer stall
/// probe on the 1M-node database: 106-158 us per check, and 289-426 us per
/// group header write against 70-84 us on main. The check must not wait for
/// a writer of the header.
#[test]
fn the_check_after_a_commit_does_not_wait_for_a_header_writer() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("check.kitedb");
  let db = Arc::new(
    open_single_file(&path, SingleFileOpenOptions::new().wal_size(64 * 1024)).expect("open"),
  );
  db.begin(false).expect("begin");
  db.create_node(Some("a")).expect("node");
  db.commit().expect("commit");

  // A commit group's leader, between its header write and its sync.
  let leader = db.header.write();
  let (done, finished) = mpsc::channel();
  let checker = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || {
      db.auto_checkpoint_if_needed(false);
      let _ = done.send(());
    })
  };
  let waited = finished.recv_timeout(Duration::from_secs(5)).is_err();
  drop(leader);
  checker.join().expect("the checker");
  assert!(
    !waited,
    "the automatic checkpoint's check waited for the header's writer (a commit group's leader \
     mid-sync)"
  );
}

/// The WAL's headroom below the checkpoint trigger that the check reads
/// instead of the header (`HeaderCell::wal_headroom`) follows every write of
/// the header: after commits, spills, a background checkpoint (which keeps
/// the segments after its cut) and a blocking one, it is what the header
/// says, and the check's verdict is the ratio's.
#[test]
fn the_wal_headroom_follows_every_header_write() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("headroom.kitedb");
  let db = open_single_file(
    &path,
    SingleFileOpenOptions::new()
      .wal_size(64 * 1024)
      .auto_checkpoint(false),
  )
  .expect("open");
  let check = |what: &str| {
    let expected = {
      let header = db.header.read();
      let table = &header.wal_segments;
      let uncovered: u64 = table
        .entries
        .iter()
        .filter(|segment| segment.seq > table.covered)
        .map(|segment| segment.byte_len)
        .sum();
      db.checkpoint_log_trigger(&header)
        .max(1)
        .saturating_sub(uncovered)
    };
    assert_eq!(db.header.wal_headroom(), expected, "{what}: the headroom");
    assert_eq!(
      db.log_reached_trigger(),
      db.log_usage_ratio() >= 1.0,
      "{what}: the check's verdict"
    );
  };
  check("opened");
  let mut index = 0;
  let mut commit_until = |db: &SingleFileDB, spills: u64, what: &str| {
    let target = db.wal_spills.load(Ordering::Acquire) + spills;
    while db.wal_spills.load(Ordering::Acquire) < target {
      db.begin(false).expect("begin");
      db.create_node(Some(&format!("node-{index:06}-{}", "k".repeat(200))))
        .expect("node");
      db.commit().expect("commit");
      index += 1;
      check(&format!("{what}, commit {index}"));
    }
  };
  commit_until(&db, 3, "before any checkpoint");
  let reached = db.log_reached_trigger();
  db.background_checkpoint().expect("background checkpoint");
  check("after a background checkpoint");
  commit_until(&db, 2, "after a background checkpoint");
  db.checkpoint().expect("checkpoint");
  check("after a blocking checkpoint");
  commit_until(&db, 1, "after a blocking checkpoint");
  assert!(
    reached,
    "setup: three spills of a 64 KiB WAL never reached the trigger"
  );
}
