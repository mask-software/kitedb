import type { Component } from "solid-js";
import { createSignal, createResource, Suspense, Show } from "solid-js";
import { Check, Copy, Terminal } from "lucide-solid";
import { highlightCode } from "~/lib/highlighter";

interface CodeBlockProps {
	code: string;
	language?: string;
	filename?: string;
	class?: string;
}

export const CodeBlock: Component<CodeBlockProps> = (props) => {
	const [copied, setCopied] = createSignal(false);
	const [highlightedHtml] = createResource(
		() => ({ code: props.code, lang: props.language }),
		async ({ code, lang }) => {
			try {
				return await highlightCode(code, lang || "text");
			} catch (e) {
				console.error("Highlighting failed:", e);
				return null;
			}
		}
	);

	const copyToClipboard = async () => {
		try {
			await navigator.clipboard.writeText(props.code);
			setCopied(true);
			setTimeout(() => setCopied(false), 2000);
		} catch (err) {
			console.error("Failed to copy:", err);
		}
	};

	return (
		<div
			class={`group relative console-container overflow-hidden ${props.class ?? ""}`}
		>
			<div class="console-scanlines opacity-5" aria-hidden="true" />

			{/* Console-style header */}
			<Show when={props.filename || props.language}>
				<div class="relative flex items-center justify-between px-4 py-2.5 bg-[#0a1628] border-b border-[#1a2a42]">
					<div class="flex items-center gap-3">
						{/* Terminal dots */}
						<div class="flex gap-1.5" aria-hidden="true">
							<div class="w-2.5 h-2.5 rounded-full bg-[#ff5f57]" />
							<div class="w-2.5 h-2.5 rounded-full bg-[#febc2e]" />
							<div class="w-2.5 h-2.5 rounded-full bg-[#28c840]" />
						</div>
						<Show when={props.filename}>
							<span class="text-xs font-mono text-slate-400">
								{props.filename}
							</span>
						</Show>
						<Show when={props.language && !props.filename}>
							<span class="text-xs font-mono text-slate-500 uppercase tracking-wider">
								{props.language}
							</span>
						</Show>
					</div>
					<button
						type="button"
						onClick={copyToClipboard}
						class="flex items-center gap-1.5 px-2 py-1 text-xs font-mono rounded text-slate-500 hover:text-[#00d4ff] bg-[#1a2a42]/50 hover:bg-[#1a2a42] transition-colors duration-150 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[#00d4ff]"
						aria-label={copied() ? "Copied!" : "Copy code to clipboard"}
					>
						<Show
							when={copied()}
							fallback={<Copy size={12} aria-hidden="true" />}
						>
							<Check size={12} class="text-[#28c840]" aria-hidden="true" />
						</Show>
						<span>{copied() ? "copied" : "copy"}</span>
					</button>
				</div>
			</Show>

			{/* Code content with Shiki highlighting */}
			<div class="relative overflow-x-auto scrollbar-thin">
				<Suspense
					fallback={
						<pre class="p-4 text-sm leading-relaxed border-0">
							<code class="font-mono text-slate-300 whitespace-pre">
								{props.code}
							</code>
						</pre>
					}
				>
					<Show
						when={highlightedHtml()}
						fallback={
							<pre class="p-4 text-sm leading-relaxed border-0">
								<code class="font-mono text-slate-300 whitespace-pre">
									{props.code}
								</code>
							</pre>
						}
					>
						<div
							class="shiki-wrapper [&_pre]:p-4 [&_pre]:text-sm [&_pre]:leading-relaxed [&_pre]:bg-transparent! [&_pre]:border-0 [&_code]:font-mono"
							innerHTML={highlightedHtml() ?? undefined}
						/>
					</Show>
				</Suspense>
			</div>

			{/* Copy button overlay for blocks without header */}
			<Show when={!props.filename && !props.language}>
				<button
					type="button"
					onClick={copyToClipboard}
					class="absolute top-3 right-3 p-2 rounded text-slate-500 hover:text-[#00d4ff] bg-[#1a2a42]/80 hover:bg-[#1a2a42] transition-all duration-150 opacity-0 group-hover:opacity-100 focus:opacity-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[#00d4ff]"
					aria-label={copied() ? "Copied!" : "Copy code to clipboard"}
				>
					<Show
						when={copied()}
						fallback={<Copy size={14} aria-hidden="true" />}
					>
						<Check size={14} class="text-[#28c840]" aria-hidden="true" />
					</Show>
				</button>
			</Show>
		</div>
	);
};

export default CodeBlock;
