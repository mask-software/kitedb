//! WAL performance findings from the core audit (raydb-b4 lane `wal-perf`,
//! Oct 2026), each a measurable assertion written to fail before its fix.
//! Test names start with the finding's number:
//!
//! - f1: a commit read the WAL page holding the head back from disk (pending
//!   pages were dropped after every flush), wrote pages one at a time in
//!   HashMap order, and synced the file's metadata (fsync) where its data
//!   (fdatasync) was enough.
//! - f2: `write_wal_tx` encoded every record twice, and kept a copy of every
//!   record in the transaction even when nothing reads it.
//! - f3: scanning a WAL region cost one read per page.
//! - f4: open read the whole WAL area page by page, wherever the head was;
//!   a writable open also checked the primary region twice (record types,
//!   then torn tail).
//! - f5: moving post-cut records back into the primary region (under the
//!   commit lock, at a background checkpoint's install) read every secondary
//!   page, read every primary page it wrote, and wrote them one at a time.

use tempfile::tempdir;

use super::buffer::WalBuffer;
use super::record::{build_create_node_payload, built_bytes_during, WalRecord};
use crate::core::header::read_header_slots;
use crate::core::pager::io_hooks::{self, SyncKind};
use crate::core::pager::{create_pager, open_pager, FilePager};
use crate::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions, SyncMode,
};
use crate::types::WalRecordType;

const PAGE_SIZE: usize = 4096;

fn options() -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .auto_checkpoint(false)
    .background_checkpoint(false)
}

fn commit_node(db: &SingleFileDB, key: &str) {
  db.begin(false).expect("begin");
  db.create_node(Some(key)).expect("create node");
  db.commit().expect("commit");
}

/// A pager with `pages` pages, and a WAL of `wal_pages` pages after its
/// first page.
fn wal_fixture(dir: &tempfile::TempDir, pages: u32, wal_pages: u64) -> (FilePager, WalBuffer) {
  let mut pager = create_pager(dir.path().join("wal.kitedb"), PAGE_SIZE).expect("pager");
  pager.allocate_pages(pages).expect("allocate");
  let wal = WalBuffer::new(PAGE_SIZE as u64, wal_pages * PAGE_SIZE as u64, PAGE_SIZE);
  (pager, wal)
}

/// A CreateNode record of about 1 KiB.
fn big_record(txid: u64) -> WalRecord {
  let key = format!("{txid:04}-{}", "k".repeat(1000));
  WalRecord::new(
    WalRecordType::CreateNode,
    txid,
    build_create_node_payload(txid, Some(&key)),
  )
}

// ============================================================================
// f1: the commit's WAL I/O
// ============================================================================

/// The WAL page holding the head was flushed by the previous commit, so its
/// bytes are known: reading it back costs a system call per commit.
#[test]
fn f1_commit_reads_nothing_back_from_disk() {
  let dir = tempdir().expect("tempdir");
  for sync_mode in [SyncMode::Normal, SyncMode::Full] {
    let path = dir.path().join(format!("f1-reads-{sync_mode:?}.kitedb"));
    let db = open_single_file(&path, options().sync_mode(sync_mode)).expect("open");
    for index in 0..3 {
      commit_node(&db, &format!("warm-{index}"));
    }
    let ((), reads) = io_hooks::reads_during(|| commit_node(&db, "measured"));
    close_single_file(db).expect("close");
    assert_eq!(
      reads, 0,
      "{sync_mode:?}: a one-node commit read {reads} WAL pages back from disk"
    );
  }
}

