//! Page-based I/O abstraction for single-file database format
//!
//! Provides page-level read/write, mmap support, and area management.
//! Ported from src/core/pager.ts

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::util::mmap::{map_file, Mmap};

use crate::constants::{
  LOCK_BYTE_OFFSET, LOCK_BYTE_RANGE, MAX_PAGE_SIZE, MIN_PAGE_SIZE, OS_PAGE_SIZE,
};
use crate::error::{KiteError, Result};

#[cfg(test)]
thread_local! {
  /// Test probe: the OS primitive each `FilePager` sync on this thread issued,
  /// oldest first. "fsync" is plain fsync(2), which on macOS leaves data in
  /// the drive's volatile cache; "F_FULLFSYNC" is fcntl(F_FULLFSYNC) (macOS),
  /// and "sync_all" is `File::sync_all` (other platforms). New sync
  /// primitives should log here too.
  pub(crate) static SYNC_PRIMITIVE_LOG: std::cell::RefCell<Vec<&'static str>> =
    const { std::cell::RefCell::new(Vec::new()) };
}
pub(crate) mod io_hooks;

static DATABASE_FILE_LOCKS: OnceLock<Mutex<HashMap<PathBuf, InProcessLockState>>> = OnceLock::new();
const WRITABLE_OPEN_MAX_ATTEMPTS: usize = 4;

#[derive(Clone, Copy)]
enum FileLockMode {
  Shared,
  Exclusive,
}

#[derive(Default)]
struct InProcessLockState {
  readers: usize,
  writer: bool,
}

struct DatabaseFileLock {
  path: PathBuf,
  mode: FileLockMode,
}

impl DatabaseFileLock {
  fn acquire(path: &Path, mode: FileLockMode) -> Result<Self> {
    let path = normalize_lock_path(path)?;
    let registry = DATABASE_FILE_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut registry = registry
      .lock()
      .map_err(|_| KiteError::LockFailed("database file lock registry poisoned".to_string()))?;
    let state = registry.entry(path.clone()).or_default();

    let conflict = match mode {
      FileLockMode::Shared => state.writer,
      FileLockMode::Exclusive => state.writer || state.readers != 0,
    };
    if conflict {
      return Err(KiteError::LockFailed(format!(
        "database file is already open in this process: {}",
        path.display()
      )));
    }

    match mode {
      FileLockMode::Shared => state.readers += 1,
      FileLockMode::Exclusive => state.writer = true,
    }

    Ok(Self { path, mode })
  }
}

impl Drop for DatabaseFileLock {
  fn drop(&mut self) {
    let Some(registry) = DATABASE_FILE_LOCKS.get() else {
      return;
    };
    let Ok(mut registry) = registry.lock() else {
      return;
    };
    let Some(state) = registry.get_mut(&self.path) else {
      return;
    };

    match self.mode {
      FileLockMode::Shared => state.readers = state.readers.saturating_sub(1),
      FileLockMode::Exclusive => state.writer = false,
    }
    if state.readers == 0 && !state.writer {
      registry.remove(&self.path);
    }
  }
}

// ============================================================================
// Positioned I/O
// ============================================================================

/// One positioned read; it may return fewer bytes than asked.
#[cfg(unix)]
fn read_at(file: &File, buffer: &mut [u8], offset: u64) -> std::io::Result<usize> {
  std::os::unix::fs::FileExt::read_at(file, buffer, offset)
}

/// One positioned read; it may return fewer bytes than asked. It also moves
/// the file cursor, which page I/O never relies on.
#[cfg(windows)]
fn read_at(file: &File, buffer: &mut [u8], offset: u64) -> std::io::Result<usize> {
  std::os::windows::fs::FileExt::seek_read(file, buffer, offset)
}

#[cfg(not(any(unix, windows)))]
fn read_at(mut file: &File, buffer: &mut [u8], offset: u64) -> std::io::Result<usize> {
  use std::io::{Read, Seek, SeekFrom};
  file.seek(SeekFrom::Start(offset))?;
  file.read(buffer)
}

/// One positioned write; it may write fewer bytes than given.
#[cfg(unix)]
fn write_at(file: &File, data: &[u8], offset: u64) -> std::io::Result<usize> {
  std::os::unix::fs::FileExt::write_at(file, data, offset)
}

/// One positioned write; it may write fewer bytes than given. It also moves
/// the file cursor, which page I/O never relies on.
#[cfg(windows)]
fn write_at(file: &File, data: &[u8], offset: u64) -> std::io::Result<usize> {
  std::os::windows::fs::FileExt::seek_write(file, data, offset)
}

#[cfg(not(any(unix, windows)))]
fn write_at(mut file: &File, data: &[u8], offset: u64) -> std::io::Result<usize> {
  use std::io::{Seek, SeekFrom, Write};
  file.seek(SeekFrom::Start(offset))?;
  file.write(data)
}

