//! WAL reader for file loading and recovery
//!
//! Provides functions to load WAL files from disk and recover transactions.
//! Ported from src/core/wal.ts (reading portion)

use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use crate::constants::*;
use crate::error::{RayError, Result};
use crate::types::*;
use crate::util::binary::*;

use super::record::{scan_wal, ParsedWalRecord};

// ============================================================================
// WAL Header Parsing
// ============================================================================

/// Parse WAL header from buffer
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
// WAL File Loading
// ============================================================================

/// Loaded WAL segment with header and records
#[derive(Debug)]
pub struct LoadedWalSegment {
  pub header: WalHeaderV1,
  pub records: Vec<ParsedWalRecord>,
  pub raw_data: Vec<u8>,
}

/// Load and parse a WAL segment file
pub fn load_wal_segment<P: AsRef<Path>>(filepath: P) -> Result<LoadedWalSegment> {
  let filepath = filepath.as_ref();

  // Read entire file
  let mut file = File::open(filepath)?;
  let metadata = file.metadata()?;
  let mut buffer = Vec::with_capacity(metadata.len() as usize);
  file.read_to_end(&mut buffer)?;

  // Parse header
  let header = parse_wal_header(&buffer)?;

  // Scan records
  let records = scan_wal(&buffer);

  Ok(LoadedWalSegment {
    header,
    records,
    raw_data: buffer,
  })
}

/// Load WAL segment by segment ID from database directory
pub fn load_wal_segment_by_id<P: AsRef<Path>>(
  db_path: P,
  segment_id: u64,
) -> Result<Option<LoadedWalSegment>> {
  let wal_dir = db_path.as_ref().join(WAL_DIR);
  let filename = wal_filename(segment_id);
  let filepath = wal_dir.join(filename);

  if !filepath.exists() {
    return Ok(None);
  }

  load_wal_segment(&filepath).map(Some)
}

/// List all WAL segments in database directory
pub fn list_wal_segments<P: AsRef<Path>>(db_path: P) -> Result<Vec<u64>> {
  let wal_dir = db_path.as_ref().join(WAL_DIR);

  if !wal_dir.exists() {
    return Ok(Vec::new());
  }

  let mut segments = Vec::new();

  for entry in fs::read_dir(&wal_dir)? {
    let entry = entry?;
    let filename = entry.file_name();
    let filename_str = filename.to_string_lossy();

    if let Some(seg_id) = parse_wal_seg(&filename_str) {
      segments.push(seg_id);
    }
  }

  segments.sort();
  Ok(segments)
}

/// Load all WAL segments from database directory
pub fn load_all_wal_segments<P: AsRef<Path>>(db_path: P) -> Result<Vec<LoadedWalSegment>> {
  let db_path = db_path.as_ref();
  let segment_ids = list_wal_segments(db_path)?;

  let mut segments = Vec::with_capacity(segment_ids.len());
  for seg_id in segment_ids {
    if let Some(segment) = load_wal_segment_by_id(db_path, seg_id)? {
      segments.push(segment);
    }
  }

  Ok(segments)
}

// ============================================================================
// WAL Recovery
// ============================================================================

/// Recovery result containing all committed transactions
#[derive(Debug)]
pub struct WalRecoveryResult {
  /// All committed transactions, ordered by txid
  pub committed_txids: Vec<TxId>,
  /// Maximum txid seen (for continuing sequence)
  pub max_txid: TxId,
  /// Records grouped by transaction
  pub transactions: std::collections::HashMap<TxId, Vec<ParsedWalRecord>>,
}

/// Recover transactions from a single WAL segment
pub fn recover_from_segment(segment: &LoadedWalSegment) -> WalRecoveryResult {
  use super::record::extract_committed_transactions;
  use std::collections::HashMap;

  let committed = extract_committed_transactions(&segment.records);

  // Find max txid and collect committed txids
  let mut max_txid: TxId = 0;
  let mut committed_txids: Vec<TxId> = Vec::new();

  for record in &segment.records {
    if record.txid > max_txid {
      max_txid = record.txid;
    }
  }

  // Convert to owned records
  let mut transactions: HashMap<TxId, Vec<ParsedWalRecord>> = HashMap::new();
  for (txid, records) in committed {
    committed_txids.push(txid);
    transactions.insert(txid, records.into_iter().cloned().collect());
  }

  committed_txids.sort();

  WalRecoveryResult {
    committed_txids,
    max_txid,
    transactions,
  }
}

