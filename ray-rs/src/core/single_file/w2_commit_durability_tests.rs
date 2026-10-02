//! Wave-2 commit-durability reproductions that need crate-private access
//! (header, pager, WAL buffer, raw WAL records) or the commit test hooks in
//! `transaction.rs`. Public-API reproductions (D4 race, D5, D6) live in
//! `tests/w2_commit_durability.rs`.
//!
//! Each test fails on 39fefea for the reason in its doc comment.
use super::*;
use crate::core::single_file::{open_single_file, SingleFileOpenOptions};
use crate::core::wal::record::build_set_node_vector_payload;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use tempfile::tempdir;

fn set_string_prop(db: &SingleFileDB, node: NodeId, key: PropKeyId, value: &str) {
  db.begin(false).expect("begin");
  db.set_node_prop(node, key, PropValue::String(value.to_string()))
    .expect("set prop");
  db.commit().expect("commit");
}

// ---------------------------------------------------------------------------
// D1: group commit persists the header before the WAL bytes it names.
// ---------------------------------------------------------------------------

/// D1 [critical]. With group commit, `commit_transaction` persists a header
/// whose WAL head covers the new COMMIT record, then `wait_for_group_commit`
/// flushes the WAL bytes (after sleeping `group_commit_window_ms` while other
/// writers are open). A crash image taken in that window has a header naming
/// WAL bytes that are not on disk; the file holds stale records of the
/// previous WAL cycle there, and recovery replays them.
///
/// Scenario: cycle 1 writes p=AAAA then p=BBBB, a checkpoint resets the WAL,
/// cycle 2 writes p=CCCC (acknowledged: commit() returned) at the same offsets
/// as AAAA, then p=DDDD (in flight) at the offsets of BBBB. An image taken
/// while DDDD's commit waits for its group flush must still hold CCCC (or
/// DDDD). On 39fefea it replays the stale BBBB transaction: the acknowledged
/// CCCC is lost.
#[test]
fn d1_group_commit_crash_image_keeps_acknowledged_commit() {
  let temp_dir = tempdir().expect("temp dir");
  let db_path = temp_dir.path().join("d1-group-commit.kitedb");
  let options = SingleFileOpenOptions::new()
    .auto_checkpoint(false)
    .sync_mode(SyncMode::Normal)
    .mvcc(false)
    .group_commit_enabled(true)
    .group_commit_window_ms(400);
  let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));
  db.begin(false).expect("begin");
  let node = db.create_node(Some("n")).expect("node");
  let key = db.define_propkey("p").expect("propkey");
  db.commit().expect("commit");
  db.checkpoint().expect("checkpoint 0");

  // Cycle 1: two same-shape transactions.
  set_string_prop(&db, node, key, "AAAA");
  set_string_prop(&db, node, key, "BBBB");
  db.checkpoint().expect("checkpoint 1");
  assert_eq!(db.wal_stats().primary_head, 0, "WAL should be empty");

  // Cycle 2: CCCC overwrites AAAA's bytes and is acknowledged.
  set_string_prop(&db, node, key, "CCCC");
  let head_after_ack = db.header.read().wal_head;

  // (On 39fefea a bulk transaction held open here made the group-commit
  // leader sleep its window between the header and the WAL flush. Group
  // commit no longer sleeps, and without MVCC a second writer can no longer be
  // open beside DDDD's, so the image is taken right after DDDD's header.)

  let writer_db = Arc::clone(&db);
  let writer = std::thread::spawn(move || {
    writer_db.begin(false).expect("begin");
    writer_db
      .set_node_prop(node, key, PropValue::String("DDDD".to_string()))
      .expect("set prop");
    writer_db.commit()
  });

  // Wait until DDDD's header is persisted (header.read() waits out the
  // write guard held across persist_header), or the commit finished.
  let deadline = Instant::now() + Duration::from_secs(20);
  while db.header.read().wal_head == head_after_ack && !writer.is_finished() {
    assert!(
      Instant::now() < deadline,
      "DDDD's commit never persisted a header"
    );
    std::thread::sleep(Duration::from_micros(200));
  }
  let image = db_path.with_extension("image.kitedb");
  let unflushed_at_image = {
    let _pager = db.pager.lock();
    let unflushed = db.wal_buffer.lock().has_pending_writes();
    std::fs::copy(&db_path, &image).expect("copy image");
    unflushed
  };
  eprintln!(
    "D1: image taken with header wal_head={} and unflushed WAL pages={unflushed_at_image}",
    db.header.read().wal_head
  );
  writer.join().expect("writer thread").expect("DDDD commit");

  let crashed = open_single_file(&image, options.clone().group_commit_enabled(false))
    .expect("crash image taken after an acknowledged group commit must open");
  let value = crashed.node_prop(node, key);
  assert!(
    matches!(&value, Some(PropValue::String(v)) if v == "CCCC" || v == "DDDD"),
    "crash image lost the acknowledged CCCC commit: p = {value:?} (BBBB means stale WAL \
     bytes of the previous cycle were replayed under a header persisted before its WAL flush)"
  );
}

