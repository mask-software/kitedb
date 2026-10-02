//! Wave-3 repros for the replication lane: pagination cursors (P1), replica
//! progress and sidecar resets (P2), OTLP push (P5, P7), epoch fencing (P8),
//! the snapshot transport (P9), and export labels, metrics schema counts and
//! point-in-time export (P10).
//!
//! Every test here fails until its finding is fixed. The OTLP tests use a
//! local port with no listener, so they need no network.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions, SyncMode,
};
use kitedb::export::{
  export_to_json, export_to_object_single, import_from_json, import_from_object_single,
  ExportOptions, ImportOptions,
};
use kitedb::metrics::collect_metrics_single_file;
use kitedb::replication::primary::default_replication_sidecar_path;
use kitedb::replication::progress::{load_replica_progress, remove_replica_progress};
use kitedb::replication::types::{CommitToken, ReplicationCursor, ReplicationRole};
use kitedb::streaming::{edges_page_single, nodes_page_single, PaginationOptions};
use kitedb::types::{Edge, NodeId, PropValue};

fn open_db(path: &Path) -> SingleFileDB {
  open_single_file(path, SingleFileOpenOptions::new().auto_checkpoint(false)).expect("open db")
}

fn primary_options() -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .sync_mode(SyncMode::Full)
    .auto_checkpoint(false)
    .replication_role(ReplicationRole::Primary)
}

fn open_primary(path: &Path) -> SingleFileDB {
  open_single_file(path, primary_options()).expect("open primary")
}

fn open_replica(path: &Path, source_db_path: &Path) -> SingleFileDB {
  open_single_file(
    path,
    SingleFileOpenOptions::new()
      .replication_role(ReplicationRole::Replica)
      .replication_source_db_path(source_db_path),
  )
  .expect("open replica")
}

/// Commit one transaction that creates a node with `key`.
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

fn edge_key(edge: &Edge) -> (NodeId, u32, NodeId) {
  (edge.src, edge.etype, edge.dst)
}

/// Every page after `cursor`, concatenated.
fn edges_after(db: &SingleFileDB, mut cursor: Option<String>, limit: usize) -> Vec<Edge> {
  let mut edges = Vec::new();
  for _ in 0..10_000 {
    let page = edges_page_single(db, PaginationOptions { limit, cursor });
    edges.extend(page.items);
    if !page.has_more {
      return edges;
    }
    cursor = page.next_cursor;
  }
  panic!("edge pagination did not terminate");
}

// ============================================================================
// P1: pagination cursors drop or repeat results when data changes
// ============================================================================

#[test]
fn w3_p1_nodes_page_continues_after_cursor_node_is_deleted() {
  let dir = tempfile::tempdir().expect("tempdir");
  let db = open_db(&dir.path().join("p1-nodes.kitedb"));

  db.begin(false).expect("begin");
  let nodes: Vec<NodeId> = (0..10)
    .map(|i| db.create_node(Some(&format!("n{i}"))).expect("create node"))
    .collect();
  db.commit().expect("commit");

  let first = nodes_page_single(
    &db,
    PaginationOptions {
      limit: 3,
      cursor: None,
    },
  );
  assert_eq!(first.items, nodes[..3], "setup: first page");
  let cursor = first.next_cursor.expect("first page cursor");

  // The node the cursor points at is deleted before the next page.
  db.begin(false).expect("begin delete");
  db.delete_node(nodes[2]).expect("delete cursor node");
  db.commit().expect("commit delete");

  let second = nodes_page_single(
    &db,
    PaginationOptions {
      limit: 3,
      cursor: Some(cursor.clone()),
    },
  );
  assert_eq!(
    second.items,
    nodes[3..6],
    "page after cursor {cursor} must continue with the next ids once the cursor's node is deleted \
     (got has_more={}, next_cursor={:?})",
    second.has_more,
    second.next_cursor
  );
  assert!(
    second.has_more,
    "four more nodes remain after the second page"
  );

  close_single_file(db).expect("close");
}

