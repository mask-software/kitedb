//! raydb-b4 `write-scaling` lane: the commit queue. Included from
//! transaction.rs for its commit hooks.
//!
//! Findings, each written to fail before its fix:
//!
//! - Commits that arrive while another is written were written one at a
//!   time unless `group_commit_enabled` was set (one WAL flush and one header
//!   write each), and never in `SyncMode::Full` (two fsyncs each) or with
//!   primary replication. More writers made 1-node transactions slower.
//! - A transaction's BEGIN and data records waited for every commit's file
//!   I/O: they took the pager lock, which a commit holds from its first WAL
//!   byte to its header.
//!
//! The rest pin what grouping must keep: a member that conflicts with an
//! earlier one aborts alone; a failed group write or sync fails every member
//! and leaves nothing behind; crash images taken mid-group hold the whole
//! group or none of it, and never a refused member; replication frames follow
//! the commit order; and snapshots see a group whole or not at all.
//!
//! Each group forms deterministically: its first member leads, and waits
//! before it takes the queued commits until every other member is queued.

use super::*;
use crate::core::pager::io_hooks::{self, IoEvent};
use crate::core::single_file::{close_single_file, open_single_file, SingleFileOpenOptions};
use crate::replication::types::ReplicationRole;
use std::path::Path;
use std::sync::{mpsc, Barrier};
use std::time::{Duration, Instant};
use tempfile::tempdir;

const PAGE_SIZE: u64 = 4096;

fn options(sync_mode: SyncMode) -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .mvcc(true)
    .mvcc_gc_interval_ms(10)
    .sync_mode(sync_mode)
    .auto_checkpoint(false)
}

fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
  let deadline = Instant::now() + Duration::from_secs(20);
  while !condition() {
    assert!(Instant::now() < deadline, "timed out waiting for {what}");
    std::thread::sleep(Duration::from_millis(1));
  }
}

fn commit_node(db: &SingleFileDB, key: &str) -> Result<()> {
  db.begin(false)?;
  db.create_node(Some(key))?;
  db.commit()
}

/// One member of a group: begins, writes and commits on its own thread.
type Member<R> = Box<dyn FnOnce(&SingleFileDB) -> R + Send>;

/// Run `members` as one group: each on its own thread, the first leading.
/// Returns their results, in order, and the headers the group wrote.
fn run_group<R: Send + 'static>(db: &Arc<SingleFileDB>, members: Vec<Member<R>>) -> (Vec<R>, u64) {
  let count = members.len();
  let generation = db.header.read().change_counter;
  let mut members = members.into_iter();
  let first = members.next().expect("a group has members");
  let leader_db = Arc::clone(db);
  let leader = std::thread::spawn(move || {
    let hook_db = Arc::clone(&leader_db);
    BEFORE_NEXT_COMMIT_LOCK.with(|hook| {
      *hook.borrow_mut() = Some(Box::new(move || {
        // Queued, not just handed over: a member counts as waiting just
        // before it queues, and one caught between would form a group of
        // its own.
        wait_until("every member to queue", || {
          hook_db.commit_queue.state.lock().queued.len() == count - 1
        });
      }));
    });
    first(&leader_db)
  });
  wait_until("the leader", || {
    db.commits_waiting.load(Ordering::SeqCst) == 1
  });
  let followers: Vec<_> = members
    .map(|member| {
      let db = Arc::clone(db);
      std::thread::spawn(move || member(&db))
    })
    .collect();
  let mut results = vec![leader.join().expect("leader thread")];
  results.extend(
    followers
      .into_iter()
      .map(|follower| follower.join().expect("member thread")),
  );
  (results, db.header.read().change_counter - generation)
}

fn creator(key: String) -> Member<Result<()>> {
  Box::new(move |db: &SingleFileDB| commit_node(db, &key))
}

fn copy_and_open(db_path: &Path, image: &[u8], name: &str) -> SingleFileDB {
  let path = db_path.with_extension(name);
  std::fs::write(&path, image).expect("write image");
  open_single_file(&path, options(SyncMode::Normal)).expect("open image")
}