// ---------------------------------------------------------------------------
// D2: a commit failing before its header persists comes back after reopen.
// ---------------------------------------------------------------------------

/// Commit `failed-a` with its header persist failing, then commit `b`.
/// The fault: an in-memory header generation of u64::MAX makes
/// `persist_header` fail (`header generation overflow`) after the COMMIT
/// record is buffered (and, without group commit, flushed): the same window
/// an I/O error on the header write or WAL flush hits.
fn d2_failed_commit_then_next_commit(group_commit: bool) {
  let temp_dir = tempdir().expect("temp dir");
  let db_path = temp_dir.path().join("d2-failed-commit.kitedb");
  let options = SingleFileOpenOptions::new()
    .auto_checkpoint(false)
    .sync_mode(SyncMode::Normal)
    .group_commit_enabled(group_commit)
    .group_commit_window_ms(0);
  let db = open_single_file(&db_path, options.clone()).expect("open");
  db.begin(false).expect("begin");
  db.create_node(Some("base")).expect("base");
  db.commit().expect("commit base");

  db.begin(false).expect("begin A");
  db.create_node(Some("failed-a")).expect("create A");
  let generation = db.header.read().change_counter;
  db.header.write().change_counter = u64::MAX;
  let failed = db.commit();
  db.header.write().change_counter = generation;
  let error = failed.expect_err("injected header-persist failure did not fail the commit");
  eprintln!("D2 (group_commit={group_commit}): A failed with {error:?}");
  assert!(!db.has_transaction());
  assert!(
    db.node_by_key("failed-a").is_none(),
    "a failed commit is visible in this process"
  );

  // The next commit (if the database still accepts one).
  db.begin(false).expect("begin B");
  db.create_node(Some("b")).expect("create B");
  let next = db.commit();
  eprintln!("D2 (group_commit={group_commit}): next commit -> {next:?}");
  if next.is_ok() {
    assert!(db.node_by_key("b").is_some());
  }
  assert!(
    db.node_by_key("failed-a").is_none(),
    "the failed commit became visible after the next commit"
  );
  drop(db);

  let reopened = open_single_file(&db_path, options).expect("reopen");
  assert!(reopened.node_by_key("base").is_some());
  if next.is_ok() {
    assert!(
      reopened.node_by_key("b").is_some(),
      "the next commit was lost"
    );
  }
  assert!(
    reopened.node_by_key("failed-a").is_none(),
    "a commit that returned Err reappeared after the next commit plus reopen \
     (its COMMIT record stayed in the WAL and the next header covered it)"
  );
}

/// D2 [high], plain commit path (COMMIT flushed, then the header persist
/// fails). Fails on 39fefea: `failed-a` reappears after reopen.
#[test]
fn d2_failed_commit_does_not_reappear_after_next_commit() {
  d2_failed_commit_then_next_commit(false);
}

