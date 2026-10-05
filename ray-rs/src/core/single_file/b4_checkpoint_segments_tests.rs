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

/// How a crash image models the disk after a crash (see `crash_image`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CrashModel {
  /// Every write so far landed, in order: a process crash (the OS writes
  /// back everything it took), or an OS crash that wrote back everything.
  InOrder,
  /// The writes before the last successful sync landed, and after it only
  /// the header pages': an OS crash that wrote back the headers early.
  HeadersAhead,
  /// The writes before the last successful sync landed, and after it all
  /// but the header pages': an OS crash that wrote back the data early.
  DataAhead,
  /// Every write landed, but the last, a header write, only in part: its
  /// slot is torn (its checksum fails).
  TornHeader,
}

impl CrashModel {
  pub(super) const ALL: [CrashModel; 4] = [
    CrashModel::InOrder,
    CrashModel::HeadersAhead,
    CrashModel::DataAhead,
    CrashModel::TornHeader,
  ];

  /// Whether a database in `sync_mode` holds its acknowledged commits
  /// through such a crash: every mode that writes commits through a process
  /// crash (in order), only `Full` through the OS crashes.
  pub(super) fn fits(self, sync_mode: SyncMode) -> bool {
    match self {
      CrashModel::InOrder => sync_mode != SyncMode::Off,
      _ => sync_mode == SyncMode::Full,
    }
  }
}

/// The disk after a crash once the first `cut` of `events` (recorded from
/// `base`) happened, as `model` has it; the header pages are the bytes
/// below `header_end`. `None` for a torn header slot unless the last of
/// them is a header write.
pub(super) fn crash_image(
  base: &[u8],
  events: &[IoEvent],
  header_end: u64,
  cut: usize,
  model: CrashModel,
) -> Option<Vec<u8>> {
  let prefix = &events[..cut];
  let last_sync = prefix
    .iter()
    .rposition(|event| matches!(event, IoEvent::Sync { ok: true }));
  let torn = match (model, prefix.last()) {
    (CrashModel::TornHeader, Some(IoEvent::Write { offset, .. })) if *offset < header_end => {
      Some(cut - 1)
    }
    (CrashModel::TornHeader, _) => return None,
    _ => None,
  };
  let mut image = base.to_vec();
  for (index, event) in prefix.iter().enumerate() {
    let IoEvent::Write { offset, data } = event else {
      continue;
    };
    let durable = last_sync.is_some_and(|sync| index < sync);
    let header = *offset < header_end;
    let lands = match model {
      CrashModel::InOrder | CrashModel::TornHeader => true,
      CrashModel::HeadersAhead => durable || header,
      CrashModel::DataAhead => durable || !header,
    };
    if !lands {
      continue;
    }
    let data = if Some(index) == torn {
      &data[..data.len() / 2]
    } else {
      &data[..]
    };
    apply(&mut image, *offset, data);
  }
  Some(image)
}

/// Every crash image of `events` (see `crash_image`) that `sync_mode`
/// holds acknowledged commits through: each point, each model that fits.
/// Each with the number of events before it, and a name.
pub(super) fn crash_images(
  base: &[u8],
  events: &[IoEvent],
  header_end: u64,
  sync_mode: SyncMode,
) -> Vec<(usize, String, Vec<u8>)> {
  let mut images = Vec::new();
  for cut in 0..=events.len() {
    for model in CrashModel::ALL {
      if !model.fits(sync_mode) {
        continue;
      }
      if let Some(image) = crash_image(base, events, header_end, cut, model) {
        images.push((cut, format!("crash after {cut} events, {model:?}"), image));
      }
    }
  }
  images
}

/// S1. A spill copies the WAL's records into a segment, syncs it, and
/// installs a header naming it with an empty WAL in both slots. A crash at
/// any write or sync of the commits around it keeps every acknowledged
/// commit: in `Full` mode whatever an OS crash leaves (in order, the header
/// pages ahead of the rest or behind it, a torn header slot), in `Normal`
/// mode what a process crash leaves (in order).
#[test]
fn spill_survives_a_crash_at_every_io_event() {
  for sync_mode in [SyncMode::Full, SyncMode::Normal] {
    spill_survives_a_crash_at_every_io_event_in(sync_mode);
  }
}

