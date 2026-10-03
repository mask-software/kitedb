/**
 * Benchmark figures for the homepage, the /docs/benchmarks pages and the docs
 * pages that cite a measured number.
 *
 * No number here is typed by hand. `bun run bench:data` (scripts/
 * benchmark-data.ts) turns the raw logs of one run of
 * ray-rs/scripts/bench-refresh.sh, in docs/benchmarks/results/, into
 * benchmark-data.gen.ts; this file picks rows from it and formats them. Each
 * dataset names its log. A row is the line of the round with the median
 * value, copied whole from the log. Latencies are in nanoseconds.
 *
 * To refresh: run the script on a quiet machine, commit the logs, run
 * `bun run bench:data`, and check the pages whose text describes a result.
 */

import { BENCH_LOGS, BENCH_STAMP } from "./benchmark-data.gen";

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
// Reading the generated data
// ---------------------------------------------------------------------------

type LogName = keyof typeof BENCH_LOGS;

interface LogRow {
	readonly fields: Readonly<Record<string, number>>;
	readonly line: string;
	readonly round: number;
}

interface LogData {
	readonly meta: Readonly<Record<string, string>>;
	readonly header: Readonly<Record<string, string>>;
	readonly rows: Readonly<Record<string, LogRow>>;
}

function logData(log: LogName): LogData {
	return BENCH_LOGS[log] as unknown as LogData;
}

/** One numeric field of a row; a missing row or field is an error. */
function field(log: LogName, row: string, name = "value"): number {
	const value = logData(log).rows[row]?.fields[name];
	if (value === undefined) {
		throw new Error(`${log} has no field "${name}" in row "${row}"`);
	}
	return value;
}

function percentiles(log: LogName, row: string): Percentiles {
	return { p50: field(log, row, "p50"), p95: field(log, row, "p95") };
}

/** A "Key: value" line the bench printed, or a "# key: value" header line. */
function text(
	log: LogName,
	key: string,
	from: "header" | "meta" = "header",
): string {
	const value = logData(log)[from][key];
	if (value === undefined) throw new Error(`${log} has no ${from} "${key}"`);
	return value;
}

/** First match of `pattern` in `value`; no match is an error. */
function pick(value: string, pattern: RegExp): string {
	const match = pattern.exec(value);
	if (!match) throw new Error(`"${value}" does not match ${pattern}`);
	return match[1];
}

/** 10000 -> "10k", 1000000 -> "1M", 768 -> "768". */
export function formatCount(n: number): string {
	if (n >= 1_000_000 && n % 100_000 === 0) return `${n / 1_000_000}M`;
	if (n >= 1_000 && n % 100 === 0) return `${n / 1_000}k`;
	return n.toLocaleString("en-US");
}

const MONTHS = [
	"January",
	"February",
	"March",
	"April",
	"May",
	"June",
	"July",
	"August",
	"September",
	"October",
	"November",
	"December",
];

/** The day of this refresh, as "October 3, 2026". */
export const BENCH_DATE = (() => {
	const [year, month, day] = BENCH_STAMP.split("-").map(Number);
	return `${MONTHS[month - 1]} ${day}, ${year}`;
})();

// ---------------------------------------------------------------------------
// Logs of this refresh
// ---------------------------------------------------------------------------

const LOG = {
	rust: `${BENCH_STAMP}-single-file-raw-rust-mvcc-normal.txt`,
	rustNoMvcc: `${BENCH_STAMP}-single-file-raw-rust-nomvcc-normal.txt`,
	python: `${BENCH_STAMP}-single-file-raw-python-mvcc-normal.txt`,
	ts: `${BENCH_STAMP}-bench-fluent-vs-lowlevel-mvcc-normal.txt`,
	sqlite: `${BENCH_STAMP}-sqlite-single-file-raw-edges-normal.txt`,
	vector: `${BENCH_STAMP}-vector-bench-rust.txt`,
	annIvf: `${BENCH_STAMP}-vector-ann-768d-ivf.txt`,
	annIvfPq: `${BENCH_STAMP}-vector-ann-768d-ivf-pq.txt`,
	open100k: `${BENCH_STAMP}-query-core-open-100k.txt`,
	open1m: `${BENCH_STAMP}-query-core-open-1m.txt`,
	paging: `${BENCH_STAMP}-query-core-paging.txt`,
	mvccOverhead: `${BENCH_STAMP}-mvcc-overhead.txt`,
	bulkMvcc: `${BENCH_STAMP}-bulk-load-mvcc.txt`,
	bulkNoMvcc: `${BENCH_STAMP}-bulk-load-nomvcc.txt`,
	bulkReader: `${BENCH_STAMP}-bulk-load-mvcc-reader.txt`,
} as const satisfies Record<string, LogName>;