#[test]
fn w3_p1_edges_page_continues_after_cursor_edge_is_deleted() {
  let dir = tempfile::tempdir().expect("tempdir");
  let db = open_db(&dir.path().join("p1-edges-delete.kitedb"));

  db.begin(false).expect("begin");
  let link = db.define_etype("LINK").expect("define LINK");
  let nodes: Vec<NodeId> = (0..11)
    .map(|i| db.create_node(Some(&format!("n{i}"))).expect("create node"))
    .collect();
  for pair in nodes.windows(2) {
    db.add_edge(pair[0], link, pair[1]).expect("add edge");
  }
  db.commit().expect("commit");
  let all: BTreeSet<_> = db
    .list_edges(None)
    .iter()
    .map(|edge| (edge.src, edge.etype, edge.dst))
    .collect();
  assert_eq!(all.len(), 10, "setup: ten edges");

  let first = edges_page_single(
    &db,
    PaginationOptions {
      limit: 3,
      cursor: None,
    },
  );
  assert_eq!(first.items.len(), 3, "setup: first page");
  let cursor_edge = edge_key(first.items.last().expect("cursor edge"));

  db.begin(false).expect("begin delete");
  db.delete_edge(cursor_edge.0, cursor_edge.1, cursor_edge.2)
    .expect("delete cursor edge");
  db.commit().expect("commit delete");

  let first_page: BTreeSet<_> = first.items.iter().map(edge_key).collect();
  let expected: BTreeSet<_> = all.difference(&first_page).copied().collect();
  let rest: Vec<_> = edges_after(&db, first.next_cursor, 3)
    .iter()
    .map(edge_key)
    .collect();
  let rest_set: BTreeSet<_> = rest.iter().copied().collect();
  assert_eq!(
    rest_set,
    expected,
    "pages after a deleted cursor edge {cursor_edge:?} must return the {} edges not yet seen \
     (got {} edges)",
    expected.len(),
    rest.len()
  );
  assert_eq!(rest.len(), rest_set.len(), "no edge may repeat");

  close_single_file(db).expect("close");
}

#[test]
fn w3_p1_edge_pages_return_each_edge_once_while_edges_are_added() {
  let dir = tempfile::tempdir().expect("tempdir");
  let db = open_db(&dir.path().join("p1-edges-insert.kitedb"));

  db.begin(false).expect("begin");
  let link = db.define_etype("LINK").expect("define LINK");
  let hub = db.create_node(Some("hub")).expect("hub");
  for i in 0..64 {
    let src = db.create_node(Some(&format!("src{i}"))).expect("src");
    db.add_edge(src, link, hub).expect("add edge");
  }
  db.commit().expect("commit");
  let original: BTreeSet<_> = db
    .list_edges(None)
    .iter()
    .map(|edge| (edge.src, edge.etype, edge.dst))
    .collect();
  assert_eq!(original.len(), 64, "setup: 64 edges");

  let first = edges_page_single(
    &db,
    PaginationOptions {
      limit: 32,
      cursor: None,
    },
  );
  assert_eq!(first.items.len(), 32, "setup: first page");

  // A writer adds edges between pages. Their source ids are all larger than
  // every original edge's, so none of them sorts before the cursor.
  db.begin(false).expect("begin inserts");
  for i in 0..2048 {
    let src = db.create_node(Some(&format!("late{i}"))).expect("late src");
    db.add_edge(src, link, hub).expect("add late edge");
  }
  db.commit().expect("commit inserts");

  let mut seen: HashMap<(NodeId, u32, NodeId), usize> = HashMap::new();
  for edge in first
    .items
    .iter()
    .chain(edges_after(&db, first.next_cursor.clone(), 32).iter())
  {
    *seen.entry(edge_key(edge)).or_default() += 1;
  }
  let skipped: Vec<_> = original
    .iter()
    .filter(|edge| !seen.contains_key(edge))
    .collect();
  let repeated: Vec<_> = seen
    .iter()
    .filter(|(_, count)| **count > 1)
    .map(|(edge, count)| (*edge, *count))
    .collect();
  assert!(
    skipped.is_empty() && repeated.is_empty(),
    "paging edges while edges are added must return every original edge exactly once: \
     {} skipped (e.g. {:?}), {} repeated (e.g. {:?})",
    skipped.len(),
    skipped.first(),
    repeated.len(),
    repeated.first()
  );

  close_single_file(db).expect("close");
}