fn spill_survives_a_crash_at_every_io_event_in(sync_mode: SyncMode) {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("spill-crash.kitedb");
  // No checkpoint: only spills make room.
  let options = options().sync_mode(sync_mode).auto_checkpoint(false);
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
    "{sync_mode:?}: the WAL never spilled into a segment: the commits that filled it ran \
     checkpoints instead"
  );

  let images = crash_images(&base, &events, header_end, sync_mode);
  for model in CrashModel::ALL
    .into_iter()
    .filter(|model| model.fits(sync_mode))
  {
    assert!(
      images
        .iter()
        .any(|(_, what, _)| what.ends_with(&format!("{model:?}"))),
      "{sync_mode:?}: no {model:?} image"
    );
  }
  let image_path = dir.path().join("spill-crash-image.kitedb");
  for (cut, what, image) in images {
    std::fs::write(&image_path, &image).expect("write image");
    let crashed = open_single_file(&image_path, options.clone())
      .unwrap_or_else(|error| panic!("{sync_mode:?}, {what}: unopenable: {error:?}"));
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
    assert_eq!(
      lost, 0,
      "{sync_mode:?}, {what}: lost {lost} acknowledged commits"
    );
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

/// S2, a checkpoint's steps, on its thread: its cut released, its snapshot
/// durable, its install durable in one header slot, and in both but before
/// it frees the segments it covers.
///
/// - A crash at the step (a copy of the file while the run is held there,
///   nothing else writing) reopens with every acknowledged commit, on the
///   new snapshot (and its segment table) once a header slot names it.
/// - A run that fails there runs on the checkpoint thread (never on a
///   committer) and leaves a database that goes on: a crash at any write of
///   the next two spills (crash images), and after each of several WALs of
///   writes (crash copies), keeps every commit, since nothing a spill
///   allocates is a page a header slot still names; and the next checkpoint
///   succeeds. At `SegmentsReleased` the
///   fault stands for a crash before the segments are freed (the run
///   succeeds and frees nothing): a reopen reclaims them, and the clean close
///   after it leaves a compact file.
///
/// (Reworked with the fresh review: the crash copy after a fault usually
/// opened the old header slot, since commits had rewritten the failed one,
/// and under one WAL of writes followed each fault.)
#[test]
fn checkpoint_thread_fails_safely_at_each_of_its_steps() {
  for phase in [
    CheckpointPhase::CutReleased,
    CheckpointPhase::SnapshotDurable,
    CheckpointPhase::HeaderDurable,
    CheckpointPhase::SegmentsReleased,
  ] {
    crash_while_the_checkpoint_thread_is_held_at(phase);
    checkpoint_thread_run_failing_at(phase);
  }
}

fn crash_while_the_checkpoint_thread_is_held_at(phase: CheckpointPhase) {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("checkpoint-thread-held.kitedb");
  let db = Arc::new(open_single_file(&path, options().sync_mode(SyncMode::Normal)).expect("open"));
  watch_checkpoint_phases(&db);
  let generation = db.header.read().active_snapshot_gen;
  let held = Arc::new(Barrier::new(2));
  set_checkpoint_test_barrier(&db, phase, Arc::clone(&held));
  let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
  let acked = Arc::new(std::sync::Mutex::new(Vec::new()));
  let writer = {
    let (db, stop, acked) = (Arc::clone(&db), Arc::clone(&stop), Arc::clone(&acked));
    std::thread::spawn(move || {
      for index in 0..20_000 {
        if stop.load(Ordering::Acquire) {
          break;
        }
        let key = key("before", index);
        if commit_key(&db, &key).is_ok() {
          acked.lock().expect("acked").push(key);
        }
      }
    })
  };
  let deadline = Instant::now() + Duration::from_secs(20);
  let parked = wait_for("a held checkpoint", deadline, || {
    checkpoint_test_reached(&db)
      .iter()
      .any(|(reached, thread, parked)| {
        *reached == phase && *parked && thread.as_deref() == Some(CHECKPOINT_THREAD_NAME)
      })
  });
  stop.store(true, Ordering::Release);
  let installing = matches!(
    phase,
    CheckpointPhase::HeaderDurable | CheckpointPhase::SegmentsReleased
  );
  let mut writer = Some(writer);
  if !installing {
    // The held run holds no lock here: let the writer finish its commit.
    writer.take().map(|writer| writer.join().expect("writer"));
  }
  // Nothing writes now: the run is held, and in its install it holds the
  // commit lock, so the writer cannot write.
  let crash_acked = acked.lock().expect("acked").clone();
  let copy = path.with_extension("crash.kitedb");
  std::fs::copy(&path, &copy).expect("copy the file");
  if parked {
    held.wait();
  } else {
    disarm_checkpoint_test_barrier(&db, phase);
  }
  if let Some(writer) = writer {
    writer.join().expect("writer");
  }
  assert!(
    parked,
    "{phase:?}: the checkpoint thread was never held there"
  );

  let crashed = open_single_file(&copy, options())
    .unwrap_or_else(|error| panic!("{phase:?}: the crash copy is unopenable: {error:?}"));
  assert!(
    missing(&crashed, &crash_acked).is_empty(),
    "{phase:?}: the crash copy lost commits"
  );
  if installing {
    // The install's header, with its segment table (none it dropped).
    assert!(
      crashed.header.read().active_snapshot_gen > generation,
      "{phase:?}: the crash copy opened the old header slot, not the install"
    );
  }
  close_single_file(crashed).expect("close the copy");

  let acked = acked.lock().expect("acked").clone();
  let db = Arc::into_inner(db).expect("sole owner");
  close_single_file(db).expect("close");
  let reopened = open_single_file(&path, options()).expect("reopen");
  assert!(missing(&reopened, &acked).is_empty(), "{phase:?}: reopened");
}

fn checkpoint_thread_run_failing_at(phase: CheckpointPhase) {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("checkpoint-thread-fault.kitedb");
  let db = open_single_file(&path, options().sync_mode(SyncMode::Normal)).expect("open");
  // A snapshot of some hundred pages first (keys that do not compress), so
  // the one the failed run writes is larger than an extent: were its pages
  // free while a header slot names them, the next extent would take them.
  let mut acked = Vec::new();
  let mut noise = 0x9e37_79b9_7f4a_7c15_u64;
  for index in 0..2_000 {
    let key: String = (0..25)
      .map(|_| {
        noise ^= noise << 13;
        noise ^= noise >> 7;
        noise ^= noise << 17;
        format!("{:08x}", noise as u32)
      })
      .collect();
    let key = format!("seed-{index}-{key}");
    commit_key(&db, &key).expect("seed commit");
    acked.push(key);
  }
  db.checkpoint().expect("seed checkpoint");
  watch_checkpoint_phases(&db);
  set_checkpoint_test_db_fault(&db, phase, false);
  let (before, reached) = commit_until_reached(&db, "before", phase, 10_000);
  acked.extend(before);
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

  // A crash at any write of the next two spills, with the commits around
  // them (each a crash image, every write landed in order): a header slot
  // may still name what the failed run wrote, so nothing they allocate may
  // be its pages. The checkpoint thread stops first, so every write is this
  // thread's (automatic checkpoints then run inline, recorded too).
  db.stop_checkpoint_thread();
  let base = std::fs::read(&path).expect("read the file");
  let acked_before = acked.clone();
  let spills_then = db.wal_spills.load(Ordering::Acquire);
  let (recorded, events) = io_hooks::record_io_during(|| {
    let mut recorded = Vec::new();
    let mut index = 0;
    while db.wal_spills.load(Ordering::Acquire) < spills_then + 2 && index < 3_000 {
      recorded.extend(commit_keys(&db, "recorded", index, 1));
      index += 1;
    }
    recorded
  });
  assert!(
    db.wal_spills.load(Ordering::Acquire) >= spills_then + 2,
    "{phase:?}: no two spills after the fault"
  );
  // The crashes that matter: every write before a header write landed, the
  // header write not (and the end).
  let header_end = 2 * 4096;
  let cuts: Vec<usize> = (0..events.len())
    .filter(|&cut| matches!(&events[cut], IoEvent::Write { offset, .. } if *offset < header_end))
    .chain([events.len()])
    .collect();
  let image_path = path.with_extension("image.kitedb");
  for &cut in &cuts {
    let what = format!("crash after {cut} events, in order");
    let image =
      crash_image(&base, &events, header_end, cut, CrashModel::InOrder).expect("an in-order image");
    std::fs::write(&image_path, &image).expect("write the image");
    let crashed = open_single_file(&image_path, options())
      .unwrap_or_else(|error| panic!("{phase:?}, {what}: unopenable: {error:?}"));
    let lost = missing(&crashed, &acked_before).len();
    close_single_file(crashed).expect("close the image");
    assert_eq!(lost, 0, "{phase:?}, {what}: lost {lost} commits");
  }
  let _ = std::fs::remove_file(&image_path);
  acked.extend(recorded);

  // Several WALs of writes, with a crash copy at each spill.
  let mut spills = db.wal_spills.load(Ordering::Acquire);
  let mut copies = 0;
  for index in 0..1_200 {
    acked.extend(commit_keys(&db, "after-fault", index, 1));
    let now = db.wal_spills.load(Ordering::Acquire);
    if now != spills && !db.is_checkpoint_running() {
      spills = now;
      copies += 1;
      assert_crash_copy_holds(&path, &acked, &format!("{phase:?}, after spill {now}"));
    }
  }
  assert!(
    copies >= 3,
    "{phase:?}: only {copies} spills after the fault were copied"
  );

  let covered = wal_segment_test_stats(&db).covered;
  acked.extend(commit_keys(&db, "between", 0, 1));
  db.background_checkpoint().expect("the next checkpoint");
  assert!(
    wal_segment_test_stats(&db).covered > covered,
    "{phase:?}: the next checkpoint covered no segment"
  );
  acked.extend(commit_keys(&db, "after", 0, 50));
  close_single_file(db).expect("close");
  let reopened = open_single_file(&path, options()).expect("reopen");
  assert!(missing(&reopened, &acked).is_empty(), "{phase:?}: reopened");
  if phase == CheckpointPhase::SegmentsReleased {
    // The segments the faulted run did not free are reclaimed: the clean
    // close leaves the header pages, the WAL and the snapshot.
    close_single_file(reopened).expect("close");
    let (header, segments) = newest_header(&path);
    let expected = (header.wal_start_page + header.wal_page_count + header.snapshot_page_count)
      * header.page_size as u64;
    assert_eq!(segments, 0, "{phase:?}: the closed file names segments");
    assert_eq!(
      std::fs::metadata(&path).expect("metadata").len(),
      expected,
      "{phase:?}: the closed file kept pages"
    );
  }
}

/// S3. Checkpoints free the segments they cover, and later spills reuse
/// their pages: over a long run of commits the file stays bounded by the
/// snapshot, the segment limit and the WAL, not by what was ever written.
#[test]
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
  // While it waits (the checkpoint is held, nothing frees), the segments it
  // waits on are at their limit: it did not wait before.
  let at_wait = wal_segment_test_stats(&db);
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
  assert!(
    at_wait.bytes >= 512 * 1024 || at_wait.live >= crate::constants::MAX_WAL_SEGMENTS - 1,
    "a writer waited with the segments below their limit: {at_wait:?}"
  );
  assert_eq!(acked.len(), 6_000, "writes failed instead of waiting");
  assert!(missing(&db, &acked).is_empty());
}

