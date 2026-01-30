//! Lightweight mmap abstraction for native + wasm builds.
//!
//! On native targets, this is a thin wrapper over memmap2::Mmap.
//! On wasm32-wasi, we fall back to reading the full file into memory.

use std::fs::File;

#[cfg(target_arch = "wasm32")]
use std::io::{Read, Seek, SeekFrom};
#[cfg(target_arch = "wasm32")]
use std::sync::Arc;

#[cfg(not(target_arch = "wasm32"))]
pub type Mmap = memmap2::Mmap;

#[cfg(target_arch = "wasm32")]
#[derive(Clone)]
pub struct Mmap {
  data: Arc<Vec<u8>>,
}

#[cfg(target_arch = "wasm32")]
impl Mmap {
  /// Read the entire file into memory.
  pub fn map(file: &File) -> std::io::Result<Self> {
    let mut handle = file.try_clone()?;
    handle.seek(SeekFrom::Start(0))?;
    let mut buffer = Vec::new();
    handle.read_to_end(&mut buffer)?;
    Ok(Self {
      data: Arc::new(buffer),
    })
  }

  pub fn len(&self) -> usize {
    self.data.len()
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
pub fn map_file(file: &File) -> std::io::Result<Mmap> {
  #[cfg(not(target_arch = "wasm32"))]
  unsafe {
    Mmap::map(file)
  }
  #[cfg(target_arch = "wasm32")]
  {
    Mmap::map(file)
  }
}
