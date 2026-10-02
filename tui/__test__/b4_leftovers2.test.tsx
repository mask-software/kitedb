/**
 * B4 leftovers 2: the TUI on an 80x24 terminal, and its direct dependencies.
 *
 * small:  the layout was sized for a tall terminal. At 80x24 the header (9 rows), tab bar and status
 *         bar (5 rows) left the panes 4 rows, 0 of them inside their padding, and nothing clipped
 *         what didn't fit: the sidebar's shortcuts ran across the panes' bottom borders, into the
 *         status bar ("Readyock writes") and below it, and the list's empty-state line drew over
 *         its pane's border.
 * deps:   __test__ imported TextAttributes from @opentui/core, which was only a dependency of
 *         @opentui/solid: it resolved through hoisting alone.
 */

import { afterEach, describe, expect, test } from "bun:test";
import { readdir, readFile } from "node:fs/promises";
import { join } from "node:path";
import { openViaKeys, press, renderApp, runCleanups, scratchDir, writeFollowsGraph, type TestSetup } from "./harness.ts";

afterEach(runCleanups);

const WIDTH = 80;
const HEIGHT = 24;

/** A box drawn with rounded corners: the rows and columns of its border. */
interface Box {
  top: number;
  bottom: number;
  left: number;
  right: number;
}

/** Every rounded box on screen, found by its top-left corner. */
function roundedBoxes(lines: string[]): Box[] {
  const boxes: Box[] = [];
  lines.forEach((line, top) => {
    for (let left = line.indexOf("╭"); left >= 0; left = line.indexOf("╭", left + 1)) {
      const right = line.indexOf("╮", left + 1);
      const bottom = lines.findIndex((row, index) => index > top && row[left] === "╰");
      boxes.push({ top, bottom, left, right });
    }
  });
  return boxes;
}

/**
 * What is wrong with the boxes' borders: a missing corner or edge, or text drawn over one. An empty
 * list means every box is whole.
 */
function brokenBorders(frame: string): string[] {
  const lines = frame.split("\n");
  const problems: string[] = [];
  for (const { top, bottom, left, right } of roundedBoxes(lines)) {
    const at = `box at row ${top}, column ${left}`;
    if (right < 0 || bottom < 0 || lines[bottom]![right] !== "╯") {
      problems.push(`${at}: incomplete`);
      continue;
    }
    const topEdge = lines[top]!.slice(left + 1, right);
    const bottomEdge = lines[bottom]!.slice(left + 1, right);
    if (!/^─*$/.test(topEdge)) problems.push(`${at}: top edge "${topEdge}"`);
    if (!/^─*$/.test(bottomEdge)) problems.push(`${at}: bottom edge "${bottomEdge}"`);
    for (let row = top + 1; row < bottom; row++) {
      const sides = lines[row]![left]! + lines[row]![right]!;
      if (sides !== "││") problems.push(`${at}: row ${row} sides "${sides}"`);
    }
  }
  return problems;
}

/** The non-blank lines inside a box, trimmed. */
function boxText(frame: string, box: Box): string[] {
  return frame
    .split("\n")
    .slice(box.top + 1, box.bottom)
    .map((line) => line.slice(box.left + 1, box.right).trim())
    .filter((line) => line !== "");
}

/** The box whose interior shows `text`. */
function boxShowing(frame: string, text: string): Box {
  const box = roundedBoxes(frame.split("\n")).find((candidate) => boxText(frame, candidate).some((line) => line.includes(text)));
  if (!box) throw new Error(`no box shows "${text}"`);
  return box;
}

/** The status bar: the lowest box on screen. */
function statusBar(frame: string): Box {
  const boxes = roundedBoxes(frame.split("\n"));
  if (boxes.length === 0) throw new Error("no box on screen");
  return boxes.reduce((lowest, box) => (box.top > lowest.top ? box : lowest));
}

async function renderSmall(): Promise<TestSetup> {
  return renderApp(WIDTH, HEIGHT);
}

/** An 80x24 app with a read-only database open (user:alice follows user:bob). */
async function renderSmallOpenDb(): Promise<TestSetup> {
  const path = join(await scratchDir(), "graph.kitedb");
  writeFollowsGraph(path);
  const setup = await renderSmall();
  await openViaKeys(setup, path);
  return setup;
}

