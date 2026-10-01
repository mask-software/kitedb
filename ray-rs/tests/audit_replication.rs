//! Audit repros for replication findings R1, R3 and R4.
//!
//! Every test here reproduces a finding through the public API and fails until
//! the finding is fixed. Crash scenarios use a child process that aborts (the
//! same pattern as `replication_phase_b.rs`) or, for the replica cursor, a
//! rollback of the on-disk cursor file to the state a crash would leave.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use kitedb::api::kite::{Kite, KiteOptions, NodeDef, PropDef};
use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions, SyncMode,
};
use kitedb::replication::primary::default_replication_sidecar_path;
use kitedb::replication::types::{CommitToken, ReplicationRole};
use kitedb::types::PropValue;
use kitedb::KiteError;

const R3_CHILD_ENV: &str = "KITEDB_AUDIT_R3_CHILD";
const R3_DB_PATH_ENV: &str = "KITEDB_AUDIT_R3_DB_PATH";
const R3_TOKEN_PATH_ENV: &str = "KITEDB_AUDIT_R3_TOKEN_PATH";
const R3_CRASH_COMMITS: u64 = 5;
const R3_IDLE_COMMITS: u64 = 3;
/// Upper bound for an idle primary to expose buffered frames to a replica.
const R3_IDLE_VISIBILITY_BOUND: Duration = Duration::from_secs(5);
const R3_IDLE_CHILD_LIFETIME: Duration = Duration::from_secs(60);
const R3_CHILD_STARTUP_TIMEOUT: Duration = Duration::from_secs(120);
/// Normal/Off sync modes persist the manifest (and flush the segment) every
/// this many appends (`DEFAULT_MANIFEST_REFRESH_APPEND_INTERVAL`).
const NORMAL_MODE_MANIFEST_INTERVAL: u64 = 256;
/// On-disk replica cursor file (`replica.rs` `CURSOR_FILE_NAME`).
const REPLICA_CURSOR_FILE: &str = "replica-cursor.json";

type GraphState = (BTreeSet<(u64, Option<String>)>, BTreeSet<(u64, u32, u64)>);

fn open_primary(path: &Path, sync_mode: SyncMode) -> SingleFileDB {
  open_single_file(
    path,
    SingleFileOpenOptions::new()
      .sync_mode(sync_mode)
      .auto_checkpoint(false)
      .replication_role(ReplicationRole::Primary),
  )
  .expect("open primary")
}

fn open_replica(path: &Path, primary_path: &Path) -> SingleFileDB {
  open_single_file(
    path,
    SingleFileOpenOptions::new()
      .replication_role(ReplicationRole::Replica)
      .replication_source_db_path(primary_path),
  )
  .expect("open replica")
}

fn commit(db: &SingleFileDB) -> CommitToken {
  db.commit_with_token()
    .expect("commit")
    .expect("primary commit token")
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

fn graph_state(db: &SingleFileDB) -> GraphState {
  let nodes = db
    .list_nodes()
    .into_iter()
    .map(|node_id| (node_id, db.node_key(node_id)))
    .collect();
  let edges = db
    .list_edges(None)
    .into_iter()
    .map(|edge| (edge.src, edge.etype, edge.dst))
    .collect();
  (nodes, edges)
}

fn cursor_path(replica_path: &Path) -> PathBuf {
  default_replication_sidecar_path(replica_path).join(REPLICA_CURSOR_FILE)
}

fn spawn_child(test_name: &str, db_path: &Path, token_path: &Path) -> Command {
  let mut command = Command::new(std::env::current_exe().expect("current test binary"));
  command
    .arg("--test-threads=1")
    .arg("--exact")
    .arg(test_name)
    .arg("--nocapture")
    .env(R3_CHILD_ENV, test_name)
    .env(R3_DB_PATH_ENV, db_path.as_os_str())
    .env(R3_TOKEN_PATH_ENV, token_path.as_os_str());
  command
}

fn child_requested(test_name: &str) -> Option<PathBuf> {
  match std::env::var(R3_CHILD_ENV) {
    Ok(value) if value == test_name => Some(PathBuf::from(
      std::env::var(R3_DB_PATH_ENV).expect("child db path env"),
    )),
    _ => None,
  }
}

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
  fn drop(&mut self) {
    let _ = self.0.kill();
    let _ = self.0.wait();
  }
}

