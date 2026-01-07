/**
 * Memgraph Batch Write Stress Test
 * 
 * Tests how Memgraph handles increasingly large batch sizes
 * to find optimal throughput.
 */

import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import {
  addEdge,
  beginTx,
  closeGraphDB,
  commit,
  createNode,
  defineEtype,
  openGraphDB,
  type NodeID,
} from "../src/index.ts";

import { MemgraphClient } from "./memgraph/client.ts";
import {
  getMemgraphUri,
  startMemgraph,
  stopMemgraph,
} from "./memgraph/docker.ts";

const TOTAL_NODES = 100_000;
const TOTAL_EDGES = 500_000;
const BATCH_SIZES = [100, 500, 1000, 2500, 5000, 10000, 25000, 50000, 100000];

async function testRayDBBatch(batchSize: number): Promise<number> {
  const tempDir = await mkdtemp(join(tmpdir(), "raydb-batch-"));
  const db = await openGraphDB(tempDir);

  const start = Bun.nanoseconds();
  for (let batch = 0; batch < TOTAL_NODES; batch += batchSize) {
    const tx = beginTx(db);
    const end = Math.min(batch + batchSize, TOTAL_NODES);
    for (let i = batch; i < end; i++) {
      createNode(tx, { key: `node-${i}` });
    }
    await commit(tx);
  }
  const elapsed = Bun.nanoseconds() - start;

  await closeGraphDB(db);
  await rm(tempDir, { recursive: true });

  return elapsed;
}

async function testMemgraphBatch(client: MemgraphClient, batchSize: number): Promise<number> {
  await client.clearDatabase();
  await client.createIndex();

  const start = Bun.nanoseconds();
  for (let batch = 0; batch < TOTAL_NODES; batch += batchSize) {
    const end = Math.min(batch + batchSize, TOTAL_NODES);
    const keys: string[] = [];
    for (let i = batch; i < end; i++) {
      keys.push(`node-${i}`);
    }
    await client.createNodes(keys);
  }
  const elapsed = Bun.nanoseconds() - start;

  return elapsed;
}

function formatTime(ns: number): string {
  if (ns < 1_000_000) return `${(ns / 1000).toFixed(2)}us`;
  if (ns < 1_000_000_000) return `${(ns / 1_000_000).toFixed(2)}ms`;
  return `${(ns / 1_000_000_000).toFixed(2)}s`;
}

function formatThroughput(count: number, ns: number): string {
  const seconds = ns / 1_000_000_000;
  const perSec = count / seconds;
  if (perSec >= 1_000_000) return `${(perSec / 1_000_000).toFixed(2)}M/s`;
  if (perSec >= 1_000) return `${(perSec / 1_000).toFixed(2)}K/s`;
  return `${perSec.toFixed(0)}/s`;
}

// Pre-create nodes and return their IDs for edge testing
async function setupNodesForEdgeTest(): Promise<{ rayNodeIds: NodeID[]; memNodeIds: number[] }> {
  // RayDB setup
  const tempDir = await mkdtemp(join(tmpdir(), "raydb-edge-setup-"));
  const db = await openGraphDB(tempDir);
  const rayNodeIds: NodeID[] = [];
  
  const tx = beginTx(db);
  for (let i = 0; i < 10000; i++) {
    rayNodeIds.push(createNode(tx, { key: `node-${i}` }));
  }
  await commit(tx);
  await closeGraphDB(db);
  await rm(tempDir, { recursive: true });

  // Memgraph setup
  const client = new MemgraphClient(getMemgraphUri());
  await client.clearDatabase();
  await client.createIndex();
  const keys = Array.from({ length: 10000 }, (_, i) => `node-${i}`);
  const keyToId = await client.createNodes(keys);
  const memNodeIds = keys.map(k => keyToId.get(k)!);
  await client.close();

  return { rayNodeIds, memNodeIds };
}

async function testRayDBEdgeBatch(batchSize: number, nodeCount: number): Promise<number> {
  const tempDir = await mkdtemp(join(tmpdir(), "raydb-edge-"));
  const db = await openGraphDB(tempDir);
  
  // Create nodes first
  const nodeIds: NodeID[] = [];
  let etype: number;
  {
    const tx = beginTx(db);
    etype = defineEtype(tx, "CONNECTS");
    for (let i = 0; i < nodeCount; i++) {
      nodeIds.push(createNode(tx, { key: `node-${i}` }));
    }
    await commit(tx);
  }

  // Now benchmark edge creation
  const start = Bun.nanoseconds();
  let edgesCreated = 0;
  while (edgesCreated < TOTAL_EDGES) {
    const tx = beginTx(db);
    const batchEnd = Math.min(edgesCreated + batchSize, TOTAL_EDGES);
    while (edgesCreated < batchEnd) {
      const src = nodeIds[Math.floor(Math.random() * nodeIds.length)]!;
      const dst = nodeIds[Math.floor(Math.random() * nodeIds.length)]!;
      if (src !== dst) {
        addEdge(tx, src, etype, dst);
        edgesCreated++;
      }
    }
    await commit(tx);
  }
  const elapsed = Bun.nanoseconds() - start;

  await closeGraphDB(db);
  await rm(tempDir, { recursive: true });

  return elapsed;
}