/// Fill `buffer` from file `offset` until it is full or the file ends, and
/// return the bytes read; the rest of `buffer` is left as it was. A read may
/// return fewer bytes than asked before the end of the file (POSIX allows it:
/// NFS, signals), so this reads again until a read returns none.
fn read_full_at(file: &File, buffer: &mut [u8], offset: u64) -> std::io::Result<usize> {
  let mut filled = 0;
  while filled < buffer.len() {
    let at = offset + filled as u64;
    io_hooks::syscall();
    match read_at(file, io_hooks::read_window(at, &mut buffer[filled..]), at) {
      Ok(0) => break,
      Ok(read) => filled += read,
      Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
      Err(error) => return Err(error),
    }
  }
  Ok(filled)
}

/// Write all of `data` at file `offset`, writing again after a short write.
fn write_all_at(file: &File, mut data: &[u8], mut offset: u64) -> std::io::Result<()> {
  while !data.is_empty() {
    io_hooks::syscall();
    match write_at(file, data, offset) {
      Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
      Ok(written) => {
        data = &data[written..];
        offset += written as u64;
      }
      Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
      Err(error) => return Err(error),
    }
  }
  Ok(())
}

fn normalize_lock_path(path: &Path) -> Result<PathBuf> {
  if path.exists() {
    return Ok(std::fs::canonicalize(path)?);
  }

  let parent = path
    .parent()
    .filter(|parent| !parent.as_os_str().is_empty());
  let parent = std::fs::canonicalize(parent.unwrap_or_else(|| Path::new(".")))?;
  let file_name = path.file_name().ok_or_else(|| {
    KiteError::InvalidPath(format!(
      "database path has no file name: {}",
      path.display()
    ))
  })?;
  Ok(parent.join(file_name))
}

fn try_lock_file(file: &File, path: &Path, mode: FileLockMode) -> Result<()> {
  #[cfg(not(target_arch = "wasm32"))]
  {
    let result = match mode {
      FileLockMode::Shared => fs2::FileExt::try_lock_shared(file),
      FileLockMode::Exclusive => fs2::FileExt::try_lock_exclusive(file),
    };
    result.map_err(|error| {
      KiteError::LockFailed(format!(
        "database file is locked by another process: {} ({error})",
        path.display()
      ))
    })?;
  }

  #[cfg(target_arch = "wasm32")]
  let _ = (file, path, mode);

  Ok(())
}

#[cfg(unix)]
fn locked_file_matches_path(file: &File, path: &Path) -> Result<bool> {
  use std::os::unix::fs::MetadataExt;

  let locked = file.metadata()?;
  let current = std::fs::metadata(path)?;
  Ok(locked.dev() == current.dev() && locked.ino() == current.ino())
}

#[cfg(windows)]
fn locked_file_matches_path(file: &File, path: &Path) -> Result<bool> {
  use std::os::windows::fs::MetadataExt;

  let locked = file.metadata()?;
  let current = std::fs::metadata(path)?;
  Ok(
    match (
      locked.volume_serial_number(),
      locked.file_index(),
      current.volume_serial_number(),
      current.file_index(),
    ) {
      (Some(locked_volume), Some(locked_index), Some(current_volume), Some(current_index)) => {
        locked_volume == current_volume && locked_index == current_index
      }
      _ => false,
    },
  )
}

#[cfg(not(any(unix, windows)))]
fn locked_file_matches_path(_file: &File, _path: &Path) -> Result<bool> {
  // Stable std APIs expose no portable file identity on this platform. Keep
  // its existing lock behavior rather than introducing unsafe platform code.
  Ok(true)
}

/// FilePager implementation for single-file database
pub struct FilePager {
  file: File,
  file_lock: Option<DatabaseFileLock>,
  file_path: PathBuf,
  page_size: usize,
  file_size: u64,
  read_only: bool,
  free_pages: HashSet<u32>,
  /// Pages of a snapshot whose header install failed. A header slot may still
  /// name them, so they join `free_pages` only when a later install is
  /// durable in both slots (see `release_deferred_free_pages`).
  deferred_free_pages: HashSet<u32>,
  /// Cached mmap for the entire file (lazily created)
  mmap: Option<Mmap>,
  /// Make [`Self::sync`] flush the drive's write cache too (`F_FULLFSYNC` on
  /// macOS); see [`Self::set_full_fsync`].
  full_fsync: bool,
}

impl FilePager {
  /// Create a new FilePager from an open file
  pub fn new(file: File, file_path: PathBuf, page_size: usize) -> Result<Self> {
    Self::new_locked(file, None, file_path, page_size, false)
  }