/// Finding 1: queued commits are written together, with one WAL flush and
/// one header, without any option.
#[test]
fn b4_ws_queued_commits_share_one_header() {
  const MEMBERS: usize = 6;
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("one-header.kitedb");
  let db = Arc::new(open_single_file(&path, options(SyncMode::Normal)).expect("open"));
  let members = (0..MEMBERS).map(|i| creator(format!("m{i}"))).collect();
  let (results, headers) = run_group(&db, members);
  assert!(results.iter().all(Result::is_ok), "{results:?}");
  assert_eq!(
    headers, 1,
    "{MEMBERS} queued commits must share one header; they wrote {headers}"
  );

  let image = std::fs::read(&path).expect("read file");
  let crashed = copy_and_open(&path, &image, "image.kitedb");
  for i in 0..MEMBERS {
    let key = format!("m{i}");
    assert!(db.node_by_key(&key).is_some(), "{key} is not visible");
    assert!(crashed.node_by_key(&key).is_some(), "{key} is not durable");
  }
}

/// Finding 1, `SyncMode::Full`: a group makes its records durable with one
/// sync (of its WAL records and the header naming them), not two per commit.
/// (The first commit after an open also zeroes the WAL ahead of its records,
/// and syncs that first; see `WalBuffer`'s "Zeros ahead".)
#[test]
fn b4_ws_full_mode_queued_commits_share_syncs() {
  const MEMBERS: usize = 6;
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("full-syncs.kitedb");
  let db = Arc::new(open_single_file(&path, options(SyncMode::Full)).expect("open"));
  commit_node(&db, "warm").expect("warm commit");
  let members = (0..MEMBERS)
    .map(|i| -> Member<(Result<()>, usize)> {
      Box::new(move |db: &SingleFileDB| {
        db.begin(false).expect("begin");
        db.create_node(Some(&format!("m{i}"))).expect("create");
        let (committed, syncs) = io_hooks::sync_kinds_during(|| db.commit());
        (committed, syncs.len())
      })
    })
    .collect();
  let (results, headers) = run_group(&db, members);
  assert!(results.iter().all(|(result, _)| result.is_ok()));
  let syncs: usize = results.iter().map(|(_, syncs)| syncs).sum();
  assert_eq!(
    (headers, syncs),
    (1, 1),
    "(headers, syncs) of {MEMBERS} queued Full-mode commits"
  );
}

/// Finding 2: a transaction writes its BEGIN and data records while another
/// commit's file I/O runs: a small one keeps them back until its commit, and
/// a large one (here a 20 KB key, past `WAL_DEFER_BYTES`) appends them to the
/// WAL buffer, which the I/O does not hold.
#[test]
fn b4_ws_wal_appends_do_not_wait_for_commit_io() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("appends-during-io.kitedb");
  let db = Arc::new(open_single_file(&path, options(SyncMode::Normal)).expect("open"));

  let (in_io_tx, in_io_rx) = mpsc::channel::<()>();
  let (appended_tx, appended_rx) = mpsc::channel::<()>();
  let (commit_tx, commit_rx) = mpsc::channel::<()>();
  let writer_db = Arc::clone(&db);
  let writer = std::thread::spawn(move || {
    in_io_rx.recv().expect("commit in its I/O");
    writer_db.begin(false)?;
    writer_db.create_node(Some("during"))?;
    writer_db.create_node(Some(&"x".repeat(20_000)))?;
    let (_, tx) = writer_db.require_write_tx_handle()?;
    assert!(
      tx.lock().wal_begun,
      "setup: the large transaction wrote its records"
    );
    // The commit's hook stops listening once it gives up on this.
    let _ = appended_tx.send(());
    commit_rx.recv().expect("go commit");
    writer_db.commit()
  });

  let appended_during_io = std::rc::Rc::new(std::cell::Cell::new(false));
  let flag = std::rc::Rc::clone(&appended_during_io);
  DURING_NEXT_COMMIT_IO.with(|hook| {
    *hook.borrow_mut() = Some(Box::new(move || {
      in_io_tx.send(()).expect("signal in I/O");
      flag.set(appended_rx.recv_timeout(Duration::from_secs(2)).is_ok());
    }));
  });
  commit_node(&db, "first").expect("first commit");
  commit_tx.send(()).expect("let the writer commit");
  writer
    .join()
    .expect("writer thread")
    .expect("writer commit");

  assert!(
    appended_during_io.get(),
    "BEGIN and data records waited for another commit's file I/O"
  );
  assert!(db.node_by_key("first").is_some());
  assert!(db.node_by_key("during").is_some());
  assert!(db.node_by_key(&"x".repeat(20_000)).is_some());
}