// ============================================================================
// R1: replicas never receive label / etype / propkey name definitions
// ============================================================================

fn define_r1_schema_and_data(primary: &SingleFileDB) -> (u64, u32, u32) {
  primary.begin(false).expect("begin schema tx");
  // Definition order fixes the primary's ids: Contact < User, FOLLOWS < KNOWS,
  // email < name.
  let contact = primary.define_label("Contact").expect("define Contact");
  let user = primary.define_label("User").expect("define User");
  primary.define_etype("FOLLOWS").expect("define FOLLOWS");
  let knows = primary.define_etype("KNOWS").expect("define KNOWS");
  let email = primary.define_propkey("email").expect("define email");
  let name = primary.define_propkey("name").expect("define name");
  let alice = primary.create_node(Some("user:alice")).expect("alice");
  let bob = primary.create_node(Some("contact:bob")).expect("bob");
  primary.add_node_label(alice, user).expect("label alice");
  primary.add_node_label(bob, contact).expect("label bob");
  primary
    .set_node_prop(alice, email, PropValue::String("alice@example.com".into()))
    .expect("alice email");
  primary
    .set_node_prop(alice, name, PropValue::String("Alice".into()))
    .expect("alice name");
  primary
    .add_edge(alice, knows, bob)
    .expect("alice knows bob");
  commit(primary);
  (alice, email, name)
}

fn assert_schema_names_match(primary: &SingleFileDB, replica: &SingleFileDB) {
  let labels = ["Contact", "User"];
  let etypes = ["FOLLOWS", "KNOWS"];
  let propkeys = ["email", "name"];

  let label_map = |db: &SingleFileDB| -> BTreeMap<&str, Option<u32>> {
    labels
      .iter()
      .map(|name| (*name, db.label_id(name)))
      .collect()
  };
  let etype_map = |db: &SingleFileDB| -> BTreeMap<&str, Option<u32>> {
    etypes
      .iter()
      .map(|name| (*name, db.etype_id(name)))
      .collect()
  };
  let propkey_map = |db: &SingleFileDB| -> BTreeMap<&str, Option<u32>> {
    propkeys
      .iter()
      .map(|name| (*name, db.propkey_id(name)))
      .collect()
  };

  assert_eq!(
    propkey_map(replica),
    propkey_map(primary),
    "replica propkey name->id map must equal the primary's"
  );
  assert_eq!(
    label_map(replica),
    label_map(primary),
    "replica label name->id map must equal the primary's"
  );
  assert_eq!(
    etype_map(replica),
    etype_map(primary),
    "replica etype name->id map must equal the primary's"
  );
  for name in propkeys {
    let id = primary.propkey_id(name).expect("primary propkey id");
    assert_eq!(
      replica.propkey_name(id),
      primary.propkey_name(id),
      "replica propkey id {id} must name the same key as on the primary"
    );
  }
}

#[test]
fn audit_r1_catch_up_replicates_schema_name_ids() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("r1-frames-primary.kitedb");
  let replica_path = dir.path().join("r1-frames-replica.kitedb");

  let primary = open_primary(&primary_path, SyncMode::Full);
  let replica = open_replica(&replica_path, &primary_path);
  replica
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap empty primary");

  let (alice, email, name) = define_r1_schema_and_data(&primary);
  catch_up_all(&replica).expect("catch up");

  // Data replicates by numeric id; only the names are missing.
  assert_eq!(
    replica.node_prop(alice, email),
    primary.node_prop(alice, email)
  );
  assert_eq!(
    replica.node_prop(alice, name),
    primary.node_prop(alice, name)
  );
  assert_schema_names_match(&primary, &replica);

  close_single_file(replica).expect("close replica");
  close_single_file(primary).expect("close primary");
}

