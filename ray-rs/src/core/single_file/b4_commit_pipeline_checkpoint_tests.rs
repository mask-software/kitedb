//! raydb-b4 `commit-pipeline` lane: commits during a background checkpoint
//! (engine-concurrency F6), held at a phase hook instead of timed. Included
//! from checkpoint.rs for its phase hooks.
use super::*;
use crate::core::single_file::{open_single_file, SingleFileOpenOptions, SyncMode};
use std::sync::mpsc;
use std::sync::{Arc, Barrier};
use tempfile::tempdir;

/// Commits made while the checkpoint is held.
const COMMITS: usize = 20;
/// How long they get. Each takes milliseconds; only commits that wait for the
/// checkpoint to finish run out of it.
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
    commits_while_background_checkpoint_is_held(mvcc);
  }
}

fn commits_while_background_checkpoint_is_held(mvcc: bool) {
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

  let cut_released = Arc::new(Barrier::new(2));
  let snapshot_durable = Arc::new(Barrier::new(2));
  set_checkpoint_test_barrier(&db, CheckpointPhase::CutReleased, Arc::clone(&cut_released));
  set_checkpoint_test_barrier(
    &db,
    CheckpointPhase::SnapshotDurable,
    Arc::clone(&snapshot_durable),
  );
  let checkpointer = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || db.background_checkpoint())
  };
  // Its cut is taken and the gate open again. From here it collects, writes
  // and syncs its snapshot, then parks before its install.
  cut_released.wait();

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
  snapshot_durable.wait();
  checkpointer
    .join()
    .expect("checkpointer")
    .expect("background checkpoint");
  committer.join().expect("committer");

  assert!(
    finished,
    "{COMMITS} commits did not finish within {COMMIT_DEADLINE:?} while a background checkpoint \
     (mvcc: {mvcc}) was held between its cut and its install"
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
