import { createFileRoute } from '@tanstack/solid-router'
import { For } from 'solid-js'
import {
  Rocket,
  BookOpen,
  Code,
  Zap,
  Database,
  GitBranch,
  ArrowRight,
  Terminal,
  Layers,
  Search,
  Shield,
} from 'lucide-solid'
import { docsStructure } from '~/lib/docs'

export const Route = createFileRoute('/docs/')({
  component: DocsIndex,
})

// Console-style doc card
function DocCard(props: {
  title: string
  description?: string
  href: string
  icon: any
  accentColor?: string
}) {
  const accent = props.accentColor || '#00d4ff'

  return (
    <a
      href={props.href}
      class="group console-container p-5 block hover:border-[#00d4ff]/50 transition-all duration-200"
    >
      <div class="console-scanlines opacity-5" aria-hidden="true" />
      <div class="relative flex items-start gap-4">
        <div
          class="flex-shrink-0 w-10 h-10 icon-tile rounded-lg border transition-all duration-200"
          style={{
            background: `${accent}10`,
            'border-color': `${accent}30`,
            color: accent,
          }}
        >
          {props.icon}
        </div>
        <div class="min-w-0 flex-1">
          <h3 class="font-mono font-semibold text-white text-sm group-hover:text-[#00d4ff] transition-colors duration-150">
            {props.title}
          </h3>
          {props.description && (
            <p class="mt-1.5 text-xs text-slate-500 leading-relaxed line-clamp-2">
              {props.description}
            </p>
          )}
        </div>
        <ArrowRight
          size={14}
          class="flex-shrink-0 text-slate-600 group-hover:text-[#00d4ff] group-hover:translate-x-0.5 transition-all duration-150 mt-0.5"
          aria-hidden="true"
        />
      </div>
    </a>
  )
}

