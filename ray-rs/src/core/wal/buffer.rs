//! Circular WAL buffer for single-file format with dual-region support
//!
//! Ported from src/core/wal-buffer.ts
//!
//! The WAL uses a circular buffer design within the database file.
//! Records wrap around when reaching the end of the WAL area.
//! Checkpoint advances the tail pointer to reclaim space.
//!
//! Dual-Region Mode (for background checkpointing):
//! - Primary region: 75% of WAL space (normal writes)
//! - Secondary region: 25% of WAL space (writes during checkpoint)
//!
//! Optimization: Uses page-level write batching to reduce I/O amplification.
//! Instead of writing each small record individually (causing read-modify-write
//! for each ~100 byte record on a 4KB page), we buffer writes in memory and
//! flush entire pages at once.

use std::collections::HashMap;

use crate::constants::*;
use crate::core::pager::FilePager;
use crate::error::{RayError, Result};
use crate::types::*;
use crate::util::binary::*;

use super::record::{parse_wal_record, ParsedWalRecord, WalRecord};

/// Skip marker magic value (recLen=0 followed by this)
const SKIP_MARKER_MAGIC: u32 = 0xFFFFFFFF;

/// WAL region split ratio: primary gets 75%, secondary gets 25%
const PRIMARY_REGION_RATIO: f64 = 0.75;

/// Circular WAL buffer for single-file database format
pub struct WalBuffer {
  /// Base offset in file (start of WAL area)
  base_offset: u64,
  /// Total size of WAL area in bytes
  capacity: u64,
  /// Current head position (write pointer, relative to base)
  head: u64,
  /// Current tail position (oldest valid record, relative to base)
  tail: u64,
  /// Whether buffer has wrapped around
  wrapped: bool,
  /// Page size for batching
  page_size: usize,
  /// Pending page writes (page_offset -> page_data)
  /// page_offset is absolute file offset
  pending_writes: HashMap<u64, Vec<u8>>,

  // Dual-region support for background checkpointing
  /// Size of primary region (75%)
  primary_region_size: u64,
  /// Start offset of secondary region (relative to base)
  secondary_region_start: u64,
  /// Size of secondary region (25%)
  secondary_region_size: u64,
  /// Active region: 0=primary, 1=secondary
  active_region: u8,
  /// Primary region write position (relative to base)
  primary_head: u64,
  /// Secondary region write position (relative to base)
  secondary_head: u64,
}

impl WalBuffer {
  /// Create a new WAL buffer
  pub fn new(base_offset: u64, capacity: u64, page_size: usize) -> Self {
    let primary_region_size = (capacity as f64 * PRIMARY_REGION_RATIO) as u64;
    let secondary_region_start = primary_region_size;
    let secondary_region_size = capacity - primary_region_size;

    Self {
      base_offset,
      capacity,
      head: 0,
      tail: 0,
      wrapped: false,
      page_size,
      pending_writes: HashMap::new(),
      primary_region_size,
      secondary_region_start,
      secondary_region_size,
      active_region: 0,
      primary_head: 0,
      secondary_head: secondary_region_start,
    }
  }

  /// Create from existing header state
  pub fn from_header(header: &DbHeaderV1) -> Self {
    let base_offset = header.wal_start_page * header.page_size as u64;
    let capacity = header.wal_page_count * header.page_size as u64;

    let primary_region_size = (capacity as f64 * PRIMARY_REGION_RATIO) as u64;
    let secondary_region_start = primary_region_size;
    let secondary_region_size = capacity - primary_region_size;

    // Initialize from V2 header fields
    let active_region = header.active_wal_region;
    let mut primary_head = header.wal_primary_head;
    let mut secondary_head = header.wal_secondary_head;

    // Backward compatibility: if V2 fields are 0 and head is non-zero,
    // initialize primaryHead from head
    if primary_head == 0 && header.wal_head > 0 {
      primary_head = header.wal_head;
    }

    // Initialize secondaryHead to its start position if not set
    if secondary_head == 0 {
      secondary_head = secondary_region_start;
    }

    // If we were writing to secondary and the header update was interrupted,
    // wal_head may be ahead of wal_secondary_head. Use wal_head as fallback.
    if active_region == 1
      && secondary_head <= secondary_region_start
      && header.wal_head >= secondary_region_start
    {
      secondary_head = header.wal_head;
    }

    Self {
      base_offset,
      capacity,
      head: header.wal_head,
      tail: header.wal_tail,
      wrapped: header.wal_head < header.wal_tail,
      page_size: header.page_size as usize,
      pending_writes: HashMap::new(),
      primary_region_size,
      secondary_region_start,
      secondary_region_size,
      active_region,
      primary_head,
      secondary_head,
    }
  }