  fn new_locked(
    file: File,
    file_lock: Option<DatabaseFileLock>,
    file_path: PathBuf,
    page_size: usize,
    read_only: bool,
  ) -> Result<Self> {
    let file_size = file.metadata()?.len();
    Ok(Self {
      file,
      file_lock,
      file_path,
      page_size,
      file_size,
      read_only,
      free_pages: HashSet::new(),
      deferred_free_pages: HashSet::new(),
      mmap: None,
      full_fsync: false,
    })
  }

  /// Create a new FilePager with explicit file size (for new files)
  pub fn with_size(file: File, file_path: PathBuf, page_size: usize, file_size: u64) -> Self {
    Self {
      file,
      file_lock: None,
      file_path,
      page_size,
      file_size,
      read_only: false,
      free_pages: HashSet::new(),
      deferred_free_pages: HashSet::new(),
      mmap: None,
      full_fsync: false,
    }
  }

  /// Get the file path
  pub fn file_path(&self) -> &Path {
    &self.file_path
  }

  /// Get the page size
  pub fn page_size(&self) -> usize {
    self.page_size
  }

  /// Get the current file size
  pub fn file_size(&self) -> u64 {
    self.file_size
  }

  /// Calculate the page number range for the lock byte region
  fn lock_byte_page_range(&self) -> (u32, u32) {
    let start = (LOCK_BYTE_OFFSET / self.page_size as u64) as u32;
    let end = (LOCK_BYTE_OFFSET + LOCK_BYTE_RANGE as u64).div_ceil(self.page_size as u64) as u32;
    (start, end)
  }

  /// Check if a page number overlaps with the lock byte range
  fn is_lock_byte_page(&self, page_num: u32) -> bool {
    let (start, end) = self.lock_byte_page_range();
    page_num >= start && page_num < end
  }

  /// Read a single page by page number
  pub fn read_page(&mut self, page_num: u32) -> Result<Vec<u8>> {
    let offset = page_num as u64 * self.page_size as u64;

    // Safety check: don't read beyond file size
    if offset >= self.file_size {
      return Ok(vec![0u8; self.page_size]);
    }

    // Bytes past the end of the file read as zeros.
    let mut buffer = vec![0u8; self.page_size];
    read_full_at(&self.file, &mut buffer, offset)?;
    Ok(buffer)
  }

  /// Write a single page by page number
  pub fn write_page(&mut self, page_num: u32, data: &[u8]) -> Result<()> {
    if self.read_only {
      return Err(KiteError::ReadOnly);
    }
    if data.len() != self.page_size {
      return Err(KiteError::Internal(format!(
        "Page data must be exactly {} bytes, got {}",
        self.page_size,
        data.len()
      )));
    }

    // Safety check: don't write to lock byte range
    if self.is_lock_byte_page(page_num) {
      return Err(KiteError::Internal(format!(
        "Cannot write to lock byte page range (page {page_num})"
      )));
    }
    self.ensure_no_live_mmap()?;

    let offset = page_num as u64 * self.page_size as u64;

    // Extend file if necessary
    let required_size = offset + self.page_size as u64;
    if required_size > self.file_size {
      self.file.set_len(required_size)?;
      self.file_size = required_size;
    }

    write_all_at(&self.file, data, offset)?;
    Ok(())
  }

  /// Memory-map the entire file for read-only pager tests and tooling.
  ///
  /// The pager rejects all writes, extension, relocation, and truncation while
  /// this mapping is cached. Call `release_mmap` before mutating the file.
  pub fn mmap_file(&mut self) -> Result<&Mmap> {
    if let Some(mmap) = self.mmap.as_ref() {
      let file_len = self.file.metadata()?.len() as usize;
      if file_len != mmap.len() {
        // Drop stale mapping and remap with the current file size.
        self.mmap = None;
      }
    }

    if self.mmap.is_none() {
      // SAFETY: FilePager rejects every mutation while this mapping is live.
      let mmap = map_file(&self.file)?;
      self.mmap = Some(mmap);
    }
    self
      .mmap
      .as_ref()
      .ok_or_else(|| KiteError::Internal("mmap not initialized after mapping".to_string()))
  }

  /// Get a slice of the mmap'd file for a page range
  pub fn mmap_range(&mut self, start_page: u32, page_count: u32) -> Result<&[u8]> {
    let start_offset = start_page as usize * self.page_size;
    let length = page_count as usize * self.page_size;

    // Validate mmap alignment
    if !start_offset.is_multiple_of(OS_PAGE_SIZE) {
      return Err(KiteError::Internal(format!(
        "mmap offset {start_offset} must be aligned to OS page size {OS_PAGE_SIZE}"
      )));
    }

    let mmap = self.mmap_file()?;

    // Check bounds
    if start_offset + length > mmap.len() {
      return Err(KiteError::Internal(format!(
        "mmap range {}..{} exceeds file size {}",
        start_offset,
        start_offset + length,
        mmap.len()
      )));
    }

    Ok(&mmap[start_offset..start_offset + length])
  }