/// A member that conflicts with an earlier member of its group aborts, and
/// only it: both incrementers read 0, so the later one must not commit.
#[test]
fn b4_ws_group_member_conflicting_with_an_earlier_member_aborts_alone() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("group-conflict.kitedb");
  let db = Arc::new(open_single_file(&path, options(SyncMode::Normal)).expect("open"));
  db.begin(false).expect("begin");
  let counter = db.create_node(Some("counter")).expect("node");
  let count = db.define_propkey("count").expect("propkey");
  db.set_node_prop(counter, count, PropValue::I64(0))
    .expect("set");
  db.commit().expect("commit");

  let both_read = Arc::new(Barrier::new(2));
  let incrementer = |both_read: Arc<Barrier>| -> Member<Result<()>> {
    Box::new(move |db: &SingleFileDB| {
      db.begin(false)?;
      let value = match db.node_prop(counter, count) {
        Some(PropValue::I64(value)) => value,
        other => panic!("unexpected count {other:?}"),
      };
      both_read.wait();
      db.set_node_prop(counter, count, PropValue::I64(value + 1))?;
      db.commit()
    })
  };
  let members = vec![
    creator("leader".to_string()),
    incrementer(Arc::clone(&both_read)),
    incrementer(both_read),
    creator("other".to_string()),
  ];
  let (results, headers) = run_group(&db, members);

  assert_eq!(headers, 1, "the members commit as one group: {results:?}");
  assert!(results[0].is_ok() && results[3].is_ok(), "{results:?}");
  let increments = &results[1..3];
  assert_eq!(
    increments.iter().filter(|result| result.is_ok()).count(),
    1,
    "exactly one increment commits: {results:?}"
  );
  assert!(
    increments
      .iter()
      .any(|result| matches!(result, Err(KiteError::Conflict { .. }))),
    "the other conflicts: {results:?}"
  );
  assert_eq!(db.node_prop(counter, count), Some(PropValue::I64(1)));
  let image = std::fs::read(&path).expect("read file");
  let crashed = copy_and_open(&path, &image, "image.kitedb");
  assert_eq!(crashed.node_prop(counter, count), Some(PropValue::I64(1)));
  assert!(crashed.node_by_key("leader").is_some() && crashed.node_by_key("other").is_some());
}

/// Every member fails when its group's WAL write fails; none is visible or
/// comes back, and the next commit works.
#[test]
fn b4_ws_failed_group_wal_write_fails_every_member() {
  const MEMBERS: usize = 4;
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("failed-write.kitedb");
  let db = Arc::new(open_single_file(&path, options(SyncMode::Normal)).expect("open"));
  commit_node(&db, "base").expect("base");

  let mut members: Vec<Member<Result<()>>> = vec![Box::new(|db: &SingleFileDB| {
    db.begin(false)?;
    db.create_node(Some("m0"))?;
    io_hooks::with_failing_writes(1, || db.commit())
  })];
  members.extend((1..MEMBERS).map(|i| creator(format!("m{i}"))));
  let (results, _) = run_group(&db, members);
  assert!(
    results.iter().all(Result::is_err),
    "every member of the failed group fails: {results:?}"
  );
  for i in 0..MEMBERS {
    assert!(
      db.node_by_key(&format!("m{i}")).is_none(),
      "m{i} is visible"
    );
  }

  commit_node(&db, "after").expect("the next commit");
  let db = Arc::try_unwrap(db).ok().expect("sole owner");
  close_single_file(db).expect("close");
  let reopened = open_single_file(&path, options(SyncMode::Normal)).expect("reopen");
  assert!(reopened.node_by_key("base").is_some());
  assert!(reopened.node_by_key("after").is_some());
  for i in 0..MEMBERS {
    assert!(
      reopened.node_by_key(&format!("m{i}")).is_none(),
      "m{i} came back"
    );
  }
}

