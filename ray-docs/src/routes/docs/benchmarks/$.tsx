import { createFileRoute } from "@tanstack/solid-router";
import { For, type JSX, Match, Show, Switch } from "solid-js";
import CodeBlock from "~/components/code-block";
import { DocNotFound } from "~/components/doc-not-found";
import DocPage from "~/components/doc-page";
import { GITHUB_URL } from "~/components/github-icon";
import {
	ANN_COMPARISON,
	ANN_CONFIG,
	BENCH_DATE,
	BENCH_MACHINE,
	type BenchSource,
	FULL_FSYNC,
	formatCount,
	formatMicros,
	formatNs,
	formatNsExact,
	formatRate,
	formatRatio,
	GRAPH_LATENCY_ROWS,
	GRAPH_METRICS,
	GRAPH_RUN,
	GRAPH_SIZE,
	type GraphMetric,
	OPEN_TIME,
	PAGING_SOURCE,
	type Percentiles,
	PYTHON_GRAPH,
	PYTHON_GRAPH_SOURCE,
	QUERY_ROWS,
	RESULTS_DIR,
	RUST_GRAPH,
	RUST_GRAPH_NO_MVCC,
	RUST_GRAPH_NO_MVCC_SOURCE,
	RUST_GRAPH_SOURCE,
	RUST_SYNC_SWEEP,
	resultsUrl,
	SQLITE_BATCH_NODES,
	SQLITE_BATCH_SOURCE,
	scaling,
	TS_GRAPH_SIZE,
	TS_OVERHEAD,
	TS_OVERHEAD_SOURCE,
	VECTOR_INDEX,
	VECTOR_LATENCY_ROW,
	VECTOR_SOURCE,
	WRITE_SCALING,
	WRITE_SCALING_NO_MVCC,
	WRITE_SCALING_SHAPES,
	WRITER_COUNTS,
	writeScalingRow,
} from "~/lib/benchmarks";
import { loadDocSlug } from "~/lib/doc-route";

export const Route = createFileRoute("/docs/benchmarks/$")({
	loader: loadDocSlug,
	component: BenchmarksSplatPage,
	notFoundComponent: () => (
		<DocNotFound backHref="/docs/benchmarks" backLabel="Back to benchmarks" />
	),
});

function BenchmarksSplatPage() {
	const data = Route.useLoaderData();
	return <DocPageContent slug={data().slug} />;
}

// Commands copied from docs/BENCHMARKS.md ("Running Benchmarks").
const REFRESH_COMMAND = `ray-rs/scripts/bench-refresh.sh --list   # the matrix
ray-rs/scripts/bench-refresh.sh          # every run on this site
cd ray-docs && bun run bench:data        # this site's data from the new logs`;

const RUST_GRAPH_COMMAND = `cd ray-rs
cargo run --release --example single_file_raw_bench --no-default-features -- \\
  --nodes 10000 --edges 50000 --iterations 10000 \\
  --wal-size 268435456 --no-auto-checkpoint --seed 42 --sync-mode normal --mvcc`;

const PYTHON_GRAPH_COMMAND = `cd ray-rs/python/benchmarks
python3 benchmark_single_file_raw.py \\
  --nodes 10000 --edges 50000 --iterations 10000 \\
  --wal-size 268435456 --no-auto-checkpoint --seed 42 --sync-mode normal --mvcc`;

const TS_OVERHEAD_COMMAND = `cd ray-rs
node --import @oxc-node/core/register benchmark/bench-fluent-vs-lowlevel.ts --mvcc --seed 42`;

const VECTOR_COMMAND = `cd ray-rs
cargo run --release --example vector_bench --no-default-features -- \\
  --vectors 10000 --dimensions 768 --iterations 1000 --k 10 --n-probe 10 --seed 42 --no-output`;

const SQLITE_COMMAND = `cd docs/benchmarks
python3 sqlite_single_file_raw_bench.py \\
  --nodes 10000 --edges 50000 --iterations 10000 --sync-mode normal`;

const WRITE_SCALING_COMMAND = `cd ray-rs
cargo run --release --example multi_writer_throughput_bench --no-default-features -- \\
  --threads 8 --tx-per-thread 200 --batch-size 200 --edges-per-node 1 \\
  --edge-types 3 --edge-props 10 --wal-size 1073741824 --sync-mode normal --mvcc`;

