/**
 * File locking for cross-process safety
 * 
 * Uses native flock() via Bun FFI - no external dependencies required.
 * Supports both multi-file (legacy) and single-file (SQLite-style) locking.
 * 
 * Multi-file: Uses a separate lock file with flock()
 * Single-file: Uses flock() on the database file itself
 */

import { closeSync, openSync, unlinkSync } from "node:fs";
import { join } from "node:path";
import { dlopen, FFIType, suffix } from "bun:ffi";
import { LOCK_FILENAME } from "../constants.ts";
import { lockLogger } from "./logger.ts";

// ============================================================================
// Native flock() via Bun FFI
// ============================================================================

// flock() operation flags
const LOCK_SH = 1;  // Shared lock
const LOCK_EX = 2;  // Exclusive lock
const LOCK_NB = 4;  // Non-blocking
const LOCK_UN = 8;  // Unlock

// Native flock function - loaded lazily
let nativeFlock: ((fd: number, operation: number) => number) | null = null;
let flockLoadAttempted = false;
let flockLoadError: string | null = null;

/**
 * Load native flock() function via FFI
 * Works on macOS and Linux without any npm dependencies
 */
function loadNativeFlock(): boolean {
  if (flockLoadAttempted) {
    return nativeFlock !== null;
  }
  flockLoadAttempted = true;

  try {
    // On macOS and Linux, flock is in libc
    // macOS: /usr/lib/libc.dylib or libSystem.B.dylib
    // Linux: libc.so.6
    const libPath = process.platform === "darwin" 
      ? "/usr/lib/libSystem.B.dylib"
      : `libc.so.6`;

    const lib = dlopen(libPath, {
      flock: {
        args: [FFIType.i32, FFIType.i32],
        returns: FFIType.i32,
      },
    });

    nativeFlock = (fd: number, operation: number): number => {
      return lib.symbols.flock(fd, operation) as number;
    };

    return true;
  } catch (err) {
    flockLoadError = String(err);
    // FFI not available - will use fallback
    return false;
  }
}

/**
 * Check if proper file locking is available
 * Returns true if native flock() is available via FFI
 */
export function isProperLockingAvailable(): boolean {
  return loadNativeFlock();
}

// For backwards compatibility - async version
export async function isProperLockingAvailableAsync(): Promise<boolean> {
  return isProperLockingAvailable();
}

// Track if we've already warned about missing locking (avoid log spam)
let warnedAboutMissingLocking = false;

function warnNoLocking(): void {
  if (!warnedAboutMissingLocking) {
    warnedAboutMissingLocking = true;
    lockLogger.warn(
      `No locking mechanism available${flockLoadError ? `: ${flockLoadError}` : ""}. ` +
      "Concurrent access from multiple processes may cause data corruption."
    );
  }
}

// ============================================================================
// Multi-file locking (directory-based databases)
// ============================================================================

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

    // Open file descriptor for locking
    const fd = openSync(lockPath, "r+");

    // Try native flock
    if (loadNativeFlock() && nativeFlock) {
      const result = nativeFlock(fd, LOCK_EX | LOCK_NB);
      if (result !== 0) {
        closeSync(fd);
        return null; // Lock already held or error
      }
      return { fd, path: lockPath };
    }

    // No locking available - fd provides minimal protection
    warnNoLocking();
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

    // Open file descriptor for locking
    const fd = openSync(lockPath, "r");

    // Try native flock
    if (loadNativeFlock() && nativeFlock) {
      const result = nativeFlock(fd, LOCK_SH | LOCK_NB);
      if (result !== 0) {
        closeSync(fd);
        return null; // Could not acquire lock
      }
      return { fd, path: lockPath };
    }

    // No locking available
    warnNoLocking();
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
  // Unlock the file first
  if (nativeFlock && lock.fd >= 0) {
    try {
      nativeFlock(lock.fd, LOCK_UN);
    } catch (err) {
      lockLogger.warn(`Failed to unlock`, { path: lock.path, error: String(err) });
    }
  }

  // Close file descriptor
  if (lock.fd >= 0) {
    try {
      closeSync(lock.fd);
    } catch (err) {
      lockLogger.warn(`Failed to close lock fd`, { path: lock.path, error: String(err) });
    }
  }

  // Try to remove lock file (only matters for exclusive locks)
  try {
    unlinkSync(lock.path);
  } catch (err) {
    // ENOENT is fine (file already removed), log others
    const errno = err as NodeJS.ErrnoException;
    if (errno.code !== "ENOENT") {
      lockLogger.warn(`Failed to remove lock file`, { path: lock.path, error: String(err) });
    }
  }
}

// ============================================================================
// Single-file locking (SQLite-style)
// ============================================================================

/**
 * Single-file lock handle
 */
export interface SingleFileLockHandle {
  fd: number;
  exclusive: boolean;
}

/**
 * Acquire exclusive lock on a single database file
 * Uses flock() for whole-file locking
 */
export async function acquireExclusiveFileLock(
  fd: number,
): Promise<SingleFileLockHandle | null> {
  // Try native flock
  if (loadNativeFlock() && nativeFlock) {
    const result = nativeFlock(fd, LOCK_EX | LOCK_NB);
    if (result !== 0) {
      return null; // Lock already held or error
    }
    return { fd, exclusive: true };
  }

  // No locking available
  warnNoLocking();
  return { fd, exclusive: true };
}

/**
 * Acquire shared lock on a single database file
 * Uses flock() for whole-file locking
 */
export async function acquireSharedFileLock(
  fd: number,
): Promise<SingleFileLockHandle | null> {
  // Try native flock
  if (loadNativeFlock() && nativeFlock) {
    const result = nativeFlock(fd, LOCK_SH | LOCK_NB);
    if (result !== 0) {
      return null; // Could not acquire lock
    }
    return { fd, exclusive: false };
  }

  // No locking available
  warnNoLocking();
  return { fd, exclusive: false };
}

/**
 * Release a single-file lock
 * Note: The file descriptor is NOT closed - caller is responsible
 */
export async function releaseFileLock(lock: SingleFileLockHandle): Promise<void> {
  if (nativeFlock) {
    try {
      nativeFlock(lock.fd, LOCK_UN);
    } catch {
      // Ignore unlock errors
    }
  }
}

/**
 * Check if we can acquire an exclusive lock (without actually acquiring it)
 * Note: This actually acquires and immediately releases the lock
 */
export async function canAcquireExclusiveLock(fd: number): Promise<boolean> {
  if (!loadNativeFlock() || !nativeFlock) {
    return true; // Can't check, assume yes
  }

  // Try to acquire, then immediately release
  const result = nativeFlock(fd, LOCK_EX | LOCK_NB);
  if (result === 0) {
    nativeFlock(fd, LOCK_UN);
    return true;
  }
  return false;
}
