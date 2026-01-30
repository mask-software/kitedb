//! Page-based I/O abstraction for single-file database format
//!
//! Provides page-level read/write, mmap support, and area management.
//! Ported from src/core/pager.ts

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::util::mmap::{map_file, Mmap};

use crate::constants::{
  LOCK_BYTE_OFFSET, LOCK_BYTE_RANGE, MAX_PAGE_SIZE, MIN_PAGE_SIZE, OS_PAGE_SIZE,
};
use crate::error::{RayError, Result};

/// FilePager implementation for single-file database
pub struct FilePager {
  file: File,
  file_path: PathBuf,
  page_size: usize,
  file_size: u64,
  free_pages: HashSet<u32>,
  /// Cached mmap for the entire file (lazily created)
  mmap: Option<Mmap>,
}

impl FilePager {
  /// Create a new FilePager from an open file
  pub fn new(file: File, file_path: PathBuf, page_size: usize) -> Result<Self> {
    let file_size = file.metadata()?.len();
    Ok(Self {
      file,
      file_path,
      page_size,
      file_size,
      free_pages: HashSet::new(),
      mmap: None,
    })
  }

  /// Create a new FilePager with explicit file size (for new files)
  pub fn with_size(file: File, file_path: PathBuf, page_size: usize, file_size: u64) -> Self {
    Self {
      file,
      file_path,
      page_size,
      file_size,
      free_pages: HashSet::new(),
      mmap: None,
    }
  }

  /// Get the file path
  pub fn file_path(&self) -> &Path {
    &self.file_path
  }

  /// Get the page size
  pub fn page_size(&self) -> usize {
    self.page_size
  }

  /// Get the current file size
  pub fn file_size(&self) -> u64 {
    self.file_size
  }

  /// Calculate the page number range for the lock byte region
  fn lock_byte_page_range(&self) -> (u32, u32) {
    let start = (LOCK_BYTE_OFFSET / self.page_size as u64) as u32;
    let end = (LOCK_BYTE_OFFSET + LOCK_BYTE_RANGE as u64).div_ceil(self.page_size as u64) as u32;
    (start, end)
  }

  /// Check if a page number overlaps with the lock byte range
  fn is_lock_byte_page(&self, page_num: u32) -> bool {
    let (start, end) = self.lock_byte_page_range();
    page_num >= start && page_num < end
  }

  /// Read a single page by page number
  pub fn read_page(&mut self, page_num: u32) -> Result<Vec<u8>> {
    let offset = page_num as u64 * self.page_size as u64;

    // Safety check: don't read beyond file size
    if offset >= self.file_size {
      return Ok(vec![0u8; self.page_size]);
    }

    let mut buffer = vec![0u8; self.page_size];
    self.file.seek(SeekFrom::Start(offset))?;

    // Read as much as we can (may be less than page_size at end of file)
    let _bytes_read = self.file.read(&mut buffer)?;

    // Rest is already zeros
    Ok(buffer)
  }

  /// Write a single page by page number
  pub fn write_page(&mut self, page_num: u32, data: &[u8]) -> Result<()> {
    if data.len() != self.page_size {
      return Err(RayError::Internal(format!(
        "Page data must be exactly {} bytes, got {}",
        self.page_size,
        data.len()
      )));
    }

    // Safety check: don't write to lock byte range
    if self.is_lock_byte_page(page_num) {
      return Err(RayError::Internal(format!(
        "Cannot write to lock byte page range (page {page_num})"
      )));
    }

    let offset = page_num as u64 * self.page_size as u64;

    // Extend file if necessary
    let required_size = offset + self.page_size as u64;
    if required_size > self.file_size {
      self.file.set_len(required_size)?;
      self.file_size = required_size;
    }

    self.file.seek(SeekFrom::Start(offset))?;
    self.file.write_all(data)?;

    // Invalidate mmap cache since file contents changed
    self.invalidate_mmap_cache();

    Ok(())
  }

