import { For } from "solid-js";
import {
	LANGUAGES,
	selectedLanguage,
	setSelectedLanguage,
} from "~/lib/language-store";

/** Segmented TS / Python / Rust control bound to the global language preference. */
export function LanguageToggle(props: { class?: string }) {
	return (
		<fieldset
			class={`inline-flex items-center rounded-lg border border-kite-line bg-kite-bg/60 p-0.5 ${props.class ?? ""}`}
		>
			<legend class="sr-only">Code language</legend>
			<For each={LANGUAGES}>
				{(lang) => (
					<button
						type="button"
						aria-pressed={selectedLanguage().id === lang.id}
						class="rounded-md px-2.5 py-1 font-mono text-[11px] tracking-wide transition-colors duration-150 text-slate-500 hover:text-slate-200 aria-pressed:bg-white/[0.07] aria-pressed:text-white"
						onClick={() => setSelectedLanguage(lang)}
					>
						{lang.label}
					</button>
				)}
			</For>
		</fieldset>
	);
}
