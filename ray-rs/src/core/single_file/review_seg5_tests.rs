//! Delta review of round 4 (ead997f): header pages mixed from several
//! writes, sector by sector. Included from checkpoint.rs.
use crate::types::{DbHeaderV1, WalSegment, WalSegmentTable};

/// The bytes a disk writes whole.
const SECTOR: usize = 512;
const PAGE: usize = 4096;
const SECTORS: usize = PAGE / SECTOR;

/// A header naming `count` segments of 24 pages, the last `appended` bytes
/// longer and sealed if `sealed`, at change counter `counter` (its WAL
/// fields follow it, as a spill's or a commit's do).
fn header(count: u64, appended: u64, sealed: bool, counter: u64) -> Vec<u8> {
  let mut header = DbHeaderV1::new(PAGE as u32, 16);
  header.change_counter = counter;
  header.wal_head = (counter % 3) * 4096;
  header.wal_primary_head = header.wal_head;
  header.wal_primary_salt = 1 + counter as u32;
  header.max_node_id = 1000 + counter;
  header.next_tx_id = 50 + counter;
  header.wal_segments = WalSegmentTable {
    next_seq: count + 1,
    covered: 0,
    entries: (1..=count)
      .map(|seq| WalSegment {
        seq,
        start_page: 18 + 24 * seq,
        page_count: 24,
        byte_len: 8_192 + 8 * seq + if seq == count { appended } else { 0 },
        sealed: seq < count || sealed,
      })
      .collect::<Vec<_>>()
      .into(),
  };
  header.serialize_to_page()
}

/// Whether `page` parses, and if so whether it is one of `versions`.
fn verdict(page: &[u8], versions: &[&Vec<u8>]) -> Option<bool> {
  DbHeaderV1::parse(page)
    .ok()
    .map(|_| versions.iter().any(|version| version.as_slice() == page))
}

/// R12, every way round: a header page made of sectors from two or three
/// writes of it (any subset of sectors from each: tears at several
/// boundaries, sectors persisted out of order, writes of one slot still in
/// flight together in Normal mode) is one of those writes, or invalid. The
/// writes differ as a spill's, a commit's and a cut's headers do: in the
/// fixed fields only, or in them and one table entry's length (and seal), at
/// table sizes that put that entry in each sector the table reaches.
#[test]
fn review5_a_header_page_mixed_from_several_writes_is_one_of_them_or_invalid() {
  let mut checked = 0usize;
  for count in [1u64, 9, 10, 14, 25, 40, 57, 58, 64] {
    let versions = [
      header(count, 0, false, 7),
      header(count, 0, false, 8),
      header(count, 512, false, 9),
      header(count, 512, true, 10),
    ];
    for version in &versions {
      assert!(DbHeaderV1::parse(version).is_ok(), "setup: {count} entries");
    }
    // Two writes: every subset of sectors from the second.
    for (a, b) in [(0, 1), (1, 2), (0, 2), (2, 3), (1, 3)] {
      let (old, new) = (&versions[a], &versions[b]);
      for mask in 0u32..(1 << SECTORS) {
        let mut page = old.clone();
        for sector in 0..SECTORS {
          if mask >> sector & 1 == 1 {
            let range = sector * SECTOR..(sector + 1) * SECTOR;
            page[range.clone()].copy_from_slice(&new[range]);
          }
        }
        checked += 1;
        assert_ne!(
          verdict(&page, &[old, new]),
          Some(false),
          "{count} entries, writes {a} and {b}, sectors {mask:08b} from the second: a valid \
           header that is neither write"
        );
      }
    }
    // Three writes of one slot: each sector from any of them.
    let three = [&versions[0], &versions[2], &versions[3]];
    for combination in 0..3usize.pow(SECTORS as u32) {
      let mut page = vec![0u8; PAGE];
      let mut digits = combination;
      for sector in 0..SECTORS {
        let range = sector * SECTOR..(sector + 1) * SECTOR;
        page[range.clone()].copy_from_slice(&three[digits % 3][range]);
        digits /= 3;
      }
      checked += 1;
      assert_ne!(
        verdict(&page, &three),
        Some(false),
        "{count} entries, three writes, sectors {combination} (base 3): a valid header that is \
         none of them"
      );
    }
  }
  assert_eq!(checked, 9 * (5 * 256 + 6561), "pages checked");
}
