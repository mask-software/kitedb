//! raydb-b4 `repl-flake` lane: replica catch-up retries a source manifest
//! that is missing for a moment.
//!
//! This replaces `replication_phase_d::replica_catch_up_retries_transient_source_manifest_errors`,
//! which restored the hidden manifest from a thread after a 40 ms sleep and
//! failed whenever that thread woke after catch-up's whole retry budget
//! (about 150 ms of backoff): 2 of 80 runs of the test binary with 8 copies in
//! parallel, 22 of 96 with 16. Here the retry hook restores it, so the first
//! attempt always fails and the retry always finds it. Included from
//! replication.rs for the hook.

use super::BEFORE_NEXT_CATCH_UP_RETRY;
use crate::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions, SyncMode,
};
use crate::replication::types::ReplicationRole;
use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;
use tempfile::tempdir;

fn open_primary(path: &Path, sidecar: &Path) -> SingleFileDB {
  open_single_file(
    path,
    SingleFileOpenOptions::new()
      .sync_mode(SyncMode::Full)
      .replication_role(ReplicationRole::Primary)
      .replication_sidecar_path(sidecar)
      .replication_segment_max_bytes(128)
      .replication_retention_min_entries(8),
  )
  .expect("open primary")
}

fn open_replica(path: &Path, sidecar: &Path, source: &Path, source_sidecar: &Path) -> SingleFileDB {
  open_single_file(
    path,
    SingleFileOpenOptions::new()
      .replication_role(ReplicationRole::Replica)
      .replication_sidecar_path(sidecar)
      .replication_source_db_path(source)
      .replication_source_sidecar_path(source_sidecar),
  )
  .expect("open replica")
}

fn commit_node(db: &SingleFileDB, key: &str) {
  db.begin(false).expect("begin");
  db.create_node(Some(key)).expect("create node");
  db.commit_with_token().expect("commit").expect("token");
}

#[test]
fn replica_catch_up_retries_transient_source_manifest_errors() {
  let dir = tempdir().expect("tempdir");
  let primary_path = dir.path().join("retry-primary.kitedb");
  let primary_sidecar = dir.path().join("retry-primary.sidecar");
  let replica_path = dir.path().join("retry-replica.kitedb");
  let replica_sidecar = dir.path().join("retry-replica.sidecar");

  let primary = open_primary(&primary_path, &primary_sidecar);
  commit_node(&primary, "seed");
  let replica = open_replica(
    &replica_path,
    &replica_sidecar,
    &primary_path,
    &primary_sidecar,
  );
  replica
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap snapshot");
  commit_node(&primary, "backlog");

  let manifest_path = primary_sidecar.join("manifest.json");
  let hidden_path = primary_sidecar.join("manifest.json.hidden");
  std::fs::rename(&manifest_path, &hidden_path).expect("hide manifest");
  let retried = Rc::new(Cell::new(false));
  BEFORE_NEXT_CATCH_UP_RETRY.with(|hook| {
    let retried = Rc::clone(&retried);
    let (hidden_path, manifest_path) = (hidden_path.clone(), manifest_path.clone());
    *hook.borrow_mut() = Some(Box::new(move || {
      retried.set(true);
      std::fs::rename(&hidden_path, &manifest_path).expect("restore manifest");
    }));
  });

  let applied = replica
    .replica_catch_up_once(64)
    .expect("replica catch-up should retry transient manifest read failures");
  BEFORE_NEXT_CATCH_UP_RETRY.with(|hook| hook.borrow_mut().take());
  assert!(
    retried.get(),
    "the first catch-up attempt must fail on the hidden manifest and retry"
  );
  assert!(applied > 0, "the retry should apply the backlog frame");
  assert!(replica.node_by_key("backlog").is_some());
  let status = replica
    .replica_replication_status()
    .expect("replica status");
  assert!(!status.needs_reseed);
  assert!(status.last_error.is_none(), "{:?}", status.last_error);

  close_single_file(replica).expect("close replica");
  close_single_file(primary).expect("close primary");
}
