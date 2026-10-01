import { Check, Copy } from "lucide-solid";
import { type Component, createSignal, onCleanup, Show } from "solid-js";
import { CodeView } from "~/components/code-view";

interface CodeBlockProps {
	code: string;
	language?: string;
	filename?: string;
	class?: string;
	showLineNumbers?: boolean;
	showHeader?: boolean;
	/** Minimal style: highlighted code on a subtle surface, no header or gutter */
	inline?: boolean;
}

const LANGUAGE_LABELS: Record<string, string> = {
	typescript: "TypeScript",
	ts: "TypeScript",
	tsx: "TSX",
	javascript: "JavaScript",
	js: "JavaScript",
	rust: "Rust",
	rs: "Rust",
	python: "Python",
	py: "Python",
	bash: "Shell",
	sh: "Shell",
	shell: "Shell",
	json: "JSON",
};

export const CodeBlock: Component<CodeBlockProps> = (props) => {
	const [copied, setCopied] = createSignal(false);
	let resetTimer: ReturnType<typeof setTimeout> | undefined;
	onCleanup(() => clearTimeout(resetTimer));

	const lang = () => props.language ?? "text";
	const label = () =>
		props.filename ??
		(props.language ? LANGUAGE_LABELS[props.language] : undefined);
	const showHeader = () => (props.showHeader ?? !props.inline) && !!label();

	const copy = async () => {
		try {
			await navigator.clipboard.writeText(props.code);
			setCopied(true);
			clearTimeout(resetTimer);
			resetTimer = setTimeout(() => setCopied(false), 1800);
		} catch (error) {
			console.error("Failed to copy:", error);
		}
	};

	const CopyButton = (buttonProps: { floating?: boolean }) => (
		<button
			type="button"
			onClick={copy}
			class="grid h-7 w-7 place-items-center rounded-md text-slate-500 transition-colors hover:bg-white/[0.06] hover:text-slate-200"
			classList={{
				"absolute right-2.5 top-2.5 bg-kite-bg/80 opacity-0 backdrop-blur group-hover:opacity-100 focus-visible:opacity-100":
					buttonProps.floating,
				"ml-auto": !buttonProps.floating,
			}}
			aria-label={copied() ? "Copied" : "Copy code"}
		>
			<Show when={copied()} fallback={<Copy size={13} aria-hidden="true" />}>
				<Check size={13} class="text-kite-mint" aria-hidden="true" />
			</Show>
		</button>
	);

	return (
		<Show
			when={!props.inline}
			fallback={
				<div
					class={`group relative my-5 overflow-x-auto rounded-lg border border-kite-line bg-white/[0.02] py-3 ${props.class ?? ""}`}
				>
					<CodeView code={props.code} lang={lang()} showLineNumbers={false} />
					<CopyButton floating />
				</div>
			}
		>
			<div
				class={`group relative my-6 overflow-hidden rounded-xl border border-kite-line bg-[#070a12] ${props.class ?? ""}`}
			>
				<Show when={showHeader()}>
					<div class="flex items-center gap-3 border-b border-kite-line px-4 py-2">
						<span class="font-mono text-[12px] text-slate-500">{label()}</span>
						<CopyButton />
					</div>
				</Show>
				<div class="overflow-x-auto py-3.5">
					<CodeView
						code={props.code}
						lang={lang()}
						showLineNumbers={props.showLineNumbers ?? true}
					/>
				</div>
				<Show when={!showHeader()}>
					<CopyButton floating />
				</Show>
			</div>
		</Show>
	);
};

export default CodeBlock;
