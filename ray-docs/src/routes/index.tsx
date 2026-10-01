import { createFileRoute } from "@tanstack/solid-router";
import { Architecture } from "~/components/home/architecture";
import { Benchmarks } from "~/components/home/benchmarks";
import { ClosingCta } from "~/components/home/closing-cta";
import { CodeTour } from "~/components/home/code-tour";
import { Features } from "~/components/home/features";
import { Hero } from "~/components/home/hero";
import { SiteFooter } from "~/components/home/site-footer";
import { SiteNav } from "~/components/site-nav";
import { UseCases } from "~/components/home/use-cases";

const TITLE = "KiteDB · The embedded graph database with vector search";
const DESCRIPTION =
	"KiteDB is an embedded graph database with built-in vector search. One file on disk, ACID transactions, and nanosecond traversals, for TypeScript, Python, and Rust.";

export const Route = createFileRoute("/")({
	head: () => ({
		meta: [
			{ title: TITLE },
			{ name: "description", content: DESCRIPTION },
			{ property: "og:title", content: TITLE },
			{ property: "og:description", content: DESCRIPTION },
			{ property: "og:type", content: "website" },
			{ name: "twitter:card", content: "summary" },
		],
	}),
	component: HomePage,
});

function HomePage() {
	return (
		<div class="home relative min-h-screen overflow-x-clip bg-kite-bg text-slate-200">
			<a
				href="#main-content"
				class="sr-only focus:not-sr-only focus:fixed focus:left-4 focus:top-4 focus:z-[100] focus:rounded-lg focus:bg-white focus:px-4 focus:py-2 focus:text-sm focus:font-semibold focus:text-kite-bg"
			>
				Skip to content
			</a>
			<SiteNav />
			<main id="main-content">
				<Hero />
				<Features />
				<CodeTour />
				<Architecture />
				<Benchmarks />
				<UseCases />
				<ClosingCta />
			</main>
			<SiteFooter />
		</div>
	);
}
