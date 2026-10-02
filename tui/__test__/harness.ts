/**
 * Shared helpers for the TUI render tests: render the app at a size, drive it with keys, and read
 * the rendered frame cell by cell.
 *
 * Every test file using `renderApp` or `scratchDir` must run `runCleanups` after each test:
 * `afterEach(runCleanups)`.
 */

import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Database } from "@kitedb/core";
import type { CapturedFrame, CapturedSpan } from "@opentui/core";
import { testRender } from "@opentui/solid";
import { App } from "../src/app.tsx";

export type TestSetup = Awaited<ReturnType<typeof testRender>>;

const cleanups: Array<() => void | Promise<void>> = [];

/** Undo what the last test set up (renderers, scratch directories), newest first. */
export async function runCleanups(): Promise<void> {
  while (cleanups.length > 0) {
    const cleanup = cleanups.pop()!;
    await cleanup();
  }
}

/** A temporary directory, removed after the test. */
export async function scratchDir(): Promise<string> {
  const dir = await mkdtemp(join(tmpdir(), "kitedb-tui-test-"));
  cleanups.push(() => rm(dir, { recursive: true, force: true }));
  return dir;
}

/** Render the app on a terminal of `width` x `height` cells. */
export async function renderApp(width = 140, height = 45): Promise<TestSetup> {
  const setup = await testRender(App, { width, height });
  cleanups.push(() => setup.renderer.destroy());
  await setup.renderOnce();
  return setup;
}

/** Give the stdin parser time to flush (a lone ESC waits for a possible escape sequence). */
export async function settle(setup: TestSetup): Promise<void> {
  await Bun.sleep(40);
  await setup.renderOnce();
}

export async function press(setup: TestSetup, key: string): Promise<void> {
  setup.mockInput.pressKey(key);
  await settle(setup);
}

/** Open a database read-only through the Open path input, as a user would. */
export async function openViaKeys(setup: TestSetup, path: string): Promise<void> {
  await press(setup, "o");
  await setup.mockInput.typeText(path);
  await settle(setup);
  setup.mockInput.pressEnter();
  await settle(setup);
}

/** Write a database at `path` where user:alice follows user:bob. */
export function writeFollowsGraph(path: string): void {
  const db = Database.open(path);
  db.begin();
  const alice = db.createNode("user:alice");
  const bob = db.createNode("user:bob");
  db.addEdgeByName(alice, "follows", bob);
  db.commit();
  db.close();
}

/** One screen cell: its character and the styled span that drew it. */
export interface Cell {
  char: string;
  span: CapturedSpan;
}

/** The frame as rows of cells (every character on screen here is one cell wide). */
export function cellRows(frame: CapturedFrame): Cell[][] {
  return frame.lines.map((line) => line.spans.flatMap((span) => Array.from(span.text, (char) => ({ char, span }))));
}

export function rowText(row: Cell[]): string {
  return row.map((cell) => cell.char).join("");
}
