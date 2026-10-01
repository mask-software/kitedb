import { createEffect, createSignal, For } from "solid-js";
import { highlightTokens, peekTokens } from "~/lib/highlighter";

const ITALIC = 1; // shiki FontStyle.Italic bit

type Line = { content: string; color?: string; italic: boolean }[];

interface CodeViewProps {
	code: string;
	lang: string;
	/** Zero-based lines to emphasize; when provided, the rest recede. */
	activeLines?: readonly number[];
	showLineNumbers?: boolean;
	class?: string;
}

/**
 * Token-rendered code (no innerHTML) using the kite-night theme. Renders plain
 * text on the server and until tokens resolve, with an identical layout, so
 * hydration matches and nothing shifts when color arrives.
 */
export function CodeView(props: CodeViewProps) {
	const [tokenVersion, setTokenVersion] = createSignal(0);

	createEffect(() => {
		const code = props.code;
		const lang = props.lang;
		if (peekTokens(code, lang)) return;
		highlightTokens(code, lang)
			.then(() => setTokenVersion((v) => v + 1))
			.catch((error) => console.error("Highlighting failed:", error));
	});

	const lines = (): Line[] => {
		tokenVersion();
		const tokens = peekTokens(props.code, props.lang);
		if (!tokens) {
			return props.code
				.split("\n")
				.map((content) => [{ content, italic: false }]);
		}
		return tokens.map((line) =>
			line.map((token) => ({
				content: token.content,
				color: token.color,
				// FontStyle.NotSet is -1, so only positive bitmasks carry flags
				italic:
					(token.fontStyle ?? 0) > 0 && ((token.fontStyle ?? 0) & ITALIC) !== 0,
			})),
		);
	};

	const isActive = (index: number) =>
		props.activeLines === undefined || props.activeLines.includes(index);

	return (
		<pre class={`font-mono text-[13px] leading-[1.75] ${props.class ?? ""}`}>
			<code class="block min-w-max">
				<For each={lines()}>
					{(line, index) => (
						<span
							class="code-line"
							data-dim={props.activeLines !== undefined && !isActive(index())}
							data-lit={props.activeLines !== undefined && isActive(index())}
						>
							{(props.showLineNumbers ?? true) && (
								<span class="code-line-no" aria-hidden="true">
									{index() + 1}
								</span>
							)}
							<For each={line}>
								{(token) => (
									<span
										style={{
											color: token.color,
											"font-style": token.italic ? "italic" : undefined,
										}}
									>
										{token.content}
									</span>
								)}
							</For>
						</span>
					)}
				</For>
			</code>
		</pre>
	);
}
