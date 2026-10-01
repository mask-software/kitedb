/**
 * Regression tests for the playground security / correctness audit (G1-G3).
 *
 * Env contract these tests rely on (read per request, not at module load):
 * - PLAYGROUND_DATA_DIR: the only directory /api/db/open may touch.
 * - REPLICATION_ADMIN_TOKEN unset: replication admin routes are denied.
 */

import { afterAll, afterEach, beforeAll, describe, expect, test } from "bun:test";
import { existsSync, mkdtempSync, readdirSync } from "node:fs";
import { mkdir, mkdtemp, readFile, rm } from "node:fs/promises";
import { networkInterfaces, tmpdir } from "node:os";
import { basename, join, resolve, sep } from "node:path";

const DATA_ROOT = mkdtempSync(join(tmpdir(), "kitedb-audit-data-"));
const PREVIOUS_DATA_DIR = process.env.PLAYGROUND_DATA_DIR;
process.env.PLAYGROUND_DATA_DIR = DATA_ROOT;

const { Elysia } = await import("elysia");
const { apiRoutes } = await import("./routes.ts");
const { closeDatabase, getDb, getDbPath, openDatabase, nodes, edges } = await import("./db.ts");
const { kite } = await import("../../../ray-rs/ts/index.ts");

const PLAYGROUND_DIR = resolve(import.meta.dir, "../..");
const SERVER_ENTRY = join(PLAYGROUND_DIR, "src/server.ts");

let api: InstanceType<typeof Elysia>;
const scratchDirs: string[] = [];

interface ApiResponse<T = Record<string, unknown>> {
  status: number;
  text: string;
  body: T;
  headers: Headers;
}

async function send<T = Record<string, unknown>>(
  handler: { handle: (request: Request) => Promise<Response> },
  method: string,
  path: string,
  init: { json?: unknown; form?: FormData; headers?: Record<string, string> } = {},
): Promise<ApiResponse<T>> {
  const headers: Record<string, string> = { ...(init.headers ?? {}) };
  let body: BodyInit | undefined;
  if (init.json !== undefined) {
    headers["content-type"] = "application/json";
    body = JSON.stringify(init.json);
  } else if (init.form) {
    body = init.form;
  }
  const response = await handler.handle(
    new Request(`http://localhost${path}`, { method, headers, body }),
  );
  const text = await response.text();
  let parsed: unknown = null;
  try {
    parsed = JSON.parse(text);
  } catch {
    parsed = null;
  }
  return { status: response.status, text, body: parsed as T, headers: response.headers };
}

function describeResponse(response: ApiResponse): string {
  return `${response.status} ${response.text.slice(0, 300)}`;
}

/** "denied" when the route refused the request, otherwise a description of what leaked. */
function deniedOrLeak(response: ApiResponse<{ success?: boolean }>): string {
  if (response.status >= 400 || response.body?.success === false) {
    return "denied";
  }
  return `allowed: ${describeResponse(response)}`;
}

async function makeScratch(prefix: string): Promise<string> {
  const dir = await mkdtemp(join(tmpdir(), prefix));
  scratchDirs.push(dir);
  return dir;
}

async function withEnv<T>(
  overrides: Record<string, string | null>,
  run: () => Promise<T>,
): Promise<T> {
  const previous: Record<string, string | undefined> = {};
  for (const [key, value] of Object.entries(overrides)) {
    previous[key] = process.env[key];
    if (value === null) {
      delete process.env[key];
    } else {
      process.env[key] = value;
    }
  }
  try {
    return await run();
  } finally {
    for (const [key, value] of Object.entries(previous)) {
      if (value === undefined) {
        delete process.env[key];
      } else {
        process.env[key] = value;
      }
    }
  }
}

const NO_REPLICATION_AUTH = {
  REPLICATION_ADMIN_TOKEN: null,
  REPLICATION_ADMIN_AUTH_MODE: null,
  REPLICATION_MTLS_HEADER: null,
  REPLICATION_MTLS_SUBJECT_REGEX: null,
  REPLICATION_MTLS_NATIVE_TLS: null,
};

