import { Pause, Play } from "lucide-solid";
import {
	batch,
	createEffect,
	createMemo,
	createSignal,
	For,
	Index,
	on,
	onCleanup,
	onMount,
	Show,
} from "solid-js";
import { selectedLanguage } from "~/lib/language-store";
import { CodeView } from "~/components/code-view";
import { FILE_EXT, SCENES, SHIKI_LANG } from "./examples";
import { InlineCode } from "~/components/inline-code";
import { LanguageToggle } from "~/components/language-toggle";

// ---------------------------------------------------------------------------
// Graph model: a tiny knowledge graph of authors, documents, and topics.
// ---------------------------------------------------------------------------

type NodeKind = "user" | "doc" | "topic";
type LabelPos = "above" | "below" | "right";

interface GraphNode {
	id: string;
	label: string;
	kind: NodeKind;
	x: number;
	y: number;
	labelPos: LabelPos;
}

interface GraphEdge {
	from: string;
	to: string;
	type: "wrote" | "discusses" | "knows";
	/** Show the edge type name beside this edge; defaults to above its midpoint */
	labeled?: boolean;
	labelAt?: { x: number; y: number; anchor: "start" | "middle" | "end" };
}

const NODES: GraphNode[] = [
	{
		id: "alice",
		label: "alice",
		kind: "user",
		x: 64,
		y: 200,
		labelPos: "below",
	},
	{ id: "bob", label: "bob", kind: "user", x: 92, y: 338, labelPos: "below" },
	{
		id: "csr",
		label: "csr-layout",
		kind: "doc",
		x: 236,
		y: 88,
		labelPos: "above",
	},
	{
		id: "wal",
		label: "wal-recovery",
		kind: "doc",
		x: 252,
		y: 200,
		labelPos: "below",
	},
	{
		id: "recall",
		label: "ivf-recall",
		kind: "doc",
		x: 238,
		y: 300,
		labelPos: "below",
	},
	{
		id: "pq",
		label: "pq-codes",
		kind: "doc",
		x: 222,
		y: 378,
		labelPos: "below",
	},
	{
		id: "graphs",
		label: "graphs",
		kind: "topic",
		x: 432,
		y: 70,
		labelPos: "right",
	},
	{
		id: "storage",
		label: "storage",
		kind: "topic",
		x: 446,
		y: 176,
		labelPos: "right",
	},
	{
		id: "search",
		label: "search",
		kind: "topic",
		x: 438,
		y: 286,
		labelPos: "right",
	},
	{ id: "ml", label: "ml", kind: "topic", x: 430, y: 372, labelPos: "right" },
];

const EDGES: GraphEdge[] = [
	{ from: "alice", to: "csr", type: "wrote" },
	{ from: "alice", to: "wal", type: "wrote", labeled: true },
	{ from: "alice", to: "recall", type: "wrote" },
	{
		from: "alice",
		to: "bob",
		type: "knows",
		labeled: true,
		labelAt: { x: 70, y: 273, anchor: "end" },
	},
	{ from: "bob", to: "pq", type: "wrote" },
	{ from: "csr", to: "graphs", type: "discusses" },
	{ from: "csr", to: "storage", type: "discusses" },
	{ from: "wal", to: "storage", type: "discusses", labeled: true },
	{ from: "recall", to: "search", type: "discusses" },
	{ from: "pq", to: "search", type: "discusses" },
	{ from: "pq", to: "ml", type: "discusses" },
];

const QUERY_POINT = { x: 92, y: 74 };
const DOCS = ["csr", "wal", "recall", "pq"];
const SIMILARITY: Record<string, string> = {
	wal: "0.93",
	csr: "0.81",
	recall: "0.74",
};
// Keep `in` out of JSX attributes: Solid's SSR escapes each operand of an
// attribute expression, and from 1.9.15 that stringifies objects.
const isHit = (id: string) => id in SIMILARITY;