// ============================================================================
// P2: stale replica progress pins retention; a reset sidecar is not detected
// ============================================================================

#[test]
fn w3_p2_decommissioned_replica_progress_can_be_removed() {
  const DECOMMISSIONED: &str = "decommissioned-replica";

  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("p2-progress-primary.kitedb");
  let sidecar = default_replication_sidecar_path(&primary_path);
  let primary = open_single_file(
    &primary_path,
    primary_options()
      .replication_segment_max_bytes(1)
      .replication_retention_min_entries(2),
  )
  .expect("open primary");

  commit_node(&primary, "n0").expect("token");
  primary
    .primary_report_replica_progress(DECOMMISSIONED, 1, 1)
    .expect("report progress");
  for i in 1..10 {
    commit_node(&primary, &format!("n{i}")).expect("token");
  }

  let pinned = primary.primary_run_retention().expect("retention");
  assert_eq!(
    pinned.retained_floor, 2,
    "setup: the replica at log 1 pins the floor"
  );

  assert!(
    remove_replica_progress(&sidecar, DECOMMISSIONED)
      .expect("remove decommissioned replica progress"),
    "the decommissioned replica had progress recorded"
  );

  let outcome = primary
    .primary_run_retention()
    .expect("retention after removal");
  assert_eq!(
    outcome.retained_floor, 8,
    "with the replica removed, retention keeps only retention_min_entries=2 below head 10"
  );
  let status = primary
    .primary_replication_status()
    .expect("primary status");
  assert!(
    status
      .replica_lags
      .iter()
      .all(|lag| lag.replica_id != DECOMMISSIONED),
    "status must not list a removed replica: {:?}",
    status.replica_lags
  );
  assert!(
    !load_replica_progress(&sidecar)
      .expect("load progress")
      .contains_key(DECOMMISSIONED),
    "replica-progress.json must drop a removed replica"
  );

  close_single_file(primary).expect("close primary");
}

#[test]
fn w3_p2_replica_detects_a_reset_primary_sidecar() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("p2-reset-primary.kitedb");
  let replica_path = dir.path().join("p2-reset-replica.kitedb");

  let primary = open_primary(&primary_path);
  let replica = open_replica(&replica_path, &primary_path);
  replica
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap empty primary");
  for i in 0..5 {
    commit_node(&primary, &format!("before{i}")).expect("token");
  }
  catch_up_all(&replica).expect("catch up");
  let applied = replica
    .replica_replication_status()
    .expect("replica status");
  assert_eq!(
    (applied.applied_epoch, applied.applied_log_index),
    (1, 5),
    "setup: replica caught up"
  );

  // An operator clears `sidecar_needs_repair` the only way available: delete
  // the sidecar. The checkpoint empties the WAL, so the reopened primary has
  // no commit to compare against and starts a fresh manifest at 1:0.
  primary.checkpoint().expect("checkpoint");
  close_single_file(primary).expect("close primary");
  std::fs::remove_dir_all(default_replication_sidecar_path(&primary_path))
    .expect("delete primary sidecar");
  let primary = open_primary(&primary_path);
  let reset = primary
    .primary_replication_status()
    .expect("primary status");
  assert_eq!(
    (reset.epoch, reset.head_log_index),
    (1, 0),
    "setup: sidecar restarted"
  );

  let after: Vec<String> = (0..3).map(|i| format!("after{i}")).collect();
  for key in &after {
    commit_node(&primary, key).expect("token");
  }

  let result = catch_up_all(&replica);
  let status = replica
    .replica_replication_status()
    .expect("replica status");
  let missing: Vec<&String> = after
    .iter()
    .filter(|key| replica.node_by_key(key).is_none())
    .collect();
  assert!(
    missing.is_empty() || status.needs_reseed,
    "a replica at 1:5 must either apply or flag needs_reseed for frames 1..3 of a reset sidecar; \
     it silently skipped them: catch-up={result:?}, missing={missing:?}, applied={}:{}, \
     needs_reseed={}, last_error={:?}",
    status.applied_epoch,
    status.applied_log_index,
    status.needs_reseed,
    status.last_error
  );

  // A reseed adopts the new sidecar history, though its head (1:3) is below
  // the cursor the replica held in the old one (1:5).
  replica
    .replica_reseed_from_snapshot()
    .expect("reseed from the reset primary");
  let reseeded = replica
    .replica_replication_status()
    .expect("replica status");
  assert_eq!(
    (
      reseeded.applied_epoch,
      reseeded.applied_log_index,
      reseeded.needs_reseed
    ),
    (1, 3, false),
    "after the reseed the replica follows the new sidecar from its head"
  );
  commit_node(&primary, "after-reseed").expect("token");
  catch_up_all(&replica).expect("catch up after reseed");
  for key in after.iter().map(String::as_str).chain(["after-reseed"]) {
    assert!(
      replica.node_by_key(key).is_some(),
      "replica must hold {key} after the reseed"
    );
  }

  close_single_file(replica).expect("close replica");
  close_single_file(primary).expect("close primary");
}