function DocsIndex() {
  // Map section/slug to appropriate icon and color
  const getIconForSlug = (slug: string) => {
    if (slug.includes('installation')) return { icon: <Rocket size={18} aria-hidden="true" />, color: '#00d4ff' }
    if (slug.includes('quick-start')) return { icon: <Zap size={18} aria-hidden="true" />, color: '#febc2e' }
    if (slug.includes('schema')) return { icon: <Database size={18} aria-hidden="true" />, color: '#7c3aed' }
    if (slug.includes('traversal')) return { icon: <GitBranch size={18} aria-hidden="true" />, color: '#28c840' }
    if (slug.includes('vector')) return { icon: <Search size={18} aria-hidden="true" />, color: '#ff5f57' }
    if (slug.includes('api')) return { icon: <Code size={18} aria-hidden="true" />, color: '#00d4ff' }
    if (slug.includes('internal') || slug.includes('architecture')) return { icon: <Layers size={18} aria-hidden="true" />, color: '#7c3aed' }
    if (slug.includes('transaction') || slug.includes('mvcc')) return { icon: <Shield size={18} aria-hidden="true" />, color: '#28c840' }
    return { icon: <BookOpen size={18} aria-hidden="true" />, color: '#00d4ff' }
  }

  return (
    <div class="max-w-4xl mx-auto px-6 py-12">
      {/* Hero - Console style */}
      <div class="console-container p-8 mb-12">
        <div class="console-scanlines opacity-10" aria-hidden="true" />
        <div class="relative">
          {/* Console header decoration */}
          <div class="flex items-center gap-2 mb-6">
            <div class="console-dots flex gap-1.5">
              <div class="w-2.5 h-2.5 rounded-full bg-[#ff5f57]" />
              <div class="w-2.5 h-2.5 rounded-full bg-[#febc2e]" />
              <div class="w-2.5 h-2.5 rounded-full bg-[#28c840]" />
            </div>
            <span class="font-mono text-xs text-slate-500 ml-2">docs — raydb</span>
          </div>

          <div class="flex items-center gap-3 mb-4">
            <Terminal size={24} class="text-[#00d4ff]" aria-hidden="true" />
            <h1 class="text-2xl md:text-3xl font-mono font-bold text-white">
              DOCUMENTATION
            </h1>
          </div>
          <p class="text-slate-400 font-mono text-sm leading-relaxed max-w-2xl">
            <span class="text-[#00d4ff]">$</span> Learn how to build high-performance graph databases with vector search using RayDB.
          </p>

          {/* Quick command hint */}
          <div class="mt-6 flex items-center gap-4 text-xs font-mono">
            <span class="text-slate-500">Try:</span>
            <code class="px-2 py-1 rounded bg-[#1a2a42] text-[#28c840]">bun add @ray-db/ray</code>
          </div>
        </div>
      </div>

      {/* Quick start cards - featured */}
      <section class="mb-16" aria-labelledby="quickstart-heading">
        <div class="flex items-center gap-3 mb-6">
          <Zap size={18} class="text-[#febc2e]" aria-hidden="true" />
          <h2 id="quickstart-heading" class="font-mono text-sm text-slate-400 uppercase tracking-wider">
            QUICK_START
          </h2>
          <div class="flex-1 h-px bg-gradient-to-r from-[#1a2a42] to-transparent" />
        </div>

        <div class="grid sm:grid-cols-2 gap-4">
          <a
            href="/docs/getting-started/installation"
            class="group console-container p-6 block electric-glow"
          >
            <div class="console-scanlines opacity-5" aria-hidden="true" />
            <div class="relative flex items-start gap-4">
              <div class="flex-shrink-0 w-12 h-12 icon-tile rounded-xl bg-[#00d4ff]/10 border border-[#00d4ff]/30 text-[#00d4ff] group-hover:shadow-[0_0_20px_rgba(0,212,255,0.4)] transition-all">
                <Rocket size={22} aria-hidden="true" />
              </div>
              <div class="min-w-0 flex-1">
                <h3 class="font-mono font-semibold text-white group-hover:text-[#00d4ff] transition-colors">
                  ./install
                </h3>
                <p class="mt-1.5 text-sm text-slate-500">
                  Set up RayDB in your project in under 2&nbsp;minutes.
                </p>
              </div>
              <ArrowRight size={18} class="flex-shrink-0 text-slate-600 group-hover:text-[#00d4ff] group-hover:translate-x-1 transition-all duration-150 mt-1" aria-hidden="true" />
            </div>
            {/* Electric border effect */}
            <div class="absolute inset-0 rounded-lg opacity-0 group-hover:opacity-100 transition-opacity duration-300 electric-border pointer-events-none" aria-hidden="true" />
          </a>

          <a
            href="/docs/getting-started/quick-start"
            class="group console-container p-6 block electric-glow"
          >
            <div class="console-scanlines opacity-5" aria-hidden="true" />
            <div class="relative flex items-start gap-4">
              <div class="flex-shrink-0 w-12 h-12 icon-tile rounded-xl bg-[#7c3aed]/10 border border-[#7c3aed]/30 text-[#7c3aed] group-hover:shadow-[0_0_20px_rgba(124,58,237,0.4)] transition-all">
                <Code size={22} aria-hidden="true" />
              </div>
              <div class="min-w-0 flex-1">
                <h3 class="font-mono font-semibold text-white group-hover:text-[#7c3aed] transition-colors">
                  ./quickstart
                </h3>
                <p class="mt-1.5 text-sm text-slate-500">
                  Build your first graph database in 5&nbsp;minutes.
                </p>
              </div>
              <ArrowRight size={18} class="flex-shrink-0 text-slate-600 group-hover:text-[#7c3aed] group-hover:translate-x-1 transition-all duration-150 mt-1" aria-hidden="true" />
            </div>
          </a>
        </div>
      </section>

      {/* All sections */}
      <For each={docsStructure}>
        {(section) => (
          <section class="mb-12" aria-labelledby={`section-${section.label.toLowerCase().replace(/\s+/g, '-')}`}>
            <div class="flex items-center gap-3 mb-6">
              <BookOpen size={18} class="text-[#00d4ff]" aria-hidden="true" />
              <h2
                id={`section-${section.label.toLowerCase().replace(/\s+/g, '-')}`}
                class="font-mono text-sm text-slate-400 uppercase tracking-wider"
              >
                {section.label.replace(/\s+/g, '_')}
              </h2>
              <div class="flex-1 h-px bg-gradient-to-r from-[#1a2a42] to-transparent" />
            </div>

            <div class="grid sm:grid-cols-2 gap-3">
              <For each={section.items}>
                {(item) => {
                  const { icon, color } = getIconForSlug(item.slug)
                  return (
                    <DocCard
                      title={item.title}
                      description={item.description}
                      href={`/docs/${item.slug}`}
                      icon={icon}
                      accentColor={color}
                    />
                  )
                }}
              </For>
            </div>
          </section>
        )}
      </For>

      {/* Footer hint */}
      <div class="mt-16 pt-8 border-t border-[#1a2a42]">
        <p class="font-mono text-xs text-slate-600 text-center">
          <span class="text-[#00d4ff]">$</span> Press <kbd class="px-1.5 py-0.5 bg-[#1a2a42] rounded text-slate-400">⌘K</kbd> to search docs
        </p>
      </div>
    </div>
  )
}
