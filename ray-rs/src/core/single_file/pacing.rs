//! Soft backpressure: pacing writers while a background checkpoint runs.
//!
//! Writers that outrun a running background checkpoint grow the log the
//! snapshot does not cover; at the WAL segment limit they stop until the
//! run installs (`wait_for_segment_space`), which on a large database is
//! seconds. Pacing slows them before that: once the log is past the
//! checkpoint trigger and a run is running, each commit, after it has
//! released every lock, waits long enough that the room left below the
//! limit lasts the run's expected remaining time (its last run's duration,
//! or twice its elapsed time before any run finished). The commits of every
//! writer share one schedule, so the log grows at that rate in all. A
//! commit waits at most `MAX_PACE`, and stops waiting as soon as the run
//! ends (installed or not) or the database closes. The limit stays the
//! backstop.
//!
//! Only automatic background checkpoints pace, on the checkpoint thread or
//! inline: a run then goes on beside commits. With blocking checkpoints
//! (`background_checkpoint` off) no run goes on beside a commit, and
//! read-only handles, reads, rollbacks, close and drop are never paced.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use super::SingleFileDB;

/// The longest a commit waits for pacing.
pub(crate) const MAX_PACE: Duration = Duration::from_millis(100);

/// The least run time to expect before any run finished.
const MIN_EXPECTED_RUN: Duration = Duration::from_millis(100);

/// The least remaining run time pacing plans for: a run past its expected
/// time is expected to end soon, not never.
const MIN_REMAINING: Duration = Duration::from_millis(10);

/// A wait shorter than this is not worth its wakeup: the time stays owed,
/// and the next commit waits it.
const MIN_WAIT: Duration = Duration::from_micros(50);

/// How far behind the schedule the next commit may start from: the time
/// writers spent committing since the last paced one counts toward its
/// wait, up to this (so a writer back from a pause does not burst).
const MAX_CREDIT: Duration = Duration::from_millis(10);

/// The pacing state of a database (see `SingleFileDB::pace_writer`).
#[derive(Default)]
pub(crate) struct LogPacer {
  state: Mutex<PacerState>,
  /// Background checkpoint runs ended so far: a pacing writer stops waiting
  /// when it changes.
  runs_ended: AtomicU64,
  #[cfg(test)]
  test: Mutex<PacerTest>,
}

#[derive(Default)]
struct PacerState {
  /// When the running background checkpoint run started, if one runs.
  run_started: Option<Instant>,
  /// How long the last run that installed took.
  last_run: Option<Duration>,
  /// The log's bytes when a commit was last charged in this run (`None`:
  /// none was yet).
  charged: Option<u64>,
  /// When the schedule lets the next commit go on.
  next_free: Option<Instant>,
}

/// Test overrides and counters (`set_pacing_test`, `pacing_test_stats`).
#[cfg(test)]
#[derive(Default)]
struct PacerTest {
  expected_run: Option<Duration>,
  max_delay: Option<Duration>,
  paced: u64,
  pacing_now: u64,
}

impl LogPacer {
  /// A background checkpoint run started.
  pub(crate) fn run_started(&self) {
    let mut state = self.state.lock();
    state.run_started = Some(Instant::now());
    state.charged = None;
    state.next_free = None;
  }

  /// The run ended, after `took`; `installed` if it installed its snapshot.
  /// Call before waking the writers (`notify_segment_waiters`).
  pub(crate) fn run_ended(&self, took: Duration, installed: bool) {
    {
      let mut state = self.state.lock();
      state.run_started = None;
      state.charged = None;
      state.next_free = None;
      if installed {
        state.last_run = Some(took);
      }
    }
    self.runs_ended.fetch_add(1, Ordering::AcqRel);
  }

