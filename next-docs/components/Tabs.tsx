"use client";

import { useId, useState } from "react";
import CodeBlockClient from "./CodeBlockClient";

interface TabItem {
  label: string;
  code: string;
  language?: string;
  filename?: string;
  html?: string | null;
}

interface TabsProps {
  items: TabItem[];
  defaultIndex?: number;
}

export default function Tabs({ items, defaultIndex = 0 }: TabsProps) {
  const [activeIndex, setActiveIndex] = useState(defaultIndex);
  const baseId = useId();

  const handleKeyDown = (event: React.KeyboardEvent, index: number) => {
    if (event.key === "ArrowRight") {
      event.preventDefault();
      const nextIndex = (index + 1) % items.length;
      setActiveIndex(nextIndex);
      document.getElementById(`${baseId}-tab-${nextIndex}`)?.focus();
    } else if (event.key === "ArrowLeft") {
      event.preventDefault();
      const prevIndex = (index - 1 + items.length) % items.length;
      setActiveIndex(prevIndex);
      document.getElementById(`${baseId}-tab-${prevIndex}`)?.focus();
    } else if (event.key === "Home") {
      event.preventDefault();
      setActiveIndex(0);
      document.getElementById(`${baseId}-tab-0`)?.focus();
    } else if (event.key === "End") {
      event.preventDefault();
      const lastIndex = items.length - 1;
      setActiveIndex(lastIndex);
      document.getElementById(`${baseId}-tab-${lastIndex}`)?.focus();
    }
  };

  return (
    <div className="rounded-2xl overflow-hidden border border-[#1a2a42] bg-[#0b1220] shadow-[0_0_40px_rgba(0,0,0,0.3)] speed-card">
      <div className="flex bg-[#0f1a2b] border-b border-[#1a2a42]" role="tablist" aria-label="Code examples">
        {items.map((item, index) => (
          <button
            key={item.label}
            type="button"
            role="tab"
            id={`${baseId}-tab-${index}`}
            aria-selected={activeIndex === index}
            aria-controls={`${baseId}-tabpanel-${index}`}
            tabIndex={activeIndex === index ? 0 : -1}
            className={`px-6 py-3.5 text-sm font-medium transition-colors duration-150 relative ${
              activeIndex === index
                ? "text-[#2af2ff] bg-[#0b1220]"
                : "text-slate-400 hover:text-white hover:bg-[#1a2a42]/40"
            }`}
            onClick={() => setActiveIndex(index)}
            onKeyDown={(event) => handleKeyDown(event, index)}
          >
            {item.label}
            {activeIndex === index && (
              <div
                className="absolute bottom-0 left-0 right-0 h-0.5 bg-[#2af2ff] shadow-[0_0_10px_rgba(42,242,255,0.5)]"
                aria-hidden="true"
              />
            )}
          </button>
        ))}
      </div>

      <div>
        {items.map((item, index) => (
          <div
            key={`${item.label}-panel`}
            role="tabpanel"
            id={`${baseId}-tabpanel-${index}`}
            aria-labelledby={`${baseId}-tab-${index}`}
            aria-hidden={activeIndex !== index}
            style={{ display: activeIndex === index ? "block" : "none" }}
            tabIndex={0}
          >
            <CodeBlockClient
              code={item.code}
              language={item.language}
              filename={item.filename}
              html={item.html}
            />
          </div>
        ))}
      </div>
    </div>
  );
}