// ---------------------------------------------------------------------------
// Machine
// ---------------------------------------------------------------------------

/** The machine and toolchains of this refresh, from the log headers. */
export const BENCH_MACHINE = (() => {
	const machine = text(LOG.rust, "machine", "meta");
	const os = text(LOG.rust, "os", "meta");
	return {
		cpu: pick(machine, /^([^,]+),/),
		cores: pick(machine, /, (\d+ cores \([^)]*\))/),
		memory: pick(machine, /(\d+ GB)/),
		os: pick(os, /^(macOS [\d.]+)/),
		darwin: pick(os, /(Darwin [\d.]+)/),
		rust: pick(text(LOG.rust, "toolchain", "meta"), /rustc ([\d.]+)/),
		node: pick(text(LOG.ts, "toolchain", "meta"), /node v([\d.]+)/),
		python: pick(text(LOG.python, "toolchain", "meta"), /Python ([\d.]+)/),
		sqlite: pick(text(LOG.sqlite, "toolchain", "meta"), /SQLite ([\d.]+)/),
		commit: pick(text(LOG.rust, "commit", "meta"), /^([0-9a-f]{7})/),
		power: text(LOG.rust, "power", "meta"),
	};
})();

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
	{ id: "getVector", label: "Get vector (random)" },
	{ id: "hasVector", label: "Has vector (random)" },
	{ id: "setVectors", label: "Set vectors (batch of 100)" },
];

/** Row names in the logs; the bindings name the vector getter differently. */
function graphRows(vectorGetter: string): Record<GraphMetric, string> {
	return {
		keyLookup: "Random existing keys",
		traverseOut: "Random nodes",
		edgeExists: "Random edge exists",
		batchNodes: "Batch of 100 nodes",
		batchEdges: "Batch of 100 edges",
		batchEdgesProps: "Batch of 100 edges + props",
		getVector: vectorGetter,
		hasVector: "has_node_vector() random",
		setVectors: "Set vectors (batch 100)",
	};
}

function graphFrom(
	log: LogName,
	vectorGetter = "node_vector() random",
): Record<GraphMetric, Percentiles> {
	const rows = graphRows(vectorGetter);
	return Object.fromEntries(
		GRAPH_METRICS.map(({ id }) => [id, percentiles(log, rows[id])]),
	) as Record<GraphMetric, Percentiles>;
}

/** "10k nodes, 50k edges, ..." from a single_file_raw_bench log header. */
function graphConfig(log: LogName, mvcc: string): string {
	const n = (key: string) => formatCount(field(log, key));
	return `${n("Nodes")} nodes, ${n("Edges")} edges, ${n("Edge types")} edge types, ${n("Edge props")} edge props, ${n("Vector count")} vectors of ${n("Vector dims")} dims, sync=${text(log, "Sync mode").toLowerCase()}, MVCC ${mvcc}`;
}

export const RUST_GRAPH_SOURCE: BenchSource = {
	log: LOG.rust,
	config: graphConfig(LOG.rust, "on"),
};
export const RUST_GRAPH = graphFrom(LOG.rust);

/** The same run with MVCC off, for comparison. */
export const RUST_GRAPH_NO_MVCC_SOURCE: BenchSource = {
	log: LOG.rustNoMvcc,
	config: graphConfig(LOG.rustNoMvcc, "off"),
};
export const RUST_GRAPH_NO_MVCC = graphFrom(LOG.rustNoMvcc);

export const PYTHON_GRAPH_SOURCE: BenchSource = {
	log: LOG.python,
	config: graphConfig(LOG.python, "on"),
};
export const PYTHON_GRAPH = graphFrom(LOG.python, "get_node_vector() random");

