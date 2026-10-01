/**
 * Benchmark figures for the homepage and the /docs/benchmarks pages.
 *
 * Every value is copied from a raw log in docs/benchmarks/results/ (see
 * docs/BENCHMARKS.md). Each dataset names its log; update both together.
 * Latencies are stored in nanoseconds.
 */

/** Latency percentiles in nanoseconds. */
export interface Percentiles {
	p50: number;
	p95: number;
}

/** A raw log plus the settings a reader needs before comparing it to another run. */
export interface BenchSource {
	/** File name in docs/benchmarks/results/ */
	log: string;
	/** Dataset and durability settings, taken from the log header */
	config: string;
}

/** Directory that holds every raw log, relative to the repository root. */
export const RESULTS_DIR = "docs/benchmarks/results";

// ---------------------------------------------------------------------------
// Graph latency: single_file_raw_bench (Rust) and benchmark_single_file_raw.py
// ---------------------------------------------------------------------------

export type GraphMetric =
	| "keyLookup"
	| "traverseOut"
	| "edgeExists"
	| "batchNodes"
	| "batchEdges"
	| "batchEdgesProps"
	| "getVector"
	| "hasVector"
	| "setVectors";

/** Display order and labels for the single-file raw benchmark. */
export const GRAPH_METRICS: { id: GraphMetric; label: string }[] = [
	{ id: "keyLookup", label: "Key lookup (random existing key)" },
	{ id: "traverseOut", label: "1-hop traversal (out, random node)" },
	{ id: "edgeExists", label: "Edge exists (random pair)" },
	{ id: "batchNodes", label: "Batch write (100 nodes)" },
	{ id: "batchEdges", label: "Batch write (100 edges)" },
	{ id: "batchEdgesProps", label: "Batch write (100 edges + props)" },
	{ id: "getVector", label: "get_node_vector() (random)" },
	{ id: "hasVector", label: "has_node_vector() (random)" },
	{ id: "setVectors", label: "Set vectors (batch of 100)" },
];

const GRAPH_CONFIG =
	"10k nodes, 50k edges, 3 edge types, 10 edge props, sync=normal, group commit off";

export const RUST_GRAPH_SOURCE: BenchSource = {
	log: "2026-02-04-single-file-raw-rust-edges-normal-nogc.txt",
	config: GRAPH_CONFIG,
};

/** Source: 2026-02-04-single-file-raw-rust-edges-normal-nogc.txt */
export const RUST_GRAPH: Record<GraphMetric, Percentiles> = {
	keyLookup: { p50: 125, p95: 291 },
	traverseOut: { p50: 208, p95: 292 },
	edgeExists: { p50: 83, p95: 125 },
	batchNodes: { p50: 34_080, p95: 56_540 },
	batchEdges: { p50: 40_250, p95: 65_580 },
	batchEdgesProps: { p50: 172_330, p95: 253_120 },
	getVector: { p50: 125, p95: 209 },
	hasVector: { p50: 42, p95: 84 },
	setVectors: { p50: 94_330, p95: 195_380 },
};

export const PYTHON_GRAPH_SOURCE: BenchSource = {
	log: "2026-02-04-single-file-raw-python-edges-normal-nogc.txt",
	config: GRAPH_CONFIG,
};

/** Source: 2026-02-04-single-file-raw-python-edges-normal-nogc.txt */
export const PYTHON_GRAPH: Record<GraphMetric, Percentiles> = {
	keyLookup: { p50: 208, p95: 334 },
	traverseOut: { p50: 458, p95: 708 },
	edgeExists: { p50: 167, p95: 209 },
	batchNodes: { p50: 49_710, p95: 57_960 },
	batchEdges: { p50: 53_960, p95: 64_210 },
	batchEdgesProps: { p50: 436_580, p95: 647_460 },
	getVector: { p50: 1_250, p95: 2_040 },
	hasVector: { p50: 167, p95: 208 },
	setVectors: { p50: 241_830, p95: 658_420 },
};

export interface SyncSweepRow {
	syncMode: "normal" | "full" | "off";
	/** Batch write (100 nodes) p50, group commit off */
	groupCommitOff: number;
	/** Batch write (100 nodes) p50, group commit on (2 ms window) */
	groupCommitOn: number;
}

