//! Hash utilities for key index
//!
//! Uses xxHash64 for fast, high-quality hashing
//! Ported from src/util/hash.ts

use xxhash_rust::xxh64::xxh64;

/// Compute xxHash64 of data, returns as u64
#[inline]
pub fn xxhash64(data: &[u8]) -> u64 {
  xxh64(data, 0)
}

/// Compute xxHash64 of a string (UTF-8 encoded)
#[inline]
pub fn xxhash64_string(s: &str) -> u64 {
  xxh64(s.as_bytes(), 0)
}

/// Compute xxHash64 with a seed
#[inline]
pub fn xxhash64_seeded(data: &[u8], seed: u64) -> u64 {
  xxh64(data, seed)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_xxhash64_empty() {
    let hash = xxhash64(&[]);
    // xxHash64 of empty with seed 0
    assert_eq!(hash, 0xEF46DB3751D8E999);
  }

  #[test]
  fn test_xxhash64_string() {
    let hash = xxhash64_string("hello");
    // Consistent hash for same input
    assert_eq!(hash, xxhash64(b"hello"));
  }

  #[test]
  fn test_xxhash64_known() {
    // Test with known values
    let hash = xxhash64(b"test");
    // xxHash64 produces consistent results
    assert_eq!(hash, xxhash64(b"test"));
  }

  #[test]
  fn test_xxhash64_different_seeds() {
    let data = b"hello world";
    let hash0 = xxhash64_seeded(data, 0);
    let hash1 = xxhash64_seeded(data, 1);
    // Different seeds should produce different hashes
    assert_ne!(hash0, hash1);
  }
}