  /// Release the cached full-file mapping after all borrowed slices are gone.
  pub fn release_mmap(&mut self) {
    self.mmap = None;
  }

  /// Allocate new pages at end of file
  /// Returns the starting page number of the allocated range
  pub fn allocate_pages(&mut self, count: u32) -> Result<u32> {
    if self.read_only {
      return Err(KiteError::ReadOnly);
    }
    if count == 0 {
      return Err(KiteError::Internal(
        "Must allocate at least 1 page".to_string(),
      ));
    }
    self.ensure_no_live_mmap()?;

    // Calculate current page count
    let current_page_count = self.file_size.div_ceil(self.page_size as u64) as u32;
    let mut start_page = current_page_count;

    // Check if we need to skip the lock byte range
    let (lock_start, lock_end) = self.lock_byte_page_range();

    // If the new allocation would overlap with lock byte range, skip past it
    if start_page < lock_end && start_page + count > lock_start {
      // Move start past the lock byte range
      start_page = lock_end;
    }

    // Extend file
    let new_size = (start_page + count) as u64 * self.page_size as u64;
    self.file.set_len(new_size)?;
    self.file_size = new_size;

    Ok(start_page)
  }

  /// Mark pages as free. The next checkpoint may write its snapshot over them,
  /// so only pages no valid header slot can name belong here: never the
  /// header, the WAL, or the installed snapshot.
  pub fn free_pages(&mut self, start_page: u32, count: u32) {
    for i in 0..count {
      self.free_pages.insert(start_page + i);
    }
  }

  /// Find a contiguous free range without consuming it.
  pub(crate) fn find_free_range(&self, count: u32) -> Option<u32> {
    if count == 0 {
      return None;
    }
    let mut pages: Vec<u32> = self.free_pages.iter().copied().collect();
    pages.sort_unstable();

    let mut run_start = None;
    let mut previous = 0u32;
    let mut run_len = 0u32;
    for page in pages {
      if run_start.is_some() && page == previous.saturating_add(1) {
        run_len += 1;
      } else {
        run_start = Some(page);
        run_len = 1;
      }
      if run_len >= count {
        return run_start;
      }
      previous = page;
    }
    None
  }

  /// Consume a range previously returned by `find_free_range`.
  pub(crate) fn consume_free_range(&mut self, start_page: u32, count: u32) {
    for page in start_page..start_page.saturating_add(count) {
      self.free_pages.remove(&page);
    }
  }

  pub(crate) fn is_range_free(&self, start_page: u32, end_page: u32) -> bool {
    (start_page..end_page).all(|page| self.free_pages.contains(&page))
  }

  /// Hold pages back from reuse until the next `release_deferred_free_pages`.
  pub(crate) fn defer_free_pages(&mut self, start_page: u32, count: u32) {
    self
      .deferred_free_pages
      .extend(start_page..start_page.saturating_add(count));
  }

  /// Make every deferred page free. Call only once no valid header slot can
  /// name them, i.e. after a newer snapshot is durable in both slots.
  pub(crate) fn release_deferred_free_pages(&mut self) {
    self.free_pages.extend(self.deferred_free_pages.drain());
  }

  /// Remove pages `start_page..end_page` from the free and deferred lists, so
  /// they are never reused, and return how many were listed. For pages that
  /// hold, or are about to hold, data a header names.
  pub(crate) fn withdraw_free_pages(&mut self, start_page: u32, end_page: u32) -> usize {
    let range = start_page..end_page;
    let listed = self.free_pages.len() + self.deferred_free_pages.len();
    self.free_pages.retain(|page| !range.contains(page));
    self
      .deferred_free_pages
      .retain(|page| !range.contains(page));
    listed - self.free_pages.len() - self.deferred_free_pages.len()
  }

  /// Pages waiting in `defer_free_pages`, sorted
  #[cfg(test)]
  pub(crate) fn deferred_free_page_list(&self) -> Vec<u32> {
    let mut pages: Vec<u32> = self.deferred_free_pages.iter().copied().collect();
    pages.sort_unstable();
    pages
  }

  /// Free pages, sorted
  #[cfg(test)]
  pub(crate) fn free_page_list(&self) -> Vec<u32> {
    let mut pages: Vec<u32> = self.free_pages.iter().copied().collect();
    pages.sort_unstable();
    pages
  }

  /// Get count of free pages
  pub fn free_page_count(&self) -> usize {
    self.free_pages.len()
  }

