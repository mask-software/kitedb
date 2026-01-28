import type { Component, JSX } from 'solid-js'

interface StatCardProps {
  value: string
  label: string
  unit?: string
}

export const StatCard: Component<StatCardProps> = (props) => {
  return (
    <article class="group console-container p-5 text-center hover:border-[#00d4ff]/40 transition-all duration-200">
      <div class="console-scanlines opacity-5" aria-hidden="true" />
      <div class="relative">
        <div class="terminal-stat-label mb-2 font-mono text-xs uppercase tracking-wider text-slate-500">
          {props.label.replace(/\s+/g, '_')}
        </div>
        <div class="terminal-stat-value text-3xl md:text-4xl font-mono font-bold tabular-nums text-[#00d4ff] flex items-baseline justify-center gap-1">
          {props.value}
          {props.unit && (
            <span class="text-sm text-slate-500">{props.unit}</span>
          )}
        </div>
      </div>
    </article>
  )
}

interface StatGridProps {
  children: JSX.Element
}

export const StatGrid: Component<StatGridProps> = (props) => {
  return (
    <div class="grid grid-cols-2 md:grid-cols-4 gap-3" role="list" aria-label="Performance statistics">
      {props.children}
    </div>
  )
}

export default StatCard
