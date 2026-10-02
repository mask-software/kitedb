//! Wave-2 `wal-format` lane: reproductions of the WAL format and header
//! findings, written against the public API. Crashes are simulated on file
//! images: a copy taken while the database is open, then pages swapped in from
//! an earlier copy (a page write that never reached the disk) or header slots
//! rewritten with crafted fields (both slots, valid checksums).
//!
//! - W2: WAL records carry no per-reset salt, so committed records of an
//!   earlier WAL cycle right after the head replay as if they were new.
//! - W3 (guard): a commit made after reopening past unwritten WAL bytes is
//!   replayed (the WAL head is trimmed at open).
//! - W4 (guard): an unfinished background-checkpoint cut whose records don't
//!   fit merged into the primary region still opens.
//! - W5 (guard): an empty-primary cut header doesn't make replay scan the
//!   primary region's stale bytes.
//! - W8: header `version`, `min_reader_version`, and `flags` are checked
//!   against what this build supports.
//! - W9 (medium): the read-only WAL scan bounds each record by the WAL head.
//!
//! W7 (fsync) needs a crate-internal probe; see `w2_tests` in `core/pager.rs`.

use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

use kitedb::constants::{DB_FLAG_ENCRYPTED, MIN_READER_SINGLE_FILE, VERSION_SINGLE_FILE};
use kitedb::core::single_file::{open_single_file, SingleFileDB, SingleFileOpenOptions};
use kitedb::core::wal::record::{
  build_begin_payload, build_commit_payload, build_create_node_payload, WalRecord,
};
use kitedb::types::{DbHeaderV1, NodeId, PropKeyId, PropValue, WalRecordType};
use tempfile::tempdir;

// ============================================================================
// Helpers
// ============================================================================

const HEADER_SLOTS: usize = 2;
/// Primary region share of the WAL (`PRIMARY_REGION_RATIO` in wal/buffer.rs).
const PRIMARY_REGION_RATIO: f64 = 0.75;

fn options() -> SingleFileOpenOptions {
  SingleFileOpenOptions::new().auto_checkpoint(false)
}

fn commit_node(db: &SingleFileDB, key: &str) {
  db.begin(false).expect("begin");
  db.create_node(Some(key)).expect("create node");
  db.commit().expect("commit");
}

fn set_prop(db: &SingleFileDB, node: NodeId, key: PropKeyId, value: &str) {
  db.begin(false).expect("begin");
  db.set_node_prop(node, key, PropValue::String(value.to_string()))
    .expect("set prop");
  db.commit().expect("commit");
}

fn string_prop(db: &SingleFileDB, node: NodeId, key: PropKeyId) -> Option<String> {
  match db.node_prop(node, key) {
    Some(PropValue::String(value)) => Some(value),
    None => None,
    other => panic!("unexpected property value {other:?}"),
  }
}

fn page_size_of(image: &[u8]) -> usize {
  u32::from_le_bytes(image[16..20].try_into().unwrap()) as usize
}

/// The header open would select: the newest valid slot.
fn newest_header(image: &[u8]) -> DbHeaderV1 {
  let page_size = page_size_of(image);
  (0..HEADER_SLOTS)
    .filter_map(|slot| DbHeaderV1::parse(&image[slot * page_size..(slot + 1) * page_size]).ok())
    .max_by_key(|header| header.change_counter)
    .expect("a valid header slot")
}

/// Write `header` (checksummed) into both slots of `image`.
fn install_header_in_both_slots(image: &mut [u8], header: &DbHeaderV1) {
  let page = header.serialize_to_page();
  let page_size = page.len();
  for slot in 0..HEADER_SLOTS {
    image[slot * page_size..(slot + 1) * page_size].copy_from_slice(&page);
  }
}

/// Byte range of the WAL area in the file.
fn wal_area(header: &DbHeaderV1) -> Range<usize> {
  let page_size = header.page_size as usize;
  let start = header.wal_start_page as usize * page_size;
  start..start + header.wal_page_count as usize * page_size
}

