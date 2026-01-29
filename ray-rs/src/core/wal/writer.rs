//! WAL writer and header management
//!
//! Ported from src/core/wal.ts

use crate::constants::*;
use crate::error::{RayError, Result};
use crate::types::*;
use crate::util::binary::*;
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

use super::record::WalRecord;

// ============================================================================
// WAL Header
// ============================================================================

/// Create a new WAL header
pub fn create_wal_header(segment_id: u64) -> WalHeaderV1 {
  let now_ns = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.as_nanos() as u64)
    .unwrap_or(0);

  WalHeaderV1 {
    magic: MAGIC_WAL,
    version: VERSION_WAL,
    min_reader_version: MIN_READER_WAL,
    reserved: 0,
    segment_id,
    created_unix_ns: now_ns,
    reserved2: [0; 8],
  }
}

/// Serialize WAL header to bytes
pub fn serialize_wal_header(header: &WalHeaderV1) -> [u8; WAL_HEADER_SIZE] {
  let mut buffer = [0u8; WAL_HEADER_SIZE];

  write_u32(&mut buffer, 0, header.magic);
  write_u32(&mut buffer, 4, header.version);
  write_u32(&mut buffer, 8, header.min_reader_version);
  write_u32(&mut buffer, 12, header.reserved);
  write_u64(&mut buffer, 16, header.segment_id);
  write_u64(&mut buffer, 24, header.created_unix_ns);

  for i in 0..8 {
    write_u64(&mut buffer, 32 + i * 8, header.reserved2[i]);
  }

  buffer
}

/// Parse WAL header from bytes
pub fn parse_wal_header(buffer: &[u8]) -> Result<WalHeaderV1> {
  if buffer.len() < WAL_HEADER_SIZE {
    return Err(RayError::InvalidWal(format!(
      "WAL header too small: {} bytes",
      buffer.len()
    )));
  }

  let magic = read_u32(buffer, 0);
  if magic != MAGIC_WAL {
    return Err(RayError::InvalidMagic {
      expected: MAGIC_WAL,
      got: magic,
    });
  }

  let version = read_u32(buffer, 4);
  let min_reader_version = read_u32(buffer, 8);

  if MIN_READER_WAL < min_reader_version {
    return Err(RayError::VersionMismatch {
      required: min_reader_version,
      current: MIN_READER_WAL,
    });
  }

  let reserved = read_u32(buffer, 12);
  let segment_id = read_u64(buffer, 16);
  let created_unix_ns = read_u64(buffer, 24);

  let mut reserved2 = [0u64; 8];
  for (i, slot) in reserved2.iter_mut().enumerate() {
    *slot = read_u64(buffer, 32 + i * 8);
  }

  Ok(WalHeaderV1 {
    magic,
    version,
    min_reader_version,
    reserved,
    segment_id,
    created_unix_ns,
    reserved2,
  })
}

// ============================================================================
// WAL Writer
// ============================================================================

/// WAL segment writer
pub struct WalWriter {
  file: File,
  offset: usize,
  segment_id: u64,
}

impl WalWriter {
  /// Create a new WAL segment file
  pub fn create(path: impl AsRef<Path>, segment_id: u64) -> Result<Self> {
    let mut file = OpenOptions::new()
      .write(true)
      .create(true)
      .truncate(true)
      .open(path.as_ref())?;

    let header = create_wal_header(segment_id);
    let header_bytes = serialize_wal_header(&header);
    file.write_all(&header_bytes)?;
    file.sync_all()?;

    Ok(Self {
      file,
      offset: WAL_HEADER_SIZE,
      segment_id,
    })
  }

  /// Open an existing WAL segment for appending
  pub fn open(path: impl AsRef<Path>) -> Result<Self> {
    let mut file = OpenOptions::new()
      .read(true)
      .write(true)
      .open(path.as_ref())?;

    // Read header
    let mut header_buf = [0u8; WAL_HEADER_SIZE];
    use std::io::Read;
    file.read_exact(&mut header_buf)?;
    let header = parse_wal_header(&header_buf)?;

    // Get file size for offset
    let offset = file.seek(SeekFrom::End(0))? as usize;

    Ok(Self {
      file,
      offset,
      segment_id: header.segment_id,
    })
  }

  /// Get current write offset
  pub fn offset(&self) -> usize {
    self.offset
  }

  /// Get segment ID
  pub fn segment_id(&self) -> u64 {
    self.segment_id
  }

  /// Append a single record
  pub fn append(&mut self, record: &WalRecord) -> Result<usize> {
    let bytes = record.build();
    self.file.write_all(&bytes)?;
    self.offset += bytes.len();
    Ok(self.offset)
  }

  /// Append multiple records
  pub fn append_all(&mut self, records: &[WalRecord]) -> Result<usize> {
    // Combine all records into single buffer for efficient write
    let total_size: usize = records.iter().map(|r| r.estimated_size()).sum();
    let mut buffer = Vec::with_capacity(total_size);

    for record in records {
      buffer.extend_from_slice(&record.build());
    }

    self.file.write_all(&buffer)?;
    self.offset += buffer.len();
    Ok(self.offset)
  }

  /// Sync to disk
  pub fn sync(&mut self) -> Result<()> {
    self.file.sync_all()?;
    Ok(())
  }

  /// Get current position (alias for offset)
  pub fn position(&self) -> usize {
    self.offset
  }

  /// Consume the writer and return the underlying file
  pub fn into_inner(self) -> File {
    self.file
  }
}

// ============================================================================
// WAL Segment Creation
// ============================================================================

/// Create a new WAL segment file in the database directory
///
/// Creates the WAL directory if it doesn't exist, then creates a new
/// WAL segment file with the given segment ID.
pub fn create_wal_segment(db_path: &Path, segment_id: u64) -> Result<String> {
  use std::fs;

  let wal_dir = db_path.join(WAL_DIR);
  fs::create_dir_all(&wal_dir)?;

  let filename = wal_filename(segment_id);
  let wal_path = wal_dir.join(&filename);

  let _writer = WalWriter::create(&wal_path, segment_id)?;

  Ok(filename)
}

/// Generate WAL filename from segment ID
/// Format: "wal_NNNNNNNNNN.gdw"
pub fn wal_filename(segment_id: u64) -> String {
  format!("wal_{segment_id:010}.gdw")
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;
  use tempfile::tempdir;

  #[test]
  fn test_wal_header_roundtrip() {
    let header = create_wal_header(42);
    let bytes = serialize_wal_header(&header);
    let parsed = parse_wal_header(&bytes).unwrap();

    assert_eq!(parsed.magic, MAGIC_WAL);
    assert_eq!(parsed.segment_id, 42);
  }

  #[test]
  fn test_wal_writer() {
    let dir = tempdir().unwrap();
    let wal_path = dir.path().join("test.gdw");

    // Create and write
    {
      let mut writer = WalWriter::create(&wal_path, 1).unwrap();

      let record = WalRecord::new(WalRecordType::Begin, 100, Vec::new());
      writer.append(&record).unwrap();
      writer.sync().unwrap();

      assert!(writer.offset() > WAL_HEADER_SIZE);
    }

    // Reopen and verify
    {
      let writer = WalWriter::open(&wal_path).unwrap();
      assert_eq!(writer.segment_id(), 1);
      assert!(writer.offset() > WAL_HEADER_SIZE);
    }
  }
}