  /// Truncate file to the given number of pages
  pub fn truncate_pages(&mut self, page_count: u32) -> Result<()> {
    if self.read_only {
      return Err(KiteError::ReadOnly);
    }
    self.ensure_no_live_mmap()?;
    let new_size = page_count as u64 * self.page_size as u64;
    self.file.set_len(new_size)?;
    self.file_size = new_size;
    self.free_pages.retain(|page| *page < page_count);
    self.deferred_free_pages.retain(|page| *page < page_count);
    Ok(())
  }

  /// Make every [`Self::sync`] durable against power loss, not just against
  /// a process or OS crash (`SingleFileOpenOptions::full_fsync` with
  /// `SyncMode::Full`).
  ///
  /// On macOS, fsync(2) hands data to the drive, whose volatile write cache
  /// can still lose it or persist it out of order (a header before the WAL
  /// or snapshot pages it names); `F_FULLFSYNC` flushes that cache too, at a
  /// cost of milliseconds per sync. Elsewhere `File::sync_all` is used either
  /// way.
  pub fn set_full_fsync(&mut self, full_fsync: bool) {
    self.full_fsync = full_fsync;
  }

  /// Sync file to disk
  pub fn sync(&self) -> Result<()> {
    if self.read_only {
      return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
      use std::os::unix::io::AsRawFd;
      // F_FULLFSYNC fails on file systems without it (some network and FUSE
      // mounts); fall back to fsync there, as SQLite does.
      // SAFETY: file descriptor is valid for the pager file.
      if self.full_fsync && unsafe { libc::fcntl(self.file.as_raw_fd(), libc::F_FULLFSYNC) } == 0 {
        #[cfg(test)]
        SYNC_PRIMITIVE_LOG.with(|log| log.borrow_mut().push("F_FULLFSYNC"));
        return Ok(());
      }
      #[cfg(test)]
      SYNC_PRIMITIVE_LOG.with(|log| log.borrow_mut().push("fsync"));
      // SAFETY: file descriptor is valid for the pager file.
      let result = unsafe { libc::fsync(self.file.as_raw_fd()) };
      if result != 0 {
        return Err(std::io::Error::last_os_error().into());
      }
    }

    #[cfg(not(target_os = "macos"))]
    {
      #[cfg(test)]
      SYNC_PRIMITIVE_LOG.with(|log| log.borrow_mut().push("sync_all"));
      self.file.sync_all()?;
    }
    Ok(())
  }

  /// Relocate an area to a new location (for growth/compaction)
  /// This is an expensive operation that copies data page by page
  pub fn relocate_area(&mut self, src_page: u32, page_count: u32, dst_page: u32) -> Result<()> {
    if self.read_only {
      return Err(KiteError::ReadOnly);
    }
    self.ensure_no_live_mmap()?;
    if src_page == dst_page {
      return Ok(());
    }

    // Validate destination doesn't overlap with lock byte range
    let (lock_start, lock_end) = self.lock_byte_page_range();
    if dst_page < lock_end && dst_page + page_count > lock_start {
      return Err(KiteError::Internal(
        "Cannot relocate to lock byte range".to_string(),
      ));
    }

    // Copy the pages furthest into the overlap first, so no source page is
    // overwritten before it is read.
    let page_size = self.page_size as u64;
    let copy_forward = src_page < dst_page;
    let mut buffer = vec![0u8; self.page_size];
    for step in 0..page_count {
      let i = if copy_forward {
        page_count - 1 - step
      } else {
        step
      };
      let src_offset = (src_page + i) as u64 * page_size;
      let dst_offset = (dst_page + i) as u64 * page_size;
      if read_full_at(&self.file, &mut buffer, src_offset)? < buffer.len() {
        return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
      }

      // Extend file if needed
      let required_size = dst_offset + page_size;
      if required_size > self.file_size {
        self.file.set_len(required_size)?;
        self.file_size = required_size;
      }
      write_all_at(&self.file, &buffer, dst_offset)?;
    }

    // Sync to ensure data is durable before marking old pages as free
    self.sync()?;

    // Free the old pages, except those the copy now occupies.
    let copy = dst_page..dst_page + page_count;
    self
      .free_pages
      .extend((src_page..src_page + page_count).filter(|page| !copy.contains(page)));

    Ok(())
  }

  fn ensure_no_live_mmap(&self) -> Result<()> {
    if self.mmap.is_some() {
      return Err(KiteError::Internal(
        "cannot mutate file while a pager mmap is live; call release_mmap after readers drop"
          .to_string(),
      ));
    }
    Ok(())
  }

  /// Get a reference to the underlying file
  pub fn file(&self) -> &File {
    &self.file
  }

  /// Get a mutable reference to the underlying file
  pub fn file_mut(&mut self) -> &mut File {
    &mut self.file
  }
}

impl Drop for FilePager {
  fn drop(&mut self) {
    #[cfg(not(target_arch = "wasm32"))]
    if self.file_lock.is_some() {
      let _ = fs2::FileExt::unlock(&self.file);
    }
  }
}