/** Settings of the single-file raw runs, from the Rust log header. */
export const GRAPH_RUN = {
	nodes: field(LOG.rust, "Nodes"),
	edges: field(LOG.rust, "Edges"),
	edgeTypes: field(LOG.rust, "Edge types"),
	edgeProps: field(LOG.rust, "Edge props"),
	iterations: field(LOG.rust, "Iterations"),
	walMb: field(LOG.rust, "WAL size") / (1024 * 1024),
	autoCheckpoint: text(LOG.rust, "Auto-checkpoint") === "true",
	vectors: field(LOG.rust, "Vector count"),
	vectorDims: field(LOG.rust, "Vector dims"),
	seed: field(LOG.rust, "Seed"),
};

/** Graph size of the single-file raw benchmark, e.g. "10k nodes, 50k edges". */
export const GRAPH_SIZE = `${formatCount(field(LOG.rust, "Nodes"))} nodes, ${formatCount(field(LOG.rust, "Edges"))} edges`;

export type SyncMode = "normal" | "full" | "off";

export interface SyncSweepRow {
	syncMode: SyncMode;
	log: string;
	batchNodes: Percentiles;
	batchEdges: Percentiles;
	setVectors: Percentiles;
}

/** Batch writes from the Rust benchmark in each sync mode, MVCC on. */
export const RUST_SYNC_SWEEP: SyncSweepRow[] = (
	["normal", "full", "off"] as const
).map((syncMode) => {
	const log =
		`${BENCH_STAMP}-single-file-raw-rust-mvcc-${syncMode}.txt` as const satisfies LogName;
	return {
		syncMode,
		log,
		batchNodes: percentiles(log, "Batch of 100 nodes"),
		batchEdges: percentiles(log, "Batch of 100 edges"),
		setVectors: percentiles(log, "Set vectors (batch 100)"),
	};
});

// ---------------------------------------------------------------------------
// Write scaling: multi_writer_throughput_bench
// ---------------------------------------------------------------------------

export type TxShape = "200node" | "1node";
export const WRITER_COUNTS = [1, 4, 8] as const;
export type WriterCount = (typeof WRITER_COUNTS)[number];

export interface WriterRun {
	writers: number;
	log: string;
	txPerSec: number;
	nodesPerSec: number;
}

export interface WriteScalingRow {
	shape: TxShape;
	syncMode: "normal" | "full";
	runs: Record<WriterCount, WriterRun>;
}

function writerRun(log: LogName, writers: number): WriterRun {
	return {
		writers,
		log,
		txPerSec: field(log, "Tx rate"),
		nodesPerSec: field(log, "Node rate"),
	};
}

/** Commits per second with 1, 4 and 8 writer threads, MVCC on. */
export const WRITE_SCALING: WriteScalingRow[] = (
	[
		["200node", "normal"],
		["200node", "full"],
		["1node", "normal"],
		["1node", "full"],
	] as const
).map(([shape, syncMode]) => {
	const run = (writers: WriterCount) =>
		writerRun(
			`${BENCH_STAMP}-multi-writer-throughput-mvcc-${syncMode}-${shape}-${writers}w.txt`,
			writers,
		);
	return { shape, syncMode, runs: { 1: run(1), 4: run(4), 8: run(8) } };
});

/** One writer with MVCC off, sync=normal: the baseline MVCC writers compare with. */
export const WRITE_SCALING_NO_MVCC: Record<TxShape, WriterRun> = {
	"200node": writerRun(
		`${BENCH_STAMP}-multi-writer-throughput-nomvcc-normal-200node-1w.txt`,
		1,
	),
	"1node": writerRun(
		`${BENCH_STAMP}-multi-writer-throughput-nomvcc-normal-1node-1w.txt`,
		1,
	),
};

/** One-node transactions in sync=full with F_FULLFSYNC (the sync reaches the drive). */
export const FULL_FSYNC: WriterRun[] = [
	writerRun(
		`${BENCH_STAMP}-multi-writer-throughput-mvcc-fullfsync-1node-1w.txt`,
		1,
	),
	writerRun(
		`${BENCH_STAMP}-multi-writer-throughput-mvcc-fullfsync-1node-8w.txt`,
		8,
	),
];

const MW_200 =
	`${BENCH_STAMP}-multi-writer-throughput-mvcc-normal-200node-8w.txt` as const satisfies LogName;
const MW_1 =
	`${BENCH_STAMP}-multi-writer-throughput-mvcc-normal-1node-8w.txt` as const satisfies LogName;

export const WRITE_SCALING_SHAPES: Record<
	TxShape,
	{ label: string; config: string }
