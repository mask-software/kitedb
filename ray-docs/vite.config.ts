import { defineConfig } from 'vite'
import path from 'node:path'
import fs from 'node:fs'
import { fileURLToPath } from 'node:url'
import { devtools } from '@tanstack/devtools-vite'
import viteTsConfigPaths from 'vite-tsconfig-paths'
import tailwindcss from '@tailwindcss/vite'

import { tanstackStart } from '@tanstack/solid-start/plugin/vite'
import solidPlugin from 'vite-plugin-solid'
import { nitro } from 'nitro/vite'

import lucidePreprocess from 'vite-plugin-lucide-preprocess'

const configDir = path.dirname(fileURLToPath(import.meta.url))
const kitePackagePath = path.resolve(configDir, '../ray-rs/package.json')
const kitePackage = JSON.parse(fs.readFileSync(kitePackagePath, 'utf-8')) as {
  version?: string
}
const kiteVersion = kitePackage.version ?? '0.0.0'

export default defineConfig({
  define: {
    __KITE_VERSION__: JSON.stringify(kiteVersion),
  },
  plugins: [
    lucidePreprocess(),
    devtools(),
    nitro({
      // Vercel's build (VERCEL=1) writes its Build Output API layout to
      // .vercel/output. Every other build writes the Node server to .output,
      // which `bun run start` and the docs workflow run; pinned so Nitro's
      // provider auto-detection can't pick another layout in some CI.
      preset: process.env.VERCEL ? 'vercel' : 'node-server',
    }),
    // this is the plugin that enables path aliases
    viteTsConfigPaths({
      projects: ['./tsconfig.json'],
    }),
    tailwindcss(),
    tanstackStart(),
    solidPlugin({ ssr: true, hot: false }),
  ],
})
