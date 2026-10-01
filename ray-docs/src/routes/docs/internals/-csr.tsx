import { ArrowRight } from "lucide-solid";
import { For, type JSX, Show } from "solid-js";
import CodeBlock from "~/components/code-block";
import DocPage from "~/components/doc-page";
import { Code, Figure, Slot, StepNumber } from "./-components";

// ============================================================================
// CSR-SPECIFIC COMPONENTS
// ============================================================================

/** Small mono label used for array names. */
function ArrayName(props: { children: JSX.Element }) {
	return (
		<span class="w-28 shrink-0 font-mono text-[12px] text-slate-400">
			{props.children}
		</span>
	);
}

const MATRIX_NODES = ["A", "B", "C", "D"];
const MATRIX = [
	[0, 1, 1, 0],
	[0, 0, 0, 1],
	[1, 0, 0, 0],
	[0, 0, 0, 0],
];

// Adjacency matrix problem
function AdjacencyMatrixProblem() {
	return (
		<Figure title="Adjacency matrix" accent="red" variant="problem">
			<div class="flex flex-col gap-6 sm:flex-row sm:items-start">
				<div
					class="grid shrink-0 grid-cols-[1.25rem_repeat(4,1.75rem)] gap-1 font-mono text-[11px]"
					role="img"
					aria-label="4 by 4 adjacency matrix with 4 edges set"
				>
					<span />
					<For each={MATRIX_NODES}>
						{(node) => (
							<span class="grid h-5 place-items-center text-slate-500">
								{node}
							</span>
						)}
					</For>
					<For each={MATRIX}>
						{(row, r) => (
							<>
								<span class="grid h-7 place-items-center text-slate-500">
									{MATRIX_NODES[r()]}
								</span>
								<For each={row}>
									{(cell) => (
										<span
											class="grid h-7 place-items-center rounded-md border text-[12px]"
											classList={{
												"border-kite-cyan/25 bg-kite-cyan/10 text-kite-cyan":
													cell === 1,
												"border-kite-line bg-white/[0.03] text-slate-600":
													cell === 0,
											}}
										>
											{cell}
										</span>
									)}
								</For>
							</>
						)}
					</For>
				</div>

				<div class="flex-1 space-y-2 text-[14px] text-slate-400">
					<p>
						<span class="font-mono text-slate-200">100K × 100K</span> ={" "}
						<span class="font-semibold text-white">10 billion</span> entries
					</p>
					<p>
						Actual edges: <span class="font-semibold text-white">1M</span>{" "}
						<span class="text-slate-500">(0.01% used)</span>
					</p>
					<p class="mt-3 inline-flex items-center gap-2 rounded-lg border border-red-400/25 bg-red-400/[0.06] px-3 py-1.5 text-[13px] text-slate-200">
						<span
							class="h-1.5 w-1.5 rounded-full bg-red-400"
							aria-hidden="true"
						/>
						99.99% of the matrix is empty space
					</p>
				</div>
			</div>
		</Figure>
	);
}

/** Linked-list row: head node, then chained cells ending in null. */
function ListRow(props: { head: string; cells: string[] }) {
	return (
		<div class="flex items-center gap-1.5">
			<span class="w-5 font-mono text-[13px] text-slate-300">{props.head}</span>
			<For each={props.cells}>
				{(cell) => (
					<>
						<ArrowRight size={13} class="text-slate-600" aria-hidden="true" />
						<Slot class="w-8">{cell}</Slot>
					</>
				)}
			</For>
			<ArrowRight size={13} class="text-slate-600" aria-hidden="true" />
			<span class="font-mono text-[12px] text-slate-600">null</span>
		</div>
	);
}

