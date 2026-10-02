import {
	Binary,
	DatabaseBackup,
	FileBox,
	Radar,
	ShieldCheck,
	Waypoints,
} from "lucide-solid";
import { For, type JSX } from "solid-js";
import { InlineCode } from "~/components/inline-code";
import { SectionHeading } from "./section-heading";

interface Feature {
	icon: (props: { class?: string }) => JSX.Element;
	title: string;
	body: string;
	accent: "cyan" | "violet" | "mint";
}

const FEATURES: Feature[] = [
	{
		icon: Waypoints,
		title: "Graph traversal",
		body: "Typed nodes and edges with properties. Multi-hop traversal, filters, and pathfinding with BFS, Dijkstra, and k-shortest paths.",
		accent: "cyan",
	},
	{
		icon: Radar,
		title: "Vector search",
		body: "IVF and IVF-PQ approximate nearest-neighbor indexes with cosine, L2, and dot-product metrics. Embeddings are stored with their nodes.",
		accent: "violet",
	},
	{
		icon: ShieldCheck,
		title: "ACID transactions",
		body: "MVCC snapshot isolation: every transaction reads a consistent view, and every commit is written to a write-ahead log with CRC-32 checksums first.",
		accent: "mint",
	},
	{
		icon: FileBox,
		title: "One file",
		body: "A single `.kitedb` file holds the log and the snapshot. The snapshot is memory-mapped, and online backups are built in.",
		accent: "cyan",
	},
	{
		icon: DatabaseBackup,
		title: "Replication",
		body: "Primary / replica replication with epoch fencing, log catch-up, snapshot reseed, and Prometheus or OpenTelemetry metrics.",
		accent: "violet",
	},
	{
		icon: Binary,
		title: "One core, three languages",
		body: "A Rust core with N-API bindings for Node and Bun and PyO3 bindings for Python.",
		accent: "mint",
	},
];

export function Features() {
	return (
		<section class="py-28 sm:py-36" aria-labelledby="features-heading">
			<div class="mx-auto max-w-6xl px-5 sm:px-8">
				<SectionHeading
					id="features-heading"
					eyebrow="Capabilities"
					title="One engine for connected data"
				>
					Graph traversal, vector search, and transactions share a storage
					engine, a file format, and an API, so relationships and embeddings are
					queried from the same database handle.
				</SectionHeading>

				<div class="mt-16 grid gap-px overflow-hidden rounded-2xl border border-kite-line bg-kite-line sm:grid-cols-2 lg:grid-cols-3">
					<For each={FEATURES}>
						{(feature) => (
							<article class="feature-cell reveal group relative bg-kite-bg p-7 sm:p-8">
								<div
									class="feature-icon grid h-10 w-10 place-items-center rounded-xl border"
									data-accent={feature.accent}
								>
									<feature.icon class="h-[18px] w-[18px]" />
								</div>
								<h3 class="mt-6 text-[17px] font-semibold tracking-[-0.01em] text-white">
									{feature.title}
								</h3>
								<p class="mt-2.5 text-[15px] leading-relaxed text-slate-400">
									<InlineCode text={feature.body} />
								</p>
							</article>
						)}
					</For>
				</div>
			</div>
		</section>
	);
}
