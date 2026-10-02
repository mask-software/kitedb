//! raydb-b4 `fsync-group` lane: a `SyncMode::Full` commit group is made
//! durable with one sync, not two (one for its WAL records, one for the
//! header naming them).
//!
//! - fg1 (perf): a Full-mode group synced twice: its WAL bytes, then the
//!   header naming them.
//!
//! The rest pin what one sync per group must keep. A header written in the
//! same sync as the records it names can land without them (and in
//! `SyncMode::Normal`, whose writes are never synced, it always could), so
//! recovery may read WAL bytes no write of this salt cycle reached; they must
//! never parse as records. Crash images are built from the pager's write and
//! sync log: every write before the last successful sync landed, and each
//! write after it landed whole, not at all, or torn at page boundaries. In
//! every image each acknowledged commit survives, the commits recovered are a
//! prefix of the commit order, and no failed or dropped commit appears.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tempfile::tempdir;

use crate::core::pager::io_hooks::{self, IoEvent, SyncKind};
use crate::core::single_file::transaction::BEFORE_NEXT_COMMIT_LOCK;
use crate::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions, SyncMode,
};
use crate::error::Result;
use crate::types::{TxId, WalRecordType};

const PAGE: u64 = 4096;
/// The two header slots.
const HEADER_END: u64 = 2 * PAGE;

pub(crate) fn options(sync_mode: SyncMode) -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .auto_checkpoint(false)
    .background_checkpoint(false)
    .sync_mode(sync_mode)
}

pub(crate) fn commit_node(db: &SingleFileDB, key: &str) -> Result<TxId> {
  let txid = db.begin(false)?;
  db.create_node(Some(key))?;
  db.commit()?;
  Ok(txid)
}

fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
  let deadline = Instant::now() + Duration::from_secs(20);
  while !condition() {
    assert!(Instant::now() < deadline, "timed out waiting for {what}");
    std::thread::sleep(Duration::from_millis(1));
  }
}

/// One member of a group: begins, writes and commits on its own thread.
type Member<R> = Box<dyn FnOnce(&SingleFileDB) -> R + Send>;

/// Run `members` as one commit group, each on its own thread: the first
/// leads, and takes the queued commits only once every other member queued.
/// Its commits come first in the group, the others follow in queue order.
fn run_group<R: Send + 'static>(db: &Arc<SingleFileDB>, members: Vec<Member<R>>) -> Vec<R> {
  let count = members.len();
  let mut members = members.into_iter();
  let first = members.next().expect("a group has members");
  let leader_db = Arc::clone(db);
  let leading = Arc::new(AtomicBool::new(false));
  let leader = {
    let leading = Arc::clone(&leading);
    std::thread::spawn(move || {
      let hook_db = Arc::clone(&leader_db);
      BEFORE_NEXT_COMMIT_LOCK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
          leading.store(true, Ordering::SeqCst);
          wait_until("every member to queue", || {
            hook_db.commits_waiting.load(Ordering::SeqCst) == count
          });
        }));
      });
      first(&leader_db)
    })
  };
  // Only once it leads do the others queue behind it.
  wait_until("the leader", || leading.load(Ordering::SeqCst));
  let mut followers = Vec::new();
  for (index, member) in members.enumerate() {
    let member_db = Arc::clone(db);
    let queued = db.commits_waiting.load(Ordering::SeqCst);
    followers.push(std::thread::spawn(move || member(&member_db)));
    // Queue them one at a time, so their order in the group is this one.
    // (Once the last one queues, the group is written and leaves the queue.)
    if index + 2 < count {
      wait_until("the member to queue", || {
        db.commits_waiting.load(Ordering::SeqCst) > queued
      });
    }
  }
  let mut results = vec![leader.join().expect("leader thread")];
  results.extend(
    followers
      .into_iter()
      .map(|follower| follower.join().expect("member thread")),
  );
  results
}

// ============================================================================
// Crash images
// ============================================================================

/// How a write made after the last successful sync lands in a crash image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Landing {
  Whole,
  Lost,
  /// Every page but the first it touches.
  FirstPageLost,
  /// Only the first page it touches.
  OnlyFirstPage,
  /// Every page but the last it touches.
  LastPageLost,
}

