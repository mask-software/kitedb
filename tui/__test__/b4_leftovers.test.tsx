/**
 * B4 leftovers: TUI rendering reproductions.
 *
 * The OpenTUI Solid reconciler assigns any prop it has no special case for as a plain field on the
 * renderable (`node[name] = value`). A prop the renderable doesn't define is therefore dropped
 * without an error at runtime, which is how these went unnoticed.
 *
 * tab-bar:  <tab_select> got `items` and `selected`. TabSelectRenderable takes
 *           `options: { name, description, value? }[]` and is moved with setSelectedIndex(), so the
 *           tab bar rendered empty and could not follow the active tab.
 * bold:     <text bold> sets an unused `bold` field. Text is bolded by its `attributes` or by a <b>
 *           child, so no heading rendered bold.
 * modal:    (found while writing modal-bg) the unlock modal sat in a <Portal>, which mounts a plain
 *           box into the renderer's root. The root lays its children out in a column, so that box
 *           went below the full-height app, off screen, and the modal with it: pressing w showed
 *           nothing.
 * modal-bg: the unlock modal's <box bg="black"> sets an unused `bg` field. A box fills with
 *           `backgroundColor`, so the modal stayed transparent and the panes showed through it.
 *           Only observable once the modal is on screen (see modal).
 * layout:   (found while checking the render) OpenTUI boxes default to flexDirection "column", but
 *           the header and the pane row were written as rows without saying so: their columns
 *           stacked, overflowed their boxes and drew over one another. Boxes also shrink by default,
 *           so the header gave up rows to the panes and its border ran through its text, a long
 *           database path spilled across the header's other columns, and once a database was open
 *           the details pane grew to its content and squeezed the fixed-width panes.
 * echo:     (found while checking the render) InputField read props.value once, outside JSX, so the
 *           Open path / Import / Export fields never showed what is typed into them.
 * arrow:    (found while checking the render) OpenTUI's Solid transform keeps JSX text as written,
 *           so `-&gt;` drew a literal "-&gt;" in the edge list and the node details.
 * stale:    (found while checking the render) @opentui/solid 0.1.77 doesn't remove a <Show>'s or
 *           <For>'s children placed directly in a <scrollbox>, and places a <For>'s new items at the
 *           end of its parent rather than where the <For> is. The lists' "No nodes on this page"
 *           stayed above the nodes, and moving the selection left the previous node's edges and
 *           empty-state lines in the details, with the new node's edges appended below.
 *
 * Each test looks only at text the other bugs leave intact, so it fails for its own finding: the
 * bold test checks headings the overlapping layout doesn't garble, the echo test looks for the typed
 * value without its (garbled) label, and the modal title is found by a word, not by its spaces,
 * which a transparent modal lets the panes show through.
 */

import { afterEach, describe, expect, test } from "bun:test";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import { Database } from "@kitedb/core";
import { TextAttributes, type CapturedFrame } from "@opentui/core";
import {
  type Cell,
  type TestSetup,
  cellRows,
  openViaKeys,
  press,
  renderApp,
  rowText,
  runCleanups,
  scratchDir,
  settle,
  writeFollowsGraph,
} from "./harness.ts";

afterEach(runCleanups);

/**
 * Render the app with a read-only database open: user:alice follows user:bob. The database sits
 * under a long directory name, so its path is too long for the header on any platform.
 */
async function renderOpenDb(): Promise<TestSetup> {
  const dir = join(await scratchDir(), "a-directory-name-long-enough-to-overflow-the-header-column");
  await mkdir(dir);
  const path = join(dir, "graph.kitedb");
  writeFollowsGraph(path);

  const setup = await renderApp();
  await openViaKeys(setup, path);
  expect(setup.captureCharFrame()).toContain("Opened read-only");
  return setup;
}

/** Render the app with a read-only database open, and press w to ask for the write-unlock modal. */
async function renderUnlockModal(): Promise<TestSetup> {
  const setup = await renderOpenDb();
  await press(setup, "w");
  return setup;
}

/** The cells showing the first occurrence of `text` on screen. */
function findCells(frame: CapturedFrame, text: string): Cell[] {
  for (const row of cellRows(frame)) {
    const column = rowText(row).indexOf(text);
    if (column >= 0) return row.slice(column, column + text.length);
  }
  throw new Error(`"${text}" is not on screen`);
}

function isBold(cells: Cell[]): boolean {
  const visible = cells.filter((cell) => cell.char !== " ");
  return visible.length > 0 && visible.every((cell) => (cell.span.attributes & TextAttributes.BOLD) !== 0);
}

/** A row showing the four tab labels in order: the tab bar. */
const TAB_BAR = /Nodes\s+Edges\s+Stats\s+Import\/Export/;

/** The label of the highlighted tab: the text the tab bar draws on a filled background. */
function highlightedTab(frame: CapturedFrame): string {
  const row = cellRows(frame).find((cells) => TAB_BAR.test(rowText(cells)));
  if (!row) throw new Error("no row shows the tab labels Nodes, Edges, Stats, Import/Export");
  return rowText(row.filter((cell) => cell.span.bg.a > 0)).trim();
}

