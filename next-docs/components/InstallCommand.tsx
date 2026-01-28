"use client";

interface InstallCommandProps {
  command: string;
}

export default function InstallCommand({ command }: InstallCommandProps) {
  const handleCopy = () => {
    navigator.clipboard.writeText(command);
  };

  return (
    <div className="group relative inline-flex items-center gap-4 px-6 py-4 bg-[#0b1220] rounded-xl border border-[#1a2a42] shadow-[0_0_30px_rgba(0,0,0,0.3)] speed-card">
      <svg
        className="w-4 h-4 text-slate-500"
        fill="none"
        stroke="currentColor"
        viewBox="0 0 24 24"
        aria-hidden="true"
      >
        <path strokeLinecap="round" strokeLinejoin="round" strokeWidth="2" d="M5 7l5 5-5 5M12 19h7" />
      </svg>
      <code className="text-sm font-mono">
        <span className="text-slate-500">$</span>
        <span className="text-[#00d4ff] ml-2">{command.split(" ")[0]}</span>
        <span className="text-white ml-2">{command.split(" ").slice(1).join(" ")}</span>
      </code>
      <button
        type="button"
        className="p-2 rounded-lg text-slate-500 hover:text-[#00d4ff] hover:bg-white/5 transition-colors duration-150 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[#00d4ff]"
        aria-label="Copy install command"
        onClick={handleCopy}
      >
        <svg className="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24" aria-hidden="true">
          <path
            strokeLinecap="round"
            strokeLinejoin="round"
            strokeWidth="2"
            d="M8 16H6a2 2 0 01-2-2V6a2 2 0 012-2h8a2 2 0 012 2v2m-6 12h8a2 2 0 002-2v-8a2 2 0 00-2-2h-8a2 2 0 00-2 2v8a2 2 0 002 2z"
          />
        </svg>
      </button>
    </div>
  );
}