pub(crate) const EVERY_LANDING: [Landing; 5] = [
  Landing::Whole,
  Landing::Lost,
  Landing::FirstPageLost,
  Landing::OnlyFirstPage,
  Landing::LastPageLost,
];

fn apply(image: &mut Vec<u8>, offset: u64, data: &[u8]) {
  let (start, end) = (offset as usize, offset as usize + data.len());
  if image.len() < end {
    image.resize(end, 0);
  }
  image[start..end].copy_from_slice(data);
}

/// Apply the pages of the write of `data` at `offset` that `landing` keeps.
fn apply_landing(image: &mut Vec<u8>, offset: u64, data: &[u8], landing: Landing) {
  let end = offset + data.len() as u64;
  let first_page = offset / PAGE;
  let last_page = (end - 1) / PAGE;
  for page in first_page..=last_page {
    let keep = match landing {
      Landing::Whole => true,
      Landing::Lost => false,
      Landing::FirstPageLost => page != first_page,
      Landing::OnlyFirstPage => page == first_page,
      Landing::LastPageLost => page != last_page,
    };
    if keep {
      let from = offset.max(page * PAGE);
      let to = end.min((page + 1) * PAGE);
      apply(
        image,
        from,
        &data[(from - offset) as usize..(to - offset) as usize],
      );
    }
  }
}

/// A crash image of the I/O in `events` up to `cut` (recorded from `base`):
/// the writes before the last successful sync landed, in order, and the
/// writes after it as `landings` says, one per write, in order.
fn crash_image(base: &[u8], events: &[IoEvent], cut: usize, landings: &[Landing]) -> Vec<u8> {
  let prefix = &events[..cut];
  let durable = prefix
    .iter()
    .rposition(|event| matches!(event, IoEvent::Sync { ok: true }))
    .map_or(0, |sync| sync + 1);
  let mut image = base.to_vec();
  for event in &prefix[..durable] {
    if let IoEvent::Write { offset, data } = event {
      apply(&mut image, *offset, data);
    }
  }
  let mut landings = landings.iter();
  for event in &prefix[durable..] {
    if let IoEvent::Write { offset, data } = event {
      let landing = landings.next().copied().unwrap_or(Landing::Whole);
      apply_landing(&mut image, *offset, data, landing);
    }
  }
  image
}

/// The writes in `events[..cut]` after its last successful sync.
fn unsynced_writes(events: &[IoEvent], cut: usize) -> usize {
  let prefix = &events[..cut];
  let durable = prefix
    .iter()
    .rposition(|event| matches!(event, IoEvent::Sync { ok: true }))
    .map_or(0, |sync| sync + 1);
  prefix[durable..]
    .iter()
    .filter(|event| matches!(event, IoEvent::Write { .. }))
    .count()
}

/// Every combination of `choices` for `writes` writes.
fn combinations(writes: usize, choices: &[Landing]) -> Vec<Vec<Landing>> {
  let mut all = vec![Vec::new()];
  for _ in 0..writes {
    all = all
      .into_iter()
      .flat_map(|prefix| {
        choices.iter().map(move |choice| {
          let mut next = prefix.clone();
          next.push(*choice);
          next
        })
      })
      .collect();
  }
  all
}

/// Commits recorded for crash images: their keys in commit order, and for
/// each the index of the event after which it was acknowledged (its last
/// successful sync), if it was.
pub(crate) struct Recorded {
  pub(crate) base: Vec<u8>,
  pub(crate) events: Vec<IoEvent>,
  keys: Vec<String>,
  acknowledged_after: Vec<Option<usize>>,
  /// Commits acknowledged before the recording: every image holds them.
  durable: Vec<String>,
}

impl Recorded {
  pub(crate) fn new(base: Vec<u8>) -> Self {
    Self {
      base,
      events: Vec::new(),
      keys: Vec::new(),
      acknowledged_after: Vec::new(),
      durable: Vec::new(),
    }
  }

  /// Commits of `keys` were acknowledged before the recording started.
  pub(crate) fn durable_before(&mut self, keys: &[String]) {
    self.durable.extend_from_slice(keys);
  }

