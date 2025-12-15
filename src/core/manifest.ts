/**
 * Manifest file handling with atomic updates
 */

import { join } from "node:path";
import {
  INITIAL_SNAPSHOT_GEN,
  INITIAL_WAL_SEG,
  MAGIC_MANIFEST,
  MANIFEST_FILENAME,
  MIN_READER_MANIFEST,
  VERSION_MANIFEST,
} from "../constants.ts";
import type { ManifestV1 } from "../types.ts";
import { MANIFEST_SIZE } from "../types.ts";
import {
  readU32,
  readU64,
  viewOf,
  writeU32,
  writeU64,
} from "../util/binary.ts";
import { crc32c } from "../util/crc.ts";

/**
 * Create a new empty manifest
 */
export function createEmptyManifest(): ManifestV1 {
  return {
    magic: MAGIC_MANIFEST,
    version: VERSION_MANIFEST,
    minReaderVersion: MIN_READER_MANIFEST,
    reserved: 0,
    activeSnapshotGen: INITIAL_SNAPSHOT_GEN,
    prevSnapshotGen: 0n,
    activeWalSeg: INITIAL_WAL_SEG,
    reserved2: [0n, 0n, 0n, 0n, 0n],
    crc32c: 0, // Will be computed on write
  };
}

/**
 * Serialize manifest to bytes
 */
export function serializeManifest(manifest: ManifestV1): Uint8Array {
  const buffer = new Uint8Array(MANIFEST_SIZE);
  const view = viewOf(buffer);

  let offset = 0;

  // Header
  writeU32(view, offset, manifest.magic);
  offset += 4;
  writeU32(view, offset, manifest.version);
  offset += 4;
  writeU32(view, offset, manifest.minReaderVersion);
  offset += 4;
  writeU32(view, offset, manifest.reserved);
  offset += 4;

  // Snapshot and WAL info
  writeU64(view, offset, manifest.activeSnapshotGen);
  offset += 8;
  writeU64(view, offset, manifest.prevSnapshotGen);
  offset += 8;
  writeU64(view, offset, manifest.activeWalSeg);
  offset += 8;

  // Reserved u64[5]
  for (let i = 0; i < 5; i++) {
    writeU64(view, offset, manifest.reserved2[i] ?? 0n);
    offset += 8;
  }

  // Compute CRC over everything except the CRC field itself
  const crc = crc32c(buffer.subarray(0, offset));
  writeU32(view, offset, crc);

  return buffer;
}

/**
 * Parse manifest from bytes
 */
export function parseManifest(buffer: Uint8Array): ManifestV1 {
  if (buffer.length < MANIFEST_SIZE) {
    throw new Error(`Manifest too small: ${buffer.length} < ${MANIFEST_SIZE}`);
  }

  const view = viewOf(buffer);
  let offset = 0;

  // Header
  const magic = readU32(view, offset);
  offset += 4;
  if (magic !== MAGIC_MANIFEST) {
    throw new Error(`Invalid manifest magic: 0x${magic.toString(16)}`);
  }

  const version = readU32(view, offset);
  offset += 4;
  const minReaderVersion = readU32(view, offset);
  offset += 4;

  if (MIN_READER_MANIFEST < minReaderVersion) {
    throw new Error(
      `Manifest requires reader version ${minReaderVersion}, we are ${MIN_READER_MANIFEST}`,
    );
  }

  const reserved = readU32(view, offset);
  offset += 4;

  // Snapshot and WAL info
  const activeSnapshotGen = readU64(view, offset);
  offset += 8;
  const prevSnapshotGen = readU64(view, offset);
  offset += 8;
  const activeWalSeg = readU64(view, offset);
  offset += 8;

  // Reserved u64[5]
  const reserved2: bigint[] = [];
  for (let i = 0; i < 5; i++) {
    reserved2.push(readU64(view, offset));
    offset += 8;
  }

  // CRC verification
  const storedCrc = readU32(view, offset);
  const computedCrc = crc32c(buffer.subarray(0, offset));

  if (storedCrc !== computedCrc) {
    throw new Error(
      `Manifest CRC mismatch: stored=0x${storedCrc.toString(16)}, computed=0x${computedCrc.toString(16)}`,
    );
  }

  return {
    magic,
    version,
    minReaderVersion,
    reserved,
    activeSnapshotGen,
    prevSnapshotGen,
    activeWalSeg,
    reserved2,
    crc32c: storedCrc,
  };
}

/**
 * Read manifest from database path
 */
export async function readManifest(dbPath: string): Promise<ManifestV1 | null> {
  const manifestPath = join(dbPath, MANIFEST_FILENAME);

  try {
    const file = Bun.file(manifestPath);
    if (!(await file.exists())) {
      return null;
    }

    const buffer = new Uint8Array(await file.arrayBuffer());
    return parseManifest(buffer);
  } catch (err) {
    if ((err as NodeJS.ErrnoException).code === "ENOENT") {
      return null;
    }
    throw err;
  }
}

/**
 * Write manifest atomically using tmp + rename pattern
 */
export async function writeManifest(
  dbPath: string,
  manifest: ManifestV1,
): Promise<void> {
  const manifestPath = join(dbPath, MANIFEST_FILENAME);
  const tmpPath = join(dbPath, "manifest.tmp");

  const data = serializeManifest(manifest);

  // Write to temp file
  await Bun.write(tmpPath, data);

  // Sync temp file to disk before rename for durability
  const fs = await import("node:fs/promises");
  const fd = await fs.open(tmpPath, "r+");
  await fd.sync();
  await fd.close();

  // Atomic rename
  await fs.rename(tmpPath, manifestPath);

  // Sync directory (best effort - not all platforms support this)
  try {
    const dirFd = await fs.open(dbPath, "r");
    await dirFd.sync();
    await dirFd.close();
  } catch {
    // Directory sync not supported on all platforms
  }
}

/**
 * Update manifest with new snapshot generation
 */
export function updateManifestForCompaction(
  manifest: ManifestV1,
  newSnapshotGen: bigint,
  newWalSeg: bigint,
): ManifestV1 {
  return {
    ...manifest,
    prevSnapshotGen: manifest.activeSnapshotGen,
    activeSnapshotGen: newSnapshotGen,
    activeWalSeg: newWalSeg,
  };
}
