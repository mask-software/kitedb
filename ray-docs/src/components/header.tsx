import { Link } from '@tanstack/solid-router'
import { createSignal } from 'solid-js'
import { Menu, X } from 'lucide-solid'
import Logo from './logo'

export default function Header() {
  const [isOpen, setIsOpen] = createSignal(false)

  return (
    <>
      <header class="sticky top-0 z-50 h-14 flex items-center justify-between px-4 border-b border-[#1a2a42] bg-[#030712]/90 backdrop-blur-xl">
        <div class="flex items-center gap-4">
          <button
            type="button"
            onClick={() => setIsOpen(true)}
            class="lg:hidden p-2 text-slate-500 hover:text-[#00d4ff] hover:bg-[#1a2a42]/50 rounded-lg transition-colors duration-150"
            aria-label="Open navigation menu"
          >
            <Menu size={18} aria-hidden="true" />
          </button>
          <Link to="/" class="flex items-center gap-2 group" aria-label="RayDB Home">
            <div class="flex items-center gap-2 px-2 py-1 rounded bg-[#0a1628] border border-[#1a2a42] group-hover:border-[#00d4ff]/50 transition-colors">
              <span class="text-[#00d4ff] font-mono text-xs">❯</span>
              <Logo size={18} />
              <span class="font-mono font-bold text-white text-sm">raydb</span>
            </div>
          </Link>
        </div>

        <div class="flex items-center gap-2">
          <a
            href="https://github.com/maskdotdev/ray"
            target="_blank"
            rel="noopener noreferrer"
            class="flex items-center gap-2 px-3 py-1.5 rounded-lg font-mono text-sm text-slate-500 hover:text-[#00d4ff] bg-[#0a1628] border border-[#1a2a42] hover:border-[#00d4ff]/50 transition-colors duration-150"
            aria-label="View RayDB on GitHub"
          >
            <svg class="w-4 h-4" fill="currentColor" viewBox="0 0 24 24" aria-hidden="true">
              <path fill-rule="evenodd" d="M12 2C6.477 2 2 6.484 2 12.017c0 4.425 2.865 8.18 6.839 9.504.5.092.682-.217.682-.483 0-.237-.008-.868-.013-1.703-2.782.605-3.369-1.343-3.369-1.343-.454-1.158-1.11-1.466-1.11-1.466-.908-.62.069-.608.069-.608 1.003.07 1.531 1.032 1.531 1.032.892 1.53 2.341 1.088 2.91.832.092-.647.35-1.088.636-1.338-2.22-.253-4.555-1.113-4.555-4.951 0-1.093.39-1.988 1.029-2.688-.103-.253-.446-1.272.098-2.65 0 0 .84-.27 2.75 1.026A9.564 9.564 0 0112 6.844c.85.004 1.705.115 2.504.337 1.909-1.296 2.747-1.027 2.747-1.027.546 1.379.202 2.398.1 2.651.64.7 1.028 1.595 1.028 2.688 0 3.848-2.339 4.695-4.566 4.943.359.309.678.92.678 1.855 0 1.338-.012 2.419-.012 2.747 0 .268.18.58.688.482A10.019 10.019 0 0022 12.017C22 6.484 17.522 2 12 2z" clip-rule="evenodd" />
            </svg>
            <span class="hidden sm:inline">clone</span>
          </a>
        </div>

        {/* Electric border */}
        <div class="absolute bottom-0 left-0 right-0 h-px bg-gradient-to-r from-transparent via-[#00d4ff]/30 to-transparent" aria-hidden="true" />
      </header>

      {/* Mobile sidebar backdrop */}
      {isOpen() && (
        <div
          class="fixed inset-0 bg-black/60 backdrop-blur-sm z-40 lg:hidden"
          onClick={() => setIsOpen(false)}
          aria-hidden="true"
        />
      )}

      {/* Mobile sidebar */}
      <aside
        class={`fixed top-0 left-0 h-full w-72 bg-[#030712]/95 backdrop-blur-xl border-r border-[#1a2a42] shadow-2xl z-50 transform transition-transform duration-300 ease-out flex flex-col ${isOpen() ? 'translate-x-0' : '-translate-x-full'
          }`}
        role="navigation"
        aria-label="Mobile navigation"
      >
        <div class="flex items-center justify-between h-14 px-4 border-b border-[#1a2a42]">
          <div class="flex items-center gap-2">
            <span class="text-[#00d4ff] font-mono text-sm">❯</span>
            <span class="font-mono font-bold text-white">raydb</span>
          </div>
          <button
            type="button"
            onClick={() => setIsOpen(false)}
            class="p-2 text-slate-500 hover:text-[#00d4ff] hover:bg-[#1a2a42]/50 rounded-lg transition-colors duration-150"
            aria-label="Close navigation menu"
          >
            <X size={18} aria-hidden="true" />
          </button>
        </div>

        <nav class="flex-1 p-4 overflow-y-auto font-mono">
          <Link
            to="/"
            onClick={() => setIsOpen(false)}
            class="flex items-center gap-3 px-3 py-2 rounded-lg text-slate-400 hover:text-[#00d4ff] hover:bg-[#1a2a42]/50 transition-colors duration-150"
          >
            <span class="text-[#00d4ff]">→</span>
            <span>./home</span>
          </Link>
          <Link
            to="/docs"
            onClick={() => setIsOpen(false)}
            class="flex items-center gap-3 px-3 py-2 rounded-lg text-slate-400 hover:text-[#00d4ff] hover:bg-[#1a2a42]/50 transition-colors duration-150"
          >
            <span class="text-[#00d4ff]">→</span>
            <span>./docs</span>
          </Link>
        </nav>
      </aside>
    </>
  )
}