/// D2 [high], group-commit path (COMMIT buffered, header persist fails; the
/// next commit's flush writes it). Fails on 39fefea like the plain path.
#[test]
fn d2_failed_group_commit_does_not_reappear_after_next_commit() {
  d2_failed_commit_then_next_commit(true);
}

// ---------------------------------------------------------------------------
// D3: a transaction beginning during an in-flight commit.
// ---------------------------------------------------------------------------

fn read_count(db: &SingleFileDB, node: NodeId, key: PropKeyId) -> i64 {
  match db.node_prop(node, key) {
    Some(PropValue::I64(value)) => value,
    other => panic!("unexpected count {other:?}"),
  }
}

/// MVCC database with `count = 0`.
fn d3_setup() -> (tempfile::TempDir, Arc<SingleFileDB>, NodeId, PropKeyId) {
  let temp_dir = tempdir().expect("temp dir");
  let db_path = temp_dir.path().join("d3-mvcc.kitedb");
  let options = SingleFileOpenOptions::new()
    .auto_checkpoint(false)
    .mvcc(true)
    .mvcc_gc_interval_ms(10);
  let db = Arc::new(open_single_file(&db_path, options).expect("open"));
  db.begin(false).expect("begin");
  let node = db.create_node(Some("counter")).expect("node");
  let key = db.define_propkey("count").expect("propkey");
  db.set_node_prop(node, key, PropValue::I64(0))
    .expect("set count");
  db.commit().expect("commit");
  (temp_dir, db, node, key)
}

/// Start T1 (count += 1) on another thread and return once its commit is
/// durable but not yet merged into the delta. T1 then waits (at most a second,
/// in case a fix makes the caller's begin wait for it) for `go`.
fn d3_start_t1_and_pause_before_merge(
  db: &Arc<SingleFileDB>,
  node: NodeId,
  key: PropKeyId,
) -> (mpsc::Sender<()>, std::thread::JoinHandle<Result<()>>) {
  let (paused_tx, paused_rx) = mpsc::channel::<()>();
  let (go_tx, go_rx) = mpsc::channel::<()>();
  let t1_db = Arc::clone(db);
  let t1 = std::thread::spawn(move || {
    t1_db.begin(false).expect("T1 begin");
    let value = read_count(&t1_db, node, key);
    t1_db
      .set_node_prop(node, key, PropValue::I64(value + 1))
      .expect("T1 set");
    BEFORE_NEXT_COMMIT_MERGE.with(|hook| {
      *hook.borrow_mut() = Some(Box::new(move || {
        paused_tx.send(()).expect("signal paused");
        let _ = go_rx.recv_timeout(Duration::from_secs(1));
      }));
    });
    t1_db.commit()
  });
  paused_rx
    .recv_timeout(Duration::from_secs(20))
    .expect("T1 never reached the point between its durable commit and its delta merge");
  (go_tx, t1)
}

/// D3 [high, MVCC] lost update. T1 gets its commit timestamp (and
/// `next_commit_ts` advances) before its write reaches the delta. T2 begins in
/// that window with `start_ts > T1.commit_ts`, reads the old count from the
/// delta, and writes count+1. Its conflict check only looks for writes with
/// `commit_ts >= start_ts`, so it passes. On 39fefea both commits succeed and
/// the count ends at 1, not 2.
#[test]
fn d3_tx_begun_during_in_flight_commit_does_not_lose_update() {
  let (_dir, db, node, key) = d3_setup();
  let (go, t1) = d3_start_t1_and_pause_before_merge(&db, node, key);

  // T2, on this thread.
  db.begin(false).expect("T2 begin");
  let seen = read_count(&db, node, key);
  let _ = go.send(());
  db.set_node_prop(node, key, PropValue::I64(seen + 1))
    .expect("T2 set");
  let t2 = db.commit();
  t1.join().expect("T1 thread").expect("T1 commit");

  assert!(
    matches!(t2, Ok(()) | Err(KiteError::Conflict { .. })),
    "T2 commit: {t2:?}"
  );
  let increments = 1 + i64::from(t2.is_ok());
  let count = read_count(&db, node, key);
  assert_eq!(
    count, increments,
    "lost update: T2 began while T1's commit was in flight, read count={seen}, and its \
     commit returned {t2:?} without a conflict"
  );
}

