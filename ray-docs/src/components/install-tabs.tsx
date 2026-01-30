import type { Component } from 'solid-js'
import { createSignal, For, Show } from 'solid-js'
import { Copy, Check } from 'lucide-solid'
import { LANGUAGES, selectedLanguage, setSelectedLanguage } from '~/lib/language-store'

interface InstallCommand {
  id: string
  label: string
  command: string
  secondary?: string
}

const INSTALL_COMMANDS: InstallCommand[] = [
  {
    id: 'typescript',
    label: 'TypeScript',
    command: 'bun add @ray-db/core',
    secondary: 'npm install @ray-db/core',
  },
  {
    id: 'rust',
    label: 'Rust',
    command: 'cargo add raydb',
  },
  {
    id: 'python',
    label: 'Python',
    command: 'pip install raydb',
    secondary: 'uv add raydb',
  },
]

/**
 * Tabbed install command component with copy functionality.
 * Shows TypeScript/Rust/Python install commands with copy buttons.
 * Synced with the global language preference.
 */
export const InstallTabs: Component = () => {
  const [copied, setCopied] = createSignal(false)

  const activeCommand = () =>
    INSTALL_COMMANDS.find((c) => c.id === selectedLanguage().id) ?? INSTALL_COMMANDS[0]

  const copyCommand = async () => {
    const cmd = activeCommand()
    await navigator.clipboard.writeText(cmd.command)
    setCopied(true)
    setTimeout(() => setCopied(false), 2000)
  }

  const handleTabClick = (cmdId: string) => {
    const lang = LANGUAGES.find((l) => l.id === cmdId)
    if (lang) {
      setSelectedLanguage(lang)
    }
  }

  return (
    <div class="bg-[#0a1628] rounded-lg border border-[#1a2a42]">
      {/* Tabs */}
      <div class="flex items-center border-b border-[#1a2a42]">
        <For each={INSTALL_COMMANDS}>
          {(cmd) => (
            <button
              type="button"
              class={`px-4 py-2 text-xs font-mono transition-colors ${
                selectedLanguage().id === cmd.id
                  ? 'text-[#00d4ff] border-b-2 border-[#00d4ff] -mb-px bg-[#00d4ff]/5'
                  : 'text-slate-500 hover:text-white'
              }`}
              onClick={() => handleTabClick(cmd.id)}
            >
              {cmd.label}
            </button>
          )}
        </For>
      </div>

      {/* Command display */}
      <div class="p-4">
        <div class="flex items-center gap-3 flex-wrap">
          <span class="text-[#00d4ff]">$</span>
          <code class="text-white font-mono">{activeCommand().command}</code>
          <button
            type="button"
            class="ml-auto px-3 py-1 text-xs rounded bg-[#1a2a42] text-slate-400 hover:text-[#00d4ff] hover:bg-[#1a2a42]/80 transition-colors flex items-center gap-1.5"
            aria-label="Copy install command"
            onClick={copyCommand}
          >
            {copied() ? (
              <>
                <Check size={12} class="text-[#28c840]" />
                <span class="text-[#28c840]">copied</span>
              </>
            ) : (
              <>
                <Copy size={12} />
                <span>copy</span>
              </>
            )}
          </button>
        </div>

        {/* Secondary command (alternative package manager) */}
        <Show when={activeCommand().secondary}>
          <div class="mt-2 pt-2 border-t border-[#1a2a42]/50">
            <div class="flex items-center gap-3 text-slate-500 text-sm">
              <span class="text-slate-600">$</span>
              <code class="font-mono">{activeCommand().secondary}</code>
            </div>
          </div>
        </Show>
      </div>
    </div>
  )
}

export default InstallTabs
