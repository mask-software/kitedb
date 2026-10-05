//! Adversarial review of `fix/b4-checkpoint-segments` (WAL segments and the
//! checkpoint thread, d21717a). Each test reproduces one bug the review
//! found, and fails on that commit. Included from checkpoint.rs for its
//! private steps and test hooks.
use super::*;
use crate::constants::MAX_WAL_SEGMENTS;
use crate::core::single_file::{open_single_file, SingleFileOpenOptions, SyncMode};
use std::sync::mpsc;
use std::sync::Arc;
use tempfile::tempdir;

/// The smallest WAL a database accepts: a 48 KiB primary region.
const SMALL_WAL: usize = 64 * 1024;

/// The key of the `index`th node of `prefix`; its commit takes about 300
/// bytes of WAL.
fn key(prefix: &str, index: usize) -> String {
  format!("{prefix}-{index:05}-{}", "k".repeat(200))
}

fn commit_key(db: &SingleFileDB, key: &str) -> Result<()> {
  db.begin(false)?;
  if let Err(error) = db.create_node(Some(key)) {
    let _ = db.rollback();
    return Err(error);
  }
  db.commit()
}

/// R1. A background checkpoint chooses its snapshot's pages at the end of
/// the file (`checkpoint_snapshot_start_page`, which then releases the pager
/// lock) and extends the file and writes them only later
/// (`write_unnamed_snapshot_pages`). A writer's spill in between needs only
/// the commit and pager locks: `allocate_wal_segment_extent` finds no free
/// range either and takes the same end-of-file pages for its WAL segment,
/// which it syncs and names in both header slots; the snapshot is then
/// written over it. This replays that interleaving of the two threads'
/// pager-lock sections, step by step, with the functions the checkpoint
/// thread runs (`write_new_snapshot`'s two steps) and real commits for the
/// writer.
#[test]
fn review_snapshot_pages_chosen_at_eof_are_not_taken_by_a_concurrent_spill() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("eof-race.kitedb");
  let options = SingleFileOpenOptions::new()
    .wal_size(SMALL_WAL)
    .sync_mode(SyncMode::Normal)
    .auto_checkpoint(false);
  let db = open_single_file(&path, options.clone()).expect("open");
  let mut acked = Vec::new();
  for index in 0..100 {
    let key = key("pre", index);
    commit_key(&db, &key).expect("commit");
    acked.push(key);
  }

  // The checkpoint thread, in `write_new_snapshot`: its snapshot, and the
  // pages chosen for it (no free range yet: the end of the file).
  let header = db.header.read().clone();
  let generation = header.active_snapshot_gen + 1;
  let graph = db.collect_graph_data().expect("collect");
  let (buffer, _stores) = db.build_snapshot_buffer(generation, graph).expect("build");
  let page_size = header.page_size as usize;
  let page_count = pages_to_store(buffer.len(), page_size) as u64;
  let start = db
    .checkpoint_snapshot_start_page(&header, page_count)
    .expect("snapshot pages");

  // A writer, meanwhile: its commits fill the WAL, which spills.
  let spills = wal_segment_test_stats(&db).next_seq;
  let mut index = 0;
  while wal_segment_test_stats(&db).next_seq == spills && index < 1_000 {
    let key = key("spill", index);
    commit_key(&db, &key).expect("commit");
    acked.push(key);
    index += 1;
  }
  let segment = *db
    .header
    .read()
    .wal_segments
    .entries
    .last()
    .expect("setup: the WAL spilled into a segment");

  // The checkpoint thread goes on: it writes its snapshot where it chose.
  db.write_unnamed_snapshot_pages(start as u32, &buffer, page_size)
    .expect("write the snapshot");
  let overlap = segment.start_page < start + page_count && start < segment.end_page();
  drop(db);

  let reopened = open_single_file(&path, options);
  let outcome = match &reopened {
    Ok(db) => format!(
      "the reopened database misses {} of {} acknowledged commits",
      acked
        .iter()
        .filter(|key| db.node_by_key(key).is_none())
        .count(),
      acked.len()
    ),
    Err(error) => format!("reopening fails: {error}"),
  };
  let intact = reopened
    .as_ref()
    .is_ok_and(|db| acked.iter().all(|key| db.node_by_key(key).is_some()));
  assert!(
    !overlap && intact,
    "the snapshot's pages {start}..{} overlap the spill's WAL segment {segment:?} \
     (overlap: {overlap}); {outcome}",
    start + page_count
  );
}