/** The cells inside the unlock modal's double-line border. */
function modalInterior(frame: CapturedFrame): Cell[][] {
  const rows = cellRows(frame);
  const top = rows.findIndex((row) => rowText(row).includes("╔"));
  if (top < 0) throw new Error("the modal's border is not on screen");
  const left = rowText(rows[top]!).indexOf("╔");
  const right = rowText(rows[top]!).indexOf("╗", left);
  const bottom = rows.findIndex((row, index) => index > top && rowText(row)[left] === "╚");
  if (right < 0 || bottom < 0) throw new Error("the modal's border is incomplete");
  return rows.slice(top + 1, bottom).map((row) => row.slice(left + 1, right));
}

describe("tab-bar: the tab bar", () => {
  test("tab-bar: shows every tab label", async () => {
    const setup = await renderApp();
    expect(setup.captureCharFrame()).toMatch(TAB_BAR);
  });

  test("tab-bar: highlights the active tab as n/e/s/p and Tab switch tabs", async () => {
    const setup = await renderApp();
    expect(highlightedTab(setup.captureSpans())).toBe("Nodes");

    const seen: string[] = [];
    for (const key of ["e", "s", "p", "n"]) {
      await press(setup, key);
      seen.push(highlightedTab(setup.captureSpans()));
    }
    setup.mockInput.pressTab();
    await settle(setup);
    seen.push(highlightedTab(setup.captureSpans()));

    expect(seen).toEqual(["Edges", "Stats", "Import/Export", "Nodes", "Edges"]);
  });

  test("tab-bar: a clicked tab bar switches tabs with the arrow keys", async () => {
    const setup = await renderApp();
    const lines = setup.captureCharFrame().split("\n");
    const row = lines.findIndex((line) => TAB_BAR.test(line));
    expect(row).toBeGreaterThanOrEqual(0);

    // A click focuses the tab bar; focused, it moves its selection with the arrow keys.
    await setup.mockMouse.click(lines[row]!.indexOf("Nodes"), row);
    setup.mockInput.pressKey("ARROW_RIGHT");
    await settle(setup);
    expect(setup.captureCharFrame()).toContain("Edges (page 1)");
  });
});

describe("bold: bold text", () => {
  test("bold: the title and the section headings render bold, plain text does not", async () => {
    const setup = await renderApp();
    const frame = setup.captureSpans();

    const headings = ["KiteDB Explorer", "Filters", "Shortcuts", "Details"];
    expect(headings.map((heading) => ({ heading, bold: isBold(findCells(frame, heading)) }))).toEqual(
      headings.map((heading) => ({ heading, bold: true })),
    );
    expect(isBold(findCells(frame, "Ready"))).toBe(false);
  });

  test("bold: the unlock modal's title renders bold", async () => {
    const setup = await renderUnlockModal();
    const frame = setup.captureSpans();
    expect(isBold(findCells(frame, "Unlock"))).toBe(true);
    expect(isBold(findCells(frame, "mode?"))).toBe(true);
  });
});

describe("modal: the unlock modal", () => {
  test("modal: pressing w with a read-only database shows the unlock modal", async () => {
    const setup = await renderUnlockModal();
    const frame = setup.captureCharFrame();
    expect(frame).toMatch(/╔═+╗/);
    expect(frame).toContain("Unlock");
  });

  test("modal-bg: fills its whole interior with its black background", async () => {
    const setup = await renderUnlockModal();
    const interior = modalInterior(setup.captureSpans()).flat();
    expect(interior.length).toBeGreaterThan(0);

    const notBlack = interior.filter(({ span: { bg } }) => !(bg.a === 1 && bg.r === 0 && bg.g === 0 && bg.b === 0));
    expect({ cells: interior.length, notBlack: notBlack.length }).toEqual({ cells: interior.length, notBlack: 0 });
  });

  test("modal-bg: hides the panes behind it (only the modal's own text shows inside it)", async () => {
    const setup = await renderUnlockModal();
    const lines = modalInterior(setup.captureSpans())
      .map((row) => rowText(row).trim())
      .filter((line) => line !== "");
    expect(lines).toEqual([
      "Unlock write mode?",
      "Reopen the database with write access.",
      "Press y to confirm, n to cancel.",
    ]);
  });
});