/// Records appended across several pages are one contiguous byte range: one
/// positioned write, and nothing read first.
#[test]
fn f1_flush_writes_a_contiguous_append_in_one_call() {
  let dir = tempdir().expect("tempdir");
  let (mut pager, mut wal) = wal_fixture(&dir, 40, 32);
  wal.write_record(&big_record(1)).expect("write");
  wal.flush(&mut pager).expect("flush");

  let start = wal.head();
  let ((), syscalls) = io_hooks::syscalls_during(|| {
    for txid in 2..=12 {
      wal.write_record(&big_record(txid)).expect("write");
    }
    wal.flush(&mut pager).expect("flush");
  });
  let pages = (wal.head() - start).div_ceil(PAGE_SIZE as u64) + 1;
  assert_eq!(
    syscalls,
    1,
    "appending {} bytes (spanning up to {pages} pages) and flushing them took {syscalls} \
     system calls",
    wal.head() - start
  );
}

/// The WAL and header pages a commit writes lie inside the file, so its
/// syncs need only make the data durable (fdatasync), not the file's
/// metadata; `full_fsync` keeps F_FULLFSYNC for both (see pager::w2_tests).
#[test]
fn f1_commit_syncs_only_data() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("f1-syncs.kitedb");
  let db = open_single_file(&path, options().sync_mode(SyncMode::Full)).expect("open");
  commit_node(&db, "warm");
  let ((), kinds) = io_hooks::sync_kinds_during(|| commit_node(&db, "measured"));
  close_single_file(db).expect("close");
  assert!(!kinds.is_empty(), "a Full-mode commit synced nothing");
  assert!(
    kinds.iter().all(|kind| *kind == SyncKind::Data),
    "a Full-mode commit, which grows no file, synced the file's metadata: {kinds:?}"
  );
}

/// A data sync is enough only while the file's length is as the last full
/// sync left it; after any length change (WAL or snapshot allocation, a
/// write past the end, truncation) the next one is a full sync.
#[test]
fn f1_data_sync_is_full_after_the_file_length_changes() {
  let dir = tempdir().expect("tempdir");
  let mut pager = create_pager(dir.path().join("lengths.kitedb"), PAGE_SIZE).expect("pager");
  let page = vec![7u8; PAGE_SIZE];
  let sync_kinds =
    |pager: &FilePager| io_hooks::sync_kinds_during(|| pager.sync_data().expect("sync")).1;
  use SyncKind::{Data, Full};

  pager.allocate_pages(4).expect("allocate");
  assert_eq!(sync_kinds(&pager), [Full], "after allocating pages");
  assert_eq!(sync_kinds(&pager), [Data], "with nothing changed since");
  pager.write_page(2, &page).expect("write");
  assert_eq!(sync_kinds(&pager), [Data], "after a write inside the file");
  pager.write_page(9, &page).expect("write");
  assert_eq!(sync_kinds(&pager), [Full], "after a write past its end");
  pager.write_range(100, &[1, 2, 3]).expect("write");
  assert_eq!(sync_kinds(&pager), [Data], "after a range write inside it");
  pager.truncate_pages(3).expect("truncate");
  assert_eq!(sync_kinds(&pager), [Full], "after truncating it");
  pager.allocate_pages(1).expect("allocate");
  pager.sync().expect("full sync");
  assert_eq!(sync_kinds(&pager), [Data], "after a full sync");
}

/// One positioned read for a range; bytes past the end of the file read as
/// zeros.
#[test]
fn f1_read_range_reads_once_and_zero_fills_past_the_end() {
  let dir = tempdir().expect("tempdir");
  let mut pager = create_pager(dir.path().join("range.kitedb"), PAGE_SIZE).expect("pager");
  let data: Vec<u8> = (0..3 * PAGE_SIZE).map(|i| (i % 251) as u8 + 1).collect();
  pager.write_range(0, &data).expect("write");
  let (read, reads) = io_hooks::reads_during(|| pager.read_range(100, 3 * PAGE_SIZE));
  let read = read.expect("read");
  assert_eq!(reads, 1);
  assert_eq!(&read[..3 * PAGE_SIZE - 100], &data[100..]);
  assert!(read[3 * PAGE_SIZE - 100..].iter().all(|byte| *byte == 0));
}

