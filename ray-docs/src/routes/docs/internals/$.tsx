import { createFileRoute, useLocation } from "@tanstack/solid-router";
import { type JSX, Show } from "solid-js";
import CodeBlock from "~/components/code-block";
import DocPage from "~/components/doc-page";
import { findDocBySlug } from "~/lib/docs";

// Inline code component for data flow
function Code(props: { children: JSX.Element; color?: string }) {
	const colorClass = () => {
		switch (props.color) {
			case "cyan":
				return "text-cyan-300/80";
			case "violet":
				return "text-violet-300/80";
			case "emerald":
				return "text-emerald-300/80";
			default:
				return "text-cyan-300/80";
		}
	};
	return (
		<code class={`${colorClass()} text-xs px-1 bg-slate-700/50 rounded`}>
			{props.children}
		</code>
	);
}

// Bold label for storage layer items
function Label(props: { children: JSX.Element; color?: string }) {
	const colorClass = () => {
		switch (props.color) {
			case "emerald":
				return "text-emerald-400";
			case "violet":
				return "text-violet-400";
			default:
				return "text-cyan-400";
		}
	};
	return <strong class={colorClass()}>{props.children}</strong>;
}

// Flow item row
function FlowItem(props: {
	isLast?: boolean;
	color: string;
	children: JSX.Element;
}) {
	const connectorColor = () => {
		switch (props.color) {
			case "cyan":
				return "text-cyan-500/60";
			case "violet":
				return "text-violet-500/60";
			case "emerald":
				return "text-emerald-500/60";
			default:
				return "text-slate-500/60";
		}
	};
	return (
		<div class="flex items-start gap-2 text-sm text-slate-300">
			<span class={`mt-0.5 ${connectorColor()}`}>
				{props.isLast ? "└" : "├"}
			</span>
			<span>{props.children}</span>
		</div>
	);
}

// Data flow step card
function FlowStep(props: {
	number: string;
	title: string;
	color: string;
	children: JSX.Element;
}) {
	const borderColor = () => {
		switch (props.color) {
			case "cyan":
				return "border-cyan-500/30";
			case "violet":
				return "border-violet-500/30";
			case "emerald":
				return "border-emerald-500/30";
			default:
				return "border-slate-500/30";
		}
	};
	const badgeColor = () => {
		switch (props.color) {
			case "cyan":
				return "bg-cyan-500/20 text-cyan-400";
			case "violet":
				return "bg-violet-500/20 text-violet-400";
			case "emerald":
				return "bg-emerald-500/20 text-emerald-400";
			default:
				return "bg-slate-500/20 text-slate-400";
		}
	};
	const titleColor = () => {
		switch (props.color) {
			case "cyan":
				return "text-cyan-400";
			case "violet":
				return "text-violet-400";
			case "emerald":
				return "text-emerald-400";
			default:
				return "text-slate-400";
		}
	};
	return (
		<div
			class={`rounded-xl border ${borderColor()} bg-gradient-to-br from-slate-900 to-slate-800 p-4 shadow-lg`}
		>
			<div class="flex items-center gap-3 mb-3">
				<span
					class={`flex items-center justify-center w-7 h-7 rounded-lg text-sm font-bold ${badgeColor()}`}
				>
					{props.number}
				</span>
				<h4 class={`font-semibold ${titleColor()}`}>{props.title}</h4>
			</div>
			<div class="space-y-2 pl-2">{props.children}</div>
		</div>
	);
}

// Arrow connector between steps
function FlowArrow() {
	return (
		<div class="flex justify-center">
			<svg
				class="w-5 h-5 text-slate-500"
				viewBox="0 0 24 24"
				fill="none"
				stroke="currentColor"
				stroke-width="2"
			>
				<path
					stroke-linecap="round"
					stroke-linejoin="round"
					d="M19 14l-7 7m0 0l-7-7m7 7V3"
				/>
			</svg>
		</div>
	);
}

// Insert data flow diagram
function InsertDataFlow() {
	return (
		<div class="my-6 space-y-3">
			<FlowStep number="1" title="Query Layer" color="cyan">
				<FlowItem color="cyan">
					Validates schema <Code color="cyan">user</Code> has required
					properties
				</FlowItem>
				<FlowItem color="cyan">
					Converts <Code color="cyan">age: 30</Code> → internal I64 type
				</FlowItem>
				<FlowItem color="cyan" isLast>
					Calls graph layer: <Code color="cyan">createNode(...)</Code>
				</FlowItem>
			</FlowStep>

			<FlowArrow />

			<FlowStep number="2" title="Graph Layer" color="violet">
				<FlowItem color="violet">
					Begins transaction (if not already in one)
				</FlowItem>
				<FlowItem color="violet">
					Allocates new <Code color="violet">NodeID</Code> (monotonic counter)
				</FlowItem>
				<FlowItem color="violet">Records in transaction state</FlowItem>
				<FlowItem color="violet" isLast>
					On commit → writes to WAL and Delta
				</FlowItem>
			</FlowStep>

			<FlowArrow />

			<FlowStep number="3" title="Storage Layer" color="emerald">
				<FlowItem color="emerald">
					<Label color="emerald">WAL</Label>: Appends CREATE_NODE record
					(durability)
				</FlowItem>
				<FlowItem color="emerald">
					<Label color="emerald">Delta</Label>: Adds node to{" "}
					<Code color="emerald">createdNodes</Code> map
				</FlowItem>
				<FlowItem color="emerald" isLast>
					<Label color="emerald">Later</Label>: Checkpoint merges into snapshot
				</FlowItem>
			</FlowStep>
		</div>
	);
}

// Read data flow diagram
function ReadDataFlow() {
	return (
		<div class="my-6 space-y-3">
			<FlowStep number="1" title="Key Index Lookup" color="cyan">
				<FlowItem color="cyan">
					Check <Code color="cyan">delta.keyIndex</Code> (recent changes)
				</FlowItem>
				<FlowItem color="cyan">
					If not found → check snapshot's hash-bucketed index
				</FlowItem>
				<FlowItem color="cyan" isLast>
					Returns <Code color="cyan">NodeID</Code>
				</FlowItem>
			</FlowStep>

			<FlowArrow />

			<FlowStep number="2" title="Property Fetch" color="violet">
				<FlowItem color="violet">
					Check <Code color="violet">delta.modifiedNodes</Code> for changes
				</FlowItem>
				<FlowItem color="violet">
					Fall back to snapshot for unchanged properties
				</FlowItem>
				<FlowItem color="violet" isLast>
					Merge and return combined result
				</FlowItem>
			</FlowStep>

			{/* Result badge */}
			<div class="flex justify-center pt-2">
				<div class="inline-flex items-center gap-2 px-4 py-2 rounded-lg bg-emerald-500/10 border border-emerald-500/30 text-emerald-400 text-sm">
					<svg
						class="w-4 h-4"
						viewBox="0 0 24 24"
						fill="none"
						stroke="currentColor"
						stroke-width="2"
					>
						<path
							stroke-linecap="round"
							stroke-linejoin="round"
							d="M5 13l4 4L19 7"
						/>
					</svg>
					<span>Returns latest committed data</span>
				</div>
			</div>
		</div>
	);
}

// Delta State structure visualization
function DeltaStateStructure() {
	return (
		<div class="my-6 rounded-xl border border-violet-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<div class="flex items-center gap-2 mb-4">
				<svg
					class="w-5 h-5 text-violet-400"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
				>
					<path
						stroke-linecap="round"
						stroke-linejoin="round"
						d="M4 7v10c0 2.21 3.582 4 8 4s8-1.79 8-4V7M4 7c0 2.21 3.582 4 8 4s8-1.79 8-4M4 7c0-2.21 3.582-4 8-4s8 1.79 8 4m0 5c0 2.21-3.582 4-8 4s-8-1.79-8-4"
					/>
				</svg>
				<h4 class="font-semibold text-violet-400">Delta State</h4>
			</div>
			<div class="space-y-2">
				<DeltaRow
					connector="├"
					name="createdNodes"
					type="Map<NodeID, NodeData>"
					desc="New nodes"
					color="emerald"
				/>
				<DeltaRow
					connector="├"
					name="deletedNodes"
					type="Set<NodeID>"
					desc="Tombstones"
					color="red"
				/>
				<DeltaRow
					connector="├"
					name="modifiedNodes"
					type="Map<NodeID, PropChanges>"
					desc="Property updates"
					color="amber"
				/>
				<DeltaRow
					connector="├"
					name="outAdd/outDel"
					type="Map<NodeID, EdgePatch[]>"
					desc="Edge changes"
					color="violet"
				/>
				<DeltaRow
					connector="├"
					name="inAdd/inDel"
					type="Map<NodeID, EdgePatch[]>"
					desc="Reverse index"
					color="violet"
				/>
				<DeltaRow
					connector="└"
					name="keyIndex"
					type="Map<string, NodeID>"
					desc="Key lookups"
					color="cyan"
				/>
			</div>
		</div>
	);
}

function DeltaRow(props: {
	connector: string;
	name: string;
	type: string;
	desc: string;
	color: string;
}) {
	const colorClass = () => {
		switch (props.color) {
			case "emerald":
				return "text-emerald-400";
			case "red":
				return "text-red-400";
			case "amber":
				return "text-amber-400";
			case "violet":
				return "text-violet-400";
			default:
				return "text-cyan-400";
		}
	};
	return (
		<div class="flex items-center gap-3 text-sm">
			<span class="text-slate-600">{props.connector}</span>
			<code class={`font-mono ${colorClass()}`}>{props.name}</code>
			<span class="text-slate-500 text-xs hidden sm:inline">{props.type}</span>
			<span class="text-slate-400 ml-auto text-xs">{props.desc}</span>
		</div>
	);
}

// Read decision flow visualization
function ReadFlowDiagram() {
	return (
		<div class="my-6 space-y-3">
			{/* Step 1 */}
			<div class="rounded-xl border border-slate-600/50 bg-slate-800/50 p-4">
				<div class="flex items-start gap-3">
					<span class="flex items-center justify-center w-6 h-6 rounded-full bg-cyan-500/20 text-cyan-400 text-xs font-bold shrink-0">
						1
					</span>
					<div class="flex-1">
						<p class="text-slate-300 text-sm">
							Is{" "}
							<code class="text-red-400 text-xs px-1 bg-slate-700/50 rounded">
								nodeId
							</code>{" "}
							in{" "}
							<code class="text-violet-400 text-xs px-1 bg-slate-700/50 rounded">
								delta.deletedNodes
							</code>
							?
						</p>
						<div class="mt-2 flex items-center gap-2 text-xs">
							<span class="text-emerald-400">→ Yes:</span>
							<span class="text-slate-400">return null (deleted)</span>
						</div>
					</div>
				</div>
			</div>

			{/* Step 2 */}
			<div class="rounded-xl border border-slate-600/50 bg-slate-800/50 p-4">
				<div class="flex items-start gap-3">
					<span class="flex items-center justify-center w-6 h-6 rounded-full bg-cyan-500/20 text-cyan-400 text-xs font-bold shrink-0">
						2
					</span>
					<div class="flex-1">
						<p class="text-slate-300 text-sm">
							Is{" "}
							<code class="text-red-400 text-xs px-1 bg-slate-700/50 rounded">
								nodeId
							</code>{" "}
							in{" "}
							<code class="text-violet-400 text-xs px-1 bg-slate-700/50 rounded">
								delta.createdNodes
							</code>
							?
						</p>
						<div class="mt-2 flex items-center gap-2 text-xs">
							<span class="text-emerald-400">→ Yes:</span>
							<span class="text-slate-400">return delta data (new node)</span>
						</div>
					</div>
				</div>
			</div>

			{/* Step 3 */}
			<div class="rounded-xl border border-slate-600/50 bg-slate-800/50 p-4">
				<div class="flex items-start gap-3">
					<span class="flex items-center justify-center w-6 h-6 rounded-full bg-cyan-500/20 text-cyan-400 text-xs font-bold shrink-0">
						3
					</span>
					<div class="flex-1">
						<p class="text-slate-300 text-sm">
							Does{" "}
							<code class="text-cyan-400 text-xs px-1 bg-slate-700/50 rounded">
								snapshot
							</code>{" "}
							have this node?
						</p>
						<div class="mt-2 flex items-center gap-2 text-xs">
							<span class="text-red-400">→ No:</span>
							<span class="text-slate-400">return null (never existed)</span>
						</div>
					</div>
				</div>
			</div>

			{/* Step 4 - Result */}
			<div class="rounded-xl border border-emerald-500/30 bg-emerald-500/5 p-4">
				<div class="flex items-start gap-3">
					<span class="flex items-center justify-center w-6 h-6 rounded-full bg-emerald-500/20 text-emerald-400 text-xs font-bold shrink-0">
						4
					</span>
					<div class="flex-1">
						<p class="text-slate-300 text-sm">
							Merge{" "}
							<code class="text-cyan-400 text-xs px-1 bg-slate-700/50 rounded">
								snapshot
							</code>{" "}
							+{" "}
							<code class="text-violet-400 text-xs px-1 bg-slate-700/50 rounded">
								delta.modifiedNodes
							</code>
						</p>
						<div class="mt-2 flex items-center gap-2 text-xs">
							<span class="text-emerald-400">→</span>
							<span class="text-emerald-400 font-medium">
								Return combined result
							</span>
						</div>
					</div>
				</div>
			</div>
		</div>
	);
}

// Write flow visualization
function WriteFlowDiagram() {
	return (
		<div class="my-6 rounded-xl border border-slate-600/50 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<h4 class="text-sm font-semibold text-slate-400 mb-4">
				Transaction Commit
			</h4>
			<div class="space-y-3">
				{/* WAL */}
				<div class="flex items-center gap-3">
					<span class="flex items-center justify-center w-6 h-6 rounded-full bg-emerald-500/20 text-emerald-400 text-xs font-bold shrink-0">
						1
					</span>
					<div class="flex items-center gap-2 flex-1">
						<span class="font-semibold text-emerald-400 w-16">WAL</span>
						<span class="text-slate-500">→</span>
						<span class="text-slate-300 text-sm">
							Append records (ensures durability)
						</span>
					</div>
				</div>

				{/* Delta */}
				<div class="flex items-center gap-3">
					<span class="flex items-center justify-center w-6 h-6 rounded-full bg-violet-500/20 text-violet-400 text-xs font-bold shrink-0">
						2
					</span>
					<div class="flex items-center gap-2 flex-1">
						<span class="font-semibold text-violet-400 w-16">Delta</span>
						<span class="text-slate-500">→</span>
						<span class="text-slate-300 text-sm">
							Update in-memory state (visible to reads)
						</span>
					</div>
				</div>

				{/* Cache */}
				<div class="flex items-center gap-3">
					<span class="flex items-center justify-center w-6 h-6 rounded-full bg-amber-500/20 text-amber-400 text-xs font-bold shrink-0">
						3
					</span>
					<div class="flex items-center gap-2 flex-1">
						<span class="font-semibold text-amber-400 w-16">Cache</span>
						<span class="text-slate-500">→</span>
						<span class="text-slate-300 text-sm">
							Invalidate affected entries
						</span>
					</div>
				</div>
			</div>

			{/* Note */}
			<div class="mt-4 pt-4 border-t border-slate-700/50">
				<p class="text-xs text-slate-500 flex items-center gap-2">
					<svg
						class="w-4 h-4 text-cyan-500"
						viewBox="0 0 24 24"
						fill="none"
						stroke="currentColor"
						stroke-width="2"
					>
						<path
							stroke-linecap="round"
							stroke-linejoin="round"
							d="M13 16h-1v-4h-1m1-4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z"
						/>
					</svg>
					The snapshot is NOT touched during normal writes
				</p>
			</div>
		</div>
	);
}

// Checkpoint step component
function CheckpointStep(props: { num: number; text: string }) {
	const colorClass = () => {
		if (props.num <= 2) return "bg-cyan-500/20 text-cyan-400";
		if (props.num <= 4) return "bg-violet-500/20 text-violet-400";
		return "bg-emerald-500/20 text-emerald-400";
	};
	return (
		<div class="flex items-center gap-3 relative">
			<span
				class={`flex items-center justify-center w-6 h-6 rounded-full text-xs font-bold shrink-0 z-10 ${colorClass()}`}
			>
				{props.num}
			</span>
			<span class="text-slate-300 text-sm">{props.text}</span>
		</div>
	);
}

// Checkpoint process visualization
function CheckpointProcess() {
	return (
		<div class="my-6">
			<div class="rounded-xl border border-cyan-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
				<h4 class="text-sm font-semibold text-slate-400 mb-4">
					Checkpoint Process
				</h4>

				{/* Steps */}
				<div class="relative">
					{/* Vertical line */}
					<div class="absolute left-3 top-3 bottom-3 w-px bg-gradient-to-b from-cyan-500/50 via-violet-500/50 to-emerald-500/50" />

					<div class="space-y-3">
						<CheckpointStep num={1} text="Read current snapshot" />
						<CheckpointStep num={2} text="Apply all delta changes" />
						<CheckpointStep
							num={3}
							text="Write new snapshot (CSR, compressed)"
						/>
						<CheckpointStep
							num={4}
							text="Update header to point to new snapshot"
						/>
						<CheckpointStep num={5} text="Clear delta and WAL" />
					</div>
				</div>

				{/* Timing note */}
				<div class="mt-4 pt-4 border-t border-slate-700/50 flex flex-wrap gap-4 text-xs">
					<div class="flex items-center gap-2">
						<span class="w-2 h-2 rounded-full bg-cyan-400" />
						<span class="text-slate-400">Auto: when WAL reaches threshold</span>
					</div>
					<div class="flex items-center gap-2">
						<span class="w-2 h-2 rounded-full bg-violet-400" />
						<span class="text-slate-400">
							Manual: <code class="text-violet-400">db.checkpoint()</code>
						</span>
					</div>
				</div>
			</div>
		</div>
	);
}

// ============================================================================
// CSR PAGE COMPONENTS
// ============================================================================