// Linked list problem
function LinkedListProblem() {
	return (
		<Figure title="Linked adjacency lists" accent="amber" variant="tradeoff">
			<div class="space-y-2">
				<ListRow head="A" cells={["B", "C"]} />
				<ListRow head="B" cells={["D"]} />
			</div>

			<p class="mt-4 text-[14px] text-slate-400">
				<span class="font-medium text-slate-200">Problem:</span> pointer
				chasing. Each hop goes to a random memory location.
			</p>
			<div class="mt-2 flex flex-wrap gap-x-5 gap-y-1 text-[13px] text-slate-400">
				<p class="flex items-center gap-2">
					<span
						class="h-1.5 w-1.5 rounded-full bg-red-400"
						aria-hidden="true"
					/>
					Cache miss <span class="font-mono text-slate-200">~100 ns</span>
				</p>
				<p class="flex items-center gap-2">
					<span
						class="h-1.5 w-1.5 rounded-full bg-kite-mint"
						aria-hidden="true"
					/>
					Cache hit <span class="font-mono text-slate-200">~1 ns</span>
				</p>
			</div>
			<p class="mt-4 rounded-lg border border-amber-400/25 bg-amber-400/[0.06] px-3 py-2 text-[13px] text-slate-200">
				1,000 edges × 100 ns = 100 μs spent waiting on RAM
			</p>
		</Figure>
	);
}

// Destinations grouped by source node; A's group is highlighted to match the traversal example
const DEST_GROUPS = [
	{ source: "A", dsts: ["B", "C"] },
	{ source: "B", dsts: ["D"] },
	{ source: "C", dsts: ["A"] },
];

const OFFSETS = [
	{ value: 0, label: "A" },
	{ value: 2, label: "B" },
	{ value: 3, label: "C" },
	{ value: 4, label: "D" },
	{ value: 4, label: "end" },
];

// CSR solution
function CSRSolutionDiagram() {
	return (
		<Figure title="CSR layout" accent="mint">
			<div class="rounded-lg border border-kite-line bg-white/[0.02] px-4 py-3">
				<p class="font-mono text-[11px] uppercase tracking-[0.08em] text-slate-500">
					Graph
				</p>
				<div class="mt-2 flex flex-wrap gap-x-6 gap-y-1 font-mono text-[13px] text-slate-300">
					<span>A → B, C</span>
					<span>B → D</span>
					<span>C → A</span>
					<span>
						D → <span class="text-slate-500">(none)</span>
					</span>
				</div>
			</div>

			<div class="mt-6 space-y-7">
				<div class="flex gap-3">
					<StepNumber accent="cyan">1</StepNumber>
					<div class="min-w-0">
						<p class="text-[14px] text-slate-300">
							Concatenate every node's destinations:
						</p>
						<div class="mt-3 flex flex-col gap-2 sm:flex-row sm:items-start">
							<span class="pt-2">
								<ArrayName>destinations</ArrayName>
							</span>
							<div class="flex gap-1">
								<For each={DEST_GROUPS}>
									{(group) => (
										<div class="flex flex-col items-stretch">
											<div class="flex gap-1">
												<For each={group.dsts}>
													{(dst) => (
														<Slot
															tone={group.source === "A" ? "cyan" : undefined}
														>
															{dst}
														</Slot>
													)}
												</For>
											</div>
											<div
												class="mx-2 mt-1.5 h-1.5 rounded-b-sm border-x border-b border-slate-600"
												aria-hidden="true"
											/>
											<span class="mt-1 text-center font-mono text-[11px] text-slate-500">
												{group.source}
											</span>
										</div>
									)}
								</For>
							</div>
						</div>
					</div>
				</div>

				<div class="flex gap-3">
					<StepNumber accent="cyan">2</StepNumber>
					<div class="min-w-0">
						<p class="text-[14px] text-slate-300">
							Record where each node's edges start:
						</p>
						<div class="mt-3 flex flex-col gap-2 sm:flex-row sm:items-start">
							<span class="pt-2">
								<ArrayName>offsets</ArrayName>
							</span>
							<div class="flex gap-1">
								<For each={OFFSETS}>
									{(offset, i) => (
										<div class="flex flex-col items-center">
											<Slot tone={i() < 2 ? "cyan" : undefined}>
												{offset.value}
											</Slot>
											<span class="mt-1.5 font-mono text-[11px] text-slate-500">
												{offset.label}
											</span>
										</div>
									)}
								</For>
							</div>
						</div>
					</div>
				</div>
			</div>

			<p class="mt-6 text-[13px] text-slate-500">
				Highlighted: node A's edges, and the two offsets that bound them.
			</p>
		</Figure>
	);
}

