//! CRC-32 checksums, hardware accelerated when available.
//!
//! Every on-disk and wire checksum (database header, WAL records, snapshots,
//! replication frames) is CRC-32 with the IEEE 802.3 polynomial, as the
//! crc32fast crate computes it. It is not CRC-32C (Castagnoli), which the
//! functions here were once named after; changing the polynomial would make
//! every existing file fail its checksums.
//!
//! Ported from src/util/crc.ts

use crc32fast::Hasher;

/// Compute the CRC-32 (IEEE) of `data`
#[inline]
pub fn crc32(data: &[u8]) -> u32 {
  let mut hasher = Hasher::new();
  hasher.update(data);
  hasher.finalize()
}

/// Compute the CRC-32 (IEEE) of `data` in fixed-size chunks.
///
/// Useful for throughput experiments that cap per-update buffer size.
pub fn crc32_chunked(data: &[u8], chunk_size: usize) -> u32 {
  if data.is_empty() {
    return crc32(data);
  }
  if chunk_size == 0 || chunk_size >= data.len() {
    return crc32(data);
  }

  let mut hasher = Hasher::new();
  for chunk in data.chunks(chunk_size) {
    hasher.update(chunk);
  }
  hasher.finalize()
}

/// Compute the CRC-32 (IEEE) of multiple data segments, as if concatenated
pub fn crc32_multi(segments: &[&[u8]]) -> u32 {
  let mut hasher = Hasher::new();
  for segment in segments {
    hasher.update(segment);
  }
  hasher.finalize()
}

/// Verify that the CRC-32 (IEEE) of `data` is `expected`
#[inline]
pub fn verify_crc32(data: &[u8], expected: u32) -> bool {
  crc32(data) == expected
}

/// CRC-32 (IEEE) hasher for incremental computation
pub struct Crc32Hasher {
  hasher: Hasher,
}

impl Crc32Hasher {
  /// Create a new hasher
  pub fn new() -> Self {
    Self {
      hasher: Hasher::new(),
    }
  }

  /// Update the hash with more data
  #[inline]
  pub fn update(&mut self, data: &[u8]) {
    self.hasher.update(data);
  }

  /// Finalize and return the hash
  #[inline]
  pub fn finalize(self) -> u32 {
    self.hasher.finalize()
  }

  /// Reset the hasher for reuse
  pub fn reset(&mut self) {
    self.hasher = Hasher::new();
  }
}

impl Default for Crc32Hasher {
  fn default() -> Self {
    Self::new()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_crc32_empty() {
    assert_eq!(crc32(&[]), 0);
  }

  /// The check value of CRC-32 (IEEE 802.3). CRC-32C's is 0xE3069283. Every
  /// stored checksum depends on this staying put.
  #[test]
  fn test_crc32_is_ieee() {
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
  }

  #[test]
  fn test_crc32_multi() {
    let data = b"hello world";
    let single = crc32(data);
    let multi = crc32_multi(&[b"hello", b" ", b"world"]);
    assert_eq!(single, multi);
  }

  #[test]
  fn test_crc32_chunked_matches_single() {
    let data = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let single = crc32(data);
    let chunked = crc32_chunked(data, 7);
    assert_eq!(single, chunked);
  }

  #[test]
  fn test_verify_crc32() {
    let data = b"test data";
    let crc = crc32(data);
    assert!(verify_crc32(data, crc));
    assert!(!verify_crc32(data, crc + 1));
  }

  #[test]
  fn test_incremental_hasher() {
    let data = b"hello world";
    let single = crc32(data);

    let mut hasher = Crc32Hasher::new();
    hasher.update(b"hello");
    hasher.update(b" ");
    hasher.update(b"world");
    let incremental = hasher.finalize();

    assert_eq!(single, incremental);
  }
}