/// D3 [high, MVCC] non-repeatable read: T2 begins while T1's commit is in
/// flight, reads the old count, then reads again after T1 finishes and gets
/// T1's value inside the same snapshot (T1 saw no active reader at commit
/// time, so it recorded no version chain). On 39fefea: 0 then 1.
#[test]
fn d3_tx_begun_during_in_flight_commit_reads_repeatably() {
  let (_dir, db, node, key) = d3_setup();
  let (go, t1) = d3_start_t1_and_pause_before_merge(&db, node, key);

  db.begin(true).expect("T2 begin");
  let first = read_count(&db, node, key);
  let _ = go.send(());
  t1.join().expect("T1 thread").expect("T1 commit");
  let second = read_count(&db, node, key);
  db.rollback().expect("T2 end");

  assert_eq!(
    first, second,
    "non-repeatable read: one snapshot saw count={first}, then count={second} after the \
     in-flight commit merged"
  );
}

/// What a reader sees of node `n` and its neighbour `a` in
/// `d3_tx_begun_during_in_flight_recreate_keeps_the_old_node`.
#[derive(Debug, PartialEq)]
struct RecreateView {
  exists: bool,
  key: Option<String>,
  by_old_key: Option<NodeId>,
  by_new_key: Option<NodeId>,
  prop: Option<PropValue>,
  labels: Vec<LabelId>,
  out_edges: Vec<(ETypeId, NodeId)>,
  a_in_edges: Vec<(ETypeId, NodeId)>,
  weight: Option<PropValue>,
  nodes: Vec<NodeId>,
}

/// Wave-2 lanes together: delta-recreate (a recreated id starts fresh),
/// mvcc-chains (chains hold the history older readers need) and D3 (the MVCC
/// timestamp, version chains and delta merge land at once). T1 deletes a
/// snapshot node and recreates its id in one transaction; T2 begins while
/// T1's commit is durable but not merged. T2 must keep seeing the old node
/// (key, prop, label, edges, edge prop) after T1 merges, and reads after T2
/// see only the recreated node.
#[test]
fn d3_tx_begun_during_in_flight_recreate_keeps_the_old_node() {
  let temp_dir = tempdir().expect("temp dir");
  let db_path = temp_dir.path().join("d3-recreate.kitedb");
  let options = SingleFileOpenOptions::new()
    .auto_checkpoint(false)
    .mvcc(true)
    .mvcc_gc_interval_ms(5)
    .mvcc_retention_ms(0);
  let db = Arc::new(open_single_file(&db_path, options).expect("open"));
  db.begin(false).expect("begin");
  let a = db.create_node(Some("a")).expect("a");
  let n = db.create_node(Some("old")).expect("n");
  let p = db.define_propkey("p").expect("propkey p");
  let weight = db.define_propkey("weight").expect("propkey weight");
  let label = db.define_label("Old").expect("label");
  let t = db.define_etype("T").expect("etype");
  db.set_node_prop(n, p, PropValue::I64(1)).expect("prop");
  db.add_node_label(n, label).expect("label n");
  db.add_edge(n, t, a).expect("edge");
  db.set_edge_prop(n, t, a, weight, PropValue::I64(5))
    .expect("edge prop");
  db.commit().expect("commit");
  db.checkpoint().expect("checkpoint");

  let view = |db: &SingleFileDB| RecreateView {
    exists: db.node_exists(n),
    key: db.node_key(n),
    by_old_key: db.node_by_key("old"),
    by_new_key: db.node_by_key("new"),
    prop: db.node_prop(n, p),
    labels: db.node_labels(n),
    out_edges: db.out_edges(n),
    a_in_edges: db.in_edges(a),
    weight: db.edge_prop(n, t, a, weight),
    nodes: db.list_nodes(),
  };
  let old = view(&db);
  assert_eq!(old.key.as_deref(), Some("old"));
  assert_eq!(old.weight, Some(PropValue::I64(5)));

  let (paused_tx, paused_rx) = mpsc::channel::<()>();
  let (go_tx, go_rx) = mpsc::channel::<()>();
  let t1_db = Arc::clone(&db);
  let t1 = std::thread::spawn(move || {
    t1_db.begin(false).expect("T1 begin");
    t1_db.delete_node(n).expect("T1 delete n");
    t1_db
      .create_node_with_id(n, Some("new"))
      .expect("T1 recreate n");
    t1_db
      .set_node_prop(n, p, PropValue::I64(2))
      .expect("T1 prop");
    BEFORE_NEXT_COMMIT_MERGE.with(|hook| {
      *hook.borrow_mut() = Some(Box::new(move || {
        paused_tx.send(()).expect("signal paused");
        let _ = go_rx.recv_timeout(Duration::from_secs(1));
      }));
    });
    t1_db.commit()
  });
  paused_rx
    .recv_timeout(Duration::from_secs(20))
    .expect("T1 never reached the point between its durable commit and its delta merge");

  db.begin(true).expect("T2 begin");
  let before_merge = view(&db);
  let _ = go_tx.send(());
  t1.join().expect("T1 thread").expect("T1 commit");
  let after_merge = view(&db);
  db.rollback().expect("T2 end");

  assert_eq!(before_merge, old, "T2 began before T1 merged");
  assert_eq!(
    after_merge, old,
    "T2's snapshot changed when the in-flight recreate merged"
  );
  let recreated = view(&db);
  assert_eq!(
    recreated,
    RecreateView {
      exists: true,
      key: Some("new".to_string()),
      by_old_key: None,
      by_new_key: Some(n),
      prop: Some(PropValue::I64(2)),
      labels: Vec::new(),
      out_edges: Vec::new(),
      a_in_edges: Vec::new(),
      weight: None,
      nodes: old.nodes.clone(),
    },
    "after T2: only the recreated node"
  );
}

