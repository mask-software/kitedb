//! File system helpers.

use std::io;
use std::path::Path;

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
  }

  #[cfg(not(unix))]
  let _ = path;

  Ok(())
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
}
