//! Single-file database header management
//!
//! Ported from src/core/header.ts

use crate::constants::*;
use crate::error::{RayError, Result};
use crate::types::DbHeaderV1;
use crate::util::binary::*;
use crate::util::crc::crc32c;

impl DbHeaderV1 {
  /// Parse header from page buffer
  pub fn parse(data: &[u8]) -> Result<Self> {
    if data.len() < DB_HEADER_SIZE {
      return Err(RayError::InvalidSnapshot(format!(
        "Header too small: {} bytes",
        data.len()
      )));
    }

    // Verify magic
    if data[0..16] != MAGIC_RAYDB {
      return Err(RayError::InvalidMagic {
        expected: u32::from_le_bytes(MAGIC_RAYDB[0..4].try_into().unwrap()),
        got: read_u32(data, 0),
      });
    }

    // Verify header checksum
    let header_crc = read_u32(data, 176);
    let computed_header_crc = crc32c(&data[0..176]);
    if header_crc != computed_header_crc {
      return Err(RayError::CrcMismatch {
        stored: header_crc,
        computed: computed_header_crc,
      });
    }

    // Parse fields
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
    })
  }

  /// Serialize header to fixed 4KB buffer (default page size)
  pub fn serialize(&self) -> [u8; DB_HEADER_SIZE] {
    let vec = self.serialize_to_page();
    let mut buf = [0u8; DB_HEADER_SIZE];
    let len = buf.len().min(vec.len());
    buf[..len].copy_from_slice(&vec[..len]);
    buf
  }

  /// Serialize header to a Vec matching the page size
  pub fn serialize_to_page(&self) -> Vec<u8> {
    let page_size = self.page_size as usize;
    let mut buf = vec![0u8; page_size];

    buf[0..16].copy_from_slice(&self.magic);
    write_u32(&mut buf, 16, self.page_size);
    write_u32(&mut buf, 20, self.version);
    write_u32(&mut buf, 24, self.min_reader_version);
    write_u32(&mut buf, 28, self.flags);
    write_u64(&mut buf, 32, self.change_counter);
    write_u64(&mut buf, 40, self.db_size_pages);
    write_u64(&mut buf, 48, self.snapshot_start_page);
    write_u64(&mut buf, 56, self.snapshot_page_count);
    write_u64(&mut buf, 64, self.wal_start_page);
    write_u64(&mut buf, 72, self.wal_page_count);
    write_u64(&mut buf, 80, self.wal_head);
    write_u64(&mut buf, 88, self.wal_tail);
    write_u64(&mut buf, 96, self.active_snapshot_gen);
    write_u64(&mut buf, 104, self.prev_snapshot_gen);
    write_u64(&mut buf, 112, self.max_node_id);
    write_u64(&mut buf, 120, self.next_tx_id);
    write_u64(&mut buf, 128, self.last_commit_ts);
    write_u64(&mut buf, 136, self.schema_cookie);
    write_u64(&mut buf, 144, self.wal_primary_head);
    write_u64(&mut buf, 152, self.wal_secondary_head);
    buf[160] = self.active_wal_region;
    buf[161] = self.checkpoint_in_progress;

    // Compute and write header checksum
    let header_crc = crc32c(&buf[0..176]);
    write_u32(&mut buf, 176, header_crc);

    // Compute and write footer checksum (at end of page)
    let footer_crc = crc32c(&buf[0..page_size - 4]);
    write_u32(&mut buf, page_size - 4, footer_crc);

    buf
  }

  /// Create a new header with default values
  pub fn new(page_size: u32, wal_pages: u64) -> Self {
    let mut magic = [0u8; 16];
    magic.copy_from_slice(&MAGIC_RAYDB);

    Self {
      magic,
      page_size,
      version: VERSION_SINGLE_FILE,
      min_reader_version: MIN_READER_SINGLE_FILE,
      flags: 0,
      change_counter: 0,
      db_size_pages: 1 + wal_pages, // header + WAL
      snapshot_start_page: 0,       // No snapshot yet
      snapshot_page_count: 0,
      wal_start_page: 1,
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
    }
  }
}
