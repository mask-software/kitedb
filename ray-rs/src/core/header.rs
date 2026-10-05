//! Single-file database header management
//!
//! Ported from src/core/header.ts

use crate::constants::*;
use crate::core::pager::FilePager;
use crate::error::{KiteError, Result};
use crate::types::{DbHeaderV1, WalSegment, WalSegmentTable, DB_HEADER_FIXED_SIZE};
use crate::util::binary::*;
use crate::util::crc::{crc32, crc32_zero_extended};

/// Two physical header pages. A new header is written to the inactive page,
/// synced, and selected by generation during the next open.
pub(crate) const HEADER_SLOT_A: u32 = 0;
pub(crate) const HEADER_SLOT_B: u32 = 1;
pub(crate) const HEADER_SLOT_COUNT: u64 = 2;

const HEADER_CRC_OFFSET: usize = DB_HEADER_FIXED_SIZE;
const HEADER_CRC_END: usize = DB_HEADER_FIXED_SIZE + std::mem::size_of::<u32>();

pub(crate) fn other_header_slot(slot: u32) -> u32 {
  match slot {
    HEADER_SLOT_A => HEADER_SLOT_B,
    HEADER_SLOT_B => HEADER_SLOT_A,
    _ => panic!("invalid header slot: {slot}"),
  }
}

impl DbHeaderV1 {
  /// Parse and verify a complete header page.
  pub fn parse(data: &[u8]) -> Result<Self> {
    if data.len() < DB_HEADER_SIZE {
      return Err(KiteError::InvalidSnapshot(format!(
        "Header too small: {} bytes",
        data.len()
      )));
    }
    if data.len() < HEADER_CRC_END {
      return Err(KiteError::InvalidSnapshot(
        "Header checksum is truncated".to_string(),
      ));
    }

    // Verify magic before interpreting any header state: the current one,
    // or the one before it (see `MAGIC_KITEDB_V1`).
    if data[0..16] != MAGIC_KITEDB && data[0..16] != MAGIC_KITEDB_V1 {
      let expected = u32::from_le_bytes([
        MAGIC_KITEDB[0],
        MAGIC_KITEDB[1],
        MAGIC_KITEDB[2],
        MAGIC_KITEDB[3],
      ]);
      return Err(KiteError::InvalidMagic {
        expected,
        got: read_u32(data, 0),
      });
    }

    let page_size = read_u32(data, 16) as usize;
    if page_size < DB_HEADER_SIZE || page_size > data.len() || !page_size.is_power_of_two() {
      return Err(KiteError::InvalidSnapshot(format!(
        "Invalid header page size: {page_size}"
      )));
    }

    // Verify the fixed-field checksum before accepting any header state.
    let header_crc = read_u32(data, HEADER_CRC_OFFSET);
    let computed_header_crc = crc32(&data[..HEADER_CRC_OFFSET]);
    if header_crc != computed_header_crc {
      return Err(KiteError::CrcMismatch {
        stored: header_crc,
        computed: computed_header_crc,
      });
    }

    // Verify the page footer as well. It protects the reserved part of the
    // header page, which is outside the fixed-field checksum.
    let footer_crc = read_u32(data, page_size - 4);
    let computed_footer_crc = crc32(&data[..page_size - 4]);
    if footer_crc != computed_footer_crc {
      return Err(KiteError::CrcMismatch {
        stored: footer_crc,
        computed: computed_footer_crc,
      });
    }

    let mut magic = [0u8; 16];
    magic.copy_from_slice(&data[0..16]);

    Ok(Self {
      magic,
      page_size: read_u32(data, 16),
      version: read_u32(data, 20),
      min_reader_version: read_u32(data, 24),
      flags: read_u32(data, 28),
      change_counter: read_u64(data, 32),
      db_size_pages: read_u64(data, 40),
      snapshot_start_page: read_u64(data, 48),
      snapshot_page_count: read_u64(data, 56),
      wal_start_page: read_u64(data, 64),
      wal_page_count: read_u64(data, 72),
      wal_head: read_u64(data, 80),
      wal_tail: read_u64(data, 88),
      active_snapshot_gen: read_u64(data, 96),
      prev_snapshot_gen: read_u64(data, 104),
      max_node_id: read_u64(data, 112),
      next_tx_id: read_u64(data, 120),
      last_commit_ts: read_u64(data, 128),
      schema_cookie: read_u64(data, 136),
      wal_primary_head: read_u64(data, 144),
      wal_secondary_head: read_u64(data, 152),
      active_wal_region: data[160],
      checkpoint_in_progress: data[161],
      wal_primary_salt: read_u32(data, 164),
      wal_secondary_salt: read_u32(data, 168),
      wal_segments: parse_wal_segment_table(data, page_size)?,
    })
  }

