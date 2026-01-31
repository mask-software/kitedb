import { createSignal } from 'solid-js'
import { isServer } from 'solid-js/web'

export const LANGUAGES = [
  { id: 'typescript', label: 'TypeScript', short: 'TS' },
  { id: 'rust', label: 'Rust', short: 'RS' },
  { id: 'python', label: 'Python', short: 'PY' },
] as const

export type Language = (typeof LANGUAGES)[number]

const STORAGE_KEY = 'kitedb-preferred-language'

// Always start with default to match SSR output
// This ensures hydration doesn't mismatch
const [_selectedLanguage, _setSelectedLanguage] = createSignal<Language>(LANGUAGES[0])

// Track if we've initialized from storage
let _initialized = false

// Call this from a component's onMount to hydrate from localStorage
export function initLanguageFromStorage() {
  if (_initialized || isServer) return
  _initialized = true
  
  try {
    const stored = localStorage.getItem(STORAGE_KEY)
    if (stored) {
      const found = LANGUAGES.find((l) => l.id === stored)
      if (found && found.id !== LANGUAGES[0].id) {
        _setSelectedLanguage(found)
      }
    }
  } catch {
    // localStorage not available
  }
}

// Export the getter
export const selectedLanguage = _selectedLanguage

// Export setter that also persists to localStorage
export function setSelectedLanguage(lang: Language) {
  _setSelectedLanguage(lang)
  
  if (!isServer) {
    try {
      localStorage.setItem(STORAGE_KEY, lang.id)
    } catch {
      // localStorage not available
    }
  }
}
