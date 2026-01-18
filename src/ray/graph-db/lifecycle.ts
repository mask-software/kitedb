import { existsSync } from "node:fs";
import { join, normalize, isAbsolute, resolve } from "node:path";
import {
  COMPACT_EDGE_RATIO,
  COMPACT_NODE_RATIO,
  COMPACT_WAL_SIZE,
  EXT_RAYDB,
  INITIAL_ETYPE_ID,
  INITIAL_LABEL_ID,
  INITIAL_NODE_ID,
  INITIAL_PROPKEY_ID,
  INITIAL_TX_ID,
  INITIAL_WAL_SEG,
  MANIFEST_FILENAME,
  SNAPSHOTS_DIR,
  snapshotFilename,
  WAL_DIR,
  walFilename,
} from "../../constants.ts";
import { createDelta } from "../../core/delta.ts";
import {
  createEmptyManifest,
  readManifest,
  writeManifest,
} from "../../core/manifest.ts";
import { closeSnapshot, loadSnapshot } from "../../core/snapshot-reader.ts";
import { snapshotLogger } from "../../util/logger.ts";
import {
  createWalSegment,
  extractCommittedTransactions,
  loadWalSegment,
  parseCreateNodePayload,
  parseDefineEtypePayload,
  parseDefineLabelPayload,
  parseDefinePropkeyPayload,
  parseAddEdgePayload,
  parseDeleteEdgePayload,
  parseDeleteNodePayload,
  parseSetNodePropPayload,
  parseDelNodePropPayload,
  parseSetEdgePropPayload,
  parseDelEdgePropPayload,
} from "../../core/wal.ts";
import type {
  GraphDB,
  OpenOptions,
} from "../../types.ts";
import { WalRecordType } from "../../types.ts";
import {
  acquireExclusiveLock,
  acquireSharedLock,
  releaseLock,
  isProperLockingAvailable,
  type LockHandle,
} from "../../util/lock.ts";
import { CacheManager } from "../../cache/index.ts";
import { replayWalRecord, replayVectorRecord } from "./wal-replay.ts";
import { MvccManager } from "../../mvcc/index.ts";
import type { PropKeyID } from "../../types.ts";
import type { VectorManifest } from "../../vector/types.ts";
import { openSingleFileDB, closeSingleFileDB, isSingleFilePath } from "./single-file.ts";
import { releaseFileLock, type SingleFileLockHandle } from "../../util/lock.ts";
import type { FilePager } from "../../core/pager.ts";

/**
 * Validate a database path for security
 * Prevents path traversal attacks and other unsafe path patterns
 * 
 * @param path The path to validate
 * @throws Error if the path is invalid or potentially dangerous
 */
function validateDbPath(path: string): void {
  if (!path || typeof path !== 'string') {
    throw new Error("Database path must be a non-empty string");
  }

  // Normalize the path to resolve . and .. 
  const normalizedPath = normalize(path);
  
  // Check for path traversal attempts (.. sequences that escape)
  // After normalization, if the path starts with .. it's trying to go above the base
  if (normalizedPath.startsWith('..') || normalizedPath.includes('/..') || normalizedPath.includes('\\..')) {
    throw new Error("Database path contains invalid path traversal sequence");
  }

  // Check for null bytes (path injection attack)
  if (path.includes('\0')) {
    throw new Error("Database path contains null bytes");
  }

  // Check for excessively long paths (platform-specific limits, but 4096 is reasonable)
  if (path.length > 4096) {
    throw new Error("Database path is too long");
  }

  // On non-Windows, check for problematic control characters
  // eslint-disable-next-line no-control-regex
  if (/[\x00-\x1f]/.test(path)) {
    throw new Error("Database path contains control characters");
  }
}

/**
 * Open a graph database
 * Automatically detects format based on path:
 * - If path ends with .raydb or is an existing .raydb file: single-file format
 * - If path is an existing directory with manifest.gdm: multi-file format
 * - If path is an existing directory (without manifest): use multi-file format (backward compat)
 * - Otherwise: new database uses single-file format by default
 */