fn primary_region_size(header: &DbHeaderV1) -> u64 {
  let capacity = header.wal_page_count * header.page_size as u64;
  (capacity as f64 * PRIMARY_REGION_RATIO) as u64
}

fn write_image(dir: &Path, name: &str, image: &[u8]) -> PathBuf {
  let path = dir.join(name);
  fs::write(&path, image).expect("write crash image");
  path
}

/// A small database with one committed node, closed.
fn closed_db_with_a_node(dir: &Path, name: &str) -> PathBuf {
  let path = dir.join(name);
  let db = open_single_file(&path, options()).expect("create");
  commit_node(&db, "existing");
  drop(db);
  path
}

/// Rewrite both header slots of the file at `path` through `edit`.
fn edit_header(path: &Path, edit: impl FnOnce(&mut DbHeaderV1)) -> Vec<u8> {
  let mut image = fs::read(path).expect("read db");
  let mut header = newest_header(&image);
  edit(&mut header);
  header.change_counter += 1;
  install_header_in_both_slots(&mut image, &header);
  fs::write(path, &image).expect("write db");
  image
}

// ============================================================================
// W2: stale records of an earlier WAL cycle
// ============================================================================

/// A checkpoint rewinds the WAL head to 0 but leaves the old records' bytes in
/// place, and a record's CRC covers only its own bytes, not anything that
/// changes per WAL reset. So when a crash leaves a durable header naming WAL
/// bytes whose page write never landed (one sync covers both; the drive may
/// persist the header page first), recovery reads the previous cycle's
/// committed BEGIN/SET/COMMIT records there, and replays them after the newer
/// acknowledged commit.
#[test]
fn w2_stale_records_of_an_earlier_wal_cycle_are_not_replayed() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("w2-stale.kitedb");
  let db = open_single_file(&path, options()).expect("open");
  db.begin(false).expect("begin");
  let node = db.create_node(Some("n")).expect("node");
  let key = db.define_propkey("p").expect("propkey");
  db.commit().expect("commit");
  db.checkpoint().expect("checkpoint 0");

  // Cycle 1: two same-shape transactions, then a WAL reset.
  set_prop(&db, node, key, "AAAA");
  set_prop(&db, node, key, "BBBB");
  db.checkpoint().expect("checkpoint 1");
  // Cycle 2: CCCC overwrites AAAA's bytes; BBBB's records stay right after.
  set_prop(&db, node, key, "CCCC");
  let acked = fs::read(&path).expect("read");
  let tx_len = newest_header(&acked).wal_head;
  // The next commit's records land exactly over BBBB's.
  set_prop(&db, node, key, "DDDD");
  let after = fs::read(&path).expect("read");
  drop(db);

  let header = newest_header(&after);
  assert_eq!(header.wal_tail, 0);
  assert_eq!(
    header.wal_head,
    2 * tx_len,
    "precondition: CCCC and DDDD transactions have the same size"
  );
  let wal = wal_area(&header);
  let stale = wal.start + tx_len as usize..wal.start + 2 * tx_len as usize;
  assert!(
    acked[stale].iter().any(|byte| *byte != 0),
    "precondition: BBBB's records are still in the WAL after CCCC"
  );

  // Crash during DDDD's commit sync: its header page is durable, its WAL page
  // is not (the WAL holds what it held after CCCC).
  let mut image = after.clone();
  image[wal.clone()].copy_from_slice(&acked[wal]);

  let mut recovered = Vec::new();
  for read_only in [true, false] {
    let crashed = write_image(
      dir.path(),
      &format!("w2-stale-ro{read_only}.kitedb"),
      &image,
    );
    let db = open_single_file(&crashed, options().read_only(read_only)).expect("open crash image");
    recovered.push((read_only, string_prop(&db, node, key)));
  }
  assert_eq!(
    recovered,
    vec![(true, Some("CCCC".into())), (false, Some("CCCC".into()))],
    "(read_only, recovered value): a committed transaction of the previous WAL cycle (BBBB) \
     was replayed over the acknowledged CCCC"
  );
}