const GRAPH_SNAPSHOT_METRICS: GraphMetric[] = [
	"keyLookup",
	"traverseOut",
	"edgeExists",
	"batchNodes",
];

const labelFor = (metric: GraphMetric) =>
	GRAPH_METRICS.find((m) => m.id === metric)?.label ?? metric;

const WRITES_200 = writeScalingRow("200node", "normal");
const WRITES_1 = writeScalingRow("1node", "normal");

/** Names the raw log(s) behind the table above it, plus the run's settings. */
function SourceNote(props: { logs: string[]; config: string }) {
	return (
		<p class="-mt-4 text-[13px] leading-relaxed text-slate-500">
			Source:{" "}
			<For each={props.logs}>
				{(log, index) => (
					<>
						<Show when={index() > 0}>, </Show>
						<a
							href={resultsUrl(GITHUB_URL, log)}
							target="_blank"
							rel="noopener noreferrer"
							class="text-slate-400"
						>
							<code>{log}</code>
						</a>
					</>
				)}
			</For>{" "}
			({props.config})
		</p>
	);
}

function RunSource(props: { source: BenchSource }) {
	return <SourceNote logs={[props.source.log]} config={props.source.config} />;
}

/** Operation | p50 | p95 for one single-file raw run, at log precision. */
function GraphTable(props: {
	data: Record<GraphMetric, Percentiles>;
	/** Adds p50 and p95 columns for this run, labelled `compareLabel` */
	compare?: Record<GraphMetric, Percentiles>;
	compareLabel?: string;
}) {
	return (
		<table>
			<thead>
				<tr>
					<th>Operation</th>
					<th>p50</th>
					<th>p95</th>
					<Show when={props.compare}>
						<th>p50, {props.compareLabel}</th>
						<th>p95, {props.compareLabel}</th>
					</Show>
				</tr>
			</thead>
			<tbody>
				<For each={GRAPH_METRICS}>
					{(metric) => (
						<tr>
							<td>{metric.label}</td>
							<td>{formatNsExact(props.data[metric.id].p50)}</td>
							<td>{formatNsExact(props.data[metric.id].p95)}</td>
							<Show when={props.compare}>
								{(compare) => (
									<>
										<td>{formatNsExact(compare()[metric.id].p50)}</td>
										<td>{formatNsExact(compare()[metric.id].p95)}</td>
									</>
								)}
							</Show>
						</tr>
					)}
				</For>
			</tbody>
		</table>
	);
}

function Note(props: { children: JSX.Element }) {
	return (
		<div class="not-prose rounded-lg border border-kite-cyan/20 bg-kite-cyan/[0.05] px-4 py-3 text-[14px] leading-relaxed text-slate-300">
			{props.children}
		</div>
	);
}

