//! raydb-b4 `checkpoint-segments`, round 6: soft backpressure. A writer
//! that outruns a running background checkpoint is paced (each commit waits
//! a bounded time after it releases its locks), so it does not run into the
//! segment limit and stop there for the rest of the run. Included from
//! checkpoint.rs for its test hooks.
use super::*;
use crate::core::single_file::pacing::{pacing_test_stats, set_pacing_test};
use crate::core::single_file::{
  close_single_file, open_single_file, SingleFileOpenOptions, SyncMode,
};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};
use tempfile::tempdir;

fn key(prefix: &str, index: usize) -> String {
  format!("{prefix}-{index:06}-{}", "k".repeat(200))
}

fn commit_key(db: &SingleFileDB, key: &str) -> Result<()> {
  db.begin(false)?;
  if let Err(error) = db.create_node(Some(key)) {
    let _ = db.rollback();
    return Err(error);
  }
  db.commit()
}

/// A small WAL, background checkpoints run inline (no thread), the trigger
/// at its floor (three eighths of the WAL), and a 1 MiB segment limit.
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

/// A background checkpoint run on another thread, held after it wrote its
/// snapshot until the barrier is passed.
struct HeldRun {
  barrier: Arc<Barrier>,
  thread: std::thread::JoinHandle<Result<()>>,
}

impl HeldRun {
  fn start(db: &SingleFileDB) -> Self {
    watch_checkpoint_phases(db);
    let barrier = Arc::new(Barrier::new(2));
    set_checkpoint_test_barrier(db, CheckpointPhase::SnapshotDurable, Arc::clone(&barrier));
    let handle = db.shared_handle();
    let thread = std::thread::spawn(move || handle.background_checkpoint());
    let held = wait_until(Instant::now() + Duration::from_secs(20), || {
      checkpoint_test_reached(db)
        .iter()
        .any(|(phase, _, parked)| *phase == CheckpointPhase::SnapshotDurable && *parked)
    });
    assert!(held, "setup: the background checkpoint was not held");
    Self { barrier, thread }
  }

  /// Let it go on, and wait for it to end.
  fn release(self) -> Result<()> {
    self.barrier.wait();
    self.thread.join().expect("the held run panicked")
  }

  /// Let it go on after `after`, from another thread (this one may be
  /// waiting for it then).
  fn release_after(self, after: Duration) -> std::thread::JoinHandle<Result<()>> {
    std::thread::spawn(move || {
      std::thread::sleep(after);
      self.release()
    })
  }
}

/// Commit a few keys, so the log is past the trigger.
fn warm_up(db: &SingleFileDB) {
  for index in 0..200 {
    commit_key(db, &key("warm", index)).expect("warm-up commit");
  }
}

/// A writer thread on its own handle committing keys until stopped; each
/// commit's duration, and when the last one returned.
struct Writer {
  stop: Arc<AtomicBool>,
  thread: std::thread::JoinHandle<(Vec<Duration>, Instant)>,
}

impl Writer {
  fn start(db: &SingleFileDB, prefix: &'static str) -> Self {
    let stop = Arc::new(AtomicBool::new(false));
    let handle = db.shared_handle();
    let thread = {
      let stop = Arc::clone(&stop);
      std::thread::spawn(move || {
        let mut durations = Vec::new();
        let mut index = 0;
        while !stop.load(Ordering::Acquire) {
          let started = Instant::now();
          match commit_key(&handle, &key(prefix, index)) {
            Ok(()) => durations.push(started.elapsed()),
            // Told to stop: the database may be closing.
            Err(_) if stop.load(Ordering::Acquire) => break,
            Err(error) => panic!("writer commit: {error}"),
          }
          index += 1;
        }
        (durations, Instant::now())
      })
    };
    Self { stop, thread }
  }

  fn finish(self) -> (Vec<Duration>, Instant) {
    self.stop.store(true, Ordering::Release);
    self.thread.join().expect("the writer panicked")
  }
}

fn wait_for_a_pacing_writer(db: &SingleFileDB) -> bool {
  wait_until(Instant::now() + Duration::from_secs(10), || {
    pacing_test_stats(db).pacing_now > 0
  })
}

