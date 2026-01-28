import type { Component, JSX } from 'solid-js'
import { Show } from 'solid-js'

import { ArrowLeft, ArrowRight, Pencil } from 'lucide-solid'
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
      {/* Page header */}
      <header class="mb-10">
        <h1 class="text-3xl md:text-4xl font-extrabold text-slate-900 dark:text-white tracking-tight text-balance">
          {doc()?.title ?? 'Documentation'}
        </h1>
        <Show when={doc()?.description}>
          <p class="mt-4 text-lg text-slate-600 dark:text-slate-400 text-pretty">
            {doc()?.description}
          </p>
        </Show>
      </header>

      {/* Content */}
      <div class="prose max-w-none">
        {props.children}
      </div>

      {/* Edit link */}
      <div class="mt-12 pt-6 border-t border-slate-200 dark:border-slate-800">
        <a
          href={`https://github.com/maskdotdev/ray/edit/main/docs-site/src/content/docs/${props.slug || 'index'}.md`}
          target="_blank"
          rel="noopener noreferrer"
          class="inline-flex items-center gap-2 text-sm text-slate-500 hover:text-slate-700 dark:hover:text-slate-300 transition-colors duration-150"
        >
          <Pencil size={14} aria-hidden="true" />
          Edit this page on GitHub
        </a>
      </div>

      {/* Prev/Next navigation */}
      <nav class="mt-8 flex items-stretch gap-4" aria-label="Documentation pages">
        <Show when={prevDoc()}>
          {(prev) => (
            <a
              href={`/docs/${prev().slug}`}
              class="group flex-1 flex flex-col items-start p-4 rounded-xl border border-slate-200 dark:border-slate-800 hover:border-cyan-500/50 hover:bg-slate-50 dark:hover:bg-slate-800/50 transition-all duration-150"
            >
              <span class="flex items-center gap-1 text-xs font-medium text-slate-500 mb-1">
                <ArrowLeft size={12} class="group-hover:-translate-x-0.5 transition-transform duration-150" aria-hidden="true" />
                Previous
              </span>
              <span class="font-medium text-slate-900 dark:text-white group-hover:text-cyan-600 dark:group-hover:text-cyan-400 transition-colors duration-150">
                {prev().title}
              </span>
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
              class="group flex-1 flex flex-col items-end p-4 rounded-xl border border-slate-200 dark:border-slate-800 hover:border-cyan-500/50 hover:bg-slate-50 dark:hover:bg-slate-800/50 transition-all duration-150"
            >
              <span class="flex items-center gap-1 text-xs font-medium text-slate-500 mb-1">
                Next
                <ArrowRight size={12} class="group-hover:translate-x-0.5 transition-transform duration-150" aria-hidden="true" />
              </span>
              <span class="font-medium text-slate-900 dark:text-white group-hover:text-cyan-600 dark:group-hover:text-cyan-400 transition-colors duration-150">
                {next().title}
              </span>
            </a>
          )}
        </Show>
      </nav>
    </article>
  )
}

export default DocPage