/// Recover transactions from all WAL segments in database directory
pub fn recover_from_wal<P: AsRef<Path>>(db_path: P) -> Result<WalRecoveryResult> {
  let segments = load_all_wal_segments(db_path)?;

  let mut max_txid: TxId = 0;
  let mut all_transactions: std::collections::HashMap<TxId, Vec<ParsedWalRecord>> =
    std::collections::HashMap::new();

  for segment in &segments {
    let result = recover_from_segment(segment);

    if result.max_txid > max_txid {
      max_txid = result.max_txid;
    }

    // Merge transactions (later segments override earlier ones in case of conflict)
    for (txid, records) in result.transactions {
      all_transactions.insert(txid, records);
    }
  }

  let mut committed_txids: Vec<TxId> = all_transactions.keys().copied().collect();
  committed_txids.sort();

  Ok(WalRecoveryResult {
    committed_txids,
    max_txid,
    transactions: all_transactions,
  })
}

// ============================================================================
// WAL Validation
// ============================================================================

/// Validate a WAL file for corruption
pub fn validate_wal<P: AsRef<Path>>(filepath: P) -> Result<WalValidationResult> {
  let filepath = filepath.as_ref();

  // Read file
  let mut file = File::open(filepath)?;
  let metadata = file.metadata()?;
  let mut buffer = Vec::with_capacity(metadata.len() as usize);
  file.read_to_end(&mut buffer)?;

  // Check header
  let header_result = parse_wal_header(&buffer);
  let header_valid = header_result.is_ok();

  // Scan records and count valid/invalid
  let records = scan_wal(&buffer);
  let valid_records = records.len();

  // Check if there's truncated data at end
  let last_record_end = records
    .last()
    .map(|r| r.record_end)
    .unwrap_or(WAL_HEADER_SIZE);
  let has_trailing_data =
    last_record_end < buffer.len() && buffer[last_record_end..].iter().any(|&b| b != 0);

  Ok(WalValidationResult {
    header_valid,
    valid_records,
    file_size: metadata.len() as usize,
    has_trailing_data,
    last_valid_offset: last_record_end,
  })
}

/// Result of WAL validation
#[derive(Debug)]
pub struct WalValidationResult {
  pub header_valid: bool,
  pub valid_records: usize,
  pub file_size: usize,
  pub has_trailing_data: bool,
  pub last_valid_offset: usize,
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::core::wal::writer::{create_wal_header, serialize_wal_header};
  use std::io::Write;
  use tempfile::NamedTempFile;

  #[test]
  fn test_parse_wal_header() {
    let header = create_wal_header(42);
    let bytes = serialize_wal_header(&header);

    let parsed = parse_wal_header(&bytes).unwrap();
    assert_eq!(parsed.magic, MAGIC_WAL);
    assert_eq!(parsed.segment_id, 42);
  }

  #[test]
  fn test_parse_wal_header_invalid_magic() {
    let mut bytes = vec![0u8; WAL_HEADER_SIZE];
    write_u32(&mut bytes, 0, 0xDEADBEEF); // Wrong magic

    let result = parse_wal_header(&bytes);
    assert!(matches!(result, Err(RayError::InvalidMagic { .. })));
  }

  #[test]
  fn test_load_wal_segment() {
    // Create a temp WAL file
    let mut temp_file = NamedTempFile::new().unwrap();

    let header = create_wal_header(1);
    let header_bytes = serialize_wal_header(&header);
    temp_file.write_all(&header_bytes).unwrap();
    temp_file.flush().unwrap();

    // Load it back
    let segment = load_wal_segment(temp_file.path()).unwrap();
    assert_eq!(segment.header.segment_id, 1);
    assert!(segment.records.is_empty()); // No records written
  }

  #[test]
  fn test_list_wal_segments() {
    let temp_dir = tempfile::tempdir().unwrap();
    let wal_dir = temp_dir.path().join(WAL_DIR);
    fs::create_dir_all(&wal_dir).unwrap();

    // Create some WAL files
    for seg_id in [1, 3, 5] {
      let filename = wal_filename(seg_id);
      let filepath = wal_dir.join(filename);

      let header = create_wal_header(seg_id);
      let header_bytes = serialize_wal_header(&header);
      fs::write(&filepath, &header_bytes).unwrap();
    }

    let segments = list_wal_segments(temp_dir.path()).unwrap();
    assert_eq!(segments, vec![1, 3, 5]);
  }

  #[test]
  fn test_validate_wal() {
    let mut temp_file = NamedTempFile::new().unwrap();

    let header = create_wal_header(1);
    let header_bytes = serialize_wal_header(&header);
    temp_file.write_all(&header_bytes).unwrap();
    temp_file.flush().unwrap();

    let result = validate_wal(temp_file.path()).unwrap();
    assert!(result.header_valid);
    assert_eq!(result.valid_records, 0);
    assert!(!result.has_trailing_data);
  }
}