/// S5. An error on the checkpoint thread is reported by
/// `checkpoint_thread_error` (`SingleFileDB::checkpoint_error`), is returned
/// to a commit that would otherwise wait for checkpoint space (instead of
/// waiting forever), and is cleared by the next checkpoint that succeeds.
#[test]
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
  // The exact variant, carrying the checkpoint's error: `WalBufferFull`'s
  // message mentions a checkpoint too.
  assert!(
    matches!(&failure, KiteError::CheckpointFailed(error) if error.contains("injected checkpoint abort")),
    "the commit at the limit failed with {failure:?}, not with the checkpoint's error"
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

/// S6. A writable database runs one checkpoint thread, started by its first
/// automatic checkpoint (a read-only one none). Closing it, or dropping it,
/// while that thread builds a snapshot abandons the build (no header names
/// its pages), joins the thread, and loses no commit.
#[test]
fn close_and_drop_abandon_an_inflight_checkpoint() {
  for close in [true, false] {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("abandon-inflight.kitedb");
    let db = open_single_file(&path, options().sync_mode(SyncMode::Normal)).expect("open");
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
    assert!(
      checkpoint_thread_running(&db),
      "close={close}: a writable database ran no checkpoint thread"
    );
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
    // (Adjusted with decision Q3 of the fresh review, where a clean close
    // checkpoints the WAL segments itself, so a new generation on disk no
    // longer tells: the held run must not have reached its install.)
    assert!(
      !checkpoint_test_reached_at(&path)
        .iter()
        .any(|(phase, thread, _)| {
          *phase == CheckpointPhase::HeaderWritten
            && thread.as_deref() == Some(CHECKPOINT_THREAD_NAME)
        }),
      "close={close}: the in-flight checkpoint installed instead of being abandoned"
    );
    if !close {
      // Dropping persists the log and checkpoints nothing.
      assert_eq!(
        snapshot_generation_on_disk(&path),
        generation,
        "close={close}"
      );
    }
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

/// Decision Q2 of the fresh review: a checkpoint thread's run abandoned
/// because the database is closing is not a failure. It is not recorded as
/// the checkpoint error (nor warned about), and the table entry its cut
/// took is given back (the newest segment is not left sealed).
#[test]
fn a_run_abandoned_by_closing_is_not_a_failure() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("abandoned-run.kitedb");
  let db = open_single_file(&path, options().sync_mode(SyncMode::Normal)).expect("open");
  watch_checkpoint_phases(&db);
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
  assert!(parked, "setup: no checkpoint was held");
  // What closing does first: stop the thread, abandoning its run.
  let stopper = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || db.stop_checkpoint_thread())
  };
  while !db.checkpoint_abandoned.load(Ordering::Acquire) {
    std::thread::sleep(Duration::from_millis(1));
  }
  held.wait();
  stopper.join().expect("stopper");
  assert_eq!(
    db.checkpoint_error(),
    None,
    "a run abandoned by a close was recorded as the checkpoint error"
  );
  let newest = db.header.read().wal_segments.entries.last().copied();
  assert!(
    newest.is_some_and(|segment| !segment.sealed),
    "the abandoned run's cut left the newest segment sealed: {newest:?}"
  );
  let db = Arc::into_inner(db).expect("sole owner");
  close_single_file(db).expect("close");
  let reopened = open_single_file(&path, options()).expect("reopen");
  assert!(missing(&reopened, &acked).is_empty());
}

