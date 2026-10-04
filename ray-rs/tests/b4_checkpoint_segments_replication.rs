//! raydb-b4 `checkpoint-segments`: replication with WAL segments (design:
//! `raydb-b4/_SEGMENTS_DESIGN.md`). A replica bootstraps from a primary
//! whose newest commits are in WAL segments (the WAL spilled; its own
//! records are a few or none), and the primary reopens on such a log without
//! taking it for a checkpointed one (which fences its sidecar for repair).

use std::path::Path;

use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions, SyncMode,
};
use kitedb::replication::types::ReplicationRole;
use kitedb::types::DbHeaderV1;

fn primary_options() -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .sync_mode(SyncMode::Full)
    .wal_size(64 * 1024)
    .auto_checkpoint(false)
    .replication_role(ReplicationRole::Primary)
}

fn key(index: usize) -> String {
  format!("node-{index}-{}", "n".repeat(200))
}

fn commit_nodes(db: &SingleFileDB, start: usize, count: usize) {
  for index in start..start + count {
    db.begin(false).expect("begin");
    db.create_node(Some(&key(index))).expect("node");
    db.commit_with_token().expect("commit");
  }
}

/// The number of WAL segments the newest header slot of the file names.
fn segments_on_disk(path: &Path) -> u32 {
  let bytes = std::fs::read(path).expect("read the file");
  (0..2)
    .filter_map(|slot| {
      let page = &bytes[slot * 4096..(slot + 1) * 4096];
      DbHeaderV1::parse(page).ok().map(|header| {
        (
          header.change_counter,
          u32::from_le_bytes(page[184..188].try_into().unwrap()),
        )
      })
    })
    .max()
    .expect("a valid header slot")
    .1
}

#[test]
fn replica_bootstraps_from_a_source_with_wal_segments() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("segments-primary.kitedb");
  let replica_path = dir.path().join("segments-replica.kitedb");
  let primary = open_single_file(&primary_path, primary_options()).expect("open primary");
  // About 120 KiB of WAL: two spills of the 48 KiB primary region.
  commit_nodes(&primary, 0, 400);
  assert!(
    segments_on_disk(&primary_path) > 0,
    "the primary's WAL never spilled"
  );

  let replica = open_single_file(
    &replica_path,
    SingleFileOpenOptions::new()
      .replication_role(ReplicationRole::Replica)
      .replication_source_db_path(&primary_path),
  )
  .expect("open replica");
  replica
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap from a source whose commits are in WAL segments");
  for index in [0, 199, 399] {
    assert!(replica.node_by_key(&key(index)).is_some(), "node {index}");
  }
  commit_nodes(&primary, 400, 5);
  loop {
    if replica.replica_catch_up_once(64).expect("catch up") == 0 {
      break;
    }
  }
  assert!(replica.node_by_key(&key(404)).is_some());

  // The primary reopens on a log whose newest commits are in segments: its
  // last committed transaction matches the sidecar's, so it is not fenced.
  close_single_file(primary).expect("close primary");
  let primary = open_single_file(&primary_path, primary_options()).expect("reopen primary");
  let status = primary
    .primary_replication_status()
    .expect("primary replication status");
  assert!(
    !status.sidecar_needs_repair,
    "the reopened primary fenced its sidecar: {:?}",
    status.last_replication_error
  );
  commit_nodes(&primary, 405, 1);
  loop {
    if replica.replica_catch_up_once(64).expect("catch up") == 0 {
      break;
    }
  }
  assert!(replica.node_by_key(&key(405)).is_some());
  close_single_file(replica).expect("close replica");
  close_single_file(primary).expect("close primary");
}