#[test]
fn audit_r1_snapshot_bootstrap_copies_schema_name_ids() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("r1-bootstrap-primary.kitedb");
  let replica_path = dir.path().join("r1-bootstrap-replica.kitedb");

  let primary = open_primary(&primary_path, SyncMode::Full);
  let (alice, email, _) = define_r1_schema_and_data(&primary);

  let replica = open_replica(&replica_path, &primary_path);
  replica
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap from snapshot");

  assert_eq!(
    replica.node_prop(alice, email),
    primary.node_prop(alice, email)
  );
  assert_schema_names_match(&primary, &replica);

  close_single_file(replica).expect("close replica");
  close_single_file(primary).expect("close primary");
}

#[test]
fn audit_r1_kite_on_replica_resolves_prop_names_like_primary() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("r1-kite-primary.kitedb");
  let replica_path = dir.path().join("r1-kite-replica.kitedb");

  // Create the primary file and sidecar, then attach a replica at log 0.
  close_single_file(open_primary(&primary_path, SyncMode::Full)).expect("close empty primary");
  let replica = open_replica(&replica_path, &primary_path);
  replica
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap empty primary");

  // Primary app: Contact first, so email gets the lower propkey id.
  let mut primary = Kite::open(
    &primary_path,
    KiteOptions::new()
      .node(NodeDef::new("Contact", "contact:").prop(PropDef::string("email")))
      .node(NodeDef::new("User", "user:").prop(PropDef::string("name")))
      .replication_role(ReplicationRole::Primary),
  )
  .expect("open primary kite");
  let alice = primary
    .create_node(
      "User",
      "alice",
      HashMap::from([("name".to_string(), PropValue::String("Alice".into()))]),
    )
    .expect("create alice")
    .id();
  primary
    .set_prop(
      alice,
      "email",
      PropValue::String("alice@example.com".into()),
    )
    .expect("set alice email");
  assert!(
    primary.raw().propkey_id("email") < primary.raw().propkey_id("name"),
    "test setup: email must get the lower id on the primary"
  );

  catch_up_all(&replica).expect("catch up");
  close_single_file(replica).expect("close replica");

  // Replica app: same schema, node types listed in the other order.
  let replica = Kite::open(
    &replica_path,
    KiteOptions::new()
      .node(NodeDef::new("User", "user:").prop(PropDef::string("name")))
      .node(NodeDef::new("Contact", "contact:").prop(PropDef::string("email")))
      .replication_role(ReplicationRole::Replica)
      .replication_source_db_path(&primary_path),
  )
  .expect("open replica kite");

  let primary_ids = (
    primary.raw().propkey_id("email"),
    primary.raw().propkey_id("name"),
  );
  let replica_ids = (
    replica.raw().propkey_id("email"),
    replica.raw().propkey_id("name"),
  );
  let name_on_replica = replica.prop(alice, "name");
  let email_on_replica = replica.prop(alice, "email");
  assert_eq!(
    name_on_replica,
    Some(PropValue::String("Alice".into())),
    "replica `name` must resolve like the primary (email={email_on_replica:?}; \
     primary (email,name) ids={primary_ids:?}, replica ids={replica_ids:?})"
  );
  assert_eq!(
    email_on_replica,
    Some(PropValue::String("alice@example.com".into())),
    "replica `email` must resolve like the primary"
  );

  replica.close().expect("close replica kite");
  primary.close().expect("close primary kite");
}

// ============================================================================
// R3: Normal-mode sidecar frames are buffered in memory
// ============================================================================

#[test]
fn audit_r3_helper_crash_after_checkpoint_child() {
  let Some(db_path) = child_requested("audit_r3_helper_crash_after_checkpoint_child") else {
    return;
  };

  let primary = open_primary(&db_path, SyncMode::Normal);
  for i in 0..R3_CRASH_COMMITS {
    primary.begin(false).expect("begin");
    primary
      .create_node(Some(&format!("r3-crash-{i}")))
      .expect("create node");
    commit(&primary);
  }
  // Checkpoint empties the WAL, so reopen has no WAL commit to compare against
  // the sidecar head.
  primary.checkpoint().expect("checkpoint");
  let marker = PathBuf::from(std::env::var(R3_TOKEN_PATH_ENV).expect("marker path env"));
  std::fs::write(marker, b"checkpointed").expect("write marker");
  std::process::abort();
}