/// (a) A writer that outruns a running background checkpoint is paced: its
/// commits slow down so the segments' room below the limit lasts the run,
/// and no single commit waits longer than the bound (`MAX_PACE`, 100 ms;
/// 300 ms here, for scheduling). Without pacing it fills the 1 MiB limit
/// in a fraction of a second and waits there for the rest of the run (1.5 s
/// here: seconds on a large database).
#[test]
fn a_writer_outrunning_a_running_checkpoint_is_paced_not_stopped() {
  let dir = tempdir().expect("tempdir");
  let db = open_single_file(dir.path().join("paced.kitedb"), options()).expect("open");
  warm_up(&db);
  // The run is expected to take 4 s; it is held for 1.5.
  set_pacing_test(&db, Some(Duration::from_secs(4)), None);
  let held = Instant::now();
  let run = HeldRun::start(&db).release_after(Duration::from_millis(1_500));
  let (mut commits, mut slowest) = (0, Duration::ZERO);
  while held.elapsed() < Duration::from_millis(1_400) {
    let started = Instant::now();
    commit_key(&db, &key("tail", commits)).expect("commit");
    slowest = slowest.max(started.elapsed());
    commits += 1;
  }
  run
    .join()
    .expect("the releasing thread")
    .expect("the held run");
  let paced = pacing_test_stats(&db).paced;
  close_single_file(db).expect("close");
  assert!(
    slowest <= Duration::from_millis(300) && paced > 0 && commits >= 50,
    "a writer outrunning a held checkpoint: its slowest commit took {slowest:?} (at most \
     300 ms: paced, not stopped at the segment limit), {paced} commits were paced, {commits} \
     made it"
  );
}

/// (b) A writer pacing holds no lock: other threads begin, write, commit
/// and read meanwhile, and the held run installs (it takes the commit lock
/// and the checkpoint gate), which also ends the pacing.
#[test]
fn a_pacing_writer_holds_no_lock() {
  let dir = tempdir().expect("tempdir");
  let db = open_single_file(dir.path().join("no-lock.kitedb"), options()).expect("open");
  warm_up(&db);
  // A paced commit waits up to 20 s: until the run ends.
  set_pacing_test(
    &db,
    Some(Duration::from_secs(100)),
    Some(Duration::from_secs(20)),
  );
  let run = HeldRun::start(&db);
  let writer = Writer::start(&db, "a");
  let pacing = wait_for_a_pacing_writer(&db);

  let started = Instant::now();
  db.begin(false).expect("begin beside a pacing writer");
  db.create_node(Some("beside")).expect("write");
  db.rollback().expect("rollback");
  let first = db.node_by_key(&key("a", 0)).is_some();
  let other = {
    let handle = db.shared_handle();
    std::thread::spawn(move || commit_key(&handle, "other"))
  };
  let visible = wait_until(Instant::now() + Duration::from_secs(5), || {
    db.node_by_key("other").is_some()
  });
  let beside = started.elapsed();

  let released = Instant::now();
  let installed = run.release();
  let install_took = released.elapsed();
  other
    .join()
    .expect("the other writer")
    .expect("the other commit");
  let (_, writer_returned) = writer.finish();
  let writer_woke = writer_returned.saturating_duration_since(released);
  close_single_file(db).expect("close");
  assert!(pacing, "no writer paced while the checkpoint was held");
  assert!(
    first && visible && beside < Duration::from_secs(2),
    "beside a pacing writer: its first commit read back {first}, another thread's commit \
     visible {visible}, in {beside:?}"
  );
  assert!(
    installed.is_ok() && install_took < Duration::from_secs(5),
    "the held run's install beside a pacing writer: {installed:?} in {install_took:?}"
  );
  assert!(
    writer_woke < Duration::from_secs(2),
    "the pacing writer went on {writer_woke:?} after the run ended (it waits up to 20 s)"
  );
}

