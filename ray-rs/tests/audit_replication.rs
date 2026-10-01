//! Audit repros for replication findings R1, R3, R4 and R5, plus the replica
//! missing-log-range escalation to `needs_reseed`.
//!
//! Every test here reproduces a finding through the public API and fails until
//! the finding is fixed. Crash scenarios use a child process that aborts (the
//! same pattern as `replication_phase_b.rs`) or, for the replica cursor, a
//! rollback of the on-disk cursor file to the state a crash would leave.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use kitedb::api::kite::{EdgeDef, Kite, KiteOptions, NodeDef, PropDef};
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

// ============================================================================
// R5: reseed creates source nodes before deleting stale ones
// ============================================================================

#[test]
fn audit_r5_reseed_with_stale_key_holder_keeps_key_lookup() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("r5-primary.kitedb");
  let replica_path = dir.path().join("r5-replica.kitedb");
  let primary = open_primary(&primary_path, SyncMode::Full);
  let replica = open_replica(&replica_path, &primary_path);
  replica
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap empty primary");

  primary.begin(false).expect("begin n1");
  let n1 = primary.create_node(Some("shared")).expect("n1");
  commit(&primary);
  catch_up_all(&replica).expect("catch up n1");
  assert_eq!(replica.node_by_key("shared"), Some(n1), "test setup");

  // The key moves to a new node while the replica is not following; the
  // replica's n1 is now a stale node holding the primary's key.
  primary.begin(false).expect("begin delete n1");
  primary.delete_node(n1).expect("delete n1");
  commit(&primary);
  primary.begin(false).expect("begin n2");
  let n2 = primary.create_node(Some("shared")).expect("n2");
  commit(&primary);

  replica
    .replica_reseed_from_snapshot()
    .expect("reseed over a stale node that holds the same key");
  assert_eq!(replica.node_by_key("shared"), Some(n2));
  assert_eq!(graph_state(&replica), graph_state(&primary));

  close_single_file(replica).expect("close replica");
  let replica = open_replica(&replica_path, &primary_path);
  assert_eq!(
    replica.node_by_key("shared"),
    Some(n2),
    "reseed WAL order (create n2, then delete stale n1) loses the key on replay"
  );
  assert_eq!(graph_state(&replica), graph_state(&primary));

  close_single_file(replica).expect("close replica");
  close_single_file(primary).expect("close primary");
}

// ============================================================================
// Missing log range: transient retries never escalate to needs_reseed
// ============================================================================

/// Pulls allowed before a permanently missing log range must be flagged. Each
/// pull already retries internally, so a few pulls exceed the transient budget.
const MISSING_RANGE_MAX_PULLS: usize = 4;

#[test]
fn audit_missing_log_range_escalates_to_needs_reseed() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("gap-primary.kitedb");
  let replica_path = dir.path().join("gap-replica.kitedb");
  let primary = open_primary(&primary_path, SyncMode::Full);
  let replica = open_replica(&replica_path, &primary_path);
  replica
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap empty primary");

  for i in 0..3 {
    primary.begin(false).expect("begin");
    primary
      .create_node(Some(&format!("gap-{i}")))
      .expect("create node");
    commit(&primary);
  }

  // The frames the replica needs next are gone for good.
  let mut removed = 0usize;
  let sidecar = default_replication_sidecar_path(&primary_path);
  for entry in std::fs::read_dir(&sidecar).expect("read primary sidecar") {
    let path = entry.expect("sidecar entry").path();
    let is_segment = path
      .file_name()
      .and_then(|name| name.to_str())
      .is_some_and(|name| name.starts_with("segment-") && name.ends_with(".rlog"));
    if is_segment {
      std::fs::remove_file(&path).expect("remove segment");
      removed += 1;
    }
  }
  assert!(removed > 0, "test setup: no segment files found");

  let mut errors = Vec::new();
  for _ in 0..MISSING_RANGE_MAX_PULLS {
    let error = replica
      .replica_catch_up_once(64)
      .expect_err("catch-up must fail while frames are missing");
    errors.push(error.to_string());
    if replica
      .replica_replication_status()
      .expect("replica status")
      .needs_reseed
    {
      break;
    }
  }

  let status = replica
    .replica_replication_status()
    .expect("replica status");
  assert!(
    status.needs_reseed,
    "a permanently missing log range never escalated to needs_reseed after {} pulls: \
     errors={errors:?}",
    errors.len()
  );
  assert!(
    errors
      .last()
      .is_some_and(|error| error.contains("needs reseed")),
    "the escalating pull must report `needs reseed`: errors={errors:?}"
  );
  assert!(
    status
      .last_error
      .as_deref()
      .is_some_and(|error| error.contains("needs reseed")),
    "status must report `needs reseed`: last_error={:?}",
    status.last_error
  );

  replica.replica_reseed_from_snapshot().expect("reseed");
  let status = replica
    .replica_replication_status()
    .expect("replica status");
  assert!(!status.needs_reseed && status.last_error.is_none());
  assert_eq!(graph_state(&replica), graph_state(&primary));

  close_single_file(replica).expect("close replica");
  close_single_file(primary).expect("close primary");
}

