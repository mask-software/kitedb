//! raydb-b4 `checkpoint-segments`: WAL segments and a checkpoint thread
//! (design: `raydb-b4/_SEGMENTS_DESIGN.md`). Included from checkpoint.rs for
//! its hooks.
//!
//! When the WAL fills, its records spill into a WAL segment (an extent of
//! pages the header names) instead of forcing a checkpoint that rewrites the
//! whole database; checkpoints cut at a spill and run on a per-database
//! thread. These pin down the crash points that adds (a spill, a cut, an
//! install that releases segments), reclamation and backpressure, the
//! checkpoint thread's errors and lifecycle, readers across its installs,
//! read-only opens of a log with segments, and commits larger than the WAL.
use super::*;
use crate::core::pager::io_hooks::{self, IoEvent};
use crate::core::single_file::{
  close_single_file, open_single_file, SingleFileOpenOptions, SyncMode,
};
use std::sync::mpsc;
use std::sync::{Arc, Barrier};
use tempfile::tempdir;

/// The smallest WAL a database accepts (16 pages): the primary region is
/// 48 KiB, so a few hundred small commits fill it.
const SMALL_WAL: usize = 64 * 1024;

fn options() -> SingleFileOpenOptions {
  SingleFileOpenOptions::new().wal_size(SMALL_WAL)
}

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

/// Commit `count` keys of `prefix` from `start` on; returns those acknowledged.
fn commit_keys(db: &SingleFileDB, prefix: &str, start: usize, count: usize) -> Vec<String> {
  (start..start + count)
    .filter_map(|index| {
      let key = key(prefix, index);
      commit_key(db, &key).ok().map(|()| key)
    })
    .collect()
}

fn missing<'k>(db: &SingleFileDB, keys: &'k [String]) -> Vec<&'k String> {
  keys
    .iter()
    .filter(|key| db.node_by_key(key).is_none())
    .collect()
}

fn wait_for(what: &str, deadline: Instant, mut done: impl FnMut() -> bool) -> bool {
  while !done() {
    if Instant::now() >= deadline {
      eprintln!("gave up waiting for {what}");
      return false;
    }
    std::thread::sleep(Duration::from_millis(1));
  }
  true
}

fn apply(image: &mut Vec<u8>, offset: u64, data: &[u8]) {
  let (start, end) = (offset as usize, offset as usize + data.len());
  if image.len() < end {
    image.resize(end, 0);
  }
  image[start..end].copy_from_slice(data);
}

/// The disk after a crash at each point of `events`, from `base`: every
/// write so far landed, in order; and the writes before the last successful
/// sync landed but after it only the header pages' (below `header_end`).
/// Two images per point, in that order.
fn crash_images(base: &[u8], events: &[IoEvent], header_end: u64) -> Vec<(String, Vec<u8>)> {
  let mut images = Vec::new();
  for cut in 0..=events.len() {
    let prefix = &events[..cut];
    let last_sync = prefix
      .iter()
      .rposition(|event| matches!(event, IoEvent::Sync { ok: true }));
    let mut in_order = base.to_vec();
    let mut reordered = base.to_vec();
    for (index, event) in prefix.iter().enumerate() {
      let IoEvent::Write { offset, data } = event else {
        continue;
      };
      apply(&mut in_order, *offset, data);
      if last_sync.is_some_and(|sync| index < sync) || *offset < header_end {
        apply(&mut reordered, *offset, data);
      }
    }
    images.push((format!("crash after {cut} events, in order"), in_order));
    images.push((
      format!("crash after {cut} events, headers ahead of data"),
      reordered,
    ));
  }
  images
}

