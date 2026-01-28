"use client";

import { useMemo, useState } from "react";
import Link from "next/link";
import { usePathname } from "next/navigation";
import { ChevronDown, ChevronRight, Menu, Search, X } from "lucide-react";
import Logo from "./Logo";
import { docsStructure } from "../lib/docs";

interface DocsShellProps {
  children: React.ReactNode;
}

export default function DocsShell({ children }: DocsShellProps) {
  const pathname = usePathname();
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const [expandedSections, setExpandedSections] = useState<Record<string, boolean>>(() =>
    Object.fromEntries(docsStructure.map((section) => [section.label, !section.collapsed]))
  );

  const currentSlug = useMemo(() => pathname.replace(/^\/docs\/?/, "").replace(/\/$/, ""), [pathname]);
  const isActive = (slug: string) => currentSlug === slug;

  const toggleSection = (label: string) => {
    setExpandedSections((prev) => ({ ...prev, [label]: !prev[label] }));
  };

  return (
    <div className="h-screen w-full bg-background speed-page overflow-hidden">

      {sidebarOpen && (
        <div
          className="fixed inset-0 bg-slate-950/50 backdrop-blur-sm z-40 lg:hidden"
          onClick={() => setSidebarOpen(false)}
          aria-hidden="true"
        />
      )}

      <div className="flex h-full w-full">
        <aside
          className={`fixed top-0 left-0 z-50 h-screen w-72 border-r border-slate-200 dark:border-slate-800 speed-glass transform transition-transform duration-300 ease-out lg:static lg:translate-x-0 lg:flex lg:flex-col lg:h-full lg:shrink-0 ${
            sidebarOpen ? "translate-x-0" : "-translate-x-full"
          }`}
          role="navigation"
          aria-label="Documentation sidebar"
        >
          <div className="flex flex-col h-full">
          <div className="flex items-center justify-between h-16 px-4 border-b border-slate-200 dark:border-slate-800">
            <Link
              href="/"
              className="flex items-center gap-2.5"
              onClick={() => setSidebarOpen(false)}
              aria-label="Go to homepage"
            >
              <Logo size={28} />
              <span className="text-lg font-bold text-gradient">RayDB</span>
            </Link>
            <button
              type="button"
              className="lg:hidden p-2 rounded-lg text-slate-500 hover:bg-slate-100 dark:hover:bg-slate-800 transition-colors duration-150"
              onClick={() => setSidebarOpen(false)}
              aria-label="Close sidebar"
            >
              <X size={20} aria-hidden="true" />
            </button>
          </div>

          <nav className="flex-1 overflow-y-auto p-4 scrollbar-thin">
            {docsStructure.map((section) => (
              <div key={section.label} className="mb-6">
                <button
                  type="button"
                  className="flex items-center justify-between w-full px-2 py-1.5 text-xs font-semibold uppercase tracking-wider text-slate-500 dark:text-slate-400 hover:text-slate-900 dark:hover:text-white transition-colors duration-150"
                  onClick={() => toggleSection(section.label)}
                  aria-expanded={expandedSections[section.label]}
                >
                  {section.label}
                  {expandedSections[section.label] ? (
                    <ChevronDown size={14} aria-hidden="true" />
                  ) : (
                    <ChevronRight size={14} aria-hidden="true" />
                  )}
                </button>

                {expandedSections[section.label] && (
                  <ul className="mt-2 space-y-0.5" role="list">
                    {section.items.map((item) => (
                      <li key={item.slug}>
                        <Link
                          href={`/docs/${item.slug}`}
                          onClick={() => setSidebarOpen(false)}
                          className={`block px-3 py-2 text-sm rounded-lg transition-all duration-150 ${isActive(item.slug)
                            ? "bg-gradient-to-r from-cyan-500/10 to-violet-500/10 text-cyan-600 dark:text-cyan-400 font-medium border-l-2 border-cyan-500 ml-0.5"
                            : "text-slate-600 dark:text-slate-400 hover:bg-slate-100 dark:hover:bg-slate-800 hover:text-slate-900 dark:hover:text-white"
                            }`}
                          aria-current={isActive(item.slug) ? "page" : undefined}
                        >
                          {item.title}
                        </Link>
                      </li>
                    ))}
                  </ul>
                )}
              </div>
            ))}
          </nav>

          <div className="p-4 border-t border-slate-200 dark:border-slate-800">
            <a
              href="https://github.com/maskdotdev/ray"
              target="_blank"
              rel="noopener noreferrer"
              className="flex items-center gap-2 px-3 py-2 text-sm text-slate-600 dark:text-slate-400 hover:text-slate-900 dark:hover:text-white rounded-lg hover:bg-slate-100 dark:hover:bg-slate-800 transition-colors duration-150"
            >
              <svg className="w-5 h-5" fill="currentColor" viewBox="0 0 24 24" aria-hidden="true">
                <path
                  fillRule="evenodd"
                  d="M12 2C6.477 2 2 6.484 2 12.017c0 4.425 2.865 8.18 6.839 9.504.5.092.682-.217.682-.483 0-.237-.008-.868-.013-1.703-2.782.605-3.369-1.343-3.369-1.343-.454-1.158-1.11-1.466-1.11-1.466-.908-.62.069-.608.069-.608 1.003.07 1.531 1.032 1.531 1.032.892 1.53 2.341 1.088 2.91.832.092-.647.35-1.088.636-1.338-2.22-.253-4.555-1.113-4.555-4.951 0-1.093.39-1.988 1.029-2.688-.103-.253-.446-1.272.098-2.65 0 0 .84-.27 2.75 1.026A9.564 9.564 0 0112 6.844c.85.004 1.705.115 2.504.337 1.909-1.296 2.747-1.027 2.747-1.027.546 1.379.202 2.398.1 2.651.64.7 1.028 1.595 1.028 2.688 0 3.848-2.339 4.695-4.566 4.943.359.309.678.92.678 1.855 0 1.338-.012 2.419-.012 2.747 0 .268.18.58.688.482A10.019 10.019 0 0022 12.017C22 6.484 17.522 2 12 2z"
                  clipRule="evenodd"
                />
              </svg>
              View on GitHub
            </a>
          </div>
          </div>
        </aside>

        <div className="flex-1 min-w-0 h-full overflow-y-auto scrollbar-thin">
          <header className="sticky top-0 z-30 flex items-center justify-between h-16 px-4 border-b border-slate-200 dark:border-slate-800 speed-glass speed-nav">
          <div className="flex items-center gap-4">
            <button
              type="button"
              className="lg:hidden p-2 rounded-lg text-slate-500 hover:bg-slate-100 dark:hover:bg-slate-800 transition-colors duration-150"
              onClick={() => setSidebarOpen(true)}
              aria-label="Open navigation menu"
            >
              <Menu size={20} aria-hidden="true" />
            </button>

            <button
              type="button"
              className="hidden sm:flex items-center gap-2 px-3 py-1.5 text-sm text-slate-500 bg-slate-100 dark:bg-slate-800 rounded-lg hover:bg-slate-200 dark:hover:bg-slate-700 transition-colors duration-150"
              aria-label="Search documentation"
            >
              <Search size={16} aria-hidden="true" />
              <span>Search docs...</span>
              <kbd className="hidden md:inline-flex items-center gap-0.5 px-1.5 py-0.5 text-xs font-mono bg-slate-200 dark:bg-slate-700 rounded">
                <span>CMD</span>K
              </kbd>
            </button>
          </div>

          <div className="flex items-center gap-1">
            <a
              href="https://github.com/maskdotdev/ray"
              target="_blank"
              rel="noopener noreferrer"
              className="p-2.5 rounded-lg text-slate-500 hover:bg-slate-100 dark:hover:bg-slate-800 hover:text-slate-900 dark:hover:text-white transition-colors duration-150"
              aria-label="View on GitHub"
            >
              <svg className="w-5 h-5" fill="currentColor" viewBox="0 0 24 24" aria-hidden="true">
                <path
                  fillRule="evenodd"
                  d="M12 2C6.477 2 2 6.484 2 12.017c0 4.425 2.865 8.18 6.839 9.504.5.092.682-.217.682-.483 0-.237-.008-.868-.013-1.703-2.782.605-3.369-1.343-3.369-1.343-.454-1.158-1.11-1.466-1.11-1.466-.908-.62.069-.608.069-.608 1.003.07 1.531 1.032 1.531 1.032.892 1.53 2.341 1.088 2.91.832.092-.647.35-1.088.636-1.338-2.22-.253-4.555-1.113-4.555-4.951 0-1.093.39-1.988 1.029-2.688-.103-.253-.446-1.272.098-2.65 0 0 .84-.27 2.75 1.026A9.564 9.564 0 0112 6.844c.85.004 1.705.115 2.504.337 1.909-1.296 2.747-1.027 2.747-1.027.546 1.379.202 2.398.1 2.651.64.7 1.028 1.595 1.028 2.688 0 3.848-2.339 4.695-4.566 4.943.359.309.678.92.678 1.855 0 1.338-.012 2.419-.012 2.747 0 .268.18.58.688.482A10.019 10.019 0 0022 12.017C22 6.484 17.522 2 12 2z"
                  clipRule="evenodd"
                />
              </svg>
            </a>
          </div>
        </header>

          <main id="doc-content" className="min-h-[calc(100vh-4rem)]">
            {children}
          </main>
        </div>
      </div>
    </div>
  );
}