  /// Fail unless this build can open the file this header describes:
  /// `min_reader_version` and `flags` must be ones it supports, and a writable
  /// open also needs a format `version` it can write. A newer version that
  /// still declares this build a capable reader opens read-only only: writing
  /// would drop or break whatever that version added.
  pub fn check_supported(&self, writable: bool) -> Result<()> {
    if self.min_reader_version > VERSION_SINGLE_FILE {
      return Err(KiteError::VersionMismatch {
        required: self.min_reader_version,
        current: VERSION_SINGLE_FILE,
      });
    }
    let unsupported_flags = self.flags & !SUPPORTED_DB_FLAGS;
    if unsupported_flags != 0 {
      return Err(KiteError::InvalidSnapshot(format!(
        "database header flags 0x{unsupported_flags:08X} are not supported by this version \
         (it supports 0x{SUPPORTED_DB_FLAGS:08X})"
      )));
    }
    if writable && self.version > VERSION_SINGLE_FILE {
      return Err(KiteError::VersionMismatch {
        required: self.version,
        current: VERSION_SINGLE_FILE,
      });
    }
    Ok(())
  }

  /// Record the WAL regions' salts. A salted region needs a reader that
  /// verifies salts, so a v1 header is upgraded to the current format here:
  /// this happens when a v1 file's WAL is first reset, and from then on no
  /// unsalted record is written to it.
  pub(crate) fn set_wal_salts(&mut self, primary: u32, secondary: u32) {
    self.wal_primary_salt = primary;
    self.wal_secondary_salt = secondary;
    if primary != 0 || secondary != 0 {
      self.version = self.version.max(VERSION_SALTED_WAL);
      self.min_reader_version = self.min_reader_version.max(MIN_READER_SALTED_WAL);
    }
  }

  /// The format version and reader version this header is written with:
  /// v3 while it names WAL segments, and otherwise its format without them
  /// (v2 for a salted WAL, v1 before), so releases that read v2 open a file
  /// whose segments a checkpoint covered.
  pub fn written_versions(&self) -> (u32, u32) {
    if !self.wal_segments.is_empty() {
      (
        self.version.max(VERSION_WAL_SEGMENTS),
        self.min_reader_version.max(MIN_READER_WAL_SEGMENTS),
      )
    } else if self.version == VERSION_WAL_SEGMENTS {
      // v3 only for the segments a checkpoint has covered since.
      (VERSION_SALTED_WAL, MIN_READER_SALTED_WAL)
    } else {
      (self.version, self.min_reader_version)
    }
  }

  /// Serialize header to fixed 4KB buffer (default page size).
  pub fn serialize(&self) -> [u8; DB_HEADER_SIZE] {
    let vec = self.serialize_to_page();
    let mut buf = [0u8; DB_HEADER_SIZE];
    let len = buf.len().min(vec.len());
    buf[..len].copy_from_slice(&vec[..len]);
    buf
  }

  /// Serialize header to a Vec matching the page size.
  pub fn serialize_to_page(&self) -> Vec<u8> {
    let mut buf = Vec::new();
    self.serialize_into_page(&mut buf);
    buf
  }

