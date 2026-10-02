import { createEffect, createMemo, createSignal, For, onCleanup, Show, type Accessor } from "solid-js";
import type { KeyEvent, TabSelectOption, TabSelectRenderable } from "@opentui/core";
import { useKeyboard, useRenderer, useTerminalDimensions, type JSX } from "@opentui/solid";
import { DbService } from "./db/db-service.ts";
import { nextPage, prevPage } from "./db/paging.ts";
import type { FullEdge, DbStats } from "@kitedb/core";

const PAGE_SIZE = 100;

/**
 * The layout puts a blank row around and between its blocks when the terminal has at least this
 * many rows: the 21 rows of the header, tab bar, status bar and their spacing, plus the sidebar's 18.
 * On a shorter one those rows would leave the panes no room, so they go.
 */
const ROOMY_MIN_HEIGHT = 39;

/** The sidebar's narrowest width: its longest line ("Node prefix: (empty)"), padding and border. */
const SIDEBAR_MIN_WIDTH = 24;

type TabKey = "nodes" | "edges" | "stats" | "import";

/** The tab bar's tabs, in order. */
const TABS: Array<TabSelectOption & { value: TabKey }> = [
  { name: "Nodes", description: "Browse nodes (n)", value: "nodes" },
  { name: "Edges", description: "Browse edges (e)", value: "edges" },
  { name: "Stats", description: "Database stats (s)", value: "stats" },
  { name: "Import/Export", description: "Import and export JSON (p)", value: "import" },
];

/** Wide enough for the longest tab name plus a space on each side. */
const TAB_WIDTH = Math.max(...TABS.map((tab) => tab.name.length)) + 2;

type InputTarget =
  | "openPath"
  | "nodeFilter"
  | "edgeFilter"
  | "importPath"
  | "exportPath"
  | null;

/** One printable character: a single code point that isn't a control character. */
const PRINTABLE = /^\P{Cc}$/u;

/**
 * The key an opentui event stands for: the typed character for printable keys, else opentui's key
 * name ("return", "escape", "backspace", "tab", "up", "pagedown", ...). Ctrl/alt/cmd combinations
 * give null, so they neither trigger shortcuts nor type text.
 */
function keyOf(event: KeyEvent): string | null {
  if (event.ctrl || event.meta || event.option || event.super) return null;
  return PRINTABLE.test(event.sequence) ? event.sequence : event.name;
}

