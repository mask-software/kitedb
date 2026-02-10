// KiteDB Brand Colors from ray-docs
export const theme = {
  // Dark mode background colors
  background: "#05070d",
  foreground: "#f5f9ff",
  card: "#0b1220",
  muted: "#131d2d",
  mutedForeground: "#9aa8ba",
  border: "#1a2a42",
  
  // Neon accent colors
  neon400: "#52c4ff",
  neon500: "#2aa7ff",
  neon600: "#0d8bf5",
  electric: "#00d4ff",
  accent: "#2af2ff",
  accentStrong: "#38f7c9",
  
  // Code syntax colors
  codeKeyword: "#ff79c6",
  codeString: "#50fa7b",
  codeNumber: "#bd93f9",
  codeComment: "#6272a4",
  codeFunction: "#00d4ff",
  codeVariable: "#f8f8f2",
  codeType: "#8be9fd",
  
  // Terminal colors
  terminalRed: "#ff5f57",
  terminalYellow: "#febc2e",
  terminalGreen: "#28c840",
  
  // Fonts
  fontMono: "'JetBrains Mono', 'SF Mono', Consolas, monospace",
  fontSans: "'Space Grotesk', 'Inter', system-ui, sans-serif",
} as const;

// Gradient definitions
export const gradients = {
  neonText: "linear-gradient(120deg, #2af2ff 0%, #38f7c9 45%, #0d8bf5 100%)",
  edgeGradient: "linear-gradient(180deg, #00F0FF 0%, #2563EB 100%)",
  kiteFill: "linear-gradient(180deg, #22D3EE 0%, #1E40AF 100%)",
  glowA: "rgba(42, 242, 255, 0.14)",
  glowB: "rgba(56, 247, 201, 0.12)",
} as const;
