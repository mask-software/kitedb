/**
 * Wave-3 reproductions for the playground client (finding C10).
 *
 * C10a: rejections from getStatus (the initial status check), findPath and analyzeImpact are never
 *       caught, so a failed request becomes an unhandled promise rejection.
 * C10b: when an older path/impact request resolves after a newer one, its result replaces the
 *       newer one's and the graph highlights stale nodes.
 * C10c: a data refresh while the fcose layout is still running adds the new elements without laying
 *       them out, so they stack at the origin.
 * C10d: opening another database keeps the previous one's selection and path/impact highlights,
 *       and a request still pending for the previous database highlights its result afterwards.
 * C10e: when the initial status check resolves after the user has opened a database, its stale
 *       "disconnected" answer overwrites the connected state.
 *
 * React runs in happy-dom. App's child components are stubs that record their props, and cytoscape
 * runs headless. happy-dom's globals are removed again after this file.
 */

import { GlobalRegistrator } from "@happy-dom/global-registrator";
import { afterAll, afterEach, describe, expect, mock, test } from "bun:test";

GlobalRegistrator.register();
(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

// --- Stubs for App's children: each records the props of its latest render. ---

type Props = Record<string, any>;
const latestProps: Record<string, Props> = {};

function recordingStub(name: string) {
  return (props: Props) => {
    latestProps[name] = props;
    return null;
  };
}

mock.module("./components/graph-canvas.tsx", () => ({ GraphCanvas: recordingStub("GraphCanvas") }));
mock.module("./components/header.tsx", () => ({ Header: recordingStub("Header") }));
mock.module("./components/sidebar.tsx", () => ({ Sidebar: recordingStub("Sidebar") }));
mock.module("./components/status-bar.tsx", () => ({ StatusBar: recordingStub("StatusBar") }));
mock.module("./components/toolbar.tsx", () => ({ Toolbar: recordingStub("Toolbar") }));

// --- Headless cytoscape: happy-dom has no canvas for the default renderer. ---

// The CommonJS build: a module instance of its own, untouched by the mock below.
const realCytoscape = require("cytoscape") as any;
function headlessCytoscape(options: Props = {}) {
  const { container: _container, ...rest } = options;
  return realCytoscape({ ...rest, headless: true, styleEnabled: true });
}
headlessCytoscape.use = (extension: unknown) => realCytoscape.use(extension);
mock.module("cytoscape", () => ({ default: headlessCytoscape }));

const React = await import("react");
const { act } = React;
const { createRoot } = await import("react-dom/client");
const { App } = await import("./app.tsx");
const { useCytoscape } = await import("./hooks/use-cytoscape.ts");

// --- fetch: each test routes API calls to its own handler. ---

type Route = (body: any) => Promise<unknown>;
let routes: Record<string, Route> = {};

globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
  const url = String(input);
  const route = routes[url];
  if (!route) {
    throw new Error(`unexpected fetch ${url}`);
  }
  const body = init?.body ? JSON.parse(String(init.body)) : undefined;
  const result = await route(body);
  return new Response(JSON.stringify(result), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}) as typeof fetch;

interface Deferred<T> {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (error: unknown) => void;
}

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

// --- Unhandled rejections raised while a test runs. ---

const unhandled: unknown[] = [];
const onUnhandled = (reason: unknown) => {
  unhandled.push(reason);
};
process.on("unhandledRejection", onUnhandled);

const roots: Array<ReturnType<typeof createRoot>> = [];

afterEach(async () => {
  for (const root of roots.splice(0)) {
    await act(async () => root.unmount());
  }
  routes = {};
  unhandled.length = 0;
  for (const key of Object.keys(latestProps)) {
    delete latestProps[key];
  }
});

afterAll(async () => {
  process.off("unhandledRejection", onUnhandled);
  await GlobalRegistrator.unregister();
});

/** Let pending promise callbacks, effects and timers run. */
async function flush(ms = 10): Promise<void> {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, ms));
  });
}

async function render(element: React.ReactElement): Promise<void> {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  roots.push(root);
  await act(async () => root.render(element));
  await flush();
}

const NODES = ["a", "b", "c", "d"].map((id) => ({ id, label: id, type: "file", degree: 1 }));
const node = (id: string) => NODES.find((n) => n.id === id)!;

