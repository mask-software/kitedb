import { ArrowRight } from "lucide-solid";
import { For } from "solid-js";
import { SectionHeading } from "./section-heading";

type Path = "write" | "read" | "checkpoint";

interface Box {
	x: number;
	y: number;
	w: number;
	h: number;
	title: string;
	sub: string[];
}

const PROCESS_BOXES: Box[] = [
	{
		x: 40,
		y: 72,
		w: 190,
		h: 128,
		title: "Your code",
		sub: ["db.from(alice)", "  .out('wrote')"],
	},
	{
		x: 270,
		y: 72,
		w: 190,
		h: 128,
		title: "Query engine",
		sub: ["traversal, paths,", "vector search"],
	},
	{
		x: 490,
		y: 72,
		w: 200,
		h: 128,
		title: "Delta overlay",
		sub: ["recent writes,", "in memory"],
	},
	{
		x: 720,
		y: 72,
		w: 200,
		h: 128,
		title: "MVCC read view",
		sub: ["snapshot ⊕ delta,", "per transaction"],
	},
];

const DISK_BOXES: Box[] = [
	{ x: 40, y: 312, w: 150, h: 72, title: "Header", sub: ["checksummed"] },
	{
		x: 210,
		y: 312,
		w: 330,
		h: 72,
		title: "Write-ahead log",
		sub: ["append-only, CRC-32 per record"],
	},
	{
		x: 560,
		y: 312,
		w: 360,
		h: 72,
		title: "CSR snapshot",
		sub: ["memory-mapped adjacency arrays"],
	},
];

interface Flow {
	d: string;
	path: Path;
	label?: { text: string; x: number; y: number };
}

const FLOWS: Flow[] = [
	{ d: "M230 136 H270", path: "read" },
	{
		d: "M820 72 V52 H365 V72",
		path: "read",
		label: { text: "results", x: 592, y: 52 },
	},
	{ d: "M690 136 H720", path: "read" },
	{ d: "M840 312 V200", path: "read", label: { text: "mmap", x: 840, y: 256 } },
	{
		d: "M365 200 V312",
		path: "write",
		label: { text: "commit", x: 365, y: 256 },
	},
	{
		d: "M520 312 V200",
		path: "write",
		label: { text: "apply", x: 520, y: 256 },
	},
	{
		d: "M650 200 V312",
		path: "checkpoint",
		label: { text: "checkpoint", x: 650, y: 256 },
	},
];

const FLOW_LABELS = FLOWS.flatMap((flow) =>
	flow.label ? [{ path: flow.path, ...flow.label }] : [],
);

const STAGES: {
	path: Path;
	title: string;
	body: string;
	link: { label: string; href: string };
}[] = [
	{
		path: "write",
		title: "Write path",
		body: "A commit appends checksummed records to the write-ahead log, then applies to the in-memory delta. After a crash, the log is replayed on open.",
		link: { label: "Write-ahead log", href: "/docs/internals/wal" },
	},
	{
		path: "read",
		title: "Read path",
		body: "Queries read a memory-mapped CSR snapshot merged with the delta, through an MVCC view, so each transaction sees one consistent state.",
		link: { label: "Snapshot + delta", href: "/docs/internals/snapshot-delta" },
	},
	{
		path: "checkpoint",
		title: "Checkpoints",
		body: "Periodically the delta is folded into a fresh CSR snapshot, keeping adjacency contiguous and the log short.",
		link: { label: "Single-file format", href: "/docs/internals/single-file" },
	},
];

function DiagramBox(props: { box: Box; tone: "process" | "disk" }) {
	return (
		<g transform={`translate(${props.box.x} ${props.box.y})`}>
			<rect
				class="arch-box"
				data-tone={props.tone}
				width={props.box.w}
				height={props.box.h}
				rx="12"
			/>
			<text class="arch-title" x="18" y={props.tone === "disk" ? 30 : 36}>
				{props.box.title}
			</text>
			<For each={props.box.sub}>
				{(line, i) => (
					<text
						class="arch-sub"
						x="18"
						y={(props.tone === "disk" ? 52 : 70) + i() * 19}
					>
						{line}
					</text>
				)}
			</For>
		</g>
	);
}

