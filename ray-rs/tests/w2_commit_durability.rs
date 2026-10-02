//! Wave-2 commit-durability reproductions through the public API: D4 (vector
//! dimension race), D5 (MVCC transaction leaks on error paths), and D6
//! (replicas accepting local data writes). The D1-D4 reproductions that need
//! crate-private access live in
//! `src/core/single_file/w2_commit_durability_tests.rs`.
//!
//! On 39fefea the D6 rejection test fails; the D4 race and D5 tests pass
//! (those findings were already fixed) and guard the fixes; the remaining D6
//! tests guard what a replica-write check must keep working.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Barrier};

use kitedb::api::kite::{Kite, KiteOptions, NodeDef, PropDef};
use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions, SyncMode,
};
use kitedb::replication::types::ReplicationRole;
use kitedb::types::PropValue;
use kitedb::KiteError;

// ---------------------------------------------------------------------------
// D4: two transactions give a new vector property different dimensions.
// ---------------------------------------------------------------------------

/// D4 [high]. Two threads each stage a vector for a new property key (3 and
/// 4 dimensions) before either commits, then commit at once. Exactly one may
/// win; the loser's commit must fail before its COMMIT record is durable, so
/// a crash image and a clean reopen both open, without the loser's node.
#[test]
fn d4_concurrent_vector_dimensions_one_wins_and_reopen_succeeds() {
  for round in 0..20 {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join(format!("d4-race-{round}.kitedb"));
    // Write transactions open at once need MVCC (without it they run one at a time).
    let options = SingleFileOpenOptions::new()
      .mvcc(true)
      .mvcc_gc_interval_ms(10)
      .auto_checkpoint(false);
    let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));
    db.begin(false).expect("begin");
    let embedding = db.define_propkey("embedding").expect("propkey");
    db.commit().expect("commit");

    let staged = Arc::new(Barrier::new(2));
    let writers: Vec<_> = [("three", 3usize), ("four", 4usize)]
      .into_iter()
      .map(|(key, dimensions)| {
        let db = Arc::clone(&db);
        let staged = Arc::clone(&staged);
        std::thread::spawn(move || {
          db.begin(false).expect("begin");
          let node = db.create_node(Some(key)).expect("create");
          db.set_node_vector(node, embedding, &vec![0.25; dimensions])
            .expect("stage vector");
          staged.wait();
          (key, dimensions, db.commit())
        })
      })
      .collect();
    let results: Vec<_> = writers
      .into_iter()
      .map(|writer| writer.join().expect("writer thread"))
      .collect();

    let winners: Vec<_> = results.iter().filter(|(_, _, r)| r.is_ok()).collect();
    assert_eq!(
      winners.len(),
      1,
      "round {round}: exactly one dimension must win: {results:?}"
    );
    let (winner_key, winner_dims, _) = winners[0];
    let (loser_key, _, loser_result) = results
      .iter()
      .find(|(_, _, r)| r.is_err())
      .expect("a loser");
    assert!(
      matches!(loser_result, Err(KiteError::VectorDimensionMismatch { .. })),
      "round {round}: loser error {loser_result:?}"
    );

    let check = |what: &str, opened: &SingleFileDB| {
      let winner = opened
        .node_by_key(winner_key)
        .unwrap_or_else(|| panic!("round {round} {what}: winner {winner_key} missing"));
      assert_eq!(
        opened.node_vector(winner, embedding).map(|v| v.len()),
        Some(*winner_dims),
        "round {round} {what}: winner vector"
      );
      assert!(
        opened.node_by_key(loser_key).is_none(),
        "round {round} {what}: the refused commit {loser_key} is present"
      );
    };
    check("live", &db);

    let image = db_path.with_extension("crash.kitedb");
    std::fs::copy(&db_path, &image).expect("copy");
    let crashed = open_single_file(&image, options.clone())
      .unwrap_or_else(|e| panic!("round {round}: crash image unopenable: {e:?}"));
    check("crash image", &crashed);
    drop(crashed);

    let db = Arc::try_unwrap(db).ok().expect("sole owner");
    close_single_file(db).expect("close");
    let reopened = open_single_file(&db_path, options)
      .unwrap_or_else(|e| panic!("round {round}: reopen failed: {e:?}"));
    check("reopen", &reopened);
  }
}