/// R2. A checkpoint run starts by rolling back abandoned transactions
/// (`run_background_checkpoint` -> `reap_abandoned_transactions`). On the
/// checkpoint thread, a ROLLBACK record that does not fit in a full WAL
/// while the segments are at their limit makes the thread wait in
/// `wait_for_segment_space` for a checkpoint: its own, which never starts.
/// Every writer that needs segment space then waits forever too (the cut of
/// the checkpoint that never runs would spill whatever the limit).
#[test]
fn review_checkpoint_thread_reaping_an_abandoned_transaction_at_the_limit_does_not_hang() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("reap-hang.kitedb");
  // MVCC: the writer and the abandoned transaction are open at once.
  let options = SingleFileOpenOptions::new()
    .wal_size(SMALL_WAL)
    .sync_mode(SyncMode::Normal)
    .mvcc(true);
  let db = Arc::new(open_single_file(&path, options).expect("open"));
  // A WAL segment, and a WAL that just spilled; below the checkpoint
  // trigger (four WALs).
  let mut index = 0;
  while wal_segment_test_stats(&db).live == 0 && index < 1_000 {
    commit_key(&db, &key("pre", index)).expect("commit");
    index += 1;
  }
  assert!(wal_segment_test_stats(&db).live > 0, "setup: no spill");
  assert!(!db.should_checkpoint(1.0), "setup: past the trigger");

  // A writer begins a small transaction (its records stay with it until it
  // commits). Begun now: every begin rolls back abandoned transactions.
  let (began_tx, began_rx) = mpsc::channel();
  let (go_tx, go_rx) = mpsc::channel::<()>();
  let (done_tx, done_rx) = mpsc::channel();
  let writer = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || {
      db.begin(false).expect("begin");
      db.create_node(Some(&key("writer", 0))).expect("node");
      began_tx.send(()).expect("signal");
      go_rx.recv().expect("wait");
      let _ = done_tx.send(db.commit());
    })
  };
  began_rx.recv().expect("the writer began");

  // A thread writes a transaction whose records go to the WAL (more than it
  // keeps back), filling the WAL to the last byte, and ends with it open:
  // abandoned, for the next begin or checkpoint to roll back.
  {
    let db = Arc::clone(&db);
    std::thread::spawn(move || {
      db.begin(false).expect("begin");
      let mut n = 0;
      while db.wal_buffer.lock().free() > 8 * 1024 {
        db.create_node(Some(&format!("abandoned-{n:06}-{}", "a".repeat(1000))))
          .expect("node");
        n += 1;
      }
      // A CreateNode record takes 36 bytes besides its key, padded to 8.
      let free = db.wal_buffer.lock().free() as usize;
      let prefix = format!("abandoned-{n:06}-");
      let last = format!("{prefix}{}", "a".repeat(free - 36 - prefix.len()));
      db.create_node(Some(&last)).expect("the last node");
      assert_eq!(
        db.wal_buffer.lock().free(),
        0,
        "setup: the WAL is not full to the byte"
      );
    })
    .join()
    .expect("the abandoning thread");
  }
  assert_eq!(
    wal_segment_test_stats(&db).live,
    1,
    "setup: the abandoned transaction's records spilled"
  );
  // The segments are at their limit.
  set_wal_segment_test_limit(&db, 1);

  // The writer's commit does not fit: it asks the checkpoint thread for a
  // checkpoint, whose cut spills whatever the limit and whose install frees
  // the segment, and waits for it.
  go_tx.send(()).expect("release the writer");
  let outcome = done_rx.recv_timeout(Duration::from_secs(10));
  // Unstick the waiters before asserting (closing stops the thread).
  db.stop_checkpoint_thread();
  writer.join().expect("the writer thread");
  match outcome {
    Ok(result) => result.expect("the writer's commit"),
    Err(_) => panic!(
      "the writer's commit hung: the checkpoint thread, rolling back the abandoned \
       transaction before its checkpoint, waits for WAL segment space only that checkpoint \
       frees"
    ),
  }
}