/// Scans see records still buffered as well as flushed ones.
#[test]
fn f1_scan_reads_flushed_and_buffered_records() {
  let dir = tempdir().expect("tempdir");
  let (mut pager, mut wal) = wal_fixture(&dir, 40, 32);
  for txid in 1..=6 {
    wal.write_record(&big_record(txid)).expect("write");
  }
  wal.flush(&mut pager).expect("flush");
  for txid in 7..=9 {
    wal.write_record(&big_record(txid)).expect("write");
  }
  let txids: Vec<u64> = wal
    .scan_region(0, &mut pager)
    .expect("scan")
    .iter()
    .map(|record| record.txid)
    .collect();
  assert_eq!(txids, (1..=9).collect::<Vec<u64>>());
  assert!(wal.has_pending_writes());
}

// ============================================================================
// f2: record encoding and the per-transaction copy
// ============================================================================

/// A transaction's records are encoded (and checksummed) once each, whether
/// it keeps them back until its commit or writes them as it goes.
#[test]
fn f2_write_wal_tx_encodes_each_record_once() {
  let dir = tempdir().expect("tempdir");
  let db = open_single_file(dir.path().join("f2-encode.kitedb"), options()).expect("open");
  for (creates, key_len) in [(10, 8), (40, 1000)] {
    let head = db.wal_stats().head;
    let ((), built) = built_bytes_during(|| {
      db.begin(false).expect("begin");
      for index in 0..creates {
        db.create_node(Some(&format!("node-{index}-{}", "k".repeat(key_len))))
          .expect("create");
      }
      db.commit().expect("commit");
    });
    let written = db.wal_stats().head - head;
    assert_eq!(
      built as u64, written,
      "a transaction of {creates} creates wrote {written} WAL bytes but encoded {built}"
    );
  }
  close_single_file(db).expect("close");
}

/// A transaction keeps a copy of its records only while they wait to be
/// written: a bulk load's (written whole at commit) and those a write
/// transaction keeps back (see `SingleFileTxState::wal_deferred_from`), and
/// for the replication sidecar. Once written, without replication, the copy
/// goes.
#[test]
fn f2_transaction_copies_its_records_only_when_read() {
  let dir = tempdir().expect("tempdir");
  let db = open_single_file(dir.path().join("f2-copy.kitedb"), options()).expect("open");
  db.begin(false).expect("begin");
  let head = db.wal_stats().head;
  let key = |index: usize| format!("plain-{index}-{}", "k".repeat(1000));
  let mut index = 0;
  let (_, tx) = db.require_write_tx_handle().expect("write tx");
  while db.wal_stats().head == head {
    db.create_node(Some(&key(index))).expect("create");
    index += 1;
    assert!(
      index * 1000 <= 2 * crate::core::single_file::WAL_DEFER_BYTES,
      "the records kept back were never written"
    );
  }
  let copied = tx.lock().pending_wal.len();
  db.create_node(Some(&key(index))).expect("create");
  let copied_after = tx.lock().pending_wal.len();
  db.commit().expect("commit");

  db.begin_bulk().expect("begin bulk");
  db.create_node(Some("bulk")).expect("create");
  let (_, tx) = db.require_write_tx_handle().expect("write tx");
  let bulk_copied = tx.lock().pending_wal.len();
  db.commit().expect("commit");
  assert!(db.node_by_key(&key(index)).is_some() && db.node_by_key("bulk").is_some());
  close_single_file(db).expect("close");

  assert!(
    bulk_copied > 0,
    "a bulk load must keep its records to write them at commit"
  );
  assert_eq!(
    (copied, copied_after),
    (0, 0),
    "a transaction without replication or bulk load kept a copy of records it wrote"
  );
}

// ============================================================================
// f3: region scans
// ============================================================================