function findFilesNamed(root: string, name: string): string[] {
  const found: string[] = [];
  const walk = (dir: string) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const full = join(dir, entry.name);
      if (entry.isDirectory()) {
        walk(full);
      } else if (entry.name === name) {
        found.push(full);
      }
    }
  };
  walk(root);
  return found;
}

function isInside(child: string, parent: string): boolean {
  const resolvedParent = resolve(parent);
  const resolvedChild = resolve(child);
  return resolvedChild === resolvedParent || resolvedChild.startsWith(resolvedParent + sep);
}

beforeAll(() => {
  api = new Elysia().use(apiRoutes);
});

afterEach(async () => {
  await closeDatabase();
  while (scratchDirs.length > 0) {
    await rm(scratchDirs.pop()!, { recursive: true, force: true });
  }
});

afterAll(async () => {
  await closeDatabase();
  await rm(DATA_ROOT, { recursive: true, force: true });
  if (PREVIOUS_DATA_DIR === undefined) {
    delete process.env.PLAYGROUND_DATA_DIR;
  } else {
    process.env.PLAYGROUND_DATA_DIR = PREVIOUS_DATA_DIR;
  }
});

// ============================================================================
// G1: upload path traversal
// ============================================================================

describe("G1 upload path traversal", () => {
  // Uploads land in mkdtemp(tmpdir()/kitedb-playground-*). Point TMPDIR two levels deep into a
  // scratch dir so "../../<name>" resolves inside the scratch dir (writable, auto-cleaned).
  async function uploadScratch(): Promise<{ root: string; tmp: string }> {
    const root = await makeScratch("kitedb-audit-upload-");
    const tmp = join(root, "a", "b");
    await mkdir(tmp, { recursive: true });
    return { root, tmp };
  }

  async function validDbBytes(dir: string): Promise<Uint8Array> {
    const fixturePath = join(dir, "fixture.kitedb");
    const db = await kite(fixturePath, { nodes, edges });
    await db.insert("file").values({ key: "src/a.ts", path: "src/a.ts", language: "ts" }).returning();
    await db.close();
    return new Uint8Array(await readFile(fixturePath));
  }

  function uploadForm(bytes: Uint8Array, filename: string): FormData {
    const form = new FormData();
    form.append("file", new File([bytes], filename));
    return form;
  }

  test("G1: upload with ../../ filename does not write outside the upload temp dir", async () => {
    const { root, tmp } = await uploadScratch();
    const bytes = await validDbBytes(root);
    const name = `g1-traversal-${crypto.randomUUID()}.kitedb`;
    const traversalTarget = join(root, "a", name);

    const response = await withEnv({ TMPDIR: tmp }, () =>
      send<{ success: boolean; error?: string }>(api, "POST", "/api/db/upload", {
        form: uploadForm(bytes, `../../${name}`),
      }),
    );

    expect(existsSync(traversalTarget)).toBe(false);
    const escaped = findFilesNamed(root, name).filter(
      (path) => !path.split(sep).some((part) => part.startsWith("kitedb-playground-")),
    );
    expect(escaped).toEqual([]);
    if (response.body?.success) {
      const openedPath = getDbPath();
      expect(openedPath).not.toBeNull();
      expect(isInside(openedPath!, tmp)).toBe(true);
    }
  });

  test("G1: failed upload with ../../ filename leaves no file behind", async () => {
    const { root, tmp } = await uploadScratch();
    const name = `g1-garbage-${crypto.randomUUID()}.kitedb`;
    const traversalTarget = join(root, "a", name);

    const response = await withEnv({ TMPDIR: tmp }, () =>
      send<{ success: boolean; error?: string }>(api, "POST", "/api/db/upload", {
        form: uploadForm(new TextEncoder().encode("definitely not a kitedb file"), `../../${name}`),
      }),
    );

    expect(response.body?.success).toBe(false);
    expect(existsSync(traversalTarget)).toBe(false);
  });

  test("G1: failed upload removes its temp dir", async () => {
    const { tmp } = await uploadScratch();

    const response = await withEnv({ TMPDIR: tmp }, () =>
      send<{ success: boolean; error?: string }>(api, "POST", "/api/db/upload", {
        form: uploadForm(new TextEncoder().encode("definitely not a kitedb file"), "bad.kitedb"),
      }),
    );

    expect(response.body?.success).toBe(false);
    expect(readdirSync(tmp)).toEqual([]);
  });
});

