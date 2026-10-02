//! raydb-b4 `replication-core` lane: the snapshot transport copy and cursor
//! (P9), the sidecar generation in the transports (P2), bootstrap size (P4),
//! and batched replica apply (P6), through the public API.
//!
//! Each test fails until its finding is fixed, except the ones marked
//! "guard", which pin behavior a fix must keep.

use std::path::Path;
use std::str::FromStr;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions, SyncMode,
};
use kitedb::replication::manifest::ManifestStore;
use kitedb::replication::primary::default_replication_sidecar_path;
use kitedb::replication::types::{CommitToken, ReplicationCursor, ReplicationRole};
use kitedb::types::PropValue;

fn primary_options(sync_mode: SyncMode) -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .sync_mode(sync_mode)
    .auto_checkpoint(false)
    .replication_role(ReplicationRole::Primary)
}

fn open_primary(path: &Path) -> SingleFileDB {
  open_single_file(path, primary_options(SyncMode::Full)).expect("open primary")
}

fn replica_options(source_db_path: &Path) -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .replication_role(ReplicationRole::Replica)
    .replication_source_db_path(source_db_path)
}

fn commit_node(db: &SingleFileDB, key: &str) -> Option<CommitToken> {
  db.begin(false).expect("begin");
  db.create_node(Some(key)).expect("create node");
  db.commit_with_token().expect("commit")
}

fn catch_up_all(replica: &SingleFileDB) -> kitedb::Result<usize> {
  let mut total = 0usize;
  loop {
    let applied = replica.replica_catch_up_once(64)?;
    if applied == 0 {
      return Ok(total);
    }
    total += applied;
  }
}

fn snapshot_json(primary: &SingleFileDB, include_data: bool) -> serde_json::Value {
  serde_json::from_str(
    &primary
      .primary_export_snapshot_transport_json(include_data)
      .expect("snapshot transport"),
  )
  .expect("parse snapshot json")
}

fn log_json(primary: &SingleFileDB, cursor: Option<&str>) -> serde_json::Value {
  serde_json::from_str(
    &primary
      .primary_export_log_transport_json(cursor, 1024, 16 << 20, false)
      .expect("log transport"),
  )
  .expect("parse log json")
}

/// Open the snapshot's data as a database file and count its nodes.
fn snapshot_node_count(snapshot: &serde_json::Value, copy_path: &Path) -> u64 {
  let data = BASE64_STANDARD
    .decode(snapshot["data_base64"].as_str().expect("data_base64"))
    .expect("decode snapshot data");
  std::fs::write(copy_path, &data).expect("write snapshot copy");
  let copy = open_single_file(
    copy_path,
    SingleFileOpenOptions::new().auto_checkpoint(false),
  )
  .expect("open snapshot copy");
  let count = copy.count_nodes() as u64;
  close_single_file(copy).expect("close snapshot copy");
  count
}

fn manifest_generation(primary_path: &Path) -> u64 {
  ManifestStore::new(default_replication_sidecar_path(primary_path).join("manifest.json"))
    .read()
    .expect("read manifest")
    .generation
}

// ============================================================================
// P9: the snapshot copy must match its head; start_cursor resumes after it
// ============================================================================

/// In `SyncMode::Off` a commit stays in memory until a checkpoint or close,
/// so the file the export copied held none of the commits its head counts.
#[test]
fn b4_p9_snapshot_in_off_mode_holds_every_commit_up_to_its_head() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("p9-off-primary.kitedb");
  let primary = open_single_file(&primary_path, primary_options(SyncMode::Off)).expect("open");
  for i in 0..3 {
    commit_node(&primary, &format!("n{i}")).expect("token");
  }

  let snapshot = snapshot_json(&primary, true);
  let head = snapshot["head_log_index"].as_u64().expect("head_log_index");
  assert_eq!(head, 3, "setup: three frames");
  let copied = snapshot_node_count(&snapshot, &dir.path().join("p9-off-copy.kitedb"));
  assert_eq!(
    copied, head,
    "the snapshot of an Off-mode primary must hold the {head} commits its head counts; it holds \
     {copied}"
  );

  close_single_file(primary).expect("close primary");
}