export function Architecture() {
	return (
		<section class="py-28 sm:py-36" aria-labelledby="architecture-heading">
			<div class="mx-auto max-w-6xl px-5 sm:px-8">
				<SectionHeading
					id="architecture-heading"
					eyebrow="Under the hood"
					title="How the storage engine works"
				>
					The file format and storage engine are designed for graph workloads.
					Everything in this diagram runs inside your application process.
				</SectionHeading>

				<figure class="reveal mt-16">
					<div class="arch-frame hidden rounded-2xl border border-kite-line bg-kite-surface/40 p-4 md:block lg:p-6">
						<svg
							class="arch-diagram block h-auto w-full"
							viewBox="0 0 960 408"
							role="img"
							aria-labelledby="arch-title arch-desc"
						>
							<title id="arch-title">KiteDB architecture</title>
							<desc id="arch-desc">
								Inside your process, your code calls the query engine. Commits
								append to the write-ahead log in the database file, then apply
								to an in-memory delta overlay. Reads go through an MVCC view
								that merges the delta with a memory-mapped CSR snapshot.
								Checkpoints fold the delta into a new snapshot.
							</desc>

							<rect
								class="arch-region"
								x="16"
								y="16"
								width="928"
								height="212"
								rx="18"
							/>
							<text class="arch-region-label" x="40" y="44">
								your process
							</text>
							<rect
								class="arch-region"
								data-tone="disk"
								x="16"
								y="268"
								width="928"
								height="128"
								rx="18"
							/>
							<text class="arch-region-label" x="40" y="296">
								app.kitedb · one file on disk
							</text>

							<For each={FLOWS}>
								{(flow) => (
									<g class="arch-flow" data-path={flow.path}>
										<path class="arch-flow-base" d={flow.d} />
										<path class="arch-flow-dash" d={flow.d} />
									</g>
								)}
							</For>

							<For each={PROCESS_BOXES}>
								{(box) => <DiagramBox box={box} tone="process" />}
							</For>
							<For each={DISK_BOXES}>
								{(box) => <DiagramBox box={box} tone="disk" />}
							</For>

							<For each={FLOW_LABELS}>
								{(label) => (
									<g class="arch-label" data-path={label.path}>
										<rect
											x={label.x - (label.text.length * 3.6 + 10)}
											y={label.y - 10}
											width={label.text.length * 7.2 + 20}
											height="20"
											rx="10"
										/>
										<text x={label.x} y={label.y + 4} text-anchor="middle">
											{label.text}
										</text>
									</g>
								)}
							</For>
						</svg>
					</div>

					{/* Compact, linear version for small screens */}
					<ol class="space-y-3 md:hidden">
						<li class="rounded-xl border border-kite-line bg-kite-surface/40 p-4 text-[14px] leading-relaxed text-slate-400">
							<span class="font-medium text-white">Commit</span>: your code →
							write-ahead log (CRC-32) → in-memory delta
						</li>
						<li class="rounded-xl border border-kite-line bg-kite-surface/40 p-4 text-[14px] leading-relaxed text-slate-400">
							<span class="font-medium text-white">Read</span>: memory-mapped
							CSR snapshot ⊕ delta, through an MVCC view
						</li>
						<li class="rounded-xl border border-kite-line bg-kite-surface/40 p-4 text-[14px] leading-relaxed text-slate-400">
							<span class="font-medium text-white">Checkpoint</span>: delta
							folded into a fresh snapshot
						</li>
					</ol>
				</figure>

				<div class="mt-12 grid gap-10 md:grid-cols-3 md:gap-8">
					<For each={STAGES}>
						{(stage) => (
							<div class="reveal">
								<div class="flex items-center gap-3">
									<span
										class="arch-key"
										data-path={stage.path}
										aria-hidden="true"
									/>
									<h3 class="text-[16px] font-semibold text-white">
										{stage.title}
									</h3>
								</div>
								<p class="mt-3 text-[15px] leading-relaxed text-slate-400">
									{stage.body}
								</p>
								<a
									href={stage.link.href}
									class="group mt-4 inline-flex items-center gap-1.5 text-[14px] font-medium text-slate-300 transition-colors hover:text-white"
								>
									{stage.link.label}
									<ArrowRight
										size={14}
										class="transition-transform group-hover:translate-x-0.5"
										aria-hidden="true"
									/>
								</a>
							</div>
						)}
					</For>
				</div>
			</div>
		</section>
	);
}
