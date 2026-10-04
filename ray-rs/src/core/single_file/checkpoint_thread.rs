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
//! before running another (doubling from one second to a minute) instead of
//! failing in a loop.
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
use std::time::Duration;

use parking_lot::{Condvar, Mutex};

use crate::error::KiteError;

use super::SingleFileDB;

/// The name the checkpoint thread runs under.
pub(crate) const CHECKPOINT_THREAD_NAME: &str = "kitedb-checkpoint";

/// The first wait after a failed run; it doubles up to `MAX_BACKOFF`.
const FIRST_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);

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
  /// running. Returns whether the thread took the request (false once the
  /// database is closing, or if the thread cannot start).
  pub(crate) fn request_background_checkpoint(&self) -> bool {
    let mut thread = self.checkpoint_thread.lock();
    if self.checkpoint_thread_stopped.load(Ordering::Acquire) {
      return false;
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
          return false;
        }
      }
    }
    let signal = &thread.as_ref().expect("started above").signal;
    let mut requests = signal.requests.lock();
    requests.checkpoint = true;
    requests.asked += 1;
    signal.wake.notify_all();
    true
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

  /// The error of the last checkpoint the checkpoint thread ran, if it
  /// failed and no checkpoint installed since. Automatic checkpoints report
  /// nothing to the commit that asked for them; this is where their failures
  /// show, besides the log, and in the writers that would wait for one once
  /// the WAL segments reach their limit (`CheckpointFailed`).
  pub fn checkpoint_error(&self) -> Option<String> {
    self.checkpoint_last_error.lock().clone()
  }

  /// Record a failed run's error (an install clears it; see
  /// `install_snapshot`), and wake the writers waiting for WAL segment
  /// space, who fail with it rather than wait for a checkpoint.
  fn record_checkpoint_result(&self, result: &crate::error::Result<()>) {
    match result {
      // Not failures: nothing ran.
      Ok(()) | Err(KiteError::CheckpointDeclined(_)) => {}
      Err(error) => {
        eprintln!("Warning: background checkpoint failed: {error}");
        *self.checkpoint_last_error.lock() = Some(error.to_string());
        self.notify_segment_waiters();
      }
    }
  }
}

fn run_checkpoint_thread(db: SingleFileDB, signal: Arc<CheckpointSignal>) {
  ON_CHECKPOINT_THREAD.with(|on| on.set(true));
  let mut backoff = None::<Duration>;
  loop {
    {
      let mut requests = signal.requests.lock();
      if let Some(wait) = backoff {
        // After a failure: wait out the backoff (or the close) first.
        if !requests.stop {
          signal.wake.wait_for(&mut requests, wait);
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
    let result = db.run_auto_checkpoint();
    db.record_checkpoint_result(&result);
    {
      let mut requests = signal.requests.lock();
      requests.answered = requests.answered.max(asked);
      signal.wake.notify_all();
    }
    backoff = match (&result, backoff) {
      (Ok(()), _) | (Err(KiteError::CheckpointDeclined(_)), _) => None,
      (Err(_), None) => Some(FIRST_BACKOFF),
      (Err(_), Some(wait)) => Some((wait * 2).min(MAX_BACKOFF)),
    };
  }
}