  /// Serialize header into `buf`, a page: resized (with zeros) to the page
  /// size unless it has that size already, in which case its bytes between
  /// the fields and the footer must be zero, as this leaves them (a buffer
  /// reused for every header write).
  pub(crate) fn serialize_into_page(&self, buf: &mut Vec<u8>) {
    let page_size = self.page_size as usize;
    if buf.len() != page_size {
      buf.clear();
      buf.resize(page_size, 0);
    }
    let buf = buf.as_mut_slice();

    let (version, min_reader_version) = self.written_versions();
    // Always the current magic: a header read in the older one is upgraded
    // by the first write (see `MAGIC_KITEDB`).
    buf[0..16].copy_from_slice(&MAGIC_KITEDB);
    write_u32(buf, 16, self.page_size);
    write_u32(buf, 20, version);
    write_u32(buf, 24, min_reader_version);
    write_u32(buf, 28, self.flags);
    write_u64(buf, 32, self.change_counter);
    write_u64(buf, 40, self.db_size_pages);
    write_u64(buf, 48, self.snapshot_start_page);
    write_u64(buf, 56, self.snapshot_page_count);
    write_u64(buf, 64, self.wal_start_page);
    write_u64(buf, 72, self.wal_page_count);
    write_u64(buf, 80, self.wal_head);
    write_u64(buf, 88, self.wal_tail);
    write_u64(buf, 96, self.active_snapshot_gen);
    write_u64(buf, 104, self.prev_snapshot_gen);
    write_u64(buf, 112, self.max_node_id);
    write_u64(buf, 120, self.next_tx_id);
    write_u64(buf, 128, self.last_commit_ts);
    write_u64(buf, 136, self.schema_cookie);
    write_u64(buf, 144, self.wal_primary_head);
    write_u64(buf, 152, self.wal_secondary_head);
    buf[160] = self.active_wal_region;
    buf[161] = self.checkpoint_in_progress;
    // 162..164 reserved
    write_u32(buf, 164, self.wal_primary_salt);
    write_u32(buf, 168, self.wal_secondary_salt);
    // 172..176 reserved

    let header_crc = crc32(&buf[..HEADER_CRC_OFFSET]);
    write_u32(buf, HEADER_CRC_OFFSET, header_crc);

    // The WAL segment table, then zeros up to the footer.
    let table_end = write_wal_segment_table(buf, &self.wal_segments);
    let footer_crc = crc32_zero_extended(&buf[..table_end], page_size - 4 - table_end);
    write_u32(buf, page_size - 4, footer_crc);
  }

  /// Create a new header with default values.
  pub fn new(page_size: u32, wal_pages: u64) -> Self {
    let mut magic = [0u8; 16];
    magic.copy_from_slice(&MAGIC_KITEDB);

    Self {
      magic,
      page_size,
      version: VERSION_SALTED_WAL,
      min_reader_version: MIN_READER_SALTED_WAL,
      flags: 0,
      change_counter: 0,
      db_size_pages: HEADER_SLOT_COUNT + wal_pages,
      snapshot_start_page: 0,
      snapshot_page_count: 0,
      wal_start_page: HEADER_SLOT_COUNT,
      wal_page_count: wal_pages,
      wal_head: 0,
      wal_tail: 0,
      active_snapshot_gen: 0,
      prev_snapshot_gen: 0,
      max_node_id: 0,
      next_tx_id: INITIAL_TX_ID,
      last_commit_ts: 0,
      schema_cookie: 0,
      wal_primary_head: 0,
      wal_secondary_head: 0,
      active_wal_region: 0,
      checkpoint_in_progress: 0,
      // Only version 2 checkpoints wrote the secondary region (and salted it
      // first); this version never does.
      wal_primary_salt: INITIAL_WAL_SALT,
      wal_secondary_salt: 0,
      wal_segments: WalSegmentTable::default(),
    }
  }
}

