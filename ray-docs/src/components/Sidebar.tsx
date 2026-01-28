import type { Component } from 'solid-js'
import { For, Show, createSignal } from 'solid-js'
import { Link, useLocation } from '@tanstack/solid-router'
import { ChevronDown, ChevronRight, X } from 'lucide-solid'
import { docsStructure } from '~/lib/docs'
import Logo from './Logo'

interface SidebarProps {
  isOpen: boolean
  onClose: () => void
}

export const Sidebar: Component<SidebarProps> = (props) => {
  const location = useLocation()
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
    <>
      {/* Mobile backdrop */}
      <Show when={props.isOpen}>
        <div
          class="fixed inset-0 bg-black/50 backdrop-blur-sm z-40 lg:hidden"
          onClick={props.onClose}
          aria-hidden="true"
        />
      </Show>

      {/* Sidebar */}
      <aside
        class={`fixed top-0 left-0 z-50 h-full w-72 bg-white dark:bg-slate-900 border-r border-slate-200 dark:border-slate-800 transform transition-transform duration-300 ease-out lg:translate-x-0 lg:static lg:z-0 ${
          props.isOpen ? 'translate-x-0' : '-translate-x-full'
        }`}
        role="navigation"
        aria-label="Documentation navigation"
      >
        <div class="flex flex-col h-full">
          {/* Header */}
          <div class="flex items-center justify-between p-4 border-b border-slate-200 dark:border-slate-800">
            <Link
              to="/"
              class="flex items-center gap-2 font-bold text-xl text-slate-900 dark:text-white hover:text-cyan-600 dark:hover:text-cyan-400 transition-colors"
              onClick={props.onClose}
            >
              <Logo size={28} />
              <span class="bg-gradient-to-r from-cyan-500 to-violet-500 bg-clip-text text-transparent">
                RayDB
              </span>
            </Link>
            <button
              type="button"
              class="lg:hidden p-2 rounded-lg text-slate-600 dark:text-slate-400 hover:bg-slate-100 dark:hover:bg-slate-800 transition-colors"
              onClick={props.onClose}
              aria-label="Close sidebar"
            >
              <X size={20} />
            </button>
          </div>

          {/* Navigation */}
          <nav class="flex-1 overflow-y-auto p-4 scrollbar-thin scrollbar-thumb-slate-300 dark:scrollbar-thumb-slate-700">
            <For each={docsStructure}>
              {(section) => (
                <div class="mb-4">
                  <button
                    type="button"
                    class="flex items-center justify-between w-full px-2 py-1.5 text-sm font-semibold text-slate-900 dark:text-white hover:text-cyan-600 dark:hover:text-cyan-400 transition-colors"
                    onClick={() => toggleSection(section.label)}
                    aria-expanded={expandedSections()[section.label]}
                  >
                    {section.label}
                    <Show
                      when={expandedSections()[section.label]}
                      fallback={<ChevronRight size={16} class="text-slate-400" />}
                    >
                      <ChevronDown size={16} class="text-slate-400" />
                    </Show>
                  </button>

                  <Show when={expandedSections()[section.label]}>
                    <ul class="mt-1 space-y-0.5" role="list">
                      <For each={section.items}>
                        {(item) => (
                          <li>
                            <a
                              href={`/docs/${item.slug}`}
                              onClick={props.onClose}
                              class={`block px-3 py-2 text-sm rounded-lg transition-all ${
                                isActive(item.slug)
                                  ? 'bg-gradient-to-r from-cyan-500/10 to-violet-500/10 text-cyan-600 dark:text-cyan-400 font-medium border-l-2 border-cyan-500'
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

          {/* Footer */}
          <div class="p-4 border-t border-slate-200 dark:border-slate-800">
            <a
              href="https://github.com/maskdotdev/ray"
              target="_blank"
              rel="noopener noreferrer"
              class="flex items-center gap-2 px-3 py-2 text-sm text-slate-600 dark:text-slate-400 hover:text-slate-900 dark:hover:text-white rounded-lg hover:bg-slate-100 dark:hover:bg-slate-800 transition-colors"
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
    </>
  )
}

export default Sidebar
