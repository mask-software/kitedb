import { Check, Copy } from "lucide-solid";
import type { Component } from "solid-js";
import { createSignal, For, onCleanup, Show } from "solid-js";
import {
	LANGUAGES,
	selectedLanguage,
	setSelectedLanguage,
} from "~/lib/language-store";

export interface InstallCommand {
	id: string;
	label: string;
	command: string;
	secondary?: string;
}

export const INSTALL_COMMANDS: InstallCommand[] = [
	{
		id: "typescript",
		label: "TypeScript",
		command: "bun add @kitedb/core",
		secondary: "npm install @kitedb/core",
	},
	{
		id: "rust",
		label: "Rust",
		// The default `napi` feature builds the Node.js binding layer; Rust users don't need it.
		command: "cargo add kitedb --no-default-features",
	},
	{
		id: "python",
		label: "Python",
		command: "pip install kitedb",
		secondary: "uv add kitedb",
	},
];

/** One copyable shell command line. `muted` renders the alternative package manager. */
function CommandLine(props: { command: string; muted?: boolean }) {
	const [copied, setCopied] = createSignal(false);
	let resetTimer: ReturnType<typeof setTimeout> | undefined;
	onCleanup(() => clearTimeout(resetTimer));

	const copy = async () => {
		try {
			await navigator.clipboard.writeText(props.command);
			setCopied(true);
			clearTimeout(resetTimer);
			resetTimer = setTimeout(() => setCopied(false), 1800);
		} catch (error) {
			console.error("Failed to copy:", error);
		}
	};

	return (
		<div
			class="flex items-center gap-3 pl-4 pr-2 font-mono"
			classList={{
				"h-12 text-[13px]": !props.muted,
				"h-10 border-t border-kite-line text-[12px]": props.muted,
			}}
		>
			<span
				class="select-none"
				classList={{
					"text-slate-600": !props.muted,
					"text-slate-700": props.muted,
				}}
				aria-hidden="true"
			>
				$
			</span>
			<code
				class="min-w-0 truncate border-0 bg-transparent p-0 text-[1em]"
				classList={{
					"text-slate-200": !props.muted,
					"text-slate-500": props.muted,
				}}
			>
				{props.command}
			</code>
			<button
				type="button"
				onClick={copy}
				class="ml-auto grid h-7 w-7 shrink-0 place-items-center rounded-md text-slate-500 transition-colors hover:bg-white/[0.06] hover:text-slate-200"
				aria-label={copied() ? "Copied" : `Copy command: ${props.command}`}
			>
				<Show when={copied()} fallback={<Copy size={13} aria-hidden="true" />}>
					<Check size={13} class="text-kite-mint" aria-hidden="true" />
				</Show>
			</button>
		</div>
	);
}

/**
 * Install commands per language, bound to the global language preference
 * (the same one that drives the header toggle and every multi-language sample).
 */
export const InstallTabs: Component = () => {
	const active = () =>
		INSTALL_COMMANDS.find((c) => c.id === selectedLanguage().id) ??
		INSTALL_COMMANDS[0];

	const select = (id: string) => {
		const lang = LANGUAGES.find((l) => l.id === id);
		if (lang) setSelectedLanguage(lang);
	};

	return (
		<div class="not-prose">
			<fieldset class="inline-flex items-center rounded-lg border border-kite-line p-0.5">
				<legend class="sr-only">Language</legend>
				<For each={INSTALL_COMMANDS}>
					{(cmd) => (
						<button
							type="button"
							aria-pressed={active().id === cmd.id}
							class="rounded-md px-3 py-1 font-mono text-[12px] tracking-wide text-slate-500 transition-colors duration-150 hover:text-slate-200 aria-pressed:bg-white/[0.07] aria-pressed:text-white"
							onClick={() => select(cmd.id)}
						>
							{cmd.label}
						</button>
					)}
				</For>
			</fieldset>

			<div class="mt-3 overflow-hidden rounded-xl border border-kite-line bg-kite-surface/60">
				<CommandLine command={active().command} />
				<Show when={active().secondary}>
					{(secondary) => <CommandLine command={secondary()} muted />}
				</Show>
			</div>
		</div>
	);
};

export default InstallTabs;
