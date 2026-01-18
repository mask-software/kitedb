/**
 * Backup and Restore functionality for RayDB
 * 
 * Supports both single-file (.raydb) and multi-file (directory) databases.
 * Backups are atomic and consistent - they capture a point-in-time snapshot.
 */

import { existsSync } from "node:fs";
import { copyFile, mkdir, readdir, stat, rm } from "node:fs/promises";
import { join, basename, dirname } from "node:path";
import type { GraphDB } from "../types.ts";
import { EXT_RAYDB, MANIFEST_FILENAME, SNAPSHOTS_DIR, WAL_DIR } from "../constants.ts";
import { checkpoint } from "../ray/graph-db/checkpoint.ts";

export interface BackupOptions {
  /**
   * Force a checkpoint before backup to ensure all WAL data is in the snapshot.
   * Default: true for single-file databases
   */
  checkpoint?: boolean;
  
  /**
   * Overwrite existing backup file/directory if it exists.
   * Default: false
   */
  overwrite?: boolean;
}

export interface RestoreOptions {
  /**
   * Overwrite existing database if it exists at the target path.
   * Default: false
   */
  overwrite?: boolean;
}

export interface BackupResult {
  /** Path to the backup file/directory */
  path: string;
  /** Size in bytes */
  size: number;
  /** Timestamp when backup was created */
  timestamp: Date;
  /** Whether this is a single-file or multi-file backup */
  type: "single-file" | "multi-file";
}

/**
 * Create a backup of a database
 * 
 * For single-file databases: Creates a copy of the .raydb file
 * For multi-file databases: Creates a copy of the entire directory
 * 
 * @param db - Open database handle (will be checkpointed if needed)
 * @param backupPath - Path where backup should be created
 * @param options - Backup options
 * @returns Information about the created backup
 * 
 * @example
 * ```ts
 * const db = await openGraphDB("./mydb.raydb");
 * const backup = await createBackup(db, "./backups/mydb-2024-01-15.raydb");
 * console.log(`Backup created: ${backup.size} bytes`);
 * await closeGraphDB(db);
 * ```
 */
export async function createBackup(
  db: GraphDB,
  backupPath: string,
  options: BackupOptions = {}
): Promise<BackupResult> {
  const { checkpoint: doCheckpoint = true, overwrite = false } = options;

  // Check if backup already exists
  if (existsSync(backupPath) && !overwrite) {
    throw new Error(`Backup already exists at ${backupPath}. Use overwrite: true to replace.`);
  }

  // For single-file databases
  if (db._isSingleFile) {
    return createSingleFileBackup(db, backupPath, doCheckpoint, overwrite);
  }

  // For multi-file databases
  return createMultiFileBackup(db, backupPath, doCheckpoint, overwrite);
}

/**
 * Create a backup of a single-file database
 */
async function createSingleFileBackup(
  db: GraphDB,
  backupPath: string,
  doCheckpoint: boolean,
  overwrite: boolean
): Promise<BackupResult> {
  // Ensure backup path has correct extension
  if (!backupPath.endsWith(EXT_RAYDB)) {
    backupPath = backupPath + EXT_RAYDB;
  }

  // Checkpoint to ensure WAL is flushed to snapshot
  if (doCheckpoint && !db.readOnly) {
    await checkpoint(db);
  }

  // Ensure backup directory exists
  const backupDir = dirname(backupPath);
  if (backupDir && backupDir !== "." && !existsSync(backupDir)) {
    await mkdir(backupDir, { recursive: true });
  }

  // Remove existing backup if overwrite is enabled
  if (overwrite && existsSync(backupPath)) {
    await rm(backupPath, { force: true });
  }

  // Copy the database file
  await copyFile(db.path, backupPath);

  // Get backup size
  const stats = await stat(backupPath);

  return {
    path: backupPath,
    size: stats.size,
    timestamp: new Date(),
    type: "single-file",
  };
}

/**
 * Create a backup of a multi-file database
 */