/// (c) No commit is paced without a running checkpoint, nor while the log
/// is below the trigger with one running; past the trigger with one
/// running, they are.
#[test]
fn pacing_only_past_the_trigger_with_a_run() {
  let dir = tempdir().expect("tempdir");
  // No run: one writer, its inline checkpoints run between its commits.
  let db = open_single_file(dir.path().join("no-run.kitedb"), options()).expect("open");
  for index in 0..3_000 {
    commit_key(&db, &key("solo", index)).expect("commit");
  }
  let without_a_run = pacing_test_stats(&db).paced;
  // Past the trigger with a run: paced.
  set_pacing_test(&db, Some(Duration::from_secs(4)), None);
  let run = HeldRun::start(&db);
  for index in 0..300 {
    commit_key(&db, &key("past", index)).expect("commit");
  }
  let past_the_trigger = pacing_test_stats(&db).paced;
  run.release().expect("the held run");
  close_single_file(db).expect("close");

  // A run, the log far below the trigger (a thousand times a snapshot of
  // 200 keys).
  let db = open_single_file(
    dir.path().join("below.kitedb"),
    options()
      .checkpoint_log_ratio(1000.0)
      .wal_segment_limit(256 * 1024 * 1024),
  )
  .expect("open");
  warm_up(&db);
  db.checkpoint().expect("a snapshot");
  // Something for the run to cover.
  for index in 0..20 {
    commit_key(&db, &key("cover", index)).expect("commit");
  }
  set_pacing_test(&db, Some(Duration::from_secs(4)), None);
  let run = HeldRun::start(&db);
  for index in 0..300 {
    commit_key(&db, &key("below", index)).expect("commit");
  }
  let below_the_trigger = pacing_test_stats(&db).paced;
  run.release().expect("the held run");
  close_single_file(db).expect("close");
  assert_eq!(
    without_a_run, 0,
    "commits paced without a running checkpoint"
  );
  assert_eq!(
    below_the_trigger, 0,
    "commits paced below the trigger with a run"
  );
  assert!(
    past_the_trigger > 0,
    "no commit paced past the trigger with a run"
  );
}

/// (d) Closing the database ends a writer's pacing at once (the close does
/// not wait for it, and it does not wait out its delay), though the run it
/// paces for still runs.
#[test]
fn close_ends_pacing() {
  let dir = tempdir().expect("tempdir");
  let db = open_single_file(dir.path().join("close.kitedb"), options()).expect("open");
  warm_up(&db);
  set_pacing_test(
    &db,
    Some(Duration::from_secs(100)),
    Some(Duration::from_secs(20)),
  );
  let run = HeldRun::start(&db);
  let writer = Writer::start(&db, "a");
  let pacing = wait_for_a_pacing_writer(&db);
  // The writer stops at its next commit, which the close refuses.
  writer.stop.store(true, Ordering::Release);
  let closing = Instant::now();
  let closer = std::thread::spawn(move || close_single_file(db));
  // The close waits for the held run (its checkpoint does); not the writer.
  let woke = wait_until(Instant::now() + Duration::from_secs(5), || {
    writer.thread.is_finished()
  });
  let writer_woke = closing.elapsed();
  let _ = run.release();
  let closed = closer.join().expect("the close panicked");
  let writer_result = writer.thread.join();
  assert!(pacing, "no writer paced while the checkpoint was held");
  assert!(
    woke && writer_woke < Duration::from_secs(2),
    "a pacing writer went on {writer_woke:?} after the close began (it waits up to 20 s)"
  );
  assert!(closed.is_ok(), "the close: {closed:?}");
  assert!(writer_result.is_ok(), "the writer panicked");
}

/// (e) Pacing ends when the run ends: the pacing writer goes on at once,
/// and its commits after the run are not paced.
#[test]
fn pacing_ends_when_the_run_ends() {
  let dir = tempdir().expect("tempdir");
  let db = open_single_file(dir.path().join("ends.kitedb"), options()).expect("open");
  warm_up(&db);
  set_pacing_test(
    &db,
    Some(Duration::from_secs(100)),
    Some(Duration::from_secs(20)),
  );
  let run = HeldRun::start(&db);
  let writer = Writer::start(&db, "a");
  let pacing = wait_for_a_pacing_writer(&db);
  let released = Instant::now();
  let installed = run.release();
  let (_, writer_returned) = writer.finish();
  let writer_woke = writer_returned.saturating_duration_since(released);
  // After the run: no run active (the inline checkpoints run between this
  // thread's commits).
  let paced = pacing_test_stats(&db).paced;
  for index in 0..300 {
    commit_key(&db, &key("after", index)).expect("commit");
  }
  let paced_after = pacing_test_stats(&db).paced - paced;
  close_single_file(db).expect("close");
  assert!(pacing, "no writer paced while the checkpoint was held");
  assert!(installed.is_ok(), "the held run: {installed:?}");
  assert!(
    writer_woke < Duration::from_secs(2),
    "the pacing writer went on {writer_woke:?} after the run ended (it waits up to 20 s)"
  );
  assert_eq!(paced_after, 0, "commits paced after the run ended");
}
