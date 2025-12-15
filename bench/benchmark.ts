/**
 * Ray Database Benchmark
 *
 * Realistic benchmarks for code graph / knowledge graph workloads.
 *
 * Usage:
 *   bun run bench/benchmark.ts [options]
 *
 * Options:
 *   --nodes N         Number of nodes (default: 10000)
 *   --edges M         Number of edges (default: 50000)
 *   --hub-percent P   Percent of nodes that are hubs (default: 1)
 *   --iterations I    Iterations for latency benchmarks (default: 10000)
 *   --output FILE     Output file path (default: bench/results/benchmark-<timestamp>.txt)
 *   --no-output       Disable file output
 *   --keep-db         Keep the database directory after benchmark (prints path)
 */

import {
  appendFile,
  mkdir,
  mkdtemp,
  readdir,
  rm,
  stat,
  writeFile,
} from "node:fs/promises";
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
  optimize,
  stats,
} from "../src/index.ts";

// =============================================================================
// Configuration
// =============================================================================

interface BenchConfig {
  nodes: number;
  edges: number;
  hubPercent: number;
  iterations: number;
  outputFile: string | null;
  keepDb: boolean;
}

function parseArgs(): BenchConfig {
  const args = process.argv.slice(2);

  // Generate default output filename with timestamp
  const now = new Date();
  const timestamp = now.toISOString().replace(/[:.]/g, "-").slice(0, 19);
  const defaultOutput = join(
    dirname(import.meta.path),
    "results",
    `benchmark-${timestamp}.txt`,
  );

  const config: BenchConfig = {
    nodes: 10000,
    edges: 50000,
    hubPercent: 1,
    iterations: 10000,
    outputFile: defaultOutput,
    keepDb: false,
  };

  for (let i = 0; i < args.length; i++) {
    switch (args[i]) {
      case "--nodes":
        config.nodes = Number.parseInt(args[++i] || "10000", 10);
        break;
      case "--edges":
        config.edges = Number.parseInt(args[++i] || "50000", 10);
        break;
      case "--hub-percent":
        config.hubPercent = Number.parseFloat(args[++i] || "1");
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

  // For progress updates that shouldn't go to file
  progress(message: string): void {
    process.stdout.write(message);
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
  if (ns < 1_000_000) return `${(ns / 1000).toFixed(2)}µs`;
  return `${(ns / 1_000_000).toFixed(2)}ms`;
}

function formatNumber(n: number): string {
  return n.toLocaleString("en-US");
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(2)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(2)} MB`;
}

async function getDirectorySize(dirPath: string): Promise<number> {
  let totalSize = 0;

  async function walkDir(dir: string): Promise<void> {
    const entries = await readdir(dir, { withFileTypes: true });
    for (const entry of entries) {
      const fullPath = join(dir, entry.name);
      if (entry.isDirectory()) {
        await walkDir(fullPath);
      } else if (entry.isFile()) {
        const fileStat = await stat(fullPath);
        totalSize += fileStat.size;
      }
    }
  }

  await walkDir(dirPath);
  return totalSize;
}

// =============================================================================
// Graph Structure
// =============================================================================

interface GraphData {
  nodeIds: NodeID[];
  nodeKeys: string[];
  hubNodes: NodeID[];
  leafNodes: NodeID[];
  outDegree: Map<bigint, number>;
  inDegree: Map<bigint, number>;
  etypes: {
    calls: number;
    references: number;
    imports: number;
    extends: number;
  };
}

async function buildRealisticGraph(
  db: GraphDB,
  config: BenchConfig,
): Promise<GraphData> {
  const nodeIds: NodeID[] = [];
  const nodeKeys: string[] = [];
  const outDegree = new Map<bigint, number>();
  const inDegree = new Map<bigint, number>();

  // Use large batch sizes for setup
  const batchSize = 5000;
  let etypes: GraphData["etypes"] | undefined;

  console.log("  Creating nodes...");
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
      outDegree.set(nodeId, 0);
      inDegree.set(nodeId, 0);
    }
    await commit(tx);
    process.stdout.write(`\r  Created ${end} / ${config.nodes} nodes`);
  }
  console.log();

  // Identify hub nodes
  const numHubs = Math.max(
    1,
    Math.floor(config.nodes * (config.hubPercent / 100)),
  );
  const hubIndices = new Set<number>();
  while (hubIndices.size < numHubs) {
    hubIndices.add(Math.floor(Math.random() * nodeIds.length));
  }

  const hubNodes = [...hubIndices].map((i) => nodeIds[i]!);
  const leafNodes = nodeIds.filter((_, i) => !hubIndices.has(i));

  // Create edges with power-law-like distribution
  const edgeTypes = [
    etypes!.calls,
    etypes!.references,
    etypes!.imports,
    etypes!.extends,
  ];
  const edgeTypeWeights = [0.4, 0.35, 0.15, 0.1];

  console.log("  Creating edges...");
  let edgesCreated = 0;
  let attempts = 0;
  const maxAttempts = config.edges * 3; // Prevent infinite loops

  while (edgesCreated < config.edges && attempts < maxAttempts) {
    const tx = beginTx(db);
    const batchTarget = Math.min(edgesCreated + batchSize, config.edges);

    while (edgesCreated < batchTarget && attempts < maxAttempts) {
      attempts++;
      let src: NodeID;
      let dst: NodeID;

      // 30% from hubs, 20% to hubs
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
        outDegree.set(src, (outDegree.get(src) || 0) + 1);
        inDegree.set(dst, (inDegree.get(dst) || 0) + 1);
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
    hubNodes,
    leafNodes,
    outDegree,
    inDegree,
    etypes: etypes!,
  };
}

// =============================================================================
// Benchmark Reporting
// =============================================================================

function printLatencyTable(name: string, stats: LatencyStats): void {
  const opsPerSec =
    stats.sum > 0 ? stats.count / (stats.sum / 1_000_000_000) : 0;
  logger.log(
    `${name.padEnd(45)} p50=${formatLatency(stats.p50).padStart(10)} p95=${formatLatency(stats.p95).padStart(10)} p99=${formatLatency(stats.p99).padStart(10)} max=${formatLatency(stats.max).padStart(10)} (${formatNumber(Math.round(opsPerSec))} ops/sec)`,
  );
}

// =============================================================================
// Key Lookup Benchmarks
// =============================================================================

function benchmarkKeyLookups(
  db: GraphDB,
  graph: GraphData,
  iterations: number,
): void {
  logger.log("\n--- Key Lookups (getNodeByKey) ---");

  // Uniform random
  const uniformTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const key =
      graph.nodeKeys[Math.floor(Math.random() * graph.nodeKeys.length)]!;
    const start = Bun.nanoseconds();
    getNodeByKey(db, key);
    uniformTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable("Uniform random keys", uniformTracker.getStats());

  // Sequential (cache-friendly)
  const seqTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const key = graph.nodeKeys[i % graph.nodeKeys.length]!;
    const start = Bun.nanoseconds();
    getNodeByKey(db, key);
    seqTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable("Sequential keys", seqTracker.getStats());

  // Missing keys
  const missingTracker = new LatencyTracker();
  for (let i = 0; i < Math.min(iterations, 1000); i++) {
    const key = `nonexistent.key.${i}`;
    const start = Bun.nanoseconds();
    getNodeByKey(db, key);
    missingTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable("Missing keys", missingTracker.getStats());
}

// =============================================================================
// Traversal Benchmarks
// =============================================================================

function benchmarkTraversals(
  db: GraphDB,
  graph: GraphData,
  iterations: number,
): void {
  logger.log("\n--- 1-Hop Traversals ---");

  // Find worst-case nodes
  let worstOutNode = graph.nodeIds[0]!;
  let worstOutDegree = 0;
  for (const [nodeId, degree] of graph.outDegree) {
    if (degree > worstOutDegree) {
      worstOutDegree = degree;
      worstOutNode = nodeId;
    }
  }

  let worstInNode = graph.nodeIds[0]!;
  let worstInDegree = 0;
  for (const [nodeId, degree] of graph.inDegree) {
    if (degree > worstInDegree) {
      worstInDegree = degree;
      worstInNode = nodeId;
    }
  }

  logger.log(
    `  Worst-case out-degree: ${worstOutDegree}, in-degree: ${worstInDegree}`,
  );

  // Uniform random - outgoing
  const uniformOutTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const node =
      graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
    const start = Bun.nanoseconds();
    let count = 0;
    for (const _ of getNeighborsOut(db, node)) count++;
    uniformOutTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable("Uniform random (out)", uniformOutTracker.getStats());

  // Uniform random - incoming
  const uniformInTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const node =
      graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
    const start = Bun.nanoseconds();
    let count = 0;
    for (const _ of getNeighborsIn(db, node)) count++;
    uniformInTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable("Uniform random (in)", uniformInTracker.getStats());

  // Hub nodes only
  if (graph.hubNodes.length > 0) {
    const hubOutTracker = new LatencyTracker();
    for (let i = 0; i < iterations; i++) {
      const node =
        graph.hubNodes[Math.floor(Math.random() * graph.hubNodes.length)]!;
      const start = Bun.nanoseconds();
      let count = 0;
      for (const _ of getNeighborsOut(db, node)) count++;
      hubOutTracker.record(Bun.nanoseconds() - start);
    }
    printLatencyTable("Hub nodes only (out)", hubOutTracker.getStats());

    const hubInTracker = new LatencyTracker();
    for (let i = 0; i < iterations; i++) {
      const node =
        graph.hubNodes[Math.floor(Math.random() * graph.hubNodes.length)]!;
      const start = Bun.nanoseconds();
      let count = 0;
      for (const _ of getNeighborsIn(db, node)) count++;
      hubInTracker.record(Bun.nanoseconds() - start);
    }
    printLatencyTable("Hub nodes only (in)", hubInTracker.getStats());
  }

  // Worst-case node
  const worstOutTracker = new LatencyTracker();
  for (let i = 0; i < Math.min(iterations, 1000); i++) {
    const start = Bun.nanoseconds();
    let count = 0;
    for (const _ of getNeighborsOut(db, worstOutNode)) count++;
    worstOutTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable(
    `Worst-case node (out, deg=${worstOutDegree})`,
    worstOutTracker.getStats(),
  );

  const worstInTracker = new LatencyTracker();
  for (let i = 0; i < Math.min(iterations, 1000); i++) {
    const start = Bun.nanoseconds();
    let count = 0;
    for (const _ of getNeighborsIn(db, worstInNode)) count++;
    worstInTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable(
    `Worst-case node (in, deg=${worstInDegree})`,
    worstInTracker.getStats(),
  );
}

// =============================================================================
// Filtered Traversal Benchmarks
// =============================================================================

function benchmarkFilteredTraversals(
  db: GraphDB,
  graph: GraphData,
  iterations: number,
): void {
  logger.log("\n--- Filtered Traversals (by edge type) ---");

  // CALLS (out) - who does X call?
  const callsOutTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const node =
      graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
    const start = Bun.nanoseconds();
    let count = 0;
    for (const _ of getNeighborsOut(db, node, graph.etypes.calls)) count++;
    callsOutTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable(
    "CALLS (out) - who does X call?",
    callsOutTracker.getStats(),
  );

  // REFERENCES (in) - where is X used?
  const refsInTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const node =
      graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
    const start = Bun.nanoseconds();
    let count = 0;
    for (const _ of getNeighborsIn(db, node, graph.etypes.references)) count++;
    refsInTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable(
    "REFERENCES (in) - where is X used?",
    refsInTracker.getStats(),
  );

  // IMPORTS (out)
  const importsTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const node =
      graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
    const start = Bun.nanoseconds();
    let count = 0;
    for (const _ of getNeighborsOut(db, node, graph.etypes.imports)) count++;
    importsTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable(
    "IMPORTS (out) - what does X import?",
    importsTracker.getStats(),
  );

  // Compare filtered vs unfiltered
  logger.log("\n  Filtered vs Unfiltered comparison:");
  const unfilteredTracker = new LatencyTracker();
  const filteredTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const node =
      graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;

    const start1 = Bun.nanoseconds();
    let c1 = 0;
    for (const _ of getNeighborsOut(db, node)) c1++;
    unfilteredTracker.record(Bun.nanoseconds() - start1);

    const start2 = Bun.nanoseconds();
    let c2 = 0;
    for (const _ of getNeighborsOut(db, node, graph.etypes.calls)) c2++;
    filteredTracker.record(Bun.nanoseconds() - start2);
  }

  const uf = unfilteredTracker.getStats();
  const f = filteredTracker.getStats();
  const speedup = uf.p50 > 0 && f.p50 > 0 ? (uf.p50 / f.p50).toFixed(2) : "N/A";
  logger.log(
    `    Unfiltered p50: ${formatLatency(uf.p50)}, Filtered p50: ${formatLatency(f.p50)}, Speedup: ${speedup}x`,
  );
}

// =============================================================================
// Edge Existence Benchmarks
// =============================================================================

function benchmarkEdgeExists(
  db: GraphDB,
  graph: GraphData,
  iterations: number,
): void {
  logger.log("\n--- Edge Existence Checks ---");

  const tracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const src =
      graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
    const dst =
      graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
    const start = Bun.nanoseconds();
    edgeExists(db, src, graph.etypes.calls, dst);
    tracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable("Random edge exists check", tracker.getStats());
}

// =============================================================================
// Multi-Hop Benchmarks
// =============================================================================

function benchmarkMultiHop(
  db: GraphDB,
  graph: GraphData,
  iterations: number,
): void {
  logger.log("\n--- Multi-Hop Traversals ---");

  // 2-hop
  const twoHopTracker = new LatencyTracker();
  let totalNodes2 = 0;
  const iters2 = Math.min(iterations, 1000);
  for (let i = 0; i < iters2; i++) {
    const start =
      graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
    const startNs = Bun.nanoseconds();
    let visited = 0;
    for (const e1 of getNeighborsOut(db, start, graph.etypes.calls)) {
      visited++;
      for (const e2 of getNeighborsOut(db, e1.dst, graph.etypes.calls)) {
        visited++;
        if (visited > 1000) break;
      }
      if (visited > 1000) break;
    }
    twoHopTracker.record(Bun.nanoseconds() - startNs);
    totalNodes2 += visited;
  }
  printLatencyTable(
    `2-hop CALLS (avg ${Math.round(totalNodes2 / iters2)} nodes)`,
    twoHopTracker.getStats(),
  );

  // 3-hop
  const threeHopTracker = new LatencyTracker();
  let totalNodes3 = 0;
  const iters3 = Math.min(iterations, 500);
  for (let i = 0; i < iters3; i++) {
    const start =
      graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
    const startNs = Bun.nanoseconds();
    let visited = 0;
    for (const e1 of getNeighborsOut(db, start, graph.etypes.calls)) {
      visited++;
      for (const e2 of getNeighborsOut(db, e1.dst, graph.etypes.calls)) {
        visited++;
        for (const e3 of getNeighborsOut(db, e2.dst, graph.etypes.calls)) {
          visited++;
          if (visited > 2000) break;
        }
        if (visited > 2000) break;
      }
      if (visited > 2000) break;
    }
    threeHopTracker.record(Bun.nanoseconds() - startNs);
    totalNodes3 += visited;
  }
  printLatencyTable(
    `3-hop CALLS (avg ${Math.round(totalNodes3 / iters3)} nodes)`,
    threeHopTracker.getStats(),
  );
}

// =============================================================================
// Delta Impact Benchmark
// =============================================================================

async function benchmarkDeltaImpact(config: BenchConfig): Promise<void> {
  logger.log("\n--- Delta Size Impact on Read Performance ---");

  const deltaPercents = [0, 5, 10, 25];
  const smallConfig = {
    ...config,
    nodes: Math.min(config.nodes, 5000),
    edges: Math.min(config.edges, 25000),
  };

  for (const deltaPercent of deltaPercents) {
    const testDir = await mkdtemp(join(tmpdir(), `ray-delta-${deltaPercent}-`));

    try {
      const db = await openGraphDB(testDir);
      const graph = await buildRealisticGraph(db, smallConfig);
      await optimize(db);

      // Add delta edges
      const deltaEdges = Math.floor(smallConfig.edges * (deltaPercent / 100));
      if (deltaEdges > 0) {
        const batchSize = 5000;
        for (let batch = 0; batch < deltaEdges; batch += batchSize) {
          const tx = beginTx(db);
          const end = Math.min(batch + batchSize, deltaEdges);
          for (let i = batch; i < end; i++) {
            const src =
              graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
            const dst =
              graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
            if (src !== dst) {
              addEdge(tx, src, graph.etypes.calls, dst);
            }
          }
          await commit(tx);
        }
      }

      // Measure
      const tracker = new LatencyTracker();
      const iters = Math.min(config.iterations, 5000);
      for (let i = 0; i < iters; i++) {
        const node =
          graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
        const start = Bun.nanoseconds();
        let count = 0;
        for (const _ of getNeighborsOut(db, node)) count++;
        tracker.record(Bun.nanoseconds() - start);
      }

      const st = tracker.getStats();
      logger.log(
        `  Delta ${deltaPercent.toString().padStart(2)}%: p50=${formatLatency(st.p50).padStart(10)} p95=${formatLatency(st.p95).padStart(10)} p99=${formatLatency(st.p99).padStart(10)}`,
      );

      await closeGraphDB(db);
    } finally {
      await rm(testDir, { recursive: true, force: true });
    }
  }
}

// =============================================================================
// Database Size Report
// =============================================================================

async function reportDatabaseSize(
  dbPath: string,
  config: BenchConfig,
): Promise<void> {
  logger.log("\n--- Database Size Report ---");

  const totalSize = await getDirectorySize(dbPath);
  const snapshotsDir = join(dbPath, "snapshots");
  const walDir = join(dbPath, "wal");

  let snapshotsSize = 0;
  let walSize = 0;
  let manifestSize = 0;

  try {
    snapshotsSize = await getDirectorySize(snapshotsDir);
  } catch {
    /* empty */
  }

  try {
    walSize = await getDirectorySize(walDir);
  } catch {
    /* empty */
  }

  try {
    const manifestStat = await stat(join(dbPath, "manifest.gdm"));
    manifestSize = manifestStat.size;
  } catch {
    /* empty */
  }

  const otherSize = totalSize - snapshotsSize - walSize - manifestSize;

  logger.log(`  Total size:      ${formatBytes(totalSize).padStart(12)}`);
  logger.log(
    `  Snapshots:       ${formatBytes(snapshotsSize).padStart(12)} (${((snapshotsSize / totalSize) * 100).toFixed(1)}%)`,
  );
  logger.log(
    `  WAL:             ${formatBytes(walSize).padStart(12)} (${((walSize / totalSize) * 100).toFixed(1)}%)`,
  );
  logger.log(
    `  Manifest:        ${formatBytes(manifestSize).padStart(12)} (${((manifestSize / totalSize) * 100).toFixed(1)}%)`,
  );
  if (otherSize > 0) {
    logger.log(
      `  Other:           ${formatBytes(otherSize).padStart(12)} (${((otherSize / totalSize) * 100).toFixed(1)}%)`,
    );
  }

  // Per-element stats
  const bytesPerNode = totalSize / config.nodes;
  const bytesPerEdge = totalSize / config.edges;
  logger.log("");
  logger.log(`  Bytes per node:  ${bytesPerNode.toFixed(1)}`);
  logger.log(`  Bytes per edge:  ${bytesPerEdge.toFixed(1)}`);
}

// =============================================================================
// Write Performance Benchmark
// =============================================================================

async function benchmarkWrites(
  db: GraphDB,
  graph: GraphData,
  iterations: number,
): Promise<void> {
  logger.log("\n--- Write Performance (Buffered) ---");

  // Batch transactions
  const batchSizes = [10, 100, 1000];
  for (const batchSize of batchSizes) {
    const tracker = new LatencyTracker();
    const batches = Math.min(Math.floor(iterations / batchSize), 50);
    for (let b = 0; b < batches; b++) {
      const start = Bun.nanoseconds();
      const tx = beginTx(db);
      for (let i = 0; i < batchSize; i++) {
        createNode(tx, { key: `bench:batch${batchSize}:${b}:${i}` });
      }
      await commit(tx);
      tracker.record(Bun.nanoseconds() - start);
    }
    const st = tracker.getStats();
    const opsPerSec =
      st.sum > 0 ? (batchSize * st.count) / (st.sum / 1_000_000_000) : 0;
    const label = `Batch of ${batchSize.toString().padStart(4)} nodes`.padEnd(
      45,
    );
    logger.log(
      `${label} p50=${formatLatency(st.p50).padStart(10)} p95=${formatLatency(st.p95).padStart(10)} (${formatNumber(Math.round(opsPerSec))} nodes/sec)`,
    );
  }
}

// =============================================================================
// Main
// =============================================================================

async function runBenchmarks(config: BenchConfig) {
  // Initialize logger
  logger = new Logger(config.outputFile);

  const now = new Date();
  logger.log("=".repeat(120));
  logger.log("Ray Database Benchmark - Realistic Code Graph Workload");
  logger.log("=".repeat(120));
  logger.log(`Date: ${now.toISOString()}`);
  logger.log(`Nodes: ${formatNumber(config.nodes)}`);
  logger.log(`Edges: ${formatNumber(config.edges)}`);
  logger.log(`Hub nodes: ${config.hubPercent}%`);
  logger.log(`Iterations: ${formatNumber(config.iterations)}`);
  logger.log(`Keep database: ${config.keepDb}`);
  logger.log("=".repeat(120));

  const testDir = await mkdtemp(join(tmpdir(), "ray-bench-"));

  try {
    logger.log("\n[1/9] Building graph...");
    const db = await openGraphDB(testDir);
    const startBuild = performance.now();
    const graph = await buildRealisticGraph(db, config);
    logger.log(`  Built in ${(performance.now() - startBuild).toFixed(0)}ms`);

    // Degree stats
    const degrees = [...graph.outDegree.values()].sort((a, b) => b - a);
    const avgDegree = degrees.reduce((a, b) => a + b, 0) / degrees.length;
    logger.log(
      `  Avg out-degree: ${avgDegree.toFixed(1)}, Top 5: ${degrees.slice(0, 5).join(", ")}`,
    );

    logger.log("\n[2/9] Compacting...");
    const startCompact = performance.now();
    await optimize(db);
    logger.log(
      `  Compacted in ${(performance.now() - startCompact).toFixed(0)}ms`,
    );

    const dbStats = stats(db);
    logger.log(
      `  Snapshot: ${formatNumber(Number(dbStats.snapshotNodes))} nodes, ${formatNumber(Number(dbStats.snapshotEdges))} edges`,
    );

    logger.log("\n[3/9] Key lookup benchmarks...");
    benchmarkKeyLookups(db, graph, config.iterations);

    logger.log("\n[4/9] Traversal benchmarks...");
    benchmarkTraversals(db, graph, config.iterations);

    logger.log("\n[5/9] Filtered traversal benchmarks...");
    benchmarkFilteredTraversals(db, graph, config.iterations);

    logger.log("\n[6/9] Edge existence benchmarks...");
    benchmarkEdgeExists(db, graph, config.iterations);

    logger.log("\n[7/9] Multi-hop benchmarks...");
    benchmarkMultiHop(db, graph, config.iterations);

    logger.log("\n[8/9] Write benchmarks...");
    await benchmarkWrites(db, graph, config.iterations);

    // Compact again after writes to get accurate size
    await optimize(db);

    await closeGraphDB(db);

    // Database size report
    logger.log("\n[9/9] Database size report...");
    await reportDatabaseSize(testDir, config);

    // Delta impact (separate instances)
    logger.log("\n[Bonus] Delta impact...");
    await benchmarkDeltaImpact(config);
  } finally {
    if (config.keepDb) {
      logger.log(`\nDatabase preserved at: ${testDir}`);
    } else {
      await rm(testDir, { recursive: true, force: true });
    }
  }

  logger.log(`\n${"=".repeat(120)}`);
  logger.log("Benchmark complete.");
  logger.log("=".repeat(120));

  // Write results to file
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
