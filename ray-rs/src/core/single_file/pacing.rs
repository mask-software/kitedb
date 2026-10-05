//! Soft backpressure: pacing writers while a background checkpoint runs.
//!
//! (Round 6: test hooks only so far.)

#[cfg(test)]
use std::time::Duration;

#[cfg(test)]
use parking_lot::Mutex;

/// The pacing state of a database (see `SingleFileDB::pace_writer`).
#[derive(Default)]
pub(crate) struct LogPacer {
  #[cfg(test)]
  test: Mutex<PacerTest>,
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
  db: &super::SingleFileDB,
  expected_run: Option<Duration>,
  max_delay: Option<Duration>,
) {
  let mut test = db.log_pacer.test.lock();
  test.expected_run = expected_run;
  test.max_delay = max_delay;
}

/// How much `db` paced its writers so far.
#[cfg(test)]
pub(crate) fn pacing_test_stats(db: &super::SingleFileDB) -> PacingTestStats {
  let test = db.log_pacer.test.lock();
  PacingTestStats {
    paced: test.paced,
    pacing_now: test.pacing_now,
  }
}