/// S1. A spill copies the WAL's records into a segment, syncs it, and
/// installs a header naming it with an empty WAL in both slots. A crash at
/// any write or sync of the commits around it (in order, or with the
/// header pages ahead of the rest) keeps every acknowledged commit.
#[test]
fn spill_survives_a_crash_at_every_io_event() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("spill-crash.kitedb");
  // No checkpoint: only spills make room.
  let options = options().sync_mode(SyncMode::Full).auto_checkpoint(false);
  let db = open_single_file(&path, options.clone()).expect("open");
  let header_end = 2 * db.header.read().page_size as u64;
  // Most of the primary region, so the commits below spill.
  let mut acked = Vec::new();
  let mut index = 0;
  while db.wal_stats().primary_head < 40 * 1024 && index < 1_000 {
    acked.extend(commit_keys(&db, "pre", index, 1));
    index += 1;
  }
  let base = std::fs::read(&path).expect("base image");
  // Each commit's writes and syncs; `bounds[i]` events precede the
  // acknowledgement of `tail[i]`.
  let (mut events, mut bounds, mut tail) = (Vec::new(), Vec::new(), Vec::new());
  for offset in 0..40 {
    let key = key("tail", index + offset);
    let (result, mut commit_events) = io_hooks::record_io_during(|| commit_key(&db, &key));
    result.expect("commit");
    events.append(&mut commit_events);
    bounds.push(events.len());
    tail.push(key);
  }
  let spilled = wal_segment_test_stats(&db).next_seq > 0;
  drop(db);
  assert!(
    spilled,
    "the WAL never spilled into a segment: the commits that filled it ran checkpoints instead"
  );

  let image_path = dir.path().join("spill-crash-image.kitedb");
  for (image_index, (what, image)) in crash_images(&base, &events, header_end)
    .into_iter()
    .enumerate()
  {
    let cut = image_index / 2;
    std::fs::write(&image_path, &image).expect("write image");
    let crashed = open_single_file(&image_path, options.clone())
      .unwrap_or_else(|error| panic!("{what}: unopenable: {error:?}"));
    let acked_by_now: Vec<String> = acked
      .iter()
      .cloned()
      .chain(
        tail
          .iter()
          .zip(&bounds)
          .filter(|(_, bound)| **bound <= cut)
          .map(|(key, _)| key.clone()),
      )
      .collect();
    let lost = missing(&crashed, &acked_by_now).len();
    close_single_file(crashed).expect("close");
    assert_eq!(lost, 0, "{what}: lost {lost} acknowledged commits");
    std::fs::remove_file(&image_path).expect("remove image");
  }
}

/// Commit keys of `prefix` until `phase` is reached on `db` (at most
/// `limit`); returns the keys acknowledged and whether it was reached.
fn commit_until_reached(
  db: &SingleFileDB,
  prefix: &str,
  phase: CheckpointPhase,
  limit: usize,
) -> (Vec<String>, bool) {
  let mut acked = Vec::new();
  for index in 0..limit {
    acked.extend(commit_keys(db, prefix, index, 1));
    if checkpoint_test_reached(db)
      .iter()
      .any(|(reached, _, _)| *reached == phase)
    {
      return (acked, true);
    }
  }
  (acked, false)
}

/// Open a copy of the file as a crash now would leave it, and check that it
/// holds `keys`.
fn assert_crash_copy_holds(path: &std::path::Path, keys: &[String], context: &str) {
  let copy = path.with_extension("crash.kitedb");
  std::fs::copy(path, &copy).expect("copy the file");
  let crashed = open_single_file(&copy, options())
    .unwrap_or_else(|error| panic!("{context}: crash copy unopenable: {error:?}"));
  let lost = missing(&crashed, keys).len();
  close_single_file(crashed).expect("close");
  std::fs::remove_file(&copy).expect("remove the copy");
  assert_eq!(lost, 0, "{context}: the crash copy lost {lost} commits");
}

/// S2, a spill's own steps: a spill that fails after its segment is
/// written, or after one header slot names it, leaves a database that a
/// crash right then reopens with every acknowledged commit, and that keeps
/// working (a later spill succeeds).
#[test]
fn spill_fails_safely_at_each_of_its_steps() {
  for phase in [
    CheckpointPhase::SpillSegmentWritten,
    CheckpointPhase::SpillHeaderDurable,
  ] {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("spill-fault.kitedb");
    // No checkpoint: only spills make room.
    let options = options().sync_mode(SyncMode::Normal).auto_checkpoint(false);
    let db = open_single_file(&path, options.clone()).expect("open");
    watch_checkpoint_phases(&db);
    set_checkpoint_test_db_fault(&db, phase, false);
    let (mut acked, reached) = commit_until_reached(&db, "before", phase, 600);
    assert!(
      reached,
      "{phase:?}: no spill reached it in 600 commits (about 4x the WAL)"
    );
    assert_crash_copy_holds(&path, &acked, &format!("{phase:?}"));
    // The next spill succeeds.
    let next_seq = wal_segment_test_stats(&db).next_seq;
    acked.extend(commit_keys(&db, "after", 0, 300));
    assert!(
      wal_segment_test_stats(&db).next_seq > next_seq,
      "{phase:?}: no spill after the failed one"
    );
    close_single_file(db).expect("close");
    let reopened = open_single_file(&path, options).expect("reopen");
    assert!(missing(&reopened, &acked).is_empty(), "{phase:?}: reopened");
  }
}

