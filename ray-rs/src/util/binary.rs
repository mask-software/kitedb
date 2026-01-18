//! Binary read/write helpers for structured I/O
//!
//! All operations are little-endian as per spec.
//! Ported from src/util/binary.ts

use crate::constants::{SECTION_ALIGNMENT, WAL_RECORD_ALIGNMENT};

// ============================================================================
// Alignment utilities
// ============================================================================

/// Round up to alignment boundary
#[inline]
pub const fn align_up(value: usize, alignment: usize) -> usize {
  (value + alignment - 1) & !(alignment - 1)
}

/// Calculate padding needed to reach alignment
#[inline]
pub const fn padding_for(value: usize, alignment: usize) -> usize {
  let remainder = value % alignment;
  if remainder == 0 {
    0
  } else {
    alignment - remainder
  }
}

/// Round up to section alignment (64 bytes)
#[inline]
pub const fn align_section(offset: usize) -> usize {
  align_up(offset, SECTION_ALIGNMENT)
}

/// Round up to WAL record alignment (8 bytes)
#[inline]
pub const fn align_wal_record(offset: usize) -> usize {
  align_up(offset, WAL_RECORD_ALIGNMENT)
}

// ============================================================================
// Read helpers (all little-endian)
// ============================================================================

/// Read u8 from byte slice at offset
#[inline]
pub fn read_u8(data: &[u8], offset: usize) -> u8 {
  data[offset]
}

/// Read u16 from byte slice at offset (little-endian)
#[inline]
pub fn read_u16(data: &[u8], offset: usize) -> u16 {
  u16::from_le_bytes([data[offset], data[offset + 1]])
}

/// Read u32 from byte slice at offset (little-endian)
#[inline]
pub fn read_u32(data: &[u8], offset: usize) -> u32 {
  u32::from_le_bytes([
    data[offset],
    data[offset + 1],
    data[offset + 2],
    data[offset + 3],
  ])
}

/// Read i32 from byte slice at offset (little-endian)
#[inline]
pub fn read_i32(data: &[u8], offset: usize) -> i32 {
  i32::from_le_bytes([
    data[offset],
    data[offset + 1],
    data[offset + 2],
    data[offset + 3],
  ])
}

/// Read u64 from byte slice at offset (little-endian)
#[inline]
pub fn read_u64(data: &[u8], offset: usize) -> u64 {
  u64::from_le_bytes([
    data[offset],
    data[offset + 1],
    data[offset + 2],
    data[offset + 3],
    data[offset + 4],
    data[offset + 5],
    data[offset + 6],
    data[offset + 7],
  ])
}

/// Read i64 from byte slice at offset (little-endian)
#[inline]
pub fn read_i64(data: &[u8], offset: usize) -> i64 {
  i64::from_le_bytes([
    data[offset],
    data[offset + 1],
    data[offset + 2],
    data[offset + 3],
    data[offset + 4],
    data[offset + 5],
    data[offset + 6],
    data[offset + 7],
  ])
}

/// Read f64 from byte slice at offset (little-endian)
#[inline]
pub fn read_f64(data: &[u8], offset: usize) -> f64 {
  f64::from_le_bytes([
    data[offset],
    data[offset + 1],
    data[offset + 2],
    data[offset + 3],
    data[offset + 4],
    data[offset + 5],
    data[offset + 6],
    data[offset + 7],
  ])
}

// ============================================================================
// Array read helpers (element-indexed)
// ============================================================================

/// Read u32 at array index (element-indexed, not byte-indexed)
#[inline]
pub fn read_u32_at(data: &[u8], index: usize) -> u32 {
  read_u32(data, index * 4)
}

/// Read i32 at array index (element-indexed)
#[inline]
pub fn read_i32_at(data: &[u8], index: usize) -> i32 {
  read_i32(data, index * 4)
}

/// Read u64 at array index (element-indexed)
#[inline]
pub fn read_u64_at(data: &[u8], index: usize) -> u64 {
  read_u64(data, index * 8)
}

// ============================================================================
// Write helpers (all little-endian)
// ============================================================================

