// @ts-nocheck
import { createFileRoute, Outlet, Link, useLocation } from '@tanstack/solid-router'
import { createSignal, For, Show } from 'solid-js'
import { ChevronDown, ChevronRight, Menu, X, Search, Terminal, Zap } from 'lucide-solid'
import Logo from '~/components/logo'
import { LanguageSwitcher } from '~/components/language-switcher'
import { docsStructure } from '~/lib/docs'
import { cn } from '~/lib/utils'
import { searchDialog } from '~/components/search-dialog'

export const Route = createFileRoute('/docs')({
  component: DocsLayout,
})

function DocsLayout() {
  const location = useLocation()
  const [sidebarOpen, setSidebarOpen] = createSignal(false)
  const [expandedSections, setExpandedSections] = createSignal<Record<string, boolean>>(
    Object.fromEntries(docsStructure.map((s) => [s.label, !s.collapsed]))
  )

  const toggleSection = (label: string) => {
    setExpandedSections((prev) => ({ ...prev, [label]: !prev[label] }))
  }

  const isActive = (slug: string) => {
    const currentPath = location().pathname.replace(/^\/docs\/?/, '').replace(/\/$/, '')
    return currentPath === slug
  }

  const UnsafeLink: any = Link

  return (
    <div class="min-h-screen bg-[#030712] circuit-pattern">
      {/* Skip link */}
      <a
        href="#doc-content"
        class="sr-only focus:not-sr-only focus:absolute focus:top-4 focus:left-4 focus:z-[100] focus:px-4 focus:py-2 focus:bg-[#00d4ff] focus:text-black focus:rounded-lg focus:font-mono focus:font-semibold"
      >
        Skip to content
      </a>

      {/* Background effects */}
      <div class="fixed inset-0 pointer-events-none z-0" aria-hidden="true">
        <div class="absolute top-0 left-1/4 w-[400px] h-[400px] bg-[#00d4ff]/3 rounded-full blur-[100px]" />
        <div class="absolute bottom-0 right-1/4 w-[300px] h-[300px] bg-[#7c3aed]/3 rounded-full blur-[80px]" />
        <div class="console-scanlines opacity-10" />
      </div>

      {/* Mobile sidebar backdrop */}
      <Show when={sidebarOpen()}>
        <div
          class="fixed inset-0 bg-black/60 backdrop-blur-sm z-40 lg:hidden"
          onClick={() => setSidebarOpen(false)}
          aria-hidden="true"
        />
      </Show>

      {/* Sidebar */}
      <aside
        class={`fixed top-0 left-0 z-50 h-full w-72 bg-[#030712]/95 backdrop-blur-xl border-r border-[#1a2a42] transform transition-transform duration-300 ease-out lg:translate-x-0 ${sidebarOpen() ? 'translate-x-0' : '-translate-x-full'
          }`}
        role="navigation"
        aria-label="Documentation sidebar"
      >
        <div class="flex flex-col h-full">
          {/* Sidebar header - console style */}
          <div class="flex items-center justify-between h-14 px-4 border-b border-[#1a2a42] bg-[#0a1628]/50">
            <Link
              to="/"
              class="flex items-center gap-2 group"
              onClick={() => setSidebarOpen(false)}
              aria-label="Go to homepage"
            >
              <div class="flex items-center gap-2 px-2 py-1 rounded bg-[#0a1628] border border-[#1a2a42] group-hover:border-[#00d4ff]/50 transition-colors">
                <span class="text-[#00d4ff] font-mono text-xs">❯</span>
                <Logo size={18} />
                <span class="font-mono font-bold text-white text-sm">raydb</span>
              </div>
            </Link>
            <button
              type="button"
              class="lg:hidden p-2 rounded-lg text-slate-500 hover:text-[#00d4ff] hover:bg-[#1a2a42]/50 transition-colors duration-150"
              onClick={() => setSidebarOpen(false)}
              aria-label="Close sidebar"
            >
              <X size={18} aria-hidden="true" />
            </button>
          </div>

          {/* Navigation */}
          <nav class="flex-1 overflow-y-auto p-4 scrollbar-thin">
            <For each={docsStructure}>
              {(section) => (
                <div class="mb-6">
                  <button
                    type="button"
                    class="flex items-center justify-between w-full px-2 py-1.5 text-xs font-mono font-semibold uppercase tracking-wider text-slate-500 hover:text-[#00d4ff] transition-colors duration-150"
                    onClick={() => toggleSection(section.label)}
                    aria-expanded={expandedSections()[section.label]}
                  >
                    <span class="flex items-center gap-2">
                      <span class="text-[#00d4ff]/50">$</span>
                      {section.label.replace(/\s+/g, '_')}
                    </span>
                    <Show
                      when={expandedSections()[section.label]}
                      fallback={<ChevronRight size={12} aria-hidden="true" />}
                    >
                      <ChevronDown size={12} aria-hidden="true" />
                    </Show>
                  </button>

                  <Show when={expandedSections()[section.label]}>
                    <ul class="mt-2 space-y-0.5" role="list">
                      <For each={section.items}>
                        {(item) => (
                          <li>
                            <UnsafeLink
                              to={`/docs/${item.slug}`}
                              onClick={() => setSidebarOpen(false)}
                              class={cn(
                                'group block px-3 py-2 text-sm font-mono transition-all duration-150',
                                isActive(item.slug)
                                  ? 'bg-[#00d4ff]/10 text-[#00d4ff] border-l-2 border-[#00d4ff] ml-0.5 rounded-r-lg'
                                  : 'text-slate-400 hover:bg-[#1a2a42]/50 hover:text-white rounded-lg'
                              )}
                              aria-current={isActive(item.slug) ? 'page' : undefined}
                            >
                              <span class="flex items-center gap-2">
                                <span class={cn(
                                  'text-xs',
                                  isActive(item.slug)
                                    ? 'text-[#00d4ff]'
                                    : 'text-slate-600 group-hover:text-[#00d4ff]'
                                )}>
                                  →
                                </span>
                                {item.title}
                              </span>
                            </UnsafeLink>
                          </li>
                        )}
                      </For>
                    </ul>
                  </Show>
                </div>
              )}
            </For>
          </nav>

          {/* Sidebar footer */}
          <div class="p-4 border-t border-[#1a2a42]">
            <a
              href="https://github.com/maskdotdev/ray"
              target="_blank"
              rel="noopener noreferrer"
              class="flex items-center gap-2 px-3 py-2 text-sm font-mono text-slate-500 hover:text-[#00d4ff] rounded-lg hover:bg-[#1a2a42]/50 transition-colors duration-150"
            >
              <svg class="w-4 h-4" fill="currentColor" viewBox="0 0 24 24" aria-hidden="true">
                <path
                  fill-rule="evenodd"
                  d="M12 2C6.477 2 2 6.484 2 12.017c0 4.425 2.865 8.18 6.839 9.504.5.092.682-.217.682-.483 0-.237-.008-.868-.013-1.703-2.782.605-3.369-1.343-3.369-1.343-.454-1.158-1.11-1.466-1.11-1.466-.908-.62.069-.608.069-.608 1.003.07 1.531 1.032 1.531 1.032.892 1.53 2.341 1.088 2.91.832.092-.647.35-1.088.636-1.338-2.22-.253-4.555-1.113-4.555-4.951 0-1.093.39-1.988 1.029-2.688-.103-.253-.446-1.272.098-2.65 0 0 .84-.27 2.75 1.026A9.564 9.564 0 0112 6.844c.85.004 1.705.115 2.504.337 1.909-1.296 2.747-1.027 2.747-1.027.546 1.379.202 2.398.1 2.651.64.7 1.028 1.595 1.028 2.688 0 3.848-2.339 4.695-4.566 4.943.359.309.678.92.678 1.855 0 1.338-.012 2.419-.012 2.747 0 .268.18.58.688.482A10.019 10.019 0 0022 12.017C22 6.484 17.522 2 12 2z"
                  clip-rule="evenodd"
                />
              </svg>
              git clone
            </a>
          </div>
        </div>
      </aside>

      {/* Main content area */}
      <div class="lg:pl-72 relative z-10">
        {/* Top header - console style */}
        <header class="sticky top-0 z-30 flex items-center justify-between h-14 px-4 border-b border-[#1a2a42] bg-[#030712]/90 backdrop-blur-xl">
          <div class="flex items-center gap-4">
            <button
              type="button"
              class="lg:hidden p-2 rounded-lg text-slate-500 hover:text-[#00d4ff] hover:bg-[#1a2a42]/50 transition-colors duration-150"
              onClick={() => setSidebarOpen(true)}
              aria-label="Open navigation menu"
            >
              <Menu size={18} aria-hidden="true" />
            </button>

            {/* Search - console style */}
            <button
              type="button"
              onClick={() => searchDialog.open()}
              class="hidden sm:flex items-center gap-2 px-3 py-1.5 text-sm font-mono text-slate-500 bg-[#0a1628] border border-[#1a2a42] rounded-lg hover:border-[#00d4ff]/50 hover:text-[#00d4ff] transition-colors duration-150"
              aria-label="Search documentation"
            >
              <Search size={14} aria-hidden="true" />
              <span>./search</span>
              <kbd class="hidden md:inline-flex items-center gap-0.5 px-1.5 py-0.5 text-xs font-mono bg-[#1a2a42] rounded">
                ⌘K
              </kbd>
            </button>
          </div>

          <div class="flex items-center gap-2">
            <LanguageSwitcher />

            <a
              href="https://github.com/maskdotdev/ray"
              target="_blank"
              rel="noopener noreferrer"
              class="flex items-center gap-2 px-3 py-1.5 rounded-lg font-mono text-sm text-slate-500 hover:text-[#00d4ff] bg-[#0a1628] border border-[#1a2a42] hover:border-[#00d4ff]/50 transition-colors duration-150"
              aria-label="View on GitHub"
            >
              <svg class="w-4 h-4" fill="currentColor" viewBox="0 0 24 24" aria-hidden="true">
                <path
                  fill-rule="evenodd"
                  d="M12 2C6.477 2 2 6.484 2 12.017c0 4.425 2.865 8.18 6.839 9.504.5.092.682-.217.682-.483 0-.237-.008-.868-.013-1.703-2.782.605-3.369-1.343-3.369-1.343-.454-1.158-1.11-1.466-1.11-1.466-.908-.62.069-.608.069-.608 1.003.07 1.531 1.032 1.531 1.032.892 1.53 2.341 1.088 2.91.832.092-.647.35-1.088.636-1.338-2.22-.253-4.555-1.113-4.555-4.951 0-1.093.39-1.988 1.029-2.688-.103-.253-.446-1.272.098-2.65 0 0 .84-.27 2.75 1.026A9.564 9.564 0 0112 6.844c.85.004 1.705.115 2.504.337 1.909-1.296 2.747-1.027 2.747-1.027.546 1.379.202 2.398.1 2.651.64.7 1.028 1.595 1.028 2.688 0 3.848-2.339 4.695-4.566 4.943.359.309.678.92.678 1.855 0 1.338-.012 2.419-.012 2.747 0 .268.18.58.688.482A10.019 10.019 0 0022 12.017C22 6.484 17.522 2 12 2z"
                  clip-rule="evenodd"
                />
              </svg>
              <span class="hidden sm:inline">clone</span>
            </a>
          </div>

          {/* Electric border */}
          <div class="absolute bottom-0 left-0 right-0 h-px bg-gradient-to-r from-transparent via-[#00d4ff]/30 to-transparent" aria-hidden="true" />
        </header>

        {/* Page content */}
        <main id="doc-content" class="min-h-[calc(100vh-3.5rem)]">
          <Outlet />
        </main>
      </div>
    </div>
  )
}