  /// Add the I/O of commits of `keys`, in commit order, acknowledged once
  /// the last successful sync in `events` (if any) returned.
  pub(crate) fn push(&mut self, keys: &[String], events: Vec<IoEvent>, acknowledged: bool) {
    let offset = self.events.len();
    let last_sync = events
      .iter()
      .rposition(|event| matches!(event, IoEvent::Sync { ok: true }))
      .filter(|_| acknowledged)
      .map(|sync| offset + sync);
    self.events.extend(events);
    for key in keys {
      self.keys.push(key.clone());
      self.acknowledged_after.push(last_sync);
    }
  }

  /// Open every crash image (see [`crash_image`]) of a cut at every event,
  /// with each unsynced write landing in each of `landings` (all
  /// combinations), and check it: every commit acknowledged by the cut is
  /// there, the commits there are a prefix of the commit order, and no key
  /// of `never` is there once the cut reaches the event index paired with
  /// it. Returns the number of images checked.
  pub(crate) fn check_images(
    &self,
    dir: &Path,
    sync_mode: SyncMode,
    landings: &[Landing],
    never: &[(&str, usize)],
  ) -> usize {
    let path = dir.join("crash-image.kitedb");
    let mut checked = 0;
    for cut in 0..=self.events.len() {
      for combination in combinations(unsynced_writes(&self.events, cut), landings) {
        let image = crash_image(&self.base, &self.events, cut, &combination);
        let what = format!(
          "crash after {cut} of {} events, unsynced writes landing {combination:?}",
          self.events.len()
        );
        self.check_image(&path, &image, sync_mode, cut, never, &what);
        checked += 1;
      }
    }
    checked
  }

  pub(crate) fn check_image(
    &self,
    path: &Path,
    image: &[u8],
    sync_mode: SyncMode,
    cut: usize,
    never: &[(&str, usize)],
    what: &str,
  ) {
    std::fs::write(path, image).expect("write image");
    let crashed = open_single_file(path, options(sync_mode))
      .unwrap_or_else(|error| panic!("{what}: unopenable: {error:?}"));
    let present: Vec<bool> = self
      .keys
      .iter()
      .map(|key| crashed.node_by_key(key).is_some())
      .collect();
    let durable_lost: Vec<&String> = self
      .durable
      .iter()
      .filter(|key| crashed.node_by_key(key).is_none())
      .collect();
    let resurrected: Vec<&str> = never
      .iter()
      .filter(|(key, from)| cut >= *from && crashed.node_by_key(key).is_some())
      .map(|(key, _)| *key)
      .collect();
    close_single_file(crashed).expect("close image");
    std::fs::remove_file(path).expect("remove image");
    let lost: Vec<&String> = self
      .keys
      .iter()
      .zip(&self.acknowledged_after)
      .zip(&present)
      .filter(|((_, acknowledged), present)| {
        acknowledged.is_some_and(|sync| sync < cut) && !**present
      })
      .map(|((key, _), _)| key)
      .collect();
    assert!(
      lost.is_empty() && durable_lost.is_empty(),
      "{what}: lost acknowledged commits {lost:?} {durable_lost:?}"
    );
    let recovered = present.iter().take_while(|present| **present).count();
    assert!(
      present[recovered..].iter().all(|present| !present),
      "{what}: the commits recovered are not a prefix of the commit order: {:?}",
      self.keys.iter().zip(&present).collect::<Vec<_>>()
    );
    assert!(resurrected.is_empty(), "{what}: replayed {resurrected:?}");
  }
}

/// Commit `keys` one at a time on this thread, recording each commit's I/O.
/// Only a Full-mode commit is acknowledged as durable (by its last sync).
pub(crate) fn record_commits(db: &SingleFileDB, recorded: &mut Recorded, keys: &[String]) {
  let acknowledged = db.sync_mode == SyncMode::Full;
  for key in keys {
    let (committed, events) = io_hooks::record_io_during(|| commit_node(db, key));
    committed.expect("commit");
    recorded.push(std::slice::from_ref(key), events, acknowledged);
  }
}