/// Decision Q3 of the fresh review: a clean close (no transaction open)
/// leaves no WAL segments. A checkpoint covers them, so the header names
/// none (format version 2: earlier releases can open the file), and the
/// compaction on close cuts off their pages: the file holds only the header
/// pages, the WAL and the snapshot. A small database keeps no segment
/// extent at rest.
#[test]
fn a_clean_close_leaves_no_wal_segments() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("clean-close.kitedb");
  // The default WAL (4 MiB), and default extents.
  let opts = SingleFileOpenOptions::new().sync_mode(SyncMode::Normal);
  let db = open_single_file(&path, opts.clone()).expect("open");
  let mut acked = Vec::new();
  let mut index = 0;
  while wal_segment_test_stats(&db).live == 0 && index < 100_000 {
    acked.extend(commit_keys(&db, "key", index, 1));
    index += 1;
  }
  assert!(
    wal_segment_test_stats(&db).live > 0,
    "setup: nothing spilled"
  );
  close_single_file(db).expect("close");

  let (header, segments) = newest_header(&path);
  assert_eq!(segments, 0, "the closed file names WAL segments");
  assert_eq!(
    header.written_versions().0,
    2,
    "the closed file is not format version 2"
  );
  let page_size = header.page_size as u64;
  let expected =
    (header.wal_start_page + header.wal_page_count + header.snapshot_page_count) * page_size;
  let size = std::fs::metadata(&path).expect("metadata").len();
  assert_eq!(
    size, expected,
    "the closed file holds {size} bytes, not just its header, WAL and snapshot ({expected})"
  );
  let reopened = open_single_file(&path, opts).expect("reopen");
  assert!(missing(&reopened, &acked).is_empty());
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
  // Dropped, not closed: a clean close checkpoints the segments away
  // (decision Q3 of the fresh review); dropping persists the log as it is,
  // as a process that ends without closing leaves it.
  drop(db);
  let (_, segments) = newest_header(&path);
  assert!(
    segments > 0,
    "the dropped database's file names no WAL segment"
  );
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
/// a checkpoint runs while it is open or after, live and after a reopen. A
/// later transaction deletes some of what it created and changes the rest:
/// replaying it a second time, or after that one, would bring the deleted
/// nodes back and the old values. (Made strict with the fresh review: it
/// accepted a declined checkpoint, and creates alone replay the same twice.)
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
  // It covers the segments, keeping those the open transaction holds.
  let covered = wal_segment_test_stats(&db).covered;
  db.background_checkpoint()
    .expect("a checkpoint while the transaction is open");
  assert!(
    wal_segment_test_stats(&db).covered > covered,
    "the checkpoint while the transaction is open covered nothing"
  );
  assert!(
    db.oldest_pinned_segment().is_some(),
    "the open transaction's records are not in a segment the install kept"
  );
  go.send(()).expect("release the open transaction");
  writer.join().expect("writer");
  assert!(missing(&db, &open_keys).is_empty(), "lost live");

  // A later transaction deletes the first ten and numbers the rest.
  db.begin(false).expect("begin");
  let value = db.define_propkey("value").expect("propkey");
  for key in &open_keys[..10] {
    let node = db.node_by_key(key).expect("node");
    db.delete_node(node).expect("delete");
  }
  for (index, key) in open_keys[10..].iter().enumerate() {
    let node = db.node_by_key(key).expect("node");
    db.set_node_prop(node, value, PropValue::I64(index as i64))
      .expect("prop");
  }
  db.commit().expect("commit the changes");
  let check = |db: &SingleFileDB, when: &str| {
    for key in &open_keys[..10] {
      assert!(
        db.node_by_key(key).is_none(),
        "{when}: a deleted node is back"
      );
    }
    for (index, key) in open_keys[10..].iter().enumerate() {
      let node = db
        .node_by_key(key)
        .unwrap_or_else(|| panic!("{when}: {key} lost"));
      assert_eq!(
        db.node_prop(node, value),
        Some(PropValue::I64(index as i64)),
        "{when}: an old value is back"
      );
    }
  };
  check(&db, "live");
  // A crash now: the reopen replays the transaction from the segment the
  // install kept, then the later one.
  let copy = path.with_extension("crash.kitedb");
  std::fs::copy(&path, &copy).expect("copy the file");
  let crashed = open_single_file(&copy, spilling_options()).expect("open the crash copy");
  check(&crashed, "a crash copy before the next checkpoint");
  drop(crashed);
  db.background_checkpoint()
    .expect("checkpoint after it committed");
  check(&db, "after the checkpoint");
  let count = db.count_nodes();
  // Dropped, so the reopen replays the log (a clean close would checkpoint
  // it away).
  let db = Arc::into_inner(db).expect("sole owner");
  drop(db);
  let reopened = open_single_file(&path, spilling_options()).expect("reopen");
  check(&reopened, "after a reopen");
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

/// Automatic checkpoints run on the database's checkpoint thread (started
/// at the first one; a read-only database runs none), which records the
/// error of a failed run for `checkpoint_error` until a checkpoint succeeds,
/// and stops when the database closes.
#[test]
fn checkpoint_thread_runs_records_errors_and_stops() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("checkpoint-thread-lifecycle.kitedb");
  let db = open_single_file(&path, options().sync_mode(SyncMode::Normal)).expect("open");
  assert!(
    !checkpoint_thread_running(&db),
    "started before any checkpoint"
  );
  set_checkpoint_test_db_fault(&db, CheckpointPhase::SnapshotWritten, true);
  let mut acked = Vec::new();
  for index in 0..2_000 {
    acked.extend(commit_keys(&db, "key", index, 1));
    db.wait_for_checkpoint_thread();
    if checkpoint_thread_error(&db).is_some() {
      break;
    }
  }
  clear_checkpoint_test_db_faults(&db);
  assert!(
    checkpoint_thread_running(&db),
    "no checkpoint thread started"
  );
  let reported = checkpoint_thread_error(&db);
  assert!(
    reported
      .as_deref()
      .is_some_and(|error| error.contains("injected checkpoint abort")),
    "the thread's error was not recorded: {reported:?}"
  );
  db.background_checkpoint()
    .expect("a checkpoint without the fault");
  assert_eq!(checkpoint_thread_error(&db), None, "the error stayed");
  close_single_file(db).expect("close");
  let reopened =
    open_single_file(&path, SingleFileOpenOptions::new().read_only(true)).expect("read-only open");
  assert!(missing(&reopened, &acked).is_empty());
  assert!(!checkpoint_thread_running(&reopened));
}

