import { Link, useLocation } from "@tanstack/solid-router";
import { ArrowRight, Menu, Search } from "lucide-solid";
import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { GITHUB_URL, GithubIcon } from "~/components/github-icon";
import { LanguageToggle } from "~/components/language-toggle";
import Logo from "~/components/logo";
import { searchDialog } from "~/components/search-dialog";

interface NavLink {
	label: string;
	href: string;
	/** Path prefix that marks this link active inside the docs */
	section: string;
}

const NAV_LINKS: NavLink[] = [
	{ label: "Docs", href: "/docs", section: "/docs" },
	{ label: "API", href: "/docs/api/high-level", section: "/docs/api" },
	{
		label: "Benchmarks",
		href: "/docs/benchmarks",
		section: "/docs/benchmarks",
	},
	{
		label: "Internals",
		href: "/docs/internals/architecture",
		section: "/docs/internals",
	},
];

interface SiteNavProps {
	/**
	 * `home` floats over the hero and gains a border on scroll; `docs` is always
	 * solid, full width, and adds the sidebar toggle and language switch.
	 */
	variant?: "home" | "docs";
	onMenuClick?: () => void;
}

export function SiteNav(props: SiteNavProps) {
	const location = useLocation();
	const [scrolled, setScrolled] = createSignal(false);
	const isDocs = () => props.variant === "docs";

	onMount(() => {
		const onScroll = () => setScrolled(window.scrollY > 8);
		onScroll();
		window.addEventListener("scroll", onScroll, { passive: true });
		onCleanup(() => window.removeEventListener("scroll", onScroll));
	});

	// The most specific matching section wins, so /docs/api highlights API, not Docs
	const activeSection = () => {
		const path = location().pathname;
		return NAV_LINKS.filter(
			(link) => path === link.section || path.startsWith(`${link.section}/`),
		).sort((a, b) => b.section.length - a.section.length)[0]?.section;
	};

	return (
		<header
			class="sticky top-0 z-50 border-b transition-colors duration-300"
			classList={{
				"border-kite-line bg-kite-bg/80 backdrop-blur-xl":
					isDocs() || scrolled(),
				"border-transparent bg-transparent": !isDocs() && !scrolled(),
			}}
		>
			<nav
				class="mx-auto flex h-16 items-center gap-8"
				classList={{
					"max-w-6xl px-5 sm:px-8": !isDocs(),
					"px-4 sm:px-6": isDocs(),
				}}
				aria-label="Main"
			>
				<div class="flex items-center gap-2">
					<Show when={props.onMenuClick}>
						<button
							type="button"
							class="-ml-1.5 grid h-9 w-9 place-items-center rounded-lg text-slate-400 transition-colors hover:bg-white/[0.05] hover:text-white lg:hidden"
							onClick={() => props.onMenuClick?.()}
							aria-label="Open navigation"
						>
							<Menu size={18} aria-hidden="true" />
						</button>
					</Show>
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
				</div>

				<div class="hidden items-center gap-1 md:flex">
					<For each={NAV_LINKS}>
						{(link) => (
							<a
								href={link.href}
								class="rounded-md px-3 py-1.5 text-[14px] transition-colors duration-150 hover:text-white"
								classList={{
									"text-white": isDocs() && activeSection() === link.section,
									"text-slate-400":
										!isDocs() || activeSection() !== link.section,
								}}
								aria-current={
									isDocs() && activeSection() === link.section
										? "page"
										: undefined
								}
							>
								{link.label}
							</a>
						)}
					</For>
				</div>

				<div class="ml-auto flex items-center gap-2">
					<Show when={!isDocs()}>
						<a
							href="/docs"
							class="px-2 text-[14px] text-slate-400 transition-colors hover:text-white md:hidden"
						>
							Docs
						</a>
					</Show>
					<button
						type="button"
						onClick={() => searchDialog.open()}
						class="flex h-9 items-center gap-2 rounded-lg border border-kite-line bg-white/[0.02] px-3 text-[13px] text-slate-500 transition-colors duration-150 hover:border-white/15 hover:text-slate-200"
						aria-label="Search documentation"
					>
						<Search size={14} aria-hidden="true" />
						<span class="hidden lg:inline">Search docs</span>
						<kbd class="hidden rounded border border-kite-line px-1.5 py-px font-mono text-[10px] text-slate-500 lg:inline">
							⌘K
						</kbd>
					</button>
					<Show when={isDocs()}>
						<div class="hidden sm:block">
							<LanguageToggle />
						</div>
					</Show>
					<a
						href={GITHUB_URL}
						target="_blank"
						rel="noopener noreferrer"
						class="flex h-9 items-center gap-2 rounded-lg px-2.5 text-[13px] text-slate-400 transition-colors duration-150 hover:text-white"
						aria-label="KiteDB on GitHub"
					>
						<GithubIcon class="h-[18px] w-[18px]" />
						<span class={isDocs() ? "hidden xl:inline" : "hidden sm:inline"}>
							GitHub
						</span>
					</a>
					<Show when={!isDocs()}>
						<Link
							to="/docs/getting-started/installation"
							class="hidden h-9 items-center gap-1.5 rounded-lg bg-white px-3.5 text-[13px] font-semibold text-kite-bg transition-colors duration-150 hover:bg-slate-200 sm:flex"
						>
							Get started
							<ArrowRight size={14} aria-hidden="true" />
						</Link>
					</Show>
				</div>
			</nav>
		</header>
	);
}