/** Routes for a connected database holding NODES. */
function connectedRoutes(): Record<string, Route> {
  return {
    "/api/status": async () => ({ connected: true, path: "w3.kitedb", nodeCount: 4, edgeCount: 0 }),
    "/api/graph/network": async () => ({ nodes: NODES, edges: [], truncated: false }),
  };
}

/**
 * Click a node. Cytoscape's tap handler drops the promise onNodeClick returns, so a rejection there
 * is unhandled; `settled` reports "ok" or the rejection instead of letting it escape.
 */
async function clickNode(id: string): Promise<{ settled: Promise<string> }> {
  let settled: Promise<string> = Promise.resolve("ok");
  await act(async () => {
    const result = latestProps.GraphCanvas.onNodeClick(node(id));
    settled = Promise.resolve(result).then(
      () => "ok",
      (error) => `rejected: ${error}`,
    );
  });
  await flush();
  return { settled };
}

async function setToolMode(mode: string): Promise<void> {
  await act(async () => latestProps.Toolbar.onToolModeChange(mode));
  await flush();
}

const sorted = (set: Set<string>) => [...set].sort();

describe("C10a: failed requests are handled", () => {
  test("C10a: a failed findPath is not an unhandled rejection", async () => {
    routes = {
      ...connectedRoutes(),
      "/api/graph/path": async () => {
        throw new TypeError("Failed to fetch");
      },
    };
    await render(<App />);
    await setToolMode("path");
    await clickNode("a");
    const click = await clickNode("b");

    expect(await click.settled).toBe("ok");
    expect(unhandled).toEqual([]);
    expect(sorted(latestProps.GraphCanvas.pathNodes)).toEqual([]);
  });

  test("C10a: a failed analyzeImpact is not an unhandled rejection", async () => {
    routes = {
      ...connectedRoutes(),
      "/api/graph/impact": async () => {
        throw new TypeError("Failed to fetch");
      },
    };
    await render(<App />);
    await setToolMode("impact");
    const click = await clickNode("a");

    expect(await click.settled).toBe("ok");
    expect(unhandled).toEqual([]);
    expect(sorted(latestProps.GraphCanvas.impactedNodes)).toEqual([]);
  });
});

describe("C10b: stale responses are ignored", () => {
  test("C10b: an older impact response that arrives last does not replace the newer one", async () => {
    const pending: Record<string, Deferred<unknown>> = { a: deferred(), b: deferred() };
    routes = {
      ...connectedRoutes(),
      "/api/graph/impact": (body: { nodeKey: string }) => pending[body.nodeKey]!.promise,
    };
    await render(<App />);
    await setToolMode("impact");
    await clickNode("a");
    await clickNode("b");

    // b's answer arrives first, then a's (stale) answer.
    await act(async () => pending.b!.resolve({ impacted: ["c"], edges: [] }));
    await flush();
    await act(async () => pending.a!.resolve({ impacted: ["b", "d"], edges: [] }));
    await flush();
    expect(unhandled).toEqual([]);

    expect(latestProps.GraphCanvas.impactSource?.id).toBe("b");
    expect(sorted(latestProps.GraphCanvas.impactedNodes)).toEqual(["c"]);
  });

  test("C10b: an older path response that arrives last does not replace the newer one", async () => {
    const pending: Record<string, Deferred<unknown>> = { "a>b": deferred(), "c>d": deferred() };
    routes = {
      ...connectedRoutes(),
      "/api/graph/path": (body: { startKey: string; endKey: string }) =>
        pending[`${body.startKey}>${body.endKey}`]!.promise,
    };
    await render(<App />);
    await setToolMode("path");
    await clickNode("a");
    await clickNode("b"); // path a -> b requested
    await clickNode("c"); // new start
    await clickNode("d"); // path c -> d requested

    await act(async () => pending["c>d"]!.resolve({ path: ["c", "d"], edges: [] }));
    await flush();
    await act(async () => pending["a>b"]!.resolve({ path: ["a", "b"], edges: [] }));
    await flush();
    expect(unhandled).toEqual([]);

    expect(latestProps.GraphCanvas.pathStart?.id).toBe("c");
    expect(latestProps.GraphCanvas.pathEnd?.id).toBe("d");
    expect(latestProps.Sidebar.pathSequence).toEqual(["c", "d"]);
    expect(sorted(latestProps.GraphCanvas.pathNodes)).toEqual(["c", "d"]);
  });
});

