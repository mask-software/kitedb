import { Link } from "@tanstack/solid-router";
import { For } from "solid-js";
import { GITHUB_URL, GithubIcon } from "~/components/github-icon";
import Logo from "~/components/logo";

const KITE_VERSION = __KITE_VERSION__;

const COLUMNS = [
	{
		title: "Learn",
		links: [
			{ label: "Introduction", href: "/docs" },
			{ label: "Quick start", href: "/docs/getting-started/quick-start" },
			{ label: "Schema", href: "/docs/guides/schema" },
			{ label: "Vector search", href: "/docs/guides/vectors" },
		],
	},
	{
		title: "Reference",
		links: [
			{ label: "High-level API", href: "/docs/api/high-level" },
			{ label: "Low-level API", href: "/docs/api/low-level" },
			{ label: "Vector API", href: "/docs/api/vector-api" },
			{ label: "Benchmarks", href: "/docs/benchmarks" },
		],
	},
	{
		title: "Internals",
		links: [
			{ label: "Architecture", href: "/docs/internals/architecture" },
			{ label: "CSR format", href: "/docs/internals/csr" },
			{ label: "Write-ahead log", href: "/docs/internals/wal" },
			{ label: "MVCC", href: "/docs/internals/mvcc" },
		],
	},
	{
		title: "Project",
		links: [
			{ label: "GitHub", href: GITHUB_URL, external: true },
			{
				label: "Changelog",
				href: `${GITHUB_URL}/blob/main/CHANGELOG.md`,
				external: true,
			},
			{ label: "Releases", href: `${GITHUB_URL}/releases`, external: true },
			{
				label: "MIT License",
				href: `${GITHUB_URL}/blob/main/ray-rs/LICENSE`,
				external: true,
			},
		],
	},
];

export function SiteFooter() {
	return (
		<footer class="border-t border-kite-line">
			<div class="mx-auto max-w-6xl px-5 py-16 sm:px-8">
				<div class="grid gap-12 lg:grid-cols-[1.2fr_2fr]">
					<div>
						<Link
							to="/"
							class="inline-flex items-center gap-2.5"
							aria-label="KiteDB home"
						>
							<Logo size={20} />
							<span class="text-[17px] font-semibold tracking-[-0.02em] text-white">
								KiteDB
							</span>
						</Link>
						<p class="mt-4 max-w-xs text-[14px] leading-relaxed text-slate-500">
							Embedded graph database with vector search. Written in Rust, open
							source under MIT.
						</p>
						<a
							href={GITHUB_URL}
							target="_blank"
							rel="noopener noreferrer"
							class="mt-6 inline-flex items-center gap-2 text-[13px] text-slate-400 transition-colors hover:text-white"
						>
							<GithubIcon class="h-4 w-4" />
							mask-software/kitedb
						</a>
					</div>

					<nav
						class="grid grid-cols-2 gap-10 sm:grid-cols-4"
						aria-label="Footer"
					>
						<For each={COLUMNS}>
							{(column) => (
								<div>
									<h3 class="text-[13px] font-medium text-slate-200">
										{column.title}
									</h3>
									<ul class="mt-4 space-y-3">
										<For each={column.links}>
											{(link) => (
												<li>
													<a
														href={link.href}
														class="text-[14px] text-slate-500 transition-colors hover:text-slate-200"
														{...("external" in link && link.external
															? { target: "_blank", rel: "noopener noreferrer" }
															: {})}
													>
														{link.label}
													</a>
												</li>
											)}
										</For>
									</ul>
								</div>
							)}
						</For>
					</nav>
				</div>

				<div class="mt-16 flex flex-col gap-3 border-t border-kite-line pt-8 text-[13px] text-slate-600 sm:flex-row sm:items-center sm:justify-between">
					<span>Open source under the MIT License</span>
					<span class="font-mono">v{KITE_VERSION}</span>
				</div>
			</div>
		</footer>
	);
}