  /// Get the base offset in the file
  pub fn base_offset(&self) -> u64 {
    self.base_offset
  }

  /// Get the capacity
  pub fn capacity(&self) -> u64 {
    self.capacity
  }

  /// Get current head position (relative to base)
  pub fn head(&self) -> u64 {
    self.head
  }

  /// Get current tail position (relative to base)
  pub fn tail(&self) -> u64 {
    self.tail
  }

  /// Get primary region head (relative to base)
  pub fn primary_head(&self) -> u64 {
    self.primary_head
  }

  /// Get secondary region head (relative to base)
  pub fn secondary_head(&self) -> u64 {
    self.secondary_head
  }

  /// Get active region (0=primary, 1=secondary)
  pub fn active_region(&self) -> u8 {
    self.active_region
  }

  /// Get primary region size in bytes
  pub fn primary_region_size(&self) -> u64 {
    self.primary_region_size
  }

  /// Get secondary region size in bytes
  pub fn secondary_region_size(&self) -> u64 {
    self.secondary_region_size
  }

  /// Check if buffer is empty
  pub fn is_empty(&self) -> bool {
    self.head == self.tail && !self.wrapped
  }

  /// Get used space (for active region in dual-region mode)
  pub fn used(&self) -> u64 {
    if self.active_region == 0 {
      // Primary region: simple linear usage
      self.primary_head - self.tail
    } else {
      // Secondary region: usage is just secondary head minus start
      self.secondary_head - self.secondary_region_start
    }
  }

  /// Get free space in active region
  pub fn free(&self) -> u64 {
    if self.active_region == 0 {
      // Primary region available space
      self
        .primary_region_size
        .saturating_sub(self.primary_head + 1)
    } else {
      // Secondary region available space
      self
        .secondary_region_size
        .saturating_sub(self.secondary_head - self.secondary_region_start + 1)
    }
  }

  /// Get usage ratio (0.0 - 1.0) for active region
  pub fn usage_ratio(&self) -> f64 {
    if self.active_region == 0 {
      self.primary_head as f64 / self.primary_region_size as f64
    } else {
      (self.secondary_head - self.secondary_region_start) as f64 / self.secondary_region_size as f64
    }
  }

  /// Check if we can fit a record of given size in active region
  pub fn can_fit(&self, size: usize) -> bool {
    let aligned_size = align_up(size, WAL_RECORD_ALIGNMENT) as u64;
    aligned_size <= self.free()
  }

  /// Check if writing would require wrap-around (only relevant for primary region)
  pub fn would_wrap(&self, size: usize) -> bool {
    if self.active_region == 1 {
      // Secondary region doesn't wrap
      return false;
    }
    let aligned_size = align_up(size, WAL_RECORD_ALIGNMENT) as u64;
    self.primary_head + aligned_size > self.primary_region_size
  }

  // ========================================================================
  // Dual-Region Methods (for background checkpointing)
  // ========================================================================

  /// Switch writes to secondary region (called when starting background checkpoint)
  pub fn switch_to_secondary(&mut self) {
    if self.active_region == 1 {
      return; // Already in secondary
    }
    self.active_region = 1;
    // Update head to track active position
    self.head = self.secondary_head;
  }

  /// Switch writes back to primary region (called after checkpoint completes)
  /// If reset_primary is true, resets the primary region (checkpoint completed)
  pub fn switch_to_primary(&mut self, reset_primary: bool) {
    if self.active_region == 0 && !reset_primary {
      return; // Already in primary and no reset needed
    }
    self.active_region = 0;
    if reset_primary {
      // Reset primary head (checkpoint completed, WAL is cleared)
      self.primary_head = 0;
      self.tail = 0;
    }
    // Update head to track active position
    self.head = self.primary_head;
  }

