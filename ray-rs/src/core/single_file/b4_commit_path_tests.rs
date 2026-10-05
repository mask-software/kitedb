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