// ============================================================================
// W3 (guard): head trimmed at open
// ============================================================================

/// A durable header names WAL bytes that never landed (zeros here). Replay
/// stops there, so the writable open must move the head back before the next
/// commit appends, or that acknowledged commit lands out of replay's reach.
#[test]
fn w3_commit_after_reopening_past_unwritten_wal_bytes_survives() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("w3-hole.kitedb");
  let db = open_single_file(&path, options()).expect("open");
  commit_node(&db, "a");
  commit_node(&db, "b");
  let acked = fs::read(&path).expect("read");
  commit_node(&db, "lost");
  let after = fs::read(&path).expect("read");
  drop(db);

  let header = newest_header(&after);
  assert!(header.wal_head > newest_header(&acked).wal_head);
  let wal = wal_area(&header);
  let mut image = after.clone();
  image[wal.clone()].copy_from_slice(&acked[wal]);
  let crashed = write_image(dir.path(), "w3-hole-crash.kitedb", &image);

  let db = open_single_file(&crashed, options()).expect("open crash image");
  assert!(db.node_by_key("a").is_some() && db.node_by_key("b").is_some());
  assert!(db.node_by_key("lost").is_none());
  commit_node(&db, "after-reopen"); // SyncMode::Full: durable on return
  drop(db); // crash

  for read_only in [true, false] {
    let db = open_single_file(&crashed, options().read_only(read_only)).expect("reopen");
    for key in ["a", "b", "after-reopen"] {
      assert!(
        db.node_by_key(key).is_some(),
        "read_only={read_only}: acknowledged node {key:?} lost"
      );
    }
  }
}

// ============================================================================
// W4 (guard): an unfinished cut too large to merge
// ============================================================================

/// A crash image of a background checkpoint's cut: the primary region is
/// mostly full and the secondary region holds more post-cut commits than the
/// primary region has room for. Open must not fail half-way through merging
/// them (leaving the file unopenable), and must not persist a partial merge.
#[test]
fn w4_unfinished_cut_too_large_to_merge_still_opens() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("w4-cut.kitedb");
  let options = options().wal_size(64 * 1024);
  let db = open_single_file(&path, options.clone()).expect("open");
  let primary_size = {
    let image = fs::read(&path).expect("read");
    primary_region_size(&newest_header(&image))
  };
  let mut primary_keys = Vec::new();
  while db.wal_stats().primary_head * 10 < primary_size * 9 {
    let key = format!("primary-{}", primary_keys.len());
    commit_node(&db, &key);
    primary_keys.push(key);
  }
  drop(db);

  let mut image = fs::read(&path).expect("read");
  let mut header = newest_header(&image);
  let capacity = header.wal_page_count * header.page_size as u64;
  let secondary_start = primary_size;
  let mut secondary = Vec::new();
  let mut secondary_keys = Vec::new();
  let mut txid = header.next_tx_id;
  let mut node_id = header.max_node_id + 1;
  loop {
    let key = format!("secondary-{}", secondary_keys.len());
    let mut tx = WalRecord::new(WalRecordType::Begin, txid, build_begin_payload()).build();
    tx.extend(
      WalRecord::new(
        WalRecordType::CreateNode,
        txid,
        build_create_node_payload(node_id, Some(&key)),
      )
      .build(),
    );
    tx.extend(WalRecord::new(WalRecordType::Commit, txid, build_commit_payload()).build());
    if secondary_start + (secondary.len() + tx.len()) as u64 > capacity {
      break;
    }
    secondary.extend(tx);
    secondary_keys.push(key);
    txid += 1;
    node_id += 1;
  }
  assert!(
    header.wal_primary_head + secondary.len() as u64 > primary_size,
    "precondition: the cut's records don't fit merged into the primary region"
  );

  let wal = wal_area(&header);
  let at = wal.start + secondary_start as usize;
  image[at..at + secondary.len()].copy_from_slice(&secondary);
  header.active_wal_region = 1;
  header.wal_secondary_head = secondary_start + secondary.len() as u64;
  header.wal_head = header.wal_secondary_head;
  header.checkpoint_in_progress = 1;
  header.next_tx_id = txid;
  header.max_node_id = node_id - 1;
  header.change_counter += 1;
  install_header_in_both_slots(&mut image, &header);
  let crashed = write_image(dir.path(), "w4-cut-crash.kitedb", &image);

  let all_keys: Vec<&String> = primary_keys.iter().chain(&secondary_keys).collect();
  let assert_all = |db: &SingleFileDB, what: &str| {
    for key in &all_keys {
      assert!(db.node_by_key(key).is_some(), "{what}: node {key:?} lost");
    }
  };
  // Twice: the first writable open must leave the file openable.
  for (round, read_only) in [(1, false), (2, false), (3, true)] {
    let db = open_single_file(&crashed, options.clone().read_only(read_only))
      .unwrap_or_else(|error| panic!("open {round} (read_only={read_only}) failed: {error:?}"));
    assert_all(&db, &format!("open {round}"));
  }
  let db = open_single_file(&crashed, options.clone()).expect("open to checkpoint");
  db.checkpoint().expect("checkpoint finishes the cut");
  drop(db);
  let db = open_single_file(&crashed, options).expect("open after checkpoint");
  assert_all(&db, "after checkpoint");
}

