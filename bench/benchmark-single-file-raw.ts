/**
 * Single-file Raw Benchmark (TS)
 *
 * Benchmarks low-level GraphDB operations against the single-file .raydb format.
 * Intended for apples-to-apples comparison with a pure Rust single-file benchmark.
 *
 * Usage:
 *   bun run bench/benchmark-single-file-raw.ts [options]
 *
 * Options:
 *   --nodes N              Number of nodes (default: 10000)
 *   --edges M              Number of edges (default: 50000)
 *   --iterations I         Iterations for latency benchmarks (default: 10000)
 *   --output FILE          Output file path (default: bench/results/benchmark-single-file-raw-<timestamp>.txt)
 *   --no-output            Disable file output
 *   --keep-db              Keep the database file after benchmark (prints path)
 *   --wal-size BYTES       WAL size in bytes (default: 67108864)
 *   --checkpoint-threshold P  Auto-checkpoint threshold (default: 0.8)
 *   --no-auto-checkpoint   Disable auto-checkpoint
 */

import { mkdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";

import {
  type GraphDB,
  type NodeID,
  addEdge,
  beginTx,
  closeGraphDB,
  commit,
  createNode,
  defineEtype,
  edgeExists,
  getNeighborsOut,
  getNodeByKey,
  openGraphDB,
  optimizeSingleFile,
} from "../src/index.ts";

// =============================================================================
// Configuration
// =============================================================================

interface BenchConfig {
  nodes: number;
  edges: number;
  iterations: number;
  outputFile: string | null;
  keepDb: boolean;
  walSize: number;
  checkpointThreshold: number;
  autoCheckpoint: boolean;
  skipCompact: boolean;
  reopenReadOnly: boolean;
  cacheEnabled: boolean;
}

function parseArgs(): BenchConfig {
  const args = process.argv.slice(2);

  const now = new Date();
  const timestamp = now.toISOString().replace(/[:.]/g, "-").slice(0, 19);
  const defaultOutput = join(
    dirname(import.meta.path),
    "results",
    `benchmark-single-file-raw-${timestamp}.txt`,
  );

  const config: BenchConfig = {
    nodes: 10000,
    edges: 50000,
    iterations: 10000,
    outputFile: defaultOutput,
    keepDb: false,
    walSize: 64 * 1024 * 1024,
    checkpointThreshold: 0.8,
    autoCheckpoint: true,
    skipCompact: false,
    reopenReadOnly: false,
    cacheEnabled: false,
  };

  for (let i = 0; i < args.length; i++) {
    switch (args[i]) {
      case "--nodes":
        config.nodes = Number.parseInt(args[++i] || "10000", 10);
        break;
      case "--edges":
        config.edges = Number.parseInt(args[++i] || "50000", 10);
        break;
      case "--iterations":
        config.iterations = Number.parseInt(args[++i] || "10000", 10);
        break;
      case "--output":
        config.outputFile = args[++i] || null;
        break;
      case "--no-output":
        config.outputFile = null;
        break;
      case "--keep-db":
        config.keepDb = true;
        break;
      case "--wal-size":
        config.walSize = Number.parseInt(args[++i] || `${64 * 1024 * 1024}`, 10);
        break;
      case "--checkpoint-threshold":
        config.checkpointThreshold = Number.parseFloat(args[++i] || "0.8");
        break;
      case "--no-auto-checkpoint":
        config.autoCheckpoint = false;
        break;
      case "--cache-enabled":
        config.cacheEnabled = true;
        break;
      case "--skip-compact":
        config.skipCompact = true;
        break;
      case "--reopen-readonly":
        config.reopenReadOnly = true;
        break;
    }
  }

  return config;
}

// =============================================================================
// Output Logger
// =============================================================================

class Logger {
  private outputFile: string | null;
  private buffer: string[] = [];

  constructor(outputFile: string | null) {
    this.outputFile = outputFile;
  }

  log(message = ""): void {
    console.log(message);
    this.buffer.push(message);
  }

  async flush(): Promise<void> {
    if (this.outputFile && this.buffer.length > 0) {
      await mkdir(dirname(this.outputFile), { recursive: true });
      await writeFile(this.outputFile, `${this.buffer.join("\n")}\n`);
    }
  }

  getOutputPath(): string | null {
    return this.outputFile;
  }
}

let logger: Logger;

// =============================================================================
// Latency Tracking
// =============================================================================

interface LatencyStats {
  count: number;
  min: number;
  max: number;
  sum: number;
  p50: number;
  p95: number;
  p99: number;
}

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
}