// ============================================================================
// Regression tests for the fixes above
// ============================================================================

#[test]
fn bootstrap_from_buffered_primary_pins_cursor_to_published_head() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("pin-primary.kitedb");
  let replica_path = dir.path().join("pin-replica.kitedb");

  // Normal mode keeps these frames in memory until the publisher writes them.
  let primary = open_primary(&primary_path, SyncMode::Normal);
  let mut head = None;
  for i in 0..3 {
    primary.begin(false).expect("begin");
    primary
      .create_node(Some(&format!("pin-{i}")))
      .expect("create node");
    head = Some(commit(&primary));
  }
  let head = head.expect("head token");

  let replica = open_replica(&replica_path, &primary_path);
  replica
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap from snapshot");
  assert_eq!(
    replica
      .replica_replication_status()
      .expect("status")
      .applied_log_index,
    head.log_index,
    "bootstrap cursor must match the copied state"
  );
  assert_eq!(graph_state(&replica), graph_state(&primary));

  close_single_file(replica).expect("close replica");
  close_single_file(primary).expect("close primary");
}

#[test]
fn catch_up_reads_active_segment_past_stale_manifest_end() {
  use kitedb::replication::manifest::ManifestStore;

  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("stale-manifest-primary.kitedb");
  let replica_path = dir.path().join("stale-manifest-replica.kitedb");
  let primary = open_primary(&primary_path, SyncMode::Full);
  let replica = open_replica(&replica_path, &primary_path);
  replica
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap empty primary");

  for i in 0..3 {
    primary.begin(false).expect("begin");
    primary
      .create_node(Some(&format!("stale-{i}")))
      .expect("create node");
    commit(&primary);
  }
  assert_eq!(replica.replica_catch_up_once(1).expect("catch up one"), 1);
  let primary_state = graph_state(&primary);
  close_single_file(primary).expect("close primary");

  // A buffered primary writes frames into its active segment before it
  // persists the manifest, so the manifest can trail the segment file.
  let manifest_store =
    ManifestStore::new(default_replication_sidecar_path(&primary_path).join("manifest.json"));
  let mut manifest = manifest_store.read().expect("read manifest");
  manifest.head_log_index = 1;
  for segment in &mut manifest.segments {
    segment.end_log_index = segment.end_log_index.min(1);
  }
  manifest_store
    .write(&manifest)
    .expect("write stale manifest");

  catch_up_all(&replica).expect("catch up");
  assert_eq!(
    replica
      .replica_replication_status()
      .expect("status")
      .applied_log_index,
    3
  );
  assert_eq!(graph_state(&replica), primary_state);

  close_single_file(replica).expect("close replica");
}

#[test]
fn replica_local_schema_translates_colliding_primary_ids() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("collide-primary.kitedb");
  let replica_path = dir.path().join("collide-replica.kitedb");
  let primary = open_primary(&primary_path, SyncMode::Full);

  // The replica file already has its own schema before it starts following.
  predefine_schema(
    &replica_path,
    SingleFileOpenOptions::new(),
    &[],
    &[],
    &["local_only"],
  );

  let replica = open_replica(&replica_path, &primary_path);
  primary.begin(false).expect("begin");
  let email = primary.define_propkey("email").expect("define email");
  let node = primary.create_node(Some("n")).expect("node");
  primary
    .set_node_prop(node, email, PropValue::String("e@example.com".into()))
    .expect("set email");
  commit(&primary);
  assert_eq!(
    replica.propkey_id("local_only"),
    Some(email),
    "test setup: ids must collide"
  );

  catch_up_all(&replica).expect("catch up translates the colliding id");
  let local_email = replica.propkey_id("email").expect("email defined locally");
  assert_ne!(local_email, email, "the primary's id is taken locally");
  assert_eq!(
    replica.node_prop(node, local_email),
    Some(PropValue::String("e@example.com".into()))
  );
  assert_eq!(
    replica.node_prop(node, email),
    None,
    "local_only stays empty"
  );
  assert_eq!(named_state(&replica), named_state(&primary));

  close_single_file(replica).expect("close replica");
  close_single_file(primary).expect("close primary");
}

