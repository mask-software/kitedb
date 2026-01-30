//! Compression utilities for snapshot sections
//!
//! Supports multiple compression algorithms with automatic detection.
//! Ported from src/util/compression.ts

use crate::error::{RayError, Result};
use flate2::read::{DeflateDecoder, GzDecoder};
use flate2::write::{DeflateEncoder, GzEncoder};
use flate2::Compression;
use std::io::{Read, Write};

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
        .map_err(|e| RayError::Compression(e.to_string()))?;
      encoder
        .finish()
        .map_err(|e| RayError::Compression(e.to_string()))
    }

    CompressionType::Deflate => {
      let level = level.clamp(0, 9) as u32;
      let mut encoder = DeflateEncoder::new(Vec::new(), Compression::new(level));
      encoder
        .write_all(data)
        .map_err(|e| RayError::Compression(e.to_string()))?;
      encoder
        .finish()
        .map_err(|e| RayError::Compression(e.to_string()))
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
      let mut out = Vec::new();
      decoder
        .read_to_end(&mut out)
        .map_err(|e| RayError::Compression(e.to_string()))?;
      Ok(out)
    }

    CompressionType::Deflate => {
      let mut decoder = DeflateDecoder::new(data);
      let mut out = Vec::new();
      decoder
        .read_to_end(&mut out)
        .map_err(|e| RayError::Compression(e.to_string()))?;
      Ok(out)
    }
  }
}

/// Decompress data with known uncompressed size (more efficient allocation)
pub fn decompress_with_size(
  data: &[u8],
  compression_type: CompressionType,
  uncompressed_size: usize,
) -> Result<Vec<u8>> {
  match compression_type {
    CompressionType::None => Ok(data.to_vec()),

    CompressionType::Zstd => zstd_decode_with_size(data, uncompressed_size),

    CompressionType::Gzip => {
      let mut out = Vec::with_capacity(uncompressed_size);
      let mut decoder = GzDecoder::new(data);
      decoder
        .read_to_end(&mut out)
        .map_err(|e| RayError::Compression(e.to_string()))?;
      Ok(out)
    }

    CompressionType::Deflate => {
      let mut out = Vec::with_capacity(uncompressed_size);
      let mut decoder = DeflateDecoder::new(data);
      decoder
        .read_to_end(&mut out)
        .map_err(|e| RayError::Compression(e.to_string()))?;
      Ok(out)
    }
  }
}

/// Determine if compression is beneficial for the given data
///
/// Only compresses if:
/// 1. Data size >= minSize
/// 2. Compressed size < original size
pub fn maybe_compress(data: &[u8], options: &CompressionOptions) -> (Vec<u8>, CompressionType) {
  if !options.enabled {
    return (data.to_vec(), CompressionType::None);
  }

  if data.len() < options.min_size {
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
  zstd::encode_all(data, level).map_err(|e| RayError::Compression(e.to_string()))
}

#[cfg(target_arch = "wasm32")]
fn zstd_encode(_data: &[u8], _level: i32) -> Result<Vec<u8>> {
  Err(RayError::Compression(
    "zstd compression is not supported on wasm targets".to_string(),
  ))
}

#[cfg(not(target_arch = "wasm32"))]
fn zstd_decode(data: &[u8]) -> Result<Vec<u8>> {
  zstd::decode_all(data).map_err(|e| RayError::Compression(e.to_string()))
}

#[cfg(target_arch = "wasm32")]
fn zstd_decode(_data: &[u8]) -> Result<Vec<u8>> {
  Err(RayError::Compression(
    "zstd decompression is not supported on wasm targets".to_string(),
  ))
}

#[cfg(not(target_arch = "wasm32"))]
fn zstd_decode_with_size(data: &[u8], uncompressed_size: usize) -> Result<Vec<u8>> {
  let mut out = Vec::with_capacity(uncompressed_size);
  let mut decoder = zstd::Decoder::new(data).map_err(|e| RayError::Compression(e.to_string()))?;
  decoder
    .read_to_end(&mut out)
    .map_err(|e| RayError::Compression(e.to_string()))?;
  Ok(out)
}

#[cfg(target_arch = "wasm32")]
fn zstd_decode_with_size(_data: &[u8], _uncompressed_size: usize) -> Result<Vec<u8>> {
  Err(RayError::Compression(
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
    let compressed = compress(data, CompressionType::None, 0).unwrap();
    assert_eq!(compressed, data);
    let decompressed = decompress(&compressed, CompressionType::None).unwrap();
    assert_eq!(decompressed, data);
  }

  #[test]
  fn test_compression_zstd() {
    let data = b"hello world hello world hello world";
    let compressed = compress(data, CompressionType::Zstd, 3).unwrap();
    assert!(compressed.len() < data.len());
    let decompressed = decompress(&compressed, CompressionType::Zstd).unwrap();
    assert_eq!(decompressed, data);
  }

  #[test]
  fn test_compression_gzip() {
    let data = b"hello world hello world hello world";
    let compressed = compress(data, CompressionType::Gzip, 6).unwrap();
    let decompressed = decompress(&compressed, CompressionType::Gzip).unwrap();
    assert_eq!(decompressed, data);
  }

  #[test]
  fn test_compression_deflate() {
    let data = b"hello world hello world hello world";
    let compressed = compress(data, CompressionType::Deflate, 6).unwrap();
    let decompressed = decompress(&compressed, CompressionType::Deflate).unwrap();
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
    let compressed = compress(&data, CompressionType::Zstd, 3).unwrap();
    let decompressed =
      decompress_with_size(&compressed, CompressionType::Zstd, data.len()).unwrap();
    assert_eq!(decompressed, data);
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