#[test]
fn audit_r3_normal_mode_crash_after_checkpoint_has_no_silent_sidecar_gap() {
  let dir = tempfile::tempdir().expect("tempdir");
  let db_path = dir.path().join("r3-crash.kitedb");
  let marker_path = dir.path().join("r3-crash.marker");

  let status = spawn_child(
    "audit_r3_helper_crash_after_checkpoint_child",
    &db_path,
    &marker_path,
  )
  .status()
  .expect("spawn crash child");
  assert!(!status.success(), "child must abort to emulate a crash");
  assert!(
    marker_path.exists(),
    "test setup: child must reach its checkpoint before aborting"
  );

  let primary = match open_single_file(
    &db_path,
    SingleFileOpenOptions::new()
      .sync_mode(SyncMode::Normal)
      .auto_checkpoint(false)
      .replication_role(ReplicationRole::Primary),
  ) {
    Ok(db) => db,
    // Failing the reopen with a clear replication error is an accepted fix.
    Err(KiteError::InvalidReplication(message)) => {
      eprintln!("reopen rejected the stale sidecar: {message}");
      return;
    }
    Err(error) => panic!("unexpected reopen error: {error}"),
  };

  for i in 0..R3_CRASH_COMMITS {
    assert!(
      primary.node_by_key(&format!("r3-crash-{i}")).is_some(),
      "test setup: local commit {i} must survive the crash"
    );
  }

  let status = primary.primary_replication_status().expect("status");
  if !status.sidecar_needs_repair {
    let exported = primary
      .primary_export_log_transport_json(None, 1024, 64 * 1024 * 1024, false)
      .expect("export log");
    let exported: serde_json::Value = serde_json::from_str(&exported).expect("parse export");
    let frame_count = exported["frame_count"].as_u64().expect("frame_count");
    assert!(
      status.head_log_index >= R3_CRASH_COMMITS && frame_count >= R3_CRASH_COMMITS,
      "silent replication gap: {R3_CRASH_COMMITS} commits are durable in the DB, but the \
       sidecar has head_log_index={} and {frame_count} frames while \
       sidecar_needs_repair=false (last_replication_error={:?})",
      status.head_log_index,
      status.last_replication_error
    );
  }

  close_single_file(primary).expect("close primary");
}

#[test]
fn audit_r3_helper_idle_primary_child() {
  let Some(db_path) = child_requested("audit_r3_helper_idle_primary_child") else {
    return;
  };
  let token_path = PathBuf::from(std::env::var(R3_TOKEN_PATH_ENV).expect("token path env"));

  let primary = open_primary(&db_path, SyncMode::Normal);
  let mut last = None;
  for i in 0..R3_IDLE_COMMITS {
    primary.begin(false).expect("begin");
    primary
      .create_node(Some(&format!("r3-idle-{i}")))
      .expect("create node");
    last = Some(commit(&primary));
  }
  let tmp = token_path.with_extension("tmp");
  std::fs::write(&tmp, last.expect("token").to_string()).expect("write token");
  std::fs::rename(&tmp, &token_path).expect("publish token");

  // Stay idle with the DB open; never close (close would flush the buffer).
  std::thread::sleep(R3_IDLE_CHILD_LIFETIME);
  std::process::abort();
}