export async function openGraphDB(
  path: string,
  options: OpenOptions = {},
): Promise<GraphDB> {
  // Validate path for security before any filesystem operations
  validateDbPath(path);

  const { readOnly = false, createIfMissing = true, lockFile = true } = options;

  // Check if path is an existing directory (multi-file format)
  // This includes directories with manifest.gdm and empty directories
  // for backward compatibility with existing code that creates empty tmp dirs
  const fs = await import("node:fs");
  if (existsSync(path)) {
    try {
      const stat = fs.statSync(path);
      if (stat.isDirectory()) {
        // Existing directory - use multi-file format
        return openMultiFileDB(path, options);
      }
    } catch {
      // Ignore stat errors, continue with single-file detection
    }
  }
  
  // Check if path already ends with .raydb or exists as a .raydb file
  let dbPath = path;
  if (!path.endsWith(EXT_RAYDB)) {
    const raydbPath = path + EXT_RAYDB;
    if (existsSync(raydbPath)) {
      // Existing single-file database without extension in path
      dbPath = raydbPath;
    } else {
      // New database - use single-file format by default
      dbPath = raydbPath;
    }
  }
  
  // Single-file format
  return openSingleFileDB(dbPath, options);
}

/**
 * Open a multi-file graph database (directory format)
 * @internal
 */
