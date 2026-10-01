import { For } from "solid-js";

/** Renders `backtick` spans in plain copy as inline code, without innerHTML. */
export function InlineCode(props: { text: string }) {
	const parts = () => props.text.split("`");
	return (
		<For each={parts()}>
			{(part, i) =>
				i() % 2 === 1 ? (
					<code class="rounded bg-white/[0.06] px-1 py-px font-mono text-[0.88em] text-slate-100">
						{part}
					</code>
				) : (
					part
				)
			}
		</For>
	);
}