describe("small: an 80x24 terminal", () => {
  test("small: every box is whole, with no text drawn over a border", async () => {
    const setup = await renderSmall();
    const frame = setup.captureCharFrame();
    // The header, the three panes and the status bar.
    expect(roundedBoxes(frame.split("\n"))).toHaveLength(5);
    expect(brokenBorders(frame)).toEqual([]);
  });

  test("small: every tab keeps every box whole with a database open", async () => {
    const setup = await renderSmallOpenDb();
    const problems: Record<string, string[]> = {};
    for (const key of ["n", "e", "s", "p"]) {
      await press(setup, key);
      problems[key] = brokenBorders(setup.captureCharFrame());
    }
    expect(problems).toEqual({ n: [], e: [], s: [], p: [] });
  });

  test("small: the status bar shows only the status, and nothing is drawn below it", async () => {
    const setup = await renderSmall();
    const frame = setup.captureCharFrame();
    const status = statusBar(frame);
    expect(boxText(frame, status)).toEqual(["Ready"]);
    expect(frame.split("\n").slice(status.bottom + 1).join("").trim()).toBe("");
  });

  test("small: the header, the tab bar and the pane headings fit", async () => {
    const setup = await renderSmall();
    const frame = setup.captureCharFrame();
    expect(frame).toMatch(/│ KiteDB Explorer +Read-only: yes +Open path: \(press o\) +│/);
    expect(frame).toMatch(/Nodes\s+Edges\s+Stats\s+Import\/Export/);
    expect(frame).toMatch(/│ Filters +│ │ Nodes \(page 1\) +│ │ Details +│/);
  });

  test("small: the panes have room for a node, its details and the status", async () => {
    const setup = await renderSmallOpenDb();
    const frame = setup.captureCharFrame();
    expect(boxText(frame, boxShowing(frame, "Nodes (page 1)"))).toContainEqual(expect.stringContaining("user:alice"));
    const details = boxText(frame, boxShowing(frame, "Details"));
    expect(details).toContain("Key: user:alice");
    expect(details).toContainEqual(expect.stringMatching(/^follows -> \d+$/));
    expect(boxText(frame, statusBar(frame))).toEqual(["Opened read-only"]);
  });

  test("small: the unlock modal fits, whole", async () => {
    const setup = await renderSmallOpenDb();
    await press(setup, "w");
    const lines = setup.captureCharFrame().split("\n");
    const top = lines.findIndex((line) => line.includes("╔"));
    expect(top).toBeGreaterThanOrEqual(0);
    const left = lines[top]!.indexOf("╔");
    const right = lines[top]!.indexOf("╗", left);
    const bottom = lines.findIndex((line, index) => index > top && line[left] === "╚");
    expect({ right: right > left, bottom: bottom > top, corner: lines[bottom]?.[right] }).toEqual({
      right: true,
      bottom: true,
      corner: "╝",
    });
    const interior = lines.slice(top + 1, bottom).map((line) => line.slice(left + 1, right).trim());
    expect(interior.filter((line) => line !== "")).toEqual([
      "Unlock write mode?",
      "Reopen the database with write access.",
      "Press y to confirm, n to cancel.",
    ]);
  });
});

/**
 * The specifiers a module imports: `import ... from` and `export ... from` statements (spanning
 * lines, but not crossing a string), side-effect imports and dynamic `import()` calls.
 */
const IMPORT_PATTERNS = [
  /^\s*(?:import|export)\s[^"'`]*?\sfrom\s+"([^"\n]+)"/gm,
  /^\s*import\s+"([^"\n]+)"/gm,
  /\bimport\(\s*"([^"\n]+)"\s*\)/g,
];

function importedSpecifiers(source: string): string[] {
  return IMPORT_PATTERNS.flatMap((pattern) => [...source.matchAll(pattern)].map((match) => match[1]!));
}

/** The package name of a bare import specifier ("@scope/name/sub" -> "@scope/name"). */
function packageName(specifier: string): string {
  const parts = specifier.split("/");
  return specifier.startsWith("@") ? parts.slice(0, 2).join("/") : parts[0]!;
}

async function sourceFiles(dir: string): Promise<string[]> {
  const entries = await readdir(dir, { withFileTypes: true, recursive: true });
  return entries
    .filter((entry) => entry.isFile() && /\.(ts|tsx)$/.test(entry.name))
    .map((entry) => join(entry.parentPath, entry.name));
}

describe("deps: direct dependencies", () => {
  test("deps: every package the app, its scripts and its tests import is a direct dependency", async () => {
    const root = join(import.meta.dir, "..");
    const pkg = JSON.parse(await readFile(join(root, "package.json"), "utf8"));
    const declared = new Set([...Object.keys(pkg.dependencies ?? {}), ...Object.keys(pkg.devDependencies ?? {})]);

    const imported = new Set<string>();
    for (const dir of ["src", "scripts", "__test__"]) {
      for (const file of await sourceFiles(join(root, dir))) {
        for (const specifier of importedSpecifiers(await readFile(file, "utf8"))) {
          if (specifier.startsWith(".") || /^(node|bun):/.test(specifier)) continue;
          imported.add(packageName(specifier));
        }
      }
    }
    expect([...imported].filter((name) => !declared.has(name)).sort()).toEqual([]);
  });
});