// Adjacency Matrix problem visualization
function AdjacencyMatrixProblem() {
	return (
		<div class="my-6 rounded-xl border border-red-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<div class="flex items-center gap-2 mb-4">
				<svg
					class="w-5 h-5 text-red-400"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
				>
					<path
						stroke-linecap="round"
						stroke-linejoin="round"
						d="M12 9v2m0 4h.01m-6.938 4h13.856c1.54 0 2.502-1.667 1.732-3L13.732 4c-.77-1.333-2.694-1.333-3.464 0L3.34 16c-.77 1.333.192 3 1.732 3z"
					/>
				</svg>
				<h4 class="font-semibold text-red-400">Adjacency Matrix</h4>
			</div>

			{/* Mini matrix visualization */}
			<div class="flex flex-col sm:flex-row gap-4 items-start">
				<div class="font-mono text-xs">
					<div class="text-slate-500 mb-1">{"     A  B  C  D"}</div>
					<div class="text-slate-400">
						A <span class="text-slate-600">[</span> 0{" "}
						<span class="text-cyan-400">1</span>{" "}
						<span class="text-cyan-400">1</span> 0{" "}
						<span class="text-slate-600">]</span>
					</div>
					<div class="text-slate-400">
						B <span class="text-slate-600">[</span> 0 0 0{" "}
						<span class="text-cyan-400">1</span>{" "}
						<span class="text-slate-600">]</span>
					</div>
					<div class="text-slate-400">
						C <span class="text-slate-600">[</span>{" "}
						<span class="text-cyan-400">1</span> 0 0 0{" "}
						<span class="text-slate-600">]</span>
					</div>
					<div class="text-slate-400">
						D <span class="text-slate-600">[</span> 0 0 0 0{" "}
						<span class="text-slate-600">]</span>
					</div>
				</div>
				<div class="flex-1 space-y-2 text-sm">
					<div class="flex items-center gap-2">
						<span class="text-red-400 font-mono">100K × 100K</span>
						<span class="text-slate-500">=</span>
						<span class="text-red-400 font-bold">10 billion</span>
						<span class="text-slate-400">entries</span>
					</div>
					<div class="flex items-center gap-2">
						<span class="text-slate-400">Actual edges:</span>
						<span class="text-cyan-400 font-mono">1M</span>
						<span class="text-slate-500">(0.01% used)</span>
					</div>
					<div class="mt-3 px-3 py-2 rounded-lg bg-red-500/10 border border-red-500/20 text-red-400 text-xs">
						Wastes 99.99% of space
					</div>
				</div>
			</div>
		</div>
	);
}

// Linked List problem visualization
function LinkedListProblem() {
	return (
		<div class="my-6 rounded-xl border border-amber-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<div class="flex items-center gap-2 mb-4">
				<svg
					class="w-5 h-5 text-amber-400"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
				>
					<path
						stroke-linecap="round"
						stroke-linejoin="round"
						d="M13 10V3L4 14h7v7l9-11h-7z"
					/>
				</svg>
				<h4 class="font-semibold text-amber-400">Linked Adjacency Lists</h4>
			</div>

			{/* Linked list visualization */}
			<div class="space-y-2 font-mono text-sm mb-4">
				<div class="flex items-center gap-2">
					<span class="text-cyan-400 w-4">A</span>
					<span class="text-slate-500">→</span>
					<span class="px-2 py-0.5 rounded bg-slate-700 text-slate-300">B</span>
					<span class="text-slate-500">→</span>
					<span class="px-2 py-0.5 rounded bg-slate-700 text-slate-300">C</span>
					<span class="text-slate-500">→</span>
					<span class="text-slate-600">null</span>
				</div>
				<div class="flex items-center gap-2">
					<span class="text-cyan-400 w-4">B</span>
					<span class="text-slate-500">→</span>
					<span class="px-2 py-0.5 rounded bg-slate-700 text-slate-300">D</span>
					<span class="text-slate-500">→</span>
					<span class="text-slate-600">null</span>
				</div>
			</div>

			<div class="space-y-2 text-sm">
				<p class="text-slate-400">
					<span class="text-amber-400 font-semibold">Problem:</span> Pointer
					chasing. Each lookup goes to random memory.
				</p>
				<div class="flex flex-wrap gap-4 text-xs">
					<div class="flex items-center gap-2">
						<span class="text-red-400">Cache miss:</span>
						<span class="text-slate-300 font-mono">~100ns</span>
					</div>
					<div class="flex items-center gap-2">
						<span class="text-emerald-400">Cache hit:</span>
						<span class="text-slate-300 font-mono">~1ns</span>
					</div>
				</div>
				<div class="mt-3 px-3 py-2 rounded-lg bg-amber-500/10 border border-amber-500/20 text-amber-400 text-xs">
					1000 edges × 100ns = 100μs wasted waiting for RAM
				</div>
			</div>
		</div>
	);
}

// CSR Solution visualization
function CSRSolutionDiagram() {
	return (
		<div class="my-6 rounded-xl border border-emerald-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<div class="flex items-center gap-2 mb-4">
				<svg
					class="w-5 h-5 text-emerald-400"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
				>
					<path
						stroke-linecap="round"
						stroke-linejoin="round"
						d="M9 12l2 2 4-4m6 2a9 9 0 11-18 0 9 9 0 0118 0z"
					/>
				</svg>
				<h4 class="font-semibold text-emerald-400">CSR Solution</h4>
			</div>

			{/* Graph */}
			<div class="mb-6 p-3 rounded-lg bg-slate-800/50 border border-slate-700">
				<div class="text-xs text-slate-500 mb-2">Graph:</div>
				<div class="flex flex-wrap gap-x-6 gap-y-1 font-mono text-sm">
					<span>
						<span class="text-cyan-400">A</span>{" "}
						<span class="text-slate-500">→</span> B, C
					</span>
					<span>
						<span class="text-cyan-400">B</span>{" "}
						<span class="text-slate-500">→</span> D
					</span>
					<span>
						<span class="text-cyan-400">C</span>{" "}
						<span class="text-slate-500">→</span> A
					</span>
					<span>
						<span class="text-cyan-400">D</span>{" "}
						<span class="text-slate-500">→</span>{" "}
						<span class="text-slate-600">(none)</span>
					</span>
				</div>
			</div>

			{/* Step by step */}
			<div class="space-y-6">
				{/* Step 1 - Destinations */}
				<div class="flex gap-3">
					<span class="flex items-center justify-center w-6 h-6 rounded-full bg-cyan-500/20 text-cyan-400 text-xs font-bold shrink-0 mt-0.5">
						1
					</span>
					<div>
						<div class="text-sm text-slate-400 mb-3">
							Concatenate all destinations:
						</div>
						<div class="flex items-center font-mono text-sm">
							<span class="text-violet-400 mr-3">destinations</span>
							<span class="text-slate-500">=</span>
							{/* Boxes with bracket labels below */}
							<div class="ml-3">
								<div class="flex">
									<div class="w-10 py-1 text-center rounded-l bg-cyan-500/20 text-cyan-400 border-r border-cyan-500/30">
										B
									</div>
									<div class="w-10 py-1 text-center bg-cyan-500/20 text-cyan-400 border-r border-cyan-500/30">
										C
									</div>
									<div class="w-10 py-1 text-center bg-violet-500/20 text-violet-400 border-r border-violet-500/30">
										D
									</div>
									<div class="w-10 py-1 text-center rounded-r bg-emerald-500/20 text-emerald-400">
										A
									</div>
								</div>
								{/* Bracket labels using borders for cleaner look */}
								<div class="flex mt-1">
									{/* A's edges (B, C) - spans 2 boxes */}
									<div class="w-20 flex flex-col items-center">
										<div class="w-full h-2 border-l border-r border-b border-slate-500 rounded-b-sm" />
										<span class="text-xs text-cyan-400 mt-0.5">A</span>
									</div>
									{/* B's edge (D) */}
									<div class="w-10 flex flex-col items-center">
										<div class="w-4 h-2 border-l border-r border-b border-slate-500 rounded-b-sm" />
										<span class="text-xs text-violet-400 mt-0.5">B</span>
									</div>
									{/* C's edge (A) */}
									<div class="w-10 flex flex-col items-center">
										<div class="w-4 h-2 border-l border-r border-b border-slate-500 rounded-b-sm" />
										<span class="text-xs text-emerald-400 mt-0.5">C</span>
									</div>
								</div>
							</div>
						</div>
					</div>
				</div>

				{/* Step 2 - Offsets */}
				<div class="flex gap-3">
					<span class="flex items-center justify-center w-6 h-6 rounded-full bg-cyan-500/20 text-cyan-400 text-xs font-bold shrink-0 mt-0.5">
						2
					</span>
					<div>
						<div class="text-sm text-slate-400 mb-3">
							Record where each node's edges start:
						</div>
						<div class="flex items-center font-mono text-sm">
							<span class="text-violet-400 mr-3">offsets</span>
							<span class="text-slate-500">=</span>
							{/* Boxes with arrows and labels below - each column aligned */}
							<div class="ml-3">
								<div class="flex">
									<div class="w-10 py-1 text-center rounded-l bg-slate-700 text-slate-300 border-r border-slate-600">
										0
									</div>
									<div class="w-10 py-1 text-center bg-slate-700 text-slate-300 border-r border-slate-600">
										2
									</div>
									<div class="w-10 py-1 text-center bg-slate-700 text-slate-300 border-r border-slate-600">
										3
									</div>
									<div class="w-10 py-1 text-center bg-slate-700 text-slate-300 border-r border-slate-600">
										4
									</div>
									<div class="w-10 py-1 text-center rounded-r bg-slate-700 text-slate-300">
										4
									</div>
								</div>
								<div class="flex text-xs text-slate-500 mt-0.5">
									<div class="w-10 text-center">↑</div>
									<div class="w-10 text-center">↑</div>
									<div class="w-10 text-center">↑</div>
									<div class="w-10 text-center">↑</div>
									<div class="w-10 text-center">↑</div>
								</div>
								<div class="flex text-xs text-slate-500">
									<div class="w-10 text-center">A</div>
									<div class="w-10 text-center">B</div>
									<div class="w-10 text-center">C</div>
									<div class="w-10 text-center">D</div>
									<div class="w-10 text-center text-slate-400">end</div>
								</div>
							</div>
						</div>
					</div>
				</div>
			</div>
		</div>
	);
}

// CSR Traversal example
function CSRTraversalExample() {
	return (
		<div class="my-6 space-y-3">
			{/* Example 1 */}
			<div class="rounded-xl border border-cyan-500/30 bg-slate-800/50 p-4">
				<div class="text-sm text-slate-400 mb-2">
					"Who does <span class="text-cyan-400 font-semibold">A</span> connect
					to?"
				</div>
				<div class="space-y-1 font-mono text-sm">
					<div class="text-slate-400">
						start = offsets[<span class="text-cyan-400">0</span>] ={" "}
						<span class="text-emerald-400">0</span>
					</div>
					<div class="text-slate-400">
						end = offsets[<span class="text-cyan-400">1</span>] ={" "}
						<span class="text-emerald-400">2</span>
					</div>
					<div class="text-slate-400">
						destinations[<span class="text-emerald-400">0</span>:
						<span class="text-emerald-400">2</span>] ={" "}
						<span class="text-cyan-400">[B, C]</span>{" "}
						<span class="text-emerald-400">✓</span>
					</div>
				</div>
			</div>

			{/* Example 2 */}
			<div class="rounded-xl border border-slate-600/50 bg-slate-800/50 p-4">
				<div class="text-sm text-slate-400 mb-2">
					"Who does <span class="text-cyan-400 font-semibold">D</span> connect
					to?"
				</div>
				<div class="space-y-1 font-mono text-sm">
					<div class="text-slate-400">
						start = offsets[<span class="text-cyan-400">3</span>] ={" "}
						<span class="text-emerald-400">4</span>
					</div>
					<div class="text-slate-400">
						end = offsets[<span class="text-cyan-400">4</span>] ={" "}
						<span class="text-emerald-400">4</span>
					</div>
					<div class="text-slate-400">
						destinations[<span class="text-emerald-400">4</span>:
						<span class="text-emerald-400">4</span>] ={" "}
						<span class="text-slate-500">[]</span>{" "}
						<span class="text-slate-500">(no edges)</span>{" "}
						<span class="text-emerald-400">✓</span>
					</div>
				</div>
			</div>

			{/* Algorithm */}
			<div class="rounded-xl border border-violet-500/30 bg-slate-800/50 p-4">
				<div class="text-xs text-violet-400 font-semibold mb-2">Algorithm:</div>
				<div class="font-mono text-sm text-slate-300">
					<div>start = offsets[node]</div>
					<div>end = offsets[node + 1]</div>
					<div class="text-emerald-400">return destinations[start:end]</div>
				</div>
			</div>
		</div>
	);
}

// Memory layout comparison
function MemoryLayoutComparison() {
	return (
		<div class="my-6 grid sm:grid-cols-2 gap-4">
			{/* Linked List - Bad */}
			<div class="rounded-xl border border-red-500/30 bg-slate-800/50 p-4">
				<div class="flex items-center gap-2 mb-4">
					<span class="text-red-400 text-xs font-semibold">LINKED LIST</span>
					<span class="text-slate-600">—</span>
					<span class="text-slate-500 text-xs">scattered</span>
				</div>
				{/* Boxes with addresses aligned below each */}
				<div class="flex gap-6 mb-1">
					<div class="w-10 px-3 py-2 rounded bg-slate-700 text-slate-300 font-mono text-sm text-center">
						B
					</div>
					<div class="w-10 px-3 py-2 rounded bg-slate-700 text-slate-300 font-mono text-sm text-center">
						C
					</div>
					<div class="w-10 px-3 py-2 rounded bg-slate-700 text-slate-300 font-mono text-sm text-center">
						D
					</div>
				</div>
				<div class="flex gap-6 mb-3">
					<div class="w-10 text-center text-xs font-mono text-red-400/70">
						0x1000
					</div>
					<div class="w-10 text-center text-xs font-mono text-red-400/70">
						0x5F00
					</div>
					<div class="w-10 text-center text-xs font-mono text-red-400/70">
						0x2A00
					</div>
				</div>
				<div class="text-xs text-red-400">
					↑ Random locations = cache misses
				</div>
			</div>

			{/* CSR - Good */}
			<div class="rounded-xl border border-emerald-500/30 bg-slate-800/50 p-4">
				<div class="flex items-center gap-2 mb-4">
					<span class="text-emerald-400 text-xs font-semibold">CSR</span>
					<span class="text-slate-600">—</span>
					<span class="text-slate-500 text-xs">contiguous</span>
				</div>
				{/* Connected boxes with addresses aligned below each */}
				<div class="flex mb-1">
					<div class="w-10 py-2 rounded-l bg-cyan-500/20 text-cyan-400 font-mono text-sm text-center border-r border-cyan-500/30">
						B
					</div>
					<div class="w-10 py-2 bg-cyan-500/20 text-cyan-400 font-mono text-sm text-center border-r border-cyan-500/30">
						C
					</div>
					<div class="w-10 py-2 bg-cyan-500/20 text-cyan-400 font-mono text-sm text-center border-r border-cyan-500/30">
						D
					</div>
					<div class="w-10 py-2 rounded-r bg-cyan-500/20 text-cyan-400 font-mono text-sm text-center">
						A
					</div>
				</div>
				<div class="flex mb-3">
					<div class="w-10 text-center text-xs font-mono text-emerald-400/70">
						0x1000
					</div>
					<div class="w-10 text-center text-xs font-mono text-emerald-400/70">
						+4
					</div>
					<div class="w-10 text-center text-xs font-mono text-emerald-400/70">
						+8
					</div>
					<div class="w-10 text-center text-xs font-mono text-emerald-400/70">
						+C
					</div>
				</div>
				<div class="text-xs text-emerald-400">
					↑ Sequential = CPU prefetcher works
				</div>
			</div>
		</div>
	);
}

// Bidirectional edges visualization
function BidirectionalEdges() {
	return (
		<div class="my-6 rounded-xl border border-violet-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<div class="grid sm:grid-cols-2 gap-4">
				{/* Out-edges */}
				<div class="p-3 rounded-lg bg-slate-800/50 border border-cyan-500/20">
					<div class="text-xs text-cyan-400 font-semibold mb-2">
						Out-edges (A → B)
					</div>
					<div class="space-y-1 font-mono text-xs">
						<div class="text-slate-400">
							out_offsets = <span class="text-slate-300">[0, 2, 3, 4, 4]</span>
						</div>
						<div class="text-slate-400">
							out_dst = <span class="text-cyan-400">[B, C, D, A]</span>
						</div>
					</div>
					<div class="mt-2 text-xs text-slate-500">
						"Who does Alice follow?"
					</div>
				</div>

				{/* In-edges */}
				<div class="p-3 rounded-lg bg-slate-800/50 border border-violet-500/20">
					<div class="text-xs text-violet-400 font-semibold mb-2">
						In-edges (A ← C)
					</div>
					<div class="space-y-1 font-mono text-xs">
						<div class="text-slate-400">
							in_offsets = <span class="text-slate-300">[0, 1, 2, 3, 4]</span>
						</div>
						<div class="text-slate-400">
							in_src = <span class="text-violet-400">[C, A, A, B]</span>
						</div>
					</div>
					<div class="mt-2 text-xs text-slate-500">"Who follows Alice?"</div>
				</div>
			</div>

			<div class="mt-4 px-3 py-2 rounded-lg bg-slate-700/30 text-xs text-slate-400">
				<span class="text-amber-400">Trade-off:</span> 2× storage, but O(1)
				traversal in both directions
			</div>
		</div>
	);
}

// Edge types visualization
function EdgeTypesSorting() {
	return (
		<div class="my-6 rounded-xl border border-cyan-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<div class="mb-4">
				<div class="font-mono text-sm space-y-1">
					<div class="text-slate-400">
						out_dst = <span class="text-slate-600">[</span>
						<span class="text-cyan-400">B</span>,{" "}
						<span class="text-cyan-400">C</span>,{" "}
						<span class="text-cyan-400">D</span>,{" "}
						<span class="text-cyan-400">A</span>
						<span class="text-slate-600">]</span>
					</div>
					<div class="text-slate-400">
						out_etype = <span class="text-slate-600">[</span>
						<span class="text-emerald-400">0</span>,{" "}
						<span class="text-violet-400">1</span>,{" "}
						<span class="text-emerald-400">0</span>,{" "}
						<span class="text-emerald-400">0</span>
						<span class="text-slate-600">]</span>
					</div>
				</div>
				<div class="flex gap-4 mt-2 text-xs">
					<div class="flex items-center gap-1">
						<span class="w-2 h-2 rounded-full bg-emerald-400" />
						<span class="text-slate-400">0 = KNOWS</span>
					</div>
					<div class="flex items-center gap-1">
						<span class="w-2 h-2 rounded-full bg-violet-400" />
						<span class="text-slate-400">1 = LIKES</span>
					</div>
				</div>
			</div>

			<div class="text-xs text-slate-500 mb-3">
				Sorted by (etype, dst) within each node:
			</div>

			<div class="space-y-2">
				<div class="flex items-center gap-2 text-sm">
					<span class="w-4 h-4 rounded bg-emerald-500/20 text-emerald-400 flex items-center justify-center text-xs">
						1
					</span>
					<span class="text-slate-300">
						Binary search to find specific edge type
					</span>
				</div>
				<div class="flex items-center gap-2 text-sm">
					<span class="w-4 h-4 rounded bg-emerald-500/20 text-emerald-400 flex items-center justify-center text-xs">
						2
					</span>
					<span class="text-slate-300">
						Early termination when past desired type
					</span>
				</div>
				<div class="flex items-center gap-2 text-sm">
					<span class="w-4 h-4 rounded bg-emerald-500/20 text-emerald-400 flex items-center justify-center text-xs">
						3
					</span>
					<span class="text-slate-300">
						"Get A's KNOWS edges" doesn't scan all
					</span>
				</div>
			</div>
		</div>
	);
}