// ============================================================================
// W5 (guard): empty-primary cut header
// ============================================================================

/// The marker header of a background cut taken while the primary region was
/// empty: `wal_primary_head == 0`, `wal_head` at the secondary region's start.
/// The legacy fallback (`primary_head = wal_head` when it is 0) would scan the
/// whole primary region, which still holds earlier cycles' records.
#[test]
fn w5_empty_primary_cut_header_does_not_replay_stale_primary_bytes() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("w5-cut.kitedb");
  let db = open_single_file(&path, options()).expect("open");
  db.begin(false).expect("begin");
  let node = db.create_node(Some("n")).expect("node");
  let key = db.define_propkey("p").expect("propkey");
  db.commit().expect("commit");
  db.checkpoint().expect("checkpoint 0");
  set_prop(&db, node, key, "AAAA");
  set_prop(&db, node, key, "BBBB");
  db.checkpoint().expect("checkpoint 1");
  set_prop(&db, node, key, "CCCC");
  db.checkpoint().expect("checkpoint 2");
  assert_eq!(db.wal_stats().primary_head, 0);
  drop(db);

  let mut image = fs::read(&path).expect("read");
  let mut header = newest_header(&image);
  let secondary_start = primary_region_size(&header);
  header.active_wal_region = 1;
  header.wal_primary_head = 0;
  header.wal_tail = 0;
  header.wal_head = secondary_start;
  header.wal_secondary_head = secondary_start;
  header.checkpoint_in_progress = 1;
  header.change_counter += 1;
  install_header_in_both_slots(&mut image, &header);

  for read_only in [true, false] {
    let crashed = write_image(dir.path(), &format!("w5-cut-ro{read_only}.kitedb"), &image);
    let db = open_single_file(&crashed, options().read_only(read_only)).expect("open cut image");
    assert_eq!(
      string_prop(&db, node, key).as_deref(),
      Some("CCCC"),
      "read_only={read_only}: stale primary-region records replayed over the snapshot"
    );
  }
}

// ============================================================================
// W8: header version, min_reader_version, and flags
// ============================================================================

/// Open the file at `path` writable and try to write to it. Returns the open
/// error, or panics if the open succeeds and the write lands.
fn assert_not_written_by_this_build(path: &Path, what: &str) {
  let before = fs::read(path).expect("read");
  match open_single_file(path, options()) {
    Err(_) => {}
    Ok(db) => {
      let wrote = db.begin(false).is_ok()
        && db.create_node(Some("written-by-old-build")).is_ok()
        && db.commit().is_ok();
      drop(db);
      assert!(
        !wrote,
        "{what}: this build opened the file writable and committed to it"
      );
    }
  }
  assert!(
    fs::read(path).expect("read") == before,
    "{what}: this build modified the file"
  );
}

