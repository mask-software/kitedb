interface StatCardProps {
  value: string;
  label: string;
}

export function StatCard({ value, label }: StatCardProps) {
  return (
    <article className="group text-center p-8 rounded-2xl bg-[#0b1220]/85 border border-[#1a2a42] hover:border-[#2af2ff]/35 hover:shadow-[0_0_30px_rgba(42,242,255,0.12)] transition-[border-color,box-shadow] duration-300 speed-card">
      <div className="text-4xl md:text-5xl font-black text-gradient tabular-nums leading-none">
        {value}
      </div>
      <div className="mt-3 text-sm font-medium text-slate-400">{label}</div>
    </article>
  );
}

interface StatGridProps {
  children: React.ReactNode;
}

export function StatGrid({ children }: StatGridProps) {
  return (
    <div className="grid grid-cols-2 md:grid-cols-4 gap-4 md:gap-6" role="list" aria-label="Performance statistics">
      {children}
    </div>
  );
}