/// The checkpoint trigger is `checkpoint_log_ratio` of the snapshot's size,
/// at least half the WAL's usable region (three eighths of the WAL: what
/// earlier releases checkpointed at by default, so a small database holds
/// no more log in memory than they did), at most `checkpoint_log_budget`
/// (which also caps that floor); the WAL segment limit is twice the
/// trigger, at least 16 WALs, at most four budgets, unless
/// `wal_segment_limit` sets it. (Floor changed with decision Q4 of the fresh
/// review: it was four WALs, which made a 64 MiB WAL hold 128 MiB of log on
/// a tiny database, about 1.3 GB of delta, against 24 MiB before.)
#[test]
fn checkpoint_trigger_and_segment_limit_follow_the_log_options() {
  const MIB: u64 = 1024 * 1024;
  let wal = SMALL_WAL as u64;
  // Earlier releases checkpointed once the WAL's primary region (three
  // quarters of it) was half full (`checkpoint_threshold`, 0.5).
  let earlier_trigger = |wal: u64| wal * 3 / 4 / 2;
  let dir = tempdir().expect("tempdir");
  let with_snapshot = |db: &SingleFileDB, bytes: u64| {
    let mut header = db.header.read().clone();
    header.snapshot_page_count = bytes / header.page_size as u64;
    header
  };

  let db = open_single_file(dir.path().join("defaults.kitedb"), options()).expect("open");
  let empty = with_snapshot(&db, 0);
  assert_eq!(db.checkpoint_log_trigger(&empty), earlier_trigger(wal));
  assert_eq!(db.wal_segment_limit(&empty), 16 * wal);
  let medium = with_snapshot(&db, 100 * MIB);
  assert_eq!(db.checkpoint_log_trigger(&medium), 50 * MIB);
  assert_eq!(db.wal_segment_limit(&medium), 100 * MIB);
  let large = with_snapshot(&db, 1024 * MIB);
  assert_eq!(db.checkpoint_log_trigger(&large), 128 * MIB);
  assert_eq!(db.wal_segment_limit(&large), 256 * MIB);
  close_single_file(db).expect("close");

  // The WAL of `recommended_balanced` (64 MiB): a tiny database holds no
  // more log than earlier releases let it.
  let db = open_single_file(
    dir.path().join("balanced.kitedb"),
    SingleFileOpenOptions::new().wal_size(64 * MIB as usize),
  )
  .expect("open");
  assert_eq!(
    db.checkpoint_log_trigger(&with_snapshot(&db, 0)),
    earlier_trigger(64 * MIB)
  );
  close_single_file(db).expect("close");

  // The budget caps the floor too.
  let db = open_single_file(
    dir.path().join("small-budget.kitedb"),
    options().checkpoint_log_budget(16 * 1024),
  )
  .expect("open");
  assert_eq!(db.checkpoint_log_trigger(&with_snapshot(&db, 0)), 16 * 1024);
  close_single_file(db).expect("close");

  let tuned = options()
    .checkpoint_log_ratio(2.0)
    .checkpoint_log_budget(MIB / 8);
  let db = open_single_file(dir.path().join("tuned.kitedb"), tuned.clone()).expect("open");
  assert_eq!(
    db.checkpoint_log_trigger(&with_snapshot(&db, 0)),
    earlier_trigger(wal)
  );
  let medium = with_snapshot(&db, 100 * MIB);
  assert_eq!(db.checkpoint_log_trigger(&medium), MIB / 8);
  assert_eq!(db.wal_segment_limit(&medium), MIB / 2);
  close_single_file(db).expect("close");

  let db = open_single_file(
    dir.path().join("limited.kitedb"),
    tuned.wal_segment_limit(3 * MIB),
  )
  .expect("open");
  assert_eq!(
    db.wal_segment_limit(&with_snapshot(&db, 100 * MIB)),
    3 * MIB
  );
  close_single_file(db).expect("close");

  for refused in [
    options().checkpoint_log_ratio(f64::NAN),
    options().checkpoint_log_ratio(-1.0),
    options().checkpoint_log_budget(0),
    options().wal_segment_size(0),
    options().wal_segment_size(crate::constants::WAL_SEGMENT_MAX_SIZE + 1),
    options().wal_segment_limit(0),
  ] {
    assert!(open_single_file(dir.path().join("refused.kitedb"), refused).is_err());
  }
}

/// Review findings 6 and R9. A panic in a checkpoint thread's run is caught
/// and reported (`checkpoint_error`). It may have left memory and disk out
/// of step (here it strikes inside an install, between its header writes),
/// so the handle refuses writes from then on with a clear error, checkpoints
/// included, and closing it persists nothing (and says so); reads go on. A
/// reopen recovers every acknowledged commit from disk, and takes writes
/// and checkpoints again.
#[test]
fn a_panic_on_the_checkpoint_thread_is_reported_and_writes_are_refused() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("checkpoint-thread-panic.kitedb");
  let db = open_single_file(&path, options().sync_mode(SyncMode::Normal)).expect("open");
  // Far above what this test writes: no writer waits for segment space.
  set_wal_segment_test_limit(&db, 64 * 1024 * 1024);
  set_checkpoint_test_db_panic(&db, CheckpointPhase::HeaderWritten);
  let mut acked = Vec::new();
  let mut index = 0;
  // The log reaches the checkpoint trigger (four WALs) after about 900
  // commits; the thread's run then panics in its install. A commit fails
  // only once it is refused: the refusal comes just before the report.
  while checkpoint_thread_error(&db).is_none() && index < 3_000 {
    let key = key("a", index);
    match commit_key(&db, &key) {
      Ok(()) => acked.push(key),
      Err(error) => assert!(
        error.to_string().contains("refuses writes"),
        "a commit failed before the panic: {error}"
      ),
    }
    index += 1;
  }
  let deadline = Instant::now() + Duration::from_secs(5);
  wait_for("the panic to be reported", deadline, || {
    checkpoint_thread_error(&db).is_some()
  });
  let reported = checkpoint_thread_error(&db);
  assert!(
    reported
      .as_deref()
      .is_some_and(|error| error.contains("panic")),
    "the checkpoint thread's panic was not reported: {reported:?}"
  );

  let refused = |what: &str, result: Result<()>| {
    assert!(
      result
        .as_ref()
        .is_err_and(|error| error.to_string().contains("refuses writes")),
      "{what} after the panic: {result:?}"
    );
  };
  refused("a commit", commit_key(&db, &key("after", 0)));
  refused("a background checkpoint", db.background_checkpoint());
  refused("a blocking checkpoint", db.checkpoint());
  // Reads go on.
  assert!(missing(&db, &acked).is_empty(), "reads lost commits");
  refused("closing", close_single_file(db));

  let reopened = open_single_file(&path, options()).expect("reopen");
  assert!(
    missing(&reopened, &acked).is_empty(),
    "the reopen lost commits"
  );
  let after = commit_keys(&reopened, "reopened", 0, 10);
  assert_eq!(after.len(), 10, "the reopened database refused commits");
  acked.extend(after);
  reopened
    .checkpoint()
    .expect("a checkpoint after the reopen");
  close_single_file(reopened).expect("close");
  let reopened = open_single_file(&path, options()).expect("reopen again");
  assert!(missing(&reopened, &acked).is_empty());
}

