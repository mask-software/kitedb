import { For, Show } from "solid-js";
import DocPage from "~/components/doc-page";
import {
	BENCH_DATE,
	BENCH_MACHINE,
	GRAPH_SIZE,
	headlineParts,
	RUST_GRAPH,
} from "~/lib/benchmarks";
import {
	CELL,
	CELL_HIGHLIGHT,
	CELL_PLAIN,
	Code,
	Figure,
	FlowArrow,
	FlowItem,
	FlowStep,
	Panel,
	StepNumber,
} from "./-components";

// ============================================================================
// SHARED DIAGRAM PRIMITIVES
// ============================================================================

// ============================================================================
// KEY-INDEX DIAGRAMS
// ============================================================================

function KeyIndexProblem() {
	return (
		<div class="not-prose grid gap-3 sm:grid-cols-2">
			<Figure title="Without an index" accent="red" variant="problem">
				<p class="text-[14px] text-slate-400">
					Find the node with key <Code>user:alice</Code>
				</p>
				<p class="mt-2 text-[14px] text-slate-300">
					Scan every node and compare its key.
				</p>
				<div class="mt-4 flex items-baseline gap-2 border-t border-kite-line pt-3">
					<span class="font-mono text-[15px] font-semibold text-red-400">
						O(n)
					</span>
					<span class="text-[13px] text-slate-500">
						grows with the number of nodes
					</span>
				</div>
			</Figure>

			<Figure title="With the hash index" accent="cyan">
				<p class="text-[14px] text-slate-400">
					Find the node with key <Code>user:alice</Code>
				</p>
				<p class="mt-2 text-[14px] text-slate-300">
					Hash the key and read one bucket.
				</p>
				<div class="mt-4 flex items-baseline gap-2 border-t border-kite-line pt-3">
					<span class="font-mono text-[15px] font-semibold text-kite-cyan">
						O(1)
					</span>
					<span class="text-[13px] text-slate-500">average case</span>
				</div>
				<p class="mt-2 text-[13px] text-slate-500">
					Measured p50:{" "}
					<span class="font-semibold text-white">
						{headlineParts(RUST_GRAPH.keyLookup.p50).value}
					</span>{" "}
					{headlineParts(RUST_GRAPH.keyLookup.p50).unit}
				</p>
			</Figure>
		</div>
	);
}

const BUCKET_OFFSETS = [0, 1, 1, 3, 4];
/** Bucket 2 owns entries [offsets[2], offsets[3]) = [1, 3). */
const HIGHLIGHT_BUCKET = 2;

const ENTRIES = [{ bucket: 0 }, { bucket: 2 }, { bucket: 2 }, { bucket: 3 }];

const ENTRY_FIELDS = [
	{ name: "hash64", bytes: "8 B" },
	{ name: "string_id", bytes: "4 B" },
	{ name: "reserved", bytes: "4 B" },
	{ name: "node_id", bytes: "8 B" },
];

const ENTRY_GRID = "grid grid-cols-[2fr_1fr_1fr_2fr] gap-1";

