import { ArrowLeft, ArrowRight } from "lucide-solid";
import {
	createSignal,
	For,
	type JSX,
	onCleanup,
	onMount,
	Show,
} from "solid-js";
import { GITHUB_URL, GithubIcon } from "~/components/github-icon";
import {
	findDocBySlug,
	findSectionBySlug,
	getNextDoc,
	getPrevDoc,
} from "~/lib/docs";

interface DocPageProps {
	slug: string;
	children: JSX.Element;
}

/** Route file that renders a slug, for the "Edit this page" link. */
function sourcePathForSlug(slug: string): string {
	const base = "ray-docs/src/routes/docs";
	if (slug.startsWith("internals/")) {
		return `${base}/internals/-${slug.slice("internals/".length)}.tsx`;
	}
	if (slug === "getting-started/installation") {
		return `${base}/getting-started/installation.tsx`;
	}
	for (const dir of ["getting-started", "guides", "api", "benchmarks"]) {
		if (slug === dir || slug.startsWith(`${dir}/`))
			return `${base}/${dir}/$.tsx`;
	}
	return `${base}/$.tsx`;
}

interface Heading {
	id: string;
	text: string;
	level: 2 | 3;
}

function OnThisPage(props: { headings: Heading[]; active?: string }) {
	return (
		<Show when={props.headings.length > 1}>
			<nav aria-label="On this page">
				<p class="font-mono text-[11px] uppercase tracking-[0.08em] text-slate-500">
					On this page
				</p>
				<ul class="mt-4 space-y-2 border-l border-kite-line">
					<For each={props.headings}>
						{(heading) => (
							<li>
								<a
									href={`#${heading.id}`}
									class="-ml-px block border-l py-0.5 text-[13px] leading-snug transition-colors duration-150 hover:text-slate-100"
									classList={{
										"pl-4": heading.level === 2,
										"pl-7": heading.level === 3,
										"border-kite-cyan text-slate-100":
											props.active === heading.id,
										"border-transparent text-slate-500":
											props.active !== heading.id,
									}}
								>
									{heading.text}
								</a>
							</li>
						)}
					</For>
				</ul>
			</nav>
		</Show>
	);
}

export function DocPage(props: DocPageProps) {
	const doc = () => findDocBySlug(props.slug);
	const section = () => findSectionBySlug(props.slug);
	const prevDoc = () => getPrevDoc(props.slug);
	const nextDoc = () => getNextDoc(props.slug);

	const [headings, setHeadings] = createSignal<Heading[]>([]);
	const [activeHeading, setActiveHeading] = createSignal<string>();
	let content: HTMLDivElement | undefined;

	onMount(() => {
		if (!content) return;
		const elements = [
			...content.querySelectorAll<HTMLElement>("h2[id], h3[id]"),
		];
		setHeadings(
			elements.map((el) => ({
				id: el.id,
				text: el.textContent?.trim() ?? "",
				level: el.tagName === "H2" ? 2 : 3,
			})),
		);

		// Active heading = the last one scrolled past the sticky header
		let frame = 0;
		const update = () => {
			frame = 0;
			let current = elements[0]?.id;
			for (const el of elements) {
				if (el.getBoundingClientRect().top <= 120) current = el.id;
				else break;
			}
			setActiveHeading(current);
		};
		const onScroll = () => {
			if (!frame) frame = requestAnimationFrame(update);
		};
		update();
		window.addEventListener("scroll", onScroll, { passive: true });
		onCleanup(() => {
			window.removeEventListener("scroll", onScroll);
			cancelAnimationFrame(frame);
		});
	});

	return (
		<div class="mx-auto flex max-w-[76rem] gap-14 px-5 py-12 sm:px-8 lg:px-12 lg:py-16">
			<article class="min-w-0 max-w-3xl flex-1">
				<header class="mb-12">
					<Show when={section()}>
						{(s) => <p class="eyebrow">{s().label}</p>}
					</Show>
					<h1 class="mt-4 text-balance text-[2.25rem] font-semibold leading-[1.08] tracking-[-0.035em] text-white sm:text-[2.75rem]">
						{doc()?.title ?? "Documentation"}
					</h1>
					<Show when={doc()?.description}>
						<p class="mt-4 text-pretty text-[18px] leading-relaxed text-slate-400">
							{doc()?.description}
						</p>
					</Show>
				</header>

				<div ref={content} class="prose">
					{props.children}
				</div>

				<footer class="mt-20 border-t border-kite-line pt-8">
					<nav
						class="grid gap-3 sm:grid-cols-2"
						aria-label="Previous and next pages"
					>
						<Show when={prevDoc()} fallback={<div class="hidden sm:block" />}>
							{(prev) => (
								<a
									href={`/docs/${prev().slug}`}
									class="group rounded-xl border border-kite-line px-5 py-4 transition-colors duration-150 hover:border-white/15 hover:bg-white/[0.02]"
								>
									<span class="flex items-center gap-1.5 text-[12px] text-slate-500">
										<ArrowLeft
											size={12}
											class="transition-transform group-hover:-translate-x-0.5"
											aria-hidden="true"
										/>
										Previous
									</span>
									<span class="mt-1 block text-[15px] font-medium text-slate-100">
										{prev().title}
									</span>
								</a>
							)}
						</Show>
						<Show when={nextDoc()}>
							{(next) => (
								<a
									href={`/docs/${next().slug}`}
									class="group rounded-xl border border-kite-line px-5 py-4 text-right transition-colors duration-150 hover:border-white/15 hover:bg-white/[0.02]"
								>
									<span class="flex items-center justify-end gap-1.5 text-[12px] text-slate-500">
										Next
										<ArrowRight
											size={12}
											class="transition-transform group-hover:translate-x-0.5"
											aria-hidden="true"
										/>
									</span>
									<span class="mt-1 block text-[15px] font-medium text-slate-100">
										{next().title}
									</span>
								</a>
							)}
						</Show>
					</nav>
					<a
						href={`${GITHUB_URL}/edit/main/${sourcePathForSlug(props.slug)}`}
						target="_blank"
						rel="noopener noreferrer"
						class="mt-8 inline-flex items-center gap-2 text-[13px] text-slate-500 transition-colors hover:text-slate-200"
					>
						<GithubIcon class="h-4 w-4" />
						Edit this page on GitHub
					</a>
				</footer>
			</article>

			<aside class="hidden w-52 shrink-0 xl:block">
				<div class="sticky top-28">
					<OnThisPage headings={headings()} active={activeHeading()} />
				</div>
			</aside>
		</div>
	);
}

export default DocPage;