// ============================================================================
// Factory functions
// ============================================================================

/// Open a pager for an existing file and hold its process lock for the pager lifetime.
pub fn open_pager<P: AsRef<Path>>(
  file_path: P,
  page_size: usize,
  read_only: bool,
) -> Result<FilePager> {
  open_pager_with_locking(file_path, page_size, read_only, true)
}

pub(crate) fn open_pager_with_locking<P: AsRef<Path>>(
  file_path: P,
  page_size: usize,
  read_only: bool,
  lock_file: bool,
) -> Result<FilePager> {
  let file_path = file_path.as_ref();
  let mode = if read_only {
    FileLockMode::Shared
  } else {
    FileLockMode::Exclusive
  };
  let attempts = if lock_file && !read_only {
    WRITABLE_OPEN_MAX_ATTEMPTS
  } else {
    1
  };
  for attempt in 0..attempts {
    let file_lock = if lock_file {
      Some(DatabaseFileLock::acquire(file_path, mode)?)
    } else {
      None
    };
    let file = OpenOptions::new()
      .read(true)
      .write(!read_only)
      .open(file_path)?;
    if lock_file {
      try_lock_file(&file, file_path, mode)?;
    }
    // A concurrent legacy migration can rename a replacement over the path
    // between open and lock. Never trust the now-unlinked, stale descriptor.
    if lock_file && !read_only && !locked_file_matches_path(&file, file_path)? {
      drop(file);
      drop(file_lock);
      if attempt + 1 == attempts {
        break;
      }
      continue;
    }
    return FilePager::new_locked(
      file,
      file_lock,
      file_path.to_path_buf(),
      page_size,
      read_only,
    );
  }
  Err(KiteError::LockFailed(format!(
    "database path changed while acquiring its lock after {attempts} attempts: {}",
    file_path.display()
  )))
}

/// Create a new pager for a new file
pub fn create_pager<P: AsRef<Path>>(file_path: P, page_size: usize) -> Result<FilePager> {
  create_pager_with_locking(file_path, page_size, true)
}

pub(crate) fn create_pager_with_locking<P: AsRef<Path>>(
  file_path: P,
  page_size: usize,
  lock_file: bool,
) -> Result<FilePager> {
  let file_path = file_path.as_ref();
  io_hooks::before_create_lock(file_path);
  let attempts = if lock_file {
    WRITABLE_OPEN_MAX_ATTEMPTS
  } else {
    1
  };
  for attempt in 0..attempts {
    let file_lock = if lock_file {
      Some(DatabaseFileLock::acquire(
        file_path,
        FileLockMode::Exclusive,
      )?)
    } else {
      None
    };
    let file = OpenOptions::new()
      .read(true)
      .write(true)
      .create(true)
      .truncate(false)
      .open(file_path)?;
    if lock_file {
      try_lock_file(&file, file_path, FileLockMode::Exclusive)?;
    }
    if lock_file && !locked_file_matches_path(&file, file_path)? {
      drop(file);
      drop(file_lock);
      if attempt + 1 == attempts {
        break;
      }
      continue;
    }
    file.set_len(0)?;
    return Ok(FilePager {
      file,
      file_lock,
      file_path: file_path.to_path_buf(),
      page_size,
      file_size: 0,
      read_only: false,
      free_pages: HashSet::new(),
      deferred_free_pages: HashSet::new(),
      mmap: None,
      full_fsync: false,
    });
  }
  Err(KiteError::LockFailed(format!(
    "database path changed while acquiring its lock after {attempts} attempts: {}",
    file_path.display()
  )))
}

/// Validate that a page size is valid (power of 2, within bounds)
pub fn is_valid_page_size(page_size: usize) -> bool {
  if !(MIN_PAGE_SIZE..=MAX_PAGE_SIZE).contains(&page_size) {
    return false;
  }
  // Check power of 2
  (page_size & (page_size - 1)) == 0
}

/// Calculate the number of pages needed to store a given byte count
pub fn pages_to_store(byte_count: usize, page_size: usize) -> u32 {
  byte_count.div_ceil(page_size) as u32
}

#[cfg(test)]
mod tests {
  use super::*;
  use tempfile::NamedTempFile;

  #[test]
  fn test_is_valid_page_size() {
    assert!(is_valid_page_size(4096));
    assert!(is_valid_page_size(8192));
    assert!(is_valid_page_size(16384));
    assert!(is_valid_page_size(32768));
    assert!(is_valid_page_size(65536));

    // Invalid: too small
    assert!(!is_valid_page_size(2048));
    // Invalid: too large
    assert!(!is_valid_page_size(131072));
    // Invalid: not power of 2
    assert!(!is_valid_page_size(5000));
    assert!(!is_valid_page_size(6000));
  }