// ---------------------------------------------------------------------------
// D5: MVCC transactions leaked by failed begins / fenced commits.
// ---------------------------------------------------------------------------

fn active_mvcc_transactions(db: &SingleFileDB) -> usize {
  db.stats()
    .mvcc_stats
    .expect("MVCC enabled")
    .active_transactions
}

/// D5 [medium]. A begin whose BEGIN record the full WAL refuses must not
/// leave its MVCC transaction active (that would pin min_active_ts and stop
/// GC for good).
#[test]
fn d5_begin_refused_by_full_wal_leaves_no_active_mvcc_transaction() {
  let dir = tempfile::tempdir().expect("tempdir");
  let db_path = dir.path().join("d5-full-wal.kitedb");
  let db = open_single_file(
    &db_path,
    SingleFileOpenOptions::new()
      .wal_size(64 * 1024)
      .auto_checkpoint(false)
      .mvcc(true)
      .mvcc_gc_interval_ms(10),
  )
  .expect("open");

  // Fill the WAL with commits, then with begins (each commit of an empty
  // transaction that no longer fits fails and aborts), until begin itself
  // is refused.
  let big_key = "k".repeat(1000);
  let mut n = 0;
  loop {
    db.begin(false).expect("begin while filling");
    let created = db.create_node(Some(&format!("{big_key}-{n}")));
    let committed = created.and_then(|_| db.commit());
    n += 1;
    match committed {
      Ok(()) => {}
      Err(KiteError::WalBufferFull) => {
        if db.has_transaction() {
          let _ = db.rollback();
        }
        break;
      }
      Err(error) => panic!("unexpected fill error: {error:?}"),
    }
    assert!(n < 10_000, "WAL never filled");
  }
  let mut begin_refused = false;
  for _ in 0..10_000 {
    match db.begin(false) {
      Ok(_) => {
        let _ = db.commit();
        if db.has_transaction() {
          let _ = db.rollback();
        }
      }
      Err(KiteError::WalBufferFull) => {
        begin_refused = true;
        break;
      }
      Err(error) => panic!("unexpected begin error: {error:?}"),
    }
  }
  assert!(
    begin_refused,
    "test setup: the WAL never refused a BEGIN record"
  );
  assert!(!db.has_transaction());

  let stats = db.stats().mvcc_stats.expect("MVCC enabled");
  assert_eq!(
    stats.active_transactions, 0,
    "a refused begin leaked an active MVCC transaction (min_active_ts={})",
    stats.min_active_ts
  );

  // GC can make progress again once the WAL has room.
  db.checkpoint().expect("checkpoint");
  db.begin(false).expect("begin after checkpoint");
  db.create_node(Some("after")).expect("create");
  db.commit().expect("commit after checkpoint");
  assert_eq!(active_mvcc_transactions(&db), 0);
}

/// D5 [medium]. A commit refused by the stale-primary fence (after its
/// transaction handle was removed) must abort its MVCC transaction.
#[test]
fn d5_fenced_commit_leaves_no_active_mvcc_transaction() {
  let dir = tempfile::tempdir().expect("tempdir");
  let db_path = dir.path().join("d5-fenced.kitedb");
  let sidecar = dir.path().join("d5-fenced.sidecar");
  let open_primary = || {
    open_single_file(
      &db_path,
      SingleFileOpenOptions::new()
        .sync_mode(SyncMode::Full)
        .mvcc(true)
        .mvcc_gc_interval_ms(10)
        .replication_role(ReplicationRole::Primary)
        .replication_sidecar_path(&sidecar)
        .replication_segment_max_bytes(256)
        .replication_retention_min_entries(4)
        .danger_bypass_file_lock_for_multi_node_simulation(true),
    )
    .expect("open primary")
  };
  let stale = open_primary();
  let promoted = open_primary();

  stale.begin(false).expect("begin");
  stale.create_node(Some("a0")).expect("create");
  stale.commit().expect("commit before promotion");
  promoted.primary_promote_to_next_epoch().expect("promote");

  stale.begin(false).expect("begin stale");
  stale.create_node(Some("stale")).expect("create stale");
  let error = stale.commit().expect_err("stale primary commit must fail");
  assert!(
    error.to_string().contains("stale primary"),
    "unexpected error: {error}"
  );
  assert!(!stale.has_transaction());
  assert_eq!(
    active_mvcc_transactions(&stale),
    0,
    "a fenced commit leaked an active MVCC transaction"
  );

  close_single_file(promoted).expect("close promoted");
  close_single_file(stale).expect("close stale");
}

