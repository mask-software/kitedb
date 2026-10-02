/**
 * Wave-3 reproductions for the TUI (finding C11, plus C11-keys and C11-open found while writing them).
 *
 * C11-keys: the keyboard handler reads `event.key`, but opentui's KeyEvent has `name`/`sequence`
 *           (no `key`), so no shortcut works.
 * C11-open: DbService calls Database#nodeTypes/#edgeTypes, which only exist on Kite, so opening
 *           any database fails with "this.db.nodeTypes is not a function".
 * C11a:     q/Esc call process.exit(0) without closing the open database.
 * C11b:     DbService.nodeKeyCache stores null for missing nodes and is not cleared by importJson,
 *           so nodes created by an import keep showing no key.
 * C11-edges: getNodeDetail reads `src`/`dst` from getOutEdges/getInEdges, which return
 *           `{ etype, nodeId }`, so every edge in the node details shows undefined endpoints.
 * C11-build: scripts/build.ts imports a named `solidPlugin` that @opentui/solid/bun-plugin doesn't
 *           export (it has a default export), so `bun run build` fails.
 *
 * Each test isolates its finding from the others: the C11-open and C11a tests add a `key` field to
 * each key event (withLegacyKeyField), and the C11a tests stub the missing type-list methods
 * (withTypeListStubs). Both helpers do nothing once the app no longer needs them.
 */