/// With every frame in its own segment, the head frame sits in a sealed
/// segment and the active one is empty: a start cursor at segment 0 replays
/// it.
#[test]
fn b4_p9_start_cursor_skips_the_head_frame_after_a_segment_rotation() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("p9-rotate-primary.kitedb");
  let primary = open_single_file(
    &primary_path,
    primary_options(SyncMode::Full).replication_segment_max_bytes(1),
  )
  .expect("open primary");
  for i in 0..3 {
    commit_node(&primary, &format!("n{i}")).expect("token");
  }

  let snapshot = snapshot_json(&primary, false);
  let start_cursor = snapshot["start_cursor"]
    .as_str()
    .expect("start_cursor")
    .to_string();
  ReplicationCursor::from_str(&start_cursor).expect("parse start_cursor");
  let replayed = log_json(&primary, Some(&start_cursor))["frame_count"]
    .as_u64()
    .expect("frame_count");
  assert_eq!(
    replayed, 0,
    "a log pull from start_cursor {start_cursor} must replay none of the frames the snapshot holds"
  );

  // The next commit is the first frame after the snapshot.
  commit_node(&primary, "after").expect("token");
  let page = log_json(&primary, Some(&start_cursor));
  let frames = page["frames"].as_array().expect("frames");
  assert_eq!(frames.len(), 1, "exactly the new frame follows: {page}");
  assert_eq!(frames[0]["log_index"].as_u64(), Some(4));

  close_single_file(primary).expect("close primary");
}

// ============================================================================
// P2: HTTP-transport replicas need the sidecar generation
// ============================================================================

#[test]
fn b4_p2_log_transport_reports_the_sidecar_generation() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("p2-gen-log-primary.kitedb");
  let primary = open_primary(&primary_path);
  commit_node(&primary, "n0").expect("token");

  let expected = format!("{:016x}", manifest_generation(&primary_path));
  let page = log_json(&primary, None);
  assert_eq!(
    page["generation"].as_str(),
    Some(expected.as_str()),
    "the log page must carry the sidecar generation (as 16 hex digits, exact in JSON): {page}"
  );

  // A recreated sidecar starts a new history with a new generation.
  primary.checkpoint().expect("checkpoint");
  close_single_file(primary).expect("close primary");
  std::fs::remove_dir_all(default_replication_sidecar_path(&primary_path)).expect("delete sidecar");
  let primary = open_primary(&primary_path);
  commit_node(&primary, "n1").expect("token");
  let reset = log_json(&primary, None);
  assert_ne!(
    reset["generation"].as_str(),
    Some(expected.as_str()),
    "a recreated sidecar must report a new generation: {reset}"
  );

  close_single_file(primary).expect("close primary");
}

#[test]
fn b4_p2_snapshot_transport_reports_the_sidecar_generation() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("p2-gen-snapshot-primary.kitedb");
  let primary = open_primary(&primary_path);
  commit_node(&primary, "n0").expect("token");

  let expected = format!("{:016x}", manifest_generation(&primary_path));
  let snapshot = snapshot_json(&primary, false);
  assert_eq!(
    snapshot["generation"].as_str(),
    Some(expected.as_str()),
    "the snapshot must name the sidecar history its start_cursor belongs to: {snapshot}"
  );

  close_single_file(primary).expect("close primary");
}

// ============================================================================
// P4: bootstrap applied the whole graph as one transaction
// ============================================================================