// ============================================================================
// P8: epoch fencing gaps
// ============================================================================

#[test]
fn w3_p8_stale_primary_rejects_commits_while_sidecar_needs_repair() {
  let dir = tempfile::tempdir().expect("tempdir");
  let db_a = dir.path().join("p8-fence-a.kitedb");
  let db_b = dir.path().join("p8-fence-b.kitedb");
  let sidecar = dir.path().join("p8-fence.sidecar");

  // Two primaries share one sidecar, as after a failover on shared storage.
  let primary_a = open_single_file(&db_a, primary_options().replication_sidecar_path(&sidecar))
    .expect("open primary a");
  let primary_b = open_single_file(
    &db_b,
    primary_options()
      .replication_sidecar_path(&sidecar)
      .replication_fail_after_append_for_testing(0),
  )
  .expect("open primary b");

  let t0 = commit_node(&primary_a, "a0").expect("a0 token");
  assert_eq!(t0.epoch, 1, "setup: a writes epoch 1");
  assert_eq!(
    primary_b
      .primary_promote_to_next_epoch()
      .expect("promote b"),
    2
  );

  // The new primary's sidecar append fails (injected), which fences the
  // shared sidecar for repair.
  let b_token = commit_node(&primary_b, "b0");
  assert!(b_token.is_none(), "setup: b's append fails");
  assert!(
    primary_b
      .primary_replication_status()
      .expect("b status")
      .sidecar_needs_repair,
    "setup: sidecar needs repair"
  );

  // A is a stale primary (epoch 1 < 2); the repair fence must not hide that.
  primary_a.begin(false).expect("begin stale");
  primary_a.create_node(Some("stale")).expect("create stale");
  let result = primary_a.commit_with_token();
  let committed_locally = primary_a.node_by_key("stale").is_some();
  assert!(
    matches!(&result, Err(error) if error.to_string().contains("stale primary")),
    "a stale primary must reject local commits even while the sidecar needs repair: \
     commit={result:?}, committed_locally={committed_locally}"
  );

  close_single_file(primary_b).expect("close b");
  close_single_file(primary_a).expect("close a");
}

/// Copy every file of a sidecar directory (no subdirectories).
fn copy_sidecar(from: &Path, to: &Path) {
  std::fs::create_dir_all(to).expect("create sidecar copy");
  for entry in std::fs::read_dir(from).expect("read sidecar") {
    let entry = entry.expect("sidecar entry");
    std::fs::copy(entry.path(), to.join(entry.file_name())).expect("copy sidecar file");
  }
}