  #[test]
  fn test_pages_to_store() {
    assert_eq!(pages_to_store(0, 4096), 0);
    assert_eq!(pages_to_store(1, 4096), 1);
    assert_eq!(pages_to_store(4096, 4096), 1);
    assert_eq!(pages_to_store(4097, 4096), 2);
    assert_eq!(pages_to_store(8192, 4096), 2);
    assert_eq!(pages_to_store(10000, 4096), 3);
  }

  #[test]
  fn test_read_write_page() {
    let temp_file = NamedTempFile::new().expect("expected value");
    let mut pager = create_pager(temp_file.path(), 4096).expect("expected value");

    // Write a page
    let data: Vec<u8> = (0..4096).map(|i| (i % 256) as u8).collect();
    pager.write_page(0, &data).expect("expected value");

    // Read it back
    let read_data = pager.read_page(0).expect("expected value");
    assert_eq!(read_data, data);
  }

  #[test]
  fn test_read_empty_page() {
    let temp_file = NamedTempFile::new().expect("expected value");
    let mut pager = create_pager(temp_file.path(), 4096).expect("expected value");

    // Reading beyond file size should return zeros
    let read_data = pager.read_page(100).expect("expected value");
    assert_eq!(read_data, vec![0u8; 4096]);
  }

  #[test]
  fn test_allocate_pages() {
    let temp_file = NamedTempFile::new().expect("expected value");
    let mut pager = create_pager(temp_file.path(), 4096).expect("expected value");

    // Allocate first batch
    let start1 = pager.allocate_pages(5).expect("expected value");
    assert_eq!(start1, 0);
    assert_eq!(pager.file_size(), 5 * 4096);

    // Allocate second batch
    let start2 = pager.allocate_pages(3).expect("expected value");
    assert_eq!(start2, 5);
    assert_eq!(pager.file_size(), 8 * 4096);
  }

  #[test]
  fn test_free_pages() {
    let temp_file = NamedTempFile::new().expect("expected value");
    let mut pager = create_pager(temp_file.path(), 4096).expect("expected value");

    // Allocate and free some pages
    pager.allocate_pages(10).expect("expected value");
    pager.free_pages(2, 3);

    assert_eq!(pager.free_page_count(), 3);
  }

  #[test]
  fn relocate_area_frees_only_pages_the_copy_left() {
    let temp_file = NamedTempFile::new().expect("expected value");
    let mut pager = create_pager(temp_file.path(), 4096).expect("expected value");
    pager.allocate_pages(10).expect("expected value");

    // Pages 4..8 move down to 2..6, so pages 4 and 5 hold part of the copy.
    pager.relocate_area(4, 4, 2).expect("expected value");
    assert_eq!(pager.free_page_list(), vec![6, 7]);
  }

  #[test]
  fn test_mmap_file() {
    let temp_file = NamedTempFile::new().expect("expected value");
    let mut pager = create_pager(temp_file.path(), 4096).expect("expected value");

    // Write some data first
    let data: Vec<u8> = (0..4096).map(|i| (i % 256) as u8).collect();
    pager.write_page(0, &data).expect("expected value");
    pager.sync().expect("expected value");

    // Now mmap and verify
    let mmap = pager.mmap_file().expect("expected value");
    assert_eq!(&mmap[..4096], &data[..]);
  }

  #[test]
  fn test_mmap_rejects_mutation_until_released() {
    let temp_file = NamedTempFile::new().expect("expected value");
    let mut pager = create_pager(temp_file.path(), 4096).expect("expected value");
    pager
      .write_page(0, &vec![0x11; 4096])
      .expect("expected value");
    pager.sync().expect("expected value");

    {
      let mmap = pager.mmap_file().expect("expected value");
      assert_eq!(mmap[0], 0x11);
    }
    assert!(pager.write_page(1, &vec![0x22; 4096]).is_err());
    pager.release_mmap();
    pager
      .write_page(1, &vec![0x22; 4096])
      .expect("expected value");
  }

  #[test]
  fn test_write_extends_file() {
    let temp_file = NamedTempFile::new().expect("expected value");
    let mut pager = create_pager(temp_file.path(), 4096).expect("expected value");

    assert_eq!(pager.file_size(), 0);

    // Writing to page 5 should extend the file
    let data = vec![0xAB; 4096];
    pager.write_page(5, &data).expect("expected value");

    assert_eq!(pager.file_size(), 6 * 4096);
  }

  #[test]
  fn test_page_size_validation() {
    let temp_file = NamedTempFile::new().expect("expected value");
    let mut pager = create_pager(temp_file.path(), 4096).expect("expected value");

    // Wrong size data should fail
    let small_data = vec![0u8; 100];
    assert!(pager.write_page(0, &small_data).is_err());

    let large_data = vec![0u8; 8192];
    assert!(pager.write_page(0, &large_data).is_err());
  }