import { afterEach, describe, expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { Database } from "@kitedb/core";
import { testRender } from "@opentui/solid";
import { App } from "../src/app.tsx";
import { DbService } from "../src/db/db-service.ts";

type TestSetup = Awaited<ReturnType<typeof testRender>>;

const cleanups: Array<() => void | Promise<void>> = [];

afterEach(async () => {
  while (cleanups.length > 0) {
    const cleanup = cleanups.pop()!;
    await cleanup();
  }
});

async function scratchDir(): Promise<string> {
  const dir = await mkdtemp(join(tmpdir(), "kitedb-tui-w3-"));
  cleanups.push(() => rm(dir, { recursive: true, force: true }));
  return dir;
}

/** Create a database with the given node keys, and return its path. */
function createDb(path: string, keys: string[]): void {
  const db = Database.open(path);
  db.begin();
  for (const key of keys) {
    db.createNode(key);
  }
  db.commit();
  db.close();
}

async function renderApp(): Promise<TestSetup> {
  const setup = await testRender(App, { width: 140, height: 45 });
  cleanups.push(() => setup.renderer.destroy());
  await setup.renderOnce();
  return setup;
}

/** Give the stdin parser time to flush (a lone ESC waits for a possible escape sequence). */
async function settle(setup: TestSetup): Promise<void> {
  await Bun.sleep(40);
  await setup.renderOnce();
}

/**
 * Add the `key` field that src/app.tsx reads, derived from opentui's `sequence`/`name`.
 * It only fills `key` when it's missing, so it does nothing once the app reads opentui's fields.
 */
function withLegacyKeyField(setup: TestSetup): void {
  const names: Record<string, string> = {
    return: "Enter",
    enter: "Enter",
    escape: "Escape",
    backspace: "Backspace",
    tab: "Tab",
  };
  setup.renderer.keyInput.prependListener("keypress", (event: any) => {
    if (event.key !== undefined) return;
    const sequence: string = event.sequence ?? "";
    const printable = sequence.length === 1 && sequence >= " " && sequence !== "\x7f";
    event.key = printable ? sequence : (names[event.name] ?? event.name);
  });
}

/**
 * Give Database the nodeTypes/edgeTypes methods DbService calls, if it lacks them, so a database
 * can be opened in the app. Removed after the test.
 */
function withTypeListStubs(): void {
  const proto = Database.prototype as unknown as Record<string, unknown>;
  for (const name of ["nodeTypes", "edgeTypes"]) {
    if (name in proto) continue;
    proto[name] = () => [];
    cleanups.push(() => {
      delete proto[name];
    });
  }
}

async function openViaKeys(setup: TestSetup, path: string): Promise<void> {
  setup.mockInput.pressKey("o");
  await settle(setup);
  await setup.mockInput.typeText(path);
  await settle(setup);
  setup.mockInput.pressEnter();
  await settle(setup);
}

/** Record Database#close calls and process.exit calls (without exiting). */
function spyCloseAndExit(): { closeCalls: () => number; exits: Array<{ code: unknown; closedBefore: number }> } {
  const proto = Database.prototype as unknown as { close: () => void };
  const ownClose = Object.getOwnPropertyDescriptor(proto, "close");
  const originalClose = proto.close;
  let closeCalls = 0;
  proto.close = function (this: unknown) {
    closeCalls++;
    return originalClose.call(this);
  };

  const originalExit = process.exit;
  const exits: Array<{ code: unknown; closedBefore: number }> = [];
  process.exit = ((code?: unknown) => {
    exits.push({ code, closedBefore: closeCalls });
  }) as typeof process.exit;

  cleanups.push(() => {
    process.exit = originalExit;
    if (ownClose) {
      Object.defineProperty(proto, "close", ownClose);
    } else {
      delete (proto as { close?: unknown }).close;
    }
  });

  return { closeCalls: () => closeCalls, exits };
}

describe("C11-keys: keyboard shortcuts", () => {
  test("C11-keys: pressing 'o' opens the path input", async () => {
    const setup = await renderApp();
    expect(setup.captureCharFrame()).toContain("Ready");

    setup.mockInput.pressKey("o");
    await settle(setup);

    expect(setup.captureCharFrame()).toContain("Open path input");
  });
});

describe("C11-open: opening a database", () => {
  test("C11-open: opening a database from the path input shows it and its nodes", async () => {
    const dir = await scratchDir();
    const dbPath = join(dir, "open.kitedb");
    createDb(dbPath, ["user:alice", "user:bob"]);

    const setup = await renderApp();
    withLegacyKeyField(setup);
    await openViaKeys(setup, dbPath);

    const frame = setup.captureCharFrame();
    expect(frame).not.toContain("is not a function");
    expect(frame).toContain("Opened read-only");
    expect(frame).toContain("user:alice");
    expect(frame).toContain("user:bob");
  });
});

describe("C11a: quitting closes the database", () => {
  for (const [label, press] of [
    ["q", (setup: TestSetup) => setup.mockInput.pressKey("q")],
    ["Escape", (setup: TestSetup) => setup.mockInput.pressEscape()],
  ] as const) {
    test(`C11a: ${label} closes the open database before exiting`, async () => {
      const dir = await scratchDir();
      const dbPath = join(dir, "quit.kitedb");
      createDb(dbPath, ["user:alice"]);

      withTypeListStubs();
      const setup = await renderApp();
      withLegacyKeyField(setup);
      await openViaKeys(setup, dbPath);
      // Precondition: the database is open in the app.
      expect(setup.captureCharFrame()).toContain("Opened read-only");

      const spy = spyCloseAndExit();
      press(setup);
      await settle(setup);

      // The app must close the database when it quits. If it still calls process.exit, the close
      // must come first (exit would skip it otherwise).
      expect(spy.closeCalls()).toBeGreaterThanOrEqual(1);
      for (const exit of spy.exits) {
        expect(exit.closedBefore).toBeGreaterThanOrEqual(1);
      }
    });
  }
});

describe("C11b: DbService node key cache", () => {
  test("C11b: nodes created by importJson show their keys even if looked up before the import", async () => {
    const dir = await scratchDir();
    const sourcePath = join(dir, "source.kitedb");
    const dumpPath = join(dir, "dump.json");
    const targetPath = join(dir, "target.kitedb");

    createDb(sourcePath, ["user:alice", "user:bob", "user:carol"]);
    const source = Database.open(sourcePath, { readOnly: true });
    source.exportToJson(dumpPath);
    source.close();
    createDb(targetPath, []);

    const service = new DbService();
    cleanups.push(() => service.close());
    service.open(targetPath, false);

    // Browsing before the import: these ids don't exist yet, so the lookups return null.
    for (let id = 0; id <= 10; id++) {
      expect(service.getNodeKey(id)).toBeNull();
    }

    service.importJson(dumpPath);

    const page = service.getNodesPage({ limit: 100 });
    expect(page.items.length).toBe(3);
    const keys = page.items.map((id) => service.getNodeKey(id)).sort();
    expect(keys).toEqual(["user:alice", "user:bob", "user:carol"]);
  });
});

describe("C11-edges: node details", () => {
  test("C11-edges: getNodeDetail reports each edge's source and destination", async () => {
    const dir = await scratchDir();
    const dbPath = join(dir, "edges.kitedb");
    const db = Database.open(dbPath);
    db.begin();
    const alice = db.createNode("user:alice");
    const bob = db.createNode("user:bob");
    db.addEdgeByName(alice, "follows", bob);
    db.commit();
    db.close();

    const service = new DbService();
    cleanups.push(() => service.close());
    service.open(dbPath, true);

    const aliceDetail = service.getNodeDetail(alice)!;
    expect(aliceDetail.outEdges.map(({ src, etypeName, dst }) => ({ src, etypeName, dst }))).toEqual([
      { src: alice, etypeName: "follows", dst: bob },
    ]);
    const bobDetail = service.getNodeDetail(bob)!;
    expect(bobDetail.inEdges.map(({ src, etypeName, dst }) => ({ src, etypeName, dst }))).toEqual([
      { src: alice, etypeName: "follows", dst: bob },
    ]);
  });
});

describe("C11-build: build script", () => {
  test("C11-build: bun run build bundles the app", async () => {
    const outdir = await scratchDir();
    const tuiDir = resolve(import.meta.dir, "..");
    // The script writes to dist/; run a copy of it that writes to a scratch directory instead.
    const script = (await Bun.file(join(tuiDir, "scripts/build.ts")).text()).replace(
      'outdir: "dist"',
      `outdir: ${JSON.stringify(outdir)}`,
    );
    expect(script).toContain(outdir);
    const scriptPath = join(tuiDir, "scripts", `.w3-build-${process.pid}.ts`);
    await Bun.write(scriptPath, script);
    cleanups.push(() => rm(scriptPath, { force: true }));

    const proc = Bun.spawn(["bun", "run", scriptPath], { cwd: tuiDir, stdout: "pipe", stderr: "pipe" });
    const [exitCode, stderr] = await Promise.all([proc.exited, new Response(proc.stderr).text()]);
    expect({ exitCode, stderr: stderr.slice(0, 500) }).toEqual({ exitCode: 0, stderr: "" });
    expect(await Bun.file(join(outdir, "main.js")).exists()).toBe(true);
  });
});