  /// Memory-map the entire file (for snapshot access)
  /// Returns a view into mmap'd memory
  pub fn mmap_file(&mut self) -> Result<&Mmap> {
    if self.mmap.is_none() {
      // Safety: The file should not be modified while mmap is active
      // In practice, we invalidate the mmap on writes
      let mmap = map_file(&self.file)?;
      self.mmap = Some(mmap);
    }
    Ok(self.mmap.as_ref().unwrap())
  }

  /// Get a slice of the mmap'd file for a page range
  pub fn mmap_range(&mut self, start_page: u32, page_count: u32) -> Result<&[u8]> {
    let start_offset = start_page as usize * self.page_size;
    let length = page_count as usize * self.page_size;

    // Validate mmap alignment
    if start_offset % OS_PAGE_SIZE != 0 {
      return Err(RayError::Internal(format!(
        "mmap offset {start_offset} must be aligned to OS page size {OS_PAGE_SIZE}"
      )));
    }

    let mmap = self.mmap_file()?;

    // Check bounds
    if start_offset + length > mmap.len() {
      return Err(RayError::Internal(format!(
        "mmap range {}..{} exceeds file size {}",
        start_offset,
        start_offset + length,
        mmap.len()
      )));
    }

    Ok(&mmap[start_offset..start_offset + length])
  }

  /// Allocate new pages at end of file
  /// Returns the starting page number of the allocated range
  pub fn allocate_pages(&mut self, count: u32) -> Result<u32> {
    if count == 0 {
      return Err(RayError::Internal(
        "Must allocate at least 1 page".to_string(),
      ));
    }

    // Calculate current page count
    let current_page_count = self.file_size.div_ceil(self.page_size as u64) as u32;
    let mut start_page = current_page_count;

    // Check if we need to skip the lock byte range
    let (lock_start, lock_end) = self.lock_byte_page_range();

    // If the new allocation would overlap with lock byte range, skip past it
    if start_page < lock_end && start_page + count > lock_start {
      // Move start past the lock byte range
      start_page = lock_end;
    }

    // Extend file
    let new_size = (start_page + count) as u64 * self.page_size as u64;
    self.file.set_len(new_size)?;
    self.file_size = new_size;

    // Invalidate mmap cache
    self.invalidate_mmap_cache();

    Ok(start_page)
  }

  /// Mark pages as free (for vacuum)
  /// In v1, this just tracks free pages; actual reclamation happens during vacuum
  pub fn free_pages(&mut self, start_page: u32, count: u32) {
    for i in 0..count {
      self.free_pages.insert(start_page + i);
    }
  }

  /// Get count of free pages
  pub fn free_page_count(&self) -> usize {
    self.free_pages.len()
  }

  /// Truncate file to the given number of pages
  pub fn truncate_pages(&mut self, page_count: u32) -> Result<()> {
    let new_size = page_count as u64 * self.page_size as u64;
    self.file.set_len(new_size)?;
    self.file_size = new_size;
    self.invalidate_mmap_cache();
    Ok(())
  }

