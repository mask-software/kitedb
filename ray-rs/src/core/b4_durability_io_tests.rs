//! Durability and file I/O findings from the wave-1 core audit (raydb-b4
//! lane `durability-io`, Oct 2026), each written to fail before its fix.
//! Test names start with the finding's number:
//!
//! - f1: a short read (POSIX allows one before EOF: NFS, signals) left the
//!   rest of the page zeroed, which recovery took for the end of the WAL;
//!   and each page read or write cost two system calls (seek, then I/O).
//! - f2: allocations that skipped the page at 1 GiB (reserved, as in SQLite,
//!   for byte-range locks KiteDB never takes) were ignored by their callers.
//! - f3: creating a database truncated one created since `open_single_file`
//!   saw no file, and never fsynced the new directory entry.
//! - f4: replay skipped records whose CRC checks but whose payload does not
//!   parse (a writer bug or a format mismatch, not a torn write).
//! - f5: WAL positions outside their regions were accepted at open, and
//!   underflowed later.
//! - f6: the docs called the CRC-32 (IEEE) checksum CRC32C.
//! - f8: vacuum and WAL resize kept `SyncMode::Off` commits (a guard: it did
//!   not reproduce on the crash-safe compactor).
//! - f9: dropping a `SyncMode::Off` database without closing it lost every
//!   commit since the last checkpoint.
//! - x (from the wave-2 integration report): in `SyncMode::Normal`, a commit
//!   whose header write failed left its COMMIT record in the file under the
//!   region's salt; an OS crash after the next commit's header write, before
//!   its WAL page landed, replayed the failed commit.
//!
//! Finding 7 (hazardous test-only `WalBuffer` methods) needs no test.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use tempfile::tempdir;

use crate::core::header::{read_header_slots, write_header_slot, HEADER_SLOT_A, HEADER_SLOT_B};
use crate::core::pager::io_hooks::{self, IoEvent};
use crate::core::pager::{create_pager, open_pager};
use crate::core::single_file::{
  close_single_file, open_single_file, ResizeWalOptions, SingleFileDB, SingleFileOpenOptions,
  SyncMode,
};
use crate::core::wal::buffer::WalBuffer;
use crate::core::wal::record::{
  build_create_node_payload, extract_committed_transactions_in_order, WalRecord,
};
use crate::error::KiteError;
use crate::types::{DbHeaderV1, WalRecordType};

const PAGE_SIZE: usize = 4096;

/// The page at file offset 1 GiB, with 4 KiB pages.
const ONE_GIB_PAGE: u64 = (1 << 30) / PAGE_SIZE as u64;

fn options() -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .auto_checkpoint(false)
    .background_checkpoint(false)
}

fn commit_nodes(db: &SingleFileDB, keys: &[String]) {
  db.begin(false).expect("begin");
  for key in keys {
    db.create_node(Some(key)).expect("create node");
  }
  db.commit().expect("commit");
}

fn keys(prefix: &str, count: usize) -> Vec<String> {
  (0..count)
    .map(|index| format!("{prefix}-{index}"))
    .collect()
}

fn missing<'a>(db: &SingleFileDB, keys: &'a [String]) -> Vec<&'a str> {
  keys
    .iter()
    .filter(|key| db.node_by_key(key).is_none())
    .map(String::as_str)
    .collect()
}

// ============================================================================
// f1: short reads, and two system calls per page
// ============================================================================

#[test]
fn f1_read_page_finishes_a_short_read() {
  let dir = tempdir().expect("tempdir");
  let mut pager = create_pager(dir.path().join("short-read.kitedb"), PAGE_SIZE).expect("pager");
  let page: Vec<u8> = (0..PAGE_SIZE).map(|i| (i % 251) as u8 + 1).collect();
  pager.write_page(3, &page).expect("write page");

  let read = io_hooks::with_short_reads(0, 1000, || pager.read_page(3)).expect("read page");
  let zeroed = read.iter().filter(|byte| **byte == 0).count();
  assert!(
    read == page,
    "a short read returned the page with {zeroed} of its {PAGE_SIZE} bytes zeroed"
  );
}

