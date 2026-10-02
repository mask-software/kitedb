//! Lightweight mmap abstraction for native + wasm builds.
//!
//! On native targets, this is a thin wrapper over memmap2::Mmap.
//! On wasm32-wasi, we fall back to reading the requested bytes into memory.
//!
//! A mapping is an immutable view for its entire lifetime. Native callers must
//! never mutate, truncate, or extend any byte in a mapped range until the
//! mapping is dropped. Embedded single-file snapshots use `map_file_range` so
//! only the immutable snapshot pages are mapped; headers and WAL pages remain
//! ordinary mutable I/O ranges.
//!
//! Only a handle that can keep every other writer out of the file can uphold
//! that: touching a mapped page that another handle truncated from the file
//! kills the process (SIGBUS). Without that guarantee, copy the range into
//! `private_map` memory instead.

use std::fs::File;

#[cfg(target_arch = "wasm32")]
use std::io::{Read, Seek, SeekFrom};
#[cfg(target_arch = "wasm32")]
use std::sync::Arc;

#[cfg(not(target_arch = "wasm32"))]
pub type Mmap = memmap2::Mmap;

#[cfg(not(target_arch = "wasm32"))]
use memmap2::MmapOptions;

#[cfg(target_arch = "wasm32")]
#[derive(Clone)]
pub struct Mmap {
  data: Arc<Vec<u8>>,
}

#[cfg(target_arch = "wasm32")]
impl Mmap {
  /// Read the entire file into memory.
  pub fn map(file: &File) -> std::io::Result<Self> {
    // Through `&File`: WASI cannot duplicate a descriptor (`try_clone`).
    let mut handle = file;
    handle.seek(SeekFrom::Start(0))?;
    let mut buffer = Vec::new();
    handle.read_to_end(&mut buffer)?;
    Ok(Self {
      data: Arc::new(buffer),
    })
  }
}

#[cfg(target_arch = "wasm32")]
impl std::ops::Deref for Mmap {
  type Target = [u8];

  fn deref(&self) -> &Self::Target {
    &self.data
  }
}

/// Map a file into memory (native uses unsafe mmap, wasm reads to memory).
///
/// The caller must keep the entire file immutable while the returned mapping
/// is live. For an embedded database, prefer `map_file_range`.
pub fn map_file(file: &File) -> std::io::Result<Mmap> {
  #[cfg(not(target_arch = "wasm32"))]
  unsafe {
    let len = file.metadata()?.len();
    if len == 0 {
      return Err(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "cannot mmap an empty file",
      ));
    }
    Mmap::map(file)
  }
  #[cfg(target_arch = "wasm32")]
  {
    Mmap::map(file)
  }
}

/// Private, read-only memory of `length` bytes, filled by `fill`.
///
/// No file backs it, so no change to any file can fault it, unlike a file
/// mapping: for a range of a file that another handle may change meanwhile.
pub fn private_map(
  length: usize,
  fill: impl FnOnce(&mut [u8]) -> std::io::Result<()>,
) -> std::io::Result<Mmap> {
  if length == 0 {
    return Err(std::io::Error::new(
      std::io::ErrorKind::InvalidInput,
      "cannot map an empty range",
    ));
  }

  #[cfg(not(target_arch = "wasm32"))]
  {
    let mut memory = memmap2::MmapMut::map_anon(length)?;
    fill(&mut memory)?;
    memory.make_read_only()
  }

  #[cfg(target_arch = "wasm32")]
  {
    let mut buffer = vec![0u8; length];
    fill(&mut buffer)?;
    Ok(Mmap {
      data: Arc::new(buffer),
    })
  }
}

/// Map exactly one immutable byte range of a file.
///
/// KiteDB snapshot starts are page-aligned, so the native alignment rule is an
/// invariant of the file format rather than a caller-controlled unsafe
/// precondition.
pub fn map_file_range(file: &File, offset: u64, length: usize) -> std::io::Result<Mmap> {
  if length == 0 {
    return Err(std::io::Error::new(
      std::io::ErrorKind::InvalidInput,
      "cannot mmap an empty range",
    ));
  }

  #[cfg(not(target_arch = "wasm32"))]
  {
    let file_len = file.metadata()?.len();
    let end = offset.checked_add(length as u64).ok_or_else(|| {
      std::io::Error::new(std::io::ErrorKind::InvalidInput, "mmap range overflow")
    })?;
    if end > file_len {
      return Err(std::io::Error::new(
        std::io::ErrorKind::UnexpectedEof,
        "mmap range exceeds file length",
      ));
    }
    if !offset.is_multiple_of(4096) {
      return Err(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "mmap offset must be aligned to 4096 bytes",
      ));
    }

    // SAFETY: the caller owns the immutable-range invariant documented above;
    // the offset is page-aligned and the requested range is within the file.
    unsafe { MmapOptions::new().offset(offset).len(length).map(file) }
  }

  #[cfg(target_arch = "wasm32")]
  {
    let mut handle = file;
    handle.seek(SeekFrom::Start(offset))?;
    let mut buffer = vec![0u8; length];
    handle.read_exact(&mut buffer)?;
    Ok(Mmap {
      data: Arc::new(buffer),
    })
  }
}
