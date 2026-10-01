//! Compression utilities for snapshot sections
//!
//! Supports multiple compression algorithms with automatic detection.
//! Ported from src/util/compression.ts

use crate::error::{KiteError, Result};
use flate2::read::{DeflateDecoder, GzDecoder};
use flate2::write::{DeflateEncoder, GzEncoder};
use flate2::Compression;
use std::io::{Read, Write};

/// Absolute decompressed-size ceiling for untrusted snapshot metadata.
///
/// Only compressed sections are inflated into memory, so only they are
/// capped: 4 GiB on 64-bit targets, and a lower ceiling on 32-bit and wasm32
/// targets to avoid exhausting the address space. `maybe_compress` leaves
/// larger data uncompressed, so writers never emit a section readers refuse.
/// `decompress_with_size` uses the CRC-covered declared size exactly; this
/// ceiling is enforced while validating the section table. The sizeless
/// `decompress` API uses it as its fallback bomb limit.
#[cfg(target_pointer_width = "64")]
pub(crate) const MAX_DECOMPRESSED_BYTES: usize = 4 * 1024 * 1024 * 1024;

#[cfg(not(target_pointer_width = "64"))]
pub(crate) const MAX_DECOMPRESSED_BYTES: usize = 256 * 1024 * 1024;

fn read_limited<R: Read>(reader: &mut R, limit: usize) -> Result<Vec<u8>> {
  let mut output = Vec::new();
  let mut chunk = [0u8; 8192];

  loop {
    let read = reader
      .read(&mut chunk)
      .map_err(|e| KiteError::Compression(e.to_string()))?;
    if read == 0 {
      return Ok(output);
    }
    if read > limit.saturating_sub(output.len()) {
      return Err(KiteError::Compression(format!(
        "decompressed data exceeds safe limit of {limit} bytes"
      )));
    }
    output
      .try_reserve(read)
      .map_err(|e| KiteError::Compression(format!("decompressed data allocation failed: {e}")))?;
    output.extend_from_slice(&chunk[..read]);
  }
}

// ============================================================================
// Compression Types
// ============================================================================

/// Compression algorithm identifier
/// Stored in section entry's compression field (u32)
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompressionType {
  /// No compression
  #[default]
  None = 0,
  /// Zstandard compression (default)
  Zstd = 1,
  /// Gzip compression
  Gzip = 2,
  /// Raw deflate compression
  Deflate = 3,
}

impl CompressionType {
  /// Create from u32 value
  pub fn from_u32(v: u32) -> Option<Self> {
    match v {
      0 => Some(Self::None),
      1 => Some(Self::Zstd),
      2 => Some(Self::Gzip),
      3 => Some(Self::Deflate),
      _ => None,
    }
  }

  /// Get display name
  pub fn name(&self) -> &'static str {
    match self {
      Self::None => "none",
      Self::Zstd => "zstd",
      Self::Gzip => "gzip",
      Self::Deflate => "deflate",
    }
  }
}

/// Compression options for snapshot building
#[derive(Debug, Clone)]
pub struct CompressionOptions {
  /// Enable compression (default: false for backwards compatibility)
  pub enabled: bool,
  /// Compression algorithm to use (default: ZSTD)
  pub compression_type: CompressionType,
  /// Minimum section size to compress (default: 64 bytes)
  pub min_size: usize,
  /// Compression level (zstd: 1-22, gzip/deflate: 0-9)
  pub level: i32,
}

impl Default for CompressionOptions {
  fn default() -> Self {
    Self {
      enabled: false,
      compression_type: CompressionType::Zstd,
      min_size: 64,
      level: 3,
    }
  }
}

// ============================================================================
// Compression Functions
// ============================================================================

/// Compress data using the specified algorithm
pub fn compress(data: &[u8], compression_type: CompressionType, level: i32) -> Result<Vec<u8>> {
  match compression_type {
    CompressionType::None => Ok(data.to_vec()),

    CompressionType::Zstd => zstd_encode(data, level),

    CompressionType::Gzip => {
      let level = level.clamp(0, 9) as u32;
      let mut encoder = GzEncoder::new(Vec::new(), Compression::new(level));
      encoder
        .write_all(data)
        .map_err(|e| KiteError::Compression(e.to_string()))?;
      encoder
        .finish()
        .map_err(|e| KiteError::Compression(e.to_string()))
    }

    CompressionType::Deflate => {
      let level = level.clamp(0, 9) as u32;
      let mut encoder = DeflateEncoder::new(Vec::new(), Compression::new(level));
      encoder
        .write_all(data)
        .map_err(|e| KiteError::Compression(e.to_string()))?;
      encoder
        .finish()
        .map_err(|e| KiteError::Compression(e.to_string()))
    }
  }
}