/// S2, a checkpoint's steps, on its thread: a checkpoint that fails at its
/// cut, once its snapshot is durable, once its install is durable in one
/// header slot, or once both slots name it but before it frees the segments
/// it covers, runs on the checkpoint thread (never on a committer), leaves a
/// database that a crash right then reopens with every acknowledged commit,
/// and the next checkpoint succeeds.
#[test]
#[ignore = "needs WAL segments and the checkpoint thread (lands later on this branch)"]
fn checkpoint_thread_fails_safely_at_each_of_its_steps() {
  for phase in [
    CheckpointPhase::CutReleased,
    CheckpointPhase::SnapshotDurable,
    CheckpointPhase::HeaderDurable,
    CheckpointPhase::SegmentsReleased,
  ] {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("checkpoint-thread-fault.kitedb");
    let db = open_single_file(&path, options().sync_mode(SyncMode::Normal)).expect("open");
    watch_checkpoint_phases(&db);
    set_checkpoint_test_db_fault(&db, phase, false);
    // About 3 MiB of WAL, 12 times the checkpoint trigger.
    let (mut acked, reached) = commit_until_reached(&db, "before", phase, 10_000);
    let threads: Vec<Option<String>> = checkpoint_test_reached(&db)
      .into_iter()
      .filter(|(reached, _, _)| *reached == phase)
      .map(|(_, thread, _)| thread)
      .collect();
    assert!(reached, "{phase:?}: never reached");
    assert!(
      threads
        .iter()
        .all(|thread| thread.as_deref() == Some(CHECKPOINT_THREAD_NAME)),
      "{phase:?}: reached on {threads:?}, not only on the checkpoint thread"
    );
    // The failed run may still be ending; then it is as a crash leaves it.
    let deadline = Instant::now() + Duration::from_secs(10);
    wait_for("the failed run to end", deadline, || {
      !db.is_checkpoint_running()
    });
    assert_crash_copy_holds(&path, &acked, &format!("{phase:?}"));
    let covered = wal_segment_test_stats(&db).covered;
    db.background_checkpoint().expect("the next checkpoint");
    assert!(
      wal_segment_test_stats(&db).covered > covered,
      "{phase:?}: the next checkpoint covered no segment"
    );
    acked.extend(commit_keys(&db, "after", 0, 50));
    close_single_file(db).expect("close");
    let reopened = open_single_file(&path, options()).expect("reopen");
    assert!(missing(&reopened, &acked).is_empty(), "{phase:?}: reopened");
  }
}

/// S3. Checkpoints free the segments they cover, and later spills reuse
/// their pages: over a long run of commits the file stays bounded by the
/// snapshot, the segment limit and the WAL, not by what was ever written.
#[test]
#[ignore = "needs WAL segments and the checkpoint thread (lands later on this branch)"]
fn segments_are_reclaimed_after_a_checkpoint() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("segments-reclaimed.kitedb");
  let db = open_single_file(&path, options().sync_mode(SyncMode::Normal)).expect("open");
  // About 3 MiB of WAL: 64 times the WAL's primary region.
  let acked = commit_keys(&db, "key", 0, 10_000);
  let deadline = Instant::now() + Duration::from_secs(20);
  wait_for("the checkpoint thread to go idle", deadline, || {
    !db.is_checkpoint_running()
  });
  let stats = wal_segment_test_stats(&db);
  let (snapshot_bytes, page_size) = {
    let header = db.header.read();
    (
      header.snapshot_page_count * header.page_size as u64,
      header.page_size as u64,
    )
  };
  let file_bytes = db.pager.lock().file_size();
  assert!(
    stats.next_seq >= 10,
    "only {} segments were ever written: the WAL did not spill",
    stats.next_seq
  );
  assert!(
    stats.covered > 0 && stats.live < stats.next_seq as usize,
    "no checkpoint covered and released segments: {stats:?}"
  );
  // Two snapshots (one being built), the segments allowed, the WAL, and an
  // extent's slack.
  let bound = 2 * snapshot_bytes + 16 * SMALL_WAL as u64 + 64 * 1024 * 1024;
  assert!(
    file_bytes <= bound + 64 * page_size,
    "the file holds {file_bytes} bytes, more than {bound}: segments were not reused"
  );
  close_single_file(db).expect("close");
  let reopened = open_single_file(&path, options()).expect("reopen");
  assert!(missing(&reopened, &acked).is_empty());
}