#[test]
fn audit_r3_idle_primary_frames_reach_replica_within_bound() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("r3-idle-primary.kitedb");
  let replica_path = dir.path().join("r3-idle-replica.kitedb");
  let token_path = dir.path().join("r3-idle.token");

  let mut child = KillOnDrop(
    spawn_child(
      "audit_r3_helper_idle_primary_child",
      &primary_path,
      &token_path,
    )
    .spawn()
    .expect("spawn idle primary child"),
  );

  let startup_deadline = Instant::now() + R3_CHILD_STARTUP_TIMEOUT;
  while !token_path.exists() {
    if let Some(status) = child.0.try_wait().expect("poll child") {
      panic!("idle primary child exited early: {status}");
    }
    assert!(
      Instant::now() < startup_deadline,
      "idle primary child did not commit in time"
    );
    std::thread::sleep(Duration::from_millis(20));
  }
  let token: CommitToken = std::fs::read_to_string(&token_path)
    .expect("read token")
    .parse()
    .expect("parse token");
  assert_eq!(token.log_index, R3_IDLE_COMMITS, "test setup: token");

  let replica = open_replica(&replica_path, &primary_path);
  let started = Instant::now();
  let mut last_error = None;
  loop {
    match replica.replica_catch_up_once(64) {
      Ok(_) => {}
      Err(error) => last_error = Some(error.to_string()),
    }
    let applied = replica
      .replica_replication_status()
      .expect("replica status")
      .applied_log_index;
    if applied >= token.log_index || started.elapsed() >= R3_IDLE_VISIBILITY_BOUND {
      break;
    }
    std::thread::sleep(Duration::from_millis(50));
  }

  let applied = replica
    .replica_replication_status()
    .expect("replica status")
    .applied_log_index;
  assert!(
    applied >= token.log_index,
    "idle primary kept {R3_IDLE_COMMITS} committed frames invisible to the replica for {:?}: \
     primary token={token}, replica applied_log_index={applied}, last catch-up error={last_error:?}",
    started.elapsed()
  );
  for i in 0..R3_IDLE_COMMITS {
    assert!(replica.node_by_key(&format!("r3-idle-{i}")).is_some());
  }

  drop(child);
  close_single_file(replica).expect("close replica");
}

// ============================================================================
// R4: replica replay is not atomic with its cursor
// ============================================================================

/// Commit `setup` on the primary and catch the replica up, save the replica
/// cursor file, commit `later` and catch up again, then restore the saved
/// cursor file. That is the on-disk state a crash leaves after per-frame
/// commits of the `later` frames but before `mark_applied`.
fn replay_later_frames_after_simulated_crash(
  primary: &SingleFileDB,
  primary_path: &Path,
  replica_path: &Path,
  setup: impl FnOnce(&SingleFileDB),
  later: impl FnOnce(&SingleFileDB) -> CommitToken,
) -> (SingleFileDB, CommitToken) {
  let replica = open_replica(replica_path, primary_path);
  replica
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap empty primary");

  setup(primary);
  catch_up_all(&replica).expect("catch up setup frames");
  let saved_cursor = std::fs::read(cursor_path(replica_path)).expect("read replica cursor");

  let head = later(primary);
  catch_up_all(&replica).expect("catch up later frames");
  assert_eq!(
    graph_state(&replica),
    graph_state(primary),
    "test setup: replica must match the primary before the simulated crash"
  );
  close_single_file(replica).expect("close replica");

  std::fs::write(cursor_path(replica_path), saved_cursor).expect("roll back replica cursor");
  let replica = open_replica(replica_path, primary_path);
  catch_up_all(&replica).expect("catch up after simulated crash");
  (replica, head)
}

#[test]
fn audit_r4_replay_after_crash_leaves_no_phantom_edge() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("r4-edge-primary.kitedb");
  let replica_path = dir.path().join("r4-edge-replica.kitedb");
  let primary = open_primary(&primary_path, SyncMode::Full);

  let ids = std::cell::Cell::new((0u64, 0u32, 0u64));
  let (replica, head) = replay_later_frames_after_simulated_crash(
    &primary,
    &primary_path,
    &replica_path,
    |db| {
      db.begin(false).expect("begin");
      let links = db.define_etype("LINKS").expect("etype");
      let a = db.create_node(Some("a")).expect("a");
      let b = db.create_node(Some("b")).expect("b");
      commit(db);
      ids.set((a, links, b));
    },
    |db| {
      let (a, links, b) = ids.get();
      db.begin(false).expect("begin edge");
      db.add_edge(a, links, b).expect("AddEdge(a,b)");
      commit(db);
      db.begin(false).expect("begin delete");
      db.delete_node(b).expect("DeleteNode(b)");
      commit(db)
    },
  );

  let (a, links, b) = ids.get();
  assert!(!primary.node_exists(b) && !primary.edge_exists(a, links, b));
  assert!(
    !replica.edge_exists(a, links, b),
    "replayed AddEdge(a,b) over a deleted b left a phantom edge: replica out_edges(a)={:?}",
    replica.out_edges(a)
  );
  assert!(
    !replica.node_exists(b),
    "b must stay deleted on the replica"
  );
  assert_eq!(
    graph_state(&replica),
    graph_state(&primary),
    "replica state must equal the primary's after replay"
  );
  assert_eq!(
    replica
      .replica_replication_status()
      .expect("status")
      .applied_log_index,
    head.log_index
  );

  close_single_file(replica).expect("close replica");
  close_single_file(primary).expect("close primary");
}