/// Read the WAL segment table of a header page (zeros in v1 and v2 pages:
/// an empty table). Fails on a table no writer produces: more entries than
/// fit, seqs out of order, an extent before the WAL, or more bytes than its
/// pages hold.
fn parse_wal_segment_table(data: &[u8], page_size: usize) -> Result<WalSegmentTable> {
  let invalid = |what: String| {
    Err(KiteError::InvalidSnapshot(format!(
      "header names an invalid WAL segment table: {what}"
    )))
  };
  let table = WAL_SEGMENT_TABLE_OFFSET;
  let count = read_u32(data, table) as usize;
  let entries_start = table + WAL_SEGMENT_TABLE_HEADER_SIZE;
  if count > MAX_WAL_SEGMENTS || entries_start + count * WAL_SEGMENT_ENTRY_SIZE > page_size - 4 {
    return invalid(format!("{count} entries"));
  }
  let next_seq = read_u64(data, table + 8);
  let covered = read_u64(data, table + 16);
  let mut entries = Vec::with_capacity(count);
  for index in 0..count {
    let offset = entries_start + index * WAL_SEGMENT_ENTRY_SIZE;
    let segment = WalSegment {
      seq: read_u64(data, offset),
      start_page: read_u64(data, offset + 8),
      page_count: read_u32(data, offset + 16) as u64,
      sealed: read_u32(data, offset + 20) & 1 != 0,
      byte_len: read_u64(data, offset + 24),
    };
    let previous_seq = entries
      .last()
      .map_or(0, |previous: &WalSegment| previous.seq);
    if segment.seq <= previous_seq
      || segment.seq >= next_seq.max(1)
      || segment.start_page < HEADER_SLOT_COUNT
      || segment.page_count == 0
      || segment.byte_len > segment.page_count * page_size as u64
    {
      return invalid(format!(
        "entry {index} is {segment:?} (next seq {next_seq})"
      ));
    }
    entries.push(segment);
  }
  Ok(WalSegmentTable {
    next_seq,
    covered,
    entries: entries.into(),
  })
}

/// Write `table` into a header page from `WAL_SEGMENT_TABLE_OFFSET` on;
/// returns where it ends (bytes after it up to the footer stay zero).
fn write_wal_segment_table(buf: &mut [u8], table: &WalSegmentTable) -> usize {
  let start = WAL_SEGMENT_TABLE_OFFSET;
  // A reused page buffer may hold a longer table from an earlier header.
  let table_capacity_end =
    start + WAL_SEGMENT_TABLE_HEADER_SIZE + MAX_WAL_SEGMENTS * WAL_SEGMENT_ENTRY_SIZE;
  let zero_end = table_capacity_end.min(buf.len() - 4);
  buf[start..zero_end].fill(0);
  write_u32(buf, start, table.entries.len() as u32);
  write_u32(buf, start + 4, 0);
  write_u64(buf, start + 8, table.next_seq);
  write_u64(buf, start + 16, table.covered);
  let mut offset = start + WAL_SEGMENT_TABLE_HEADER_SIZE;
  for segment in table.entries.iter() {
    write_u64(buf, offset, segment.seq);
    write_u64(buf, offset + 8, segment.start_page);
    write_u32(buf, offset + 16, segment.page_count as u32);
    write_u32(buf, offset + 20, u32::from(segment.sealed));
    write_u64(buf, offset + 24, segment.byte_len);
    offset += WAL_SEGMENT_ENTRY_SIZE;
  }
  offset
}

/// Read both physical header pages and return the newest valid generation.
/// A torn or partially written inactive page is ignored.
pub(crate) fn read_header_slots(pager: &mut FilePager) -> Result<(DbHeaderV1, u32)> {
  read_header_slots_with_fallback(pager).map(|slots| (slots.header, slots.slot))
}

