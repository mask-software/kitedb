import { ArrowRight, ArrowUpRight, ChartScatter, Table2 } from "lucide-solid";
import { createSignal, For, Show } from "solid-js";
import { GITHUB_URL } from "~/components/github-icon";
import {
	BENCH_ENVIRONMENT,
	formatNs,
	LATENCY_ROWS,
	type LatencyRow,
} from "~/lib/benchmarks";
import { SectionHeading } from "./section-heading";

// Log scale spanning exactly five decades: 30 ns → 3 ms.
const DOMAIN_MIN = 30;
const DOMAIN_MAX = 3_000_000;
const TICKS = [100, 1_000, 10_000, 100_000, 1_000_000];

const xPercent = (ns: number) =>
	((Math.log10(ns) - Math.log10(DOMAIN_MIN)) /
		(Math.log10(DOMAIN_MAX) - Math.log10(DOMAIN_MIN))) *
	100;

const tickLabel = (ns: number) =>
	ns >= 1_000_000 ? "1 ms" : ns >= 1_000 ? `${ns / 1_000} µs` : `${ns} ns`;

function LatencyChart() {
	const [hovered, setHovered] = createSignal<number | null>(null);

	return (
		<div class="lat-chart relative">
			{/* Gridlines live in one layer behind the plot column */}
			<div
				class="lat-plot-layer pointer-events-none absolute inset-y-0 right-0"
				aria-hidden="true"
			>
				<For each={TICKS}>
					{(tick) => (
						<span class="lat-gridline" style={{ left: `${xPercent(tick)}%` }} />
					)}
				</For>
			</div>

			<ul aria-label="p50 and p95 latency by operation">
				<For each={LATENCY_ROWS}>
					{(row: LatencyRow, index) => {
						const p50 = xPercent(row.p50);
						const p95 = xPercent(row.p95);
						const labelLeft = p95 > 72;
						return (
							<li
								class="lat-row group relative grid items-center outline-none"
								// biome-ignore lint/a11y/noNoninteractiveTabindex: focus reveals the same p95 tooltip as hover
								tabIndex={0}
								aria-label={`${row.label}, ${row.detail}: p50 ${formatNs(row.p50)}, p95 ${formatNs(row.p95)}`}
								data-active={hovered() === index()}
								onPointerEnter={() => setHovered(index())}
								onPointerLeave={() => setHovered(null)}
								onFocus={() => setHovered(index())}
								onBlur={() => setHovered(null)}
							>
								<div class="min-w-0 pr-4">
									<div class="text-[14px] font-medium leading-tight text-slate-200 sm:truncate">
										{row.label}
									</div>
									<div class="hidden truncate text-[12px] text-slate-500 sm:block">
										{row.detail}
									</div>
								</div>
								<div class="relative h-12" aria-hidden="true">
									<span
										class="lat-whisker"
										style={{ left: `${p50}%`, width: `${p95 - p50}%` }}
									/>
									<span class="lat-cap" style={{ left: `${p95}%` }} />
									<span class="lat-dot" style={{ left: `${p50}%` }} />
									<span
										class="lat-value"
										classList={{ "lat-value--left": labelLeft }}
										style={
											labelLeft
												? { right: `calc(${100 - p50}% + 14px)` }
												: { left: `calc(${p95}% + 12px)` }
										}
									>
										{formatNs(row.p50)}
									</span>
									<Show when={hovered() === index()}>
										<span
											class="lat-tooltip"
											style={{ left: `${(p50 + p95) / 2}%` }}
										>
											<span class="flex items-baseline gap-2">
												<strong class="text-[13px] font-semibold text-white">
													{formatNs(row.p50)}
												</strong>
												<span class="text-slate-500">p50</span>
											</span>
											<span class="flex items-baseline gap-2">
												<strong class="text-[13px] font-semibold text-white">
													{formatNs(row.p95)}
												</strong>
												<span class="text-slate-500">p95</span>
											</span>
										</span>
									</Show>
								</div>
							</li>
						);
					}}
				</For>
			</ul>

			{/* Axis */}
			<div class="lat-row grid" aria-hidden="true">
				<div />
				<div class="relative h-7">
					<For each={TICKS}>
						{(tick, index) => (
							<span
								class="lat-tick"
								// Every other label on narrow screens so decades don't collide
								classList={{ "max-sm:hidden": index() % 2 === 1 }}
								style={{ left: `${xPercent(tick)}%` }}
							>
								{tickLabel(tick)}
							</span>
						)}
					</For>
				</div>
			</div>
		</div>
	);
}