#[test]
fn replay_after_crash_skips_records_for_deleted_node() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("replay-props-primary.kitedb");
  let replica_path = dir.path().join("replay-props-replica.kitedb");
  let primary = open_primary(&primary_path, SyncMode::Full);

  let ids = std::cell::Cell::new((0u64, 0u32, 0u64, 0u32, 0u32));
  let (replica, head) = replay_later_frames_after_simulated_crash(
    &primary,
    &primary_path,
    &replica_path,
    |db| {
      db.begin(false).expect("begin");
      let links = db.define_etype("LINKS").expect("etype");
      let tag = db.define_label("Tag").expect("label");
      let weight = db.define_propkey("weight").expect("propkey");
      let a = db.create_node(Some("a")).expect("a");
      let b = db.create_node(Some("b")).expect("b");
      commit(db);
      ids.set((a, links, b, tag, weight));
    },
    |db| {
      let (a, links, b, tag, weight) = ids.get();
      db.begin(false).expect("begin writes");
      db.set_node_prop(b, weight, PropValue::I64(7))
        .expect("prop on b");
      db.add_node_label(b, tag).expect("label on b");
      db.add_edge(a, links, b).expect("edge a->b");
      db.set_edge_prop(a, links, b, weight, PropValue::I64(9))
        .expect("edge prop");
      commit(db);
      db.begin(false).expect("begin delete");
      db.delete_node(b).expect("delete b");
      commit(db)
    },
  );

  let (a, links, b, _, weight) = ids.get();
  assert!(!replica.node_exists(b));
  assert_eq!(replica.node_prop(b, weight), None);
  assert!(replica.node_labels(b).is_empty());
  assert_eq!(replica.edge_prop(a, links, b, weight), None);
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
fn clean_close_of_buffered_primary_publishes_and_keeps_sidecar_healthy() {
  use kitedb::replication::manifest::ManifestStore;

  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("clean-close-primary.kitedb");
  let primary = open_primary(&primary_path, SyncMode::Normal);
  let mut head = None;
  for i in 0..3 {
    primary.begin(false).expect("begin");
    primary
      .create_node(Some(&format!("close-{i}")))
      .expect("create node");
    head = Some(commit(&primary));
  }
  let head = head.expect("head token");
  // The checkpoint empties the WAL, so reopen has no commit to compare.
  primary.checkpoint().expect("checkpoint");
  close_single_file(primary).expect("close primary");

  let manifest =
    ManifestStore::new(default_replication_sidecar_path(&primary_path).join("manifest.json"))
      .read()
      .expect("read manifest");
  assert_eq!(manifest.head_log_index, head.log_index);

  let primary = open_primary(&primary_path, SyncMode::Normal);
  let status = primary.primary_replication_status().expect("status");
  assert!(
    !status.sidecar_needs_repair,
    "a clean close must not look like lost frames: {:?}",
    status.last_replication_error
  );
  assert_eq!(status.head_log_index, head.log_index);
  close_single_file(primary).expect("close primary");
}

// ============================================================================
// R1 rework: replicas keep their own schema ids and translate the primary's
// ============================================================================

/// On-disk primary-id -> local-id schema map (`replica.rs` `SCHEMA_MAP_FILE_NAME`).
const REPLICA_SCHEMA_MAP_FILE: &str = "replica-schema-map.json";

type NamedProps = BTreeMap<String, PropValue>;
type NamedNode = (Option<String>, BTreeSet<String>, NamedProps);
type NamedState = (
  BTreeMap<u64, NamedNode>,
  BTreeMap<(u64, String, u64), NamedProps>,
);

