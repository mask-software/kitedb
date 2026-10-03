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
  let held = db.commit_queue.state.lock();
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

/// Group commit batches: commits that arrive while a group is written wait in
/// the queue and go out together as the next group, with one header write.
/// Guard for the leader/queue design wave 2 introduced (the old design slept
/// its window holding the commit lock, so no commit could join a batch).
///
/// Deterministic: each round's first committer leads and stops inside its
/// durable step (`DURING_NEXT_COMMIT_IO`) until the round's other committers
/// are queued behind it. (Committers released together by a barrier rarely
/// overlap a leader's write on a machine with few CPUs, so a version of this
/// test that counted headers after such commits saw tiny groups there.)
#[test]
fn group_commit_batches_concurrent_commits_guard() {
  const THREADS: usize = 8;
  const ROUNDS: usize = 10;
  let dir = tempdir().expect("tempdir");
  let db = Arc::new(
    open_single_file(
      dir.path().join("group-batch.kitedb"),
      group_commit_options(true),
    )
    .expect("open"),
  );
  let commit = |db: &SingleFileDB, key: String| -> Result<()> {
    db.begin(false)?;
    db.create_node(Some(&key))?;
    db.commit()
  };
  for round in 0..ROUNDS {
    let headers_before = db.header.read().change_counter;
    let (in_io_tx, in_io_rx) = mpsc::channel::<()>();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let leader = {
      let db = Arc::clone(&db);
      std::thread::spawn(move || {
        DURING_NEXT_COMMIT_IO.with(|hook| {
          *hook.borrow_mut() = Some(Box::new(move || {
            in_io_tx.send(()).expect("signal the durable step");
            go_rx.recv().expect("wait for the queued commits");
          }));
        });
        commit(&db, format!("r{round}-leader"))
      })
    };
    in_io_rx.recv().expect("the leader in its durable step");
    let followers: Vec<_> = (1..THREADS)
      .map(|writer| {
        let db = Arc::clone(&db);
        std::thread::spawn(move || commit(&db, format!("r{round}-w{writer}")))
      })
      .collect();
    // Every follower hands its commit over; with queueing working, each is
    // queued behind the leader. Without it, the header count below fails.
    wait_until("every follower to hand its commit over", || {
      db.commits_waiting.load(Ordering::SeqCst) == THREADS
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut queued = db.commit_queue.state.lock().queued.len();
    while queued < THREADS - 1 && Instant::now() < deadline {
      std::thread::sleep(Duration::from_millis(1));
      queued = db.commit_queue.state.lock().queued.len();
    }
    go_tx.send(()).expect("release the leader");
    leader.join().expect("leader").expect("leader commit");
    for follower in followers {
      follower.join().expect("follower").expect("follower commit");
    }
    let header_writes = db.header.read().change_counter - headers_before;
    assert_eq!(
      header_writes,
      2,
      "round {round}: a leader's commit and the {} commits that arrived during its write ({queued} \
       of them queued) wrote {header_writes} headers, not 2 (one per group): little or nothing \
       was batched",
      THREADS - 1
    );
    for key in std::iter::once(format!("r{round}-leader"))
      .chain((1..THREADS).map(|writer| format!("r{round}-w{writer}")))
    {
      assert!(db.node_by_key(&key).is_some(), "{key} is not visible");
    }
  }
}
