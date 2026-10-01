import { ArrowDown, Check } from "lucide-solid";
import { For, type JSX, Show } from "solid-js";

// ============================================================================
// ACCENTS
// cyan = reads / graph, violet = vectors / checkpoints, mint = writes / results,
// amber = tradeoffs, red = problems, slate = neutral. "emerald" is accepted as a
// legacy alias for mint.
// ============================================================================

export type Accent = "cyan" | "violet" | "mint" | "amber" | "red" | "slate";
type AccentInput = string | undefined;

const resolve = (accent: AccentInput): Accent | undefined => {
	if (accent === "emerald") return "mint";
	return accent && accent in ACCENT_DOT ? (accent as Accent) : undefined;
};

/** Small solid marks: dots, bars. */
export const ACCENT_DOT: Record<Accent, string> = {
	cyan: "bg-kite-cyan",
	violet: "bg-kite-violet",
	mint: "bg-kite-mint",
	amber: "bg-amber-400",
	red: "bg-red-400",
	slate: "bg-slate-500",
};

/** Tinted chips, tags, highlighted cells. */
export const ACCENT_TINT: Record<Accent, string> = {
	cyan: "border-kite-cyan/25 bg-kite-cyan/10 text-kite-cyan",
	violet: "border-kite-violet/25 bg-kite-violet/10 text-kite-violet",
	mint: "border-kite-mint/25 bg-kite-mint/10 text-kite-mint",
	amber: "border-amber-400/25 bg-amber-400/10 text-amber-300",
	red: "border-red-400/25 bg-red-400/10 text-red-400",
	slate: "border-kite-line bg-white/[0.04] text-slate-300",
};

/** Outlined markers that sit on a line (step numbers). */
const ACCENT_RING: Record<Accent, string> = {
	cyan: "border-kite-cyan/30 text-kite-cyan",
	violet: "border-kite-violet/30 text-kite-violet",
	mint: "border-kite-mint/30 text-kite-mint",
	amber: "border-amber-400/30 text-amber-300",
	red: "border-red-400/30 text-red-400",
	slate: "border-kite-line text-slate-400",
};

const ACCENT_TEXT: Record<Accent, string> = {
	cyan: "text-kite-cyan",
	violet: "text-kite-violet",
	mint: "text-kite-mint",
	amber: "text-amber-300",
	red: "text-red-400",
	slate: "text-slate-300",
};

const dot = (accent: AccentInput) => ACCENT_DOT[resolve(accent) ?? "slate"];
const tint = (accent: AccentInput) => ACCENT_TINT[resolve(accent) ?? "slate"];
const ring = (accent: AccentInput) => ACCENT_RING[resolve(accent) ?? "slate"];

// ============================================================================
// FIGURES
// ============================================================================

const FIGURE_SURFACE = {
	default: "border-kite-line bg-kite-surface/60",
	problem: "border-red-400/25 bg-red-400/[0.04]",
	tradeoff: "border-amber-400/25 bg-amber-400/[0.04]",
} as const;

/** Diagram card: optional accent dot, title, and a mono meta label on the right. */
export function Figure(props: {
	title: string;
	accent?: Accent;
	meta?: string;
	variant?: keyof typeof FIGURE_SURFACE;
	children: JSX.Element;
}) {
	return (
		<figure
			class={`rounded-xl border p-5 ${FIGURE_SURFACE[props.variant ?? "default"]}`}
		>
			<figcaption class="mb-4 flex flex-wrap items-center gap-x-2.5 gap-y-1">
				<Show when={props.accent}>
					{(accent) => (
						<span
							class={`h-1.5 w-1.5 shrink-0 rounded-full ${ACCENT_DOT[accent()]}`}
							aria-hidden="true"
						/>
					)}
				</Show>
				<span class="text-[15px] font-semibold text-white">{props.title}</span>
				<Show when={props.meta}>
					<span class="ml-auto font-mono text-[11px] text-slate-500">
						{props.meta}
					</span>
				</Show>
			</figcaption>
			{props.children}
		</figure>
	);
}

/** Inset panel inside a figure. */
export function Panel(props: {
	label: string;
	accent?: Accent;
	meta?: string;
	children: JSX.Element;
}) {
	return (
		<div class="rounded-lg border border-kite-line bg-white/[0.02] p-4">
			<div class="mb-3 flex flex-wrap items-center justify-between gap-x-4 gap-y-1">
				<span class="flex items-center gap-2 text-[14px] font-semibold text-slate-100">
					<Show when={props.accent}>
						{(accent) => (
							<span
								class={`h-1.5 w-1.5 shrink-0 rounded-full ${ACCENT_DOT[accent()]}`}
								aria-hidden="true"
							/>
						)}
					</Show>
					{props.label}
				</span>
				<Show when={props.meta}>
					<span class="font-mono text-[11px] text-slate-500">{props.meta}</span>
				</Show>
			</div>
			{props.children}
		</div>
	);
}

// ============================================================================
// STEPS
// ============================================================================

/** Round step marker; use inside custom layouts. */
export function StepNumber(props: { children: JSX.Element; accent?: Accent }) {
	return (
		<span
			class={`grid h-6 w-6 shrink-0 place-items-center rounded-full border bg-kite-bg font-mono text-[11px] ${ring(props.accent)}`}
		>
			{props.children}
		</span>
	);
}

