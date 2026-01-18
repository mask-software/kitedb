/**
 * Single-file database format lifecycle management
 * Opens, creates, and manages .raydb single-file databases
 */

import { existsSync, openSync } from "node:fs";
import {
  DEFAULT_PAGE_SIZE,
  EXT_RAYDB,
  INITIAL_ETYPE_ID,
  INITIAL_LABEL_ID,
  INITIAL_NODE_ID,
  INITIAL_PROPKEY_ID,
  INITIAL_TX_ID,
  WAL_DEFAULT_SIZE,
} from "../../constants.ts";
import { createDelta } from "../../core/delta.ts";
import {
  createEmptyHeader,
  hasValidHeader,
  parseHeader,
  readHeader,
  serializeHeader,
  updateHeaderForCommit,
  writeHeader,
  writeHeaderSync,
} from "../../core/header.ts";
import type { DbHeaderV1, GraphDB, OpenOptions, DeltaState } from "../../types.ts";
import { createPager, FilePager, isValidPageSize, openPager, pagesToStore } from "../../core/pager.ts";
import { createWalBuffer, WalBuffer } from "../../core/wal-buffer.ts";
import { isCheckpointRunning, getCheckpointPromise } from "./checkpoint.ts";
import { extractCommittedTransactions, type ParsedWalRecord } from "../../core/wal.ts";
import { WalRecordType } from "../../types.ts";
import {
  acquireExclusiveFileLock,
  acquireSharedFileLock,
  releaseFileLock,
  isProperLockingAvailable,
  type SingleFileLockHandle,
} from "../../util/lock.ts";
import { CacheManager } from "../../cache/index.ts";
import { replayWalRecord, replayVectorRecord } from "./wal-replay.ts";
import { MvccManager } from "../../mvcc/index.ts";
import type { PropKeyID } from "../../types.ts";
import type { VectorManifest } from "../../vector/types.ts";
import {
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
import { checkpointLogger } from "../../util/logger.ts";

/**
 * Open a single-file database (.raydb format)
 * @internal This is an internal function - use openGraphDB instead
 */
export async function openSingleFileDB(
  path: string,
  options: OpenOptions = {},
): Promise<GraphDB> {
  const {
    readOnly = false,
    createIfMissing = true,
    lockFile = true,
    requireLocking = false,
    pageSize = DEFAULT_PAGE_SIZE,
    walSize = WAL_DEFAULT_SIZE,
    autoCheckpoint = true,
    checkpointThreshold = 0.8,
    cacheSnapshot = true,
  } = options;

  // Check if strict locking is required but not available
  if (requireLocking && lockFile) {
    if (!isProperLockingAvailable()) {
      throw new Error(
        "requireLocking is enabled but proper file locking is not available. " +
        "This may happen if Bun FFI cannot load the system's libc."
      );
    }
  }

  // Validate page size
  if (!isValidPageSize(pageSize)) {
    throw new Error(`Invalid page size: ${pageSize}. Must be power of 2 between 4KB and 64KB`);
  }

  // Check if file exists
  const fileExists = existsSync(path);

  if (!fileExists && !createIfMissing) {
    throw new Error(`Database does not exist at ${path}`);
  }

  if (!fileExists && readOnly) {
    throw new Error("Cannot create database in read-only mode");
  }

  // Open or create the pager
  let pager: FilePager;
  let header: DbHeaderV1;
  let isNew = false;

  if (fileExists) {
    // Open existing database
    pager = openPager(path, pageSize);
    
    // Read and validate header
    const headerBuffer = pager.readPage(0);
    if (!hasValidHeader(headerBuffer)) {
      pager.close();
      throw new Error(`Invalid database file: ${path}`);
    }
    
    header = parseHeader(headerBuffer);
  } else {
    // Create new database
    pager = createPager(path, pageSize);
    
    // Calculate WAL page count
    const walPageCount = BigInt(pagesToStore(walSize, pageSize));
    
    // Create initial header
    header = createEmptyHeader(pageSize, walPageCount);
    
    // Write initial header
    const headerBuffer = serializeHeader(header);
    pager.writePage(0, headerBuffer);
    
    // Allocate WAL pages
    pager.allocatePages(Number(walPageCount));
    
    // Sync to disk
    await pager.sync();
    
    isNew = true;
  }

  // Acquire lock
  let lockFd: SingleFileLockHandle | null = null;
  if (lockFile) {
    if (readOnly) {
      lockFd = await acquireSharedFileLock(pager.fd);
    } else {
      lockFd = await acquireExclusiveFileLock(pager.fd);
      if (!lockFd) {
        pager.close();
        throw new Error("Failed to acquire exclusive lock - database may be in use");
      }
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
  let nextTxId = Number(header.nextTxId);

  // Set maxNodeId from header if we have snapshot data
  if (header.maxNodeId > 0n) {
    nextNodeId = Number(header.maxNodeId) + 1;
  }

  // Create WAL buffer
  const walBuffer = createWalBuffer(pager, header);
  
  // Check for crash recovery during background checkpoint
  // If checkpointInProgress was set, we need to replay both WAL regions
  if (header.checkpointInProgress === 1) {
    // Crash during background checkpoint - replay both regions
    // The primary region has committed data, secondary has writes during checkpoint
    // Both need to be replayed to restore full state
    checkpointLogger.warn(`Recovering from interrupted background checkpoint`, { path });
  }

  // Replay WAL for recovery (getRecordsForRecovery handles dual-region)
  const walRecords = walBuffer.getRecordsForRecovery();
  const committed = extractCommittedTransactions(walRecords);
  let nextCommitTs = 1n;

  // Create a temporary db object for vector replay (only needs _vectorStores)
  const tempDbForVectorReplay = { _vectorStores: vectorStores } as GraphDB;

  for (const [txid, records] of committed) {
    if (Number(txid) >= nextTxId) {
      nextTxId = Number(txid) + 1;
    }

    // Assign commit timestamp
    nextCommitTs++;

    // Replay each record
    for (const record of records) {
      replayWalRecord(record, delta);
      
      // Also replay vector records
      if (record.type === WalRecordType.SET_NODE_VECTOR || 
          record.type === WalRecordType.DEL_NODE_VECTOR) {
        replayVectorRecord(record, tempDbForVectorReplay);
      }

      // Update ID allocators
      updateAllocatorsFromRecord(record, {
        nextNodeId: () => nextNodeId,
        nextLabelId: () => nextLabelId,
        nextEtypeId: () => nextEtypeId,
        nextPropkeyId: () => nextPropkeyId,
        setNextNodeId: (id) => { nextNodeId = id; },
        setNextLabelId: (id) => { nextLabelId = id; },
        setNextEtypeId: (id) => { nextEtypeId = id; },
        setNextPropkeyId: (id) => { nextPropkeyId = id; },
      });
    }
  }

  // Clear checkpointInProgress flag if it was set (crash recovery complete)
  if (header.checkpointInProgress === 1 && !readOnly) {
    header = {
      ...header,
      checkpointInProgress: 0,
      // Also reset active region to primary and clear V2 state
      activeWalRegion: 0,
      walPrimaryHead: walBuffer.getPrimaryHead(),
      walSecondaryHead: BigInt(walBuffer.getPrimaryRegionSize()), // Reset secondary to start
      changeCounter: header.changeCounter + 1n,
    };
    await writeHeader(pager, header);
  }

  // Initialize MVCC if enabled
  let mvcc: MvccManager | null = null;
  if (options.mvcc) {
    const gcIntervalMs = options.mvccGcInterval ?? 5000;
    const retentionMs = options.mvccRetentionMs ?? 60000;
    const maxChainDepth = options.mvccMaxChainDepth ?? 10;
    mvcc = new MvccManager(BigInt(nextTxId), nextCommitTs, gcIntervalMs, retentionMs, maxChainDepth);
    
    // Rebuild version chains from WAL
    if (walRecords.length > 0) {
      rebuildVersionChains(mvcc, committed, delta);
    }
    
    // Start GC thread
    if (!readOnly) {
      mvcc.start();
    }
  }

  // Initialize cache
  const cache = new CacheManager(options.cache);

  // mmap snapshot area if present
  let snapshotMmap: Uint8Array | null = null;
  if (header.snapshotPageCount > 0n) {
    snapshotMmap = pager.mmapRange(
      Number(header.snapshotStartPage),
      Number(header.snapshotPageCount)
    );
  }

  return {
    path,
    readOnly,
    _isSingleFile: true,
    
    // Multi-file fields (null for single-file)
    _manifest: null,
    _snapshot: null,
    _walFd: null,
    _walOffset: 0,
    
    // Single-file fields
    _header: header,
    _pager: pager,
    _snapshotMmap: snapshotMmap,
    _snapshotCache: null,
    _walWritePos: Number(header.walHead),
    
    // Shared fields
    _delta: delta,
    _nextNodeId: nextNodeId,
    _nextLabelId: nextLabelId,
    _nextEtypeId: nextEtypeId,
    _nextPropkeyId: nextPropkeyId,
    _nextTxId: BigInt(nextTxId),
    _currentTx: null,
    _lockFd: lockFd,
    _cache: cache,
    _mvcc: mvcc,
    _mvccEnabled: mvcc !== null,
    
    // Single-file options
    _autoCheckpoint: autoCheckpoint,
    _checkpointThreshold: checkpointThreshold,
    _cacheSnapshot: cacheSnapshot,
    
    // Vector embeddings storage
    _vectorStores: vectorStores,
    _vectorIndexes: new Map(),
  };
}

/**
 * Close a single-file database
 * @internal This is an internal function - use closeGraphDB instead
 */
export async function closeSingleFileDB(db: GraphDB): Promise<void> {
  if (!db._isSingleFile) {
    throw new Error("closeSingleFileDB called on multi-file database");
  }
  
  // Wait for any running checkpoint to complete before closing
  // This prevents the file from being closed/deleted while checkpoint is still running
  if (isCheckpointRunning(db)) {
    const checkpointPromise = getCheckpointPromise(db);
    if (checkpointPromise) {
      try {
        await checkpointPromise;
      } catch {
        // Ignore checkpoint errors during close - we're shutting down anyway
      }
    } else {
      // Checkpoint is in 'completing' state - wait for it to finish
      while (isCheckpointRunning(db)) {
        await new Promise(resolve => setImmediate(resolve));
      }
    }
  }
  
  // Stop MVCC GC thread
  if (db._mvcc) {
    const mvcc = db._mvcc as MvccManager;
    mvcc.stop();
  }

  // Release lock
  if (db._lockFd) {
    await releaseFileLock(db._lockFd as SingleFileLockHandle);
    (db as { _lockFd: unknown })._lockFd = null;
  }

  // Clear snapshot mmap reference
  (db as { _snapshotMmap: Uint8Array | null })._snapshotMmap = null;
  (db as { _snapshotCache: unknown })._snapshotCache = null;

  // Close pager (which closes the file descriptor)
  if (db._pager) {
    (db._pager as FilePager).close();
  }
}

/**
 * Helper to update ID allocators from a WAL record
 */
interface AllocatorRefs {
  nextNodeId: () => number;
  nextLabelId: () => number;
  nextEtypeId: () => number;
  nextPropkeyId: () => number;
  setNextNodeId: (id: number) => void;
  setNextLabelId: (id: number) => void;
  setNextEtypeId: (id: number) => void;
  setNextPropkeyId: (id: number) => void;
}

function updateAllocatorsFromRecord(record: ParsedWalRecord, refs: AllocatorRefs): void {
  switch (record.type) {
    case WalRecordType.CREATE_NODE: {
      const data = parseCreateNodePayload(record.payload);
      if (data.nodeId >= refs.nextNodeId()) {
        refs.setNextNodeId(data.nodeId + 1);
      }
      break;
    }
    case WalRecordType.DEFINE_LABEL: {
      const data = parseDefineLabelPayload(record.payload);
      if (data.labelId >= refs.nextLabelId()) {
        refs.setNextLabelId(data.labelId + 1);
      }
      break;
    }
    case WalRecordType.DEFINE_ETYPE: {
      const data = parseDefineEtypePayload(record.payload);
      if (data.etypeId >= refs.nextEtypeId()) {
        refs.setNextEtypeId(data.etypeId + 1);
      }
      break;
    }
    case WalRecordType.DEFINE_PROPKEY: {
      const data = parseDefinePropkeyPayload(record.payload);
      if (data.propkeyId >= refs.nextPropkeyId()) {
        refs.setNextPropkeyId(data.propkeyId + 1);
      }
      break;
    }
  }
}

/**
 * Rebuild MVCC version chains from committed transactions
 */
function rebuildVersionChains(
  mvcc: MvccManager,
  committed: Map<bigint, ParsedWalRecord[]>,
  delta: DeltaState
): void {
  let rebuildCommitTs = 1n;
  
  for (const [txid, records] of committed) {
    const commitTs = rebuildCommitTs++;
    
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

/**
 * Check if a path looks like a single-file database
 */
export function isSingleFilePath(path: string): boolean {
  return path.endsWith(EXT_RAYDB);
}

/**
 * Get database file extension
 */
export function getDbExtension(): string {
  return EXT_RAYDB;
}