/** Log name pattern; each cell maps to one file. */
export const RUST_SYNC_SWEEP_LOGS =
	"2026-02-04-single-file-raw-rust-edges-{normal,full,off}-{nogc,gc}.txt";

/** Source: 2026-02-04-single-file-raw-rust-edges-{normal,full,off}-{nogc,gc}.txt */
export const RUST_SYNC_SWEEP: SyncSweepRow[] = [
	{ syncMode: "normal", groupCommitOff: 34_080, groupCommitOn: 2_570_000 },
	{ syncMode: "full", groupCommitOff: 54_920, groupCommitOn: 61_420 },
	{ syncMode: "off", groupCommitOff: 29_250, groupCommitOn: 29_330 },
];

export interface MultiWriterRun {
	groupCommit: boolean;
	log: string;
	txPerSec: number;
	nodesPerSec: number;
	edgesPerSec: number;
}

export const MULTI_WRITER_CONFIG =
	"8 threads, 200 transactions per thread, 200 nodes per transaction, 1 edge per node, 3 edge types, 10 edge props, 1 GB WAL, sync=normal";

/** Source: 2026-02-04-multi-writer-throughput-normal-{nogc,gc}.txt */
export const MULTI_WRITER: MultiWriterRun[] = [
	{
		groupCommit: false,
		log: "2026-02-04-multi-writer-throughput-normal-nogc.txt",
		txPerSec: 724.42,
		nodesPerSec: 144_880,
		edgesPerSec: 144_880,
	},
	{
		groupCommit: true,
		log: "2026-02-04-multi-writer-throughput-normal-gc.txt",
		txPerSec: 868.44,
		nodesPerSec: 173_690,
		edgesPerSec: 173_690,
	},
];

export const SQLITE_BATCH_SOURCE: BenchSource = {
	log: "2026-02-04-sqlite-single-file-raw-edges-normal.txt",
	config:
		"10k nodes, 50k edges, 3 edge types, 10 edge props, WAL mode, synchronous=normal",
};

/** Batch write (100 nodes). Source: 2026-02-04-sqlite-single-file-raw-edges-normal.txt */
export const SQLITE_BATCH_NODES: Percentiles = { p50: 120_670, p95: 2_980_000 };

// ---------------------------------------------------------------------------
// Vector index: vector_bench (Rust)
// ---------------------------------------------------------------------------

export const VECTOR_SOURCE: BenchSource = {
	log: "2026-02-03-vector-bench-rust.txt",
	config:
		"10k vectors, 768 dims, 1k iterations, k=10, nProbe=10, cosine, IVF with 100 clusters",
};

/** Source: 2026-02-03-vector-bench-rust.txt */
export const VECTOR_INDEX = {
	set: { p50: 833, p95: 2_120 } satisfies Percentiles,
	get: { p50: 167, p95: 459 } satisfies Percentiles,
	search: { p50: 557_540, p95: 918_790 } satisfies Percentiles,
	/** One build, so there are no percentiles */
	buildIndex: 801_950_000,
};

// ---------------------------------------------------------------------------
// TypeScript fluent vs low-level API: bench-fluent-vs-lowlevel.ts
// ---------------------------------------------------------------------------

export interface OverheadRow {
	label: string;
	/** p50, nanoseconds */
	lowLevel: number;
	/** p50, nanoseconds */
	fluent: number;
	/** fluent / low-level, as printed by the benchmark */
	overhead: number;
}

export const TS_OVERHEAD_SOURCE: BenchSource = {
	log: "2026-02-04-bench-fluent-vs-lowlevel-edges-normal-nogc.txt",
	config:
		"1k nodes, 5k edges, 3 edge types, 10 edge props, 1k iterations, sync=normal, group commit off",
};

