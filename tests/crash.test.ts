/**
 * Crash and corruption tests
 */

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { MANIFEST_FILENAME, WAL_DIR, walFilename } from "../src/constants.ts";
import {
  type GraphDB,
  beginTx,
  closeGraphDB,
  commit,
  createNode,
  getNodeByKey,
  openGraphDB,
  optimize,
} from "../src/index.ts";

describe("WAL Truncation", () => {
  let testDir: string;

  beforeEach(async () => {
    testDir = await mkdtemp(join(tmpdir(), "ray-crash-test-"));
  });

  afterEach(async () => {
    await rm(testDir, { recursive: true, force: true });
  });

  test("truncated WAL record is ignored", async () => {
    // Create database with some data
    const db1 = await openGraphDB(testDir);

    const tx1 = beginTx(db1);
    createNode(tx1, { key: "node1" });
    await commit(tx1);

    const tx2 = beginTx(db1);
    createNode(tx2, { key: "node2" });
    await commit(tx2);

    await closeGraphDB(db1);

    // Truncate the WAL (remove last few bytes)
    const walPath = join(testDir, WAL_DIR, walFilename(1n));
    const walData = await readFile(walPath);
    const truncated = walData.slice(0, walData.length - 20);
    await writeFile(walPath, truncated);

    // Reopen - should recover what it can
    const db2 = await openGraphDB(testDir);

    // First transaction should be recovered
    expect(getNodeByKey(db2, "node1")).not.toBeNull();

    // Second transaction may or may not be recovered depending on truncation point
    // But database should open successfully

    await closeGraphDB(db2);
  });

  test("completely corrupted WAL tail is ignored", async () => {
    const db1 = await openGraphDB(testDir);

    const tx = beginTx(db1);
    createNode(tx, { key: "safe-node" });
    await commit(tx);

    await closeGraphDB(db1);

    // Append garbage to WAL
    const walPath = join(testDir, WAL_DIR, walFilename(1n));
    const walData = await readFile(walPath);
    const garbage = new Uint8Array(100);
    for (let i = 0; i < garbage.length; i++) {
      garbage[i] = Math.floor(Math.random() * 256);
    }
    const corrupted = Buffer.concat([walData, garbage]);
    await writeFile(walPath, corrupted);

    // Reopen - should still work
    const db2 = await openGraphDB(testDir);

    expect(getNodeByKey(db2, "safe-node")).not.toBeNull();

    await closeGraphDB(db2);
  });
});

describe("Manifest Corruption", () => {
  let testDir: string;

  beforeEach(async () => {
    testDir = await mkdtemp(join(tmpdir(), "ray-manifest-test-"));
  });

  afterEach(async () => {
    await rm(testDir, { recursive: true, force: true });
  });

  test("corrupted manifest CRC is detected", async () => {
    const db1 = await openGraphDB(testDir);
    await closeGraphDB(db1);

    // Corrupt manifest
    const manifestPath = join(testDir, MANIFEST_FILENAME);
    const manifestData = await readFile(manifestPath);
    manifestData[10] ^= 0xff; // Flip some bits
    await writeFile(manifestPath, manifestData);

    // Opening should fail with CRC or version error (depending on which byte was flipped)
    await expect(openGraphDB(testDir)).rejects.toThrow();
  });
});

describe("Snapshot Corruption", () => {
  let testDir: string;

  beforeEach(async () => {
    testDir = await mkdtemp(join(tmpdir(), "ray-snap-corrupt-test-"));
  });

  afterEach(async () => {
    await rm(testDir, { recursive: true, force: true });
  });

  test("corrupted snapshot CRC is detected", async () => {
    const db1 = await openGraphDB(testDir);

    const tx = beginTx(db1);
    createNode(tx, { key: "test" });
    await commit(tx);

    await optimize(db1);
    await closeGraphDB(db1);

    // Corrupt snapshot
    const snapshotsDir = join(testDir, "snapshots");
    const files = await readdir(snapshotsDir);
    const snapshotFile = files.find((f) => f.endsWith(".gds"));

    if (snapshotFile) {
      const snapshotPath = join(snapshotsDir, snapshotFile);
      const data = await readFile(snapshotPath);
      data[100] ^= 0xff; // Flip some bits
      await writeFile(snapshotPath, data);

      // Snapshot corruption is caught but DB continues with warning
      // The openGraphDB catches snapshot load errors and logs warning
      const db = await openGraphDB(testDir);
      // Database should be opened but with no valid snapshot
      expect(db._snapshot).toBeNull();
      await closeGraphDB(db);
    }
  });
});

describe("Recovery Scenarios", () => {
  let testDir: string;

  beforeEach(async () => {
    testDir = await mkdtemp(join(tmpdir(), "ray-recovery-scenario-"));
  });

  afterEach(async () => {
    await rm(testDir, { recursive: true, force: true });
  });

  test("crash during transaction (no COMMIT)", async () => {
    const db1 = await openGraphDB(testDir);

    // Committed transaction
    const tx1 = beginTx(db1);
    createNode(tx1, { key: "committed" });
    await commit(tx1);

    // Start but don't commit
    const tx2 = beginTx(db1);
    createNode(tx2, { key: "uncommitted" });

    // Close without committing (simulates crash)
    await closeGraphDB(db1);

    // Reopen
    const db2 = await openGraphDB(testDir);

    // Committed data should be there
    expect(getNodeByKey(db2, "committed")).not.toBeNull();

    // Uncommitted data should not be there
    expect(getNodeByKey(db2, "uncommitted")).toBeNull();

    await closeGraphDB(db2);
  });

  test("recovery with multiple WAL segments", async () => {
    const db1 = await openGraphDB(testDir);

    // Create data
    const tx1 = beginTx(db1);
    createNode(tx1, { key: "before-compact" });
    await commit(tx1);

    // Compact (creates new WAL segment)
    await optimize(db1);

    // Create more data
    const tx2 = beginTx(db1);
    createNode(tx2, { key: "after-compact" });
    await commit(tx2);

    await closeGraphDB(db1);

    // Reopen
    const db2 = await openGraphDB(testDir);

    expect(getNodeByKey(db2, "before-compact")).not.toBeNull();
    expect(getNodeByKey(db2, "after-compact")).not.toBeNull();

    await closeGraphDB(db2);
  });

  test("many transactions recovery", async () => {
    const db1 = await openGraphDB(testDir);

    // Create many small transactions
    for (let i = 0; i < 100; i++) {
      const tx = beginTx(db1);
      createNode(tx, { key: `node-${i}` });
      await commit(tx);
    }

    await closeGraphDB(db1);

    // Reopen
    const db2 = await openGraphDB(testDir);

    // All nodes should be recovered
    for (let i = 0; i < 100; i++) {
      expect(getNodeByKey(db2, `node-${i}`)).not.toBeNull();
    }

    await closeGraphDB(db2);
  });
});