function KeyIndexStructure() {
	const isOffsetInRange = (i: number) =>
		i === HIGHLIGHT_BUCKET || i === HIGHLIGHT_BUCKET + 1;

	return (
		<Figure title="Snapshot key index" accent="cyan">
			<div class="space-y-3">
				<Panel label="Bucket array" meta="KeyBuckets · u32 × (num_buckets + 1)">
					<div class="flex flex-wrap gap-1">
						<For each={BUCKET_OFFSETS}>
							{(offset, i) => (
								<div class="w-12">
									<div class="mb-1 text-center font-mono text-[11px] text-slate-500">
										{i()}
									</div>
									<div
										class={`${CELL} ${isOffsetInRange(i()) ? CELL_HIGHLIGHT : CELL_PLAIN}`}
									>
										{offset}
									</div>
								</div>
							)}
						</For>
						<div class="w-12">
							<div class="mb-1 text-center font-mono text-[11px] text-slate-500">
								&nbsp;
							</div>
							<div class={`${CELL} border-transparent text-slate-500`}>…</div>
						</div>
					</div>
					<p class="mt-3 text-[13px] text-slate-400">
						Bucket <span class="font-mono text-slate-200">b</span> owns entries{" "}
						<span class="font-mono text-slate-200">key_buckets[b]</span> up to{" "}
						<span class="font-mono text-slate-200">key_buckets[b + 1]</span>.
						Here bucket {HIGHLIGHT_BUCKET} holds entries 1 and 2; bucket 1 is
						empty.
					</p>
				</Panel>

				<Panel
					label="Entry array"
					meta="KeyEntries · 24 B each, sorted by bucket, then hash"
				>
					<div class="overflow-x-auto">
						<div class="min-w-[26rem] space-y-1">
							<div class="flex items-center gap-2">
								<span class="w-4 shrink-0" />
								<div class={`${ENTRY_GRID} flex-1`}>
									<For each={ENTRY_FIELDS}>
										{(field) => (
											<span class="text-center font-mono text-[11px] text-slate-500">
												{field.bytes}
											</span>
										)}
									</For>
								</div>
								<span class="w-16 shrink-0" />
							</div>
							<For each={ENTRIES}>
								{(entry, i) => {
									const active = entry.bucket === HIGHLIGHT_BUCKET;
									return (
										<div class="flex items-center gap-2">
											<span class="w-4 shrink-0 text-right font-mono text-[11px] text-slate-500">
												{i()}
											</span>
											<div class={`${ENTRY_GRID} flex-1`}>
												<For each={ENTRY_FIELDS}>
													{(field) => (
														<span
															class={`${CELL} ${
																field.name === "reserved"
																	? "border-kite-line bg-transparent text-slate-600"
																	: active
																		? CELL_HIGHLIGHT
																		: CELL_PLAIN
															}`}
														>
															{field.name}
														</span>
													)}
												</For>
											</div>
											<span
												class={`w-16 shrink-0 font-mono text-[11px] ${active ? "text-kite-cyan" : "text-slate-500"}`}
											>
												bucket {entry.bucket}
											</span>
										</div>
									);
								}}
							</For>
						</div>
					</div>
				</Panel>
			</div>

			<dl class="mt-4 grid gap-x-4 gap-y-1.5 border-t border-kite-line pt-4 text-[13px] sm:grid-cols-[auto_1fr]">
				<dt class="font-mono text-slate-200">hash64</dt>
				<dd class="text-slate-400">xxHash64 of the key string</dd>
				<dt class="font-mono text-slate-200">string_id</dt>
				<dd class="text-slate-400">
					Index of the key in the snapshot string table, used to confirm a match
				</dd>
				<dt class="font-mono text-slate-200">reserved</dt>
				<dd class="text-slate-400">4 bytes, written as zero</dd>
				<dt class="font-mono text-slate-200">node_id</dt>
				<dd class="text-slate-400">The node this key maps to</dd>
			</dl>
		</Figure>
	);
}

function MonoLines(props: { lines: string[] }) {
	return (
		<div class="mt-1 rounded-lg border border-kite-line bg-[#070a12] px-4 py-3 overflow-x-auto whitespace-pre font-mono text-[13px] leading-6 text-slate-300">
			<For each={props.lines}>{(line) => <div>{line}</div>}</For>
		</div>
	);
}

function KeyLookupProcess() {
	return (
		<div class="not-prose">
			<FlowStep number="1" title="Check the delta" color="cyan">
				<FlowItem color="cyan">
					If the key is in <Code>delta.key_index_deleted</Code>, return null.
				</FlowItem>
				<FlowItem color="cyan">
					If the key is in <Code>delta.key_index</Code>, return its node ID.
				</FlowItem>
			</FlowStep>
			<FlowArrow />
			<FlowStep number="2" title="Find the bucket in the snapshot" color="cyan">
				<MonoLines
					lines={[
						"hash   = xxhash64(key)",
						"bucket = hash % num_buckets",
						"start  = key_buckets[bucket]",
						"end    = key_buckets[bucket + 1]",
					]}
				/>
			</FlowStep>
			<FlowArrow />
			<FlowStep number="3" title="Scan the bucket" color="emerald">
				<FlowItem color="emerald">
					Skip entries where <Code>entry.hash64 != hash</Code>.
				</FlowItem>
				<FlowItem color="emerald">
					On a hash match, compare <Code>string_table[entry.string_id]</Code>{" "}
					with the key.
				</FlowItem>
				<FlowItem color="emerald">
					If the strings are equal, return <Code>entry.node_id</Code>. If no
					entry matches, return null.
				</FlowItem>
			</FlowStep>
		</div>
	);
}