// ============================================================================
// G2: network exposure / unauthenticated admin surface
// ============================================================================

function externalIPv4(): string | null {
  for (const addresses of Object.values(networkInterfaces())) {
    for (const address of addresses ?? []) {
      if (address.family === "IPv4" && !address.internal) {
        return address.address;
      }
    }
  }
  return null;
}

async function waitForListenPort(
  stream: ReadableStream<Uint8Array>,
  timeoutMs: number,
): Promise<{ port: number | null; output: string }> {
  const reader = stream.getReader();
  const decoder = new TextDecoder();
  let output = "";
  const deadline = Date.now() + timeoutMs;
  try {
    while (Date.now() < deadline) {
      const chunk = await Promise.race([
        reader.read(),
        Bun.sleep(Math.max(deadline - Date.now(), 0)).then(() => null),
      ]);
      if (!chunk || chunk.done) {
        break;
      }
      output += decoder.decode(chunk.value, { stream: true });
      const match = output.match(/https?:\/\/[^\s/]+:(\d+)/);
      if (match) {
        return { port: Number(match[1]), output };
      }
    }
  } finally {
    reader.releaseLock();
  }
  return { port: null, output };
}

async function reachable(url: string): Promise<boolean> {
  try {
    await fetch(url, { signal: AbortSignal.timeout(2000) });
    return true;
  } catch {
    return false;
  }
}