describe("layout: pane layout", () => {
  test("layout: the header's three columns sit side by side", async () => {
    const setup = await renderApp();
    expect(setup.captureCharFrame()).toMatch(/KiteDB Explorer.*Read-only: yes.*Open path: \(press o\)/);
  });

  test("layout: the sidebar, list and details panes sit side by side", async () => {
    const setup = await renderApp();
    expect(setup.captureCharFrame()).toMatch(/Filters.*Nodes \(page 1\).*Details/);
  });

  test("layout: the header keeps its height, its border clear of its text", async () => {
    const setup = await renderApp();
    const frame = setup.captureCharFrame();
    expect(frame).toMatch(/│ KiteDB Explorer +Read-only: yes +Open path: \(press o\) +│/);
    expect(frame).toMatch(/│ No database +Nodes: - +Press w to unlock +│/);
    expect(frame).toMatch(/│ Path: - +Edges: - +Press r to refresh +│/);
  });

  test("layout: the panes keep their widths with a database open", async () => {
    const setup = await renderApp();
    const before = setup.captureCharFrame().split("\n").find((line) => line.includes("Filters"));
    const dir = await scratchDir();
    const path = join(dir, "widths.kitedb");
    const db = Database.open(path);
    db.begin();
    db.createNode("user:alice");
    db.commit();
    db.close();
    await openViaKeys(setup, path);

    const frame = setup.captureCharFrame();
    expect(frame).toContain("Node prefix: (empty)");
    expect(frame.split("\n").find((line) => line.includes("Filters"))).toBe(before);
  });

  test("layout: a long database path stays inside its header column", async () => {
    const setup = await renderOpenDb();
    const frame = setup.captureCharFrame();
    expect(frame).toMatch(/│ KiteDB Explorer +Read-only: yes +Open path: \S+ +│/);
    expect(frame).toMatch(/│ Connected +Nodes: \d+ +Press w to unlock +│/);
    expect(frame).toMatch(/│ Path: \S+ +Edges: \d+ +Press r to refresh +│/);
  });
});

describe("arrow: edges", () => {
  test("arrow: the node details and the edge list draw edges with ->", async () => {
    const setup = await renderOpenDb();
    // user:alice is selected: her outgoing edge.
    expect(setup.captureCharFrame()).toMatch(/follows -> \d+/);
    // user:bob: his incoming edge.
    await press(setup, "j");
    expect(setup.captureCharFrame()).toMatch(/\d+ -> follows/);
    await press(setup, "e");
    expect(setup.captureCharFrame()).toMatch(/\d+ -\[follows\]-> \d+/);
  });
});

/** The index of the first frame row matching `pattern`, or -1. */
function rowOf(frame: string, pattern: RegExp): number {
  return frame.split("\n").findIndex((line) => pattern.test(line));
}

/**
 * Patterns for the stale tests that the other findings can't hide or trip: an arrow is "->" or the
 * "-&gt;" the arrow bug draws, and an empty-state line matches even with other text drawn into its
 * spaces (the overlapping layout does that).
 */
const ARROW = String.raw`(?:->|-&gt;)`;
const NO_NODES = /No.nodes.on.this.page/;
const NO_EDGES = /No.edges.on.this.page/;

describe("stale: lists and details", () => {
  test("stale: the node list drops its empty-state line once the nodes load", async () => {
    const setup = await renderOpenDb();
    const frame = setup.captureCharFrame();
    expect(frame).toContain("user:alice");
    expect(frame).not.toMatch(NO_NODES);
  });

  test("stale: the edge list drops its empty-state line once the edges load", async () => {
    const setup = await renderApp();
    await press(setup, "e");
    expect(setup.captureCharFrame()).toMatch(NO_EDGES);

    const dir = await scratchDir();
    const path = join(dir, "edges.kitedb");
    const db = Database.open(path);
    db.begin();
    db.addEdgeByName(db.createNode("user:alice"), "follows", db.createNode("user:bob"));
    db.commit();
    db.close();
    await openViaKeys(setup, path);

    const frame = setup.captureCharFrame();
    expect(frame).toMatch(new RegExp(String.raw`-\[follows\]${ARROW}`));
    expect(frame).not.toMatch(NO_EDGES);
  });

  test("stale: moving the selection shows only the selected node's edges, in their sections", async () => {
    const setup = await renderOpenDb();
    // user:alice: one outgoing edge, no incoming.
    await press(setup, "j");
    // user:bob: no outgoing edge, one incoming.
    const frame = setup.captureCharFrame();
    expect(frame).toContain("Key: user:bob");
    expect(frame).not.toMatch(new RegExp(String.raw`follows ${ARROW} \d+`));
    expect(frame).not.toContain("No incoming edges");

    const sections = [/Outgoing/, /No outgoing edges/, /Incoming/, new RegExp(String.raw`\d+ ${ARROW} follows`)];
    const rows = sections.map((pattern) => rowOf(frame, pattern));
    expect(rows.every((row) => row >= 0)).toBe(true);
    expect([...rows].sort((a, b) => a - b)).toEqual(rows);
  });
});

describe("echo: input fields", () => {
  test("echo: the Open path field shows the path being typed", async () => {
    const setup = await renderApp();
    await press(setup, "o");
    await setup.mockInput.typeText("/tmp/echo-check");
    await settle(setup);
    expect(setup.captureCharFrame()).toContain("/tmp/echo-check");
  });
});
