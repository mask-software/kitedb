import type { Component } from 'solid-js'
import { createSignal, createResource, Show, Suspense } from 'solid-js'
import { Check, Copy, FileCode } from 'lucide-solid'
import { highlightCode } from '~/lib/highlighter'

interface CodeBlockProps {
  code: string
  language?: string
  filename?: string
  class?: string
}

export const CodeBlock: Component<CodeBlockProps> = (props) => {
  const [copied, setCopied] = createSignal(false)

  // Create a resource for highlighted code
  const [highlightedHtml] = createResource(
    () => ({ code: props.code, lang: props.language || 'text' }),
    async ({ code, lang }) => {
      try {
        return await highlightCode(code, lang)
      } catch (e) {
        console.error('Highlighting failed:', e)
        return null
      }
    }
  )

  const copyToClipboard = async () => {
    try {
      await navigator.clipboard.writeText(props.code)
      setCopied(true)
      setTimeout(() => setCopied(false), 2000)
    } catch (err) {
      console.error('Failed to copy:', err)
    }
  }

  return (
    <div class={`group relative rounded-2xl overflow-hidden border border-[#1e3a5f] bg-[#0d1117] shadow-[0_0_40px_rgba(0,0,0,0.3)] ${props.class ?? ''}`}>
      {/* Header */}
      <Show when={props.filename || props.language}>
        <div class="flex items-center justify-between px-5 py-3 bg-[#161b22] border-b border-[#30363d]">
          <div class="flex items-center gap-3">
            <FileCode size={16} class="text-[#00d4ff]" aria-hidden="true" />
            <Show when={props.filename}>
              <span class="text-sm font-medium text-slate-300">
                {props.filename}
              </span>
            </Show>
            <Show when={props.language && !props.filename}>
              <span class="text-xs font-mono text-slate-500 uppercase tracking-wider">
                {props.language}
              </span>
            </Show>
          </div>
          <button
            type="button"
            onClick={copyToClipboard}
            class="flex items-center gap-2 px-3 py-1.5 text-xs font-medium rounded-lg text-slate-400 hover:text-[#00d4ff] bg-[#21262d] hover:bg-[#30363d] transition-colors duration-150"
            aria-label={copied() ? 'Copied!' : 'Copy code to clipboard'}
          >
            <Show when={copied()} fallback={<Copy size={14} aria-hidden="true" />}>
              <Check size={14} class="text-emerald-400" aria-hidden="true" />
            </Show>
            <span>{copied() ? 'Copied!' : 'Copy'}</span>
          </button>
        </div>
      </Show>

      {/* Code content with Shiki highlighting */}
      <div class="overflow-x-auto scrollbar-thin">
        <Suspense fallback={
          <pre class="p-6 text-sm leading-relaxed">
            <code class="font-mono text-slate-200 whitespace-pre">{props.code}</code>
          </pre>
        }>
          <Show
            when={highlightedHtml()}
            fallback={
              <pre class="p-6 text-sm leading-relaxed">
                <code class="font-mono text-slate-200 whitespace-pre">{props.code}</code>
              </pre>
            }
          >
            <div
              class="shiki-wrapper [&_pre]:p-6 [&_pre]:text-sm [&_pre]:leading-relaxed [&_pre]:bg-transparent! [&_code]:font-mono"
              innerHTML={highlightedHtml() ?? undefined}
            />
          </Show>
        </Suspense>
      </div>

      {/* Copy button overlay for blocks without header */}
      <Show when={!props.filename && !props.language}>
        <button
          type="button"
          onClick={copyToClipboard}
          class="absolute top-4 right-4 p-2.5 rounded-lg text-slate-400 hover:text-[#00d4ff] bg-[#21262d]/80 hover:bg-[#30363d] transition-all duration-150 opacity-0 group-hover:opacity-100 focus:opacity-100"
          aria-label={copied() ? 'Copied!' : 'Copy code to clipboard'}
        >
          <Show when={copied()} fallback={<Copy size={16} aria-hidden="true" />}>
            <Check size={16} class="text-emerald-400" aria-hidden="true" />
          </Show>
        </button>
      </Show>
    </div>
  )
}

export default CodeBlock