  /// Sync file to disk
  pub fn sync(&self) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
      use std::os::unix::io::AsRawFd;
      let result = unsafe { libc::fsync(self.file.as_raw_fd()) };
      if result != 0 {
        return Err(std::io::Error::last_os_error().into());
      }
    }

    #[cfg(not(target_os = "macos"))]
    {
      self.file.sync_all()?;
    }
    Ok(())
  }

  /// Relocate an area to a new location (for growth/compaction)
  /// This is an expensive operation that copies data page by page
  pub fn relocate_area(&mut self, src_page: u32, page_count: u32, dst_page: u32) -> Result<()> {
    if src_page == dst_page {
      return Ok(());
    }

    // Validate destination doesn't overlap with lock byte range
    let (lock_start, lock_end) = self.lock_byte_page_range();
    if dst_page < lock_end && dst_page + page_count > lock_start {
      return Err(RayError::Internal(
        "Cannot relocate to lock byte range".to_string(),
      ));
    }

    // Determine copy direction to avoid overwriting source before reading
    let copy_forward = src_page < dst_page;

    if copy_forward {
      // Copy from end to start to avoid overwriting
      for i in (0..page_count).rev() {
        let src_offset = (src_page + i) as u64 * self.page_size as u64;
        let dst_offset = (dst_page + i) as u64 * self.page_size as u64;

        // Read source page
        let mut buffer = vec![0u8; self.page_size];
        self.file.seek(SeekFrom::Start(src_offset))?;
        self.file.read_exact(&mut buffer)?;

        // Extend file if needed
        let required_size = dst_offset + self.page_size as u64;
        if required_size > self.file_size {
          self.file.set_len(required_size)?;
          self.file_size = required_size;
        }

        // Write to destination
        self.file.seek(SeekFrom::Start(dst_offset))?;
        self.file.write_all(&buffer)?;
      }
    } else {
      // Copy from start to end
      for i in 0..page_count {
        let src_offset = (src_page + i) as u64 * self.page_size as u64;
        let dst_offset = (dst_page + i) as u64 * self.page_size as u64;

        // Read source page
        let mut buffer = vec![0u8; self.page_size];
        self.file.seek(SeekFrom::Start(src_offset))?;
        self.file.read_exact(&mut buffer)?;

        // Extend file if needed
        let required_size = dst_offset + self.page_size as u64;
        if required_size > self.file_size {
          self.file.set_len(required_size)?;
          self.file_size = required_size;
        }

        // Write to destination
        self.file.seek(SeekFrom::Start(dst_offset))?;
        self.file.write_all(&buffer)?;
      }
    }

    // Sync to ensure data is durable before marking old pages as free
    self.sync()?;

    // Mark old pages as free
    self.free_pages(src_page, page_count);

    // Invalidate mmap cache
    self.invalidate_mmap_cache();

    Ok(())
  }

  /// Invalidate mmap cache
  fn invalidate_mmap_cache(&mut self) {
    self.mmap = None;
  }

  /// Get a reference to the underlying file
  pub fn file(&self) -> &File {
    &self.file
  }

  /// Get a mutable reference to the underlying file
  pub fn file_mut(&mut self) -> &mut File {
    &mut self.file
  }
}

// ============================================================================
// Factory functions
// ============================================================================

/// Open a pager for an existing file
pub fn open_pager<P: AsRef<Path>>(file_path: P, page_size: usize) -> Result<FilePager> {
  let file = OpenOptions::new().read(true).write(true).open(&file_path)?;
  FilePager::new(file, file_path.as_ref().to_path_buf(), page_size)
}

/// Create a new pager for a new file
pub fn create_pager<P: AsRef<Path>>(file_path: P, page_size: usize) -> Result<FilePager> {
  let file = OpenOptions::new()
    .read(true)
    .write(true)
    .create(true)
    .truncate(true)
    .open(&file_path)?;
  Ok(FilePager::with_size(
    file,
    file_path.as_ref().to_path_buf(),
    page_size,
    0,
  ))
}

/// Validate that a page size is valid (power of 2, within bounds)
pub fn is_valid_page_size(page_size: usize) -> bool {
  if !(MIN_PAGE_SIZE..=MAX_PAGE_SIZE).contains(&page_size) {
    return false;
  }
  // Check power of 2
  (page_size & (page_size - 1)) == 0
}

/// Calculate the number of pages needed to store a given byte count
pub fn pages_to_store(byte_count: usize, page_size: usize) -> u32 {
  byte_count.div_ceil(page_size) as u32
}

#[cfg(test)]
mod tests {
  use super::*;
  use tempfile::NamedTempFile;

