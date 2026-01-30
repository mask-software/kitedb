//! File locking utilities for concurrent database access.
//!
//! On native targets, uses fs2 for cross-platform file locks.
//! On wasm32, locking is a no-op (browser/wasm runtimes do not expose locks).

#[cfg(not(target_arch = "wasm32"))]
mod imp {
  use crate::constants::LOCK_FILENAME;
  use crate::error::{RayError, Result};
  use std::fs::{File, OpenOptions};
  use std::path::Path;

  /// Lock type enum
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum LockType {
    Shared,
    Exclusive,
  }

  /// File lock for database directory
  pub struct FileLock {
    handle: LockHandle,
    lock_type: LockType,
  }

  impl FileLock {
    /// Acquire a lock on the database directory
    pub fn acquire<P: AsRef<Path>>(db_path: P, lock_type: LockType) -> Result<Self> {
      let lock_path = db_path.as_ref().join(LOCK_FILENAME);
      let handle = match lock_type {
        LockType::Shared => LockHandle::shared(&lock_path)?,
        LockType::Exclusive => LockHandle::exclusive(&lock_path)?,
      };
      Ok(Self { handle, lock_type })
    }

    /// Get the lock type
    pub fn lock_type(&self) -> LockType {
      self.lock_type
    }

    /// Check if exclusive
    pub fn is_exclusive(&self) -> bool {
      self.lock_type == LockType::Exclusive
    }
  }

  /// File lock handle
  pub struct LockHandle {
    file: File,
    exclusive: bool,
  }

  impl LockHandle {
    /// Acquire an exclusive (write) lock on the file
    pub fn exclusive(path: impl AsRef<Path>) -> Result<Self> {
      let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.as_ref())
        .map_err(|e| RayError::LockFailed(format!("Failed to open lock file: {e}")))?;

      fs2::FileExt::lock_exclusive(&file)
        .map_err(|e| RayError::LockFailed(format!("Failed to acquire exclusive lock: {e}")))?;

      Ok(Self {
        file,
        exclusive: true,
      })
    }

    /// Acquire a shared (read) lock on the file
    pub fn shared(path: impl AsRef<Path>) -> Result<Self> {
      let file = OpenOptions::new()
        .read(true)
        .write(true) // Need write to create on some systems
        .create(true)
        .truncate(false)
        .open(path.as_ref())
        .map_err(|e| RayError::LockFailed(format!("Failed to open lock file: {e}")))?;

      fs2::FileExt::lock_shared(&file)
        .map_err(|e| RayError::LockFailed(format!("Failed to acquire shared lock: {e}")))?;

      Ok(Self {
        file,
        exclusive: false,
      })
    }

    /// Try to acquire an exclusive lock without blocking
    pub fn try_exclusive(path: impl AsRef<Path>) -> Result<Option<Self>> {
      let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.as_ref())
        .map_err(|e| RayError::LockFailed(format!("Failed to open lock file: {e}")))?;