/// `SyncMode::Full`: a follower's commit returns only once its records are
/// synced. When its group's WAL sync fails, every member fails.
#[test]
fn b4_ws_full_mode_failed_group_sync_fails_every_member() {
  const MEMBERS: usize = 4;
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("failed-sync.kitedb");
  let db = Arc::new(open_single_file(&path, options(SyncMode::Full)).expect("open"));
  commit_node(&db, "base").expect("base");

  let mut members: Vec<Member<Result<()>>> = vec![Box::new(|db: &SingleFileDB| {
    db.begin(false)?;
    db.create_node(Some("m0"))?;
    io_hooks::with_failing_syncs(1, || db.commit())
  })];
  members.extend((1..MEMBERS).map(|i| creator(format!("m{i}"))));
  let head_before = db.header.read().wal_head;
  let (results, _) = run_group(&db, members);
  // The header written with the group, before its sync, is overwritten by
  // one naming only the commits before it.
  assert_eq!(
    db.header.read().wal_head,
    head_before,
    "the header names the failed group"
  );
  assert!(
    results.iter().all(Result::is_err),
    "a follower acknowledged before its group's sync: {results:?}"
  );

  commit_node(&db, "after").expect("the next commit");
  let db = Arc::try_unwrap(db).ok().expect("sole owner");
  close_single_file(db).expect("close");
  let reopened = open_single_file(&path, options(SyncMode::Full)).expect("reopen");
  assert!(reopened.node_by_key("after").is_some());
  for i in 0..MEMBERS {
    assert!(
      reopened.node_by_key(&format!("m{i}")).is_none(),
      "m{i} came back"
    );
  }
}

/// A crash at any point of a group's file I/O leaves an openable file with
/// the whole group or none of it (one header names them all), never its
/// refused member, and the acknowledged members once the I/O is done.
#[test]
fn b4_ws_crash_images_mid_group_hold_the_group_whole_or_not_at_all() {
  const CREATORS: usize = 4;
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("crash-mid-group.kitedb");
  let db = Arc::new(open_single_file(&path, options(SyncMode::Normal)).expect("open"));
  db.begin(false).expect("begin");
  let counter = db.create_node(Some("counter")).expect("node");
  let count = db.define_propkey("count").expect("propkey");
  db.set_node_prop(counter, count, PropValue::I64(0))
    .expect("set");
  db.commit().expect("commit");
  let base = std::fs::read(&path).expect("base image");

  let both_read = Arc::new(Barrier::new(2));
  let incrementer = |key: &'static str, both_read: Arc<Barrier>| -> Member<Result<Vec<IoEvent>>> {
    Box::new(move |db: &SingleFileDB| {
      db.begin(false)?;
      db.create_node(Some(key))?;
      let value = match db.node_prop(counter, count) {
        Some(PropValue::I64(value)) => value,
        other => panic!("unexpected count {other:?}"),
      };
      both_read.wait();
      db.set_node_prop(counter, count, PropValue::I64(value + 1))?;
      db.commit().map(|()| Vec::new())
    })
  };
  let mut members: Vec<Member<Result<Vec<IoEvent>>>> = vec![Box::new(|db: &SingleFileDB| {
    db.begin(false)?;
    db.create_node(Some("c0"))?;
    let (committed, events) = io_hooks::record_io_during(|| db.commit());
    committed.map(|()| events)
  })];
  members.extend((1..CREATORS).map(|i| -> Member<Result<Vec<IoEvent>>> {
    Box::new(move |db: &SingleFileDB| commit_node(db, &format!("c{i}")).map(|()| Vec::new()))
  }));
  members.push(incrementer("inc-a", Arc::clone(&both_read)));
  members.push(incrementer("inc-b", both_read));
  let (results, headers) = run_group(&db, members);
  assert_eq!(headers, 1, "the members commit as one group: {results:?}");
  let events = results[0].as_ref().expect("leader commit").clone();
  let (winner, loser) = match (&results[CREATORS], &results[CREATORS + 1]) {
    (Ok(_), Err(KiteError::Conflict { .. })) => ("inc-a", "inc-b"),
    (Err(KiteError::Conflict { .. }), Ok(_)) => ("inc-b", "inc-a"),
    other => panic!("exactly one increment commits: {other:?}"),
  };
  let members: Vec<String> = (0..CREATORS)
    .map(|i| format!("c{i}"))
    .chain(std::iter::once(winner.to_string()))
    .collect();

  for end in 0..=events.len() {
    let image = io_hooks::crash_image(&base, &events[..end], 2 * PAGE_SIZE);
    let crashed = copy_and_open(&path, &image, &format!("crash-{end}.kitedb"));
    let present: Vec<bool> = members
      .iter()
      .map(|key| crashed.node_by_key(key).is_some())
      .collect();
    assert!(
      present.iter().all(|&p| p) || present.iter().all(|&p| !p),
      "a crash after {end} of {} writes kept part of the group: {members:?} {present:?}",
      events.len()
    );
    assert!(
      crashed.node_by_key(loser).is_none(),
      "the refused member is in the crash image after {end} writes"
    );
    let expected = if present[0] { 1 } else { 0 };
    assert_eq!(
      crashed.node_prop(counter, count),
      Some(PropValue::I64(expected)),
      "crash after {end} writes"
    );
    if end == events.len() {
      assert!(
        present[0],
        "an acknowledged group is missing once its I/O is done"
      );
    }
  }
}

