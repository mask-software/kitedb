import { ArrowRight } from "lucide-solid";
import { For, Show } from "solid-js";
import CodeBlock from "~/components/code-block";
import DocPage from "~/components/doc-page";
import { GITHUB_URL } from "~/components/github-icon";
import {
	BENCH_DATE,
	BENCH_MACHINE,
	BULK_LOAD,
	formatChange,
	formatCount,
	formatNsExact,
	formatRateShort,
	formatRatio,
	GRAPH_METRICS,
	GRAPH_SIZE,
	type GraphMetric,
	headlineParts,
	MVCC_COST,
	MVCC_COST_SOURCE,
	RUST_GRAPH,
	RUST_GRAPH_SOURCE,
	resultsUrl,
	WRITE_SCALING_NO_MVCC,
	writeScalingRow,
} from "~/lib/benchmarks";
import {
	type Accent,
	CELL,
	CELL_HIGHLIGHT,
	CELL_PLAIN,
	Code,
	Figure,
	Panel,
} from "./-components";

// ============================================================================
// SHARED DIAGRAM PRIMITIVES
// ============================================================================

/** Horizontal chain of labelled boxes joined by arrows. */
function Chain(props: { steps: { label: string; accent?: "cyan" | "red" }[] }) {
	return (
		<div class="flex flex-wrap items-center gap-1.5">
			<For each={props.steps}>
				{(step, i) => (
					<>
						<Show when={i() > 0}>
							<ArrowRight
								size={13}
								class="shrink-0 text-slate-600"
								aria-hidden="true"
							/>
						</Show>
						<span
							class={`rounded-md border px-2 py-0.5 text-[12px] ${
								step.accent === "red"
									? "border-red-400/25 bg-red-400/10 text-red-300"
									: step.accent === "cyan"
										? "border-kite-cyan/25 bg-kite-cyan/10 text-kite-cyan"
										: "border-kite-line bg-white/[0.03] text-slate-300"
							}`}
						>
							{step.label}
						</span>
					</>
				)}
			</For>
		</div>
	);
}

/** Measured value with its unit in a quieter color. */
function Stat(props: { value: string; unit: string }) {
	return (
		<span class="whitespace-nowrap font-mono">
			<span class="font-semibold text-white">{props.value}</span>{" "}
			<span class="text-slate-500">{props.unit}</span>
		</span>
	);
}

// ============================================================================
// PERFORMANCE DIAGRAMS
// ============================================================================

// p50 values from RUST_GRAPH_SOURCE (src/lib/benchmarks.ts)
const EMBEDDED_P50 = [
	{ label: "Key lookup", ...headlineParts(RUST_GRAPH.keyLookup.p50) },
	{ label: "1-hop traversal", ...headlineParts(RUST_GRAPH.traverseOut.p50) },
	{ label: "Commit 100 nodes", ...headlineParts(RUST_GRAPH.batchNodes.p50) },
];

const WRITES_200 = writeScalingRow("200node", "normal");
const NODE_METRICS: GraphMetric[] = ["keyLookup", "batchNodes"];
const EDGE_METRICS: GraphMetric[] = [
	"traverseOut",
	"edgeExists",
	"batchEdges",
	"batchEdgesProps",
];

/** Operation | p50 | p95 rows of the Rust single-file raw run. */
function ResultTable(props: { metrics: GraphMetric[] }) {
	return (
		<table>
			<thead>
				<tr>
					<th>Operation</th>
					<th>p50</th>
					<th>p95</th>
				</tr>
			</thead>
			<tbody>
				<For each={props.metrics}>
					{(metric) => (
						<tr>
							<td>{GRAPH_METRICS.find((m) => m.id === metric)?.label}</td>
							<td>{formatNsExact(RUST_GRAPH[metric].p50)}</td>
							<td>{formatNsExact(RUST_GRAPH[metric].p95)}</td>
						</tr>
					)}
				</For>
			</tbody>
		</table>
	);
}

