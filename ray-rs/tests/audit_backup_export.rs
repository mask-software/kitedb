//! Audit lane `backup-export`: reproductions for findings BE1-BE4.
//!
//! BE1: export holds `delta.read()` while re-reading through the public read
//!      API; a writer parked on `delta.write()` (commit or background
//!      checkpoint) makes the recursive read block forever.
//! BE2: `stats()` takes `header.read()` then the WAL buffer lock, while commit
//!      takes the WAL buffer lock then `header.write()`.
//! BE3: backup/restore check `exists()` before appending `.kitedb`, so they can
//!      overwrite a database with `overwrite: false`; restore deletes the
//!      target before copying and ignores the target's open-file lock.
//! BE4: export builds its schema only from the uncommitted delta and import
//!      never sets edge props, so export -> import after a checkpoint loses data.
//!
//! The deadlock tests leak their blocked threads on failure.

use kitedb::backup::{create_backup_single_file, restore_backup, BackupOptions, RestoreOptions};
use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions,
};
use kitedb::export::{
  export_to_object_single, import_from_object_single, ExportOptions, ImportOptions,
};
use kitedb::types::PropValue;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

/// How long each racing workload runs when nothing hangs.
const RUN_FOR: Duration = Duration::from_secs(3);
/// A worker that has not finished by then is treated as deadlocked.
const WATCHDOG: Duration = Duration::from_secs(15);
/// Caps commits so a fast disk cannot fill the WAL (auto-checkpoint is off).
const MAX_COMMITS: usize = 2000;
const SEED_NODES: usize = 300;

fn quiet_options() -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .auto_checkpoint(false)
    .background_checkpoint(false)
}

// =============================================================================
// Watchdog helpers
// =============================================================================