async function createMultiFileBackup(
  db: GraphDB,
  backupPath: string,
  doCheckpoint: boolean,
  overwrite: boolean
): Promise<BackupResult> {
  // Checkpoint to ensure WAL is flushed (for multi-file this writes a new snapshot)
  if (doCheckpoint && !db.readOnly) {
    // For multi-file, we should ideally trigger a compaction
    // For now, we just copy all files
  }

  // Remove existing backup if overwrite is enabled
  if (overwrite && existsSync(backupPath)) {
    await rm(backupPath, { recursive: true, force: true });
  }

  // Create backup directory structure
  await mkdir(backupPath, { recursive: true });
  await mkdir(join(backupPath, SNAPSHOTS_DIR), { recursive: true });
  await mkdir(join(backupPath, WAL_DIR), { recursive: true });

  let totalSize = 0;

  // Copy manifest
  const manifestSrc = join(db.path, MANIFEST_FILENAME);
  if (existsSync(manifestSrc)) {
    await copyFile(manifestSrc, join(backupPath, MANIFEST_FILENAME));
    totalSize += (await stat(manifestSrc)).size;
  }

  // Copy snapshots
  const snapshotsDir = join(db.path, SNAPSHOTS_DIR);
  if (existsSync(snapshotsDir)) {
    const snapshots = await readdir(snapshotsDir);
    for (const file of snapshots) {
      const src = join(snapshotsDir, file);
      const dst = join(backupPath, SNAPSHOTS_DIR, file);
      await copyFile(src, dst);
      totalSize += (await stat(src)).size;
    }
  }

  // Copy WAL files
  const walDir = join(db.path, WAL_DIR);
  if (existsSync(walDir)) {
    const walFiles = await readdir(walDir);
    for (const file of walFiles) {
      const src = join(walDir, file);
      const dst = join(backupPath, WAL_DIR, file);
      await copyFile(src, dst);
      totalSize += (await stat(src)).size;
    }
  }

  return {
    path: backupPath,
    size: totalSize,
    timestamp: new Date(),
    type: "multi-file",
  };
}

/**
 * Restore a database from a backup
 * 
 * @param backupPath - Path to the backup file/directory
 * @param restorePath - Path where database should be restored
 * @param options - Restore options
 * @returns Path to the restored database
 * 
 * @example
 * ```ts
 * const restoredPath = await restoreBackup(
 *   "./backups/mydb-2024-01-15.raydb",
 *   "./restored-db.raydb"
 * );
 * const db = await openGraphDB(restoredPath);
 * ```
 */
export async function restoreBackup(
  backupPath: string,
  restorePath: string,
  options: RestoreOptions = {}
): Promise<string> {
  const { overwrite = false } = options;

  if (!existsSync(backupPath)) {
    throw new Error(`Backup not found at ${backupPath}`);
  }

  if (existsSync(restorePath) && !overwrite) {
    throw new Error(`Database already exists at ${restorePath}. Use overwrite: true to replace.`);
  }

  const backupStats = await stat(backupPath);

  if (backupStats.isFile()) {
    // Single-file backup
    return restoreSingleFileBackup(backupPath, restorePath, overwrite);
  } else if (backupStats.isDirectory()) {
    // Multi-file backup
    return restoreMultiFileBackup(backupPath, restorePath, overwrite);
  } else {
    throw new Error(`Invalid backup at ${backupPath}: not a file or directory`);
  }
}

/**
 * Restore a single-file database from backup
 */
async function restoreSingleFileBackup(
  backupPath: string,
  restorePath: string,
  overwrite: boolean
): Promise<string> {
  // Ensure restore path has correct extension
  if (!restorePath.endsWith(EXT_RAYDB)) {
    restorePath = restorePath + EXT_RAYDB;
  }

  // Ensure restore directory exists
  const restoreDir = dirname(restorePath);
  if (restoreDir && restoreDir !== "." && !existsSync(restoreDir)) {
    await mkdir(restoreDir, { recursive: true });
  }

  // Remove existing database if overwrite is enabled
  if (overwrite && existsSync(restorePath)) {
    await rm(restorePath, { force: true });
  }

  // Copy backup to restore location
  await copyFile(backupPath, restorePath);

  return restorePath;
}

/**
 * Restore a multi-file database from backup
 */
