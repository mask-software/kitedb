//! raydb-b4 `leftovers` lane: a second primary instance opened on a sidecar
//! that a live instance in this process holds. Found by
//! `examples/replication_soak_bench.rs`, whose stale-writer probe is such an
//! instance. Included from replication.rs for the publisher test hook.

use crate::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions, SyncMode,
};
use crate::replication::types::{CommitToken, ReplicationRole};
use std::path::Path;
use tempfile::tempdir;

fn open_normal_sync_primary(path: &Path, sidecar: &Path) -> SingleFileDB {
  open_single_file(
    path,
    SingleFileOpenOptions::new()
      .sync_mode(SyncMode::Normal)
      .auto_checkpoint(false)
      .replication_role(ReplicationRole::Primary)
      .replication_sidecar_path(sidecar),
  )
  .expect("open primary")
}

fn commit_node(db: &SingleFileDB, key: &str) -> Option<CommitToken> {
  db.begin(false).expect("begin");
  db.create_node(Some(key)).expect("create node");
  db.commit_with_token().expect("commit")
}

/// The `primary-unflushed` marker tells a primary that opens the sidecar that
/// its predecessor stopped with frames still in memory. Opening a second
/// instance while another instance in this process held the sidecar (and its
/// frames were still buffered) read the live instance's marker as that
/// crash: it fenced the sidecar for repair, so the live primary's next
/// commit got no replication token, and it deleted the marker the live
/// instance still needed.
///
/// Deterministic: the live instance's publisher is stopped, so its frames
/// stay buffered and the marker stays on disk.
#[test]
fn b4_leftovers_second_primary_on_a_live_sidecar_does_not_fence_it() {
  let dir = tempdir().expect("tempdir");
  let sidecar = dir.path().join("live.sidecar");
  let marker = sidecar.join("primary-unflushed");

  let mut live = open_normal_sync_primary(&dir.path().join("live.kitedb"), &sidecar);
  live
    .primary_replication
    .as_mut()
    .expect("primary replication")
    .stop_publisher_for_testing();
  assert!(
    commit_node(&live, "before").is_some(),
    "setup: the live primary replicates"
  );
  assert!(marker.exists(), "setup: the live primary buffers its frame");

  let second = open_normal_sync_primary(&dir.path().join("second.kitedb"), &sidecar);

  let marker_kept = marker.exists();
  let token = commit_node(&live, "after");
  let status = live
    .primary_replication_status()
    .expect("primary replication status");
  assert!(
    token.is_some() && !status.sidecar_needs_repair && marker_kept,
    "opening a second primary on a live sidecar must leave the live primary replicating: \
     token={token:?}, sidecar_needs_repair={}, last_replication_error={:?}, marker_kept={marker_kept}",
    status.sidecar_needs_repair,
    status.last_replication_error,
  );

  close_single_file(second).expect("close second");
  close_single_file(live).expect("close live");
}
