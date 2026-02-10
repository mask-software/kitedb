import { interpolate, spring, useCurrentFrame, useVideoConfig } from "remotion";
import { theme } from "./theme";

interface KiteLogoProps {
  scale?: number;
  showGlow?: boolean;
  animateIn?: boolean;
  delay?: number;
}

export const KiteLogo: React.FC<KiteLogoProps> = ({
  scale = 1,
  showGlow = true,
  animateIn = true,
  delay = 0,
}) => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();

  // Animation progress
  const progress = animateIn
    ? spring({
        frame: frame - delay,
        fps,
        config: { damping: 15, stiffness: 80 },
      })
    : 1;

  // Animated opacity and scale
  const opacity = interpolate(progress, [0, 1], [0, 1], {
    extrapolateRight: "clamp",
  });
  const scaleAnim = interpolate(progress, [0, 1], [0.8, 1], {
    extrapolateRight: "clamp",
  });

  // Glow pulse animation
  const glowPulse = interpolate(
    Math.sin((frame - delay) * 0.08),
    [-1, 1],
    [0.15, 0.35]
  );

  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      viewBox="0 0 200 240"
      fill="none"
      width={200 * scale}
      height={240 * scale}
      style={{
        opacity,
        transform: `scale(${scaleAnim})`,
      }}
      aria-label="KiteDB Logo"
    >
      {/* Neon Background Glow */}
      {showGlow && (
        <circle
          cx="108"
          cy="115"
          r="70"
          fill="url(#neonGlow)"
          fillOpacity={glowPulse}
        />
      )}

      {/* The Kite Fill */}
      <path
        d="M100 20L175 90L115 210L35 105L100 20Z"
        fill="url(#kiteFill)"
        fillOpacity="0.15"
      />

      {/* Edges */}
      <g
        stroke="url(#edgeGradient)"
        strokeWidth="3"
        strokeLinecap="round"
        strokeLinejoin="round"
      >
        {/* Outer */}
        <path d="M100 20L175 90" />
        <path d="M175 90L115 210" />
        <path d="M115 210L35 105" />
        <path d="M35 105L100 20" />
        {/* Internal Hub */}
        <path d="M100 20L108 108" />
        <path d="M175 90L108 108" />
        <path d="M115 210L108 108" />
        <path d="M35 105L108 108" />
      </g>

      {/* Nodes */}
      <circle cx="100" cy="20" r="5" fill="#06B6D4" stroke="white" strokeWidth="1.5" />
      <circle cx="175" cy="90" r="5" fill="#06B6D4" stroke="white" strokeWidth="1.5" />
      <circle cx="115" cy="210" r="5" fill="#3B82F6" stroke="white" strokeWidth="1.5" />
      <circle cx="35" cy="105" r="5" fill="#06B6D4" stroke="white" strokeWidth="1.5" />

      {/* Center Node */}
      <circle cx="108" cy="108" r="7" fill="white" />
      <circle
        cx="108"
        cy="108"
        r="14"
        stroke="#00F0FF"
        strokeWidth="1.5"
        strokeOpacity="0.6"
        strokeDasharray="4 2"
      />

      <defs>
        <linearGradient
          id="edgeGradient"
          x1="100"
          y1="20"
          x2="115"
          y2="210"
          gradientUnits="userSpaceOnUse"
        >
          <stop stopColor="#00F0FF" />
          <stop offset="1" stopColor="#2563EB" />
        </linearGradient>
        <linearGradient
          id="kiteFill"
          x1="100"
          y1="20"
          x2="115"
          y2="210"
          gradientUnits="userSpaceOnUse"
        >
          <stop stopColor="#22D3EE" />
          <stop offset="1" stopColor="#1E40AF" />
        </linearGradient>
        <radialGradient id="neonGlow">
          <stop offset="0%" stopColor="#00F0FF" />
          <stop offset="100%" stopColor="transparent" />
        </radialGradient>
      </defs>
    </svg>
  );
};