/// S4. Writers wait for a checkpoint only once the segments reach their
/// limit: while the checkpoint thread is held after writing its snapshot,
/// a writer keeps spilling into new segments until the limit, then waits,
/// and goes on once the install frees space.
#[test]
#[ignore = "needs WAL segments and the checkpoint thread (lands later on this branch)"]
fn writers_wait_only_at_the_segment_limit() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("segment-backpressure.kitedb");
  let db = Arc::new(open_single_file(&path, options().sync_mode(SyncMode::Normal)).expect("open"));
  set_wal_segment_test_limit(&db, 512 * 1024);
  watch_checkpoint_phases(&db);
  let held = Arc::new(Barrier::new(2));
  set_checkpoint_test_barrier(&db, CheckpointPhase::SnapshotDurable, Arc::clone(&held));
  let writer = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || commit_keys(&db, "key", 0, 6_000))
  };
  let deadline = Instant::now() + Duration::from_secs(20);
  let parked = || {
    checkpoint_test_reached(&db)
      .into_iter()
      .find(|(phase, _, parked)| *phase == CheckpointPhase::SnapshotDurable && *parked)
      .map(|(_, thread, _)| thread)
  };
  let held_thread = if wait_for("a held checkpoint", deadline, || parked().is_some()) {
    parked().flatten()
  } else {
    disarm_checkpoint_test_barrier(&db, CheckpointPhase::SnapshotDurable);
    None
  };
  let waited = held_thread.as_deref() == Some(CHECKPOINT_THREAD_NAME)
    && wait_for("a writer to wait for segment space", deadline, || {
      writers_waiting_for_segments(&db) > 0
    });
  if parked().is_some() {
    held.wait();
  }
  let acked = writer.join().expect("writer");
  assert_eq!(
    held_thread.as_deref(),
    Some(CHECKPOINT_THREAD_NAME),
    "the checkpoint was held on {held_thread:?}, not on the checkpoint thread"
  );
  assert!(
    waited,
    "no writer waited for segment space while the checkpoint was held"
  );
  assert_eq!(acked.len(), 6_000, "writes failed instead of waiting");
  assert!(missing(&db, &acked).is_empty());
}

/// S5. An error on the checkpoint thread is reported by
/// `checkpoint_thread_error` (`SingleFileDB::checkpoint_error`), is returned
/// to a commit that would otherwise wait for checkpoint space (instead of
/// waiting forever), and is cleared by the next checkpoint that succeeds.
#[test]
#[ignore = "needs WAL segments and the checkpoint thread (lands later on this branch)"]
fn checkpoint_thread_error_surfaces() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("checkpoint-thread-error.kitedb");
  let db = open_single_file(&path, options().sync_mode(SyncMode::Normal)).expect("open");
  set_wal_segment_test_limit(&db, 512 * 1024);
  set_checkpoint_test_db_fault(&db, CheckpointPhase::SnapshotWritten, true);
  let mut acked = Vec::new();
  let mut failure = None;
  for index in 0..10_000 {
    let key = key("key", index);
    match commit_key(&db, &key) {
      Ok(()) => acked.push(key),
      Err(error) => {
        failure = Some(error);
        break;
      }
    }
  }
  let reported = checkpoint_thread_error(&db);
  clear_checkpoint_test_db_faults(&db);
  assert!(
    reported
      .as_deref()
      .is_some_and(|error| error.contains("injected checkpoint abort")),
    "the checkpoint thread's error was not reported: {reported:?}"
  );
  let failure = failure.expect("every commit succeeded past the segment limit");
  assert!(
    failure.to_string().to_lowercase().contains("checkpoint"),
    "the commit at the limit failed with {failure}, not the checkpoint's error"
  );
  db.background_checkpoint()
    .expect("a checkpoint without the fault");
  assert_eq!(
    checkpoint_thread_error(&db),
    None,
    "the error was not cleared"
  );
  acked.extend(commit_keys(&db, "after", 0, 100));
  close_single_file(db).expect("close");
  let reopened = open_single_file(&path, options()).expect("reopen");
  assert!(missing(&reopened, &acked).is_empty());
}

/// The snapshot generation the newest header slot of the file at `path`
/// names.
fn snapshot_generation_on_disk(path: &std::path::Path) -> u64 {
  let bytes = std::fs::read(path).expect("read the file");
  let page_size = 4096;
  (0..2)
    .filter_map(|slot| DbHeaderV1::parse(&bytes[slot * page_size..(slot + 1) * page_size]).ok())
    .max_by_key(|header| header.change_counter)
    .expect("a valid header slot")
    .active_snapshot_gen
}