async function openMultiFileDB(
  path: string,
  options: OpenOptions = {},
): Promise<GraphDB> {
  const { readOnly = false, createIfMissing = true, lockFile = true, requireLocking = false } = options;

  // Check if strict locking is required but not available
  if (requireLocking && lockFile) {
    if (!isProperLockingAvailable()) {
      throw new Error(
        "requireLocking is enabled but proper file locking is not available. " +
        "This may happen if Bun FFI cannot load the system's libc."
      );
    }
  }

  // Ensure directory exists
  const fs = await import("node:fs/promises");

  const manifestPath = join(path, MANIFEST_FILENAME);
  let manifestExists = false;

  try {
    await fs.access(manifestPath);
    manifestExists = true;
  } catch {
    manifestExists = false;
  }

  if (!manifestExists && !createIfMissing) {
    throw new Error(`Database does not exist at ${path}`);
  }

  // Create directory structure
  if (!manifestExists) {
    await fs.mkdir(path, { recursive: true });
    await fs.mkdir(join(path, SNAPSHOTS_DIR), { recursive: true });
    await fs.mkdir(join(path, WAL_DIR), { recursive: true });
  }

  // Acquire lock
  let lockFd: LockHandle | null = null;
  if (lockFile) {
    if (readOnly) {
      lockFd = await acquireSharedLock(path);
    } else {
      lockFd = await acquireExclusiveLock(path);
      if (!lockFd) {
        throw new Error(
          "Failed to acquire exclusive lock - database may be in use",
        );
      }
    }
  }

  // Read or create manifest
  let manifest = await readManifest(path);
  if (!manifest) {
    if (readOnly) {
      throw new Error("Cannot create database in read-only mode");
    }
    manifest = createEmptyManifest();
    await writeManifest(path, manifest);
  }

  // Load snapshot if exists
  let snapshot = null;
  if (manifest.activeSnapshotGen > 0n) {
    try {
      snapshot = await loadSnapshot(path, manifest.activeSnapshotGen);
    } catch (err) {
      snapshotLogger.warn(`Failed to load snapshot`, { error: String(err), path });
    }
  }

  // Initialize delta
  const delta = createDelta();

  // Initialize vector stores
  const vectorStores: Map<PropKeyID, VectorManifest> = new Map();

  // Initialize ID allocators
  let nextNodeId = INITIAL_NODE_ID;
  let nextLabelId = INITIAL_LABEL_ID;
  let nextEtypeId = INITIAL_ETYPE_ID;
  let nextPropkeyId = INITIAL_PROPKEY_ID;

  if (snapshot) {
    nextNodeId = Number(snapshot.header.maxNodeId) + 1;
    nextLabelId = Number(snapshot.header.numLabels) + 1;
    nextEtypeId = Number(snapshot.header.numEtypes) + 1;
    nextPropkeyId = Number(snapshot.header.numPropkeys) + 1;
  }

  // Ensure WAL exists
  let walOffset = 0;
  const walPath = join(path, WAL_DIR, walFilename(manifest.activeWalSeg));

  try {
    const walFile = Bun.file(walPath);
    if (!(await walFile.exists())) {
      if (!readOnly) {
        await createWalSegment(path, manifest.activeWalSeg);
      }
    }
    walOffset = (await walFile.arrayBuffer()).byteLength;
  } catch {
    if (!readOnly) {
      await createWalSegment(path, manifest.activeWalSeg);
      const walFile = Bun.file(walPath);
      walOffset = (await walFile.arrayBuffer()).byteLength;
    }
  }

  // Replay WAL for recovery
  const walData = await loadWalSegment(path, manifest.activeWalSeg);
  let nextTxId = INITIAL_TX_ID;
  let nextCommitTs = 1n;

  // Create a temporary db object for vector replay (only needs _vectorStores)
  const tempDbForVectorReplay = { _vectorStores: vectorStores } as GraphDB;

  if (walData) {
    const committed = extractCommittedTransactions(walData.records);

    for (const [txid, records] of committed) {
      if (txid >= nextTxId) {
        nextTxId = txid + 1n;
      }

      // Assign commit timestamp for MVCC (sequential order in WAL = commit order)
      const commitTs = nextCommitTs++;

      // Replay each record
      for (const record of records) {
        replayWalRecord(record, delta);
        
        // Also replay vector records
        if (record.type === WalRecordType.SET_NODE_VECTOR || 
            record.type === WalRecordType.DEL_NODE_VECTOR) {
          replayVectorRecord(record, tempDbForVectorReplay);
        }

        // Update ID allocators
        if (record.type === WalRecordType.CREATE_NODE) {
          const data = parseCreateNodePayload(record.payload);
          if (data.nodeId >= nextNodeId) {
            nextNodeId = data.nodeId + 1;
          }
        } else if (record.type === WalRecordType.DEFINE_LABEL) {
          const data = parseDefineLabelPayload(record.payload);
          if (data.labelId >= nextLabelId) {
            nextLabelId = data.labelId + 1;
          }
        } else if (record.type === WalRecordType.DEFINE_ETYPE) {
          const data = parseDefineEtypePayload(record.payload);
          if (data.etypeId >= nextEtypeId) {
            nextEtypeId = data.etypeId + 1;
          }
        } else if (record.type === WalRecordType.DEFINE_PROPKEY) {
          const data = parseDefinePropkeyPayload(record.payload);
          if (data.propkeyId >= nextPropkeyId) {
            nextPropkeyId = data.propkeyId + 1;
          }
        }
      }
    }

    walOffset =
      walData.records.length > 0
        ? walData.records[walData.records.length - 1]!.recordEnd
        : walOffset;
  }

  // Initialize MVCC if enabled (after WAL replay to get correct nextTxId/nextCommitTs)
  let mvcc: MvccManager | null = null;
  if (options.mvcc) {
    const gcIntervalMs = options.mvccGcInterval ?? 5000;
    const retentionMs = options.mvccRetentionMs ?? 60000;
    const maxChainDepth = options.mvccMaxChainDepth ?? 10;
    mvcc = new MvccManager(nextTxId, nextCommitTs, gcIntervalMs, retentionMs, maxChainDepth);
    
    // Rebuild version chains from WAL if MVCC is enabled
    if (walData) {
      const committed = extractCommittedTransactions(walData.records);
      let rebuildCommitTs = 1n;
      
      for (const [txid, records] of committed) {
        const commitTs = rebuildCommitTs++;
        
        // Rebuild version chains
        for (const record of records) {
          switch (record.type) {
            case WalRecordType.CREATE_NODE: {
              const data = parseCreateNodePayload(record.payload);
              const nodeDelta = delta.createdNodes.get(data.nodeId);
              if (nodeDelta) {
                mvcc.versionChain.appendNodeVersion(
                  data.nodeId,
                  { nodeId: data.nodeId, delta: nodeDelta },
                  txid,
                  commitTs,
                );
              }
              break;
            }
            case WalRecordType.DELETE_NODE: {
              const data = parseDeleteNodePayload(record.payload);
              mvcc.versionChain.deleteNodeVersion(data.nodeId, txid, commitTs);
              break;
            }
            case WalRecordType.ADD_EDGE: {
              const data = parseAddEdgePayload(record.payload);
              mvcc.versionChain.appendEdgeVersion(
                data.src,
                data.etype,
                data.dst,
                true,
                txid,
                commitTs,
              );
              break;
            }
            case WalRecordType.DELETE_EDGE: {
              const data = parseDeleteEdgePayload(record.payload);
              mvcc.versionChain.appendEdgeVersion(
                data.src,
                data.etype,
                data.dst,
                false,
                txid,
                commitTs,
              );
              break;
            }
            case WalRecordType.SET_NODE_PROP: {
              const data = parseSetNodePropPayload(record.payload);
              mvcc.versionChain.appendNodePropVersion(
                data.nodeId,
                data.keyId,
                data.value,
                txid,
                commitTs,
              );
              break;
            }
            case WalRecordType.DEL_NODE_PROP: {
              const data = parseDelNodePropPayload(record.payload);
              mvcc.versionChain.appendNodePropVersion(
                data.nodeId,
                data.keyId,
                null,
                txid,
                commitTs,
              );
              break;
            }
            case WalRecordType.SET_EDGE_PROP: {
              const data = parseSetEdgePropPayload(record.payload);
              mvcc.versionChain.appendEdgePropVersion(
                data.src,
                data.etype,
                data.dst,
                data.keyId,
                data.value,
                txid,
                commitTs,
              );
              break;
            }
            case WalRecordType.DEL_EDGE_PROP: {
              const data = parseDelEdgePropPayload(record.payload);
              mvcc.versionChain.appendEdgePropVersion(
                data.src,
                data.etype,
                data.dst,
                data.keyId,
                null,
                txid,
                commitTs,
              );
              break;
            }
          }
        }
      }
    }
    
    // Start GC thread (only for writable databases)
    if (!readOnly) {
      mvcc.start();
    }
  }

  // Initialize cache
  const cache = new CacheManager(options.cache);

  return {
    path,
    readOnly,
    _isSingleFile: false,
    
    // Multi-file fields
    _manifest: manifest,
    _snapshot: snapshot,
    _walFd: null,
    _walOffset: walOffset,
    
    // Single-file fields (null for multi-file)
    _header: null,
    _pager: null,
    _snapshotMmap: null,
    _snapshotCache: null,
    _walWritePos: 0,
    
    // Shared fields
    _delta: delta,
    _nextNodeId: nextNodeId,
    _nextLabelId: nextLabelId,
    _nextEtypeId: nextEtypeId,
    _nextPropkeyId: nextPropkeyId,
    _nextTxId: nextTxId,
    _currentTx: null,
    _lockFd: lockFd,
    _cache: cache,
    _mvcc: mvcc,
    _mvccEnabled: mvcc !== null,
    
    // Vector embeddings storage
    _vectorStores: vectorStores,
    _vectorIndexes: new Map(),
  };
}

/**
 * Close the database
 */
export async function closeGraphDB(db: GraphDB): Promise<void> {
  if (db._isSingleFile) {
    // Single-file format
    await closeSingleFileDB(db);
  } else {
    // Multi-file format
    // Stop MVCC GC thread
    if (db._mvcc) {
      const mvcc = db._mvcc as MvccManager;
      mvcc.stop();
    }

    // Close snapshot
    if (db._snapshot) {
      closeSnapshot(db._snapshot);
      (db as { _snapshot: null })._snapshot = null;
    }

    // Release lock
    if (db._lockFd) {
      releaseLock(db._lockFd as LockHandle);
      (db as { _lockFd: null })._lockFd = null;
    }
  }
}

