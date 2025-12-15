/**
 * MVCC Benchmark V2 - Comprehensive MVCC performance validation
 *
 * Addresses limitations in benchmark-mvcc.ts:
 * 1. Subprocess isolation to eliminate JIT/cache warmup bias
 * 2. Contested reads with active writer transactions (not auto-commit fast-paths)
 * 3. Scale testing beyond L3 cache (configurable up to 1M+ nodes)
 * 4. Version chain depth stress testing with GC control
 *
 * Usage:
 *   bun run bench/benchmark-mvcc-v2.ts [options]
 *
 * Options:
 *   --nodes N           Base node count (default: 10000)
 *   --scale-nodes N     Scale test node count (default: 1000000)
 *   --iterations I      Iterations per benchmark (default: 10000)
 *   --scenarios S       Comma-separated: contested,scale,version-chain,warm (default: all)
 *   --output FILE       Output file path
 *   --no-output         Disable file output
 *   --subprocess        Internal: run as isolated subprocess
 *   --mode MODE         Internal: baseline or mvcc (for subprocess)
 *   --scenario NAME     Internal: specific scenario to run (for subprocess)
 */

import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";

import {
  type GraphDB,
  type NodeID,
  type TxHandle,
  addEdge,
  beginTx,
  closeGraphDB,
  commit,
  createNode,
  defineEtype,
  definePropkey,
  edgeExists,
  getNeighborsOut,
  getNodeByKey,
  openGraphDB,
  optimize,
  rollback,
  setNodeProp,
  stats,
} from "../src/index.ts";
import { PropValueTag } from "../src/types.ts";

// =============================================================================
// Types
// =============================================================================

interface BenchConfig {
  nodes: number;
  scaleNodes: number;
  iterations: number;
  scenarios: string[];
  outputFile: string | null;
  // Subprocess-specific
  isSubprocess: boolean;
  mode: "baseline" | "mvcc" | null;
  scenario: string | null;
}

interface LatencyStats {
  count: number;
  min: number;
  max: number;
  sum: number;
  p50: number;
  p95: number;
  p99: number;
}

interface ScenarioResult {
  name: string;
  metrics: Record<string, LatencyStats>;
  metadata?: Record<string, number | string>;
}

interface BenchmarkResult {
  mode: "baseline" | "mvcc";
  scenario: string;
  result: ScenarioResult;
}

interface GraphData {
  nodeIds: NodeID[];
  nodeKeys: string[];
  etypes: { calls: number };
  propkeys: { counter: number };
}

// =============================================================================
// Configuration
// =============================================================================

const ALL_SCENARIOS = ["warm", "contested", "scale", "version-chain"];

function parseArgs(): BenchConfig {
  const args = process.argv.slice(2);

  const now = new Date();
  const timestamp = now.toISOString().replace(/[:.]/g, "-").slice(0, 19);
  const defaultOutput = join(
    dirname(import.meta.path),
    "results",
    `benchmark-mvcc-v2-${timestamp}.txt`,
  );

  const config: BenchConfig = {
    nodes: 10000,
    scaleNodes: 1000000,
    iterations: 10000,
    scenarios: [...ALL_SCENARIOS],
    outputFile: defaultOutput,
    isSubprocess: false,
    mode: null,
    scenario: null,
  };

  for (let i = 0; i < args.length; i++) {
    switch (args[i]) {
      case "--nodes":
        config.nodes = Number.parseInt(args[++i] || "10000", 10);
        break;
      case "--scale-nodes":
        config.scaleNodes = Number.parseInt(args[++i] || "1000000", 10);
        break;
      case "--iterations":
        config.iterations = Number.parseInt(args[++i] || "10000", 10);
        break;
      case "--scenarios":
        config.scenarios = (args[++i] || "").split(",").filter(Boolean);
        break;
      case "--output":
        config.outputFile = args[++i] || null;
        break;
      case "--no-output":
        config.outputFile = null;
        break;
      case "--subprocess":
        config.isSubprocess = true;
        break;
      case "--mode":
        config.mode = args[++i] as "baseline" | "mvcc";
        break;
      case "--scenario":
        config.scenario = args[++i] || null;
        break;
    }
  }

  return config;
}

// =============================================================================
// Latency Tracking
// =============================================================================

class LatencyTracker {
  private samples: number[] = [];

  record(latencyNs: number): void {
    this.samples.push(latencyNs);
  }

