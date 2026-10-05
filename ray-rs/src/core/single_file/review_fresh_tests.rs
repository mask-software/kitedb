//! Fresh adversarial review of the WAL segment / checkpoint thread change
//! (main..8f89b44). Included from checkpoint.rs for its private steps and
//! test hooks. Each test names the finding it demonstrates.
use super::*;
use crate::core::single_file::{
  close_single_file, open_single_file, SingleFileOpenOptions, SyncMode,
};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tempfile::tempdir;

/// The smallest WAL a database accepts: a 48 KiB primary region.
const SMALL_WAL: usize = 64 * 1024;

fn key(prefix: &str, index: usize) -> String {
  format!("{prefix}-{index:06}-{}", "k".repeat(200))
}

fn commit_key(db: &SingleFileDB, key: &str) -> Result<()> {
  db.begin(false)?;
  if let Err(error) = db.create_node(Some(key)) {
    let _ = db.rollback();
    return Err(error);
  }
  db.commit()
}

/// A transaction of `count` nodes (about 300 bytes of WAL each): past
/// `WAL_DEFER_BYTES` it writes its records to the WAL while open, so they
/// spill into WAL segments with it open. Commits it, or rolls it back.
fn big_transaction(
  db: &SingleFileDB,
  prefix: &str,
  start: usize,
  count: usize,
  commit: bool,
) -> Result<Vec<String>> {
  db.begin(false)?;
  let mut keys = Vec::with_capacity(count);
  for index in start..start + count {
    let key = key(prefix, index);
    if let Err(error) = db.create_node(Some(&key)) {
      let _ = db.rollback();
      return Err(error);
    }
    keys.push(key);
  }
  if commit {
    db.commit()?;
  } else {
    db.rollback()?;
  }
  Ok(keys)
}

/// In one session (no reopen, so not R7): four writers commit small and
/// large transactions and roll large ones back, while the checkpoint thread
/// and application background checkpoints run. Every acknowledged commit is
/// there, live and after a reopen, and no rolled-back write is. (Passes. With
/// the application's checkpoints, a few writes fail with `WalBufferFull`:
/// F1; they are not counted as acknowledged.)
///
/// (With F1's fix: no write fails. Automatic checkpoints are on, and every
/// transaction ends, so a writer at the segment limit always has a
/// checkpoint that frees space to wait for.)
#[test]
fn fresh_stress_one_session_keeps_every_acked_commit() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("stress.kitedb");
  let options = SingleFileOpenOptions::new()
    .wal_size(SMALL_WAL)
    .sync_mode(SyncMode::Normal);
  let db = Arc::new(open_single_file(&path, options.clone()).expect("open"));
  let stop = Arc::new(AtomicBool::new(false));
  let checkpointer = {
    let db = Arc::clone(&db);
    let stop = Arc::clone(&stop);
    std::thread::spawn(move || {
      let mut runs = 0;
      while !stop.load(Ordering::Acquire) {
        if db.background_checkpoint().is_ok() {
          runs += 1;
        }
        std::thread::sleep(Duration::from_millis(3));
      }
      runs
    })
  };
  let writers: Vec<_> = (0..4)
    .map(|writer| {
      let db = Arc::clone(&db);
      std::thread::spawn(move || {
        let mut acked = Vec::new();
        let mut rolled_back = Vec::new();
        let mut errors = Vec::new();
        for round in 0..400 {
          let prefix = format!("w{writer}-r{round}");
          match round % 4 {
            0 | 2 => {
              for index in 0..20 {
                let key = key(&prefix, index);
                match commit_key(&db, &key) {
                  Ok(()) => acked.push(key),
                  Err(error) => errors.push(error.to_string()),
                }
              }
            }
            1 => match big_transaction(&db, &prefix, 0, 300, true) {
              Ok(keys) => acked.extend(keys),
              Err(error) => errors.push(error.to_string()),
            },
            _ => match big_transaction(&db, &prefix, 0, 300, false) {
              Ok(keys) => rolled_back.extend(keys),
              Err(error) => errors.push(error.to_string()),
            },
          }
        }
        (acked, rolled_back, errors)
      })
    })
    .collect();
  let mut acked = Vec::new();
  let mut rolled_back = Vec::new();
  let mut errors = Vec::new();
  for writer in writers {
    let (a, r, e) = writer.join().expect("writer");
    acked.extend(a);
    rolled_back.extend(r);
    errors.extend(e);
  }
  stop.store(true, Ordering::Release);
  let runs = checkpointer.join().expect("checkpointer");
  eprintln!(
    "acked {} rolled back {} failed {} (first: {:?}) application runs {runs} spills {}",
    acked.len(),
    rolled_back.len(),
    errors.len(),
    errors.first(),
    db.wal_spills.load(Ordering::Relaxed)
  );
  let check = |db: &SingleFileDB, when: &str| {
    let missing: Vec<_> = acked
      .iter()
      .filter(|key| db.node_by_key(key).is_none())
      .collect();
    let present: Vec<_> = rolled_back
      .iter()
      .filter(|key| db.node_by_key(key).is_some())
      .collect();
    assert!(
      missing.is_empty(),
      "{when}: {} acked commits missing, e.g. {:?}",
      missing.len(),
      &missing[..missing.len().min(3)]
    );
    assert!(
      present.is_empty(),
      "{when}: {} rolled-back nodes present",
      present.len()
    );
  };
  check(&db, "live");
  assert!(
    errors.is_empty(),
    "{} writes failed beside application background checkpoints (first: {:?})",
    errors.len(),
    errors.first()
  );
  let db = Arc::try_unwrap(db).ok().expect("only handle");
  close_single_file(db).expect("close");
  let reopened = open_single_file(&path, options.clone().read_only(true)).expect("reopen");
  check(&reopened, "after reopen");
}