/// A zero-filled WAL page reads as the end of the WAL: replay stops there,
/// and a writable open trims the WAL to that point for good.
#[test]
fn f1_reopen_under_short_reads_keeps_every_commit() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("short-read-recovery.kitedb");
  let options = options().sync_mode(SyncMode::Normal);
  let db = open_single_file(&path, options.clone()).expect("open");
  let committed: Vec<String> = (0..4)
    .flat_map(|tx| {
      let tx_keys = keys(&format!("tx{tx}"), 100);
      commit_nodes(&db, &tx_keys);
      tx_keys
    })
    .collect();
  let wal_offset = db.header.read().wal_start_page * PAGE_SIZE as u64;
  close_single_file(db).expect("close");

  // Header pages read whole; every WAL page read returns 512 bytes at a time.
  let db = io_hooks::with_short_reads(wal_offset, 512, || open_single_file(&path, options.clone()))
    .expect("open under short reads");
  let lost = missing(&db, &committed).len();
  close_single_file(db).expect("close");
  let db = open_single_file(&path, options).expect("reopen");
  let lost_for_good = missing(&db, &committed).len();
  close_single_file(db).expect("close");
  assert_eq!(
    (lost, lost_for_good),
    (0, 0),
    "of {} committed nodes, an open under short reads lost {lost}, and {lost_for_good} stayed \
     lost after a normal reopen",
    committed.len()
  );
}

/// Positioned I/O (pread/pwrite) reads or writes a page in one call.
#[test]
fn f1_page_io_costs_one_syscall_per_page() {
  let dir = tempdir().expect("tempdir");
  let mut pager = create_pager(dir.path().join("syscalls.kitedb"), PAGE_SIZE).expect("pager");
  let page = vec![0x5a; PAGE_SIZE];
  pager.write_page(0, &page).expect("extend");

  let (written, writes) = io_hooks::syscalls_during(|| pager.write_page(0, &page));
  written.expect("write page");
  let (read, reads) = io_hooks::syscalls_during(|| pager.read_page(0));
  assert_eq!(read.expect("read page"), page);
  assert_eq!(
    (reads, writes),
    (1, 1),
    "system calls per page (read, write): a seek before every read and write"
  );
}

// ============================================================================
// f2: allocations past the page at 1 GiB
// ============================================================================

/// A checkpoint whose snapshot is appended at the page at 1 GiB. The file is
/// grown sparsely to put the append point there.
#[cfg(unix)]
#[test]
fn f2_checkpoint_writes_a_snapshot_at_one_gib() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("one-gib-snapshot.kitedb");
  let options = options().wal_size(64 * 1024);
  let db = open_single_file(&path, options.clone()).expect("open");
  let committed = keys("node", 64);
  commit_nodes(&db, &committed);
  {
    let mut pager = db.pager.lock();
    let file_pages = pager.file_size() / PAGE_SIZE as u64;
    pager
      .allocate_pages((ONE_GIB_PAGE - file_pages) as u32)
      .expect("grow sparsely");
    assert_eq!(pager.file_size(), ONE_GIB_PAGE * PAGE_SIZE as u64);
  }

  let checkpointed = db.checkpoint();
  close_single_file(db).expect("close");
  checkpointed.expect("checkpoint with its snapshot appended at 1 GiB");
  let db = open_single_file(&path, options).expect("reopen");
  assert_eq!(missing(&db, &committed), Vec::<&str>::new());
  close_single_file(db).expect("close");
}

