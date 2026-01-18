/**
 * Ray Database Vector Embeddings Benchmark
 *
 * Performance benchmarks for vector operations:
 * - Distance calculations (cosine, euclidean, dot product)
 * - IVF index training and search
 * - Vector insertion and deletion
 * - Search with and without index
 *
 * Usage:
 *   bun run bench/benchmark-vector.ts [options]
 *
 * Options:
 *   --vectors N        Number of vectors (default: 10000)
 *   --dimensions D     Vector dimensions (default: 768)
 *   --iterations I     Iterations for latency benchmarks (default: 1000)
 *   --output FILE      Output file path (default: bench/results/benchmark-vector-<timestamp>.txt)
 *   --no-output        Disable file output
 */

import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";

import {
  dotProduct,
  cosineDistance,
  squaredEuclidean,
  euclideanDistance,
  batchCosineDistance,
  batchSquaredEuclidean,
  batchDotProductDistance,
  findKNearest,
  MaxHeap,
} from "../src/vector/distance.ts";
import { normalize, l2Norm } from "../src/vector/normalize.ts";
import {
  createVectorStore,
  vectorStoreInsert,
  vectorStoreDelete,
  vectorStoreGet,
  vectorStoreStats,
} from "../src/vector/columnar-store.ts";
import {
  createIvfIndex,
  ivfAddTrainingVectors,
  ivfTrain,
  ivfInsert,
  ivfSearch,
  ivfStats,
  ivfBuildFromStore,
} from "../src/vector/ivf-index.ts";
import {
  createPQIndex,
  pqTrain,
  pqEncode,
  pqBuildDistanceTable,
  pqDistanceADC,
  pqSearch,
  pqStats,
} from "../src/vector/pq.ts";
import {
  createIvfPqIndex,
  ivfPqAddTrainingVectors,
  ivfPqTrain,
  ivfPqInsert,
  ivfPqSearch,
  ivfPqStats,
} from "../src/vector/ivf-pq.ts";
import type { VectorManifest, IvfIndex } from "../src/vector/types.ts";

// =============================================================================
// Configuration
// =============================================================================

interface BenchConfig {
  vectors: number;
  dimensions: number;
  iterations: number;
  outputFile: string | null;
}