  getStats(): LatencyStats {
    if (this.samples.length === 0) {
      return { count: 0, min: 0, max: 0, sum: 0, p50: 0, p95: 0, p99: 0 };
    }

    const sorted = [...this.samples].sort((a, b) => a - b);
    const count = sorted.length;

    return {
      count,
      min: sorted[0]!,
      max: sorted[count - 1]!,
      sum: sorted.reduce((a, b) => a + b, 0),
      p50: sorted[Math.floor(count * 0.5)]!,
      p95: sorted[Math.floor(count * 0.95)]!,
      p99: sorted[Math.floor(count * 0.99)]!,
    };
  }

  clear(): void {
    this.samples = [];
  }
}

// =============================================================================
// Formatting Utilities
// =============================================================================

function formatLatency(ns: number): string {
  if (ns < 1000) return `${ns.toFixed(0)}ns`;
  if (ns < 1_000_000) return `${(ns / 1000).toFixed(2)}µs`;
  return `${(ns / 1_000_000).toFixed(2)}ms`;
}

function formatNumber(n: number): string {
  return n.toLocaleString("en-US");
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes}B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)}KB`;
  if (bytes < 1024 * 1024 * 1024)
    return `${(bytes / (1024 * 1024)).toFixed(1)}MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)}GB`;
}

// =============================================================================
// Graph Building
// =============================================================================

async function buildGraph(
  db: GraphDB,
  nodeCount: number,
  edgeCount: number,
  verbose = false,
): Promise<GraphData> {
  const nodeIds: NodeID[] = [];
  const nodeKeys: string[] = [];
  const batchSize = 5000;
  let etypes: { calls: number } | undefined;
  let propkeys: { counter: number } | undefined;

  for (let batch = 0; batch < nodeCount; batch += batchSize) {
    const tx = beginTx(db);

    if (batch === 0) {
      etypes = { calls: defineEtype(tx, "CALLS") };
      propkeys = { counter: definePropkey(tx, "counter") };
    }

    const end = Math.min(batch + batchSize, nodeCount);
    for (let i = batch; i < end; i++) {
      const key = `pkg.module${Math.floor(i / 100)}.Class${i % 100}`;
      const nodeId = createNode(tx, { key });
      nodeIds.push(nodeId);
      nodeKeys.push(key);
    }
    await commit(tx);
    if (verbose) {
      process.stderr.write(`\r  Created ${end} / ${nodeCount} nodes`);
    }
  }
  if (verbose) process.stderr.write("\n");

  let edgesCreated = 0;
  while (edgesCreated < edgeCount) {
    const tx = beginTx(db);
    const batchTarget = Math.min(edgesCreated + batchSize, edgeCount);

    while (edgesCreated < batchTarget) {
      const src = nodeIds[Math.floor(Math.random() * nodeIds.length)]!;
      const dst = nodeIds[Math.floor(Math.random() * nodeIds.length)]!;

      if (src !== dst) {
        addEdge(tx, src, etypes!.calls, dst);
        edgesCreated++;
      }
    }
    await commit(tx);
    if (verbose) {
      process.stderr.write(`\r  Created ${edgesCreated} / ${edgeCount} edges`);
    }
  }
  if (verbose) process.stderr.write("\n");

  return {
    nodeIds,
    nodeKeys,
    etypes: etypes!,
    propkeys: propkeys!,
  };
}

// =============================================================================
// Scenario: Warm Cache Reads
// =============================================================================

async function runWarmReadsScenario(
  db: GraphDB,
  graph: GraphData,
  iterations: number,
  mvccEnabled: boolean,
): Promise<ScenarioResult> {
  const metrics: Record<string, LatencyStats> = {};

  // Warm up cache with a few reads
  for (let i = 0; i < 100; i++) {
    const key = graph.nodeKeys[Math.floor(Math.random() * graph.nodeKeys.length)]!;
    getNodeByKey(db, key);
    const node = graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
    for (const _ of getNeighborsOut(db, node)) {
      // consume iterator
    }
  }

  // Key lookup benchmark (auto-commit mode - fast path)
  const keyLookupTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const key = graph.nodeKeys[Math.floor(Math.random() * graph.nodeKeys.length)]!;
    const start = Bun.nanoseconds();
    getNodeByKey(db, key);
    keyLookupTracker.record(Bun.nanoseconds() - start);
  }
  metrics["key_lookup_auto"] = keyLookupTracker.getStats();

  // Traversal benchmark (auto-commit mode - fast path)
  const traversalTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const node = graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
    const start = Bun.nanoseconds();
    let count = 0;
    for (const _ of getNeighborsOut(db, node)) count++;
    traversalTracker.record(Bun.nanoseconds() - start);
  }
  metrics["traversal_auto"] = traversalTracker.getStats();

  // Edge exists benchmark (auto-commit mode - fast path)
  const edgeExistsTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const src = graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
    const dst = graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
    const start = Bun.nanoseconds();
    edgeExists(db, src, graph.etypes.calls, dst);
    edgeExistsTracker.record(Bun.nanoseconds() - start);
  }
  metrics["edge_exists_auto"] = edgeExistsTracker.getStats();

  // Key lookup with transaction handle (forces visibility checks in MVCC)
  const keyLookupTxTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const key = graph.nodeKeys[Math.floor(Math.random() * graph.nodeKeys.length)]!;
    const tx = beginTx(db);
    const start = Bun.nanoseconds();
    getNodeByKey(tx, key);
    keyLookupTxTracker.record(Bun.nanoseconds() - start);
    rollback(tx);
  }
  metrics["key_lookup_tx"] = keyLookupTxTracker.getStats();

  // Traversal with transaction handle
  const traversalTxTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const node = graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
    const tx = beginTx(db);
    const start = Bun.nanoseconds();
    let count = 0;
    for (const _ of getNeighborsOut(tx, node)) count++;
    traversalTxTracker.record(Bun.nanoseconds() - start);
    rollback(tx);
  }
  metrics["traversal_tx"] = traversalTxTracker.getStats();

  return {
    name: "warm",
    metrics,
    metadata: {
      nodeCount: graph.nodeIds.length,
      iterations,
      mvccEnabled: mvccEnabled ? 1 : 0,
    },
  };
}

// =============================================================================
// Scenario: Contested Reads
// =============================================================================

interface BackgroundWriter {
  tx: TxHandle;
  nodeId: NodeID;
}

function startBackgroundWriters(
  db: GraphDB,
  count: number,
  graph: GraphData,
): BackgroundWriter[] {
  const writers: BackgroundWriter[] = [];
  for (let i = 0; i < count; i++) {
    const tx = beginTx(db);
    // Create a node to hold the transaction open with pending writes
    const nodeId = createNode(tx, { key: `writer:${i}:${Date.now()}` });
    setNodeProp(tx, nodeId, graph.propkeys.counter, {
      tag: PropValueTag.I64,
      value: BigInt(i),
    });
    writers.push({ tx, nodeId });
  }
  return writers;
}

function stopBackgroundWriters(writers: BackgroundWriter[]): void {
  for (const writer of writers) {
    rollback(writer.tx);
  }
}

async function runContestedReadsScenario(
  db: GraphDB,
  graph: GraphData,
  iterations: number,
  mvccEnabled: boolean,
): Promise<ScenarioResult> {
  const metrics: Record<string, LatencyStats> = {};
  
  // In baseline mode, we can't have concurrent transactions
  // So we only test without contention for comparison
  const writerCounts = mvccEnabled ? [0, 1, 5, 10] : [0];

  for (const writerCount of writerCounts) {
    // Start background writers that hold transactions open (only in MVCC mode)
    const writers = writerCount > 0 
      ? startBackgroundWriters(db, writerCount, graph)
      : [];

    // Traversal under contention (with transaction handle in MVCC, auto-commit in baseline)
    const traversalTracker = new LatencyTracker();
    if (mvccEnabled) {
      for (let i = 0; i < iterations; i++) {
        const node = graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
        const tx = beginTx(db);
        const start = Bun.nanoseconds();
        let count = 0;
        for (const _ of getNeighborsOut(tx, node)) count++;
        traversalTracker.record(Bun.nanoseconds() - start);
        rollback(tx);
      }
    } else {
      // Baseline: use sequential tx begin/commit to simulate the overhead
      for (let i = 0; i < iterations; i++) {
        const node = graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
        const tx = beginTx(db);
        const start = Bun.nanoseconds();
        let count = 0;
        for (const _ of getNeighborsOut(tx, node)) count++;
        traversalTracker.record(Bun.nanoseconds() - start);
        rollback(tx);
      }
    }
    metrics[`traversal_${writerCount}w`] = traversalTracker.getStats();

    // Edge exists under contention
    const edgeExistsTracker = new LatencyTracker();
    if (mvccEnabled) {
      for (let i = 0; i < iterations; i++) {
        const src = graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
        const dst = graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
        const tx = beginTx(db);
        const start = Bun.nanoseconds();
        edgeExists(tx, src, graph.etypes.calls, dst);
        edgeExistsTracker.record(Bun.nanoseconds() - start);
        rollback(tx);
      }
    } else {
      for (let i = 0; i < iterations; i++) {
        const src = graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
        const dst = graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
        const tx = beginTx(db);
        const start = Bun.nanoseconds();
        edgeExists(tx, src, graph.etypes.calls, dst);
        edgeExistsTracker.record(Bun.nanoseconds() - start);
        rollback(tx);
      }
    }
    metrics[`edge_exists_${writerCount}w`] = edgeExistsTracker.getStats();

    // Key lookup under contention
    const keyLookupTracker = new LatencyTracker();
    if (mvccEnabled) {
      for (let i = 0; i < iterations; i++) {
        const key = graph.nodeKeys[Math.floor(Math.random() * graph.nodeKeys.length)]!;
        const tx = beginTx(db);
        const start = Bun.nanoseconds();
        getNodeByKey(tx, key);
        keyLookupTracker.record(Bun.nanoseconds() - start);
        rollback(tx);
      }
    } else {
      for (let i = 0; i < iterations; i++) {
        const key = graph.nodeKeys[Math.floor(Math.random() * graph.nodeKeys.length)]!;
        const tx = beginTx(db);
        const start = Bun.nanoseconds();
        getNodeByKey(tx, key);
        keyLookupTracker.record(Bun.nanoseconds() - start);
        rollback(tx);
      }
    }
    metrics[`key_lookup_${writerCount}w`] = keyLookupTracker.getStats();

    if (writers.length > 0) {
      stopBackgroundWriters(writers);
    }
  }

  return {
    name: "contested",
    metrics,
    metadata: {
      nodeCount: graph.nodeIds.length,
      iterations,
      mvccEnabled: mvccEnabled ? 1 : 0,
    },
  };
}

// =============================================================================
// Scenario: Scale Test (beyond L3 cache)
// =============================================================================

async function runScaleScenario(
  testDir: string,
  scaleNodes: number,
  iterations: number,
  mvccEnabled: boolean,
): Promise<ScenarioResult> {
  const metrics: Record<string, LatencyStats> = {};
  const metadata: Record<string, number | string> = {};

  // Memory before
  const memBefore = process.memoryUsage();

  // Open a fresh database for scale test
  const db = await openGraphDB(testDir, { mvcc: mvccEnabled });

  // Build large graph (5 edges per node on average)
  const edgeCount = Math.floor(scaleNodes * 5);
  process.stderr.write(`  Building scale graph: ${formatNumber(scaleNodes)} nodes, ${formatNumber(edgeCount)} edges\n`);
  const startBuild = performance.now();
  const graph = await buildGraph(db, scaleNodes, edgeCount, true);
  const buildTime = performance.now() - startBuild;
  metadata["buildTimeMs"] = Math.round(buildTime);

  // Compact for realistic read performance
  process.stderr.write("  Compacting...\n");
  const startCompact = performance.now();
  await optimize(db);
  metadata["compactTimeMs"] = Math.round(performance.now() - startCompact);

  // Memory after
  const memAfter = process.memoryUsage();
  metadata["heapUsedMB"] = Math.round((memAfter.heapUsed - memBefore.heapUsed) / (1024 * 1024));
  metadata["rssMB"] = Math.round(memAfter.rss / (1024 * 1024));

  const dbStats = stats(db);
  metadata["snapshotNodes"] = Number(dbStats.snapshotNodes);
  metadata["snapshotEdges"] = Number(dbStats.snapshotEdges);

  // Warm up
  for (let i = 0; i < 1000; i++) {
    const node = graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
    for (const _ of getNeighborsOut(db, node)) {
      // consume
    }
  }

  // Traversal benchmark at scale (auto-commit)
  const traversalAutoTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const node = graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
    const start = Bun.nanoseconds();
    let count = 0;
    for (const _ of getNeighborsOut(db, node)) count++;
    traversalAutoTracker.record(Bun.nanoseconds() - start);
  }
  metrics["traversal_auto"] = traversalAutoTracker.getStats();

  // Traversal with transaction handle
  const traversalTxTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const node = graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
    const tx = beginTx(db);
    const start = Bun.nanoseconds();
    let count = 0;
    for (const _ of getNeighborsOut(tx, node)) count++;
    traversalTxTracker.record(Bun.nanoseconds() - start);
    rollback(tx);
  }
  metrics["traversal_tx"] = traversalTxTracker.getStats();

  // Key lookup at scale
  const keyLookupTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const key = graph.nodeKeys[Math.floor(Math.random() * graph.nodeKeys.length)]!;
    const start = Bun.nanoseconds();
    getNodeByKey(db, key);
    keyLookupTracker.record(Bun.nanoseconds() - start);
  }
  metrics["key_lookup_auto"] = keyLookupTracker.getStats();

  // Edge exists at scale
  const edgeExistsTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const src = graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
    const dst = graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
    const start = Bun.nanoseconds();
    edgeExists(db, src, graph.etypes.calls, dst);
    edgeExistsTracker.record(Bun.nanoseconds() - start);
  }
  metrics["edge_exists_auto"] = edgeExistsTracker.getStats();

  await closeGraphDB(db);

  return {
    name: "scale",
    metrics,
    metadata,
  };
}

// =============================================================================
// Scenario: Version Chain Depth
// =============================================================================

async function runVersionChainScenario(
  testDir: string,
  iterations: number,
  mvccEnabled: boolean,
): Promise<ScenarioResult> {
  const metrics: Record<string, LatencyStats> = {};
  const metadata: Record<string, number | string> = {};

  // Configuration: fewer nodes, many updates per node
  const nodeCount = 1000;
  const chainDepths = [1, 10, 50, 100];

  // Open database with GC disabled (very long retention)
  const db = await openGraphDB(testDir, {
    mvcc: mvccEnabled,
    mvccGcInterval: 999999999, // Effectively disabled
    mvccRetentionMs: 999999999,
  });

  // Build initial graph
  process.stderr.write(`  Building base graph: ${nodeCount} nodes\n`);
  const graph = await buildGraph(db, nodeCount, nodeCount * 2, false);

  await optimize(db);

  // For each depth, update nodes and measure read latency
  let currentDepth = 0;
  for (const targetDepth of chainDepths) {
    const updatesToAdd = targetDepth - currentDepth;
    process.stderr.write(`  Building version chain depth ${targetDepth}...\n`);

    // Add updates to build version chain depth
    for (let u = 0; u < updatesToAdd; u++) {
      for (let n = 0; n < nodeCount; n++) {
        const tx = beginTx(db);
        setNodeProp(tx, graph.nodeIds[n]!, graph.propkeys.counter, {
          tag: PropValueTag.I64,
          value: BigInt(currentDepth + u),
        });
        await commit(tx);
      }
    }
    currentDepth = targetDepth;

    // Measure read latency at this chain depth
    // Use transaction handle to force visibility checks
    const readTracker = new LatencyTracker();
    for (let i = 0; i < Math.min(iterations, 5000); i++) {
      const node = graph.nodeIds[Math.floor(Math.random() * nodeCount)]!;
      const tx = beginTx(db);
      const start = Bun.nanoseconds();
      for (const _ of getNeighborsOut(tx, node)) {
        // consume
      }
      readTracker.record(Bun.nanoseconds() - start);
      rollback(tx);
    }
    metrics[`traversal_depth_${targetDepth}`] = readTracker.getStats();
  }

  // Get final MVCC stats if available
  const dbStats = stats(db);
  if (dbStats.mvccStats) {
    metadata["versionsPruned"] = Number(dbStats.mvccStats.versionsPruned);
    metadata["gcRuns"] = dbStats.mvccStats.gcRuns;
  }
  metadata["finalChainDepth"] = currentDepth;
  metadata["nodeCount"] = nodeCount;

  await closeGraphDB(db);

  return {
    name: "version-chain",
    metrics,
    metadata,
  };
}

// =============================================================================
// Subprocess Runner
// =============================================================================

async function runInSubprocess(
  mode: "baseline" | "mvcc",
  scenario: string,
  config: BenchConfig,
): Promise<BenchmarkResult> {
  const args = [
    "run",
    "bench/benchmark-mvcc-v2.ts",
    "--subprocess",
    "--mode",
    mode,
    "--scenario",
    scenario,
    "--nodes",
    String(config.nodes),
    "--scale-nodes",
    String(config.scaleNodes),
    "--iterations",
    String(config.iterations),
    "--no-output",
  ];

  const proc = Bun.spawn(["bun", ...args], {
    stdout: "pipe",
    stderr: "inherit",
  });

  const output = await new Response(proc.stdout).text();
  const exitCode = await proc.exited;

  if (exitCode !== 0) {
    throw new Error(`Subprocess failed with exit code ${exitCode}`);
  }

  try {
    return JSON.parse(output) as BenchmarkResult;
  } catch (e) {
    throw new Error(`Failed to parse subprocess output: ${output}`);
  }
}

// =============================================================================
// Subprocess Entry Point
// =============================================================================

async function runAsSubprocess(config: BenchConfig): Promise<void> {
  if (!config.mode || !config.scenario) {
    throw new Error("Subprocess requires --mode and --scenario");
  }

  const mvccEnabled = config.mode === "mvcc";
  const testDir = await mkdtemp(
    join(tmpdir(), `ray-bench-${config.mode}-${config.scenario}-`),
  );

  try {
    let result: ScenarioResult;

    if (config.scenario === "scale" || config.scenario === "version-chain") {
      // These scenarios manage their own database
      if (config.scenario === "scale") {
        result = await runScaleScenario(
          testDir,
          config.scaleNodes,
          config.iterations,
          mvccEnabled,
        );
      } else {
        result = await runVersionChainScenario(
          testDir,
          config.iterations,
          mvccEnabled,
        );
      }
    } else {
      // Standard scenarios use a shared base graph
      const db = await openGraphDB(testDir, { mvcc: mvccEnabled });
      const graph = await buildGraph(
        db,
        config.nodes,
        config.nodes * 5,
        false,
      );
      await optimize(db);

      if (config.scenario === "warm") {
        result = await runWarmReadsScenario(
          db,
          graph,
          config.iterations,
          mvccEnabled,
        );
      } else if (config.scenario === "contested") {
        result = await runContestedReadsScenario(
          db,
          graph,
          config.iterations,
          mvccEnabled,
        );
      } else {
        throw new Error(`Unknown scenario: ${config.scenario}`);
      }

      await closeGraphDB(db);
    }

    const output: BenchmarkResult = {
      mode: config.mode,
      scenario: config.scenario,
      result,
    };

    // Write JSON to stdout for parent process
    console.log(JSON.stringify(output));
  } finally {
    await rm(testDir, { recursive: true, force: true });
  }
}

// =============================================================================
// Reporter
// =============================================================================

class Reporter {
  private lines: string[] = [];
  private outputFile: string | null;

  constructor(outputFile: string | null) {
    this.outputFile = outputFile;
  }

  log(message = ""): void {
    console.log(message);
    this.lines.push(message);
  }

  header(title: string): void {
    this.log("=".repeat(80));
    this.log(title);
    this.log("=".repeat(80));
  }

  subheader(title: string): void {
    this.log("");
    this.log("-".repeat(80));
    this.log(title);
    this.log("-".repeat(80));
  }

  compareMetric(
    name: string,
    baseline: LatencyStats,
    mvcc: LatencyStats,
  ): void {
    const overheadP50 =
      baseline.p50 > 0
        ? ((mvcc.p50 - baseline.p50) / baseline.p50) * 100
        : 0;
    const overheadP99 =
      baseline.p99 > 0
        ? ((mvcc.p99 - baseline.p99) / baseline.p99) * 100
        : 0;

    const status =
      overheadP50 <= 10 ? "OK" : overheadP50 <= 50 ? "WARN" : "HIGH";
    const statusIcon = status === "OK" ? "+" : status === "WARN" ? "!" : "X";

    this.log(
      `  ${statusIcon} ${name.padEnd(25)} ` +
        `baseline: ${formatLatency(baseline.p50).padStart(10)} ` +
        `mvcc: ${formatLatency(mvcc.p50).padStart(10)} ` +
        `overhead: ${overheadP50.toFixed(1).padStart(7)}% (p50) ` +
        `${overheadP99.toFixed(1).padStart(7)}% (p99)`,
    );
  }

  showMvccOnlyMetric(name: string, stats: LatencyStats): void {
    this.log(
      `  * ${name.padEnd(25)} ` +
        `mvcc: ${formatLatency(stats.p50).padStart(10)} (p50) ` +
        `${formatLatency(stats.p99).padStart(10)} (p99) ` +
        `${formatLatency(stats.max).padStart(10)} (max)`,
    );
  }

  compareResults(
    scenario: string,
    baseline: ScenarioResult,
    mvcc: ScenarioResult,
  ): void {
    this.subheader(`${scenario.toUpperCase()} SCENARIO`);

    // Show metadata if present
    if (mvcc.metadata) {
      const metaStr = Object.entries(mvcc.metadata)
        .map(([k, v]) => `${k}=${v}`)
        .join(", ");
      this.log(`  Metadata: ${metaStr}`);
      this.log("");
    }

    // Compare all metrics
    const allMetrics = new Set([
      ...Object.keys(baseline.metrics),
      ...Object.keys(mvcc.metrics),
    ]);

    // First show comparable metrics
    for (const metric of [...allMetrics].sort()) {
      const b = baseline.metrics[metric];
      const m = mvcc.metrics[metric];
      if (b && m) {
        this.compareMetric(metric, b, m);
      }
    }

    // Then show MVCC-only metrics (e.g., contention metrics with >0 writers)
    const mvccOnlyMetrics = [...allMetrics].filter(
      (m) => !baseline.metrics[m] && mvcc.metrics[m],
    );
    if (mvccOnlyMetrics.length > 0) {
      this.log("");
      this.log("  MVCC-only metrics (contention test):");
      for (const metric of mvccOnlyMetrics.sort()) {
        this.showMvccOnlyMetric(metric, mvcc.metrics[metric]!);
      }
    }
  }

  async flush(): Promise<void> {
    if (this.outputFile && this.lines.length > 0) {
      await mkdir(dirname(this.outputFile), { recursive: true });
      await writeFile(this.outputFile, `${this.lines.join("\n")}\n`);
    }
  }

  getOutputPath(): string | null {
    return this.outputFile;
  }
}

// =============================================================================
// Main Orchestrator
// =============================================================================

async function runOrchestrator(config: BenchConfig): Promise<void> {
  const reporter = new Reporter(config.outputFile);

  const now = new Date();
  reporter.header("MVCC Benchmark V2 - Comprehensive Performance Validation");
  reporter.log(`Date: ${now.toISOString()}`);
  reporter.log(`Base nodes: ${formatNumber(config.nodes)}`);
  reporter.log(`Scale nodes: ${formatNumber(config.scaleNodes)}`);
  reporter.log(`Iterations: ${formatNumber(config.iterations)}`);
  reporter.log(`Scenarios: ${config.scenarios.join(", ")}`);
  reporter.log("");

  const results: Map<string, { baseline: ScenarioResult; mvcc: ScenarioResult }> =
    new Map();

  for (const scenario of config.scenarios) {
    if (!ALL_SCENARIOS.includes(scenario)) {
      reporter.log(`Skipping unknown scenario: ${scenario}`);
      continue;
    }

    reporter.log(`\nRunning scenario: ${scenario}`);

    // Run baseline in subprocess
    reporter.log(`  [Baseline] Starting subprocess...`);
    const baselineResult = await runInSubprocess("baseline", scenario, config);
    reporter.log(`  [Baseline] Complete`);

    // Run MVCC in subprocess
    reporter.log(`  [MVCC] Starting subprocess...`);
    const mvccResult = await runInSubprocess("mvcc", scenario, config);
    reporter.log(`  [MVCC] Complete`);

    results.set(scenario, {
      baseline: baselineResult.result,
      mvcc: mvccResult.result,
    });
  }

  // Report results
  reporter.header("RESULTS");

  for (const [scenario, { baseline, mvcc }] of results) {
    reporter.compareResults(scenario, baseline, mvcc);
  }

  reporter.log("");
  reporter.header("END OF BENCHMARK");

  await reporter.flush();
  if (reporter.getOutputPath()) {
    console.log(`\nResults saved to: ${reporter.getOutputPath()}`);
  }
}

// =============================================================================
// Main
// =============================================================================

async function main(): Promise<void> {
  const config = parseArgs();

  if (config.isSubprocess) {
    await runAsSubprocess(config);
  } else {
    await runOrchestrator(config);
  }
}

main().catch((err) => {
  console.error("Benchmark failed:", err);
  process.exit(1);
});
