import { createSignal } from 'solid-js'

export const LANGUAGES = [
  { id: 'typescript', label: 'TypeScript', short: 'TS' },
  { id: 'rust', label: 'Rust', short: 'RS' },
  { id: 'python', label: 'Python', short: 'PY' },
] as const

export type Language = typeof LANGUAGES[number]

const STORAGE_KEY = 'raydb-preferred-language'

// Get initial language from localStorage or default to TypeScript
function getInitialLanguage(): Language {
  if (typeof window === 'undefined') {
    return LANGUAGES[0]
  }
  
  try {
    const stored = localStorage.getItem(STORAGE_KEY)
    if (stored) {
      const found = LANGUAGES.find(l => l.id === stored)
      if (found) return found
    }
  } catch {
    // localStorage not available
  }
  
  return LANGUAGES[0]
}

// Create a global signal for the selected language
const [selectedLanguage, setSelectedLanguageInternal] = createSignal<Language>(getInitialLanguage())

// Wrapper that also persists to localStorage
export function setSelectedLanguage(lang: Language) {
  setSelectedLanguageInternal(lang)
  
  if (typeof window !== 'undefined') {
    try {
      localStorage.setItem(STORAGE_KEY, lang.id)
    } catch {
      // localStorage not available
    }
  }
}

export { selectedLanguage }