  /// Merge secondary region records into primary region
  /// Called after checkpoint completes to preserve any writes that occurred during checkpoint
  pub fn merge_secondary_into_primary(&mut self, pager: &mut FilePager) -> Result<()> {
    // Read all records from secondary region (if any)
    let has_secondary_records = self.secondary_head > self.secondary_region_start;
    let secondary_records = if has_secondary_records {
      self.scan_region(1, pager)?
    } else {
      Vec::new()
    };

    // Reset both regions - this must happen even if no secondary records exist,
    // because checkpoint has incorporated all primary WAL data into the snapshot
    self.primary_head = 0;
    self.secondary_head = self.secondary_region_start;
    self.tail = 0;
    self.active_region = 0;
    self.head = 0;
    self.wrapped = false;

    // Re-write secondary records to primary region
    for record in secondary_records {
      // Rebuild the record and write it
      let wal_record = WalRecord::new(record.record_type, record.txid, record.payload.clone());
      let record_bytes = wal_record.build();
      self.write_record_bytes_to_primary(&record_bytes, pager)?;
    }

    Ok(())
  }

  /// Recover an incomplete background checkpoint by merging primary + secondary
  /// WAL records into a fresh primary region. This preserves all committed
  /// records when a crash occurs mid-checkpoint.
  pub fn recover_incomplete_checkpoint(&mut self, pager: &mut FilePager) -> Result<()> {
    let primary_records = self.scan_region(0, pager)?;
    let secondary_records = self.scan_region(1, pager)?;

    // Reset both regions to a clean primary state
    self.primary_head = 0;
    self.secondary_head = self.secondary_region_start;
    self.tail = 0;
    self.active_region = 0;
    self.head = 0;
    self.wrapped = false;

    for record in primary_records
      .into_iter()
      .chain(secondary_records.into_iter())
    {
      let wal_record = WalRecord::new(record.record_type, record.txid, record.payload.clone());
      let record_bytes = wal_record.build();
      self.write_record_bytes_to_primary(&record_bytes, pager)?;
    }

    Ok(())
  }

  /// Scan records from a specific region
  /// region: 0 for primary, 1 for secondary
  pub fn scan_region(&mut self, region: u8, pager: &mut FilePager) -> Result<Vec<ParsedWalRecord>> {
    let mut records = Vec::new();

    let (mut pos, end_pos, region_start) = if region == 0 {
      (self.tail, self.primary_head, 0u64)
    } else {
      (
        self.secondary_region_start,
        self.secondary_head,
        self.secondary_region_start,
      )
    };

    while pos < end_pos {
      let file_offset = self.file_offset(pos);
      let header_bytes = self.read_at_offset(file_offset, 8, pager)?;

      let rec_len = read_u32(&header_bytes, 0) as usize;

      // Check for skip marker
      if rec_len == 0 {
        let marker = read_u32(&header_bytes, 4);
        if marker == SKIP_MARKER_MAGIC {
          // Skip to start of region
          pos = region_start;
          continue;
        }
        // Invalid record
        break;
      }

      // Calculate total record size with alignment
      let pad_len = padding_for(rec_len, WAL_RECORD_ALIGNMENT);
      let total_len = rec_len + pad_len;

      let record_bytes = self.read_at_offset(file_offset, total_len, pager)?;

      // Parse the record
      match parse_wal_record(&record_bytes, 0) {
        Some(record) => {
          records.push(record);
          pos += total_len as u64;
        }
        None => break, // Invalid record
      }
    }

    Ok(records)
  }

  /// Write record bytes specifically to primary region (used during merge)
  fn write_record_bytes_to_primary(
    &mut self,
    record_bytes: &[u8],
    pager: &mut FilePager,
  ) -> Result<u64> {
    let record_size = record_bytes.len();
    let aligned_size = align_up(record_size, WAL_RECORD_ALIGNMENT);

    // Check if fits in primary region
    if self.primary_head + aligned_size as u64 > self.primary_region_size {
      return Err(RayError::WalBufferFull);
    }

    // Calculate file offset
    let file_offset = self.file_offset(self.primary_head);

    // Buffer the write
    self.buffer_write(file_offset, record_bytes, pager)?;

    // Update primary head
    self.primary_head += aligned_size as u64;
    self.head = self.primary_head;

    Ok(self.primary_head)
  }

  /// Calculate the file offset for a buffer-relative position
  pub fn file_offset(&self, buffer_pos: u64) -> u64 {
    self.base_offset + buffer_pos
  }