/// Graph state with every schema id resolved to its name, so databases that
/// hold the same data under different local ids compare equal.
fn named_state(db: &SingleFileDB) -> NamedState {
  let named_props = |props: Option<HashMap<u32, PropValue>>| -> NamedProps {
    props
      .unwrap_or_default()
      .into_iter()
      .map(|(key, value)| (db.propkey_name(key).expect("propkey name"), value))
      .collect()
  };
  let nodes = db
    .list_nodes()
    .into_iter()
    .map(|node_id| {
      let labels = db
        .node_labels(node_id)
        .into_iter()
        .map(|label| db.label_name(label).expect("label name"))
        .collect();
      (
        node_id,
        (
          db.node_key(node_id),
          labels,
          named_props(db.node_props(node_id)),
        ),
      )
    })
    .collect();
  let edges = db
    .list_edges(None)
    .into_iter()
    .map(|edge| {
      let etype = db.etype_name(edge.etype).expect("etype name");
      let props = named_props(db.edge_props(edge.src, edge.etype, edge.dst));
      ((edge.src, etype, edge.dst), props)
    })
    .collect();
  (nodes, edges)
}

/// Define schema names in this exact order, fixing the file's ids before any
/// Kite opens it (Kite defines missing names in HashMap order).
fn predefine_schema(
  path: &Path,
  options: SingleFileOpenOptions,
  labels: &[&str],
  etypes: &[&str],
  propkeys: &[&str],
) {
  let db = open_single_file(path, options).expect("open for schema");
  db.begin(false).expect("begin schema");
  for name in labels {
    db.define_label(name).expect("define label");
  }
  for name in etypes {
    db.define_etype(name).expect("define etype");
  }
  for name in propkeys {
    db.define_propkey(name).expect("define propkey");
  }
  db.commit().expect("commit schema");
  close_single_file(db).expect("close schema db");
}

fn primary_db_options() -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .sync_mode(SyncMode::Full)
    .auto_checkpoint(false)
    .replication_role(ReplicationRole::Primary)
}

fn project_schema(options: KiteOptions) -> KiteOptions {
  options
    .node(
      NodeDef::new("File", "file:")
        .prop(PropDef::string("path"))
        .prop(PropDef::string("language")),
    )
    .node(NodeDef::new("Dir", "dir:").prop(PropDef::string("path")))
    .edge(EdgeDef::new("CONTAINS").prop(PropDef::int("order")))
}

fn open_primary_kite(path: &Path) -> Kite {
  Kite::open(
    path,
    project_schema(KiteOptions::new()).replication_role(ReplicationRole::Primary),
  )
  .expect("open primary kite")
}

fn open_replica_kite(path: &Path, primary_path: &Path) -> Kite {
  Kite::open(
    path,
    project_schema(KiteOptions::new())
      .replication_role(ReplicationRole::Replica)
      .replication_source_db_path(primary_path),
  )
  .expect("open replica kite")
}

/// Primary ids: Dir < File, CONTAINS < LINKS, language < order < path.
fn predefine_primary_order(primary_path: &Path) {
  predefine_schema(
    primary_path,
    primary_db_options(),
    &["Dir", "File"],
    &["CONTAINS", "LINKS"],
    &["language", "order", "path"],
  );
}

/// Replica ids in the opposite order, so no id matches the primary's.
fn predefine_replica_reverse_order(replica_path: &Path, extra_propkeys: &[&str]) {
  let mut propkeys = extra_propkeys.to_vec();
  propkeys.extend(["path", "order", "language"]);
  predefine_schema(
    replica_path,
    SingleFileOpenOptions::new(),
    &["File", "Dir"],
    &["LINKS", "CONTAINS"],
    &propkeys,
  );
}

fn write_project(primary: &mut Kite, suffix: &str) -> (u64, u64) {
  let file = primary
    .create_node(
      "File",
      &format!("a-{suffix}.ts"),
      HashMap::from([
        (
          "path".to_string(),
          PropValue::String(format!("src/a-{suffix}.ts")),
        ),
        (
          "language".to_string(),
          PropValue::String("typescript".into()),
        ),
      ]),
    )
    .expect("create file")
    .id();
  let dir = primary
    .create_node(
      "Dir",
      &format!("src-{suffix}"),
      HashMap::from([(
        "path".to_string(),
        PropValue::String(format!("src-{suffix}")),
      )]),
    )
    .expect("create dir")
    .id();
  primary
    .link_with_props(
      dir,
      "CONTAINS",
      file,
      HashMap::from([("order".to_string(), PropValue::I64(1))]),
    )
    .expect("link");
  (file, dir)
}

