import { createHighlighterCore, type HighlighterCore, type ThemedToken } from 'shiki/core'
import { createJavaScriptRegexEngine } from 'shiki/engine/javascript'
import { kiteNight } from './kite-theme'

let highlighterPromise: Promise<HighlighterCore> | null = null

// Fine-grained language imports for lazy loading
// These use default exports, so we import them directly
const langImports = {
  typescript: () => import('shiki/dist/langs/typescript.mjs').then(m => m.default),
  javascript: () => import('shiki/dist/langs/javascript.mjs').then(m => m.default),
  bash: () => import('shiki/dist/langs/bash.mjs').then(m => m.default),
  json: () => import('shiki/dist/langs/json.mjs').then(m => m.default),
  tsx: () => import('shiki/dist/langs/tsx.mjs').then(m => m.default),
  jsx: () => import('shiki/dist/langs/jsx.mjs').then(m => m.default),
  rust: () => import('shiki/dist/langs/rust.mjs').then(m => m.default),
  python: () => import('shiki/dist/langs/python.mjs').then(m => m.default),
}

// Map common language aliases
const langAliases: Record<string, keyof typeof langImports> = {
  ts: 'typescript',
  js: 'javascript',
  sh: 'bash',
  shell: 'bash',
  py: 'python',
  rs: 'rust',
}

// Track which languages have been loaded
const loadedLangs = new Set<string>()

function getHighlighter(): Promise<HighlighterCore> {
  // Assign synchronously so concurrent callers share one highlighter instance
  highlighterPromise ??= createHighlighterCore({
    themes: [kiteNight],
    langs: [], // Start with no languages, load on demand
    engine: createJavaScriptRegexEngine(),
  })
  return highlighterPromise
}

async function ensureLangLoaded(highlighter: HighlighterCore, lang: string): Promise<string> {
  // Resolve alias
  const resolvedLang = (langAliases[lang] || lang) as keyof typeof langImports

  // Check if it's a supported language
  if (!(resolvedLang in langImports)) {
    return 'text' // Fallback to plain text
  }

  // Load language if not already loaded
  if (!loadedLangs.has(resolvedLang)) {
    const langModule = await langImports[resolvedLang]()
    await highlighter.loadLanguage(langModule)
    loadedLangs.add(resolvedLang)
  }

  return resolvedLang
}

export type CodeTokens = ThemedToken[][]

const tokenCache = new Map<string, CodeTokens>()
const tokenCacheKey = (code: string, lang: string) => `${lang}\u0000${code}`

/** Synchronous cache lookup, so already-highlighted code renders without a flash. */
export function peekTokens(code: string, lang: string): CodeTokens | undefined {
  return tokenCache.get(tokenCacheKey(code, lang))
}

/** Tokenize code with the kite-night theme, one token array per line. */
export async function highlightTokens(code: string, lang: string): Promise<CodeTokens> {
  const key = tokenCacheKey(code, lang)
  const cached = tokenCache.get(key)
  if (cached) return cached

  const highlighter = await getHighlighter()
  const finalLang = await ensureLangLoaded(highlighter, lang)
  const { tokens } = highlighter.codeToTokens(code, {
    lang: finalLang as Parameters<HighlighterCore['codeToTokens']>[1]['lang'],
    theme: 'kite-night',
  })
  tokenCache.set(key, tokens)
  return tokens
}
