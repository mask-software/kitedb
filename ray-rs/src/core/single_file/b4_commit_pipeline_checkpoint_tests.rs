//! raydb-b4 `commit-pipeline` lane: commits during a background checkpoint
//! (engine-concurrency F6), held at phase hooks instead of timed. Included
//! from checkpoint.rs for its phase hooks.
use super::*;
use crate::core::single_file::{open_single_file, SingleFileOpenOptions, SyncMode};
use std::sync::mpsc;
use std::sync::{Arc, Barrier};
use tempfile::tempdir;

/// Commits made while the checkpoint is held.
const COMMITS: usize = 20;
/// How long they get. Each takes milliseconds; only commits that wait for the
/// checkpoint to move on run out of it.
const COMMIT_DEADLINE: Duration = Duration::from_secs(20);

fn commit_node(db: &SingleFileDB, key: &str) {
  db.begin(false).expect("begin");
  db.create_node(Some(key)).expect("create node");
  db.commit().expect("commit");
}

/// Commits complete while a background checkpoint is between its cut and its
/// install (collecting, serializing and syncing its snapshot): they wait for
/// the cut and the install, never for the whole run. The run is held at
/// `SnapshotDurable`, after its collect, until the commits are done, so a
/// commit that waited for the run would never finish. (This replaces a
/// wall-clock check, "no commit waits a third of the checkpoint", that failed
/// under machine load.)
#[test]
fn f6_commits_complete_while_a_background_checkpoint_is_held_after_its_cut() {
  for mvcc in [false, true] {
    commits_while_background_checkpoint_is_held(
      CheckpointPhase::CutReleased,
      CheckpointPhase::SnapshotDurable,
      mvcc,
    );
  }
}

/// Commits complete while a background checkpoint writes and syncs its
/// snapshot: held after its first page is written, then again before its
/// sync. No header names those pages until the install, so nothing orders
/// commits after them. Under machine load the checkpoint wrote and synced
/// them holding the pager lock, which every commit takes to append to the
/// WAL, and a slow fsync stalled every commit.
#[test]
fn f6_commits_complete_while_a_background_checkpoint_writes_its_snapshot() {
  for mvcc in [false, true] {
    commits_while_background_checkpoint_is_held(
      CheckpointPhase::SnapshotPageWritten,
      CheckpointPhase::SnapshotWritten,
      mvcc,
    );
  }
}

/// Run a background checkpoint, and once it has reached `reached`, commit
/// while it is held at `held` (reached at or after `reached`); the commits
/// must complete before it is released.
fn commits_while_background_checkpoint_is_held(
  reached: CheckpointPhase,
  held: CheckpointPhase,
  mvcc: bool,
) {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join(format!("f6-held-mvcc-{mvcc}.kitedb"));
  let options = SingleFileOpenOptions::new()
    .mvcc(mvcc)
    .mvcc_gc_interval_ms(10)
    .sync_mode(SyncMode::Normal)
    .auto_checkpoint(false);
  let db = Arc::new(open_single_file(&path, options.clone()).expect("open"));
  for index in 0..100 {
    commit_node(&db, &format!("pre-{index}"));
  }

  let reached_barrier = Arc::new(Barrier::new(2));
  let held_barrier = Arc::new(Barrier::new(2));
  set_checkpoint_test_barrier(&db, reached, Arc::clone(&reached_barrier));
  set_checkpoint_test_barrier(&db, held, Arc::clone(&held_barrier));
  let checkpointer = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || db.background_checkpoint())
  };
  // From here it runs on to `held` and parks there.
  reached_barrier.wait();

  let (done_tx, done_rx) = mpsc::channel();
  let committer = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || {
      for index in 0..COMMITS {
        commit_node(&db, &format!("during-{index}"));
      }
      let _ = done_tx.send(());
    })
  };
  let finished = done_rx.recv_timeout(COMMIT_DEADLINE).is_ok();
  let still_running = db.checkpoint_state.lock().status == CheckpointStatus::Running;
  // Release the run either way, so a failure ends instead of hanging.
  held_barrier.wait();
  checkpointer
    .join()
    .expect("checkpointer")
    .expect("background checkpoint");
  committer.join().expect("committer");

  assert!(
    finished,
    "{COMMITS} commits did not finish within {COMMIT_DEADLINE:?} while a background checkpoint \
     (mvcc: {mvcc}) was held at {held:?}"
  );
  assert!(still_running, "the checkpoint was not running meanwhile");
  for index in 0..COMMITS {
    assert!(db.node_by_key(&format!("during-{index}")).is_some());
  }
  drop(db);
  let reopened = open_single_file(&path, options).expect("reopen");
  for index in 0..COMMITS {
    assert!(reopened.node_by_key(&format!("during-{index}")).is_some());
  }
}
