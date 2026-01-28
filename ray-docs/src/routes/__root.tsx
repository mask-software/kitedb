import {
  HeadContent,
  Outlet,
  Scripts,
  createRootRouteWithContext,
} from '@tanstack/solid-router'
import { TanStackRouterDevtools } from '@tanstack/solid-router-devtools'
import { HydrationScript } from 'solid-js/web'
import { Suspense } from 'solid-js'

// Import styles directly - Vite will handle injection
import '../styles.css'
import NotFound from '../components/NotFound'

function RootErrorComponent({ error }: { error: Error }) {
  return (
    <div class="min-h-screen flex items-center justify-center bg-[#030712] text-white p-8">
      <div class="max-w-md text-center">
        <h1 class="text-4xl font-bold text-[#00d4ff] mb-4">Oops!</h1>
        <p class="text-slate-400 mb-6">Something went wrong.</p>
        <pre class="text-left text-sm bg-[#0a1628] p-4 rounded-lg overflow-auto text-red-400 mb-6">
          {error.message}
        </pre>
        <a
          href="/"
          class="inline-flex items-center gap-2 px-6 py-3 bg-[#00d4ff] text-black font-semibold rounded-lg hover:bg-[#00d4ff]/90 transition-colors"
        >
          Go Home
        </a>
      </div>
    </div>
  )
}

export const Route = createRootRouteWithContext()({
  head: () => ({
    meta: [
      { charSet: 'utf-8' },
      { name: 'viewport', content: 'width=device-width, initial-scale=1' },
      { name: 'description', content: 'RayDB - High-performance embedded graph database with vector search for Bun/TypeScript' },
      { name: 'theme-color', content: '#00d4ff' },
    ],
    links: [
      { rel: 'icon', type: 'image/svg+xml', href: '/favicon.svg' },
      { rel: 'preconnect', href: 'https://fonts.googleapis.com' },
      { rel: 'preconnect', href: 'https://fonts.gstatic.com', crossorigin: 'anonymous' },
    ],
  }),
  errorComponent: RootErrorComponent,
  notFoundComponent: NotFound,
  shellComponent: RootComponent,
})

function RootComponent() {
  return (
    <html lang="en" class="dark">
      <head>
        <HydrationScript />
      </head>
      <body class="min-h-screen bg-[#030712] text-white antialiased">
        <HeadContent />
        <Suspense fallback={
          <div class="min-h-screen flex items-center justify-center bg-[#030712]">
            <div class="flex items-center gap-3">
              <div class="w-2 h-2 bg-[#00d4ff] rounded-full animate-pulse" />
              <div class="w-2 h-2 bg-[#00d4ff] rounded-full animate-pulse [animation-delay:200ms]" />
              <div class="w-2 h-2 bg-[#00d4ff] rounded-full animate-pulse [animation-delay:400ms]" />
            </div>
          </div>
        }>
          <Outlet />
        </Suspense>
        <TanStackRouterDevtools position="bottom-right" />
        <Scripts />
      </body>
    </html>
  )
}
