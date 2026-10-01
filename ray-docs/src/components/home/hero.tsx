import { Link } from "@tanstack/solid-router";
import { ArrowRight, ArrowUpRight } from "lucide-solid";
import { GITHUB_URL } from "~/components/github-icon";
import { InstallCommand } from "~/components/install-command";
import { QueryTheater } from "./query-theater";
import { StatsStrip } from "./stats-strip";

export function Hero() {
	return (
		<section class="relative" aria-labelledby="hero-heading">
			<div class="hero-backdrop" aria-hidden="true" />

			<div class="relative mx-auto max-w-6xl px-5 pt-16 sm:px-8 sm:pt-24">
				<div class="mx-auto max-w-4xl text-center">
					<a
						href={`${GITHUB_URL}/blob/main/docs/REPLICATION_RUNBOOK.md`}
						target="_blank"
						rel="noopener noreferrer"
						class="hero-rise group inline-flex items-center gap-2.5 rounded-full border border-kite-line bg-white/[0.03] py-1 pl-1 pr-3 text-[13px] text-slate-400 transition-colors hover:border-white/15 hover:text-slate-200"
					>
						<span class="rounded-full bg-kite-cyan/10 px-2 py-0.5 font-mono text-[11px] font-medium text-kite-cyan">
							New
						</span>
						Primary / replica replication
						<ArrowUpRight
							size={13}
							class="text-slate-600 transition-transform group-hover:-translate-y-px group-hover:translate-x-px group-hover:text-slate-300"
							aria-hidden="true"
						/>
					</a>

					<h1
						id="hero-heading"
						class="hero-rise hero-title mt-8 text-balance text-[2.75rem] font-semibold leading-[1.02] tracking-[-0.045em] sm:text-[4rem] lg:text-[4.75rem]"
						style={{ "animation-delay": "60ms" }}
					>
						The graph database <br class="hidden sm:block" />
						that lives in your process.
					</h1>

					<p
						class="hero-rise mx-auto mt-7 max-w-2xl text-pretty text-[17px] leading-relaxed text-slate-400 sm:text-[19px]"
						style={{ "animation-delay": "120ms" }}
					>
						KiteDB is an embedded graph database with built-in vector search.
						Nodes, edges, and embeddings live in a single file, and a one-hop
						traversal takes about 200 nanoseconds. Bindings for TypeScript,
						Python, and Rust.
					</p>

					<div
						class="hero-rise mt-10 flex flex-col items-center justify-center gap-3 sm:flex-row"
						style={{ "animation-delay": "180ms" }}
					>
						<InstallCommand />
						<Link
							to="/docs/getting-started/$"
							params={{ _splat: "quick-start" }}
							class="group inline-flex h-11 items-center gap-2 rounded-xl bg-white px-5 text-[14px] font-semibold text-kite-bg transition-colors duration-150 hover:bg-slate-200"
						>
							Quick start
							<ArrowRight
								size={15}
								class="transition-transform duration-150 group-hover:translate-x-0.5"
								aria-hidden="true"
							/>
						</Link>
					</div>
				</div>

				<div
					class="hero-rise relative mt-16 sm:mt-20"
					style={{ "animation-delay": "260ms" }}
				>
					<div class="theater-aura" aria-hidden="true" />
					<QueryTheater />
				</div>

				<StatsStrip />
			</div>
		</section>
	);
}