/// The header pages as open finds them (see `read_header_slots_with_fallback`).
pub(crate) struct HeaderSlots {
  /// The newest valid header, and its slot.
  pub(crate) header: DbHeaderV1,
  pub(crate) slot: u32,
  /// The other slot's header, if it is valid: what a crash could fall back
  /// to.
  pub(crate) fallback: Option<DbHeaderV1>,
  /// Both pages begin with the current magic (`MAGIC_KITEDB`), valid or
  /// not. A page in the older magic, even one this version cannot use, may
  /// pass the check of a release that knows only that magic.
  pub(crate) in_current_magic: bool,
}

/// Read both independently checksummed header pages and select the newest
/// valid generation: a torn newest slot falls back to the other slot.
pub(crate) fn read_header_slots_with_fallback(pager: &mut FilePager) -> Result<HeaderSlots> {
  let mut valid = Vec::with_capacity(2);
  let mut errors = Vec::with_capacity(2);
  let mut in_current_magic = true;

  for slot in [HEADER_SLOT_A, HEADER_SLOT_B] {
    let page = pager.read_page(slot)?;
    in_current_magic &= page.get(0..16) == Some(&MAGIC_KITEDB[..]);
    match DbHeaderV1::parse(&page) {
      Ok(header) if header.page_size as usize == pager.page_size() => {
        valid.push((header, slot));
      }
      Ok(header) => errors.push(format!(
        "slot {slot} has page size {}, expected {}",
        header.page_size,
        pager.page_size()
      )),
      Err(error) => errors.push(format!("slot {slot}: {error}")),
    }
  }

  valid.sort_by_key(|(header, slot)| (header.change_counter, *slot == HEADER_SLOT_A));
  let Some((header, slot)) = valid.pop() else {
    return Err(KiteError::InvalidSnapshot(format!(
      "no valid database header slot; {}",
      errors.join("; ")
    )));
  };
  Ok(HeaderSlots {
    header,
    slot,
    fallback: valid.pop().map(|(fallback, _)| fallback),
    in_current_magic,
  })
}