// ============================================================================
// SINGLE-FILE FORMAT COMPONENTS
// ============================================================================

// File layout diagram
function FileLayoutDiagram() {
	return (
		<div class="my-6 rounded-xl border border-cyan-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<div class="space-y-0">
				{/* Header */}
				<div class="rounded-t-lg border-2 border-cyan-500/40 bg-cyan-500/10 p-4">
					<div class="flex items-center justify-between">
						<div class="flex items-center gap-3">
							<svg
								class="w-5 h-5 text-cyan-400"
								viewBox="0 0 24 24"
								fill="none"
								stroke="currentColor"
								stroke-width="2"
							>
								<path
									stroke-linecap="round"
									stroke-linejoin="round"
									d="M9 12h6m-6 4h6m2 5H7a2 2 0 01-2-2V5a2 2 0 012-2h5.586a1 1 0 01.707.293l5.414 5.414a1 1 0 01.293.707V19a2 2 0 01-2 2z"
								/>
							</svg>
							<span class="font-semibold text-cyan-400">Header</span>
						</div>
						<span class="text-xs text-cyan-400/70 font-mono">4 KB</span>
					</div>
					<p class="text-xs text-slate-400 mt-1 ml-8">
						Database metadata, pointers, checksums
					</p>
				</div>

				{/* WAL Area */}
				<div class="border-2 border-t-0 border-violet-500/40 bg-violet-500/5 p-4">
					<div class="flex items-center justify-between mb-3">
						<div class="flex items-center gap-3">
							<svg
								class="w-5 h-5 text-violet-400"
								viewBox="0 0 24 24"
								fill="none"
								stroke="currentColor"
								stroke-width="2"
							>
								<path
									stroke-linecap="round"
									stroke-linejoin="round"
									d="M4 4v5h.582m15.356 2A8.001 8.001 0 004.582 9m0 0H9m11 11v-5h-.581m0 0a8.003 8.003 0 01-15.357-2m15.357 2H15"
								/>
							</svg>
							<span class="font-semibold text-violet-400">WAL Area</span>
						</div>
						<span class="text-xs text-violet-400/70 font-mono">~64 MB</span>
					</div>
					{/* Inner regions */}
					<div class="ml-8 space-y-2">
						<div class="rounded border border-violet-500/30 bg-violet-500/10 px-3 py-2 flex justify-between items-center">
							<span class="text-sm text-violet-300">Primary Region</span>
							<span class="text-xs text-slate-500">75% — normal writes</span>
						</div>
						<div class="rounded border border-violet-500/20 bg-violet-500/5 px-3 py-2 flex justify-between items-center">
							<span class="text-sm text-violet-300/70">Secondary Region</span>
							<span class="text-xs text-slate-500">
								25% — during checkpoint
							</span>
						</div>
					</div>
				</div>

				{/* Snapshot Area */}
				<div class="rounded-b-lg border-2 border-t-0 border-emerald-500/40 bg-emerald-500/5 p-4">
					<div class="flex items-center justify-between">
						<div class="flex items-center gap-3">
							<svg
								class="w-5 h-5 text-emerald-400"
								viewBox="0 0 24 24"
								fill="none"
								stroke="currentColor"
								stroke-width="2"
							>
								<path
									stroke-linecap="round"
									stroke-linejoin="round"
									d="M4 7v10c0 2.21 3.582 4 8 4s8-1.79 8-4V7M4 7c0 2.21 3.582 4 8 4s8-1.79 8-4M4 7c0-2.21 3.582-4 8-4s8 1.79 8 4"
								/>
							</svg>
							<span class="font-semibold text-emerald-400">Snapshot Area</span>
						</div>
						<span class="text-xs text-emerald-400/70 font-mono">grows</span>
					</div>
					<p class="text-xs text-slate-400 mt-1 ml-8">
						CSR data, compressed with zstd
					</p>
				</div>
			</div>
		</div>
	);
}

// Header contents visualization
function HeaderContents() {
	return (
		<div class="my-6 rounded-xl border border-cyan-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<div class="flex items-center gap-2 mb-4">
				<svg
					class="w-5 h-5 text-cyan-400"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
				>
					<path
						stroke-linecap="round"
						stroke-linejoin="round"
						d="M9 12h6m-6 4h6m2 5H7a2 2 0 01-2-2V5a2 2 0 012-2h5.586a1 1 0 01.707.293l5.414 5.414a1 1 0 01.293.707V19a2 2 0 01-2 2z"
					/>
				</svg>
				<h4 class="font-semibold text-cyan-400">Header Contents</h4>
				<span class="text-xs text-slate-500 ml-auto">
					offset 0, always 4 KB
				</span>
			</div>
			<div class="space-y-1.5">
				<HeaderRow
					connector="├"
					name="Magic bytes"
					value={`"KITE" + version`}
				/>
				<HeaderRow connector="├" name="Page size" value="4096 (default)" />
				<HeaderRow
					connector="├"
					name="Snapshot location"
					value="Start page, page count"
				/>
				<HeaderRow
					connector="├"
					name="WAL location"
					value="Start page, page count"
				/>
				<HeaderRow
					connector="├"
					name="WAL pointers"
					value="Head and tail positions"
				/>
				<HeaderRow
					connector="├"
					name="Counters"
					value="Max node ID, next tx ID"
				/>
				<HeaderRow
					connector="├"
					name="Snapshot generation"
					value="Incremented on checkpoint"
				/>
				<HeaderRow
					connector="└"
					name="Checksums"
					value="CRC32C of header data"
				/>
			</div>
		</div>
	);
}

function HeaderRow(props: { connector: string; name: string; value: string }) {
	return (
		<div class="flex items-center gap-3 text-sm">
			<span class="text-slate-600 font-mono">{props.connector}</span>
			<span class="text-cyan-400 font-medium w-36">{props.name}</span>
			<span class="text-slate-400">{props.value}</span>
		</div>
	);
}

// Atomic checkpoint process
function AtomicCheckpointProcess() {
	return (
		<div class="my-6 rounded-xl border border-emerald-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<h4 class="text-sm font-semibold text-slate-400 mb-4">
				Checkpoint Process
			</h4>

			<div class="relative">
				{/* Vertical line */}
				<div class="absolute left-3 top-3 bottom-16 w-px bg-gradient-to-b from-cyan-500/50 via-violet-500/50 to-emerald-500/50" />

				<div class="space-y-3">
					<CheckpointAtomicStep
						num={1}
						text="Write new snapshot to free space at end of file"
					/>
					<CheckpointAtomicStep
						num={2}
						text="fsync() to ensure snapshot is durable"
					/>
					<CheckpointAtomicStep
						num={3}
						text="Update header with new snapshot location"
					/>
					<CheckpointAtomicStep num={4} text="fsync() header" highlight />
					<CheckpointAtomicStep
						num={5}
						text="Old snapshot space becomes free"
					/>
				</div>
			</div>

			{/* Crash recovery note */}
			<div class="mt-5 pt-4 border-t border-slate-700/50 space-y-2">
				<p class="text-xs text-slate-500 font-medium">If crash occurs:</p>
				<div class="grid grid-cols-2 gap-3 text-xs">
					<div class="px-3 py-2 rounded bg-amber-500/10 border border-amber-500/20">
						<span class="text-amber-400">Before step 4:</span>
						<span class="text-slate-400 ml-1">Old snapshot valid</span>
					</div>
					<div class="px-3 py-2 rounded bg-emerald-500/10 border border-emerald-500/20">
						<span class="text-emerald-400">After step 4:</span>
						<span class="text-slate-400 ml-1">New snapshot valid</span>
					</div>
				</div>
				<p class="text-xs text-slate-500 italic">
					No intermediate state is possible.
				</p>
			</div>
		</div>
	);
}

function CheckpointAtomicStep(props: {
	num: number;
	text: string;
	highlight?: boolean;
}) {
	const bgColor = () => {
		if (props.highlight) return "bg-emerald-500/20 text-emerald-400";
		if (props.num <= 2) return "bg-cyan-500/20 text-cyan-400";
		if (props.num <= 4) return "bg-violet-500/20 text-violet-400";
		return "bg-slate-500/20 text-slate-400";
	};
	return (
		<div class="flex items-center gap-3 relative">
			<span
				class={`flex items-center justify-center w-6 h-6 rounded-full text-xs font-bold shrink-0 z-10 ${bgColor()}`}
			>
				{props.num}
			</span>
			<span
				class={`text-sm ${props.highlight ? "text-emerald-300 font-medium" : "text-slate-300"}`}
			>
				{props.text}
			</span>
		</div>
	);
}

// WAL dual region diagram
function WALDualRegion() {
	return (
		<div class="my-6 rounded-xl border border-violet-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<div class="flex items-center justify-between mb-4">
				<h4 class="font-semibold text-violet-400">WAL Area</h4>
				<span class="text-xs text-slate-500 font-mono">64 MB example</span>
			</div>

			{/* Visual representation */}
			<div class="mb-4">
				<div class="flex rounded-lg overflow-hidden border border-violet-500/30">
					<div class="w-3/4 bg-violet-500/20 p-3 border-r border-violet-500/30">
						<div class="text-sm font-medium text-violet-300">Primary</div>
						<div class="text-xs text-slate-400">48 MB</div>
					</div>
					<div class="w-1/4 bg-violet-500/10 p-3">
						<div class="text-sm font-medium text-violet-300/70">Secondary</div>
						<div class="text-xs text-slate-400">16 MB</div>
					</div>
				</div>
			</div>

			{/* Explanation */}
			<div class="space-y-2 text-sm">
				<p class="text-slate-400">
					<span class="text-violet-400 font-medium">Why two regions?</span>
				</p>
				<ul class="space-y-1 text-slate-400 ml-4">
					<li class="flex items-start gap-2">
						<span class="text-violet-400 mt-1">•</span>
						<span>Checkpoint reads primary to build new snapshot</span>
					</li>
					<li class="flex items-start gap-2">
						<span class="text-violet-400 mt-1">•</span>
						<span>Concurrent transactions write to secondary</span>
					</li>
					<li class="flex items-start gap-2">
						<span class="text-emerald-400 mt-1">→</span>
						<span class="text-emerald-400">
							No blocking between reads and writes
						</span>
					</li>
				</ul>
			</div>
		</div>
	);
}

// Snapshot sections tree
function SnapshotSections() {
	return (
		<div class="my-6 rounded-xl border border-emerald-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<div class="flex items-center gap-2 mb-4">
				<svg
					class="w-5 h-5 text-emerald-400"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
				>
					<path
						stroke-linecap="round"
						stroke-linejoin="round"
						d="M4 7v10c0 2.21 3.582 4 8 4s8-1.79 8-4V7M4 7c0 2.21 3.582 4 8 4s8-1.79 8-4M4 7c0-2.21 3.582-4 8-4s8 1.79 8 4"
					/>
				</svg>
				<h4 class="font-semibold text-emerald-400">Snapshot Sections</h4>
			</div>
			<div class="space-y-1.5">
				<SnapshotRow
					connector="├"
					name="Node ID mappings"
					desc="Physical ↔ Logical ID translation"
				/>
				<SnapshotRow
					connector="├"
					name="Out-edge CSR"
					desc="offsets[], destinations[], edge_types[]"
					code
				/>
				<SnapshotRow
					connector="├"
					name="In-edge CSR"
					desc="offsets[], sources[], edge_types[]"
					code
				/>
				<SnapshotRow
					connector="├"
					name="Properties"
					desc="Node and edge property values"
				/>
				<SnapshotRow
					connector="├"
					name="String table"
					desc="Deduplicated string storage"
				/>
				<SnapshotRow
					connector="├"
					name="Key index"
					desc="Hash-bucketed node key lookups"
				/>
				<SnapshotRow
					connector="└"
					name="Schema"
					desc="Labels, edge types, property keys"
				/>
			</div>
			<div class="mt-4 pt-3 border-t border-slate-700/50 text-xs text-slate-500">
				Each section independently compressed (zstd). Typical ratio: 40-60% of
				raw size.
			</div>
		</div>
	);
}

function SnapshotRow(props: {
	connector: string;
	name: string;
	desc: string;
	code?: boolean;
}) {
	return (
		<div class="flex items-center gap-3 text-sm">
			<span class="text-slate-600 font-mono">{props.connector}</span>
			<span class="text-emerald-400 font-medium w-32">{props.name}</span>
			<span
				class={
					props.code ? "text-slate-500 font-mono text-xs" : "text-slate-400"
				}
			>
				{props.desc}
			</span>
		</div>
	);
}

// File growth visualization
function FileGrowthDiagram() {
	return (
		<div class="my-6 rounded-xl border border-slate-600/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<h4 class="text-sm font-semibold text-slate-400 mb-4">
				File Size Examples
			</h4>

			<div class="space-y-4">
				{/* Initial */}
				<div class="flex items-center gap-4">
					<div class="w-20 text-xs text-slate-500">Initial</div>
					<div class="flex-1 flex items-center gap-1">
						<div class="h-6 w-1 bg-cyan-500/60 rounded" title="Header 4KB" />
						<div class="h-6 flex-1 bg-violet-500/40 rounded" title="WAL 64MB" />
						<div
							class="h-6 w-1 bg-emerald-500/40 rounded"
							title="Empty snapshot"
						/>
					</div>
					<div class="w-20 text-right text-xs text-slate-400 font-mono">
						~64 MB
					</div>
				</div>

				{/* 100K nodes */}
				<div class="flex items-center gap-4">
					<div class="w-20 text-xs text-slate-500">100K nodes</div>
					<div class="flex-1 flex items-center gap-1">
						<div class="h-6 w-1 bg-cyan-500/60 rounded" />
						<div class="h-6 w-3/4 bg-violet-500/40 rounded" />
						<div class="h-6 w-1/5 bg-emerald-500/60 rounded" />
					</div>
					<div class="w-20 text-right text-xs text-slate-400 font-mono">
						~72 MB
					</div>
				</div>

				{/* 1M nodes */}
				<div class="flex items-center gap-4">
					<div class="w-20 text-xs text-slate-500">1M nodes</div>
					<div class="flex-1 flex items-center gap-1">
						<div class="h-6 w-1 bg-cyan-500/60 rounded" />
						<div class="h-6 w-1/2 bg-violet-500/40 rounded" />
						<div class="h-6 w-2/5 bg-emerald-500/60 rounded" />
					</div>
					<div class="w-20 text-right text-xs text-slate-400 font-mono">
						~150 MB
					</div>
				</div>
			</div>

			{/* Legend */}
			<div class="mt-4 pt-3 border-t border-slate-700/50 flex flex-wrap gap-4 text-xs">
				<div class="flex items-center gap-1.5">
					<div class="w-2 h-2 rounded-sm bg-cyan-500/60" />
					<span class="text-slate-500">Header (fixed)</span>
				</div>
				<div class="flex items-center gap-1.5">
					<div class="w-2 h-2 rounded-sm bg-violet-500/40" />
					<span class="text-slate-500">WAL (configurable)</span>
				</div>
				<div class="flex items-center gap-1.5">
					<div class="w-2 h-2 rounded-sm bg-emerald-500/60" />
					<span class="text-slate-500">Snapshot (grows)</span>
				</div>
			</div>
		</div>
	);
}

// Database open process
function DatabaseOpenProcess() {
	return (
		<div class="my-6 rounded-xl border border-cyan-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<h4 class="text-sm font-semibold text-slate-400 mb-4">
				Opening a Database
			</h4>

			<div class="relative">
				{/* Vertical line */}
				<div class="absolute left-3 top-3 bottom-12 w-px bg-gradient-to-b from-cyan-500/50 via-violet-500/50 to-emerald-500/50" />

				<div class="space-y-2.5">
					<OpenStep num={1} text="Read header (4 KB at offset 0)" />
					<OpenStep num={2} text="Validate magic bytes and checksums" />
					<OpenStep num={3} text="mmap() snapshot area (zero-copy)" />
					<OpenStep num={4} text="Parse snapshot sections" />
					<OpenStep num={5} text="Replay WAL to rebuild delta" />
					<OpenStep num={6} text="Ready for queries" success />
				</div>
			</div>

			{/* Recovery note */}
			<div class="mt-4 pt-3 border-t border-slate-700/50">
				<p class="text-xs text-slate-500">
					<span class="text-amber-400">
						If WAL replay finds incomplete transaction:
					</span>
					<span class="text-slate-400 ml-1">
						Discard it (never committed). Recovery is automatic and fast.
					</span>
				</p>
			</div>
		</div>
	);
}

function OpenStep(props: { num: number; text: string; success?: boolean }) {
	const bgColor = () => {
		if (props.success) return "bg-emerald-500/20 text-emerald-400";
		if (props.num <= 2) return "bg-cyan-500/20 text-cyan-400";
		if (props.num <= 4) return "bg-violet-500/20 text-violet-400";
		return "bg-slate-500/20 text-slate-400";
	};
	return (
		<div class="flex items-center gap-3 relative">
			<span
				class={`flex items-center justify-center w-6 h-6 rounded-full text-xs font-bold shrink-0 z-10 ${bgColor()}`}
			>
				{props.success ? "✓" : props.num}
			</span>
			<span
				class={`text-sm ${props.success ? "text-emerald-400 font-medium" : "text-slate-300"}`}
			>
				{props.text}
			</span>
		</div>
	);
}

// ============================================================================
// WAL & DURABILITY COMPONENTS
// ============================================================================