const LOOKUP_ORDER = [
	{ source: "delta.key_index_deleted", result: "found: return null" },
	{ source: "delta.key_index", result: "found: return the node ID" },
	{ source: "snapshot index", result: "hash, then scan one bucket" },
];

function TwoLevelLookup() {
	return (
		<Figure title="Two-level lookup">
			<div class="grid gap-3 sm:grid-cols-2">
				<Panel label="Delta" meta="in memory">
					<dl class="space-y-1.5 text-[13px]">
						<div class="flex flex-wrap items-baseline gap-x-2">
							<dt class="font-mono text-kite-mint">key_index</dt>
							<dd class="font-mono text-slate-500">
								HashMap&lt;String, NodeId&gt;
							</dd>
						</div>
						<div class="flex flex-wrap items-baseline gap-x-2">
							<dt class="font-mono text-red-400">key_index_deleted</dt>
							<dd class="font-mono text-slate-500">HashSet&lt;String&gt;</dd>
						</div>
					</dl>
				</Panel>
				<Panel label="Snapshot" meta="memory-mapped file">
					<dl class="space-y-1.5 text-[13px]">
						<div class="flex flex-wrap items-baseline gap-x-2">
							<dt class="font-mono text-kite-cyan">KeyBuckets</dt>
							<dd class="font-mono text-slate-500">u32[]</dd>
						</div>
						<div class="flex flex-wrap items-baseline gap-x-2">
							<dt class="font-mono text-kite-cyan">KeyEntries</dt>
							<dd class="font-mono text-slate-500">
								{"{hash64, string_id, node_id}[]"}
							</dd>
						</div>
					</dl>
				</Panel>
			</div>

			<div class="mt-4 border-t border-kite-line pt-4">
				<p class="mb-2.5 font-mono text-[11px] uppercase tracking-[0.08em] text-slate-500">
					Lookup order
				</p>
				<ol class="space-y-2">
					<For each={LOOKUP_ORDER}>
						{(step, i) => (
							<li class="flex flex-wrap items-center gap-x-3 gap-y-1 text-[13px]">
								<StepNumber>{i() + 1}</StepNumber>
								<span class="min-w-[13.5rem] font-mono text-slate-200">
									{step.source}
								</span>
								<span class="text-slate-400">{step.result}</span>
							</li>
						)}
					</For>
				</ol>
				<p class="mt-3 text-[13px] text-slate-500">
					Deletions are checked first, so a key deleted since the last
					checkpoint is not found in the snapshot. Inside a transaction, its own
					uncommitted key changes are checked before the delta.
				</p>
			</div>
		</Figure>
	);
}

const HASH_REQUIREMENTS = [
	{
		name: "Fast",
		detail:
			"Computed once per lookup, and once per key when a checkpoint builds the index.",
	},
	{
		name: "Even distribution",
		detail:
			"Spreads keys across buckets so most buckets hold zero or one entry.",
	},
	{
		name: "Deterministic",
		detail:
			"Hashes are stored in the snapshot, so a key must hash to the same value in every process. KiteDB uses seed 0.",
	},
];

