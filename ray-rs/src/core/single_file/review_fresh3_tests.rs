//! Delta review of round 3 (7b1798d..89d395a). Included from checkpoint.rs
//! for its private steps and test hooks.
use crate::core::header::{other_header_slot, read_header_slots, write_header_slot};
use crate::core::single_file::{close_single_file, open_single_file, SingleFileOpenOptions};
use std::path::Path;
use tempfile::tempdir;

/// v0.2.18's acceptance check of a header page (as in b4_downgrade_tests,
/// checked against tag v0.2.18's `DbHeaderV1::parse`).
fn v0_2_18_accepts(page: &[u8]) -> bool {
  if page.len() < 180 || page[0..16] != *b"KiteDB format 1\0" {
    return false;
  }
  let stored = u32::from_le_bytes(page[176..180].try_into().expect("four bytes"));
  let mut hasher = crc32fast::Hasher::new();
  hasher.update(&page[0..176]);
  hasher.finalize() == stored
}

fn accepted_slots(path: &Path) -> [bool; 2] {
  let bytes = std::fs::read(path).expect("read");
  [
    v0_2_18_accepts(&bytes[0..4096]),
    v0_2_18_accepts(&bytes[4096..8192]),
  ]
}

/// Probe: a file left with mixed magics by a crash between the two slot
/// writes of the magic upgrade (the selected slot still in the old magic,
/// the other already rewritten, newer, in the new one). A read-only open
/// reads the newer slot and writes nothing; a writable open upgrades the
/// remaining slot, after which v0.2.18 refuses both.
#[test]
fn fresh3_a_crash_mid_upgrade_leaves_a_file_the_next_writable_open_finishes() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("mixed.kitedb");
  std::fs::copy(
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v2_wal_records.kitedb"),
    &path,
  )
  .expect("copy the fixture");
  let nodes = {
    let db = open_single_file(&path, SingleFileOpenOptions::new().read_only(true)).expect("ro");
    db.count_nodes()
  };
  // The first write of the upgrade, then a crash: the other slot holds the
  // selected header, newer, in the new magic.
  {
    let mut pager = crate::core::pager::open_pager(&path, 4096, false).expect("pager");
    let (mut header, slot) = read_header_slots(&mut pager).expect("slots");
    header.change_counter += 1;
    write_header_slot(&mut pager, &header, other_header_slot(slot)).expect("write");
    pager.sync().expect("sync");
  }
  let accepted = accepted_slots(&path);
  assert_eq!(
    accepted.iter().filter(|&&accepted| accepted).count(),
    1,
    "setup: not a mixed-magic file: {accepted:?}"
  );
  let before = std::fs::read(&path).expect("read");
  let db = open_single_file(&path, SingleFileOpenOptions::new().read_only(true)).expect("ro");
  assert_eq!(db.count_nodes(), nodes, "read-only open of the mixed file");
  drop(db);
  assert_eq!(
    std::fs::read(&path).expect("read"),
    before,
    "read-only open wrote"
  );
  let db = open_single_file(&path, SingleFileOpenOptions::new()).expect("writable");
  assert_eq!(
    accepted_slots(&path),
    [false, false],
    "upgrade not finished"
  );
  assert_eq!(db.count_nodes(), nodes, "writable open of the mixed file");
  close_single_file(db).expect("close");
  assert_eq!(accepted_slots(&path), [false, false]);
}

/// N1 (low). 89d395a moved the magic upgrade after "the checks that may
/// refuse the open", and its comment says every such check is behind it
/// (open.rs, before the upgrade). Not so: a writable open of an old-magic
/// file whose snapshot does not load (here a damaged snapshot page) still
/// rewrites both header slots, then fails; the older fallback slot is gone
/// and the file is in the new magic, though the open refused it.
/// `a_refused_open_of_an_old_magic_file_writes_nothing` covers only a WAL
/// size refusal.
#[test]
fn fresh3_an_open_refused_for_a_damaged_snapshot_writes_nothing() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("damaged.kitedb");
  std::fs::copy(
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v2_wal_records.kitedb"),
    &path,
  )
  .expect("copy the fixture");
  let mut bytes = std::fs::read(&path).expect("read");
  let header = crate::types::DbHeaderV1::parse(&bytes[0..4096]).expect("slot 0");
  assert!(header.snapshot_page_count > 0, "setup: no snapshot");
  // Damage the middle of the snapshot.
  let at = (header.snapshot_start_page * 4096 + header.snapshot_page_count * 2048) as usize;
  for byte in &mut bytes[at..at + 64] {
    *byte ^= 0xff;
  }
  std::fs::write(&path, &bytes).expect("write");
  let before = std::fs::read(&path).expect("read");
  let refused = open_single_file(&path, SingleFileOpenOptions::new());
  assert!(refused.is_err(), "setup: the open was not refused");
  let after = std::fs::read(&path).expect("read");
  let changed = (0..before.len().max(after.len()) / 4096)
    .filter(|&page| {
      before.get(page * 4096..(page + 1) * 4096) != after.get(page * 4096..(page + 1) * 4096)
    })
    .collect::<Vec<_>>();
  assert!(
    changed.is_empty(),
    "an open that refused the file ({}) wrote to it: pages {changed:?} changed; v0.2.18 now \
     accepts {:?} of its slots",
    refused
      .err()
      .map(|error| error.to_string())
      .unwrap_or_default(),
    accepted_slots(&path)
  );
}
