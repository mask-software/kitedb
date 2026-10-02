import { createEffect, createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import type { KeyEvent } from "@opentui/core";
import { Portal, useKeyboard, useRenderer } from "@opentui/solid";
import { DbService } from "./db/db-service.ts";
import { nextPage, prevPage } from "./db/paging.ts";
import type { JsFullEdge, DbStats } from "@kitedb/core";

const PAGE_SIZE = 100;

type TabKey = "nodes" | "edges" | "stats" | "import";

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
  const [edgesPage, setEdgesPage] = createSignal<{ items: JsFullEdge[]; cursor?: string; nextCursor?: string; hasMore: boolean; total?: number }>({
    items: [],
    hasMore: false,
  });
  const [nodeHistory, setNodeHistory] = createSignal<string[]>([]);
  const [edgeHistory, setEdgeHistory] = createSignal<string[]>([]);

  const [selectedNodeIndex, setSelectedNodeIndex] = createSignal(0);
  const [selectedEdgeIndex, setSelectedEdgeIndex] = createSignal(0);
  const [selectedNodeId, setSelectedNodeId] = createSignal<number | null>(null);
  const [selectedEdge, setSelectedEdge] = createSignal<JsFullEdge | null>(null);

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
      const order: TabKey[] = ["nodes", "edges", "stats", "import"];
      const index = order.indexOf(activeTab());
      const next = order[(index + 1) % order.length] ?? "nodes";
      setActiveTab(next);
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

  const tabs = ["Nodes", "Edges", "Stats", "Import/Export"];

  return (
    <box flexDirection="column" height="100%" width="100%" padding={1} gap={1}>
      <box borderStyle="rounded" padding={1} gap={2}>
        <box flexDirection="column" gap={1} flexGrow={1}>
          <text bold>KiteDB Explorer</text>
          <text fg={dbConnected() ? "green" : "yellow"}>
            {dbConnected() ? "Connected" : "No database"}
          </text>
          <text>Path: {dbPath() ?? "-"}</text>
        </box>
        <box flexDirection="column" gap={1}>
          <text>Read-only: {dbReadOnly() ? "yes" : "no"}</text>
          <text>Nodes: {stats()?.snapshotNodes?.toString() ?? "-"}</text>
          <text>Edges: {stats()?.snapshotEdges?.toString() ?? "-"}</text>
        </box>
        <box flexDirection="column" gap={1}>
          <InputField label="Open path" value={openPath()} placeholder="(press o)" active={activeInput() === "openPath"} />
          <text fg={dbReadOnly() ? "yellow" : "green"}>
            {dbReadOnly() ? "Press w to unlock" : "Write enabled"}
          </text>
          <text>Press r to refresh</text>
        </box>
      </box>

      <box flexGrow={1} gap={1}>
        <box borderStyle="rounded" padding={1} width="26%" flexDirection="column" gap={1}>
          <tab_select
            items={tabs}
            selected={tabs.indexOf(activeTab() === "nodes" ? "Nodes" : activeTab() === "edges" ? "Edges" : activeTab() === "stats" ? "Stats" : "Import/Export")}
            onSelect={(index: number) => {
              const next = index === 0 ? "nodes" : index === 1 ? "edges" : index === 2 ? "stats" : "import";
              setActiveTab(next);
            }}
          />

          <box flexDirection="column" gap={1}>
            <text bold>Filters</text>
            <FilterField label="Node prefix" value={nodeFilter()} active={activeInput() === "nodeFilter"} />
            <FilterField label="Edge type" value={edgeFilter()} active={activeInput() === "edgeFilter"} />
            <Show when={edgeFilterError()}>
              {(message) => <text fg="red">{message()}</text>}
            </Show>
          </box>

          <box flexDirection="column" gap={1}>
            <text bold>Shortcuts</text>
            <text>o open path</text>
            <text>c close db</text>
            <text>w unlock writes</text>
            <text>n/e/s/p tabs</text>
            <text>f filter field</text>
            <text>i import / x export</text>
            <text>j/k or arrows</text>
          </box>
        </box>

        <box borderStyle="rounded" padding={1} width="40%" flexDirection="column" gap={1}>
          <Show when={activeTab() === "nodes"}>
            <box flexDirection="column" gap={1}>
              <text bold>Nodes (page {nodeHistory().length + 1})</text>
              <scrollbox flexGrow={1}>
                <For each={nodesPage().items}>
                  {(nodeId, index) => (
                    <text bg={index() === selectedNodeIndex() ? "cyan" : undefined} fg={index() === selectedNodeIndex() ? "black" : "white"}>
                      {nodeId.toString().padEnd(8)} {db.getNodeKey(nodeId) ?? "(no key)"}
                    </text>
                  )}
                </For>
                <Show when={nodesPage().items.length === 0}>
                  <text fg="yellow">No nodes on this page</text>
                </Show>
              </scrollbox>
              <text>PageUp/PageDown to navigate</text>
            </box>
          </Show>

          <Show when={activeTab() === "edges"}>
            <box flexDirection="column" gap={1}>
              <text bold>Edges (page {edgeHistory().length + 1})</text>
              <scrollbox flexGrow={1}>
                <For each={edgesPage().items}>
                  {(edge, index) => {
                    const name = db.getEdgeTypeName(edge.etype) ?? `#${edge.etype}`;
                    return (
                      <text bg={index() === selectedEdgeIndex() ? "cyan" : undefined} fg={index() === selectedEdgeIndex() ? "black" : "white"}>
                        {edge.src} -[{name}]-&gt; {edge.dst}
                      </text>
                    );
                  }}
                </For>
                <Show when={edgesPage().items.length === 0}>
                  <text fg="yellow">No edges on this page</text>
                </Show>
              </scrollbox>
              <text>PageUp/PageDown to navigate</text>
            </box>
          </Show>

          <Show when={activeTab() === "stats"}>
            <box flexDirection="column" gap={1}>
              <text bold>Stats</text>
              <Show when={stats()} fallback={<text fg="yellow">No stats available</text>}>
                {(current) => (
                  <box flexDirection="column" gap={1}>
                    <text>Snapshot nodes: {current().snapshotNodes.toString()}</text>
                    <text>Snapshot edges: {current().snapshotEdges.toString()}</text>
                    <text>Delta created: {current().deltaNodesCreated}</text>
                    <text>Delta edges: {current().deltaEdgesAdded}</text>
                    <text>WAL bytes: {current().walBytes}</text>
                    <text>Recommend compact: {current().recommendCompact ? "yes" : "no"}</text>
                  </box>
                )}
              </Show>
              <box flexDirection="column" gap={1}>
                <text bold>Labels</text>
                <For each={labels()}>{(name) => <text>- {name}</text>}</For>
                <Show when={labels().length === 0}>
                  <text fg="yellow">No labels</text>
                </Show>
              </box>
              <box flexDirection="column" gap={1}>
                <text bold>Edge types</text>
                <For each={edgeTypes()}>{(name) => <text>- {name}</text>}</For>
                <Show when={edgeTypes().length === 0}>
                  <text fg="yellow">No edge types</text>
                </Show>
              </box>
            </box>
          </Show>

          <Show when={activeTab() === "import"}>
            <box flexDirection="column" gap={1}>
              <text bold>Import / Export</text>
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
        </box>

        <box borderStyle="rounded" padding={1} flexGrow={1} flexDirection="column" gap={1}>
          <text bold>Details</text>
          <Show when={activeTab() === "nodes" && selectedNodeDetail()}>
            {(detail) => (
              <scrollbox flexGrow={1}>
                <text>ID: {detail().id}</text>
                <text>Key: {detail().key ?? "(none)"}</text>
                <text>Labels: {detail().labels.join(", ") || "-"}</text>
                <text>Out degree: {detail().outDegree}</text>
                <text>In degree: {detail().inDegree}</text>
                <text bold>Props</text>
                <For each={detail().props}>
                  {(prop) => <text>{prop.key}: {prop.value}</text>}
                </For>
                <Show when={detail().props.length === 0}>
                  <text fg="yellow">No props</text>
                </Show>
                <text bold>Outgoing</text>
                <For each={detail().outEdges}>
                  {(edge) => <text>{edge.etypeName} -&gt; {edge.dst}</text>}
                </For>
                <Show when={detail().outEdges.length === 0}>
                  <text fg="yellow">No outgoing edges</text>
                </Show>
                <text bold>Incoming</text>
                <For each={detail().inEdges}>
                  {(edge) => <text>{edge.src} -&gt; {edge.etypeName}</text>}
                </For>
                <Show when={detail().inEdges.length === 0}>
                  <text fg="yellow">No incoming edges</text>
                </Show>
              </scrollbox>
            )}
          </Show>

          <Show when={activeTab() === "edges" && selectedEdgeDetail()}>
            {(detail) => (
              <scrollbox flexGrow={1}>
                <text>Src: {detail().src}</text>
                <text>Type: {detail().etypeName}</text>
                <text>Dst: {detail().dst}</text>
                <text bold>Props</text>
                <For each={detail().props}>
                  {(prop) => <text>{prop.key}: {prop.value}</text>}
                </For>
                <Show when={detail().props.length === 0}>
                  <text fg="yellow">No props</text>
                </Show>
              </scrollbox>
            )}
          </Show>

          <Show when={activeTab() !== "nodes" && activeTab() !== "edges"}>
            <text fg="yellow">Select Nodes or Edges to inspect details</text>
          </Show>
        </box>
      </box>

      <box borderStyle="rounded" padding={1}>
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

      <Show when={showUnlockConfirm()}>
        <Portal>
          <box
            position="absolute"
            top={4}
            left={8}
            width={50}
            borderStyle="double"
            padding={1}
            bg="black"
          >
            <text bold fg="yellow">Unlock write mode?</text>
            <text>Reopen the database with write access.</text>
            <text>Press y to confirm, n to cancel.</text>
          </box>
        </Portal>
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
  const display = props.value || props.placeholder || "(empty)";
  return (
    <box flexDirection="row" gap={1}>
      <text>{props.label}:</text>
      <text bg={props.active ? "blue" : undefined} fg={props.active ? "white" : "gray"}>
        {display}
      </text>
    </box>
  );
}
