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
//! - Format 1 -> 2 migration (W2's salts): a format-1 file written before
//!   salts existed (`tests/fixtures/wal_format_v1.kitedb`) opens, replays its
//!   unsalted WAL, and is salted from its next WAL reset on.
//!
//! W7 (fsync) needs a crate-internal probe; see `w2_tests` in `core/pager.rs`.

use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

use kitedb::constants::{DB_FLAG_ENCRYPTED, MIN_READER_SINGLE_FILE, VERSION_SINGLE_FILE};
use kitedb::core::single_file::{open_single_file, SingleFileDB, SingleFileOpenOptions};
use kitedb::core::wal::record::{
  build_begin_payload, build_commit_payload, build_create_node_payload, parse_wal_record, WalRecord,
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
///
/// Records are now written only over zeros a sync made durable first (see
/// `WalBuffer`'s "Zeros ahead"), so CCCC's commit erases BBBB's bytes before
/// it writes; the image puts them back, as they lay after the reset, to check
/// that the salt alone still rejects them.
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
  let reset = fs::read(&path).expect("read");
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
    reset[stale.clone()].iter().any(|byte| *byte != 0),
    "precondition: BBBB's records are still in the WAL after the reset"
  );

  // Crash during DDDD's commit sync: its header page is durable, its WAL page
  // is not (the WAL holds what it held after CCCC, with BBBB's bytes as the
  // reset left them).
  let mut image = after.clone();
  image[wal.clone()].copy_from_slice(&acked[wal]);
  image[stale.clone()].copy_from_slice(&reset[stale]);

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

