import type { Component } from 'solid-js'
import { createSignal, createEffect, onMount, onCleanup, For, Show } from 'solid-js'
import { useNavigate } from '@tanstack/solid-router'
import { Search, X } from 'lucide-solid'
import { search, type SearchResult } from '~/lib/search'

// Recommended pages shown when search is empty
const RECOMMENDED_PAGES: SearchResult[] = [
  { id: 'quick-start', title: 'Quick Start', description: 'Build your first graph database in 5 minutes', slug: 'getting-started/quick-start', section: 'Getting Started', score: 1 },
  { id: 'schema', title: 'Schema Definition', description: 'Define type-safe node and edge schemas', slug: 'guides/schema', section: 'Guides', score: 1 },
  { id: 'vectors', title: 'Vector Search', description: 'Semantic similarity search with embeddings', slug: 'guides/vectors', section: 'Guides', score: 1 },
]

interface SearchDialogProps {
  open: boolean
  onClose: () => void
}

export const SearchDialog: Component<SearchDialogProps> = (props) => {
  const [query, setQuery] = createSignal('')
  const [results, setResults] = createSignal<SearchResult[]>([])
  const [selectedIndex, setSelectedIndex] = createSignal(0)
  const navigate = useNavigate()
  let inputRef: HTMLInputElement | undefined
  let dialogRef: HTMLDivElement | undefined

  // Search when query changes
  createEffect(() => {
    const q = query()
    const searchResults = search(q, 8)
    setResults(searchResults)
    setSelectedIndex(0)
  })

  // Focus input when dialog opens
  createEffect(() => {
    if (props.open) {
      setTimeout(() => inputRef?.focus(), 10)
    } else {
      setQuery('')
      setResults([])
    }
  })

  // Get active list (search results or recommended pages)
  const activeList = () => results().length > 0 ? results() : (query().length === 0 ? RECOMMENDED_PAGES : [])

  // Handle keyboard navigation
  const handleKeyDown = (e: KeyboardEvent) => {
    const list = activeList()
    
    switch (e.key) {
      case 'ArrowDown':
        e.preventDefault()
        setSelectedIndex((i) => Math.min(i + 1, list.length - 1))
        break
      case 'ArrowUp':
        e.preventDefault()
        setSelectedIndex((i) => Math.max(i - 1, 0))
        break
      case 'Enter':
        e.preventDefault()
        if (list[selectedIndex()]) {
          navigateToResult(list[selectedIndex()])
        }
        break
      case 'Escape':
        e.preventDefault()
        props.onClose()
        break
    }
  }

  const navigateToResult = (result: SearchResult) => {
    const path = result.slug ? `/docs/${result.slug}` : '/docs'
    navigate({ to: path })
    props.onClose()
  }

  // Handle click outside
  const handleBackdropClick = (e: MouseEvent) => {
    if (e.target === e.currentTarget) {
      props.onClose()
    }
  }

  return (
    <Show when={props.open}>
      <div
        class="fixed inset-0 z-[100] flex items-start justify-center pt-[15vh] px-4 bg-black/60 backdrop-blur-sm"
        onClick={handleBackdropClick}
        role="dialog"
        aria-modal="true"
        aria-label="Search documentation"
      >
        <div
          ref={dialogRef}
          class="w-full max-w-xl console-container overflow-hidden"
          onKeyDown={handleKeyDown}
        >
          <div class="console-scanlines opacity-5" aria-hidden="true" />
          
          {/* Console header */}
          <div class="relative flex items-center gap-3 px-4 py-2.5 bg-[#0a1628] border-b border-[#1a2a42]">
            <div class="flex gap-1.5" aria-hidden="true">
              <div class="w-2.5 h-2.5 rounded-full bg-[#ff5f57]" />
              <div class="w-2.5 h-2.5 rounded-full bg-[#febc2e]" />
              <div class="w-2.5 h-2.5 rounded-full bg-[#28c840]" />
            </div>
            <span class="text-xs font-mono text-slate-500">search — kitedb docs</span>
          </div>

          {/* Search input - console style */}
          <div class="relative flex items-center gap-3 mx-4 my-4">
            <span class="text-[#00d4ff] font-mono text-base flex-shrink-0">❯</span>
            <input
              ref={inputRef}
              type="text"
              value={query()}
              onInput={(e) => setQuery(e.currentTarget.value)}
              placeholder="search_docs..."
              class="flex-1 px-3 py-1.5 rounded-md bg-[#0a1628]/50 border border-[#1a2a42] text-white placeholder-slate-600 outline-none ring-0 focus:outline-none focus:ring-0 focus-visible:outline-none focus-visible:ring-0 font-mono text-sm caret-[#00d4ff]"
              aria-label="Search query"
            />
            <span class="console-cursor h-5 w-2 flex-shrink-0" aria-hidden="true" />
          </div>

          {/* Results */}
          <div class="max-h-[50vh] overflow-y-auto border-t border-[#1a2a42]">
            <Show
              when={results().length > 0}
              fallback={
                <Show 
                  when={query().length > 0}
                  fallback={
                    /* Recommended pages when no query */
                    <div class="py-3">
                      <div class="px-4 py-2 text-xs font-mono text-slate-600">
                        <span class="text-slate-700">//</span> recommended
                      </div>
                      <For each={RECOMMENDED_PAGES}>
                        {(page, index) => (
                          <button
                            type="button"
                            class={`w-full flex items-start gap-3 px-4 py-3 text-left transition-colors ${
                              selectedIndex() === index()
                                ? 'bg-[#00d4ff]/10 border-l-2 border-[#00d4ff]'
                                : 'hover:bg-[#1a2a42]/50 border-l-2 border-transparent'
                            }`}
                            onClick={() => navigateToResult(page)}
                            onMouseEnter={() => setSelectedIndex(index())}
                          >
                            <span class={`font-mono text-xs flex-shrink-0 mt-0.5 ${
                              selectedIndex() === index() ? 'text-[#00d4ff]' : 'text-slate-600'
                            }`}>
                              →
                            </span>
                            <div class="flex-1 min-w-0">
                              <div class={`font-mono text-sm ${
                                selectedIndex() === index() ? 'text-[#00d4ff]' : 'text-white'
                              }`}>
                                {page.title}
                              </div>
                              <div class="text-xs text-slate-500 truncate mt-0.5">
                                {page.description}
                              </div>
                            </div>
                            <Show when={selectedIndex() === index()}>
                              <kbd class="px-1.5 py-0.5 rounded bg-[#1a2a42] text-[#00d4ff] text-xs font-mono flex-shrink-0">↵</kbd>
                            </Show>
                          </button>
                        )}
                      </For>
                    </div>
                  }
                >
                  <div class="px-4 py-8 text-center font-mono">
                    <span class="text-slate-600">// </span>
                    <span class="text-slate-500">no results for "</span>
                    <span class="text-[#00d4ff]">{query()}</span>
                    <span class="text-slate-500">"</span>
                  </div>
                </Show>
              }
            >
              <ul class="py-2" role="listbox">
                <For each={results()}>
                  {(result, index) => (
                    <li role="option" aria-selected={selectedIndex() === index()}>
                      <button
                        type="button"
                        class={`w-full flex items-start gap-3 px-4 py-3 text-left transition-colors ${
                          selectedIndex() === index()
                            ? 'bg-[#00d4ff]/10 border-l-2 border-[#00d4ff]'
                            : 'hover:bg-[#1a2a42]/50 border-l-2 border-transparent'
                        }`}
                        onClick={() => navigateToResult(result)}
                        onMouseEnter={() => setSelectedIndex(index())}
                      >
                        <span class={`font-mono text-xs flex-shrink-0 mt-0.5 ${
                          selectedIndex() === index() ? 'text-[#00d4ff]' : 'text-slate-600'
                        }`}>
                          →
                        </span>
                        <div class="flex-1 min-w-0">
                          <div class={`font-mono text-sm ${
                            selectedIndex() === index() ? 'text-[#00d4ff]' : 'text-white'
                          }`}>
                            {result.title}
                          </div>
                          <div class="text-xs text-slate-500 truncate mt-0.5">
                            {result.description}
                          </div>
                          <div class="text-xs text-slate-600 mt-1 font-mono">
                            <span class="text-slate-700">$</span> {result.section.toLowerCase().replace(/\s+/g, '_')}
                          </div>
                        </div>
                        <Show when={selectedIndex() === index()}>
                          <kbd class="px-1.5 py-0.5 rounded bg-[#1a2a42] text-[#00d4ff] text-xs font-mono flex-shrink-0">↵</kbd>
                        </Show>
                      </button>
                    </li>
                  )}
                </For>
              </ul>
            </Show>
          </div>

          {/* Footer with keyboard hints - console style */}
          <div class="flex items-center gap-6 px-4 py-2.5 border-t border-[#1a2a42] bg-[#0a1628]/50 text-xs font-mono text-slate-600">
            <span class="flex items-center gap-1.5">
              <kbd class="px-1.5 py-0.5 rounded bg-[#1a2a42] text-slate-500">↑↓</kbd>
              <span class="text-slate-500">nav</span>
            </span>
            <span class="flex items-center gap-1.5">
              <kbd class="px-1.5 py-0.5 rounded bg-[#1a2a42] text-slate-500">↵</kbd>
              <span class="text-slate-500">open</span>
            </span>
            <span class="flex items-center gap-1.5">
              <kbd class="px-1.5 py-0.5 rounded bg-[#1a2a42] text-slate-500">esc</kbd>
              <span class="text-slate-500">close</span>
            </span>
          </div>
        </div>
      </div>
    </Show>
  )
}

// Global search state
const [globalSearchOpen, setGlobalSearchOpen] = createSignal(false)

export const searchDialog = {
  isOpen: globalSearchOpen,
  open: () => setGlobalSearchOpen(true),
  close: () => setGlobalSearchOpen(false),
}

// Component that sets up the keyboard shortcut listener
export const SearchKeyboardShortcut: Component = () => {
  onMount(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      // Cmd/Ctrl + K to open search
      if ((e.metaKey || e.ctrlKey) && e.key === 'k') {
        e.preventDefault()
        setGlobalSearchOpen(true)
      }
    }

    document.addEventListener('keydown', handleKeyDown)
    onCleanup(() => document.removeEventListener('keydown', handleKeyDown))
  })

  return null
}

export default SearchDialog
