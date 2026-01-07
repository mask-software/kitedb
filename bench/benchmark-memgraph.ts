/**
 * RayDB vs Memgraph Comparison Benchmark
 *
 * Runs identical workloads against both databases for fair comparison.
 *
 * Usage:
 *   bun run bench/benchmark-memgraph.ts [options]
 *
 * Options:
 *   --nodes N         Number of nodes (default: 10000)
 *   --edges M         Number of edges (default: 50000)
 *   --iterations I    Iterations for latency benchmarks (default: 10000)
 *   --scale           Run both small (10k/50k) and large (100k/1M) scales
 *   --skip-docker     Assume Memgraph is already running
 *   --output FILE     Output file path (default: bench/results/benchmark-memgraph-<timestamp>.txt)
 *   --no-output       Disable file output
 */

import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
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
  getNeighborsIn,
  getNeighborsOut,
  getNodeByKey,
  openGraphDB,
} from "../src/index.ts";

import { MemgraphClient } from "./memgraph/client.ts";
import {
  getMemgraphUri,
  startMemgraph,
  stopMemgraph,
} from "./memgraph/docker.ts";

// =============================================================================
// Configuration
// =============================================================================

interface BenchConfig {
  nodes: number;
  edges: number;
  iterations: number;
  outputFile: string | null;
  skipDocker: boolean;
  runScale: boolean;
}

interface ScaleConfig {
  name: string;
  nodes: number;
  edges: number;
  iterations: number;
}