function LatencyTable() {
	return (
		<table class="w-full text-left text-[14px]">
			<caption class="sr-only">p50 and p95 latency by operation</caption>
			<thead>
				<tr class="border-b border-kite-line text-[12px] text-slate-500">
					<th scope="col" class="py-3 pr-4 font-medium">
						Operation
					</th>
					<th scope="col" class="py-3 pr-4 font-medium">
						Workload
					</th>
					<th scope="col" class="py-3 pr-4 text-right font-medium">
						p50
					</th>
					<th scope="col" class="py-3 text-right font-medium">
						p95
					</th>
				</tr>
			</thead>
			<tbody class="tabular-nums">
				<For each={LATENCY_ROWS}>
					{(row) => (
						<tr class="border-b border-kite-line/60 last:border-0">
							<th scope="row" class="py-3 pr-4 font-medium text-slate-200">
								{row.label}
							</th>
							<td class="py-3 pr-4 text-slate-500">{row.detail}</td>
							<td class="py-3 pr-4 text-right text-white">
								{formatNs(row.p50)}
							</td>
							<td class="py-3 text-right text-slate-400">
								{formatNs(row.p95)}
							</td>
						</tr>
					)}
				</For>
			</tbody>
		</table>
	);
}

export function Benchmarks() {
	const [view, setView] = createSignal<"chart" | "table">("chart");

	return (
		<section
			id="benchmarks"
			class="relative border-y border-kite-line bg-kite-surface/40 py-28 sm:py-36"
			aria-labelledby="benchmarks-heading"
		>
			<div class="mx-auto max-w-6xl px-5 sm:px-8">
				<SectionHeading
					id="benchmarks-heading"
					eyebrow="Benchmarks"
					title="Latency on an Apple M4"
				>
					Every number here comes from a raw log in the repository. The
					benchmark docs list the commands to reproduce them on your own
					hardware.
				</SectionHeading>

				<div class="reveal mt-16 overflow-hidden rounded-2xl border border-kite-line bg-kite-bg">
					<div class="flex flex-wrap items-center gap-4 border-b border-kite-line px-5 py-4 sm:px-7">
						<div>
							<h3 class="text-[15px] font-semibold text-white">
								Latency by operation
							</h3>
							<p class="mt-0.5 text-[13px] text-slate-500">
								Rust core · log scale · dot = p50, line = p95
							</p>
						</div>
						<fieldset class="ml-auto inline-flex items-center rounded-lg border border-kite-line p-0.5">
							<legend class="sr-only">Benchmark view</legend>
							<button
								type="button"
								aria-pressed={view() === "chart"}
								onClick={() => setView("chart")}
								class="flex items-center gap-1.5 rounded-md px-2.5 py-1 text-[12px] text-slate-500 transition-colors hover:text-slate-200 aria-pressed:bg-white/[0.07] aria-pressed:text-white"
							>
								<ChartScatter size={13} aria-hidden="true" />
								Chart
							</button>
							<button
								type="button"
								aria-pressed={view() === "table"}
								onClick={() => setView("table")}
								class="flex items-center gap-1.5 rounded-md px-2.5 py-1 text-[12px] text-slate-500 transition-colors hover:text-slate-200 aria-pressed:bg-white/[0.07] aria-pressed:text-white"
							>
								<Table2 size={13} aria-hidden="true" />
								Table
							</button>
						</fieldset>
					</div>
					<div class="overflow-x-auto px-5 py-6 sm:px-7 sm:py-8">
						<div class="min-w-[300px]">
							<Show when={view() === "chart"} fallback={<LatencyTable />}>
								<LatencyChart />
							</Show>
						</div>
					</div>
				</div>

				<div class="mt-8 grid gap-px overflow-hidden rounded-2xl border border-kite-line bg-kite-line sm:grid-cols-3">
					<For each={BENCH_ENVIRONMENT}>
						{(item) => (
							<div class="bg-kite-bg px-5 py-4 sm:px-6">
								<div class="text-[12px] text-slate-500">{item.label}</div>
								<div class="mt-1 text-[14px] text-slate-200">{item.value}</div>
							</div>
						)}
					</For>
				</div>

				<div class="mt-8 flex flex-wrap items-center gap-x-6 gap-y-3 text-[14px] font-medium">
					<a
						href="/docs/benchmarks"
						class="group inline-flex items-center gap-1.5 text-slate-200 transition-colors hover:text-white"
					>
						All benchmarks
						<ArrowRight
							size={14}
							class="transition-transform group-hover:translate-x-0.5"
							aria-hidden="true"
						/>
					</a>
					<a
						href={`${GITHUB_URL}/tree/main/docs/benchmarks/results`}
						target="_blank"
						rel="noopener noreferrer"
						class="group inline-flex items-center gap-1.5 text-slate-400 transition-colors hover:text-white"
					>
						Raw logs on GitHub
						<ArrowUpRight
							size={14}
							class="transition-transform group-hover:-translate-y-px group-hover:translate-x-px"
							aria-hidden="true"
						/>
					</a>
				</div>
			</div>
		</section>
	);
}
