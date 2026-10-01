import { createFileRoute } from "@tanstack/solid-router";
import { ArrowRight, Rocket, Zap } from "lucide-solid";
import { For, type JSX } from "solid-js";
import { InstallCommand } from "~/components/install-command";
import { docsStructure } from "~/lib/docs";

export const Route = createFileRoute("/docs/")({
	head: () => ({
		meta: [
			{ title: "Documentation · KiteDB" },
			{
				name: "description",
				content:
					"Install KiteDB, define a schema, and query graphs and vectors from TypeScript, Python, or Rust.",
			},
		],
	}),
	component: DocsIndex,
});

function FeaturedCard(props: {
	href: string;
	title: string;
	body: string;
	icon: (p: { class?: string }) => JSX.Element;
	accent: "cyan" | "mint";
}) {
	return (
		<a
			href={props.href}
			class="group relative rounded-2xl border border-kite-line bg-kite-surface/40 p-6 transition-colors duration-200 hover:border-white/15 sm:p-7"
		>
			<div
				class="feature-icon grid h-10 w-10 place-items-center rounded-xl border"
				data-accent={props.accent}
			>
				<props.icon class="h-[18px] w-[18px]" />
			</div>
			<h2 class="mt-6 flex items-center gap-2 text-[18px] font-semibold tracking-[-0.01em] text-white">
				{props.title}
				<ArrowRight
					size={16}
					class="text-slate-600 transition-all duration-150 group-hover:translate-x-0.5 group-hover:text-slate-300"
					aria-hidden="true"
				/>
			</h2>
			<p class="mt-2 text-[15px] leading-relaxed text-slate-400">
				{props.body}
			</p>
		</a>
	);
}

function DocsIndex() {
	return (
		<div class="mx-auto max-w-[76rem] px-5 py-12 sm:px-8 lg:px-12 lg:py-16">
			<header class="max-w-3xl">
				<p class="eyebrow">Documentation</p>
				<h1 class="mt-4 text-balance text-[2.5rem] font-semibold leading-[1.05] tracking-[-0.04em] text-white sm:text-[3.25rem]">
					KiteDB documentation
				</h1>
				<p class="mt-5 text-pretty text-[18px] leading-relaxed text-slate-400">
					Install KiteDB, define a schema, and query your graph from TypeScript,
					Python, or Rust. The internals section explains how the storage engine
					works, from the file format to MVCC.
				</p>
				<div class="mt-8">
					<InstallCommand />
				</div>
			</header>

			<div class="mt-14 grid gap-4 sm:grid-cols-2">
				<FeaturedCard
					href="/docs/getting-started/installation"
					title="Installation"
					body="Add the package for your language and open your first database file."
					icon={Rocket}
					accent="cyan"
				/>
				<FeaturedCard
					href="/docs/getting-started/quick-start"
					title="Quick start"
					body="Build a small social graph: define a schema, insert nodes, and traverse edges."
					icon={Zap}
					accent="mint"
				/>
			</div>

			<For
				each={docsStructure.filter(
					(section) => section.label !== "Getting started",
				)}
			>
				{(section) => (
					<section class="mt-16" aria-label={section.label}>
						<p class="eyebrow">{section.label}</p>
						<div class="mt-5 grid gap-px overflow-hidden rounded-2xl border border-kite-line bg-kite-line sm:grid-cols-2 lg:grid-cols-3">
							<For each={section.items}>
								{(item) => (
									<a
										href={`/docs/${item.slug}`}
										class="group bg-kite-bg p-5 transition-colors duration-150 hover:bg-kite-surface sm:p-6"
									>
										<h3 class="flex items-center gap-2 text-[15px] font-semibold text-slate-100">
											{item.title}
											<ArrowRight
												size={14}
												class="text-slate-700 transition-all duration-150 group-hover:translate-x-0.5 group-hover:text-slate-400"
												aria-hidden="true"
											/>
										</h3>
										<p class="mt-1.5 text-[14px] leading-relaxed text-slate-500">
											{item.description}
										</p>
									</a>
								)}
							</For>
						</div>
					</section>
				)}
			</For>
		</div>
	);
}