  /// Reserve space for a record, returning the write position
  /// Returns None if buffer is full
  pub fn reserve(&mut self, size: usize) -> Option<u64> {
    let aligned_size = align_up(size, WAL_RECORD_ALIGNMENT) as u64;

    if !self.can_fit(aligned_size as usize) {
      return None;
    }

    if self.active_region == 0 {
      // Primary region
      let write_pos = self.primary_head;

      // Check if we need to wrap
      if self.primary_head + aligned_size > self.primary_region_size {
        // Need to wrap around
        if self.tail <= aligned_size {
          // Can't fit even at the start
          return None;
        }
        self.primary_head = aligned_size;
        self.wrapped = true;
      } else {
        self.primary_head += aligned_size;
      }
      self.head = self.primary_head;
      Some(write_pos)
    } else {
      // Secondary region (no wrap)
      let write_pos = self.secondary_head;
      self.secondary_head += aligned_size;
      self.head = self.secondary_head;
      Some(write_pos)
    }
  }

  /// Write a WAL record to the buffer
  /// Returns the new head position
  ///
  /// Note: Records are buffered in memory. Call flush() to write to disk.
  pub fn write_record(&mut self, record: &WalRecord, pager: &mut FilePager) -> Result<u64> {
    let record_bytes = record.build();
    self.write_record_bytes(&record_bytes, pager)
  }

  /// Write raw record bytes to the active region
  fn write_record_bytes(&mut self, record_bytes: &[u8], pager: &mut FilePager) -> Result<u64> {
    let record_size = record_bytes.len();
    let aligned_size = align_up(record_size, WAL_RECORD_ALIGNMENT);

    if !self.can_fit(aligned_size) {
      return Err(RayError::WalBufferFull);
    }

    if self.active_region == 0 {
      // Primary region
      if self.would_wrap(aligned_size) {
        // Write a skip marker at current position and wrap to start
        self.write_skip_marker(pager)?;
        self.primary_head = 0;
      }

      // Calculate file offset
      let file_offset = self.file_offset(self.primary_head);

      // Buffer the write
      self.buffer_write(file_offset, record_bytes, pager)?;

      // Update head
      self.primary_head += aligned_size as u64;
      self.head = self.primary_head;
    } else {
      // Secondary region (no wrap-around)
      let file_offset = self.file_offset(self.secondary_head);

      // Buffer the write
      self.buffer_write(file_offset, record_bytes, pager)?;

      // Update head
      self.secondary_head += aligned_size as u64;
      self.head = self.secondary_head;
    }

    Ok(self.head)
  }

  /// Write a skip marker to indicate end of valid data before wrap
  fn write_skip_marker(&mut self, pager: &mut FilePager) -> Result<()> {
    // A skip marker is: recLen = 0 (4 bytes) + magic = 0xFFFFFFFF (4 bytes)
    let mut marker = [0u8; 8];
    write_u32(&mut marker, 0, 0); // recLen = 0 means skip
    write_u32(&mut marker, 4, SKIP_MARKER_MAGIC);

    let file_offset = self.file_offset(self.head);
    self.buffer_write(file_offset, &marker, pager)?;

    Ok(())
  }

  /// Buffer a write for later flushing (page-level batching)
  /// This reduces I/O amplification by accumulating writes to the same page
  fn buffer_write(&mut self, offset: u64, data: &[u8], pager: &mut FilePager) -> Result<()> {
    let page_size = self.page_size as u64;
    let start_page = offset / page_size;
    let end_page = (offset + data.len() as u64 - 1) / page_size;

    let mut data_offset = 0usize;

    for page_idx in start_page..=end_page {
      let page_file_offset = page_idx * page_size;

      // Get or create the page buffer
      let page_buffer = if let Some(buffer) = self.pending_writes.get_mut(&page_file_offset) {
        buffer
      } else {
        // First write to this page - load existing content
        let page_num = (page_file_offset / page_size) as u32;
        let existing = pager.read_page(page_num)?;
        self.pending_writes.insert(page_file_offset, existing);
        self.pending_writes.get_mut(&page_file_offset).unwrap()
      };

      let page_start = page_file_offset;
      let page_end = page_start + page_size;

      let write_start = offset.max(page_start);
      let write_end = (offset + data.len() as u64).min(page_end);
      let write_len = (write_end - write_start) as usize;

      let page_write_offset = (write_start - page_start) as usize;

      page_buffer[page_write_offset..page_write_offset + write_len]
        .copy_from_slice(&data[data_offset..data_offset + write_len]);

      data_offset += write_len;
    }

    Ok(())
  }

