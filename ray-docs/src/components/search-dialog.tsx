import { useNavigate } from "@tanstack/solid-router";
import { CornerDownLeft, FileText, Search } from "lucide-solid";
import type { Component } from "solid-js";
import {
	createEffect,
	createSignal,
	For,
	onCleanup,
	onMount,
	Show,
} from "solid-js";
import { findDocBySlug, findSectionBySlug } from "~/lib/docs";
import { type SearchResult, search } from "~/lib/search";

// Shown before the user types anything
const RECOMMENDED_SLUGS = [
	"getting-started/quick-start",
	"guides/schema",
	"guides/traversal",
	"guides/vectors",
];

const RECOMMENDED_PAGES: SearchResult[] = RECOMMENDED_SLUGS.flatMap((slug) => {
	const doc = findDocBySlug(slug);
	return doc
		? [
				{
					id: slug,
					title: doc.title,
					description: doc.description,
					slug,
					section: findSectionBySlug(slug)?.label ?? "",
					score: 1,
				},
			]
		: [];
});

interface SearchDialogProps {
	open: boolean;
	onClose: () => void;
}

export const SearchDialog: Component<SearchDialogProps> = (props) => {
	const [query, setQuery] = createSignal("");
	const [selectedIndex, setSelectedIndex] = createSignal(0);
	const navigate = useNavigate();
	let inputRef: HTMLInputElement | undefined;

	const results = () => search(query(), 8);
	const activeList = () =>
		query().trim().length === 0 ? RECOMMENDED_PAGES : results();

	createEffect(() => {
		query();
		setSelectedIndex(0);
	});

	createEffect(() => {
		if (props.open) {
			setTimeout(() => inputRef?.focus(), 10);
		} else {
			setQuery("");
		}
	});

	const navigateToResult = (result: SearchResult) => {
		navigate({ to: result.slug ? `/docs/${result.slug}` : "/docs" });
		props.onClose();
	};

	const handleKeyDown = (event: KeyboardEvent) => {
		const list = activeList();
		switch (event.key) {
			case "ArrowDown":
				event.preventDefault();
				setSelectedIndex((i) => Math.min(i + 1, list.length - 1));
				break;
			case "ArrowUp":
				event.preventDefault();
				setSelectedIndex((i) => Math.max(i - 1, 0));
				break;
			case "Enter": {
				event.preventDefault();
				const selected = list[selectedIndex()];
				if (selected) navigateToResult(selected);
				break;
			}
			case "Escape":
				event.preventDefault();
				props.onClose();
				break;
		}
	};

	return (
		<Show when={props.open}>
			<div class="fixed inset-0 z-[100] flex items-start justify-center px-4 pt-[14vh]">
				{/* Pointer-only dismissal; keyboard users close with Escape from the input */}
				<div
					class="absolute inset-0 bg-black/60 backdrop-blur-sm"
					onClick={() => props.onClose()}
					aria-hidden="true"
				/>
				<div
					class="relative w-full max-w-xl overflow-hidden rounded-2xl border border-kite-line bg-kite-surface shadow-[0_40px_120px_-30px_rgba(0,0,0,0.9)]"
					role="dialog"
					aria-modal="true"
					aria-label="Search documentation"
				>
					<div class="flex items-center gap-3 border-b border-kite-line px-4">
						<Search
							size={16}
							class="shrink-0 text-slate-500"
							aria-hidden="true"
						/>
						<input
							ref={inputRef}
							type="text"
							value={query()}
							onInput={(event) => setQuery(event.currentTarget.value)}
							placeholder="Search the docs"
							class="h-14 flex-1 bg-transparent text-[15px] text-white placeholder-slate-500 outline-none focus:outline-none focus-visible:ring-0"
							onKeyDown={handleKeyDown}
							role="combobox"
							aria-expanded={activeList().length > 0}
							aria-autocomplete="list"
							aria-label="Search query"
							aria-controls="search-results"
							aria-activedescendant={
								activeList().length > 0
									? `search-option-${selectedIndex()}`
									: undefined
							}
						/>
						<kbd class="rounded border border-kite-line px-1.5 py-px font-mono text-[10px] text-slate-500">
							esc
						</kbd>
					</div>

					<div class="max-h-[52vh] overflow-y-auto p-2">
						<Show
							when={activeList().length > 0}
							fallback={
								<p class="px-4 py-10 text-center text-[14px] text-slate-500">
									No pages match “{query()}”.
								</p>
							}
						>
							<Show when={query().trim().length === 0}>
								<p class="px-3 pb-1 pt-2 font-mono text-[11px] uppercase tracking-[0.08em] text-slate-600">
									Suggested
								</p>
							</Show>
							<div id="search-results" role="listbox" aria-label="Results">
								<For each={activeList()}>
									{(result, index) => (
										<div
											id={`search-option-${index()}`}
											role="option"
											tabIndex={-1}
											aria-selected={selectedIndex() === index()}
											class="flex cursor-pointer items-start gap-3 rounded-lg px-3 py-2.5 transition-colors duration-100"
											classList={{
												"bg-white/[0.06]": selectedIndex() === index(),
											}}
											onClick={() => navigateToResult(result)}
											onKeyDown={handleKeyDown}
											onMouseEnter={() => setSelectedIndex(index())}
										>
											<FileText
												size={15}
												class="mt-0.5 shrink-0 text-slate-600"
												aria-hidden="true"
											/>
											<span class="min-w-0 flex-1">
												<span class="flex items-baseline gap-2">
													<span class="text-[14px] font-medium text-slate-100">
														{result.title}
													</span>
													<span class="text-[12px] text-slate-600">
														{result.section}
													</span>
												</span>
												<span class="mt-0.5 block truncate text-[13px] text-slate-500">
													{result.description}
												</span>
											</span>
											<Show when={selectedIndex() === index()}>
												<CornerDownLeft
													size={14}
													class="mt-1 shrink-0 text-slate-500"
													aria-hidden="true"
												/>
											</Show>
										</div>
									)}
								</For>
							</div>
						</Show>
					</div>

					<div class="flex items-center gap-5 border-t border-kite-line px-4 py-2.5 text-[12px] text-slate-500">
						<span class="flex items-center gap-1.5">
							<kbd class="rounded border border-kite-line px-1 font-mono text-[10px]">
								↑↓
							</kbd>
							navigate
						</span>
						<span class="flex items-center gap-1.5">
							<kbd class="rounded border border-kite-line px-1 font-mono text-[10px]">
								↵
							</kbd>
							open
						</span>
					</div>
				</div>
			</div>
		</Show>
	);
};

// Global search state
const [globalSearchOpen, setGlobalSearchOpen] = createSignal(false);

export const searchDialog = {
	isOpen: globalSearchOpen,
	open: () => setGlobalSearchOpen(true),
	close: () => setGlobalSearchOpen(false),
};

/** Registers the ⌘K / Ctrl+K shortcut that opens search. */
export const SearchKeyboardShortcut: Component = () => {
	onMount(() => {
		const handleKeyDown = (event: KeyboardEvent) => {
			if ((event.metaKey || event.ctrlKey) && event.key === "k") {
				event.preventDefault();
				setGlobalSearchOpen(true);
			}
		};
		document.addEventListener("keydown", handleKeyDown);
		onCleanup(() => document.removeEventListener("keydown", handleKeyDown));
	});

	return null;
};

export default SearchDialog;
