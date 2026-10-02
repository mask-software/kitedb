/**
 * Wave-3 reproductions for the playground API (findings C7 and C8).
 *
 * C7: GET /api/replication/snapshot/latest reads the whole live database file into memory and
 *     base64s it, with no size limit. Env contract: PLAYGROUND_SNAPSHOT_MAX_BYTES (read per
 *     request) caps the size of a snapshot returned with includeData=true.
 * C8a: two concurrent /api/db/open calls both close, then both open; the handle opened first is
 *      overwritten and never closed, so its file stays locked.
 * C8b: if closing the database throws, its temp directory is never removed.
 */

import { afterEach, describe, expect, test } from "bun:test";
import { existsSync } from "node:fs";
import { mkdtemp, realpath, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";

const { Elysia } = await import("elysia");
const { apiRoutes } = await import("./routes.ts");
const { closeDatabase, getDb, getDbPath, nodes, edges, FileNode } = await import("./db.ts");
const { kite } = await import("../../../ray-rs/ts/index.ts");

const app = new Elysia().use(apiRoutes);
const ADMIN_TOKEN = "w3-admin-token";

const scratchDirs: string[] = [];
const PREVIOUS_ENV: Record<string, string | undefined> = {
  PLAYGROUND_DATA_DIR: process.env.PLAYGROUND_DATA_DIR,
  PLAYGROUND_SNAPSHOT_MAX_BYTES: process.env.PLAYGROUND_SNAPSHOT_MAX_BYTES,
  REPLICATION_ADMIN_TOKEN: process.env.REPLICATION_ADMIN_TOKEN,
  REPLICATION_ADMIN_AUTH_MODE: process.env.REPLICATION_ADMIN_AUTH_MODE,
};

afterEach(async () => {
  await closeDatabase();
  for (const [key, value] of Object.entries(PREVIOUS_ENV)) {
    if (value === undefined) {
      delete process.env[key];
    } else {
      process.env[key] = value;
    }
  }
  while (scratchDirs.length > 0) {
    await rm(scratchDirs.pop()!, { recursive: true, force: true });
  }
});

/** A fresh PLAYGROUND_DATA_DIR; /api/db/open only accepts paths inside it. */
async function useScratchDataDir(): Promise<string> {
  const dir = await mkdtemp(join(tmpdir(), "kitedb-playground-w3-"));
  scratchDirs.push(dir);
  process.env.PLAYGROUND_DATA_DIR = dir;
  return dir;
}

async function send<T = Record<string, unknown>>(
  method: string,
  path: string,
  json?: unknown,
  headers: Record<string, string> = {},
): Promise<{ status: number; body: T }> {
  const response = await app.handle(
    new Request(`http://localhost${path}`, {
      method,
      headers: {
        ...(json !== undefined ? { "content-type": "application/json" } : {}),
        ...headers,
      },
      body: json !== undefined ? JSON.stringify(json) : undefined,
    }),
  );
  return { status: response.status, body: (await response.json()) as T };
}

describe("C7: replication snapshot size", () => {
  test("C7: snapshot/latest?includeData=true refuses a database larger than PLAYGROUND_SNAPSHOT_MAX_BYTES", async () => {
    const dataDir = await useScratchDataDir();
    process.env.REPLICATION_ADMIN_TOKEN = ADMIN_TOKEN;
    delete process.env.REPLICATION_ADMIN_AUTH_MODE;
    const auth = { Authorization: `Bearer ${ADMIN_TOKEN}` };

    const dbPath = join(dataDir, "primary.kitedb");
    const opened = await send<{ success: boolean; error?: string }>("POST", "/api/db/open", {
      path: dbPath,
      options: { replicationRole: "primary" },
    });
    expect(opened.body).toEqual({ success: true });
    const db = getDb()!;
    for (let i = 0; i < 3; i++) {
      await db
        .insert(FileNode)
        .values({ key: `src/w3-${i}.ts`, path: `src/w3-${i}.ts`, language: "typescript" })
        .returning();
    }

    const fileSize = (await stat(dbPath)).size;
    const cap = Math.floor(fileSize / 2);
    process.env.PLAYGROUND_SNAPSHOT_MAX_BYTES = String(cap);

    const withData = await send<{
      success: boolean;
      error?: string;
      snapshot?: { byteLength?: number; dataBase64?: string };
    }>("GET", "/api/replication/snapshot/latest?includeData=true", undefined, auth);
    // The database is twice the cap: no bytes may be returned.
    expect(withData.body.snapshot?.dataBase64).toBeUndefined();
    expect(withData.body.success).toBe(false);
    expect(withData.body.error ?? "").toMatch(/exceeds|too large|limit/i);

    // Metadata needs no inline payload, so the cap doesn't apply to it.
    const metadataOnly = await send<{
      success: boolean;
      snapshot?: { byteLength?: number; sha256?: string; dataBase64?: string };
    }>("GET", "/api/replication/snapshot/latest?includeData=false", undefined, auth);
    expect(metadataOnly.body.success).toBe(true);
    expect(metadataOnly.body.snapshot?.byteLength).toBeGreaterThan(cap);
    expect(metadataOnly.body.snapshot?.sha256).toBeTruthy();
    expect(metadataOnly.body.snapshot?.dataBase64).toBeUndefined();
  });
});

describe("C8: database handle lifecycle", () => {
  test("C8a: concurrent /api/db/open calls leave no handle open once the database is closed", async () => {
    // Canonical paths: the playground stores the realpath (macOS tmpdir is behind /var -> /private/var).
    const dataDir = await realpath(await useScratchDataDir());
    const pathA = join(dataDir, "a.kitedb");
    const pathB = join(dataDir, "b.kitedb");

    const results = await Promise.all([
      send<{ success: boolean; error?: string }>("POST", "/api/db/open", { path: pathA }),
      send<{ success: boolean; error?: string }>("POST", "/api/db/open", { path: pathB }),
    ]);
    for (const result of results) {
      expect(result.body).toEqual({ success: true });
    }
    expect([pathA, pathB]).toContain(getDbPath()!);

    await send("POST", "/api/db/close");

    // A handle that is still open holds the file lock, and opening its file again fails.
    const reopenErrors: string[] = [];
    for (const path of [pathA, pathB]) {
      try {
        const db = await kite(path, { nodes, edges });
        await db.close();
      } catch (error) {
        reopenErrors.push(`${path}: ${(error as Error).message}`);
      }
    }
    expect(reopenErrors).toEqual([]);
  });

  test("C8b: closing removes the temp directory even when close throws", async () => {
    const created = await send<{ success: boolean }>("POST", "/api/db/demo");
    expect(created.body.success).toBe(true);
    const tempDir = dirname(getDbPath()!);
    expect(existsSync(tempDir)).toBe(true);
    // Removed after the test if the close leaves it behind.
    scratchDirs.push(tempDir);

    // Close for real (so the handle doesn't leak), then report a failure.
    const db = getDb()!;
    const realClose = db.close.bind(db);
    db.close = async () => {
      await realClose();
      throw new Error("simulated close failure");
    };

    await send("POST", "/api/db/close");

    expect(getDb()).toBeNull();
    expect(existsSync(tempDir)).toBe(false);
  });
});