/// R3. Writers keep one table entry free for a checkpoint's cut, but an
/// install keeps every segment from the oldest an open transaction holds
/// records in, so while one is open the cuts fill the table to all 64
/// entries. Once that transaction ends, no checkpoint can ever cover the
/// log again: each cut must spill the WAL into a new segment (the last one
/// is sealed) and finds the table full (`spill_wal`: `WalBufferFull`). With
/// automatic checkpoints every run fails, and writers at the segment limit
/// get `CheckpointFailed`, until someone runs a blocking `checkpoint()`.
///
/// (Adjusted with the fix: while such a transaction is open, cuts leave the
/// last entry free, so the table fills to 63 entries and more checkpoints
/// keep it there; the ones after the transaction ends must cover the log
/// and empty the table. Adjusted with decision Q1 of the fresh review: a
/// background checkpoint that can do nothing, here because the transaction
/// holds every segment and the WAL's records would need the last entry,
/// says so with `CheckpointDeclined` instead of returning `Ok`.)
#[test]
fn review_checkpoints_recover_once_a_transaction_holding_a_full_segment_table_ends() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("table-wedge.kitedb");
  let options = SingleFileOpenOptions::new()
    .wal_size(SMALL_WAL)
    .sync_mode(SyncMode::Normal)
    .auto_checkpoint(false)
    .wal_segment_size(1)
    .mvcc(true);
  let db = Arc::new(open_single_file(&path, options).expect("open"));

  // A write transaction with records in the log, open throughout.
  let (held_tx, held_rx) = mpsc::channel();
  let (go_tx, go_rx) = mpsc::channel::<()>();
  let holder = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || {
      db.begin(false).expect("begin");
      for n in 0..20 {
        db.create_node(Some(&format!("held-{n}-{}", "h".repeat(1000))))
          .expect("node");
      }
      held_tx.send(()).expect("signal");
      go_rx.recv().expect("wait");
      db.commit()
    })
  };
  held_rx.recv().expect("the holder wrote");

  // Each checkpoint's cut spills the WAL into a new segment (the previous
  // cut sealed the last one), and its install keeps them all for the open
  // transaction, up to the last entry, which cuts leave free while it is
  // open.
  let mut index = 0;
  while wal_segment_test_stats(&db).live < MAX_WAL_SEGMENTS - 1 && index < 200 {
    commit_key(&db, &key("k", index)).expect("commit");
    db.background_checkpoint()
      .expect("a checkpoint while the transaction is open");
    index += 1;
  }
  assert_eq!(
    wal_segment_test_stats(&db).live,
    MAX_WAL_SEGMENTS - 1,
    "setup: the table did not fill"
  );
  for more in 0..3 {
    commit_key(&db, &key("more", more)).expect("commit");
    let declined = db.background_checkpoint();
    assert!(
      matches!(&declined, Err(KiteError::CheckpointDeclined(reason)) if reason.contains("open write transactions")),
      "a checkpoint that can do nothing while the transaction holds a full table: {declined:?}"
    );
    assert_eq!(
      wal_segment_test_stats(&db).live,
      MAX_WAL_SEGMENTS - 1,
      "a checkpoint took the table's last entry while the transaction is open"
    );
  }

  // The transaction commits: nothing holds the segments any more.
  go_tx.send(()).expect("release the holder");
  holder
    .join()
    .expect("the holder thread")
    .expect("the holder's commit");
  commit_key(&db, &key("after", 0)).expect("commit");
  let first = db.background_checkpoint();
  let after_first = wal_segment_test_stats(&db);
  let second = db.background_checkpoint();
  assert!(
    first.is_ok() && second.is_ok() && after_first.live == 0,
    "no checkpoint covers the log once an install kept all {MAX_WAL_SEGMENTS} segments: \
     {first:?}, {second:?} (after the first: {after_first:?})"
  );
}

/// R4. A cut with nothing to spill and no segment past those the snapshot
/// covers returns `None` (`cut_log`), so the checkpoint frees nothing even
/// when the segments it covered and kept for a transaction that has since
/// ended without a record (a commit that failed its conflict check writes
/// none) are what holds the segments at their limit. A commit too large
/// for the WAL, arriving while the WAL is empty, then waits forever: it
/// asks the checkpoint thread every 100 ms, each run does nothing, and
/// nothing reports an error (without the thread, `make_segment_space_here`
/// spins).
#[test]
fn review_a_commit_larger_than_the_wal_does_not_wait_forever_for_covered_segments() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("empty-wal-hang.kitedb");
  let options = SingleFileOpenOptions::new()
    .wal_size(SMALL_WAL)
    .sync_mode(SyncMode::Normal)
    .mvcc(true);
  let db = Arc::new(open_single_file(&path, options).expect("open"));
  db.begin(false).expect("begin");
  let node = db.create_node(Some("contended")).expect("node");
  let prop = db.define_propkey("value").expect("propkey");
  db.set_node_prop(node, prop, PropValue::I64(0))
    .expect("prop");
  db.commit().expect("commit");

  // A transaction writes the node, then enough that its records go to the
  // WAL; another commits the node first, so its commit will fail the
  // conflict check, writing nothing more.
  let (wrote_tx, wrote_rx) = mpsc::channel();
  let (go_tx, go_rx) = mpsc::channel::<()>();
  let loser = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || {
      db.begin(false).expect("begin");
      db.set_node_prop(node, prop, PropValue::I64(1))
        .expect("prop");
      for n in 0..20 {
        db.create_node(Some(&format!("loser-{n}-{}", "l".repeat(1000))))
          .expect("node");
      }
      wrote_tx.send(()).expect("signal");
      go_rx.recv().expect("wait");
      db.commit()
    })
  };
  wrote_rx.recv().expect("the loser wrote");
  db.begin(false).expect("begin");
  db.set_node_prop(node, prop, PropValue::I64(2))
    .expect("prop");
  db.commit().expect("the winner commits");

  // A checkpoint: its cut spills the WAL, and its install keeps that
  // segment, which the open transaction holds records in.
  db.background_checkpoint().expect("checkpoint");
  let stats = wal_segment_test_stats(&db);
  assert!(stats.live > 0 && stats.covered > 0, "setup: {stats:?}");
  // The kept segment is at the limit.
  set_wal_segment_test_limit(&db, 1);
  go_tx.send(()).expect("release the loser");
  let lost = loser.join().expect("the loser thread");
  assert!(
    matches!(lost, Err(KiteError::Conflict { .. })),
    "setup: the loser's commit returned {lost:?}"
  );
  assert!(
    db.wal_buffer.lock().is_empty(),
    "setup: the WAL is not empty"
  );

  // A bulk commit larger than the WAL (about 90 KiB): it can only go to a
  // segment, and waits for a checkpoint to free one. The segment is
  // covered, and no open transaction holds it.
  let (done_tx, done_rx) = mpsc::channel();
  let bulk = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || {
      let keys: Vec<String> = (0..300).map(|index| key("bulk", index)).collect();
      let refs: Vec<Option<&str>> = keys.iter().map(|key| Some(key.as_str())).collect();
      db.begin_bulk().expect("begin bulk");
      db.create_nodes_batch(&refs).expect("nodes");
      let _ = done_tx.send(db.commit());
    })
  };
  let outcome = done_rx.recv_timeout(Duration::from_secs(10));
  // Unstick the waiter before asserting (closing stops the thread).
  db.stop_checkpoint_thread();
  bulk.join().expect("the bulk thread");
  match outcome {
    Ok(result) => result.expect("the bulk commit"),
    Err(_) => panic!(
      "the bulk commit hung: every checkpoint it asked for found nothing to cut, and the \
       covered segment no transaction holds stayed at the limit ({:?})",
      wal_segment_test_stats(&db)
    ),
  }
}