// ---------------------------------------------------------------------------
// D6: replicas accept local data writes.
// ---------------------------------------------------------------------------

fn open_primary(path: &Path) -> SingleFileDB {
  open_single_file(
    path,
    SingleFileOpenOptions::new()
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

fn catch_up_all(replica: &SingleFileDB) -> usize {
  let mut total = 0;
  loop {
    let applied = replica.replica_catch_up_once(64).expect("catch up");
    if applied == 0 {
      return total;
    }
    total += applied;
  }
}

/// Primary with nodes `p0 -[E]-> p1` and `p0.name = "zero"`, and a
/// bootstrapped replica.
fn primary_and_replica(dir: &Path) -> (SingleFileDB, SingleFileDB) {
  let primary_path = dir.join("d6-primary.kitedb");
  let replica_path = dir.join("d6-replica.kitedb");
  let primary = open_primary(&primary_path);
  primary.begin(false).expect("begin");
  let p0 = primary.create_node(Some("p0")).expect("p0");
  let p1 = primary.create_node(Some("p1")).expect("p1");
  let etype = primary.define_etype("E").expect("etype");
  let name = primary.define_propkey("name").expect("propkey");
  primary.add_edge(p0, etype, p1).expect("edge");
  primary
    .set_node_prop(p0, name, PropValue::String("zero".into()))
    .expect("prop");
  primary.commit().expect("commit");

  let replica = open_replica(&replica_path, &primary_path);
  replica
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap");
  assert!(replica.node_by_key("p0").is_some());
  (primary, replica)
}

fn is_replica_write_rejection(error: &KiteError) -> bool {
  matches!(error, KiteError::ReadOnly) || error.to_string().to_lowercase().contains("replica")
}

/// D6 [high]. Data writes on a replica-role database must fail with a clear
/// ReadOnly-style error, and leave nothing behind (live or after reopen). On
/// 39fefea every one of these local writes commits.
#[test]
fn d6_replica_rejects_local_data_writes() {
  let dir = tempfile::tempdir().expect("tempdir");
  let (primary, replica) = primary_and_replica(dir.path());
  let p0 = replica.node_by_key("p0").expect("p0");
  let p1 = replica.node_by_key("p1").expect("p1");
  let etype = replica.etype_id("E").expect("etype");
  let name = replica.propkey_id("name").expect("propkey");
  let edges_before = replica.count_edges();

  // Run `op` in its own transaction; the write must fail at begin, at the op,
  // or at commit.
  let attempt = |op: &dyn Fn(&SingleFileDB) -> kitedb::Result<()>| -> kitedb::Result<()> {
    replica.begin(false)?;
    let result = op(&replica).and_then(|()| replica.commit());
    if replica.has_transaction() {
      let _ = replica.rollback();
    }
    result
  };
  let outcomes: Vec<(&str, kitedb::Result<()>)> = vec![
    (
      "create_node",
      attempt(&|db| db.create_node(Some("local")).map(|_| ())),
    ),
    (
      "set_node_prop",
      attempt(&|db| db.set_node_prop(p0, name, PropValue::String("local".into()))),
    ),
    ("delete_edge", attempt(&|db| db.delete_edge(p0, etype, p1))),
    ("add_edge", attempt(&|db| db.add_edge(p1, etype, p0))),
    ("delete_node", attempt(&|db| db.delete_node(p1))),
  ];

  let accepted: Vec<&str> = outcomes
    .iter()
    .filter(|(_, result)| result.is_ok())
    .map(|(what, _)| *what)
    .collect();
  let unclear: Vec<String> = outcomes
    .iter()
    .filter_map(|(what, result)| match result {
      Err(error) if !is_replica_write_rejection(error) => Some(format!("{what}: {error:?}")),
      _ => None,
    })
    .collect();
  assert!(
    accepted.is_empty(),
    "replica accepted local data writes: {accepted:?}"
  );
  assert!(
    unclear.is_empty(),
    "replica rejected writes without a ReadOnly-style error: {unclear:?}"
  );

  let verify = |what: &str, db: &SingleFileDB| {
    assert!(db.node_by_key("local").is_none(), "{what}: local node");
    let p0 = db.node_by_key("p0").expect("p0");
    let p1 = db.node_by_key("p1").expect("p1");
    assert_eq!(
      db.node_prop(p0, name),
      Some(PropValue::String("zero".into())),
      "{what}: p0.name"
    );
    assert!(db.edge_exists(p0, etype, p1), "{what}: p0->p1");
    assert!(!db.edge_exists(p1, etype, p0), "{what}: p1->p0");
    assert_eq!(db.count_edges(), edges_before, "{what}: edges");
  };
  verify("live", &replica);
  close_single_file(replica).expect("close replica");
  let reopened = open_replica(
    &dir.path().join("d6-replica.kitedb"),
    &dir.path().join("d6-primary.kitedb"),
  );
  verify("reopen", &reopened);
  close_single_file(reopened).expect("close");
  close_single_file(primary).expect("close primary");
}

/// D6 guard: a replica must still accept schema-definition-only transactions
/// (Kite defines missing names locally; replicas translate ids by name), and
/// its own replication apply paths (catch-up, reseed) must keep writing.
#[test]
fn d6_replica_schema_defines_and_apply_paths_still_work() {
  let dir = tempfile::tempdir().expect("tempdir");
  let (primary, replica) = primary_and_replica(dir.path());

  replica.begin(false).expect("begin schema tx");
  replica
    .define_label("ReplicaOnlyLabel")
    .expect("define label");
  replica.define_etype("REPLICA_ONLY").expect("define etype");
  replica
    .define_propkey("replica_only_prop")
    .expect("define propkey");
  replica.commit().expect("schema-only commit on a replica");
  assert!(replica.propkey_id("replica_only_prop").is_some());

  primary.begin(false).expect("begin");
  let p2 = primary.create_node(Some("p2")).expect("p2");
  let name = primary.propkey_id("name").expect("name");
  primary
    .set_node_prop(p2, name, PropValue::String("two".into()))
    .expect("prop");
  primary.commit().expect("commit");
  assert!(catch_up_all(&replica) > 0, "catch-up applied nothing");
  let replica_p2 = replica.node_by_key("p2").expect("p2 replicated");
  let replica_name = replica.propkey_id("name").expect("name");
  assert_eq!(
    replica.node_prop(replica_p2, replica_name),
    Some(PropValue::String("two".into()))
  );

  replica
    .replica_reseed_from_snapshot()
    .expect("reseed on a replica");
  assert!(replica.node_by_key("p2").is_some());

  close_single_file(replica).expect("close replica");
  close_single_file(primary).expect("close primary");
}

/// D6 guard: a Kite opened on a fresh replica defines its schema's names
/// locally at open, then reads replicated values by name.
#[test]
fn d6_kite_opened_on_replica_defines_missing_schema() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("d6-kite-primary.kitedb");
  let replica_path = dir.path().join("d6-kite-replica.kitedb");
  let schema = |options: KiteOptions| {
    options.node(
      NodeDef::new("Doc", "doc:")
        .prop(PropDef::string("title"))
        .prop(PropDef::int("rank")),
    )
  };
  let mut primary = Kite::open(
    &primary_path,
    schema(KiteOptions::new()).replication_role(ReplicationRole::Primary),
  )
  .expect("open primary kite");
  let replica = Kite::open(
    &replica_path,
    schema(KiteOptions::new())
      .replication_role(ReplicationRole::Replica)
      .replication_source_db_path(&primary_path),
  )
  .expect("Kite must open on a replica (it defines missing schema names)");
  assert!(replica.raw().propkey_id("title").is_some());

  let doc = primary
    .create_node(
      "Doc",
      "one",
      HashMap::from([("title".to_string(), PropValue::String("One".into()))]),
    )
    .expect("create doc")
    .id();
  replica
    .raw()
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap");
  catch_up_all(replica.raw());
  assert_eq!(
    replica.prop(doc, "title"),
    Some(PropValue::String("One".into()))
  );

  replica.close().expect("close replica");
  primary.close().expect("close primary");
}