/** A worked neighbor lookup. */
function Lookup(props: {
	node: string;
	index: number;
	start: number;
	end: number;
	result: string;
	empty?: boolean;
}) {
	return (
		<div class="rounded-xl border border-kite-line bg-kite-surface/60 p-4">
			<p class="text-[14px] font-medium text-slate-200">
				Who does {props.node} connect to?
			</p>
			<div class="mt-3 space-y-1 font-mono text-[12.5px] text-slate-400">
				<p>
					start = offsets[{props.index}] ={" "}
					<span class="text-slate-100">{props.start}</span>
				</p>
				<p>
					end = offsets[{props.index + 1}] ={" "}
					<span class="text-slate-100">{props.end}</span>
				</p>
				<p>
					destinations[{props.start}:{props.end}] ={" "}
					<span classList={{ "text-kite-mint": !props.empty }}>
						{props.result}
					</span>
					<Show when={props.empty}>
						<span class="text-slate-500"> (no edges)</span>
					</Show>
				</p>
			</div>
		</div>
	);
}

// CSR traversal examples
function CSRTraversalExample() {
	return (
		<figure class="space-y-3">
			<div class="grid gap-3 sm:grid-cols-2">
				<Lookup node="A" index={0} start={0} end={2} result="[B, C]" />
				<Lookup node="D" index={3} start={4} end={4} result="[]" empty />
			</div>
			<div class="rounded-xl border border-kite-line bg-[#070a12] p-4">
				<p class="font-mono text-[11px] uppercase tracking-[0.08em] text-slate-500">
					Algorithm
				</p>
				<div class="mt-2 space-y-0.5 font-mono text-[13px] text-slate-300">
					<p>start = offsets[node]</p>
					<p>end = offsets[node + 1]</p>
					<p>return destinations[start:end]</p>
				</div>
			</div>
		</figure>
	);
}

// Memory layout comparison
function MemoryLayoutComparison() {
	return (
		<figure>
			<div class="grid gap-3 sm:grid-cols-2">
				<div class="rounded-xl border border-red-400/25 bg-red-400/[0.04] p-4">
					<p class="flex items-center gap-2.5 text-[15px] font-semibold text-white">
						<span
							class="h-1.5 w-1.5 rounded-full bg-red-400"
							aria-hidden="true"
						/>
						Linked list
						<span class="text-[13px] font-normal text-slate-500">
							scattered
						</span>
					</p>
					<div class="mt-4 flex gap-4">
						<For
							each={[
								{ v: "B", addr: "0x1000" },
								{ v: "C", addr: "0x5F00" },
								{ v: "D", addr: "0x2A00" },
							]}
						>
							{(cell) => (
								<div class="flex w-14 flex-col items-center">
									<Slot class="w-10">{cell.v}</Slot>
									<span class="mt-1.5 font-mono text-[11px] text-slate-500">
										{cell.addr}
									</span>
								</div>
							)}
						</For>
					</div>
					<p class="mt-4 text-[13px] text-slate-400">
						Random locations, so each hop can miss the cache.
					</p>
				</div>

				<div class="rounded-xl border border-kite-line bg-kite-surface/60 p-4">
					<p class="flex items-center gap-2.5 text-[15px] font-semibold text-white">
						<span
							class="h-1.5 w-1.5 rounded-full bg-kite-mint"
							aria-hidden="true"
						/>
						CSR
						<span class="text-[13px] font-normal text-slate-500">
							contiguous
						</span>
					</p>
					<div class="mt-4 flex gap-1">
						<For
							each={[
								{ v: "B", addr: "0x1000" },
								{ v: "C", addr: "+4" },
								{ v: "D", addr: "+8" },
								{ v: "A", addr: "+C" },
							]}
						>
							{(cell) => (
								<div class="flex w-12 flex-col items-center">
									<Slot tone="mint" class="w-12">
										{cell.v}
									</Slot>
									<span class="mt-1.5 font-mono text-[11px] text-slate-500">
										{cell.addr}
									</span>
								</div>
							)}
						</For>
					</div>
					<p class="mt-4 text-[13px] text-slate-400">
						Sequential addresses, so the CPU prefetcher can load ahead.
					</p>
				</div>
			</div>
			<figcaption class="mt-3 text-[14px] text-slate-400">
				After the first access, B, C, D, and A are already in the CPU cache.
			</figcaption>
		</figure>
	);
}