/// Keys of different sizes: small ones, and one whose record spans pages.
pub(crate) fn mixed_keys(prefix: &str, count: usize) -> Vec<String> {
  (0..count)
    .map(|index| match index % 3 {
      1 => format!("{prefix}-{index}-{}", "k".repeat(6000)),
      _ => format!("{prefix}-{index}"),
    })
    .collect()
}

// ============================================================================
// fg1: one sync per Full-mode group
// ============================================================================

/// A Full-mode group of queued commits, and a lone commit, each sync once.
#[test]
fn fg1_full_mode_group_syncs_once() {
  const MEMBERS: usize = 6;
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("one-sync.kitedb");
  let db = Arc::new(open_single_file(&path, options(SyncMode::Full)).expect("open"));
  commit_node(&db, "warm").expect("warm commit");

  let (committed, lone) = io_hooks::sync_kinds_during(|| commit_node(&db, "lone"));
  committed.expect("lone commit");
  let members = (0..MEMBERS)
    .map(|index| -> Member<(Result<TxId>, Vec<SyncKind>)> {
      Box::new(move |db: &SingleFileDB| {
        let key = format!("member-{index}");
        io_hooks::sync_kinds_during(|| commit_node(db, &key))
      })
    })
    .collect();
  let results = run_group(&db, members);
  assert!(results.iter().all(|(result, _)| result.is_ok()));
  let group: Vec<SyncKind> = results.into_iter().flat_map(|(_, syncs)| syncs).collect();
  assert_eq!(
    (lone.as_slice(), group.as_slice()),
    (&[SyncKind::Data][..], &[SyncKind::Data][..]),
    "(syncs of a lone Full-mode commit, of a group of {MEMBERS})"
  );
}

/// With `full_fsync`, the one sync is still F_FULLFSYNC on macOS.
#[cfg(target_os = "macos")]
#[test]
fn fg1_full_fsync_is_the_one_sync() {
  use crate::core::pager::SYNC_PRIMITIVE_LOG;
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("full-fsync.kitedb");
  let db = open_single_file(&path, options(SyncMode::Full).full_fsync(true)).expect("open");
  commit_node(&db, "warm").expect("warm commit");
  SYNC_PRIMITIVE_LOG.with(|log| log.borrow_mut().clear());
  commit_node(&db, "measured").expect("commit");
  let primitives = SYNC_PRIMITIVE_LOG.with(|log| log.borrow().clone());
  close_single_file(db).expect("close");
  assert_eq!(
    primitives,
    ["F_FULLFSYNC"],
    "a Full-mode commit with full_fsync"
  );
}

// ============================================================================
// Records land only on synced zeros
// ============================================================================