describe("G2 network exposure", () => {
  const externalAddress = externalIPv4();

  test.skipIf(externalAddress === null)(
    "G2: server binds to loopback by default (not reachable on a non-loopback interface)",
    async () => {
      const env: Record<string, string | undefined> = { ...process.env, PORT: "0" };
      for (const key of ["NODE_ENV", "HOST", "HOSTNAME", "PLAYGROUND_HOST", "PLAYGROUND_HOSTNAME"]) {
        delete env[key];
      }
      const child = Bun.spawn([process.execPath, "run", SERVER_ENTRY], {
        cwd: PLAYGROUND_DIR,
        env,
        stdout: "pipe",
        stderr: "pipe",
      });
      try {
        const { port, output } = await waitForListenPort(child.stdout, 20_000);
        if (port === null) {
          child.kill();
          const stderr = await new Response(child.stderr).text();
          expect({ port, output, stderr }).toMatchObject({ port: expect.any(Number) });
        }

        expect(await reachable(`http://127.0.0.1:${port}/api/status`)).toBe(true);
        const external = `http://${externalAddress}:${port}/api/status`;
        expect(
          (await reachable(external)) ? `reachable via ${external}` : "loopback only",
        ).toBe("loopback only");
      } finally {
        child.kill();
        await child.exited;
      }
    },
    30_000,
  );

  test("G2: cross-origin requests do not get Access-Control-Allow-Origin reflected", async () => {
    const { app: server } = await import("../server.ts");
    const evil = "https://evil.example";

    const simple = await send(server, "GET", "/api/status", { headers: { Origin: evil } });
    const preflight = await send(server, "OPTIONS", "/api/db/open", {
      headers: {
        Origin: evil,
        "Access-Control-Request-Method": "POST",
        "Access-Control-Request-Headers": "content-type",
      },
    });

    for (const response of [simple, preflight]) {
      const allowOrigin = response.headers.get("access-control-allow-origin");
      expect(
        allowOrigin === evil || allowOrigin === "*"
          ? `Access-Control-Allow-Origin: ${allowOrigin}`
          : "not reflected",
      ).toBe("not reflected");
    }
  });

  test("G2: /db/open rejects an absolute path outside PLAYGROUND_DATA_DIR", async () => {
    const outside = await makeScratch("kitedb-audit-outside-");
    const target = join(outside, "abs.kitedb");

    const response = await send<{ success?: boolean }>(api, "POST", "/api/db/open", {
      json: { path: target },
    });
    await closeDatabase();

    expect(deniedOrLeak(response)).toBe("denied");
    expect(existsSync(target)).toBe(false);

    const inside = await send<{ success?: boolean }>(api, "POST", "/api/db/open", {
      json: { path: join(DATA_ROOT, "inside.kitedb") },
    });
    expect({ status: inside.status, body: inside.body }).toMatchObject({
      status: 200,
      body: { success: true },
    });
  });

  test("G2: /db/open rejects a ../ traversal out of PLAYGROUND_DATA_DIR", async () => {
    const outside = await makeScratch("kitedb-audit-outside-");
    const traversal = `${DATA_ROOT}${sep}..${sep}${basename(outside)}${sep}traversal.kitedb`;

    const response = await send<{ success?: boolean }>(api, "POST", "/api/db/open", {
      json: { path: traversal },
    });
    await closeDatabase();

    expect(deniedOrLeak(response)).toBe("denied");
    expect(existsSync(join(outside, "traversal.kitedb"))).toBe(false);
  });

  test("G2: /db/open rejects a replicationSidecarPath outside PLAYGROUND_DATA_DIR", async () => {
    const outside = await makeScratch("kitedb-audit-outside-");
    const sidecar = join(outside, "sidecar");

    const response = await send<{ success?: boolean }>(api, "POST", "/api/db/open", {
      json: {
        path: join(DATA_ROOT, "sidecar-probe.kitedb"),
        options: { replicationRole: "primary", replicationSidecarPath: sidecar },
      },
    });
    await closeDatabase();

    expect(deniedOrLeak(response)).toBe("denied");
    expect(existsSync(sidecar)).toBe(false);
  });

  async function openPrimaryInDataDir(name: string): Promise<void> {
    const opened = await openDatabase(join(DATA_ROOT, name), { replicationRole: "primary" });
    expect(opened).toEqual({ success: true });
  }

  test("G2: snapshot/latest?includeData=true is denied when REPLICATION_ADMIN_TOKEN is unset", async () => {
    await openPrimaryInDataDir("admin-snapshot.kitedb");

    const response = await withEnv(NO_REPLICATION_AUTH, () =>
      send<{ success?: boolean; snapshot?: { dataBase64?: string } }>(
        api,
        "GET",
        "/api/replication/snapshot/latest?includeData=true",
      ),
    );

    expect(deniedOrLeak(response)).toBe("denied");
    expect(response.body?.snapshot?.dataBase64).toBeUndefined();
  });

  test("G2: /replication/promote is denied when REPLICATION_ADMIN_TOKEN is unset", async () => {
    await openPrimaryInDataDir("admin-promote.kitedb");
    const epochBefore = getDb()!.primaryReplicationStatus()?.epoch;

    const response = await withEnv(NO_REPLICATION_AUTH, () =>
      send<{ success?: boolean; epoch?: number }>(api, "POST", "/api/replication/promote"),
    );

    expect(deniedOrLeak(response)).toBe("denied");
    expect(getDb()!.primaryReplicationStatus()?.epoch).toBe(epochBefore);
  });
});

// ============================================================================
// G3: graph view / path / impact on the NAPI engine
// ============================================================================