/// Decompress data using the specified algorithm
pub fn decompress(data: &[u8], compression_type: CompressionType) -> Result<Vec<u8>> {
  match compression_type {
    CompressionType::None => Ok(data.to_vec()),

    CompressionType::Zstd => zstd_decode(data),

    CompressionType::Gzip => {
      let mut decoder = GzDecoder::new(data);
      read_limited(&mut decoder, MAX_DECOMPRESSED_BYTES)
    }

    CompressionType::Deflate => {
      let mut decoder = DeflateDecoder::new(data);
      read_limited(&mut decoder, MAX_DECOMPRESSED_BYTES)
    }
  }
}

/// Decompress data with a declared uncompressed size.
///
/// The declared size is a validation hint, never an allocation request.
pub fn decompress_with_size(
  data: &[u8],
  compression_type: CompressionType,
  uncompressed_size: usize,
) -> Result<Vec<u8>> {
  let output = match compression_type {
    CompressionType::None => Ok(data.to_vec()),

    CompressionType::Zstd => zstd_decode_with_size(data, uncompressed_size),

    CompressionType::Gzip => {
      let mut decoder = GzDecoder::new(data);
      read_limited(&mut decoder, uncompressed_size)
    }

    CompressionType::Deflate => {
      let mut decoder = DeflateDecoder::new(data);
      read_limited(&mut decoder, uncompressed_size)
    }
  }?;

  if output.len() != uncompressed_size {
    return Err(KiteError::Compression(format!(
      "decompressed data size mismatch: expected {uncompressed_size} bytes, found {}",
      output.len()
    )));
  }

  Ok(output)
}

/// Whether `len` bytes are worth trying to compress. Readers refuse to
/// inflate more than `MAX_DECOMPRESSED_BYTES`, so larger data stays raw.
fn should_try_compress(len: usize, options: &CompressionOptions) -> bool {
  options.enabled && len >= options.min_size && len <= MAX_DECOMPRESSED_BYTES
}

/// Determine if compression is beneficial for the given data
///
/// Only compresses if:
/// 1. minSize <= data size <= MAX_DECOMPRESSED_BYTES
/// 2. Compressed size < original size
pub fn maybe_compress(data: &[u8], options: &CompressionOptions) -> (Vec<u8>, CompressionType) {
  if !should_try_compress(data.len(), options) {
    return (data.to_vec(), CompressionType::None);
  }

  match compress(data, options.compression_type, options.level) {
    Ok(compressed) if compressed.len() < data.len() => (compressed, options.compression_type),
    _ => (data.to_vec(), CompressionType::None),
  }
}

// ========================================================================
// Zstd helpers (native only)
// ========================================================================

#[cfg(not(target_arch = "wasm32"))]
fn zstd_encode(data: &[u8], level: i32) -> Result<Vec<u8>> {
  zstd::encode_all(data, level).map_err(|e| KiteError::Compression(e.to_string()))
}

#[cfg(target_arch = "wasm32")]
fn zstd_encode(_data: &[u8], _level: i32) -> Result<Vec<u8>> {
  Err(KiteError::Compression(
    "zstd compression is not supported on wasm targets".to_string(),
  ))
}

#[cfg(not(target_arch = "wasm32"))]
fn zstd_decode(data: &[u8]) -> Result<Vec<u8>> {
  let mut decoder = zstd::Decoder::new(data).map_err(|e| KiteError::Compression(e.to_string()))?;
  read_limited(&mut decoder, MAX_DECOMPRESSED_BYTES)
}

#[cfg(target_arch = "wasm32")]
fn zstd_decode(_data: &[u8]) -> Result<Vec<u8>> {
  Err(KiteError::Compression(
    "zstd decompression is not supported on wasm targets".to_string(),
  ))
}

#[cfg(not(target_arch = "wasm32"))]
fn zstd_decode_with_size(data: &[u8], uncompressed_size: usize) -> Result<Vec<u8>> {
  let mut decoder = zstd::Decoder::new(data).map_err(|e| KiteError::Compression(e.to_string()))?;
  read_limited(&mut decoder, uncompressed_size)
}

