import type { ComponentProps } from "react";

interface LogoProps extends ComponentProps<"svg"> {
  size?: number;
}

export default function Logo({ size = 32, className, ...props }: LogoProps) {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      viewBox="0 0 128 128"
      fill="none"
      className={className}
      width={size}
      height={size}
      aria-hidden="true"
      {...props}
    >
      <defs>
        <linearGradient id="neonGradient" x1="0%" y1="0%" x2="100%" y2="100%">
          <stop offset="0%" stopColor="#00d4ff" />
          <stop offset="50%" stopColor="#2aa7ff" />
          <stop offset="100%" stopColor="#0d8bf5" />
        </linearGradient>
        <filter id="glow" x="-50%" y="-50%" width="200%" height="200%">
          <feGaussianBlur stdDeviation="3" result="coloredBlur" />
          <feMerge>
            <feMergeNode in="coloredBlur" />
            <feMergeNode in="SourceGraphic" />
          </feMerge>
        </filter>
      </defs>
      <circle cx="64" cy="64" r="60" fill="url(#neonGradient)" />
      <g filter="url(#glow)">
        <circle cx="64" cy="40" r="10" fill="white" />
        <circle cx="40" cy="75" r="10" fill="white" />
        <circle cx="88" cy="75" r="10" fill="white" />
        <line x1="64" y1="50" x2="40" y2="65" stroke="white" strokeWidth="3" strokeLinecap="round" />
        <line x1="64" y1="50" x2="88" y2="65" stroke="white" strokeWidth="3" strokeLinecap="round" />
        <line x1="50" y1="75" x2="78" y2="75" stroke="white" strokeWidth="3" strokeLinecap="round" />
      </g>
      <circle cx="64" cy="62" r="6" fill="white" opacity="0.9" />
    </svg>
  );
}