export function App() {
  const db = new DbService();
  const renderer = useRenderer();
  const dimensions = useTerminalDimensions();
  /** Blank rows around and between blocks: 1, or 0 on a short terminal. */
  const space = createMemo(() => (dimensions().height >= ROOMY_MIN_HEIGHT ? 1 : 0));
  // q/Esc, Ctrl+C and exit signals all end the app by destroying the renderer, which disposes this
  // component: close the database here so every way out closes it.
  onCleanup(() => db.close());

  const [activeTab, setActiveTab] = createSignal<TabKey>("nodes");
  const [openPath, setOpenPath] = createSignal("");
  const [nodeFilter, setNodeFilter] = createSignal("");
  const [edgeFilter, setEdgeFilter] = createSignal("");
  const [importPath, setImportPath] = createSignal("");
  const [exportPath, setExportPath] = createSignal("");
  const [exportMode, setExportMode] = createSignal<"json" | "jsonl">("json");

  const [stats, setStats] = createSignal<DbStats | null>(null);
  const [nodesPage, setNodesPage] = createSignal<{ items: number[]; cursor?: string; nextCursor?: string; hasMore: boolean; total?: number }>({
    items: [],
    hasMore: false,
  });
  const [edgesPage, setEdgesPage] = createSignal<{ items: FullEdge[]; cursor?: string; nextCursor?: string; hasMore: boolean; total?: number }>({
    items: [],
    hasMore: false,
  });
  const [nodeHistory, setNodeHistory] = createSignal<string[]>([]);
  const [edgeHistory, setEdgeHistory] = createSignal<string[]>([]);

  const [selectedNodeIndex, setSelectedNodeIndex] = createSignal(0);
  const [selectedEdgeIndex, setSelectedEdgeIndex] = createSignal(0);
  const [selectedNodeId, setSelectedNodeId] = createSignal<number | null>(null);
  const [selectedEdge, setSelectedEdge] = createSignal<FullEdge | null>(null);

  const [activeInput, setActiveInput] = createSignal<InputTarget>(null);
  const [statusMessage, setStatusMessage] = createSignal<string | null>(null);
  const [showUnlockConfirm, setShowUnlockConfirm] = createSignal(false);

  const [opening, setOpening] = createSignal(false);
  const [refreshing, setRefreshing] = createSignal(false);
  const [importing, setImporting] = createSignal(false);
  const [exporting, setExporting] = createSignal(false);

  const [connected, setConnected] = createSignal(false);
  const [currentPath, setCurrentPath] = createSignal<string | null>(null);
  const [currentReadOnly, setCurrentReadOnly] = createSignal(true);

  const dbConnected = createMemo(() => connected());
  const dbPath = createMemo(() => currentPath());
  const dbReadOnly = createMemo(() => currentReadOnly());

  const labels = createMemo(() => (dbConnected() ? db.getLabels() : []));
  const edgeTypes = createMemo(() => (dbConnected() ? db.getEdgeTypes() : []));

  const edgeFilterId = createMemo(() => {
    if (!dbConnected()) return null;
    const filter = edgeFilter().trim();
    if (!filter) return null;
    return db.resolveEdgeTypeFilter(filter);
  });

  const edgeFilterError = createMemo(() => {
    if (!dbConnected()) return null;
    const filter = edgeFilter().trim();
    if (!filter) return null;
    if (edgeFilterId() === null) return "Unknown edge type";
    return null;
  });

  const canImport = createMemo(() => dbConnected() && !dbReadOnly());
  const canExport = createMemo(() => dbConnected());

  const selectedNodeDetail = createMemo(() => {
    const nodeId = selectedNodeId();
    if (nodeId === null || !dbConnected()) return null;
    return db.getNodeDetail(nodeId);
  });

  const selectedEdgeDetail = createMemo(() => {
    const edge = selectedEdge();
    if (!edge || !dbConnected()) return null;
    return db.getEdgeDetail(edge);
  });

  function setStatus(message: string | null) {
    setStatusMessage(message);
  }

  function refreshStats() {
    if (!dbConnected()) {
      setStats(null);
      return;
    }
    setStats(db.stats());
  }

  function loadNodesPage(cursor?: string) {
    if (!dbConnected()) {
      setNodesPage({ items: [], hasMore: false });
      return;
    }
    const page = db.getNodesPage({ limit: PAGE_SIZE, cursor });
    const filter = nodeFilter().trim();
    const filteredItems = filter
      ? page.items.filter((nodeId) => {
          const key = db.getNodeKey(nodeId) ?? "";
          return key.startsWith(filter);
        })
      : page.items;
    setNodesPage({ ...page, items: filteredItems, cursor });
  }

  function loadEdgesPage(cursor?: string) {
    if (!dbConnected()) {
      setEdgesPage({ items: [], hasMore: false });
      return;
    }
    const page = db.getEdgesPage({ limit: PAGE_SIZE, cursor });
    const filterId = edgeFilterId();
    const filterText = edgeFilter().trim();
    const filteredItems = filterText && filterId === null
      ? []
      : filterId
        ? page.items.filter((edge) => edge.etype === filterId)
        : page.items;
    setEdgesPage({ ...page, items: filteredItems, cursor });
  }

  function refreshAll() {
    if (!dbConnected()) {
      setNodesPage({ items: [], hasMore: false });
      setEdgesPage({ items: [], hasMore: false });
      setStats(null);
      return;
    }
    refreshStats();
    loadNodesPage(undefined);
    loadEdgesPage(undefined);
  }

  function openDatabase(path: string, readOnly: boolean) {
    if (!path.trim()) {
      setStatus("Provide a database path");
      return;
    }
    setOpening(true);
    try {
      db.open(path.trim(), readOnly);
      setConnected(true);
      setCurrentPath(path.trim());
      setCurrentReadOnly(readOnly);
      setStatus(readOnly ? "Opened read-only" : "Opened read/write");
      setNodeHistory([]);
      setEdgeHistory([]);
      refreshAll();
    } catch (error) {
      setStatus(error instanceof Error ? error.message : "Failed to open database");
      db.close();
      setConnected(false);
      setCurrentPath(null);
      setCurrentReadOnly(true);
    } finally {
      setOpening(false);
    }
  }

  function closeDatabase() {
    db.close();
    setConnected(false);
    setCurrentPath(null);
    setCurrentReadOnly(true);
    setSelectedNodeId(null);
    setSelectedEdge(null);
    setStats(null);
    setNodesPage({ items: [], hasMore: false });
    setEdgesPage({ items: [], hasMore: false });
    setNodeHistory([]);
    setEdgeHistory([]);
    setStatus("Closed database");
  }

  function unlockWrites() {
    if (!dbConnected()) return;
    const path = dbPath();
    if (!path) return;
    db.close();
    openDatabase(path, false);
  }

  function handleExport() {
    if (!canExport()) {
      setStatus("Open a database to export");
      return;
    }
    const path = exportPath().trim();
    if (!path) {
      setStatus("Provide an export path");
      return;
    }
    setExporting(true);
    try {
      const mode = exportMode();
      if (mode === "json") {
        db.exportJson(path);
      } else {
        db.exportJsonl(path);
      }
      setStatus(`Exported ${mode.toUpperCase()} to ${path}`);
    } catch (error) {
      setStatus(error instanceof Error ? error.message : "Export failed");
    } finally {
      setExporting(false);
    }
  }

  function handleImport() {
    if (!canImport()) {
      setStatus("Unlock write mode to import");
      return;
    }
    const path = importPath().trim();
    if (!path) {
      setStatus("Provide an import path");
      return;
    }
    setImporting(true);
    try {
      db.importJson(path);
      setStatus(`Imported JSON from ${path}`);
      refreshAll();
    } catch (error) {
      setStatus(error instanceof Error ? error.message : "Import failed");
    } finally {
      setImporting(false);
    }
  }

  function moveNodeSelection(delta: number) {
    const items = nodesPage().items;
    if (items.length === 0) return;
    const nextIndex = Math.max(0, Math.min(items.length - 1, selectedNodeIndex() + delta));
    setSelectedNodeIndex(nextIndex);
    setSelectedNodeId(items[nextIndex]);
  }

  function moveEdgeSelection(delta: number) {
    const items = edgesPage().items;
    if (items.length === 0) return;
    const nextIndex = Math.max(0, Math.min(items.length - 1, selectedEdgeIndex() + delta));
    setSelectedEdgeIndex(nextIndex);
    setSelectedEdge(items[nextIndex]);
  }

  function nextNodesPage() {
    const page = nodesPage();
    const move = nextPage(page, nodeHistory());
    if (move.cursor === page.cursor) return;
    setNodeHistory(move.history);
    loadNodesPage(move.cursor);
  }

  function prevNodesPage() {
    const move = prevPage(nodeHistory());
    setNodeHistory(move.history);
    loadNodesPage(move.cursor);
  }

  function nextEdgesPage() {
    const page = edgesPage();
    const move = nextPage(page, edgeHistory());
    if (move.cursor === page.cursor) return;
    setEdgeHistory(move.history);
    loadEdgesPage(move.cursor);
  }

  function prevEdgesPage() {
    const move = prevPage(edgeHistory());
    setEdgeHistory(move.history);
    loadEdgesPage(move.cursor);
  }

  function handleInputKey(key: string) {
    const target = activeInput();
    if (!target) return;

    if (key === "escape") {
      setActiveInput(null);
      setStatus(null);
      return;
    }

    // opentui's own inputs submit on both.
    if (key === "return" || key === "linefeed") {
      if (target === "openPath") {
        openDatabase(openPath(), true);
      } else if (target === "importPath") {
        handleImport();
      } else if (target === "exportPath") {
        handleExport();
      } else {
        if (target === "nodeFilter") {
          setNodeHistory([]);
          loadNodesPage(undefined);
        }
        if (target === "edgeFilter") {
          setEdgeHistory([]);
          loadEdgesPage(undefined);
        }
      }
      setActiveInput(null);
      return;
    }

    if (key === "backspace") {
      updateInputValue(target, (value) => value.slice(0, -1));
      return;
    }

    if (PRINTABLE.test(key)) {
      updateInputValue(target, (value) => value + key);
    }
  }

  function updateInputValue(target: Exclude<InputTarget, null>, updater: (value: string) => string) {
    switch (target) {
      case "openPath":
        setOpenPath(updater);
        break;
      case "nodeFilter":
        setNodeFilter(updater);
        break;
      case "edgeFilter":
        setEdgeFilter(updater);
        break;
      case "importPath":
        setImportPath(updater);
        break;
      case "exportPath":
        setExportPath(updater);
        break;
      default:
        break;
    }
  }

  useKeyboard((event) => {
    const key = keyOf(event);
    if (key === null) return;

    if (showUnlockConfirm()) {
      if (key.toLowerCase() === "y") {
        setShowUnlockConfirm(false);
        unlockWrites();
      } else if (key.toLowerCase() === "n" || key === "escape") {
        setShowUnlockConfirm(false);
        setStatus("Write unlock cancelled");
      }
      return;
    }

    if (activeInput()) {
      handleInputKey(key);
      return;
    }

    if (key === "q" || key === "escape") {
      // Restores the terminal and disposes the app (closing the database); main.tsx exits then.
      renderer.destroy();
      return;
    }

    if (key === "o") {
      setActiveInput("openPath");
      setStatus("Open path input (Enter to open)");
      return;
    }

    if (key === "c") {
      closeDatabase();
      return;
    }

    if (key === "r") {
      setRefreshing(true);
      try {
        refreshAll();
        setStatus("Refreshed");
      } finally {
        setRefreshing(false);
      }
      return;
    }

    if (key === "n") {
      setActiveTab("nodes");
      return;
    }

    if (key === "e") {
      setActiveTab("edges");
      return;
    }

    if (key === "s") {
      setActiveTab("stats");
      return;
    }

    if (key === "p") {
      setActiveTab("import");
      return;
    }

    if (key === "tab") {
      const index = TABS.findIndex((tab) => tab.value === activeTab());
      setActiveTab(TABS[(index + 1) % TABS.length].value);
      return;
    }

    if (key === "i") {
      setActiveTab("import");
      setActiveInput("importPath");
      setStatus("Import path input (Enter to import)");
      return;
    }

    if (key === "x") {
      setActiveTab("import");
      setActiveInput("exportPath");
      setStatus("Export path input (Enter to export)");
      return;
    }

    if (key === "m") {
      setExportMode((current) => (current === "json" ? "jsonl" : "json"));
      return;
    }

    if (key === "f") {
      if (activeTab() === "nodes") {
        setActiveInput("nodeFilter");
        setStatus("Node filter input (prefix)");
      } else if (activeTab() === "edges") {
        setActiveInput("edgeFilter");
        setStatus("Edge filter input (type name or id)");
      }
      return;
    }

    if (key === "w") {
      if (dbConnected() && dbReadOnly()) {
        setShowUnlockConfirm(true);
      }
      return;
    }

    if (key === "down" || key === "j") {
      if (activeTab() === "nodes") moveNodeSelection(1);
      if (activeTab() === "edges") moveEdgeSelection(1);
      return;
    }

    if (key === "up" || key === "k") {
      if (activeTab() === "nodes") moveNodeSelection(-1);
      if (activeTab() === "edges") moveEdgeSelection(-1);
      return;
    }

    if (key === "pagedown") {
      if (activeTab() === "nodes") nextNodesPage();
      if (activeTab() === "edges") nextEdgesPage();
      return;
    }

    if (key === "pageup") {
      if (activeTab() === "nodes") prevNodesPage();
      if (activeTab() === "edges") prevEdgesPage();
    }
  });

  createEffect(() => {
    if (!dbConnected()) return;
    loadNodesPage(undefined);
    loadEdgesPage(undefined);
    refreshStats();
  });

  createEffect(() => {
    const items = nodesPage().items;
    if (items.length === 0) {
      setSelectedNodeId(null);
      return;
    }
    const idx = Math.min(selectedNodeIndex(), items.length - 1);
    setSelectedNodeIndex(idx);
    setSelectedNodeId(items[idx]);
  });

  createEffect(() => {
    const items = edgesPage().items;
    if (items.length === 0) {
      setSelectedEdge(null);
      return;
    }
    const idx = Math.min(selectedEdgeIndex(), items.length - 1);
    setSelectedEdgeIndex(idx);
    setSelectedEdge(items[idx]);
  });

  let tabBar: TabSelectRenderable | undefined;
  // <tab_select> takes no selected-tab prop; keep its selection on the active tab.
  createEffect(() => {
    const index = TABS.findIndex((tab) => tab.value === activeTab());
    tabBar?.setSelectedIndex(index);
  });

  return (
    <box flexDirection="column" height="100%" width="100%" padding={space()} gap={space()}>
      {/*
        Boxes shrink by default: the header and the status bar keep their size, the panes give.
        Every bordered box clips its content (overflow hidden): what doesn't fit is cut off at its
        border instead of drawing over its neighbours.
      */}
      <box
        flexDirection="row"
        flexShrink={0}
        borderStyle="rounded"
        overflow="hidden"
        paddingLeft={1}
        paddingRight={1}
        paddingTop={space()}
        paddingBottom={space()}
        gap={2}
      >
        {/* The first and last columns share the width the middle one leaves; long paths truncate. */}
        <box flexDirection="column" gap={space()} flexGrow={1} flexBasis={0}>
          <text><b>KiteDB Explorer</b></text>
          <text fg={dbConnected() ? "green" : "yellow"}>
            {dbConnected() ? "Connected" : "No database"}
          </text>
          <text wrapMode="none" truncate>Path: {dbPath() ?? "-"}</text>
        </box>
        <box flexDirection="column" gap={space()} flexShrink={0}>
          <text>Read-only: {dbReadOnly() ? "yes" : "no"}</text>
          <text>Nodes: {stats()?.snapshotNodes?.toString() ?? "-"}</text>
          <text>Edges: {stats()?.snapshotEdges?.toString() ?? "-"}</text>
        </box>
        <box flexDirection="column" gap={space()} flexGrow={1} flexBasis={0}>
          <InputField label="Open path" value={openPath()} placeholder="(press o)" active={activeInput() === "openPath"} />
          <text fg={dbReadOnly() ? "yellow" : "green"}>
            {dbReadOnly() ? "Press w to unlock" : "Write enabled"}
          </text>
          <text>Press r to refresh</text>
        </box>
      </box>

      {/* A clicked tab bar takes focus and moves its selection with the arrow keys: follow it. */}
      <tab_select
        ref={tabBar}
        options={TABS}
        tabWidth={TAB_WIDTH}
        showDescription={false}
        onChange={(index) => setActiveTab(TABS[index].value)}
      />

      <box flexDirection="row" flexGrow={1} gap={1}>
        <Pane space={space()} width="26%" minWidth={SIDEBAR_MIN_WIDTH}>
          {/* Blocks keep their height: what doesn't fit is clipped at the bottom, not overlapped. */}
          <box flexDirection="column" flexShrink={0} gap={space()}>
            <text><b>Filters</b></text>
            <FilterField label="Node prefix" value={nodeFilter()} active={activeInput() === "nodeFilter"} />
            <FilterField label="Edge type" value={edgeFilter()} active={activeInput() === "edgeFilter"} />
            <Show when={edgeFilterError()}>
              {(message) => <text fg="red">{message()}</text>}
            </Show>
          </box>

          <box flexDirection="column" flexShrink={0}>
            <text><b>Shortcuts</b></text>
            <text>o open path</text>
            <text>c close db</text>
            <text>w unlock writes</text>
            <text>n/e/s/p tabs</text>
            <text>f filter field</text>
            <text>i import / x export</text>
            <text>j/k or arrows</text>
          </box>
        </Pane>

        <Pane space={space()} width="40%">
          <Show when={activeTab() === "nodes"}>
            <box flexDirection="column" flexGrow={1} gap={space()}>
              <text><b>Nodes (page {nodeHistory().length + 1})</b></text>
              <scrollbox flexGrow={1}>
                <ListOrEmpty each={nodesPage().items} empty="No nodes on this page">
                  {(nodeId, index) => (
                    <text
                      wrapMode="none"
                      truncate
                      bg={index() === selectedNodeIndex() ? "cyan" : undefined}
                      fg={index() === selectedNodeIndex() ? "black" : "white"}
                    >
                      {nodeId.toString().padEnd(8)} {db.getNodeKey(nodeId) ?? "(no key)"}
                    </text>
                  )}
                </ListOrEmpty>
              </scrollbox>
              <text>PageUp/PageDown to navigate</text>
            </box>
          </Show>

          <Show when={activeTab() === "edges"}>
            <box flexDirection="column" flexGrow={1} gap={space()}>
              <text><b>Edges (page {edgeHistory().length + 1})</b></text>
              <scrollbox flexGrow={1}>
                <ListOrEmpty each={edgesPage().items} empty="No edges on this page">
                  {(edge, index) => {
                    const name = db.getEdgeTypeName(edge.etype) ?? `#${edge.etype}`;
                    return (
                      <text
                        wrapMode="none"
                        truncate
                        bg={index() === selectedEdgeIndex() ? "cyan" : undefined}
                        fg={index() === selectedEdgeIndex() ? "black" : "white"}
                      >
                        {`${edge.src} -[${name}]-> ${edge.dst}`}
                      </text>
                    );
                  }}
                </ListOrEmpty>
              </scrollbox>
              <text>PageUp/PageDown to navigate</text>
            </box>
          </Show>

          <Show when={activeTab() === "stats"}>
            <box flexDirection="column" gap={space()}>
              <text><b>Stats</b></text>
              <Show when={stats()} fallback={<text fg="yellow">No stats available</text>}>
                {(current) => (
                  <box flexDirection="column" gap={space()}>
                    <text>Snapshot nodes: {current().snapshotNodes.toString()}</text>
                    <text>Snapshot edges: {current().snapshotEdges.toString()}</text>
                    <text>Delta created: {current().deltaNodesCreated}</text>
                    <text>Delta edges: {current().deltaEdgesAdded}</text>
                    <text>WAL bytes: {current().walBytes}</text>
                    <text>Recommend compact: {current().recommendCompact ? "yes" : "no"}</text>
                  </box>
                )}
              </Show>
              <box flexDirection="column" gap={space()}>
                <text><b>Labels</b></text>
                <For each={labels()}>{(name) => <text>- {name}</text>}</For>
                <Show when={labels().length === 0}>
                  <text fg="yellow">No labels</text>
                </Show>
              </box>
              <box flexDirection="column" gap={space()}>
                <text><b>Edge types</b></text>
                <For each={edgeTypes()}>{(name) => <text>- {name}</text>}</For>
                <Show when={edgeTypes().length === 0}>
                  <text fg="yellow">No edge types</text>
                </Show>
              </box>
            </box>
          </Show>

          <Show when={activeTab() === "import"}>
            <box flexDirection="column" gap={space()}>
              <text><b>Import / Export</b></text>
              <text>Import (JSON):</text>
              <InputField label="Path" value={importPath()} active={activeInput() === "importPath"} />
              <text>Export:</text>
              <InputField label="Path" value={exportPath()} active={activeInput() === "exportPath"} />
              <box flexDirection="row" gap={1}>
                <text>Mode:</text>
                <text fg={exportMode() === "json" ? "cyan" : "white"}>JSON</text>
                <text fg={exportMode() === "jsonl" ? "cyan" : "white"}>JSONL</text>
              </box>
              <text>Press i to import, x to export</text>
              <text>Press m to toggle export mode</text>
            </box>
          </Show>
        </Pane>

        <Pane space={space()} flexGrow={1} flexBasis={0}>
          <text><b>Details</b></text>
          <Show when={activeTab() === "nodes" && selectedNodeDetail()}>
            {(detail) => (
              <scrollbox flexGrow={1}>
                <text>ID: {detail().id}</text>
                <text>Key: {detail().key ?? "(none)"}</text>
                <text>Labels: {detail().labels.join(", ") || "-"}</text>
                <text>Out degree: {detail().outDegree}</text>
                <text>In degree: {detail().inDegree}</text>
                <text><b>Props</b></text>
                <ListOrEmpty each={detail().props} empty="No props">
                  {(prop) => <text>{prop.key}: {prop.value}</text>}
                </ListOrEmpty>
                <text><b>Outgoing</b></text>
                <ListOrEmpty each={detail().outEdges} empty="No outgoing edges">
                  {(edge) => <text>{`${edge.etypeName} -> ${edge.dst}`}</text>}
                </ListOrEmpty>
                <text><b>Incoming</b></text>
                <ListOrEmpty each={detail().inEdges} empty="No incoming edges">
                  {(edge) => <text>{`${edge.src} -> ${edge.etypeName}`}</text>}
                </ListOrEmpty>
              </scrollbox>
            )}
          </Show>

          <Show when={activeTab() === "edges" && selectedEdgeDetail()}>
            {(detail) => (
              <scrollbox flexGrow={1}>
                <text>Src: {detail().src}</text>
                <text>Type: {detail().etypeName}</text>
                <text>Dst: {detail().dst}</text>
                <text><b>Props</b></text>
                <ListOrEmpty each={detail().props} empty="No props">
                  {(prop) => <text>{prop.key}: {prop.value}</text>}
                </ListOrEmpty>
              </scrollbox>
            )}
          </Show>

          <Show when={activeTab() !== "nodes" && activeTab() !== "edges"}>
            <text fg="yellow">Select Nodes or Edges to inspect details</text>
          </Show>
        </Pane>
      </box>

      <box
        flexDirection="row"
        flexShrink={0}
        borderStyle="rounded"
        overflow="hidden"
        paddingLeft={1}
        paddingRight={1}
        paddingTop={space()}
        paddingBottom={space()}
      >
        <text>
          {statusMessage() ?? "Ready"}
        </text>
        <text fg={refreshing() ? "yellow" : "white"}>
          {refreshing() ? " | refreshing" : ""}
        </text>
        <text fg={opening() ? "yellow" : "white"}>
          {opening() ? " | opening" : ""}
        </text>
        <text fg={importing() ? "yellow" : "white"}>
          {importing() ? " | importing" : ""}
        </text>
        <text fg={exporting() ? "yellow" : "white"}>
          {exporting() ? " | exporting" : ""}
        </text>
      </box>

      {/*
        An absolute child of the full-screen root box, drawn over the panes. Not a <Portal>: that
        mounts into the renderer's root, which stacks it below this full-height box, off screen.
      */}
      <Show when={showUnlockConfirm()}>
        <box
          position="absolute"
          top={4}
          left={8}
          zIndex={1}
          width={50}
          borderStyle="double"
          padding={1}
          backgroundColor="black"
        >
          <text fg="yellow"><b>Unlock write mode?</b></text>
          <text>Reopen the database with write access.</text>
          <text>Press y to confirm, n to cancel.</text>
        </box>
      </Show>
    </box>
  );
}

