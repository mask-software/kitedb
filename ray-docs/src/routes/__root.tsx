import {
  HeadContent,
  Outlet,
  Scripts,
  createRootRouteWithContext,
} from "@tanstack/solid-router";
import { TanStackRouterDevtools } from "@tanstack/solid-router-devtools";
import { HydrationScript } from "solid-js/web";
import { Suspense, onMount } from "solid-js";
import { initLanguageFromStorage } from "~/lib/language-store";

// Import styles as URL to ensure explicit stylesheet link
import stylesHref from "../styles.css?url";
import NotFound from "../components/not-found";
import { SearchDialog, SearchKeyboardShortcut, searchDialog } from "../components/search-dialog";

function RootErrorComponent({ error }: { error: Error }) {
  return (
    <div class="min-h-screen flex items-center justify-center bg-kite-bg text-white p-8">
      <div class="w-full max-w-lg">
        <p class="eyebrow">Error</p>
        <h1 class="mt-4 text-[2.25rem] font-semibold leading-[1.08] tracking-[-0.035em]">
          Something went wrong
        </h1>
        <pre class="mt-6 overflow-auto rounded-xl border border-kite-line bg-kite-surface p-4 font-mono text-[13px] text-red-300">
          {error.message}
        </pre>
        <a
          href="/"
          class="mt-8 inline-flex h-10 items-center rounded-xl bg-white px-4 text-[14px] font-semibold text-kite-bg transition-colors hover:bg-slate-200"
        >
          Back to the homepage
        </a>
      </div>
    </div>
  );
}

export const Route = createRootRouteWithContext()({
  head: () => ({
    meta: [
      { charSet: "utf-8" },
      { name: "viewport", content: "width=device-width, initial-scale=1" },
      {
        name: "description",
        content:
          "KiteDB is an embedded graph database with built-in vector search for TypeScript, Python, and Rust.",
      },
      { name: "theme-color", content: "#05070d" },
    ],
    links: [
      { rel: "icon", type: "image/svg+xml", href: "/favicon.svg" },
      { rel: "icon", type: "image/png", sizes: "96x96", href: "/favicon-96x96.png" },
      { rel: "shortcut icon", href: "/favicon.ico" },
      { rel: "apple-touch-icon", sizes: "180x180", href: "/apple-touch-icon.png" },
      { rel: "manifest", href: "/site.webmanifest" },
      { rel: "preconnect", href: "https://fonts.googleapis.com" },
      {
        rel: "preconnect",
        href: "https://fonts.gstatic.com",
        crossorigin: "anonymous",
      },
      { rel: "stylesheet", href: stylesHref },
    ],
  }),
  errorComponent: RootErrorComponent,
  notFoundComponent: NotFound,
  shellComponent: RootComponent,
});

function RootComponent() {
  // Initialize language preference from localStorage after hydration
  onMount(() => {
    initLanguageFromStorage()
  })

  return (
    <html lang="en" class="dark">
      <head>
        <link rel="stylesheet" href={stylesHref} />
        <HydrationScript />
      </head>
      <body class="min-h-screen bg-kite-bg text-white antialiased">
        <HeadContent />
        <Suspense
          fallback={
            <div class="min-h-screen flex items-center justify-center bg-kite-bg">
              <div class="flex items-center gap-3">
                <div class="w-2 h-2 bg-kite-cyan rounded-full animate-pulse" />
                <div class="w-2 h-2 bg-kite-cyan rounded-full animate-pulse [animation-delay:200ms]" />
                <div class="w-2 h-2 bg-kite-cyan rounded-full animate-pulse [animation-delay:400ms]" />
              </div>
            </div>
          }
        >
          <Outlet />
        </Suspense>
        <SearchKeyboardShortcut />
        <SearchDialog open={searchDialog.isOpen()} onClose={searchDialog.close} />
        <TanStackRouterDevtools position="bottom-right" />
        <Scripts />
      </body>
    </html>
  );
}