function XxHash64Explanation() {
	return (
		<Figure title="What the index needs from a hash" accent="mint">
			<dl class="space-y-3">
				<For each={HASH_REQUIREMENTS}>
					{(req) => (
						<div class="grid gap-1 text-[14px] sm:grid-cols-[10rem_1fr] sm:gap-4">
							<dt class="font-semibold text-slate-100">{req.name}</dt>
							<dd class="text-slate-400">{req.detail}</dd>
						</div>
					)}
				</For>
			</dl>

			<div class="mt-4 grid gap-3 border-t border-kite-line pt-4 sm:grid-cols-2">
				<div class="rounded-lg border border-kite-mint/25 bg-kite-mint/[0.05] p-4">
					<p class="font-mono text-[13px] text-kite-mint">xxHash64</p>
					<p class="mt-1.5 text-[13px] text-slate-400">
						Non-cryptographic, 64-bit output, designed for throughput.
					</p>
				</div>
				<div class="rounded-lg border border-kite-line bg-white/[0.02] p-4">
					<p class="font-mono text-[13px] text-slate-300">SHA-256</p>
					<p class="mt-1.5 text-[13px] text-slate-400">
						Cryptographic. The extra work per byte buys resistance to deliberate
						collisions, which a local key index does not need.
					</p>
				</div>
			</div>
		</Figure>
	);
}

/** Step text with an optional inline identifier: `text` + `code` + `after`. */
const COLLISION_STEPS: { text: string; code?: string; after?: string }[] = [
	{ text: "Both entries sit in the same bucket." },
	{ text: "The lookup hash matches both entries." },
	{
		text: "For each match, the stored key is read from the string table through ",
		code: "string_id",
		after: ".",
	},
	{ text: "The entry whose stored key equals the lookup key is returned." },
];

function CollisionHandling() {
	return (
		<Figure title="When two keys share a hash" accent="amber">
			<div class="rounded-lg border border-kite-line bg-white/[0.02] p-4">
				<p class="mb-2 text-[13px] text-slate-500">
					Two keys with the same 64-bit hash (illustrative values)
				</p>
				<div class="space-y-1.5 font-mono text-[13px]">
					<div class="flex flex-wrap items-center gap-x-3">
						<span class="min-w-[8rem] text-slate-200">"user:alice"</span>
						<span class="text-slate-500">hash</span>
						<span class="text-slate-300">0x1234…</span>
					</div>
					<div class="flex flex-wrap items-center gap-x-3">
						<span class="min-w-[8rem] text-slate-200">"user:alfred"</span>
						<span class="text-slate-500">hash</span>
						<span class="text-amber-300">0x1234…</span>
						<span class="rounded-md border border-amber-400/25 bg-amber-400/10 px-1.5 font-sans text-[11px] text-amber-300">
							same hash
						</span>
					</div>
				</div>
			</div>

			<ol class="mt-4 space-y-2">
				<For each={COLLISION_STEPS}>
					{(step, i) => {
						const last = i() === COLLISION_STEPS.length - 1;
						return (
							<li class="flex items-start gap-3 text-[14px]">
								<StepNumber accent={last ? "mint" : undefined}>
									{i() + 1}
								</StepNumber>
								<span class={last ? "text-slate-100" : "text-slate-300"}>
									{step.text}
									<Show when={step.code}>
										{(code) => <Code>{code()}</Code>}
									</Show>
									{step.after}
								</span>
							</li>
						);
					}}
				</For>
			</ol>

			<p class="mt-4 border-t border-kite-line pt-3 text-[13px] text-slate-400">
				Keys that only share a bucket are cheaper: the{" "}
				<span class="font-mono text-slate-200">hash64</span> comparison skips
				them without reading a string. String comparisons happen once per entry
				with the same 64-bit hash, so the cost is{" "}
				<span class="font-mono text-amber-300">O(k)</span> for{" "}
				<span class="font-mono text-slate-200">k</span> such entries, and{" "}
				<span class="font-mono text-slate-200">k</span> is almost always 1.
			</p>
		</Figure>
	);
}

/** 8 keys spread across 16 buckets. */
const BUCKET_FILL = [1, 0, 0, 2, 1, 0, 1, 0, 0, 1, 0, 2, 0, 0, 0, 0];

const INDEX_SIZE_ROWS = [
	{ part: "Bucket array", math: "2M × 4 B", size: "8 MB" },
	{ part: "Entry array", math: "1M × 24 B", size: "24 MB" },
];

