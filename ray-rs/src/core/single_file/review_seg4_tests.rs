//! Delta review of `fix/b4-checkpoint-segments` (89d395a): header tears.
//! Included from checkpoint.rs for its test hooks.
use super::*;
use crate::core::pager::io_hooks::{self, IoEvent};
use crate::core::single_file::{open_single_file, SingleFileOpenOptions, SyncMode};
use tempfile::tempdir;

/// The smallest WAL a database accepts: a 48 KiB primary region.
const SMALL_WAL: usize = 64 * 1024;

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

fn apply(image: &mut Vec<u8>, offset: u64, data: &[u8]) {
  let (start, end) = (offset as usize, offset as usize + data.len());
  if image.len() < end {
    image.resize(end, 0);
  }
  image[start..end].copy_from_slice(data);
}

/// R12. A header page torn at a sector boundary inside its WAL segment
/// table (512 bytes: the fixed fields and the table's first entries new,
/// the rest of the page old) passes both of its checksums when the table's
/// bytes before the tear did not change: the fixed fields' checksum covers
/// only them, and the footer checksum, over the whole page, does not depend
/// on them at all (CRC-32 of a message followed by its own CRC is a
/// constant), so it holds for the old table. The torn page is a valid
/// header that pairs the new fixed fields with the old table.
///
/// A spill that appends to the newest segment, the tenth or later in the
/// table, writes such a header first: its fixed fields name the emptied WAL
/// under a fresh salt, and the table entry whose `byte_len` grew lies past
/// the first sector. Torn there (an OS crash during that write, on a disk
/// that writes 512-byte sectors atomically but not 4 KiB pages), the newest
/// valid header names an empty WAL and the segment's old length: the WAL's
/// records, synced and acknowledged in Full mode, are in neither.
#[test]
fn review4_a_spill_header_torn_inside_its_segment_table_loses_no_commit() {
  spill_header_torn_inside_its_segment_table(false);
}

/// R12, the other way round: the page's later sectors land and its first
/// does not (sectors written out of order). The old fixed fields (the WAL as
/// it was, under its old salt) pair with the new table (the segment holding
/// the WAL's records too), which is valid as well: recovery reads the
/// spilled records twice, in the segment and in the WAL.
#[test]
fn review4_a_spill_header_torn_the_other_way_replays_nothing_twice() {
  spill_header_torn_inside_its_segment_table(true);
}