/// Review finding R9, on the caller's thread. A write operation's panic
/// reaches its caller, who may catch it and go on (the Python bindings
/// raise it as an exception); the handle refuses writes from then on, as
/// after a checkpoint thread's panic. `operation` panics at `phase`, armed
/// on `db`, whose acknowledged commits are `acked`.
fn assert_writes_refused_after_a_caught_panic(
  db: SingleFileDB,
  path: &std::path::Path,
  reopen_options: SingleFileOpenOptions,
  mut acked: Vec<String>,
  phase: CheckpointPhase,
  mut operation: impl FnMut(&SingleFileDB, &mut Vec<String>) -> Result<()>,
) {
  set_checkpoint_test_db_panic(&db, phase);
  let caught =
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(&db, &mut acked)));
  assert!(caught.is_err(), "setup: no panic at {phase:?}: {caught:?}");
  // A rollback ends a transaction the panic left open, and succeeds.
  if db.has_transaction() {
    db.rollback().expect("the rollback after the panic");
  }
  let refused = |what: &str, result: Result<()>| {
    assert!(
      result
        .as_ref()
        .is_err_and(|error| error.to_string().contains("refuses writes")),
      "{what} after the panic at {phase:?}: {result:?}"
    );
  };
  refused("a commit", commit_key(&db, &key("after", 0)));
  refused("a background checkpoint", db.background_checkpoint());
  refused("a blocking checkpoint", db.checkpoint());
  assert!(missing(&db, &acked).is_empty(), "reads lost commits");
  refused("closing", close_single_file(db));

  let reopened = open_single_file(path, reopen_options.clone()).expect("reopen");
  assert!(
    missing(&reopened, &acked).is_empty(),
    "the reopen after the panic at {phase:?} lost commits"
  );
  assert!(reopened.node_by_key(&key("after", 0)).is_none());
  let after = commit_keys(&reopened, "reopened", 0, 10);
  assert_eq!(after.len(), 10, "the reopened database refused commits");
  acked.extend(after);
  reopened
    .checkpoint()
    .expect("a checkpoint after the reopen");
  close_single_file(reopened).expect("close");
  let reopened = open_single_file(path, reopen_options).expect("reopen again");
  assert!(missing(&reopened, &acked).is_empty());
}

/// R9 on a committing thread: a commit's spill panics between its header's
/// two slots (the WAL's records are in a segment, one slot names it, the
/// other still names them in the WAL).
#[test]
fn a_panic_in_a_commits_spill_refuses_writes() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("commit-spill-panic.kitedb");
  let db = open_single_file(&path, spilling_options()).expect("open");
  let acked = commit_keys(&db, "a", 0, 10);
  assert_writes_refused_after_a_caught_panic(
    db,
    &path,
    spilling_options(),
    acked,
    CheckpointPhase::SpillHeaderDurable,
    |db, acked| {
      // Commits until one spills (about 170 fill the WAL).
      for index in 10..2_000 {
        let key = key("a", index);
        commit_key(db, &key)?;
        acked.push(key);
      }
      Ok(())
    },
  );
}

/// R9 inside a transaction: a write too large for what the WAL has left
/// spills the transaction's records so far, and the spill panics.
#[test]
fn a_panic_in_a_transactions_spill_refuses_writes() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("transaction-spill-panic.kitedb");
  let db = open_single_file(&path, spilling_options()).expect("open");
  let acked = commit_keys(&db, "a", 0, 10);
  assert_writes_refused_after_a_caught_panic(
    db,
    &path,
    spilling_options(),
    acked,
    CheckpointPhase::SpillHeaderDurable,
    |db, _| {
      db.begin(false)?;
      for index in 0..2_000 {
        db.create_node(Some(&key("big", index)))?;
      }
      db.commit()
    },
  );
}

/// R9 in a blocking checkpoint: it panics in its install, between its
/// header writes.
#[test]
fn a_panic_in_a_blocking_checkpoint_refuses_writes() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("blocking-checkpoint-panic.kitedb");
  let db = open_single_file(&path, spilling_options()).expect("open");
  let acked = commit_keys(&db, "a", 0, 400);
  assert!(wal_segment_test_stats(&db).live > 0, "setup: no spill");
  assert_writes_refused_after_a_caught_panic(
    db,
    &path,
    spilling_options(),
    acked,
    CheckpointPhase::HeaderWritten,
    |db, _| db.checkpoint(),
  );
}

/// R9 in a background checkpoint the caller runs: it panics in its
/// install, between its header writes.
#[test]
fn a_panic_in_a_callers_background_checkpoint_refuses_writes() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("background-checkpoint-panic.kitedb");
  let db = open_single_file(&path, spilling_options()).expect("open");
  let acked = commit_keys(&db, "a", 0, 400);
  assert!(wal_segment_test_stats(&db).live > 0, "setup: no spill");
  assert_writes_refused_after_a_caught_panic(
    db,
    &path,
    spilling_options(),
    acked,
    CheckpointPhase::HeaderWritten,
    |db, _| db.background_checkpoint(),
  );
}

/// The other side of R9: a panic of the caller's own, outside the
/// database's write sections, leaves the handle writable, though the
/// transaction it drops rolls back while unwinding, and a destructor
/// commits while unwinding (taking the write sections then).
#[test]
fn a_callers_own_panic_leaves_writes_allowed() {
  struct CommitOnDrop<'db>(&'db SingleFileDB, String);
  impl Drop for CommitOnDrop<'_> {
    fn drop(&mut self) {
      commit_key(self.0, &self.1).expect("a commit while unwinding");
    }
  }
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("caller-panic.kitedb");
  let db = open_single_file(&path, spilling_options()).expect("open");
  let mut acked = commit_keys(&db, "a", 0, 10);
  let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
    let _commit = CommitOnDrop(&db, key("on-unwind", 0));
    let _tx = db.begin_guard(false).expect("begin");
    db.create_node(Some(&key("dropped", 0))).expect("node");
    panic!("the caller's own panic");
  }));
  assert!(caught.is_err());
  acked.push(key("on-unwind", 0));
  assert!(!db.has_transaction(), "the guard did not roll back");
  assert!(db.node_by_key(&key("dropped", 0)).is_none());
  acked.extend(commit_keys(&db, "b", 0, 400));
  assert_eq!(acked.len(), 411, "commits after the caller's panic failed");
  assert!(wal_segment_test_stats(&db).live > 0, "setup: no spill");
  db.checkpoint()
    .expect("a checkpoint after the caller's panic");
  close_single_file(db).expect("close");
  let reopened = open_single_file(&path, spilling_options()).expect("reopen");
  assert!(missing(&reopened, &acked).is_empty());
  assert!(reopened.node_by_key(&key("dropped", 0)).is_none());
}

/// Review finding 8(a). A rollback succeeds even when the WAL and its
/// segments are full: its ROLLBACK record is not needed (recovery drops a
/// transaction without a COMMIT record), so it never waits or fails for log
/// space.
#[test]
fn a_rollback_succeeds_when_the_log_is_full() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("rollback-log-full.kitedb");
  let db = open_single_file(&path, spilling_options()).expect("open");
  let acked = commit_keys(&db, "before", 0, 10);
  // A transaction whose records go to the WAL as it writes them: past a
  // spill, then filling the WAL to the last byte.
  db.begin(false).expect("begin");
  let big = |n: usize| format!("big-{n:06}-{}", "b".repeat(1000));
  let mut n = 0;
  while wal_segment_test_stats(&db).live == 0 || db.wal_buffer.lock().free() > 8 * 1024 {
    db.create_node(Some(&big(n))).expect("node");
    n += 1;
  }
  // A CreateNode record takes 36 bytes besides its key, padded to 8.
  let free = db.wal_buffer.lock().free() as usize;
  let prefix = format!("big-{n:06}-");
  db.create_node(Some(&format!(
    "{prefix}{}",
    "b".repeat(free - 36 - prefix.len())
  )))
  .expect("the last node");
  assert_eq!(db.wal_buffer.lock().free(), 0, "setup: the WAL is not full");
  // The segments are at their limit, and nothing checkpoints.
  set_wal_segment_test_limit(&db, 1);
  db.rollback()
    .expect("the rollback of a transaction that filled the log");
  assert!(db.node_by_key(&big(0)).is_none());
  close_single_file(db).expect("close");
  let reopened = open_single_file(&path, spilling_options()).expect("reopen");
  assert!(missing(&reopened, &acked).is_empty());
  assert!(reopened.node_by_key(&big(0)).is_none());
}

