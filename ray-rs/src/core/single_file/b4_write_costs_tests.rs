//! raydb-b4 `write-costs` lane: what begins and commits cost with several
//! writers. Included from transaction.rs for its commit hooks.
//!
//! Findings, each written to fail before its fix:
//!
//! - 1-node transactions did not scale with writers in `SyncMode::Normal`
//!   (8 writers 0.95x of 1 writer on a RAM disk). Each commit group was
//!   written by another thread: a leader handed the lead to the oldest
//!   queued committer as soon as its group was durable. The group's two
//!   writes (WAL, header) then ran on a core that had not touched the file
//!   or the commit structures: two such writes took 0.93 us when one thread
//!   made them all, and 3.3 us when 4 to 8 threads took turns.
//! - An MVCC begin, and the end of a read-only or rolled-back transaction,
//!   took the transaction manager's lock, which a commit group holds for its
//!   members' conflict checks and MVCC commits: with 8 writers a begin
//!   waited about 0.5 us for it, and commit groups waited for begins.

use super::*;
use crate::core::single_file::{open_single_file, SingleFileOpenOptions};
use std::cell::Cell;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tempfile::tempdir;

/// How long a step may take before it counts as blocked.
const DEADLINE: Duration = Duration::from_secs(2);

fn options() -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .mvcc(true)
    .sync_mode(SyncMode::Normal)
    .auto_checkpoint(false)
}

fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
  let deadline = Instant::now() + Duration::from_secs(20);
  while !condition() {
    assert!(Instant::now() < deadline, "timed out waiting for {what}");
    std::thread::sleep(Duration::from_millis(1));
  }
}

/// Commit a node with `key`, and return how many commit groups this thread
/// has led.
fn commit_node(db: &SingleFileDB, key: &str) -> Result<u64> {
  db.begin(false)?;
  db.create_node(Some(key))?;
  db.commit()?;
  Ok(GROUPS_LED.with(Cell::get))
}

/// A leader whose group is durable writes the commits queued meanwhile
/// itself, as the next group, instead of handing them to the oldest of
/// their committers, whose core would write the file cold.
#[test]
fn b4_wc_leader_writes_the_commits_queued_while_it_wrote() {
  const FOLLOWERS: usize = 3;
  let dir = tempdir().expect("tempdir");
  let db = Arc::new(open_single_file(dir.path().join("lead.kitedb"), options()).expect("open"));
  let (writing_tx, writing_rx) = mpsc::channel();
  let leader = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || {
      let hook_db = Arc::clone(&db);
      DURING_NEXT_COMMIT_IO.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
          writing_tx.send(()).expect("writing");
          wait_until("the followers to queue", || {
            hook_db.commit_queue.state.lock().queued.len() == FOLLOWERS
          });
        }));
      });
      commit_node(&db, "leader")
    })
  };
  // The leader writes its own commit alone; the followers queue meanwhile.
  writing_rx.recv().expect("the leader writes");
  let followers: Vec<_> = (0..FOLLOWERS)
    .map(|i| {
      let db = Arc::clone(&db);
      std::thread::spawn(move || commit_node(&db, &format!("follower-{i}")))
    })
    .collect();
  let led_by_leader = leader.join().expect("leader").expect("leader commit");
  let led_by_followers: Vec<u64> = followers
    .into_iter()
    .map(|follower| follower.join().expect("follower").expect("follower commit"))
    .collect();

  for key in
    std::iter::once("leader".to_string()).chain((0..FOLLOWERS).map(|i| format!("follower-{i}")))
  {
    assert!(db.node_by_key(&key).is_some(), "{key} is not visible");
  }
  assert_eq!(
    led_by_followers,
    vec![0; FOLLOWERS],
    "a follower led the group queued while the leader wrote (the leader led {led_by_leader})"
  );
  assert_eq!(led_by_leader, 2, "the leader wrote both groups");
}

/// An MVCC transaction begins, and a read-only or rolled-back one ends,
/// without the transaction manager's lock: a commit group holds it while it
/// checks and commits its members.
#[test]
fn b4_wc_mvcc_begin_and_end_take_no_transaction_manager_lock() {
  let dir = tempdir().expect("tempdir");
  let db = Arc::new(open_single_file(dir.path().join("begin.kitedb"), options()).expect("open"));
  let (done_tx, done_rx) = mpsc::channel();
  let mvcc = db.mvcc.as_ref().expect("mvcc");
  let held = mvcc.tx_manager.lock();
  let worker = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || -> Result<()> {
      db.begin(true)?;
      db.commit()?;
      db.begin(true)?;
      db.rollback()?;
      db.begin(false)?;
      db.create_node(Some("rolled-back"))?;
      db.rollback()?;
      let _ = done_tx.send(());
      Ok(())
    })
  };
  let finished = done_rx.recv_timeout(DEADLINE).is_ok();
  drop(held);
  worker.join().expect("worker").expect("transactions");
  assert!(
    finished,
    "begins and read-only or rolled-back ends did not finish within {DEADLINE:?} while the \
     transaction manager's lock was held"
  );
  assert!(db.node_by_key("rolled-back").is_none());
  let tx_mgr = mvcc.tx_manager.lock();
  assert_eq!(tx_mgr.active_count(), 0, "every transaction ended in MVCC");
}