/** Source: 2026-02-04-bench-fluent-vs-lowlevel-edges-normal-nogc.txt */
export const TS_OVERHEAD: OverheadRow[] = [
	{
		label: "Insert (single node + props)",
		lowLevel: 7_710,
		fluent: 8_630,
		overhead: 1.12,
	},
	{
		label: "Key lookup (get, with props)",
		lowLevel: 208,
		fluent: 1_710,
		overhead: 8.21,
	},
	{
		label: "Key lookup (getRef, no props)",
		lowLevel: 208,
		fluent: 750,
		overhead: 3.61,
	},
	{
		label: "Key lookup (getId, id only)",
		lowLevel: 208,
		fluent: 417,
		overhead: 2.0,
	},
	{
		label: "1-hop traversal (count)",
		lowLevel: 875,
		fluent: 5_000,
		overhead: 5.71,
	},
	{
		label: "1-hop traversal (node ids)",
		lowLevel: 875,
		fluent: 4_630,
		overhead: 5.29,
	},
	{
		label: "1-hop traversal (toArray, with props)",
		lowLevel: 875,
		fluent: 6_290,
		overhead: 7.19,
	},
	{
		label: "Pathfinding BFS (max depth 5)",
		lowLevel: 6_670,
		fluent: 7_290,
		overhead: 1.09,
	},
];

// ---------------------------------------------------------------------------
// Homepage
// ---------------------------------------------------------------------------

export interface LatencyRow {
	label: string;
	detail: string;
	/** nanoseconds */
	p50: number;
	/** nanoseconds */
	p95: number;
}

/** Graph rows of the homepage chart. Source: RUST_GRAPH_SOURCE */
export const GRAPH_LATENCY_ROWS: LatencyRow[] = [
	{
		label: "Edge exists",
		detail: "random pair, 50k edges",
		...RUST_GRAPH.edgeExists,
	},
	{
		label: "Key lookup",
		detail: "random existing key",
		...RUST_GRAPH.keyLookup,
	},
	{
		label: "1-hop traversal",
		detail: "outgoing edges, random node",
		...RUST_GRAPH.traverseOut,
	},
	{
		label: "Commit 100 nodes",
		detail: "one transaction, WAL append",
		...RUST_GRAPH.batchNodes,
	},
	{
		label: "Commit 100 edges",
		detail: "one transaction, WAL append",
		...RUST_GRAPH.batchEdges,
	},
];

/** Vector row of the homepage chart. Source: VECTOR_SOURCE */
export const VECTOR_LATENCY_ROW: LatencyRow = {
	label: "Vector search",
	detail: "top-10 of 10k × 768d, IVF",
	...VECTOR_INDEX.search,
};

export const LATENCY_ROWS: LatencyRow[] = [
	...GRAPH_LATENCY_ROWS,
	VECTOR_LATENCY_ROW,
];

export const HEADLINE_STATS = [
	{ label: "Edge existence check", value: "83", unit: "ns" },
	{ label: "Key lookup", value: "125", unit: "ns" },
	{ label: "1-hop traversal", value: "208", unit: "ns" },
	{ label: "Commit 100 nodes", value: "34", unit: "µs" },
];

export const BENCH_ENVIRONMENT = [
	{ label: "Hardware", value: "Apple M4, 16 GB, Darwin 25.3" },
	{
		label: "Graph dataset",
		value: "10k nodes · 50k edges · 3 edge types · 10 props",
	},
	{ label: "Durability", value: "sync=normal · group commit off" },
];

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

/** Format nanoseconds with an adaptive unit: 83 ns, 34.1 µs, 558 µs, 1.2 ms. */
export function formatNs(ns: number): string {
	if (ns < 1_000) return `${Math.round(ns)} ns`;
	if (ns < 1_000_000) {
		const us = ns / 1_000;
		return `${us < 100 ? us.toFixed(1) : Math.round(us)} µs`;
	}
	return `${(ns / 1_000_000).toFixed(1)} ms`;
}

/** Format nanoseconds with the precision the raw logs use: 83 ns, 34.08 µs, 801.95 ms. */
export function formatNsExact(ns: number): string {
	if (ns < 1_000) return `${Math.round(ns)} ns`;
	if (ns < 1_000_000) return `${(ns / 1_000).toFixed(2)} µs`;
	return `${(ns / 1_000_000).toFixed(2)} ms`;
}

/** Format a per-second rate the way the raw logs do: 724.42/s, 144.88K/s. */
export function formatRate(perSec: number): string {
	if (perSec < 1_000) return `${perSec.toFixed(2)}/s`;
	return `${(perSec / 1_000).toFixed(2)}K/s`;
}

/** GitHub URL for a raw log, or for the results directory when `log` is omitted. */
export function resultsUrl(githubUrl: string, log?: string): string {
	return log
		? `${githubUrl}/blob/main/${RESULTS_DIR}/${log}`
		: `${githubUrl}/tree/main/${RESULTS_DIR}`;
}