/// F1. A writer at the WAL segment limit asks the checkpoint thread for a
/// checkpoint. While a `background_checkpoint()` the application called is
/// running, the thread's run declines (`AlreadyRunning`), which still
/// "answers" the writer's request. The application's run cut before the
/// writer filled the segments, with a transaction open that pinned them, so
/// its install frees nothing; `wait_for_segment_space` then sees an answered
/// request and no frees, and fails the write with `WalBufferFull`, although
/// a checkpoint started now would free every segment (nothing is open any
/// more). Automatic checkpoints are on; nothing is pinned at the failure.
#[test]
fn fresh_writer_fails_when_a_declined_run_answers_its_checkpoint_request() {
  use std::sync::{mpsc, Barrier};
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("declined-answer.kitedb");
  let options = SingleFileOpenOptions::new()
    .wal_size(SMALL_WAL)
    .sync_mode(SyncMode::Normal);
  let db = Arc::new(open_single_file(&path, options).expect("open"));

  // T: a transaction whose records (about 60 KiB) outgrow the WAL while it is
  // open, so they spill into a WAL segment it then pins.
  let (to_t, t_rx) = mpsc::channel::<()>();
  let (t_ready, from_t) = mpsc::channel::<()>();
  let holder = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || {
      db.begin(false).expect("begin T");
      for index in 0..200 {
        db.create_node(Some(&key("t", index))).expect("T node");
      }
      t_ready.send(()).expect("T ready");
      t_rx.recv().expect("commit T");
      db.commit().expect("commit T");
      t_ready.send(()).expect("T committed");
    })
  };
  from_t.recv().expect("T wrote its records");
  assert!(
    db.oldest_pinned_segment().is_some(),
    "T's records did not spill while it was open"
  );

  // The application's background checkpoint: its cut sees T open, so its
  // install will keep (free) nothing. It is held right after the cut.
  let barrier = Arc::new(Barrier::new(2));
  set_checkpoint_test_barrier(&db, CheckpointPhase::CutReleased, Arc::clone(&barrier));
  let app_checkpoint = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || db.background_checkpoint())
  };
  let deadline = Instant::now() + Duration::from_secs(10);
  while !db.is_checkpoint_running() {
    assert!(
      Instant::now() < deadline,
      "the application's checkpoint never started"
    );
    std::thread::sleep(Duration::from_millis(1));
  }
  // Its cut is done once the WAL is empty (it spilled it).
  while db.wal_buffer.lock().used() != 0 {
    assert!(Instant::now() < deadline, "the cut never spilled");
    std::thread::sleep(Duration::from_millis(1));
  }

  // T commits after the cut: the segments it began in stay needed until a
  // later checkpoint covers its COMMIT record.
  to_t.send(()).expect("commit T");
  from_t.recv().expect("T committed");
  holder.join().expect("T thread");
  assert!(db.oldest_pinned_segment().is_none(), "nothing is open now");

  // The limit: two more WALs' worth of segments past what there is now.
  let bytes_now = wal_segment_test_stats(&db).bytes;
  set_wal_segment_test_limit(&db, bytes_now + 2 * 48 * 1024);

  // W: commits until one waits for segment space.
  let writer = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || {
      for index in 0..2_000 {
        if let Err(error) = commit_key(&db, &key("w", index)) {
          return Err((index, error));
        }
      }
      Ok(())
    })
  };
  while writers_waiting_for_segments(&db) == 0 {
    assert!(
      Instant::now() < deadline && !writer.is_finished(),
      "the writer never waited for segment space"
    );
    std::thread::sleep(Duration::from_millis(1));
  }
  // The checkpoint thread answered the writer's request: it declined, the
  // application's run holding the checkpoint status.
  db.wait_for_checkpoint_thread();

  // Release the application's run: its install frees nothing.
  barrier.wait();
  app_checkpoint
    .join()
    .expect("application checkpoint thread")
    .expect("application checkpoint");
  let written = writer.join().expect("writer thread");
  assert!(
    written.is_ok(),
    "a write failed although automatic checkpoints are on, nothing is pinned, and a checkpoint \
     now would free every segment: {:?}",
    written
      .err()
      .map(|(index, error)| format!("commit {index}: {error}"))
  );
}

