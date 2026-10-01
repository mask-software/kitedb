import { createFileRoute } from "@tanstack/solid-router";
import { type Component, Show } from "solid-js";
import { Dynamic } from "solid-js/web";
import { DocNotFound } from "~/components/doc-not-found";
import DocPage from "~/components/doc-page";
import { loadDocSlug } from "~/lib/doc-route";

// Import page components
import { ArchitecturePage } from "./-architecture";
import { CSRPage } from "./-csr";
import { KeyIndexPage } from "./-key-index";
import { MVCCPage } from "./-mvcc";
import { PerformancePage } from "./-performance";
import { SingleFilePage } from "./-single-file";
import { SnapshotDeltaPage } from "./-snapshot-delta";
import { WALPage } from "./-wal";

export const Route = createFileRoute("/docs/internals/$")({
	loader: loadDocSlug,
	component: InternalsSplatPage,
	notFoundComponent: () => (
		<DocNotFound
			backHref="/docs/internals/architecture"
			backLabel="Back to internals"
		/>
	),
});

function InternalsSplatPage() {
	const data = Route.useLoaderData();
	return <DocPageContent slug={data().slug} />;
}

const PAGES: Record<string, Component> = {
	"internals/architecture": ArchitecturePage,
	"internals/snapshot-delta": SnapshotDeltaPage,
	"internals/csr": CSRPage,
	"internals/single-file": SingleFilePage,
	"internals/wal": WALPage,
	"internals/mvcc": MVCCPage,
	"internals/key-index": KeyIndexPage,
	"internals/performance": PerformancePage,
};

function DocPageContent(props: { slug: string }) {
	return (
		<Show
			when={PAGES[props.slug]}
			fallback={
				<DocPage slug={props.slug}>
					<p>This internals page is coming soon.</p>
				</DocPage>
			}
		>
			{(page) => <Dynamic component={page()} />}
		</Show>
	);
}
