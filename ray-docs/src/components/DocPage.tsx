import type { Component, JSX } from 'solid-js'
import { Show } from 'solid-js'

import { ArrowLeft, ArrowRight, Pencil, Terminal } from 'lucide-solid'
import { findDocBySlug, getNextDoc, getPrevDoc } from '~/lib/docs'

interface DocPageProps {
  slug: string
  children: JSX.Element
}

export const DocPage: Component<DocPageProps> = (props) => {
  const doc = () => findDocBySlug(props.slug)
  const prevDoc = () => getPrevDoc(props.slug)
  const nextDoc = () => getNextDoc(props.slug)

  return (
    <article class="max-w-4xl mx-auto px-6 py-12">
      {/* Page header - console style */}
      <header class="mb-10">
        {/* Breadcrumb path */}
        <div class="flex items-center gap-2 text-xs font-mono text-slate-500 mb-4">
          <span class="text-[#00d4ff]">~</span>
          <span>/docs/</span>
          <span class="text-[#00d4ff]">{props.slug || 'index'}</span>
        </div>

        {/* Console container for title */}
        <div class="console-container p-6 mb-6">
          <div class="console-scanlines opacity-10" aria-hidden="true" />
          <div class="relative">
            <div class="flex items-center gap-3 mb-4">
              <Terminal size={20} class="text-[#00d4ff]" aria-hidden="true" />
              <span class="font-mono text-xs text-slate-500 uppercase tracking-wider">
                DOCUMENTATION
              </span>
            </div>
            <h1 class="text-2xl md:text-3xl font-mono font-bold text-white tracking-tight">
              {doc()?.title ?? 'Documentation'}
            </h1>
            <Show when={doc()?.description}>
              <p class="mt-3 text-slate-400 font-mono text-sm leading-relaxed">
                {doc()?.description}
              </p>
            </Show>
          </div>
        </div>
      </header>

      {/* Content */}
      <div class="prose prose-console max-w-none">
        {props.children}
      </div>

      {/* Edit link */}
      <div class="mt-12 pt-6 border-t border-[#1a2a42]">
        <a
          href={`https://github.com/maskdotdev/ray/edit/main/docs-site/src/content/docs/${props.slug || 'index'}.md`}
          target="_blank"
          rel="noopener noreferrer"
          class="inline-flex items-center gap-2 text-sm font-mono text-slate-500 hover:text-[#00d4ff] transition-colors duration-150"
        >
          <Pencil size={14} aria-hidden="true" />
          ./edit --remote
        </a>
      </div>

      {/* Prev/Next navigation - console style */}
      <nav class="mt-8 flex items-stretch gap-4" aria-label="Documentation pages">
        <Show when={prevDoc()}>
          {(prev) => (
            <a
              href={`/docs/${prev().slug}`}
              class="group flex-1 console-container p-4 hover:border-[#00d4ff]/50 transition-colors"
            >
              <div class="console-scanlines opacity-5" aria-hidden="true" />
              <div class="relative">
                <span class="flex items-center gap-1 text-xs font-mono text-slate-500 mb-2">
                  <ArrowLeft size={12} class="group-hover:-translate-x-0.5 transition-transform duration-150" aria-hidden="true" />
                  cd ..
                </span>
                <span class="font-mono text-sm text-white group-hover:text-[#00d4ff] transition-colors duration-150">
                  {prev().title}
                </span>
              </div>
            </a>
          )}
        </Show>

        <Show when={!prevDoc()}>
          <div class="flex-1" />
        </Show>

        <Show when={nextDoc()}>
          {(next) => (
            <a
              href={`/docs/${next().slug}`}
              class="group flex-1 console-container p-4 hover:border-[#00d4ff]/50 transition-colors text-right"
            >
              <div class="console-scanlines opacity-5" aria-hidden="true" />
              <div class="relative">
                <span class="flex items-center justify-end gap-1 text-xs font-mono text-slate-500 mb-2">
                  cd ./next
                  <ArrowRight size={12} class="group-hover:translate-x-0.5 transition-transform duration-150" aria-hidden="true" />
                </span>
                <span class="font-mono text-sm text-white group-hover:text-[#00d4ff] transition-colors duration-150">
                  {next().title}
                </span>
              </div>
            </a>
          )}
        </Show>
      </nav>
    </article>
  )
}

export default DocPage
