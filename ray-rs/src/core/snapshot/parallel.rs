//! Spreading snapshot load work over threads.
//!
//! Loading a snapshot checks its CRC, then inflates and checks every
//! section. The CRC splits into chunks and the sections are independent, so
//! a large snapshot spreads them over a few scoped threads, the caller being
//! one of them. A small one stays on the caller: starting a thread costs tens
//! of microseconds, as much as inflating a few hundred KiB.

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

/// Most threads one load uses, the caller included. The largest section
/// bounds the speedup well before this.
const MAX_THREADS: usize = 8;

/// Threads this process can run at once, capped at `MAX_THREADS`. Cached:
/// on Linux, `available_parallelism` reads cgroup files.
fn available_threads() -> usize {
  static THREADS: OnceLock<usize> = OnceLock::new();
  *THREADS.get_or_init(|| {
    std::thread::available_parallelism()
      .map_or(1, NonZeroUsize::get)
      .min(MAX_THREADS)
  })
}

#[cfg(test)]
thread_local! {
  /// Thread count every load on this thread uses, regardless of its size.
  static FORCED_THREADS: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// Runs `f` with every load on this thread split over exactly `threads`
/// threads (as far as it has tasks), so tests reach the multi-threaded path
/// with small snapshots and on single-core machines.
#[cfg(test)]
pub(crate) fn with_forced_threads<R>(threads: usize, f: impl FnOnce() -> R) -> R {
  let previous = FORCED_THREADS.with(|forced| forced.replace(Some(threads)));
  let result = f();
  FORCED_THREADS.with(|forced| forced.set(previous));
  result
}

/// Threads for `work` units of work in `tasks` tasks: one per
/// `min_work_per_thread` units, no more than there are tasks or cores.
pub(super) fn threads_for(work: usize, min_work_per_thread: usize, tasks: usize) -> usize {
  #[cfg(test)]
  if let Some(threads) = FORCED_THREADS.with(std::cell::Cell::get) {
    return threads.clamp(1, tasks.max(1));
  }
  (work / min_work_per_thread.max(1))
    .min(tasks)
    .min(available_threads())
    .max(1)
}

/// Runs `task` for every index in `0..count` on up to `threads` threads, the
/// caller included, and returns the results in index order. Threads take the
/// next index as they finish one, so list long tasks first. If a thread
/// cannot be started, the others run its share.
pub(super) fn run<T, F>(count: usize, threads: usize, task: F) -> Vec<T>
where
  T: Send,
  F: Fn(usize) -> T + Sync,
{
  if threads <= 1 || count <= 1 {
    return (0..count).map(task).collect();
  }

  let next = AtomicUsize::new(0);
  let work = || {
    let mut done = Vec::new();
    loop {
      let index = next.fetch_add(1, Ordering::Relaxed);
      if index >= count {
        return done;
      }
      done.push((index, task(index)));
    }
  };
  let work = &work;

  let mut results = std::thread::scope(|scope| {
    let helpers: Vec<_> = (1..threads.min(count))
      .filter_map(|_| {
        std::thread::Builder::new()
          .name("kitedb-snapshot-load".to_string())
          .spawn_scoped(scope, work)
          .ok()
      })
      .collect();
    let mut results = work();
    for helper in helpers {
      match helper.join() {
        Ok(done) => results.extend(done),
        // A panicking task panics the load, as it would on one thread.
        Err(panic) => std::panic::resume_unwind(panic),
      }
    }
    results
  });
  // The caller takes indexes until none are left, so every index ran once.
  results.sort_unstable_by_key(|(index, _)| *index);
  results.into_iter().map(|(_, result)| result).collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn run_returns_results_in_index_order() {
    for threads in [1, 2, 3, 8] {
      let results = run(50, threads, |index| index * 3);
      assert_eq!(results, (0..50).map(|index| index * 3).collect::<Vec<_>>());
    }
    assert!(run(0, 4, |index| index).is_empty());
  }

  #[test]
  fn run_uses_other_threads() {
    let caller = std::thread::current().id();
    let barrier = std::sync::Barrier::new(2);
    // Two tasks that wait for each other finish only if they run at once.
    let threads = run(2, 2, |_| {
      barrier.wait();
      std::thread::current().id()
    });
    assert!(threads.contains(&caller));
    assert_ne!(threads[0], threads[1]);
  }

  #[test]
  #[should_panic(expected = "task 3 failed")]
  fn run_propagates_task_panics() {
    run(8, 4, |index| {
      if index == 3 {
        panic!("task {index} failed");
      }
    });
  }

  #[test]
  fn threads_follow_work_tasks_and_cores() {
    let cores = available_threads();
    assert_eq!(threads_for(0, 100, 10), 1);
    assert_eq!(threads_for(99, 100, 10), 1);
    assert_eq!(threads_for(250, 100, 10), 2.min(cores));
    assert_eq!(threads_for(10_000, 100, 3), 3.min(cores));
    assert_eq!(threads_for(usize::MAX, 1, usize::MAX), cores);
    assert_eq!(with_forced_threads(4, || threads_for(0, 100, 10)), 4);
    assert_eq!(with_forced_threads(4, || threads_for(0, 100, 2)), 2);
  }
}
