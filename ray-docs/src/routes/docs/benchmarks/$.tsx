import { createFileRoute } from "@tanstack/solid-router";
import { For, type JSX, Match, Show, Switch } from "solid-js";
import CodeBlock from "~/components/code-block";
import { DocNotFound } from "~/components/doc-not-found";
import DocPage from "~/components/doc-page";
import { GITHUB_URL } from "~/components/github-icon";
import {
	type BenchSource,
	formatNs,
	formatNsExact,
	formatRate,
	GRAPH_LATENCY_ROWS,
	GRAPH_METRICS,
	type GraphMetric,
	MULTI_WRITER,
	MULTI_WRITER_CONFIG,
	type Percentiles,
	PYTHON_GRAPH,
	PYTHON_GRAPH_SOURCE,
	RESULTS_DIR,
	RUST_GRAPH,
	RUST_GRAPH_SOURCE,
	RUST_SYNC_SWEEP,
	RUST_SYNC_SWEEP_LOGS,
	resultsUrl,
	SQLITE_BATCH_NODES,
	SQLITE_BATCH_SOURCE,
	TS_OVERHEAD,
	TS_OVERHEAD_SOURCE,
	VECTOR_INDEX,
	VECTOR_LATENCY_ROW,
	VECTOR_SOURCE,
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
const RUST_GRAPH_COMMAND = `cd ray-rs
cargo run --release --example single_file_raw_bench --no-default-features -- \\
  --nodes 10000 --edges 50000 --iterations 10000 \\
  --wal-size 268435456 --no-auto-checkpoint --sync-mode normal`;

const PYTHON_GRAPH_COMMAND = `cd ray-rs/python/benchmarks
python3 benchmark_single_file_raw.py \\
  --nodes 10000 --edges 50000 --iterations 10000 \\
  --wal-size 268435456 --no-auto-checkpoint --sync-mode normal`;

const TS_OVERHEAD_COMMAND = `cd ray-rs
node --import @oxc-node/core/register benchmark/bench-fluent-vs-lowlevel.ts`;

const VECTOR_COMMAND = `cd ray-rs
cargo run --release --example vector_bench --no-default-features -- \\
  --vectors 10000 --dimensions 768 --iterations 1000 --k 10 --n-probe 10`;

const SQLITE_COMMAND = `cd docs/benchmarks
python3 sqlite_single_file_raw_bench.py \\
  --nodes 10000 --edges 50000 --iterations 10000 --sync-mode normal`;

const GRAPH_SNAPSHOT_METRICS: GraphMetric[] = [
	"keyLookup",
	"traverseOut",
	"edgeExists",
	"batchNodes",
];

const labelFor = (metric: GraphMetric) =>
	GRAPH_METRICS.find((m) => m.id === metric)?.label ?? metric;

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
							href={resultsUrl(GITHUB_URL, log.includes("{") ? undefined : log)}
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
function GraphTable(props: { data: Record<GraphMetric, Percentiles> }) {
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
				<For each={GRAPH_METRICS}>
					{(metric) => (
						<tr>
							<td>{metric.label}</td>
							<td>{formatNsExact(props.data[metric.id].p50)}</td>
							<td>{formatNsExact(props.data[metric.id].p95)}</td>
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
				Latency for KiteDB's single-file engine, vector index, and language
				bindings, measured on one machine. Every table names the raw log in{" "}
				<code>{RESULTS_DIR}/</code> that its numbers come from, along with the
				dataset and durability settings of that run.{" "}
				<code>docs/BENCHMARKS.md</code> has the full notes.
			</p>

			<h2 id="benchmark-categories">Benchmark pages</h2>
			<ul>
				<li>
					<a href="/docs/benchmarks/graph">Graph benchmarks</a>: single-file
					engine latency from Rust and Python, sync modes, group commit, and
					parallel writes
				</li>
				<li>
					<a href="/docs/benchmarks/vector">Vector benchmarks</a>: vector index
					insert, build, lookup, and search (Rust)
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
				<li>Apple M4, 16 GB RAM</li>
				<li>macOS, Darwin 25.3.0</li>
				<li>Rust 1.88.0</li>
				<li>Node 24.12.0</li>
				<li>Bun 1.3.5</li>
				<li>Python 3.12.8</li>
			</ul>

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
						<td>Insert one vector (10k inserts)</td>
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
				Group commit was off for the graph runs. When they were made, a commit
				in sync=normal mode with group commit on could wait up to the
				group-commit window (2 ms by default), so a single-threaded batch write
				took milliseconds instead of microseconds. Since then every commit is
				group-committed, no commit waits for others, and the option has no
				effect. The{" "}
				<a href="/docs/benchmarks/graph#sync-mode-group-commit">
					graph benchmarks
				</a>{" "}
				show both settings.
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
				Write throughput doesn't grow linearly with writer threads. Commits
				serialize WAL ordering and delta application behind{" "}
				<code>commit_lock</code>, so the fastest way to ingest is to prepare
				batches in parallel and send them through one writer using batched
				transactions. The{" "}
				<a href="/docs/benchmarks/graph#parallel-write-scaling">
					graph benchmarks
				</a>{" "}
				have the 8-thread measurements.
			</p>

			<h2 id="running">Running benchmarks</h2>
			<p>
				Commands from <code>docs/BENCHMARKS.md</code>, run from the repository
				root.
			</p>
			<p>Rust core, graph operations:</p>
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
				Latency of the single-file engine on a graph of 10,000 nodes and 50,000
				edges, measured from Rust and through the Python bindings. The numbers
				come from the February 4, 2026 runs, and each table names its raw log.
			</p>

			<h2 id="test-configuration">Test configuration</h2>
			<ul>
				<li>
					Graph: 10,000 nodes, 50,000 edges, 3 edge types, 10 props per edge
				</li>
				<li>Iterations: 10,000</li>
				<li>Vectors: 1,000 vectors of 128 dimensions (for the vector rows)</li>
				<li>
					WAL: 256 MB with auto-checkpoint off, so write timings show the raw
					commit cost
				</li>
				<li>
					Durability: sync=normal, group commit off, unless a table says
					otherwise
				</li>
				<li>
					MVCC: off. These runs predate MVCC as the default (0.3.0);{" "}
					<code>single_file_raw_bench</code> now runs with it unless you pass{" "}
					<code>--no-mvcc</code>
				</li>
			</ul>

			<h2 id="rust-core">Rust core</h2>
			<p>
				Measured with the <code>single_file_raw_bench</code> example.
			</p>
			<GraphTable data={RUST_GRAPH} />
			<RunSource source={RUST_GRAPH_SOURCE} />

			<h2 id="python-bindings">Python bindings</h2>
			<p>
				The same benchmark through the Python bindings, with{" "}
				<code>benchmark_single_file_raw.py</code>.
			</p>
			<GraphTable data={PYTHON_GRAPH} />
			<RunSource source={PYTHON_GRAPH_SOURCE} />

			<h2 id="sync-mode-group-commit">Sync mode and group commit</h2>
			<p>
				Batch write (100 nodes) p50 from the Rust benchmark on the same graph,
				for each sync mode with group commit off and on (2 ms window; these
				runs predate the removal of the window).
			</p>
			<table>
				<thead>
					<tr>
						<th>Sync mode</th>
						<th>Group commit off</th>
						<th>Group commit on</th>
					</tr>
				</thead>
				<tbody>
					<For each={RUST_SYNC_SWEEP}>
						{(row) => (
							<tr>
								<td>
									<code>{row.syncMode}</code>
								</td>
								<td>{formatNsExact(row.groupCommitOff)}</td>
								<td>{formatNsExact(row.groupCommitOn)}</td>
							</tr>
						)}
					</For>
				</tbody>
			</table>
			<SourceNote
				logs={[RUST_SYNC_SWEEP_LOGS]}
				config="10k nodes, 50k edges, 3 edge types, 10 edge props; one log per cell"
			/>
			<p>
				When these runs were made, group commit was an option that applied
				only in <code>normal</code> mode (ignored in <code>full</code> and{" "}
				<code>off</code>, which is why those rows barely change), and a single
				writer waited up to the group-commit window on each commit, so batch
				writes went from microseconds to milliseconds. Now every commit is
				group-committed in every mode, and no commit waits for others: a single
				writer pays nothing, and concurrent writers share WAL writes, headers
				and (in <code>full</code> mode) fsyncs.
			</p>

			<h2 id="parallel-write-scaling">Parallel writes</h2>
			<p>
				Commits that arrive together are written as one group (one WAL write,
				one header write, one fsync in <code>full</code> mode), and the next
				group is written while one publishes. Publishing (each commit's merge
				into the in-memory delta) still runs one group at a time (
				<code>ray-rs/src/core/single_file/transaction.rs</code>
				), so write throughput doesn't scale linearly with writer threads. For
				the highest ingest rate, prepare batches in parallel and send them
				through one writer using batched transactions.
			</p>

			<h3 id="parallel-nodes-edges">Nodes and edges, 8 writer threads</h3>
			<table>
				<thead>
					<tr>
						<th>Group commit</th>
						<th>Transaction rate</th>
						<th>Node rate</th>
						<th>Edge rate</th>
					</tr>
				</thead>
				<tbody>
					<For each={MULTI_WRITER}>
						{(run) => (
							<tr>
								<td>{run.groupCommit ? "On" : "Off"}</td>
								<td>{formatRate(run.txPerSec)}</td>
								<td>{formatRate(run.nodesPerSec)}</td>
								<td>{formatRate(run.edgesPerSec)}</td>
							</tr>
						)}
					</For>
				</tbody>
			</table>
			<SourceNote
				logs={MULTI_WRITER.map((run) => run.log)}
				config={MULTI_WRITER_CONFIG}
			/>
			<p>
				With eight concurrent writers, the group-commit option (since replaced
				by group commit for every commit) raised throughput, the opposite of
				its effect on a single writer at the time.
			</p>
			<p>
				These runs used concurrent write transactions without MVCC, which
				releases up to v0.2.18 allowed. Since then, non-MVCC mode runs one write
				transaction at a time and is deprecated; MVCC, the default since 0.3.0,
				runs write transactions concurrently. These numbers are pending a re-run
				with MVCC.
			</p>

			<h3 id="parallel-vectors">Thread-count sweeps</h3>
			<p>
				A 2026-02-05 sweep of 1 to 16 writer threads (
				<code>multi_writer_throughput_bench</code> and{" "}
				<code>multi_writer_vector_throughput_bench</code>) was run without
				keeping its raw output, so no numbers are published for it. Its takeaway
				still holds: commits are serialized, so prepare data in parallel and
				send it through one writer in batched transactions.
			</p>

			<h2 id="sqlite">SQLite baseline</h2>
			<p>
				<code>docs/benchmarks/sqlite_single_file_raw_bench.py</code> runs the
				batch-write benchmark against SQLite on a graph of the same size. It
				configures SQLite with WAL mode, <code>synchronous=normal</code>,{" "}
				<code>temp_store=MEMORY</code>, <code>locking_mode=EXCLUSIVE</code>,{" "}
				<code>cache_size=256MB</code>, and WAL autocheckpoint disabled. Edge
				props live in a separate table; edges use <code>INSERT OR IGNORE</code>{" "}
				and props use <code>INSERT OR REPLACE</code>.
			</p>
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
						<td>Batch write (100 nodes)</td>
						<td>{formatNsExact(SQLITE_BATCH_NODES.p50)}</td>
						<td>{formatNsExact(SQLITE_BATCH_NODES.p95)}</td>
					</tr>
				</tbody>
			</table>
			<RunSource source={SQLITE_BATCH_SOURCE} />
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
					<code>--group-commit-enabled</code> and{" "}
					<code>--group-commit-window-ms N</code> (accepted, but they have no
					effect now that every commit is group-committed)
				</li>
			</ul>
			<p>
				The Rust and Python commands above match the 2026-02-04 logs (256 MB
				WAL, auto-checkpoint off). Without <code>--wal-size</code> and{" "}
				<code>--no-auto-checkpoint</code> the scripts use a 64 MB WAL with
				auto-checkpoint on. Change <code>--sync-mode</code> to reproduce the
				other sweep files (the <code>-gc</code> files used{" "}
				<code>--group-commit-enabled</code>, which no longer has an effect).
			</p>
		</DocPage>
	);
}

function VectorPage() {
	return (
		<DocPage slug="benchmarks/vector">
			<p>
				Vector index latency through the Rust API, measured with the{" "}
				<code>vector_bench</code> example on February 3, 2026.
			</p>

			<h2 id="config">Test configuration</h2>
			<ul>
				<li>Vectors: 10,000 random vectors of 768 dimensions</li>
				<li>Metric: cosine</li>
				<li>Index: IVF with 100 clusters</li>
				<li>Search: k=10, nProbe=10, 1,000 iterations</li>
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
						<td>Insert one vector (10k inserts)</td>
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
						<td>Search (k=10, nProbe=10)</td>
						<td>{formatNsExact(VECTOR_INDEX.search.p50)}</td>
						<td>{formatNsExact(VECTOR_INDEX.search.p95)}</td>
					</tr>
				</tbody>
			</table>
			<RunSource source={VECTOR_SOURCE} />

			<Note>
				This run used IVF. <code>vector_bench</code> uses the default ANN
				algorithm of <code>VectorIndex</code>, which is now <code>auto</code>:
				plain IVF below 50,000 vectors or 512 dimensions, IVF-PQ from there on.
				At 10,000 vectors the command below measures IVF, as this run did.{" "}
				<code>docs/BENCHMARKS.md</code> compares IVF and IVF-PQ recall and
				latency.
			</Note>

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
				p50 latency from the single-file raw benchmark on the same 10k-node,
				50k-edge graph.
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
				a smaller graph (1k nodes, 5k edges), so compare its rows with each
				other, not with the Rust and Python table.
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
