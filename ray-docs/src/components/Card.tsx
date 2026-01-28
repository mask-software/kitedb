import type { Component, JSX } from 'solid-js'
import { Show } from 'solid-js'
import { ArrowRight } from 'lucide-solid'

interface CardProps {
  title: string
  description?: string
  icon?: JSX.Element
  href?: string
  class?: string
  children?: JSX.Element
  accentColor?: string
}

export const Card: Component<CardProps> = (props) => {
  const accent = () => props.accentColor || '#00d4ff'

  const cardContent = () => (
    <>
      <div class="console-scanlines opacity-5" aria-hidden="true" />
      <div class="relative flex items-start gap-4">
        <Show when={props.icon}>
          <div
            class="flex-shrink-0 w-10 h-10 icon-tile rounded-lg border transition-all duration-200 group-hover:shadow-[0_0_15px_var(--glow-color)]"
            style={{
              background: `${accent()}10`,
              'border-color': `${accent()}30`,
              color: accent(),
              '--glow-color': `${accent()}40`,
            }}
          >
            {props.icon}
          </div>
        </Show>
        <div class="flex-1 min-w-0">
          <h3 class="font-mono font-semibold text-white text-sm group-hover:text-[#00d4ff] transition-colors duration-150">
            {props.title}
          </h3>
          <Show when={props.description}>
            <p class="mt-1.5 text-xs text-slate-500 leading-relaxed line-clamp-2">
              {props.description}
            </p>
          </Show>
          {props.children}
        </div>
        <Show when={props.href}>
          <ArrowRight
            size={14}
            class="flex-shrink-0 text-slate-600 group-hover:text-[#00d4ff] group-hover:translate-x-0.5 transition-all duration-150 mt-0.5"
            aria-hidden="true"
          />
        </Show>
      </div>
    </>
  )

  const cardClass = `group relative console-container p-5 hover:border-[#00d4ff]/50 transition-all duration-200 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[#00d4ff] focus-visible:ring-offset-2 focus-visible:ring-offset-[#030712] ${props.class ?? ''}`

  return (
    <Show
      when={props.href}
      fallback={<article class={cardClass}>{cardContent()}</article>}
    >
      <a href={props.href!} class={`block ${cardClass}`}>
        {cardContent()}
      </a>
    </Show>
  )
}

interface CardGridProps {
  columns?: 1 | 2 | 3 | 4
  children: JSX.Element
}

export const CardGrid: Component<CardGridProps> = (props) => {
  const gridCols = () => {
    switch (props.columns ?? 2) {
      case 1: return 'grid-cols-1'
      case 2: return 'grid-cols-1 sm:grid-cols-2'
      case 3: return 'grid-cols-1 sm:grid-cols-2 lg:grid-cols-3'
      case 4: return 'grid-cols-1 sm:grid-cols-2 lg:grid-cols-4'
      default: return 'grid-cols-1 sm:grid-cols-2'
    }
  }

  return (
    <div class={`grid gap-3 ${gridCols()}`}>
      {props.children}
    </div>
  )
}

export default Card
