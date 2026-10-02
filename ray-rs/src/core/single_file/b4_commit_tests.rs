//! raydb-b4 `engine-concurrency` lane: group commit and background cuts.
//! Included from transaction.rs for its commit hooks.
use super::*;
use crate::core::single_file::{open_single_file, SingleFileOpenOptions, SyncMode};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tempfile::tempdir;

fn group_commit_options(mvcc: bool) -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .mvcc(mvcc)
    .mvcc_gc_interval_ms(10)
    .sync_mode(SyncMode::Normal)
    .group_commit_enabled(true)
    .auto_checkpoint(false)
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
  let deadline = Instant::now() + Duration::from_secs(10);
  while !cond() {
    assert!(Instant::now() < deadline, "timed out waiting for {what}");
    std::thread::sleep(Duration::from_millis(1));
  }
}

/// With group commit, a committer learns its outcome only when it wakes after
/// the leader delivers the batch, and its transaction counted as open (in
/// `open_write_txids`) until then, though its COMMIT was durable. A background
/// cut in that window skips the committed transaction's records, its install
/// drops them from the WAL, and the next cut finds an "open" transaction with
/// no BEGIN record and declines: "no BEGIN record found for open transactions
/// {N}", until that thread wakes.
///
/// Deterministic: the committer (its own leader) stops in its publish step,
/// the test takes the group-commit lock so it cannot deliver its outcome and
/// finish, and runs two background checkpoints.
#[test]
fn group_committed_transaction_does_not_make_background_cuts_decline() {
  let dir = tempdir().expect("tempdir");
  let db = Arc::new(
    open_single_file(
      dir.path().join("group-cut.kitedb"),
      group_commit_options(false),
    )
    .expect("open"),
  );
  db.begin(false).expect("begin");
  db.create_node(Some("seed")).expect("seed");
  db.commit().expect("commit");

  let (publishing_tx, publishing_rx) = mpsc::channel();
  let (go_tx, go_rx) = mpsc::channel::<()>();
  let committer = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || {
      BEFORE_NEXT_COMMIT_MERGE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
          publishing_tx.send(()).expect("report publishing");
          go_rx.recv().expect("wait to publish");
        }));
      });
      db.begin(false).expect("begin");
      db.create_node(Some("grouped")).expect("create");
      db.commit()
    })
  };
  publishing_rx.recv().expect("commit durable and publishing");
  // The leader finishes publishing, then waits for this lock to deliver the
  // batch's outcomes, so its committer cannot finish its transaction.
  let held = db.group_commit_state.lock();
  go_tx.send(()).expect("let it publish");
  wait_until("the grouped commit to merge", || {
    db.node_by_key("grouped").is_some()
  });

  let first = db.background_checkpoint();
  let second = db.background_checkpoint();
  drop(held);
  committer
    .join()
    .expect("committer thread")
    .expect("grouped commit");

  assert!(
    first.is_ok() && second.is_ok(),
    "background checkpoints declined because a durable group commit still counted as open: \
     first {first:?}, second {second:?}"
  );
  for key in ["seed", "grouped"] {
    assert!(db.node_by_key(key).is_some(), "{key} missing");
  }
}

/// Group commit batches: when many committers (MVCC, so writers are not
/// serialized) commit at once, one header write covers several commits.
/// Guard for the leader/queue design wave 2 introduced (the old design slept
/// its window holding the commit lock, so no commit could join a batch).
#[test]
fn group_commit_batches_concurrent_commits_guard() {
  const THREADS: usize = 8;
  const ROUNDS: usize = 50;
  let dir = tempdir().expect("tempdir");
  let db = Arc::new(
    open_single_file(
      dir.path().join("group-batch.kitedb"),
      group_commit_options(true),
    )
    .expect("open"),
  );
  let headers_before = db.header.read().change_counter;
  let ready = Arc::new(std::sync::Barrier::new(THREADS));
  let writers: Vec<_> = (0..THREADS)
    .map(|writer| {
      let (db, ready) = (Arc::clone(&db), Arc::clone(&ready));
      std::thread::spawn(move || {
        for round in 0..ROUNDS {
          db.begin(false).expect("begin");
          db.create_node(Some(&format!("w{writer}-{round}")))
            .expect("create");
          // Everyone commits at once.
          ready.wait();
          db.commit().expect("commit");
        }
      })
    })
    .collect();
  for writer in writers {
    writer.join().expect("writer");
  }
  let header_writes = db.header.read().change_counter - headers_before;
  let commits = (THREADS * ROUNDS) as u64;
  println!("group commit: {commits} commits, {header_writes} header writes");
  assert!(
    header_writes * 4 <= commits * 3,
    "{commits} group commits arriving together wrote {header_writes} headers: little or \
     nothing was batched"
  );
}
