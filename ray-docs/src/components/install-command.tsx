import { Check, Copy } from "lucide-solid";
import { createSignal, onCleanup, Show } from "solid-js";
import { INSTALL_COMMANDS } from "~/components/install-tabs";
import { selectedLanguage } from "~/lib/language-store";

/** One-line install command for the selected language, with copy. */
export function InstallCommand(props: { class?: string }) {
	const [copied, setCopied] = createSignal(false);
	let resetTimer: ReturnType<typeof setTimeout> | undefined;
	onCleanup(() => clearTimeout(resetTimer));

	const command = () =>
		(
			INSTALL_COMMANDS.find((c) => c.id === selectedLanguage().id) ??
			INSTALL_COMMANDS[0]
		).command;

	const copy = async () => {
		try {
			await navigator.clipboard.writeText(command());
			setCopied(true);
			clearTimeout(resetTimer);
			resetTimer = setTimeout(() => setCopied(false), 1800);
		} catch (error) {
			console.error("Failed to copy:", error);
		}
	};

	return (
		<button
			type="button"
			onClick={copy}
			class={`group inline-flex h-11 max-w-full items-center gap-2 rounded-xl border border-kite-line bg-kite-surface/80 pl-4 pr-3 font-mono text-[12px] tracking-[-0.02em] text-slate-300 backdrop-blur sm:gap-3 sm:text-[13px] sm:tracking-normal transition-colors duration-150 hover:border-white/20 hover:text-white ${props.class ?? ""}`}
			aria-label={`Copy install command: ${command()}`}
		>
			<span class="text-slate-600 select-none" aria-hidden="true">
				$
			</span>
			{/* One line; the copy button always copies the full command */}
			<span class="min-w-0 truncate whitespace-nowrap">{command()}</span>
			<span class="ml-1 grid h-6 w-6 place-items-center rounded-md text-slate-500 transition-colors group-hover:bg-white/[0.06] group-hover:text-slate-200">
				<Show when={copied()} fallback={<Copy size={13} aria-hidden="true" />}>
					<Check size={13} class="text-kite-mint" aria-hidden="true" />
				</Show>
			</span>
			<span class="sr-only" aria-live="polite">
				{copied() ? "Copied" : ""}
			</span>
		</button>
	);
}
