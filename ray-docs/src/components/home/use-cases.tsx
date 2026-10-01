import { For } from "solid-js";
import { SectionHeading } from "./section-heading";

const USE_CASES = [
	{
		title: "Agent memory & RAG",
		body: "Retrieve passages by meaning, then expand along citations, authors, and entities for grounded context.",
		snippet:
			"const [hit] = index.search(q, { k: 8 })\ndb.from(hit.nodeId).out('cites').nodes()",
	},
	{
		title: "Knowledge graphs",
		body: "Model entities and typed relationships with properties, and answer multi-hop questions without a server round-trip.",
		snippet: "db.from(drug).out('targets').in('associated_with').nodes()",
	},
	{
		title: "Recommendations",
		body: "Blend collaborative paths with embedding similarity at request time, in the same process as your API.",
		snippet: "db.from(user).out('liked').in('liked').out('liked').nodes()",
	},
	{
		title: "Local-first software",
		body: "Ship the database inside your desktop app, CLI, or service. The whole database is one file you can back up or sync.",
		snippet: "await kite('./app.kitedb', { nodes, edges })",
	},
];

export function UseCases() {
	return (
		<section class="py-28 sm:py-36" aria-labelledby="usecases-heading">
			<div class="mx-auto max-w-6xl px-5 sm:px-8">
				<SectionHeading
					id="usecases-heading"
					eyebrow="Use cases"
					title="Where KiteDB fits"
				/>

				<div class="mt-16 grid gap-4 md:grid-cols-2">
					<For each={USE_CASES}>
						{(useCase, index) => (
							<article class="usecase reveal group relative overflow-hidden rounded-2xl border border-kite-line bg-kite-surface/40 p-7 transition-colors duration-300 hover:border-white/15 sm:p-8">
								<div class="font-mono text-[11px] text-slate-600">
									0{index() + 1}
								</div>
								<h3 class="mt-5 text-[20px] font-semibold tracking-[-0.02em] text-white">
									{useCase.title}
								</h3>
								<p class="mt-3 max-w-md text-[15px] leading-relaxed text-slate-400">
									{useCase.body}
								</p>
								<div class="mt-7 overflow-x-auto rounded-lg border border-kite-line bg-kite-bg/70 px-3.5 py-2.5">
									<code class="whitespace-pre font-mono text-[12.5px] leading-relaxed text-slate-300">
										{useCase.snippet}
									</code>
								</div>
							</article>
						)}
					</For>
				</div>
			</div>
		</section>
	);
}