const nodeById = new Map(NODES.map((n) => [n.id, n]));
function graphNode(id: string): GraphNode {
	const node = nodeById.get(id);
	if (!node) throw new Error(`Unknown graph node: ${id}`);
	return node;
}
const edgeKey = (e: GraphEdge) => `${e.from}->${e.to}`;

/** Line segment between two points, trimmed so it stops short of node shapes. */
function trimmedSegment(
	a: { x: number; y: number },
	b: { x: number; y: number },
	trimStart = 13,
	trimEnd = 14,
) {
	const dx = b.x - a.x;
	const dy = b.y - a.y;
	const len = Math.hypot(dx, dy);
	const ux = dx / len;
	const uy = dy / len;
	return {
		x1: a.x + ux * trimStart,
		y1: a.y + uy * trimStart,
		x2: b.x - ux * trimEnd,
		y2: b.y - uy * trimEnd,
	};
}

// ---------------------------------------------------------------------------
// Scene state: which nodes/edges are lit at each step.
// ---------------------------------------------------------------------------

type NodeState =
	| "idle"
	| "muted"
	| "visited"
	| "frontier"
	| "result"
	| "candidate"
	| "hit";

interface Frame {
	nodes: Record<string, NodeState>;
	/** Nodes that light up this step; they wait for their edges to draw. */
	arriving: Set<string>;
	litEdges: Set<string>;
	probes: "hidden" | "scanning" | "ranked";
	showQuery: boolean;
}

const TRAVERSAL_FRONTIERS: string[][] = [
	["alice"],
	["csr", "wal", "recall"],
	["graphs", "storage", "search"],
];
const TRAVERSAL_EDGES: string[][] = [
	[],
	["alice->csr", "alice->wal", "alice->recall"],
	["csr->graphs", "csr->storage", "wal->storage", "recall->search"],
];

function traverseFrame(step: number): Frame {
	const nodes: Record<string, NodeState> = {};
	const litEdges = new Set<string>();
	const hop = Math.min(step, TRAVERSAL_FRONTIERS.length - 1);
	for (let h = 0; h <= hop; h++) {
		for (const id of TRAVERSAL_FRONTIERS[h]) {
			nodes[id] = h === hop ? "frontier" : "visited";
		}
		for (const key of TRAVERSAL_EDGES[h]) litEdges.add(key);
	}
	const isResult = step >= TRAVERSAL_FRONTIERS.length;
	if (isResult) {
		for (const id of TRAVERSAL_FRONTIERS[hop]) nodes[id] = "result";
	}
	return {
		nodes,
		arriving: new Set(isResult ? [] : TRAVERSAL_FRONTIERS[hop]),
		litEdges,
		probes: "hidden",
		showQuery: false,
	};
}

function vectorFrame(step: number): Frame {
	const nodes: Record<string, NodeState> = {};
	for (const n of NODES) nodes[n.id] = n.kind === "doc" ? "idle" : "muted";
	if (step >= 1) for (const id of DOCS) nodes[id] = "candidate";
	if (step >= 2) {
		for (const id of DOCS) nodes[id] = isHit(id) ? "hit" : "muted";
	}
	return {
		nodes,
		arriving: new Set(),
		litEdges: new Set(),
		probes: step === 0 ? "hidden" : step === 1 ? "scanning" : "ranked",
		showQuery: true,
	};
}