/// Write one complete header page. The caller must sync before treating the
/// target slot as installed.
pub(crate) fn write_header_slot(
  pager: &mut FilePager,
  header: &DbHeaderV1,
  slot: u32,
) -> Result<()> {
  if slot >= HEADER_SLOT_COUNT as u32 {
    return Err(KiteError::Internal(format!("invalid header slot: {slot}")));
  }
  if header.page_size as usize != pager.page_size() {
    return Err(KiteError::InvalidSnapshot(format!(
      "header page size {} does not match pager page size {}",
      header.page_size,
      pager.page_size()
    )));
  }
  // Every commit group writes a header: reuse one page buffer per thread.
  thread_local! {
    static PAGE: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
  }
  PAGE.with(|page| {
    let mut page = page.borrow_mut();
    header.serialize_into_page(&mut page);
    pager.write_page(slot, &page)
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn wal_salts_round_trip_inside_the_checksummed_fields() {
    let mut header = DbHeaderV1::new(4096, 16);
    header.set_wal_salts(0xA1B2_C3D4, 0x0102_0304);
    let page = header.serialize_to_page();
    assert_eq!(read_u32(&page, 164), 0xA1B2_C3D4);
    assert_eq!(read_u32(&page, 168), 0x0102_0304);
    let parsed = DbHeaderV1::parse(&page).expect("parse");
    assert_eq!(
      (parsed.wal_primary_salt, parsed.wal_secondary_salt),
      (0xA1B2_C3D4, 0x0102_0304)
    );
    assert_eq!(parsed.serialize_to_page(), page);

    let mut torn = page.clone();
    torn[168] ^= 1;
    assert!(DbHeaderV1::parse(&torn).is_err());
  }

  #[test]
  fn new_headers_are_the_current_salted_format() {
    let header = DbHeaderV1::new(4096, 16);
    assert_eq!(header.version, VERSION_SALTED_WAL);
    assert_eq!(header.min_reader_version, MIN_READER_SALTED_WAL);
    assert_eq!(header.wal_primary_salt, INITIAL_WAL_SALT);
    assert_ne!(header.wal_primary_salt, 0);
    assert!(header.check_supported(true).is_ok());
  }

  #[test]
  fn check_supported_gates_versions_and_flags() {
    let header = |version, min_reader_version, flags| {
      let mut header = DbHeaderV1::new(4096, 16);
      header.version = version;
      header.min_reader_version = min_reader_version;
      header.flags = flags;
      header
    };
    let current = VERSION_SINGLE_FILE;
    for writable in [false, true] {
      // Format 1 files open and are upgraded; WAL mode is implied.
      assert!(header(1, 1, 0).check_supported(writable).is_ok());
      assert!(header(current, current, DB_FLAG_WAL_MODE)
        .check_supported(writable)
        .is_ok());
      // A file that needs a newer reader, or has flags this build lacks.
      assert!(matches!(
        header(current + 1, current + 1, 0).check_supported(writable),
        Err(KiteError::VersionMismatch { required, current: supported })
          if required == current + 1 && supported == current
      ));
      for flag in [DB_FLAG_COMPRESSION, DB_FLAG_ENCRYPTED, 1 << 31] {
        assert!(header(current, current, flag)
          .check_supported(writable)
          .is_err());
      }
    }
    // A newer format this build can still read opens read-only only.
    let newer = header(current + 1, MIN_READER_SINGLE_FILE, 0);
    assert!(newer.check_supported(false).is_ok());
    assert!(matches!(
      newer.check_supported(true),
      Err(KiteError::VersionMismatch { .. })
    ));
  }

  /// A header naming `count` segments of 24 pages, the last `appended`
  /// bytes longer, at change counter `counter`.
  fn header_with_segments(count: u64, appended: u64, counter: u64) -> DbHeaderV1 {
    let mut header = DbHeaderV1::new(4096, 16);
    header.change_counter = counter;
    header.wal_head = counter * 100;
    header.wal_primary_salt = counter as u32;
    header.wal_segments = WalSegmentTable {
      next_seq: count + 1,
      covered: 0,
      entries: (1..=count)
        .map(|seq| WalSegment {
          seq,
          start_page: 18 + 24 * seq,
          page_count: 24,
          byte_len: 1_000 + if seq == count { appended } else { 0 },
          sealed: seq < count,
        })
        .collect::<Vec<_>>()
        .into(),
    };
    header
  }

  /// R12. A header page torn between two writes of it at a 512-byte sector
  /// boundary (a disk writes sectors whole, not pages), either side from
  /// either write, is invalid unless it is one of the two pages: one
  /// checksum covers every byte that gives the page meaning. Here the
  /// second write changes the fixed fields and the table, its last entry in
  /// a later sector than the first (the 10th, 20th, 40th and 64th entry,
  /// the last a table holds), or only the fixed fields.
  #[test]
  fn a_header_page_torn_at_a_sector_boundary_is_invalid() {
    let changes = [(10, 64), (20, 64), (40, 64), (64, 64), (64, 0)];
    for (count, appended) in changes {
      let old = header_with_segments(count, 0, 7).serialize_to_page();
      let new = header_with_segments(count, appended, 8).serialize_to_page();
      assert!(DbHeaderV1::parse(&old).is_ok() && DbHeaderV1::parse(&new).is_ok());
      for boundary in (512..4096).step_by(512) {
        for (first, rest) in [(&new, &old), (&old, &new)] {
          let mut torn = first[..boundary].to_vec();
          torn.extend_from_slice(&rest[boundary..]);
          if torn == old || torn == new {
            continue;
          }
          let parsed = DbHeaderV1::parse(&torn).ok().map(|header| {
            (
              header.change_counter,
              header
                .wal_segments
                .entries
                .last()
                .map(|entry| entry.byte_len),
            )
          });
          assert!(
            parsed.is_none(),
            "{count} segments, the last {appended} bytes longer: a page torn at byte \
             {boundary} ({} write first) is a valid header: {parsed:?}",
            if std::ptr::eq(first, &new) {
              "new"
            } else {
              "old"
            }
          );
        }
      }
    }
  }
}