// Bidirectional edges
function BidirectionalEdges() {
	return (
		<figure class="rounded-xl border border-kite-line bg-kite-surface/60 p-5">
			<div class="grid gap-3 sm:grid-cols-2">
				<div class="rounded-lg border border-kite-line bg-white/[0.02] p-4">
					<p class="flex items-center gap-2.5 text-[14px] font-semibold text-white">
						<span
							class="h-1.5 w-1.5 rounded-full bg-kite-cyan"
							aria-hidden="true"
						/>
						Out-edges
						<span class="font-mono text-[12px] font-normal text-slate-500">
							A → B
						</span>
					</p>
					<div class="mt-3 space-y-1 font-mono text-[12px] text-slate-400">
						<p>
							out_offsets = <span class="text-slate-200">[0, 2, 3, 4, 4]</span>
						</p>
						<p>
							out_dst = <span class="text-slate-200">[B, C, D, A]</span>
						</p>
					</div>
					<p class="mt-3 text-[13px] text-slate-500">Who does A follow?</p>
				</div>

				<div class="rounded-lg border border-kite-line bg-white/[0.02] p-4">
					<p class="flex items-center gap-2.5 text-[14px] font-semibold text-white">
						<span
							class="h-1.5 w-1.5 rounded-full bg-kite-violet"
							aria-hidden="true"
						/>
						In-edges
						<span class="font-mono text-[12px] font-normal text-slate-500">
							A ← C
						</span>
					</p>
					<div class="mt-3 space-y-1 font-mono text-[12px] text-slate-400">
						<p>
							in_offsets = <span class="text-slate-200">[0, 1, 2, 3, 4]</span>
						</p>
						<p>
							in_src = <span class="text-slate-200">[C, A, A, B]</span>
						</p>
					</div>
					<p class="mt-3 text-[13px] text-slate-500">Who follows A?</p>
				</div>
			</div>

			<p class="mt-4 rounded-lg border border-amber-400/25 bg-amber-400/[0.06] px-3 py-2 text-[13px] text-slate-300">
				<span class="font-medium text-amber-300">Tradeoff:</span> 2× edge
				storage, in exchange for O(1) traversal in both directions.
			</p>
		</figure>
	);
}

const OUT_EDGES = [
	{ dst: "B", etype: 0 },
	{ dst: "C", etype: 1 },
	{ dst: "D", etype: 0 },
	{ dst: "A", etype: 0 },
];

// Edge types and sorting
function EdgeTypesSorting() {
	return (
		<figure class="rounded-xl border border-kite-line bg-kite-surface/60 p-5">
			<div class="space-y-2">
				<div class="flex items-center gap-2">
					<ArrayName>out_dst</ArrayName>
					<div class="flex gap-1">
						<For each={OUT_EDGES}>{(edge) => <Slot>{edge.dst}</Slot>}</For>
					</div>
				</div>
				<div class="flex items-center gap-2">
					<ArrayName>out_etype</ArrayName>
					<div class="flex gap-1">
						<For each={OUT_EDGES}>
							{(edge) => (
								<Slot tone={edge.etype === 0 ? "mint" : "violet"}>
									{edge.etype}
								</Slot>
							)}
						</For>
					</div>
				</div>
			</div>

			<div class="mt-3 flex gap-5 font-mono text-[12px] text-slate-400">
				<p class="flex items-center gap-2">
					<span
						class="h-1.5 w-1.5 rounded-full bg-kite-mint"
						aria-hidden="true"
					/>
					0 = KNOWS
				</p>
				<p class="flex items-center gap-2">
					<span
						class="h-1.5 w-1.5 rounded-full bg-kite-violet"
						aria-hidden="true"
					/>
					1 = LIKES
				</p>
			</div>

			<div class="mt-5 border-t border-kite-line pt-4">
				<p class="text-[14px] text-slate-300">
					Edges are sorted by <Code>(etype, dst)</Code> within each node, so:
				</p>
				<ul class="mt-2 space-y-1.5 text-[14px] text-slate-400">
					<For
						each={[
							"Binary search finds a specific edge type",
							"Scanning stops once it passes the wanted type",
							"Getting A's KNOWS edges doesn't scan all of A's edges",
						]}
					>
						{(item) => (
							<li class="flex items-start gap-3">
								<span
									class="mt-[0.6em] h-1.5 w-1.5 shrink-0 rounded-full bg-kite-mint opacity-70"
									aria-hidden="true"
								/>
								{item}
							</li>
						)}
					</For>
				</ul>
			</div>
		</figure>
	);
}