#[test]
fn w3_p8_replica_rejects_newer_epoch_frame_that_breaks_index_continuity() {
  let dir = tempfile::tempdir().expect("tempdir");
  let db_a = dir.path().join("p8-continuity-a.kitedb");
  let db_b = dir.path().join("p8-continuity-b.kitedb");
  let replica_path = dir.path().join("p8-continuity-replica.kitedb");

  let primary_a = open_primary(&db_a);
  let replica = open_replica(&replica_path, &db_a);
  replica
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap empty primary");
  // B takes over from a stale copy of A's sidecar (same history, head 0),
  // as after restoring shared storage from an old backup.
  copy_sidecar(
    &default_replication_sidecar_path(&db_a),
    &default_replication_sidecar_path(&db_b),
  );
  for i in 0..3 {
    commit_node(&primary_a, &format!("a{i}")).expect("token");
  }
  catch_up_all(&replica).expect("catch up");
  close_single_file(replica).expect("close replica");
  close_single_file(primary_a).expect("close a");

  // B never saw A's frames 1..3: its epoch-2 history starts at log 1.
  let primary_b = open_primary(&db_b);
  assert_eq!(
    primary_b
      .primary_promote_to_next_epoch()
      .expect("promote b"),
    2
  );
  primary_b.begin(false).expect("begin b");
  primary_b
    .create_node_with_id(1_000, Some("b1"))
    .expect("create b1");
  let token = primary_b
    .commit_with_token()
    .expect("commit b1")
    .expect("b1 token");
  assert_eq!(
    (token.epoch, token.log_index),
    (2, 1),
    "setup: b's first frame"
  );

  // The replica, at 1:3, follows B after the failover.
  let replica = open_replica(&replica_path, &db_b);
  let result = replica.replica_catch_up_once(64);
  let status = replica
    .replica_replication_status()
    .expect("replica status");
  let applied_b1 = replica.node_by_key("b1").is_some();
  assert!(
    !applied_b1
      && (status.applied_epoch, status.applied_log_index) == (1, 3)
      && status.needs_reseed,
    "a replica at 1:3 must not apply frame 2:1 (expected next log 4) and must need a reseed: \
     catch-up={result:?}, applied_b1={applied_b1}, applied={}:{}, needs_reseed={}",
    status.applied_epoch,
    status.applied_log_index,
    status.needs_reseed
  );

  close_single_file(replica).expect("close replica");
  close_single_file(primary_b).expect("close b");
}

// ============================================================================
// P9: snapshot transport exposes db_path and starts at the retained floor
// ============================================================================

#[test]
fn w3_p9_snapshot_transport_does_not_expose_db_path() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("p9-path-primary.kitedb");
  let primary = open_primary(&primary_path);
  commit_node(&primary, "n0").expect("token");

  let json = primary
    .primary_export_snapshot_transport_json(false)
    .expect("snapshot transport");
  let value: serde_json::Value = serde_json::from_str(&json).expect("parse snapshot json");
  let path = primary_path.to_string_lossy().to_string();
  assert!(
    value.get("db_path").is_none() && !json.contains(&path),
    "snapshot transport JSON must not expose the primary's filesystem path: {json}"
  );

  close_single_file(primary).expect("close primary");
}