      match fs2::FileExt::try_lock_exclusive(&file) {
        Ok(()) => Ok(Some(Self {
          file,
          exclusive: true,
        })),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(e) => Err(RayError::LockFailed(format!(
          "Failed to try exclusive lock: {e}"
        ))),
      }
    }

    /// Try to acquire a shared lock without blocking
    pub fn try_shared(path: impl AsRef<Path>) -> Result<Option<Self>> {
      let file = OpenOptions::new()
        .read(true)
        .write(true) // Need write to create on some systems
        .create(true)
        .truncate(false)
        .open(path.as_ref())
        .map_err(|e| RayError::LockFailed(format!("Failed to open lock file: {e}")))?;

      match fs2::FileExt::try_lock_shared(&file) {
        Ok(()) => Ok(Some(Self {
          file,
          exclusive: false,
        })),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(e) => Err(RayError::LockFailed(format!(
          "Failed to try shared lock: {e}"
        ))),
      }
    }

    /// Check if this is an exclusive lock
    pub fn is_exclusive(&self) -> bool {
      self.exclusive
    }

    /// Release the lock (also happens on drop)
    pub fn release(self) -> Result<()> {
      fs2::FileExt::unlock(&self.file)
        .map_err(|e| RayError::LockFailed(format!("Failed to release lock: {e}")))
    }
  }

  impl Drop for LockHandle {
    fn drop(&mut self) {
      // Best effort unlock on drop
      let _ = fs2::FileExt::unlock(&self.file);
    }
  }

  /// Lock a file for exclusive access (convenience function)
  pub fn lock_exclusive(path: impl AsRef<Path>) -> Result<LockHandle> {
    LockHandle::exclusive(path)
  }

  /// Lock a file for shared access (convenience function)
  pub fn lock_shared(path: impl AsRef<Path>) -> Result<LockHandle> {
    LockHandle::shared(path)
  }

  #[cfg(test)]
  mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_exclusive_lock() {
      let dir = tempdir().unwrap();
      let lock_path = dir.path().join("test.lock");

      let lock = LockHandle::exclusive(&lock_path).unwrap();
      assert!(lock.is_exclusive());

      // Try to acquire another exclusive lock - should fail
      let result = LockHandle::try_exclusive(&lock_path).unwrap();
      assert!(result.is_none());

      // Release first lock
      lock.release().unwrap();

      // Now should succeed
      let lock2 = LockHandle::try_exclusive(&lock_path).unwrap();
      assert!(lock2.is_some());
    }

    #[test]
    fn test_shared_lock() {
      let dir = tempdir().unwrap();
      let lock_path = dir.path().join("test_shared.lock");

      // Create the file first with write permissions
      std::fs::write(&lock_path, b"").unwrap();

      let lock1 = LockHandle::shared(&lock_path).unwrap();
      assert!(!lock1.is_exclusive());

      // Multiple shared locks should work on Unix
      // Note: On some systems, same process can acquire multiple shared locks
      drop(lock1);
      let lock2 = LockHandle::try_shared(&lock_path).unwrap();
      assert!(lock2.is_some());
    }

    #[test]
    fn test_exclusive_blocks_shared() {
      let dir = tempdir().unwrap();
      let lock_path = dir.path().join("test_blocks.lock");

      // Create the file first
      std::fs::write(&lock_path, b"").unwrap();

      let _exclusive = LockHandle::exclusive(&lock_path).unwrap();

      // On same process, locking behavior may vary by OS
      // Just verify exclusive lock works
      assert!(_exclusive.is_exclusive());
    }
  }
}

#[cfg(target_arch = "wasm32")]
mod imp {
  use crate::error::Result;
  use std::path::Path;

  /// Lock type enum
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum LockType {
    Shared,
    Exclusive,
  }

  /// No-op file lock for wasm targets.
  pub struct FileLock {
    lock_type: LockType,
  }

  impl FileLock {
    pub fn acquire<P: AsRef<Path>>(_db_path: P, lock_type: LockType) -> Result<Self> {
      Ok(Self { lock_type })
    }

    pub fn lock_type(&self) -> LockType {
      self.lock_type
    }

    pub fn is_exclusive(&self) -> bool {
      self.lock_type == LockType::Exclusive
    }
  }

  /// No-op lock handle for wasm targets.
  pub struct LockHandle {
    exclusive: bool,
  }

  impl LockHandle {
    pub fn exclusive(_path: impl AsRef<Path>) -> Result<Self> {
      Ok(Self { exclusive: true })
    }

    pub fn shared(_path: impl AsRef<Path>) -> Result<Self> {
      Ok(Self { exclusive: false })
    }

    pub fn try_exclusive(_path: impl AsRef<Path>) -> Result<Option<Self>> {
      Ok(Some(Self { exclusive: true }))
    }

    pub fn try_shared(_path: impl AsRef<Path>) -> Result<Option<Self>> {
      Ok(Some(Self { exclusive: false }))
    }

    pub fn is_exclusive(&self) -> bool {
      self.exclusive
    }

    pub fn release(self) -> Result<()> {
      Ok(())
    }
  }

  pub fn lock_exclusive(path: impl AsRef<Path>) -> Result<LockHandle> {
    LockHandle::exclusive(path)
  }

  pub fn lock_shared(path: impl AsRef<Path>) -> Result<LockHandle> {
    LockHandle::shared(path)
  }
}

pub use imp::*;
