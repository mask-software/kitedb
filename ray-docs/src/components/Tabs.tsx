import type { Component } from 'solid-js'
import { createSignal, createUniqueId, For, Show } from 'solid-js'
import CodeBlock from './CodeBlock'

interface TabItem {
  label: string
  code: string
  language?: string
}

interface TabsProps {
  items: TabItem[]
  defaultIndex?: number
}

export const Tabs: Component<TabsProps> = (props) => {
  const [activeIndex, setActiveIndex] = createSignal(props.defaultIndex ?? 0)
  const baseId = createUniqueId()

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
    <div class="console-container overflow-hidden">
      <div class="console-scanlines opacity-5" aria-hidden="true" />

      {/* Tab headers - console style */}
      <div
        class="relative flex bg-[#0a1628] border-b border-[#1a2a42]"
        role="tablist"
        aria-label="Code examples"
      >
        {/* Terminal dots */}
        <div class="flex items-center gap-1.5 px-4" aria-hidden="true">
          <div class="w-2.5 h-2.5 rounded-full bg-[#ff5f57]" />
          <div class="w-2.5 h-2.5 rounded-full bg-[#febc2e]" />
          <div class="w-2.5 h-2.5 rounded-full bg-[#28c840]" />
        </div>

        <For each={props.items}>
          {(item, index) => (
            <button
              type="button"
              role="tab"
              id={`${baseId}-tab-${index()}`}
              aria-selected={activeIndex() === index()}
              aria-controls={`${baseId}-tabpanel-${index()}`}
              tabIndex={activeIndex() === index() ? 0 : -1}
              class={`px-4 py-2.5 text-xs font-mono transition-colors duration-150 relative ${activeIndex() === index()
                  ? 'text-[#00d4ff] bg-[#030712]'
                  : 'text-slate-500 hover:text-white hover:bg-[#1a2a42]/40'
                }`}
              onClick={() => setActiveIndex(index())}
              onKeyDown={(e) => handleKeyDown(e, index())}
            >
              {item.label.toLowerCase().replace(/\s+/g, '_')}
              <Show when={activeIndex() === index()}>
                <div
                  class="absolute bottom-0 left-0 right-0 h-0.5 bg-[#00d4ff] shadow-[0_0_10px_rgba(0,212,255,0.5)]"
                  aria-hidden="true"
                />
              </Show>
            </button>
          )}
        </For>
      </div>

      {/* Tab content */}
      <div class="relative">
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
              <CodeBlock code={item.code} language={item.language} />
            </div>
          )}
        </For>
      </div>
    </div>
  )
}

export default Tabs