function parseArgs(): BenchConfig {
  const args = process.argv.slice(2);

  const now = new Date();
  const timestamp = now.toISOString().replace(/[:.]/g, "-").slice(0, 19);
  const defaultOutput = join(
    dirname(import.meta.path),
    "results",
    `benchmark-memgraph-${timestamp}.txt`,
  );

  const config: BenchConfig = {
    nodes: 10000,
    edges: 50000,
    iterations: 10000,
    outputFile: defaultOutput,
    skipDocker: false,
    runScale: false,
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
      case "--skip-docker":
        config.skipDocker = true;
        break;
      case "--scale":
        config.runScale = true;
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

  clear(): void {
    this.samples = [];
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

function formatSpeedup(rayNs: number, memNs: number): string {
  if (rayNs === 0 || memNs === 0) return "N/A";
  const ratio = memNs / rayNs;
  return `${ratio.toFixed(1)}x`;
}

// =============================================================================
// Graph Data Structures
// =============================================================================

interface RayGraphData {
  nodeIds: NodeID[];
  nodeKeys: string[];
  hubNodes: NodeID[];
  etypes: {
    calls: number;
    references: number;
    imports: number;
    extends: number;
  };
}

interface MemgraphData {
  nodeIds: number[];
  nodeKeys: string[];
  hubNodes: number[];
  edgeTypes: {
    calls: string;
    references: string;
    imports: string;
    extends: string;
  };
}

// =============================================================================
// Graph Building
// =============================================================================

async function buildRayGraph(
  db: GraphDB,
  config: ScaleConfig,
): Promise<RayGraphData> {
  const nodeIds: NodeID[] = [];
  const nodeKeys: string[] = [];
  const batchSize = 5000;
  let etypes: RayGraphData["etypes"] | undefined;

  // Create nodes
  for (let batch = 0; batch < config.nodes; batch += batchSize) {
    const tx = beginTx(db);

    if (batch === 0) {
      etypes = {
        calls: defineEtype(tx, "CALLS"),
        references: defineEtype(tx, "REFERENCES"),
        imports: defineEtype(tx, "IMPORTS"),
        extends: defineEtype(tx, "EXTENDS"),
      };
    }

    const end = Math.min(batch + batchSize, config.nodes);
    for (let i = batch; i < end; i++) {
      const key = `pkg.module${Math.floor(i / 100)}.Class${i % 100}`;
      const nodeId = createNode(tx, { key });
      nodeIds.push(nodeId);
      nodeKeys.push(key);
    }
    await commit(tx);
    process.stdout.write(`\r    RayDB nodes: ${end} / ${config.nodes}`);
  }
  console.log();

  // Identify hub nodes (1%)
  const numHubs = Math.max(1, Math.floor(config.nodes * 0.01));
  const hubIndices = new Set<number>();
  while (hubIndices.size < numHubs) {
    hubIndices.add(Math.floor(Math.random() * nodeIds.length));
  }
  const hubNodes = [...hubIndices].map((i) => nodeIds[i]!);

  // Create edges
  const edgeTypes = [etypes!.calls, etypes!.references, etypes!.imports, etypes!.extends];
  const edgeTypeWeights = [0.4, 0.35, 0.15, 0.1];

  let edgesCreated = 0;
  while (edgesCreated < config.edges) {
    const tx = beginTx(db);
    const batchTarget = Math.min(edgesCreated + batchSize, config.edges);

    while (edgesCreated < batchTarget) {
      let src: NodeID;
      let dst: NodeID;

      if (Math.random() < 0.3 && hubNodes.length > 0) {
        src = hubNodes[Math.floor(Math.random() * hubNodes.length)]!;
      } else {
        src = nodeIds[Math.floor(Math.random() * nodeIds.length)]!;
      }

      if (Math.random() < 0.2 && hubNodes.length > 0) {
        dst = hubNodes[Math.floor(Math.random() * hubNodes.length)]!;
      } else {
        dst = nodeIds[Math.floor(Math.random() * nodeIds.length)]!;
      }

      if (src !== dst) {
        const r = Math.random();
        let cumulative = 0;
        let etype = edgeTypes[0]!;
        for (let j = 0; j < edgeTypes.length; j++) {
          cumulative += edgeTypeWeights[j]!;
          if (r < cumulative) {
            etype = edgeTypes[j]!;
            break;
          }
        }

        addEdge(tx, src, etype, dst);
        edgesCreated++;
      }
    }
    await commit(tx);
    process.stdout.write(`\r    RayDB edges: ${edgesCreated} / ${config.edges}`);
  }
  console.log();

  return { nodeIds, nodeKeys, hubNodes, etypes: etypes! };
}

async function buildMemgraphGraph(
  client: MemgraphClient,
  config: ScaleConfig,
): Promise<MemgraphData> {
  const nodeKeys: string[] = [];
  const batchSize = 5000;

  // Generate all keys
  for (let i = 0; i < config.nodes; i++) {
    nodeKeys.push(`pkg.module${Math.floor(i / 100)}.Class${i % 100}`);
  }

  // Create nodes in batches
  const keyToId = new Map<string, number>();
  for (let batch = 0; batch < config.nodes; batch += batchSize) {
    const end = Math.min(batch + batchSize, config.nodes);
    const batchKeys = nodeKeys.slice(batch, end);
    const batchResult = await client.createNodes(batchKeys);
    for (const [key, id] of batchResult) {
      keyToId.set(key, id);
    }
    process.stdout.write(`\r    Memgraph nodes: ${end} / ${config.nodes}`);
  }
  console.log();

  const nodeIds = nodeKeys.map((key) => keyToId.get(key)!);

  // Identify hub nodes (1%)
  const numHubs = Math.max(1, Math.floor(config.nodes * 0.01));
  const hubIndices = new Set<number>();
  while (hubIndices.size < numHubs) {
    hubIndices.add(Math.floor(Math.random() * nodeIds.length));
  }
  const hubNodes = [...hubIndices].map((i) => nodeIds[i]!);

  // Edge types
  const edgeTypes = {
    calls: "CALLS",
    references: "REFERENCES",
    imports: "IMPORTS",
    extends: "EXTENDS",
  };
  const edgeTypeNames = ["CALLS", "REFERENCES", "IMPORTS", "EXTENDS"];
  const edgeTypeWeights = [0.4, 0.35, 0.15, 0.1];

  // Create edges in batches
  let edgesCreated = 0;
  while (edgesCreated < config.edges) {
    const batchTarget = Math.min(edgesCreated + batchSize, config.edges);
    const edges: { src: number; dst: number; type: string }[] = [];

    while (edgesCreated < batchTarget) {
      let srcIdx: number;
      let dstIdx: number;

      if (Math.random() < 0.3 && hubNodes.length > 0) {
        srcIdx = [...hubIndices][Math.floor(Math.random() * hubIndices.size)]!;
      } else {
        srcIdx = Math.floor(Math.random() * nodeIds.length);
      }

      if (Math.random() < 0.2 && hubNodes.length > 0) {
        dstIdx = [...hubIndices][Math.floor(Math.random() * hubIndices.size)]!;
      } else {
        dstIdx = Math.floor(Math.random() * nodeIds.length);
      }

      if (srcIdx !== dstIdx) {
        const r = Math.random();
        let cumulative = 0;
        let edgeType = edgeTypeNames[0]!;
        for (let j = 0; j < edgeTypeNames.length; j++) {
          cumulative += edgeTypeWeights[j]!;
          if (r < cumulative) {
            edgeType = edgeTypeNames[j]!;
            break;
          }
        }

        edges.push({
          src: nodeIds[srcIdx]!,
          dst: nodeIds[dstIdx]!,
          type: edgeType,
        });
        edgesCreated++;
      }
    }

    await client.createEdges(edges);
    process.stdout.write(`\r    Memgraph edges: ${edgesCreated} / ${config.edges}`);
  }
  console.log();

  return { nodeIds, nodeKeys, hubNodes, edgeTypes };
}

// =============================================================================
// Comparison Output Helpers
// =============================================================================

function printComparisonHeader(operation: string): void {
  logger.log(`\n--- ${operation} ---`);
  logger.log(
    `${"Operation".padEnd(30)} ${"RayDB p50".padStart(15)} ${"Memgraph p50".padStart(15)} ${"Speedup".padStart(12)}`,
  );
  logger.log("-".repeat(75));
}

function printComparisonRow(
  operation: string,
  rayStats: LatencyStats,
  memStats: LatencyStats,
): void {
  const speedup = formatSpeedup(rayStats.p50, memStats.p50);
  logger.log(
    `${operation.padEnd(30)} ${formatLatency(rayStats.p50).padStart(15)} ${formatLatency(memStats.p50).padStart(15)} ${speedup.padStart(12)}`,
  );
}

// =============================================================================
// Benchmark Functions
// =============================================================================

async function benchmarkKeyLookups(
  db: GraphDB,
  rayGraph: RayGraphData,
  client: MemgraphClient,
  memGraph: MemgraphData,
  iterations: number,
  warmup: number = 1000,
): Promise<{ raySpeedups: number[] }> {
  printComparisonHeader("Key Lookups");
  const speedups: number[] = [];

  // Warmup
  for (let i = 0; i < warmup; i++) {
    const key = rayGraph.nodeKeys[i % rayGraph.nodeKeys.length]!;
    getNodeByKey(db, key);
    await client.getNodeByKey(key);
  }

  // Uniform random
  const rayUniform = new LatencyTracker();
  const memUniform = new LatencyTracker();

  for (let i = 0; i < iterations; i++) {
    const keyIdx = Math.floor(Math.random() * rayGraph.nodeKeys.length);
    const key = rayGraph.nodeKeys[keyIdx]!;

    const start1 = Bun.nanoseconds();
    getNodeByKey(db, key);
    rayUniform.record(Bun.nanoseconds() - start1);

    const start2 = Bun.nanoseconds();
    await client.getNodeByKey(key);
    memUniform.record(Bun.nanoseconds() - start2);
  }
  const rayUniformStats = rayUniform.getStats();
  const memUniformStats = memUniform.getStats();
  printComparisonRow("Uniform random", rayUniformStats, memUniformStats);
  speedups.push(memUniformStats.p50 / rayUniformStats.p50);

  // Sequential
  const raySeq = new LatencyTracker();
  const memSeq = new LatencyTracker();

  for (let i = 0; i < iterations; i++) {
    const key = rayGraph.nodeKeys[i % rayGraph.nodeKeys.length]!;

    const start1 = Bun.nanoseconds();
    getNodeByKey(db, key);
    raySeq.record(Bun.nanoseconds() - start1);

    const start2 = Bun.nanoseconds();
    await client.getNodeByKey(key);
    memSeq.record(Bun.nanoseconds() - start2);
  }
  const raySeqStats = raySeq.getStats();
  const memSeqStats = memSeq.getStats();
  printComparisonRow("Sequential", raySeqStats, memSeqStats);
  speedups.push(memSeqStats.p50 / raySeqStats.p50);

  // Missing keys
  const rayMissing = new LatencyTracker();
  const memMissing = new LatencyTracker();
  const missingIterations = Math.min(iterations, 1000);

  for (let i = 0; i < missingIterations; i++) {
    const key = `nonexistent.key.${i}`;

    const start1 = Bun.nanoseconds();
    getNodeByKey(db, key);
    rayMissing.record(Bun.nanoseconds() - start1);

    const start2 = Bun.nanoseconds();
    await client.getNodeByKey(key);
    memMissing.record(Bun.nanoseconds() - start2);
  }
  const rayMissingStats = rayMissing.getStats();
  const memMissingStats = memMissing.getStats();
  printComparisonRow("Missing keys", rayMissingStats, memMissingStats);
  speedups.push(memMissingStats.p50 / rayMissingStats.p50);

  return { raySpeedups: speedups };
}

async function benchmarkTraversals(
  db: GraphDB,
  rayGraph: RayGraphData,
  client: MemgraphClient,
  memGraph: MemgraphData,
  iterations: number,
  warmup: number = 1000,
): Promise<{ raySpeedups: number[] }> {
  printComparisonHeader("1-Hop Traversals");
  const speedups: number[] = [];

  // Warmup
  for (let i = 0; i < warmup; i++) {
    const rayNode = rayGraph.nodeIds[i % rayGraph.nodeIds.length]!;
    const memNode = memGraph.nodeIds[i % memGraph.nodeIds.length]!;
    for (const _ of getNeighborsOut(db, rayNode)) break;
    await client.getNeighborsOut(memNode);
  }

  // Outgoing neighbors
  const rayOut = new LatencyTracker();
  const memOut = new LatencyTracker();

  for (let i = 0; i < iterations; i++) {
    const idx = Math.floor(Math.random() * rayGraph.nodeIds.length);
    const rayNode = rayGraph.nodeIds[idx]!;
    const memNode = memGraph.nodeIds[idx]!;

    const start1 = Bun.nanoseconds();
    let count = 0;
    for (const _ of getNeighborsOut(db, rayNode)) count++;
    rayOut.record(Bun.nanoseconds() - start1);

    const start2 = Bun.nanoseconds();
    await client.getNeighborsOut(memNode);
    memOut.record(Bun.nanoseconds() - start2);
  }
  const rayOutStats = rayOut.getStats();
  const memOutStats = memOut.getStats();
  printComparisonRow("Out neighbors", rayOutStats, memOutStats);
  speedups.push(memOutStats.p50 / rayOutStats.p50);

  // Incoming neighbors
  const rayIn = new LatencyTracker();
  const memIn = new LatencyTracker();

  for (let i = 0; i < iterations; i++) {
    const idx = Math.floor(Math.random() * rayGraph.nodeIds.length);
    const rayNode = rayGraph.nodeIds[idx]!;
    const memNode = memGraph.nodeIds[idx]!;

    const start1 = Bun.nanoseconds();
    let count = 0;
    for (const _ of getNeighborsIn(db, rayNode)) count++;
    rayIn.record(Bun.nanoseconds() - start1);

    const start2 = Bun.nanoseconds();
    await client.getNeighborsIn(memNode);
    memIn.record(Bun.nanoseconds() - start2);
  }
  const rayInStats = rayIn.getStats();
  const memInStats = memIn.getStats();
  printComparisonRow("In neighbors", rayInStats, memInStats);
  speedups.push(memInStats.p50 / rayInStats.p50);

  // Filtered by edge type (CALLS)
  const rayFiltered = new LatencyTracker();
  const memFiltered = new LatencyTracker();

  for (let i = 0; i < iterations; i++) {
    const idx = Math.floor(Math.random() * rayGraph.nodeIds.length);
    const rayNode = rayGraph.nodeIds[idx]!;
    const memNode = memGraph.nodeIds[idx]!;

    const start1 = Bun.nanoseconds();
    let count = 0;
    for (const _ of getNeighborsOut(db, rayNode, rayGraph.etypes.calls)) count++;
    rayFiltered.record(Bun.nanoseconds() - start1);

    const start2 = Bun.nanoseconds();
    await client.getNeighborsOut(memNode, "CALLS");
    memFiltered.record(Bun.nanoseconds() - start2);
  }
  const rayFilteredStats = rayFiltered.getStats();
  const memFilteredStats = memFiltered.getStats();
  printComparisonRow("Filtered (CALLS)", rayFilteredStats, memFilteredStats);
  speedups.push(memFilteredStats.p50 / rayFilteredStats.p50);

  return { raySpeedups: speedups };
}

async function benchmarkEdgeExists(
  db: GraphDB,
  rayGraph: RayGraphData,
  client: MemgraphClient,
  memGraph: MemgraphData,
  iterations: number,
  warmup: number = 1000,
): Promise<{ raySpeedups: number[] }> {
  printComparisonHeader("Edge Existence");
  const speedups: number[] = [];

  // Warmup
  for (let i = 0; i < warmup; i++) {
    const srcIdx = Math.floor(Math.random() * rayGraph.nodeIds.length);
    const dstIdx = Math.floor(Math.random() * rayGraph.nodeIds.length);
    edgeExists(db, rayGraph.nodeIds[srcIdx]!, rayGraph.etypes.calls, rayGraph.nodeIds[dstIdx]!);
    await client.edgeExists(memGraph.nodeIds[srcIdx]!, "CALLS", memGraph.nodeIds[dstIdx]!);
  }

  const rayTracker = new LatencyTracker();
  const memTracker = new LatencyTracker();

  for (let i = 0; i < iterations; i++) {
    const srcIdx = Math.floor(Math.random() * rayGraph.nodeIds.length);
    const dstIdx = Math.floor(Math.random() * rayGraph.nodeIds.length);

    const start1 = Bun.nanoseconds();
    edgeExists(db, rayGraph.nodeIds[srcIdx]!, rayGraph.etypes.calls, rayGraph.nodeIds[dstIdx]!);
    rayTracker.record(Bun.nanoseconds() - start1);

    const start2 = Bun.nanoseconds();
    await client.edgeExists(memGraph.nodeIds[srcIdx]!, "CALLS", memGraph.nodeIds[dstIdx]!);
    memTracker.record(Bun.nanoseconds() - start2);
  }

  const rayStats = rayTracker.getStats();
  const memStats = memTracker.getStats();
  printComparisonRow("Random edge check", rayStats, memStats);
  speedups.push(memStats.p50 / rayStats.p50);

  return { raySpeedups: speedups };
}

async function benchmarkMultiHop(
  db: GraphDB,
  rayGraph: RayGraphData,
  client: MemgraphClient,
  memGraph: MemgraphData,
  iterations: number,
  warmup: number = 100,
): Promise<{ raySpeedups: number[] }> {
  printComparisonHeader("Multi-Hop Traversals");
  const speedups: number[] = [];

  // Use fewer iterations for multi-hop (expensive)
  const hopIterations = Math.min(iterations, 1000);

  // Warmup
  for (let i = 0; i < Math.min(warmup, 100); i++) {
    const idx = Math.floor(Math.random() * rayGraph.nodeIds.length);
    const rayNode = rayGraph.nodeIds[idx]!;

    // 2-hop RayDB
    for (const neighbor of getNeighborsOut(db, rayNode)) {
      for (const _ of getNeighborsOut(db, neighbor)) break;
      break;
    }
    await client.getNeighbors2Hop(memGraph.nodeIds[idx]!);
  }

  // 2-hop traversal
  const ray2Hop = new LatencyTracker();
  const mem2Hop = new LatencyTracker();

  for (let i = 0; i < hopIterations; i++) {
    const idx = Math.floor(Math.random() * rayGraph.nodeIds.length);
    const rayNode = rayGraph.nodeIds[idx]!;
    const memNode = memGraph.nodeIds[idx]!;

    const start1 = Bun.nanoseconds();
    const visited = new Set<NodeID>();
    for (const n1 of getNeighborsOut(db, rayNode)) {
      for (const n2 of getNeighborsOut(db, n1)) {
        visited.add(n2);
      }
    }
    ray2Hop.record(Bun.nanoseconds() - start1);

    const start2 = Bun.nanoseconds();
    await client.getNeighbors2Hop(memNode);
    mem2Hop.record(Bun.nanoseconds() - start2);
  }
  const ray2HopStats = ray2Hop.getStats();
  const mem2HopStats = mem2Hop.getStats();
  printComparisonRow("2-hop traversal", ray2HopStats, mem2HopStats);
  speedups.push(mem2HopStats.p50 / ray2HopStats.p50);

  // 3-hop traversal (even fewer iterations)
  const hop3Iterations = Math.min(iterations, 500);
  const ray3Hop = new LatencyTracker();
  const mem3Hop = new LatencyTracker();

  for (let i = 0; i < hop3Iterations; i++) {
    const idx = Math.floor(Math.random() * rayGraph.nodeIds.length);
    const rayNode = rayGraph.nodeIds[idx]!;
    const memNode = memGraph.nodeIds[idx]!;

    const start1 = Bun.nanoseconds();
    const visited = new Set<NodeID>();
    for (const n1 of getNeighborsOut(db, rayNode)) {
      for (const n2 of getNeighborsOut(db, n1)) {
        for (const n3 of getNeighborsOut(db, n2)) {
          visited.add(n3);
        }
      }
    }
    ray3Hop.record(Bun.nanoseconds() - start1);

    const start2 = Bun.nanoseconds();
    await client.getNeighbors3Hop(memNode);
    mem3Hop.record(Bun.nanoseconds() - start2);
  }
  const ray3HopStats = ray3Hop.getStats();
  const mem3HopStats = mem3Hop.getStats();
  printComparisonRow("3-hop traversal", ray3HopStats, mem3HopStats);
  speedups.push(mem3HopStats.p50 / ray3HopStats.p50);

  return { raySpeedups: speedups };
}

async function benchmarkWrites(
  rayGraph: RayGraphData,
  memGraph: MemgraphData,
  config: ScaleConfig,
): Promise<{ raySpeedups: number[] }> {
  printComparisonHeader("Write Performance");
  const speedups: number[] = [];

  // Create fresh databases for write test
  const rayTempDir = await mkdtemp(join(tmpdir(), "raydb-bench-write-"));
  const rayDb = await openGraphDB(rayTempDir);
  const memClient = new MemgraphClient(getMemgraphUri());

  // Clear Memgraph for write test
  await memClient.clearDatabase();
  await memClient.createIndex();

  const batchSize = 5000;
  const nodesToCreate = Math.min(config.nodes, 10000);

  // Measure node creation
  const rayNodeTracker = new LatencyTracker();
  const memNodeTracker = new LatencyTracker();

  // RayDB node creation
  const rayStart = Bun.nanoseconds();
  for (let batch = 0; batch < nodesToCreate; batch += batchSize) {
    const tx = beginTx(rayDb);
    const end = Math.min(batch + batchSize, nodesToCreate);
    for (let i = batch; i < end; i++) {
      createNode(tx, { key: `write-test-${i}` });
    }
    await commit(tx);
  }
  rayNodeTracker.record(Bun.nanoseconds() - rayStart);

  // Memgraph node creation
  const memStart = Bun.nanoseconds();
  for (let batch = 0; batch < nodesToCreate; batch += batchSize) {
    const end = Math.min(batch + batchSize, nodesToCreate);
    const keys: string[] = [];
    for (let i = batch; i < end; i++) {
      keys.push(`write-test-${i}`);
    }
    await memClient.createNodes(keys);
  }
  memNodeTracker.record(Bun.nanoseconds() - memStart);

  const rayNodeStats = rayNodeTracker.getStats();
  const memNodeStats = memNodeTracker.getStats();
  printComparisonRow(`Batch insert (${formatNumber(nodesToCreate)} nodes)`, rayNodeStats, memNodeStats);
  speedups.push(memNodeStats.p50 / rayNodeStats.p50);

  // Cleanup
  await closeGraphDB(rayDb);
  await rm(rayTempDir, { recursive: true });
  await memClient.close();

  return { raySpeedups: speedups };
}

// =============================================================================
// Main Benchmark Suite
// =============================================================================

async function runBenchmarkSuite(config: ScaleConfig): Promise<{
  lookupSpeedups: number[];
  traversalSpeedups: number[];
  edgeSpeedups: number[];
  multiHopSpeedups: number[];
  writeSpeedups: number[];
}> {
  logger.log(`\n${"=".repeat(80)}`);
  logger.log(`Scale: ${formatNumber(config.nodes)} nodes / ${formatNumber(config.edges)} edges / ${formatNumber(config.iterations)} iterations`);
  logger.log("=".repeat(80));

  // Setup RayDB
  logger.log("\n--- Graph Setup ---");
  const rayTempDir = await mkdtemp(join(tmpdir(), "raydb-bench-"));
  const rayDb = await openGraphDB(rayTempDir);

  const rayStartTime = Date.now();
  const rayGraph = await buildRayGraph(rayDb, config);
  const raySetupTime = Date.now() - rayStartTime;
  logger.log(`  RayDB setup time: ${(raySetupTime / 1000).toFixed(2)}s`);

  // Setup Memgraph
  const memClient = new MemgraphClient(getMemgraphUri());
  await memClient.clearDatabase();
  await memClient.createIndex();

  const memStartTime = Date.now();
  const memGraph = await buildMemgraphGraph(memClient, config);
  const memSetupTime = Date.now() - memStartTime;
  logger.log(`  Memgraph setup time: ${(memSetupTime / 1000).toFixed(2)}s`);
  logger.log(`  Setup speedup: ${(memSetupTime / raySetupTime).toFixed(1)}x`);

  // Verify counts
  const memNodeCount = await memClient.countNodes();
  const memEdgeCount = await memClient.countEdges();
  logger.log(`\n  RayDB: ${formatNumber(rayGraph.nodeIds.length)} nodes, ~${formatNumber(config.edges)} edges`);
  logger.log(`  Memgraph: ${formatNumber(memNodeCount)} nodes, ${formatNumber(memEdgeCount)} edges`);

  // Run benchmarks
  const { raySpeedups: lookupSpeedups } = await benchmarkKeyLookups(
    rayDb,
    rayGraph,
    memClient,
    memGraph,
    config.iterations,
  );

  const { raySpeedups: traversalSpeedups } = await benchmarkTraversals(
    rayDb,
    rayGraph,
    memClient,
    memGraph,
    config.iterations,
  );

  const { raySpeedups: edgeSpeedups } = await benchmarkEdgeExists(
    rayDb,
    rayGraph,
    memClient,
    memGraph,
    config.iterations,
  );

  const { raySpeedups: multiHopSpeedups } = await benchmarkMultiHop(
    rayDb,
    rayGraph,
    memClient,
    memGraph,
    config.iterations,
  );

  const { raySpeedups: writeSpeedups } = await benchmarkWrites(rayGraph, memGraph, config);

  // Cleanup
  await closeGraphDB(rayDb);
  await rm(rayTempDir, { recursive: true });
  await memClient.close();

  return {
    lookupSpeedups,
    traversalSpeedups,
    edgeSpeedups,
    multiHopSpeedups,
    writeSpeedups,
  };
}

function geometricMean(values: number[]): number {
  if (values.length === 0) return 0;
  const product = values.reduce((a, b) => a * b, 1);
  return Math.pow(product, 1 / values.length);
}

function printSummary(results: {
  lookupSpeedups: number[];
  traversalSpeedups: number[];
  edgeSpeedups: number[];
  multiHopSpeedups: number[];
  writeSpeedups: number[];
}): void {
  logger.log(`\n${"=".repeat(80)}`);
  logger.log("SUMMARY (geometric mean speedup - higher = RayDB faster)");
  logger.log("=".repeat(80));

  const lookupGM = geometricMean(results.lookupSpeedups);
  const traversalGM = geometricMean(results.traversalSpeedups);
  const edgeGM = geometricMean(results.edgeSpeedups);
  const multiHopGM = geometricMean(results.multiHopSpeedups);
  const writeGM = geometricMean(results.writeSpeedups);

  logger.log(`Key Lookups:    ${lookupGM.toFixed(1)}x`);
  logger.log(`Traversals:     ${traversalGM.toFixed(1)}x`);
  logger.log(`Edge Checks:    ${edgeGM.toFixed(1)}x`);
  logger.log(`Multi-Hop:      ${multiHopGM.toFixed(1)}x`);
  logger.log(`Writes:         ${writeGM.toFixed(1)}x`);

  const allSpeedups = [
    ...results.lookupSpeedups,
    ...results.traversalSpeedups,
    ...results.edgeSpeedups,
    ...results.multiHopSpeedups,
    ...results.writeSpeedups,
  ];
  const overallGM = geometricMean(allSpeedups);

  logger.log("-".repeat(40));
  logger.log(`Overall:        ${overallGM.toFixed(1)}x faster`);
  logger.log("=".repeat(80));
}

// =============================================================================
// Main Entry Point
// =============================================================================

async function main(): Promise<void> {
  const config = parseArgs();
  logger = new Logger(config.outputFile);

  logger.log("================================================================================");
  logger.log("RayDB vs Memgraph Comparison Benchmark");
  logger.log("================================================================================");
  logger.log(`Date: ${new Date().toISOString()}`);
  logger.log(`Docker: memgraph/memgraph:latest`);
  logger.log(`RayDB: embedded (in-process)`);

  // Handle Ctrl+C gracefully
  let shuttingDown = false;
  const cleanup = async () => {
    if (shuttingDown) return;
    shuttingDown = true;
    console.log("\n\nInterrupted, cleaning up...");
    if (!config.skipDocker) {
      await stopMemgraph();
    }
    await logger.flush();
    process.exit(1);
  };

  process.on("SIGINT", cleanup);
  process.on("SIGTERM", cleanup);

  try {
    // Start Memgraph
    if (!config.skipDocker) {
      logger.log("\n--- Docker Setup ---");
      await startMemgraph();
    }

    const scales: ScaleConfig[] = [];

    if (config.runScale) {
      // Run both scales
      scales.push({
        name: "small",
        nodes: 10000,
        edges: 50000,
        iterations: 10000,
      });
      scales.push({
        name: "large",
        nodes: 100000,
        edges: 1000000,
        iterations: 10000,
      });
    } else {
      // Single scale from CLI args
      scales.push({
        name: "custom",
        nodes: config.nodes,
        edges: config.edges,
        iterations: config.iterations,
      });
    }

    let allResults = {
      lookupSpeedups: [] as number[],
      traversalSpeedups: [] as number[],
      edgeSpeedups: [] as number[],
      multiHopSpeedups: [] as number[],
      writeSpeedups: [] as number[],
    };

    for (const scale of scales) {
      const results = await runBenchmarkSuite(scale);
      allResults.lookupSpeedups.push(...results.lookupSpeedups);
      allResults.traversalSpeedups.push(...results.traversalSpeedups);
      allResults.edgeSpeedups.push(...results.edgeSpeedups);
      allResults.multiHopSpeedups.push(...results.multiHopSpeedups);
      allResults.writeSpeedups.push(...results.writeSpeedups);
    }

    printSummary(allResults);

    // Stop Memgraph
    if (!config.skipDocker) {
      logger.log("\n--- Cleanup ---");
      await stopMemgraph();
    }

    // Flush output
    await logger.flush();
    const outputPath = logger.getOutputPath();
    if (outputPath) {
      logger.log(`\nResults saved to: ${outputPath}`);
    }
  } catch (error) {
    console.error("\nBenchmark failed:", error);
    if (!config.skipDocker) {
      await stopMemgraph();
    }
    process.exit(1);
  }
}

main();
