//! The checkpoint thread: automatic checkpoints run on a thread of their
//! own, so the commit that crosses the trigger returns at once instead of
//! running a whole snapshot rebuild first.
//!
//! One thread per writable database, started at its first automatic
//! checkpoint (none for read-only databases, or with `checkpoint_thread`
//! off, or on wasm32, where the auto-checkpoint runs inline as before). It
//! holds a handle to the database (`SingleFileDB::shared_handle`) and sleeps
//! until a commit asks for a checkpoint; it runs one, records its error if
//! it fails (`SingleFileDB::checkpoint_error`), and after a failure waits
//! before running another (doubling from one second to a minute, however
//! often it is asked meanwhile; a checkpoint that succeeds meanwhile, a
//! caller's, ends the wait) instead of failing in a loop. Without the thread
//! the automatic checkpoints that run inline record their failures and back
//! off the same way (`record_checkpoint_result`, `AutoCheckpointFailure`). A
//! panic in a run is caught and recorded too; it may have struck between
//! writes that keep memory and disk in step, so the handle refuses writes
//! from then on (`KiteError::WritesRefused`; reads go on, and reopening
//! recovers from disk), and the thread ends.
//!
//! Closing (or dropping) the database stops it: a run still building its
//! snapshot stops at its next progress point (nothing names the pages it
//! wrote: they are free again, at the latest at the next open), a run in its
//! install finishes it, and the thread is joined before anything else
//! closes. No commit depends on a run: every one is in the WAL or a WAL
//! segment. Writers depend on runs only once the WAL segments reach their
//! limit (see `wait_for_segment_space`).

use std::cell::Cell;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};

use crate::error::KiteError;

use super::SingleFileDB;

/// The name the checkpoint thread runs under.
pub(crate) const CHECKPOINT_THREAD_NAME: &str = "kitedb-checkpoint";

/// The first wait after a failed automatic checkpoint; it doubles up to
/// `MAX_BACKOFF` while failures go on.
const FIRST_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// The last automatic checkpoint's failure, and the back-off after it: no
/// automatic checkpoint starts before `retry_at`, on the checkpoint thread
/// or inline. A checkpoint that succeeds (any: automatic, a caller's,
/// optimize's) clears it (`SingleFileDB::end_checkpoint_backoff`).
#[derive(Debug, Default)]
pub(crate) struct AutoCheckpointFailure {
  /// The error (`SingleFileDB::checkpoint_error`).
  error: Option<String>,
  /// The back-off after it: the first, doubled for each failure in a row.
  backoff: Option<Duration>,
  /// When the back-off ends.
  retry_at: Option<Instant>,
}

/// The first and the longest back-off of the checkpoint threads of the
/// databases at these paths, set by tests (`set_checkpoint_test_backoff`).
#[cfg(test)]
static TEST_BACKOFF: std::sync::Mutex<Vec<(std::path::PathBuf, Duration, Duration)>> =
  std::sync::Mutex::new(Vec::new());

/// Make the back-off of `db`'s automatic checkpoints start at `first` and
/// double up to `max`.
#[cfg(test)]
pub(crate) fn set_checkpoint_test_backoff(db: &SingleFileDB, first: Duration, max: Duration) {
  TEST_BACKOFF
    .lock()
    .expect("test backoff lock")
    .push((db.path().to_path_buf(), first, max));
}

/// The first and the longest wait after a failed automatic checkpoint of
/// `db`.
fn backoff_bounds(db: &SingleFileDB) -> (Duration, Duration) {
  #[cfg(test)]
  if let Some((_, first, max)) = TEST_BACKOFF
    .lock()
    .expect("test backoff lock")
    .iter()
    .rev()
    .find(|(path, _, _)| path == db.path())
  {
    return (*first, *max);
  }
  let _ = db;
  (FIRST_BACKOFF, MAX_BACKOFF)
}

thread_local! {
  /// Set on a checkpoint thread: its runs stop when the database closes.
  static ON_CHECKPOINT_THREAD: Cell<bool> = const { Cell::new(false) };
}

/// Whether the calling thread is a checkpoint thread.
pub(crate) fn on_checkpoint_thread() -> bool {
  ON_CHECKPOINT_THREAD.with(Cell::get)
}

#[derive(Default)]
struct Requests {
  /// A commit asked for a checkpoint since the last run started.
  checkpoint: bool,
  /// The database is closing: stop.
  stop: bool,
  /// Requests made so far.
  asked: u64,
  /// Requests a finished run answered (a run answers every request made
  /// before it started).
  answered: u64,
}

/// What a database and its checkpoint thread share.
#[derive(Default)]
pub(crate) struct CheckpointSignal {
  requests: Mutex<Requests>,
  wake: Condvar,
}

