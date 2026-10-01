import { Link } from "@tanstack/solid-router";
import { ArrowLeft } from "lucide-solid";
import type { Component } from "solid-js";
import Logo from "./logo";

export const NotFound: Component = () => {
	return (
		<div class="flex min-h-screen items-center justify-center bg-kite-bg p-8">
			<div class="max-w-md text-center">
				<Logo size={44} class="mx-auto" />
				<p class="eyebrow mt-10 justify-center">404</p>
				<h1 class="hero-title mt-4 text-[2.75rem] font-semibold leading-[1.05] tracking-[-0.04em]">
					Page not found
				</h1>
				<p class="mt-4 text-[17px] text-slate-400">
					The page you asked for doesn't exist.
				</p>
				<div class="mt-9 flex items-center justify-center gap-3">
					<Link
						to="/"
						class="inline-flex h-10 items-center gap-2 rounded-xl bg-white px-4 text-[14px] font-semibold text-kite-bg transition-colors hover:bg-slate-200"
					>
						<ArrowLeft size={15} aria-hidden="true" />
						Home
					</Link>
					<a
						href="/docs"
						class="inline-flex h-10 items-center rounded-xl border border-kite-line px-4 text-[14px] text-slate-300 transition-colors hover:border-white/15 hover:text-white"
					>
						Documentation
					</a>
				</div>
			</div>
		</div>
	);
};

export default NotFound;