/// R5. After a failed run the checkpoint thread is meant to wait (one
/// second, doubling to a minute) before the next. The wait is
/// `Condvar::wait_for`, and every request (each commit past the trigger
/// makes one) wakes it: failing checkpoints, each a full snapshot build,
/// run back to back while commits go on.
#[test]
fn review_a_failing_checkpoint_thread_backs_off_between_runs() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("backoff.kitedb");
  let options = SingleFileOpenOptions::new()
    .wal_size(SMALL_WAL)
    .sync_mode(SyncMode::Normal);
  let db = open_single_file(&path, options).expect("open");
  watch_checkpoint_phases(&db);
  set_checkpoint_test_db_fault(&db, CheckpointPhase::SnapshotWritten, true);
  let failed_runs = |db: &SingleFileDB| {
    checkpoint_test_reached(db)
      .iter()
      .filter(|(phase, _, _)| *phase == CheckpointPhase::SnapshotWritten)
      .count()
  };
  let mut index = 0;
  while checkpoint_thread_error(&db).is_none() && index < 5_000 {
    let _ = commit_key(&db, &key("a", index));
    index += 1;
  }
  assert!(
    checkpoint_thread_error(&db).is_some(),
    "setup: no checkpoint failed"
  );
  let first = failed_runs(&db);
  // Commits go on for half a second, inside the first backoff (one
  // second); each one past the trigger asks for a checkpoint.
  let until = Instant::now() + Duration::from_millis(500);
  while Instant::now() < until {
    let _ = commit_key(&db, &key("b", index));
    index += 1;
  }
  let retries = failed_runs(&db) - first;
  clear_checkpoint_test_db_faults(&db);
  assert!(
    retries <= 1,
    "{retries} checkpoints ran (and failed) within half a second of the first failure: the \
     thread does not back off while commits keep asking"
  );
}

/// R6. `wal_segment_limit` computes `4 * checkpoint_log_budget` (and
/// `2 * trigger`) unchecked. The option is only checked to be above 0, so a
/// budget above `u64::MAX / 4` (`KiteOptions::checkpoint_log_budget_mb`
/// saturates to `u64::MAX`) overflows at the first spill: a panic in a
/// commit in debug builds.
#[test]
fn review_a_huge_checkpoint_log_budget_does_not_overflow() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("budget-overflow.kitedb");
  let options = SingleFileOpenOptions::new()
    .wal_size(SMALL_WAL)
    .sync_mode(SyncMode::Normal)
    .auto_checkpoint(false)
    .checkpoint_log_budget(u64::MAX);
  let db = Arc::new(open_single_file(&path, options).expect("open"));
  let writer = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || {
      for index in 0..400 {
        commit_key(&db, &key("k", index)).expect("commit");
      }
    })
  };
  assert!(
    writer.join().is_ok(),
    "commits that spill panicked with checkpoint_log_budget(u64::MAX)"
  );
}