type Done = mpsc::Sender<(&'static str, thread::Result<()>)>;

fn spawn_worker<F>(name: &'static str, done: &Done, work: F)
where
  F: FnOnce() + Send + 'static,
{
  let done = done.clone();
  thread::spawn(move || {
    let result = panic::catch_unwind(AssertUnwindSafe(work));
    let _ = done.send((name, result));
  });
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
  if let Some(message) = payload.downcast_ref::<&str>() {
    message.to_string()
  } else if let Some(message) = payload.downcast_ref::<String>() {
    message.clone()
  } else {
    "<non-string panic>".to_string()
  }
}

fn await_workers(
  done: mpsc::Receiver<(&'static str, thread::Result<()>)>,
  workers: &[&'static str],
  scenario: &str,
  progress: impl Fn() -> String,
) {
  let deadline = Instant::now() + WATCHDOG;
  let mut pending = workers.to_vec();
  while !pending.is_empty() {
    let remaining = deadline.saturating_duration_since(Instant::now());
    match done.recv_timeout(remaining) {
      Ok((name, Ok(()))) => pending.retain(|worker| *worker != name),
      Ok((name, Err(payload))) => {
        panic!(
          "{scenario}: worker `{name}` panicked: {}",
          panic_message(&*payload)
        )
      }
      Err(mpsc::RecvTimeoutError::Timeout) => panic!(
        "deadlock: {scenario}: workers {pending:?} still blocked after {WATCHDOG:?} ({})",
        progress()
      ),
      Err(mpsc::RecvTimeoutError::Disconnected) => {
        panic!("{scenario}: workers {pending:?} vanished without reporting")
      }
    }
  }
}

/// Opens a database with `SEED_NODES` committed nodes, each carrying a key and
/// two props, so one export performs many nested reads.
fn seeded_db(path: &Path) -> Arc<SingleFileDB> {
  let db = open_single_file(path, quiet_options()).expect("open");
  db.begin(false).expect("seed begin");
  let name = db.define_propkey("name").expect("define name");
  let rank = db.define_propkey("rank").expect("define rank");
  let follows = db.define_etype("FOLLOWS").expect("define FOLLOWS");
  let mut previous = None;
  for index in 0..SEED_NODES {
    let node = db
      .create_node(Some(&format!("seed:{index}")))
      .expect("seed node");
    db.set_node_prop(node, name, PropValue::String(format!("node {index}")))
      .expect("seed name");
    db.set_node_prop(node, rank, PropValue::I64(index as i64))
      .expect("seed rank");
    if let Some(prev) = previous {
      db.add_edge(prev, follows, node).expect("seed edge");
    }
    previous = Some(node);
  }
  db.commit().expect("seed commit");
  Arc::new(db)
}

/// Commits small transactions until `stop` is set or `MAX_COMMITS` is reached.
fn commit_loop(db: &SingleFileDB, stop: &AtomicBool, commits: &AtomicUsize) {
  let rank = db.propkey_id("rank").expect("rank propkey");
  for index in 0..MAX_COMMITS {
    if stop.load(Ordering::Acquire) {
      break;
    }
    db.begin(false).expect("writer begin");
    let node = db
      .create_node(Some(&format!("writer:{index}")))
      .expect("writer create_node");
    db.set_node_prop(node, rank, PropValue::I64(index as i64))
      .expect("writer set_node_prop");
    db.commit().expect("writer commit");
    commits.fetch_add(1, Ordering::Relaxed);
  }
}

// =============================================================================
// BE1: export vs. concurrent writer
// =============================================================================

fn export_loop(db: &SingleFileDB, exports: &AtomicUsize) {
  let started = Instant::now();
  while started.elapsed() < RUN_FOR {
    let exported = export_to_object_single(db, ExportOptions::default()).expect("export");
    assert!(exported.nodes.len() >= SEED_NODES);
    exports.fetch_add(1, Ordering::Relaxed);
  }
}

#[test]
fn audit_be1_export_vs_concurrent_commit_deadlock() {
  let dir = tempfile::tempdir().expect("tempdir");
  let db = seeded_db(&dir.path().join("be1-commit.kitedb"));
  let stop = Arc::new(AtomicBool::new(false));
  let commits = Arc::new(AtomicUsize::new(0));
  let exports = Arc::new(AtomicUsize::new(0));
  let (done_tx, done_rx) = mpsc::channel();

  {
    let (db, stop, exports) = (Arc::clone(&db), Arc::clone(&stop), Arc::clone(&exports));
    spawn_worker("exporter", &done_tx, move || {
      export_loop(&db, &exports);
      stop.store(true, Ordering::Release);
    });
  }
  {
    let (db, stop, commits) = (Arc::clone(&db), Arc::clone(&stop), Arc::clone(&commits));
    spawn_worker("committer", &done_tx, move || {
      commit_loop(&db, &stop, &commits)
    });
  }

  await_workers(
    done_rx,
    &["exporter", "committer"],
    "export_to_object_single racing commits",
    || {
      format!(
        "exports={}, commits={}",
        exports.load(Ordering::Relaxed),
        commits.load(Ordering::Relaxed)
      )
    },
  );
  assert!(exports.load(Ordering::Relaxed) > 0, "no export completed");
  assert!(commits.load(Ordering::Relaxed) > 0, "no commit completed");
}

#[test]
fn audit_be1_export_vs_background_checkpoint_deadlock() {
  let dir = tempfile::tempdir().expect("tempdir");
  let db = seeded_db(&dir.path().join("be1-checkpoint.kitedb"));
  let stop = Arc::new(AtomicBool::new(false));
  let checkpoints = Arc::new(AtomicUsize::new(0));
  let exports = Arc::new(AtomicUsize::new(0));
  let (done_tx, done_rx) = mpsc::channel();

  {
    let (db, stop, exports) = (Arc::clone(&db), Arc::clone(&stop), Arc::clone(&exports));
    spawn_worker("exporter", &done_tx, move || {
      export_loop(&db, &exports);
      stop.store(true, Ordering::Release);
    });
  }
  {
    let (db, stop, checkpoints) = (Arc::clone(&db), Arc::clone(&stop), Arc::clone(&checkpoints));
    spawn_worker("checkpointer", &done_tx, move || {
      while !stop.load(Ordering::Acquire) {
        db.background_checkpoint().expect("background_checkpoint");
        checkpoints.fetch_add(1, Ordering::Relaxed);
      }
    });
  }

  await_workers(
    done_rx,
    &["exporter", "checkpointer"],
    "export_to_object_single racing background_checkpoint",
    || {
      format!(
        "exports={}, checkpoints={}",
        exports.load(Ordering::Relaxed),
        checkpoints.load(Ordering::Relaxed)
      )
    },
  );
  assert!(exports.load(Ordering::Relaxed) > 0, "no export completed");
  assert!(
    checkpoints.load(Ordering::Relaxed) > 0,
    "no checkpoint completed"
  );
}

// =============================================================================
// BE2: stats() vs. concurrent commit
// =============================================================================

#[test]
fn audit_be2_stats_vs_concurrent_commit_deadlock() {
  let dir = tempfile::tempdir().expect("tempdir");
  let db = seeded_db(&dir.path().join("be2-stats.kitedb"));
  let stop = Arc::new(AtomicBool::new(false));
  let commits = Arc::new(AtomicUsize::new(0));
  let scrapes = Arc::new(AtomicUsize::new(0));
  let (done_tx, done_rx) = mpsc::channel();

  {
    let (db, stop, scrapes) = (Arc::clone(&db), Arc::clone(&stop), Arc::clone(&scrapes));
    spawn_worker("stats", &done_tx, move || {
      let started = Instant::now();
      while started.elapsed() < RUN_FOR {
        let stats = db.stats();
        assert!(stats.snapshot_nodes as usize + stats.delta_nodes_created >= SEED_NODES);
        scrapes.fetch_add(1, Ordering::Relaxed);
      }
      stop.store(true, Ordering::Release);
    });
  }
  {
    let (db, stop, commits) = (Arc::clone(&db), Arc::clone(&stop), Arc::clone(&commits));
    spawn_worker("committer", &done_tx, move || {
      commit_loop(&db, &stop, &commits)
    });
  }

  await_workers(
    done_rx,
    &["stats", "committer"],
    "stats() racing commits",
    || {
      format!(
        "stats calls={}, commits={}",
        scrapes.load(Ordering::Relaxed),
        commits.load(Ordering::Relaxed)
      )
    },
  );
  assert!(
    scrapes.load(Ordering::Relaxed) > 0,
    "no stats() call completed"
  );
  assert!(commits.load(Ordering::Relaxed) > 0, "no commit completed");
}

// =============================================================================
// BE3: backup / restore must never clobber an existing database
// =============================================================================

/// Creates a closed single-file database holding one node keyed `key`.
fn write_db_file(path: &Path, key: &str) {
  let db = open_single_file(path, quiet_options()).expect("open fixture db");
  db.begin(false).expect("fixture begin");
  db.create_node(Some(key)).expect("fixture node");
  db.commit().expect("fixture commit");
  close_single_file(db).expect("close fixture db");
}

fn read_bytes(path: &Path) -> Option<Vec<u8>> {
  fs::read(path).ok()
}

fn describe_target(before: &[u8], after: &Option<Vec<u8>>) -> &'static str {
  match after {
    None => "was deleted",
    Some(bytes) if bytes.as_slice() == before => "is unchanged",
    Some(_) => "was overwritten",
  }
}

#[test]
fn audit_be3_restore_without_extension_refuses_to_overwrite_existing_kitedb() {
  let dir = tempfile::tempdir().expect("tempdir");
  let existing = dir.path().join("prod.kitedb");
  let backup = dir.path().join("backup.kitedb");
  write_db_file(&existing, "prod-node");
  write_db_file(&backup, "backup-node");
  let before = fs::read(&existing).expect("read existing");
  assert_ne!(before, fs::read(&backup).expect("read backup"));

  let result = restore_backup(
    &backup,
    dir.path().join("prod"),
    RestoreOptions { overwrite: false },
  );
  let after = read_bytes(&existing);

  assert!(
    result.is_err(),
    "restore_backup(backup, \"<dir>/prod\", overwrite=false) returned {result:?} \
     although <dir>/prod.kitedb exists; prod.kitedb {}",
    describe_target(&before, &after)
  );
  assert!(
    after.as_deref() == Some(before.as_slice()),
    "existing prod.kitedb {} by a refused restore",
    describe_target(&before, &after)
  );
}

#[test]
fn audit_be3_create_backup_without_extension_refuses_to_overwrite_existing_kitedb() {
  let dir = tempfile::tempdir().expect("tempdir");
  let existing = dir.path().join("prod.kitedb");
  write_db_file(&existing, "prod-node");
  let before = fs::read(&existing).expect("read existing");

  let db = open_single_file(dir.path().join("source.kitedb"), quiet_options()).expect("open");
  db.begin(false).expect("begin");
  db.create_node(Some("source-node")).expect("source node");
  db.commit().expect("commit");

  let result = create_backup_single_file(
    &db,
    dir.path().join("prod"),
    BackupOptions {
      checkpoint: true,
      overwrite: false,
    },
  );
  let after = read_bytes(&existing);
  close_single_file(db).expect("close source");

  assert!(
    result.is_err(),
    "create_backup_single_file(db, \"<dir>/prod\", overwrite=false) returned {:?} \
     although <dir>/prod.kitedb exists; prod.kitedb {}",
    result.as_ref().map(|r| &r.path),
    describe_target(&before, &after)
  );
  assert!(
    after.as_deref() == Some(before.as_slice()),
    "existing prod.kitedb {} by a refused backup",
    describe_target(&before, &after)
  );
}

#[test]
fn audit_be3_restore_refuses_target_that_is_open() {
  let dir = tempfile::tempdir().expect("tempdir");
  let live_path = dir.path().join("live.kitedb");
  let backup = dir.path().join("backup.kitedb");
  write_db_file(&backup, "backup-node");

  let live = open_single_file(&live_path, quiet_options()).expect("open live");
  live.begin(false).expect("begin");
  live.create_node(Some("live-node")).expect("live node");
  live.commit().expect("commit");
  let before = fs::read(&live_path).expect("read live");

  let result = restore_backup(&backup, &live_path, RestoreOptions { overwrite: true });
  let after = read_bytes(&live_path);
  close_single_file(live).expect("close live");

  assert!(
    result.is_err(),
    "restore_backup onto a database that is open (and file-locked) returned {result:?}; \
     live.kitedb {} underneath the open handle",
    describe_target(&before, &after)
  );
  assert!(
    after.as_deref() == Some(before.as_slice()),
    "open live.kitedb {} by a refused restore",
    describe_target(&before, &after)
  );
}

#[cfg(unix)]
#[test]
fn audit_be3_failed_restore_keeps_existing_target() {
  use std::os::unix::fs::PermissionsExt;

  let dir = tempfile::tempdir().expect("tempdir");
  let existing = dir.path().join("prod.kitedb");
  let backup = dir.path().join("backup.kitedb");
  write_db_file(&existing, "prod-node");
  write_db_file(&backup, "backup-node");
  let before = fs::read(&existing).expect("read existing");

  // An unreadable backup makes the copy fail after restore has started.
  fs::set_permissions(&backup, fs::Permissions::from_mode(0o000)).expect("chmod 000");
  if fs::File::open(&backup).is_ok() {
    eprintln!("skipping: running with privileges that ignore file permissions");
    return;
  }
  let result = restore_backup(&backup, &existing, RestoreOptions { overwrite: true });
  fs::set_permissions(&backup, fs::Permissions::from_mode(0o644)).expect("chmod 644");
  let after = read_bytes(&existing);

  assert!(
    result.is_err(),
    "restore from an unreadable backup returned {result:?}"
  );
  assert!(
    after.as_deref() == Some(before.as_slice()),
    "a failed restore must leave the existing database intact, but prod.kitedb {} \
     (restore error: {:?})",
    describe_target(&before, &after),
    result.err()
  );
}

// =============================================================================
// BE4: export -> import round trip
// =============================================================================

type NodeDump = BTreeMap<String, BTreeMap<String, String>>;
type EdgeDump = BTreeSet<(String, String, String, BTreeMap<String, String>)>;

fn props_by_name(
  db: &SingleFileDB,
  props: Option<std::collections::HashMap<u32, PropValue>>,
) -> BTreeMap<String, String> {
  props
    .unwrap_or_default()
    .into_iter()
    .map(|(key_id, value)| {
      let name = db
        .propkey_name(key_id)
        .unwrap_or_else(|| format!("<unnamed propkey {key_id}>"));
      (name, format!("{value:?}"))
    })
    .collect()
}

/// Name-based view of a database: node key -> props, and edges as
/// (src key, etype name, dst key, props), independent of numeric ids.
fn dump(db: &SingleFileDB) -> (NodeDump, EdgeDump) {
  let node_key = |id| {
    db.node_key(id)
      .unwrap_or_else(|| format!("<unkeyed node {id}>"))
  };
  let nodes = db
    .list_nodes()
    .into_iter()
    .map(|id| (node_key(id), props_by_name(db, db.node_props(id))))
    .collect();
  let edges = db
    .list_edges(None)
    .into_iter()
    .map(|edge| {
      let etype = db
        .etype_name(edge.etype)
        .unwrap_or_else(|| format!("<unnamed etype {}>", edge.etype));
      (
        node_key(edge.src),
        etype,
        node_key(edge.dst),
        props_by_name(db, db.edge_props(edge.src, edge.etype, edge.dst)),
      )
    })
    .collect();
  (nodes, edges)
}

fn build_graph(db: &SingleFileDB) {
  db.begin(false).expect("begin");
  let name = db.define_propkey("name").expect("name");
  let age = db.define_propkey("age").expect("age");
  let since = db.define_propkey("since").expect("since");
  let weight = db.define_propkey("weight").expect("weight");
  let knows = db.define_etype("KNOWS").expect("KNOWS");
  let likes = db.define_etype("LIKES").expect("LIKES");

  let alice = db.create_node(Some("user:alice")).expect("alice");
  let bob = db.create_node(Some("user:bob")).expect("bob");
  let carol = db.create_node(Some("user:carol")).expect("carol");
  for (node, label, years) in [(alice, "Alice", 30), (bob, "Bob", 41), (carol, "Carol", 27)] {
    db.set_node_prop(node, name, PropValue::String(label.to_string()))
      .expect("name prop");
    db.set_node_prop(node, age, PropValue::I64(years))
      .expect("age prop");
  }

  db.add_edge(alice, knows, bob).expect("alice knows bob");
  db.set_edge_prop(alice, knows, bob, since, PropValue::I64(2020))
    .expect("since prop");
  db.set_edge_prop(alice, knows, bob, weight, PropValue::F64(0.5))
    .expect("weight prop");
  db.add_edge(bob, likes, carol).expect("bob likes carol");
  db.set_edge_prop(bob, likes, carol, since, PropValue::I64(2021))
    .expect("since prop");
  db.commit().expect("commit");
}

fn assert_round_trip(checkpoint_before_export: bool) {
  let dir = tempfile::tempdir().expect("tempdir");
  let source = open_single_file(dir.path().join("source.kitedb"), quiet_options()).expect("open");
  build_graph(&source);
  if checkpoint_before_export {
    source.checkpoint().expect("checkpoint");
  }
  let expected = dump(&source);
  let exported = export_to_object_single(&source, ExportOptions::default()).expect("export");
  close_single_file(source).expect("close source");

  let target = open_single_file(dir.path().join("target.kitedb"), quiet_options()).expect("open");
  let imported = import_from_object_single(&target, &exported, ImportOptions::default())
    .expect("import into a fresh database");
  let actual = dump(&target);
  close_single_file(target).expect("close target");

  assert_eq!(
    (imported.node_count, imported.edge_count),
    (expected.0.len(), expected.1.len()),
    "import counts"
  );
  assert!(
    actual == expected,
    "export -> import (checkpoint before export: {checkpoint_before_export}) lost data\n\
     exported schema: {:?}\n\
     source nodes:   {:?}\n\
     imported nodes: {:?}\n\
     source edges:   {:?}\n\
     imported edges: {:?}",
    exported.schema,
    expected.0,
    actual.0,
    expected.1,
    actual.1,
  );
}

#[test]
fn audit_be4_export_import_round_trip_after_checkpoint() {
  assert_round_trip(true);
}

/// Isolates the edge-prop half of BE4: without a checkpoint the schema is
/// still in the delta, but import never writes edge props.
#[test]
fn audit_be4_import_restores_edge_props() {
  assert_round_trip(false);
}