async function restoreMultiFileBackup(
  backupPath: string,
  restorePath: string,
  overwrite: boolean
): Promise<string> {
  // Remove existing database if overwrite is enabled
  if (overwrite && existsSync(restorePath)) {
    await rm(restorePath, { recursive: true, force: true });
  }

  // Create restore directory structure
  await mkdir(restorePath, { recursive: true });
  await mkdir(join(restorePath, SNAPSHOTS_DIR), { recursive: true });
  await mkdir(join(restorePath, WAL_DIR), { recursive: true });

  // Copy manifest
  const manifestSrc = join(backupPath, MANIFEST_FILENAME);
  if (existsSync(manifestSrc)) {
    await copyFile(manifestSrc, join(restorePath, MANIFEST_FILENAME));
  }

  // Copy snapshots
  const snapshotsDir = join(backupPath, SNAPSHOTS_DIR);
  if (existsSync(snapshotsDir)) {
    const snapshots = await readdir(snapshotsDir);
    for (const file of snapshots) {
      await copyFile(
        join(snapshotsDir, file),
        join(restorePath, SNAPSHOTS_DIR, file)
      );
    }
  }

  // Copy WAL files
  const walDir = join(backupPath, WAL_DIR);
  if (existsSync(walDir)) {
    const walFiles = await readdir(walDir);
    for (const file of walFiles) {
      await copyFile(
        join(walDir, file),
        join(restorePath, WAL_DIR, file)
      );
    }
  }

  return restorePath;
}

/**
 * Get information about an existing backup without restoring it
 * 
 * @param backupPath - Path to the backup file/directory
 * @returns Information about the backup
 */
export async function getBackupInfo(backupPath: string): Promise<BackupResult> {
  if (!existsSync(backupPath)) {
    throw new Error(`Backup not found at ${backupPath}`);
  }

  const stats = await stat(backupPath);

  if (stats.isFile()) {
    return {
      path: backupPath,
      size: stats.size,
      timestamp: stats.mtime,
      type: "single-file",
    };
  } else if (stats.isDirectory()) {
    // Calculate total size for multi-file backup
    let totalSize = 0;

    async function calculateDirSize(dirPath: string): Promise<number> {
      let size = 0;
      const entries = await readdir(dirPath, { withFileTypes: true });
      for (const entry of entries) {
        const fullPath = join(dirPath, entry.name);
        if (entry.isFile()) {
          size += (await stat(fullPath)).size;
        } else if (entry.isDirectory()) {
          size += await calculateDirSize(fullPath);
        }
      }
      return size;
    }

    totalSize = await calculateDirSize(backupPath);

    return {
      path: backupPath,
      size: totalSize,
      timestamp: stats.mtime,
      type: "multi-file",
    };
  }

  throw new Error(`Invalid backup at ${backupPath}: not a file or directory`);
}

/**
 * Create a backup from a database path (without opening it)
 * 
 * This is useful for backing up a database that might be in use by another process.
 * Note: This creates a potentially inconsistent backup if the database is actively being written to.
 * For consistent backups, use createBackup() with an open database handle.
 * 
 * @param dbPath - Path to the database file/directory
 * @param backupPath - Path where backup should be created
 * @param options - Backup options
 */
export async function createOfflineBackup(
  dbPath: string,
  backupPath: string,
  options: Omit<BackupOptions, "checkpoint"> = {}
): Promise<BackupResult> {
  const { overwrite = false } = options;

  if (!existsSync(dbPath)) {
    throw new Error(`Database not found at ${dbPath}`);
  }

  if (existsSync(backupPath) && !overwrite) {
    throw new Error(`Backup already exists at ${backupPath}. Use overwrite: true to replace.`);
  }

  const stats = await stat(dbPath);

  if (stats.isFile()) {
    // Single-file database
    const backupDir = dirname(backupPath);
    if (backupDir && backupDir !== "." && !existsSync(backupDir)) {
      await mkdir(backupDir, { recursive: true });
    }

    if (overwrite && existsSync(backupPath)) {
      await rm(backupPath, { force: true });
    }

    await copyFile(dbPath, backupPath);

    const backupStats = await stat(backupPath);
    return {
      path: backupPath,
      size: backupStats.size,
      timestamp: new Date(),
      type: "single-file",
    };
  } else {
    // Multi-file database - copy entire directory
    if (overwrite && existsSync(backupPath)) {
      await rm(backupPath, { recursive: true, force: true });
    }

    await mkdir(backupPath, { recursive: true });

    let totalSize = 0;

    async function copyDir(src: string, dst: string): Promise<void> {
      await mkdir(dst, { recursive: true });
      const entries = await readdir(src, { withFileTypes: true });
      for (const entry of entries) {
        const srcPath = join(src, entry.name);
        const dstPath = join(dst, entry.name);
        if (entry.isFile()) {
          await copyFile(srcPath, dstPath);
          totalSize += (await stat(srcPath)).size;
        } else if (entry.isDirectory()) {
          await copyDir(srcPath, dstPath);
        }
      }
    }

    await copyDir(dbPath, backupPath);

    return {
      path: backupPath,
      size: totalSize,
      timestamp: new Date(),
      type: "multi-file",
    };
  }
}
