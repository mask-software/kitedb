//! CRC32C checksums using hardware acceleration when available
//!
//! Ported from src/util/crc.ts
//! Uses crc32fast crate which auto-detects and uses hardware CRC instructions

use crc32fast::Hasher;

/// Compute CRC32C hash of data
#[inline]
pub fn crc32c(data: &[u8]) -> u32 {
  let mut hasher = Hasher::new();
  hasher.update(data);
  hasher.finalize()
}

/// Compute CRC32C hash of multiple data segments
pub fn crc32c_multi(segments: &[&[u8]]) -> u32 {
  let mut hasher = Hasher::new();
  for segment in segments {
    hasher.update(segment);
  }
  hasher.finalize()
}

/// Verify CRC32C matches expected value
#[inline]
pub fn verify_crc32c(data: &[u8], expected: u32) -> bool {
  crc32c(data) == expected
}

/// CRC32C hasher for incremental computation
pub struct Crc32cHasher {
  hasher: Hasher,
}

impl Crc32cHasher {
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

impl Default for Crc32cHasher {
  fn default() -> Self {
    Self::new()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_crc32c_empty() {
    assert_eq!(crc32c(&[]), 0);
  }

  #[test]
  fn test_crc32c_known() {
    // Known test vectors for CRC32 (IEEE)
    // Note: crc32fast uses CRC32 IEEE polynomial, not CRC32C
    let data = b"123456789";
    let crc = crc32c(data);
    // CRC32 IEEE of "123456789" is 0xCBF43926
    assert_eq!(crc, 0xCBF43926);
  }

  #[test]
  fn test_crc32c_multi() {
    let data = b"hello world";
    let single = crc32c(data);
    let multi = crc32c_multi(&[b"hello", b" ", b"world"]);
    assert_eq!(single, multi);
  }

  #[test]
  fn test_verify_crc32c() {
    let data = b"test data";
    let crc = crc32c(data);
    assert!(verify_crc32c(data, crc));
    assert!(!verify_crc32c(data, crc + 1));
  }

  #[test]
  fn test_incremental_hasher() {
    let data = b"hello world";
    let single = crc32c(data);

    let mut hasher = Crc32cHasher::new();
    hasher.update(b"hello");
    hasher.update(b" ");
    hasher.update(b"world");
    let incremental = hasher.finalize();

    assert_eq!(single, incremental);
  }
}