  #[cfg(unix)]
  #[test]
  fn locked_file_identity_detects_rename_over_path() {
    let temp_dir = tempfile::tempdir().expect("expected temp directory");
    let database_path = temp_dir.path().join("database.kite");
    let replacement_path = temp_dir.path().join("replacement.kite");
    std::fs::write(&database_path, b"original").expect("expected original file");
    std::fs::write(&replacement_path, b"replacement").expect("expected replacement file");

    let stale_file = File::open(&database_path).expect("expected stale file descriptor");
    std::fs::rename(&replacement_path, &database_path).expect("expected atomic replacement");

    assert!(
      !locked_file_matches_path(&stale_file, &database_path).expect("expected identity check")
    );
    let current_file = File::open(&database_path).expect("expected current file descriptor");
    assert!(
      locked_file_matches_path(&current_file, &database_path).expect("expected identity check")
    );
  }
}

/// Wave-2 `wal-format` W7: `SyncMode::Full` with `full_fsync` must survive
/// power loss. Plain fsync(2) on macOS only hands data to the drive, whose
/// volatile cache can lose it or persist it out of order (the header page
/// before the WAL page it names, or before the snapshot it points to).
/// `full_fsync` is opt-in (off by default, like SQLite's `fullfsync`).
#[cfg(test)]
mod w2_tests {
  use super::SYNC_PRIMITIVE_LOG;
  use crate::core::single_file::{open_single_file, SingleFileOpenOptions, SyncMode};

  /// The sync primitives `run` issued on this thread.
  fn syncs_during(run: impl FnOnce()) -> Vec<&'static str> {
    SYNC_PRIMITIVE_LOG.with(|log| log.borrow_mut().clear());
    run();
    SYNC_PRIMITIVE_LOG.with(|log| log.borrow_mut().drain(..).collect())
  }

  fn assert_drive_cache_flushed(what: &str, syncs: &[&str]) {
    assert!(!syncs.is_empty(), "{what}: issued no sync at all");
    assert!(
      syncs.iter().all(|primitive| *primitive != "fsync"),
      "{what}: used plain fsync, which on macOS leaves the data in the drive cache (power \
       loss can drop it, or persist a header before the pages it names): {syncs:?}"
    );
  }

  #[test]
  fn w7_full_sync_mode_commit_flushes_the_drive_cache() {
    let dir = tempfile::tempdir().expect("tempdir");
    let options = SingleFileOpenOptions::new()
      .auto_checkpoint(false)
      .sync_mode(SyncMode::Full)
      .full_fsync(true);
    let db = open_single_file(dir.path().join("w7-commit.kitedb"), options).expect("open");
    let syncs = syncs_during(|| {
      db.begin(false).expect("begin");
      db.create_node(Some("n")).expect("node");
      db.commit().expect("commit");
    });
    assert_drive_cache_flushed("SyncMode::Full commit", &syncs);
  }

  #[test]
  fn w7_full_sync_mode_checkpoint_flushes_the_drive_cache() {
    let dir = tempfile::tempdir().expect("tempdir");
    let options = SingleFileOpenOptions::new()
      .auto_checkpoint(false)
      .sync_mode(SyncMode::Full)
      .full_fsync(true);
    let db = open_single_file(dir.path().join("w7-checkpoint.kitedb"), options).expect("open");
    db.begin(false).expect("begin");
    db.create_node(Some("n")).expect("node");
    db.commit().expect("commit");
    let syncs = syncs_during(|| db.checkpoint().expect("checkpoint"));
    assert_drive_cache_flushed("SyncMode::Full checkpoint", &syncs);
  }

  /// Without the option, Full mode keeps the plain sync primitive (fast, not
  /// power-loss durable on macOS), and the option never affects Normal mode.
  #[test]
  fn w7_full_fsync_is_opt_in_and_only_for_full_mode() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cases = [
      ("default Full", SyncMode::Full, false),
      ("Normal with full_fsync", SyncMode::Normal, true),
    ];
    for (what, mode, full_fsync) in cases {
      let options = SingleFileOpenOptions::new()
        .auto_checkpoint(false)
        .sync_mode(mode)
        .full_fsync(full_fsync);
      let path = dir.path().join(format!("w7-{mode:?}.kitedb"));
      let db = open_single_file(path, options).expect("open");
      let syncs = syncs_during(|| {
        db.begin(false).expect("begin");
        db.create_node(Some("n")).expect("node");
        db.commit().expect("commit");
        db.checkpoint().expect("checkpoint");
      });
      assert!(!syncs.is_empty(), "{what}: issued no sync at all");
      assert!(
        !syncs.contains(&"F_FULLFSYNC"),
        "{what}: used F_FULLFSYNC: {syncs:?}"
      );
    }
  }
}