/// S6. A writable database runs one checkpoint thread (a read-only one
/// none). Closing it, or dropping it, while that thread builds a snapshot
/// abandons the build (no header names its pages), joins the thread, and
/// loses no commit.
#[test]
#[ignore = "needs WAL segments and the checkpoint thread (lands later on this branch)"]
fn close_and_drop_abandon_an_inflight_checkpoint() {
  for close in [true, false] {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("abandon-inflight.kitedb");
    let db = open_single_file(&path, options().sync_mode(SyncMode::Normal)).expect("open");
    assert!(
      checkpoint_thread_running(&db),
      "a writable database runs no checkpoint thread"
    );
    watch_checkpoint_phases(&db);
    let generation = db.header.read().active_snapshot_gen;
    let held = Arc::new(Barrier::new(2));
    set_checkpoint_test_barrier(&db, CheckpointPhase::SnapshotDurable, Arc::clone(&held));
    let db = Arc::new(db);
    let writer = {
      let db = Arc::clone(&db);
      std::thread::spawn(move || commit_keys(&db, "key", 0, 1_500))
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    let parked = wait_for("a held checkpoint", deadline, || {
      checkpoint_test_reached(&db)
        .iter()
        .any(|(phase, _, parked)| *phase == CheckpointPhase::SnapshotDurable && *parked)
    });
    if !parked {
      disarm_checkpoint_test_barrier(&db, CheckpointPhase::SnapshotDurable);
    }
    let acked = writer.join().expect("writer");
    let db = Arc::into_inner(db).expect("sole owner");
    let (closed, closing) = mpsc::channel();
    let closer = std::thread::spawn(move || {
      let result = if close {
        close_single_file(db)
      } else {
        drop(db);
        Ok(())
      };
      let _ = closed.send(());
      result
    });
    if parked {
      held.wait();
    }
    let finished = closing.recv_timeout(Duration::from_secs(5));
    closer.join().expect("closer").expect("close");
    assert!(parked, "close={close}: no checkpoint was held");
    assert!(finished.is_ok(), "close={close}: did not finish");
    assert_eq!(
      snapshot_generation_on_disk(&path),
      generation,
      "close={close}: the in-flight checkpoint installed instead of being abandoned"
    );
    let reopened = open_single_file(&path, options()).expect("reopen");
    assert!(missing(&reopened, &acked).is_empty(), "close={close}");
    drop(reopened);
    let read_only = open_single_file(&path, SingleFileOpenOptions::new().read_only(true))
      .expect("read-only open");
    assert!(
      !checkpoint_thread_running(&read_only),
      "a read-only database runs a checkpoint thread"
    );
  }
}

/// S7. A read transaction keeps reading what it began with while the
/// checkpoint thread installs a snapshot that holds later commits, and sees
/// them once it ends.
#[test]
fn reader_keeps_its_snapshot_across_a_background_install() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("reader-across-install.kitedb");
  let db = Arc::new(open_single_file(&path, options().sync_mode(SyncMode::Normal)).expect("open"));
  db.begin(false).expect("begin");
  let node = db.create_node(Some("watched")).expect("node");
  let prop = db.define_propkey("value").expect("propkey");
  db.set_node_prop(node, prop, PropValue::I64(1))
    .expect("prop");
  db.commit().expect("commit");

  let (began, reader_began) = mpsc::channel();
  let (go, reader_go) = mpsc::channel::<()>();
  let reader = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || {
      db.begin(true).expect("read transaction");
      let before = db.node_prop(node, prop);
      began.send(()).expect("signal");
      reader_go.recv().expect("wait");
      let during = db.node_prop(node, prop);
      db.commit().expect("end read transaction");
      (before, during, db.node_prop(node, prop))
    })
  };
  reader_began.recv().expect("reader began");
  db.begin(false).expect("begin");
  db.set_node_prop(node, prop, PropValue::I64(2))
    .expect("prop");
  db.commit().expect("commit");
  let generation = db.header.read().active_snapshot_gen;
  let acked = commit_keys(&db, "key", 0, 3_000);
  let deadline = Instant::now() + Duration::from_secs(20);
  let installed = wait_for("a background install", deadline, || {
    db.header.read().active_snapshot_gen > generation
  });
  go.send(()).expect("release reader");
  let (before, during, after) = reader.join().expect("reader");
  assert!(installed, "no checkpoint installed a snapshot");
  assert_eq!(before, Some(PropValue::I64(1)));
  assert_eq!(
    during,
    Some(PropValue::I64(1)),
    "the reader saw a commit made after it began"
  );
  assert_eq!(after, Some(PropValue::I64(2)));
  assert!(missing(&db, &acked).is_empty());
}

/// The newest header slot of the file at `path`, and the number of WAL
/// segments its page names (v3 field at byte 184).
fn newest_header(path: &std::path::Path) -> (DbHeaderV1, u32) {
  let bytes = std::fs::read(path).expect("read the file");
  let page_size = 4096;
  (0..2)
    .filter_map(|slot| {
      let page = &bytes[slot * page_size..(slot + 1) * page_size];
      DbHeaderV1::parse(page).ok().map(|header| {
        (
          header,
          u32::from_le_bytes(page[184..188].try_into().unwrap()),
        )
      })
    })
    .max_by_key(|(header, _)| header.change_counter)
    .expect("a valid header slot")
}