> = {
	"200node": {
		label: "200-node transactions",
		config: `${field(MW_200, "Batch size")} nodes and ${field(MW_200, "Batch size") * field(MW_200, "Edges per node")} edges (${field(MW_200, "Edge props")} props each) per transaction, ${field(MW_200, "Tx per thread")} transactions per writer`,
	},
	"1node": {
		label: "1-node transactions",
		config: `one keyed node per transaction, ${field(MW_1, "Tx per thread").toLocaleString("en-US")} transactions per writer`,
	},
};

/** Throughput of `writers` over one writer, e.g. 3.1. */
export function scaling(row: WriteScalingRow, writers: WriterCount): number {
	return row.runs[writers].txPerSec / row.runs[1].txPerSec;
}

export function writeScalingRow(
	shape: TxShape,
	syncMode: "normal" | "full",
): WriteScalingRow {
	const row = WRITE_SCALING.find(
		(r) => r.shape === shape && r.syncMode === syncMode,
	);
	if (!row) throw new Error(`no write scaling row ${shape} ${syncMode}`);
	return row;
}

// ---------------------------------------------------------------------------
// SQLite baseline: docs/benchmarks/sqlite_single_file_raw_bench.py
// ---------------------------------------------------------------------------

export const SQLITE_BATCH_SOURCE: BenchSource = {
	log: LOG.sqlite,
	config: `${formatCount(field(LOG.sqlite, "Nodes"))} nodes, ${formatCount(field(LOG.sqlite, "Edges"))} edges, ${field(LOG.sqlite, "Edge types")} edge types, ${field(LOG.sqlite, "Edge props")} edge props, WAL mode, synchronous=${text(LOG.sqlite, "Sync mode")}, SQLite ${BENCH_MACHINE.sqlite}`,
};

/** Batch write (100 nodes). */
export const SQLITE_BATCH_NODES = percentiles(LOG.sqlite, "Batch of 100 nodes");

// ---------------------------------------------------------------------------
// Vector index: vector_bench (Rust) and vector_ann_bench
// ---------------------------------------------------------------------------

const VECTOR_COUNT = field(LOG.vector, "Vectors");

export const VECTOR_SOURCE: BenchSource = {
	log: LOG.vector,
	config: `${formatCount(VECTOR_COUNT)} vectors, ${field(LOG.vector, "Dimensions")} dims, ${formatCount(field(LOG.vector, "Iterations"))} iterations, k=${field(LOG.vector, "k")}, nProbe=${field(LOG.vector, "nProbe")}, ${text(LOG.vector, "Metric").toLowerCase()}, ${text(LOG.vector, "Index algorithm").toUpperCase()} with ${field(LOG.vector, "Index clusters")} clusters`,
};

export const VECTOR_INDEX = {
	set: percentiles(
		LOG.vector,
		`Set (${VECTOR_COUNT.toLocaleString("en-US")} vectors)`,
	),
	get: percentiles(LOG.vector, "Random get"),
	search: percentiles(
		LOG.vector,
		`Search (k=${field(LOG.vector, "k")}, nProbe=${field(LOG.vector, "nProbe")})`,
	),
	/** One build, so there are no percentiles */
	buildIndex: field(LOG.vector, "build_index()"),
	clusters: field(LOG.vector, "Index clusters"),
	vectors: VECTOR_COUNT,
	dimensions: field(LOG.vector, "Dimensions"),
	iterations: field(LOG.vector, "Iterations"),
	k: field(LOG.vector, "k"),
	nProbe: field(LOG.vector, "nProbe"),
};

export interface AnnRun {
	algorithm: "IVF" | "IVF-PQ";
	log: string;
	/** Index build, ns */
	build: number;
	search: Percentiles;
	recallAtK: number;
}

function annRun(log: LogName, algorithm: AnnRun["algorithm"]): AnnRun {
	return {
		algorithm,
		log,
		build: field(log, "build_elapsed_ms") * 1_000_000,
		search: {
			p50: field(log, "search_p50_ms") * 1_000_000,
			p95: field(log, "search_p95_ms") * 1_000_000,
		},
		recallAtK: field(log, "mean_recall_at_k"),
	};
}

/** IVF vs IVF-PQ where the auto backend switches to IVF-PQ. */
export const ANN_COMPARISON: AnnRun[] = [
	annRun(LOG.annIvf, "IVF"),
	annRun(LOG.annIvfPq, "IVF-PQ"),
];

