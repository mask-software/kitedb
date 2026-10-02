//! How the replication sidecar makes its files durable.
//!
//! The sidecar follows the database's sync policy:
//!
//! - `SyncMode::Full`: each commit's frame is synced before the commit
//!   returns, and so is the manifest naming it.
//! - `SyncMode::Normal` and `SyncMode::Off`: frames are buffered and never
//!   synced per commit; a checkpoint or close syncs them, as the database
//!   file is synced then.
//!
//! In every mode a small metadata file (manifest, health, replica progress,
//! replica cursor) is replaced atomically with its content synced first, so
//! a crash can revert it but never leave it torn.
//!
//! A sync is a plain `fsync`, and `F_FULLFSYNC` (macOS) only with the
//! database's `full_fsync` opt-in in Full mode, as for the database file.
//! (`File::sync_all` is `F_FULLFSYNC` on macOS, a cost of milliseconds per
//! call that the sidecar paid on every commit.)

use crate::core::single_file::SyncMode;
use std::fs::File;
use std::io;
use std::path::Path;

#[cfg(test)]
thread_local! {
  static SYNC_LOG: std::cell::RefCell<Vec<&'static str>> =
    const { std::cell::RefCell::new(Vec::new()) };
}

/// The sync policy of a replication sidecar, from the database's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidecarSync {
  mode: SyncMode,
  full_fsync: bool,
}

impl Default for SidecarSync {
  /// Full mode with plain `fsync`: the database's defaults.
  fn default() -> Self {
    Self::new(SyncMode::Full, false)
  }
}

impl SidecarSync {
  /// `full_fsync` takes effect in `SyncMode::Full` only, as for the database.
  pub fn new(mode: SyncMode, full_fsync: bool) -> Self {
    Self {
      mode,
      full_fsync: full_fsync && mode == SyncMode::Full,
    }
  }

  pub fn mode(&self) -> SyncMode {
    self.mode
  }

  /// Whether each commit's frame is synced before the commit returns.
  pub fn durable_append(&self) -> bool {
    self.mode == SyncMode::Full
  }

  /// Make `file`'s written content durable.
  pub fn sync_file(&self, file: &File) -> io::Result<()> {
    sync_file(file, self.full_fsync)
  }

  /// Sync the directory holding `path`, so a file created in it or renamed
  /// into it survives a crash. Unix only, like `util::fs::sync_parent_dir`.
  pub fn sync_parent_dir(&self, path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
      let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
      sync_file(&File::open(parent)?, self.full_fsync)?;
    }

    #[cfg(not(unix))]
    let _ = path;

    Ok(())
  }
}

fn sync_file(file: &File, full_fsync: bool) -> io::Result<()> {
  #[cfg(target_os = "macos")]
  {
    use std::os::unix::io::AsRawFd;
    // F_FULLFSYNC fails on file systems without it (some network and FUSE
    // mounts); fall back to fsync there, as the pager and SQLite do.
    // SAFETY: the descriptor belongs to `file`, open for this call.
    if full_fsync && unsafe { libc::fcntl(file.as_raw_fd(), libc::F_FULLFSYNC) } == 0 {
      log_sync("F_FULLFSYNC");
      return Ok(());
    }
    log_sync("fsync");
    // SAFETY: as above.
    if unsafe { libc::fsync(file.as_raw_fd()) } != 0 {
      return Err(io::Error::last_os_error());
    }
    Ok(())
  }

  #[cfg(not(target_os = "macos"))]
  {
    let _ = full_fsync;
    log_sync("sync_all");
    file.sync_all()
  }
}

#[cfg_attr(not(test), allow(unused_variables))]
fn log_sync(primitive: &'static str) {
  #[cfg(test)]
  SYNC_LOG.with(|log| log.borrow_mut().push(primitive));
}

/// Run `run`, returning its result and the sync primitives the sidecar used
/// on this thread, oldest first.
#[cfg(test)]
pub(crate) fn sidecar_syncs_during<R>(run: impl FnOnce() -> R) -> (R, Vec<&'static str>) {
  SYNC_LOG.with(|log| log.borrow_mut().clear());
  let result = run();
  (result, SYNC_LOG.with(|log| log.take()))
}

#[cfg(test)]
mod tests {
  use super::{sidecar_syncs_during, SidecarSync};
  use crate::core::single_file::SyncMode;

  #[test]
  fn full_fsync_applies_to_full_mode_only() {
    assert!(!SidecarSync::new(SyncMode::Normal, true).full_fsync);
    assert!(!SidecarSync::new(SyncMode::Off, true).full_fsync);
    assert!(SidecarSync::new(SyncMode::Full, true).full_fsync);
    assert!(!SidecarSync::default().full_fsync);
    assert!(SidecarSync::default().durable_append());
    assert!(!SidecarSync::new(SyncMode::Normal, false).durable_append());
  }

  #[test]
  fn syncs_files_and_directories_with_the_policy_primitive() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("file");
    let file = std::fs::File::create(&path).expect("create");
    let ((), plain) = sidecar_syncs_during(|| {
      let sync = SidecarSync::default();
      sync.sync_file(&file).expect("sync file");
      sync.sync_parent_dir(&path).expect("sync dir");
    });
    let ((), full) = sidecar_syncs_during(|| {
      SidecarSync::new(SyncMode::Full, true)
        .sync_file(&file)
        .expect("sync file");
    });
    if cfg!(target_os = "macos") {
      assert_eq!(plain, vec!["fsync", "fsync"]);
      assert_eq!(full, vec!["F_FULLFSYNC"]);
    } else {
      assert_eq!(plain, vec!["sync_all", "sync_all"]);
      assert_eq!(full, vec!["sync_all"]);
    }
  }
}
