"use client";

import { useState } from "react";
import { Check, Copy, FileCode } from "lucide-react";

interface CodeBlockClientProps {
  code: string;
  language?: string;
  filename?: string;
  className?: string;
  html?: string | null;
}

export default function CodeBlockClient({
  code,
  language,
  filename,
  className,
  html,
}: CodeBlockClientProps) {
  const [copied, setCopied] = useState(false);

  const copyToClipboard = async () => {
    try {
      await navigator.clipboard.writeText(code);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 2000);
    } catch (err) {
      console.error("Failed to copy:", err);
    }
  };

  return (
    <div
      className={`group relative rounded-2xl overflow-hidden border border-[#1e3a5f] bg-[#0d1117] shadow-[0_0_40px_rgba(0,0,0,0.3)] ${
        className ?? ""
      }`}
    >
      {(filename || language) && (
        <div className="flex items-center justify-between px-5 py-3 bg-[#161b22] border-b border-[#30363d]">
          <div className="flex items-center gap-3">
            <FileCode size={16} className="text-[#00d4ff]" aria-hidden="true" />
            {filename && <span className="text-sm font-medium text-slate-300">{filename}</span>}
            {!filename && language && (
              <span className="text-xs font-mono text-slate-500 uppercase tracking-wider">
                {language}
              </span>
            )}
          </div>
          <button
            type="button"
            onClick={copyToClipboard}
            className="flex items-center gap-2 px-3 py-1.5 text-xs font-medium rounded-lg text-slate-400 hover:text-[#00d4ff] bg-[#21262d] hover:bg-[#30363d] transition-colors duration-150"
            aria-label={copied ? "Copied!" : "Copy code to clipboard"}
          >
            {copied ? <Check size={14} className="text-emerald-400" aria-hidden="true" /> : <Copy size={14} aria-hidden="true" />}
            <span>{copied ? "Copied!" : "Copy"}</span>
          </button>
        </div>
      )}

      <div className="overflow-x-auto scrollbar-thin">
        {html ? (
          <div
            className="shiki-wrapper [&_pre]:p-6 [&_pre]:text-sm [&_pre]:leading-relaxed [&_pre]:bg-transparent! [&_code]:font-mono"
            dangerouslySetInnerHTML={{ __html: html }}
          />
        ) : (
          <pre className="p-6 text-sm leading-relaxed">
            <code className="font-mono text-slate-200 whitespace-pre">{code}</code>
          </pre>
        )}
      </div>

      {!filename && !language && (
        <button
          type="button"
          onClick={copyToClipboard}
          className="absolute top-4 right-4 p-2.5 rounded-lg text-slate-400 hover:text-[#00d4ff] bg-[#21262d]/80 hover:bg-[#30363d] transition-all duration-150 opacity-0 group-hover:opacity-100 focus:opacity-100"
          aria-label={copied ? "Copied!" : "Copy code to clipboard"}
        >
          {copied ? <Check size={16} className="text-emerald-400" aria-hidden="true" /> : <Copy size={16} aria-hidden="true" />}
        </button>
      )}
    </div>
  );
}
