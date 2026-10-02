//! File system helpers.

use std::fs::File;
use std::io;
use std::path::Path;

#[cfg(test)]
use std::cell::RefCell;
#[cfg(test)]
use std::path::PathBuf;

#[cfg(test)]
thread_local! {
  static DIR_SYNCS: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
}

/// Sync the directory that holds `path`, so a file created in it or renamed
/// into it survives a crash. A bare file name means the current directory.
///
/// Unix only: elsewhere this does nothing (Windows rejects `FlushFileBuffers`
/// on a directory handle opened for reading, and NTFS journals renames).
pub fn sync_parent_dir(path: &Path) -> io::Result<()> {
  #[cfg(unix)]
  {
    let parent = path
      .parent()
      .filter(|parent| !parent.as_os_str().is_empty())
      .unwrap_or_else(|| Path::new("."));
    std::fs::File::open(parent)?.sync_all()?;
    #[cfg(test)]
    DIR_SYNCS.with(|synced| synced.borrow_mut().push(parent.to_path_buf()));
  }

  #[cfg(not(unix))]
  let _ = path;

  Ok(())
}

// Advisory whole-file locks (flock on Unix, LockFileEx on Windows). WASI has
// no file locking, so on wasm32 they succeed without locking anything, as the
// database file lock in the pager does: a wasm32 build cannot keep two
// processes off the same files.

/// Block until this process holds an exclusive lock on `file`.
pub(crate) fn lock_exclusive(file: &File) -> io::Result<()> {
  #[cfg(not(target_arch = "wasm32"))]
  {
    fs2::FileExt::lock_exclusive(file)
  }
  #[cfg(target_arch = "wasm32")]
  {
    let _ = file;
    Ok(())
  }
}

/// Take an exclusive lock on `file`, or fail at once if another holds a lock.
pub(crate) fn try_lock_exclusive(file: &File) -> io::Result<()> {
  #[cfg(not(target_arch = "wasm32"))]
  {
    fs2::FileExt::try_lock_exclusive(file)
  }
  #[cfg(target_arch = "wasm32")]
  {
    let _ = file;
    Ok(())
  }
}

/// Release this process's lock on `file`.
pub(crate) fn unlock(file: &File) -> io::Result<()> {
  #[cfg(not(target_arch = "wasm32"))]
  {
    fs2::FileExt::unlock(file)
  }
  #[cfg(target_arch = "wasm32")]
  {
    let _ = file;
    Ok(())
  }
}

/// Run `run`, returning its result and the directories [`sync_parent_dir`]
/// synced on this thread, oldest first.
#[cfg(test)]
pub(crate) fn dir_syncs_during<R>(run: impl FnOnce() -> R) -> (R, Vec<PathBuf>) {
  DIR_SYNCS.with(|synced| synced.borrow_mut().clear());
  let result = run();
  (result, DIR_SYNCS.with(|synced| synced.take()))
}

#[cfg(test)]
mod tests {
  use super::sync_parent_dir;
  use std::path::Path;

  #[test]
  fn syncs_the_parent_of_nested_and_bare_paths() {
    let dir = tempfile::tempdir().expect("tempdir");
    sync_parent_dir(&dir.path().join("file.json")).expect("nested path");
    sync_parent_dir(Path::new("file.json")).expect("bare file name");
  }

  #[cfg(unix)]
  #[test]
  fn logs_the_directories_it_syncs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ((), synced) = super::dir_syncs_during(|| {
      sync_parent_dir(&dir.path().join("file.json")).expect("nested path");
      sync_parent_dir(Path::new("file.json")).expect("bare file name");
    });
    assert_eq!(
      synced,
      vec![dir.path().to_path_buf(), Path::new(".").into()]
    );
  }
}