export const ANN_CONFIG = `${formatCount(field(LOG.annIvf, "vectors"))} vectors of ${field(LOG.annIvf, "dimensions")} dims (${text(LOG.annIvf, "dataset")} dataset), ${field(LOG.annIvf, "queries")} queries, k=${field(LOG.annIvf, "k")}, nProbe=${field(LOG.annIvf, "n_probe")}, ${field(LOG.annIvf, "n_clusters")} clusters, IVF-PQ with ${field(LOG.annIvfPq, "pq_subspaces")} subspaces and the default re-rank`;

// ---------------------------------------------------------------------------
// Open, paging and listings: query_core_bench
// ---------------------------------------------------------------------------

export interface OpenRun {
	nodes: number;
	edges: number;
	log: string;
	/** Median read-only open + close, ns */
	median: number;
}

function openRun(log: LogName): OpenRun {
	const what = text(log, "open");
	return {
		nodes: Number(pick(what, /^(\d+) nodes/)),
		edges: Number(pick(what, /(\d+) edges/)),
		log,
		median: field(log, "snapshot open_single_file (read-only)", "median"),
	};
}

/** Opening a checkpointed database read-only (open + close). */
export const OPEN_TIME: OpenRun[] = [
	openRun(LOG.open100k),
	openRun(LOG.open1m),
];

export interface QueryRow {
	label: string;
	/** Median, ns, with every change still in the WAL */
	delta: number;
	/** Median, ns, after a checkpoint folded them into the snapshot */
	snapshot: number;
}

function queryRow(label: string, op: string): QueryRow {
	return {
		label,
		delta: field(LOG.paging, `delta ${op}`, "median"),
		snapshot: field(LOG.paging, `snapshot ${op}`, "median"),
	};
}

const PAGE = pick(text(LOG.paging, "paging"), /pages 1 and (\d+)/);
const PAGE_SIZE = pick(text(LOG.paging, "paging"), /page size (\d+)/);
const HUB_EDGES = Number(pick(text(LOG.paging, "hub"), /with (\d+) out-edges/));

export const PAGING_SOURCE: BenchSource = {
	log: LOG.paging,
	config: `paging: ${text(LOG.paging, "paging")}; types: ${text(LOG.paging, "types")}; ${text(LOG.paging, "hub")}`,
};

export const QUERY_ROWS: QueryRow[] = [
	queryRow(`Node page 1 (${PAGE_SIZE} nodes)`, "nodes page 1"),
	queryRow(`Node page ${PAGE}`, `nodes page ${PAGE}`),
	queryRow(`Edge page 1 (${PAGE_SIZE} edges)`, "edges page 1"),
	queryRow(`Edge page ${PAGE}`, `edges page ${PAGE}`),
	queryRow("Count nodes (page total)", "count_nodes (page total)"),
	queryRow("Count edges (page total)", "count_edges (page total)"),
	queryRow("Count nodes of one type", "count_nodes_by_type(T2)"),
	queryRow(
		`First neighbor of a ${formatCount(HUB_EDGES)}-edge hub, take(1)`,
		"from(hub).out(None).take(1)",
	),
	queryRow(
		`All neighbors of a ${formatCount(HUB_EDGES)}-edge hub`,
		"neighbors_out(hub) (all edges)",
	),
];

// ---------------------------------------------------------------------------
// MVCC cost: mvcc_overhead_bench, and bulk loads: bulk_load_bench
// ---------------------------------------------------------------------------

export interface MvccCostRow {
	label: string;
	/** Operations per second, MVCC off */
	off: number;
	/** Operations per second, MVCC on */
	on: number;
	/** on / off - 1, in percent, as printed */
	change: number;
}

function mvccRow(label: string, row: string): MvccCostRow {
	return {
		label,
		off: field(LOG.mvccOverhead, row, "off"),
		on: field(LOG.mvccOverhead, row, "on"),
		change: field(LOG.mvccOverhead, row, "vs off"),
	};
}

export const MVCC_COST_SOURCE: BenchSource = {
	log: LOG.mvccOverhead,
	config: text(LOG.mvccOverhead, "mvcc_overhead_bench"),
};