/// A spill's first header write torn at its first sector boundary, inside
/// its WAL segment table: `later_sectors` lands the rest of the page and not
/// the first sector, else the first sector only.
fn spill_header_torn_inside_its_segment_table(later_sectors: bool) {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("table-tear.kitedb");
  // Extents of one and a half WALs: two spills fill one, so every other
  // spill appends to the newest segment.
  let options = SingleFileOpenOptions::new()
    .wal_size(SMALL_WAL)
    .sync_mode(SyncMode::Full)
    .auto_checkpoint(false)
    .wal_segment_size(1)
    .wal_segment_limit(64 * 1024 * 1024);
  let db = open_single_file(&path, options.clone()).expect("open");
  let header_end = 2 * db.header.read().page_size as u64;
  let mut acked = Vec::new();
  let mut index = 0;
  while wal_segment_test_stats(&db).live < 11 && index < 20_000 {
    let key = key("fill", index);
    commit_key(&db, &key).expect("commit");
    acked.push(key);
    index += 1;
  }
  assert!(
    wal_segment_test_stats(&db).live >= 11,
    "setup: fewer than 11 segments"
  );

  // The next commit whose spill appends to the newest segment.
  let mut found = None;
  for _ in 0..2_000 {
    let newest = |db: &SingleFileDB| {
      *db
        .header
        .read()
        .wal_segments
        .entries
        .last()
        .expect("a segment")
    };
    let before = newest(&db);
    let live = wal_segment_test_stats(&db).live;
    let base = std::fs::read(&path).expect("base image");
    let key = key("tail", index);
    index += 1;
    let (result, events) = io_hooks::record_io_during(|| commit_key(&db, &key));
    result.expect("commit");
    let after = newest(&db);
    if after.seq == before.seq
      && after.byte_len > before.byte_len
      && wal_segment_test_stats(&db).live == live
    {
      found = Some((base, events, live));
      break;
    }
    acked.push(key);
  }
  let (base, events, live) = found.expect("setup: no spill appended to the newest segment");
  // Entry `live - 1`'s byte_len lies at 208 + 32 * index + 24 in the page.
  assert!(
    208 + 32 * (live - 1) + 24 >= 512,
    "setup: the appended entry lies in the first sector"
  );

  // A crash during the spill's first header write (its segment is written
  // and synced), the page torn after its first sector.
  let first_header = events
    .iter()
    .position(|event| matches!(event, IoEvent::Write { offset, .. } if *offset < header_end))
    .expect("the spill's header write");
  let mut image = base;
  for event in &events[..first_header] {
    if let IoEvent::Write { offset, data } = event {
      apply(&mut image, *offset, data);
    }
  }
  if let IoEvent::Write { offset, data } = &events[first_header] {
    if later_sectors {
      apply(&mut image, *offset + 512, &data[512..]);
    } else {
      apply(&mut image, *offset, &data[..512]);
    }
  }
  drop(db);
  let image_path = dir.path().join("table-tear-image.kitedb");
  std::fs::write(&image_path, &image).expect("write the image");
  let crashed = open_single_file(&image_path, options).expect("open the crash image");
  let lost = acked
    .iter()
    .filter(|key| crashed.node_by_key(key).is_none())
    .count();
  let nodes = crashed.count_nodes();
  assert!(
    lost == 0 && nodes == acked.len(),
    "a spill header torn at its first sector boundary (later sectors landed: {later_sectors}) \
     is a valid header pairing one write's fixed fields with another's segment table: {lost} \
     of {} acknowledged commits are gone, and the database holds {nodes} nodes",
    acked.len()
  );
}

/// R13 (pre-existing; the code is main's). In `SyncMode::Normal` commit
/// headers are written without a sync, to alternating slots, so two of them
/// can be in flight at once: an OS crash then may tear both slots, and the
/// file has no valid header left. Normal mode may lose the commits since its
/// last sync, but not the database: this crash leaves it unopenable, though
/// everything up to the creating sync was durable.
#[test]
fn review4_normal_mode_survives_both_unsynced_header_writes_torn() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("double-tear.kitedb");
  let options = SingleFileOpenOptions::new()
    .wal_size(SMALL_WAL)
    .sync_mode(SyncMode::Normal)
    .auto_checkpoint(false);
  let db = open_single_file(&path, options.clone()).expect("open");
  let header_end = 2 * db.header.read().page_size as u64;
  // As the creating sync left it.
  let base = std::fs::read(&path).expect("base image");
  let (result, events) = io_hooks::record_io_during(|| {
    commit_key(&db, &key("a", 0))?;
    commit_key(&db, &key("b", 1))
  });
  result.expect("commits");
  assert!(
    !events
      .iter()
      .any(|event| matches!(event, IoEvent::Sync { ok: true })),
    "setup: a commit synced in Normal mode"
  );
  // The data landed; both header writes (one per slot) tore.
  let mut image = base;
  let mut torn_slots = std::collections::HashSet::new();
  for event in &events {
    if let IoEvent::Write { offset, data } = event {
      if *offset < header_end {
        torn_slots.insert(*offset);
        apply(&mut image, *offset, &data[..64]);
      } else {
        apply(&mut image, *offset, data);
      }
    }
  }
  assert_eq!(torn_slots.len(), 2, "setup: the commits wrote one slot");
  drop(db);
  let image_path = dir.path().join("double-tear-image.kitedb");
  std::fs::write(&image_path, &image).expect("write the image");
  let reopened = open_single_file(&image_path, options);
  assert!(
    reopened.is_ok(),
    "an OS crash during two unsynced header writes in Normal mode left no valid header: {:?}",
    reopened.err()
  );
}