describe("G3 graph endpoints on the demo database", () => {
  async function openDemo(): Promise<{ nodeCount: number; edgeCount: number }> {
    const demo = await send<{ success: boolean }>(api, "POST", "/api/db/demo");
    expect(demo.body).toEqual({ success: true });
    const status = await send<{ nodeCount: number; edgeCount: number }>(api, "GET", "/api/status");
    expect(status.body.nodeCount).toBeGreaterThan(0);
    expect(status.body.edgeCount).toBeGreaterThan(0);
    return status.body;
  }

  test("G3: GET /graph/network returns every demo node and edge", async () => {
    const { nodeCount, edgeCount } = await openDemo();

    const response = await send<{
      nodes: Array<{ id: string; label: string; type: string; degree: number }>;
      edges: Array<{ source: string; target: string; type: string }>;
      truncated: boolean;
    }>(api, "GET", "/api/graph/network");

    expect({ status: response.status, text: response.text.slice(0, 300) }).toMatchObject({
      status: 200,
    });
    const { nodes: visNodes, edges: visEdges, truncated } = response.body;
    expect(truncated).toBe(false);
    expect(visNodes.length).toBe(nodeCount);
    expect(visEdges.length).toBe(edgeCount);

    const ids = new Set(visNodes.map((node) => node.id));
    expect(ids.size).toBe(visNodes.length);
    expect(ids.has("file:src/index.ts")).toBe(true);
    expect(ids.has("fn:main")).toBe(true);
    expect(ids.has("class:UserHandler")).toBe(true);
    expect(ids.has("module:db")).toBe(true);
    for (const node of visNodes) {
      expect(["file", "function", "class", "module"]).toContain(node.type);
      expect(node.label.length).toBeGreaterThan(0);
    }
    for (const edge of visEdges) {
      expect(ids.has(edge.source)).toBe(true);
      expect(ids.has(edge.target)).toBe(true);
      expect(["imports", "calls", "contains", "extends"]).toContain(edge.type);
    }
    expect(visEdges).toContainEqual({ source: "fn:query", target: "fn:connect", type: "calls" });
    const degreeSum = visNodes.reduce((sum, node) => sum + node.degree, 0);
    expect(degreeSum).toBe(visEdges.length * 2);
  });

  test("G3: POST /graph/path returns the shortest outgoing path", async () => {
    await openDemo();

    const response = await send<{ path: string[]; edges: string[]; error?: string }>(
      api,
      "POST",
      "/api/graph/path",
      { json: { startKey: "fn:main", endKey: "fn:connect" } },
    );

    expect({ status: response.status, text: response.text.slice(0, 300) }).toMatchObject({
      status: 200,
    });
    expect(response.body.error).toBeUndefined();
    expect(response.body.path).toEqual([
      "fn:main",
      "fn:startServer",
      "fn:handleRequest",
      "fn:validateToken",
      "fn:query",
      "fn:connect",
    ]);
    expect(response.body.edges).toEqual(["calls", "calls", "calls", "calls", "calls"]);
  });

  test("G3: POST /graph/impact returns transitive dependents", async () => {
    await openDemo();

    const response = await send<{ impacted: string[]; edges: string[]; error?: string }>(
      api,
      "POST",
      "/api/graph/impact",
      { json: { nodeKey: "fn:query" } },
    );

    expect({ status: response.status, text: response.text.slice(0, 300) }).toMatchObject({
      status: 200,
    });
    expect(response.body.error).toBeUndefined();
    const impacted = new Set(response.body.impacted);
    for (const dependent of [
      "fn:validateToken",
      "fn:findUser",
      "fn:saveUser",
      "fn:handleRequest",
      "fn:main",
      "file:src/db/queries.ts",
    ]) {
      expect(impacted.has(dependent)).toBe(true);
    }
    for (const unaffected of ["fn:query", "fn:connect", "fn:loadConfig", "fn:log"]) {
      expect(impacted.has(unaffected)).toBe(false);
    }
    expect(response.body.edges).toEqual(expect.arrayContaining(["calls", "contains"]));
  });
});