function LoadFactorDiagram() {
	return (
		<Figure title="Load factor" accent="cyan">
			<p class="text-[14px] text-slate-300">
				<span class="font-mono text-slate-100">
					load factor = entries / buckets
				</span>
			</p>
			<p class="mt-2 text-[14px] text-slate-400">
				A checkpoint sizes the bucket array at{" "}
				<span class="font-mono text-slate-200">max(16, 2 × entries)</span>, so
				the load factor is at most 50%.
			</p>

			<div class="mt-4 rounded-lg border border-kite-line bg-white/[0.02] p-4">
				<div class="grid grid-cols-8 gap-1 sm:grid-cols-16">
					<For each={BUCKET_FILL}>
						{(count) => (
							<span
								class={`${CELL} px-0 ${count > 0 ? CELL_HIGHLIGHT : "border-kite-line bg-white/[0.02] text-slate-600"}`}
							>
								{count}
							</span>
						)}
					</For>
				</div>
				<p class="mt-3 text-[13px] text-slate-500">
					8 keys in 16 buckets: most buckets hold zero or one entry, which keeps
					collisions rare and bucket scans short.
				</p>
			</div>

			<div class="mt-4 rounded-lg border border-kite-line bg-white/[0.02] p-4">
				<p class="mb-3 text-[13px] text-slate-500">Index size for 1M keys</p>
				<table class="w-full text-[13px]">
					<tbody>
						<For each={INDEX_SIZE_ROWS}>
							{(row) => (
								<tr>
									<td class="py-1 text-slate-400">{row.part}</td>
									<td class="py-1 font-mono text-slate-500">{row.math}</td>
									<td class="py-1 text-right font-mono text-slate-200">
										{row.size}
									</td>
								</tr>
							)}
						</For>
						<tr class="border-t border-kite-line">
							<td class="pt-2 font-semibold text-slate-100" colSpan={2}>
								Total
							</td>
							<td class="pt-2 text-right font-mono font-semibold text-white">
								~32 MB
							</td>
						</tr>
					</tbody>
				</table>
				<p class="mt-3 text-[13px] text-slate-500">
					The key strings themselves live in the snapshot string table. A hit
					reads two adjacent bucket offsets, one or two entries, and one string.
				</p>
			</div>
		</Figure>
	);
}

// ============================================================================
// PAGE COMPONENT
// ============================================================================

export function KeyIndexPage() {
	return (
		<DocPage slug="internals/key-index">
			<p>
				Every node in KiteDB can have a string key. The key index maps keys to
				node IDs with O(1) average-case lookups.
			</p>

			<h2 id="the-problem">The problem</h2>

			<p>
				Applications usually find nodes by an identifier they already have, such
				as a username or an external ID. Without an index, that lookup is a
				scan.
			</p>

			<KeyIndexProblem />

			<p class="text-[14px] text-slate-500">
				The measured figure is the p50 for random existing keys on the Rust
				core, {GRAPH_SIZE}, read from the snapshot index after a checkpoint (
				{BENCH_DATE}, {BENCH_MACHINE.cpu}, MVCC on). See the{" "}
				<a href="/docs/benchmarks">benchmarks</a> for the full results.
			</p>

			<h2 id="structure">Index structure</h2>

			<p>
				The snapshot stores the key index as two arrays: a bucket array of
				offsets and an entry array sorted by bucket. Each bucket is a contiguous
				run of entries, so there is no probing.
			</p>

			<KeyIndexStructure />

			<h2 id="lookup">Lookup process</h2>

			<KeyLookupProcess />

			<h2 id="two-level">Two-level lookup</h2>

			<p>
				The key index is split between the delta in memory and the snapshot on
				disk:
			</p>

			<TwoLevelLookup />

			<h2 id="hashing">Why xxHash64</h2>

			<XxHash64Explanation />

			<h2 id="collisions">Handling collisions</h2>

			<CollisionHandling />

			<h2 id="load-factor">Load factor</h2>

			<LoadFactorDiagram />

			<h2 id="next">Next steps</h2>
			<ul>
				<li>
					<a href="/docs/internals/snapshot-delta">Snapshot and delta</a>: how
					the two-level lookup fits into the storage model
				</li>
				<li>
					<a href="/docs/internals/performance">Performance</a>: measured
					latencies and tuning
				</li>
			</ul>
		</DocPage>
	);
}
