import type { JSX } from "solid-js";

interface SectionHeadingProps {
	id: string;
	eyebrow: string;
	title: JSX.Element;
	children?: JSX.Element;
	align?: "left" | "center";
}

export function SectionHeading(props: SectionHeadingProps) {
	return (
		<div
			class="reveal max-w-2xl"
			classList={{ "mx-auto text-center": props.align === "center" }}
		>
			<p
				class="eyebrow"
				classList={{ "justify-center": props.align === "center" }}
			>
				{props.eyebrow}
			</p>
			<h2
				id={props.id}
				class="mt-4 text-balance text-[2rem] font-semibold leading-[1.08] tracking-[-0.035em] text-white sm:text-[2.75rem]"
			>
				{props.title}
			</h2>
			{props.children && (
				<p class="mt-5 text-pretty text-[17px] leading-relaxed text-slate-400">
					{props.children}
				</p>
			)}
		</div>
	);
}