export const MVCC_COST: MvccCostRow[] = [
	mvccRow("node_prop, 1 reader", "reads node_prop 1 reads/s"),
	mvccRow("node_props, 1 reader", "reads node_props 1 reads/s"),
	mvccRow("out_edges, 1 reader", "reads out_edges 1 reads/s"),
	mvccRow(
		"node_prop in read transactions, 1 reader",
		"reads tx_node_prop 1 reads/s",
	),
	mvccRow("node_prop, 8 readers", "reads node_prop 8 reads/s"),
	mvccRow(
		"node_prop, 4 readers beside 1 writer",
		"mixed node_prop 4r+1w reads/s",
	),
	mvccRow("Update one prop, 1 writer", "writes update_prop 1w commits/s"),
	mvccRow(
		"Insert a node + prop + edge, 1 writer",
		"writes insert 1w commits/s",
	),
];

export interface BulkRun {
	log: string;
	nodesPerSec: number;
	edgesPerSec: number;
}

function bulkRun(log: LogName): BulkRun {
	return {
		log,
		nodesPerSec: field(log, "Node rate"),
		edgesPerSec: field(log, "Edge rate"),
	};
}

/** "200000 (2 props), edges: 1000000 (2 props), batch: 5000" */
const BULK_SHAPE = text(LOG.bulkMvcc, "Nodes");

export const BULK_LOAD = {
	mvcc: bulkRun(LOG.bulkMvcc),
	noMvcc: bulkRun(LOG.bulkNoMvcc),
	/** MVCC on, a read transaction held open for the whole load */
	reader: bulkRun(LOG.bulkReader),
	nodes: Number(pick(BULK_SHAPE, /^(\d+)/)),
	edges: Number(pick(BULK_SHAPE, /edges: (\d+)/)),
	props: Number(pick(BULK_SHAPE, /\((\d+) props\)/)),
	batch: Number(pick(BULK_SHAPE, /batch: (\d+)/)),
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
	log: LOG.ts,
	config: `${formatCount(field(LOG.ts, "Nodes"))} nodes, ${formatCount(field(LOG.ts, "Edges"))} edges, ${field(LOG.ts, "Edge types")} edge types, ${field(LOG.ts, "Edge props")} edge props, ${formatCount(field(LOG.ts, "Iterations"))} iterations, sync=${text(LOG.ts, "Sync mode")}, MVCC ${text(LOG.ts, "MVCC")}, Node ${BENCH_MACHINE.node}`,
};

/** Graph size of the TypeScript run, e.g. "1k nodes, 5k edges". */
export const TS_GRAPH_SIZE = `${formatCount(field(LOG.ts, "Nodes"))} nodes, ${formatCount(field(LOG.ts, "Edges"))} edges`;

const TS_ROWS: [label: string, row: string][] = [
	["Insert (single node + props)", "Insert (single node + props)"],
	["Key lookup (get, with props)", "Key lookup (raw vs get with props)"],
	["Key lookup (getRef, no props)", "Key lookup (raw vs getRef, no props)"],
	["Key lookup (getId, id only)", "Key lookup (raw vs getId, id-only)"],
	["1-hop traversal (count)", "1-hop traversal (count)"],
	["1-hop traversal (node ids)", "1-hop traversal (nodes/ids)"],
	[
		"1-hop traversal (toArray, with props)",
		"1-hop traversal (toArray with props)",
	],
	["Pathfinding BFS (max depth 5)", "Pathfinding BFS (max depth 5)"],
];

export const TS_OVERHEAD: OverheadRow[] = TS_ROWS.map(([label, row]) => ({
	label,
	lowLevel: field(LOG.ts, row, "lowLevel"),
	fluent: field(LOG.ts, row, "fluent"),
	overhead: field(LOG.ts, row, "overhead"),
}));

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
		detail: `random pair, ${formatCount(field(LOG.rust, "Edges"))} edges`,
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
	detail: `top-${field(LOG.vector, "k")} of ${formatCount(VECTOR_COUNT)} × ${VECTOR_INDEX.dimensions}d, ${text(LOG.vector, "Index algorithm").toUpperCase()}`,
	...VECTOR_INDEX.search,
};

export const LATENCY_ROWS: LatencyRow[] = [
	...GRAPH_LATENCY_ROWS,
	VECTOR_LATENCY_ROW,
];