/// F2. Without the checkpoint thread (`checkpoint_thread(false)`, and
/// always on wasm32) automatic checkpoints run inline: after each commit
/// once the log reaches the trigger, and in `make_segment_space_here` for a
/// writer at the WAL segment limit. When they fail, nothing records it:
/// `checkpoint_error()`, which the docs and bindings describe as "the error
/// of the last automatic checkpoint", stays `None` while writers get
/// `CheckpointFailed`; and nothing backs off: every commit past the trigger
/// cuts the log and builds and writes a whole snapshot again before it
/// returns (and each failed cut takes a WAL segment table entry, see F3).
#[test]
fn fresh_inline_auto_checkpoint_failures_are_not_recorded_or_backed_off() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("inline-failures.kitedb");
  let options = SingleFileOpenOptions::new()
    .wal_size(SMALL_WAL)
    .sync_mode(SyncMode::Normal)
    .checkpoint_thread(false);
  let db = open_single_file(&path, options).expect("open");
  set_checkpoint_test_db_fault(&db, CheckpointPhase::SnapshotDurable, true);

  // Up to the checkpoint trigger: the commit that reaches it runs the
  // (failing) automatic checkpoint.
  let mut index = 0;
  while db.log_usage_ratio() < 1.0 {
    commit_key(&db, &key("w", index)).expect("commit below the trigger");
    index += 1;
  }
  commit_key(&db, &key("w", index)).expect("the commit that runs the first checkpoint");

  // Twenty more commits while the failure lasts.
  let cuts_before = checkpoint_test_cuts(&db);
  let mut failed = Vec::new();
  for more in 0..20 {
    if let Err(error) = commit_key(&db, &key("more", more)) {
      failed.push(error);
    }
  }
  let cuts = checkpoint_test_cuts(&db) - cuts_before;
  let recorded = db.checkpoint_error();
  clear_checkpoint_test_db_faults(&db);
  eprintln!("recorded error: {recorded:?}; cuts during 20 commits: {cuts}; failed: {failed:?}");
  assert!(
    recorded.is_some(),
    "automatic checkpoints failed {cuts} times, but checkpoint_error() reports none"
  );
  assert!(
    cuts <= 2,
    "20 commits ran {cuts} failing automatic checkpoints (each a cut, a rebuild and a snapshot \
     write, on the committing thread): no back-off"
  );
}