  /// Read bytes from a specific file offset
  /// If there are pending writes, reads from the buffered data
  fn read_at_offset(&self, offset: u64, length: usize, pager: &mut FilePager) -> Result<Vec<u8>> {
    let page_size = self.page_size as u64;
    let start_page = offset / page_size;
    let end_page = (offset + length as u64 - 1) / page_size;

    // For reads within a single page
    if start_page == end_page {
      let page_file_offset = start_page * page_size;
      let page_offset = (offset - page_file_offset) as usize;

      // Check for pending writes first
      if let Some(pending_page) = self.pending_writes.get(&page_file_offset) {
        return Ok(pending_page[page_offset..page_offset + length].to_vec());
      }

      let page_num = start_page as u32;
      let page = pager.read_page(page_num)?;
      return Ok(page[page_offset..page_offset + length].to_vec());
    }

    // For reads spanning multiple pages
    let mut result = vec![0u8; length];
    let mut result_offset = 0;

    for page_idx in start_page..=end_page {
      let page_file_offset = page_idx * page_size;
      let page_start = page_file_offset;
      let page_end = page_start + page_size;

      let read_start = offset.max(page_start);
      let read_end = (offset + length as u64).min(page_end);
      let read_len = (read_end - read_start) as usize;

      let page_read_offset = (read_start - page_start) as usize;

      // Check for pending writes first
      let page_data = if let Some(pending) = self.pending_writes.get(&page_file_offset) {
        pending.clone()
      } else {
        let page_num = page_idx as u32;
        pager.read_page(page_num)?
      };

      result[result_offset..result_offset + read_len]
        .copy_from_slice(&page_data[page_read_offset..page_read_offset + read_len]);

      result_offset += read_len;
    }

    Ok(result)
  }

  /// Flush all pending writes to disk
  /// This writes all buffered pages in a single batch
  pub fn flush(&mut self, pager: &mut FilePager) -> Result<()> {
    let page_size = self.page_size as u64;

    for (&page_file_offset, data) in &self.pending_writes {
      let page_num = (page_file_offset / page_size) as u32;
      pager.write_page(page_num, data)?;
    }

    self.pending_writes.clear();
    Ok(())
  }

  /// Flush and sync to disk
  pub fn sync(&mut self, pager: &mut FilePager) -> Result<()> {
    self.flush(pager)?;
    pager.sync()?;
    Ok(())
  }

  /// Check if there are pending writes
  pub fn has_pending_writes(&self) -> bool {
    !self.pending_writes.is_empty()
  }

  /// Advance tail after checkpoint
  pub fn advance_tail(&mut self, new_tail: u64) {
    self.tail = new_tail;
    if self.tail >= self.head {
      self.wrapped = false;
    }
  }

  /// Reset the buffer (after checkpoint)
  pub fn reset(&mut self) {
    self.head = 0;
    self.tail = 0;
    self.wrapped = false;
    self.pending_writes.clear();
    // Also reset dual-region state
    self.primary_head = 0;
    self.secondary_head = self.secondary_region_start;
    self.active_region = 0;
  }

  /// Clear pending writes without flushing
  pub fn discard_pending(&mut self) {
    self.pending_writes.clear();
  }

  /// Scan all valid records from tail to head
  pub fn scan_records(&mut self, pager: &mut FilePager) -> Result<Vec<ParsedWalRecord>> {
    let mut records = Vec::new();

    if self.is_empty() {
      return Ok(records);
    }

    let mut pos = self.tail;

    loop {
      // Check if we've reached head
      if !self.wrapped && pos >= self.head {
        break;
      }
      if self.wrapped && pos >= self.capacity {
        pos = 0;
        continue;
      }
      if self.wrapped && pos >= self.head && pos < self.tail {
        break;
      }

      // Read the record header
      let file_offset = self.file_offset(pos);
      let header_bytes = self.read_at_offset(file_offset, 8, pager)?;

      let rec_len = read_u32(&header_bytes, 0) as usize;

      // Check for skip marker
      if rec_len == 0 {
        let marker = read_u32(&header_bytes, 4);
        if marker == SKIP_MARKER_MAGIC {
          // Skip to start
          pos = 0;
          continue;
        }
        // Invalid record
        break;
      }

      // Calculate total record size with alignment
      let pad_len = padding_for(rec_len, WAL_RECORD_ALIGNMENT);
      let total_len = rec_len + pad_len;

      // Read full record
      let record_bytes = self.read_at_offset(file_offset, total_len, pager)?;

      // Parse the record
      match parse_wal_record(&record_bytes, 0) {
        Some(record) => {
          records.push(record);
          pos += total_len as u64;
        }
        None => break, // Invalid record
      }
    }

    Ok(records)
  }

