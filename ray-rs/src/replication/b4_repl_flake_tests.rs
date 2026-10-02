//! raydb-b4 `repl-flake` lane: closing a primary must release the sidecar's
//! `primary.lock` even while another descriptor for the same open file
//! description is alive.
//!
//! A child process spawned at any moment (`posix_spawn` or fork + exec)
//! starts with a copy of every descriptor of the parent and closes the
//! close-on-exec ones only when its exec completes. Until then it holds the
//! lock file's open file description, so a `flock` lock released only by
//! closing the parent's descriptor outlives the close, and a reopen in the
//! parent failed with "primary sidecar lock is held by another process".
//! `try_clone` (dup) shares the open file description the same way, which
//! makes the window deterministic. Included from primary.rs for the lock.

use crate::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions, SyncMode,
};
use crate::replication::types::ReplicationRole;
use std::fs::File;
use std::path::Path;
use tempfile::tempdir;

fn open_primary(db_path: &Path, sidecar: &Path) -> crate::Result<SingleFileDB> {
  open_single_file(
    db_path,
    SingleFileOpenOptions::new()
      .replication_role(ReplicationRole::Primary)
      .replication_sidecar_path(sidecar)
      .sync_mode(SyncMode::Normal),
  )
}

/// A second descriptor for the open file description holding the sidecar's
/// `primary.lock`: what a child process spawned now starts with.
fn inherited_lock_descriptor(db: &SingleFileDB) -> File {
  db.primary_replication
    .as_ref()
    .expect("primary replication")
    .inner
    ._sidecar_primary_lock
    .file
    .try_clone()
    .expect("duplicate primary.lock descriptor")
}

fn commit_node(db: &SingleFileDB, key: &str) -> u64 {
  db.begin(false).expect("begin");
  db.create_node(Some(key)).expect("create node");
  db.commit_with_token()
    .expect("commit")
    .expect("token")
    .log_index
}

#[test]
fn closed_primary_releases_sidecar_lock_held_by_inherited_descriptor() {
  let dir = tempdir().expect("tempdir");
  let db_path = dir.path().join("inherited-lock.kitedb");
  let sidecar = dir.path().join("inherited-lock.sidecar");

  let first = open_primary(&db_path, &sidecar).expect("open primary");
  let first_index = commit_node(&first, "first");
  let inherited = inherited_lock_descriptor(&first);
  close_single_file(first).expect("close first primary");

  let second = open_primary(&db_path, &sidecar)
    .expect("reopen primary while a spawned child still holds a copy of primary.lock");
  let second_index = commit_node(&second, "second");
  assert!(
    second_index > first_index,
    "log indexes must keep increasing across the reopen: first={first_index} second={second_index}"
  );
  close_single_file(second).expect("close second primary");
  drop(inherited);
}

#[test]
fn closed_primary_releases_sidecar_lock_for_other_processes() {
  // The descriptor a child inherited must not keep other openers out either:
  // a fresh open file description (another process's open) can lock it.
  let dir = tempdir().expect("tempdir");
  let db_path = dir.path().join("inherited-lock-other.kitedb");
  let sidecar = dir.path().join("inherited-lock-other.sidecar");

  let primary = open_primary(&db_path, &sidecar).expect("open primary");
  let inherited = inherited_lock_descriptor(&primary);
  close_single_file(primary).expect("close primary");

  let other = std::fs::OpenOptions::new()
    .read(true)
    .write(true)
    .open(sidecar.join(super::PRIMARY_LOCK_FILE_NAME))
    .expect("open primary.lock");
  fs2::FileExt::try_lock_exclusive(&other)
    .expect("primary.lock must be free once its primary closed");
  fs2::FileExt::unlock(&other).expect("unlock");
  drop(inherited);
}
