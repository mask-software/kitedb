import { For } from "solid-js";
import CodeBlock from "~/components/code-block";
import DocPage from "~/components/doc-page";
import { Code, FlowArrow, FlowItem, FlowStep, Label } from "./-components";

// ============================================================================
// ARCHITECTURE-SPECIFIC COMPONENTS
// ============================================================================

interface Layer {
	name: string;
	dot: string;
	summary: string;
	parts: string[];
	code: boolean;
}

const LAYERS: Layer[] = [
	{
		name: "Query layer",
		dot: "bg-kite-cyan",
		summary: "Fluent API, type inference, schema validation",
		parts: ["db.insert(user).values({...})"],
		code: true,
	},
	{
		name: "Graph layer",
		dot: "bg-kite-violet",
		summary: "Nodes, edges, traversal, transactions",
		parts: ["create_node()", "add_edge()", "out_edges()"],
		code: true,
	},
	{
		name: "Storage layer",
		dot: "bg-kite-mint",
		summary: "Memory-mapped file, crash recovery",
		parts: ["Snapshot (CSR)", "Delta", "WAL", "Key index"],
		code: false,
	},
];

// Three stacked layers, top (API) to bottom (file)
function ArchitectureDiagram() {
	return (
		<figure aria-label="KiteDB layers, from the query API down to storage">
			<For each={LAYERS}>
				{(layer, i) => (
					<>
						{i() > 0 && <FlowArrow />}
						<div class="rounded-xl border border-kite-line bg-kite-surface/60 p-5">
							<div class="flex items-center gap-2.5">
								<span
									class={`h-1.5 w-1.5 shrink-0 rounded-full ${layer.dot}`}
									aria-hidden="true"
								/>
								<p class="text-[15px] font-semibold text-white">{layer.name}</p>
							</div>
							<p class="mt-1.5 text-[14px] leading-relaxed text-slate-400">
								{layer.summary}
							</p>
							<div class="mt-3 flex flex-wrap gap-2">
								<For each={layer.parts}>
									{(part) =>
										layer.code ? (
											<code class="rounded-md border border-kite-line bg-white/[0.03] px-2 py-1 font-mono text-[12px] text-slate-200">
												{part}
											</code>
										) : (
											<span class="rounded-md border border-kite-line bg-white/[0.03] px-2 py-1 text-[13px] text-slate-200">
												{part}
											</span>
										)
									}
								</For>
							</div>
						</div>
					</>
				)}
			</For>
		</figure>
	);
}