function OverviewPage() {
	return (
		<DocPage slug="benchmarks">
			<p>
				Latency and throughput of KiteDB's single-file engine, vector index, and
				language bindings, measured on one machine on {BENCH_DATE} with MVCC on
				(the default). Every table names the raw log in{" "}
				<code>{RESULTS_DIR}/</code> that its numbers come from, along with the
				dataset and durability settings of that run.{" "}
				<code>docs/BENCHMARKS.md</code> has the full notes and the earlier
				results.
			</p>

			<h2 id="benchmark-categories">Benchmark pages</h2>
			<ul>
				<li>
					<a href="/docs/benchmarks/graph">Graph benchmarks</a>: single-file
					engine latency from Rust and Python, MVCC on and off, sync modes,
					write scaling, open time and paging
				</li>
				<li>
					<a href="/docs/benchmarks/vector">Vector benchmarks</a>: vector index
					insert, build, lookup, and search (Rust), and IVF against IVF-PQ
				</li>
				<li>
					<a href="/docs/benchmarks/cross-language">
						Cross-language benchmarks
					</a>
					: Rust and Python side by side, plus the cost of the TypeScript fluent
					API
				</li>
			</ul>

			<h2 id="test-environment">Test environment</h2>
			<ul>
				<li>
					{BENCH_MACHINE.cpu}, {BENCH_MACHINE.cores}, {BENCH_MACHINE.memory} RAM
				</li>
				<li>
					{BENCH_MACHINE.os} ({BENCH_MACHINE.darwin}), {BENCH_MACHINE.power}
				</li>
				<li>Rust {BENCH_MACHINE.rust}</li>
				<li>Node {BENCH_MACHINE.node}</li>
				<li>Python {BENCH_MACHINE.python}</li>
				<li>SQLite {BENCH_MACHINE.sqlite} (baseline)</li>
				<li>
					Commit <code>{BENCH_MACHINE.commit}</code>, release builds
				</li>
			</ul>
			<p>
				Each configuration ran five times, interleaved with the others, with a
				fixed seed; each figure is the run with the median value, copied from
				the log.
			</p>

			<h2 id="highlights">Highlights</h2>
			<p>
				These are the numbers on the homepage, rounded. The graph and vector
				pages list them at full log precision.
			</p>

			<h3 id="graph-highlights">Graph operations</h3>
			<table>
				<thead>
					<tr>
						<th>Operation</th>
						<th>Workload</th>
						<th>p50</th>
						<th>p95</th>
					</tr>
				</thead>
				<tbody>
					<For each={GRAPH_LATENCY_ROWS}>
						{(row) => (
							<tr>
								<td>{row.label}</td>
								<td>{row.detail}</td>
								<td>{formatNs(row.p50)}</td>
								<td>{formatNs(row.p95)}</td>
							</tr>
						)}
					</For>
				</tbody>
			</table>
			<RunSource source={RUST_GRAPH_SOURCE} />

			<h3 id="vector-highlights">Vector index</h3>
			<table>
				<thead>
					<tr>
						<th>Operation</th>
						<th>p50</th>
						<th>p95</th>
					</tr>
				</thead>
				<tbody>
					<tr>
						<td>
							Insert one vector ({formatCount(VECTOR_INDEX.vectors)} inserts)
						</td>
						<td>{formatNs(VECTOR_INDEX.set.p50)}</td>
						<td>{formatNs(VECTOR_INDEX.set.p95)}</td>
					</tr>
					<tr>
						<td>build_index() (single run)</td>
						<td>{formatNs(VECTOR_INDEX.buildIndex)}</td>
						<td>n/a</td>
					</tr>
					<tr>
						<td>Get vector (random)</td>
						<td>{formatNs(VECTOR_INDEX.get.p50)}</td>
						<td>{formatNs(VECTOR_INDEX.get.p95)}</td>
					</tr>
					<tr>
						<td>Search ({VECTOR_LATENCY_ROW.detail})</td>
						<td>{formatNs(VECTOR_LATENCY_ROW.p50)}</td>
						<td>{formatNs(VECTOR_LATENCY_ROW.p95)}</td>
					</tr>
				</tbody>
			</table>
			<RunSource source={VECTOR_SOURCE} />

			<Note>
				These runs use MVCC, the default since 0.3.0, and every commit is
				group-committed. The{" "}
				<a href="/docs/benchmarks/graph#rust-core">graph benchmarks</a> compare
				the same run with MVCC off. The February 2026 results in{" "}
				<code>docs/BENCHMARKS.md</code> ran without MVCC on another machine (an
				Apple M4), so a difference from them mixes the code and the hardware.
				macOS's clock ticks every 41.67 ns, so 42 ns, the smallest nonzero time
				these benchmarks report, means one tick or less.
			</Note>

			<h2 id="bindings">Bindings snapshot</h2>
			<p>
				p50 latency for the same graph workload from Rust and through the Python
				bindings.
			</p>
			<table>
				<thead>
					<tr>
						<th>Operation</th>
						<th>Rust</th>
						<th>Python</th>
					</tr>
				</thead>
				<tbody>
					<For each={GRAPH_SNAPSHOT_METRICS}>
						{(metric) => (
							<tr>
								<td>{labelFor(metric)}</td>
								<td>{formatNs(RUST_GRAPH[metric].p50)}</td>
								<td>{formatNs(PYTHON_GRAPH[metric].p50)}</td>
							</tr>
						)}
					</For>
				</tbody>
			</table>
			<SourceNote
				logs={[RUST_GRAPH_SOURCE.log, PYTHON_GRAPH_SOURCE.log]}
				config={RUST_GRAPH_SOURCE.config}
			/>
			<p>
				<a href="/docs/benchmarks/cross-language">Cross-language benchmarks</a>{" "}
				has every operation, plus the TypeScript API.
			</p>

			<h2 id="parallel-write-scaling">Parallel write scaling</h2>
			<p>
				With MVCC, writer threads build their transactions in parallel, and
				commits that arrive together are written as one group. With{" "}
				<code>syncMode=Normal</code>, eight writers reach{" "}
				{formatRate(WRITES_200.runs[8].txPerSec)} with transactions of 200 nodes
				and 200 edges, {formatRatio(scaling(WRITES_200, 8))} the rate of one
				writer, and {formatRate(WRITES_1.runs[8].txPerSec)} with one-node
				transactions, against {formatRate(WRITES_1.runs[1].txPerSec)} for one
				writer. The{" "}
				<a href="/docs/benchmarks/graph#parallel-write-scaling">
					graph benchmarks
				</a>{" "}
				have the 1, 4 and 8-writer measurements in both sync modes.
			</p>

			<h2 id="running">Running benchmarks</h2>
			<p>
				One script reruns every benchmark on this site and writes dated logs;
				run it on a quiet machine, from the repository root:
			</p>
			<CodeBlock code={REFRESH_COMMAND} language="bash" />
			<p>
				The individual commands, from <code>docs/BENCHMARKS.md</code>. Rust
				core, graph operations:
			</p>
			<CodeBlock code={RUST_GRAPH_COMMAND} language="bash" />
			<p>Python bindings, graph operations:</p>
			<CodeBlock code={PYTHON_GRAPH_COMMAND} language="bash" />
			<p>TypeScript, fluent vs low-level API:</p>
			<CodeBlock code={TS_OVERHEAD_COMMAND} language="bash" />
			<p>Rust vector index:</p>
			<CodeBlock code={VECTOR_COMMAND} language="bash" />
		</DocPage>
	);
}