function formatLatency(ns: number): string {
  if (ns < 1000) return `${ns.toFixed(0)}ns`;
  if (ns < 1_000_000) return `${(ns / 1000).toFixed(2)}us`;
  return `${(ns / 1_000_000).toFixed(2)}ms`;
}

function formatNumber(n: number): string {
  return n.toLocaleString("en-US");
}

function printLatencyTable(name: string, stats: LatencyStats): void {
  const opsPerSec = stats.sum > 0 ? stats.count / (stats.sum / 1_000_000_000) : 0;
  logger.log(
    `${name.padEnd(45)} p50=${formatLatency(stats.p50).padStart(10)} p95=${formatLatency(stats.p95).padStart(10)} p99=${formatLatency(stats.p99).padStart(10)} max=${formatLatency(stats.max).padStart(10)} (${formatNumber(Math.round(opsPerSec))} ops/sec)`,
  );
}

// =============================================================================
// Graph Build
// =============================================================================

interface GraphData {
  nodeIds: NodeID[];
  nodeKeys: string[];
  etypes: {
    calls: number;
  };
}

async function buildGraph(db: GraphDB, config: BenchConfig): Promise<GraphData> {
  const nodeIds: NodeID[] = [];
  const nodeKeys: string[] = [];
  const batchSize = 5000;
  let etypes: GraphData["etypes"] | undefined;

  logger.log("  Creating nodes...");
  for (let batch = 0; batch < config.nodes; batch += batchSize) {
    const tx = beginTx(db);

    if (batch === 0) {
      etypes = { calls: defineEtype(tx, "CALLS") };
    }

    const end = Math.min(batch + batchSize, config.nodes);
    for (let i = batch; i < end; i++) {
      const key = `pkg.module${Math.floor(i / 100)}.Class${i % 100}`;
      const nodeId = createNode(tx, { key });
      nodeIds.push(nodeId);
      nodeKeys.push(key);
    }
    await commit(tx);
    process.stdout.write(`\r  Created ${end} / ${config.nodes} nodes`);
  }
  console.log();

  logger.log("  Creating edges...");
  let edgesCreated = 0;
  let attempts = 0;
  const maxAttempts = config.edges * 3;

  while (edgesCreated < config.edges && attempts < maxAttempts) {
    const tx = beginTx(db);
    const batchTarget = Math.min(edgesCreated + batchSize, config.edges);

    while (edgesCreated < batchTarget && attempts < maxAttempts) {
      attempts++;
      const src = nodeIds[Math.floor(Math.random() * nodeIds.length)]!;
      const dst = nodeIds[Math.floor(Math.random() * nodeIds.length)]!;
      if (src !== dst) {
        addEdge(tx, src, etypes!.calls, dst);
        edgesCreated++;
      }
    }
    await commit(tx);
    process.stdout.write(`\r  Created ${edgesCreated} / ${config.edges} edges`);
  }
  console.log();

  return {
    nodeIds,
    nodeKeys,
    etypes: etypes!,
  };
}

// =============================================================================
// Benchmarks
// =============================================================================

function benchmarkKeyLookups(db: GraphDB, graph: GraphData, iterations: number): void {
  logger.log("\n--- Key Lookups (getNodeByKey) ---");

  const tracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const key = graph.nodeKeys[Math.floor(Math.random() * graph.nodeKeys.length)]!;
    const start = Bun.nanoseconds();
    getNodeByKey(db, key);
    tracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable("Random existing keys", tracker.getStats());
}

function benchmarkTraversals(db: GraphDB, graph: GraphData, iterations: number): void {
  logger.log("\n--- 1-Hop Traversals (out) ---");

  const tracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const node = graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
    const start = Bun.nanoseconds();
    let count = 0;
    for (const _ of getNeighborsOut(db, node)) count++;
    tracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable("Random nodes", tracker.getStats());
}

function benchmarkEdgeExists(db: GraphDB, graph: GraphData, iterations: number): void {
  logger.log("\n--- Edge Exists ---");

  const tracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const src = graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
    const dst = graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
    const start = Bun.nanoseconds();
    edgeExists(db, src, graph.etypes.calls, dst);
    tracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable("Random edge exists", tracker.getStats());
}