/// With primary replication, a group's sidecar frames follow its commit
/// order (the order of its COMMIT records), and a replica applies them all.
#[test]
fn b4_ws_replication_frames_follow_the_commit_order() {
  const MEMBERS: usize = 5;
  let dir = tempdir().expect("tempdir");
  let primary_path = dir.path().join("group-primary.kitedb");
  let primary = Arc::new(
    open_single_file(
      &primary_path,
      options(SyncMode::Full).replication_role(ReplicationRole::Primary),
    )
    .expect("open primary"),
  );
  let replica = open_single_file(
    dir.path().join("group-replica.kitedb"),
    SingleFileOpenOptions::new()
      .auto_checkpoint(false)
      .replication_role(ReplicationRole::Replica)
      .replication_source_db_path(&primary_path),
  )
  .expect("open replica");
  replica
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap empty primary");

  let members = (0..MEMBERS)
    .map(|i| -> Member<Result<(TxId, CommitToken)>> {
      Box::new(move |db: &SingleFileDB| {
        let txid = db.begin(false)?;
        db.create_node(Some(&format!("m{i}")))?;
        let token = db
          .commit_with_token()?
          .expect("a primary's commit has a token");
        Ok((txid, token))
      })
    })
    .collect();
  let (results, headers) = run_group(&primary, members);
  assert_eq!(headers, 1, "the members commit as one group: {results:?}");
  let tokens: HashMap<TxId, CommitToken> = results
    .into_iter()
    .map(|result| result.expect("member commit"))
    .collect();

  let commit_order: Vec<TxId> = {
    let mut pager = primary.pager.lock();
    let mut wal = primary.wal_buffer.lock();
    wal
      .scan_records(&mut pager)
      .expect("scan WAL")
      .into_iter()
      .filter(|record| {
        record.record_type == WalRecordType::Commit && tokens.contains_key(&record.txid)
      })
      .map(|record| record.txid)
      .collect()
  };
  assert_eq!(commit_order.len(), MEMBERS);
  let log_indexes: Vec<u64> = commit_order
    .iter()
    .map(|txid| tokens[txid].log_index)
    .collect();
  assert!(
    log_indexes.windows(2).all(|pair| pair[0] < pair[1]),
    "frames out of commit order: commits {commit_order:?} got log indexes {log_indexes:?}"
  );

  let applied = replica.replica_catch_up_once(64).expect("catch up");
  assert_eq!(applied, MEMBERS);
  for i in 0..MEMBERS {
    assert!(
      replica.node_by_key(&format!("m{i}")).is_some(),
      "replica misses m{i}"
    );
  }
  close_single_file(replica).expect("close replica");
  let primary = Arc::into_inner(primary).expect("primary unique");
  close_single_file(primary).expect("close primary");
}

