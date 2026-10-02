//! Test seams in the pager's file I/O.
//!
//! Outside `cfg(test)` every hook is an empty inline function. Tests use them
//! to make the OS behave in ways POSIX allows but a local disk rarely shows
//! (a read returning fewer bytes than asked before EOF, as on NFS or after a
//! signal), to count the system calls page I/O costs, and to run code at a
//! point no barrier reaches (just before `create_pager` takes the file lock).
//! The state is per thread, so tests running in parallel never see each
//! other's.

use std::path::Path;

#[cfg(test)]
use std::cell::{Cell, RefCell};
#[cfg(test)]
use std::path::PathBuf;

/// Reads at file offsets from `from_offset` on return at most `max_len` bytes.
#[cfg(test)]
#[derive(Clone, Copy)]
struct ShortReads {
  from_offset: u64,
  max_len: usize,
}

#[cfg(test)]
type CreateHook = Box<dyn FnOnce(&Path)>;

#[cfg(test)]
thread_local! {
  static SYSCALLS: Cell<usize> = const { Cell::new(0) };
  static SHORT_READS: Cell<Option<ShortReads>> = const { Cell::new(None) };
  static BEFORE_CREATE_LOCK: RefCell<Option<CreateHook>> = const { RefCell::new(None) };
  static DIR_SYNCS: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
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

/// Run `run`, returning its result and the directories it fsynced on this
/// thread.
#[cfg(test)]
pub(crate) fn dir_syncs_during<R>(run: impl FnOnce() -> R) -> (R, Vec<PathBuf>) {
  DIR_SYNCS.with(|synced| synced.borrow_mut().clear());
  let result = run();
  (result, DIR_SYNCS.with(|synced| synced.take()))
}
