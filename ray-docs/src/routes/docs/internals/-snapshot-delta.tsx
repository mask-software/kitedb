import { For, type JSX } from "solid-js";
import DocPage from "~/components/doc-page";
import { CheckpointStep, Code } from "./-components";

// ============================================================================
// SNAPSHOT-DELTA SPECIFIC COMPONENTS
// ============================================================================

function StatePart(props: {
	name: string;
	where: string;
	dot: string;
	points: string[];
}) {
	return (
		<div class="flex-1 rounded-lg border border-kite-line bg-white/[0.02] p-4">
			<div class="flex items-center gap-2.5">
				<span
					class={`h-1.5 w-1.5 shrink-0 rounded-full ${props.dot}`}
					aria-hidden="true"
				/>
				<p class="text-[15px] font-semibold text-white">{props.name}</p>
				<span class="ml-auto font-mono text-[11px] text-slate-500">
					{props.where}
				</span>
			</div>
			<ul class="mt-3 space-y-1 text-[14px] text-slate-300">
				<For each={props.points}>{(point) => <li>{point}</li>}</For>
			</ul>
		</div>
	);
}

// Snapshot + delta model diagram
function SnapshotDeltaModel() {
	return (
		<figure class="rounded-xl border border-kite-line bg-kite-surface/60 p-5">
			<p class="font-mono text-[11px] uppercase tracking-[0.08em] text-slate-500">
				Database state
			</p>

			<div class="mt-4 flex flex-col items-stretch gap-2 sm:flex-row sm:gap-3">
				<StatePart
					name="Snapshot"
					where="on disk"
					dot="bg-kite-cyan"
					points={["Immutable", "CSR format", "Memory-mapped"]}
				/>
				<div
					class="self-center font-mono text-[18px] text-slate-500"
					aria-hidden="true"
				>
					+
				</div>
				<StatePart
					name="Delta"
					where="in memory"
					dot="bg-kite-violet"
					points={[
						"Changes since the last checkpoint",
						"Fast writes",
						"Merged on read",
					]}
				/>
			</div>

			<div class="mt-3 flex flex-col gap-1 rounded-lg border border-kite-line bg-white/[0.02] px-4 py-3 sm:flex-row sm:items-center sm:gap-3">
				<div class="flex items-center gap-2.5">
					<span
						class="h-1.5 w-1.5 shrink-0 rounded-full bg-kite-mint"
						aria-hidden="true"
					/>
					<p class="text-[15px] font-semibold text-white">WAL</p>
					<span class="font-mono text-[11px] text-slate-500 sm:hidden">
						on disk
					</span>
				</div>
				<p class="text-[14px] text-slate-400">
					Write-ahead log behind the delta. Every commit is appended here first,
					and replayed into the delta after a crash.
				</p>
				<span class="ml-auto hidden shrink-0 font-mono text-[11px] text-slate-500 sm:block">
					on disk
				</span>
			</div>
		</figure>
	);
}

interface DeltaField {
	name: string;
	type: string;
	desc: string;
	dot: string;
}

const DELTA_FIELDS: DeltaField[] = [
	{
		name: "created_nodes",
		type: "HashMap<NodeId, NodeDelta>",
		desc: "New and recreated nodes",
		dot: "bg-kite-mint",
	},
	{
		name: "deleted_nodes",
		type: "HashSet<NodeId>",
		desc: "Tombstones; they keep hiding a recreated node's old snapshot copy",
		dot: "bg-red-400",
	},
	{
		name: "modified_nodes",
		type: "HashMap<NodeId, NodeDelta>",
		desc: "Label and property changes",
		dot: "bg-amber-400",
	},
	{
		name: "out_add / out_del",
		type: "HashMap<NodeId, BTreeSet<EdgePatch>>",
		desc: "Outgoing edge changes",
		dot: "bg-kite-violet",
	},
	{
		name: "in_add / in_del",
		type: "HashMap<NodeId, BTreeSet<EdgePatch>>",
		desc: "Incoming edge changes",
		dot: "bg-kite-violet",
	},
	{
		name: "key_index",
		type: "HashMap<String, NodeId>",
		desc: "Key lookups",
		dot: "bg-kite-cyan",
	},
];

