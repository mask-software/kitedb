//! raydb-b4 `checkpoint-segments`: downgrade safety. Released versions up to
//! v0.2.18 check a database header only by its magic and the checksum of
//! its first 176 bytes; they read neither its format version nor the second
//! header slot. Every file this version writes must fail that check, so an
//! old binary refuses it instead of misreading it. Included from
//! checkpoint.rs for its test hooks.
use super::*;
use crate::core::single_file::{
  close_single_file, open_single_file, SingleFileOpenOptions, SyncMode,
};
use std::path::Path;
use tempfile::tempdir;

/// v0.2.18's magic: "KiteDB format 1\0".
const V0_2_18_MAGIC: [u8; 16] = *b"KiteDB format 1\0";

/// v0.2.18's whole acceptance check of a database header page, frozen from
/// `DbHeaderV1::parse` at tag v0.2.18 (ray-rs/src/core/header.rs): the
/// first 16 bytes are its magic, and the CRC-32 of bytes 0..176 (its
/// `crc32c`, crc32fast's IEEE CRC-32) matches the little-endian u32 at 176.
/// It reads page 0 only.
fn v0_2_18_accepts(page: &[u8]) -> bool {
  if page.len() < 180 || page[0..16] != V0_2_18_MAGIC {
    return false;
  }
  let stored = u32::from_le_bytes(page[176..180].try_into().expect("four bytes"));
  let mut hasher = crc32fast::Hasher::new();
  hasher.update(&page[0..176]);
  hasher.finalize() == stored
}

/// Both header pages of the file at `path`, as v0.2.18 would see them:
/// whether it accepts each.
fn accepted_slots(path: &Path) -> [bool; 2] {
  let bytes = std::fs::read(path).expect("read the file");
  [
    v0_2_18_accepts(&bytes[0..4096]),
    v0_2_18_accepts(&bytes[4096..8192]),
  ]
}

fn assert_refused(path: &Path, state: &str) {
  assert_eq!(
    accepted_slots(path),
    [false, false],
    "v0.2.18 accepts a header slot of a file this version wrote ({state}): it would misread it"
  );
}

fn key(index: usize) -> String {
  format!("node-{index:05}-{}", "n".repeat(200))
}

fn commit_keys(db: &SingleFileDB, from: usize, count: usize) {
  for index in from..from + count {
    db.begin(false).expect("begin");
    db.create_node(Some(&key(index))).expect("node");
    db.commit().expect("commit");
  }
}

fn options() -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .wal_size(64 * 1024)
    .sync_mode(SyncMode::Normal)
    .auto_checkpoint(false)
}

/// Every file this version writes, at rest or open, fails v0.2.18's header
/// check: fresh (open and closed), version 2 at rest (closed), version 3
/// with WAL segments (open, and dropped without closing), after a
/// checkpoint, and after a reopen.
#[test]
fn v0_2_18_refuses_every_file_this_version_writes() {
  let dir = tempdir().expect("tempdir");

  let fresh = dir.path().join("fresh.kitedb");
  let db = open_single_file(&fresh, options()).expect("create");
  assert_refused(&fresh, "fresh, open");
  close_single_file(db).expect("close");
  assert_refused(&fresh, "fresh, closed");

  let at_rest = dir.path().join("at-rest.kitedb");
  let db = open_single_file(&at_rest, options()).expect("create");
  commit_keys(&db, 0, 50);
  assert_refused(&at_rest, "version 2, open with commits in the WAL");
  db.checkpoint().expect("checkpoint");
  assert_refused(&at_rest, "after a checkpoint");
  close_single_file(db).expect("close");
  assert_refused(&at_rest, "version 2 at rest, closed");

  let with_segments = dir.path().join("with-segments.kitedb");
  let db = open_single_file(&with_segments, options()).expect("create");
  commit_keys(&db, 0, 400);
  assert!(
    wal_segment_test_stats(&db).live > 0,
    "setup: the WAL never spilled"
  );
  assert_eq!(
    db.header.read().written_versions().0,
    3,
    "setup: not version 3"
  );
  assert_refused(&with_segments, "version 3 with WAL segments, open");
  drop(db);
  assert_refused(&with_segments, "version 3 with WAL segments, dropped");
  let db = open_single_file(&with_segments, options()).expect("reopen");
  assert_refused(&with_segments, "reopened");
  close_single_file(db).expect("close");
  assert_refused(&with_segments, "closed after a reopen");
}

/// A dual-header file in the old magic (written by an unreleased build
/// before this one: version 1 or 2) still opens, read-only without writing
/// anything, and a writable open upgrades both header slots at once, before
/// any other write, so an old binary refuses the file from then on.
#[test]
fn old_magic_files_open_and_upgrade_both_slots() {
  for fixture in ["wal_format_v1.kitedb", "v2_wal_records.kitedb"] {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join(fixture);
    std::fs::copy(
      Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(fixture),
      &path,
    )
    .expect("copy the fixture");
    assert_eq!(
      accepted_slots(&path),
      [true, true],
      "setup: {fixture} is not in the old magic"
    );
    let before = std::fs::read(&path).expect("read");

    let read_only =
      open_single_file(&path, SingleFileOpenOptions::new().read_only(true)).expect("read-only");
    let nodes = read_only.count_nodes();
    drop(read_only);
    assert_eq!(
      std::fs::read(&path).expect("read"),
      before,
      "{fixture}: a read-only open wrote to the file"
    );

    let db = open_single_file(&path, SingleFileOpenOptions::new()).expect("writable open");
    assert_refused(&path, &format!("{fixture} right after a writable open"));
    assert_eq!(db.count_nodes(), nodes, "{fixture}: the upgrade lost nodes");
    close_single_file(db).expect("close");
    assert_refused(&path, &format!("{fixture} closed"));
    let db = open_single_file(&path, SingleFileOpenOptions::new()).expect("reopen");
    assert_eq!(db.count_nodes(), nodes, "{fixture}: reopened");
  }
}