/// A WAL spanning the page at 1 GiB is allocated where the header says it
/// is, and every page of it is writable. No WAL record is written, so
/// nothing reads the (sparse) 1 GiB WAL back.
#[cfg(unix)]
#[test]
fn f2_wal_spanning_one_gib_lies_where_the_header_says() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("one-gib-wal.kitedb");
  let options = options().wal_size((1 << 30) + 64 * 1024);
  let db = open_single_file(&path, options.clone()).expect("open");

  let header = db.header.read().clone();
  let wal_pages = header.wal_start_page..header.wal_start_page + header.wal_page_count;
  let file_pages = db.pager.lock().file_size() / PAGE_SIZE as u64;
  let one_gib_page_written = if wal_pages.contains(&ONE_GIB_PAGE) {
    Some(
      db.pager
        .lock()
        .write_page(ONE_GIB_PAGE as u32, &vec![0; PAGE_SIZE]),
    )
  } else {
    None
  };
  close_single_file(db).expect("close");

  assert_eq!(
    file_pages, wal_pages.end,
    "the header names WAL pages {wal_pages:?}, but the file was allocated to page {file_pages}"
  );
  if let Some(written) = one_gib_page_written {
    written.expect("WAL page at 1 GiB must be writable");
  }
  open_single_file(&path, options)
    .and_then(close_single_file)
    .expect("reopen");
}

// ============================================================================
// f3: create races, and the new directory entry
// ============================================================================

/// `open_single_file` saw no file, then another opener created a database
/// there and closed it before this create took the lock.
#[test]
fn f3_create_never_truncates_a_database_created_since_the_exists_check() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("create-race.kitedb");
  let racer_options = options();
  io_hooks::before_next_create_lock(move |path| {
    let racer = open_single_file(path, racer_options).expect("racing create");
    commit_nodes(&racer, &["racer".to_string()]);
    close_single_file(racer).expect("racing close");
  });

  let db = open_single_file(&path, options()).expect("open");
  let found = db.node_by_key("racer").is_some();
  close_single_file(db).expect("close");
  assert!(
    found,
    "the create truncated a database created after the existence check"
  );
}

/// `create_pager` claims an empty file, and refuses one that holds data
/// rather than truncate it.
#[test]
fn f3_create_pager_never_truncates_a_file_that_holds_data() {
  let dir = tempdir().expect("tempdir");
  let empty = dir.path().join("empty.kitedb");
  std::fs::write(&empty, b"").expect("empty file");
  create_pager(&empty, PAGE_SIZE).expect("claim an empty file");

  let full = dir.path().join("full.kitedb");
  std::fs::write(&full, b"data").expect("file with data");
  assert!(matches!(
    create_pager(&full, PAGE_SIZE),
    Err(KiteError::CreateFailed(_))
  ));
  assert_eq!(std::fs::read(&full).expect("read"), b"data");
}

/// Until its directory is fsynced, a new file can vanish on power loss.
#[cfg(unix)]
#[test]
fn f3_create_fsyncs_the_parent_directory() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("new.kitedb");
  let (db, synced) = crate::util::fs::dir_syncs_during(|| open_single_file(&path, options()));
  close_single_file(db.expect("open")).expect("close");

  let parent = std::fs::canonicalize(dir.path()).expect("canonical dir");
  assert!(
    synced
      .iter()
      .any(|synced| std::fs::canonicalize(synced).is_ok_and(|synced| synced == parent)),
    "creating a database never fsynced its directory {}; directories synced: {synced:?}",
    parent.display()
  );
}

// ============================================================================
// f4: CRC-valid records that do not parse
// ============================================================================

/// Commit one record of `record_type` whose frame and CRC check but whose
/// payload does not parse as that type's.
fn commit_unparseable_record(db: &SingleFileDB, record_type: WalRecordType) {
  db.begin(false).expect("begin");
  let (txid, tx_handle) = db.require_write_tx_handle().expect("write tx");
  db.write_wal_tx(&tx_handle, WalRecord::new(record_type, txid, vec![0xab; 3]))
    .expect("write record");
  db.commit().expect("commit");
}

