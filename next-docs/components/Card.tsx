import type { ReactNode } from "react";
import Link from "next/link";

interface CardProps {
  title: string;
  description?: string;
  icon?: ReactNode;
  href?: string;
  className?: string;
  children?: ReactNode;
}

export function Card({ title, description, icon, href, className, children }: CardProps) {
  const cardContent = (
    <>
      {icon && (
        <div className="flex-shrink-0 w-12 h-12 icon-tile rounded-xl bg-[#2af2ff]/10 text-[#2af2ff] group-hover:bg-[#2af2ff]/20 group-hover:scale-110 group-hover:shadow-[0_0_20px_rgba(42,242,255,0.3)] transition-[background-color,transform,box-shadow] duration-200">
          {icon}
        </div>
      )}
      <div className="flex-1 min-w-0">
        <h3 className="font-semibold text-white group-hover:text-[#2af2ff] transition-colors duration-150">
          {title}
        </h3>
        {description && (
          <p className="mt-2 text-sm text-slate-400 leading-relaxed truncate-2">
            {description}
          </p>
        )}
        {children}
      </div>
    </>
  );

  const cardClass = `group relative flex items-start gap-4 p-6 rounded-2xl bg-[#0b1220] border border-[#1a2a42] hover:border-[#2af2ff]/40 hover:shadow-[0_0_40px_rgba(42,242,255,0.12)] transition-[border-color,box-shadow] duration-300 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[#2af2ff] focus-visible:ring-offset-2 focus-visible:ring-offset-[#05070d] speed-card ${
    className ?? ""
  }`;

  if (href) {
    return (
      <Link href={href} className={cardClass}>
        {cardContent}
        <div
          className="absolute inset-x-0 top-0 h-px bg-gradient-to-r from-transparent via-[#2af2ff] to-transparent opacity-0 group-hover:opacity-100 transition-opacity duration-300"
          aria-hidden="true"
        />
      </Link>
    );
  }

  return <article className={cardClass}>{cardContent}</article>;
}

interface CardGridProps {
  columns?: 1 | 2 | 3 | 4;
  children: ReactNode;
}

export function CardGrid({ columns = 2, children }: CardGridProps) {
  const gridCols =
    columns === 1
      ? "grid-cols-1"
      : columns === 2
      ? "grid-cols-1 md:grid-cols-2"
      : columns === 3
      ? "grid-cols-1 md:grid-cols-2 lg:grid-cols-3"
      : "grid-cols-1 sm:grid-cols-2 lg:grid-cols-4";

  return <div className={`grid gap-6 ${gridCols}`}>{children}</div>;
}