/// S8. A read-only open replays the records in WAL segments in place (it
/// writes nothing): a replication source opened while segments hold
/// commits sees them all.
#[test]
fn read_only_open_replays_segments() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("read-only-segments.kitedb");
  let db = open_single_file(
    &path,
    options().sync_mode(SyncMode::Normal).auto_checkpoint(false),
  )
  .expect("open");
  // About 120 KiB: two or three spills.
  let acked = commit_keys(&db, "key", 0, 400);
  close_single_file(db).expect("close");
  let (_, segments) = newest_header(&path);
  assert!(segments > 0, "the closed file names no WAL segment");
  let read_only =
    open_single_file(&path, SingleFileOpenOptions::new().read_only(true)).expect("read-only open");
  assert!(missing(&read_only, &acked).is_empty());
}

/// S9. A commit whose records do not fit in an empty WAL (a bulk load of a
/// few thousand rows with the default 4 MiB WAL) spills into a segment
/// instead of failing with `WalBufferFull`.
#[test]
fn oversized_bulk_commit_spills_into_a_segment() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("oversized-commit.kitedb");
  let db = open_single_file(&path, options()).expect("open");
  let keys: Vec<String> = (0..2_000).map(|index| key("bulk", index)).collect();
  let key_refs: Vec<Option<&str>> = keys.iter().map(|key| Some(key.as_str())).collect();
  db.begin_bulk().expect("begin bulk");
  db.create_nodes_batch(&key_refs).expect("create nodes");
  db.commit()
    .expect("a commit larger than the WAL (about 500 KiB of records)");
  close_single_file(db).expect("close");
  let reopened = open_single_file(&path, options()).expect("reopen");
  assert!(missing(&reopened, &keys).is_empty());
}