/// The invariant the crash images rely on, checked on the pager's log of a
/// workload on a reopened database (commits of several sizes from several
/// threads, and a blocking checkpoint that resets the WAL): every byte of
/// WAL records written to the file lands where zeros were written and then
/// synced, since the open. (A database's first open creates its WAL as
/// synced zeros, so its records need no zeros written first.)
fn wal_records_land_only_on_synced_zeros(sync_mode: SyncMode) {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("zeros.kitedb");
  let options = options(sync_mode).wal_size(256 * 1024);
  let db = open_single_file(&path, options.clone()).expect("create");
  commit_node(&db, "created").expect("commit");
  close_single_file(db).expect("close");
  let db = Arc::new(open_single_file(&path, options).expect("reopen"));
  let wal_start = db.header.read().wal_start_page * PAGE;
  let wal_end = wal_start + db.header.read().wal_page_count * PAGE;
  // Every writer's I/O is logged on its own thread; only a group's leader
  // writes, so the logs, merged by when each commit returned, keep the
  // order of the writes to any one byte within this check's needs: each
  // commit's zeros and sync come before its records.
  let mut logs: Vec<Vec<IoEvent>> = Vec::new();
  let (_, events) = io_hooks::record_io_during(|| {
    for key in mixed_keys("solo", 6) {
      commit_node(&db, &key).expect("commit");
    }
  });
  logs.push(events);
  let members = (0..4)
    .map(|index| -> Member<(Result<TxId>, Vec<IoEvent>)> {
      Box::new(move |db: &SingleFileDB| {
        io_hooks::record_io_during(|| {
          commit_node(db, &format!("group-{index}-{}", "g".repeat(9000)))
        })
      })
    })
    .collect();
  for (committed, events) in run_group(&db, members) {
    committed.expect("member commit");
    logs.push(events);
  }
  let (_, events) = io_hooks::record_io_during(|| {
    db.checkpoint().expect("checkpoint");
    for key in mixed_keys("after", 9) {
      commit_node(&db, &key).expect("commit");
    }
  });
  logs.push(events);

  let mut zero_written = vec![false; (wal_end - wal_start) as usize];
  let mut zero_durable = vec![false; (wal_end - wal_start) as usize];
  let mut record_bytes = 0usize;
  for event in logs.iter().flatten() {
    match event {
      IoEvent::Sync { ok: true } => {
        for (durable, written) in zero_durable.iter_mut().zip(&zero_written) {
          *durable |= *written;
        }
      }
      IoEvent::Write { offset, data } if *offset >= wal_start && *offset < wal_end => {
        let at = (*offset - wal_start) as usize;
        if data.iter().all(|byte| *byte == 0) {
          zero_written[at..at + data.len()].fill(true);
          continue;
        }
        let unprepared = (at..at + data.len()).find(|byte| !zero_durable[*byte]);
        assert!(
          unprepared.is_none(),
          "a {}-byte WAL write at WAL offset {at} covers offset {:?}, never zeroed and synced",
          data.len(),
          unprepared
        );
        // Records (and then a page's other bytes) are no longer zeros.
        zero_written[at..at + data.len()].fill(false);
        zero_durable[at..at + data.len()].fill(false);
        record_bytes += data.len();
      }
      _ => {}
    }
  }
  assert!(record_bytes > 0, "the workload wrote no WAL records");
}

/// Zeros ahead cost a small session little: a new database's WAL is created
/// zeroed, so its commits write no zeros; a reopened one zeroes 64 KiB (and
/// syncs) before its first commit's records, and nothing more for the next
/// few small commits.
#[test]
fn fg_zeros_ahead_cost_a_small_session_little() {
  let zero_bytes = |events: &[IoEvent]| -> (usize, usize) {
    let zeros = events
      .iter()
      .filter_map(|event| match event {
        IoEvent::Write { offset, data }
          if *offset >= HEADER_END && data.iter().all(|b| *b == 0) =>
        {
          Some(data.len())
        }
        _ => None,
      })
      .sum();
    let syncs = events
      .iter()
      .filter(|event| matches!(event, IoEvent::Sync { .. }))
      .count();
    (zeros, syncs)
  };
  for sync_mode in [SyncMode::Full, SyncMode::Normal] {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("small-session.kitedb");
    let db = open_single_file(&path, options(sync_mode)).expect("create");
    let (_, created) = io_hooks::record_io_during(|| {
      for index in 0..20 {
        commit_node(&db, &format!("new-{index}")).expect("commit");
      }
    });
    close_single_file(db).expect("close");
    let db = open_single_file(&path, options(sync_mode)).expect("reopen");
    let (_, first) = io_hooks::record_io_during(|| commit_node(&db, "first").expect("commit"));
    let (_, next) = io_hooks::record_io_during(|| {
      for index in 0..20 {
        commit_node(&db, &format!("next-{index}")).expect("commit");
      }
    });
    close_single_file(db).expect("close");
    let commit_syncs = usize::from(sync_mode == SyncMode::Full);
    // 64 KiB past the first commit's records, page aligned.
    let (first_zeros, first_syncs) = zero_bytes(&first);
    assert!(
      (64 * 1024..=68 * 1024).contains(&first_zeros),
      "{sync_mode:?}: the first commit after a reopen zeroed {first_zeros} bytes"
    );
    assert_eq!(
      (zero_bytes(&created), first_syncs, zero_bytes(&next)),
      (
        (0, 20 * commit_syncs),
        1 + commit_syncs,
        (0, 20 * commit_syncs)
      ),
      "{sync_mode:?}: (zero bytes, syncs) of 20 commits to a new database, of the first commit \
       after a reopen, and of the 20 after it"
    );
  }
}

