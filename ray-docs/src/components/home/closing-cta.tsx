import { Link } from "@tanstack/solid-router";
import { ArrowRight } from "lucide-solid";
import { InstallCommand } from "~/components/install-command";
import { LanguageToggle } from "~/components/language-toggle";

export function ClosingCta() {
	return (
		<section
			class="relative overflow-hidden border-t border-kite-line py-32 sm:py-40"
			aria-labelledby="cta-heading"
		>
			<div class="cta-backdrop" aria-hidden="true" />
			<div class="reveal relative mx-auto max-w-3xl px-5 text-center sm:px-8">
				<h2
					id="cta-heading"
					class="hero-title text-balance text-[2.5rem] font-semibold leading-[1.04] tracking-[-0.045em] sm:text-[3.75rem]"
				>
					Start with one file
				</h2>
				<p class="mx-auto mt-6 max-w-xl text-pretty text-[17px] leading-relaxed text-slate-400 sm:text-[18px]">
					Install the package, open a database, and write your first traversal.
					It takes about five minutes.
				</p>
				<div class="mt-10 flex justify-center">
					<LanguageToggle />
				</div>
				<div class="mt-4 flex flex-col items-center justify-center gap-3 sm:flex-row">
					<InstallCommand />
					<Link
						to="/docs/getting-started/$"
						params={{ _splat: "quick-start" }}
						class="group inline-flex h-11 items-center gap-2 rounded-xl bg-white px-5 text-[14px] font-semibold text-kite-bg transition-colors duration-150 hover:bg-slate-200"
					>
						Read the quick start
						<ArrowRight
							size={15}
							class="transition-transform duration-150 group-hover:translate-x-0.5"
							aria-hidden="true"
						/>
					</Link>
				</div>
			</div>
		</section>
	);
}
