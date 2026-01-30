import type { Component } from 'solid-js'
import CodeBlock from './code-block'
import { selectedLanguage } from '~/lib/language-store'

export interface MultiLangCodeProps {
  typescript: string
  rust: string
  python: string
  filename?: { ts?: string; rs?: string; py?: string }
  inline?: boolean
}

/**
 * A code block component that displays code in the user's selected language.
 * Uses the global language preference from language-store.
 */
export const MultiLangCode: Component<MultiLangCodeProps> = (props) => {
  const code = () => {
    switch (selectedLanguage().id) {
      case 'rust':
        return props.rust
      case 'python':
        return props.python
      default:
        return props.typescript
    }
  }

  const language = () => {
    switch (selectedLanguage().id) {
      case 'rust':
        return 'rust'
      case 'python':
        return 'python'
      default:
        return 'typescript'
    }
  }

  const filename = () => {
    const lang = selectedLanguage().id
    if (!props.filename) return undefined
    return lang === 'rust'
      ? props.filename.rs
      : lang === 'python'
        ? props.filename.py
        : props.filename.ts
  }

  return (
    <CodeBlock
      code={code()}
      language={language()}
      filename={filename()}
      inline={props.inline}
    />
  )
}

export default MultiLangCode