#[test]
fn fg_wal_records_land_only_on_synced_zeros_in_full_mode() {
  wal_records_land_only_on_synced_zeros(SyncMode::Full);
}

#[test]
fn fg_wal_records_land_only_on_synced_zeros_in_normal_mode() {
  wal_records_land_only_on_synced_zeros(SyncMode::Normal);
}

// ============================================================================
// Crash images of Full-mode groups
// ============================================================================

/// Every prefix of the writes of a run of Full-mode commits, in order.
#[test]
fn fg_crash_every_prefix_of_writes_keeps_acknowledged_commits() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("prefixes.kitedb");
  let db = open_single_file(&path, options(SyncMode::Full)).expect("open");
  commit_node(&db, "base").expect("base");
  let mut recorded = Recorded::new(std::fs::read(&path).expect("base image"));
  record_commits(&db, &mut recorded, &mixed_keys("p", 6));
  drop(db);
  let checked = recorded.check_images(dir.path(), SyncMode::Full, &[Landing::Whole], &[]);
  assert!(checked > 6);
}

/// The header of a group lands without (some of) the group's WAL pages, or
/// they land without it, in every combination.
#[test]
fn fg_crash_with_headers_ahead_of_and_behind_data() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("reordered.kitedb");
  let db = open_single_file(&path, options(SyncMode::Full)).expect("open");
  commit_node(&db, "base").expect("base");
  let mut recorded = Recorded::new(std::fs::read(&path).expect("base image"));
  record_commits(&db, &mut recorded, &mixed_keys("r", 4));
  drop(db);
  recorded.check_images(
    dir.path(),
    SyncMode::Full,
    &[Landing::Whole, Landing::Lost],
    &[],
  );
}

/// A group's WAL write torn at page boundaries, mid-record, with or without
/// its header: recovery stops at the torn record, after a prefix of the
/// group's commits.
#[test]
fn fg_crash_with_a_torn_record_mid_group() {
  const MEMBERS: usize = 4;
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("torn-group.kitedb");
  let db = Arc::new(open_single_file(&path, options(SyncMode::Full)).expect("open"));
  commit_node(&db, "base").expect("base");
  let mut recorded = Recorded::new(std::fs::read(&path).expect("base image"));
  let keys: Vec<String> = (0..MEMBERS)
    .map(|index| format!("g{index}-{}", "x".repeat(2500)))
    .collect();
  let members = keys
    .iter()
    .enumerate()
    .map(|(index, key)| -> Member<(Result<TxId>, Vec<IoEvent>)> {
      let key = key.clone();
      Box::new(move |db: &SingleFileDB| {
        if index == 0 {
          io_hooks::record_io_during(|| commit_node(db, &key))
        } else {
          (commit_node(db, &key), Vec::new())
        }
      })
    })
    .collect();
  let mut results = run_group(&db, members);
  assert!(results.iter().all(|(result, _)| result.is_ok()));
  let events = std::mem::take(&mut results[0].1);
  recorded.push(&keys, events, true);
  drop(results);
  let db = Arc::into_inner(db).expect("db unique");
  drop(db);
  recorded.check_images(dir.path(), SyncMode::Full, &EVERY_LANDING, &[]);
}

/// The newest header slot is torn by a crash right after its write: the
/// other slot, one group older, is the fallback, and every commit it names
/// (each acknowledged one) is there.
#[test]
fn fg_crash_with_the_newest_header_torn() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("torn-header.kitedb");
  let db = open_single_file(&path, options(SyncMode::Full)).expect("open");
  commit_node(&db, "base").expect("base");
  let mut recorded = Recorded::new(std::fs::read(&path).expect("base image"));
  record_commits(&db, &mut recorded, &mixed_keys("h", 4));
  drop(db);
  let image_path = dir.path().join("crash-image.kitedb");
  let mut torn = 0;
  for (index, event) in recorded.events.iter().enumerate() {
    if !matches!(event, IoEvent::Write { offset, .. } if *offset < HEADER_END) {
      continue;
    }
    // Every earlier write landed; this one only its first half.
    let mut image = recorded.base.clone();
    for (other, event) in recorded.events.iter().enumerate().take(index + 1) {
      if let IoEvent::Write { offset, data } = event {
        let len = if other == index {
          data.len() / 2
        } else {
          data.len()
        };
        apply(&mut image, *offset, &data[..len]);
      }
    }
    recorded.check_image(
      &image_path,
      &image,
      SyncMode::Full,
      index,
      &[],
      &format!("header write at event {index} torn, crash right after it"),
    );
    torn += 1;
  }
  assert!(torn >= 4, "the commits wrote {torn} headers");
}

