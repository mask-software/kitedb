import type { Component } from 'solid-js'
import { createSignal, createUniqueId, For, Show } from 'solid-js'
import { ChevronDown, Check } from 'lucide-solid'
import CodeBlock from './code-block'
import { LANGUAGES, selectedLanguage, setSelectedLanguage } from '~/lib/language-store'

interface MultiLangCode {
  typescript: string
  rust: string
  python: string
}

interface TabItem {
  label: string
  code: MultiLangCode
}

interface MultiLangTabsProps {
  items: TabItem[]
  defaultIndex?: number
}

/**
 * A tabbed code display component that respects the global language preference.
 * Combines tab switching (for different code examples) with language switching.
 */
export const MultiLangTabs: Component<MultiLangTabsProps> = (props) => {
  const [activeIndex, setActiveIndex] = createSignal(props.defaultIndex ?? 0)
  const [langDropdownOpen, setLangDropdownOpen] = createSignal(false)
  const baseId = createUniqueId()

  const getCodeForLanguage = (code: MultiLangCode) => {
    switch (selectedLanguage().id) {
      case 'rust':
        return code.rust
      case 'python':
        return code.python
      default:
        return code.typescript
    }
  }

  const getLanguageId = () => {
    switch (selectedLanguage().id) {
      case 'rust':
        return 'rust'
      case 'python':
        return 'python'
      default:
        return 'typescript'
    }
  }

  const handleKeyDown = (e: KeyboardEvent, index: number) => {
    if (e.key === 'ArrowRight') {
      e.preventDefault()
      const nextIndex = (index + 1) % props.items.length
      setActiveIndex(nextIndex)
      document.getElementById(`${baseId}-tab-${nextIndex}`)?.focus()
    } else if (e.key === 'ArrowLeft') {
      e.preventDefault()
      const prevIndex = (index - 1 + props.items.length) % props.items.length
      setActiveIndex(prevIndex)
      document.getElementById(`${baseId}-tab-${prevIndex}`)?.focus()
    } else if (e.key === 'Home') {
      e.preventDefault()
      setActiveIndex(0)
      document.getElementById(`${baseId}-tab-0`)?.focus()
    } else if (e.key === 'End') {
      e.preventDefault()
      setActiveIndex(props.items.length - 1)
      document.getElementById(`${baseId}-tab-${props.items.length - 1}`)?.focus()
    }
  }

  return (
    <div>
      {/* Standalone tabs above the code window */}
      <div class="flex items-center justify-between mb-3">
        {/* Tab buttons */}
        <div
          class="inline-flex items-center gap-1 p-1 rounded-lg bg-[#0a1628] border border-[#1a2a42]"
          role="tablist"
          aria-label="Code examples"
        >
          <For each={props.items}>
            {(item, index) => (
              <button
                type="button"
                role="tab"
                id={`${baseId}-tab-${index()}`}
                aria-selected={activeIndex() === index()}
                aria-controls={`${baseId}-tabpanel-${index()}`}
                tabIndex={activeIndex() === index() ? 0 : -1}
                class={`px-3 py-1.5 text-xs font-mono rounded-md transition-all duration-150 ${
                  activeIndex() === index()
                    ? 'text-[#00d4ff] bg-[#00d4ff]/10 shadow-[0_0_10px_rgba(0,212,255,0.2)]'
                    : 'text-slate-500 hover:text-white hover:bg-[#1a2a42]/50'
                }`}
                onClick={() => setActiveIndex(index())}
                onKeyDown={(e) => handleKeyDown(e, index())}
              >
                {item.label.toLowerCase().replace(/\s+/g, '_')}
              </button>
            )}
          </For>
        </div>

        {/* Language dropdown */}
        <div class="relative">
          <button
            type="button"
            class="flex items-center gap-1.5 px-3 py-1.5 text-xs font-mono rounded-lg text-slate-400 hover:text-[#00d4ff] bg-[#0a1628] border border-[#1a2a42] hover:border-[#00d4ff]/30 transition-colors duration-150"
            onClick={() => setLangDropdownOpen(!langDropdownOpen())}
            aria-expanded={langDropdownOpen()}
            aria-haspopup="listbox"
          >
            {selectedLanguage().label}
            <ChevronDown
              size={12}
              class={`transition-transform duration-150 ${langDropdownOpen() ? 'rotate-180' : ''}`}
            />
          </button>

          <Show when={langDropdownOpen()}>
            {/* Backdrop */}
            <div
              class="fixed inset-0 z-40"
              onClick={() => setLangDropdownOpen(false)}
              aria-hidden="true"
            />
            <div
              class="absolute right-0 top-full mt-1 z-50 min-w-[120px] py-1 rounded-lg bg-[#0a1628] border border-[#1a2a42] shadow-xl shadow-black/50"
              role="listbox"
            >
              <For each={LANGUAGES}>
                {(lang) => (
                  <button
                    type="button"
                    role="option"
                    aria-selected={selectedLanguage().id === lang.id}
                    class={`w-full flex items-center gap-2 px-3 py-2 text-xs font-mono transition-colors ${
                      selectedLanguage().id === lang.id
                        ? 'text-[#00d4ff] bg-[#00d4ff]/10'
                        : 'text-slate-400 hover:text-white hover:bg-[#1a2a42]/50'
                    }`}
                    onClick={() => {
                      setSelectedLanguage(lang)
                      setLangDropdownOpen(false)
                    }}
                  >
                    <Show when={selectedLanguage().id === lang.id}>
                      <Check size={12} class="text-[#00d4ff]" />
                    </Show>
                    <Show when={selectedLanguage().id !== lang.id}>
                      <div class="w-3" />
                    </Show>
                    {lang.label}
                  </button>
                )}
              </For>
            </div>
          </Show>
        </div>
      </div>

      {/* Code window */}
      <For each={props.items}>
        {(item, index) => (
          <div
            role="tabpanel"
            id={`${baseId}-tabpanel-${index()}`}
            aria-labelledby={`${baseId}-tab-${index()}`}
            aria-hidden={activeIndex() !== index()}
            style={{ display: activeIndex() === index() ? 'block' : 'none' }}
            tabIndex={0}
          >
            <CodeBlock code={getCodeForLanguage(item.code)} language={getLanguageId()} />
          </div>
        )}
      </For>
    </div>
  )
}

export default MultiLangTabs