// Delta state structure
function DeltaStateStructure() {
	return (
		<figure class="rounded-xl border border-kite-line bg-kite-surface/60 p-5">
			<div class="flex items-baseline gap-3">
				<p class="text-[15px] font-semibold text-white">Delta state</p>
				<code class="font-mono text-[12px] text-slate-500">
					struct DeltaState
				</code>
			</div>
			<p class="mt-1 text-[13px] text-slate-500">
				Main fields. Edge changes are kept in both directions.
			</p>
			<dl class="mt-4 divide-y divide-kite-line border-y border-kite-line">
				<For each={DELTA_FIELDS}>
					{(field) => (
						<div class="grid grid-cols-[auto_1fr] items-baseline gap-x-3 gap-y-0.5 py-2.5 sm:grid-cols-[auto_11rem_1fr]">
							<span
								class={`h-1.5 w-1.5 -translate-y-px rounded-full ${field.dot}`}
								aria-hidden="true"
							/>
							<dt class="font-mono text-[13px] text-slate-100">{field.name}</dt>
							<dd class="col-start-2 text-[13px] text-slate-400 sm:col-start-3 sm:row-start-1">
								{field.desc}
								<span class="mt-0.5 block font-mono text-[11px] text-slate-500">
									{field.type}
								</span>
							</dd>
						</div>
					)}
				</For>
			</dl>
		</figure>
	);
}

interface ReadStep {
	question: () => JSX.Element;
	answer: string;
	outcome: string;
	final?: boolean;
}

const READ_STEPS: ReadStep[] = [
	{
		question: () => (
			<>
				Is <Code>node_id</Code> in <Code>delta.created_nodes</Code>?
			</>
		),
		answer: "Yes",
		outcome: "return the node from the delta (new or recreated node)",
	},
	{
		question: () => (
			<>
				Is <Code>node_id</Code> in <Code>delta.deleted_nodes</Code>?
			</>
		),
		answer: "Yes",
		outcome: "not found (deleted)",
	},
	{
		question: () => <>Does the snapshot have this node?</>,
		answer: "No",
		outcome: "not found (never existed)",
	},
	{
		question: () => (
			<>
				Merge the snapshot with <Code>delta.modified_nodes</Code>
			</>
		),
		answer: "Then",
		outcome: "return the combined result",
		final: true,
	},
];

// Read decision flow
function ReadFlowDiagram() {
	return (
		<figure class="rounded-xl border border-kite-line bg-kite-surface/60 p-5">
			<p class="text-[15px] font-semibold text-white">Reading a node</p>
			<ol class="mt-4 space-y-2">
				<For each={READ_STEPS}>
					{(step, i) => (
						<li
							class="flex items-start gap-3 rounded-lg border p-3.5"
							classList={{
								"border-kite-line bg-white/[0.02]": !step.final,
								"border-kite-mint/25 bg-kite-mint/[0.05]": step.final,
							}}
						>
							<span
								class="grid h-6 w-6 shrink-0 place-items-center rounded-full border font-mono text-[11px]"
								classList={{
									"border-kite-cyan/25 bg-kite-cyan/10 text-kite-cyan":
										!step.final,
									"border-kite-mint/25 bg-kite-mint/10 text-kite-mint":
										step.final,
								}}
							>
								{i() + 1}
							</span>
							<div class="min-w-0 pt-0.5">
								<p class="text-[14px] leading-relaxed text-slate-200">
									{step.question()}
								</p>
								<p class="mt-1 text-[13px] text-slate-400">
									<span class="font-medium text-slate-200">{step.answer}:</span>{" "}
									{step.outcome}
								</p>
							</div>
						</li>
					)}
				</For>
			</ol>
		</figure>
	);
}

const WRITE_STEPS = [
	{
		name: "WAL",
		action: "Append records (durability)",
		tint: "border-kite-mint/25 bg-kite-mint/10 text-kite-mint",
	},
	{
		name: "Delta",
		action: "Update in-memory state (visible to reads)",
		tint: "border-kite-violet/25 bg-kite-violet/10 text-kite-violet",
	},
];

// Write flow
function WriteFlowDiagram() {
	return (
		<figure class="rounded-xl border border-kite-line bg-kite-surface/60 p-5">
			<p class="text-[15px] font-semibold text-white">Transaction commit</p>
			<ol class="mt-4 space-y-3">
				<For each={WRITE_STEPS}>
					{(step, i) => (
						<li class="flex items-center gap-3">
							<span
								class={`grid h-6 w-6 shrink-0 place-items-center rounded-full border font-mono text-[11px] ${step.tint}`}
							>
								{i() + 1}
							</span>
							<span class="w-14 shrink-0 text-[14px] font-semibold text-slate-100">
								{step.name}
							</span>
							<span class="text-[14px] text-slate-400">{step.action}</span>
						</li>
					)}
				</For>
			</ol>
			<p class="mt-4 rounded-lg border border-kite-cyan/20 bg-kite-cyan/[0.05] px-4 py-3 text-[14px] text-slate-300">
				Normal writes never modify the snapshot.
			</p>
		</figure>
	);
}

