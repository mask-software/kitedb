//! Multi-file manifest management
//!
//! Handles atomic updates to the manifest file which tracks the current
//! snapshot generation and WAL segment.
//! Ported from src/core/manifest.ts

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;

use crate::constants::*;
use crate::error::{RayError, Result};
use crate::types::{ManifestV1, MANIFEST_SIZE};
use crate::util::binary::*;
use crate::util::crc::crc32c;

// ============================================================================
// Manifest Creation
// ============================================================================

/// Create a new empty manifest
pub fn create_empty_manifest() -> ManifestV1 {
  ManifestV1 {
    magic: MAGIC_MANIFEST,
    version: VERSION_MANIFEST,
    min_reader_version: MIN_READER_MANIFEST,
    reserved: 0,
    active_snapshot_gen: INITIAL_SNAPSHOT_GEN,
    prev_snapshot_gen: 0,
    active_wal_seg: INITIAL_WAL_SEG,
    reserved2: [0; 5],
    crc32c: 0, // Will be computed on write
  }
}

// ============================================================================
// Serialization
// ============================================================================

/// Serialize manifest to bytes
pub fn serialize_manifest(manifest: &ManifestV1) -> Vec<u8> {
  let mut buffer = vec![0u8; MANIFEST_SIZE];

  let mut offset = 0;

  // Header
  write_u32(&mut buffer, offset, manifest.magic);
  offset += 4;
  write_u32(&mut buffer, offset, manifest.version);
  offset += 4;
  write_u32(&mut buffer, offset, manifest.min_reader_version);
  offset += 4;
  write_u32(&mut buffer, offset, manifest.reserved);
  offset += 4;

  // Snapshot and WAL info
  write_u64(&mut buffer, offset, manifest.active_snapshot_gen);
  offset += 8;
  write_u64(&mut buffer, offset, manifest.prev_snapshot_gen);
  offset += 8;
  write_u64(&mut buffer, offset, manifest.active_wal_seg);
  offset += 8;

  // Reserved u64[5]
  for i in 0..5 {
    write_u64(&mut buffer, offset, manifest.reserved2[i]);
    offset += 8;
  }

  // Compute CRC over everything except the CRC field itself
  let crc = crc32c(&buffer[..offset]);
  write_u32(&mut buffer, offset, crc);

  buffer
}

/// Parse manifest from bytes
pub fn parse_manifest(buffer: &[u8]) -> Result<ManifestV1> {
  if buffer.len() < MANIFEST_SIZE {
    return Err(RayError::InvalidSnapshot(format!(
      "Manifest too small: {} < {}",
      buffer.len(),
      MANIFEST_SIZE
    )));
  }

  let mut offset = 0;

  // Header
  let magic = read_u32(buffer, offset);
  offset += 4;
  if magic != MAGIC_MANIFEST {
    return Err(RayError::InvalidMagic {
      expected: MAGIC_MANIFEST,
      got: magic,
    });
  }

  let version = read_u32(buffer, offset);
  offset += 4;
  let min_reader_version = read_u32(buffer, offset);
  offset += 4;

  if MIN_READER_MANIFEST < min_reader_version {
    return Err(RayError::VersionMismatch {
      required: min_reader_version,
      current: MIN_READER_MANIFEST,
    });
  }

  let reserved = read_u32(buffer, offset);
  offset += 4;

  // Snapshot and WAL info
  let active_snapshot_gen = read_u64(buffer, offset);
  offset += 8;
  let prev_snapshot_gen = read_u64(buffer, offset);
  offset += 8;
  let active_wal_seg = read_u64(buffer, offset);
  offset += 8;

  // Reserved u64[5]
  let mut reserved2 = [0u64; 5];
  for slot in reserved2.iter_mut() {
    *slot = read_u64(buffer, offset);
    offset += 8;
  }

  // CRC verification
  let stored_crc = read_u32(buffer, offset);
  let computed_crc = crc32c(&buffer[..offset]);

  if stored_crc != computed_crc {
    return Err(RayError::CrcMismatch {
      stored: stored_crc,
      computed: computed_crc,
    });
  }

  Ok(ManifestV1 {
    magic,
    version,
    min_reader_version,
    reserved,
    active_snapshot_gen,
    prev_snapshot_gen,
    active_wal_seg,
    reserved2,
    crc32c: stored_crc,
  })
}

// ============================================================================
// File Operations
// ============================================================================

/// Read manifest from database path
pub fn read_manifest<P: AsRef<Path>>(db_path: P) -> Result<Option<ManifestV1>> {
  let manifest_path = db_path.as_ref().join(MANIFEST_FILENAME);

  if !manifest_path.exists() {
    return Ok(None);
  }

  let mut file = File::open(&manifest_path)?;
  let mut buffer = Vec::with_capacity(MANIFEST_SIZE);
  file.read_to_end(&mut buffer)?;

  parse_manifest(&buffer).map(Some)
}