async function testMemgraphEdgeBatch(client: MemgraphClient, batchSize: number, nodeCount: number): Promise<number> {
  await client.clearDatabase();
  await client.createIndex();
  
  // Create nodes first
  const keys = Array.from({ length: nodeCount }, (_, i) => `node-${i}`);
  const keyToId = await client.createNodes(keys);
  const nodeIds = keys.map(k => keyToId.get(k)!);

  // Now benchmark edge creation
  const start = Bun.nanoseconds();
  let edgesCreated = 0;
  while (edgesCreated < TOTAL_EDGES) {
    const edges: { src: number; dst: number; type: string }[] = [];
    const batchEnd = Math.min(edgesCreated + batchSize, TOTAL_EDGES);
    while (edgesCreated < batchEnd) {
      const srcIdx = Math.floor(Math.random() * nodeIds.length);
      const dstIdx = Math.floor(Math.random() * nodeIds.length);
      if (srcIdx !== dstIdx) {
        edges.push({
          src: nodeIds[srcIdx]!,
          dst: nodeIds[dstIdx]!,
          type: "CONNECTS",
        });
        edgesCreated++;
      }
    }
    await client.createEdges(edges);
  }
  const elapsed = Bun.nanoseconds() - start;

  return elapsed;
}

async function main() {
  console.log("================================================================================");
  console.log("Memgraph Batch Write Stress Test");
  console.log(`Total nodes: ${TOTAL_NODES.toLocaleString()}`);
  console.log(`Total edges: ${TOTAL_EDGES.toLocaleString()}`);
  console.log("================================================================================\n");

  // Start Memgraph
  console.log("--- Starting Memgraph ---");
  await startMemgraph();
  const client = new MemgraphClient(getMemgraphUri());

  // NODE BATCHING TEST
  console.log("\n--- Node Batch Results ---");
  console.log(`${"Batch Size".padEnd(12)} ${"RayDB Time".padStart(12)} ${"RayDB Tput".padStart(12)} ${"Memgraph Time".padStart(14)} ${"Memgraph Tput".padStart(14)} ${"Speedup".padStart(10)}`);
  console.log("-".repeat(80));

  for (const batchSize of BATCH_SIZES) {
    process.stdout.write(`${batchSize.toString().padEnd(12)} `);

    // Test RayDB
    const rayTime = await testRayDBBatch(batchSize);
    process.stdout.write(`${formatTime(rayTime).padStart(12)} ${formatThroughput(TOTAL_NODES, rayTime).padStart(12)} `);

    // Test Memgraph
    try {
      const memTime = await testMemgraphBatch(client, batchSize);
      const speedup = memTime / rayTime;
      console.log(`${formatTime(memTime).padStart(14)} ${formatThroughput(TOTAL_NODES, memTime).padStart(14)} ${speedup.toFixed(1).padStart(9)}x`);
    } catch (err) {
      console.log(`${"FAILED".padStart(14)} ${"-".padStart(14)} ${"-".padStart(10)}`);
      console.error(`  Error: ${err}`);
    }
  }

  // EDGE BATCHING TEST
  console.log("\n--- Edge Batch Results (10k nodes, 500k edges) ---");
  console.log(`${"Batch Size".padEnd(12)} ${"RayDB Time".padStart(12)} ${"RayDB Tput".padStart(12)} ${"Memgraph Time".padStart(14)} ${"Memgraph Tput".padStart(14)} ${"Speedup".padStart(10)}`);
  console.log("-".repeat(80));

  const edgeBatchSizes = [500, 1000, 2500, 5000, 10000, 25000, 50000];
  for (const batchSize of edgeBatchSizes) {
    process.stdout.write(`${batchSize.toString().padEnd(12)} `);

    // Test RayDB
    const rayTime = await testRayDBEdgeBatch(batchSize, 10000);
    process.stdout.write(`${formatTime(rayTime).padStart(12)} ${formatThroughput(TOTAL_EDGES, rayTime).padStart(12)} `);

    // Test Memgraph
    try {
      const memTime = await testMemgraphEdgeBatch(client, batchSize, 10000);
      const speedup = memTime / rayTime;
      console.log(`${formatTime(memTime).padStart(14)} ${formatThroughput(TOTAL_EDGES, memTime).padStart(14)} ${speedup.toFixed(1).padStart(9)}x`);
    } catch (err) {
      console.log(`${"FAILED".padStart(14)} ${"-".padStart(14)} ${"-".padStart(10)}`);
      console.error(`  Error: ${err}`);
    }
  }

  // Cleanup
  console.log("\n--- Cleanup ---");
  await client.close();
  await stopMemgraph();
}

// Handle Ctrl+C
process.on("SIGINT", async () => {
  console.log("\n\nInterrupted, cleaning up...");
  await stopMemgraph();
  process.exit(1);
});

main().catch(async (err) => {
  console.error("Error:", err);
  await stopMemgraph();
  process.exit(1);
});