export interface Step {
	text: JSX.Element;
	accent?: Accent;
	/** Render a check instead of the number and emphasize the text */
	done?: boolean;
	sub?: JSX.Element;
	note?: string;
}

/** Numbered sequence joined by a hairline. */
export function Steps(props: { steps: Step[] }) {
	return (
		<div class="relative">
			<span
				class="absolute top-3 bottom-3 left-3 w-px bg-kite-line"
				aria-hidden="true"
			/>
			<ol class="relative space-y-3">
				<For each={props.steps}>
					{(step, i) => (
						<li class="flex items-start gap-3">
							<StepNumber accent={step.accent}>
								<Show when={step.done} fallback={i() + 1}>
									<Check size={13} stroke-width={2.5} />
								</Show>
							</StepNumber>
							<div class="flex min-w-0 flex-1 flex-wrap items-baseline justify-between gap-x-4 pt-[3px]">
								<div>
									<p
										class={`text-[14px] ${step.done ? "font-medium text-kite-mint" : "text-slate-200"}`}
									>
										{step.text}
									</p>
									<Show when={step.sub}>
										<p class="mt-0.5 text-[13px] text-slate-500">{step.sub}</p>
									</Show>
								</div>
								<Show when={step.note}>
									<span class="font-mono text-[11px] text-slate-500">
										{step.note}
									</span>
								</Show>
							</div>
						</li>
					)}
				</For>
			</ol>
		</div>
	);
}

// ============================================================================
// CELLS
// ============================================================================

/** Class strings for inline byte/array cells. */
export const CELL =
	"rounded-md border px-2 py-1 font-mono text-[12px] leading-5 text-center";
export const CELL_PLAIN = "border-kite-line bg-white/[0.03] text-slate-200";
export const CELL_HIGHLIGHT = ACCENT_TINT.cyan;

/** Fixed-height array slot / memory cell. */
export function Slot(props: {
	children: JSX.Element;
	tone?: Accent;
	class?: string;
}) {
	return (
		<span
			class={`grid h-8 place-items-center rounded-md border font-mono text-[12px] ${
				props.tone ? ACCENT_TINT[props.tone] : CELL_PLAIN
			} ${props.class ?? "w-10"}`}
		>
			{props.children}
		</span>
	);
}

// ============================================================================
// TEXT
// ============================================================================

/** Inline code inside diagrams. */
export function Code(props: { children: JSX.Element; color?: string }) {
	return (
		<code class="rounded border border-kite-line bg-white/[0.04] px-1 py-px font-mono text-[0.85em] text-slate-100">
			{props.children}
		</code>
	);
}

/** Emphasized term for storage-layer items. */
export function Label(props: { children: JSX.Element; color?: string }) {
	return (
		<strong
			class={`font-semibold ${ACCENT_TEXT[resolve(props.color) ?? "cyan"]}`}
		>
			{props.children}
		</strong>
	);
}

// ============================================================================
// FLOW DIAGRAMS
// ============================================================================

/** One line inside a flow step. */
export function FlowItem(props: {
	isLast?: boolean;
	color: string;
	children: JSX.Element;
}) {
	return (
		<div class="flex items-start gap-3 text-[14px] leading-relaxed text-slate-300">
			<span
				class={`mt-[0.6em] h-1.5 w-1.5 shrink-0 rounded-full opacity-70 ${dot(props.color)}`}
				aria-hidden="true"
			/>
			<span>{props.children}</span>
		</div>
	);
}

/** Numbered step card in a data-flow diagram. */
export function FlowStep(props: {
	number: string;
	title: string;
	color: string;
	children: JSX.Element;
}) {
	return (
		<div class="rounded-xl border border-kite-line bg-kite-surface/60 p-5">
			<div class="mb-3 flex items-center gap-3">
				<span
					class={`grid h-6 w-6 shrink-0 place-items-center rounded-md border font-mono text-[12px] ${tint(props.color)}`}
				>
					{props.number}
				</span>
				<h4 class="text-[15px] font-semibold text-white">{props.title}</h4>
			</div>
			<div class="space-y-1.5">{props.children}</div>
		</div>
	);
}

/** Connector between stacked flow steps. */
export function FlowArrow() {
	return (
		<div class="flex justify-center py-1 text-slate-600" aria-hidden="true">
			<ArrowDown size={16} />
		</div>
	);
}

// ============================================================================
// BADGES
// ============================================================================

export function RecordTypeBadge(props: { name: string; color: string }) {
	return (
		<span
			class={`rounded-md border px-2 py-0.5 font-mono text-[11px] ${tint(props.color)}`}
		>
			{props.name}
		</span>
	);
}

/** Numbered checkpoint stage; color advances through the read, merge, write phases. */
export function CheckpointStep(props: { num: number; text: string }) {
	const accent = (): Accent =>
		props.num <= 2 ? "cyan" : props.num <= 4 ? "violet" : "mint";
	return (
		<div class="relative flex items-center gap-3">
			<span
				class={`z-10 grid h-6 w-6 shrink-0 place-items-center rounded-full border font-mono text-[11px] ${ACCENT_TINT[accent()]}`}
			>
				{props.num}
			</span>
			<span class="text-[14px] text-slate-300">{props.text}</span>
		</div>
	);
}