/// Write u8 to byte slice at offset
#[inline]
pub fn write_u8(data: &mut [u8], offset: usize, value: u8) {
  data[offset] = value;
}

/// Write u16 to byte slice at offset (little-endian)
#[inline]
pub fn write_u16(data: &mut [u8], offset: usize, value: u16) {
  let bytes = value.to_le_bytes();
  data[offset] = bytes[0];
  data[offset + 1] = bytes[1];
}

/// Write u32 to byte slice at offset (little-endian)
#[inline]
pub fn write_u32(data: &mut [u8], offset: usize, value: u32) {
  let bytes = value.to_le_bytes();
  data[offset..offset + 4].copy_from_slice(&bytes);
}

/// Write i32 to byte slice at offset (little-endian)
#[inline]
pub fn write_i32(data: &mut [u8], offset: usize, value: i32) {
  let bytes = value.to_le_bytes();
  data[offset..offset + 4].copy_from_slice(&bytes);
}

/// Write u64 to byte slice at offset (little-endian)
#[inline]
pub fn write_u64(data: &mut [u8], offset: usize, value: u64) {
  let bytes = value.to_le_bytes();
  data[offset..offset + 8].copy_from_slice(&bytes);
}

/// Write i64 to byte slice at offset (little-endian)
#[inline]
pub fn write_i64(data: &mut [u8], offset: usize, value: i64) {
  let bytes = value.to_le_bytes();
  data[offset..offset + 8].copy_from_slice(&bytes);
}

/// Write f64 to byte slice at offset (little-endian)
#[inline]
pub fn write_f64(data: &mut [u8], offset: usize, value: f64) {
  let bytes = value.to_le_bytes();
  data[offset..offset + 8].copy_from_slice(&bytes);
}

// ============================================================================
// Array write helpers (element-indexed)
// ============================================================================

/// Write u32 at array index (element-indexed)
#[inline]
pub fn write_u32_at(data: &mut [u8], index: usize, value: u32) {
  write_u32(data, index * 4, value);
}

/// Write i32 at array index (element-indexed)
#[inline]
pub fn write_i32_at(data: &mut [u8], index: usize, value: i32) {
  write_i32(data, index * 4, value);
}

/// Write u64 at array index (element-indexed)
#[inline]
pub fn write_u64_at(data: &mut [u8], index: usize, value: u64) {
  write_u64(data, index * 8, value);
}

// ============================================================================
// Bitwise utilities for property encoding
// ============================================================================

/// Reinterpret f64 as u64 bits
#[inline]
pub fn f64_to_u64_bits(value: f64) -> u64 {
  value.to_bits()
}

/// Reinterpret u64 bits as f64
#[inline]
pub fn u64_bits_to_f64(bits: u64) -> f64 {
  f64::from_bits(bits)
}

// ============================================================================
// Buffer building utilities
// ============================================================================

/// Dynamic buffer builder for constructing binary data
pub struct BufferBuilder {
  data: Vec<u8>,
}

impl BufferBuilder {
  /// Create a new buffer builder with optional initial capacity
  pub fn new() -> Self {
    Self { data: Vec::new() }
  }

  /// Create a new buffer builder with specified capacity
  pub fn with_capacity(capacity: usize) -> Self {
    Self {
      data: Vec::with_capacity(capacity),
    }
  }

  /// Get current offset (total bytes written)
  #[inline]
  pub fn offset(&self) -> usize {
    self.data.len()
  }

  /// Get current length
  #[inline]
  pub fn len(&self) -> usize {
    self.data.len()
  }

  /// Check if empty
  #[inline]
  pub fn is_empty(&self) -> bool {
    self.data.is_empty()
  }

  /// Write u8
  #[inline]
  pub fn write_u8(&mut self, value: u8) -> &mut Self {
    self.data.push(value);
    self
  }

  /// Write u16 (little-endian)
  #[inline]
  pub fn write_u16(&mut self, value: u16) -> &mut Self {
    self.data.extend_from_slice(&value.to_le_bytes());
    self
  }

  /// Write u32 (little-endian)
  #[inline]
  pub fn write_u32(&mut self, value: u32) -> &mut Self {
    self.data.extend_from_slice(&value.to_le_bytes());
    self
  }