/// Review finding 8(b). Dropping the free pages at the end of the file
/// after an install is housekeeping: if it fails, the checkpoint (which
/// installed) still succeeds, and no error is left behind to stick.
#[test]
fn a_failed_tail_truncation_after_an_install_is_not_a_checkpoint_error() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("tail-truncation.kitedb");
  let db = open_single_file(&path, options().sync_mode(SyncMode::Normal)).expect("open");
  let acked = commit_keys(&db, "key", 0, 200);
  let generation = db.header.read().active_snapshot_gen;
  set_checkpoint_test_db_fault(&db, CheckpointPhase::TailTruncate, true);
  let result = db.background_checkpoint();
  let blocking = db.checkpoint();
  clear_checkpoint_test_db_faults(&db);
  assert!(
    result.is_ok() && blocking.is_ok(),
    "checkpoints that installed failed for their tail truncation: {result:?}, {blocking:?}"
  );
  assert!(db.header.read().active_snapshot_gen >= generation + 2);
  assert_eq!(checkpoint_thread_error(&db), None);
  close_single_file(db).expect("close");
  let reopened = open_single_file(&path, options()).expect("reopen");
  assert!(missing(&reopened, &acked).is_empty());
}

/// Review finding 8(c). A writer refused with `CheckpointFailed` (it needs
/// WAL segment space while the last automatic checkpoint failed) asks for
/// another checkpoint: once the failure's cause is gone, that run (after the
/// back-off) installs and clears the error, though nothing else asks.
#[test]
fn a_writer_refused_for_a_failed_checkpoint_asks_for_another() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("refused-writer-retries.kitedb");
  let db = open_single_file(&path, options().sync_mode(SyncMode::Normal)).expect("open");
  // One spill reaches the limit, far below the checkpoint trigger: only
  // writers waiting for space ask for checkpoints.
  set_wal_segment_test_limit(&db, 1);
  set_checkpoint_test_db_fault(&db, CheckpointPhase::SnapshotWritten, true);
  let mut acked = Vec::new();
  let mut failure = None;
  for index in 0..2_000 {
    let key = key("key", index);
    match commit_key(&db, &key) {
      Ok(()) => acked.push(key),
      Err(error) => {
        failure = Some(error);
        break;
      }
    }
  }
  clear_checkpoint_test_db_faults(&db);
  assert!(
    matches!(failure, Some(KiteError::CheckpointFailed(_))),
    "setup: the writer at the limit got {failure:?}"
  );
  let deadline = Instant::now() + Duration::from_secs(10);
  let cleared = wait_for("a retried checkpoint", deadline, || {
    checkpoint_thread_error(&db).is_none()
  });
  assert!(
    cleared,
    "no checkpoint ran after the refused writer: the error stays ({:?})",
    checkpoint_thread_error(&db)
  );
  acked.extend(commit_keys(&db, "after", 0, 10));
  close_single_file(db).expect("close");
  let reopened = open_single_file(&path, options()).expect("reopen");
  assert!(missing(&reopened, &acked).is_empty());
}

/// A transaction whose records spilled pins the segment it began in, and
/// every later one, until a checkpoint covers its commit. Segments are kept
/// whole, so the records written before it in that segment count against the
/// segment limit too while it is open. The default extent is small next to
/// the default limit, so a pin holds little of it: here a transaction that
/// begins in a nearly full extent, and a checkpoint after, leave at most a
/// quarter of the limit held. With extents half the limit, as the default
/// was (eight WALs against sixteen), such a transaction held half the limit
/// by itself, and writers beside it failed at random with `WalBufferFull`
/// (`segments_full_of_pinned`; the fresh review's stress test, once F1 was
/// fixed).
#[test]
fn an_open_transaction_pins_little_of_the_segment_limit() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("pinned-window.kitedb");
  let opts = options()
    .sync_mode(SyncMode::Normal)
    .auto_checkpoint(false)
    .mvcc(true);
  let db = Arc::new(open_single_file(&path, opts).expect("open"));
  let primary = 48 * 1024;
  let limit = db.wal_segment_limit(&db.header.read());
  let room = |db: &SingleFileDB| {
    let header = db.header.read();
    let page_size = header.page_size as u64;
    header
      .wal_segments
      .entries
      .last()
      .map_or(0, |last| last.page_count * page_size - last.byte_len)
  };
  // Fill the newest extent until one more spill (the transaction's records
  // with the commits before them) still fits, and no more.
  let mut index = 0;
  while !(wal_segment_test_stats(&db).live > 0 && room(&db) < 2 * primary) {
    commit_key(&db, &key("before", index)).expect("commit");
    index += 1;
  }
  // T: about 24 KiB of records, written to the WAL as it makes them; the
  // next spill moves them into the newest extent, which T then pins.
  let (go_tx, go_rx) = mpsc::channel::<()>();
  let (wrote_tx, wrote_rx) = mpsc::channel::<()>();
  let holder = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || {
      db.begin(false).expect("begin T");
      for n in 0..80 {
        db.create_node(Some(&key("t", n))).expect("T node");
      }
      wrote_tx.send(()).expect("T wrote");
      go_rx.recv().expect("finish T");
      db.rollback()
    })
  };
  wrote_rx.recv().expect("T wrote");
  let spills = db.wal_spills.load(Ordering::Acquire);
  while db.wal_spills.load(Ordering::Acquire) == spills {
    commit_key(&db, &key("before", index)).expect("commit");
    index += 1;
  }
  let pinned = db.oldest_pinned_segment().expect("T's records spilled");

  // A checkpoint covers the log; it keeps what T pins.
  db.background_checkpoint().expect("checkpoint");
  let held: u64 = db
    .header
    .read()
    .wal_segments
    .entries
    .iter()
    .filter(|segment| segment.seq >= pinned)
    .map(|segment| segment.byte_len)
    .sum();
  go_tx.send(()).expect("finish T");
  holder.join().expect("T thread").expect("T rollback");
  assert!(
    held <= limit / 4,
    "one open transaction holds {held} bytes of WAL segments after a checkpoint, of a \
     {limit}-byte limit: most were written before it, in the extent it began in"
  );
}