#[test]
fn w3_p9_snapshot_transport_start_cursor_resumes_after_the_snapshot() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("p9-cursor-primary.kitedb");
  let primary = open_primary(&primary_path);
  for i in 0..3 {
    commit_node(&primary, &format!("n{i}")).expect("token");
  }

  let snapshot: serde_json::Value = serde_json::from_str(
    &primary
      .primary_export_snapshot_transport_json(false)
      .expect("snapshot transport"),
  )
  .expect("parse snapshot json");
  let head = snapshot["head_log_index"].as_u64().expect("head_log_index");
  assert_eq!(head, 3, "setup: three frames");
  let start_cursor = snapshot["start_cursor"]
    .as_str()
    .expect("start_cursor")
    .to_string();
  let cursor = ReplicationCursor::from_str(&start_cursor).expect("parse start_cursor");

  // The snapshot holds every commit up to the head, so log pulls resume there.
  let log: serde_json::Value = serde_json::from_str(
    &primary
      .primary_export_log_transport_json(Some(&start_cursor), 64, 1 << 20, false)
      .expect("log transport"),
  )
  .expect("parse log json");
  let replayed = log["frame_count"].as_u64().expect("frame_count");
  assert!(
    cursor.log_index == head && replayed == 0,
    "start_cursor {start_cursor} must point at head {head} so a log pull after the snapshot \
     replays nothing; it replays {replayed} frames the snapshot already holds"
  );

  close_single_file(primary).expect("close primary");
}

// ============================================================================
// P10: export drops labels; metrics schema counts; point-in-time export
// ============================================================================

fn label_names(db: &SingleFileDB, node_id: NodeId) -> BTreeSet<String> {
  db.node_labels(node_id)
    .into_iter()
    .map(|label| db.label_name(label).expect("label name"))
    .collect()
}

#[test]
fn w3_p10_export_import_round_trip_keeps_node_labels() {
  let dir = tempfile::tempdir().expect("tempdir");
  let source = open_db(&dir.path().join("p10-labels-source.kitedb"));

  source.begin(false).expect("begin");
  let user = source.define_label("User").expect("define User");
  let admin = source.define_label("Admin").expect("define Admin");
  let alice = source.create_node(Some("user:alice")).expect("alice");
  let bob = source.create_node(Some("user:bob")).expect("bob");
  source
    .add_node_label(alice, user)
    .expect("label alice User");
  source
    .add_node_label(alice, admin)
    .expect("label alice Admin");
  source.add_node_label(bob, user).expect("label bob User");
  source.commit().expect("commit");

  let export_path = dir.path().join("p10-labels.json");
  let data = export_to_object_single(&source, ExportOptions::default()).expect("export");
  export_to_json(&data, &export_path, false).expect("write export");
  let loaded = import_from_json(&export_path).expect("read export");

  let target = open_db(&dir.path().join("p10-labels-target.kitedb"));
  import_from_object_single(&target, &loaded, ImportOptions::default()).expect("import");
  let alice = target.node_by_key("user:alice").expect("imported alice");
  let bob = target.node_by_key("user:bob").expect("imported bob");

  let expected_alice: BTreeSet<String> = ["Admin", "User"].map(String::from).into();
  let expected_bob: BTreeSet<String> = ["User"].map(String::from).into();
  assert_eq!(
    (label_names(&target, alice), label_names(&target, bob)),
    (expected_alice, expected_bob),
    "export/import must keep node labels"
  );

  close_single_file(target).expect("close target");
  close_single_file(source).expect("close source");
}

#[test]
fn w3_p10_metrics_schema_counts_survive_checkpoint() {
  let dir = tempfile::tempdir().expect("tempdir");
  let db = open_db(&dir.path().join("p10-metrics.kitedb"));

  db.begin(false).expect("begin");
  for name in ["User", "Admin"] {
    db.define_label(name).expect("define label");
  }
  for name in ["KNOWS", "FOLLOWS", "BLOCKS"] {
    db.define_etype(name).expect("define etype");
  }
  for name in ["name", "email", "age", "score"] {
    db.define_propkey(name).expect("define propkey");
  }
  db.commit().expect("commit");

  let counts = |db: &SingleFileDB| {
    let data = collect_metrics_single_file(db).data;
    (
      data.schema_labels,
      data.schema_etypes,
      data.schema_prop_keys,
    )
  };
  let before = counts(&db);
  db.checkpoint().expect("checkpoint");
  let after = counts(&db);
  assert_eq!(
    (before, after),
    ((2, 3, 4), (2, 3, 4)),
    "metrics schema counts (labels, etypes, prop keys) must count every committed definition, \
     before and after a checkpoint"
  );

  close_single_file(db).expect("close");
}