describe("C10c: graph layout", () => {
  test("C10c: nodes added while the layout is running get laid out too", async () => {
    let cyRef: { current: any } = { current: null };
    const noop = () => {};
    const empty = new Set<string>();

    function Harness(props: { nodes: typeof NODES; edges: Array<{ source: string; target: string; type: string }> }) {
      const containerRef = React.useRef<HTMLDivElement | null>(null);
      const handle = useCytoscape({
        containerRef,
        nodes: props.nodes,
        edges: props.edges,
        toolMode: "select",
        selectedNode: null,
        pathNodes: empty,
        pathStart: null,
        pathEnd: null,
        impactSource: null,
        impactedNodes: empty,
        searchHighlightedNodes: empty,
        showLabels: true,
        onNodeClick: noop,
        onNodeHover: noop,
        onZoomChange: noop,
      });
      cyRef = handle.cyRef;
      return <div ref={containerRef} />;
    }

    const container = document.createElement("div");
    document.body.appendChild(container);
    const root = createRoot(container);
    roots.push(root);

    const first = NODES.slice(0, 2);
    await act(async () => root.render(<Harness nodes={first} edges={[{ source: "a", target: "b", type: "calls" }]} />));
    // The refresh lands while the first (animated) layout is still running.
    const edges = [
      { source: "a", target: "b", type: "calls" },
      { source: "c", target: "d", type: "calls" },
    ];
    await act(async () => root.render(<Harness nodes={NODES} edges={edges} />));
    // Let every layout animation finish (fcose animates for 500ms).
    await flush(1500);

    const cy = cyRef.current;
    expect(cy).not.toBeNull();
    const positions = cy.nodes().map((n: any) => {
      const { x, y } = n.position();
      return `${Math.round(x)},${Math.round(y)}`;
    });
    expect(positions.length).toBe(4);
    // Laid-out nodes are spread out; nodes that were never laid out all sit at the origin.
    expect(new Set(positions).size).toBe(4);
  });
});

describe("C10d/C10e: switching databases", () => {
  test("C10d: opening another database clears the selection and highlights, even from a pending request", async () => {
    const pendingImpact = deferred<unknown>();
    routes = {
      ...connectedRoutes(),
      "/api/graph/impact": () => pendingImpact.promise,
      "/api/db/demo": async () => ({ success: true }),
    };
    await render(<App />);
    await clickNode("a"); // select mode: selects a
    await setToolMode("impact");
    await clickNode("b"); // impact request for b, still pending
    expect(latestProps.GraphCanvas.selectedNode?.id).toBe("a");

    await act(async () => {
      await latestProps.Header.onCreateDemo();
    });
    await flush();
    await act(async () => pendingImpact.resolve({ impacted: ["c"], edges: [] }));
    await flush();

    expect(latestProps.Header.dbPath).toBe("demo.kitedb");
    expect(latestProps.GraphCanvas.selectedNode).toBeNull();
    expect(latestProps.GraphCanvas.impactSource).toBeNull();
    expect(sorted(latestProps.GraphCanvas.impactedNodes)).toEqual([]);
  });

  test("C10e: a late initial status check does not undo a database opened meanwhile", async () => {
    const initialStatus = deferred<unknown>();
    let statusCalls = 0;
    routes = {
      ...connectedRoutes(),
      "/api/status": () => {
        statusCalls++;
        return statusCalls === 1
          ? initialStatus.promise
          : Promise.resolve({ connected: true, path: "demo.kitedb", isDemo: true, nodeCount: 4, edgeCount: 0 });
      },
      "/api/db/demo": async () => ({ success: true }),
    };
    await render(<App />);
    await act(async () => {
      await latestProps.Header.onCreateDemo();
    });
    await flush();
    expect(latestProps.Header.connected).toBe(true);

    // The first status request answers last, from before the demo was opened.
    await act(async () => initialStatus.resolve({ connected: false }));
    await flush();

    expect(latestProps.Header.connected).toBe(true);
    expect(latestProps.Header.dbPath).toBe("demo.kitedb");
    expect(latestProps.Header.isDemo).toBe(true);
  });
});

// Last in the file: before the fix, bun fails this test the moment the rejection goes unhandled and
// starts the next test while this one is still running, which would disturb any test after it.
describe("C10a: failed status check", () => {
  test("C10a: a failed initial status check is not an unhandled rejection", async () => {
    routes = {
      "/api/status": async () => {
        throw new TypeError("Failed to fetch");
      },
    };
    await render(<App />);
    await flush(20);

    expect(unhandled).toEqual([]);
    expect(latestProps.Header.connected).toBe(false);
  });
});