// WAL Principle diagram
function WALPrincipleDiagram() {
	return (
		<div class="my-6 rounded-xl border border-emerald-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<div class="flex items-center gap-2 mb-4">
				<svg
					class="w-5 h-5 text-emerald-400"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
				>
					<path
						stroke-linecap="round"
						stroke-linejoin="round"
						d="M9 12l2 2 4-4m5.618-4.016A11.955 11.955 0 0112 2.944a11.955 11.955 0 01-8.618 3.04A12.02 12.02 0 003 9c0 5.591 3.824 10.29 9 11.622 5.176-1.332 9-6.03 9-11.622 0-1.042-.133-2.052-.382-3.016z"
					/>
				</svg>
				<h4 class="font-semibold text-emerald-400">Rule: Log before you do</h4>
			</div>

			<div class="space-y-2 mb-5">
				<WALStep num={1} text="Write all changes to WAL" />
				<WALStep
					num={2}
					text="fsync() WAL to disk"
					highlight
					note="Data is now durable"
				/>
				<WALStep
					num={3}
					text="Update in-memory delta"
					note="Data is now visible"
				/>
				<WALStep num={4} text="Return success to caller" />
			</div>

			{/* Crash scenarios */}
			<div class="pt-4 border-t border-slate-700/50 space-y-2">
				<div class="flex items-start gap-3 text-sm">
					<span class="px-2 py-0.5 rounded bg-emerald-500/20 text-emerald-400 text-xs font-medium shrink-0">
						After step 2
					</span>
					<span class="text-slate-400">
						Replay WAL on restart → changes recovered
					</span>
				</div>
				<div class="flex items-start gap-3 text-sm">
					<span class="px-2 py-0.5 rounded bg-amber-500/20 text-amber-400 text-xs font-medium shrink-0">
						Before step 2
					</span>
					<span class="text-slate-400">
						Changes lost, but OK (transaction didn't commit)
					</span>
				</div>
			</div>
		</div>
	);
}

function WALStep(props: {
	num: number;
	text: string;
	highlight?: boolean;
	note?: string;
}) {
	return (
		<div class="flex items-center gap-3">
			<span
				class={`flex items-center justify-center w-6 h-6 rounded-full text-xs font-bold shrink-0 ${props.highlight ? "bg-emerald-500/30 text-emerald-400" : "bg-slate-700 text-slate-400"}`}
			>
				{props.num}
			</span>
			<span
				class={`text-sm ${props.highlight ? "text-emerald-300 font-medium" : "text-slate-300"}`}
			>
				{props.text}
			</span>
			{props.note && (
				<span class="text-xs text-slate-500 ml-auto">← {props.note}</span>
			)}
		</div>
	);
}

// WAL Record Format visualization
function WALRecordFormat() {
	return (
		<div class="my-6 rounded-xl border border-violet-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<div class="flex items-center gap-2 mb-4">
				<svg
					class="w-5 h-5 text-violet-400"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
				>
					<path
						stroke-linecap="round"
						stroke-linejoin="round"
						d="M9 12h6m-6 4h6m2 5H7a2 2 0 01-2-2V5a2 2 0 012-2h5.586a1 1 0 01.707.293l5.414 5.414a1 1 0 01.293.707V19a2 2 0 01-2 2z"
					/>
				</svg>
				<h4 class="font-semibold text-violet-400">WAL Record Format</h4>
			</div>

			{/* Record structure */}
			<div class="space-y-0 mb-5">
				<div class="rounded-t-lg border-2 border-cyan-500/30 bg-cyan-500/10 px-4 py-2 flex justify-between items-center">
					<span class="text-sm text-cyan-400 font-medium">Length</span>
					<span class="text-xs text-slate-500 font-mono">4 bytes</span>
				</div>
				<div class="border-2 border-t-0 border-violet-500/30 bg-violet-500/5 px-4 py-2">
					<div class="grid grid-cols-2 gap-2 text-sm">
						<div class="flex justify-between">
							<span class="text-violet-300">Type</span>
							<span class="text-xs text-slate-500 font-mono">1 byte</span>
						</div>
						<div class="flex justify-between">
							<span class="text-violet-300">Flags</span>
							<span class="text-xs text-slate-500 font-mono">1 byte</span>
						</div>
						<div class="flex justify-between">
							<span class="text-violet-300">Reserved</span>
							<span class="text-xs text-slate-500 font-mono">2 bytes</span>
						</div>
						<div class="flex justify-between">
							<span class="text-violet-300">TxID</span>
							<span class="text-xs text-slate-500 font-mono">8 bytes</span>
						</div>
					</div>
				</div>
				<div class="border-2 border-t-0 border-slate-600/50 bg-slate-800/50 px-4 py-3 flex justify-between items-center">
					<span class="text-sm text-slate-300">Payload</span>
					<span class="text-xs text-slate-500 font-mono">variable</span>
				</div>
				<div class="rounded-b-lg border-2 border-t-0 border-emerald-500/30 bg-emerald-500/10 px-4 py-2 flex justify-between items-center">
					<span class="text-sm text-emerald-400 font-medium">
						CRC32C + Padding
					</span>
					<span class="text-xs text-slate-500 font-mono">align to 8</span>
				</div>
			</div>

			{/* Record types */}
			<div class="pt-4 border-t border-slate-700/50">
				<p class="text-xs text-slate-500 mb-2">Record Types:</p>
				<div class="flex flex-wrap gap-2">
					<RecordTypeBadge name="BEGIN" color="cyan" />
					<RecordTypeBadge name="COMMIT" color="emerald" />
					<RecordTypeBadge name="ROLLBACK" color="red" />
					<RecordTypeBadge name="CREATE_NODE" color="violet" />
					<RecordTypeBadge name="DELETE_NODE" color="violet" />
					<RecordTypeBadge name="ADD_EDGE" color="violet" />
					<RecordTypeBadge name="DELETE_EDGE" color="violet" />
					<RecordTypeBadge name="SET_NODE_PROP" color="violet" />
					<RecordTypeBadge name="DEL_NODE_PROP" color="violet" />
				</div>
			</div>
		</div>
	);
}

function RecordTypeBadge(props: { name: string; color: string }) {
	const colorClass = () => {
		switch (props.color) {
			case "cyan":
				return "bg-cyan-500/20 text-cyan-400 border-cyan-500/30";
			case "emerald":
				return "bg-emerald-500/20 text-emerald-400 border-emerald-500/30";
			case "red":
				return "bg-red-500/20 text-red-400 border-red-500/30";
			default:
				return "bg-violet-500/20 text-violet-400 border-violet-500/30";
		}
	};
	return (
		<span
			class={`px-2 py-0.5 rounded border text-xs font-mono ${colorClass()}`}
		>
			{props.name}
		</span>
	);
}

// Circular buffer visualization
function CircularBufferDiagram() {
	return (
		<div class="my-6 rounded-xl border border-cyan-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<div class="flex items-center justify-between mb-4">
				<h4 class="font-semibold text-cyan-400">Circular Buffer</h4>
				<span class="text-xs text-slate-500 font-mono">64 MB</span>
			</div>

			{/* Visual buffer */}
			<div class="mb-4">
				<div class="h-8 rounded-lg overflow-hidden flex border border-slate-600">
					<div class="w-1/4 bg-slate-700/50 flex items-center justify-center">
						<span class="text-xs text-slate-500">reclaimed</span>
					</div>
					<div class="w-1/6 bg-cyan-500/30 flex items-center justify-center border-l-2 border-cyan-400">
						<span class="text-xs text-cyan-400 font-medium">TAIL</span>
					</div>
					<div class="flex-1 bg-slate-800 flex items-center justify-center">
						<span class="text-xs text-slate-500">free space</span>
					</div>
					<div class="w-1/4 bg-violet-500/30 flex items-center justify-center border-l-2 border-violet-400">
						<span class="text-xs text-violet-400 font-medium">HEAD</span>
					</div>
				</div>
			</div>

			{/* Legend */}
			<div class="space-y-1.5 text-sm">
				<div class="flex items-center gap-3">
					<span class="text-violet-400 font-medium w-12">HEAD</span>
					<span class="text-slate-400">Where new records are written</span>
				</div>
				<div class="flex items-center gap-3">
					<span class="text-cyan-400 font-medium w-12">TAIL</span>
					<span class="text-slate-400">
						Start of unprocessed records (for replay)
					</span>
				</div>
			</div>

			{/* Note */}
			<div class="mt-4 pt-3 border-t border-slate-700/50">
				<p class="text-xs text-amber-400">
					When HEAD catches up to TAIL → Trigger checkpoint to free space
				</p>
			</div>
		</div>
	);
}

// Dual region explanation (reusing WALDualRegion from single-file but with more detail)
function WALDualRegionDetailed() {
	return (
		<div class="my-6 rounded-xl border border-violet-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<h4 class="font-semibold text-violet-400 mb-4">Why Two Regions?</h4>

			{/* Problem */}
			<div class="mb-4 p-3 rounded-lg bg-slate-800/50 border border-slate-700">
				<p class="text-sm text-slate-400">
					<span class="text-amber-400 font-medium">During checkpoint:</span>
				</p>
				<ul class="mt-2 space-y-1 text-sm text-slate-400 ml-4">
					<li>
						1. Primary region is being <span class="text-cyan-400">READ</span>{" "}
						to build new snapshot
					</li>
					<li>
						2. New transactions need somewhere to{" "}
						<span class="text-violet-400">WRITE</span>
					</li>
				</ul>
			</div>

			{/* Solution visual */}
			<div class="mb-4">
				<div class="flex rounded-lg overflow-hidden border border-violet-500/30">
					<div class="w-3/4 bg-violet-500/20 p-3 border-r border-violet-500/30">
						<div class="text-sm font-medium text-violet-300">Primary (75%)</div>
						<div class="text-xs text-slate-400 mt-1">
							Being read for checkpoint
						</div>
					</div>
					<div class="w-1/4 bg-emerald-500/20 p-3">
						<div class="text-sm font-medium text-emerald-300">
							Secondary (25%)
						</div>
						<div class="text-xs text-slate-400 mt-1">New writes go here</div>
					</div>
				</div>
			</div>

			{/* After checkpoint */}
			<div class="pt-4 border-t border-slate-700/50">
				<p class="text-xs text-slate-500 mb-2">After checkpoint completes:</p>
				<ul class="space-y-1 text-sm text-slate-400">
					<li class="flex items-center gap-2">
						<span class="text-emerald-400">•</span>
						Primary is cleared (data is in new snapshot)
					</li>
					<li class="flex items-center gap-2">
						<span class="text-emerald-400">•</span>
						Secondary becomes the new primary
					</li>
					<li class="flex items-center gap-2">
						<span class="text-emerald-400">→</span>
						<span class="text-emerald-400">
							Writes continue without interruption
						</span>
					</li>
				</ul>
			</div>
		</div>
	);
}

// Durability modes comparison
function DurabilityModes() {
	return (
		<div class="my-6 rounded-xl border border-slate-600/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<h4 class="font-semibold text-slate-400 mb-4">Sync Modes</h4>

			<div class="space-y-3">
				{/* Full */}
				<div class="rounded-lg border border-emerald-500/30 bg-emerald-500/5 p-3">
					<div class="flex items-center justify-between mb-1">
						<span class="font-medium text-emerald-400">full</span>
						<span class="text-xs text-emerald-400/70 px-2 py-0.5 rounded bg-emerald-500/20">
							default
						</span>
					</div>
					<p class="text-sm text-slate-400">fsync every commit</p>
					<p class="text-xs text-slate-500 mt-1">Safest, slower writes</p>
				</div>

				{/* Batch */}
				<div class="rounded-lg border border-amber-500/30 bg-amber-500/5 p-3">
					<div class="flex items-center justify-between mb-1">
						<span class="font-medium text-amber-400">batch</span>
					</div>
					<p class="text-sm text-slate-400">fsync every N commits or T ms</p>
					<p class="text-xs text-slate-500 mt-1">
						Better throughput, small loss window
					</p>
				</div>

				{/* Off */}
				<div class="rounded-lg border border-red-500/30 bg-red-500/5 p-3">
					<div class="flex items-center justify-between mb-1">
						<span class="font-medium text-red-400">off</span>
						<span class="text-xs text-red-400/70 px-2 py-0.5 rounded bg-red-500/20">
							danger
						</span>
					</div>
					<p class="text-sm text-slate-400">No fsync (OS decides)</p>
					<p class="text-xs text-slate-500 mt-1">Fastest, data loss on crash</p>
				</div>
			</div>

			<p class="text-xs text-slate-500 mt-4 pt-3 border-t border-slate-700/50">
				For most applications, <span class="text-emerald-400">full</span> is the
				right choice. Use <span class="text-amber-400">batch</span> for high
				write throughput with acceptable risk.
			</p>
		</div>
	);
}

// Recovery process visualization
function RecoveryProcess() {
	return (
		<div class="my-6 rounded-xl border border-cyan-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<h4 class="font-semibold text-cyan-400 mb-4">Recovery Process</h4>

			<div class="relative">
				{/* Vertical line */}
				<div class="absolute left-3 top-3 bottom-12 w-px bg-gradient-to-b from-cyan-500/50 via-violet-500/50 to-emerald-500/50" />

				<div class="space-y-2.5">
					<RecoveryStep num={1} text="Read header to find WAL boundaries" />
					<RecoveryStep num={2} text="Scan from TAIL to HEAD" />
					<RecoveryStep num={3} text="For each record: validate CRC32C" />
					<RecoveryStep
						num={4}
						text="If valid → apply to delta"
						sub="If invalid → stop (incomplete write)"
					/>
					<RecoveryStep
						num={5}
						text="Handle incomplete transactions"
						sub="BEGIN without COMMIT → discard"
					/>
				</div>
			</div>

			{/* Performance note */}
			<div class="mt-4 pt-3 border-t border-slate-700/50 flex items-center gap-2">
				<svg
					class="w-4 h-4 text-emerald-400"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
				>
					<path
						stroke-linecap="round"
						stroke-linejoin="round"
						d="M13 10V3L4 14h7v7l9-11h-7z"
					/>
				</svg>
				<p class="text-xs text-slate-400">
					Recovery time:{" "}
					<span class="text-emerald-400 font-mono">O(WAL size)</span>, typically
					&lt; 1 second
				</p>
			</div>
		</div>
	);
}

function RecoveryStep(props: { num: number; text: string; sub?: string }) {
	const bgColor = () => {
		if (props.num <= 2) return "bg-cyan-500/20 text-cyan-400";
		if (props.num <= 4) return "bg-violet-500/20 text-violet-400";
		return "bg-emerald-500/20 text-emerald-400";
	};
	return (
		<div class="flex items-start gap-3 relative">
			<span
				class={`flex items-center justify-center w-6 h-6 rounded-full text-xs font-bold shrink-0 z-10 ${bgColor()}`}
			>
				{props.num}
			</span>
			<div>
				<span class="text-sm text-slate-300">{props.text}</span>
				{props.sub && <p class="text-xs text-slate-500 mt-0.5">{props.sub}</p>}
			</div>
		</div>
	);
}

// Checkpoint triggers
function CheckpointTriggers() {
	return (
		<div class="my-6 rounded-xl border border-emerald-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<h4 class="font-semibold text-emerald-400 mb-4">
				When Checkpoint Happens
			</h4>

			{/* Automatic triggers */}
			<div class="mb-4">
				<p class="text-xs text-slate-500 mb-2">Automatic triggers:</p>
				<div class="space-y-2">
					<div class="flex items-center gap-3 text-sm">
						<span class="w-5 h-5 rounded bg-violet-500/20 text-violet-400 flex items-center justify-center text-xs font-bold">
							1
						</span>
						<span class="text-slate-300">WAL reaches 75% capacity</span>
					</div>
					<div class="flex items-center gap-3 text-sm">
						<span class="w-5 h-5 rounded bg-violet-500/20 text-violet-400 flex items-center justify-center text-xs font-bold">
							2
						</span>
						<span class="text-slate-300">
							Configured time interval (e.g., every 5 minutes)
						</span>
					</div>
					<div class="flex items-center gap-3 text-sm">
						<span class="w-5 h-5 rounded bg-violet-500/20 text-violet-400 flex items-center justify-center text-xs font-bold">
							3
						</span>
						<span class="text-slate-300">On graceful shutdown</span>
					</div>
				</div>
			</div>

			{/* Manual */}
			<div class="mb-4 p-3 rounded-lg bg-slate-800/50 border border-slate-700">
				<p class="text-xs text-slate-500 mb-1">Manual checkpoint:</p>
				<code class="text-sm text-cyan-400 font-mono">
					await db.checkpoint();
				</code>
			</div>

			{/* During checkpoint */}
			<div class="pt-4 border-t border-slate-700/50">
				<p class="text-xs text-slate-500 mb-2">During checkpoint:</p>
				<ul class="space-y-1 text-sm text-slate-400">
					<li class="flex items-center gap-2">
						<span class="text-emerald-400">✓</span>
						Reads continue (from old snapshot + delta)
					</li>
					<li class="flex items-center gap-2">
						<span class="text-emerald-400">✓</span>
						Writes continue (to secondary WAL region)
					</li>
					<li class="flex items-center gap-2">
						<span class="text-emerald-400">✓</span>
						<span class="text-emerald-400 font-medium">No downtime</span>
					</li>
				</ul>
			</div>
		</div>
	);
}

// ============================================================================
// MVCC & TRANSACTIONS COMPONENTS
// ============================================================================

// Snapshot isolation timeline
function SnapshotIsolationTimeline() {
	return (
		<div class="my-6 rounded-xl border border-cyan-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<h4 class="font-semibold text-cyan-400 mb-4">
				Snapshot Isolation Timeline
			</h4>

			{/* Timeline visual */}
			<div class="relative mb-6">
				{/* Timeline line */}
				<div class="h-1 bg-gradient-to-r from-cyan-500/50 via-violet-500/50 to-emerald-500/50 rounded-full" />

				{/* Events */}
				<div class="flex justify-between mt-2">
					<div class="flex flex-col items-center">
						<div class="w-3 h-3 rounded-full bg-cyan-400 -mt-4 mb-2" />
						<span class="text-xs text-cyan-400 font-medium">T1 starts</span>
						<span class="text-xs text-slate-500">sees v1</span>
					</div>
					<div class="flex flex-col items-center">
						<div class="w-3 h-3 rounded-full bg-violet-400 -mt-4 mb-2" />
						<span class="text-xs text-violet-400 font-medium">T2 starts</span>
						<span class="text-xs text-slate-500">sees v1</span>
					</div>
					<div class="flex flex-col items-center">
						<div class="w-3 h-3 rounded-full bg-emerald-400 -mt-4 mb-2" />
						<span class="text-xs text-emerald-400 font-medium">T1 commits</span>
						<span class="text-xs text-slate-500">writes v2</span>
					</div>
				</div>
			</div>

			{/* Key point */}
			<div class="p-3 rounded-lg bg-violet-500/10 border border-violet-500/20">
				<p class="text-sm text-slate-300">
					<span class="text-violet-400 font-medium">T2 still sees v1</span> —
					T1's changes are invisible until T2 restarts
				</p>
			</div>
		</div>
	);
}

// Version chain visualization
function VersionChainDiagram() {
	return (
		<div class="my-6 rounded-xl border border-violet-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<h4 class="font-semibold text-violet-400 mb-4">
				Version Chain for Node "alice"
			</h4>

			{/* Chain visualization */}
			<div class="flex items-center justify-center gap-2 mb-4 overflow-x-auto py-2">
				{/* v3 */}
				<div class="shrink-0 rounded-lg border-2 border-emerald-500/40 bg-emerald-500/10 p-3 min-w-[120px]">
					<div class="text-sm font-medium text-emerald-400">v3: age=32</div>
					<div class="text-xs text-slate-500 font-mono mt-1">commitTs=150</div>
				</div>
				<div class="text-slate-500 shrink-0">←</div>
				{/* v2 */}
				<div class="shrink-0 rounded-lg border-2 border-violet-500/40 bg-violet-500/10 p-3 min-w-[120px]">
					<div class="text-sm font-medium text-violet-400">v2: age=31</div>
					<div class="text-xs text-slate-500 font-mono mt-1">commitTs=120</div>
				</div>
				<div class="text-slate-500 shrink-0">←</div>
				{/* v1 */}
				<div class="shrink-0 rounded-lg border-2 border-cyan-500/40 bg-cyan-500/10 p-3 min-w-[120px]">
					<div class="text-sm font-medium text-cyan-400">v1: age=30</div>
					<div class="text-xs text-slate-500 font-mono mt-1">commitTs=80</div>
				</div>
			</div>

			{/* Who sees what */}
			<div class="grid grid-cols-3 gap-2 text-center text-xs">
				<div class="p-2 rounded bg-emerald-500/10">
					<span class="text-emerald-400">T3 sees this</span>
					<div class="text-slate-500 font-mono">startTs=145</div>
				</div>
				<div class="p-2 rounded bg-violet-500/10">
					<span class="text-violet-400">T2 sees this</span>
					<div class="text-slate-500 font-mono">startTs=115</div>
				</div>
				<div class="p-2 rounded bg-cyan-500/10">
					<span class="text-cyan-400">T1 sees this</span>
					<div class="text-slate-500 font-mono">startTs=75</div>
				</div>
			</div>

			<p class="text-xs text-slate-500 mt-4 pt-3 border-t border-slate-700/50">
				Each transaction follows the chain to find the version committed before
				it started.
			</p>
		</div>
	);
}

// Visibility rules
function VisibilityRules() {
	return (
		<div class="my-6 rounded-xl border border-cyan-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<h4 class="font-semibold text-cyan-400 mb-4">Visibility Rules</h4>

			<p class="text-sm text-slate-400 mb-4">
				A version is visible to transaction T if:
			</p>

			<div class="space-y-3 mb-4">
				{/* Rule 1 */}
				<div class="flex items-start gap-3 p-3 rounded-lg bg-slate-800/50 border border-slate-700">
					<span class="flex items-center justify-center w-6 h-6 rounded-full bg-cyan-500/20 text-cyan-400 text-xs font-bold shrink-0">
						1
					</span>
					<div>
						<code class="text-sm text-cyan-400 font-mono">
							version.commitTs &lt;= T.startTs
						</code>
						<p class="text-xs text-slate-500 mt-1">
							Version was committed before T started
						</p>
					</div>
				</div>

				{/* OR */}
				<div class="text-center text-slate-500 text-sm font-medium">OR</div>

				{/* Rule 2 */}
				<div class="flex items-start gap-3 p-3 rounded-lg bg-slate-800/50 border border-slate-700">
					<span class="flex items-center justify-center w-6 h-6 rounded-full bg-violet-500/20 text-violet-400 text-xs font-bold shrink-0">
						2
					</span>
					<div>
						<code class="text-sm text-violet-400 font-mono">
							version.txid == T.txid
						</code>
						<p class="text-xs text-slate-500 mt-1">
							T created this version itself (read-your-own-writes)
						</p>
					</div>
				</div>
			</div>

			{/* Process */}
			<div class="pt-3 border-t border-slate-700/50 space-y-1 text-sm text-slate-400">
				<p>Walk the chain from newest to oldest.</p>
				<p>Return first visible version.</p>
				<p class="text-slate-500">
					If none visible → entity doesn't exist for this transaction.
				</p>
			</div>
		</div>
	);
}

// Write conflict diagram
function WriteConflictDiagram() {
	return (
		<div class="my-6 rounded-xl border border-amber-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<div class="flex items-center gap-2 mb-4">
				<svg
					class="w-5 h-5 text-amber-400"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
				>
					<path
						stroke-linecap="round"
						stroke-linejoin="round"
						d="M12 9v2m0 4h.01m-6.938 4h13.856c1.54 0 2.502-1.667 1.732-3L13.732 4c-.77-1.333-2.694-1.333-3.464 0L3.34 16c-.77 1.333.192 3 1.732 3z"
					/>
				</svg>
				<h4 class="font-semibold text-amber-400">First-Committer-Wins</h4>
			</div>

			{/* Scenario */}
			<div class="mb-4 p-3 rounded-lg bg-slate-800/50 border border-slate-700">
				<p class="text-xs text-slate-500 mb-2">
					Scenario: Both T1 and T2 modify "alice"
				</p>
				<div class="space-y-1 text-sm">
					<div class="flex items-center gap-2">
						<span class="text-cyan-400 font-mono text-xs">T1</span>
						<span class="text-slate-400">starts at ts=100</span>
					</div>
					<div class="flex items-center gap-2">
						<span class="text-violet-400 font-mono text-xs">T2</span>
						<span class="text-slate-400">starts at ts=105</span>
					</div>
				</div>
			</div>

			{/* Outcomes */}
			<div class="space-y-2">
				<div class="flex items-start gap-3 p-3 rounded-lg bg-emerald-500/10 border border-emerald-500/20">
					<span class="text-emerald-400 text-lg">✓</span>
					<div>
						<p class="text-sm text-emerald-400 font-medium">
							T1 commits first (ts=110)
						</p>
						<p class="text-xs text-slate-400">Succeeds — no conflict</p>
					</div>
				</div>
				<div class="flex items-start gap-3 p-3 rounded-lg bg-red-500/10 border border-red-500/20">
					<span class="text-red-400 text-lg">✗</span>
					<div>
						<p class="text-sm text-red-400 font-medium">
							T2 tries to commit (ts=115)
						</p>
						<p class="text-xs text-slate-400">
							Was "alice" modified after T2.startTs (105)? Yes!
						</p>
						<p class="text-xs text-red-400 mt-1">
							Rolled back with ConflictError
						</p>
					</div>
				</div>
			</div>

			<p class="text-xs text-slate-500 mt-4 pt-3 border-t border-slate-700/50">
				<span class="text-amber-400">Resolution:</span> T2 must retry with fresh
				read
			</p>
		</div>
	);
}

// Lazy MVCC optimization
function LazyMVCCDiagram() {
	return (
		<div class="my-6 rounded-xl border border-emerald-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<div class="flex items-center gap-2 mb-4">
				<svg
					class="w-5 h-5 text-emerald-400"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
				>
					<path
						stroke-linecap="round"
						stroke-linejoin="round"
						d="M13 10V3L4 14h7v7l9-11h-7z"
					/>
				</svg>
				<h4 class="font-semibold text-emerald-400">Lazy MVCC Optimization</h4>
			</div>

			<p class="text-sm text-slate-400 mb-4">When T1 modifies "alice":</p>

			<div class="space-y-3 mb-4">
				{/* No concurrent */}
				<div class="flex items-start gap-3 p-3 rounded-lg bg-emerald-500/10 border border-emerald-500/20">
					<span class="text-emerald-400 font-medium text-sm shrink-0">IF</span>
					<div>
						<p class="text-sm text-slate-300">
							No other transactions are active
						</p>
						<p class="text-xs text-emerald-400 mt-1">
							→ Modify in-place (no version chain)
						</p>
					</div>
				</div>

				{/* Concurrent */}
				<div class="flex items-start gap-3 p-3 rounded-lg bg-violet-500/10 border border-violet-500/20">
					<span class="text-violet-400 font-medium text-sm shrink-0">ELSE</span>
					<div>
						<p class="text-sm text-slate-300">Other transactions are active</p>
						<p class="text-xs text-violet-400 mt-1">
							→ Create version chain (preserve old value)
						</p>
					</div>
				</div>
			</div>

			{/* Result */}
			<div class="pt-3 border-t border-slate-700/50">
				<p class="text-sm text-slate-400">
					<span class="text-emerald-400 font-medium">Result:</span> Serial
					workloads have zero MVCC overhead. Concurrent workloads get correct
					isolation.
				</p>
			</div>
		</div>
	);
}

// Garbage collection process
function MVCCGarbageCollection() {
	return (
		<div class="my-6 rounded-xl border border-slate-600/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<h4 class="font-semibold text-slate-400 mb-4">Garbage Collection</h4>

			<div class="space-y-2 mb-4">
				<div class="flex items-center gap-3 text-sm">
					<span class="w-5 h-5 rounded bg-cyan-500/20 text-cyan-400 flex items-center justify-center text-xs font-bold">
						1
					</span>
					<span class="text-slate-300">
						Track oldest active transaction (
						<code class="text-cyan-400 text-xs">minStartTs</code>)
					</span>
				</div>
				<div class="flex items-center gap-3 text-sm">
					<span class="w-5 h-5 rounded bg-cyan-500/20 text-cyan-400 flex items-center justify-center text-xs font-bold">
						2
					</span>
					<span class="text-slate-300">For each version chain:</span>
				</div>
				<div class="ml-8 space-y-1 text-sm">
					<div class="flex items-center gap-2">
						<span class="text-emerald-400">•</span>
						<span class="text-slate-400">
							Keep versions where{" "}
							<code class="text-emerald-400 text-xs">
								commitTs &gt;= minStartTs
							</code>
						</span>
					</div>
					<div class="flex items-center gap-2">
						<span class="text-red-400">•</span>
						<span class="text-slate-400">
							Delete older versions (no one can see them)
						</span>
					</div>
				</div>
			</div>

			{/* Triggers */}
			<div class="p-3 rounded-lg bg-slate-800/50 border border-slate-700 mb-4">
				<p class="text-xs text-slate-500 mb-2">Triggered:</p>
				<ul class="space-y-1 text-sm text-slate-400">
					<li>• After transaction commits</li>
					<li>• Periodically in background</li>
				</ul>
			</div>

			{/* Warning */}
			<div class="pt-3 border-t border-slate-700/50">
				<p class="text-xs text-amber-400">
					<span class="font-medium">Note:</span> Long-running transactions delay
					GC and hold memory.
				</p>
			</div>
		</div>
	);
}

// ============================================================================
// KEY INDEX COMPONENTS
// ============================================================================

// Problem comparison
function KeyIndexProblem() {
	return (
		<div class="my-6 grid sm:grid-cols-2 gap-4">
			{/* Without index */}
			<div class="rounded-xl border border-red-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-4 shadow-lg">
				<div class="flex items-center gap-2 mb-3">
					<svg
						class="w-5 h-5 text-red-400"
						viewBox="0 0 24 24"
						fill="none"
						stroke="currentColor"
						stroke-width="2"
					>
						<path
							stroke-linecap="round"
							stroke-linejoin="round"
							d="M12 8v4m0 4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z"
						/>
					</svg>
					<span class="font-semibold text-red-400">Without Index</span>
				</div>
				<p class="text-sm text-slate-400 mb-2">"Find user:alice"</p>
				<p class="text-sm text-slate-300">
					Scan all nodes → <span class="text-red-400 font-mono">O(n)</span>
				</p>
				<div class="mt-3 pt-3 border-t border-slate-700/50">
					<p class="text-xs text-slate-500">
						1M nodes × 1μs ={" "}
						<span class="text-red-400 font-medium">1 second</span>
					</p>
				</div>
			</div>

			{/* With index */}
			<div class="rounded-xl border border-emerald-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-4 shadow-lg">
				<div class="flex items-center gap-2 mb-3">
					<svg
						class="w-5 h-5 text-emerald-400"
						viewBox="0 0 24 24"
						fill="none"
						stroke="currentColor"
						stroke-width="2"
					>
						<path
							stroke-linecap="round"
							stroke-linejoin="round"
							d="M9 12l2 2 4-4m6 2a9 9 0 11-18 0 9 9 0 0118 0z"
						/>
					</svg>
					<span class="font-semibold text-emerald-400">With Hash Index</span>
				</div>
				<p class="text-sm text-slate-400 mb-2">"Find user:alice"</p>
				<p class="text-sm text-slate-300">
					Hash lookup → <span class="text-emerald-400 font-mono">O(1)</span>
				</p>
				<div class="mt-3 pt-3 border-t border-slate-700/50">
					<p class="text-xs text-slate-500">
						1M nodes = <span class="text-emerald-400 font-medium">~100ns</span>
					</p>
				</div>
			</div>
		</div>
	);
}

// Index structure visualization
function KeyIndexStructure() {
	return (
		<div class="my-6 rounded-xl border border-cyan-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<h4 class="font-semibold text-cyan-400 mb-4">Key Index Structure</h4>

			<div class="space-y-4">
				{/* Bucket array */}
				<div class="p-3 rounded-lg bg-slate-800/50 border border-cyan-500/20">
					<div class="flex items-center justify-between mb-2">
						<span class="text-sm font-medium text-cyan-400">Bucket Array</span>
						<span class="text-xs text-slate-500">n buckets</span>
					</div>
					<div class="flex gap-0.5">
						<div class="w-10 py-1 text-center text-xs font-mono rounded-l bg-cyan-500/20 text-cyan-400 border-r border-cyan-500/30">
							0
						</div>
						<div class="w-10 py-1 text-center text-xs font-mono bg-cyan-500/20 text-cyan-400 border-r border-cyan-500/30">
							1
						</div>
						<div class="w-10 py-1 text-center text-xs font-mono bg-cyan-500/20 text-cyan-400 border-r border-cyan-500/30">
							2
						</div>
						<div class="w-10 py-1 text-center text-xs font-mono bg-cyan-500/20 text-cyan-400 border-r border-cyan-500/30">
							3
						</div>
						<div class="w-10 py-1 text-center text-xs font-mono rounded-r bg-cyan-500/20 text-cyan-400">
							...
						</div>
					</div>
					<p class="text-xs text-slate-500 mt-1">
						← Start offsets into entry array
					</p>
				</div>

				{/* Entry array */}
				<div class="p-3 rounded-lg bg-slate-800/50 border border-violet-500/20">
					<div class="flex items-center justify-between mb-2">
						<span class="text-sm font-medium text-violet-400">Entry Array</span>
						<span class="text-xs text-slate-500">
							sorted by bucket, then hash
						</span>
					</div>
					<div class="space-y-1">
						<div class="flex gap-0.5 text-xs">
							<div class="flex-1 py-1 px-2 rounded-l bg-violet-500/20 text-violet-400 font-mono">
								hash64
							</div>
							<div class="flex-1 py-1 px-2 bg-violet-500/20 text-violet-400 font-mono">
								stringId
							</div>
							<div class="flex-1 py-1 px-2 rounded-r bg-violet-500/20 text-violet-400 font-mono">
								nodeId
							</div>
							<div class="w-20 py-1 text-center text-slate-500">← bucket 0</div>
						</div>
						<div class="flex gap-0.5 text-xs">
							<div class="flex-1 py-1 px-2 rounded-l bg-violet-500/10 text-violet-300 font-mono">
								hash64
							</div>
							<div class="flex-1 py-1 px-2 bg-violet-500/10 text-violet-300 font-mono">
								stringId
							</div>
							<div class="flex-1 py-1 px-2 rounded-r bg-violet-500/10 text-violet-300 font-mono">
								nodeId
							</div>
							<div class="w-20 py-1 text-center text-slate-500">← bucket 1</div>
						</div>
						<div class="flex gap-0.5 text-xs">
							<div class="flex-1 py-1 px-2 rounded-l bg-slate-700/50 text-slate-400 font-mono">
								...
							</div>
							<div class="flex-1 py-1 px-2 bg-slate-700/50 text-slate-400 font-mono">
								...
							</div>
							<div class="flex-1 py-1 px-2 rounded-r bg-slate-700/50 text-slate-400 font-mono">
								...
							</div>
							<div class="w-20" />
						</div>
					</div>
				</div>
			</div>

			{/* Legend */}
			<div class="mt-4 pt-3 border-t border-slate-700/50 space-y-1 text-xs text-slate-500">
				<p>
					<span class="text-cyan-400 font-mono">hash64</span>: xxHash64 of the
					key string
				</p>
				<p>
					<span class="text-cyan-400 font-mono">stringId</span>: Index into
					string table (for collision resolution)
				</p>
				<p>
					<span class="text-cyan-400 font-mono">nodeId</span>: The NodeID this
					key maps to
				</p>
			</div>
		</div>
	);
}

// Lookup process
function KeyLookupProcess() {
	return (
		<div class="my-6 rounded-xl border border-violet-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<h4 class="font-semibold text-violet-400 mb-4">Lookup Process</h4>

			<div class="space-y-4">
				{/* Step 1 - Delta */}
				<div class="p-3 rounded-lg bg-cyan-500/10 border border-cyan-500/20">
					<div class="flex items-center gap-2 mb-2">
						<span class="flex items-center justify-center w-5 h-5 rounded-full bg-cyan-500/20 text-cyan-400 text-xs font-bold">
							1
						</span>
						<span class="text-sm font-medium text-cyan-400">
							Check Delta First
						</span>
						<span class="text-xs text-slate-500 ml-auto">recent changes</span>
					</div>
					<div class="ml-7 space-y-1 text-sm text-slate-400">
						<p>
							If key in{" "}
							<code class="text-red-400 text-xs">delta.keyIndexDeleted</code> →
							return null
						</p>
						<p>
							If key in{" "}
							<code class="text-emerald-400 text-xs">delta.keyIndex</code> →
							return NodeID
						</p>
					</div>
				</div>

				{/* Step 2 - Snapshot */}
				<div class="p-3 rounded-lg bg-violet-500/10 border border-violet-500/20">
					<div class="flex items-center gap-2 mb-2">
						<span class="flex items-center justify-center w-5 h-5 rounded-full bg-violet-500/20 text-violet-400 text-xs font-bold">
							2
						</span>
						<span class="text-sm font-medium text-violet-400">
							Search Snapshot Index
						</span>
						<span class="text-xs text-slate-500 ml-auto">on disk</span>
					</div>
					<div class="ml-7 space-y-1 text-sm font-mono text-slate-400">
						<p>hash = xxHash64(key)</p>
						<p>bucket = hash % numBuckets</p>
						<p>start = bucketArray[bucket]</p>
						<p>end = bucketArray[bucket + 1]</p>
					</div>
				</div>

				{/* Step 3 - Search */}
				<div class="p-3 rounded-lg bg-emerald-500/10 border border-emerald-500/20">
					<div class="flex items-center gap-2 mb-2">
						<span class="flex items-center justify-center w-5 h-5 rounded-full bg-emerald-500/20 text-emerald-400 text-xs font-bold">
							3
						</span>
						<span class="text-sm font-medium text-emerald-400">
							Binary Search in Bucket
						</span>
					</div>
					<div class="ml-7 space-y-1 text-sm text-slate-400">
						<p>
							Find entry where{" "}
							<code class="text-cyan-400 text-xs">entry.hash64 == hash</code>
						</p>
						<p>
							Verify:{" "}
							<code class="text-cyan-400 text-xs">
								stringTable[entry.stringId] == key
							</code>
						</p>
						<p>
							Return <code class="text-emerald-400 text-xs">entry.nodeId</code>
						</p>
					</div>
				</div>
			</div>
		</div>
	);
}

// Two-level lookup
function TwoLevelLookup() {
	return (
		<div class="my-6 rounded-xl border border-slate-600/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<h4 class="font-semibold text-slate-400 mb-4">Two-Level Lookup</h4>

			<div class="grid sm:grid-cols-2 gap-4 mb-4">
				{/* Delta */}
				<div class="p-3 rounded-lg bg-violet-500/10 border border-violet-500/20">
					<div class="text-sm font-medium text-violet-400 mb-2">
						Delta (in-memory)
					</div>
					<div class="space-y-1 text-xs">
						<div class="flex items-center gap-2">
							<span class="text-slate-600">├</span>
							<code class="text-emerald-400">keyIndex</code>
							<span class="text-slate-500">Map&lt;string, NodeID&gt;</span>
						</div>
						<div class="flex items-center gap-2">
							<span class="text-slate-600">└</span>
							<code class="text-red-400">keyIndexDeleted</code>
							<span class="text-slate-500">Set&lt;string&gt;</span>
						</div>
					</div>
				</div>

				{/* Snapshot */}
				<div class="p-3 rounded-lg bg-cyan-500/10 border border-cyan-500/20">
					<div class="text-sm font-medium text-cyan-400 mb-2">
						Snapshot (on disk)
					</div>
					<div class="space-y-1 text-xs">
						<div class="flex items-center gap-2">
							<span class="text-slate-600">├</span>
							<code class="text-cyan-400">bucketArray</code>
							<span class="text-slate-500">u32[]</span>
						</div>
						<div class="flex items-center gap-2">
							<span class="text-slate-600">└</span>
							<code class="text-cyan-400">entries</code>
							<span class="text-slate-500">
								{"{hash64, stringId, nodeId}[]"}
							</span>
						</div>
					</div>
				</div>
			</div>

			{/* Lookup order */}
			<div class="pt-3 border-t border-slate-700/50">
				<p class="text-xs text-slate-500 mb-2">Lookup order:</p>
				<div class="space-y-1 text-sm">
					<div class="flex items-center gap-2">
						<span class="w-5 h-5 rounded bg-slate-700 text-slate-400 flex items-center justify-center text-xs font-bold">
							1
						</span>
						<code class="text-red-400 text-xs">delta.keyIndexDeleted</code>
						<span class="text-slate-500">→ If found, return null</span>
					</div>
					<div class="flex items-center gap-2">
						<span class="w-5 h-5 rounded bg-slate-700 text-slate-400 flex items-center justify-center text-xs font-bold">
							2
						</span>
						<code class="text-emerald-400 text-xs">delta.keyIndex</code>
						<span class="text-slate-500">→ If found, return NodeID</span>
					</div>
					<div class="flex items-center gap-2">
						<span class="w-5 h-5 rounded bg-slate-700 text-slate-400 flex items-center justify-center text-xs font-bold">
							3
						</span>
						<code class="text-cyan-400 text-xs">snapshot index</code>
						<span class="text-slate-500">→ Search hash buckets</span>
					</div>
				</div>
				<p class="text-xs text-slate-500 mt-2 italic">
					This order ensures recent changes override old data.
				</p>
			</div>
		</div>
	);
}

// xxHash64 explanation
function XxHash64Explanation() {
	return (
		<div class="my-6 rounded-xl border border-emerald-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<div class="flex items-center gap-2 mb-4">
				<svg
					class="w-5 h-5 text-emerald-400"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
				>
					<path
						stroke-linecap="round"
						stroke-linejoin="round"
						d="M13 10V3L4 14h7v7l9-11h-7z"
					/>
				</svg>
				<h4 class="font-semibold text-emerald-400">Why xxHash64</h4>
			</div>

			{/* Requirements */}
			<div class="mb-4 space-y-1 text-sm">
				<div class="flex items-center gap-2">
					<span class="text-emerald-400">✓</span>
					<span class="text-emerald-400 font-medium">Fast</span>
					<span class="text-slate-400">— Called on every key lookup</span>
				</div>
				<div class="flex items-center gap-2">
					<span class="text-emerald-400">✓</span>
					<span class="text-emerald-400 font-medium">Good distribution</span>
					<span class="text-slate-400">— Minimize bucket collisions</span>
				</div>
				<div class="flex items-center gap-2">
					<span class="text-emerald-400">✓</span>
					<span class="text-emerald-400 font-medium">Deterministic</span>
					<span class="text-slate-400">— Same key always same hash</span>
				</div>
			</div>

			{/* Comparison */}
			<div class="p-3 rounded-lg bg-slate-800/50 border border-slate-700 mb-4">
				<p class="text-xs text-slate-500 mb-2">
					For typical key lengths (10-100 bytes):
				</p>
				<div class="grid grid-cols-2 gap-3">
					<div>
						<p class="text-sm text-emerald-400 font-medium">xxHash64</p>
						<p class="text-xs text-slate-400 font-mono">~50-100ns per hash</p>
						<p class="text-xs text-slate-500">~10 GB/s throughput</p>
					</div>
					<div>
						<p class="text-sm text-slate-400 font-medium">SHA-256</p>
						<p class="text-xs text-slate-400 font-mono">~500-1000ns per hash</p>
						<p class="text-xs text-slate-500">Cryptographic (overkill)</p>
					</div>
				</div>
			</div>

			<p class="text-xs text-emerald-400">
				10x faster, and we don't need cryptographic security.
			</p>
		</div>
	);
}

// Collision handling
function CollisionHandling() {
	return (
		<div class="my-6 rounded-xl border border-amber-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<div class="flex items-center gap-2 mb-4">
				<svg
					class="w-5 h-5 text-amber-400"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
				>
					<path
						stroke-linecap="round"
						stroke-linejoin="round"
						d="M12 9v2m0 4h.01m-6.938 4h13.856c1.54 0 2.502-1.667 1.732-3L13.732 4c-.77-1.333-2.694-1.333-3.464 0L3.34 16c-.77 1.333.192 3 1.732 3z"
					/>
				</svg>
				<h4 class="font-semibold text-amber-400">Handling Collisions</h4>
			</div>

			{/* Scenario */}
			<div class="mb-4 p-3 rounded-lg bg-slate-800/50 border border-slate-700">
				<p class="text-xs text-slate-500 mb-2">
					Scenario: Two keys hash to same bucket
				</p>
				<div class="space-y-1 text-sm font-mono">
					<p>
						<span class="text-cyan-400">"user:alice"</span> → hash:{" "}
						<span class="text-slate-400">0x1234...</span>
					</p>
					<p>
						<span class="text-cyan-400">"user:alfred"</span> → hash:{" "}
						<span class="text-amber-400">0x1234...</span>{" "}
						<span class="text-xs text-amber-400">(collision!)</span>
					</p>
				</div>
			</div>

			{/* Resolution steps */}
			<div class="space-y-2 mb-4">
				<div class="flex items-center gap-3 text-sm">
					<span class="w-5 h-5 rounded bg-slate-700 text-slate-400 flex items-center justify-center text-xs font-bold">
						1
					</span>
					<span class="text-slate-300">Both entries stored in same bucket</span>
				</div>
				<div class="flex items-center gap-3 text-sm">
					<span class="w-5 h-5 rounded bg-slate-700 text-slate-400 flex items-center justify-center text-xs font-bold">
						2
					</span>
					<span class="text-slate-300">On lookup, hash matches both</span>
				</div>
				<div class="flex items-center gap-3 text-sm">
					<span class="w-5 h-5 rounded bg-slate-700 text-slate-400 flex items-center justify-center text-xs font-bold">
						3
					</span>
					<span class="text-slate-300">stringId comparison breaks tie</span>
				</div>
				<div class="flex items-center gap-3 text-sm">
					<span class="w-5 h-5 rounded bg-emerald-500/20 text-emerald-400 flex items-center justify-center text-xs font-bold">
						4
					</span>
					<span class="text-emerald-300">
						Actual string comparison confirms match
					</span>
				</div>
			</div>

			<p class="text-xs text-slate-500">
				Cost: <span class="text-amber-400">O(k)</span> string comparisons where
				k = entries with same hash. With 64-bit hash:{" "}
				<span class="text-emerald-400">k ≈ 1</span>
			</p>
		</div>
	);
}

// Load factor visualization
function LoadFactorDiagram() {
	return (
		<div class="my-6 rounded-xl border border-slate-600/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<h4 class="font-semibold text-slate-400 mb-4">Load Factor</h4>

			<p class="text-sm text-slate-400 mb-4">
				<code class="text-cyan-400">Load factor = entries / buckets</code>
			</p>

			<div class="p-3 rounded-lg bg-emerald-500/10 border border-emerald-500/20 mb-4">
				<p class="text-sm text-emerald-400 font-medium mb-2">
					KiteDB uses ~50% load factor
				</p>
				<p class="text-xs text-slate-400">(2x buckets as entries)</p>
				<ul class="mt-2 space-y-1 text-sm text-slate-400">
					<li class="flex items-center gap-2">
						<span class="text-emerald-400">•</span>
						Low collision rate
					</li>
					<li class="flex items-center gap-2">
						<span class="text-emerald-400">•</span>
						Reasonable memory usage
					</li>
					<li class="flex items-center gap-2">
						<span class="text-emerald-400">•</span>
						Fast lookups
					</li>
				</ul>
			</div>

			{/* Example */}
			<div class="p-3 rounded-lg bg-slate-800/50 border border-slate-700">
				<p class="text-xs text-slate-500 mb-2">With 1M keys:</p>
				<div class="space-y-1 text-sm">
					<div class="flex justify-between">
						<span class="text-slate-400">Buckets:</span>
						<span class="text-cyan-400 font-mono">2M × 4 bytes = 8 MB</span>
					</div>
					<div class="flex justify-between">
						<span class="text-slate-400">Entries:</span>
						<span class="text-cyan-400 font-mono">1M × 24 bytes = 24 MB</span>
					</div>
					<div class="flex justify-between pt-2 border-t border-slate-700 mt-2">
						<span class="text-slate-300 font-medium">Total index:</span>
						<span class="text-emerald-400 font-mono font-medium">~32 MB</span>
					</div>
				</div>
				<p class="text-xs text-slate-500 mt-3">
					Lookup: 1 bucket read + 1-2 entry reads ={" "}
					<span class="text-emerald-400">~100ns</span>
				</p>
			</div>
		</div>
	);
}

// ============================================================================
// PERFORMANCE COMPONENTS
// ============================================================================

// Network overhead comparison
function NetworkOverheadComparison() {
	return (
		<div class="my-6 grid sm:grid-cols-2 gap-4">
			{/* Traditional */}
			<div class="rounded-xl border border-red-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-4 shadow-lg">
				<div class="text-sm font-medium text-red-400 mb-3">
					Traditional Database
				</div>
				<div class="flex items-center gap-1 text-xs mb-3 flex-wrap">
					<span class="px-2 py-1 rounded bg-slate-700 text-slate-300">App</span>
					<span class="text-slate-500">→</span>
					<span class="px-2 py-1 rounded bg-red-500/20 text-red-400">
						Network
					</span>
					<span class="text-slate-500">→</span>
					<span class="px-2 py-1 rounded bg-slate-700 text-slate-300">DB</span>
					<span class="text-slate-500">→</span>
					<span class="px-2 py-1 rounded bg-slate-700 text-slate-300">
						Disk
					</span>
					<span class="text-slate-500">→</span>
					<span class="text-slate-400">...</span>
				</div>
				<div class="text-sm">
					<span class="text-slate-400">Latency:</span>
					<span class="text-red-400 font-mono ml-2">1-10ms</span>
					<span class="text-slate-500 text-xs ml-1">per operation</span>
				</div>
			</div>

			{/* KiteDB */}
			<div class="rounded-xl border border-emerald-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-4 shadow-lg">
				<div class="text-sm font-medium text-emerald-400 mb-3">
					KiteDB (embedded)
				</div>
				<div class="flex items-center gap-1 text-xs mb-3">
					<span class="px-2 py-1 rounded bg-slate-700 text-slate-300">App</span>
					<span class="text-slate-500">→</span>
					<span class="px-2 py-1 rounded bg-emerald-500/20 text-emerald-400">
						Memory/Disk
					</span>
					<span class="text-slate-500">→</span>
					<span class="px-2 py-1 rounded bg-slate-700 text-slate-300">App</span>
				</div>
				<div class="text-sm">
					<span class="text-slate-400">Latency:</span>
					<span class="text-emerald-400 font-mono ml-2">1-100μs</span>
					<span class="text-slate-500 text-xs ml-1">per operation</span>
				</div>
			</div>
		</div>
	);
}

// Zero-copy mmap comparison
function ZeroCopyComparison() {
	return (
		<div class="my-6 grid sm:grid-cols-2 gap-4">
			{/* Traditional */}
			<div class="rounded-xl border border-red-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-4 shadow-lg">
				<div class="text-sm font-medium text-red-400 mb-3">
					Traditional Read
				</div>
				<div class="space-y-1 text-xs mb-3">
					<div class="flex items-center gap-2">
						<span class="w-24 text-slate-400">Disk</span>
						<span class="text-slate-500">→</span>
						<span class="text-slate-300">Kernel buffer</span>
					</div>
					<div class="flex items-center gap-2">
						<span class="w-24 text-slate-400" />
						<span class="text-slate-500">→</span>
						<span class="text-slate-300">User buffer</span>
					</div>
					<div class="flex items-center gap-2">
						<span class="w-24 text-slate-400" />
						<span class="text-slate-500">→</span>
						<span class="text-slate-300">Parse → Use</span>
					</div>
				</div>
				<div class="text-sm text-red-400">
					2+ memory copies, allocation overhead
				</div>
			</div>

			{/* KiteDB */}
			<div class="rounded-xl border border-emerald-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-4 shadow-lg">
				<div class="text-sm font-medium text-emerald-400 mb-3">
					KiteDB (mmap)
				</div>
				<div class="space-y-1 text-xs mb-3">
					<div class="flex items-center gap-2">
						<span class="w-24 text-slate-400">Disk</span>
						<span class="text-slate-500">→</span>
						<span class="text-emerald-300">Page cache</span>
					</div>
					<div class="flex items-center gap-2">
						<span class="w-24 text-slate-400" />
						<span class="text-slate-500">→</span>
						<span class="text-emerald-300">Direct access</span>
					</div>
				</div>
				<div class="text-sm text-emerald-400">
					0 copies — OS handles caching
				</div>
			</div>
		</div>
	);
}

// Cache-friendly layout comparison
function CacheFriendlyComparison() {
	return (
		<div class="my-6 rounded-xl border border-cyan-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<h4 class="font-semibold text-cyan-400 mb-4">Traversing 10 Neighbors</h4>

			<div class="space-y-3">
				{/* Linked list */}
				<div class="flex items-center gap-4 p-3 rounded-lg bg-red-500/10 border border-red-500/20">
					<div class="w-24 text-sm font-medium text-red-400">Linked List</div>
					<div class="flex-1 text-sm text-slate-400">
						10 random accesses × <span class="font-mono">100ns</span>
					</div>
					<div class="text-red-400 font-mono font-medium">= 1000ns</div>
				</div>

				{/* CSR */}
				<div class="flex items-center gap-4 p-3 rounded-lg bg-emerald-500/10 border border-emerald-500/20">
					<div class="w-24 text-sm font-medium text-emerald-400">CSR</div>
					<div class="flex-1 text-sm text-slate-400">
						1 seq read × <span class="font-mono">10ns</span> + 10 cache hits ×{" "}
						<span class="font-mono">1ns</span>
					</div>
					<div class="text-emerald-400 font-mono font-medium">= 20ns</div>
				</div>
			</div>

			<div class="mt-4 pt-3 border-t border-slate-700/50 text-center">
				<span class="text-emerald-400 font-bold text-lg">50x</span>
				<span class="text-slate-400 text-sm ml-2">
					speedup for traversal operations
				</span>
			</div>
		</div>
	);
}

// Lazy MVCC comparison (simpler than the one in MVCC section)
function LazyMVCCComparison() {
	return (
		<div class="my-6 rounded-xl border border-violet-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<h4 class="font-semibold text-violet-400 mb-4">
				Version Chains: Only When Needed
			</h4>

			<div class="space-y-3">
				{/* Serial */}
				<div class="p-3 rounded-lg bg-emerald-500/10 border border-emerald-500/20">
					<div class="flex items-center justify-between mb-1">
						<span class="text-sm font-medium text-emerald-400">
							Serial workload
						</span>
						<span class="text-xs text-slate-500">(no concurrent readers)</span>
					</div>
					<div class="flex items-center justify-between text-sm">
						<span class="text-slate-400">Modify → Update in-place</span>
						<span class="text-emerald-400 font-mono">Overhead: 0</span>
					</div>
				</div>

				{/* Concurrent */}
				<div class="p-3 rounded-lg bg-violet-500/10 border border-violet-500/20">
					<div class="flex items-center justify-between mb-1">
						<span class="text-sm font-medium text-violet-400">
							Concurrent workload
						</span>
						<span class="text-xs text-slate-500">(active readers)</span>
					</div>
					<div class="flex items-center justify-between text-sm">
						<span class="text-slate-400">Modify → Create version chain</span>
						<span class="text-violet-400 font-mono">∝ concurrency</span>
					</div>
				</div>
			</div>

			<p class="text-xs text-slate-500 mt-4 pt-3 border-t border-slate-700/50">
				Most workloads are mostly serial. MVCC overhead is paid only when
				required.
			</p>
		</div>
	);
}

// Memory usage breakdown
function MemoryUsageBreakdown() {
	return (
		<div class="my-6 rounded-xl border border-slate-600/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<h4 class="font-semibold text-slate-400 mb-4">Memory Breakdown</h4>

			<div class="space-y-3">
				{/* Snapshot */}
				<div class="p-3 rounded-lg bg-cyan-500/10 border border-cyan-500/20">
					<div class="flex items-center justify-between mb-2">
						<span class="text-sm font-medium text-cyan-400">
							1. Snapshot (mmap'd)
						</span>
					</div>
					<ul class="space-y-1 text-xs text-slate-400 ml-4">
						<li>• Not counted against process memory</li>
						<li>• OS manages page cache</li>
						<li>• Hot pages in RAM, cold pages on disk</li>
					</ul>
				</div>

				{/* Delta */}
				<div class="p-3 rounded-lg bg-violet-500/10 border border-violet-500/20">
					<div class="flex items-center justify-between mb-2">
						<span class="text-sm font-medium text-violet-400">2. Delta</span>
					</div>
					<div class="grid grid-cols-2 gap-2 text-xs ml-4">
						<div class="flex justify-between">
							<span class="text-slate-400">Created nodes:</span>
							<span class="text-slate-300 font-mono">~200 B/node</span>
						</div>
						<div class="flex justify-between">
							<span class="text-slate-400">Modified nodes:</span>
							<span class="text-slate-300 font-mono">~100 B/change</span>
						</div>
						<div class="flex justify-between">
							<span class="text-slate-400">Edges:</span>
							<span class="text-slate-300 font-mono">~20 B/edge</span>
						</div>
					</div>
				</div>

				{/* Caches */}
				<div class="p-3 rounded-lg bg-amber-500/10 border border-amber-500/20">
					<div class="flex items-center justify-between mb-2">
						<span class="text-sm font-medium text-amber-400">3. Caches</span>
						<span class="text-xs text-slate-500">(configurable)</span>
					</div>
					<ul class="space-y-1 text-xs text-slate-400 ml-4">
						<li>• Property cache: LRU, default 10K entries</li>
						<li>• Traversal cache: LRU, invalidated on writes</li>
					</ul>
				</div>

				{/* MVCC */}
				<div class="p-3 rounded-lg bg-slate-700/30 border border-slate-600/30">
					<div class="flex items-center justify-between mb-2">
						<span class="text-sm font-medium text-slate-400">
							4. MVCC version chains
						</span>
					</div>
					<ul class="space-y-1 text-xs text-slate-500 ml-4">
						<li>• Only when concurrent transactions exist</li>
						<li>• Cleaned up by GC</li>
					</ul>
				</div>
			</div>

			{/* Example */}
			<div class="mt-4 pt-4 border-t border-slate-700/50 p-3 rounded-lg bg-emerald-500/10 border border-emerald-500/20">
				<p class="text-xs text-slate-500 mb-2">Typical 100K node graph:</p>
				<div class="flex justify-between text-sm">
					<span class="text-slate-400">Snapshot on disk:</span>
					<span class="text-slate-300 font-mono">~10MB (compressed)</span>
				</div>
				<div class="flex justify-between text-sm">
					<span class="text-slate-400">Memory footprint:</span>
					<span class="text-emerald-400 font-mono font-medium">~5MB</span>
				</div>
			</div>
		</div>
	);
}

// ============================================================================
// SNAPSHOT + DELTA COMPONENTS
// ============================================================================

// Snapshot + Delta model diagram
function SnapshotDeltaModel() {
	return (
		<div class="my-8">
			{/* Outer container with title */}
			<div class="rounded-2xl border border-slate-600/50 bg-gradient-to-br from-slate-900 to-slate-800 p-6 shadow-xl">
				<h4 class="text-center text-lg font-semibold text-slate-300 mb-6">
					Database State
				</h4>

				{/* Three boxes in a row */}
				<div class="flex flex-col sm:flex-row items-stretch gap-4">
					{/* Snapshot */}
					<div class="flex-1 rounded-xl border border-cyan-500/40 bg-slate-800/50 p-4 relative">
						<div class="absolute -top-px left-4 right-4 h-px bg-gradient-to-r from-transparent via-cyan-400/50 to-transparent" />
						<div class="flex items-center gap-2 mb-3">
							<svg
								class="w-5 h-5 text-cyan-400"
								viewBox="0 0 24 24"
								fill="none"
								stroke="currentColor"
								stroke-width="2"
							>
								<path
									stroke-linecap="round"
									stroke-linejoin="round"
									d="M4 7v10c0 2.21 3.582 4 8 4s8-1.79 8-4V7M4 7c0 2.21 3.582 4 8 4s8-1.79 8-4M4 7c0-2.21 3.582-4 8-4s8 1.79 8 4"
								/>
							</svg>
							<h5 class="font-bold text-cyan-400">Snapshot</h5>
						</div>
						<p class="text-xs text-slate-400 mb-2">(disk)</p>
						<ul class="space-y-1 text-sm text-slate-300">
							<li class="flex items-center gap-2">
								<span class="w-1 h-1 rounded-full bg-cyan-400/60" />
								Immutable
							</li>
							<li class="flex items-center gap-2">
								<span class="w-1 h-1 rounded-full bg-cyan-400/60" />
								CSR format
							</li>
							<li class="flex items-center gap-2">
								<span class="w-1 h-1 rounded-full bg-cyan-400/60" />
								Zero-copy
							</li>
						</ul>
					</div>

					{/* Plus sign */}
					<div class="hidden sm:flex items-center justify-center text-2xl text-slate-500 font-light">
						+
					</div>
					<div class="sm:hidden flex justify-center text-2xl text-slate-500 font-light">
						+
					</div>

					{/* Delta */}
					<div class="flex-1 rounded-xl border border-violet-500/40 bg-slate-800/50 p-4 relative">
						<div class="absolute -top-px left-4 right-4 h-px bg-gradient-to-r from-transparent via-violet-400/50 to-transparent" />
						<div class="flex items-center gap-2 mb-3">
							<svg
								class="w-5 h-5 text-violet-400"
								viewBox="0 0 24 24"
								fill="none"
								stroke="currentColor"
								stroke-width="2"
							>
								<path
									stroke-linecap="round"
									stroke-linejoin="round"
									d="M11 5H6a2 2 0 00-2 2v11a2 2 0 002 2h11a2 2 0 002-2v-5m-1.414-9.414a2 2 0 112.828 2.828L11.828 15H9v-2.828l8.586-8.586z"
								/>
							</svg>
							<h5 class="font-bold text-violet-400">Delta</h5>
						</div>
						<p class="text-xs text-slate-400 mb-2">(memory)</p>
						<ul class="space-y-1 text-sm text-slate-300">
							<li class="flex items-center gap-2">
								<span class="w-1 h-1 rounded-full bg-violet-400/60" />
								Pending changes
							</li>
							<li class="flex items-center gap-2">
								<span class="w-1 h-1 rounded-full bg-violet-400/60" />
								Fast writes
							</li>
							<li class="flex items-center gap-2">
								<span class="w-1 h-1 rounded-full bg-violet-400/60" />
								Merged on read
							</li>
						</ul>
					</div>

					{/* Arrow */}
					<div class="hidden sm:flex items-center justify-center text-slate-500">
						<svg
							class="w-6 h-6"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2"
						>
							<path
								stroke-linecap="round"
								stroke-linejoin="round"
								d="M14 5l7 7m0 0l-7 7m7-7H3"
							/>
						</svg>
					</div>
					<div class="sm:hidden flex justify-center text-slate-500">
						<svg
							class="w-6 h-6"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2"
						>
							<path
								stroke-linecap="round"
								stroke-linejoin="round"
								d="M19 14l-7 7m0 0l-7-7m7 7V3"
							/>
						</svg>
					</div>

					{/* WAL */}
					<div class="flex-1 rounded-xl border border-emerald-500/40 bg-slate-800/50 p-4 relative">
						<div class="absolute -top-px left-4 right-4 h-px bg-gradient-to-r from-transparent via-emerald-400/50 to-transparent" />
						<div class="flex items-center gap-2 mb-3">
							<svg
								class="w-5 h-5 text-emerald-400"
								viewBox="0 0 24 24"
								fill="none"
								stroke="currentColor"
								stroke-width="2"
							>
								<path
									stroke-linecap="round"
									stroke-linejoin="round"
									d="M9 12l2 2 4-4m5.618-4.016A11.955 11.955 0 0112 2.944a11.955 11.955 0 01-8.618 3.04A12.02 12.02 0 003 9c0 5.591 3.824 10.29 9 11.622 5.176-1.332 9-6.03 9-11.622 0-1.042-.133-2.052-.382-3.016z"
								/>
							</svg>
							<h5 class="font-bold text-emerald-400">WAL</h5>
						</div>
						<p class="text-xs text-slate-400 mb-2">(durability)</p>
						<ul class="space-y-1 text-sm text-slate-300">
							<li class="flex items-center gap-2">
								<span class="w-1 h-1 rounded-full bg-emerald-400/60" />
								Recovery log
							</li>
							<li class="flex items-center gap-2">
								<span class="w-1 h-1 rounded-full bg-emerald-400/60" />
								Crash safety
							</li>
							<li class="flex items-center gap-2">
								<span class="w-1 h-1 rounded-full bg-emerald-400/60" />
								Write-ahead
							</li>
						</ul>
					</div>
				</div>
			</div>
		</div>
	);
}

// Architecture diagram component with thematic styling
function ArchitectureDiagram() {
	return (
		<div class="my-8 space-y-3">
			{/* Query Layer */}
			<div class="relative rounded-2xl border border-cyan-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-6 shadow-lg shadow-cyan-500/10">
				<div class="absolute -top-px left-8 right-8 h-px bg-gradient-to-r from-transparent via-cyan-400 to-transparent" />
				<h3 class="text-xl font-bold text-cyan-400 mb-3">Query Layer</h3>
				<div class="flex items-start gap-4">
					<div class="flex-1">
						<div class="flex items-center gap-2 text-slate-300 mb-2">
							<svg
								class="w-5 h-5 text-cyan-400 fill-none stroke-current"
								viewBox="0 0 24 24"
								stroke-width="2"
							>
								<path
									stroke-linecap="round"
									stroke-linejoin="round"
									d="M13 10V3L4 14h7v7l9-11h-7z"
								/>
							</svg>
							<span>Fluent API, type inference, schema validation</span>
						</div>
						<code class="text-sm text-cyan-300/80 font-mono">
							db.insert(user).values({"{...}"})
						</code>
					</div>
					<div class="hidden sm:flex items-center gap-1 text-cyan-400/60">
						<svg
							class="w-5 h-5 fill-none stroke-current"
							viewBox="0 0 24 24"
							stroke-width="2"
						>
							<path
								stroke-linecap="round"
								stroke-linejoin="round"
								d="M19 14l-7 7m0 0l-7-7m7 7V3"
							/>
						</svg>
					</div>
				</div>
			</div>

			{/* Arrow connector */}
			<div class="flex justify-center">
				<svg
					class="w-6 h-6 text-cyan-400/50"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
				>
					<path
						stroke-linecap="round"
						stroke-linejoin="round"
						d="M19 14l-7 7m0 0l-7-7m7 7V3"
					/>
				</svg>
			</div>

			{/* Graph Layer */}
			<div class="relative rounded-2xl border border-cyan-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-6 shadow-lg shadow-cyan-500/10">
				<h3 class="text-xl font-bold text-cyan-400 mb-3">Graph Layer</h3>
				<div class="flex items-start gap-4">
					<div class="flex-1">
						<div class="flex items-center gap-2 text-slate-300 mb-2">
							<svg
								class="w-5 h-5 text-cyan-400 fill-none stroke-current"
								viewBox="0 0 24 24"
								stroke-width="2"
							>
								<path
									stroke-linecap="round"
									stroke-linejoin="round"
									d="M13.828 10.172a4 4 0 00-5.656 0l-4 4a4 4 0 105.656 5.656l1.102-1.101m-.758-4.899a4 4 0 005.656 0l4-4a4 4 0 00-5.656-5.656l-1.1 1.1"
								/>
							</svg>
							<span>Nodes, edges, traversal, transactions</span>
						</div>
						<code class="text-sm text-cyan-300/80 font-mono">
							createNode(), addEdge(), getNeighborsOut()
						</code>
					</div>
					{/* Mini graph visualization - using CSS circles */}
					<div class="hidden sm:flex items-center gap-1">
						<div class="flex flex-col items-center gap-1">
							<div class="w-3 h-3 rounded-full bg-cyan-400/80" />
							<div class="w-px h-3 bg-cyan-400/40" />
							<div class="w-3 h-3 rounded-full bg-cyan-400/80" />
						</div>
						<div class="flex flex-col gap-1">
							<div class="w-6 h-px bg-cyan-400/40" />
							<div class="w-6 h-px bg-cyan-400/40" />
						</div>
						<div class="flex flex-col items-center gap-1">
							<div class="w-3 h-3 rounded-full bg-cyan-400/80" />
							<div class="w-px h-3 bg-cyan-400/40" />
							<div class="w-3 h-3 rounded-full bg-cyan-400/80" />
						</div>
					</div>
				</div>
			</div>

			{/* Arrow connector */}
			<div class="flex justify-center">
				<svg
					class="w-6 h-6 text-cyan-400/50"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
				>
					<path
						stroke-linecap="round"
						stroke-linejoin="round"
						d="M19 14l-7 7m0 0l-7-7m7 7V3"
					/>
				</svg>
			</div>

			{/* Storage Layer */}
			<div class="relative rounded-2xl border border-cyan-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-6 shadow-lg shadow-cyan-500/10">
				<div class="absolute -bottom-px left-8 right-8 h-px bg-gradient-to-r from-transparent via-cyan-400 to-transparent" />
				<h3 class="text-xl font-bold text-cyan-400 mb-3">Storage Layer</h3>
				<div class="flex items-start gap-4">
					<div class="flex-1">
						<div class="flex items-center gap-2 text-slate-300 mb-2">
							<svg
								class="w-5 h-5 text-cyan-400 fill-none stroke-current"
								viewBox="0 0 24 24"
								stroke-width="2"
							>
								<path
									stroke-linecap="round"
									stroke-linejoin="round"
									d="M4 7v10c0 2.21 3.582 4 8 4s8-1.79 8-4V7M4 7c0 2.21 3.582 4 8 4s8-1.79 8-4M4 7c0-2.21 3.582-4 8-4s8 1.79 8 4"
								/>
							</svg>
							<span>Snapshot (CSR), Delta, WAL, Key Index</span>
						</div>
						<div class="flex items-center gap-2 text-slate-300">
							<svg
								class="w-5 h-5 text-cyan-400 fill-none stroke-current"
								viewBox="0 0 24 24"
								stroke-width="2"
							>
								<path
									stroke-linecap="round"
									stroke-linejoin="round"
									d="M9 12l2 2 4-4m5.618-4.016A11.955 11.955 0 0112 2.944a11.955 11.955 0 01-8.618 3.04A12.02 12.02 0 003 9c0 5.591 3.824 10.29 9 11.622 5.176-1.332 9-6.03 9-11.622 0-1.042-.133-2.052-.382-3.016z"
								/>
							</svg>
							<span>Memory-mapped files, crash recovery</span>
						</div>
					</div>
					{/* Storage icons */}
					<div class="hidden sm:flex gap-3 text-cyan-400/60">
						<svg
							class="w-8 h-8 fill-none stroke-current"
							viewBox="0 0 24 24"
							stroke-width="1.5"
						>
							<path
								stroke-linecap="round"
								stroke-linejoin="round"
								d="M19 11H5m14 0a2 2 0 012 2v6a2 2 0 01-2 2H5a2 2 0 01-2-2v-6a2 2 0 012-2m14 0V9a2 2 0 00-2-2M5 11V9a2 2 0 012-2m0 0V5a2 2 0 012-2h6a2 2 0 012 2v2M7 7h10"
							/>
						</svg>
						<svg
							class="w-8 h-8 fill-none stroke-current"
							viewBox="0 0 24 24"
							stroke-width="1.5"
						>
							<path
								stroke-linecap="round"
								stroke-linejoin="round"
								d="M21 21l-6-6m2-5a7 7 0 11-14 0 7 7 0 0114 0z"
							/>
						</svg>
					</div>
				</div>
			</div>
		</div>
	);
}

export const Route = createFileRoute("/docs/internals/$")({
	component: InternalsSplatPage,
});

function InternalsSplatPage() {
	const location = useLocation();
	const slug = () => {
		const path = location().pathname;
		const match = path.match(/^\/docs\/(.+)$/);
		return match ? match[1] : "";
	};
	const doc = () => findDocBySlug(slug());

	return (
		<Show when={doc()} fallback={<DocNotFound slug={slug()} />}>
			<DocPageContent slug={slug()} />
		</Show>
	);
}

function DocNotFound(props: { slug: string }) {
	return (
		<div class="max-w-4xl mx-auto px-6 py-12">
			<div class="text-center">
				<h1 class="text-4xl font-extrabold text-slate-900 dark:text-white mb-4">
					Page Not Found
				</h1>
				<p class="text-lg text-slate-600 dark:text-slate-400 mb-8">
					The internals page{" "}
					<code class="px-2 py-1 bg-slate-100 dark:bg-slate-800 rounded">
						{props.slug}
					</code>{" "}
					doesn't exist yet.
				</p>
				<a
					href="/docs"
					class="inline-flex items-center gap-2 px-6 py-3 bg-gradient-to-r from-cyan-500 to-violet-500 text-white font-semibold rounded-xl hover:shadow-lg hover:shadow-cyan-500/25 transition-all duration-200"
				>
					Back to Documentation
				</a>
			</div>
		</div>
	);
}

function DocPageContent(props: { slug: string }) {
	const slug = props.slug;

	// ============================================================================
	// ARCHITECTURE
	// ============================================================================
	if (slug === "internals/architecture") {
		return (
			<DocPage slug={slug}>
				<p>
					KiteDB is built as a layered system. Each layer has a specific job,
					and they work together to provide fast, reliable graph storage.
				</p>

				<h2 id="the-layers">The Three Layers</h2>

				<ArchitectureDiagram />

				<h3>Query Layer</h3>
				<p>
					This is what you interact with. It provides the Drizzle-style API with
					full TypeScript type inference. When you write{" "}
					<code>db.insert(user).values(...)</code>, the query layer validates
					your schema, converts TypeScript types to storage types, and calls
					into the graph layer.
				</p>

				<h3>Graph Layer</h3>
				<p>
					Manages the graph abstraction: nodes with properties, edges between
					nodes, and traversals. Handles transaction boundaries and coordinates
					reads between the snapshot and delta.
				</p>

				<h3>Storage Layer</h3>
				<p>
					The foundation. Stores data in a format optimized for graph
					operations. The key insight here is the{" "}
					<strong>Snapshot + Delta</strong> model, which separates immutable
					historical data from pending changes.
				</p>

				<h2 id="data-flow">What Happens When You Insert a Node</h2>

				<p>Let's trace through a simple insert:</p>

				<CodeBlock
					code={`await db.insert(user).values({ key: 'alice', name: 'Alice', age: 30 });`}
					language="typescript"
				/>

				<InsertDataFlow />

				<h2 id="read-path">What Happens When You Read</h2>

				<p>Reads merge data from two sources:</p>

				<CodeBlock
					code={`const alice = await db.get(user, 'alice');`}
					language="typescript"
				/>

				<ReadDataFlow />

				<h2 id="why-this-design">Why This Design</h2>

				<table>
					<thead>
						<tr>
							<th>Design Choice</th>
							<th>Benefit</th>
						</tr>
					</thead>
					<tbody>
						<tr>
							<td>Snapshot + Delta</td>
							<td>
								Reads don't block writes. Snapshot is immutable, delta is small.
							</td>
						</tr>
						<tr>
							<td>CSR format for edges</td>
							<td>Traversals read contiguous memory. CPU cache loves this.</td>
						</tr>
						<tr>
							<td>WAL for durability</td>
							<td>Committed data survives crashes. Recovery is fast.</td>
						</tr>
						<tr>
							<td>Single file</td>
							<td>Portable, atomic operations, simpler deployment.</td>
						</tr>
						<tr>
							<td>Memory-mapped I/O</td>
							<td>OS handles caching. Zero-copy reads.</td>
						</tr>
					</tbody>
				</table>

				<h2 id="next">Next Steps</h2>
				<ul>
					<li>
						<a href="/docs/internals/snapshot-delta">Snapshot + Delta</a> – The
						core storage model in detail
					</li>
					<li>
						<a href="/docs/internals/csr">CSR Format</a> – How edges are stored
					</li>
					<li>
						<a href="/docs/internals/single-file">Single-File Format</a> – The
						.kitedb file layout
					</li>
				</ul>
			</DocPage>
		);
	}

	// ============================================================================
	// SNAPSHOT + DELTA
	// ============================================================================
	if (slug === "internals/snapshot-delta") {
		return (
			<DocPage slug={slug}>
				<p>
					KiteDB separates storage into two parts: a <strong>snapshot</strong>{" "}
					(immutable, on disk) and a <strong>delta</strong> (mutable, in
					memory). This separation is the foundation of how KiteDB achieves fast
					reads and writes.
				</p>

				<h2 id="the-model">The Model</h2>

				<SnapshotDeltaModel />

				<h2 id="snapshot">Snapshot</h2>

				<p>
					The snapshot is a point-in-time image of the entire database. It's
					stored in <a href="/docs/internals/csr">CSR format</a> and
					memory-mapped directly from disk.
				</p>

				<p>
					<strong>Key properties:</strong>
				</p>
				<ul>
					<li>
						<strong>Immutable</strong> – Once written, never modified. Safe for
						concurrent reads.
					</li>
					<li>
						<strong>Zero-copy</strong> – Memory-mapped via <code>mmap()</code>.
						The OS handles caching.
					</li>
					<li>
						<strong>Compressed</strong> – zstd compression reduces disk usage by
						~60%.
					</li>
					<li>
						<strong>Complete</strong> – Contains all nodes, edges, properties,
						and indexes.
					</li>
				</ul>

				<h2 id="delta">Delta</h2>

				<p>
					The delta holds all changes since the last snapshot. It's a collection
					of in-memory data structures optimized for both reads and writes.
				</p>

				<DeltaStateStructure />

				<h2 id="reading">How Reads Work</h2>

				<p>Every read operation merges snapshot and delta:</p>

				<ReadFlowDiagram />

				<p>
					Edge traversals work similarly—scan snapshot edges, skip deleted ones,
					add new ones from delta.
				</p>

				<h2 id="writing">How Writes Work</h2>

				<p>Writes go to three places:</p>

				<WriteFlowDiagram />

				<h2 id="checkpoint">Checkpoint: Merging Delta into Snapshot</h2>

				<p>
					Periodically, KiteDB creates a new snapshot that incorporates all
					delta changes. This is called a <strong>checkpoint</strong>.
				</p>

				<CheckpointProcess />

				<p>
					During checkpoint, reads continue against the old snapshot + delta.
					The switch to the new snapshot is atomic.
				</p>

				<h2 id="why-it-works">Why This Works Well</h2>

				<table>
					<thead>
						<tr>
							<th>Property</th>
							<th>How It's Achieved</th>
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

				<h2 id="next">Next Steps</h2>
				<ul>
					<li>
						<a href="/docs/internals/csr">CSR Format</a> – How the snapshot
						stores edges
					</li>
					<li>
						<a href="/docs/internals/wal">WAL & Durability</a> – How the
						write-ahead log works
					</li>
					<li>
						<a href="/docs/internals/mvcc">MVCC & Transactions</a> – How
						concurrent access is handled
					</li>
				</ul>
			</DocPage>
		);
	}

	// ============================================================================
	// CSR FORMAT
	// ============================================================================
	if (slug === "internals/csr") {
		return (
			<DocPage slug={slug}>
				<p>
					KiteDB uses <strong>Compressed Sparse Row (CSR)</strong> format to
					store graph edges. CSR is a standard format for sparse matrices that
					provides fast traversal with minimal memory overhead.
				</p>

				<h2 id="the-problem">The Problem with Naive Edge Storage</h2>

				<p>Consider a graph with 100,000 nodes and 1 million edges.</p>

				<AdjacencyMatrixProblem />

				<LinkedListProblem />

				<h2 id="csr-solution">The CSR Solution</h2>

				<p>
					CSR stores all edges in two flat arrays: <strong>offsets</strong> and{" "}
					<strong>destinations</strong>. No pointers, no wasted space.
				</p>

				<CSRSolutionDiagram />

				<h2 id="traversal">How Traversal Works</h2>

				<p>Finding a node's neighbors is two array lookups:</p>

				<CSRTraversalExample />

				<h2 id="memory-layout">Why It's Fast: Memory Layout</h2>

				<MemoryLayoutComparison />

				<p class="text-sm text-slate-400">
					After the first access, B/C/D/A are already in CPU cache.
				</p>

				<h2 id="bidirectional">Bidirectional Edges</h2>

				<p>
					KiteDB stores edges in <strong>both directions</strong> for fast
					traversal either way:
				</p>

				<BidirectionalEdges />

				<h2 id="edge-types">Edge Types and Sorting</h2>

				<p>
					Real graphs have different edge types (follows, likes, knows). KiteDB
					stores edge types in a parallel array, sorted within each node:
				</p>

				<EdgeTypesSorting />

				<h2 id="existence-check">Edge Existence Check</h2>

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

				<h2 id="numbers">Performance Numbers</h2>

				<table>
					<thead>
						<tr>
							<th>Operation</th>
							<th>CSR</th>
							<th>Linked List</th>
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

				<h2 id="next">Next Steps</h2>
				<ul>
					<li>
						<a href="/docs/internals/snapshot-delta">Snapshot + Delta</a> – How
						CSR fits into the storage model
					</li>
					<li>
						<a href="/docs/internals/key-index">Key Index</a> – How node lookups
						work
					</li>
					<li>
						<a href="/docs/internals/performance">Performance</a> – Optimization
						techniques
					</li>
				</ul>
			</DocPage>
		);
	}

	// ============================================================================
	// SINGLE-FILE FORMAT
	// ============================================================================
	if (slug === "internals/single-file") {
		return (
			<DocPage slug={slug}>
				<p>
					KiteDB stores everything in a single <code>.kitedb</code> file. This
					makes databases portable, simplifies deployment, and enables atomic
					operations.
				</p>

				<h2 id="file-layout">File Layout</h2>

				<FileLayoutDiagram />

				<h2 id="header">The Header</h2>

				<p>
					The header is 4 KB and contains all metadata needed to open the
					database:
				</p>

				<HeaderContents />

				<h2 id="atomicity">Atomic Updates</h2>

				<p>
					The header enables atomic state transitions. A checkpoint works like
					this:
				</p>

				<AtomicCheckpointProcess />

				<h2 id="wal-area">WAL Area</h2>

				<p>The WAL area is a circular buffer divided into two regions:</p>

				<WALDualRegion />

				<h2 id="snapshot-area">Snapshot Area</h2>

				<p>The snapshot area holds the CSR-formatted graph data:</p>

				<SnapshotSections />

				<h2 id="growth">File Growth</h2>

				<p>The file grows in predictable ways:</p>

				<FileGrowthDiagram />

				<h2 id="vs-directory">Single-File vs Multi-File</h2>

				<p>
					KiteDB previously supported a directory-based format. Single-file is
					now the default:
				</p>

				<table>
					<thead>
						<tr>
							<th>Aspect</th>
							<th>Single-File</th>
							<th>Directory (legacy)</th>
						</tr>
					</thead>
					<tbody>
						<tr>
							<td>Portability</td>
							<td>Copy one file</td>
							<td>Copy entire directory</td>
						</tr>
						<tr>
							<td>Atomic ops</td>
							<td>Header flip</td>
							<td>Manifest + renames</td>
						</tr>
						<tr>
							<td>Disk usage</td>
							<td>~40% smaller</td>
							<td>More overhead</td>
						</tr>
						<tr>
							<td>Complexity</td>
							<td>Simpler</td>
							<td>More moving parts</td>
						</tr>
					</tbody>
				</table>

				<h2 id="opening">Opening a Database</h2>

				<DatabaseOpenProcess />

				<h2 id="next">Next Steps</h2>
				<ul>
					<li>
						<a href="/docs/internals/wal">WAL & Durability</a> – How the
						write-ahead log provides crash safety
					</li>
					<li>
						<a href="/docs/internals/snapshot-delta">Snapshot + Delta</a> – How
						reads merge these two sources
					</li>
				</ul>
			</DocPage>
		);
	}

	// ============================================================================
	// WAL & DURABILITY
	// ============================================================================
	if (slug === "internals/wal") {
		return (
			<DocPage slug={slug}>
				<p>
					The Write-Ahead Log (WAL) ensures that committed transactions survive
					crashes. Before any data is considered committed, it must be written
					to the WAL and flushed to disk.
				</p>

				<h2 id="principle">The WAL Principle</h2>

				<WALPrincipleDiagram />

				<h2 id="record-format">WAL Record Format</h2>

				<p>Each operation is stored as a framed record:</p>

				<WALRecordFormat />

				<h2 id="circular-buffer">Circular Buffer</h2>

				<p>
					The WAL is a fixed-size circular buffer. When it fills up, old
					(already checkpointed) data is overwritten:
				</p>

				<CircularBufferDiagram />

				<h2 id="dual-region">Dual-Region Design</h2>

				<p>The WAL is split into primary (75%) and secondary (25%) regions:</p>

				<WALDualRegionDetailed />

				<h2 id="fsync">Durability Guarantees</h2>

				<p>KiteDB provides configurable durability:</p>

				<DurabilityModes />

				<h2 id="recovery">Crash Recovery</h2>

				<p>On database open, the WAL is replayed to rebuild the delta:</p>

				<RecoveryProcess />

				<h2 id="checkpoint-trigger">When Checkpoint Happens</h2>

				<CheckpointTriggers />

				<h2 id="next">Next Steps</h2>
				<ul>
					<li>
						<a href="/docs/internals/single-file">Single-File Format</a> – How
						WAL fits in the file layout
					</li>
					<li>
						<a href="/docs/internals/snapshot-delta">Snapshot + Delta</a> – What
						checkpoint produces
					</li>
					<li>
						<a href="/docs/internals/mvcc">MVCC & Transactions</a> – How
						transactions work
					</li>
				</ul>
			</DocPage>
		);
	}

	// ============================================================================
	// MVCC & TRANSACTIONS
	// ============================================================================
	if (slug === "internals/mvcc") {
		return (
			<DocPage slug={slug}>
				<p>
					KiteDB supports concurrent transactions using{" "}
					<strong>Multi-Version Concurrency Control (MVCC)</strong>. Multiple
					readers can access the database simultaneously without blocking each
					other or writers.
				</p>

				<h2 id="isolation">Snapshot Isolation</h2>

				<p>
					Each transaction sees a consistent snapshot of the database as it
					existed when the transaction started. Other transactions' uncommitted
					changes are invisible.
				</p>

				<SnapshotIsolationTimeline />

				<h2 id="version-chains">Version Chains</h2>

				<p>
					When data is modified while readers exist, KiteDB keeps old versions
					in a chain:
				</p>

				<VersionChainDiagram />

				<h2 id="visibility">Visibility Rules</h2>

				<VisibilityRules />

				<h2 id="conflict-detection">Write Conflicts</h2>

				<p>
					KiteDB uses <strong>First-Committer-Wins</strong> to handle conflicts:
				</p>

				<WriteConflictDiagram />

				<CodeBlock
					code={`// Handling conflicts
try {
  await db.transaction(async () => {
    const alice = await db.get(user, 'alice');
    await db.update(user)
      .set({ age: alice.age + 1 })
      .where({ key: 'alice' });
  });
} catch (e) {
  if (e instanceof ConflictError) {
    // Another transaction modified alice
    // Retry with fresh data
  }
}`}
					language="typescript"
				/>

				<h2 id="lazy-versioning">Lazy Version Chains</h2>

				<p>
					Version chains are only created when necessary. If there are no
					concurrent readers, modifications happen in-place without versioning
					overhead.
				</p>

				<LazyMVCCDiagram />

				<h2 id="garbage-collection">Garbage Collection</h2>

				<p>Old versions are cleaned up when no transaction can see them:</p>

				<MVCCGarbageCollection />

				<h2 id="transaction-api">Transaction API</h2>

				<CodeBlock
					code={`// Explicit transaction
await db.transaction(async (ctx) => {
  const alice = await ctx.get(user, 'alice');
  await ctx.update(user).set({ age: alice.age + 1 }).where({ key: 'alice' });
  // Commits on successful return
  // Rolls back on exception
});

// Batch operations (single transaction)
await db.batch([
  db.insert(user).values({ key: 'bob', name: 'Bob' }),
  db.link(user, follows, user).from({ key: 'alice' }).to({ key: 'bob' }),
]);

// Without explicit transaction: each operation is auto-committed`}
					language="typescript"
				/>

				<h2 id="next">Next Steps</h2>
				<ul>
					<li>
						<a href="/docs/internals/wal">WAL & Durability</a> – How commits are
						made durable
					</li>
					<li>
						<a href="/docs/guides/transactions">Transactions Guide</a> –
						Practical usage patterns
					</li>
					<li>
						<a href="/docs/guides/concurrency">Concurrency Guide</a> –
						Multi-threaded access
					</li>
				</ul>
			</DocPage>
		);
	}

	// ============================================================================
	// KEY INDEX
	// ============================================================================
	if (slug === "internals/key-index") {
		return (
			<DocPage slug={slug}>
				<p>
					Every node in KiteDB can have a string key for lookup. The key index
					provides O(1) average-case lookups from key to NodeID.
				</p>

				<h2 id="the-problem">The Problem</h2>

				<KeyIndexProblem />

				<h2 id="structure">Index Structure</h2>

				<p>The key index uses hash buckets with linear probing:</p>

				<KeyIndexStructure />

				<h2 id="lookup">Lookup Process</h2>

				<KeyLookupProcess />

				<h2 id="two-level">Two-Level Lookup</h2>

				<p>
					The key index is split between delta (memory) and snapshot (disk):
				</p>

				<TwoLevelLookup />

				<h2 id="hashing">Why xxHash64</h2>

				<XxHash64Explanation />

				<h2 id="collisions">Handling Collisions</h2>

				<CollisionHandling />

				<h2 id="load-factor">Load Factor</h2>

				<LoadFactorDiagram />

				<h2 id="next">Next Steps</h2>
				<ul>
					<li>
						<a href="/docs/internals/snapshot-delta">Snapshot + Delta</a> – How
						the two-level lookup fits in
					</li>
					<li>
						<a href="/docs/internals/performance">Performance</a> – Index
						optimization techniques
					</li>
				</ul>
			</DocPage>
		);
	}

	// ============================================================================
	// PERFORMANCE
	// ============================================================================
	if (slug === "internals/performance") {
		return (
			<DocPage slug={slug}>
				<p>
					KiteDB is designed for speed. This page explains why it's fast and how
					to get the best performance from it.
				</p>

				<h2 id="why-fast">Why KiteDB is Fast</h2>

				<h3>1. No Network Overhead</h3>
				<NetworkOverheadComparison />
				<p class="text-sm text-slate-400 mb-6">
					<span class="text-emerald-400 font-bold">10-1000x</span> speedup just
					from eliminating network.
				</p>

				<h3>2. Zero-Copy Memory Mapping</h3>
				<ZeroCopyComparison />
				<p class="text-sm text-slate-400 mb-6">
					Hot data stays in RAM automatically. Cold data is paged in on demand.
				</p>

				<h3>3. Cache-Friendly Data Layout</h3>
				<CacheFriendlyComparison />

				<h3>4. Lazy MVCC</h3>
				<LazyMVCCComparison />

				<h2 id="benchmarks">Benchmark Results</h2>

				<p>
					Measured against Memgraph (a fast graph database) at 100K nodes / 1M
					edges:
				</p>

				<table>
					<thead>
						<tr>
							<th>Operation</th>
							<th>KiteDB</th>
							<th>Memgraph</th>
							<th>Speedup</th>
						</tr>
					</thead>
					<tbody>
						<tr>
							<td>Key lookup</td>
							<td>160ns</td>
							<td>100μs</td>
							<td>624x</td>
						</tr>
						<tr>
							<td>1-hop traversal</td>
							<td>1.9μs</td>
							<td>100μs</td>
							<td>52x</td>
						</tr>
						<tr>
							<td>Edge existence</td>
							<td>610ns</td>
							<td>100μs</td>
							<td>164x</td>
						</tr>
						<tr>
							<td>2-hop traversal</td>
							<td>397μs</td>
							<td>100ms</td>
							<td>252x</td>
						</tr>
						<tr>
							<td>Batch insert</td>
							<td>6.7ms</td>
							<td>10ms</td>
							<td>1.5x</td>
						</tr>
					</tbody>
				</table>

				<p>
					The large speedups come from eliminating network overhead and using
					cache-friendly data structures.
				</p>

				<h2 id="best-practices">Best Practices</h2>

				<h3>Batch Writes</h3>
				<CodeBlock
					code={`// Slow: Individual inserts (1 WAL sync per operation)
for (const user of users) {
  await db.insert(userSchema).values(user);
}
// 1000 users × 1ms sync = 1000ms

// Fast: Batch insert (1 WAL sync for all)
await db.insert(userSchema).values(users);
// 1000 users × 1μs + 1ms sync = ~2ms

Speedup: 500x for bulk operations`}
					language="typescript"
				/>

				<h3>Limit Traversal Depth</h3>
				<CodeBlock
					code={`// Dangerous: Unbounded traversal
const all = await db.from(user)
  .traverse().out(follows)
  .nodes().toArray();
// Could return millions of nodes

// Safe: Bounded traversal
const friends = await db.from(user)
  .traverse().out(follows)
  .depth({ max: 2 })
  .limit(100)
  .nodes().toArray();
// Returns at most 100 nodes, max 2 hops`}
					language="typescript"
				/>

				<h3>Use Keys for Lookups</h3>
				<CodeBlock
					code={`// Fast: Key lookup (O(1) hash index)
const alice = await db.get(user, 'alice');

// Slower: Property scan (O(n) nodes)
const alice = await db.from(user)
  .where({ name: 'Alice' })
  .first();

Design keys to match your access patterns.`}
					language="typescript"
				/>

				<h3>Checkpoint Timing</h3>
				<CodeBlock
					code={`// Default: Automatic checkpoint when WAL fills
// Good for most workloads

// For write-heavy bursts: Manual checkpoint after
await importLargeDataset();
await db.checkpoint();  // Consolidate before queries

// For read-heavy: Larger WAL, less frequent checkpoints
const db = await kite('./mydb', {
  walSize: 256 * 1024 * 1024,  // 256MB WAL
});`}
					language="typescript"
				/>

				<h2 id="memory">Memory Usage</h2>

				<MemoryUsageBreakdown />

				<h2 id="profiling">Profiling Tips</h2>

				<CodeBlock
					code={`// Get database statistics
const stats = await db.stats();
console.log(stats);
// {
//   nodes: 100000,
//   edges: 500000,
//   snapshotSize: 10485760,
//   deltaSize: 524288,
//   walUsage: 0.45
// }

// If walUsage is consistently high:
// → Checkpoint more frequently or increase WAL size

// If deltaSize is large:
// → Checkpoint to consolidate into snapshot`}
					language="typescript"
				/>

				<h2 id="next">Next Steps</h2>
				<ul>
					<li>
						<a href="/docs/internals/csr">CSR Format</a> – Why traversals are
						fast
					</li>
					<li>
						<a href="/docs/internals/snapshot-delta">Snapshot + Delta</a> – How
						reads stay fast during writes
					</li>
					<li>
						<a href="/docs/benchmarks">Benchmarks</a> – Detailed performance
						measurements
					</li>
				</ul>
			</DocPage>
		);
	}

	// Default fallback
	return (
		<DocPage slug={slug}>
			<p>This internals documentation is coming soon.</p>
		</DocPage>
	);
}