/**
 * One of the three panes: a bordered column that clips what doesn't fit. `space` is the blank rows
 * around and between its blocks.
 */
function Pane(props: {
  space: number;
  width?: `${number}%`;
  minWidth?: number;
  flexGrow?: number;
  flexBasis?: number;
  children: JSX.Element;
}) {
  return (
    <box
      borderStyle="rounded"
      overflow="hidden"
      flexDirection="column"
      width={props.width}
      minWidth={props.minWidth}
      flexGrow={props.flexGrow}
      flexBasis={props.flexBasis}
      paddingLeft={1}
      paddingRight={1}
      paddingTop={props.space}
      paddingBottom={props.space}
      gap={props.space}
    >
      {props.children}
    </box>
  );
}

/**
 * A list, or a line saying it is empty, in a box of its own. @opentui/solid 0.1.77 mishandles a
 * <For> or <Show> that shares its parent: directly in a <scrollbox> the children they drop stay on
 * screen, and a <For> that was empty adds its items after the siblings that follow it. Here the
 * only sibling after the <For> is the empty-state line, which goes as the items come.
 */
function ListOrEmpty<T>(props: {
  each: readonly T[];
  empty: string;
  children: (item: T, index: Accessor<number>) => JSX.Element;
}) {
  return (
    <box flexDirection="column">
      <For each={props.each}>{props.children}</For>
      <Show when={props.each.length === 0}>
        <text fg="yellow">{props.empty}</text>
      </Show>
    </box>
  );
}

function FilterField(props: { label: string; value: string; active: boolean }) {
  return (
    <box flexDirection="row" gap={1}>
      <text>{props.label}:</text>
      <text bg={props.active ? "blue" : undefined} fg={props.active ? "white" : "gray"}>
        {props.value || "(empty)"}
      </text>
    </box>
  );
}

function InputField(props: { label: string; value: string; active: boolean; placeholder?: string }) {
  return (
    <box flexDirection="row" gap={1}>
      <text flexShrink={0}>{props.label}:</text>
      <text wrapMode="none" truncate bg={props.active ? "blue" : undefined} fg={props.active ? "white" : "gray"}>
        {props.value || props.placeholder || "(empty)"}
      </text>
    </box>
  );
}