/// Commits after a background checkpoint's install (which moved the WAL
/// back to a rewritten primary region) and after a blocking checkpoint
/// (which reset it): every crash image of them keeps the commits.
#[test]
fn fg_crash_after_a_checkpoint_install_keeps_acknowledged_commits() {
  for background in [true, false] {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("after-install.kitedb");
    let db = open_single_file(&path, options(SyncMode::Full)).expect("open");
    let before = mixed_keys("pre", 4);
    for key in &before {
      commit_node(&db, key).expect("commit");
    }
    if background {
      db.background_checkpoint().expect("background checkpoint");
    } else {
      db.checkpoint().expect("checkpoint");
    }
    let mut recorded = Recorded::new(std::fs::read(&path).expect("base image"));
    recorded.durable_before(&before);
    record_commits(&db, &mut recorded, &mixed_keys("post", 4));
    drop(db);
    recorded.check_images(
      dir.path(),
      SyncMode::Full,
      &[Landing::Whole, Landing::Lost, Landing::FirstPageLost],
      &[],
    );
  }
}

// ============================================================================
// Failed groups
// ============================================================================

/// A Full-mode commit whose group sync fails returns an error, once its
/// records are rewritten as a rollback and that is synced. No crash image
/// after it returned holds it, whichever of the writes after the last
/// successful sync landed; the next commit is in every image after its sync.
#[test]
fn fg_failed_group_stays_failed_after_an_os_crash() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("failed.kitedb");
  let db = open_single_file(&path, options(SyncMode::Full)).expect("open");
  commit_node(&db, "base").expect("base");
  let mut recorded = Recorded::new(std::fs::read(&path).expect("base image"));
  let (failed, events) =
    io_hooks::record_io_during(|| io_hooks::with_failing_syncs(1, || commit_node(&db, "failed")));
  assert!(failed.is_err(), "the group's sync was made to fail");
  assert!(db.node_by_key("failed").is_none());
  recorded.push(&[], events, false);
  let returned = recorded.events.len();
  record_commits(&db, &mut recorded, &["after".to_string()]);
  drop(db);
  recorded.check_images(
    dir.path(),
    SyncMode::Full,
    &[Landing::Whole, Landing::Lost],
    &[("failed", returned)],
  );
}

/// The rollback's sync fails too. Until a later sync succeeds the failed
/// group's records and the header written with them may both be on disk, as
/// with any write whose sync failed; so this checks the pager log's own
/// model of the page cache (after the last successful sync, header pages
/// keep their last write and other pages their first): the header written
/// after the failure, naming only the commits before the group, wins.
#[test]
fn fg_failed_group_stays_failed_when_its_rollback_cannot_sync() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("failed-twice.kitedb");
  let db = open_single_file(&path, options(SyncMode::Full)).expect("open");
  commit_node(&db, "base").expect("base");
  let mut recorded = Recorded::new(std::fs::read(&path).expect("base image"));
  let (failed, events) =
    io_hooks::record_io_during(|| io_hooks::with_failing_syncs(2, || commit_node(&db, "failed")));
  assert!(failed.is_err(), "the group's sync was made to fail");
  recorded.push(&[], events, false);
  let returned = recorded.events.len();
  record_commits(&db, &mut recorded, &["after".to_string()]);
  drop(db);
  for cut in returned..=recorded.events.len() {
    let image = io_hooks::crash_image(&recorded.base, &recorded.events[..cut], HEADER_END);
    recorded.check_image(
      &dir.path().join("model-image.kitedb"),
      &image,
      SyncMode::Full,
      cut,
      &[("failed", returned)],
      &format!("pager-model crash after {cut} events"),
    );
  }
}

