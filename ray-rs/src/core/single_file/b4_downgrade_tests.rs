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
/// anything, and a writable open upgrades both header slots before it
/// returns, so an old binary refuses the file from then on.
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

/// An open that refuses a file in the old magic (here for a WAL size the
/// options require and the file does not have) writes nothing to it: the
/// upgrade of its header slots waits for the checks that may refuse it.
#[test]
fn a_refused_open_of_an_old_magic_file_writes_nothing() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("refused.kitedb");
  std::fs::copy(
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v2_wal_records.kitedb"),
    &path,
  )
  .expect("copy the fixture");
  let before = std::fs::read(&path).expect("read");
  let wal_pages = u64::from_le_bytes(before[72..80].try_into().expect("eight bytes"));
  let wrong = (wal_pages as usize + 16) * 4096;
  let refused = open_single_file(&path, SingleFileOpenOptions::new().wal_size(wrong));
  assert!(refused.is_err(), "setup: the open was not refused");
  let after = std::fs::read(&path).expect("read");
  let changed = (0..before.len().max(after.len()) / 4096)
    .filter(|&page| {
      before.get(page * 4096..(page + 1) * 4096) != after.get(page * 4096..(page + 1) * 4096)
    })
    .collect::<Vec<_>>();
  assert!(
    changed.is_empty(),
    "an open that refused the file wrote to it: pages {changed:?} changed"
  );
}

/// An open of a file in the old magic refused at its last check, the
/// replication sidecar (here a file where its directory goes), writes
/// nothing to the file: the upgrade of its header slots comes after every
/// check that may refuse the open.
#[test]
fn an_open_refused_at_the_replication_sidecar_writes_nothing() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("sidecar.kitedb");
  std::fs::copy(
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v2_wal_records.kitedb"),
    &path,
  )
  .expect("copy the fixture");
  let sidecar = dir.path().join("sidecar-is-a-file");
  std::fs::write(&sidecar, b"not a directory").expect("write");
  let before = std::fs::read(&path).expect("read");
  let refused = open_single_file(
    &path,
    SingleFileOpenOptions::new()
      .replication_role(crate::replication::types::ReplicationRole::Primary)
      .replication_sidecar_path(&sidecar),
  );
  assert!(refused.is_err(), "setup: the open was not refused");
  let after = std::fs::read(&path).expect("read");
  let changed = (0..before.len().max(after.len()) / 4096)
    .filter(|&page| {
      before.get(page * 4096..(page + 1) * 4096) != after.get(page * 4096..(page + 1) * 4096)
    })
    .collect::<Vec<_>>();
  assert!(
    changed.is_empty(),
    "an open refused at the replication sidecar wrote to the file: pages {changed:?} changed; \
     v0.2.18 accepts {:?} of its slots",
    accepted_slots(&path)
  );
}

/// `page`, a header page, in the old magic with the checksums a header in
/// that magic carries: the CRC-32 of bytes 0..176 at 176, and of the whole
/// page but its last four bytes in them.
fn in_old_magic(page: &[u8]) -> Vec<u8> {
  let mut page = page.to_vec();
  page[0..16].copy_from_slice(&V0_2_18_MAGIC);
  let header_crc = crc32fast::hash(&page[..176]);
  page[176..180].copy_from_slice(&header_crc.to_le_bytes());
  let footer = page.len() - 4;
  let footer_crc = crc32fast::hash(&page[..footer]);
  page[footer..].copy_from_slice(&footer_crc.to_le_bytes());
  page
}

/// A header in the old magic that names WAL segments (only unreleased
/// builds before this one wrote such headers: no release has segments) is
/// refused, and so is its file, writable or read-only, with nothing
/// written: its footer checksum, over a page that holds the fixed fields'
/// own checksum, does not depend on the fixed fields, so a page torn at a
/// sector boundary inside its segment table passes both checks (R12), and
/// this version cannot tell such a page from a whole one. The other slot is
/// no fallback either, even one naming no segments: the commits the
/// segments hold would be gone without a word.
#[test]
fn old_magic_headers_that_name_wal_segments_are_refused() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("segments.kitedb");
  let db = open_single_file(&path, options()).expect("create");
  commit_keys(&db, 0, 400);
  assert!(
    wal_segment_test_stats(&db).live > 0,
    "setup: the WAL never spilled"
  );
  drop(db);
  let bytes = std::fs::read(&path).expect("read");
  let slots: Vec<DbHeaderV1> = (0..2)
    .map(|slot| DbHeaderV1::parse(&bytes[slot * 4096..(slot + 1) * 4096]).expect("a valid slot"))
    .collect();
  let newest = usize::from(slots[1].change_counter > slots[0].change_counter);
  assert!(
    !slots[newest].wal_segments.is_empty(),
    "setup: the newest slot names no segments"
  );

  // Both slots in the old magic; or the newest so, the other naming no
  // segments, older.
  let mut both = bytes.clone();
  for slot in 0..2 {
    let page = in_old_magic(&bytes[slot * 4096..(slot + 1) * 4096]);
    both[slot * 4096..(slot + 1) * 4096].copy_from_slice(&page);
  }
  let mut fallback = both.clone();
  let mut older = slots[newest].clone();
  older.wal_segments = Default::default();
  older.change_counter -= 1;
  let other = 1 - newest;
  fallback[other * 4096..(other + 1) * 4096]
    .copy_from_slice(&in_old_magic(&older.serialize_to_page()));

  for (case, image) in [("both slots", both), ("the newest slot", fallback)] {
    for read_only in [false, true] {
      std::fs::write(&path, &image).expect("write the image");
      let opened = open_single_file(&path, options().read_only(read_only));
      let error = opened
        .as_ref()
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
      drop(opened);
      assert!(
        error.contains("WAL segments"),
        "{case} in the old magic naming WAL segments, read-only {read_only}: the open was \
         not refused for it: {error:?}"
      );
      assert!(
        std::fs::read(&path).expect("read") == image,
        "{case}, read-only {read_only}: the refused open wrote to the file"
      );
    }
  }
}
