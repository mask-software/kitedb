import { afterEach, beforeAll, describe, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

process.env.REPLICATION_ADMIN_TOKEN = "test-repl-admin-token";
const PREVIOUS_DATA_DIR = process.env.PLAYGROUND_DATA_DIR;

const { Elysia } = await import("elysia");
const { apiRoutes } = await import("./routes.ts");
const { closeDatabase, getDb, FileNode } = await import("./db.ts");

const AUTH_HEADER = {
  Authorization: `Bearer ${process.env.REPLICATION_ADMIN_TOKEN}`,
};

interface JsonResponse<T = Record<string, unknown>> {
  status: number;
  body: T;
}

interface TextResponse {
  status: number;
  body: string;
}

interface ManifestEnvelope {
  version: number;
  payload_crc32: number;
  manifest: {
    version: number;
    epoch: number;
    head_log_index: number;
    retained_floor: number;
    active_segment_id: number;
    segments: Array<{
      id: number;
      start_log_index: number;
      end_log_index: number;
      size_bytes: number;
    }>;
  };
}

let app: InstanceType<typeof Elysia>;
let tempDir: string;
let dbPath: string;

type ReplicationAuthEnvKey =
  | "REPLICATION_ADMIN_TOKEN"
  | "REPLICATION_ADMIN_AUTH_MODE"
  | "REPLICATION_MTLS_HEADER"
  | "REPLICATION_MTLS_SUBJECT_REGEX"
  | "REPLICATION_MTLS_NATIVE_TLS"
  | "PLAYGROUND_TLS_REQUEST_CERT"
  | "PLAYGROUND_TLS_REJECT_UNAUTHORIZED";

async function withReplicationAuthEnv<T>(
  overrides: Partial<Record<ReplicationAuthEnvKey, string | null>>,
  run: () => Promise<T>,
): Promise<T> {
  const keys: ReplicationAuthEnvKey[] = [
    "REPLICATION_ADMIN_TOKEN",
    "REPLICATION_ADMIN_AUTH_MODE",
    "REPLICATION_MTLS_HEADER",
    "REPLICATION_MTLS_SUBJECT_REGEX",
    "REPLICATION_MTLS_NATIVE_TLS",
    "PLAYGROUND_TLS_REQUEST_CERT",
    "PLAYGROUND_TLS_REJECT_UNAUTHORIZED",
  ];
  const previous: Partial<Record<ReplicationAuthEnvKey, string | undefined>> = {};
  for (const key of keys) {
    previous[key] = process.env[key];
  }

  for (const [key, value] of Object.entries(overrides) as Array<
    [ReplicationAuthEnvKey, string | null]
  >) {
    if (value === null) {
      delete process.env[key];
    } else {
      process.env[key] = value;
    }
  }

  try {
    return await run();
  } finally {
    for (const key of keys) {
      const value = previous[key];
      if (value === undefined) {
        delete process.env[key];
      } else {
        process.env[key] = value;
      }
    }
  }
}

async function requestJson<T = Record<string, unknown>>(
  method: string,
  path: string,
  body?: unknown,
  headers?: Record<string, string>,
  origin = "http://localhost",
): Promise<JsonResponse<T>> {
  const request = new Request(`${origin}${path}`, {
    method,
    headers: {
      ...(body !== undefined ? { "content-type": "application/json" } : {}),
      ...(headers ?? {}),
    },
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });

  const response = await app.handle(request);
  return {
    status: response.status,
    body: (await response.json()) as T,
  };
}

async function requestText(
  method: string,
  path: string,
  body?: unknown,
  headers?: Record<string, string>,
  origin = "http://localhost",
): Promise<TextResponse> {
  const request = new Request(`${origin}${path}`, {
    method,
    headers: {
      ...(body !== undefined ? { "content-type": "application/json" } : {}),
      ...(headers ?? {}),
    },
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });

  const response = await app.handle(request);
  return {
    status: response.status,
    body: await response.text(),
  };
}

/** Fresh temp dir per test; /api/db/open only accepts paths inside PLAYGROUND_DATA_DIR. */
async function useTempDataDir(): Promise<void> {
  tempDir = await mkdtemp(join(tmpdir(), "playground-repl-test-"));
  process.env.PLAYGROUND_DATA_DIR = tempDir;
}

async function openPrimary(): Promise<void> {
  await useTempDataDir();
  dbPath = join(tempDir, "primary.kitedb");

  const response = await requestJson<{ success: boolean; error?: string }>(
    "POST",
    "/api/db/open",
    {
      path: dbPath,
      options: {
        replicationRole: "primary",
      },
    },
  );

  expect(response.status).toBe(200);
  expect(response.body.success).toBe(true);
}

// Keys are unique across calls: a test may append to the same database more
// than once, and inserting an existing key is an error.
let nextCommitFile = 0;

async function appendCommits(count: number): Promise<void> {
  const db = getDb();
  expect(db).not.toBeNull();
  for (let i = 0; i < count; i++) {
    const file = `src/file-${nextCommitFile++}.ts`;
    await db!
      .insert(FileNode)
      .values({
        key: file,
        path: file,
        language: "typescript",
      })
      .returning();
  }
}

beforeAll(() => {
  app = new Elysia().use(apiRoutes);
});

afterEach(async () => {
  await closeDatabase();
  if (tempDir) {
    await rm(tempDir, { recursive: true, force: true });
  }
  if (PREVIOUS_DATA_DIR === undefined) {
    delete process.env.PLAYGROUND_DATA_DIR;
  } else {
    process.env.PLAYGROUND_DATA_DIR = PREVIOUS_DATA_DIR;
  }
});

describe("replication log endpoints", () => {
  test("paginates log frames using maxFrames + nextCursor", async () => {
    await openPrimary();
    await appendCommits(5);

    const first = await requestJson<{
      success: boolean;
      frameCount: number;
      eof: boolean;
      nextCursor: string | null;
      frames: Array<{ logIndex: string }>;
    }>("GET", "/api/replication/log?maxFrames=2", undefined, AUTH_HEADER);

    expect(first.status).toBe(200);
    expect(first.body.success).toBe(true);
    expect(first.body.frameCount).toBe(2);
    expect(first.body.eof).toBe(false);
    expect(first.body.nextCursor).toBeTruthy();
    expect(first.body.frames.length).toBe(2);

    const lastFirstLogIndex = BigInt(first.body.frames[1].logIndex);
    const second = await requestJson<{
      success: boolean;
      frameCount: number;
      frames: Array<{ logIndex: string }>;
      cursor: string | null;
    }>(
      "GET",
      `/api/replication/log?maxFrames=2&cursor=${encodeURIComponent(first.body.nextCursor!)}`,
      undefined,
      AUTH_HEADER,
    );

    expect(second.status).toBe(200);
    expect(second.body.success).toBe(true);
    expect(second.body.cursor).toBe(first.body.nextCursor);
    expect(second.body.frameCount).toBeGreaterThan(0);
    expect(BigInt(second.body.frames[0].logIndex) > lastFirstLogIndex).toBe(true);
  });

  test("respects maxBytes and returns one frame minimum", async () => {
    await openPrimary();
    await appendCommits(3);

    const response = await requestJson<{
      success: boolean;
      frameCount: number;
      eof: boolean;
      totalBytes: number;
      nextCursor: string | null;
    }>("GET", "/api/replication/log?maxBytes=1", undefined, AUTH_HEADER);

    expect(response.status).toBe(200);
    expect(response.body.success).toBe(true);
    expect(response.body.frameCount).toBe(1);
    expect(response.body.totalBytes).toBeGreaterThan(0);
    expect(response.body.eof).toBe(false);
    expect(response.body.nextCursor).toBeTruthy();
  });

  test("returns structured error on malformed cursor", async () => {
    await openPrimary();
    await appendCommits(1);

    const response = await requestJson<{ success: boolean; error?: string }>(
      "GET",
      "/api/replication/log?cursor=bad-cursor",
      undefined,
      AUTH_HEADER,
    );

    expect(response.status).toBe(200);
    expect(response.body.success).toBe(false);
    expect(response.body.error).toBeTruthy();
  });

  test("returns structured error on malformed 4-part cursor with non-numeric components", async () => {
    await openPrimary();
    await appendCommits(2);

    const response = await requestJson<{ success: boolean; error?: string }>(
      "GET",
      "/api/replication/log?cursor=1:abc:def:ghi",
      undefined,
      AUTH_HEADER,
    );

    expect(response.status).toBe(200);
    expect(response.body.success).toBe(false);
    expect(response.body.error).toBeTruthy();
  });

  test("returns structured error on cursor with too many components", async () => {
    await openPrimary();
    await appendCommits(2);

    const response = await requestJson<{ success: boolean; error?: string }>(
      "GET",
      "/api/replication/log?cursor=1:2:3:4:5",
      undefined,
      AUTH_HEADER,
    );

    expect(response.status).toBe(200);
    expect(response.body.success).toBe(false);
    expect(response.body.error).toBeTruthy();
  });

  test("accepts cursors with empty numeric components as zero (current behavior)", async () => {
    await openPrimary();
    await appendCommits(2);

    const emptySegmentId = await requestJson<{
      success: boolean;
      frameCount: number;
      cursor: string | null;
      nextCursor: string | null;
    }>(
      "GET",
      "/api/replication/log?cursor=1::3:4",
      undefined,
      AUTH_HEADER,
    );
    expect(emptySegmentId.status).toBe(200);
    expect(emptySegmentId.body.success).toBe(true);
    expect(emptySegmentId.body.cursor).toBe("1::3:4");
    expect(emptySegmentId.body.frameCount).toBe(0);
    expect(emptySegmentId.body.nextCursor).toBe("1::3:4");

    const emptyEpoch = await requestJson<{
      success: boolean;
      frameCount: number;
      headLogIndex: number;
      cursor: string | null;
      nextCursor: string | null;
    }>(
      "GET",
      "/api/replication/log?cursor=:2",
      undefined,
      AUTH_HEADER,
    );
    expect(emptyEpoch.status).toBe(200);
    expect(emptyEpoch.body.success).toBe(true);
    expect(emptyEpoch.body.cursor).toBe(":2");
    // ":2" parses as epoch 0, so every retained epoch-1 frame is after it: the schema
    // bootstrap frame Kite::open logs on a fresh primary plus the 2 commits.
    expect(emptyEpoch.body.frameCount).toBe(emptyEpoch.body.headLogIndex);
    expect(emptyEpoch.body.nextCursor).toBeTruthy();
  });

  test("accepts 2-part cursor format epoch:logIndex", async () => {
    await openPrimary();
    await appendCommits(5);

    const first = await requestJson<{
      success: boolean;
      frameCount: number;
      frames: Array<{ epoch: string; logIndex: string }>;
    }>("GET", "/api/replication/log?maxFrames=2", undefined, AUTH_HEADER);
    expect(first.status).toBe(200);
    expect(first.body.success).toBe(true);
    expect(first.body.frameCount).toBe(2);

    const cursor = `${first.body.frames[0].epoch}:${first.body.frames[0].logIndex}`;
    const second = await requestJson<{
      success: boolean;
      frameCount: number;
      frames: Array<{ logIndex: string }>;
    }>(
      "GET",
      `/api/replication/log?maxFrames=4&cursor=${encodeURIComponent(cursor)}`,
      undefined,
      AUTH_HEADER,
    );

    expect(second.status).toBe(200);
    expect(second.body.success).toBe(true);
    expect(second.body.frameCount).toBeGreaterThan(0);
    expect(BigInt(second.body.frames[0].logIndex) > BigInt(first.body.frames[0].logIndex)).toBe(
      true,
    );
  });

  test("4-part cursor resumes consistently at frame start vs frame end offset", async () => {
    await openPrimary();
    await appendCommits(5);

    const firstPage = await requestJson<{
      success: boolean;
      frameCount: number;
      nextCursor: string | null;
      frames: Array<{
        epoch: string;
        segmentId: string;
        segmentOffset: string;
        logIndex: string;
        payloadBase64: string;
      }>;
    }>(
      "GET",
      "/api/replication/log?maxFrames=1&includePayload=false",
      undefined,
      AUTH_HEADER,
    );
    expect(firstPage.status).toBe(200);
    expect(firstPage.body.success).toBe(true);
    expect(firstPage.body.frameCount).toBe(1);
    expect(firstPage.body.nextCursor).toBeTruthy();

    const firstFrame = firstPage.body.frames[0];
    const startCursor = `${firstFrame.epoch}:${firstFrame.segmentId}:${firstFrame.segmentOffset}:${firstFrame.logIndex}`;

    const resumedFromStart = await requestJson<{
      success: boolean;
      frameCount: number;
      frames: Array<{ logIndex: string; payloadBase64: string }>;
    }>(
      "GET",
      `/api/replication/log?maxFrames=3&includePayload=false&cursor=${encodeURIComponent(startCursor)}`,
      undefined,
      AUTH_HEADER,
    );
    expect(resumedFromStart.status).toBe(200);
    expect(resumedFromStart.body.success).toBe(true);
    expect(resumedFromStart.body.frameCount).toBeGreaterThan(0);
    expect(
      BigInt(resumedFromStart.body.frames[0].logIndex) > BigInt(firstFrame.logIndex),
    ).toBe(true);

    const resumedFromEnd = await requestJson<{
      success: boolean;
      frameCount: number;
      frames: Array<{ logIndex: string; payloadBase64: string }>;
    }>(
      "GET",
      `/api/replication/log?maxFrames=3&includePayload=false&cursor=${encodeURIComponent(firstPage.body.nextCursor!)}`,
      undefined,
      AUTH_HEADER,
    );
    expect(resumedFromEnd.status).toBe(200);
    expect(resumedFromEnd.body.success).toBe(true);
    expect(resumedFromEnd.body.frameCount).toBeGreaterThan(0);

    expect(resumedFromEnd.body.frames[0].logIndex).toBe(
      resumedFromStart.body.frames[0].logIndex,
    );
    expect(resumedFromStart.body.frames[0].payloadBase64).toBe("");
    expect(resumedFromEnd.body.frames[0].payloadBase64).toBe("");
  });

  test("supports includePayload=false while preserving paging cursors", async () => {
    await openPrimary();
    await appendCommits(4);

    const first = await requestJson<{
      success: boolean;
      frameCount: number;
      nextCursor: string | null;
      frames: Array<{ payloadBase64: string; logIndex: string }>;
    }>(
      "GET",
      "/api/replication/log?maxFrames=2&includePayload=false",
      undefined,
      AUTH_HEADER,
    );

    expect(first.status).toBe(200);
    expect(first.body.success).toBe(true);
    expect(first.body.frameCount).toBe(2);
    expect(first.body.nextCursor).toBeTruthy();
    for (const frame of first.body.frames) {
      expect(frame.payloadBase64).toBe("");
    }

    const lastFirstLogIndex = BigInt(first.body.frames[1].logIndex);
    const second = await requestJson<{
      success: boolean;
      frameCount: number;
      frames: Array<{ payloadBase64: string; logIndex: string }>;
    }>(
      "GET",
      `/api/replication/log?maxFrames=2&includePayload=false&cursor=${encodeURIComponent(first.body.nextCursor!)}`,
      undefined,
      AUTH_HEADER,
    );

    expect(second.status).toBe(200);
    expect(second.body.success).toBe(true);
    expect(second.body.frameCount).toBeGreaterThan(0);
    for (const frame of second.body.frames) {
      expect(frame.payloadBase64).toBe("");
    }
    expect(BigInt(second.body.frames[0].logIndex) > lastFirstLogIndex).toBe(true);
  });

  test("includePayload=false still honors maxBytes paging and cursor resume", async () => {
    await openPrimary();
    await appendCommits(4);

    const first = await requestJson<{
      success: boolean;
      frameCount: number;
      totalBytes: number;
      nextCursor: string | null;
      eof: boolean;
      frames: Array<{ payloadBase64: string; logIndex: string }>;
    }>(
      "GET",
      "/api/replication/log?includePayload=false&maxBytes=1",
      undefined,
      AUTH_HEADER,
    );

    expect(first.status).toBe(200);
    expect(first.body.success).toBe(true);
    expect(first.body.frameCount).toBe(1);
    expect(first.body.totalBytes).toBeGreaterThan(0);
    expect(first.body.eof).toBe(false);
    expect(first.body.nextCursor).toBeTruthy();
    expect(first.body.frames[0].payloadBase64).toBe("");

    const firstLogIndex = BigInt(first.body.frames[0].logIndex);
    const second = await requestJson<{
      success: boolean;
      frameCount: number;
      totalBytes: number;
      nextCursor: string | null;
      eof: boolean;
      frames: Array<{ payloadBase64: string; logIndex: string }>;
    }>(
      "GET",
      `/api/replication/log?includePayload=false&maxBytes=1&cursor=${encodeURIComponent(first.body.nextCursor!)}`,
      undefined,
      AUTH_HEADER,
    );

    expect(second.status).toBe(200);
    expect(second.body.success).toBe(true);
    expect(second.body.frameCount).toBe(1);
    expect(second.body.totalBytes).toBeGreaterThan(0);
    expect(second.body.nextCursor).toBeTruthy();
    expect(second.body.frames[0].payloadBase64).toBe("");
    expect(BigInt(second.body.frames[0].logIndex) > firstLogIndex).toBe(true);
  });

  test("replication log uses sane defaults when query params are omitted", async () => {
    await openPrimary();
    await appendCommits(3);

    const response = await requestJson<{
      success: boolean;
      frameCount: number;
      eof: boolean;
      nextCursor: string | null;
      frames: Array<{ payloadBase64: string }>;
    }>("GET", "/api/replication/log", undefined, AUTH_HEADER);

    expect(response.status).toBe(200);
    expect(response.body.success).toBe(true);
    expect(response.body.frameCount).toBeGreaterThan(0);
    expect(response.body.frameCount).toBeLessThanOrEqual(256);
    expect(response.body.eof).toBe(true);
    expect(response.body.nextCursor).toBeTruthy();
    for (const frame of response.body.frames) {
      expect(frame.payloadBase64.length).toBeGreaterThan(0);
    }
  });

  test("replication log clamps out-of-range maxFrames/maxBytes query values", async () => {
    await openPrimary();
    await appendCommits(5);

    const zeroFrames = await requestJson<{
      success: boolean;
      frameCount: number;
      eof: boolean;
      frames: Array<{ payloadBase64: string }>;
    }>(
      "GET",
      "/api/replication/log?includePayload=false&maxFrames=0&maxBytes=999999999",
      undefined,
      AUTH_HEADER,
    );
    expect(zeroFrames.status).toBe(200);
    expect(zeroFrames.body.success).toBe(true);
    expect(zeroFrames.body.frameCount).toBe(1);
    expect(zeroFrames.body.eof).toBe(false);
    expect(zeroFrames.body.frames[0].payloadBase64).toBe("");

    const negativeFrames = await requestJson<{
      success: boolean;
      frameCount: number;
      eof: boolean;
      frames: Array<{ payloadBase64: string }>;
    }>(
      "GET",
      "/api/replication/log?includePayload=false&maxFrames=-10&maxBytes=999999999",
      undefined,
      AUTH_HEADER,
    );
    expect(negativeFrames.status).toBe(200);
    expect(negativeFrames.body.success).toBe(true);
    expect(negativeFrames.body.frameCount).toBe(1);
    expect(negativeFrames.body.eof).toBe(false);
    expect(negativeFrames.body.frames[0].payloadBase64).toBe("");

    const negativeBytes = await requestJson<{
      success: boolean;
      frameCount: number;
      eof: boolean;
      totalBytes: number;
      frames: Array<{ payloadBase64: string }>;
    }>(
      "GET",
      "/api/replication/log?includePayload=false&maxFrames=999999&maxBytes=-7",
      undefined,
      AUTH_HEADER,
    );
    expect(negativeBytes.status).toBe(200);
    expect(negativeBytes.body.success).toBe(true);
    expect(negativeBytes.body.frameCount).toBe(1);
    expect(negativeBytes.body.totalBytes).toBeGreaterThan(0);
    expect(negativeBytes.body.eof).toBe(false);
    expect(negativeBytes.body.frames[0].payloadBase64).toBe("");
  });

  test("replication log falls back to defaults on invalid query values", async () => {
    await openPrimary();
    await appendCommits(10);

    const response = await requestJson<{
      success: boolean;
      frameCount: number;
      eof: boolean;
      nextCursor: string | null;
      frames: Array<{ payloadBase64: string }>;
    }>(
      "GET",
      "/api/replication/log?maxFrames=abc&maxBytes=nan&includePayload=maybe",
      undefined,
      AUTH_HEADER,
    );

    expect(response.status).toBe(200);
    expect(response.body.success).toBe(true);
    expect(response.body.frameCount).toBeGreaterThan(1);
    expect(response.body.frameCount).toBeLessThanOrEqual(256);
    expect(response.body.eof).toBe(true);
    expect(response.body.nextCursor).toBeTruthy();
    for (const frame of response.body.frames) {
      expect(frame.payloadBase64.length).toBeGreaterThan(0);
    }
  });

  test("snapshot includeData=true returns consistent bytes/hash metadata", async () => {
    await openPrimary();
    await appendCommits(3);

    const response = await requestJson<{
      success: boolean;
      role?: string;
      snapshot?: {
        dbPath?: string;
        byteLength?: number;
        sha256?: string;
        dataBase64?: string;
      };
    }>("GET", "/api/replication/snapshot/latest?includeData=true", undefined, AUTH_HEADER);

    expect(response.status).toBe(200);
    expect(response.body.success).toBe(true);
    expect(response.body.role).toBe("primary");

    const snapshot = response.body.snapshot;
    expect(snapshot).toBeTruthy();
    expect(snapshot?.dbPath).toBeTruthy();
    expect(snapshot?.byteLength).toBeGreaterThan(0);
    expect(snapshot?.sha256).toBeTruthy();
    expect(snapshot?.dataBase64).toBeTruthy();

    const decoded = Buffer.from(snapshot!.dataBase64!, "base64");
    expect(decoded.byteLength).toBe(snapshot!.byteLength);

    const fileBytes = await readFile(snapshot!.dbPath!);
    expect(fileBytes.byteLength).toBe(snapshot!.byteLength);
    expect(Buffer.compare(decoded, fileBytes)).toBe(0);

    const computed = createHash("sha256").update(fileBytes).digest("hex");
    expect(computed).toBe(snapshot!.sha256);
  });

  test("snapshot includeData=false omits payload but keeps valid metadata", async () => {
    await openPrimary();
    await appendCommits(2);

    const response = await requestJson<{
      success: boolean;
      role?: string;
      snapshot?: {
        dbPath?: string;
        byteLength?: number;
        sha256?: string;
        dataBase64?: string;
      };
    }>("GET", "/api/replication/snapshot/latest?includeData=false", undefined, AUTH_HEADER);

    expect(response.status).toBe(200);
    expect(response.body.success).toBe(true);
    expect(response.body.role).toBe("primary");

    const snapshot = response.body.snapshot;
    expect(snapshot).toBeTruthy();
    expect(snapshot?.dbPath).toBeTruthy();
    expect(snapshot?.byteLength).toBeGreaterThan(0);
    expect(snapshot?.sha256).toBeTruthy();
    expect(snapshot?.dataBase64).toBeUndefined();

    const fileBytes = await readFile(snapshot!.dbPath!);
    expect(fileBytes.byteLength).toBe(snapshot!.byteLength);
    const computed = createHash("sha256").update(fileBytes).digest("hex");
    expect(computed).toBe(snapshot!.sha256);
  });

  test("enforces bearer token on protected endpoints", async () => {
    await openPrimary();

    const unauthorized = await requestJson<{ success: boolean; error?: string }>(
      "GET",
      "/api/replication/log",
    );

    expect(unauthorized.status).toBe(401);
    expect(unauthorized.body.success).toBe(false);
    expect(unauthorized.body.error).toContain("Unauthorized");

    const authorized = await requestJson<{ success: boolean }>(
      "GET",
      "/api/replication/log",
      undefined,
      AUTH_HEADER,
    );
    expect(authorized.status).toBe(200);
    expect(authorized.body.success).toBe(true);
  });

  test("replication status remains readable without bearer token", async () => {
    await openPrimary();
    await appendCommits(1);

    const publicStatus = await requestJson<{
      connected: boolean;
      authEnabled: boolean;
      role: string;
      primary?: { headLogIndex?: number };
    }>("GET", "/api/replication/status");
    expect(publicStatus.status).toBe(200);
    expect(publicStatus.body.connected).toBe(true);
    expect(publicStatus.body.authEnabled).toBe(true);
    expect(publicStatus.body.role).toBe("primary");
    expect((publicStatus.body.primary?.headLogIndex ?? 0) > 0).toBe(true);

    const adminBlocked = await requestJson<{ success: boolean; error?: string }>(
      "GET",
      "/api/replication/log",
    );
    expect(adminBlocked.status).toBe(401);
    expect(adminBlocked.body.success).toBe(false);
  });

  test("replication metrics endpoint exports Prometheus text when authorized", async () => {
    await openPrimary();
    await appendCommits(3);

    const metrics = await requestText(
      "GET",
      "/api/replication/metrics",
      undefined,
      AUTH_HEADER,
    );

    expect(metrics.status).toBe(200);
    expect(metrics.body).toContain("# HELP raydb_replication_enabled");
    expect(metrics.body).toContain("# TYPE raydb_replication_enabled gauge");
    expect(metrics.body).toContain('raydb_replication_enabled{role="primary"} 1');
    expect(metrics.body).toContain("raydb_replication_primary_head_log_index");
    expect(metrics.body).toContain("raydb_replication_primary_append_attempts_total");
  });

  test("replication metrics endpoint requires bearer token", async () => {
    await openPrimary();

    const unauthorized = await requestText("GET", "/api/replication/metrics");
    expect(unauthorized.status).toBe(401);
    expect(unauthorized.body).toContain("Unauthorized");
  });

  test("supports mTLS-only admin auth mode", async () => {
    await openPrimary();
    await appendCommits(1);

    await withReplicationAuthEnv(
      {
        REPLICATION_ADMIN_AUTH_MODE: "mtls",
        REPLICATION_MTLS_HEADER: "x-client-cert",
        REPLICATION_MTLS_SUBJECT_REGEX: "^CN=allowed",
      },
      async () => {
        const noMtls = await requestJson<{ success: boolean; error?: string }>(
          "GET",
          "/api/replication/log",
        );
        expect(noMtls.status).toBe(401);
        expect(noMtls.body.success).toBe(false);

        const badSubject = await requestJson<{ success: boolean; error?: string }>(
          "GET",
          "/api/replication/log",
          undefined,
          { "x-client-cert": "CN=denied-client" },
        );
        expect(badSubject.status).toBe(401);
        expect(badSubject.body.success).toBe(false);

        const goodSubject = await requestJson<{ success: boolean }>(
          "GET",
          "/api/replication/log",
          undefined,
          { "x-client-cert": "CN=allowed-client,O=RayDB" },
        );
        expect(goodSubject.status).toBe(200);
        expect(goodSubject.body.success).toBe(true);
      },
    );
  });

  test("supports native TLS mTLS auth mode without proxy header", async () => {
    await openPrimary();
    await appendCommits(1);

    await withReplicationAuthEnv(
      {
        REPLICATION_ADMIN_AUTH_MODE: "mtls",
        REPLICATION_MTLS_NATIVE_TLS: "true",
        PLAYGROUND_TLS_REQUEST_CERT: "true",
        PLAYGROUND_TLS_REJECT_UNAUTHORIZED: "true",
        REPLICATION_MTLS_HEADER: null,
        REPLICATION_MTLS_SUBJECT_REGEX: null,
      },
      async () => {
        const httpRequest = await requestJson<{ success: boolean; error?: string }>(
          "GET",
          "/api/replication/log",
        );
        expect(httpRequest.status).toBe(401);
        expect(httpRequest.body.success).toBe(false);

        const httpsRequest = await requestJson<{ success: boolean }>(
          "GET",
          "/api/replication/log",
          undefined,
          undefined,
          "https://localhost",
        );
        expect(httpsRequest.status).toBe(200);
        expect(httpsRequest.body.success).toBe(true);
      },
    );
  });

  test("rejects invalid native TLS mTLS config", async () => {
    await openPrimary();
    await appendCommits(1);

    await withReplicationAuthEnv(
      {
        REPLICATION_ADMIN_AUTH_MODE: "mtls",
        REPLICATION_MTLS_NATIVE_TLS: "true",
        PLAYGROUND_TLS_REQUEST_CERT: "false",
        PLAYGROUND_TLS_REJECT_UNAUTHORIZED: "true",
      },
      async () => {
        const response = await requestJson<{ success: boolean; error?: string }>(
          "GET",
          "/api/replication/log",
        );
        expect(response.status).toBe(500);
        expect(response.body.success).toBe(false);
        expect(response.body.error).toContain("REPLICATION_MTLS_NATIVE_TLS requires");
      },
    );
  });

  test("supports token_and_mtls admin auth mode", async () => {
    await openPrimary();
    await appendCommits(1);

    await withReplicationAuthEnv(
      {
        REPLICATION_ADMIN_TOKEN: "combo-token",
        REPLICATION_ADMIN_AUTH_MODE: "token_and_mtls",
        REPLICATION_MTLS_HEADER: "x-client-cert",
        REPLICATION_MTLS_SUBJECT_REGEX: "^CN=combo$",
      },
      async () => {
        const tokenOnly = await requestJson<{ success: boolean; error?: string }>(
          "GET",
          "/api/replication/log",
          undefined,
          { Authorization: "Bearer combo-token" },
        );
        expect(tokenOnly.status).toBe(401);
        expect(tokenOnly.body.success).toBe(false);

        const mtlsOnly = await requestJson<{ success: boolean; error?: string }>(
          "GET",
          "/api/replication/log",
          undefined,
          { "x-client-cert": "CN=combo" },
        );
        expect(mtlsOnly.status).toBe(401);
        expect(mtlsOnly.body.success).toBe(false);

        const both = await requestJson<{ success: boolean }>(
          "GET",
          "/api/replication/log",
          undefined,
          {
            Authorization: "Bearer combo-token",
            "x-client-cert": "CN=combo",
          },
        );
        expect(both.status).toBe(200);
        expect(both.body.success).toBe(true);
      },
    );
  });

  test("rejects snapshot, pull, reseed, and promote without bearer token", async () => {
    await openPrimary();

    const snapshot = await requestJson<{ success: boolean; error?: string }>(
      "GET",
      "/api/replication/snapshot/latest",
    );
    expect(snapshot.status).toBe(401);
    expect(snapshot.body.success).toBe(false);
    expect(snapshot.body.error).toContain("Unauthorized");

    const pull = await requestJson<{ success: boolean; error?: string }>(
      "POST",
      "/api/replication/pull",
      { maxFrames: 1 },
    );
    expect(pull.status).toBe(401);
    expect(pull.body.success).toBe(false);
    expect(pull.body.error).toContain("Unauthorized");

    const reseed = await requestJson<{ success: boolean; error?: string }>(
      "POST",
      "/api/replication/reseed",
    );
    expect(reseed.status).toBe(401);
    expect(reseed.body.success).toBe(false);
    expect(reseed.body.error).toContain("Unauthorized");

    const promote = await requestJson<{ success: boolean; error?: string }>(
      "POST",
      "/api/replication/promote",
    );
    expect(promote.status).toBe(401);
    expect(promote.body.success).toBe(false);
    expect(promote.body.error).toContain("Unauthorized");
  });

  test("reseed on primary role returns structured error", async () => {
    await openPrimary();

    const reseed = await requestJson<{ success: boolean; error?: string }>(
      "POST",
      "/api/replication/reseed",
      undefined,
      AUTH_HEADER,
    );
    expect(reseed.status).toBe(200);
    expect(reseed.body.success).toBe(false);
    expect(reseed.body.error).toContain("replica role");
  });

  test("reseed is idempotent on healthy replica", async () => {
    await openPrimary();
    await appendCommits(4);

    const replicaPath = join(tempDir, "replica-reseed-idempotent.kitedb");
    const openReplica = await requestJson<{ success: boolean; error?: string }>(
      "POST",
      "/api/db/open",
      {
        path: replicaPath,
        options: {
          replicationRole: "replica",
          replicationSourceDbPath: dbPath,
        },
      },
    );
    expect(openReplica.status).toBe(200);
    expect(openReplica.body.success).toBe(true);

    const first = await requestJson<{
      success: boolean;
      role: string;
      replica?: { needsReseed?: boolean; lastError?: string | null; appliedLogIndex?: number };
    }>("POST", "/api/replication/reseed", undefined, AUTH_HEADER);
    expect(first.status).toBe(200);
    expect(first.body.success).toBe(true);
    expect(first.body.role).toBe("replica");
    expect(first.body.replica?.needsReseed).toBe(false);
    expect(first.body.replica?.lastError ?? null).toBeNull();
    expect((first.body.replica?.appliedLogIndex ?? 0) > 0).toBe(true);

    const second = await requestJson<{
      success: boolean;
      role: string;
      replica?: { needsReseed?: boolean; lastError?: string | null; appliedLogIndex?: number };
    }>("POST", "/api/replication/reseed", undefined, AUTH_HEADER);
    expect(second.status).toBe(200);
    expect(second.body.success).toBe(true);
    expect(second.body.role).toBe("replica");
    expect(second.body.replica?.needsReseed).toBe(false);
    expect(second.body.replica?.lastError ?? null).toBeNull();
    expect(second.body.replica?.appliedLogIndex).toBe(first.body.replica?.appliedLogIndex);
  });

  test("reseed baseline allows later incremental pull after new primary commits", async () => {
    await openPrimary();
    await appendCommits(4);

    const replicaPath = join(tempDir, "replica-reseed-continuity.kitedb");
    const openReplica = await requestJson<{ success: boolean; error?: string }>(
      "POST",
      "/api/db/open",
      {
        path: replicaPath,
        options: {
          replicationRole: "replica",
          replicationSourceDbPath: dbPath,
        },
      },
    );
    expect(openReplica.status).toBe(200);
    expect(openReplica.body.success).toBe(true);

    const reseed = await requestJson<{
      success: boolean;
      role: string;
      replica?: { needsReseed?: boolean; lastError?: string | null; appliedLogIndex?: number };
    }>("POST", "/api/replication/reseed", undefined, AUTH_HEADER);
    expect(reseed.status).toBe(200);
    expect(reseed.body.success).toBe(true);
    expect(reseed.body.role).toBe("replica");
    expect(reseed.body.replica?.needsReseed).toBe(false);
    expect(reseed.body.replica?.lastError ?? null).toBeNull();
    const baselineApplied = reseed.body.replica?.appliedLogIndex ?? 0;
    expect(baselineApplied > 0).toBe(true);

    const reopenPrimary = await requestJson<{ success: boolean; error?: string }>(
      "POST",
      "/api/db/open",
      {
        path: dbPath,
        options: {
          replicationRole: "primary",
        },
      },
    );
    expect(reopenPrimary.status).toBe(200);
    expect(reopenPrimary.body.success).toBe(true);
    await appendCommits(3);

    const reopenReplica = await requestJson<{ success: boolean; error?: string }>(
      "POST",
      "/api/db/open",
      {
        path: replicaPath,
        options: {
          replicationRole: "replica",
          replicationSourceDbPath: dbPath,
        },
      },
    );
    expect(reopenReplica.status).toBe(200);
    expect(reopenReplica.body.success).toBe(true);

    const beforePull = await requestJson<{
      connected: boolean;
      role: string;
      replica?: { appliedLogIndex?: number; needsReseed?: boolean };
    }>("GET", "/api/replication/status");
    expect(beforePull.status).toBe(200);
    expect(beforePull.body.role).toBe("replica");
    expect(beforePull.body.replica?.needsReseed).toBe(false);
    expect(beforePull.body.replica?.appliedLogIndex).toBe(baselineApplied);

    const pull = await requestJson<{
      success: boolean;
      appliedFrames?: number;
      replica?: { appliedLogIndex?: number; needsReseed?: boolean };
    }>("POST", "/api/replication/pull", { maxFrames: 128 }, AUTH_HEADER);
    expect(pull.status).toBe(200);
    expect(pull.body.success).toBe(true);
    expect((pull.body.appliedFrames ?? 0) > 0).toBe(true);
    expect(pull.body.replica?.needsReseed).toBe(false);
    expect((pull.body.replica?.appliedLogIndex ?? 0) > baselineApplied).toBe(true);
  });

  test("replica pull advances appliedLogIndex after primary commits", async () => {
    await openPrimary();
    await appendCommits(4);

    const replicaPath = join(tempDir, "replica.kitedb");
    const openReplica = await requestJson<{ success: boolean; error?: string }>(
      "POST",
      "/api/db/open",
      {
        path: replicaPath,
        options: {
          replicationRole: "replica",
          replicationSourceDbPath: dbPath,
        },
      },
    );
    expect(openReplica.status).toBe(200);
    expect(openReplica.body.success).toBe(true);

    const before = await requestJson<{
      connected: boolean;
      role: string;
      replica?: { appliedLogIndex?: number };
    }>("GET", "/api/replication/status");
    expect(before.status).toBe(200);
    expect(before.body.connected).toBe(true);
    expect(before.body.role).toBe("replica");
    const beforeIndex = before.body.replica?.appliedLogIndex ?? 0;

    const pull = await requestJson<{
      success: boolean;
      appliedFrames?: number;
      replica?: { appliedLogIndex?: number };
    }>("POST", "/api/replication/pull", { maxFrames: 64 }, AUTH_HEADER);
    expect(pull.status).toBe(200);
    expect(pull.body.success).toBe(true);
    expect((pull.body.appliedFrames ?? 0) > 0).toBe(true);

    const after = await requestJson<{
      connected: boolean;
      role: string;
      replica?: { appliedLogIndex?: number };
    }>("GET", "/api/replication/status");
    expect(after.status).toBe(200);
    expect(after.body.connected).toBe(true);
    expect(after.body.role).toBe("replica");
    const afterIndex = after.body.replica?.appliedLogIndex ?? 0;
    expect(afterIndex > beforeIndex).toBe(true);
  });

  test("promote increments epoch and replica catches up from promoted primary", async () => {
    await openPrimary();
    await appendCommits(2);

    const promote = await requestJson<{
      success: boolean;
      epoch?: number;
      role?: string;
      primary?: { epoch?: number };
    }>("POST", "/api/replication/promote", undefined, AUTH_HEADER);
    expect(promote.status).toBe(200);
    expect(promote.body.success).toBe(true);
    expect(promote.body.role).toBe("primary");
    expect(promote.body.epoch).toBe(2);
    expect(promote.body.primary?.epoch).toBe(2);

    await appendCommits(3);

    const replicaPath = join(tempDir, "replica-promoted.kitedb");
    const openReplica = await requestJson<{ success: boolean }>("POST", "/api/db/open", {
      path: replicaPath,
      options: {
        replicationRole: "replica",
        replicationSourceDbPath: dbPath,
      },
    });
    expect(openReplica.status).toBe(200);
    expect(openReplica.body.success).toBe(true);

    const pull = await requestJson<{
      success: boolean;
      appliedFrames?: number;
      replica?: { appliedEpoch?: number; appliedLogIndex?: number };
    }>("POST", "/api/replication/pull", { maxFrames: 128 }, AUTH_HEADER);
    expect(pull.status).toBe(200);
    expect(pull.body.success).toBe(true);
    expect((pull.body.appliedFrames ?? 0) > 0).toBe(true);
    expect((pull.body.replica?.appliedEpoch ?? 0) >= 2).toBe(true);
    expect((pull.body.replica?.appliedLogIndex ?? 0) > 0).toBe(true);
  });

  test("reseed clears needsReseed after missing-segment failure", async () => {
    await closeDatabase();
    await useTempDataDir();
    dbPath = join(tempDir, "primary-needs-reseed.kitedb");
    const openPrimaryWithSmallSegments = await requestJson<{ success: boolean }>(
      "POST",
      "/api/db/open",
      {
        path: dbPath,
        options: {
          replicationRole: "primary",
          replicationSegmentMaxBytes: 1,
        },
      },
    );
    expect(openPrimaryWithSmallSegments.status).toBe(200);
    expect(openPrimaryWithSmallSegments.body.success).toBe(true);

    await appendCommits(6);

    const primaryStatus = await requestJson<{
      connected: boolean;
      role: string;
      primary?: { sidecarPath?: string; headLogIndex?: number };
    }>("GET", "/api/replication/status");
    expect(primaryStatus.status).toBe(200);
    expect(primaryStatus.body.connected).toBe(true);
    expect(primaryStatus.body.role).toBe("primary");
    const sidecarPath = primaryStatus.body.primary?.sidecarPath;
    const headLogIndex = primaryStatus.body.primary?.headLogIndex ?? 0;
    expect(sidecarPath).toBeTruthy();
    expect(headLogIndex > 0).toBe(true);

    const replicaPath = join(tempDir, "replica-needs-reseed.kitedb");
    const openReplica = await requestJson<{ success: boolean }>("POST", "/api/db/open", {
      path: replicaPath,
      options: {
        replicationRole: "replica",
        replicationSourceDbPath: dbPath,
      },
    });
    expect(openReplica.status).toBe(200);
    expect(openReplica.body.success).toBe(true);

    const initialPull = await requestJson<{ success: boolean; appliedFrames?: number }>(
      "POST",
      "/api/replication/pull",
      { maxFrames: 1 },
      AUTH_HEADER,
    );
    expect(initialPull.status).toBe(200);
    expect(initialPull.body.success).toBe(true);
    expect((initialPull.body.appliedFrames ?? 0) > 0).toBe(true);

    const replicaStatusBefore = await requestJson<{
      connected: boolean;
      role: string;
      replica?: { appliedLogIndex?: number };
    }>("GET", "/api/replication/status");
    expect(replicaStatusBefore.status).toBe(200);
    expect(replicaStatusBefore.body.role).toBe("replica");
    const appliedIndex = replicaStatusBefore.body.replica?.appliedLogIndex ?? 0;
    expect(headLogIndex > appliedIndex).toBe(true);

    const manifestPath = join(sidecarPath!, "manifest.json");
    const envelope = JSON.parse(
      await readFile(manifestPath, "utf8"),
    ) as ManifestEnvelope;

    const expectedNext = appliedIndex + 1;
    const gapSegment = envelope.manifest.segments.find(
      (segment) =>
        segment.start_log_index <= expectedNext &&
        segment.end_log_index >= expectedNext,
    );
    expect(gapSegment).toBeTruthy();
    const segmentPath = join(
      sidecarPath!,
      `segment-${String(gapSegment!.id).padStart(20, "0")}.rlog`,
    );
    await rm(segmentPath, { force: true });

    // A missing segment is first retried as transient; each pull spends part of the retry
    // budget, and the replica escalates to needs-reseed once the budget is exhausted.
    let pullAfterTamper: JsonResponse<{ success: boolean; error?: string }> | null = null;
    for (let attempt = 0; attempt < 10; attempt++) {
      pullAfterTamper = await requestJson<{ success: boolean; error?: string }>(
        "POST",
        "/api/replication/pull",
        { maxFrames: 64 },
        AUTH_HEADER,
      );
      expect(pullAfterTamper.status).toBe(200);
      expect(pullAfterTamper.body.success).toBe(false);
      if (pullAfterTamper.body.error?.includes("needs reseed")) {
        break;
      }
    }
    expect(pullAfterTamper?.body.error).toContain("needs reseed");

    const replicaStatusAfter = await requestJson<{
      connected: boolean;
      role: string;
      replica?: { needsReseed?: boolean; lastError?: string };
    }>("GET", "/api/replication/status");
    expect(replicaStatusAfter.status).toBe(200);
    expect(replicaStatusAfter.body.role).toBe("replica");
    expect(replicaStatusAfter.body.replica?.needsReseed).toBe(true);
    expect(replicaStatusAfter.body.replica?.lastError).toContain("needs reseed");

    const reseed = await requestJson<{
      success: boolean;
      role: string;
      replica?: { needsReseed?: boolean; lastError?: string | null };
    }>("POST", "/api/replication/reseed", undefined, AUTH_HEADER);
    expect(reseed.status).toBe(200);
    expect(reseed.body.success).toBe(true);
    expect(reseed.body.role).toBe("replica");
    expect(reseed.body.replica?.needsReseed).toBe(false);
    expect(reseed.body.replica?.lastError ?? null).toBeNull();

    const replicaStatusAfterReseed = await requestJson<{
      connected: boolean;
      role: string;
      replica?: { needsReseed?: boolean; lastError?: string | null };
    }>("GET", "/api/replication/status");
    expect(replicaStatusAfterReseed.status).toBe(200);
    expect(replicaStatusAfterReseed.body.role).toBe("replica");
    expect(replicaStatusAfterReseed.body.replica?.needsReseed).toBe(false);
    expect(replicaStatusAfterReseed.body.replica?.lastError ?? null).toBeNull();

    const pullAfterReseed = await requestJson<{ success: boolean; appliedFrames?: number }>(
      "POST",
      "/api/replication/pull",
      { maxFrames: 64 },
      AUTH_HEADER,
    );
    expect(pullAfterReseed.status).toBe(200);
    expect(pullAfterReseed.body.success).toBe(true);
  }, 30_000);
});