// ============================================================================
// Bytes past the head left by an earlier crash
// ============================================================================

/// Where each COMMIT record of `db`'s WAL ends (relative to the WAL start).
fn commit_ends(db: &SingleFileDB) -> Vec<u64> {
  let mut pager = db.pager.lock();
  let mut wal = db.wal_buffer.lock();
  wal
    .scan_records(&mut pager)
    .expect("scan")
    .iter()
    .filter(|record| record.record_type == WalRecordType::Commit)
    .map(|record| wal.tail() + record.record_end as u64)
    .collect()
}

/// A crash tore a group `{b, c}`: `b`'s first page never landed, `c`'s
/// records did, whole. Recovery stops at `b` and drops both. A later commit
/// `d` of `b`'s size then ends exactly where `c` starts, and `e` follows. No
/// crash image (for instance one where `e`'s header landed but its WAL page
/// did not) may replay `c`, a commit dropped before `d` was made.
fn stale_records_past_the_head_are_never_replayed(sync_mode: SyncMode) {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("stale.kitedb");
  let db = Arc::new(open_single_file(&path, options(sync_mode)).expect("open"));
  commit_node(&db, "a").expect("a");
  let base = std::fs::read(&path).expect("base image");
  let b = format!("b-{}", "b".repeat(6000));
  let members: Vec<Member<(Result<TxId>, Vec<IoEvent>)>> = vec![
    {
      let b = b.clone();
      Box::new(move |db: &SingleFileDB| io_hooks::record_io_during(|| commit_node(db, &b)))
    },
    Box::new(|db: &SingleFileDB| (commit_node(db, "c"), Vec::new())),
  ];
  let mut results = run_group(&db, members);
  assert!(results.iter().all(|(result, _)| result.is_ok()));
  let group_events = std::mem::take(&mut results[0].1);
  drop(results);
  let wal_start = db.header.read().wal_start_page * PAGE;
  let ends = commit_ends(&db);
  assert_eq!(ends.len(), 3, "setup: a, b and c commit, in that order");
  let (a_end, b_end) = (ends[0], ends[1]);
  let db = Arc::into_inner(db).expect("db unique");
  drop(db);

  // The group's WAL write, with the page holding `b`'s first record lost.
  let (group_offset, group_data) = group_events
    .iter()
    .find_map(|event| match event {
      IoEvent::Write { offset, data } if *offset == wal_start + a_end => {
        Some((*offset, data.clone()))
      }
      _ => None,
    })
    .expect("the group's WAL write");
  assert!(
    group_offset + group_data.len() as u64 >= wal_start + ends[2],
    "setup: the group's records are written with one write"
  );
  let mut torn = base;
  apply_landing(&mut torn, group_offset, &group_data, Landing::FirstPageLost);
  let torn_path = dir.path().join("torn.kitedb");
  std::fs::write(&torn_path, &torn).expect("write torn image");

  let db = open_single_file(&torn_path, options(sync_mode)).expect("open torn image");
  assert!(db.node_by_key("a").is_some());
  assert!(
    db.node_by_key(&b).is_none() && db.node_by_key("c").is_none(),
    "setup: recovery stops at b's lost first page"
  );
  let mut recorded = Recorded::new(std::fs::read(&torn_path).expect("base image"));
  let d = format!("d-{}", "d".repeat(6000));
  let e = format!("e-{}", "e".repeat(64));
  record_commits(&db, &mut recorded, &[d, e]);
  let ends = commit_ends(&db);
  assert_eq!(
    (ends.len(), ends[1]),
    (3, b_end),
    "setup: d ends where b ended, where c's records start"
  );
  drop(db);
  recorded.check_images(
    dir.path(),
    sync_mode,
    &[Landing::Whole, Landing::Lost],
    &[("c", 0)],
  );
}

#[test]
fn fg_stale_records_past_the_head_are_never_replayed_in_normal_mode() {
  stale_records_past_the_head_are_never_replayed(SyncMode::Normal);
}

#[test]
fn fg_stale_records_past_the_head_are_never_replayed_in_full_mode() {
  stale_records_past_the_head_are_never_replayed(SyncMode::Full);
}