/// A reader that begins while a group is written sees none of it, and keeps
/// seeing none (repeatable reads); one that begins after sees all of it.
#[test]
fn b4_ws_reader_beginning_mid_group_sees_none_of_it() {
  const MEMBERS: usize = 4;
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("reader-mid-group.kitedb");
  let db = Arc::new(open_single_file(&path, options(SyncMode::Normal)).expect("open"));
  commit_node(&db, "base").expect("base");
  let keys: Vec<String> = (0..MEMBERS).map(|i| format!("m{i}")).collect();
  let visible = |db: &SingleFileDB, keys: &[String]| -> Vec<bool> {
    keys
      .iter()
      .map(|key| db.node_by_key(key).is_some())
      .collect()
  };

  let (group_done_tx, group_done_rx) = mpsc::channel::<()>();
  let (reader_tx, reader_rx) = mpsc::channel();
  let reader_db = Arc::clone(&db);
  let reader_keys = keys.clone();
  let first: Member<Result<()>> = Box::new(move |db: &SingleFileDB| {
    db.begin(false)?;
    db.create_node(Some("m0"))?;
    DURING_NEXT_COMMIT_IO.with(|hook| {
      *hook.borrow_mut() = Some(Box::new(move || {
        let (began_tx, began_rx) = mpsc::channel::<()>();
        let reader = std::thread::spawn(move || {
          reader_db.begin(true).expect("reader begin");
          assert!(reader_db.node_by_key("base").is_some());
          let during = visible(&reader_db, &reader_keys);
          began_tx.send(()).expect("signal began");
          group_done_rx.recv().expect("group done");
          let after = visible(&reader_db, &reader_keys);
          reader_db.rollback().expect("reader end");
          (during, after)
        });
        began_rx
          .recv_timeout(Duration::from_secs(5))
          .expect("a reader begins while a group is written");
        reader_tx.send(reader).expect("hand over the reader");
      }));
    });
    db.commit()
  });
  let mut members = vec![first];
  members.extend((1..MEMBERS).map(|i| creator(format!("m{i}"))));
  let (results, headers) = run_group(&db, members);
  assert!(results.iter().all(Result::is_ok), "{results:?}");
  assert_eq!(headers, 1, "the members commit as one group");
  group_done_tx.send(()).expect("group done");
  let (during, after) = reader_rx
    .recv()
    .expect("the reader")
    .join()
    .expect("reader thread");
  assert!(
    during.iter().all(|&v| !v),
    "mid-group reader saw {during:?}"
  );
  assert!(
    after.iter().all(|&v| !v),
    "mid-group reader later saw {after:?}"
  );
  assert!(visible(&db, &keys).iter().all(|&v| v));
}

/// A transaction begun while a group is published sees all of it: the group
/// publishes its members' timestamps in one critical section.
#[test]
fn b4_ws_group_is_published_atomically() {
  const MEMBERS: usize = 4;
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("atomic-group.kitedb");
  let db = Arc::new(open_single_file(&path, options(SyncMode::Normal)).expect("open"));
  commit_node(&db, "base").expect("base");
  let keys: Vec<String> = (0..MEMBERS).map(|i| format!("m{i}")).collect();

  let (late_tx, late_rx) = mpsc::channel();
  let late_db = Arc::clone(&db);
  let late_keys = keys.clone();
  let first: Member<Result<()>> = Box::new(move |db: &SingleFileDB| {
    db.begin(false)?;
    db.create_node(Some("m0"))?;
    AFTER_NEXT_COMMIT_TIMESTAMP.with(|hook| {
      *hook.borrow_mut() = Some(Box::new(move || {
        let late = std::thread::spawn(move || {
          late_db.begin(true).expect("late begin");
          let seen: Vec<bool> = late_keys
            .iter()
            .map(|key| late_db.node_by_key(key).is_some())
            .collect();
          late_db.rollback().expect("late end");
          seen
        });
        late_tx.send(late).expect("hand over the late reader");
      }));
    });
    db.commit()
  });
  let mut members = vec![first];
  members.extend((1..MEMBERS).map(|i| creator(format!("m{i}"))));
  let (results, headers) = run_group(&db, members);
  assert!(results.iter().all(Result::is_ok), "{results:?}");
  assert_eq!(headers, 1, "the members commit as one group");
  let seen = late_rx
    .recv()
    .expect("the late reader")
    .join()
    .expect("late thread");
  assert!(
    seen.iter().all(|&v| v) || seen.iter().all(|&v| !v),
    "a transaction begun during a group's publish saw part of it: {seen:?}"
  );
}

/// The transaction manager's staging, which the commit groups above rely on.
mod staging {
  use crate::mvcc::{ConflictDetector, TxManager};
  use crate::types::{TxKey, TxKeySet};

  fn key(name: &str) -> TxKey {
    TxKey::Key(std::sync::Arc::from(name))
  }

