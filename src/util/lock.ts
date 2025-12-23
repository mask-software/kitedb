/**
 * Optional file locking for cross-process safety
 * Supports both multi-file (legacy) and single-file (SQLite-style) locking
 * 
 * Multi-file: Uses a separate lock file with flock()
 * Single-file: Uses byte-range locking at offset 2^30 (SQLite compatible)
 */

import { closeSync, openSync, unlinkSync, readSync, writeSync, fstatSync } from "node:fs";
import { join } from "node:path";
import { LOCK_FILENAME, LOCK_BYTE_OFFSET, LOCK_BYTE_RANGE } from "../constants.ts";

// Lazy-load fs-ext for proper flock support, fallback to basic fd locking
let flockSync: ((fd: number, flags: string) => void) | null = null;
let flockSyncLoaded = false;

async function loadFlockSync(): Promise<void> {
  if (flockSyncLoaded) return;
  flockSyncLoaded = true;

  try {
    // @ts-ignore - fs-ext may not have types
    const fsExt = await import("fs-ext");
    if (fsExt.flockSync) {
      flockSync = fsExt.flockSync;
    }
  } catch {
    // fs-ext not available, will use basic fd-based locking
  }
}

export interface LockHandle {
  fd: number;
  path: string;
}

/**
 * Acquire exclusive lock for read-write access
 * Returns null if lock cannot be acquired
 */
export async function acquireExclusiveLock(
  dbPath: string,
): Promise<LockHandle | null> {
  const lockPath = join(dbPath, LOCK_FILENAME);

  try {
    // Ensure lock file exists (create if needed)
    const file = Bun.file(lockPath);
    if (!(await file.exists())) {
      // Write PID to lock file
      const pid = process.pid;
      await Bun.write(lockPath, `${pid}\n`);
    }

    // Try to load flock support if not already loaded
    await loadFlockSync();

    // Open file descriptor for locking
    const fd = openSync(lockPath, "r+");

    // Acquire OS-level exclusive lock using flock
    if (flockSync) {
      try {
        flockSync(fd, "exnb"); // Exclusive, non-blocking
      } catch (err) {
        closeSync(fd);
        return null; // Lock already held
      }
    }
    // If flockSync not available, fd being open provides basic protection
    // (file can't be deleted while fd is open)

    return { fd, path: lockPath };
  } catch (err) {
    // Lock file might already exist or permission denied
    return null;
  }
}

/**
 * Acquire shared lock for read-only access
 * Returns null if lock file doesn't exist (no writer active)
 */
export async function acquireSharedLock(
  dbPath: string,
): Promise<LockHandle | null> {
  const lockPath = join(dbPath, LOCK_FILENAME);

  try {
    const file = Bun.file(lockPath);

    // If lock file doesn't exist, no writer is active
    if (!(await file.exists())) {
      return null;
    }

    // Try to load flock support if not already loaded
    await loadFlockSync();

    // Open file descriptor for locking
    const fd = openSync(lockPath, "r");

    // Acquire OS-level shared lock using flock
    if (flockSync) {
      try {
        flockSync(fd, "shnb"); // Shared, non-blocking
      } catch (err) {
        closeSync(fd);
        return null; // Could not acquire lock
      }
    }
    // If flockSync not available, fd being open provides basic protection

    return { fd, path: lockPath };
  } catch {
    // Lock file might not exist or permission denied
    return null;
  }
}

/**
 * Release a lock
 */
export function releaseLock(lock: LockHandle): void {
  // Unlock the file first (if using flock)
  if (flockSync && lock.fd >= 0) {
    try {
      flockSync(lock.fd, "un");
    } catch (err) {
      console.warn(`Failed to unlock ${lock.path}: ${err}`);
    }
  }

  // Close file descriptor
  if (lock.fd >= 0) {
    try {
      closeSync(lock.fd);
    } catch (err) {
      console.warn(`Failed to close lock fd for ${lock.path}: ${err}`);
    }
  }

  // Try to remove lock file (only matters for exclusive locks)
  try {
    unlinkSync(lock.path);
  } catch (err) {
    // ENOENT is fine (file already removed), log others
    const errno = err as NodeJS.ErrnoException;
    if (errno.code !== "ENOENT") {
      console.warn(`Failed to remove lock file ${lock.path}: ${err}`);
    }
  }
}

// ============================================================================
// Single-file (SQLite-style) byte-range locking
// ============================================================================

// Lazy-load fcntl for byte-range locking
let fcntlSync: ((fd: number, cmd: number, arg: FcntlLockStruct) => number) | null = null;
let fcntlSyncLoaded = false;

interface FcntlLockStruct {
  type: number;
  whence: number;
  start: bigint;
  len: bigint;
}

// fcntl commands
const F_SETLK = 6;  // Set lock, return error if blocked
const F_SETLKW = 7; // Set lock, wait if blocked
const F_GETLK = 5;  // Get lock info