// Insert data flow diagram
function InsertDataFlow() {
	return (
		<figure class="space-y-1">
			<FlowStep number="1" title="Query layer" color="cyan">
				<FlowItem color="cyan">
					Validates that <Code>user</Code> has its required properties
				</FlowItem>
				<FlowItem color="cyan">
					Converts <Code>age: 30</Code> to the internal <Code>I64</Code> type
				</FlowItem>
				<FlowItem color="cyan">
					Calls the graph layer: <Code>create_node(...)</Code>
				</FlowItem>
			</FlowStep>

			<FlowArrow />

			<FlowStep number="2" title="Graph layer" color="violet">
				<FlowItem color="violet">
					Begins a transaction (if one isn't already open)
				</FlowItem>
				<FlowItem color="violet">
					Allocates a new <Code>NodeId</Code> from a monotonic counter
				</FlowItem>
				<FlowItem color="violet">
					Records the node in transaction state
				</FlowItem>
				<FlowItem color="violet">
					On commit, writes to the WAL and the delta
				</FlowItem>
			</FlowStep>

			<FlowArrow />

			<FlowStep number="3" title="Storage layer" color="emerald">
				<FlowItem color="emerald">
					<Label color="emerald">WAL</Label>: appends a <Code>CREATE_NODE</Code>{" "}
					record for durability
				</FlowItem>
				<FlowItem color="emerald">
					<Label color="emerald">Delta</Label>: adds the node to the{" "}
					<Code>created_nodes</Code> map
				</FlowItem>
				<FlowItem color="emerald">
					<Label color="emerald">Later</Label>: a checkpoint merges it into the
					snapshot
				</FlowItem>
			</FlowStep>
		</figure>
	);
}

// Read data flow diagram
function ReadDataFlow() {
	return (
		<figure class="space-y-1">
			<FlowStep number="1" title="Key index lookup" color="cyan">
				<FlowItem color="cyan">
					Check <Code>delta.key_index</Code> for recent changes
				</FlowItem>
				<FlowItem color="cyan">
					If not found, check the snapshot's hash-bucketed index
				</FlowItem>
				<FlowItem color="cyan">
					Returns the <Code>NodeId</Code>
				</FlowItem>
			</FlowStep>

			<FlowArrow />

			<FlowStep number="2" title="Property fetch" color="violet">
				<FlowItem color="violet">
					Check <Code>delta.modified_nodes</Code> for changes
				</FlowItem>
				<FlowItem color="violet">
					Fall back to the snapshot for unchanged properties
				</FlowItem>
				<FlowItem color="violet">Merge both and return the result</FlowItem>
			</FlowStep>

			<div class="flex justify-center pt-3">
				<p class="inline-flex items-center gap-2 rounded-lg border border-kite-mint/25 bg-kite-mint/10 px-3 py-1.5 text-[13px] text-kite-mint">
					<span
						class="h-1.5 w-1.5 rounded-full bg-kite-mint"
						aria-hidden="true"
					/>
					Returns the latest committed data
				</p>
			</div>
		</figure>
	);
}

// ============================================================================
// PAGE COMPONENT
// ============================================================================

export function ArchitecturePage() {
	return (
		<DocPage slug="internals/architecture">
			<p>
				KiteDB is built in three layers. The query layer is the API you call,
				the graph layer manages nodes, edges, and transactions, and the storage
				layer keeps everything in a single file on disk.
			</p>

			<h2 id="the-layers">The three layers</h2>

			<ArchitectureDiagram />

			<h3>Query layer</h3>
			<p>
				This is what you interact with. It provides the Drizzle-style API with
				full TypeScript type inference. When you write{" "}
				<code>db.insert(user).values(...)</code>, the query layer validates your
				schema, converts TypeScript types to storage types, and calls into the
				graph layer.
			</p>

			<h3>Graph layer</h3>
			<p>
				Manages the graph abstraction: nodes with properties, edges between
				nodes, and traversals. Handles transaction boundaries and coordinates
				reads between the snapshot and delta.
			</p>

			<h3>Storage layer</h3>
			<p>
				Stores data in a format built for graph operations. It is organized
				around the <strong>snapshot + delta</strong> model, which separates an
				immutable snapshot on disk from the changes made since it was written.
			</p>

			<h2 id="data-flow">What happens when you insert a node</h2>

			<p>Here is the path a single insert takes:</p>

			<CodeBlock
				code={`const alice = db
  .insert(user)
  .values({ key: 'alice', name: 'Alice', age: 30 })
  .returning();`}
				language="typescript"
			/>

			<InsertDataFlow />

			<h2 id="read-path">What happens when you read</h2>

			<p>Reads merge data from two sources:</p>

			<CodeBlock
				code={`const alice = db.get(user, 'alice');`}
				language="typescript"
			/>

			<ReadDataFlow />

			<h2 id="why-this-design">Why this design</h2>

			<table>
				<thead>
					<tr>
						<th>Design choice</th>
						<th>Benefit</th>
					</tr>
				</thead>
				<tbody>
					<tr>
						<td>Snapshot + delta</td>
						<td>
							Reads don't block writes. The snapshot is immutable and the delta
							is small.
						</td>
					</tr>
					<tr>
						<td>CSR format for edges</td>
						<td>
							Traversals read contiguous memory, which makes good use of CPU
							caches.
						</td>
					</tr>
					<tr>
						<td>WAL for durability</td>
						<td>
							Committed data survives crashes. Recovery only replays the WAL
							written since the last checkpoint.
						</td>
					</tr>
					<tr>
						<td>Single file</td>
						<td>Portable, atomic operations, simpler deployment.</td>
					</tr>
					<tr>
						<td>Memory-mapped I/O</td>
						<td>
							The OS page cache keeps hot pages in memory. Uncompressed sections
							are read in place.
						</td>
					</tr>
				</tbody>
			</table>

			<h2 id="next">Next steps</h2>
			<ul>
				<li>
					<a href="/docs/internals/snapshot-delta">Snapshot and delta</a> – the
					core storage model in detail
				</li>
				<li>
					<a href="/docs/internals/csr">CSR format</a> – how edges are stored
				</li>
				<li>
					<a href="/docs/internals/single-file">Single-file format</a> – the{" "}
					<code>.kitedb</code> file layout
				</li>
			</ul>
		</DocPage>
	);
}
