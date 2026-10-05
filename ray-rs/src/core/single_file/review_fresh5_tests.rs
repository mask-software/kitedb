//! Delta review of rounds 5-6 (a02a9ae..9fc68b3). Included from
//! checkpoint.rs for its private steps and test hooks.
use super::super::pacing::{pacing_test_stats, set_pacing_test};
use super::*;
use crate::core::single_file::{
  close_single_file, open_single_file, SingleFileOpenOptions, SyncMode,
};
use std::sync::Barrier;
use tempfile::tempdir;

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

/// As b4_pacing_tests: inline checkpoints, the trigger at its floor, a
/// 1 MiB segment limit.
fn options() -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .wal_size(64 * 1024)
    .sync_mode(SyncMode::Normal)
    .checkpoint_thread(false)
    .checkpoint_log_ratio(0.01)
    .wal_segment_size(1)
    .wal_segment_limit(1024 * 1024)
}

fn wait_until(deadline: Instant, mut done: impl FnMut() -> bool) -> bool {
  while !done() {
    if Instant::now() >= deadline {
      return false;
    }
    std::thread::sleep(Duration::from_millis(1));
  }
  true
}

/// A background checkpoint on another thread, held after its snapshot is
/// durable; returns the barrier that releases it and its thread.
fn hold_a_run(db: &SingleFileDB) -> (Arc<Barrier>, std::thread::JoinHandle<Result<()>>) {
  watch_checkpoint_phases(db);
  let barrier = Arc::new(Barrier::new(2));
  set_checkpoint_test_barrier(db, CheckpointPhase::SnapshotDurable, Arc::clone(&barrier));
  let handle = db.shared_handle();
  let thread = std::thread::spawn(move || handle.background_checkpoint());
  assert!(
    wait_until(Instant::now() + Duration::from_secs(20), || {
      checkpoint_test_reached(db)
        .iter()
        .any(|(phase, _, parked)| *phase == CheckpointPhase::SnapshotDurable && *parked)
    }),
    "setup: the run was not held"
  );
  (barrier, thread)
}

/// Make the next paced commit wait its whole bound: no room left below the
/// segment limit (the limit at the log as it is).
fn leave_no_room(db: &SingleFileDB) {
  let log = db.header.log_state();
  let wal = db.wal_buffer.lock().used();
  set_wal_segment_test_limit(db, (log.uncovered_segments + wal).max(1));
}

/// One commit on a thread of its own (a shared handle); returns when it
/// started, and its thread.
fn commit_on_a_thread(
  db: &SingleFileDB,
  key: String,
) -> std::thread::JoinHandle<(Result<()>, Duration)> {
  let handle = db.shared_handle();
  std::thread::spawn(move || {
    let started = Instant::now();
    let result = commit_key(&handle, &key);
    (result, started.elapsed())
  })
}

/// What `start_a_long_pace` starts: the barrier that releases the held
/// run, the run's thread, and the pacing commit's thread (its result and
/// how long it took).
type LongPace = (
  Arc<Barrier>,
  std::thread::JoinHandle<Result<()>>,
  std::thread::JoinHandle<(Result<()>, Duration)>,
);

/// A writer pacing for its whole `bound`: a held run, commits past the
/// trigger (paced briefly) charging its log, then no room left below the
/// segment limit, and one commit on a thread of its own, pacing now.
fn start_a_long_pace(db: &SingleFileDB, bound: Duration) -> LongPace {
  for index in 0..200 {
    commit_key(db, &key("warm", index)).expect("warm-up commit");
  }
  let (barrier, run) = hold_a_run(db);
  set_pacing_test(
    db,
    Some(Duration::from_secs(100)),
    Some(Duration::from_millis(1)),
  );
  for index in 0..200 {
    commit_key(db, &key("charge", index)).expect("commit");
  }
  let log = db.header.log_state();
  assert!(
    log.uncovered_segments + db.wal_buffer.lock().used() > log.trigger,
    "setup: the log is not past the trigger"
  );
  set_pacing_test(db, Some(Duration::from_secs(100)), Some(bound));
  leave_no_room(db);
  let paced = commit_on_a_thread(db, key("paced", 0));
  assert!(
    wait_until(Instant::now() + Duration::from_secs(5), || {
      pacing_test_stats(db).pacing_now > 0
    }),
    "setup: no commit paced"
  );
  (barrier, run, paced)
}

