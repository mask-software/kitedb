import {
	createFileRoute,
	Link,
	Outlet,
	useLocation,
} from "@tanstack/solid-router";
import { X } from "lucide-solid";
import { createEffect, createSignal, For, on, Show } from "solid-js";
import { LanguageToggle } from "~/components/language-toggle";
import Logo from "~/components/logo";
import { SiteNav } from "~/components/site-nav";
import { docsStructure } from "~/lib/docs";

const KITE_VERSION = __KITE_VERSION__;

export const Route = createFileRoute("/docs")({
	component: DocsLayout,
});

function DocsNav(props: { onNavigate?: () => void }) {
	const location = useLocation();
	const currentSlug = () =>
		location()
			.pathname.replace(/^\/docs\/?/, "")
			.replace(/\/$/, "");

	return (
		<nav aria-label="Documentation">
			<For each={docsStructure}>
				{(section) => (
					<div class="mb-8 last:mb-0">
						<p class="px-3 pb-2 font-mono text-[11px] uppercase tracking-[0.08em] text-slate-500">
							{section.label}
						</p>
						<ul class="space-y-px" aria-label={section.label}>
							<For each={section.items}>
								{(item) => (
									<li>
										<a
											href={`/docs/${item.slug}`}
											onClick={() => props.onNavigate?.()}
											aria-current={
												currentSlug() === item.slug ? "page" : undefined
											}
											class="docs-nav-link relative block rounded-lg px-3 py-1.5 text-[14px] text-slate-400 transition-colors duration-150 hover:bg-white/[0.03] hover:text-slate-100"
										>
											{item.title}
										</a>
									</li>
								)}
							</For>
						</ul>
					</div>
				)}
			</For>
		</nav>
	);
}

function DocsLayout() {
	const location = useLocation();
	const [drawerOpen, setDrawerOpen] = createSignal(false);

	// Close the mobile drawer whenever the route changes
	createEffect(
		on(
			() => location().pathname,
			() => setDrawerOpen(false),
			{ defer: true },
		),
	);

	return (
		<div class="docs min-h-screen bg-kite-bg text-slate-300">
			<a
				href="#doc-content"
				class="sr-only focus:not-sr-only focus:fixed focus:left-4 focus:top-4 focus:z-[100] focus:rounded-lg focus:bg-white focus:px-4 focus:py-2 focus:text-sm focus:font-semibold focus:text-kite-bg"
			>
				Skip to content
			</a>

			<SiteNav variant="docs" onMenuClick={() => setDrawerOpen(true)} />

			<div class="flex">
				<aside class="sticky top-16 hidden h-[calc(100vh-4rem)] w-72 shrink-0 overflow-y-auto border-r border-kite-line px-4 py-8 scrollbar-thin lg:block">
					<DocsNav />
					<div class="mt-10 px-3 font-mono text-[11px] text-slate-600">
						v{KITE_VERSION}
					</div>
				</aside>

				<main id="doc-content" class="min-w-0 flex-1">
					<Outlet />
				</main>
			</div>

			{/* Mobile drawer */}
			<Show when={drawerOpen()}>
				<div
					class="fixed inset-0 z-[60] bg-black/60 backdrop-blur-sm lg:hidden"
					onClick={() => setDrawerOpen(false)}
					aria-hidden="true"
				/>
				<aside
					class="fixed inset-y-0 left-0 z-[70] flex w-80 max-w-[85vw] flex-col border-r border-kite-line bg-kite-bg lg:hidden"
					aria-label="Documentation navigation"
				>
					<div class="flex h-16 items-center justify-between border-b border-kite-line px-4">
						<Link
							to="/"
							class="flex items-center gap-2.5"
							aria-label="KiteDB home"
						>
							<Logo size={20} />
							<span class="text-[17px] font-semibold tracking-[-0.02em] text-white">
								KiteDB
							</span>
						</Link>
						<button
							type="button"
							class="grid h-9 w-9 place-items-center rounded-lg text-slate-400 transition-colors hover:bg-white/[0.05] hover:text-white"
							onClick={() => setDrawerOpen(false)}
							aria-label="Close navigation"
						>
							<X size={18} aria-hidden="true" />
						</button>
					</div>
					<div class="flex-1 overflow-y-auto px-4 py-6">
						<div class="mb-8 px-3 sm:hidden">
							<LanguageToggle />
						</div>
						<DocsNav onNavigate={() => setDrawerOpen(false)} />
					</div>
				</aside>
			</Show>
		</div>
	);
}
