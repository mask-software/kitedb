import { defineConfig } from 'vite'
import { devtools } from '@tanstack/devtools-vite'
import viteTsConfigPaths from 'vite-tsconfig-paths'
import tailwindcss from '@tailwindcss/vite'

import { tanstackStart } from '@tanstack/solid-start/plugin/vite'
import solidPlugin from 'vite-plugin-solid'
import { nitro } from 'nitro/vite'

import lucidePreprocess from 'vite-plugin-lucide-preprocess'

export default defineConfig({
  plugins: [
    lucidePreprocess(),
    devtools(),
    nitro({
      // Vercel will auto-detect or use vercel preset
      // For local dev, defaults to node-server
      preset: process.env.VERCEL ? 'vercel' : undefined,
    }),
    // this is the plugin that enables path aliases
    viteTsConfigPaths({
      projects: ['./tsconfig.json'],
    }),
    tailwindcss(),
    tanstackStart(),
    solidPlugin({ ssr: true }),
  ],
})