// ============================================================================
// PAGE COMPONENT
// ============================================================================

export function CSRPage() {
	return (
		<DocPage slug="internals/csr">
			<p>
				KiteDB uses <strong>Compressed Sparse Row (CSR)</strong> format to store
				graph edges. CSR is a standard format for sparse matrices: each node's
				neighbors are stored next to each other, so traversal is fast and the
				memory overhead is small.
			</p>

			<h2 id="the-problem">The problem with naive edge storage</h2>

			<p>Consider a graph with 100,000 nodes and 1 million edges.</p>

			<AdjacencyMatrixProblem />

			<LinkedListProblem />

			<h2 id="csr-solution">The CSR solution</h2>

			<p>
				CSR stores all edges in two flat arrays: <strong>offsets</strong> and{" "}
				<strong>destinations</strong>. Edges are found by array index rather
				than by following pointers, and the arrays only hold edges that exist.
			</p>

			<CSRSolutionDiagram />

			<h2 id="traversal">How traversal works</h2>

			<p>Finding a node's neighbors takes two array lookups:</p>

			<CSRTraversalExample />

			<h2 id="memory-layout">Why it's fast: memory layout</h2>

			<MemoryLayoutComparison />

			<h2 id="bidirectional">Bidirectional edges</h2>

			<p>
				KiteDB stores edges in <strong>both directions</strong>, so traversal is
				fast either way:
			</p>

			<BidirectionalEdges />

			<h2 id="edge-types">Edge types and sorting</h2>

			<p>
				Real graphs have different edge types (follows, likes, knows). KiteDB
				stores edge types in a parallel array, sorted within each node:
			</p>

			<EdgeTypesSorting />

			<h2 id="existence-check">Edge existence check</h2>

			<p>To check if edge A→B exists with type KNOWS:</p>

			<CodeBlock
				code={`function hasEdge(src: NodeID, etype: EdgeType, dst: NodeID): boolean {
  const start = offsets[src];
  const end = offsets[src + 1];

  // Binary search for etype within [start, end)
  const typeStart = binarySearchStart(etypes, start, end, etype);
  const typeEnd = binarySearchEnd(etypes, start, end, etype);

  // Binary search for dst within type range
  return binarySearch(destinations, typeStart, typeEnd, dst);
}

// Complexity: O(log k) where k = number of edges from src`}
				language="typescript"
			/>

			<h2 id="numbers">Performance comparison</h2>

			<table>
				<thead>
					<tr>
						<th>Operation</th>
						<th>CSR</th>
						<th>Linked list</th>
					</tr>
				</thead>
				<tbody>
					<tr>
						<td>Start traversal</td>
						<td>O(1) – two array lookups</td>
						<td>O(1) – follow pointer</td>
					</tr>
					<tr>
						<td>Iterate k neighbors</td>
						<td>O(k) – sequential read</td>
						<td>O(k) – but cache misses</td>
					</tr>
					<tr>
						<td>Edge existence</td>
						<td>O(log k) – binary search</td>
						<td>O(k) – linear scan</td>
					</tr>
					<tr>
						<td>Cache behavior</td>
						<td>Excellent – prefetcher works</td>
						<td>Poor – random access</td>
					</tr>
				</tbody>
			</table>

			<h2 id="next">Next steps</h2>
			<ul>
				<li>
					<a href="/docs/internals/snapshot-delta">Snapshot and delta</a> – how
					CSR fits into the storage model
				</li>
				<li>
					<a href="/docs/internals/key-index">Key index</a> – how node lookups
					work
				</li>
				<li>
					<a href="/docs/internals/performance">Performance</a> – optimization
					techniques
				</li>
			</ul>
		</DocPage>
	);
}