/// Write the v2 fixtures `tests/b4_checkpoint_segments_compat.rs` opens:
/// files as today's writer (format version 2, before WAL segments) leaves
/// them, including the background checkpoint states only it writes. Run
/// with `cargo test --no-default-features --lib generate_v2_fixtures --
/// --ignored` on a build that still has the dual-region write path; the
/// files are committed under `tests/fixtures/v2_*.kitedb`.
#[test]
#[ignore = "writes the committed v2 fixtures; needs the v2 write path"]
fn generate_v2_fixtures() {
  let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
  let dir = tempdir().expect("tempdir");
  let options = || options().auto_checkpoint(false);
  let pad = |prefix: &str, index: usize| format!("{prefix}-{index}-{}", "p".repeat(100));
  let commit = |db: &SingleFileDB, prefix: &str, count: usize| {
    for index in 0..count {
      commit_key(db, &pad(prefix, index)).expect("commit");
    }
  };
  let copy_as = |path: &std::path::Path, name: &str| {
    std::fs::copy(path, fixtures.join(name)).expect("write fixture");
  };

  // v2_wal_records: a snapshot with nodes, edges and properties; committed
  // records in the WAL; and an open transaction past 16 KiB of records, which
  // a later commit's group writes to the WAL ahead of its own, so the header
  // names them without a COMMIT (the crash drops it).
  {
    let path = dir.path().join("wal-records.kitedb");
    let db = Arc::new(open_single_file(&path, options()).expect("open"));
    db.begin(false).expect("begin");
    let knows = db.define_etype("KNOWS").expect("etype");
    let age = db.define_propkey("age").expect("propkey");
    let mut previous = None;
    for index in 0..50 {
      let node = db.create_node(Some(&pad("snap", index))).expect("node");
      db.set_node_prop(node, age, PropValue::I64(index as i64))
        .expect("prop");
      if let Some(previous) = previous {
        db.add_edge(previous, knows, node).expect("edge");
      }
      previous = Some(node);
    }
    db.commit().expect("commit");
    db.checkpoint().expect("checkpoint");
    commit(&db, "wal", 30);
    db.begin(false).expect("begin open transaction");
    for index in 0..20 {
      db.create_node(Some(&format!("open-{index}-{}", "o".repeat(1000))))
        .expect("node");
    }
    std::thread::scope(|scope| {
      scope.spawn(|| commit(&db, "late", 1));
    });
    copy_as(&path, "v2_wal_records.kitedb");
    db.rollback().expect("rollback");
  }

  // v2_cut_in_progress: a background checkpoint held after writing its
  // snapshot, with commits after its cut in the secondary region.
  // v2_primary_retired: the same checkpoint's install done, the post-cut
  // records still in the secondary region (not compacted into primary).
  {
    let path = dir.path().join("cut.kitedb");
    let db = Arc::new(open_single_file(&path, options()).expect("open"));
    watch_checkpoint_phases(&db);
    commit(&db, "pre", 20);
    let written = Arc::new(Barrier::new(2));
    set_checkpoint_test_barrier(&db, CheckpointPhase::SnapshotDurable, Arc::clone(&written));
    let retained = Arc::new(Barrier::new(2));
    set_checkpoint_test_barrier(
      &db,
      CheckpointPhase::PostCutWalRetained,
      Arc::clone(&retained),
    );
    let checkpoint = {
      let db = Arc::clone(&db);
      std::thread::spawn(move || db.background_checkpoint())
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    assert!(wait_for("the cut", deadline, || {
      db.header.read().checkpoint_in_progress != 0
    }));
    commit(&db, "post", 20);
    copy_as(&path, "v2_cut_in_progress.kitedb");
    written.wait();
    // Held at the barrier with the WAL locked: watch for it instead.
    assert!(wait_for("the install", deadline, || {
      checkpoint_test_reached(&db)
        .iter()
        .any(|(phase, _, parked)| *phase == CheckpointPhase::PostCutWalRetained && *parked)
    }));
    copy_as(&path, "v2_primary_retired.kitedb");
    retained.wait();
    checkpoint
      .join()
      .expect("checkpoint thread")
      .expect("checkpoint");
  }

  // v2_cut_too_big: a cut whose post-cut records do not fit after the
  // primary region's, so open cannot merge them back.
  {
    let path = dir.path().join("cut-too-big.kitedb");
    let db = Arc::new(open_single_file(&path, options()).expect("open"));
    let mut fills = 0;
    while db.wal_stats().primary_head < 44 * 1024 {
      commit_key(&db, &format!("fill-{fills}-{}", "f".repeat(1000))).expect("commit");
      fills += 1;
    }
    let written = Arc::new(Barrier::new(2));
    set_checkpoint_test_barrier(&db, CheckpointPhase::SnapshotDurable, Arc::clone(&written));
    let checkpoint = {
      let db = Arc::clone(&db);
      std::thread::spawn(move || db.background_checkpoint())
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    assert!(wait_for("the cut", deadline, || {
      db.header.read().checkpoint_in_progress != 0
    }));
    for index in 0..6 {
      commit_key(&db, &format!("post-{index}-{}", "p".repeat(1000))).expect("commit");
    }
    copy_as(&path, "v2_cut_too_big.kitedb");
    eprintln!("v2_cut_too_big: {fills} fill keys");
    written.wait();
    checkpoint
      .join()
      .expect("checkpoint thread")
      .expect("checkpoint");
  }
}

/// Options for a database whose WAL only spills (no automatic checkpoint).
fn spilling_options() -> SingleFileOpenOptions {
  options().sync_mode(SyncMode::Normal).auto_checkpoint(false)
}

/// A background checkpoint covers every WAL segment: it drops them from
/// the header (`covered` reaches every seq written), and a reopen finds the
/// commits in the snapshot.
#[test]
fn background_checkpoint_covers_wal_segments() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("checkpoint-covers-segments.kitedb");
  let db = open_single_file(&path, spilling_options()).expect("open");
  let acked = commit_keys(&db, "key", 0, 400);
  let before = wal_segment_test_stats(&db);
  assert!(before.live > 0, "no spill: {before:?}");
  db.background_checkpoint().expect("background checkpoint");
  let after = wal_segment_test_stats(&db);
  assert_eq!(
    after.live, 0,
    "segments left after the checkpoint: {after:?}"
  );
  assert!(after.covered + 1 >= after.next_seq, "{after:?}");
  let more = commit_keys(&db, "more", 0, 50);
  close_single_file(db).expect("close");
  let reopened = open_single_file(&path, spilling_options()).expect("reopen");
  assert!(missing(&reopened, &acked).is_empty());
  assert!(missing(&reopened, &more).is_empty());
}

/// A write transaction whose records the WAL spilled into a segment while it
/// was open (it wrote more than it keeps back) commits exactly once, whether
/// a checkpoint runs while it is open or after, live and after a reopen.
#[test]
fn transaction_open_across_a_spill_and_a_checkpoint_commits_once() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("open-across-spill.kitedb");
  let db = Arc::new(open_single_file(&path, spilling_options()).expect("open"));
  let open_keys: Vec<String> = (0..40)
    .map(|index| format!("open-{index}-{}", "o".repeat(1000)))
    .collect();
  let (wrote, open_wrote) = mpsc::channel();
  let (go, open_go) = mpsc::channel::<()>();
  let writer = {
    let db = Arc::clone(&db);
    let keys = open_keys.clone();
    std::thread::spawn(move || {
      db.begin(false).expect("begin");
      for key in &keys {
        db.create_node(Some(key)).expect("node");
      }
      wrote.send(()).expect("signal");
      open_go.recv().expect("wait");
      db.commit()
        .expect("commit after the spill and the checkpoint");
    })
  };
  open_wrote.recv().expect("the open transaction wrote");
  let spills = wal_segment_test_stats(&db).next_seq;
  let mut acked = Vec::new();
  let mut index = 0;
  while wal_segment_test_stats(&db).next_seq <= spills.max(1) && index < 2_000 {
    acked.extend(commit_keys(&db, "key", index, 1));
    index += 1;
  }
  assert!(
    wal_segment_test_stats(&db).next_seq > spills.max(1),
    "the WAL never spilled"
  );
  // Either is fine: it covers the open transaction's records or declines.
  match db.background_checkpoint() {
    Ok(()) | Err(KiteError::CheckpointDeclined(_)) => {}
    Err(error) => panic!("checkpoint failed: {error}"),
  }
  go.send(()).expect("release the open transaction");
  writer.join().expect("writer");
  assert!(missing(&db, &open_keys).is_empty(), "lost live");
  db.background_checkpoint()
    .expect("checkpoint after it committed");
  let count = db.count_nodes();
  let db = Arc::into_inner(db).expect("sole owner");
  close_single_file(db).expect("close");
  let reopened = open_single_file(&path, spilling_options()).expect("reopen");
  assert!(
    missing(&reopened, &open_keys).is_empty(),
    "lost after reopen"
  );
  assert!(missing(&reopened, &acked).is_empty());
  assert_eq!(
    reopened.count_nodes(),
    count,
    "a transaction replayed twice"
  );
}

