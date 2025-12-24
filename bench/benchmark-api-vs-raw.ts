/**
 * Ray API vs Raw GraphDB Benchmark
 *
 * Compares the high-level fluent API (ray/Query Builders/Traversal)
 * against the low-level GraphDB primitives for common read workloads.
 *
 * Usage:
 *   bun run bench/benchmark-api-vs-raw.ts [options]
 *
 * Options:
 *   --nodes N         Number of user nodes (default: 10000)
 *   --edges M         Number of edges (default: 50000)
 *   --iterations I    Iterations per benchmark (default: 10000)
 *   --output FILE     Output file path (default: bench/results/benchmark-api-vs-raw-<timestamp>.txt)
 *   --no-output       Disable file output
 *   --keep-db         Keep the database directory after benchmark (prints path)
 */

import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";

import {
  // High-level API
  ray,
  defineNode,
  defineEdge,
  prop,
  optional,
  type NodeRef,
  // Low-level API
  type GraphDB,
  type NodeID,
  type ETypeID,
  type PropKeyID,
  type PropValue,
  getNodeByKey,
  getNeighborsOut,
  beginTx,
  commit,
  addEdge,
  createNode,
  setNodeProp,
  PropValueTag,
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
}

function parseArgs(): BenchConfig {
  const args = process.argv.slice(2);

  const now = new Date();
  const timestamp = now.toISOString().replace(/[:.]/g, "-").slice(0, 19);
  const defaultOutput = join(
    dirname(import.meta.path),
    "results",
    `benchmark-api-vs-raw-${timestamp}.txt`,
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
}

function formatLatency(ns: number): string {
  if (ns < 1000) return `${ns.toFixed(0)}ns`;
  if (ns < 1_000_000) return `${(ns / 1000).toFixed(2)}µs`;
  return `${(ns / 1_000_000).toFixed(2)}ms`;
}

function formatNumber(n: number): string {
  return n.toLocaleString("en-US");
}

// =============================================================================
// Schema & Graph Data
// =============================================================================

const user = defineNode("user", {
  key: (id: string) => `user:${id}`,
  props: {
    name: prop.string("name"),
    email: prop.string("email"),
    age: optional(prop.int("age")),
  },
});

const knows = defineEdge("knows", {
  since: prop.int("since"),
});

interface GraphData {
  users: (NodeRef<typeof user> & { $id: NodeID; $key: string })[];
  userIds: NodeID[];
  userKeyArgs: string[];
  userFullKeys: string[];
  knowsEtypeId: ETypeID;
}

async function buildGraph(
  db: Awaited<ReturnType<typeof ray>>,
  config: BenchConfig,
): Promise<GraphData> {
  const users: (NodeRef<typeof user> & { $id: NodeID; $key: string })[] = [];
  const userIds: NodeID[] = [];
  const userKeyArgs: string[] = [];
  const userFullKeys: string[] = [];

  logger.log("Building graph with fluent API...");

  // Insert user nodes via fluent API
  for (let i = 0; i < config.nodes; i++) {
    const keyArg = `u${i}`;
    const fullKey = user.keyFn(keyArg);

    const u = await db
      .insert(user)
      .values({
        key: keyArg,
        name: `User ${i}`,
        email: `user${i}@example.com`,
        age: BigInt(20 + (i % 50)),
      })
      .returning();

    users.push(u as NodeRef<typeof user> & { $id: NodeID; $key: string });
    userIds.push(u.$id as NodeID);
    userKeyArgs.push(keyArg);
    userFullKeys.push(fullKey);

    if ((i + 1) % 1000 === 0 || i + 1 === config.nodes) {
      logger.log(`  Inserted ${formatNumber(i + 1)} / ${formatNumber(config.nodes)} users`);
    }
  }

  // Create edges using low-level API for speed
  const raw: GraphDB = db.$raw;
  const knowsEtypeId: ETypeID = knows._etypeId!;
  const batchSize = 5000;
  let edgesCreated = 0;
  let attempts = 0;
  const maxAttempts = config.edges * 3;

  logger.log("Creating edges (raw GraphDB)...");

  while (edgesCreated < config.edges && attempts < maxAttempts) {
    const tx = beginTx(raw);
    const batchTarget = Math.min(edgesCreated + batchSize, config.edges);

    while (edgesCreated < batchTarget && attempts < maxAttempts) {
      attempts++;
      const srcIdx = Math.floor(Math.random() * userIds.length);
      const dstIdx = Math.floor(Math.random() * userIds.length);
      const src = userIds[srcIdx]!;
      const dst = userIds[dstIdx]!;
      if (src === dst) continue;

      addEdge(tx, src, knowsEtypeId, dst);
      edgesCreated++;
    }

    await commit(tx);
    logger.log(
      `  Created ${formatNumber(edgesCreated)} / ${formatNumber(config.edges)} edges`,
    );
  }

  return {
    users,
    userIds,
    userKeyArgs,
    userFullKeys,
    knowsEtypeId,
  };
}

// =============================================================================
// Benchmarks
// =============================================================================

interface InsertUser {
  keyArg: string;
  name: string;
  email: string;
  age: bigint;
}

async function benchmarkRawInserts(
  db: GraphDB,
  values: InsertUser[],
): Promise<LatencyStats> {
  const tracker = new LatencyTracker();

  const propKeyIds = user._propKeyIds;
  if (!propKeyIds) {
    throw new Error("user._propKeyIds not initialized");
  }

  const nameKeyId = propKeyIds.get("name") as PropKeyID;
  const emailKeyId = propKeyIds.get("email") as PropKeyID;
  const ageKeyId = propKeyIds.get("age") as PropKeyID;

  for (const value of values) {
    const fullKey = user.keyFn(value.keyArg as never);
    const start = Bun.nanoseconds();
    const tx = beginTx(db);
    const nodeId = createNode(tx, { key: fullKey });
    setNodeProp(tx, nodeId, nameKeyId, {
      tag: PropValueTag.STRING,
      value: value.name,
    } as PropValue);
    setNodeProp(tx, nodeId, emailKeyId, {
      tag: PropValueTag.STRING,
      value: value.email,
    } as PropValue);
    setNodeProp(tx, nodeId, ageKeyId, {
      tag: PropValueTag.I64,
      value: value.age,
    } as PropValue);
    await commit(tx);
    tracker.record(Bun.nanoseconds() - start);
  }

  return tracker.getStats();
}

async function benchmarkFluentInserts(
  db: Awaited<ReturnType<typeof ray>>,
  values: InsertUser[],
): Promise<LatencyStats> {
  const tracker = new LatencyTracker();

  for (const value of values) {
    const start = Bun.nanoseconds();
    await db
      .insert(user)
      .values({
        key: value.keyArg,
        name: value.name,
        email: value.email,
        age: value.age,
      })
      .execute();
    tracker.record(Bun.nanoseconds() - start);
  }

  return tracker.getStats();
}

function benchmarkRawKeyLookups(
  db: GraphDB,
  keys: string[],
  iterations: number,
): LatencyStats {
  const tracker = new LatencyTracker();
  const n = keys.length;

  for (let i = 0; i < iterations; i++) {
    const idx = Math.floor(Math.random() * n);
    const key = keys[idx]!;
    const start = Bun.nanoseconds();
    getNodeByKey(db, key);
    tracker.record(Bun.nanoseconds() - start);
  }

  return tracker.getStats();
}

async function benchmarkFluentKeyLookups(
  db: Awaited<ReturnType<typeof ray>>,
  keyArgs: string[],
  iterations: number,
): Promise<LatencyStats> {
  const tracker = new LatencyTracker();
  const n = keyArgs.length;

  for (let i = 0; i < iterations; i++) {
    const idx = Math.floor(Math.random() * n);
    const keyArg = keyArgs[idx]!;
    const start = Bun.nanoseconds();
    await db.get(user, keyArg);
    tracker.record(Bun.nanoseconds() - start);
  }

  return tracker.getStats();
}

async function benchmarkFluentGetRef(
  db: Awaited<ReturnType<typeof ray>>,
  keyArgs: string[],
  iterations: number,
): Promise<LatencyStats> {
  const tracker = new LatencyTracker();
  const n = keyArgs.length;

  for (let i = 0; i < iterations; i++) {
    const idx = Math.floor(Math.random() * n);
    const keyArg = keyArgs[idx]!;
    const start = Bun.nanoseconds();
    await db.getRef(user, keyArg);
    tracker.record(Bun.nanoseconds() - start);
  }

  return tracker.getStats();
}

function benchmarkRawTraversal(
  db: GraphDB,
  nodeIds: NodeID[],
  etypeId: ETypeID,
  iterations: number,
): LatencyStats {
  const tracker = new LatencyTracker();
  const n = nodeIds.length;

  for (let i = 0; i < iterations; i++) {
    const idx = Math.floor(Math.random() * n);
    const nodeId = nodeIds[idx]!;
    const start = Bun.nanoseconds();
    let count = 0;
    for (const _edge of getNeighborsOut(db, nodeId, etypeId)) {
      count++;
    }
    tracker.record(Bun.nanoseconds() - start);
  }

  return tracker.getStats();
}

async function benchmarkFluentTraversal(
  db: Awaited<ReturnType<typeof ray>>,
  users: (NodeRef<typeof user> & { $id: NodeID })[],
  iterations: number,
): Promise<LatencyStats> {
  const tracker = new LatencyTracker();
  const n = users.length;

  for (let i = 0; i < iterations; i++) {
    const idx = Math.floor(Math.random() * n);
    const startNode = users[idx]!;
    const start = Bun.nanoseconds();
    await db.from(startNode).out(knows).count();
    tracker.record(Bun.nanoseconds() - start);
  }

  return tracker.getStats();
}

function printComparison(
  name: string,
  rawStats: LatencyStats,
  fluentStats: LatencyStats,
): void {
  const rawP50 = rawStats.p50 || 0;
  const fluentP50 = fluentStats.p50 || 0;
  const overhead = rawP50 > 0 && fluentP50 > 0
    ? (fluentP50 / rawP50).toFixed(2)
    : "N/A";

  logger.log(
    `${name.padEnd(30)} raw p50=${formatLatency(rawP50).padStart(10)}  fluent p50=${formatLatency(fluentP50).padStart(10)}  overhead=${overhead}x`,
  );
}

// =============================================================================
// Main
// =============================================================================

async function runBenchmarks(config: BenchConfig): Promise<void> {
  logger = new Logger(config.outputFile);

  const now = new Date();
  logger.log("=".repeat(120));
  logger.log("Ray API vs Raw GraphDB Benchmark");
  logger.log("=".repeat(120));
  logger.log(`Date: ${now.toISOString()}`);
  logger.log(`Nodes: ${formatNumber(config.nodes)}`);
  logger.log(`Edges: ${formatNumber(config.edges)}`);
  logger.log(`Iterations: ${formatNumber(config.iterations)}`);
  logger.log("=".repeat(120));

  const testDir = await mkdtemp(join(tmpdir(), "ray-api-vs-raw-"));

  try {
    const db = await ray(testDir, {
      nodes: [user],
      edges: [knows],
    });

    const graph = await buildGraph(db, config);

    // Prepare shared insert workload
    const insertValues: InsertUser[] = [];
    for (let i = 0; i < config.iterations; i++) {
      insertValues.push({
        keyArg: `bench${i}`,
        name: `Bench User ${i}`,
        email: `bench${i}@example.com`,
        age: BigInt(30 + (i % 10)),
      });
    }

    logger.log("Running insert benchmarks...");
    const rawInsertStats = await benchmarkRawInserts(db.$raw, insertValues);
    const fluentInsertStats = await benchmarkFluentInserts(db, insertValues);

    logger.log("Running key lookup benchmarks...");
    const rawKeyStats = benchmarkRawKeyLookups(
      db.$raw,
      graph.userFullKeys,
      config.iterations,
    );
    const fluentKeyStats = await benchmarkFluentKeyLookups(
      db,
      graph.userKeyArgs,
      config.iterations,
    );

    logger.log("Running getRef benchmarks...");
    const fluentGetRefStats = await benchmarkFluentGetRef(
      db,
      graph.userKeyArgs,
      config.iterations,
    );

    logger.log("Running 1-hop traversal benchmarks...");
    const rawTravStats = benchmarkRawTraversal(
      db.$raw,
      graph.userIds,
      graph.knowsEtypeId,
      config.iterations,
    );
    const fluentTravStats = await benchmarkFluentTraversal(
      db,
      graph.users,
      config.iterations,
    );

    logger.log("\n=== Results (lower is better) ===");
    printComparison("Insert (single)", rawInsertStats, fluentInsertStats);
    printComparison("Key lookup (get)", rawKeyStats, fluentKeyStats);
    printComparison("Key lookup (getRef)", rawKeyStats, fluentGetRefStats);
    printComparison("1-hop traversal", rawTravStats, fluentTravStats);

    await db.close();

    if (config.keepDb) {
      logger.log(`\nDatabase preserved at: ${testDir}`);
    } else {
      await rm(testDir, { recursive: true, force: true });
    }
  } finally {
    logger.log(`\n${"=".repeat(120)}`);
    logger.log("Benchmark complete.");
    logger.log("=".repeat(120));

    await logger.flush();
    if (logger.getOutputPath()) {
      console.log(`\nResults saved to: ${logger.getOutputPath()}`);
    }
  }
}

const config = parseArgs();
runBenchmarks(config).catch((err) => {
  console.error("Benchmark failed:", err);
  process.exit(1);
});