  /// Get statistics about the WAL buffer
  pub fn stats(&self) -> WalBufferStats {
    WalBufferStats {
      capacity: self.capacity,
      used: self.used(),
      free: self.free(),
      head: self.head,
      tail: self.tail,
      wrapped: self.wrapped,
      pending_pages: self.pending_writes.len(),
      primary_head: self.primary_head,
      secondary_head: self.secondary_head,
      active_region: self.active_region,
    }
  }

  /// Get records for recovery (from both regions if checkpoint was in progress)
  pub fn get_records_for_recovery(
    &mut self,
    pager: &mut FilePager,
  ) -> Result<Vec<ParsedWalRecord>> {
    // Scan primary region first
    let mut records = self.scan_region(0, pager)?;

    // If checkpoint was in progress (secondary region has data), include those too
    if self.secondary_head > self.secondary_region_start {
      let secondary_records = self.scan_region(1, pager)?;
      records.extend(secondary_records);
    }

    Ok(records)
  }
}

/// WAL buffer statistics
#[derive(Debug, Clone)]
pub struct WalBufferStats {
  pub capacity: u64,
  pub used: u64,
  pub free: u64,
  pub head: u64,
  pub tail: u64,
  pub wrapped: bool,
  pub pending_pages: usize,
  pub primary_head: u64,
  pub secondary_head: u64,
  pub active_region: u8,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;
  use crate::core::pager::create_pager;
  use crate::core::wal::record::build_create_node_payload;
  use tempfile::NamedTempFile;

  fn create_test_pager() -> (FilePager, tempfile::NamedTempFile) {
    let temp_file = NamedTempFile::new().unwrap();
    let mut pager = create_pager(temp_file.path(), 4096).unwrap();
    // Pre-allocate some pages for WAL
    pager.allocate_pages(10).unwrap();
    (pager, temp_file)
  }

  #[test]
  fn test_wal_buffer_new() {
    let buffer = WalBuffer::new(4096, 1024 * 1024, 4096);
    assert!(buffer.is_empty());
    assert_eq!(buffer.capacity(), 1024 * 1024);
    assert_eq!(buffer.used(), 0);
  }

  #[test]
  fn test_wal_buffer_reserve() {
    let mut buffer = WalBuffer::new(4096, 1024, 4096);

    // Reserve some space
    let pos = buffer.reserve(100).unwrap();
    assert_eq!(pos, 0);
    assert!(!buffer.is_empty());

    // Reserve more
    let pos2 = buffer.reserve(100).unwrap();
    assert!(pos2 > pos);
  }

  #[test]
  fn test_wal_buffer_full() {
    // With dual-region, primary gets 75% = 384 bytes
    // Each 100-byte reservation aligns to 104 bytes
    // 3 reservations = 312 bytes, 4th would be 416 > 384 (but need to leave 1 byte)
    let mut buffer = WalBuffer::new(4096, 512, 4096);

    // Fill up primary region
    buffer.reserve(100).unwrap(); // 104 bytes
    buffer.reserve(100).unwrap(); // 208 bytes
    buffer.reserve(100).unwrap(); // 312 bytes

    // Should fail now (need 104, only ~70 left in primary)
    assert!(buffer.reserve(100).is_none());
  }

  #[test]
  fn test_wal_buffer_reset() {
    let mut buffer = WalBuffer::new(4096, 1024, 4096);
    buffer.reserve(500).unwrap();
    assert!(!buffer.is_empty());

    buffer.reset();
    assert!(buffer.is_empty());
    assert_eq!(buffer.head(), 0);
    assert_eq!(buffer.tail(), 0);
  }

  #[test]
  fn test_wal_buffer_write_record() {
    let (mut pager, _temp) = create_test_pager();

    // Create WAL buffer starting at page 1 (offset 4096), size 4 pages
    let mut buffer = WalBuffer::new(4096, 4 * 4096, 4096);

    // Write a record
    let record = WalRecord::new(
      WalRecordType::CreateNode,
      1,
      build_create_node_payload(100, Some("test_key")),
    );

    let new_head = buffer.write_record(&record, &mut pager).unwrap();
    assert!(new_head > 0);
    assert!(buffer.has_pending_writes());

    // Flush to disk
    buffer.flush(&mut pager).unwrap();
    assert!(!buffer.has_pending_writes());
  }