const STEP_MS = 1500;
const HOLD_MS = 2800;

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export function QueryTheater() {
	const [sceneIndex, setSceneIndex] = createSignal(0);
	const [rawStep, setStep] = createSignal(0);
	const [playing, setPlaying] = createSignal(false);
	const [inView, setInView] = createSignal(true);
	let root: HTMLDivElement | undefined;

	const scene = () => SCENES[sceneIndex()];
	const stepCount = () => scene().captions.length;
	// Clamped so a scene change can never observe the previous scene's step
	const step = () => Math.min(rawStep(), stepCount() - 1);
	const isLastStep = () => step() === stepCount() - 1;
	const lang = () => selectedLanguage().id;
	const sceneCode = () => scene().code[lang()];

	const frame = createMemo<Frame>(() =>
		scene().id === "traverse" ? traverseFrame(step()) : vectorFrame(step()),
	);

	const goToScene = (index: number) => {
		batch(() => {
			setSceneIndex(index);
			setStep(0);
		});
	};

	const advance = () => {
		if (!isLastStep()) {
			setStep(step() + 1);
			return;
		}
		goToScene((sceneIndex() + 1) % SCENES.length);
	};

	onMount(() => {
		const reduceMotion = window.matchMedia(
			"(prefers-reduced-motion: reduce)",
		).matches;
		if (reduceMotion) {
			setStep(stepCount() - 1);
		} else {
			setPlaying(true);
		}

		if (root && "IntersectionObserver" in window) {
			const observer = new IntersectionObserver(
				([entry]) => setInView(entry.isIntersecting),
				{ threshold: 0.2 },
			);
			observer.observe(root);
			onCleanup(() => observer.disconnect());
		}
	});

	// One timer per (scene, step); re-armed whenever either changes.
	createEffect(
		on([playing, inView, sceneIndex, step], ([isPlaying, visible]) => {
			if (!isPlaying || !visible) return;
			const timer = setTimeout(advance, isLastStep() ? HOLD_MS : STEP_MS);
			onCleanup(() => clearTimeout(timer));
		}),
	);

	return (
		<div
			ref={root}
			data-scene={scene().id}
			class="theater relative overflow-hidden rounded-2xl border border-kite-line bg-kite-surface/90 shadow-[0_40px_120px_-40px_rgba(0,0,0,0.9)] backdrop-blur-xl"
		>
			{/* Top bar: scenes, progress, playback */}
			<div class="flex flex-wrap items-center gap-3 border-b border-kite-line px-3 py-2.5 sm:px-4">
				<div
					class="flex items-center gap-1"
					role="tablist"
					aria-label="Query examples"
				>
					<Index each={SCENES}>
						{(s, index) => (
							<button
								type="button"
								role="tab"
								aria-selected={sceneIndex() === index}
								class="rounded-lg px-3 py-1.5 text-[13px] font-medium text-slate-500 transition-colors duration-150 hover:text-slate-200 aria-selected:bg-white/[0.06] aria-selected:text-white"
								onClick={() => goToScene(index)}
							>
								<span
									class="mr-2 inline-block h-1.5 w-1.5 rounded-full align-middle"
									classList={{
										"bg-kite-cyan": s().id === "traverse",
										"bg-kite-violet": s().id === "vector",
									}}
									aria-hidden="true"
								/>
								{s().label}
							</button>
						)}
					</Index>
				</div>

				<div class="ml-auto flex items-center gap-3">
					<div class="hidden items-center gap-1 sm:flex" aria-hidden="true">
						<For each={Array.from({ length: stepCount() }, (_, i) => i)}>
							{(i) => (
								<button
									type="button"
									tabIndex={-1}
									class="group relative h-5 w-7"
									onClick={() => {
										setPlaying(false);
										setStep(i);
									}}
								>
									<span class="absolute inset-x-0 top-1/2 h-[3px] -translate-y-1/2 overflow-hidden rounded-full bg-white/[0.08]">
										<span
											class="theater-progress absolute inset-y-0 left-0 rounded-full"
											classList={{
												"bg-kite-cyan": scene().id === "traverse",
												"bg-kite-violet": scene().id === "vector",
											}}
											data-state={
												i < step()
													? "done"
													: i === step()
														? playing() && inView()
															? "running"
															: "done"
														: "todo"
											}
											style={{
												"animation-duration": `${isLastStep() ? HOLD_MS : STEP_MS}ms`,
											}}
										/>
									</span>
								</button>
							)}
						</For>
					</div>
					<button
						type="button"
						class="grid h-7 w-7 place-items-center rounded-md text-slate-500 transition-colors hover:bg-white/[0.06] hover:text-slate-200"
						onClick={() => setPlaying(!playing())}
						aria-label={playing() ? "Pause animation" : "Play animation"}
					>
						<Show when={playing()} fallback={<Play size={13} />}>
							<Pause size={13} />
						</Show>
					</button>
				</div>
			</div>

			<div class="grid lg:grid-cols-[minmax(0,0.95fr)_minmax(0,1.05fr)]">
				{/* Code */}
				<div class="flex min-w-0 flex-col border-b border-kite-line lg:border-b-0 lg:border-r">
					<div class="flex items-center justify-between gap-3 px-4 pt-3.5 pb-1 sm:px-5">
						<span class="font-mono text-[12px] text-slate-500">
							{scene().file}.{FILE_EXT[lang()]}
						</span>
						<LanguageToggle />
					</div>
					<div class="min-h-[208px] flex-1 overflow-x-auto px-1 py-3 sm:px-2">
						<CodeView
							code={sceneCode().code}
							lang={SHIKI_LANG[lang()]}
							activeLines={sceneCode().stepLines[step()]}
						/>
					</div>
					<div class="border-t border-kite-line px-4 py-3.5 sm:px-5">
						<div class="mb-1.5 font-mono text-[11px] uppercase tracking-[0.14em] text-slate-600">
							Step {step() + 1} / {stepCount()}
						</div>
						<p class="min-h-[2.75rem] text-[14px] leading-snug text-slate-300">
							<InlineCode text={scene().captions[step()]} />
						</p>
					</div>
				</div>

				{/* Graph */}
				<div class="relative min-w-0">
					<div
						class="pointer-events-none absolute inset-0 theater-grid"
						aria-hidden="true"
					/>
					<svg
						class="theater-graph relative mx-auto block h-auto w-full max-w-[560px] px-2 py-3"
						viewBox="0 0 520 420"
						role="img"
						aria-label={
							scene().id === "traverse"
								? "Graph traversal from alice through wrote edges to documents, then discusses edges to topics"
								: "Vector search ranking documents by similarity to a query embedding"
						}
					>
						{/* Edges */}
						<g class="qt-edges">
							<For each={EDGES}>
								{(edge) => {
									const seg = trimmedSegment(
										graphNode(edge.from),
										graphNode(edge.to),
									);
									const lit = () => frame().litEdges.has(edgeKey(edge));
									return (
										<g class="qt-edge" data-lit={lit()}>
											<line
												class="qt-edge-base"
												x1={seg.x1}
												y1={seg.y1}
												x2={seg.x2}
												y2={seg.y2}
											/>
											<path
												class="qt-edge-glow"
												d={`M${seg.x1} ${seg.y1} L${seg.x2} ${seg.y2}`}
												pathLength={1}
											/>
											<Show when={edge.labeled}>
												<text
													class="qt-edge-label"
													x={edge.labelAt?.x ?? (seg.x1 + seg.x2) / 2}
													y={edge.labelAt?.y ?? (seg.y1 + seg.y2) / 2 - 7}
													text-anchor={edge.labelAt?.anchor ?? "middle"}
												>
													{edge.type}
												</text>
											</Show>
										</g>
									);
								}}
							</For>
						</g>

						{/* Vector probes from the query point to each document */}
						<g class="qt-probes" data-state={frame().probes}>
							<For each={DOCS}>
								{(id) => {
									const seg = trimmedSegment(
										QUERY_POINT,
										graphNode(id),
										10,
										14,
									);
									return (
										<line
											class="qt-probe"
											data-hit={isHit(id)}
											x1={seg.x1}
											y1={seg.y1}
											x2={seg.x2}
											y2={seg.y2}
										/>
									);
								}}
							</For>
						</g>

						{/* Query embedding point */}
						<g
							class="qt-query"
							data-visible={frame().showQuery}
							transform={`translate(${QUERY_POINT.x} ${QUERY_POINT.y})`}
						>
							<circle class="qt-query-halo" r="16" />
							<circle class="qt-query-core" r="5" />
							<text
								class="qt-label qt-query-label"
								y="-22"
								text-anchor="middle"
							>
								query
							</text>
						</g>

						{/* Nodes */}
						<g class="qt-nodes">
							<For each={NODES}>
								{(node) => {
									const state = () => frame().nodes[node.id] ?? "idle";
									const arriving = () => frame().arriving.has(node.id);
									const labelProps = () => {
										switch (node.labelPos) {
											case "above":
												return {
													x: 0,
													y: -18,
													"text-anchor": "middle" as const,
												};
											case "right":
												return { x: 24, y: 4, "text-anchor": "start" as const };
											default:
												return {
													x: 0,
													y: 26,
													"text-anchor": "middle" as const,
												};
										}
									};
									return (
										<g
											class="qt-node"
											data-kind={node.kind}
											data-state={state()}
											data-arriving={arriving()}
											transform={`translate(${node.x} ${node.y})`}
										>
											<circle class="qt-ring" r="17" />
											<Show when={node.kind === "user"}>
												<circle class="qt-shape" r="8.5" />
											</Show>
											<Show when={node.kind === "doc"}>
												<rect
													class="qt-shape"
													x="-7.5"
													y="-7.5"
													width="15"
													height="15"
													rx="3.5"
												/>
											</Show>
											<Show when={node.kind === "topic"}>
												<rect
													class="qt-shape"
													x="-6.5"
													y="-6.5"
													width="13"
													height="13"
													rx="2"
													transform="rotate(45)"
												/>
											</Show>
											<text class="qt-label" {...labelProps()}>
												{node.label}
											</text>
											<Show when={SIMILARITY[node.id]}>
												{(score) => (
													<text
														class="qt-score"
														x="22"
														y={node.labelPos === "above" ? 4 : -12}
													>
														{score()}
													</text>
												)}
											</Show>
										</g>
									);
								}}
							</For>
						</g>
					</svg>

					<div
						class="flex items-center justify-center gap-4 pb-3 font-mono text-[11px] text-slate-500"
						aria-hidden="true"
					>
						<span class="flex items-center gap-1.5">
							<svg class="h-2.5 w-2.5" viewBox="0 0 10 10" aria-hidden="true">
								<circle
									cx="5"
									cy="5"
									r="4"
									class="fill-none stroke-slate-500"
								/>
							</svg>
							user
						</span>
						<span class="flex items-center gap-1.5">
							<svg class="h-2.5 w-2.5" viewBox="0 0 10 10" aria-hidden="true">
								<rect
									x="1"
									y="1"
									width="8"
									height="8"
									rx="2"
									class="fill-none stroke-slate-500"
								/>
							</svg>
							document
						</span>
						<span class="flex items-center gap-1.5">
							<svg class="h-2.5 w-2.5" viewBox="0 0 10 10" aria-hidden="true">
								<rect
									x="2"
									y="2"
									width="6"
									height="6"
									rx="1"
									transform="rotate(45 5 5)"
									class="fill-none stroke-slate-500"
								/>
							</svg>
							topic
						</span>
					</div>
				</div>
			</div>

			{/* Output */}
			<div class="flex min-h-[52px] flex-wrap items-center gap-x-3 gap-y-2 border-t border-kite-line bg-black/20 px-4 py-3 font-mono text-[12.5px] sm:px-5">
				<span class="text-slate-600" aria-hidden="true">
					→
				</span>
				<Show
					when={isLastStep()}
					fallback={<span class="text-slate-600">running…</span>}
				>
					<span class="text-slate-500">{scene().result.label}</span>
					<For each={scene().result.items}>
						{(item, i) => (
							<span
								class="theater-chip inline-flex items-center gap-2 rounded-md border border-white/[0.08] bg-white/[0.03] px-2 py-0.5 text-slate-200"
								style={{ "animation-delay": `${i() * 90}ms` }}
							>
								{item.text}
								<Show when={item.meta}>
									<span class="text-slate-500">{item.meta}</span>
								</Show>
							</span>
						)}
					</For>
				</Show>
			</div>
		</div>
	);
}