#[test]
fn f4_open_fails_on_a_crc_valid_record_that_does_not_parse() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("unparseable.kitedb");
  let db = open_single_file(&path, options()).expect("open");
  commit_nodes(&db, &keys("before", 3));
  commit_unparseable_record(&db, WalRecordType::SetNodeProp);
  commit_nodes(&db, &keys("after", 3));
  close_single_file(db).expect("close");

  for read_only in [true, false] {
    match open_single_file(&path, options().read_only(read_only)) {
      Err(KiteError::InvalidWal(_)) => {}
      Err(error) => panic!("read_only={read_only}: expected InvalidWal, got {error}"),
      Ok(db) => {
        close_single_file(db).expect("close");
        panic!(
          "read_only={read_only}: open replayed the WAL past a CRC-valid record it could not \
           parse, silently skipping it"
        );
      }
    }
  }
}

/// Vector maintenance records (`BatchVectors`, `SealFragment`,
/// `CompactFragments`) are skipped, whatever their payload: no version
/// writes or applies them, and replication skips them too.
#[test]
fn f4_open_skips_vector_maintenance_records() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("vector-maintenance.kitedb");
  let db = open_single_file(&path, options()).expect("open");
  commit_unparseable_record(&db, WalRecordType::SealFragment);
  let committed = keys("after", 3);
  commit_nodes(&db, &committed);
  close_single_file(db).expect("close");

  let db = open_single_file(&path, options()).expect("reopen");
  assert_eq!(missing(&db, &committed), Vec::<&str>::new());
  close_single_file(db).expect("close");
}

// ============================================================================
// f5: WAL positions outside their regions
// ============================================================================

/// Rewrite both header slots of the closed database at `path` with `edit`
/// applied (and valid checksums).
fn rewrite_header(path: &Path, edit: impl FnOnce(&mut DbHeaderV1)) {
  let mut pager = open_pager(path, PAGE_SIZE, false).expect("pager");
  let (mut header, _) = read_header_slots(&mut pager).expect("header");
  edit(&mut header);
  header.change_counter += 1;
  for slot in [HEADER_SLOT_A, HEADER_SLOT_B] {
    write_header_slot(&mut pager, &header, slot).expect("write header slot");
  }
  pager.sync().expect("sync");
}

/// The header checksum protects these fields, so only a writer bug or a
/// crafted file gets them past it.
#[test]
fn f5_open_rejects_wal_positions_outside_their_regions() {
  type Edit = fn(&mut DbHeaderV1);
  let cases: [(&str, Edit); 3] = [
    ("tail past the primary head", |header| {
      header.active_wal_region = 0;
      header.wal_head = 0;
      header.wal_primary_head = 0;
      header.wal_tail = 64;
    }),
    ("secondary head below the secondary region", |header| {
      header.active_wal_region = 1;
      header.wal_tail = 0;
      header.wal_primary_head = 0;
      header.wal_secondary_head = 8;
      header.wal_head = 8;
    }),
    ("active region 2", |header| header.active_wal_region = 2),
  ];

  let dir = tempdir().expect("tempdir");
  let mut accepted = Vec::new();
  for (index, (case, edit)) in cases.into_iter().enumerate() {
    let path = dir.path().join(format!("bad-wal-{index}.kitedb"));
    close_single_file(open_single_file(&path, options()).expect("create")).expect("close");
    rewrite_header(&path, edit);

    for read_only in [true, false] {
      let opened = catch_unwind(AssertUnwindSafe(|| {
        open_single_file(&path, options().read_only(read_only))
      }));
      let outcome = match opened {
        Ok(Err(KiteError::InvalidWal(_))) => continue,
        Ok(Err(error)) => format!("failed with {error} (not InvalidWal)"),
        Err(_) => "panicked".to_string(),
        Ok(Ok(db)) => {
          let stats = catch_unwind(AssertUnwindSafe(|| db.wal_stats().used));
          drop(db);
          match stats {
            Ok(used) => format!("opened (WAL used: {used} bytes)"),
            Err(_) => "opened, then wal_stats() panicked".to_string(),
          }
        }
      };
      accepted.push(format!("{case} (read_only={read_only}): {outcome}"));
    }
  }
  assert!(
    accepted.is_empty(),
    "open accepted invalid WAL positions:\n{}",
    accepted.join("\n")
  );
}

