import { highlightCode } from "../lib/highlighter";
import CodeBlockClient from "./CodeBlockClient";

interface CodeBlockProps {
  code: string;
  language?: string;
  filename?: string;
  className?: string;
}

export default async function CodeBlock({
  code,
  language = "text",
  filename,
  className,
}: CodeBlockProps) {
  const html = await highlightCode(code, language);
  return (
    <CodeBlockClient
      code={code}
      language={language}
      filename={filename}
      className={className}
      html={html}
    />
  );
}