/// A running checkpoint thread.
pub(crate) struct CheckpointThread {
  signal: Arc<CheckpointSignal>,
  handle: std::thread::JoinHandle<()>,
}

impl SingleFileDB {
  /// Whether automatic checkpoints run on a checkpoint thread.
  pub(crate) fn uses_checkpoint_thread(&self) -> bool {
    cfg!(not(target_arch = "wasm32"))
      && self.checkpoint_thread_enabled
      && self.background_checkpoint
      && !self.read_only
  }

  /// Ask the checkpoint thread for a checkpoint, starting it if it is not
  /// running (or starting it again if it ended unexpectedly). Returns the
  /// request's number (see `checkpoint_request_answered`), or `None` once the
  /// database is closing or refuses writes, or if the thread cannot start.
  pub(crate) fn request_background_checkpoint(&self) -> Option<u64> {
    let mut thread = self.checkpoint_thread.lock();
    if self.checkpoint_thread_stopped.load(Ordering::Acquire)
      || self.ensure_writes_allowed().is_err()
    {
      return None;
    }
    if thread
      .as_ref()
      .is_some_and(|thread| thread.handle.is_finished())
    {
      // Only a stop or a run's panic (caught; the handle refuses writes
      // since, so no request gets here) ends the thread's loop; a thread
      // gone otherwise would leave every request unanswered.
      // (Recorded without waking the writers waiting for segment space: one
      // of them may be the caller, holding their lock.)
      if let Some(ended) = thread.take() {
        let panicked = ended.handle.join().is_err();
        let error = format!(
          "the checkpoint thread ended unexpectedly{}; started it again",
          if panicked { " (it panicked)" } else { "" }
        );
        eprintln!("Warning: {error}");
        self.auto_checkpoint_failure.lock().error = Some(error);
      }
    }
    if thread.is_none() {
      let signal = Arc::new(CheckpointSignal::default());
      let db = self.shared_handle();
      let thread_signal = Arc::clone(&signal);
      let spawned = std::thread::Builder::new()
        .name(CHECKPOINT_THREAD_NAME.to_string())
        .spawn(move || run_checkpoint_thread(db, thread_signal));
      match spawned {
        Ok(handle) => *thread = Some(CheckpointThread { signal, handle }),
        Err(error) => {
          eprintln!("Warning: could not start the checkpoint thread: {error}");
          return None;
        }
      }
    }
    let signal = &thread.as_ref().expect("started above").signal;
    let mut requests = signal.requests.lock();
    requests.checkpoint = true;
    requests.asked += 1;
    signal.wake.notify_all();
    Some(requests.asked)
  }

  /// Whether a run that started after request `ticket` (see
  /// `request_background_checkpoint`) has ended, or the thread has.
  pub(crate) fn checkpoint_request_answered(&self, ticket: u64) -> bool {
    match self.checkpoint_thread.lock().as_ref() {
      Some(thread) => {
        let requests = thread.signal.requests.lock();
        requests.answered >= ticket || requests.stop
      }
      None => true,
    }
  }

  /// Wait until the checkpoint thread has answered every request made so
  /// far (run a checkpoint, or found it had nothing to do), or ended.
  pub(crate) fn wait_for_checkpoint_thread(&self) {
    let signal = match self.checkpoint_thread.lock().as_ref() {
      Some(thread) => Arc::clone(&thread.signal),
      None => return,
    };
    let mut requests = signal.requests.lock();
    let asked = requests.asked;
    while requests.answered < asked && !requests.stop {
      signal.wake.wait(&mut requests);
    }
  }

  /// Stop the checkpoint thread for good, and wait for it to end: a run
  /// still building its snapshot abandons it at its next progress point, a
  /// run in its install finishes it. Closing and dropping call this first.
  pub(crate) fn stop_checkpoint_thread(&self) {
    let thread = {
      let mut thread = self.checkpoint_thread.lock();
      self
        .checkpoint_thread_stopped
        .store(true, Ordering::Release);
      thread.take()
    };
    let Some(thread) = thread else {
      return;
    };
    self.checkpoint_abandoned.store(true, Ordering::Release);
    {
      let mut requests = thread.signal.requests.lock();
      requests.stop = true;
      thread.signal.wake.notify_all();
    }
    if thread.handle.join().is_err() {
      eprintln!("Warning: the checkpoint thread panicked");
    }
    self.checkpoint_abandoned.store(false, Ordering::Release);
    // Writers waiting for it to free WAL segment space stop waiting.
    self.notify_segment_waiters();
  }

  /// Whether a checkpoint thread runs for this database now.
  pub(crate) fn checkpoint_thread_running(&self) -> bool {
    self.checkpoint_thread.lock().is_some()
  }