/** A latency split into a headline number and its unit: 83 ns, 34 µs, 1.2 ms. */
export function headlineParts(ns: number): { value: string; unit: string } {
	if (ns < 1_000) return { value: String(Math.round(ns)), unit: "ns" };
	if (ns < 1_000_000) {
		const us = ns / 1_000;
		return {
			value: us < 10 ? us.toFixed(1) : String(Math.round(us)),
			unit: "µs",
		};
	}
	return { value: (ns / 1_000_000).toFixed(1), unit: "ms" };
}

export const HEADLINE_STATS = [
	{
		label: "Edge existence check",
		...headlineParts(RUST_GRAPH.edgeExists.p50),
	},
	{ label: "Key lookup", ...headlineParts(RUST_GRAPH.keyLookup.p50) },
	{ label: "1-hop traversal", ...headlineParts(RUST_GRAPH.traverseOut.p50) },
	{ label: "Commit 100 nodes", ...headlineParts(RUST_GRAPH.batchNodes.p50) },
];

export const BENCH_ENVIRONMENT = [
	{
		label: "Hardware",
		value: `${BENCH_MACHINE.cpu}, ${BENCH_MACHINE.memory}, ${BENCH_MACHINE.os}`,
	},
	{
		label: "Graph dataset",
		value: `${GRAPH_SIZE} · ${field(LOG.rust, "Edge types")} edge types · ${field(LOG.rust, "Edge props")} props`,
	},
	{ label: "Durability", value: "sync=normal · MVCC on" },
];

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

/** Format nanoseconds with an adaptive unit: 83 ns, 34.1 µs, 558 µs, 1.2 ms, 4.8 s. */
export function formatNs(ns: number): string {
	if (ns < 1_000) return `${Math.round(ns)} ns`;
	if (ns < 1_000_000) {
		const us = ns / 1_000;
		return `${us < 100 ? us.toFixed(1) : Math.round(us)} µs`;
	}
	if (ns < 1_000_000_000) return `${(ns / 1_000_000).toFixed(1)} ms`;
	return `${(ns / 1_000_000_000).toFixed(1)} s`;
}

/** Format nanoseconds with the precision the raw logs use: 83 ns, 34.08 µs, 801.95 ms, 4.78 s. */
export function formatNsExact(ns: number): string {
	if (ns < 1_000) return `${Math.round(ns)} ns`;
	if (ns < 1_000_000) return `${(ns / 1_000).toFixed(2)} µs`;
	if (ns < 1_000_000_000) return `${(ns / 1_000_000).toFixed(2)} ms`;
	return `${(ns / 1_000_000_000).toFixed(2)} s`;
}

/**
 * Format a query_core_bench time, which the log prints in µs with one
 * decimal: < 0.1 µs, 0.5 µs, 597.0 µs, 4.95 ms.
 */
export function formatMicros(ns: number): string {
	if (ns < 50) return "< 0.1 µs";
	if (ns < 1_000_000) return `${(ns / 1_000).toFixed(1)} µs`;
	return `${(ns / 1_000_000).toFixed(2)} ms`;
}

/** Format a per-second rate the way the raw logs do: 724.42/s, 144.88K/s, 1.26M/s. */
export function formatRate(perSec: number): string {
	if (perSec < 1_000) return `${perSec.toFixed(2)}/s`;
	if (perSec < 1_000_000) return `${(perSec / 1_000).toFixed(2)}K/s`;
	return `${(perSec / 1_000_000).toFixed(2)}M/s`;
}

/** A rate rounded for prose: 724/s, 10.6K/s, 268K/s, 1.5M/s. */
export function formatRateShort(perSec: number): string {
	if (perSec < 1_000) return `${Math.round(perSec)}/s`;
	if (perSec < 1_000_000) {
		const k = perSec / 1_000;
		return `${k < 100 ? k.toFixed(1) : Math.round(k)}K/s`;
	}
	return `${(perSec / 1_000_000).toFixed(2)}M/s`;
}

/** A signed percentage as printed: +1.8%, -6.0%. */
export function formatChange(percent: number): string {
	return `${percent > 0 ? "+" : ""}${percent.toFixed(1)}%`;
}

/** A ratio for prose: 3.1x. */
export function formatRatio(ratio: number): string {
	return `${ratio.toFixed(1)}x`;
}

/** GitHub URL for a raw log, or for the results directory when `log` is omitted. */
export function resultsUrl(githubUrl: string, log?: string): string {
	return log
		? `${githubUrl}/blob/main/${RESULTS_DIR}/${log}`
		: `${githubUrl}/tree/main/${RESULTS_DIR}`;
}