/// T1 (test gap). b4_pacing_tests' "a pacing writer holds no lock" passes
/// when the pacing wait holds the commit lock: its paced commits wait about
/// 30 ms each (the run's time spread over the room below the limit), so
/// other commits slip in between. Here one paced commit waits its whole
/// bound (2 s: no room is left below the limit), and another thread's
/// commit must not wait for it.
#[test]
fn fresh5_a_long_paced_wait_holds_no_lock() {
  let dir = tempdir().expect("tempdir");
  let db = open_single_file(dir.path().join("long-pace.kitedb"), options()).expect("open");
  let (barrier, run, paced) = start_a_long_pace(&db, Duration::from_secs(2));
  // Another commit: it is durable and visible at once, whatever its own
  // pacing after (which this does not time).
  let started = Instant::now();
  let other = commit_on_a_thread(&db, key("other", 0));
  let visible = wait_until(Instant::now() + Duration::from_secs(5), || {
    db.node_by_key(&key("other", 0)).is_some()
  });
  let other_took = started.elapsed();
  set_wal_segment_test_limit(&db, 1024 * 1024);
  barrier.wait();
  run.join().expect("the run").expect("the held run");
  let _ = paced.join().expect("the paced commit");
  let (result, _) = other.join().expect("the other commit");
  close_single_file(db).expect("close");
  assert!(result.is_ok(), "the other commit: {result:?}");
  assert!(
    visible && other_took < Duration::from_millis(1_000),
    "a commit beside a writer pacing (for up to 2 s) was visible {visible} after \
     {other_took:?}: the pacing writer holds a lock the commit needs"
  );
}

/// T2 (test gap). b4_pacing_tests' "pacing ends when the run ends" passes
/// when the pacing wait ignores the run's end, for the same reason. Here a
/// commit paces for its whole bound (20 s); the run's end must end it.
#[test]
fn fresh5_the_run_ending_ends_a_long_paced_wait() {
  let dir = tempdir().expect("tempdir");
  let db = open_single_file(dir.path().join("pace-ends.kitedb"), options()).expect("open");
  let (barrier, run, paced) = start_a_long_pace(&db, Duration::from_secs(20));
  set_wal_segment_test_limit(&db, 1024 * 1024);
  let released = Instant::now();
  barrier.wait();
  run.join().expect("the run").expect("the held run");
  let (result, _) = paced.join().expect("the paced commit");
  let woke = released.elapsed();
  close_single_file(db).expect("close");
  assert!(result.is_ok(), "the paced commit: {result:?}");
  assert!(
    woke < Duration::from_secs(2),
    "the paced commit went on {woke:?} after the run ended (it paces up to 20 s)"
  );
}

/// T3 (test gap). Pacing stops when a run ends without installing too:
/// the automatic run fails, its back-off holds off the next one, the log
/// stays past the trigger, and no commit after it may be paced (no run goes
/// on). b4_pacing_tests checks only after an install, which takes the log
/// below the trigger, so a pacer that never noted the run's end passes it.
#[test]
fn fresh5_no_pacing_after_a_failed_run() {
  let dir = tempdir().expect("tempdir");
  let db = open_single_file(
    dir.path().join("failed-run.kitedb"),
    options().checkpoint_thread(true),
  )
  .expect("open");
  super::super::checkpoint_thread::set_checkpoint_test_backoff(
    &db,
    Duration::from_secs(30),
    Duration::from_secs(30),
  );
  set_pacing_test(
    &db,
    Some(Duration::from_secs(4)),
    Some(Duration::from_millis(50)),
  );
  // The checkpoint thread's run, held after its snapshot.
  watch_checkpoint_phases(&db);
  let barrier = Arc::new(Barrier::new(2));
  set_checkpoint_test_barrier(&db, CheckpointPhase::SnapshotDurable, Arc::clone(&barrier));
  let mut index = 0;
  let held = wait_until(Instant::now() + Duration::from_secs(20), || {
    commit_key(&db, &key("warm", index)).expect("commit");
    index += 1;
    checkpoint_test_reached(&db)
      .iter()
      .any(|(phase, _, parked)| *phase == CheckpointPhase::SnapshotDurable && *parked)
  });
  assert!(held, "setup: the checkpoint thread's run was not held");
  for more in 0..50 {
    commit_key(&db, &key("during", more)).expect("commit");
  }
  assert!(
    pacing_test_stats(&db).paced > 0,
    "setup: nothing paced during the run"
  );
  // It fails in its install once released, and backs off for 30 s.
  set_checkpoint_test_db_fault(&db, CheckpointPhase::HeaderWritten, false);
  barrier.wait();
  assert!(
    wait_until(Instant::now() + Duration::from_secs(10), || {
      !db.is_checkpoint_running() && db.checkpoint_error().is_some()
    }),
    "setup: the run did not fail"
  );
  let paced = pacing_test_stats(&db).paced;
  for more in 0..100 {
    commit_key(&db, &key("after", more)).expect("commit");
  }
  let paced_after = pacing_test_stats(&db).paced - paced;
  let running_after = db.is_checkpoint_running();
  let log = db.header.log_state();
  let past_trigger = log.uncovered_segments + db.wal_buffer.lock().used() > log.trigger;
  close_single_file(db).expect("close");
  assert!(
    !running_after && past_trigger,
    "setup: a run went on, or the log fell below the trigger"
  );
  assert_eq!(
    paced_after, 0,
    "{paced_after} commits paced after the run failed, with no run going on"
  );
}