#[test]
fn w3_p10_export_is_a_point_in_time_view_under_concurrent_writes() {
  const WRITER_MAX_TXS: u64 = 20_000;
  const MAX_EXPORTS: usize = 30;

  let dir = tempfile::tempdir().expect("tempdir");
  let db = Arc::new(
    open_single_file(
      dir.path().join("p10-point-in-time.kitedb"),
      SingleFileOpenOptions::new().sync_mode(SyncMode::Off),
    )
    .expect("open db"),
  );

  db.begin(false).expect("begin seed");
  let link = db.define_etype("LINK").expect("define LINK");
  let weight = db.define_propkey("weight").expect("define weight");
  for i in 0..2_000i64 {
    let node = db.create_node(Some(&format!("seed{i}"))).expect("seed");
    db.set_node_prop(node, weight, PropValue::I64(i))
      .expect("seed prop");
  }
  db.commit().expect("commit seed");

  // Each writer transaction adds two nodes and the edge between them, so any
  // single point in time has no edge whose endpoints are missing.
  let stop = Arc::new(AtomicBool::new(false));
  let writer = {
    let db = Arc::clone(&db);
    let stop = Arc::clone(&stop);
    std::thread::spawn(move || -> kitedb::Result<u64> {
      let mut txs = 0u64;
      while !stop.load(Ordering::Relaxed) && txs < WRITER_MAX_TXS {
        db.begin(false)?;
        let a = db.create_node(None)?;
        let b = db.create_node(None)?;
        db.add_edge(a, link, b)?;
        db.commit()?;
        txs += 1;
      }
      Ok(txs)
    })
  };

  let deadline = Instant::now() + Duration::from_secs(10);
  let mut exports = 0usize;
  let mut torn = None;
  while torn.is_none() && exports < MAX_EXPORTS && Instant::now() < deadline {
    let data = export_to_object_single(&db, ExportOptions::default()).expect("export");
    exports += 1;
    let ids: HashSet<u64> = data.nodes.iter().map(|node| node.id).collect();
    torn = data
      .edges
      .iter()
      .find(|edge| !ids.contains(&edge.src) || !ids.contains(&edge.dst))
      .map(|edge| (edge.src, edge.dst, data.nodes.len(), data.edges.len()));
  }
  stop.store(true, Ordering::Relaxed);
  let writer_txs = writer
    .join()
    .expect("writer thread")
    .expect("writer commits");
  assert!(writer_txs > 0, "setup: the writer commits during exports");

  if let Some((src, dst, nodes, edges)) = torn {
    panic!(
      "export #{exports} is not a point-in-time view: edge {src}->{dst} references a node \
       missing from the export's {nodes} nodes ({edges} edges; writer committed {writer_txs} txs)"
    );
  }
}

// ============================================================================
// P5, P7: OTLP push
// ============================================================================

#[cfg(feature = "otlp")]
mod otlp {
  use std::net::TcpListener;
  use std::time::Duration;

  use kitedb::metrics::{
    push_replication_metrics_otel_grpc_payload_with_options,
    push_replication_metrics_otel_json_payload_with_options, OtlpHttpPushOptions,
  };

  const BREAKER_OPEN_MS: u64 = 50;