function NetworkOverheadComparison() {
	return (
		<div class="not-prose grid gap-3 sm:grid-cols-2">
			<Figure title="Client-server database" accent="red" variant="problem">
				<Chain
					steps={[
						{ label: "App" },
						{ label: "Network", accent: "red" },
						{ label: "Server" },
						{ label: "Storage" },
						{ label: "Network", accent: "red" },
						{ label: "App" },
					]}
				/>
				<p class="mt-4 text-[14px] text-slate-400">
					Each query is serialized, sent over a socket, executed in another
					process, and sent back.
				</p>
			</Figure>

			<Figure title="KiteDB, embedded" accent="cyan">
				<Chain
					steps={[
						{ label: "App" },
						{ label: "KiteDB", accent: "cyan" },
						{ label: "Memory-mapped file" },
					]}
				/>
				<p class="mt-4 text-[14px] text-slate-400">
					Queries are function calls into a library in your process.
				</p>
				<dl class="mt-4 space-y-1 border-t border-kite-line pt-3 text-[13px]">
					<For each={EMBEDDED_P50}>
						{(row) => (
							<div class="flex items-baseline justify-between gap-4">
								<dt class="text-slate-400">{row.label}</dt>
								<dd>
									<Stat value={row.value} unit={row.unit} />
								</dd>
							</div>
						)}
					</For>
				</dl>
			</Figure>
		</div>
	);
}

function ReadPathComparison() {
	return (
		<div class="not-prose grid gap-3 sm:grid-cols-2">
			<Figure title="Read into buffers" accent="red" variant="problem">
				<Chain
					steps={[
						{ label: "Disk" },
						{ label: "Kernel buffer" },
						{ label: "User buffer", accent: "red" },
						{ label: "Parse", accent: "red" },
						{ label: "Use" },
					]}
				/>
				<p class="mt-4 text-[14px] text-slate-400">
					Data is copied into a user buffer, then decoded into objects before
					the query can use it.
				</p>
			</Figure>

			<Figure title="KiteDB snapshot, memory-mapped" accent="cyan">
				<Chain
					steps={[
						{ label: "Disk" },
						{ label: "Page cache" },
						{ label: "Read in place", accent: "cyan" },
					]}
				/>
				<p class="mt-4 text-[14px] text-slate-400">
					Uncompressed snapshot sections are read directly from mapped pages.
					The OS keeps hot pages in RAM and evicts cold ones.
				</p>
			</Figure>
		</div>
	);
}

const POINTER_CHAIN = ["0x7f3a10", "0x1c0820", "0x9e4410", "0x2b7730"];

const OFFSETS = [
	{ index: "n-1", value: "32" },
	{ index: "n", value: "40", active: true },
	{ index: "n+1", value: "50", active: true },
	{ index: "n+2", value: "53" },
];

/** out_dst slots 38..51; slots 40..49 are node n's neighbors. */
const DST_SLOTS = Array.from({ length: 14 }, (_, i) => 38 + i);

function CacheFriendlyComparison() {
	return (
		<Figure title="Reading 10 neighbors of node n" accent="cyan">
			<div class="space-y-3">
				<Panel label="Pointer-based adjacency" accent="red">
					<div class="flex flex-wrap items-center gap-1.5">
						<For each={POINTER_CHAIN}>
							{(address, i) => (
								<>
									<Show when={i() > 0}>
										<ArrowRight
											size={13}
											class="shrink-0 text-slate-600"
											aria-hidden="true"
										/>
									</Show>
									<span class={`${CELL} ${CELL_PLAIN}`}>{address}</span>
								</>
							)}
						</For>
						<span class="font-mono text-[12px] text-slate-500">…</span>
					</div>
					<p class="mt-3 text-[13px] text-slate-400">
						10 dependent loads at scattered addresses. Each next address is
						known only after the previous load finishes, and each load can miss
						the CPU cache.
					</p>
				</Panel>

				<Panel label="CSR adjacency" accent="cyan">
					<div class="overflow-x-auto">
						<div class="min-w-[30rem] space-y-3">
							<div class="flex items-end gap-3">
								<span class="w-20 shrink-0 pb-1.5 font-mono text-[11px] text-slate-500">
									out_offsets
								</span>
								<div class="flex gap-1">
									<For each={OFFSETS}>
										{(slot) => (
											<div class="w-12">
												<div class="mb-1 text-center font-mono text-[11px] text-slate-500">
													{slot.index}
												</div>
												<div
													class={`${CELL} ${slot.active ? CELL_HIGHLIGHT : CELL_PLAIN}`}
												>
													{slot.value}
												</div>
											</div>
										)}
									</For>
								</div>
							</div>
							<div class="flex items-end gap-3">
								<span class="w-20 shrink-0 pb-1.5 font-mono text-[11px] text-slate-500">
									out_dst
								</span>
								<div class="flex gap-0.5">
									<For each={DST_SLOTS}>
										{(slot) => {
											const active = slot >= 40 && slot < 50;
											return (
												<div class="w-7">
													<div class="mb-1 text-center font-mono text-[10px] text-slate-600">
														{slot}
													</div>
													<div
														class={`h-6 rounded-sm border ${active ? "border-kite-cyan/30 bg-kite-cyan/15" : "border-kite-line bg-white/[0.03]"}`}
													/>
												</div>
											);
										}}
									</For>
								</div>
							</div>
						</div>
					</div>
					<p class="mt-3 text-[13px] text-slate-400">
						Two adjacent offsets give the range{" "}
						<span class="font-mono text-slate-200">[40, 50)</span>. The 10
						neighbor IDs are contiguous u32 values, 40 bytes in{" "}
						<span class="font-mono text-slate-200">out_dst</span> and 40 bytes
						in <span class="font-mono text-slate-200">out_etype</span>: one or
						two cache lines each, read sequentially.
					</p>
				</Panel>
			</div>

			<p class="mt-4 border-t border-kite-line pt-3 text-[13px] text-slate-400">
				Measured 1-hop outgoing traversal from a random node:{" "}
				<Stat {...headlineParts(RUST_GRAPH.traverseOut.p50)} /> p50 (
				{GRAPH_SIZE}
				).
			</p>
		</Figure>
	);
}