#[cfg(target_arch = "wasm32")]
fn zstd_decode_with_size(_data: &[u8], _uncompressed_size: usize) -> Result<Vec<u8>> {
  Err(KiteError::Compression(
    "zstd decompression is not supported on wasm targets".to_string(),
  ))
}

/// Check if a compression type value is valid
pub fn is_valid_compression_type(value: u32) -> bool {
  CompressionType::from_u32(value).is_some()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_compression_none() {
    let data = b"hello world";
    let compressed = compress(data, CompressionType::None, 0).expect("expected value");
    assert_eq!(compressed, data);
    let decompressed = decompress(&compressed, CompressionType::None).expect("expected value");
    assert_eq!(decompressed, data);
  }

  #[test]
  fn test_compression_zstd() {
    let data = b"hello world hello world hello world";
    let compressed = compress(data, CompressionType::Zstd, 3).expect("expected value");
    assert!(compressed.len() < data.len());
    let decompressed = decompress(&compressed, CompressionType::Zstd).expect("expected value");
    assert_eq!(decompressed, data);
  }

  #[test]
  fn test_compression_gzip() {
    let data = b"hello world hello world hello world";
    let compressed = compress(data, CompressionType::Gzip, 6).expect("expected value");
    let decompressed = decompress(&compressed, CompressionType::Gzip).expect("expected value");
    assert_eq!(decompressed, data);
  }

  #[test]
  fn test_compression_deflate() {
    let data = b"hello world hello world hello world";
    let compressed = compress(data, CompressionType::Deflate, 6).expect("expected value");
    let decompressed = decompress(&compressed, CompressionType::Deflate).expect("expected value");
    assert_eq!(decompressed, data);
  }

  #[test]
  fn test_maybe_compress_too_small() {
    let data = b"small";
    let options = CompressionOptions {
      enabled: true,
      min_size: 100,
      ..Default::default()
    };
    let (result, compression_type) = maybe_compress(data, &options);
    assert_eq!(compression_type, CompressionType::None);
    assert_eq!(result, data);
  }

  #[test]
  fn test_maybe_compress_disabled() {
    let data = b"hello world hello world hello world";
    let options = CompressionOptions {
      enabled: false,
      ..Default::default()
    };
    let (result, compression_type) = maybe_compress(data, &options);
    assert_eq!(compression_type, CompressionType::None);
    assert_eq!(result, data);
  }

  #[test]
  fn test_sections_readers_cannot_inflate_stay_uncompressed() {
    let options = CompressionOptions {
      enabled: true,
      ..Default::default()
    };
    assert!(should_try_compress(MAX_DECOMPRESSED_BYTES, &options));
    assert!(!should_try_compress(MAX_DECOMPRESSED_BYTES + 1, &options));
    assert!(!should_try_compress(usize::MAX, &options));
  }

  #[test]
  fn test_maybe_compress_compressible() {
    let data = vec![b'a'; 1000]; // Highly compressible
    let options = CompressionOptions {
      enabled: true,
      compression_type: CompressionType::Zstd,
      min_size: 64,
      level: 3,
    };
    let (result, compression_type) = maybe_compress(&data, &options);
    assert_eq!(compression_type, CompressionType::Zstd);
    assert!(result.len() < data.len());
  }

  #[test]
  fn test_decompress_with_size() {
    let data = vec![b'x'; 10000];
    let compressed = compress(&data, CompressionType::Zstd, 3).expect("expected value");
    let decompressed =
      decompress_with_size(&compressed, CompressionType::Zstd, data.len()).expect("expected value");
    assert_eq!(decompressed, data);
  }

  #[test]
  fn test_decompress_with_size_rejects_size_mismatch() {
    let data = vec![b'x'; 10_000];
    let compressed = compress(&data, CompressionType::Zstd, 3).expect("expected value");

    let shorter_declaration =
      decompress_with_size(&compressed, CompressionType::Zstd, data.len() - 1);
    assert!(shorter_declaration.is_err());

    let longer_declaration =
      decompress_with_size(&compressed, CompressionType::Zstd, data.len() + 1);
    assert!(longer_declaration.is_err());
  }

  #[test]
  fn test_compression_type_from_u32() {
    assert_eq!(CompressionType::from_u32(0), Some(CompressionType::None));
    assert_eq!(CompressionType::from_u32(1), Some(CompressionType::Zstd));
    assert_eq!(CompressionType::from_u32(2), Some(CompressionType::Gzip));
    assert_eq!(CompressionType::from_u32(3), Some(CompressionType::Deflate));
    assert_eq!(CompressionType::from_u32(4), None);
  }
}