/// F3. Every background checkpoint's cut seals the newest WAL segment, so
/// the next spill takes a new table entry; a cut whose run then fails keeps
/// its entries (nothing installs, nothing is dropped). After enough failed
/// runs the 64-entry table is full, and from then on `cut_log` refuses to
/// cut whenever the WAL holds records (`may_spill` is false: it would need a
/// new entry), although it could cover the segments without spilling. So
/// once the cause of the failures is gone, no background checkpoint (the
/// checkpoint thread's or the application's) ever cuts again, nothing frees
/// the table, and every write that needs a spill fails, for good: in this
/// session and after a reopen, until a blocking `checkpoint()`.
///
/// Default options (checkpoint thread on). The failures here are 64
/// application `background_checkpoint()` calls, which have no back-off;
/// the checkpoint thread's own failed runs get there too, over its back-off
/// (about an hour of a persistent failure), and so do runs abandoned by
/// closes, one a session; without the thread (`checkpoint_thread(false)`,
/// and on wasm32) 64 failing commits do it at once (see F2).
///
/// (Setup changed with the fix, which unseals the segment a failed run's
/// cut sealed: a failed cut then takes no table entry, so a commit before
/// each run no longer fills the table. With the smallest extents (one and a
/// half WALs), each round commits more than an extent holds instead, so each
/// takes an entry of its own while every checkpoint fails, with the byte
/// limit lifted: the table fills all the same, the last entry by a failed
/// cut, and a full table is what wedged background checkpoints.
/// `fresh_failed_cuts_take_no_table_entries` pins the unsealing.)
#[test]
fn fresh_failed_cuts_fill_the_segment_table_and_wedge_background_checkpoints() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("wedged-table.kitedb");
  let options = SingleFileOpenOptions::new()
    .wal_size(SMALL_WAL)
    .sync_mode(SyncMode::Normal)
    .wal_segment_size(1);
  let db = open_single_file(&path, options.clone()).expect("open");

  // A transient failure: the background checkpoints fail after their cuts,
  // until the table is full.
  set_wal_segment_test_limit(&db, u64::MAX / 4);
  set_checkpoint_test_db_fault(&db, CheckpointPhase::SnapshotDurable, true);
  let mut index = 0;
  while wal_segment_test_stats(&db).live < crate::constants::MAX_WAL_SEGMENTS && index < 200 {
    // About 120 KiB: more than an extent (one and a half WALs) holds.
    let _ = big_transaction(&db, &format!("pre{index}"), 0, 400, true);
    assert!(
      db.background_checkpoint().is_err(),
      "the fault did not fire"
    );
    index += 1;
  }
  clear_checkpoint_test_db_faults(&db);
  assert_eq!(
    wal_segment_test_stats(&db).live,
    crate::constants::MAX_WAL_SEGMENTS,
    "the failed cuts did not fill the table"
  );

  // The failure is gone. The application writes and checkpoints.
  let mut acked = 0;
  let mut first_error = None;
  for round in 0..20 {
    for write in 0..50 {
      match commit_key(&db, &key(&format!("post{round}"), write)) {
        Ok(()) => acked += 1,
        Err(error) => {
          first_error.get_or_insert(error);
        }
      }
    }
    let _ = db.background_checkpoint();
  }
  let stats = wal_segment_test_stats(&db);

  // A reopen does not help either; a blocking checkpoint does.
  close_single_file(db).expect("close");
  let db = open_single_file(&path, options).expect("reopen");
  let _ = db.background_checkpoint();
  let after_reopen = (0..300)
    .map(|write| commit_key(&db, &key("reopened", write)))
    .find_map(|result| result.err());
  db.checkpoint().expect("blocking checkpoint");
  let after_blocking = (0..300)
    .map(|write| commit_key(&db, &key("blocking", write)))
    .find_map(|result| result.err());
  eprintln!(
    "after reopen + background checkpoint: {after_reopen:?}; after a blocking checkpoint: \
     {after_blocking:?}"
  );
  assert!(
    first_error.is_none(),
    "writes still fail after the checkpoint failures stopped ({acked} of 1000 acked; first: \
     {first_error:?}); the table holds {} segments, covered {}, and every background checkpoint \
     since found nothing it could cut (after a reopen too: {after_reopen:?})",
    stats.live,
    stats.covered
  );
}

/// F3, its other half: a background checkpoint whose run fails after its
/// cut leaves the segment table as it found it. Its cut sealed the newest
/// segment (so its snapshot would cover whole segments); with no install
/// coming, the seal is undone, and the next spill fills that segment rather
/// than taking a new entry. A hundred failed runs, a commit before each,
/// leave one segment, not a full table.
#[test]
fn fresh_failed_cuts_take_no_table_entries() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("failed-cuts.kitedb");
  let options = SingleFileOpenOptions::new()
    .wal_size(SMALL_WAL)
    .sync_mode(SyncMode::Normal)
    .auto_checkpoint(false);
  let db = open_single_file(&path, options.clone()).expect("open");
  set_checkpoint_test_db_fault(&db, CheckpointPhase::SnapshotDurable, true);
  let mut acked = Vec::new();
  for index in 0..100 {
    let key = key("pre", index);
    if commit_key(&db, &key).is_ok() {
      acked.push(key);
    }
    let run = db.background_checkpoint();
    assert!(
      run.is_err(),
      "run {index} did not fail ({run:?}): it found nothing to cut, with segments {:?}",
      wal_segment_test_stats(&db)
    );
  }
  clear_checkpoint_test_db_faults(&db);
  let stats = wal_segment_test_stats(&db);
  assert_eq!(acked.len(), 100, "commits failed while the runs did");
  assert_eq!(
    stats.live, 1,
    "100 failed runs left {} segments (each failed cut sealed one)",
    stats.live
  );
  // The segment holds every commit, in order: a reopen replays them.
  close_single_file(db).expect("close");
  let reopened = open_single_file(&path, options).expect("reopen");
  for key in &acked {
    assert!(reopened.node_by_key(key).is_some(), "{key} lost");
  }
}

