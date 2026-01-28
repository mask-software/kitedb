import { createFileRoute, Outlet, Link, useLocation } from '@tanstack/solid-router'
import { createSignal, For, Show } from 'solid-js'
import { ChevronDown, ChevronRight, Menu, X, Search } from 'lucide-solid'
import Logo from '~/components/Logo'
import ThemeToggle from '~/components/ThemeToggle'
import { docsStructure } from '~/lib/docs'

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

  return (
    <div class="min-h-screen bg-background">
      {/* Skip link */}
      <a
        href="#doc-content"
        class="sr-only focus:not-sr-only focus:absolute focus:top-4 focus:left-4 focus:z-[100] focus:px-4 focus:py-2 focus:bg-cyan-500 focus:text-white focus:rounded-lg"
      >
        Skip to content
      </a>

      {/* Mobile sidebar backdrop */}
      <Show when={sidebarOpen()}>
        <div
          class="fixed inset-0 bg-slate-950/50 backdrop-blur-sm z-40 lg:hidden"
          onClick={() => setSidebarOpen(false)}
          aria-hidden="true"
        />
      </Show>

      {/* Sidebar */}
      <aside
        class={`fixed top-0 left-0 z-50 h-full w-72 bg-white dark:bg-slate-950 border-r border-slate-200 dark:border-slate-800 transform transition-transform duration-300 ease-out lg:translate-x-0 ${
          sidebarOpen() ? 'translate-x-0' : '-translate-x-full'
        }`}
        role="navigation"
        aria-label="Documentation sidebar"
      >
        <div class="flex flex-col h-full">
          {/* Sidebar header */}
          <div class="flex items-center justify-between h-16 px-4 border-b border-slate-200 dark:border-slate-800">
            <Link
              to="/"
              class="flex items-center gap-2.5"
              onClick={() => setSidebarOpen(false)}
              aria-label="Go to homepage"
            >
              <Logo size={28} />
              <span class="text-lg font-bold text-gradient">RayDB</span>
            </Link>
            <button
              type="button"
              class="lg:hidden p-2 rounded-lg text-slate-500 hover:bg-slate-100 dark:hover:bg-slate-800 transition-colors duration-150"
              onClick={() => setSidebarOpen(false)}
              aria-label="Close sidebar"
            >
              <X size={20} aria-hidden="true" />
            </button>
          </div>

          {/* Navigation */}
          <nav class="flex-1 overflow-y-auto p-4 scrollbar-thin">
            <For each={docsStructure}>
              {(section) => (
                <div class="mb-6">
                  <button
                    type="button"
                    class="flex items-center justify-between w-full px-2 py-1.5 text-xs font-semibold uppercase tracking-wider text-slate-500 dark:text-slate-400 hover:text-slate-900 dark:hover:text-white transition-colors duration-150"
                    onClick={() => toggleSection(section.label)}
                    aria-expanded={expandedSections()[section.label]}
                  >
                    {section.label}
                    <Show
                      when={expandedSections()[section.label]}
                      fallback={<ChevronRight size={14} aria-hidden="true" />}
                    >
                      <ChevronDown size={14} aria-hidden="true" />
                    </Show>
                  </button>

                  <Show when={expandedSections()[section.label]}>
                    <ul class="mt-2 space-y-0.5" role="list">
                      <For each={section.items}>
                        {(item) => (
                          <li>
                            <a
                              href={`/docs/${item.slug}`}
                              onClick={() => setSidebarOpen(false)}
                              class={`block px-3 py-2 text-sm rounded-lg transition-all duration-150 ${
                                isActive(item.slug)
                                  ? 'bg-gradient-to-r from-cyan-500/10 to-violet-500/10 text-cyan-600 dark:text-cyan-400 font-medium border-l-2 border-cyan-500 ml-0.5'
                                  : 'text-slate-600 dark:text-slate-400 hover:bg-slate-100 dark:hover:bg-slate-800 hover:text-slate-900 dark:hover:text-white'
                              }`}
                              aria-current={isActive(item.slug) ? 'page' : undefined}
                            >
                              {item.title}
                            </a>
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
          <div class="p-4 border-t border-slate-200 dark:border-slate-800">
            <a
              href="https://github.com/maskdotdev/ray"
              target="_blank"
              rel="noopener noreferrer"
              class="flex items-center gap-2 px-3 py-2 text-sm text-slate-600 dark:text-slate-400 hover:text-slate-900 dark:hover:text-white rounded-lg hover:bg-slate-100 dark:hover:bg-slate-800 transition-colors duration-150"
            >
              <svg class="w-5 h-5" fill="currentColor" viewBox="0 0 24 24" aria-hidden="true">
                <path
                  fill-rule="evenodd"
                  d="M12 2C6.477 2 2 6.484 2 12.017c0 4.425 2.865 8.18 6.839 9.504.5.092.682-.217.682-.483 0-.237-.008-.868-.013-1.703-2.782.605-3.369-1.343-3.369-1.343-.454-1.158-1.11-1.466-1.11-1.466-.908-.62.069-.608.069-.608 1.003.07 1.531 1.032 1.531 1.032.892 1.53 2.341 1.088 2.91.832.092-.647.35-1.088.636-1.338-2.22-.253-4.555-1.113-4.555-4.951 0-1.093.39-1.988 1.029-2.688-.103-.253-.446-1.272.098-2.65 0 0 .84-.27 2.75 1.026A9.564 9.564 0 0112 6.844c.85.004 1.705.115 2.504.337 1.909-1.296 2.747-1.027 2.747-1.027.546 1.379.202 2.398.1 2.651.64.7 1.028 1.595 1.028 2.688 0 3.848-2.339 4.695-4.566 4.943.359.309.678.92.678 1.855 0 1.338-.012 2.419-.012 2.747 0 .268.18.58.688.482A10.019 10.019 0 0022 12.017C22 6.484 17.522 2 12 2z"
                  clip-rule="evenodd"
                />
              </svg>
              View on GitHub
            </a>
          </div>
        </div>
      </aside>

      {/* Main content area */}
      <div class="lg:pl-72">
        {/* Top header */}
        <header class="sticky top-0 z-30 flex items-center justify-between h-16 px-4 bg-white/80 dark:bg-slate-950/80 backdrop-blur-xl border-b border-slate-200 dark:border-slate-800">
          <div class="flex items-center gap-4">
            <button
              type="button"
              class="lg:hidden p-2 rounded-lg text-slate-500 hover:bg-slate-100 dark:hover:bg-slate-800 transition-colors duration-150"
              onClick={() => setSidebarOpen(true)}
              aria-label="Open navigation menu"
            >
              <Menu size={20} aria-hidden="true" />
            </button>
            
            {/* Breadcrumb or search placeholder */}
            <button
              type="button"
              class="hidden sm:flex items-center gap-2 px-3 py-1.5 text-sm text-slate-500 bg-slate-100 dark:bg-slate-800 rounded-lg hover:bg-slate-200 dark:hover:bg-slate-700 transition-colors duration-150"
              aria-label="Search documentation"
            >
              <Search size={16} aria-hidden="true" />
              <span>Search docs…</span>
              <kbd class="hidden md:inline-flex items-center gap-0.5 px-1.5 py-0.5 text-xs font-mono bg-slate-200 dark:bg-slate-700 rounded">
                <span>⌘</span>K
              </kbd>
            </button>
          </div>

          <div class="flex items-center gap-1">
            <ThemeToggle />
            <a
              href="https://github.com/maskdotdev/ray"
              target="_blank"
              rel="noopener noreferrer"
              class="p-2.5 rounded-lg text-slate-500 hover:bg-slate-100 dark:hover:bg-slate-800 hover:text-slate-900 dark:hover:text-white transition-colors duration-150"
              aria-label="View on GitHub"
            >
              <svg class="w-5 h-5" fill="currentColor" viewBox="0 0 24 24" aria-hidden="true">
                <path
                  fill-rule="evenodd"
                  d="M12 2C6.477 2 2 6.484 2 12.017c0 4.425 2.865 8.18 6.839 9.504.5.092.682-.217.682-.483 0-.237-.008-.868-.013-1.703-2.782.605-3.369-1.343-3.369-1.343-.454-1.158-1.11-1.466-1.11-1.466-.908-.62.069-.608.069-.608 1.003.07 1.531 1.032 1.531 1.032.892 1.53 2.341 1.088 2.91.832.092-.647.35-1.088.636-1.338-2.22-.253-4.555-1.113-4.555-4.951 0-1.093.39-1.988 1.029-2.688-.103-.253-.446-1.272.098-2.65 0 0 .84-.27 2.75 1.026A9.564 9.564 0 0112 6.844c.85.004 1.705.115 2.504.337 1.909-1.296 2.747-1.027 2.747-1.027.546 1.379.202 2.398.1 2.651.64.7 1.028 1.595 1.028 2.688 0 3.848-2.339 4.695-4.566 4.943.359.309.678.92.678 1.855 0 1.338-.012 2.419-.012 2.747 0 .268.18.58.688.482A10.019 10.019 0 0022 12.017C22 6.484 17.522 2 12 2z"
                  clip-rule="evenodd"
                />
              </svg>
            </a>
          </div>
        </header>

        {/* Page content */}
        <main id="doc-content" class="min-h-[calc(100vh-4rem)]">
          <Outlet />
        </main>
      </div>
    </div>
  )
}