function GraphPage() {
	return (
		<DocPage slug="benchmarks/graph">
			<p>
				Latency of the single-file engine on a graph of {GRAPH_SIZE}, measured
				from Rust and through the Python bindings, then write throughput with
				several writer threads, open time and paging. The runs are from{" "}
				{BENCH_DATE} on an {BENCH_MACHINE.cpu}, and each table names its raw
				log.
			</p>

			<h2 id="test-configuration">Test configuration</h2>
			<ul>
				<li>
					Graph: {GRAPH_RUN.nodes.toLocaleString("en-US")} nodes,{" "}
					{GRAPH_RUN.edges.toLocaleString("en-US")} edges, {GRAPH_RUN.edgeTypes}{" "}
					edge types, {GRAPH_RUN.edgeProps} props per edge
				</li>
				<li>Iterations: {GRAPH_RUN.iterations.toLocaleString("en-US")}</li>
				<li>
					Vectors: {GRAPH_RUN.vectors.toLocaleString("en-US")} vectors of{" "}
					{GRAPH_RUN.vectorDims} dimensions (for the vector rows; set in batches
					of 100)
				</li>
				<li>
					WAL: {GRAPH_RUN.walMb} MB with auto-checkpoint{" "}
					{GRAPH_RUN.autoCheckpoint ? "on" : "off"}, so write timings show the
					raw commit cost
				</li>
				<li>
					Durability: sync=normal, unless a table says otherwise; every commit
					is group-committed
				</li>
				<li>
					MVCC: on (the default since 0.3.0); one run with{" "}
					<code>--no-mvcc</code> for comparison
				</li>
				<li>
					Seed {GRAPH_RUN.seed}; each row is the median of five interleaved runs
				</li>
			</ul>

			<h2 id="rust-core">Rust core</h2>
			<p>
				Measured with the <code>single_file_raw_bench</code> example, MVCC on,
				and the same run with MVCC off.
			</p>
			<GraphTable
				data={RUST_GRAPH}
				compare={RUST_GRAPH_NO_MVCC}
				compareLabel="MVCC off"
			/>
			<SourceNote
				logs={[RUST_GRAPH_SOURCE.log, RUST_GRAPH_NO_MVCC_SOURCE.log]}
				config={RUST_GRAPH_SOURCE.config}
			/>

			<h2 id="python-bindings">Python bindings</h2>
			<p>
				The same benchmark through the Python bindings, with{" "}
				<code>benchmark_single_file_raw.py</code>.
			</p>
			<GraphTable data={PYTHON_GRAPH} />
			<RunSource source={PYTHON_GRAPH_SOURCE} />

			<h2 id="sync-mode-group-commit">Sync modes</h2>
			<p>
				Batch writes from the Rust benchmark on the same graph, MVCC on, in each
				sync mode.
			</p>
			<table>
				<thead>
					<tr>
						<th>Sync mode</th>
						<th>100 nodes, p50</th>
						<th>100 nodes, p95</th>
						<th>100 edges, p50</th>
						<th>Set 100 vectors, p50</th>
					</tr>
				</thead>
				<tbody>
					<For each={RUST_SYNC_SWEEP}>
						{(row) => (
							<tr>
								<td>
									<code>{row.syncMode}</code>
								</td>
								<td>{formatNsExact(row.batchNodes.p50)}</td>
								<td>{formatNsExact(row.batchNodes.p95)}</td>
								<td>{formatNsExact(row.batchEdges.p50)}</td>
								<td>{formatNsExact(row.setVectors.p50)}</td>
							</tr>
						)}
					</For>
				</tbody>
			</table>
			<SourceNote
				logs={RUST_SYNC_SWEEP.map((row) => row.log)}
				config={`${GRAPH_SIZE}, one log per sync mode`}
			/>
			<p>
				<code>full</code> returns once the commit is fsynced. On macOS a plain
				fsync leaves the writes in the drive's cache; with{" "}
				<code>fullFsync</code> (<code>F_FULLFSYNC</code>) the sync reaches the
				drive and costs milliseconds, as the write-scaling numbers below show.
				Every commit is group-committed in every mode: commits that arrive
				together share one WAL write, one header write and, in <code>full</code>
				, one sync, and a single writer waits for no one. The{" "}
				<code>groupCommitEnabled</code> and <code>groupCommitWindowMs</code>{" "}
				options have no effect.
			</p>

			<h2 id="parallel-write-scaling">Parallel writes</h2>
			<p>
				With MVCC, writer threads build their transactions in parallel. Commits
				that arrive together are written as one group (one WAL write, one header
				write, one fsync in <code>full</code> mode), and the next group is
				written while one publishes. Publishing, each commit's merge into the
				in-memory delta, runs one group at a time, so with{" "}
				<code>syncMode=Normal</code> large transactions gain more from extra
				writers than small ones, whose cost is mostly that shared pipeline. In{" "}
				<code>full</code> mode a group also shares its fsync, so one-node
				transactions gain from more writers too.
			</p>
			<table>
				<thead>
					<tr>
						<th>Transactions</th>
						<th>Sync</th>
						<For each={WRITER_COUNTS}>
							{(writers) => (
								<th>
									{writers} writer{writers > 1 ? "s" : ""}
								</th>
							)}
						</For>
						<th>8 vs 1</th>
					</tr>
				</thead>
				<tbody>
					<For each={WRITE_SCALING}>
						{(row) => (
							<tr>
								<td>{WRITE_SCALING_SHAPES[row.shape].label}</td>
								<td>
									<code>{row.syncMode}</code>
								</td>
								<For each={WRITER_COUNTS}>
									{(writers) => (
										<td>{formatRate(row.runs[writers].txPerSec)}</td>
									)}
								</For>
								<td>{formatRatio(scaling(row, 8))}</td>
							</tr>
						)}
					</For>
				</tbody>
			</table>
			<SourceNote
				logs={[WRITES_200.runs[8].log, WRITES_1.runs[8].log]}
				config={`transactions per second, MVCC on, 1 GB WAL, auto-checkpoint off; ${WRITE_SCALING_SHAPES["200node"].label}: ${WRITE_SCALING_SHAPES["200node"].config}; ${WRITE_SCALING_SHAPES["1node"].label}: ${WRITE_SCALING_SHAPES["1node"].config}; one log per cell, named by sync mode, size and writers`}
			/>
			<p>
				Without MVCC (deprecated), write transactions run one at a time; with{" "}
				<code>syncMode=Normal</code> one writer reaches{" "}
				{formatRate(WRITE_SCALING_NO_MVCC["200node"].txPerSec)} with 200-node
				transactions and {formatRate(WRITE_SCALING_NO_MVCC["1node"].txPerSec)}{" "}
				with one-node transactions. With <code>full</code> and{" "}
				<code>fullFsync</code>, one-node transactions commit at{" "}
				<For each={FULL_FSYNC}>
					{(run, index) => (
						<>
							<Show when={index() > 0}> and </Show>
							{formatRate(run.txPerSec)} with {run.writers} writer
							{run.writers > 1 ? "s" : ""}
						</>
					)}
				</For>
				, since a group shares its sync.
			</p>
			<SourceNote
				logs={[
					WRITE_SCALING_NO_MVCC["200node"].log,
					WRITE_SCALING_NO_MVCC["1node"].log,
					...FULL_FSYNC.map((run) => run.log),
				]}
				config="same shapes as the table"
			/>
			<CodeBlock code={WRITE_SCALING_COMMAND} language="bash" />

			<h2 id="open-and-paging">Open time and paging</h2>
			<p>
				Opening a checkpointed database read-only checks the snapshot's
				structure and inflates its compressed sections on several threads:
			</p>
			<table>
				<thead>
					<tr>
						<th>Database</th>
						<th>Open + close (median)</th>
					</tr>
				</thead>
				<tbody>
					<For each={OPEN_TIME}>
						{(run) => (
							<tr>
								<td>
									{formatCount(run.nodes)} nodes, {formatCount(run.edges)} edges
								</td>
								<td>{formatNsExact(run.median)}</td>
							</tr>
						)}
					</For>
				</tbody>
			</table>
			<SourceNote
				logs={OPEN_TIME.map((run) => run.log)}
				config="query_core_bench --sections open, MVCC on"
			/>
			<p>
				Pages, counts and neighbor reads with every change still in the WAL and
				after a checkpoint folded them into the snapshot (
				<code>query_core_bench</code>, medians):
			</p>
			<table>
				<thead>
					<tr>
						<th>Operation</th>
						<th>In the WAL</th>
						<th>After a checkpoint</th>
					</tr>
				</thead>
				<tbody>
					<For each={QUERY_ROWS}>
						{(row) => (
							<tr>
								<td>{row.label}</td>
								<td>{formatMicros(row.delta)}</td>
								<td>{formatMicros(row.snapshot)}</td>
							</tr>
						)}
					</For>
				</tbody>
			</table>
			<RunSource source={PAGING_SOURCE} />

			<h2 id="sqlite">SQLite baseline</h2>
			<p>
				<code>docs/benchmarks/sqlite_single_file_raw_bench.py</code> runs the
				batch-write benchmark against SQLite on a graph of the same size, on the
				same machine. It configures SQLite with WAL mode,{" "}
				<code>synchronous=normal</code>, <code>temp_store=MEMORY</code>,{" "}
				<code>locking_mode=EXCLUSIVE</code>, <code>cache_size=256MB</code>, and
				WAL autocheckpoint disabled. Edge props live in a separate table; edges
				use <code>INSERT OR IGNORE</code> and props use{" "}
				<code>INSERT OR REPLACE</code>.
			</p>
			<table>
				<thead>
					<tr>
						<th>Batch write (100 nodes)</th>
						<th>p50</th>
						<th>p95</th>
					</tr>
				</thead>
				<tbody>
					<tr>
						<td>SQLite</td>
						<td>{formatNsExact(SQLITE_BATCH_NODES.p50)}</td>
						<td>{formatNsExact(SQLITE_BATCH_NODES.p95)}</td>
					</tr>
					<tr>
						<td>KiteDB (Rust core, MVCC on)</td>
						<td>{formatNsExact(RUST_GRAPH.batchNodes.p50)}</td>
						<td>{formatNsExact(RUST_GRAPH.batchNodes.p95)}</td>
					</tr>
				</tbody>
			</table>
			<SourceNote
				logs={[SQLITE_BATCH_SOURCE.log, RUST_GRAPH_SOURCE.log]}
				config={SQLITE_BATCH_SOURCE.config}
			/>
			<CodeBlock code={SQLITE_COMMAND} language="bash" />

			<h2 id="running">Running benchmarks</h2>
			<p>
				Commands from <code>docs/BENCHMARKS.md</code>, run from the repository
				root. Rust core:
			</p>
			<CodeBlock code={RUST_GRAPH_COMMAND} language="bash" />
			<p>Python bindings:</p>
			<CodeBlock code={PYTHON_GRAPH_COMMAND} language="bash" />
			<p>Both scripts accept the same optional flags:</p>
			<ul>
				<li>
					<code>--edge-types N</code> (default 3) and{" "}
					<code>--edge-props N</code> (default 10)
				</li>
				<li>
					<code>--sync-mode full|normal|off</code> (default <code>normal</code>)
				</li>
				<li>
					<code>--mvcc</code> / <code>--no-mvcc</code> (default: the library
					default, MVCC on) and <code>--seed N</code> (default 42)
				</li>
				<li>
					<code>--group-commit-enabled</code> and{" "}
					<code>--group-commit-window-ms N</code> (accepted, but they have no
					effect now that every commit is group-committed)
				</li>
			</ul>
			<p>
				Without <code>--wal-size</code> and <code>--no-auto-checkpoint</code>{" "}
				the scripts use a 64 MB WAL with auto-checkpoint on.{" "}
				<code>ray-rs/scripts/bench-refresh.sh --list</code> prints the command
				of every run on this page.
			</p>
		</DocPage>
	);
}

