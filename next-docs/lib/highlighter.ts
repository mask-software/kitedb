import "server-only";

import { cache } from "react";
import { createHighlighter, type Highlighter } from "shiki";

const getHighlighter = cache(async (): Promise<Highlighter> => {
  return createHighlighter({
    themes: ["github-dark"],
    langs: ["typescript", "javascript", "bash", "json", "tsx", "jsx", "text", "shell"],
  });
});

export async function highlightCode(code: string, lang: string): Promise<string> {
  const highlighter = await getHighlighter();
  const langMap: Record<string, string> = {
    ts: "typescript",
    js: "javascript",
    sh: "bash",
    shell: "bash",
  };

  const resolvedLang = langMap[lang] ?? lang;
  const supportedLangs = highlighter.getLoadedLanguages();
  const finalLang = supportedLangs.includes(resolvedLang as any) ? resolvedLang : "text";

  return highlighter.codeToHtml(code, {
    lang: finalLang,
    theme: "github-dark",
  });
}
