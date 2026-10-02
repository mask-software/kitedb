//! Test seams in the pager's file I/O.
//!
//! Outside `cfg(test)` every hook is an empty inline function. Tests use them
//! to make the OS behave in ways POSIX allows but a local disk rarely shows
//! (a read returning fewer bytes than asked before EOF, as on NFS or after a
//! signal), or a sync fail, to count the system calls page I/O costs, to log
//! the writes and syncs a crash image is built from, and to run code at a
//! point no barrier reaches (just before `create_pager` takes the file lock).
//! The state is per thread, so tests running in parallel never see each
//! other's.

use std::path::Path;

#[cfg(test)]
use std::cell::{Cell, RefCell};

/// Reads at file offsets from `from_offset` on return at most `max_len` bytes.
#[cfg(test)]
#[derive(Clone, Copy)]
struct ShortReads {
  from_offset: u64,
  max_len: usize,
}

#[cfg(test)]
type CreateHook = Box<dyn FnOnce(&Path)>;

/// A write to or sync of a pager's file, as [`record_io_during`] logs them.
#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) enum IoEvent {
  /// `data` was written at file `offset`.
  Write { offset: u64, data: Vec<u8> },
  /// A sync returned (`ok`: successfully).
  Sync { ok: bool },
}

#[cfg(test)]
thread_local! {
  static SYSCALLS: Cell<usize> = const { Cell::new(0) };
  static SHORT_READS: Cell<Option<ShortReads>> = const { Cell::new(None) };
  static BEFORE_CREATE_LOCK: RefCell<Option<CreateHook>> = const { RefCell::new(None) };
  static IO_LOG: RefCell<Option<Vec<IoEvent>>> = const { RefCell::new(None) };
  static SYNC_FAULTS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
fn log_io(event: impl FnOnce() -> IoEvent) {
  IO_LOG.with(|log| {
    if let Some(log) = log.borrow_mut().as_mut() {
      log.push(event());
    }
  });
}

/// Note one system call made for page I/O.
#[inline]
pub(super) fn syscall() {
  #[cfg(test)]
  SYSCALLS.with(|count| count.set(count.get() + 1));
}

/// The part of `buffer` one read at file `offset` may fill: all of it, unless
/// a test shortened reads there.
#[inline]
pub(super) fn read_window(offset: u64, buffer: &mut [u8]) -> &mut [u8] {
  #[cfg(test)]
  if let Some(short) = SHORT_READS.with(Cell::get) {
    if offset >= short.from_offset {
      let len = buffer.len().min(short.max_len);
      return &mut buffer[..len];
    }
  }
  let _ = offset;
  buffer
}

/// Note that `data` was written at file `offset`.
#[inline]
pub(super) fn wrote(offset: u64, data: &[u8]) {
  #[cfg(test)]
  log_io(|| IoEvent::Write {
    offset,
    data: data.to_vec(),
  });
  let _ = (offset, data);
}

/// Called before a sync reaches the OS: fails it if a test armed a fault.
#[inline]
pub(super) fn before_sync() -> std::io::Result<()> {
  #[cfg(test)]
  {
    let fail = SYNC_FAULTS.with(|faults| {
      let armed = faults.get();
      faults.set(armed.saturating_sub(1));
      armed > 0
    });
    if fail {
      synced(false);
      return Err(std::io::Error::other("injected sync failure"));
    }
  }
  Ok(())
}

/// Note that a sync returned, successfully or not.
#[inline]
pub(super) fn synced(ok: bool) {
  #[cfg(test)]
  log_io(|| IoEvent::Sync { ok });
  let _ = ok;
}

/// Called by `create_pager` before it takes the file lock.
#[inline]
pub(super) fn before_create_lock(path: &Path) {
  #[cfg(test)]
  if let Some(hook) = BEFORE_CREATE_LOCK.with(|hook| hook.borrow_mut().take()) {
    hook(path);
  }
  let _ = path;
}

/// Run `run`, returning its result and the page I/O system calls it made on
/// this thread.
#[cfg(test)]
pub(crate) fn syscalls_during<R>(run: impl FnOnce() -> R) -> (R, usize) {
  let before = SYSCALLS.with(Cell::get);
  let result = run();
  (result, SYSCALLS.with(Cell::get) - before)
}

/// Run `run` with every read at a file offset from `from_offset` on, on this
/// thread, returning at most `max_len` bytes.
#[cfg(test)]
pub(crate) fn with_short_reads<R>(from_offset: u64, max_len: usize, run: impl FnOnce() -> R) -> R {
  struct Disarm;
  impl Drop for Disarm {
    fn drop(&mut self) {
      SHORT_READS.with(|short| short.set(None));
    }
  }

  SHORT_READS.with(|short| {
    short.set(Some(ShortReads {
      from_offset,
      max_len,
    }))
  });
  let _disarm = Disarm;
  run()
}

/// Run `hook` with the path the next `create_pager` on this thread creates,
/// just before it takes the file lock.
#[cfg(test)]
pub(crate) fn before_next_create_lock(hook: impl FnOnce(&Path) + 'static) {
  BEFORE_CREATE_LOCK.with(|armed| *armed.borrow_mut() = Some(Box::new(hook)));
}

/// Run `run`, returning its result and the pager writes and syncs it made on
/// this thread, oldest first.
#[cfg(test)]
pub(crate) fn record_io_during<R>(run: impl FnOnce() -> R) -> (R, Vec<IoEvent>) {
  IO_LOG.with(|log| *log.borrow_mut() = Some(Vec::new()));
  let result = run();
  (result, IO_LOG.with(|log| log.take().unwrap_or_default()))
}

/// Run `run` with the next `count` pager syncs on this thread failing.
#[cfg(test)]
pub(crate) fn with_failing_syncs<R>(count: usize, run: impl FnOnce() -> R) -> R {
  struct Disarm;
  impl Drop for Disarm {
    fn drop(&mut self) {
      SYNC_FAULTS.with(|faults| faults.set(0));
    }
  }

  SYNC_FAULTS.with(|faults| faults.set(count));
  let _disarm = Disarm;
  run()
}