// ---------------------------------------------------------------------------
// D4: replay of a mismatched vector record.
// ---------------------------------------------------------------------------

/// D4 [high], replay half. A WAL holding a committed `SetNodeVector` whose
/// dimensions disagree with the property's store (what the pre-fix race left
/// on disk) makes open fail with InvalidWal. Replay must skip (and count/log)
/// the mismatched op instead. The record is written raw because this binary's
/// commit check refuses staged mismatches. Fails on 39fefea: reopen errors.
#[test]
fn d4_replay_of_mismatched_vector_dimensions_does_not_fail_open() {
  let temp_dir = tempdir().expect("temp dir");
  let db_path = temp_dir.path().join("d4-replay.kitedb");
  let options = SingleFileOpenOptions::new().auto_checkpoint(false);
  let db = open_single_file(&db_path, options.clone()).expect("open");
  db.begin(false).expect("begin");
  let a = db.create_node(Some("a")).expect("a");
  let b = db.create_node(Some("b")).expect("b");
  let embedding = db.define_propkey("embedding").expect("propkey");
  db.set_node_vector(a, embedding, &[0.5; 3])
    .expect("vector a");
  db.commit().expect("commit");

  db.begin(false).expect("begin");
  let (txid, handle) = db.require_write_tx_handle().expect("write tx");
  db.write_wal_tx(
    &handle,
    WalRecord::new(
      WalRecordType::SetNodeVector,
      txid,
      build_set_node_vector_payload(b, embedding, &[0.5; 4]),
    ),
  )
  .expect("raw vector record");
  db.commit().expect("commit raw record");
  drop(db);

  let reopened = open_single_file(&db_path, options)
    .expect("a mismatched vector record in the WAL made the database unopenable");
  assert_eq!(
    reopened.node_vector(a, embedding).map(|v| v.len()),
    Some(3),
    "the vector committed first (the store's dimensions) must survive replay"
  );
  assert!(
    !reopened.has_node_vector(b, embedding),
    "the mismatched vector must be skipped"
  );
}
