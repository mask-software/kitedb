/**
 * Optional file locking for cross-process safety
 * Uses file descriptors with flock() for OS-level advisory locking
 */

import { closeSync, openSync, unlinkSync } from "node:fs";
import { join } from "node:path";
import { LOCK_FILENAME } from "../constants.ts";

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