  /// A loopback port with no listener: connects fail at once, no network.
  fn closed_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind probe port");
    let port = listener.local_addr().expect("probe port addr").port();
    drop(listener);
    port
  }

  fn breaker_options(scope: &str) -> OtlpHttpPushOptions {
    OtlpHttpPushOptions {
      timeout_ms: 2_000,
      circuit_breaker_failure_threshold: 1,
      circuit_breaker_open_ms: BREAKER_OPEN_MS,
      circuit_breaker_half_open_probes: 1,
      circuit_breaker_scope_key: Some(scope.to_string()),
      ..OtlpHttpPushOptions::default()
    }
  }

  /// Open the breaker with one transport failure, then wait until the next
  /// push is admitted as the half-open probe.
  fn trip_breaker(endpoint: &str, options: &OtlpHttpPushOptions) {
    let error = push_replication_metrics_otel_json_payload_with_options("{}", endpoint, options)
      .expect_err("push to a closed port fails");
    assert!(
      error.to_string().contains("transport error"),
      "setup: transport failure trips the breaker: {error}"
    );
    std::thread::sleep(Duration::from_millis(BREAKER_OPEN_MS * 3));
  }

  fn assert_breaker_not_stuck(endpoint: &str, options: &OtlpHttpPushOptions, probe: &str) {
    // Twice the open window later, the breaker must admit a push again.
    std::thread::sleep(Duration::from_millis(BREAKER_OPEN_MS * 3));
    let error = push_replication_metrics_otel_json_payload_with_options("{}", endpoint, options)
      .expect_err("push to a closed port fails");
    assert!(
      !error.to_string().contains("probe already in flight"),
      "the half-open probe that failed with {probe} must not stay in flight: later push \
       failed with: {error}"
    );
  }

  #[test]
  fn w3_p7_invalid_grpc_payload_does_not_leave_breaker_half_open() {
    let port = closed_port();
    let http_endpoint = format!("http://127.0.0.1:{port}/v1/metrics");
    let grpc_endpoint = format!("http://127.0.0.1:{port}");
    let options = breaker_options("w3-p7-grpc-decode");
    trip_breaker(&http_endpoint, &options);

    // The half-open probe exits early: the payload is not valid protobuf.
    let probe = push_replication_metrics_otel_grpc_payload_with_options(
      &[0xff, 0xff, 0xff],
      &grpc_endpoint,
      &options,
    )
    .expect_err("invalid protobuf is rejected");
    assert!(
      probe.to_string().contains("Invalid OTLP protobuf payload"),
      "setup: the probe fails on its payload: {probe}"
    );

    assert_breaker_not_stuck(&http_endpoint, &options, &probe.to_string());
  }

  #[test]
  fn w3_p7_tls_setup_error_does_not_leave_breaker_half_open() {
    let port = closed_port();
    let endpoint = format!("https://127.0.0.1:{port}/v1/metrics");
    let options = breaker_options("w3-p7-http-tls");
    trip_breaker(&endpoint, &options);

    // The half-open probe exits early: its CA file does not exist.
    let mut tls_options = options.clone();
    tls_options.tls.ca_cert_pem_path = Some("/nonexistent/w3-p7-ca.pem".to_string());
    let probe =
      push_replication_metrics_otel_json_payload_with_options("{}", &endpoint, &tls_options)
        .expect_err("missing CA file is rejected");
    assert!(
      probe.to_string().contains("ca_cert_pem_path"),
      "setup: the probe fails on its TLS setup: {probe}"
    );

    assert_breaker_not_stuck(&endpoint, &options, &probe.to_string());
  }

  #[test]
  fn w3_p5_grpc_push_inside_a_tokio_runtime_does_not_panic() {
    let endpoint = format!("http://127.0.0.1:{}", closed_port());
    let options = OtlpHttpPushOptions {
      timeout_ms: 2_000,
      ..OtlpHttpPushOptions::default()
    };

    // An async application calls the (sync) push from a task.
    let runtime = tokio::runtime::Builder::new_current_thread()
      .enable_all()
      .build()
      .expect("caller runtime");
    let outcome = runtime.block_on(async {
      std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // An empty payload is a valid, empty ExportMetricsServiceRequest.
        push_replication_metrics_otel_grpc_payload_with_options(&[], &endpoint, &options)
      }))
    });

    let result = match outcome {
      Ok(result) => result,
      Err(panic) => {
        let message = panic
          .downcast_ref::<String>()
          .cloned()
          .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
          .unwrap_or_default();
        panic!("gRPC push panicked inside a tokio runtime: {message}");
      }
    };
    let error = result.expect_err("push to a closed port fails");
    assert!(
      error.to_string().contains("transport error"),
      "inside a runtime the push must still reach the transport: {error}"
    );
  }
}