function LazyMVCCComparison() {
	return (
		<Figure title="Version chains only when needed" accent="violet">
			<div class="space-y-3">
				<Panel label="No other open transactions" meta="serial workload">
					<p class="text-[13px] text-slate-400">
						A commit applies its changes to the delta. No version chain is
						written.
					</p>
				</Panel>
				<Panel label="Other transactions open" meta="concurrent workload">
					<p class="text-[13px] text-slate-400">
						A commit also appends a version for each changed node, edge, and
						property, so transactions on older snapshots keep a consistent view.
						The cost grows with the number of changes made while other
						transactions are open.
					</p>
				</Panel>
			</div>
			<p class="mt-4 border-t border-kite-line pt-3 text-[13px] text-slate-500">
				MVCC is on by default since 0.3.0. Old versions are removed by MVCC
				garbage collection.
			</p>
		</Figure>
	);
}

const MEMORY_PARTS: {
	title: string;
	meta?: string;
	accent: Accent;
	/** Bullet text with an optional inline identifier: `text` + `code` + `after`. */
	items: { text: string; code?: string; after?: string }[];
}[] = [
	{
		title: "Snapshot",
		meta: "memory-mapped",
		accent: "cyan",
		items: [
			{
				text: "File pages live in the OS page cache: hot pages stay in RAM, cold pages are read from disk on demand.",
			},
			{
				text: "Checkpoints compress sections with zstd by default. A compressed section is decompressed into process memory and cached.",
			},
		],
	},
	{
		title: "Delta",
		meta: "in memory",
		accent: "violet",
		items: [
			{
				text: "Hash maps of created and modified nodes, edge patches, edge properties, and key changes.",
			},
			{ text: "Each added edge is recorded twice, once per direction." },
			{
				text: "Grows with writes until a checkpoint folds it into a new snapshot.",
			},
			{
				text: "Takes about ten times the bytes of log it holds, so ",
				code: "checkpointLogBudget",
				after:
					" (default 128 MiB of log, about 1.3 GB of delta) bounds it at the checkpoint trigger while checkpoints keep up; writers that outrun them grow the log up to walSegmentLimit. A checkpoint briefly needs about twice that.",
			},
		],
	},
	{
		title: "MVCC version chains",
		meta: "while transactions are open",
		accent: "mint",
		items: [
			{
				text: "Only written when a commit lands while other transactions are open.",
			},
			{
				text: "Removed by garbage collection once no open transaction needs them (every 5 s by default).",
			},
		],
	},
];

