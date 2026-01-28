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
  bg: "#05070D",
  surface: "#0E1624",
  surfaceAlt: "#121E2D",
  border: "#1B2A3C",
  borderSubtle: "#223348",
  
  // Accent - Ray Cyan
  accent: "#2AF2FF",
  accentGlow: "rgba(42, 242, 255, 0.45)",
  accentBg: "rgba(42, 242, 255, 0.12)",
  accentBgHover: "rgba(42, 242, 255, 0.2)",
  accentBorder: "rgba(42, 242, 255, 0.35)",
  
  // Text
  textMain: "#F2F7FF",
  textMuted: "#9AA8BA",
  textSubtle: "#72839A",
  
  // Node types
  file: "#60A5FA",      // blue
  function: "#2AF2FF",  // cyan (accent)
  class: "#34D399",     // green
  module: "#FBBF24",    // amber

  // Highlights
  selected: "#2AF2FF",
  selectedBg: "rgba(42, 242, 255, 0.08)",
  pathStart: "#34D399",
  pathEnd: "#F87171",
  pathNode: "#4ADE80",
  impact: "#F59E0B",
  hover: "#ffffff",
  
  // Status
  success: "#34D399",
  error: "#F87171",
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