  #[test]
  fn test_wal_buffer_write_and_scan() {
    let (mut pager, _temp) = create_test_pager();

    // Create WAL buffer
    let mut buffer = WalBuffer::new(4096, 4 * 4096, 4096);

    // Write multiple records
    for i in 0..5 {
      let record = WalRecord::new(
        WalRecordType::CreateNode,
        i,
        build_create_node_payload(100 + i, None),
      );
      buffer.write_record(&record, &mut pager).unwrap();
    }

    // Flush
    buffer.flush(&mut pager).unwrap();

    // Scan records
    let records = buffer.scan_records(&mut pager).unwrap();
    assert_eq!(records.len(), 5);

    for (i, record) in records.iter().enumerate() {
      assert_eq!(record.txid, i as u64);
      assert_eq!(record.record_type, WalRecordType::CreateNode);
    }
  }

  #[test]
  fn test_wal_buffer_stats() {
    let mut buffer = WalBuffer::new(4096, 1024, 4096);
    buffer.reserve(100).unwrap();

    let stats = buffer.stats();
    assert_eq!(stats.capacity, 1024);
    assert!(stats.used > 0);
    assert!(!stats.wrapped);
  }

  #[test]
  fn test_wal_buffer_discard_pending() {
    let (mut pager, _temp) = create_test_pager();
    let mut buffer = WalBuffer::new(4096, 4 * 4096, 4096);

    let record = WalRecord::new(WalRecordType::Begin, 1, Vec::new());
    buffer.write_record(&record, &mut pager).unwrap();
    assert!(buffer.has_pending_writes());

    buffer.discard_pending();
    assert!(!buffer.has_pending_writes());
  }

  #[test]
  fn test_dual_region_switch() {
    let (mut pager, _temp) = create_test_pager();
    let mut buffer = WalBuffer::new(4096, 4 * 4096, 4096);

    // Initially in primary region
    assert_eq!(buffer.active_region(), 0);

    // Write to primary
    let record1 = WalRecord::new(WalRecordType::Begin, 1, Vec::new());
    buffer.write_record(&record1, &mut pager).unwrap();
    buffer.flush(&mut pager).unwrap();

    let primary_head_before = buffer.primary_head();
    assert!(primary_head_before > 0);

    // Switch to secondary
    buffer.switch_to_secondary();
    assert_eq!(buffer.active_region(), 1);

    // Write to secondary
    let record2 = WalRecord::new(WalRecordType::Begin, 2, Vec::new());
    buffer.write_record(&record2, &mut pager).unwrap();
    buffer.flush(&mut pager).unwrap();

    // Primary head should be unchanged
    assert_eq!(buffer.primary_head(), primary_head_before);
    // Secondary head should have advanced
    assert!(buffer.secondary_head() > buffer.secondary_region_start);
  }

  #[test]
  fn test_dual_region_merge() {
    let (mut pager, _temp) = create_test_pager();
    let mut buffer = WalBuffer::new(4096, 4 * 4096, 4096);

    // Write to primary
    let record1 = WalRecord::new(
      WalRecordType::CreateNode,
      1,
      build_create_node_payload(100, Some("node1")),
    );
    buffer.write_record(&record1, &mut pager).unwrap();
    buffer.flush(&mut pager).unwrap();

    // Switch to secondary and write more
    buffer.switch_to_secondary();
    let record2 = WalRecord::new(
      WalRecordType::CreateNode,
      2,
      build_create_node_payload(101, Some("node2")),
    );
    buffer.write_record(&record2, &mut pager).unwrap();
    buffer.flush(&mut pager).unwrap();

    // Verify both regions have data
    assert!(buffer.primary_head() > 0);
    assert!(buffer.secondary_head() > buffer.secondary_region_start);

    // Merge secondary into primary (simulates checkpoint completion)
    buffer.merge_secondary_into_primary(&mut pager).unwrap();
    buffer.flush(&mut pager).unwrap();

    // After merge, should be back in primary with just the secondary records
    assert_eq!(buffer.active_region(), 0);
    assert_eq!(buffer.tail(), 0);

    // Scan should show the merged record (just the one from secondary)
    let records = buffer.scan_records(&mut pager).unwrap();
    assert_eq!(records.len(), 1); // Only secondary record preserved
    assert_eq!(records[0].txid, 2);
  }
}