/// Vacuum compacts the file after the snapshot, which would cut off WAL
/// segments: it checkpoints them first. A WAL resize without its checkpoint
/// refuses a log that has segments.
#[test]
fn vacuum_and_resize_handle_wal_segments() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("vacuum-segments.kitedb");
  let db = open_single_file(&path, spilling_options()).expect("open");
  let acked = commit_keys(&db, "key", 0, 400);
  assert!(wal_segment_test_stats(&db).live > 0, "no spill");
  assert!(db
    .resize_wal(
      2 * SMALL_WAL,
      Some(crate::core::single_file::ResizeWalOptions {
        allow_shrink: false,
        checkpoint: false,
      }),
    )
    .is_err());
  db.vacuum_single_file(Some(crate::core::single_file::VacuumOptions {
    shrink_wal: false,
    min_wal_size: None,
  }))
  .expect("vacuum");
  assert_eq!(wal_segment_test_stats(&db).live, 0);
  let header = db.header.read().clone();
  assert_eq!(
    db.pager.lock().file_size(),
    (header.snapshot_start_page + header.snapshot_page_count) * header.page_size as u64
  );
  close_single_file(db).expect("close");
  let reopened = open_single_file(&path, spilling_options()).expect("reopen");
  assert!(missing(&reopened, &acked).is_empty());
}

/// A crash while a background checkpoint builds its snapshot, with commits
/// in WAL segments before its cut and in the WAL after it, keeps them all.
#[test]
fn crash_during_a_checkpoint_with_segments_keeps_every_commit() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("crash-checkpoint-segments.kitedb");
  let db = Arc::new(open_single_file(&path, spilling_options()).expect("open"));
  let mut acked = commit_keys(&db, "before", 0, 400);
  assert!(wal_segment_test_stats(&db).live > 0, "no spill");
  watch_checkpoint_phases(&db);
  let held = Arc::new(Barrier::new(2));
  set_checkpoint_test_barrier(&db, CheckpointPhase::SnapshotDurable, Arc::clone(&held));
  let checkpoint = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || db.background_checkpoint())
  };
  let deadline = Instant::now() + Duration::from_secs(10);
  assert!(wait_for("a held checkpoint", deadline, || {
    checkpoint_test_reached(&db)
      .iter()
      .any(|(phase, _, parked)| *phase == CheckpointPhase::SnapshotDurable && *parked)
  }));
  acked.extend(commit_keys(&db, "during", 0, 30));
  let copy = dir.path().join("crash-checkpoint-segments-copy.kitedb");
  std::fs::copy(&path, &copy).expect("crash copy");
  held.wait();
  checkpoint
    .join()
    .expect("checkpoint thread")
    .expect("checkpoint");
  let crashed = open_single_file(&copy, spilling_options()).expect("open the crash copy");
  assert!(missing(&crashed, &acked).is_empty());
  let more = commit_keys(&crashed, "after", 0, 300);
  crashed
    .background_checkpoint()
    .expect("checkpoint after the crash");
  close_single_file(crashed).expect("close");
  let reopened = open_single_file(&copy, spilling_options()).expect("reopen");
  assert!(missing(&reopened, &acked).is_empty());
  assert!(missing(&reopened, &more).is_empty());
}