/// Write manifest atomically using tmp + rename pattern
pub fn write_manifest<P: AsRef<Path>>(db_path: P, manifest: &ManifestV1) -> Result<()> {
  let db_path = db_path.as_ref();
  let manifest_path = db_path.join(MANIFEST_FILENAME);
  let tmp_path = db_path.join("manifest.tmp");

  let data = serialize_manifest(manifest);

  // Write to temp file
  {
    let mut file = File::create(&tmp_path)?;
    file.write_all(&data)?;
    file.sync_all()?;
  }

  // Atomic rename
  fs::rename(&tmp_path, &manifest_path)?;

  // Sync directory (best effort - not all platforms support this)
  // On macOS/Linux we can open the directory and fsync it
  #[cfg(unix)]
  {
    use std::os::unix::io::AsRawFd;
    if let Ok(dir) = File::open(db_path) {
      // fsync on directory ensures the rename is durable
      let _ = nix::unistd::fsync(dir.as_raw_fd());
    }
  }

  Ok(())
}

/// Update manifest with new snapshot generation after compaction
pub fn update_manifest_for_compaction(
  manifest: &ManifestV1,
  new_snapshot_gen: u64,
  new_wal_seg: u64,
) -> ManifestV1 {
  ManifestV1 {
    magic: manifest.magic,
    version: manifest.version,
    min_reader_version: manifest.min_reader_version,
    reserved: manifest.reserved,
    prev_snapshot_gen: manifest.active_snapshot_gen,
    active_snapshot_gen: new_snapshot_gen,
    active_wal_seg: new_wal_seg,
    reserved2: manifest.reserved2,
    crc32c: 0, // Will be recomputed on write
  }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;
  use tempfile::tempdir;

  #[test]
  fn test_create_empty_manifest() {
    let manifest = create_empty_manifest();
    assert_eq!(manifest.magic, MAGIC_MANIFEST);
    assert_eq!(manifest.version, VERSION_MANIFEST);
    assert_eq!(manifest.active_snapshot_gen, INITIAL_SNAPSHOT_GEN);
    assert_eq!(manifest.active_wal_seg, INITIAL_WAL_SEG);
  }

  #[test]
  fn test_manifest_roundtrip() {
    let manifest = create_empty_manifest();
    let bytes = serialize_manifest(&manifest);
    let parsed = parse_manifest(&bytes).unwrap();

    assert_eq!(parsed.magic, manifest.magic);
    assert_eq!(parsed.version, manifest.version);
    assert_eq!(parsed.active_snapshot_gen, manifest.active_snapshot_gen);
    assert_eq!(parsed.active_wal_seg, manifest.active_wal_seg);
  }

  #[test]
  fn test_manifest_crc_check() {
    let manifest = create_empty_manifest();
    let mut bytes = serialize_manifest(&manifest);

    // Corrupt a byte
    bytes[20] ^= 0xFF;

    let result = parse_manifest(&bytes);
    assert!(matches!(result, Err(RayError::CrcMismatch { .. })));
  }

  #[test]
  fn test_manifest_invalid_magic() {
    let mut bytes = vec![0u8; MANIFEST_SIZE];
    write_u32(&mut bytes, 0, 0xDEADBEEF); // Wrong magic

    let result = parse_manifest(&bytes);
    assert!(matches!(result, Err(RayError::InvalidMagic { .. })));
  }

  #[test]
  fn test_write_read_manifest() {
    let temp_dir = tempdir().unwrap();

    let manifest = ManifestV1 {
      magic: MAGIC_MANIFEST,
      version: VERSION_MANIFEST,
      min_reader_version: MIN_READER_MANIFEST,
      reserved: 0,
      active_snapshot_gen: 42,
      prev_snapshot_gen: 41,
      active_wal_seg: 100,
      reserved2: [0; 5],
      crc32c: 0,
    };

    write_manifest(temp_dir.path(), &manifest).unwrap();

    let loaded = read_manifest(temp_dir.path()).unwrap().unwrap();
    assert_eq!(loaded.active_snapshot_gen, 42);
    assert_eq!(loaded.prev_snapshot_gen, 41);
    assert_eq!(loaded.active_wal_seg, 100);
  }

  #[test]
  fn test_read_nonexistent_manifest() {
    let temp_dir = tempdir().unwrap();
    let result = read_manifest(temp_dir.path()).unwrap();
    assert!(result.is_none());
  }

  #[test]
  fn test_update_manifest_for_compaction() {
    let manifest = create_empty_manifest();
    let updated = update_manifest_for_compaction(&manifest, 5, 10);

    assert_eq!(updated.prev_snapshot_gen, INITIAL_SNAPSHOT_GEN);
    assert_eq!(updated.active_snapshot_gen, 5);
    assert_eq!(updated.active_wal_seg, 10);
  }
}