/// A file whose writer requires a newer reader (`min_reader_version` above
/// this build's format version) must be refused, writable or read-only.
#[test]
fn w8_open_rejects_a_header_requiring_a_newer_reader() {
  let dir = tempdir().expect("tempdir");
  let path = closed_db_with_a_node(dir.path(), "w8-min-reader.kitedb");
  edit_header(&path, |header| {
    header.version = VERSION_SINGLE_FILE + 1;
    header.min_reader_version = VERSION_SINGLE_FILE + 1;
  });

  for read_only in [false, true] {
    let opened = open_single_file(&path, options().read_only(read_only));
    assert!(
      opened.is_err(),
      "read_only={read_only}: opened a file with min_reader_version {} (this build reads up to \
       format {VERSION_SINGLE_FILE})",
      VERSION_SINGLE_FILE + 1
    );
  }
  assert_not_written_by_this_build(&path, "min_reader_version too high");
}

/// Flags this build doesn't implement change how the file must be read
/// (`DB_FLAG_ENCRYPTED`) or mean something only a newer build knows. Opening
/// such a file would misread it, and writing it would corrupt it.
#[test]
fn w8_open_rejects_unknown_header_flags() {
  let dir = tempdir().expect("tempdir");
  for (name, flag) in [
    ("encrypted", DB_FLAG_ENCRYPTED),
    ("unknown-bit-31", 1u32 << 31),
  ] {
    let path = closed_db_with_a_node(dir.path(), &format!("w8-flag-{name}.kitedb"));
    edit_header(&path, |header| header.flags |= flag);

    for read_only in [false, true] {
      let opened = open_single_file(&path, options().read_only(read_only));
      assert!(
        opened.is_err(),
        "read_only={read_only}: opened a file with unsupported header flag {name} (0x{flag:08X})"
      );
    }
    assert_not_written_by_this_build(&path, &format!("flag {name}"));
  }
}

/// A newer format version that older readers may still read
/// (`min_reader_version` within range) must not be written by this build:
/// open read-only, or refuse.
#[test]
fn w8_newer_format_version_is_not_written() {
  let dir = tempdir().expect("tempdir");
  let path = closed_db_with_a_node(dir.path(), "w8-newer-version.kitedb");
  edit_header(&path, |header| {
    header.version = VERSION_SINGLE_FILE + 1;
    header.min_reader_version = MIN_READER_SINGLE_FILE;
  });
  assert_not_written_by_this_build(&path, "newer format version");
}

// ============================================================================
// W9 (medium): WAL scan bounded by the head
// ============================================================================

/// A header whose WAL head falls inside the last transaction's COMMIT record.
/// The writable open scans up to the head only (the COMMIT is cut off, so the
/// transaction is not committed), while the read-only scan parses records
/// against the whole WAL area and accepts the COMMIT that runs past the head.
#[test]
fn w9_read_only_replay_stops_at_a_record_crossing_the_wal_head() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("w9-cut-record.kitedb");
  let db = open_single_file(&path, options()).expect("open");
  commit_node(&db, "a");
  commit_node(&db, "b");
  drop(db);
  // The COMMIT record (no payload) is the last 24 bytes; cut it in half.
  let image = edit_header(&path, |header| {
    header.wal_head -= 8;
    header.wal_primary_head = header.wal_head;
  });

  let mut seen = Vec::new();
  for read_only in [true, false] {
    let copy = write_image(dir.path(), &format!("w9-ro{read_only}.kitedb"), &image);
    let db = open_single_file(&copy, options().read_only(read_only)).expect("open");
    assert!(db.node_by_key("a").is_some());
    seen.push((read_only, db.node_by_key("b").is_some()));
  }
  assert_eq!(
    seen,
    vec![(true, false), (false, false)],
    "(read_only, transaction with a COMMIT past the WAL head replayed)"
  );
}