/// Headers from before the region fields name only `wal_head`, a position
/// in a WAL used as one region, possibly past where the primary region now
/// ends. They still load.
#[test]
fn f5_from_header_accepts_a_legacy_single_region_head() {
  let mut header = DbHeaderV1::new(PAGE_SIZE as u32, 16);
  let capacity = 16 * PAGE_SIZE as u64;
  header.wal_head = capacity - 64;
  header.wal_primary_head = 0;
  header.wal_secondary_head = 0;
  let buffer = WalBuffer::from_header(&header).expect("legacy header");
  assert_eq!(buffer.primary_head(), capacity - 64);
}

// ============================================================================
// f6: the checksum's name
// ============================================================================

/// The WAL, header, and snapshot checksums are CRC-32 (IEEE 802.3, via
/// crc32fast), and stay so: changing the polynomial would break every
/// existing file. The docs must say so.
#[test]
fn f6_docs_name_the_checksum_the_code_computes() {
  let docs = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../docs");
  let mut wrong = Vec::new();
  for name in ["ARCHITECTURE.md", "API.md"] {
    // The docs ship with the repository only, not with the crate.
    let Ok(text) = std::fs::read_to_string(docs.join(name)) else {
      continue;
    };
    for (line, content) in text.lines().enumerate() {
      if content.contains("CRC32C") {
        wrong.push(format!("docs/{name}:{}: {}", line + 1, content.trim()));
      }
    }
  }
  assert!(
    wrong.is_empty(),
    "the docs call the CRC-32 (IEEE) checksum CRC32C:\n{}",
    wrong.join("\n")
  );
}

// ============================================================================
// f8: vacuum and WAL resize in SyncMode::Off
// ============================================================================

/// Commits after the last checkpoint live only in the WAL buffer and the
/// in-memory header in `SyncMode::Off`. Vacuum and resize rebuild both from a
/// header, so they must not drop them. Guard: does not reproduce.
#[test]
fn f8_vacuum_and_resize_keep_sync_off_commits() {
  type Operation = fn(&SingleFileDB);
  let operations: [(&str, Operation); 3] = [
    ("vacuum", |db| db.vacuum_single_file(None).expect("vacuum")),
    ("resize_wal", |db| {
      db.resize_wal(512 * 1024, Some(ResizeWalOptions::default()))
        .expect("resize WAL")
    }),
    ("optimize", |db| {
      db.optimize_single_file(None).expect("optimize")
    }),
  ];

  let dir = tempdir().expect("tempdir");
  // Reopens take the WAL size from the header: resize_wal changes it.
  let reopen_options = options().sync_mode(SyncMode::Off);
  let options = reopen_options.clone().wal_size(256 * 1024);
  let mut lost = Vec::new();
  for (name, operation) in operations {
    let path = dir.path().join(format!("sync-off-{name}.kitedb"));
    let db = open_single_file(&path, options.clone()).expect("open");
    let mut committed = keys("first", 50);
    commit_nodes(&db, &committed);
    db.checkpoint().expect("checkpoint");
    // A second checkpoint appends its snapshot after the first's, so vacuum
    // has to move it back next to the WAL.
    let second = keys("second", 50);
    commit_nodes(&db, &second);
    db.checkpoint().expect("checkpoint");
    let wal_only = keys("wal-only", 50);
    commit_nodes(&db, &wal_only);
    assert!(
      db.wal_buffer.lock().has_pending_writes(),
      "{name}: SyncMode::Off flushed the commit; nothing is at stake"
    );
    let header = db.header.read().clone();
    assert_ne!(
      header.snapshot_start_page,
      header.wal_start_page + header.wal_page_count,
      "{name}: the snapshot already lies next to the WAL; vacuum would not move it"
    );

    operation(&db);
    let after = keys("after", 5);
    commit_nodes(&db, &after);
    close_single_file(db).expect("close");

    committed.extend(second);
    committed.extend(wal_only);
    committed.extend(after);
    let db = open_single_file(&path, reopen_options.clone()).expect("reopen");
    let gone = missing(&db, &committed).len();
    close_single_file(db).expect("close");
    if gone > 0 {
      lost.push(format!("{name}: lost {gone} of {} nodes", committed.len()));
    }
  }
  assert!(lost.is_empty(), "{}", lost.join("\n"));
}