  /// When a commit that grew the log to `logged` bytes (the uncovered
  /// segments and the WAL) may go on, with the trigger at `trigger` bytes
  /// and the segment limit at `limit`; `None` if at once. Charges the log's
  /// growth since the last commit charged to this one.
  fn schedule(&self, logged: u64, trigger: u64, limit: u64) -> Option<Instant> {
    #[cfg(test)]
    let (expected_run, max_delay) = {
      let test = self.test.lock();
      (test.expected_run, test.max_delay)
    };
    #[cfg(not(test))]
    let (expected_run, max_delay): (Option<Duration>, Option<Duration>) = (None, None);

    let mut state = self.state.lock();
    let started = state.run_started?;
    let charged = state.charged.replace(logged);
    if logged <= trigger {
      return None;
    }
    // The first commit charged in a run sets where its log starts.
    let grown = logged.saturating_sub(charged?);
    if grown == 0 {
      return None;
    }
    let elapsed = started.elapsed();
    let expected = expected_run
      .or(state.last_run)
      .unwrap_or_else(|| (elapsed * 2).max(MIN_EXPECTED_RUN));
    let remaining = expected
      .saturating_sub(elapsed)
      .max(expected / 4)
      .max(MIN_REMAINING);
    let bound = max_delay.unwrap_or(MAX_PACE);
    // The room left below the limit, spent evenly over the time left.
    let headroom = limit.saturating_sub(logged);
    let delay = if headroom == 0 {
      bound
    } else {
      remaining.mul_f64((grown as f64 / headroom as f64).min(1e6))
    };
    let now = Instant::now();
    let earliest = now.checked_sub(MAX_CREDIT).unwrap_or(now);
    let from = state.next_free.map_or(now, |free| free.max(earliest));
    let until = (from + delay).min(now + bound);
    state.next_free = Some(until);
    (until.saturating_duration_since(now) >= MIN_WAIT).then_some(until)
  }
}

impl SingleFileDB {
  /// Whether this database paces its writers (see the module docs).
  fn paces_writers(&self) -> bool {
    cfg!(not(target_arch = "wasm32"))
      && self.auto_checkpoint
      && self.background_checkpoint
      && !self.read_only
  }

  /// Soft backpressure, for a thread whose write transaction just committed
  /// and that holds no lock: while a background checkpoint runs and the log
  /// it does not cover is past the trigger, wait the commit's share of the
  /// time the room below the WAL segment limit must last (at most
  /// `MAX_PACE`; see the module docs). Stops waiting when the run ends or
  /// the database closes.
  pub(crate) fn pace_writer(&self) {
    if !self.paces_writers() {
      return;
    }
    let runs_ended = self.log_pacer.runs_ended.load(Ordering::Acquire);
    let log = self.header.log_state();
    let wal_bytes = self.wal_buffer.lock().used();
    let limit = self.wal_segment_limit_of(log.trigger, log.wal_size);
    let Some(until) =
      self
        .log_pacer
        .schedule(log.uncovered_segments + wal_bytes, log.trigger, limit)
    else {
      return;
    };
    #[cfg(test)]
    {
      let mut test = self.log_pacer.test.lock();
      test.paced += 1;
      test.pacing_now += 1;
    }
    // The condition variable's mutex is held only by its waits and wakes.
    let mut wait = self.segment_space_wait.lock();
    while Instant::now() < until
      && self.log_pacer.runs_ended.load(Ordering::Acquire) == runs_ended
      && !self.checkpoint_thread_stopped.load(Ordering::Acquire)
    {
      self.segment_space_cv.wait_until(&mut wait, until);
    }
    drop(wait);
    #[cfg(test)]
    {
      self.log_pacer.test.lock().pacing_now -= 1;
    }
  }
}

/// What `pacing_test_stats` reports.
#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PacingTestStats {
  /// Commits that were paced (waited).
  pub(crate) paced: u64,
  /// Writers pacing now.
  pub(crate) pacing_now: u64,
}

/// Make `db` expect a background checkpoint run to take `expected_run`
/// (instead of its last run's time), and pace a commit at most `max_delay`
/// (instead of `MAX_PACE`); `None` keeps the default.
#[cfg(test)]
pub(crate) fn set_pacing_test(
  db: &SingleFileDB,
  expected_run: Option<Duration>,
  max_delay: Option<Duration>,
) {
  let mut test = db.log_pacer.test.lock();
  test.expected_run = expected_run;
  test.max_delay = max_delay;
}

/// How much `db` paced its writers so far.
#[cfg(test)]
pub(crate) fn pacing_test_stats(db: &SingleFileDB) -> PacingTestStats {
  let test = db.log_pacer.test.lock();
  PacingTestStats {
    paced: test.paced,
    pacing_now: test.pacing_now,
  }
}