function parseArgs(): BenchConfig {
  const args = process.argv.slice(2);

  const now = new Date();
  const timestamp = now.toISOString().replace(/[:.]/g, "-").slice(0, 19);
  const defaultOutput = join(
    dirname(import.meta.path),
    "results",
    `benchmark-vector-${timestamp}.txt`
  );

  const config: BenchConfig = {
    vectors: 10000,
    dimensions: 768,
    iterations: 1000,
    outputFile: defaultOutput,
  };

  for (let i = 0; i < args.length; i++) {
    switch (args[i]) {
      case "--vectors":
        config.vectors = Number.parseInt(args[++i] || "10000", 10);
        break;
      case "--dimensions":
        config.dimensions = Number.parseInt(args[++i] || "768", 10);
        break;
      case "--iterations":
        config.iterations = Number.parseInt(args[++i] || "1000", 10);
        break;
      case "--output":
        config.outputFile = args[++i] || null;
        break;
      case "--no-output":
        config.outputFile = null;
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
    `${name.padEnd(50)} p50=${formatLatency(stats.p50).padStart(10)} p95=${formatLatency(stats.p95).padStart(10)} p99=${formatLatency(stats.p99).padStart(10)} (${formatNumber(Math.round(opsPerSec))} ops/sec)`
  );
}

// =============================================================================
// Vector Generation
// =============================================================================

/**
 * Generate random vectors with specified dimensions
 */
function generateRandomVectors(
  count: number,
  dimensions: number,
  shouldNormalize: boolean = true
): Float32Array {
  const data = new Float32Array(count * dimensions);

  for (let i = 0; i < count; i++) {
    const offset = i * dimensions;
    let sumSq = 0;

    // Generate random values
    for (let d = 0; d < dimensions; d++) {
      const val = Math.random() * 2 - 1; // [-1, 1]
      data[offset + d] = val;
      sumSq += val * val;
    }

    // Normalize if requested
    if (shouldNormalize) {
      const norm = Math.sqrt(sumSq);
      if (norm > 0) {
        for (let d = 0; d < dimensions; d++) {
          data[offset + d] /= norm;
        }
      }
    }
  }

  return data;
}

/**
 * Generate a single random vector
 */
function generateRandomVector(
  dimensions: number,
  shouldNormalize: boolean = true
): Float32Array {
  const vec = new Float32Array(dimensions);
  let sumSq = 0;

  for (let d = 0; d < dimensions; d++) {
    const val = Math.random() * 2 - 1;
    vec[d] = val;
    sumSq += val * val;
  }

  if (shouldNormalize) {
    const norm = Math.sqrt(sumSq);
    if (norm > 0) {
      for (let d = 0; d < dimensions; d++) {
        vec[d] /= norm;
      }
    }
  }

  return vec;
}

// =============================================================================
// Distance Function Benchmarks
// =============================================================================

function benchmarkDistanceFunctions(config: BenchConfig): void {
  logger.log("\n--- Distance Function Benchmarks ---");

  const { dimensions, iterations } = config;

  // Pre-generate test vectors
  const vec1 = generateRandomVector(dimensions);
  const vec2 = generateRandomVector(dimensions);

  // Dot Product
  const dotTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const start = Bun.nanoseconds();
    dotProduct(vec1, vec2);
    dotTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable(`Dot product (${dimensions}D)`, dotTracker.getStats());

  // Cosine Distance
  const cosineTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const start = Bun.nanoseconds();
    cosineDistance(vec1, vec2);
    cosineTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable(`Cosine distance (${dimensions}D)`, cosineTracker.getStats());

  // Squared Euclidean
  const sqEucTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const start = Bun.nanoseconds();
    squaredEuclidean(vec1, vec2);
    sqEucTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable(
    `Squared Euclidean (${dimensions}D)`,
    sqEucTracker.getStats()
  );

  // Euclidean Distance (includes sqrt)
  const eucTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const start = Bun.nanoseconds();
    euclideanDistance(vec1, vec2);
    eucTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable(`Euclidean distance (${dimensions}D)`, eucTracker.getStats());

  // Normalization
  const normTracker = new LatencyTracker();
  const unnormalized = generateRandomVector(dimensions, false);
  for (let i = 0; i < iterations; i++) {
    const start = Bun.nanoseconds();
    normalize(unnormalized);
    normTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable(`Normalize (${dimensions}D)`, normTracker.getStats());

  // L2 Norm
  const l2Tracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const start = Bun.nanoseconds();
    l2Norm(vec1);
    l2Tracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable(`L2 norm (${dimensions}D)`, l2Tracker.getStats());
}

// =============================================================================
// Batch Distance Benchmarks
// =============================================================================

function benchmarkBatchDistance(config: BenchConfig): void {
  logger.log("\n--- Batch Distance Benchmarks ---");

  const { dimensions, iterations } = config;
  const batchSizes = [64, 256, 1024];

  const query = generateRandomVector(dimensions);

  for (const batchSize of batchSizes) {
    const rowGroupData = generateRandomVectors(batchSize, dimensions);

    // Batch Cosine
    const cosineTracker = new LatencyTracker();
    const iters = Math.min(iterations, 500);
    for (let i = 0; i < iters; i++) {
      const start = Bun.nanoseconds();
      batchCosineDistance(query, rowGroupData, dimensions, 0, batchSize);
      cosineTracker.record(Bun.nanoseconds() - start);
    }
    const cosStats = cosineTracker.getStats();
    const vecsPerSec =
      cosStats.sum > 0
        ? (batchSize * cosStats.count) / (cosStats.sum / 1_000_000_000)
        : 0;
    logger.log(
      `${"Batch cosine ".padEnd(20)}batch=${batchSize.toString().padStart(4)} p50=${formatLatency(cosStats.p50).padStart(10)} (${formatNumber(Math.round(vecsPerSec))} vectors/sec)`
    );

    // Batch Squared Euclidean
    const sqEucTracker = new LatencyTracker();
    for (let i = 0; i < iters; i++) {
      const start = Bun.nanoseconds();
      batchSquaredEuclidean(query, rowGroupData, dimensions, 0, batchSize);
      sqEucTracker.record(Bun.nanoseconds() - start);
    }
    const sqEucStats = sqEucTracker.getStats();
    const vecsPerSec2 =
      sqEucStats.sum > 0
        ? (batchSize * sqEucStats.count) / (sqEucStats.sum / 1_000_000_000)
        : 0;
    logger.log(
      `${"Batch sq. euclidean ".padEnd(20)}batch=${batchSize.toString().padStart(4)} p50=${formatLatency(sqEucStats.p50).padStart(10)} (${formatNumber(Math.round(vecsPerSec2))} vectors/sec)`
    );

    // Batch Dot Product
    const dotTracker = new LatencyTracker();
    for (let i = 0; i < iters; i++) {
      const start = Bun.nanoseconds();
      batchDotProductDistance(query, rowGroupData, dimensions, 0, batchSize);
      dotTracker.record(Bun.nanoseconds() - start);
    }
    const dotStats = dotTracker.getStats();
    const vecsPerSec3 =
      dotStats.sum > 0
        ? (batchSize * dotStats.count) / (dotStats.sum / 1_000_000_000)
        : 0;
    logger.log(
      `${"Batch dot product ".padEnd(20)}batch=${batchSize.toString().padStart(4)} p50=${formatLatency(dotStats.p50).padStart(10)} (${formatNumber(Math.round(vecsPerSec3))} vectors/sec)`
    );
  }
}

// =============================================================================
// findKNearest Benchmark
// =============================================================================

function benchmarkFindKNearest(config: BenchConfig): void {
  logger.log("\n--- findKNearest Benchmarks ---");

  const { iterations } = config;
  const arraySizes = [100, 1000, 10000];
  const ks = [10, 50, 100];

  for (const size of arraySizes) {
    // Pre-generate random distances
    const distances = new Float32Array(size);
    for (let i = 0; i < size; i++) {
      distances[i] = Math.random();
    }

    for (const k of ks) {
      if (k > size) continue;

      const tracker = new LatencyTracker();
      const iters = Math.min(iterations, 200);
      for (let i = 0; i < iters; i++) {
        const start = Bun.nanoseconds();
        findKNearest(distances, k);
        tracker.record(Bun.nanoseconds() - start);
      }
      printLatencyTable(`findKNearest(n=${size}, k=${k})`, tracker.getStats());
    }
  }
}

// =============================================================================
// MaxHeap Benchmark
// =============================================================================

function benchmarkMaxHeap(config: BenchConfig): void {
  logger.log("\n--- MaxHeap Benchmarks ---");

  const { iterations } = config;
  const heapSizes = [100, 500, 1000];

  for (const size of heapSizes) {
    // Benchmark push operations
    const pushTracker = new LatencyTracker();
    const iters = Math.min(iterations, 100);
    for (let i = 0; i < iters; i++) {
      const heap = new MaxHeap();
      const start = Bun.nanoseconds();
      for (let j = 0; j < size; j++) {
        heap.push(j, Math.random());
      }
      pushTracker.record(Bun.nanoseconds() - start);
    }
    const pushStats = pushTracker.getStats();
    const pushOpsPerSec =
      pushStats.sum > 0
        ? (size * pushStats.count) / (pushStats.sum / 1_000_000_000)
        : 0;
    logger.log(
      `${"MaxHeap push ".padEnd(25)}n=${size.toString().padStart(4)} p50=${formatLatency(pushStats.p50).padStart(10)} (${formatNumber(Math.round(pushOpsPerSec))} push/sec)`
    );

    // Benchmark push+pop (maintaining k elements)
    const k = Math.min(100, size);
    const maintainTracker = new LatencyTracker();
    for (let i = 0; i < iters; i++) {
      const heap = new MaxHeap();
      const start = Bun.nanoseconds();
      for (let j = 0; j < size; j++) {
        const dist = Math.random();
        if (heap.size < k) {
          heap.push(j, dist);
        } else if (dist < heap.peek()!.distance) {
          heap.pop();
          heap.push(j, dist);
        }
      }
      maintainTracker.record(Bun.nanoseconds() - start);
    }
    const maintainStats = maintainTracker.getStats();
    logger.log(
      `${"MaxHeap maintain top-k ".padEnd(25)}n=${size.toString().padStart(4)} p50=${formatLatency(maintainStats.p50).padStart(10)} k=${k}`
    );
  }
}

// =============================================================================
// Vector Store Benchmarks
// =============================================================================

function benchmarkVectorStore(config: BenchConfig): void {
  logger.log("\n--- Vector Store Benchmarks ---");

  const { vectors, dimensions, iterations } = config;

  // Create store
  const manifest = createVectorStore(dimensions, {
    metric: "cosine",
    rowGroupSize: 1024,
    normalize: true,
  });

  // Benchmark insertions
  logger.log("\n  Insertion benchmarks:");
  const insertTracker = new LatencyTracker();
  const vectorData = generateRandomVectors(vectors, dimensions);

  const insertStart = performance.now();
  for (let i = 0; i < vectors; i++) {
    const vec = vectorData.subarray(i * dimensions, (i + 1) * dimensions);
    const start = Bun.nanoseconds();
    vectorStoreInsert(manifest, i as any, vec);
    insertTracker.record(Bun.nanoseconds() - start);
  }
  const insertTime = performance.now() - insertStart;
  printLatencyTable(`Insert (${vectors} vectors)`, insertTracker.getStats());
  logger.log(
    `  Total insert time: ${insertTime.toFixed(0)}ms (${formatNumber(Math.round((vectors / insertTime) * 1000))} vectors/sec)`
  );

  // Benchmark lookups
  logger.log("\n  Lookup benchmarks:");
  const lookupTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const nodeId = Math.floor(Math.random() * vectors) as any;
    const start = Bun.nanoseconds();
    vectorStoreGet(manifest, nodeId);
    lookupTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable("Random lookup", lookupTracker.getStats());

  // Sequential lookups (cache-friendly)
  const seqLookupTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const nodeId = (i % vectors) as any;
    const start = Bun.nanoseconds();
    vectorStoreGet(manifest, nodeId);
    seqLookupTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable("Sequential lookup", seqLookupTracker.getStats());

  // Stats
  const stats = vectorStoreStats(manifest);
  logger.log(`\n  Store stats:`);
  logger.log(`    Total vectors: ${formatNumber(stats.totalVectors)}`);
  logger.log(`    Live vectors: ${formatNumber(stats.liveVectors)}`);
  logger.log(`    Fragments: ${stats.fragmentCount}`);
  logger.log(`    Row group size: ${stats.rowGroupSize}`);
  logger.log(
    `    Memory: ${formatBytes(stats.liveVectors * dimensions * 4)} (vectors only)`
  );
}

// =============================================================================
// IVF Index Benchmarks
// =============================================================================

function benchmarkIvfIndex(config: BenchConfig): void {
  logger.log("\n--- IVF Index Benchmarks ---");

  const { vectors, dimensions, iterations } = config;

  // Create and populate store
  const manifest = createVectorStore(dimensions, {
    metric: "cosine",
    rowGroupSize: 1024,
    normalize: true,
  });

  const vectorData = generateRandomVectors(vectors, dimensions);
  for (let i = 0; i < vectors; i++) {
    const vec = vectorData.subarray(i * dimensions, (i + 1) * dimensions);
    vectorStoreInsert(manifest, i as any, vec);
  }

  // Benchmark training
  logger.log("\n  Index training:");
  const nClusters = Math.min(256, Math.floor(Math.sqrt(vectors)));

  const index = createIvfIndex(dimensions, {
    nClusters,
    nProbe: 10,
    metric: "cosine",
  });

  const trainAddStart = Bun.nanoseconds();
  ivfAddTrainingVectors(index, vectorData, dimensions, vectors);
  const trainAddTime = Bun.nanoseconds() - trainAddStart;
  logger.log(
    `  Add training vectors: ${formatLatency(trainAddTime)} (${formatNumber(vectors)} vectors)`
  );

  const trainStart = Bun.nanoseconds();
  ivfTrain(index, dimensions);
  const trainTime = Bun.nanoseconds() - trainStart;
  logger.log(
    `  K-means training: ${formatLatency(trainTime)} (${nClusters} clusters)`
  );

  // Insert all vectors into trained index
  const insertStart = Bun.nanoseconds();
  for (let i = 0; i < vectors; i++) {
    const vec = vectorData.subarray(i * dimensions, (i + 1) * dimensions);
    ivfInsert(index, i, vec, dimensions);
  }
  const insertTime = Bun.nanoseconds() - insertStart;
  logger.log(
    `  Insert into index: ${formatLatency(insertTime)} (${formatNumber(vectors)} vectors)`
  );

  // Index stats
  const stats = ivfStats(index);
  logger.log(`\n  Index stats:`);
  logger.log(`    Clusters: ${stats.nClusters}`);
  logger.log(`    Total vectors: ${formatNumber(stats.totalVectors)}`);
  logger.log(
    `    Avg vectors/cluster: ${stats.avgVectorsPerCluster.toFixed(1)}`
  );
  logger.log(`    Empty clusters: ${stats.emptyClusterCount}`);
  logger.log(`    Min cluster size: ${stats.minClusterSize}`);
  logger.log(`    Max cluster size: ${stats.maxClusterSize}`);

  // Benchmark search
  logger.log("\n  Search benchmarks:");
  const ks = [10, 50, 100];
  const nProbes = [1, 5, 10, 20];

  for (const k of ks) {
    for (const nProbe of nProbes) {
      const searchTracker = new LatencyTracker();
      const searchIterations = Math.min(iterations, 200);

      for (let i = 0; i < searchIterations; i++) {
        const query = generateRandomVector(dimensions);
        const start = Bun.nanoseconds();
        ivfSearch(index, manifest, query, k, { nProbe });
        searchTracker.record(Bun.nanoseconds() - start);
      }

      printLatencyTable(
        `IVF search k=${k}, nProbe=${nProbe}`,
        searchTracker.getStats()
      );
    }
  }
}

// =============================================================================
// Brute Force vs IVF Comparison
// =============================================================================

function benchmarkBruteForceVsIvf(config: BenchConfig): void {
  logger.log("\n--- Brute Force vs IVF Comparison ---");

  const { dimensions, iterations } = config;
  const testSizes = [1000, 5000, 10000];
  const k = 10;
  const nProbe = 10;

  for (const size of testSizes) {
    logger.log(`\n  Dataset size: ${formatNumber(size)} vectors`);

    // Create store and index
    const manifest = createVectorStore(dimensions, {
      metric: "cosine",
      rowGroupSize: 1024,
      normalize: true,
    });

    const vectorData = generateRandomVectors(size, dimensions);
    for (let i = 0; i < size; i++) {
      const vec = vectorData.subarray(i * dimensions, (i + 1) * dimensions);
      vectorStoreInsert(manifest, i as any, vec);
    }

    // Build IVF index
    const nClusters = Math.min(128, Math.floor(Math.sqrt(size)));
    const index = createIvfIndex(dimensions, {
      nClusters,
      nProbe,
      metric: "cosine",
    });
    ivfAddTrainingVectors(index, vectorData, dimensions, size);
    ivfTrain(index, dimensions);
    for (let i = 0; i < size; i++) {
      const vec = vectorData.subarray(i * dimensions, (i + 1) * dimensions);
      ivfInsert(index, i, vec, dimensions);
    }

    const searchIterations = Math.min(iterations, 100);

    // Brute force search
    const bruteTracker = new LatencyTracker();
    for (let i = 0; i < searchIterations; i++) {
      const query = generateRandomVector(dimensions);
      const queryNorm = normalize(query);
      const start = Bun.nanoseconds();

      // Compute all distances
      const distances = new Float32Array(size);
      for (let j = 0; j < size; j++) {
        const vec = vectorStoreGet(manifest, j as any);
        if (vec) {
          distances[j] = cosineDistance(queryNorm, vec);
        }
      }
      findKNearest(distances, k);
      bruteTracker.record(Bun.nanoseconds() - start);
    }

    // IVF search
    const ivfTracker = new LatencyTracker();
    for (let i = 0; i < searchIterations; i++) {
      const query = generateRandomVector(dimensions);
      const start = Bun.nanoseconds();
      ivfSearch(index, manifest, query, k, { nProbe });
      ivfTracker.record(Bun.nanoseconds() - start);
    }

    const bruteStats = bruteTracker.getStats();
    const ivfStats = ivfTracker.getStats();
    const speedup =
      bruteStats.p50 > 0 && ivfStats.p50 > 0
        ? (bruteStats.p50 / ivfStats.p50).toFixed(1)
        : "N/A";

    logger.log(
      `    Brute force: p50=${formatLatency(bruteStats.p50).padStart(10)}`
    );
    logger.log(
      `    IVF (nProbe=${nProbe}): p50=${formatLatency(ivfStats.p50).padStart(10)}`
    );
    logger.log(`    Speedup: ${speedup}x`);
  }
}

// =============================================================================
// Dimension Scaling Benchmark
// =============================================================================

function benchmarkDimensionScaling(config: BenchConfig): void {
  logger.log("\n--- Dimension Scaling Benchmark ---");

  const { iterations } = config;
  const dimensionSizes = [128, 256, 384, 512, 768, 1024, 1536];

  logger.log("\n  Distance computation scaling:");
  for (const dims of dimensionSizes) {
    const vec1 = generateRandomVector(dims);
    const vec2 = generateRandomVector(dims);

    const tracker = new LatencyTracker();
    const iters = Math.min(iterations, 1000);
    for (let i = 0; i < iters; i++) {
      const start = Bun.nanoseconds();
      dotProduct(vec1, vec2);
      tracker.record(Bun.nanoseconds() - start);
    }

    const stats = tracker.getStats();
    const flopsPerOp = dims * 2; // multiply + add per dimension
    const gflops =
      stats.p50 > 0 ? (flopsPerOp / stats.p50) * 1_000_000_000 / 1_000_000_000 : 0;
    logger.log(
      `    ${dims}D: p50=${formatLatency(stats.p50).padStart(10)} (~${gflops.toFixed(2)} GFLOPS)`
    );
  }

  logger.log("\n  Batch distance scaling (batch=1024):");
  const batchSize = 1024;
  for (const dims of dimensionSizes) {
    const query = generateRandomVector(dims);
    const rowData = generateRandomVectors(batchSize, dims);

    const tracker = new LatencyTracker();
    const iters = Math.min(iterations, 200);
    for (let i = 0; i < iters; i++) {
      const start = Bun.nanoseconds();
      batchCosineDistance(query, rowData, dims, 0, batchSize);
      tracker.record(Bun.nanoseconds() - start);
    }

    const stats = tracker.getStats();
    const vecsPerSec =
      stats.sum > 0
        ? (batchSize * stats.count) / (stats.sum / 1_000_000_000)
        : 0;
    logger.log(
      `    ${dims}D: p50=${formatLatency(stats.p50).padStart(10)} (${formatNumber(Math.round(vecsPerSec))} vectors/sec)`
    );
  }
}

// =============================================================================
// PQ (Product Quantization) Benchmarks
// =============================================================================

function benchmarkPQ(config: BenchConfig): void {
  logger.log("\n--- Product Quantization Benchmarks ---");

  const { vectors, dimensions, iterations } = config;

  // Ensure dimensions divisible by numSubspaces
  const numSubspaces = dimensions <= 384 ? 48 : 96;
  if (dimensions % numSubspaces !== 0) {
    logger.log(`  Skipping PQ benchmark: ${dimensions}D not divisible by ${numSubspaces} subspaces`);
    return;
  }

  // Generate training data
  const vectorData = generateRandomVectors(vectors, dimensions);

  // Create and train PQ index
  logger.log("\n  PQ Training:");
  const pqIndex = createPQIndex(dimensions, {
    numSubspaces,
    numCentroids: 256,
    maxIterations: 15,
  });

  const trainStart = Bun.nanoseconds();
  pqTrain(pqIndex, vectorData, vectors);
  const trainTime = Bun.nanoseconds() - trainStart;
  logger.log(`  Train PQ: ${formatLatency(trainTime)} (${numSubspaces} subspaces, 256 centroids)`);

  // Encode vectors
  const encodeStart = Bun.nanoseconds();
  pqEncode(pqIndex, vectorData, vectors);
  const encodeTime = Bun.nanoseconds() - encodeStart;
  const encodeRate = vectors / (encodeTime / 1_000_000_000);
  logger.log(`  Encode ${formatNumber(vectors)} vectors: ${formatLatency(encodeTime)} (${formatNumber(Math.round(encodeRate))} vectors/sec)`);

  // Benchmark distance table building
  logger.log("\n  PQ Distance Table:");
  const query = generateRandomVector(dimensions);
  const tableTracker = new LatencyTracker();
  for (let i = 0; i < iterations; i++) {
    const start = Bun.nanoseconds();
    pqBuildDistanceTable(pqIndex, query);
    tableTracker.record(Bun.nanoseconds() - start);
  }
  printLatencyTable(`Build distance table (${numSubspaces}x256)`, tableTracker.getStats());

  // Benchmark ADC distance lookup
  logger.log("\n  PQ ADC Distance Computation:");
  const distTable = pqBuildDistanceTable(pqIndex, query);
  const adcTracker = new LatencyTracker();
  const numAdc = Math.min(vectors, 1000);
  for (let iter = 0; iter < Math.min(iterations, 100); iter++) {
    const start = Bun.nanoseconds();
    for (let i = 0; i < numAdc; i++) {
      pqDistanceADC(distTable, pqIndex.codes!, i * numSubspaces, numSubspaces, 256);
    }
    adcTracker.record(Bun.nanoseconds() - start);
  }
  const adcStats = adcTracker.getStats();
  const adcPerSec = adcStats.sum > 0 ? (numAdc * adcStats.count) / (adcStats.sum / 1_000_000_000) : 0;
  logger.log(`  ADC distance (${numAdc} vectors): p50=${formatLatency(adcStats.p50).padStart(10)} (${formatNumber(Math.round(adcPerSec))} distances/sec)`);

  // Compare with full distance computation
  const fullDistTracker = new LatencyTracker();
  for (let iter = 0; iter < Math.min(iterations, 100); iter++) {
    const start = Bun.nanoseconds();
    for (let i = 0; i < numAdc; i++) {
      const vec = vectorData.subarray(i * dimensions, (i + 1) * dimensions);
      squaredEuclidean(query, vec);
    }
    fullDistTracker.record(Bun.nanoseconds() - start);
  }
  const fullStats = fullDistTracker.getStats();
  const speedup = fullStats.p50 > 0 && adcStats.p50 > 0 ? (fullStats.p50 / adcStats.p50).toFixed(1) : "N/A";
  logger.log(`  Full distance (${numAdc} vectors): p50=${formatLatency(fullStats.p50).padStart(10)}`);
  logger.log(`  PQ ADC speedup: ${speedup}x`);

  // PQ stats
  const stats = pqStats(pqIndex);
  logger.log(`\n  PQ Stats:`);
  logger.log(`    Compression ratio: ${stats.compressionRatio.toFixed(1)}x`);
  logger.log(`    Code size: ${formatBytes(stats.codeSizeBytes)}`);
  logger.log(`    Centroid size: ${formatBytes(stats.centroidsSizeBytes)}`);
}

// =============================================================================
// IVF-PQ (Combined) Benchmarks
// =============================================================================

function benchmarkIvfPq(config: BenchConfig): void {
  logger.log("\n--- IVF-PQ Combined Index Benchmarks ---");

  const { vectors, dimensions, iterations } = config;

  // Ensure dimensions divisible by numSubspaces
  const numSubspaces = dimensions <= 384 ? 48 : 96;
  if (dimensions % numSubspaces !== 0) {
    logger.log(`  Skipping IVF-PQ benchmark: ${dimensions}D not divisible by ${numSubspaces} subspaces`);
    return;
  }

  // Create store and generate data
  const manifest = createVectorStore(dimensions, {
    metric: "cosine",
    rowGroupSize: 1024,
    normalize: true,
  });

  const vectorData = generateRandomVectors(vectors, dimensions);
  for (let i = 0; i < vectors; i++) {
    const vec = vectorData.subarray(i * dimensions, (i + 1) * dimensions);
    vectorStoreInsert(manifest, i as any, vec);
  }

  // Create IVF-PQ index (non-residual mode for better performance on small datasets)
  const nClusters = Math.min(128, Math.floor(Math.sqrt(vectors)));

  logger.log("\n  IVF-PQ Training (non-residual mode for speed):");
  const index = createIvfPqIndex(dimensions, {
    nClusters,
    nProbe: 10,
    metric: "cosine",
    pq: {
      numSubspaces,
      numCentroids: 256,
      maxIterations: 15,
    },
    useResiduals: false, // Non-residual mode allows single distance table
  });

  const trainAddStart = Bun.nanoseconds();
  ivfPqAddTrainingVectors(index, vectorData, dimensions, vectors);
  const trainAddTime = Bun.nanoseconds() - trainAddStart;
  logger.log(`  Add training vectors: ${formatLatency(trainAddTime)}`);

  const trainStart = Bun.nanoseconds();
  ivfPqTrain(index, dimensions);
  const trainTime = Bun.nanoseconds() - trainStart;
  logger.log(`  Train IVF-PQ: ${formatLatency(trainTime)} (${nClusters} clusters, ${numSubspaces} PQ subspaces)`);

  // Insert vectors
  const insertStart = Bun.nanoseconds();
  for (let i = 0; i < vectors; i++) {
    const vec = vectorData.subarray(i * dimensions, (i + 1) * dimensions);
    ivfPqInsert(index, i, vec, dimensions);
  }
  const insertTime = Bun.nanoseconds() - insertStart;
  logger.log(`  Insert ${formatNumber(vectors)} vectors: ${formatLatency(insertTime)}`);

  // Search benchmarks
  logger.log("\n  IVF-PQ Search:");
  const ks = [10, 50];
  const nProbes = [1, 5, 10];

  for (const k of ks) {
    for (const nProbe of nProbes) {
      const searchTracker = new LatencyTracker();
      const searchIterations = Math.min(iterations, 200);

      for (let i = 0; i < searchIterations; i++) {
        const query = generateRandomVector(dimensions);
        const start = Bun.nanoseconds();
        ivfPqSearch(index, manifest, query, k, { nProbe });
        searchTracker.record(Bun.nanoseconds() - start);
      }

      printLatencyTable(`IVF-PQ search k=${k}, nProbe=${nProbe}`, searchTracker.getStats());
    }
  }

  // Compare with standard IVF
  logger.log("\n  IVF-PQ vs IVF Comparison (k=10, nProbe=10):");
  
  // Standard IVF
  const ivfIndex = createIvfIndex(dimensions, { nClusters, nProbe: 10, metric: "cosine" });
  ivfAddTrainingVectors(ivfIndex, vectorData, dimensions, vectors);
  ivfTrain(ivfIndex, dimensions);
  for (let i = 0; i < vectors; i++) {
    const vec = vectorData.subarray(i * dimensions, (i + 1) * dimensions);
    ivfInsert(ivfIndex, i, vec, dimensions);
  }

  const ivfTracker = new LatencyTracker();
  const ivfPqTracker = new LatencyTracker();
  const compIterations = Math.min(iterations, 100);

  for (let i = 0; i < compIterations; i++) {
    const query = generateRandomVector(dimensions);

    const start1 = Bun.nanoseconds();
    ivfSearch(ivfIndex, manifest, query, 10, { nProbe: 10 });
    ivfTracker.record(Bun.nanoseconds() - start1);

    const start2 = Bun.nanoseconds();
    ivfPqSearch(index, manifest, query, 10, { nProbe: 10 });
    ivfPqTracker.record(Bun.nanoseconds() - start2);
  }

  const ivfS = ivfTracker.getStats();
  const ivfPqS = ivfPqTracker.getStats();
  const searchSpeedup = ivfS.p50 > 0 && ivfPqS.p50 > 0 ? (ivfS.p50 / ivfPqS.p50).toFixed(1) : "N/A";

  logger.log(`    IVF:     p50=${formatLatency(ivfS.p50).padStart(10)}`);
  logger.log(`    IVF-PQ:  p50=${formatLatency(ivfPqS.p50).padStart(10)}`);
  logger.log(`    Speedup: ${searchSpeedup}x`);

  // Stats
  const stats = ivfPqStats(index);
  logger.log(`\n  IVF-PQ Stats:`);
  logger.log(`    Total vectors: ${formatNumber(stats.totalVectors)}`);
  logger.log(`    Memory savings: ${stats.memorySavingsRatio.toFixed(1)}x`);
}

// =============================================================================
// Memory Usage Estimation
// =============================================================================

function reportMemoryUsage(config: BenchConfig): void {
  logger.log("\n--- Memory Usage Estimation ---");

  const { vectors, dimensions } = config;

  // Vector data
  const vectorBytes = vectors * dimensions * 4; // Float32
  logger.log(`  Vector data: ${formatBytes(vectorBytes)}`);
  logger.log(`    ${formatNumber(vectors)} vectors x ${dimensions}D x 4 bytes`);

  // IVF index overhead
  const nClusters = Math.floor(Math.sqrt(vectors));
  const centroidBytes = nClusters * dimensions * 4;
  const invertedListBytes = vectors * 4; // 4 bytes per vector ID
  const ivfTotalBytes = centroidBytes + invertedListBytes;
  logger.log(`\n  IVF index overhead: ${formatBytes(ivfTotalBytes)}`);
  logger.log(`    Centroids: ${formatBytes(centroidBytes)} (${nClusters} x ${dimensions}D)`);
  logger.log(`    Inverted lists: ${formatBytes(invertedListBytes)}`);

  // Total
  const totalBytes = vectorBytes + ivfTotalBytes;
  logger.log(`\n  Total estimated memory: ${formatBytes(totalBytes)}`);
  logger.log(`  Bytes per vector: ${((totalBytes / vectors)).toFixed(1)}`);
}

// =============================================================================
// Main
// =============================================================================

async function runBenchmarks(config: BenchConfig) {
  logger = new Logger(config.outputFile);

  const now = new Date();
  logger.log("=".repeat(120));
  logger.log("Ray Database Vector Embeddings Benchmark");
  logger.log("=".repeat(120));
  logger.log(`Date: ${now.toISOString()}`);
  logger.log(`Vectors: ${formatNumber(config.vectors)}`);
  logger.log(`Dimensions: ${config.dimensions}`);
  logger.log(`Iterations: ${formatNumber(config.iterations)}`);
  logger.log("=".repeat(120));

  logger.log("\n[1/11] Distance function benchmarks...");
  benchmarkDistanceFunctions(config);

  logger.log("\n[2/11] Batch distance benchmarks...");
  benchmarkBatchDistance(config);

  logger.log("\n[3/11] findKNearest benchmarks...");
  benchmarkFindKNearest(config);

  logger.log("\n[4/11] MaxHeap benchmarks...");
  benchmarkMaxHeap(config);

  logger.log("\n[5/11] Vector store benchmarks...");
  benchmarkVectorStore(config);

  logger.log("\n[6/11] IVF index benchmarks...");
  benchmarkIvfIndex(config);

  logger.log("\n[7/11] Brute force vs IVF comparison...");
  benchmarkBruteForceVsIvf(config);

  logger.log("\n[8/11] Product Quantization benchmarks...");
  benchmarkPQ(config);

  logger.log("\n[9/11] IVF-PQ combined index benchmarks...");
  benchmarkIvfPq(config);

  logger.log("\n[10/11] Dimension scaling benchmark...");
  benchmarkDimensionScaling(config);

  logger.log("\n[11/11] Memory usage estimation...");
  reportMemoryUsage(config);

  logger.log(`\n${"=".repeat(120)}`);
  logger.log("Vector benchmark complete.");
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