function VectorPage() {
	const [ivf, ivfPq] = ANN_COMPARISON;
	return (
		<DocPage slug="benchmarks/vector">
			<p>
				Vector index latency through the Rust API, measured with the{" "}
				<code>vector_bench</code> and <code>vector_ann_bench</code> examples on{" "}
				{BENCH_DATE} ({BENCH_MACHINE.cpu}).
			</p>

			<h2 id="config">Test configuration</h2>
			<ul>
				<li>
					Vectors: {VECTOR_INDEX.vectors.toLocaleString("en-US")} random vectors
					of {VECTOR_INDEX.dimensions} dimensions
				</li>
				<li>Metric: cosine</li>
				<li>Index: IVF with {VECTOR_INDEX.clusters} clusters</li>
				<li>
					Search: k={VECTOR_INDEX.k}, nProbe={VECTOR_INDEX.nProbe},{" "}
					{VECTOR_INDEX.iterations.toLocaleString("en-US")} iterations
				</li>
			</ul>

			<h2 id="results">Results</h2>
			<table>
				<thead>
					<tr>
						<th>Operation</th>
						<th>p50</th>
						<th>p95</th>
					</tr>
				</thead>
				<tbody>
					<tr>
						<td>
							Insert one vector ({formatCount(VECTOR_INDEX.vectors)} inserts)
						</td>
						<td>{formatNsExact(VECTOR_INDEX.set.p50)}</td>
						<td>{formatNsExact(VECTOR_INDEX.set.p95)}</td>
					</tr>
					<tr>
						<td>build_index() (single run)</td>
						<td>{formatNsExact(VECTOR_INDEX.buildIndex)}</td>
						<td>n/a</td>
					</tr>
					<tr>
						<td>Get vector (random)</td>
						<td>{formatNsExact(VECTOR_INDEX.get.p50)}</td>
						<td>{formatNsExact(VECTOR_INDEX.get.p95)}</td>
					</tr>
					<tr>
						<td>Search</td>
						<td>{formatNsExact(VECTOR_INDEX.search.p50)}</td>
						<td>{formatNsExact(VECTOR_INDEX.search.p95)}</td>
					</tr>
				</tbody>
			</table>
			<RunSource source={VECTOR_SOURCE} />

			<Note>
				<code>vector_bench</code> uses the default ANN algorithm of{" "}
				<code>VectorIndex</code>, <code>auto</code>: plain IVF below 50,000
				vectors or 512 dimensions, IVF-PQ from there on. At{" "}
				{formatCount(VECTOR_INDEX.vectors)} vectors it builds IVF, as the log
				header shows.
			</Note>

			<h2 id="ivf-vs-ivf-pq">IVF and IVF-PQ</h2>
			<p>
				Where <code>auto</code> switches to IVF-PQ, at 768 dimensions and 50,000
				vectors, IVF-PQ searches compact product-quantization codes, then
				re-ranks its best candidates by exact distance. Here its search p50 is{" "}
				{formatRatio(ivf.search.p50 / ivfPq.search.p50)} faster than IVF's (
				{formatNs(ivfPq.search.p50)} against {formatNs(ivf.search.p50)}) at a
				recall@10 of {ivfPq.recallAtK.toFixed(2)} against{" "}
				{ivf.recallAtK.toFixed(2)}, and its build takes{" "}
				{formatRatio(ivfPq.build / ivf.build)} as long.
			</p>
			<table>
				<thead>
					<tr>
						<th>Algorithm</th>
						<th>Build</th>
						<th>Search p50</th>
						<th>Search p95</th>
						<th>Recall@10</th>
					</tr>
				</thead>
				<tbody>
					<For each={ANN_COMPARISON}>
						{(run) => (
							<tr>
								<td>{run.algorithm}</td>
								<td>{formatNsExact(run.build)}</td>
								<td>{formatNsExact(run.search.p50)}</td>
								<td>{formatNsExact(run.search.p95)}</td>
								<td>{run.recallAtK.toFixed(3)}</td>
							</tr>
						)}
					</For>
				</tbody>
			</table>
			<SourceNote
				logs={ANN_COMPARISON.map((run) => run.log)}
				config={ANN_CONFIG}
			/>

			<h2 id="running">Running benchmarks</h2>
			<p>
				Command from <code>docs/BENCHMARKS.md</code>, run from the repository
				root:
			</p>
			<CodeBlock code={VECTOR_COMMAND} language="bash" />
			<p>
				A Python version lives at{" "}
				<code>ray-rs/python/benchmarks/benchmark_vector.py</code>; its results
				are not published.
			</p>
		</DocPage>
	);
}