  fn write(tx_mgr: &mut TxManager, txid: u64, keys: &[&str]) {
    let writes: TxKeySet = keys.iter().map(|name| key(name)).collect();
    tx_mgr.record_reads_and_writes(txid, TxKeySet::new(), writes, None);
  }

  fn conflicts(tx_mgr: &TxManager, txid: u64) -> bool {
    ConflictDetector::new()
      .validate_commit(tx_mgr, txid)
      .is_err()
  }

  /// A later member conflicts with what an earlier member of its group
  /// wrote, before either is committed.
  #[test]
  fn staged_writes_conflict_with_later_members() {
    let mut tx_mgr = TxManager::new();
    let (a, _) = tx_mgr.begin_tx();
    let (b, _) = tx_mgr.begin_tx();
    let (c, _) = tx_mgr.begin_tx();
    write(&mut tx_mgr, a, &["k"]);
    write(&mut tx_mgr, b, &["k"]);
    write(&mut tx_mgr, c, &["other"]);
    assert!(!conflicts(&tx_mgr, a));
    let staged_ts = tx_mgr.stage_commit(a).expect("stage a");
    assert!(conflicts(&tx_mgr, b), "b wrote what staged a wrote");
    assert!(!conflicts(&tx_mgr, c));
    assert_eq!(tx_mgr.stage_commit(c).expect("stage c"), staged_ts + 1);
    assert!(tx_mgr.has_open_readers(), "b may still read");
    tx_mgr.abort_tx(b);
    assert!(
      !tx_mgr.has_open_readers(),
      "only staged transactions are left"
    );
    assert_eq!(tx_mgr.commit_tx(a).expect("commit a"), staged_ts);
    assert_eq!(tx_mgr.commit_tx(c).expect("commit c"), staged_ts + 1);
  }

  /// Unstaged transactions get their writes back and cause no conflict;
  /// staged ones commit only in staging order.
  #[test]
  fn unstaged_writes_cause_no_conflict_and_order_holds() {
    let mut tx_mgr = TxManager::new();
    let (a, _) = tx_mgr.begin_tx();
    let (b, _) = tx_mgr.begin_tx();
    let (reader, _) = tx_mgr.begin_tx();
    write(&mut tx_mgr, a, &["k"]);
    write(&mut tx_mgr, b, &["j"]);
    write(&mut tx_mgr, reader, &["k", "j"]);
    tx_mgr.stage_commit(a).expect("stage a");
    tx_mgr.stage_commit(b).expect("stage b");
    assert!(
      tx_mgr.commit_tx(b).is_err(),
      "b committed before a, staged ahead of it"
    );
    tx_mgr.unstage_last();
    tx_mgr.unstage_last();
    assert!(!conflicts(&tx_mgr, reader), "unstaged writes conflict");
    assert_eq!(tx_mgr.tx(a).expect("a").write_set.len(), 1, "a's writes");
    let staged_ts = tx_mgr.stage_commit(a).expect("stage a again");
    assert_eq!(tx_mgr.commit_tx(a).expect("commit a"), staged_ts);
    assert!(conflicts(&tx_mgr, reader), "a committed after reader began");
  }

  /// A writer staged alone indexes nothing, but a transaction that begins
  /// before it commits still conflicts with it.
  #[test]
  fn writes_staged_alone_are_indexed_for_a_transaction_begun_before_their_commit() {
    let mut tx_mgr = TxManager::new();
    let (a, _) = tx_mgr.begin_tx();
    write(&mut tx_mgr, a, &["k"]);
    tx_mgr.stage_commit(a).expect("stage a");
    let (late, _) = tx_mgr.begin_tx();
    tx_mgr.commit_tx(a).expect("commit a");
    write(&mut tx_mgr, late, &["k"]);
    assert!(
      conflicts(&tx_mgr, late),
      "a transaction begun before a staged commit missed its writes"
    );

    // With no transaction beside it, a commit leaves nothing to check.
    let (b, _) = tx_mgr.begin_tx();
    tx_mgr.abort_tx(late);
    write(&mut tx_mgr, b, &["k"]);
    tx_mgr.stage_commit(b).expect("stage b");
    tx_mgr.commit_tx(b).expect("commit b");
    let (after, _) = tx_mgr.begin_tx();
    write(&mut tx_mgr, after, &["k"]);
    assert!(!conflicts(&tx_mgr, after));
  }
}
