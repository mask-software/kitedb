/**
 * TypeScript Types for the Playground Client
 */

// ============================================================================
// Node and Edge Types for Visualization
// ============================================================================

export interface VisNode {
  id: string;
  label: string;
  type: string;
  color?: string;
  degree: number;
}

export interface VisEdge {
  source: string | VisNode;
  target: string | VisNode;
  type: string;
}

export interface GraphNetwork {
  nodes: VisNode[];
  edges: VisEdge[];
  truncated: boolean;
}

// ============================================================================
// API Response Types
// ============================================================================

export interface StatusResponse {
  connected: boolean;
  path?: string;
  isDemo?: boolean;
  nodeCount?: number;
  edgeCount?: number;
}

export interface StatsResponse {
  nodes: number;
  edges: number;
  snapshotGen: string;
  walSegment: string;
  walBytes: number;
  recommendCompact: boolean;
  error?: string;
}

export interface PathResponse {
  path?: string[];
  edges?: string[];
  error?: string;
}

export interface ImpactResponse {
  impacted?: string[];
  edges?: string[];
  error?: string;
}

export interface ApiResult {
  success: boolean;
  error?: string;
}

// ============================================================================
// UI State Types
// ============================================================================

export type ToolMode = "select" | "path" | "impact";

export interface AppState {
  // Database state
  connected: boolean;
  dbPath: string | null;
  isDemo: boolean;
  nodeCount: number;
  edgeCount: number;
  
  // Graph data
  nodes: VisNode[];
  edges: VisEdge[];
  truncated: boolean;
  loading: boolean;
  error: string | null;
  
  // Selection state
  selectedNode: VisNode | null;
  hoveredNode: VisNode | null;
  
  // Tool mode
  toolMode: ToolMode;
  
  // Path finding
  pathStart: VisNode | null;
  pathEnd: VisNode | null;
  pathNodes: Set<string>;
  pathSequence: string[];
  
  // Impact analysis
  impactSource: VisNode | null;
  impactedNodes: Set<string>;
  
  // Search
  searchQuery: string;
  
  // Viewport
  zoom: number;
  panX: number;
  panY: number;
}

// ============================================================================
// Color Scheme - Ray Electric Blue Theme
// ============================================================================

export const COLORS = {
  // Base colors
  bg: "#0D1117",
  surface: "#161B22",
  surfaceAlt: "#21262D",
  border: "#21262D",
  borderSubtle: "#30363D",
  
  // Accent - Ray Cyan
  accent: "#00E5FF",
  accentGlow: "rgba(0, 229, 255, 0.4)",
  accentBg: "#00E5FF15",
  accentBgHover: "#00E5FF22",
  accentBorder: "#00E5FF33",
  
  // Text
  textMain: "#E6EDF3",
  textMuted: "#8B949E",
  textSubtle: "#6E7681",
  
  // Node types
  file: "#3B82F6",      // blue
  function: "#00E5FF",  // cyan (accent)
  class: "#22C55E",     // green
  module: "#A855F7",    // purple

  // Highlights
  selected: "#00E5FF",
  selectedBg: "#00E5FF08",
  pathStart: "#22C55E",
  pathEnd: "#EF4444",
  pathNode: "#4ADE80",
  impact: "#F59E0B",
  hover: "#ffffff",
  
  // Status
  success: "#22C55E",
  error: "#EF4444",
  warning: "#F59E0B",
} as const;

export function getNodeColor(type: string): string {
  switch (type) {
    case "file": return COLORS.file;
    case "function": return COLORS.function;
    case "class": return COLORS.class;
    case "module": return COLORS.module;
    default: return COLORS.textMuted;
  }
}