  /// Write i32 (little-endian)
  #[inline]
  pub fn write_i32(&mut self, value: i32) -> &mut Self {
    self.data.extend_from_slice(&value.to_le_bytes());
    self
  }

  /// Write u64 (little-endian)
  #[inline]
  pub fn write_u64(&mut self, value: u64) -> &mut Self {
    self.data.extend_from_slice(&value.to_le_bytes());
    self
  }

  /// Write i64 (little-endian)
  #[inline]
  pub fn write_i64(&mut self, value: i64) -> &mut Self {
    self.data.extend_from_slice(&value.to_le_bytes());
    self
  }

  /// Write f64 (little-endian)
  #[inline]
  pub fn write_f64(&mut self, value: f64) -> &mut Self {
    self.data.extend_from_slice(&value.to_le_bytes());
    self
  }

  /// Write raw bytes
  #[inline]
  pub fn write_bytes(&mut self, bytes: &[u8]) -> &mut Self {
    self.data.extend_from_slice(bytes);
    self
  }

  /// Write zeros
  #[inline]
  pub fn write_zeros(&mut self, count: usize) -> &mut Self {
    self.data.resize(self.data.len() + count, 0);
    self
  }

  /// Pad to alignment boundary with zeros
  pub fn align_to(&mut self, alignment: usize) -> &mut Self {
    let padding = padding_for(self.data.len(), alignment);
    if padding > 0 {
      self.write_zeros(padding);
    }
    self
  }

  /// Build final buffer, consuming the builder
  pub fn build(self) -> Vec<u8> {
    self.data
  }

  /// Get reference to current data
  pub fn as_slice(&self) -> &[u8] {
    &self.data
  }

  /// Get mutable reference to current data
  pub fn as_mut_slice(&mut self) -> &mut [u8] {
    &mut self.data
  }
}

impl Default for BufferBuilder {
  fn default() -> Self {
    Self::new()
  }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_align_up() {
    assert_eq!(align_up(0, 8), 0);
    assert_eq!(align_up(1, 8), 8);
    assert_eq!(align_up(7, 8), 8);
    assert_eq!(align_up(8, 8), 8);
    assert_eq!(align_up(9, 8), 16);
    assert_eq!(align_up(0, 64), 0);
    assert_eq!(align_up(63, 64), 64);
    assert_eq!(align_up(64, 64), 64);
  }

  #[test]
  fn test_padding_for() {
    assert_eq!(padding_for(0, 8), 0);
    assert_eq!(padding_for(1, 8), 7);
    assert_eq!(padding_for(7, 8), 1);
    assert_eq!(padding_for(8, 8), 0);
    assert_eq!(padding_for(9, 8), 7);
  }

  #[test]
  fn test_read_write_u32() {
    let mut buf = [0u8; 4];
    write_u32(&mut buf, 0, 0x12345678);
    assert_eq!(read_u32(&buf, 0), 0x12345678);
    // Little-endian check
    assert_eq!(buf, [0x78, 0x56, 0x34, 0x12]);
  }

  #[test]
  fn test_read_write_u64() {
    let mut buf = [0u8; 8];
    write_u64(&mut buf, 0, 0x123456789ABCDEF0);
    assert_eq!(read_u64(&buf, 0), 0x123456789ABCDEF0);
  }

  #[test]
  fn test_f64_bits() {
    let value = 3.14159265359;
    let bits = f64_to_u64_bits(value);
    let recovered = u64_bits_to_f64(bits);
    assert_eq!(value, recovered);
  }

  #[test]
  fn test_buffer_builder() {
    let mut builder = BufferBuilder::new();
    builder
      .write_u32(0x12345678)
      .write_u64(0xDEADBEEFCAFEBABE)
      .write_zeros(4)
      .align_to(8);

    let data = builder.build();
    assert_eq!(read_u32(&data, 0), 0x12345678);
    assert_eq!(read_u64(&data, 4), 0xDEADBEEFCAFEBABE);
    assert_eq!(data.len() % 8, 0); // Aligned to 8
  }
}
