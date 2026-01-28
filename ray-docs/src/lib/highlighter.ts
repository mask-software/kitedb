import { createHighlighter, type Highlighter } from 'shiki'

let highlighterPromise: Promise<Highlighter> | null = null

export async function getHighlighter(): Promise<Highlighter> {
  if (!highlighterPromise) {
    highlighterPromise = createHighlighter({
      themes: ['github-dark'],
      langs: ['typescript', 'javascript', 'bash', 'json', 'tsx', 'jsx', 'text', 'shell'],
    })
  }
  return highlighterPromise
}

export async function highlightCode(code: string, lang: string): Promise<string> {
  const highlighter = await getHighlighter()
  
  // Map common language aliases
  const langMap: Record<string, string> = {
    ts: 'typescript',
    js: 'javascript',
    sh: 'bash',
    shell: 'bash',
  }
  
  const resolvedLang = langMap[lang] || lang
  
  // Check if language is supported, fall back to text if not
  const supportedLangs = highlighter.getLoadedLanguages()
  const finalLang = supportedLangs.includes(resolvedLang as any) ? resolvedLang : 'text'
  
  return highlighter.codeToHtml(code, {
    lang: finalLang,
    theme: 'github-dark',
  })
}
