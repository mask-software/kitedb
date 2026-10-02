/**
 * raydb-b4 replication-core reproductions for the playground's replication transport endpoints.
 *
 * GET /api/replication/transport/snapshot and /api/replication/transport/log called
 * `exportReplication*TransportJson` on the connected database, which is a Kite, and Kite had no
 * such methods: both endpoints always answered "Replication method unavailable".
 */

import { afterEach, describe, expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

const { Elysia } = await import("elysia");
const { apiRoutes } = await import("./routes.ts");
const { closeDatabase, getDb, FileNode } = await import("./db.ts");

const app = new Elysia().use(apiRoutes);
const ADMIN_TOKEN = "b4-transport-admin-token";
const AUTH_HEADER = { Authorization: `Bearer ${ADMIN_TOKEN}` };

const scratchDirs: string[] = [];
const PREVIOUS_ENV: Record<string, string | undefined> = {
  PLAYGROUND_DATA_DIR: process.env.PLAYGROUND_DATA_DIR,
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

async function getJson<T>(path: string): Promise<{ status: number; body: T }> {
  const response = await app.handle(
    new Request(`http://localhost${path}`, { headers: AUTH_HEADER }),
  );
  return { status: response.status, body: (await response.json()) as T };
}

/** Open a primary in a fresh data dir and commit `commits` nodes. */
async function openPrimaryWithCommits(commits: number): Promise<string> {
  const dir = await mkdtemp(join(tmpdir(), "kitedb-playground-b4-"));
  scratchDirs.push(dir);
  process.env.PLAYGROUND_DATA_DIR = dir;
  process.env.REPLICATION_ADMIN_TOKEN = ADMIN_TOKEN;
  delete process.env.REPLICATION_ADMIN_AUTH_MODE;

  const response = await app.handle(
    new Request("http://localhost/api/db/open", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        path: join(dir, "primary.kitedb"),
        options: { replicationRole: "primary" },
      }),
    }),
  );
  expect(((await response.json()) as { success: boolean }).success).toBe(true);

  const db = getDb();
  expect(db).not.toBeNull();
  for (let i = 0; i < commits; i++) {
    await db!
      .insert(FileNode)
      .values({ key: `src/b4-${i}.ts`, path: `src/b4-${i}.ts`, language: "typescript" })
      .returning();
  }
  return dir;
}

describe("replication transport endpoints (Kite)", () => {
  test("transport/snapshot exports a snapshot with data and no filesystem path", async () => {
    const dir = await openPrimaryWithCommits(3);

    const response = await getJson<{
      success: boolean;
      error?: string;
      snapshot?: Record<string, unknown>;
    }>("/api/replication/transport/snapshot?includeData=true");

    expect(response.status).toBe(200);
    expect(response.body.error).toBeUndefined();
    expect(response.body.success).toBe(true);
    const snapshot = response.body.snapshot!;
    expect(snapshot.db_path).toBeUndefined();
    expect(JSON.stringify(snapshot)).not.toContain(dir);
    expect(snapshot.generation).toMatch(/^[0-9a-f]{16}$/);
    const status = getDb()!.primaryReplicationStatus()!;
    expect(snapshot.head_log_index).toBe(status.headLogIndex);
    const data = Buffer.from(snapshot.data_base64 as string, "base64");
    expect(data.byteLength).toBe(snapshot.byte_length as number);
  });

  test("transport/log pages frames with payloads and the sidecar generation", async () => {
    await openPrimaryWithCommits(3);

    const first = await getJson<{
      success: boolean;
      error?: string;
      generation?: string;
      frame_count?: number;
      next_cursor?: string;
      eof?: boolean;
      frames?: Array<{ log_index: number; payload_base64?: string | null }>;
    }>("/api/replication/transport/log?maxFrames=2&includePayload=true");

    expect(first.status).toBe(200);
    expect(first.body.error).toBeUndefined();
    expect(first.body.success).toBe(true);
    expect(first.body.generation).toMatch(/^[0-9a-f]{16}$/);
    expect(first.body.frame_count).toBe(2);
    expect(first.body.eof).toBe(false);
    for (const frame of first.body.frames!) {
      expect(Buffer.from(frame.payload_base64!, "base64").byteLength).toBeGreaterThan(0);
    }

    const rest = await getJson<{
      success: boolean;
      frames?: Array<{ log_index: number }>;
      eof?: boolean;
    }>(`/api/replication/transport/log?cursor=${encodeURIComponent(first.body.next_cursor!)}`);
    expect(rest.body.success).toBe(true);
    expect(rest.body.eof).toBe(true);
    const lastFirst = first.body.frames![first.body.frames!.length - 1].log_index;
    expect(rest.body.frames!.every((frame) => frame.log_index > lastFirst)).toBe(true);
  });
});
