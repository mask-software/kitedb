import { useLocation } from "@tanstack/solid-router";
import { ArrowLeft } from "lucide-solid";

interface DocNotFoundProps {
	backHref?: string;
	backLabel?: string;
}

/** Shared "missing page" state, used as the docs splat routes' notFoundComponent. */
export function DocNotFound(props: DocNotFoundProps) {
	const location = useLocation();

	return (
		<div class="mx-auto max-w-3xl px-5 py-24 sm:px-8">
			<p class="eyebrow">404</p>
			<h1 class="mt-4 text-[2.25rem] font-semibold leading-[1.08] tracking-[-0.035em] text-white">
				Page not found
			</h1>
			<p class="mt-4 text-[17px] leading-relaxed text-slate-400">
				There is no documentation page at{" "}
				<code class="rounded-md border border-kite-line bg-white/[0.04] px-1.5 py-0.5 font-mono text-[0.88em] text-slate-100">
					{location().pathname}
				</code>
				.
			</p>
			<a
				href={props.backHref ?? "/docs"}
				class="mt-8 inline-flex h-10 items-center gap-2 rounded-xl bg-white px-4 text-[14px] font-semibold text-kite-bg transition-colors hover:bg-slate-200"
			>
				<ArrowLeft size={15} aria-hidden="true" />
				{props.backLabel ?? "Back to the docs"}
			</a>
		</div>
	);
}
