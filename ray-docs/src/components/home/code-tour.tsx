import { Check, Copy } from "lucide-solid";
import { createSignal, For, onCleanup, Show } from "solid-js";
import { selectedLanguage } from "~/lib/language-store";
import { CodeView } from "~/components/code-view";
import { FILE_EXT, SHIKI_LANG, TOUR } from "./examples";
import { LanguageToggle } from "~/components/language-toggle";
import { SectionHeading } from "./section-heading";

export function CodeTour() {
	const [active, setActive] = createSignal(0);
	const [copied, setCopied] = createSignal(false);
	let resetTimer: ReturnType<typeof setTimeout> | undefined;
	onCleanup(() => clearTimeout(resetTimer));

	const lang = () => selectedLanguage().id;
	const item = () => TOUR[active()];
	const code = () => item().code[lang()];

	const copy = async () => {
		try {
			await navigator.clipboard.writeText(code());
			setCopied(true);
			clearTimeout(resetTimer);
			resetTimer = setTimeout(() => setCopied(false), 1800);
		} catch (error) {
			console.error("Failed to copy:", error);
		}
	};

	const onTabKeyDown = (event: KeyboardEvent, index: number) => {
		const keys: Record<string, number> = {
			ArrowDown: index + 1,
			ArrowRight: index + 1,
			ArrowUp: index - 1,
			ArrowLeft: index - 1,
			Home: 0,
			End: TOUR.length - 1,
		};
		if (!(event.key in keys)) return;
		event.preventDefault();
		const next = (keys[event.key] + TOUR.length) % TOUR.length;
		setActive(next);
		document.getElementById(`tour-tab-${TOUR[next].id}`)?.focus();
	};

	return (
		<section
			class="relative border-y border-kite-line bg-kite-surface/40 py-28 sm:py-36"
			aria-labelledby="api-heading"
		>
			<div class="mx-auto max-w-6xl px-5 sm:px-8">
				<SectionHeading
					id="api-heading"
					eyebrow="API"
					title="Write queries in the language you already use"
				>
					Each binding exposes the same fluent builders. Queries are ordinary
					method calls, and in TypeScript the result types follow your schema.
				</SectionHeading>

				<div class="mt-16 grid gap-6 lg:grid-cols-[minmax(0,0.8fr)_minmax(0,1.2fr)] lg:gap-10">
					<div
						class="reveal -mx-5 flex gap-2 overflow-x-auto px-5 pb-1 lg:mx-0 lg:flex-col lg:overflow-visible lg:px-0"
						role="tablist"
						aria-label="API examples"
						aria-orientation="vertical"
					>
						<For each={TOUR}>
							{(entry, index) => (
								<button
									type="button"
									role="tab"
									id={`tour-tab-${entry.id}`}
									aria-selected={active() === index()}
									aria-controls="tour-panel"
									tabIndex={active() === index() ? 0 : -1}
									class="tour-tab group relative shrink-0 rounded-xl border border-transparent px-4 py-3 text-left transition-colors duration-200 lg:px-5 lg:py-4"
									onClick={() => setActive(index())}
									onKeyDown={(event) => onTabKeyDown(event, index())}
								>
									<span class="flex items-center gap-3">
										<span class="font-mono text-[11px] text-slate-600 transition-colors group-aria-selected:text-kite-cyan">
											0{index() + 1}
										</span>
										<span class="whitespace-nowrap text-[15px] font-medium text-slate-400 transition-colors group-hover:text-slate-200 group-aria-selected:text-white">
											{entry.title}
										</span>
									</span>
									<span class="mt-1.5 hidden pl-[30px] text-[14px] leading-relaxed text-slate-500 lg:group-aria-selected:block">
										{entry.blurb}
									</span>
								</button>
							)}
						</For>
					</div>

					<div
						id="tour-panel"
						role="tabpanel"
						aria-labelledby={`tour-tab-${item().id}`}
						class="reveal min-w-0 overflow-hidden rounded-2xl border border-kite-line bg-kite-bg shadow-[0_30px_80px_-40px_rgba(0,0,0,0.8)]"
					>
						<div class="flex items-center gap-3 border-b border-kite-line px-4 py-2.5">
							<span class="font-mono text-[12px] text-slate-500">
								{item().file}.{FILE_EXT[lang()]}
							</span>
							<div class="ml-auto flex items-center gap-2">
								<LanguageToggle />
								<button
									type="button"
									onClick={copy}
									class="grid h-7 w-7 place-items-center rounded-md text-slate-500 transition-colors hover:bg-white/[0.06] hover:text-slate-200"
									aria-label={copied() ? "Copied" : "Copy code"}
								>
									<Show when={copied()} fallback={<Copy size={13} />}>
										<Check size={13} class="text-kite-mint" />
									</Show>
								</button>
							</div>
						</div>
						<div class="h-[440px] overflow-auto px-1 py-4 sm:px-2">
							<CodeView code={code()} lang={SHIKI_LANG[lang()]} />
						</div>
					</div>
				</div>
			</div>
		</section>
	);
}
