/**
 * RayDB NAPI Bindings Benchmark
 *
 * Benchmarks for the native Rust NAPI bindings (napi_bindings).
 * Tests the SingleFileDB exposed via the Database class.
 *
 * Prerequisites:
 *   Build the NAPI bindings first:
 *   cd ray-rs && npm run build
 *
 * Usage:
 *   bun run bench/benchmark-napi.ts [options]
 *
 * Options:
 *   --nodes N         Number of nodes (default: 10000)
 *   --edges M         Number of edges (default: 50000)
 *   --iterations I    Iterations for latency benchmarks (default: 10000)
 *   --output FILE     Output file path (default: bench/results/benchmark-napi-<timestamp>.txt)
 *   --no-output       Disable file output
 *   --keep-db         Keep the database file after benchmark
 */

import { mkdir, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";

// Import the NAPI bindings
// The Database class is exported from the native binding
// eslint-disable-next-line @typescript-eslint/no-require-imports, @typescript-eslint/no-var-requires
const nativeBinding = require("../ray-rs/index.js") as {
  Database: {
    open(path: string, options?: OpenOptions): Database;
  };
  [key: string]: unknown;
};

interface Database {
  // Static methods
  open(path: string, options?: OpenOptions): Database;
  
  // Instance methods
  close(): void;
  isOpen: boolean;
  path: string;
  readOnly: boolean;
  
  // Transaction
  begin(readOnly?: boolean): number;
  commit(): void;
  rollback(): void;
  hasTransaction(): boolean;
  
  // Node operations
  createNode(key?: string): number;
  deleteNode(nodeId: number): void;
  nodeExists(nodeId: number): boolean;
  getNodeByKey(key: string): number | null;
  getNodeKey(nodeId: number): string | null;
  listNodes(): number[];
  countNodes(): number;
  
  // Edge operations
  addEdge(src: number, etype: number, dst: number): void;
  addEdgeByName(src: number, etypeName: string, dst: number): void;
  deleteEdge(src: number, etype: number, dst: number): void;
  edgeExists(src: number, etype: number, dst: number): boolean;
  getOutEdges(nodeId: number): Array<{ etype: number; nodeId: number }>;
  getInEdges(nodeId: number): Array<{ etype: number; nodeId: number }>;
  getOutDegree(nodeId: number): number;
  getInDegree(nodeId: number): number;
  countEdges(): number;
  listEdges(etype?: number): Array<{ src: number; etype: number; dst: number }>;
  
  // Property operations
  setNodeProp(nodeId: number, keyId: number, value: PropValue): void;
  getNodeProp(nodeId: number, keyId: number): PropValue | null;
  getNodeProps(nodeId: number): Array<{ keyId: number; value: PropValue }> | null;
  
  // Vector operations
  setNodeVector(nodeId: number, propKeyId: number, vector: number[]): void;
  getNodeVector(nodeId: number, propKeyId: number): number[] | null;
  hasNodeVector(nodeId: number, propKeyId: number): boolean;
  
  // Schema operations
  getOrCreateEtype(name: string): number;
  getOrCreatePropkey(name: string): number;
  getOrCreateLabel(name: string): number;
  
  // Maintenance
  optimize(): void;
  stats(): DbStats;
}

interface OpenOptions {
  readOnly?: boolean;
  createIfMissing?: boolean;
}

interface PropValue {
  propType: string;
  boolValue?: boolean;
  intValue?: number;
  floatValue?: number;
  stringValue?: string;
}

interface DbStats {
  snapshotGen: number;
  snapshotNodes: number;
  snapshotEdges: number;
  snapshotMaxNodeId: number;
  deltaNodesCreated: number;
  deltaNodesDeleted: number;
  deltaEdgesAdded: number;
  deltaEdgesDeleted: number;
  walBytes: number;
  recommendCompact: boolean;
}

// Database class from native binding - it has a static 'open' method
const DatabaseClass = nativeBinding.Database;

if (!DatabaseClass) {
  console.error("Error: Database class not found in NAPI bindings.");
  console.error("Make sure to build the NAPI bindings first:");
  console.error("  cd ray-rs && npm run build");
  process.exit(1);
}

// Create a wrapper for the Database
const Database = {
  open(path: string, options?: OpenOptions): Database {
    return DatabaseClass.open(path, options);
  }
};

// =============================================================================
// Configuration
// =============================================================================

interface BenchConfig {
  nodes: number;
  edges: number;
  iterations: number;
  outputFile: string | null;
  keepDb: boolean;
}

function parseArgs(): BenchConfig {
  const args = process.argv.slice(2);

  const now = new Date();
  const timestamp = now.toISOString().replace(/[:.]/g, "-").slice(0, 19);
  const defaultOutput = join(
    dirname(import.meta.path),
    "results",
    `benchmark-napi-${timestamp}.txt`
  );

  const config: BenchConfig = {
    nodes: 10000,
    edges: 50000,
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

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(2)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(2)} MB`;
}

function printLatencyTable(name: string, stats: LatencyStats): void {
  const opsPerSec =
    stats.sum > 0 ? stats.count / (stats.sum / 1_000_000_000) : 0;
  logger.log(
    `${name.padEnd(45)} p50=${formatLatency(stats.p50).padStart(10)} p95=${formatLatency(stats.p95).padStart(10)} p99=${formatLatency(stats.p99).padStart(10)} max=${formatLatency(stats.max).padStart(10)} (${formatNumber(Math.round(opsPerSec))} ops/sec)`
  );
}

// =============================================================================
// Graph Structure
// =============================================================================

interface GraphData {
  nodeIds: number[];
  nodeKeys: string[];
  hubNodes: number[];
  leafNodes: number[];
  outDegree: Map<number, number>;
  inDegree: Map<number, number>;
  etypes: {
    calls: number;
    references: number;
    imports: number;
    extends: number;
  };
}

function buildRealisticGraph(db: Database, config: BenchConfig): GraphData {
  const nodeIds: number[] = [];
  const nodeKeys: string[] = [];
  const outDegree = new Map<number, number>();
  const inDegree = new Map<number, number>();

  const batchSize = 5000;

  // Create edge types
  const etypes = {
    calls: db.getOrCreateEtype("CALLS"),
    references: db.getOrCreateEtype("REFERENCES"),
    imports: db.getOrCreateEtype("IMPORTS"),
    extends: db.getOrCreateEtype("EXTENDS"),
  };

  console.log("  Creating nodes...");
  for (let batch = 0; batch < config.nodes; batch += batchSize) {
    db.begin();

    const end = Math.min(batch + batchSize, config.nodes);
    for (let i = batch; i < end; i++) {
      const key = `pkg.module${Math.floor(i / 100)}.Class${i % 100}`;
      const nodeId = db.createNode(key);
      nodeIds.push(nodeId);
      nodeKeys.push(key);
      outDegree.set(nodeId, 0);
      inDegree.set(nodeId, 0);
    }
    db.commit();
    process.stdout.write(`\r  Created ${end} / ${config.nodes} nodes`);
  }
  console.log();

  // Identify hub nodes
  const numHubs = Math.max(1, Math.floor(config.nodes * 0.01));
  const hubIndices = new Set<number>();
  while (hubIndices.size < numHubs) {
    hubIndices.add(Math.floor(Math.random() * nodeIds.length));
  }

  const hubNodes = [...hubIndices].map((i) => nodeIds[i]!);
  const leafNodes = nodeIds.filter((_, i) => !hubIndices.has(i));

  // Create edges with power-law-like distribution
  const edgeTypes = [etypes.calls, etypes.references, etypes.imports, etypes.extends];
  const edgeTypeWeights = [0.4, 0.35, 0.15, 0.1];

  console.log("  Creating edges...");
  let edgesCreated = 0;
  let attempts = 0;
  const maxAttempts = config.edges * 3;

  while (edgesCreated < config.edges && attempts < maxAttempts) {
    db.begin();
    const batchTarget = Math.min(edgesCreated + batchSize, config.edges);

    while (edgesCreated < batchTarget && attempts < maxAttempts) {
      attempts++;
      let src: number;
      let dst: number;

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

        db.addEdge(src, etype, dst);
        outDegree.set(src, (outDegree.get(src) || 0) + 1);
        inDegree.set(dst, (inDegree.get(dst) || 0) + 1);
        edgesCreated++;
      }
    }
    db.commit();
    
    // Checkpoint periodically to avoid WAL overflow
    if (edgesCreated % 10000 === 0) {
      db.optimize();
    }
    
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
    etypes,
  };
}

// =============================================================================
// Key Lookup Benchmarks
// =============================================================================

function benchmarkKeyLookups(
  db: Database,
  graph: GraphData,
  iterations: number
): void {
  logger.log("\n--- Key Lookups (getNodeByKey) ---");

  // Uniform random
  const uniformTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const key =
      graph.nodeKeys[Math.floor(Math.random() * graph.nodeKeys.length)]!;
    const start = Bun.nanoseconds();
    db.getNodeByKey(key);
    uniformTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable("Uniform random keys", uniformTracker.getStats());

  // Sequential (cache-friendly)
  const seqTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const key = graph.nodeKeys[i % graph.nodeKeys.length]!;
    const start = Bun.nanoseconds();
    db.getNodeByKey(key);
    seqTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable("Sequential keys", seqTracker.getStats());

  // Missing keys
  const missingTracker = new LatencyTracker();
  for (let i = 0; i < Math.min(iterations, 1000); i++) {
    const key = `nonexistent.key.${i}`;
    const start = Bun.nanoseconds();
    db.getNodeByKey(key);
    missingTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable("Missing keys", missingTracker.getStats());
}

// =============================================================================
// Node Operations Benchmarks
// =============================================================================

function benchmarkNodeOperations(
  db: Database,
  graph: GraphData,
  iterations: number
): void {
  logger.log("\n--- Node Operations ---");

  // node_exists
  const existsTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const nodeId = graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
    const start = Bun.nanoseconds();
    db.nodeExists(nodeId);
    existsTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable("nodeExists() random", existsTracker.getStats());

  // get_node_key
  const getKeyTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const nodeId = graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
    const start = Bun.nanoseconds();
    db.getNodeKey(nodeId);
    getKeyTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable("getNodeKey() random", getKeyTracker.getStats());

  // count_nodes
  const countTracker = new LatencyTracker();
  for (let i = 0; i < Math.min(iterations, 1000); i++) {
    const start = Bun.nanoseconds();
    db.countNodes();
    countTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable("countNodes()", countTracker.getStats());
}

// =============================================================================
// Edge Operations Benchmarks
// =============================================================================

function benchmarkEdgeOperations(
  db: Database,
  graph: GraphData,
  iterations: number
): void {
  logger.log("\n--- Edge Operations ---");

  // edge_exists
  const existsTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const src = graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
    const dst = graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
    const start = Bun.nanoseconds();
    db.edgeExists(src, graph.etypes.calls, dst);
    existsTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable("edgeExists() random", existsTracker.getStats());

  // count_edges
  const countTracker = new LatencyTracker();
  for (let i = 0; i < Math.min(iterations, 1000); i++) {
    const start = Bun.nanoseconds();
    db.countEdges();
    countTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable("countEdges()", countTracker.getStats());

  // get_out_degree
  const outDegreeTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const nodeId = graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
    const start = Bun.nanoseconds();
    db.getOutDegree(nodeId);
    outDegreeTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable("getOutDegree() random", outDegreeTracker.getStats());

  // get_in_degree
  const inDegreeTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const nodeId = graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
    const start = Bun.nanoseconds();
    db.getInDegree(nodeId);
    inDegreeTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable("getInDegree() random", inDegreeTracker.getStats());
}

// =============================================================================
// Traversal Benchmarks
// =============================================================================

function benchmarkTraversals(
  db: Database,
  graph: GraphData,
  iterations: number
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
    `  Worst-case out-degree: ${worstOutDegree}, in-degree: ${worstInDegree}`
  );

  // Uniform random - outgoing
  const uniformOutTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const node = graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
    const start = Bun.nanoseconds();
    const edges = db.getOutEdges(node);
    uniformOutTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable("getOutEdges() uniform random", uniformOutTracker.getStats());

  // Uniform random - incoming
  const uniformInTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const node = graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
    const start = Bun.nanoseconds();
    const edges = db.getInEdges(node);
    uniformInTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable("getInEdges() uniform random", uniformInTracker.getStats());

  // Hub nodes only
  if (graph.hubNodes.length > 0) {
    const hubOutTracker = new LatencyTracker();
    for (let i = 0; i < iterations; i++) {
      const node = graph.hubNodes[Math.floor(Math.random() * graph.hubNodes.length)]!;
      const start = Bun.nanoseconds();
      const edges = db.getOutEdges(node);
      hubOutTracker.record(Bun.nanoseconds() - start);
    }
    printLatencyTable("getOutEdges() hub nodes", hubOutTracker.getStats());

    const hubInTracker = new LatencyTracker();
    for (let i = 0; i < iterations; i++) {
      const node = graph.hubNodes[Math.floor(Math.random() * graph.hubNodes.length)]!;
      const start = Bun.nanoseconds();
      const edges = db.getInEdges(node);
      hubInTracker.record(Bun.nanoseconds() - start);
    }
    printLatencyTable("getInEdges() hub nodes", hubInTracker.getStats());
  }

  // Worst-case node
  const worstOutTracker = new LatencyTracker();
  for (let i = 0; i < Math.min(iterations, 1000); i++) {
    const start = Bun.nanoseconds();
    const edges = db.getOutEdges(worstOutNode);
    worstOutTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable(
    `getOutEdges() worst-case (deg=${worstOutDegree})`,
    worstOutTracker.getStats()
  );
}

// =============================================================================
// Property Benchmarks
// =============================================================================

function benchmarkProperties(
  db: Database,
  graph: GraphData,
  iterations: number
): void {
  logger.log("\n--- Property Operations ---");

  // Setup: Add some properties
  const propKey = db.getOrCreatePropkey("name");
  const numNodesWithProps = Math.min(1000, graph.nodeIds.length);
  
  db.begin();
  for (let i = 0; i < numNodesWithProps; i++) {
    const nodeId = graph.nodeIds[i]!;
    db.setNodeProp(nodeId, propKey, {
      propType: "String",
      stringValue: `Node_${nodeId}`,
    });
  }
  db.commit();

  // Get property
  const getTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const nodeId = graph.nodeIds[Math.floor(Math.random() * numNodesWithProps)]!;
    const start = Bun.nanoseconds();
    db.getNodeProp(nodeId, propKey);
    getTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable("getNodeProp() random", getTracker.getStats());

  // Get all properties
  const getAllTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const nodeId = graph.nodeIds[Math.floor(Math.random() * numNodesWithProps)]!;
    const start = Bun.nanoseconds();
    db.getNodeProps(nodeId);
    getAllTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable("getNodeProps() random", getAllTracker.getStats());
}

// =============================================================================
// Write Performance Benchmark
// =============================================================================

function benchmarkWrites(
  db: Database,
  graph: GraphData,
  iterations: number
): void {
  logger.log("\n--- Write Performance ---");

  // Checkpoint before writes
  db.optimize();

  // Batch transactions
  const batchSizes = [10, 100, 1000];
  for (const batchSize of batchSizes) {
    const tracker = new LatencyTracker();
    const batches = Math.min(Math.floor(iterations / batchSize), 20); // Reduced to avoid WAL overflow
    for (let b = 0; b < batches; b++) {
      const start = Bun.nanoseconds();
      db.begin();
      for (let i = 0; i < batchSize; i++) {
        db.createNode(`bench:batch${batchSize}:${b}:${i}`);
      }
      db.commit();
      tracker.record(Bun.nanoseconds() - start);
      
      // Checkpoint periodically
      if (b > 0 && b % 5 === 0) {
        db.optimize();
      }
    }
    const st = tracker.getStats();
    const opsPerSec =
      st.sum > 0 ? (batchSize * st.count) / (st.sum / 1_000_000_000) : 0;
    const label = `Batch of ${batchSize.toString().padStart(4)} nodes`.padEnd(45);
    logger.log(
      `${label} p50=${formatLatency(st.p50).padStart(10)} p95=${formatLatency(st.p95).padStart(10)} (${formatNumber(Math.round(opsPerSec))} nodes/sec)`
    );
    db.optimize();
  }

  // Edge creation
  logger.log("\n--- Edge Creation ---");
  for (const batchSize of batchSizes) {
    const tracker = new LatencyTracker();
    const batches = Math.min(Math.floor(iterations / batchSize), 20); // Reduced to avoid WAL overflow
    for (let b = 0; b < batches; b++) {
      const start = Bun.nanoseconds();
      db.begin();
      for (let i = 0; i < batchSize; i++) {
        const src = graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
        const dst = graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
        if (src !== dst) {
          db.addEdge(src, graph.etypes.calls, dst);
        }
      }
      db.commit();
      tracker.record(Bun.nanoseconds() - start);
      
      // Checkpoint periodically
      if (b > 0 && b % 5 === 0) {
        db.optimize();
      }
    }
    const st = tracker.getStats();
    const opsPerSec =
      st.sum > 0 ? (batchSize * st.count) / (st.sum / 1_000_000_000) : 0;
    const label = `Batch of ${batchSize.toString().padStart(4)} edges`.padEnd(45);
    logger.log(
      `${label} p50=${formatLatency(st.p50).padStart(10)} p95=${formatLatency(st.p95).padStart(10)} (${formatNumber(Math.round(opsPerSec))} edges/sec)`
    );
    db.optimize();
  }
}

// =============================================================================
// Vector Operations Benchmark
// =============================================================================

function benchmarkVectorOperations(
  db: Database,
  graph: GraphData,
  iterations: number
): void {
  logger.log("\n--- Vector Operations ---");

  const vectorPropKey = db.getOrCreatePropkey("embedding");
  const dimensions = 128;
  const numNodesWithVectors = Math.min(1000, graph.nodeIds.length);

  // Generate random vectors
  const vectors: number[][] = [];
  for (let i = 0; i < numNodesWithVectors; i++) {
    const vec: number[] = [];
    for (let d = 0; d < dimensions; d++) {
      vec.push(Math.random());
    }
    vectors.push(vec);
  }

  // Set vectors
  const setTracker = new LatencyTracker();
  db.begin();
  for (let i = 0; i < numNodesWithVectors; i++) {
    const nodeId = graph.nodeIds[i]!;
    const start = Bun.nanoseconds();
    db.setNodeVector(nodeId, vectorPropKey, vectors[i]!);
    setTracker.record(Bun.nanoseconds() - start);
  }
  db.commit();
  printLatencyTable(`setNodeVector() (${dimensions}D)`, setTracker.getStats());

  // Get vectors
  const getTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const nodeId = graph.nodeIds[Math.floor(Math.random() * numNodesWithVectors)]!;
    const start = Bun.nanoseconds();
    db.getNodeVector(nodeId, vectorPropKey);
    getTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable(`getNodeVector() (${dimensions}D)`, getTracker.getStats());

  // Has vector
  const hasTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const nodeId = graph.nodeIds[Math.floor(Math.random() * graph.nodeIds.length)]!;
    const start = Bun.nanoseconds();
    db.hasNodeVector(nodeId, vectorPropKey);
    hasTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable("hasNodeVector()", hasTracker.getStats());
}

// =============================================================================
// List Operations Benchmark
// =============================================================================

function benchmarkListOperations(
  db: Database,
  graph: GraphData,
  iterations: number
): void {
  logger.log("\n--- List Operations ---");

  // list_nodes
  const listNodesTracker = new LatencyTracker();
  const listIters = Math.min(iterations, 100);
  for (let i = 0; i < listIters; i++) {
    const start = Bun.nanoseconds();
    const nodes = db.listNodes();
    listNodesTracker.record(Bun.nanoseconds() - start);
  }
  const listNodesStats = listNodesTracker.getStats();
  const nodesPerSec = listNodesStats.sum > 0
    ? (graph.nodeIds.length * listNodesStats.count) / (listNodesStats.sum / 1_000_000_000)
    : 0;
  logger.log(
    `${"listNodes() full iteration".padEnd(45)} p50=${formatLatency(listNodesStats.p50).padStart(10)} p95=${formatLatency(listNodesStats.p95).padStart(10)} (${formatNumber(Math.round(nodesPerSec))} nodes/sec)`
  );

  // list_edges
  const listEdgesTracker = new LatencyTracker();
  const listEdgeIters = Math.min(iterations, 50);
  for (let i = 0; i < listEdgeIters; i++) {
    const start = Bun.nanoseconds();
    const edges = db.listEdges();
    listEdgesTracker.record(Bun.nanoseconds() - start);
  }
  const listEdgesStats = listEdgesTracker.getStats();
  logger.log(
    `${"listEdges() full iteration".padEnd(45)} p50=${formatLatency(listEdgesStats.p50).padStart(10)} p95=${formatLatency(listEdgesStats.p95).padStart(10)}`
  );
}

// =============================================================================
// Schema Operations Benchmark
// =============================================================================

function benchmarkSchemaOperations(
  db: Database,
  iterations: number
): void {
  logger.log("\n--- Schema Operations ---");

  // getOrCreateEtype (mostly gets)
  const etypeTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const name = `EDGE_TYPE_${i % 10}`;
    const start = Bun.nanoseconds();
    db.getOrCreateEtype(name);
    etypeTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable("getOrCreateEtype() (90% hits)", etypeTracker.getStats());

  // getOrCreatePropkey
  const propkeyTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const name = `prop_key_${i % 10}`;
    const start = Bun.nanoseconds();
    db.getOrCreatePropkey(name);
    propkeyTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable("getOrCreatePropkey() (90% hits)", propkeyTracker.getStats());

  // getOrCreateLabel
  const labelTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const name = `Label_${i % 10}`;
    const start = Bun.nanoseconds();
    db.getOrCreateLabel(name);
    labelTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable("getOrCreateLabel() (90% hits)", labelTracker.getStats());
}

// =============================================================================
// Transaction Overhead Benchmark
// =============================================================================

function benchmarkTransactionOverhead(db: Database, iterations: number): void {
  logger.log("\n--- Transaction Overhead ---");

  // Begin + Commit (empty)
  const emptyTxTracker = new LatencyTracker();
  for (let i = 0; i < Math.min(iterations, 1000); i++) {
    const start = Bun.nanoseconds();
    db.begin();
    db.commit();
    emptyTxTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable("Empty transaction (begin+commit)", emptyTxTracker.getStats());

  // Begin + Rollback
  const rollbackTracker = new LatencyTracker();
  for (let i = 0; i < Math.min(iterations, 1000); i++) {
    const start = Bun.nanoseconds();
    db.begin();
    db.rollback();
    rollbackTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable("Empty transaction (begin+rollback)", rollbackTracker.getStats());
}

// =============================================================================
// Main
// =============================================================================

async function runBenchmarks(config: BenchConfig) {
  logger = new Logger(config.outputFile);

  const now = new Date();
  logger.log("=".repeat(120));
  logger.log("RayDB NAPI Bindings Benchmark");
  logger.log("=".repeat(120));
  logger.log(`Date: ${now.toISOString()}`);
  logger.log(`Nodes: ${formatNumber(config.nodes)}`);
  logger.log(`Edges: ${formatNumber(config.edges)}`);
  logger.log(`Iterations: ${formatNumber(config.iterations)}`);
  logger.log(`Keep database: ${config.keepDb}`);
  logger.log("=".repeat(120));

  const dbPath = join(tmpdir(), `ray-napi-bench-${Date.now()}.raydb`);

  try {
    logger.log("\n[1/11] Opening database...");
    const db = Database.open(dbPath);
    logger.log(`  Database opened at: ${dbPath}`);

    logger.log("\n[2/11] Building graph...");
    const startBuild = performance.now();
    const graph = buildRealisticGraph(db, config);
    logger.log(`  Built in ${(performance.now() - startBuild).toFixed(0)}ms`);

    // Degree stats
    const degrees = [...graph.outDegree.values()].sort((a, b) => b - a);
    const avgDegree = degrees.reduce((a, b) => a + b, 0) / degrees.length;
    logger.log(
      `  Avg out-degree: ${avgDegree.toFixed(1)}, Top 5: ${degrees.slice(0, 5).join(", ")}`
    );

    logger.log("\n[3/11] Compacting...");
    const startCompact = performance.now();
    db.optimize();
    logger.log(`  Compacted in ${(performance.now() - startCompact).toFixed(0)}ms`);

    const dbStats = db.stats();
    logger.log(
      `  Snapshot: ${formatNumber(Number(dbStats.snapshotNodes))} nodes, ${formatNumber(Number(dbStats.snapshotEdges))} edges`
    );

    logger.log("\n[4/11] Key lookup benchmarks...");
    benchmarkKeyLookups(db, graph, config.iterations);

    logger.log("\n[5/11] Node operation benchmarks...");
    benchmarkNodeOperations(db, graph, config.iterations);

    logger.log("\n[6/11] Edge operation benchmarks...");
    benchmarkEdgeOperations(db, graph, config.iterations);

    logger.log("\n[7/11] Traversal benchmarks...");
    benchmarkTraversals(db, graph, config.iterations);

    logger.log("\n[8/11] Property benchmarks...");
    benchmarkProperties(db, graph, config.iterations);

    logger.log("\n[9/11] Vector operation benchmarks...");
    benchmarkVectorOperations(db, graph, config.iterations);

    logger.log("\n[10/11] List operation benchmarks...");
    benchmarkListOperations(db, graph, config.iterations);

    logger.log("\n[11/11] Schema and transaction benchmarks...");
    benchmarkSchemaOperations(db, config.iterations);
    benchmarkTransactionOverhead(db, config.iterations);

    // Write benchmarks (modifies database)
    logger.log("\n[Bonus] Write benchmarks...");
    benchmarkWrites(db, graph, config.iterations);

    // Final compaction and stats
    db.optimize();
    
    // Database size
    try {
      const stats = await stat(dbPath);
      logger.log(`\n--- Database Size ---`);
      logger.log(`  File size: ${formatBytes(stats.size)}`);
      logger.log(`  Bytes per node: ${(stats.size / config.nodes).toFixed(1)}`);
      logger.log(`  Bytes per edge: ${(stats.size / config.edges).toFixed(1)}`);
    } catch (e) {
      // Ignore
    }

    db.close();
  } finally {
    if (config.keepDb) {
      logger.log(`\nDatabase preserved at: ${dbPath}`);
    } else {
      await rm(dbPath, { force: true });
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