/// Review finding R10: a successful checkpoint ends the checkpoint thread's
/// back-off. Its run failed (the thread waits before the next, here a
/// minute), and the caller's own checkpoint (`checkpoint`) then succeeded,
/// which clears the error: a writer at the WAL segment limit is served at
/// once, not after the rest of the back-off.
#[test]
fn a_successful_blocking_checkpoint_ends_the_back_off() {
  a_successful_checkpoint_ends_the_back_off("blocking", |db| db.checkpoint());
}

/// R10 with the caller's `background_checkpoint`.
#[test]
fn a_successful_background_checkpoint_ends_the_back_off() {
  a_successful_checkpoint_ends_the_back_off("background", |db| db.background_checkpoint());
}

fn a_successful_checkpoint_ends_the_back_off(
  name: &str,
  checkpoint: impl Fn(&SingleFileDB) -> Result<()>,
) {
  use super::super::checkpoint_thread::set_checkpoint_test_backoff;
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join(format!("back-off-{name}.kitedb"));
  let db = open_single_file(&path, options().sync_mode(SyncMode::Normal)).expect("open");
  // One spill reaches the limit, far below the checkpoint trigger: only
  // writers waiting for space ask for checkpoints.
  set_wal_segment_test_limit(&db, 1);
  set_checkpoint_test_backoff(&db, Duration::from_secs(60), Duration::from_secs(60));
  set_checkpoint_test_db_fault(&db, CheckpointPhase::SnapshotWritten, true);
  let mut acked = Vec::new();
  let mut failure = None;
  for index in 0..2_000 {
    let key = key("key", index);
    match commit_key(&db, &key) {
      Ok(()) => acked.push(key),
      Err(error) => {
        failure = Some(error);
        break;
      }
    }
  }
  clear_checkpoint_test_db_faults(&db);
  assert!(
    matches!(failure, Some(KiteError::CheckpointFailed(_))),
    "setup: the writer at the limit got {failure:?}"
  );
  checkpoint(&db).expect("the caller's checkpoint");
  assert_eq!(checkpoint_thread_error(&db), None);

  // Two spills' worth of commits: the second waits for the thread's
  // checkpoint.
  let (done_tx, done_rx) = mpsc::channel();
  let started = Instant::now();
  let served = std::thread::scope(|scope| {
    let db = &db;
    scope.spawn(move || {
      let _ = done_tx.send(commit_keys(db, "after", 0, 400));
    });
    done_rx.recv_timeout(Duration::from_secs(20))
  });
  let after = served.unwrap_or_else(|_| {
    panic!(
      "a writer at the limit waited {:?} for the checkpoint thread's back-off",
      started.elapsed()
    )
  });
  assert_eq!(after.len(), 400, "writers after the checkpoint failed");
  acked.extend(after);
  close_single_file(db).expect("close");
  let reopened = open_single_file(&path, options()).expect("reopen");
  assert!(missing(&reopened, &acked).is_empty());
}

/// Review finding R8. A writer's spill drops the segments its decision to
/// spill counted out as unneeded (`can_spill`), even when a background
/// checkpoint claims the checkpoint status between the decision and the
/// spill (it cannot cut before the spill ends: both take the commit lock).
/// Otherwise the spill keeps them, the table grows past what the decision
/// counted, and once it is full a spill the decision allowed fails with
/// `WalBufferFull`.
#[test]
fn a_spill_drops_what_its_decision_counted_out_though_a_checkpoint_claims_meanwhile() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("spill-decision.kitedb");
  let opts = options()
    .sync_mode(SyncMode::Normal)
    .auto_checkpoint(false)
    .wal_segment_size(1)
    .mvcc(true);
  let db = Arc::new(open_single_file(&path, opts.clone()).expect("open"));

  // 63 segments the snapshot covers, kept for a transaction that then rolls
  // back: none is needed any more.
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
      db.rollback()
    })
  };
  held_rx.recv().expect("the holder wrote");
  let mut acked = Vec::new();
  let mut index = 0;
  while wal_segment_test_stats(&db).live < MAX_WAL_SEGMENTS - 1 && index < 200 {
    acked.extend(commit_keys(&db, "k", index, 1));
    db.background_checkpoint().expect("checkpoint");
    index += 1;
  }
  go_tx.send(()).expect("release the holder");
  holder
    .join()
    .expect("the holder thread")
    .expect("the holder's rollback");
  let unneeded = db
    .unneeded_wal_segments(&db.header.read().clone(), false)
    .len();
  assert_eq!(
    unneeded,
    MAX_WAL_SEGMENTS - 1,
    "setup: the kept segments are not all unneeded"
  );

  watch_checkpoint_phases(&db);
  for round in 0..4 {
    // Fill the WAL almost to the end with commits.
    while db.wal_buffer.lock().free() > 8 * 1024 {
      acked.extend(commit_keys(&db, "fill", index, 1));
      index += 1;
    }
    // A transaction whose records then do not fit decides to spill, and
    // waits there.
    let decided = Arc::new(Barrier::new(2));
    set_checkpoint_test_barrier(&db, CheckpointPhase::SpillDecided, Arc::clone(&decided));
    let writer = {
      let db = Arc::clone(&db);
      std::thread::spawn(move || -> Result<Vec<String>> {
        let keys: Vec<String> = (0..20)
          .map(|n| format!("w{round}-{n}-{}", "w".repeat(1000)))
          .collect();
        db.begin(false)?;
        for key in &keys {
          if let Err(error) = db.create_node(Some(key)) {
            let _ = db.rollback();
            return Err(error);
          }
        }
        db.commit()?;
        Ok(keys)
      })
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    let parked = wait_for("the writer's spill decision", deadline, || {
      checkpoint_test_reached(&db)
        .iter()
        .any(|(phase, _, parked)| *phase == CheckpointPhase::SpillDecided && *parked)
    });
    assert!(
      parked,
      "round {round}: setup: the writer never decided to spill"
    );
    // A background checkpoint claims the status meanwhile (it would cut
    // once the spill releases the commit lock).
    let run = match db.claim_background_checkpoint() {
      Ok(run) => run,
      Err(_) => panic!("round {round}: setup: no checkpoint could claim the status"),
    };
    decided.wait();
    let written = writer.join().expect("the writer thread");
    drop(run);
    watch_checkpoint_phases(&db);
    match written {
      Ok(keys) => acked.extend(keys),
      Err(error) => panic!(
        "round {round}: a spill the decision allowed failed: {error} ({:?})",
        wal_segment_test_stats(&db)
      ),
    }
  }
  let db = Arc::into_inner(db).expect("sole owner");
  close_single_file(db).expect("close");
  let reopened = open_single_file(&path, opts).expect("reopen");
  assert!(missing(&reopened, &acked).is_empty());
}