// Checkpoint process
function CheckpointProcess() {
	return (
		<figure class="rounded-xl border border-kite-line bg-kite-surface/60 p-5">
			<p class="text-[15px] font-semibold text-white">Checkpoint process</p>

			<div class="relative mt-4">
				<div
					class="absolute bottom-3 left-3 top-3 w-px bg-kite-line"
					aria-hidden="true"
				/>
				<div class="space-y-3">
					<CheckpointStep num={1} text="Read the current snapshot" />
					<CheckpointStep num={2} text="Apply all delta changes" />
					<CheckpointStep
						num={3}
						text="Write a new snapshot (CSR, compressed)"
					/>
					<CheckpointStep
						num={4}
						text="Update the header to point to the new snapshot"
					/>
					<CheckpointStep num={5} text="Clear the delta and the WAL" />
				</div>
			</div>

			<div class="mt-5 flex flex-wrap gap-x-6 gap-y-2 border-t border-kite-line pt-4 text-[13px] text-slate-400">
				<p class="flex items-center gap-2">
					<span
						class="h-1.5 w-1.5 rounded-full bg-kite-cyan"
						aria-hidden="true"
					/>
					Automatic: when the WAL fills past a threshold
				</p>
				<p class="flex items-center gap-2">
					<span
						class="h-1.5 w-1.5 rounded-full bg-kite-violet"
						aria-hidden="true"
					/>
					Manual: <Code>db.checkpoint()</Code>
				</p>
			</div>
		</figure>
	);
}

// ============================================================================
// PAGE COMPONENT
// ============================================================================

export function SnapshotDeltaPage() {
	return (
		<DocPage slug="internals/snapshot-delta">
			<p>
				KiteDB separates storage into two parts: a <strong>snapshot</strong>{" "}
				(immutable, on disk) and a <strong>delta</strong> (mutable, in memory).
				Reads merge the two, writes only touch the delta and the write-ahead
				log, and a periodic checkpoint folds the delta into a new snapshot.
			</p>

			<h2 id="the-model">The model</h2>

			<SnapshotDeltaModel />

			<h2 id="snapshot">Snapshot</h2>

			<p>
				The snapshot is a point-in-time image of the entire database. It's
				stored in <a href="/docs/internals/csr">CSR format</a> and memory-mapped
				directly from disk.
			</p>

			<p>
				<strong>Key properties:</strong>
			</p>
			<ul>
				<li>
					<strong>Immutable</strong> – once written, never modified. Safe for
					concurrent reads.
				</li>
				<li>
					<strong>Memory-mapped</strong> via <code>mmap()</code>, so the OS page
					cache handles caching. Uncompressed sections are read in place;
					compressed ones are decompressed once and cached.
				</li>
				<li>
					<strong>Compressed</strong> – zstd compression reduces disk usage by
					~60%.
				</li>
				<li>
					<strong>Complete</strong> – contains all nodes, edges, properties, and
					indexes.
				</li>
			</ul>

			<h2 id="delta">Delta</h2>

			<p>
				The delta holds all changes since the last snapshot. It's a collection
				of in-memory data structures optimized for both reads and writes.
			</p>

			<DeltaStateStructure />

			<h2 id="reading">How reads work</h2>

			<p>Every read operation merges snapshot and delta:</p>

			<ReadFlowDiagram />

			<p>
				Edge traversals work the same way: scan the snapshot's edges, skip the
				ones deleted in the delta or touching a deleted (or recreated) node, and
				add new ones from the delta.
			</p>

			<h2 id="writing">How writes work</h2>

			<p>Writes go to three places:</p>

			<WriteFlowDiagram />

			<h2 id="checkpoint">Checkpoint: merging the delta into the snapshot</h2>

			<p>
				Periodically, KiteDB creates a new snapshot that incorporates all delta
				changes. This is called a <strong>checkpoint</strong>.
			</p>

			<CheckpointProcess />

			<p>
				During a checkpoint, reads continue against the old snapshot and delta.
				The switch to the new snapshot is atomic.
			</p>

			<h2 id="why-it-works">Why this works well</h2>

			<table>
				<thead>
					<tr>
						<th>Property</th>
						<th>How it's achieved</th>
					</tr>
				</thead>
				<tbody>
					<tr>
						<td>Fast reads</td>
						<td>Snapshot is mmap'd. OS caches hot pages. Delta is small.</td>
					</tr>
					<tr>
						<td>Fast writes</td>
						<td>WAL append + memory update. No disk seeks.</td>
					</tr>
					<tr>
						<td>Crash safety</td>
						<td>WAL survives crashes. Replay rebuilds delta.</td>
					</tr>
					<tr>
						<td>Concurrent reads</td>
						<td>Snapshot is immutable. MVCC handles delta visibility.</td>
					</tr>
				</tbody>
			</table>

			<h2 id="next">Next steps</h2>
			<ul>
				<li>
					<a href="/docs/internals/csr">CSR format</a> – how the snapshot stores
					edges
				</li>
				<li>
					<a href="/docs/internals/wal">WAL and durability</a> – how the
					write-ahead log works
				</li>
				<li>
					<a href="/docs/internals/mvcc">MVCC and transactions</a> – how
					concurrent access is handled
				</li>
			</ul>
		</DocPage>
	);
}
