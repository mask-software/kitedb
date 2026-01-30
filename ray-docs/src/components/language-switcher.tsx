import type { Component } from 'solid-js'
import { createSignal, For, Show } from 'solid-js'
import { ChevronDown, Check } from 'lucide-solid'
import { LANGUAGES, selectedLanguage, setSelectedLanguage } from '~/lib/language-store'

/**
 * A dropdown component for switching the preferred documentation language.
 * Persists selection to localStorage.
 */
export const LanguageSwitcher: Component = () => {
  const [open, setOpen] = createSignal(false)

  return (
    <div class="relative">
      <button
        type="button"
        class="flex items-center gap-1.5 px-3 py-1.5 text-xs font-mono rounded-lg text-slate-400 hover:text-[#00d4ff] bg-[#0a1628] border border-[#1a2a42] hover:border-[#00d4ff]/30 transition-colors duration-150"
        onClick={() => setOpen(!open())}
        aria-expanded={open()}
        aria-haspopup="listbox"
      >
        {selectedLanguage().short}
        <ChevronDown
          size={12}
          class={`transition-transform duration-150 ${open() ? 'rotate-180' : ''}`}
        />
      </button>

      <Show when={open()}>
        {/* Backdrop to close on click outside */}
        <div
          class="fixed inset-0 z-40"
          onClick={() => setOpen(false)}
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
                  setOpen(false)
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
  )
}

export default LanguageSwitcher