#[test]
fn kite_replica_with_own_schema_order_reads_primary_values_by_name() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("kite-order-primary.kitedb");
  let replica_path = dir.path().join("kite-order-replica.kitedb");

  predefine_primary_order(&primary_path);
  let mut primary = open_primary_kite(&primary_path);
  // Playground flow: the replica Kite opens with the same schema before any
  // pull, so it holds local ids for every name.
  predefine_replica_reverse_order(&replica_path, &[]);
  let replica = open_replica_kite(&replica_path, &primary_path);
  assert_ne!(
    replica.raw().propkey_id("path"),
    primary.raw().propkey_id("path"),
    "test setup: local ids must differ from the primary's"
  );

  let (file, _) = write_project(&mut primary, "one");
  catch_up_all(replica.raw()).expect("catch up");

  assert_eq!(
    replica.prop(file, "path"),
    Some(PropValue::String("src/a-one.ts".into()))
  );
  assert_eq!(
    replica.prop(file, "language"),
    Some(PropValue::String("typescript".into()))
  );
  assert_eq!(named_state(replica.raw()), named_state(primary.raw()));

  // A snapshot reseed rebuilds the translation from names.
  replica
    .raw()
    .replica_reseed_from_snapshot()
    .expect("reseed");
  write_project(&mut primary, "two");
  catch_up_all(replica.raw()).expect("catch up after reseed");
  assert_eq!(named_state(replica.raw()), named_state(primary.raw()));

  replica.close().expect("close replica");
  primary.close().expect("close primary");
}

#[test]
fn replica_with_superset_schema_translates_primary_ids() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("superset-primary.kitedb");
  let replica_path = dir.path().join("superset-replica.kitedb");

  predefine_primary_order(&primary_path);
  let mut primary = open_primary_kite(&primary_path);
  // Replica-only names take the low ids the primary uses for its own names.
  predefine_replica_reverse_order(&replica_path, &["owner", "reviewer", "notes"]);
  let replica = open_replica_kite(&replica_path, &primary_path);

  let (file, _) = write_project(&mut primary, "one");
  catch_up_all(replica.raw()).expect("catch up");
  assert_eq!(named_state(replica.raw()), named_state(primary.raw()));
  assert_eq!(
    replica.prop(file, "path"),
    Some(PropValue::String("src/a-one.ts".into()))
  );
  assert_eq!(replica.prop(file, "owner"), None);
  assert!(replica.raw().propkey_id("notes").is_some());

  replica.close().expect("close replica");
  primary.close().expect("close primary");
}

#[test]
fn replica_restart_keeps_schema_translation() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("restart-primary.kitedb");
  let replica_path = dir.path().join("restart-replica.kitedb");

  predefine_primary_order(&primary_path);
  let mut primary = open_primary_kite(&primary_path);
  predefine_replica_reverse_order(&replica_path, &[]);
  let replica = open_replica_kite(&replica_path, &primary_path);
  let (file, _) = write_project(&mut primary, "one");
  catch_up_all(replica.raw()).expect("catch up");
  replica.close().expect("close replica");

  // Frames after the restart carry only ids; the names arrived earlier.
  primary
    .set_prop(file, "language", PropValue::String("rust".into()))
    .expect("update language");
  write_project(&mut primary, "two");

  let replica = open_replica_kite(&replica_path, &primary_path);
  catch_up_all(replica.raw()).expect("catch up after restart");
  assert_eq!(
    replica.prop(file, "language"),
    Some(PropValue::String("rust".into()))
  );
  assert_eq!(named_state(replica.raw()), named_state(primary.raw()));

  replica.close().expect("close replica");
  primary.close().expect("close primary");
}

#[test]
fn replica_without_schema_translation_requires_reseed() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("lost-map-primary.kitedb");
  let replica_path = dir.path().join("lost-map-replica.kitedb");

  predefine_primary_order(&primary_path);
  let mut primary = open_primary_kite(&primary_path);
  predefine_replica_reverse_order(&replica_path, &[]);
  let replica = open_replica_kite(&replica_path, &primary_path);
  let (file, _) = write_project(&mut primary, "one");
  catch_up_all(replica.raw()).expect("catch up");
  replica.close().expect("close replica");

  let map_path = default_replication_sidecar_path(&replica_path).join(REPLICA_SCHEMA_MAP_FILE);
  assert!(map_path.exists(), "the translation must be persisted");
  std::fs::remove_file(&map_path).expect("drop the translation");

  primary
    .set_prop(file, "language", PropValue::String("rust".into()))
    .expect("update language");
  let replica = open_replica_kite(&replica_path, &primary_path);
  let error = catch_up_all(replica.raw()).expect_err("an unknown primary id must not apply");
  assert!(error.to_string().contains("reseed"), "{error}");
  assert!(
    replica
      .raw()
      .replica_replication_status()
      .expect("status")
      .needs_reseed
  );
  assert_eq!(
    replica.prop(file, "language"),
    Some(PropValue::String("typescript".into())),
    "nothing may be applied under an untranslated id"
  );

  replica
    .raw()
    .replica_reseed_from_snapshot()
    .expect("reseed rebuilds the translation");
  assert_eq!(named_state(replica.raw()), named_state(primary.raw()));
  write_project(&mut primary, "two");
  catch_up_all(replica.raw()).expect("catch up after reseed");
  assert_eq!(named_state(replica.raw()), named_state(primary.raw()));

  replica.close().expect("close replica");
  primary.close().expect("close primary");
}