// Lock types  
const F_RDLCK = 0;  // Shared (read) lock
const F_WRLCK = 1;  // Exclusive (write) lock
const F_UNLCK = 2;  // Unlock

async function loadFcntlSync(): Promise<void> {
  if (fcntlSyncLoaded) return;
  fcntlSyncLoaded = true;

  try {
    // @ts-ignore - fs-ext may not have types
    const fsExt = await import("fs-ext");
    if (fsExt.fcntl) {
      fcntlSync = (fd: number, cmd: number, arg: FcntlLockStruct) => {
        return fsExt.fcntl(fd, cmd, arg);
      };
    }
  } catch {
    // fs-ext not available, will fallback to flock
  }
}

/**
 * Single-file lock handle
 */
export interface SingleFileLockHandle {
  fd: number;
  exclusive: boolean;
}

/**
 * Acquire exclusive lock on a single database file (SQLite-style)
 * Uses byte-range locking at offset 2^30
 */
export async function acquireExclusiveFileLock(
  fd: number,
): Promise<SingleFileLockHandle | null> {
  await loadFcntlSync();
  await loadFlockSync();

  // Try fcntl byte-range lock first (more precise)
  if (fcntlSync) {
    try {
      const lockInfo: FcntlLockStruct = {
        type: F_WRLCK,
        whence: 0, // SEEK_SET
        start: BigInt(LOCK_BYTE_OFFSET),
        len: BigInt(LOCK_BYTE_RANGE),
      };
      
      const result = fcntlSync(fd, F_SETLK, lockInfo);
      if (result === 0) {
        return { fd, exclusive: true };
      }
    } catch {
      // fcntl failed, try flock fallback
    }
  }

  // Fallback to flock (whole-file lock)
  if (flockSync) {
    try {
      flockSync(fd, "exnb"); // Exclusive, non-blocking
      return { fd, exclusive: true };
    } catch {
      return null; // Lock already held
    }
  }

  // No locking available, proceed without lock (not ideal but works)
  return { fd, exclusive: true };
}

/**
 * Acquire shared lock on a single database file (SQLite-style)
 * Uses byte-range locking at offset 2^30
 */
export async function acquireSharedFileLock(
  fd: number,
): Promise<SingleFileLockHandle | null> {
  await loadFcntlSync();
  await loadFlockSync();

  // Try fcntl byte-range lock first
  if (fcntlSync) {
    try {
      const lockInfo: FcntlLockStruct = {
        type: F_RDLCK,
        whence: 0, // SEEK_SET
        start: BigInt(LOCK_BYTE_OFFSET),
        len: BigInt(LOCK_BYTE_RANGE),
      };
      
      const result = fcntlSync(fd, F_SETLK, lockInfo);
      if (result === 0) {
        return { fd, exclusive: false };
      }
    } catch {
      // fcntl failed, try flock fallback
    }
  }

  // Fallback to flock (whole-file lock)
  if (flockSync) {
    try {
      flockSync(fd, "shnb"); // Shared, non-blocking
      return { fd, exclusive: false };
    } catch {
      return null; // Could not acquire lock
    }
  }

  // No locking available
  return { fd, exclusive: false };
}

/**
 * Release a single-file lock
 * Note: The file descriptor is NOT closed - caller is responsible
 */
export async function releaseFileLock(lock: SingleFileLockHandle): Promise<void> {
  await loadFcntlSync();
  await loadFlockSync();

  // Try fcntl unlock first
  if (fcntlSync) {
    try {
      const lockInfo: FcntlLockStruct = {
        type: F_UNLCK,
        whence: 0,
        start: BigInt(LOCK_BYTE_OFFSET),
        len: BigInt(LOCK_BYTE_RANGE),
      };
      fcntlSync(lock.fd, F_SETLK, lockInfo);
      return;
    } catch {
      // Try flock fallback
    }
  }

  // Fallback to flock unlock
  if (flockSync) {
    try {
      flockSync(lock.fd, "un");
    } catch {
      // Ignore unlock errors
    }
  }
}

/**
 * Check if we can acquire an exclusive lock (without actually acquiring it)
 */
export async function canAcquireExclusiveLock(fd: number): Promise<boolean> {
  await loadFcntlSync();

  if (fcntlSync) {
    try {
      const lockInfo: FcntlLockStruct = {
        type: F_WRLCK,
        whence: 0,
        start: BigInt(LOCK_BYTE_OFFSET),
        len: BigInt(LOCK_BYTE_RANGE),
      };
      
      // F_GETLK checks if a lock COULD be acquired
      // If it returns our own lock type, we can acquire
      const result = fcntlSync(fd, F_GETLK, lockInfo);
      return lockInfo.type === F_UNLCK;
    } catch {
      return false;
    }
  }

  // Without fcntl, we can't check without trying
  return true;
}