  #[test]
  fn test_is_valid_page_size() {
    assert!(is_valid_page_size(4096));
    assert!(is_valid_page_size(8192));
    assert!(is_valid_page_size(16384));
    assert!(is_valid_page_size(32768));
    assert!(is_valid_page_size(65536));

    // Invalid: too small
    assert!(!is_valid_page_size(2048));
    // Invalid: too large
    assert!(!is_valid_page_size(131072));
    // Invalid: not power of 2
    assert!(!is_valid_page_size(5000));
    assert!(!is_valid_page_size(6000));
  }

  #[test]
  fn test_pages_to_store() {
    assert_eq!(pages_to_store(0, 4096), 0);
    assert_eq!(pages_to_store(1, 4096), 1);
    assert_eq!(pages_to_store(4096, 4096), 1);
    assert_eq!(pages_to_store(4097, 4096), 2);
    assert_eq!(pages_to_store(8192, 4096), 2);
    assert_eq!(pages_to_store(10000, 4096), 3);
  }

  #[test]
  fn test_read_write_page() {
    let temp_file = NamedTempFile::new().unwrap();
    let mut pager = create_pager(temp_file.path(), 4096).unwrap();

    // Write a page
    let data: Vec<u8> = (0..4096).map(|i| (i % 256) as u8).collect();
    pager.write_page(0, &data).unwrap();

    // Read it back
    let read_data = pager.read_page(0).unwrap();
    assert_eq!(read_data, data);
  }

  #[test]
  fn test_read_empty_page() {
    let temp_file = NamedTempFile::new().unwrap();
    let mut pager = create_pager(temp_file.path(), 4096).unwrap();

    // Reading beyond file size should return zeros
    let read_data = pager.read_page(100).unwrap();
    assert_eq!(read_data, vec![0u8; 4096]);
  }

  #[test]
  fn test_allocate_pages() {
    let temp_file = NamedTempFile::new().unwrap();
    let mut pager = create_pager(temp_file.path(), 4096).unwrap();

    // Allocate first batch
    let start1 = pager.allocate_pages(5).unwrap();
    assert_eq!(start1, 0);
    assert_eq!(pager.file_size(), 5 * 4096);

    // Allocate second batch
    let start2 = pager.allocate_pages(3).unwrap();
    assert_eq!(start2, 5);
    assert_eq!(pager.file_size(), 8 * 4096);
  }

  #[test]
  fn test_free_pages() {
    let temp_file = NamedTempFile::new().unwrap();
    let mut pager = create_pager(temp_file.path(), 4096).unwrap();

    // Allocate and free some pages
    pager.allocate_pages(10).unwrap();
    pager.free_pages(2, 3);

    assert_eq!(pager.free_page_count(), 3);
  }

  #[test]
  fn test_mmap_file() {
    let temp_file = NamedTempFile::new().unwrap();
    let mut pager = create_pager(temp_file.path(), 4096).unwrap();

    // Write some data first
    let data: Vec<u8> = (0..4096).map(|i| (i % 256) as u8).collect();
    pager.write_page(0, &data).unwrap();
    pager.sync().unwrap();

    // Now mmap and verify
    let mmap = pager.mmap_file().unwrap();
    assert_eq!(&mmap[..4096], &data[..]);
  }

  #[test]
  fn test_write_extends_file() {
    let temp_file = NamedTempFile::new().unwrap();
    let mut pager = create_pager(temp_file.path(), 4096).unwrap();

    assert_eq!(pager.file_size(), 0);

    // Writing to page 5 should extend the file
    let data = vec![0xAB; 4096];
    pager.write_page(5, &data).unwrap();

    assert_eq!(pager.file_size(), 6 * 4096);
  }

  #[test]
  fn test_page_size_validation() {
    let temp_file = NamedTempFile::new().unwrap();
    let mut pager = create_pager(temp_file.path(), 4096).unwrap();

    // Wrong size data should fail
    let small_data = vec![0u8; 100];
    assert!(pager.write_page(0, &small_data).is_err());

    let large_data = vec![0u8; 8192];
    assert!(pager.write_page(0, &large_data).is_err());
  }
}