// ============================================================================
// f9: dropping a SyncMode::Off database without closing it
// ============================================================================

#[test]
fn f9_drop_without_close_keeps_sync_off_commits() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("sync-off-drop.kitedb");
  let options = options().sync_mode(SyncMode::Off);
  let db = open_single_file(&path, options.clone()).expect("open");
  let committed = keys("node", 20);
  commit_nodes(&db, &committed);
  drop(db);

  let db = open_single_file(&path, options).expect("reopen");
  let gone = missing(&db, &committed).len();
  close_single_file(db).expect("close");
  assert_eq!(
    gone,
    0,
    "dropping the database without close lost {gone} of {} committed nodes",
    committed.len()
  );
}

/// Close persists once; the handle it consumes then drops without writing
/// again.
#[test]
fn f9_drop_after_close_writes_nothing() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("close-then-drop.kitedb");
  let db = open_single_file(&path, options().sync_mode(SyncMode::Off)).expect("open");
  commit_nodes(&db, &keys("node", 3));
  let generation = db.header.read().change_counter;
  close_single_file(db).expect("close");

  let mut pager = open_pager(&path, PAGE_SIZE, true).expect("pager");
  let (header, _) = read_header_slots(&mut pager).expect("header");
  assert_eq!(
    header.change_counter,
    generation + 1,
    "close wrote one header; the drop after it must write none"
  );
}

// ============================================================================
// x: a failed commit's records after an OS crash
// ============================================================================

/// The disk after an OS crash at the end of `events`, from `base` (see
/// [`io_hooks::crash_image`]).
fn crash_image(base: &[u8], events: &[IoEvent]) -> Vec<u8> {
  io_hooks::crash_image(base, events, 2 * PAGE_SIZE as u64)
}

/// Commit `base`; then `failed-a`, whose header write fails (an in-memory
/// header generation of u64::MAX makes `persist_header` fail after the WAL
/// flush, as an I/O error there would), with the next `failing_syncs` syncs
/// failing too; then `b`. Returns whether the crash image (see
/// [`crash_image`]) holds `failed-a`, and `b`.
fn failed_commit_after_os_crash(
  options: SingleFileOpenOptions,
  failing_syncs: usize,
) -> (bool, bool) {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("failed-commit.kitedb");
  let db = open_single_file(&path, options.clone()).expect("open");
  commit_nodes(&db, &["base".to_string()]);
  let base = std::fs::read(&path).expect("base image");

  let ((), events) = io_hooks::record_io_during(|| {
    db.begin(false).expect("begin");
    db.create_node(Some("failed-a")).expect("create node");
    let generation = db.header.read().change_counter;
    db.header.write().change_counter = u64::MAX;
    let failed = io_hooks::with_failing_syncs(failing_syncs, || db.commit());
    db.header.write().change_counter = generation;
    failed.expect_err("the header write was made to fail");
    assert!(db.node_by_key("failed-a").is_none());
    commit_nodes(&db, &["b".to_string()]);
  });
  drop(db);

  let image = dir.path().join("crash-image.kitedb");
  std::fs::write(&image, crash_image(&base, &events)).expect("write image");
  let crashed =
    open_single_file(&image, options.group_commit_enabled(false)).expect("open crash image");
  let found = (
    crashed.node_by_key("failed-a").is_some(),
    crashed.node_by_key("b").is_some(),
  );
  close_single_file(crashed).expect("close");
  found
}

#[test]
fn x_failed_commit_stays_failed_after_an_os_crash() {
  let replayed: Vec<bool> = [false, true]
    .into_iter()
    .filter(|group_commit| {
      let options = options()
        .sync_mode(SyncMode::Normal)
        .group_commit_enabled(*group_commit);
      failed_commit_after_os_crash(options, 0).0
    })
    .collect();
  assert!(
    replayed.is_empty(),
    "a commit that returned Err was replayed (group_commit = {replayed:?}): its COMMIT record \
     stayed in the WAL page the OS wrote back, and the next commit's header named it"
  );
}