/// Probe (unguarded invariants 2 and 8 of the test review): a transaction
/// whose records spilled before a background checkpoint's cut commits
/// between the run's two post-cut replay rounds, a later transaction
/// overwrites what it wrote, and a spill lands between the rounds. Each is
/// applied once, in order: live, and after a reopen.
#[test]
fn fresh_probe_commit_and_spill_between_post_cut_replay_rounds() {
  use std::sync::{mpsc, Barrier};
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("between-rounds.kitedb");
  let options = SingleFileOpenOptions::new()
    .wal_size(SMALL_WAL)
    .sync_mode(SyncMode::Normal)
    .auto_checkpoint(false);
  let db = Arc::new(open_single_file(&path, options.clone()).expect("open"));
  db.begin(false).expect("begin");
  let etype = db.define_etype("link").expect("etype");
  let prop = db.define_propkey("value").expect("propkey");
  let a = db.create_node(Some("a")).expect("a");
  let b = db.create_node(Some("b")).expect("b");
  db.set_node_prop(a, prop, PropValue::I64(0)).expect("prop");
  db.add_edge(a, etype, b).expect("edge");
  db.commit().expect("commit");

  // T: overwrites a's value, deletes the edge, and spills while open.
  let (to_t, t_rx) = mpsc::channel::<()>();
  let (t_done, from_t) = mpsc::channel::<()>();
  let holder = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || {
      db.begin(false).expect("begin T");
      db.set_node_prop(a, prop, PropValue::I64(1))
        .expect("T prop");
      db.delete_edge(a, etype, b).expect("T delete");
      for index in 0..200 {
        db.create_node(Some(&key("t", index))).expect("T node");
      }
      t_done.send(()).expect("T wrote");
      t_rx.recv().expect("commit T");
      db.commit().expect("commit T");
      t_done.send(()).expect("T committed");
    })
  };
  from_t.recv().expect("T wrote");
  assert!(db.oldest_pinned_segment().is_some(), "T did not spill");

  // The background checkpoint, held after its first post-cut read.
  let barrier = Arc::new(Barrier::new(2));
  set_checkpoint_test_barrier(&db, CheckpointPhase::PostCutReplay, Arc::clone(&barrier));
  watch_checkpoint_phases(&db);
  let run = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || db.background_checkpoint())
  };
  let deadline = Instant::now() + Duration::from_secs(20);
  while !checkpoint_test_reached(&db)
    .iter()
    .any(|(phase, _, _)| *phase == CheckpointPhase::PostCutReplay)
  {
    assert!(
      Instant::now() < deadline,
      "the run never reached its post-cut replay"
    );
    std::thread::sleep(Duration::from_millis(1));
  }

  // Between the rounds: T commits, U overwrites T's writes, and enough
  // commits follow to spill the WAL.
  to_t.send(()).expect("commit T");
  from_t.recv().expect("T committed");
  holder.join().expect("T thread");
  db.begin(false).expect("begin U");
  db.set_node_prop(a, prop, PropValue::I64(2))
    .expect("U prop");
  db.add_edge(a, etype, b).expect("U edge");
  db.commit().expect("commit U");
  let spills = db.wal_spills.load(Ordering::Acquire);
  let mut fillers = Vec::new();
  let mut index = 0;
  while db.wal_spills.load(Ordering::Acquire) == spills && index < 2_000 {
    let key = key("fill", index);
    commit_key(&db, &key).expect("filler");
    fillers.push(key);
    index += 1;
  }
  assert!(
    db.wal_spills.load(Ordering::Acquire) > spills,
    "no spill between the rounds"
  );
  barrier.wait();
  run.join().expect("run").expect("background checkpoint");

  let check = |db: &SingleFileDB, when: &str| {
    assert_eq!(
      db.node_prop(a, prop),
      Some(PropValue::I64(2)),
      "{when}: a's value"
    );
    assert!(db.edge_exists(a, etype, b), "{when}: U's edge");
    for index in 0..200 {
      assert!(
        db.node_by_key(&key("t", index)).is_some(),
        "{when}: T's node {index}"
      );
    }
    for key in &fillers {
      assert!(db.node_by_key(key).is_some(), "{when}: {key}");
    }
  };
  check(&db, "live");
  let db = Arc::try_unwrap(db).ok().expect("one handle");
  close_single_file(db).expect("close");
  let reopened = open_single_file(&path, options.read_only(true)).expect("reopen");
  check(&reopened, "after reopen");
}