function CrossLanguagePage() {
	return (
		<DocPage slug="benchmarks/cross-language">
			<p>
				Rust and Python run the same graph benchmark, so their numbers compare
				directly. The TypeScript benchmark measures the fluent API against the
				low-level API on a smaller graph.
			</p>

			<h2 id="graph-benchmarks">Rust and Python</h2>
			<p>
				p50 latency from the single-file raw benchmark on the same {GRAPH_SIZE}{" "}
				graph, MVCC on.
			</p>
			<table>
				<thead>
					<tr>
						<th>Operation</th>
						<th>Rust p50</th>
						<th>Python p50</th>
					</tr>
				</thead>
				<tbody>
					<For each={GRAPH_METRICS}>
						{(metric) => (
							<tr>
								<td>{metric.label}</td>
								<td>{formatNsExact(RUST_GRAPH[metric.id].p50)}</td>
								<td>{formatNsExact(PYTHON_GRAPH[metric.id].p50)}</td>
							</tr>
						)}
					</For>
				</tbody>
			</table>
			<SourceNote
				logs={[RUST_GRAPH_SOURCE.log, PYTHON_GRAPH_SOURCE.log]}
				config={RUST_GRAPH_SOURCE.config}
			/>

			<h2 id="typescript-overhead">TypeScript: fluent vs low-level API</h2>
			<p>
				What the fluent API (<code>db.get</code>, <code>db.from().out()</code>)
				costs over the low-level calls it wraps, measured in Node. This run uses
				a smaller graph ({TS_GRAPH_SIZE}), so compare its rows with each other,
				not with the Rust and Python table.
			</p>
			<table>
				<thead>
					<tr>
						<th>Operation</th>
						<th>Low-level p50</th>
						<th>Fluent p50</th>
						<th>Overhead</th>
					</tr>
				</thead>
				<tbody>
					<For each={TS_OVERHEAD}>
						{(row) => (
							<tr>
								<td>{row.label}</td>
								<td>{formatNsExact(row.lowLevel)}</td>
								<td>{formatNsExact(row.fluent)}</td>
								<td>{row.overhead.toFixed(2)}x</td>
							</tr>
						)}
					</For>
				</tbody>
			</table>
			<RunSource source={TS_OVERHEAD_SOURCE} />

			<p>
				Vector index numbers are on the{" "}
				<a href="/docs/benchmarks/vector">vector benchmarks</a> page.
			</p>

			<h2 id="running">Running benchmarks</h2>
			<p>
				Commands from <code>docs/BENCHMARKS.md</code>, run from the repository
				root. Rust core:
			</p>
			<CodeBlock code={RUST_GRAPH_COMMAND} language="bash" />
			<p>Python bindings:</p>
			<CodeBlock code={PYTHON_GRAPH_COMMAND} language="bash" />
			<p>TypeScript, fluent vs low-level API:</p>
			<CodeBlock code={TS_OVERHEAD_COMMAND} language="bash" />
		</DocPage>
	);
}

function DocPageContent(props: { slug: string }) {
	return (
		<Switch
			fallback={
				<DocPage slug={props.slug}>
					<p>This benchmark page is coming soon.</p>
				</DocPage>
			}
		>
			<Match when={props.slug === "benchmarks"}>
				<OverviewPage />
			</Match>
			<Match when={props.slug === "benchmarks/graph"}>
				<GraphPage />
			</Match>
			<Match when={props.slug === "benchmarks/vector"}>
				<VectorPage />
			</Match>
			<Match when={props.slug === "benchmarks/cross-language"}>
				<CrossLanguagePage />
			</Match>
		</Switch>
	);
}