/// That newer version declares this build a capable reader, so a read-only
/// open works and changes nothing.
#[test]
fn w8_newer_format_version_opens_read_only() {
  let dir = tempdir().expect("tempdir");
  let path = closed_db_with_a_node(dir.path(), "w8-newer-version-ro.kitedb");
  let image = edit_header(&path, |header| {
    header.version = VERSION_SINGLE_FILE + 1;
    header.min_reader_version = MIN_READER_SINGLE_FILE;
  });
  let db = open_single_file(&path, options().read_only(true)).expect("read-only open");
  assert!(db.node_by_key("existing").is_some());
  drop(db);
  assert!(
    fs::read(&path).expect("read") == image,
    "read-only open modified the file"
  );
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

// ============================================================================
// Format 1 -> 2 migration
// ============================================================================

fn fixture_path(name: &str) -> PathBuf {
  PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    .join("tests/fixtures")
    .join(name)
}

/// A copy of `wal_format_v1.kitedb`, written by the format-1 writer (commit
/// bf147f6, before WAL salts) with a 64 KiB WAL and auto-checkpoint off:
/// - checkpointed: label Person, edge type KNOWS, property keys name and
///   note; alice (1, Person, name "Alice", note "snapshot") and bob (2, name
///   "Bob"); alice -KNOWS-> bob.
/// - WAL only, unsalted: tx A creates carol (3), sets alice's note to
///   "v1-wal-first", and adds bob -KNOWS-> carol; tx B sets alice's note to
///   "v1-wal-second" and carol's name to "Carol".
fn v1_fixture_copy(dir: &Path, name: &str) -> PathBuf {
  let path = dir.join(name);
  fs::copy(fixture_path("wal_format_v1.kitedb"), &path).expect("copy fixture");
  path
}

fn assert_v1_fixture_state(db: &SingleFileDB, what: &str) {
  let note = db.propkey_id("note").expect("note key");
  let name = db.propkey_id("name").expect("name key");
  let knows = db.etype_id("KNOWS").expect("KNOWS");
  let person = db.label_id("Person").expect("Person");
  let alice = db.node_by_key("alice").expect("alice");
  let bob = db.node_by_key("bob").expect("bob");
  let carol = db.node_by_key("carol");
  assert_eq!((alice, bob, carol), (1, 2, Some(3)), "{what}: node ids");
  assert_eq!(
    string_prop(db, alice, note).as_deref(),
    Some("v1-wal-second"),
    "{what}: alice's note"
  );
  assert_eq!(
    string_prop(db, alice, name).as_deref(),
    Some("Alice"),
    "{what}"
  );
  assert_eq!(string_prop(db, 3, name).as_deref(), Some("Carol"), "{what}");
  assert!(db.node_labels(alice).contains(&person), "{what}: label");
  assert!(db.edge_exists(alice, knows, bob), "{what}: snapshot edge");
  assert!(db.edge_exists(bob, knows, 3), "{what}: WAL edge");
}

/// A format-1 file opens read-only without being written, and writable with
/// its unsalted WAL replayed and appended to, unsalted, in the same cycle;
/// its next checkpoint (a WAL reset) moves it to the salted format 2.
#[test]
fn format_1_file_replays_its_wal_and_upgrades_at_the_next_reset() {
  let dir = tempdir().expect("tempdir");
  let path = v1_fixture_copy(dir.path(), "v1.kitedb");
  let original = fs::read(&path).expect("read");
  let v1 = newest_header(&original);
  assert_eq!(
    (
      v1.version,
      v1.min_reader_version,
      v1.wal_primary_salt,
      v1.wal_secondary_salt
    ),
    (1, 1, 0, 0),
    "precondition: a format-1 header"
  );
  assert!(v1.wal_head > 0, "precondition: WAL-only commits");

  let db = open_single_file(&path, options().read_only(true)).expect("read-only open");
  assert_v1_fixture_state(&db, "read-only");
  drop(db);
  assert!(
    fs::read(&path).expect("read") == original,
    "read-only open wrote the file"
  );

  // Unsalted records are not followed by salted ones in the same WAL cycle.
  let db = open_single_file(&path, options()).expect("writable open");
  assert_v1_fixture_state(&db, "writable");
  commit_node(&db, "v1-cycle");
  let header = newest_header(&fs::read(&path).expect("read"));
  assert_eq!((header.version, header.wal_primary_salt), (1, 0));
  drop(db);

  let db = open_single_file(&path, options()).expect("reopen");
  assert_v1_fixture_state(&db, "reopened");
  assert!(db.node_by_key("v1-cycle").is_some());
  db.checkpoint().expect("checkpoint");
  let header = newest_header(&fs::read(&path).expect("read"));
  assert_eq!(
    (header.version, header.min_reader_version),
    (VERSION_SINGLE_FILE, MIN_READER_SINGLE_FILE),
    "the WAL reset upgrades the header"
  );
  assert_ne!(header.wal_primary_salt, 0);
  commit_node(&db, "salted");
  drop(db);

  for read_only in [true, false] {
    let db = open_single_file(&path, options().read_only(read_only)).expect("open upgraded");
    assert_v1_fixture_state(&db, &format!("upgraded, read_only={read_only}"));
    for key in ["v1-cycle", "salted"] {
      assert!(
        db.node_by_key(key).is_some(),
        "read_only={read_only}: {key} lost"
      );
    }
  }
}

/// The upgrade's reset leaves the format-1 records in place after the WAL
/// head. A crash that leaves a header naming those bytes (the next commit's
/// header landed, its WAL page did not) must not replay them: tx A would set
/// alice's note back to "v1-wal-first".
#[test]
fn format_1_records_do_not_replay_after_the_upgrade() {
  let dir = tempdir().expect("tempdir");
  let path = v1_fixture_copy(dir.path(), "v1-stale.kitedb");
  let db = open_single_file(&path, options()).expect("writable open");
  db.checkpoint().expect("checkpoint");
  drop(db);

  let mut image = fs::read(&path).expect("read");
  let mut header = newest_header(&image);
  assert_eq!(header.version, VERSION_SINGLE_FILE);
  assert_eq!(header.wal_head, 0);
  let wal = &image[wal_area(&header)];
  let mut tx_a_end = 0;
  while let Some(record) = parse_wal_record(wal, tx_a_end) {
    tx_a_end = record.record_end;
    if record.record_type == WalRecordType::Commit {
      break;
    }
  }
  assert!(
    tx_a_end > 0,
    "precondition: tx A's unsalted records are in place"
  );
  header.wal_head = tx_a_end as u64;
  header.wal_primary_head = tx_a_end as u64;
  header.change_counter += 1;
  install_header_in_both_slots(&mut image, &header);

  for read_only in [true, false] {
    let crashed = write_image(
      dir.path(),
      &format!("v1-stale-ro{read_only}.kitedb"),
      &image,
    );
    let db = open_single_file(&crashed, options().read_only(read_only)).expect("open crash image");
    assert_v1_fixture_state(&db, &format!("crash image, read_only={read_only}"));
  }
}