/// If the failed record's bytes cannot be made durably dead right away (the
/// sync fails), the next header must still not name them unsynced.
#[test]
fn x_failed_commit_stays_failed_when_its_scrub_cannot_sync() {
  let options = options().sync_mode(SyncMode::Normal);
  let (failed_a, _) = failed_commit_after_os_crash(options, 1);
  assert!(
    !failed_a,
    "a commit that returned Err was replayed after its scrub's sync failed"
  );
}

/// Full mode syncs the WAL before every header, so it was already safe; it
/// must stay so, and keep the acknowledged commit.
#[test]
fn x_full_mode_failed_commit_stays_failed_after_an_os_crash() {
  let (failed_a, b) = failed_commit_after_os_crash(options().sync_mode(SyncMode::Full), 0);
  assert_eq!(
    (failed_a, b),
    (false, true),
    "(failed commit replayed, acknowledged commit kept)"
  );
}

/// A failed commit group's COMMIT records become ROLLBACK records, and the
/// rewrite is synced before any later header can name them: here the
/// rewrite's own sync fails, a transaction appends after the group (the head
/// is not rewound, so the bytes stay where they are), and the next flush
/// syncs.
#[test]
fn x_failed_group_commits_are_rolled_back_durably_before_the_next_header() {
  let dir = tempdir().expect("tempdir");
  let mut pager = create_pager(dir.path().join("rollback.kitedb"), PAGE_SIZE).expect("pager");
  pager.allocate_pages(9).expect("allocate");
  let mut wal = WalBuffer::new(PAGE_SIZE as u64, 8 * PAGE_SIZE as u64, PAGE_SIZE);
  let write = |wal: &mut WalBuffer, record_type: WalRecordType, txid: u64| {
    let payload = match record_type {
      WalRecordType::CreateNode => build_create_node_payload(txid, Some(&format!("n{txid}"))),
      _ => Vec::new(),
    };
    wal
      .write_record(&WalRecord::new(record_type, txid, payload))
      .expect("write");
  };
  for record_type in [
    WalRecordType::Begin,
    WalRecordType::CreateNode,
    WalRecordType::Commit,
  ] {
    write(&mut wal, record_type, 1);
  }
  wal.sync(&mut pager).expect("sync");

  // Transaction 2's group: sealed and written, then its header fails.
  write(&mut wal, WalRecordType::Begin, 2);
  write(&mut wal, WalRecordType::CreateNode, 2);
  let before_commit = wal.head();
  write(&mut wal, WalRecordType::Commit, 2);
  let sealed = wal.seal(false);
  sealed.write(&mut pager).expect("write the group");
  // Transaction 3 appends while the group's I/O runs.
  write(&mut wal, WalRecordType::Begin, 3);
  write(&mut wal, WalRecordType::CreateNode, 3);

  let restored = io_hooks::with_failing_syncs(1, || {
    wal
      .restore_sealed(sealed, [(before_commit, 2)])
      .and_then(|()| wal.flush(&mut pager))
  });
  assert!(restored.is_err(), "the rollback's sync was made to fail");
  assert!(wal.needs_sync());

  write(&mut wal, WalRecordType::Commit, 3);
  let (flushed, syncs) = io_hooks::sync_kinds_during(|| wal.flush(&mut pager));
  flushed.expect("flush");
  assert!(
    !syncs.is_empty(),
    "a flush, after which a header may name the rolled-back group, did not sync"
  );
  assert!(!wal.needs_sync());

  let records = wal.scan_records(&mut pager).expect("scan");
  let committed: Vec<u64> = extract_committed_transactions_in_order(&records)
    .into_iter()
    .map(|(txid, _)| txid)
    .collect();
  assert_eq!(committed, vec![1, 3], "the failed group's commit replays");
  assert!(records
    .iter()
    .any(|record| record.record_type == WalRecordType::Rollback && record.txid == 2));
}