function MemoryUsageBreakdown() {
	return (
		<Figure title="Where memory goes">
			<div class="grid gap-3 sm:grid-cols-2">
				<For each={MEMORY_PARTS}>
					{(part, i) => (
						<Panel
							label={`${i() + 1}. ${part.title}`}
							accent={part.accent}
							meta={part.meta}
						>
							<ul class="space-y-1.5 text-[13px] text-slate-400">
								<For each={part.items}>
									{(item) => (
										<li class="flex gap-2.5">
											<span
												class="mt-[0.6em] h-1 w-1 shrink-0 rounded-full bg-slate-600"
												aria-hidden="true"
											/>
											<span>
												{item.text}
												<Show when={item.code}>
													{(code) => <Code>{code()}</Code>}
												</Show>
												{item.after}
											</span>
										</li>
									)}
								</For>
							</ul>
						</Panel>
					)}
				</For>
			</div>
		</Figure>
	);
}

// ============================================================================
// PAGE COMPONENT
// ============================================================================

export function PerformancePage() {
	return (
		<DocPage slug="internals/performance">
			<p>
				This page explains where KiteDB's read and write latency comes from,
				shows measured results, and covers the settings that trade durability
				for throughput.
			</p>

			<h2 id="why-fast">What keeps latency low</h2>

			<h3>1. No network round trip</h3>
			<NetworkOverheadComparison />
			<p>
				The p50 figures come from the benchmark run described under{" "}
				<a href="#benchmarks">benchmark results</a>.
			</p>

			<h3>2. Memory-mapped snapshot</h3>
			<ReadPathComparison />
			<p>
				Hot data stays in RAM and cold data is paged in on demand. Checkpoints
				compress snapshot sections with zstd by default; a compressed section is
				decompressed once and cached in memory, so only uncompressed sections
				are read in place.
			</p>

			<h3>3. Cache-friendly data layout</h3>
			<CacheFriendlyComparison />

			<h3>4. Lazy MVCC</h3>
			<LazyMVCCComparison />

			<h2 id="benchmarks">Benchmark results</h2>

			<p>
				Single-file raw benchmark on the Rust core ({RUST_GRAPH_SOURCE.config}
				), {BENCH_MACHINE.cpu}, {BENCH_DATE}. Each row is the median of five
				interleaved runs.
			</p>

			<h3>Node operations</h3>
			<ResultTable metrics={NODE_METRICS} />

			<h3>Edge operations</h3>
			<ResultTable metrics={EDGE_METRICS} />

			<p>
				Raw log:{" "}
				<a
					href={resultsUrl(GITHUB_URL, RUST_GRAPH_SOURCE.log)}
					target="_blank"
					rel="noopener noreferrer"
				>
					<code>docs/benchmarks/results/{RUST_GRAPH_SOURCE.log}</code>
				</a>
				. The <a href="/docs/benchmarks">benchmarks</a> pages have the other
				runs and their commands.
			</p>

			<h3 id="mvcc-cost">MVCC cost</h3>
			<p>
				Throughput with MVCC on against MVCC off on the same machine, from{" "}
				<code>mvcc_overhead_bench</code> (sync off, so a commit costs CPU rather
				than an fsync wait; each case is the median of five runs):
			</p>
			<table>
				<thead>
					<tr>
						<th>Workload</th>
						<th>MVCC off</th>
						<th>MVCC on</th>
						<th>Change</th>
					</tr>
				</thead>
				<tbody>
					<For each={MVCC_COST}>
						{(row) => (
							<tr>
								<td>{row.label}</td>
								<td>{formatRateShort(row.off)}</td>
								<td>{formatRateShort(row.on)}</td>
								<td>{formatChange(row.change)}</td>
							</tr>
						)}
					</For>
				</tbody>
			</table>
			<p class="text-[13px] text-slate-500">
				Source:{" "}
				<a
					href={resultsUrl(GITHUB_URL, MVCC_COST_SOURCE.log)}
					target="_blank"
					rel="noopener noreferrer"
				>
					<code>{MVCC_COST_SOURCE.log}</code>
				</a>{" "}
				({MVCC_COST_SOURCE.config})
			</p>
			<p>
				Writer threads need MVCC: without it, write transactions run one at a
				time. With <code>syncMode=Normal</code>, one writer without MVCC reaches{" "}
				{formatRateShort(WRITE_SCALING_NO_MVCC["200node"].txPerSec)} with
				transactions of 200 nodes and 200 edges, and eight MVCC writers reach{" "}
				{formatRateShort(WRITES_200.runs[8].txPerSec)} (
				{formatRatio(
					WRITES_200.runs[8].txPerSec /
						WRITE_SCALING_NO_MVCC["200node"].txPerSec,
				)}
				).
			</p>

			<h3>Write durability vs. throughput</h3>
			<ul>
				<li>
					<strong>Defaults:</strong> <code>syncMode=Full</code>: a commit
					returns once it is fsynced; commits that arrive together share the
					fsync.
				</li>
				<li>
					<strong>Single writer, low latency:</strong>{" "}
					<code>syncMode=Normal</code>.
				</li>
				<li>
					<strong>Several writer threads:</strong> <code>syncMode=Normal</code>{" "}
					(or <code>Full</code>, whose fsyncs the commits share). With MVCC (the
					default), writers build their transactions in parallel, and commits
					that arrive together are written as one group. For one-shot ingest, a
					bulk load through one writer is still fastest. See the{" "}
					<a href="/docs/benchmarks#parallel-write-scaling">
						parallel write scaling notes
					</a>
					.
				</li>
				<li>
					<strong>Fastest, least durable:</strong> <code>syncMode=Off</code>,
					for tests and throwaway data only.
				</li>
			</ul>
			<p>
				Every commit is group-committed: the commits that arrive while a group
				is written form the next group, written with one WAL write, one header
				write and, in <code>Full</code> mode, one fsync, then published in order
				(each commit whole). No commit waits for others to join, so a single
				writer pays nothing for it. This works in every sync mode, with MVCC and
				on a replication primary; <code>groupCommitEnabled</code> and{" "}
				<code>groupCommitWindowMs</code> have no effect.
			</p>

			<h4>Decision table</h4>
			<table>
				<thead>
					<tr>
						<th>Workload</th>
						<th>syncMode</th>
						<th>Why</th>
					</tr>
				</thead>
				<tbody>
					<tr>
						<td>Production, high durability</td>
						<td>Full</td>
						<td>A commit returns once fsynced; concurrent commits share it</td>
					</tr>
					<tr>
						<td>Single-writer ingest</td>
						<td>Normal</td>
						<td>Lowest latency per commit</td>
					</tr>
					<tr>
						<td>Several writer threads</td>
						<td>Normal</td>
						<td>Concurrent commits share a WAL write and a header</td>
					</tr>
					<tr>
						<td>Testing, throwaway data</td>
						<td>Off</td>
						<td>Fastest, weakest durability</td>
					</tr>
				</tbody>
			</table>

			<h2 id="playbook">Performance playbook</h2>
			<ul>
				<li>
					<strong>Fastest ingest (single writer):</strong>{" "}
					<code>beginBulk()</code> + <code>createNodesBatch()</code> +{" "}
					<code>addEdgesBatch()</code> / <code>addEdgesWithPropsBatch()</code>,{" "}
					<code>syncMode=Normal</code>, and the default checkpoint settings: a
					full WAL spills into WAL segments and checkpoints run on the
					checkpoint thread, so the WAL needs no resizing (a larger one means
					fewer spills, but raises the floors of the checkpoint trigger, the
					segment limit and the segment extent). With auto-checkpoint off,
					writes fail once the WAL segments reach <code>walSegmentLimit</code>{" "}
					or fill the segment table (63 extents; with default extents about four
					times a limit up to 512 MiB, and 2 GiB beyond): raise the limit, and{" "}
					<code>walSegmentSize</code> with it past that, to hold the whole load,
					then checkpoint.
				</li>
				<li>
					<strong>Several writer threads:</strong> <code>syncMode=Normal</code>,
					several operations per transaction, and a retry for commits that fail
					with a conflict.
				</li>
				<li>
					<strong>Read-heavy, mixed workload:</strong> keep write batches small,
					leave auto-checkpoint on (it runs once the log reaches half the
					snapshot's size, at least 3/8 of the WAL, at most 128 MiB; tune with{" "}
					<code>checkpointLogRatio</code> and <code>checkpointLogBudget</code>),
					and bound traversal depth.
				</li>
				<li>
					<strong>Fastest, least durable:</strong> <code>syncMode=Off</code>,
					for testing only.
				</li>
			</ul>
			<p>
				A bulk load of {formatCount(BULK_LOAD.nodes)} nodes and{" "}
				{formatCount(BULK_LOAD.edges)} edges with {BULK_LOAD.props} properties
				each, in batches of {BULK_LOAD.batch.toLocaleString("en-US")}, ran at{" "}
				{formatRateShort(BULK_LOAD.mvcc.nodesPerSec)} for nodes and{" "}
				{formatRateShort(BULK_LOAD.mvcc.edgesPerSec)} for edges with MVCC, and{" "}
				{formatRateShort(BULK_LOAD.noMvcc.nodesPerSec)} and{" "}
				{formatRateShort(BULK_LOAD.noMvcc.edgesPerSec)} without it (
				{BENCH_MACHINE.cpu}, <code>examples/bulk_load_bench.rs</code>). It runs
				alone among writers, and readers never wait for it. A read transaction
				held open across the load slows its edge inserts (
				{formatRateShort(BULK_LOAD.reader.edgesPerSec)} in the same test, with
				nodes at {formatRateShort(BULK_LOAD.reader.nodesPerSec)}), because the
				load's commits record version history for it. Use it for one-shot ingest
				or ETL jobs.
			</p>

			<h3>Bulk ingest example (low-level API)</h3>
			<CodeBlock
				code={`// Bulk ingest with the low-level API
db.beginBulk();
const nodeIds = db.createNodesBatch(keys); // keys: string[]
db.addEdgesBatch(edges); // edges: { src, etype, dst }[]
db.addEdgesWithPropsBatch(edgesWithProps);
db.commit();

// Optional: checkpoint after ingest
db.checkpoint();`}
				language="typescript"
			/>

			<h2 id="best-practices">Best practices</h2>

			<h3>Batch writes</h3>
			<CodeBlock
				code={`// Slow: one transaction (and one WAL sync) per node
for (const key of keys) {
  db.begin();
  db.createNode(key);
  db.commit();
}

// Fast: one bulk-load transaction for the whole batch
db.beginBulk();
db.createNodesBatch(keys);
db.commit();

// Rust core benchmark: 100 nodes per batch, ${formatNsExact(RUST_GRAPH.batchNodes.p50)} p50 (syncMode=Normal)`}
				language="typescript"
			/>

			<h3>Limit traversal depth</h3>
			<CodeBlock
				code={`// Potentially expensive: deep traversal
const alice = db.get(user, 'alice');
const all = db
  .from(alice)
  .traverse(follows, { direction: 'out', maxDepth: 10 })
  .nodes()
  .toArray();

// Safer: bounded traversal + limit
const friends = db
  .from(alice)
  .traverse(follows, { direction: 'out', maxDepth: 2 })
  .take(100)
  .nodes()
  .toArray();`}
				language="typescript"
			/>

			<h3>Use keys for lookups</h3>
			<CodeBlock
				code={`// Fast: key lookup (O(1) hash index)
const alice = db.get(user, 'alice');

// Slower: property scan (O(n) nodes, done in JS)
const aliceByName = db.all(user).find((u) => u.name === 'Alice');

// Design keys to match your access patterns.`}
				language="typescript"
			/>

			<h3>Checkpoint timing</h3>
			<CodeBlock
				code={`// After a large ingest, fold the delta into a fresh snapshot
await importLargeDataset();
db.checkpoint();

// Inspect storage stats
const stats = db.stats();`}
				language="typescript"
			/>

			<h2 id="memory">Memory usage</h2>

			<MemoryUsageBreakdown />

			<h2 id="profiling">Profiling tips</h2>

			<CodeBlock
				code={`// Get database statistics
const stats = db.stats();
console.log(stats);
// {
//   snapshotNodes: 100000,
//   snapshotEdges: 500000,
//   deltaNodesCreated: 1200,
//   deltaEdgesAdded: 3400,
//   walBytes: 10485760,
//   recommendCompact: false
// }

// If recommendCompact is true, run db.checkpoint()`}
				language="typescript"
			/>

			<h2 id="next">Next steps</h2>
			<ul>
				<li>
					<a href="/docs/internals/csr">CSR format</a>: how adjacency is laid
					out for traversal
				</li>
				<li>
					<a href="/docs/internals/snapshot-delta">Snapshot and delta</a>: how
					reads stay consistent during writes
				</li>
				<li>
					<a href="/docs/benchmarks">Benchmarks</a>: full measurements and run
					commands
				</li>
			</ul>
		</DocPage>
	);
}