/// A region's records lie in one byte range: one positioned read.
#[test]
fn f3_scan_region_reads_once() {
  let dir = tempdir().expect("tempdir");
  let (mut pager, mut wal) = wal_fixture(&dir, 40, 32);
  for txid in 1..=12 {
    wal.write_record(&big_record(txid)).expect("write");
  }
  wal.flush(&mut pager).expect("flush");
  let pages = wal.head().div_ceil(PAGE_SIZE as u64);

  let (records, reads) = io_hooks::reads_during(|| wal.scan_region(0, &mut pager));
  let records = records.expect("scan");
  assert_eq!(records.len(), 12);
  assert_eq!(
    reads, 1,
    "scanning {pages} pages of records took {reads} reads"
  );
}

// ============================================================================
// f4: the WAL read at open
// ============================================================================

/// Open reads the live part of the WAL ([tail, head) of each region in use),
/// not the whole WAL area page by page.
#[test]
fn f4_open_reads_only_the_live_wal() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("f4-open.kitedb");
  let options = options().wal_size(4 * 1024 * 1024);
  let db = open_single_file(&path, options.clone()).expect("open");
  for index in 0..8 {
    commit_node(&db, &format!("node-{index}"));
  }
  close_single_file(db).expect("close");

  for read_only in [false, true] {
    let (db, reads) =
      io_hooks::reads_during(|| open_single_file(&path, options.clone().read_only(read_only)));
    let db = db.expect("reopen");
    assert!(db.node_by_key("node-7").is_some());
    if read_only {
      drop(db);
    } else {
      close_single_file(db).expect("close");
    }
    assert!(
      reads <= 16,
      "opening (read_only = {read_only}) a database with 8 small commits in its 1024-page WAL \
       took {reads} reads"
    );
  }
}

/// A writable open reads the live primary region once to check it (records
/// of an unknown type refuse the open, a torn tail is trimmed) and once more
/// to replay it; it read it twice to check it, once per check.
#[test]
fn f4_writable_open_checks_the_wal_in_one_pass() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("f4-one-pass.kitedb");
  let options = options().wal_size(4 * 1024 * 1024);
  let db = open_single_file(&path, options.clone()).expect("open");
  for index in 0..8 {
    commit_node(&db, &format!("node-{index}"));
  }
  close_single_file(db).expect("close");

  let header_reads = {
    let mut pager = open_pager(&path, PAGE_SIZE, true).expect("pager");
    let (slots, reads) = io_hooks::reads_during(|| read_header_slots(&mut pager));
    slots.expect("header slots");
    reads
  };
  let (db, reads) = io_hooks::reads_during(|| open_single_file(&path, options.clone()));
  let db = db.expect("reopen");
  assert!(db.node_by_key("node-7").is_some());
  close_single_file(db).expect("close");
  assert_eq!(
    reads - header_reads,
    2,
    "a writable open read the WAL {} times (after {header_reads} header reads)",
    reads - header_reads
  );
}

/// `check_and_trim` keeps both checks' semantics: a torn tail is trimmed
/// (and the head moves back to the last readable record), and a CRC-valid
/// record of an unknown type, here in the secondary region, refuses.
#[test]
fn f4_check_and_trim_trims_torn_tails_and_refuses_unknown_types() {
  use crate::util::crc::crc32;
  let dir = tempdir().expect("tempdir");
  let (mut pager, mut wal) = wal_fixture(&dir, 40, 32);
  for txid in 1..=4 {
    wal.write_record(&big_record(txid)).expect("write");
  }
  let readable = wal.head();
  wal.write_record(&big_record(5)).expect("write");
  wal.flush(&mut pager).expect("flush");
  // Tear the last record: its tail never reached the disk.
  let torn_at = PAGE_SIZE as u64 + wal.head() - 8;
  pager.write_range(torn_at, &[0xEE; 8]).expect("tear");
  let (trimmed, reads) = io_hooks::reads_during(|| wal.check_and_trim(&mut pager));
  assert!(trimmed.expect("check and trim"), "the torn tail was kept");
  assert_eq!(wal.head(), readable);
  assert_eq!(
    reads, 1,
    "one read of the primary region (the secondary is empty)"
  );

  // A record of a type this version does not know, its CRC valid.
  wal.switch_to_secondary();
  let mut header = crate::types::DbHeaderV1::new(PAGE_SIZE as u32, 32);
  wal.store_in_header(&mut header);
  let mut record = big_record(6).build();
  record[4] = 200;
  let crc_end =
    crate::types::WAL_RECORD_HEADER_SIZE + crate::util::binary::read_u32(&record, 16) as usize;
  let crc = crc32(&record[4..crc_end]) ^ header.wal_secondary_salt;
  record[crc_end..crc_end + 4].copy_from_slice(&crc.to_le_bytes());
  pager
    .write_range(PAGE_SIZE as u64 + wal.head(), &record)
    .expect("write unknown record");
  header.wal_start_page = 1;
  header.wal_secondary_head = wal.head() + record.len() as u64;
  header.wal_head = header.wal_secondary_head;
  let mut reopened = WalBuffer::from_header(&header).expect("from header");
  let refused = reopened.check_and_trim(&mut pager);
  assert!(
    matches!(refused, Err(crate::error::KiteError::InvalidWal(_))),
    "a CRC-valid record of an unknown type was trimmed: {refused:?}"
  );
}