async function benchmarkWrites(db: GraphDB, iterations: number): Promise<void> {
  logger.log("\n--- Batch Writes (100 nodes) ---");

  const batchSize = 100;
  const batches = Math.min(Math.floor(iterations / batchSize), 50);
  const tracker = new LatencyTracker();

  for (let b = 0; b < batches; b++) {
    const start = Bun.nanoseconds();
    const tx = beginTx(db);
    for (let i = 0; i < batchSize; i++) {
      createNode(tx, { key: `bench:raw:${b}:${i}` });
    }
    await commit(tx);
    tracker.record(Bun.nanoseconds() - start);
  }

  printLatencyTable("Batch of 100 nodes", tracker.getStats());
}

// =============================================================================
// Main
// =============================================================================

async function runBenchmarks(config: BenchConfig): Promise<void> {
  logger = new Logger(config.outputFile);

  const now = new Date();
  logger.log("=".repeat(120));
  logger.log("Single-file Raw Benchmark (TS)");
  logger.log("=".repeat(120));
  logger.log(`Date: ${now.toISOString()}`);
  logger.log(`Nodes: ${formatNumber(config.nodes)}`);
  logger.log(`Edges: ${formatNumber(config.edges)}`);
  logger.log(`Iterations: ${formatNumber(config.iterations)}`);
  logger.log(`WAL size: ${formatNumber(config.walSize)} bytes`);
  logger.log(`Auto-checkpoint: ${config.autoCheckpoint}`);
  logger.log(`Checkpoint threshold: ${config.checkpointThreshold}`);
  logger.log(`Skip compact: ${config.skipCompact}`);
  logger.log(`Reopen read-only: ${config.reopenReadOnly}`);
  logger.log(`Cache enabled: ${config.cacheEnabled}`);
  logger.log("=".repeat(120));

  const testPath = join(tmpdir(), `ray-bench-raw-${Date.now()}.raydb`);

  try {
    logger.log("\n[1/5] Building graph...");
    let db = await openGraphDB(testPath, {
      autoCheckpoint: config.autoCheckpoint,
      checkpointThreshold: config.checkpointThreshold,
      walSize: config.walSize,
      cache: {
        enabled: config.cacheEnabled,
      },
    });
    const startBuild = performance.now();
    const graph = await buildGraph(db, config);
    logger.log(`  Built in ${(performance.now() - startBuild).toFixed(0)}ms`);

    logger.log("\n[2/5] Compacting...");
    if (config.skipCompact) {
      logger.log("  Skipped compaction");
    } else {
      const startCompact = performance.now();
      await optimizeSingleFile(db);
      logger.log(`  Compacted in ${(performance.now() - startCompact).toFixed(0)}ms`);
    }

    if (config.reopenReadOnly) {
      await closeGraphDB(db);
      db = await openGraphDB(testPath, {
        readOnly: true,
        createIfMissing: false,
        cache: {
          enabled: config.cacheEnabled,
        },
      });
      logger.log("  Re-opened database in read-only mode");
    }

    logger.log("\n[3/5] Key lookup benchmarks...");
    benchmarkKeyLookups(db, graph, config.iterations);

    logger.log("\n[4/5] Traversal and edge benchmarks...");
    benchmarkTraversals(db, graph, config.iterations);
    benchmarkEdgeExists(db, graph, config.iterations);

    logger.log("\n[5/5] Write benchmarks...");
    if (db.readOnly) {
      logger.log("  Skipped write benchmarks (read-only)");
    } else {
      await benchmarkWrites(db, config.iterations);
    }

    await closeGraphDB(db);
  } finally {
    if (config.keepDb) {
      logger.log(`\nDatabase preserved at: ${testPath}`);
    } else {
      await rm(testPath, { force: true });
    }
  }

  logger.log(`\n${"=".repeat(120)}`);
  logger.log("Benchmark complete.");
  logger.log("=".repeat(120));

  await logger.flush();
  if (logger.getOutputPath()) {
    console.log(`\nResults saved to: ${logger.getOutputPath()}`);
  }
}

const config = parseArgs();
runBenchmarks(config).catch((err) => {
  console.error("Benchmark failed:", err);
  process.exit(1);
});