#[test]
fn promotion_announces_schema_and_replica_rebuilds_translation() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("promote-map-primary.kitedb");
  let replica_path = dir.path().join("promote-map-replica.kitedb");

  predefine_primary_order(&primary_path);
  let mut primary = open_primary_kite(&primary_path);
  predefine_replica_reverse_order(&replica_path, &[]);
  let replica = open_replica_kite(&replica_path, &primary_path);
  let (file, _) = write_project(&mut primary, "one");
  catch_up_all(replica.raw()).expect("catch up");
  replica.close().expect("close replica");

  // Stale entries must not survive an epoch change: drop the translation and
  // rely on the new epoch's schema announcement alone.
  let map_path = default_replication_sidecar_path(&replica_path).join(REPLICA_SCHEMA_MAP_FILE);
  std::fs::remove_file(&map_path).expect("drop the translation");

  let epoch = primary
    .raw()
    .primary_promote_to_next_epoch()
    .expect("promote");
  assert_eq!(epoch, 2);
  primary
    .set_prop(file, "language", PropValue::String("rust".into()))
    .expect("update language");
  write_project(&mut primary, "two");

  let replica = open_replica_kite(&replica_path, &primary_path);
  catch_up_all(replica.raw()).expect("catch up across the promotion");
  let status = replica.raw().replica_replication_status().expect("status");
  assert_eq!(status.applied_epoch, 2);
  assert_eq!(
    replica.prop(file, "language"),
    Some(PropValue::String("rust".into()))
  );
  assert_eq!(named_state(replica.raw()), named_state(primary.raw()));

  replica.close().expect("close replica");
  primary.close().expect("close primary");
}

#[test]
fn reseed_recreates_nodes_whose_keys_were_swapped() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("swap-primary.kitedb");
  let replica_path = dir.path().join("swap-replica.kitedb");
  let primary = open_primary(&primary_path, SyncMode::Full);
  let replica = open_replica(&replica_path, &primary_path);
  replica
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap empty primary");

  primary.begin(false).expect("begin");
  let n1 = primary.create_node(Some("a")).expect("n1");
  let n2 = primary.create_node(Some("b")).expect("n2");
  let n3 = primary.create_node(Some("c")).expect("n3");
  commit(&primary);
  catch_up_all(&replica).expect("catch up");

  // While the replica is not following, n1/n2 exchange keys and n3 changes
  // its key; each id is recreated (keys are immutable on a live node).
  primary.begin(false).expect("begin delete");
  for node in [n1, n2, n3] {
    primary.delete_node(node).expect("delete");
  }
  commit(&primary);
  primary.begin(false).expect("begin recreate");
  primary.create_node_with_id(n1, Some("b")).expect("n1 b");
  primary.create_node_with_id(n2, Some("a")).expect("n2 a");
  primary.create_node_with_id(n3, Some("d")).expect("n3 d");
  commit(&primary);

  replica.replica_reseed_from_snapshot().expect("reseed");
  let expected = [("a", n2), ("b", n1), ("c", 0), ("d", n3)];
  let check = |db: &SingleFileDB| {
    for (key, node) in expected {
      let expected = (node != 0).then_some(node);
      assert_eq!(db.node_by_key(key), expected, "key {key}");
    }
    assert_eq!(graph_state(db), graph_state(&primary));
  };
  check(&replica);

  close_single_file(replica).expect("close replica");
  let replica = open_replica(&replica_path, &primary_path);
  check(&replica);

  close_single_file(replica).expect("close replica");
  close_single_file(primary).expect("close primary");
}