#[test]
fn audit_r4_replay_after_crash_keeps_key_lookup() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("r4-key-primary.kitedb");
  let replica_path = dir.path().join("r4-key-replica.kitedb");
  let primary = open_primary(&primary_path, SyncMode::Full);

  let (replica, head) = replay_later_frames_after_simulated_crash(
    &primary,
    &primary_path,
    &replica_path,
    |db| {
      db.begin(false).expect("begin");
      db.create_node(Some("anchor")).expect("anchor");
      commit(db);
    },
    |db| {
      // A key moves from one node to another: CreateNode(n1,"k"),
      // DeleteNode(n1), CreateNode(n2,"k").
      db.begin(false).expect("begin n1");
      let n1 = db.create_node(Some("k")).expect("n1");
      commit(db);
      db.begin(false).expect("begin delete n1");
      db.delete_node(n1).expect("delete n1");
      commit(db);
      db.begin(false).expect("begin n2");
      db.create_node(Some("k")).expect("n2");
      commit(db)
    },
  );

  let expected = primary.node_by_key("k");
  assert!(expected.is_some());
  assert_eq!(
    replica.node_by_key("k"),
    expected,
    "replayed CreateNode/DeleteNode over newer state broke the key index"
  );
  assert_eq!(graph_state(&replica), graph_state(&primary));
  assert_eq!(
    replica
      .replica_replication_status()
      .expect("status")
      .applied_log_index,
    head.log_index
  );

  close_single_file(replica).expect("close replica");
  close_single_file(primary).expect("close primary");
}

#[test]
fn audit_r4_normal_mode_bootstrap_cursor_matches_copied_state() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("r4-bootstrap-primary.kitedb");
  let replica_path = dir.path().join("r4-bootstrap-replica.kitedb");

  let primary = open_primary(&primary_path, SyncMode::Normal);
  primary.begin(false).expect("begin");
  let links = primary.define_etype("LINKS").expect("etype");
  let a = primary.create_node(Some("a")).expect("a");
  let b = primary.create_node(Some("b")).expect("b");
  commit(&primary);
  // Fill up to the manifest persist boundary so the on-disk head is 256.
  for i in 1..NORMAL_MODE_MANIFEST_INTERVAL {
    primary.begin(false).expect("begin filler");
    primary
      .create_node(Some(&format!("fill-{i}")))
      .expect("filler");
    commit(&primary);
  }
  // These two frames stay in the primary's in-memory buffer.
  primary.begin(false).expect("begin edge");
  primary.add_edge(a, links, b).expect("AddEdge(a,b)");
  commit(&primary);
  primary.begin(false).expect("begin delete");
  primary.delete_node(b).expect("DeleteNode(b)");
  let head = commit(&primary);
  assert_eq!(head.log_index, NORMAL_MODE_MANIFEST_INTERVAL + 2);

  let replica = open_replica(&replica_path, &primary_path);
  replica
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap from snapshot");
  let after_bootstrap = replica
    .replica_replication_status()
    .expect("status")
    .applied_log_index;

  // The primary eventually exposes the buffered frames (here: clean restart).
  let primary_state = graph_state(&primary);
  close_single_file(primary).expect("close primary");
  let primary = open_primary(&primary_path, SyncMode::Normal);
  assert_eq!(graph_state(&primary), primary_state);

  catch_up_all(&replica).expect("catch up after bootstrap");
  assert!(
    !replica.edge_exists(a, links, b),
    "bootstrap cursor {after_bootstrap} lagged the copied state (head {}): replayed \
     AddEdge(a,b) left a phantom edge, replica out_edges(a)={:?}",
    head.log_index,
    replica.out_edges(a)
  );
  assert_eq!(
    graph_state(&replica),
    graph_state(&primary),
    "replica state must equal the primary's after bootstrap + catch-up"
  );

  close_single_file(replica).expect("close replica");
  close_single_file(primary).expect("close primary");
}