/// One transaction cannot outgrow the replica's WAL: a source graph whose
/// copy writes more WAL records than the replica's WAL holds failed with
/// `WalBufferFull`. Batched commits (and checkpoints between them) fit.
#[test]
fn b4_p4_bootstrap_copies_a_graph_larger_than_the_replica_wal() {
  const NODES: usize = 6_000;
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("p4-wal-primary.kitedb");
  let replica_path = dir.path().join("p4-wal-replica.kitedb");

  let primary = open_single_file(
    &primary_path,
    primary_options(SyncMode::Full).wal_size(16 << 20),
  )
  .expect("open primary");
  primary.begin(false).expect("begin");
  let name = primary.define_propkey("name").expect("define name");
  let person = primary.define_label("Person").expect("define Person");
  let knows = primary.define_etype("KNOWS").expect("define KNOWS");
  let mut previous = None;
  for i in 0..NODES {
    let node = primary
      .create_node(Some(&format!("person-{i}")))
      .expect("create node");
    primary
      .set_node_prop(node, name, PropValue::String(format!("name of person {i}")))
      .expect("set name");
    primary.add_node_label(node, person).expect("add label");
    if let Some(previous) = previous {
      primary.add_edge(previous, knows, node).expect("add edge");
    }
    previous = Some(node);
  }
  primary.commit().expect("commit graph");

  let replica = open_single_file(
    &replica_path,
    replica_options(&primary_path).wal_size(256 << 10),
  )
  .expect("open replica");
  replica
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap a graph larger than the replica's 256 KiB WAL");
  assert_eq!(replica.count_nodes(), NODES);
  assert_eq!(replica.count_edges(), NODES - 1);
  let last = replica
    .node_by_key(&format!("person-{}", NODES - 1))
    .expect("last node copied");
  let replica_name = replica.propkey_id("name").expect("name on replica");
  assert_eq!(
    replica.node_prop(last, replica_name),
    Some(PropValue::String(format!("name of person {}", NODES - 1)))
  );

  // The cursor is set and catch-up continues from it.
  commit_node(&primary, "after-bootstrap").expect("token");
  catch_up_all(&replica).expect("catch up after bootstrap");
  assert!(replica.node_by_key("after-bootstrap").is_some());

  close_single_file(replica).expect("close replica");
  close_single_file(primary).expect("close primary");
}

// ============================================================================
// P6: batched replica apply
// ============================================================================

/// Guard: a pull whose frames fail partway applies the frames before the
/// failing one, moves the cursor to the last of them, and reports the failing
/// frame. A frame here fails on a raw label id the primary never named, which
/// names another label on the replica.
#[test]
fn b4_p6_catch_up_stops_at_a_failing_frame_and_keeps_the_frames_before_it() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("p6-fail-primary.kitedb");
  let replica_path = dir.path().join("p6-fail-replica.kitedb");

  // The replica names label id 1 before it follows the primary.
  {
    let local = open_single_file(&replica_path, SingleFileOpenOptions::new()).expect("open local");
    local.begin(false).expect("begin");
    let label = local.define_label("LocalOnly").expect("define label");
    local.commit().expect("commit");
    assert_eq!(label, 1, "setup: the replica's label id 1");
    close_single_file(local).expect("close local");
  }

  let primary = open_primary(&primary_path);
  let replica = open_single_file(&replica_path, replica_options(&primary_path)).expect("open");
  replica
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap empty primary");
  for i in 0..3 {
    commit_node(&primary, &format!("ok{i}")).expect("token");
  }
  primary.begin(false).expect("begin bad");
  let node = primary.create_node(Some("bad")).expect("create bad");
  primary.add_node_label(node, 1).expect("raw label id 1");
  primary.commit().expect("commit bad");

  let error = replica
    .replica_catch_up_once(64)
    .expect_err("the fourth frame must fail");
  assert!(
    error.to_string().contains("apply failed at 1:4"),
    "the error names the failing frame: {error}"
  );
  let status = replica.replica_replication_status().expect("status");
  assert_eq!(
    (status.applied_epoch, status.applied_log_index),
    (1, 3),
    "the cursor stops before the failing frame"
  );
  for i in 0..3 {
    assert!(
      replica.node_by_key(&format!("ok{i}")).is_some(),
      "frame {} applied",
      i + 1
    );
  }
  assert!(
    replica.node_by_key("bad").is_none(),
    "nothing of the failing frame applies"
  );

  close_single_file(replica).expect("close replica");
  close_single_file(primary).expect("close primary");
}