// ============================================================================
// f5: moving post-cut records back into the primary region
// ============================================================================

/// The background install moves the post-cut records to the start of the
/// primary region while holding the commit lock: one read of the secondary
/// region's records and one write of their new copy, whatever their size.
#[test]
fn f5_post_cut_move_back_costs_one_read_and_one_write() {
  let dir = tempdir().expect("tempdir");
  let (mut pager, mut wal) = wal_fixture(&dir, 72, 64);
  for txid in 1..=3 {
    wal.write_record(&big_record(txid)).expect("write");
  }
  wal.switch_to_secondary();
  for txid in 10..=33 {
    wal.write_record(&big_record(txid)).expect("write");
  }
  wal.flush(&mut pager).expect("flush");
  wal.retire_primary_region();
  let moved = wal.used();

  let (compacted, syscalls) =
    io_hooks::syscalls_during(|| wal.compact_secondary_into_primary(&mut pager));
  compacted.expect("compact");
  let txids: Vec<u64> = wal
    .scan_region(0, &mut pager)
    .expect("scan")
    .iter()
    .map(|record| record.txid)
    .collect();
  assert_eq!(txids, (10..=33).collect::<Vec<u64>>());
  assert!(
    syscalls <= 2,
    "moving {moved} bytes of post-cut records back took {syscalls} system calls"
  );
}

/// The install reuses the post-cut records its replay read and checked
/// before taking the commit lock: only records an open transaction appended
/// since are read under it.
#[test]
fn f5_move_back_reads_only_records_appended_since_the_replay() {
  let dir = tempdir().expect("tempdir");
  let (mut pager, mut wal) = wal_fixture(&dir, 72, 64);
  for txid in 1..=3 {
    wal.write_record(&big_record(txid)).expect("write");
  }
  wal.switch_to_secondary();
  for txid in 10..=29 {
    wal.write_record(&big_record(txid)).expect("write");
  }
  wal.flush(&mut pager).expect("flush");
  let (records, read, _) = wal
    .read_region_from(1, 0, &mut pager)
    .expect("replay scan")
    .parse();
  assert_eq!(records.len(), 20);
  for txid in 30..=33 {
    wal.write_record(&big_record(txid)).expect("write");
  }
  wal.flush(&mut pager).expect("flush");
  wal.retire_primary_region();

  let (compacted, reads) =
    io_hooks::reads_during(|| wal.compact_secondary_into_primary_reusing(read, &mut pager));
  compacted.expect("compact");
  let txids: Vec<u64> = wal
    .scan_region(0, &mut pager)
    .expect("scan")
    .iter()
    .map(|record| record.txid)
    .collect();
  assert_eq!(txids, (10..=33).collect::<Vec<u64>>());
  assert_eq!(reads, 1, "only the 4 records appended since need reading");
}