  /// A checkpoint succeeded (any: automatic, a caller's, optimize's): the
  /// last automatic checkpoint's failure, if any, is stale, so its error
  /// clears, the back-off after it ends (waking the checkpoint thread from
  /// it), and the next failure waits the first back-off again. Called under
  /// the pager lock: the locks it takes are leaves there.
  pub(crate) fn end_checkpoint_backoff(&self) {
    *self.auto_checkpoint_failure.lock() = AutoCheckpointFailure::default();
    if let Some(thread) = self.checkpoint_thread.lock().as_ref() {
      let _requests = thread.signal.requests.lock();
      thread.signal.wake.notify_all();
    }
  }

  /// How long automatic checkpoints still back off after the last one
  /// failed, if they do.
  pub(crate) fn checkpoint_backoff_remaining(&self) -> Option<Duration> {
    self
      .auto_checkpoint_failure
      .lock()
      .retry_at
      .and_then(|at| at.checked_duration_since(Instant::now()))
      .filter(|left| !left.is_zero())
  }

  /// The error of the last automatic checkpoint (on the checkpoint thread
  /// or, without it, inline), if it failed and no checkpoint succeeded
  /// since. Automatic checkpoints report nothing to the commit that asked
  /// for them; this is where their failures show, besides the log, and in
  /// the writers that would wait for one once the WAL segments reach their
  /// limit (`CheckpointFailed`).
  pub fn checkpoint_error(&self) -> Option<String> {
    self.auto_checkpoint_failure.lock().error.clone()
  }

  /// Record an automatic checkpoint's outcome. A failure records its error
  /// and starts (or doubles) the back-off before the next automatic
  /// checkpoint, warns, and wakes the writers waiting for WAL segment space,
  /// who fail with it rather than wait. A success ends both
  /// (`end_checkpoint_backoff`). `CheckpointDeclined` is neither: the run
  /// did not happen, or stopped because the database is closing.
  pub(crate) fn record_checkpoint_result(&self, result: &crate::error::Result<()>) {
    match result {
      Ok(()) => self.end_checkpoint_backoff(),
      Err(KiteError::CheckpointDeclined(_)) => {}
      Err(error) => {
        eprintln!("Warning: automatic checkpoint failed: {error}");
        let (first, max) = backoff_bounds(self);
        {
          let mut failure = self.auto_checkpoint_failure.lock();
          let wait = failure.backoff.map_or(first, |wait| (wait * 2).min(max));
          failure.error = Some(error.to_string());
          failure.backoff = Some(wait);
          failure.retry_at = Some(Instant::now() + wait);
        }
        self.notify_segment_waiters();
      }
    }
  }
}

fn run_checkpoint_thread(db: SingleFileDB, signal: Arc<CheckpointSignal>) {
  ON_CHECKPOINT_THREAD.with(|on| on.set(true));
  loop {
    {
      let mut requests = signal.requests.lock();
      // After a failure: wait out the back-off, however often a commit asks
      // meanwhile. A close ends it early, and so does a checkpoint that
      // succeeds meanwhile (a caller's; `end_checkpoint_backoff` wakes this).
      while !requests.stop {
        match db.checkpoint_backoff_remaining() {
          Some(left) => {
            signal.wake.wait_for(&mut requests, left);
          }
          None => break,
        }
      }
      while !requests.checkpoint && !requests.stop {
        signal.wake.wait(&mut requests);
      }
      if requests.stop {
        return;
      }
      requests.checkpoint = false;
    }
    let asked = signal.requests.lock().asked;
    let result =
      match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| db.run_auto_checkpoint())) {
        Ok(result) => result,
        Err(panic) => {
          // The run's guards have returned the checkpoint status to idle,
          // but the panic may have struck between writes that keep memory
          // and disk in step (a header written, the pages it names not yet
          // marked; a spill's segment table and its WAL reset): the handle
          // refuses writes from now on. Recorded as a failure, and the
          // thread ends, answering every request made, and every one still
          // to come (`stop`; none can come once writes are refused, but one
          // may have checked just before).
          let message = panic
            .downcast_ref::<&str>()
            .map(|message| message.to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "no message".to_string());
          db.refuse_writes(format!("a checkpoint run panicked: {message}"));
          db.record_checkpoint_result(&Err(KiteError::Internal(format!(
            "the checkpoint thread's run panicked: {message}"
          ))));
          {
            let mut requests = signal.requests.lock();
            requests.answered = requests.asked;
            requests.stop = true;
            signal.wake.notify_all();
          }
          db.notify_segment_waiters();
          return;
        }
      };
    db.record_checkpoint_result(&result);
    {
      let mut requests = signal.requests.lock();
      requests.answered = requests.answered.max(asked);
      signal.wake.notify_all();
    }
    db.notify_segment_waiters();
  }
}
