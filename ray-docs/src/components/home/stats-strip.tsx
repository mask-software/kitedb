import { ArrowRight } from "lucide-solid";
import { For } from "solid-js";
import { HEADLINE_STATS } from "~/lib/benchmarks";

export function StatsStrip() {
	return (
		<div class="mt-14 sm:mt-16">
			<h2 class="sr-only">Headline performance</h2>
			<dl class="grid grid-cols-2 gap-px overflow-hidden rounded-2xl border border-kite-line bg-kite-line lg:grid-cols-4">
				<For each={HEADLINE_STATS}>
					{(stat) => (
						<div class="bg-kite-bg px-5 py-6 sm:px-7 sm:py-7">
							<dt class="text-[13px] text-slate-500">{stat.label}</dt>
							<dd class="mt-2 flex items-baseline gap-1.5">
								<span class="text-[2.25rem] font-semibold leading-none tracking-[-0.04em] text-white sm:text-[2.75rem]">
									{stat.value}
								</span>
								<span class="text-[15px] font-medium text-slate-500">
									{stat.unit}
								</span>
							</dd>
						</div>
					)}
				</For>
			</dl>
			<p class="mt-4 flex flex-wrap items-center justify-center gap-x-2 gap-y-1 text-center text-[13px] text-slate-500">
				<span>
					p50 · Rust core · Apple M4 · 10k nodes, 50k edges · sync=normal
				</span>
				<a
					href="#benchmarks"
					class="group inline-flex items-center gap-1 text-slate-400 transition-colors hover:text-white"
				>
					Methodology
					<ArrowRight
						size={12}
						class="transition-transform group-hover:translate-x-0.5"
						aria-hidden="true"
					/>
				</a>
			</p>
		</div>
	);
}
