/**
 * Cytoscape.js Theme Configuration
 * Ray Electric Blue Theme
 */

import type cytoscape from "cytoscape";

// ============================================================================
// Color Constants
// ============================================================================

export const CYTOSCAPE_COLORS = {
  // Base
  background: "#0D1117",
  surface: "#161B22",
  border: "rgba(0, 229, 255, 0.2)",

  // Accent
  accent: "#00E5FF",
  accentDim: "rgba(0, 229, 255, 0.5)",

  // Text
  textPrimary: "#E6EDF3",
  textSecondary: "#8B949E",

  // Node types
  file: "#3B82F6", // blue
  function: "#00E5FF", // cyan
  class: "#22C55E", // green
  module: "#A855F7", // purple

  // Highlights
  selected: "#00E5FF",
  hovered: "rgba(0, 229, 255, 0.3)",
  pathHighlight: "#FFD700", // gold
  pathStart: "#22C55E", // green
  pathEnd: "#EF4444", // red
  impactSource: "#F59E0B", // amber
  impactHighlight: "rgba(245, 158, 11, 0.6)", // amber with transparency

  // Edges
  edgeDefault: "rgba(139, 148, 158, 0.6)",
  edgeHighlight: "#00E5FF",
} as const;

// ============================================================================
// Cytoscape Stylesheet
// ============================================================================

export const cytoscapeStylesheet: cytoscape.StylesheetStyle[] = [
  // Base node style - default for unknown types
  {
    selector: "node",
    style: {
      "background-color": CYTOSCAPE_COLORS.textSecondary,
      "background-opacity": 0.15,
      label: "data(label)",
      color: CYTOSCAPE_COLORS.textPrimary,
      "text-valign": "bottom",
      "text-halign": "center",
      "text-margin-y": 8,
      "font-size": 11,
      "font-family": "-apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif",
      "min-zoomed-font-size": 8,
      width: "mapData(degree, 0, 20, 24, 48)",
      height: "mapData(degree, 0, 20, 24, 48)",
      "border-width": 2,
      "border-color": CYTOSCAPE_COLORS.textSecondary,
      "overlay-opacity": 0,
      "text-wrap": "ellipsis",
      "text-max-width": "80px",
    },
  },

  // Node types - border color with dark tinted background
  {
    selector: 'node[type="file"]',
    style: {
      "background-color": CYTOSCAPE_COLORS.file,
      "background-opacity": 0.15,
      "border-color": CYTOSCAPE_COLORS.file,
      "border-width": 2,
    },
  },
  {
    selector: 'node[type="function"]',
    style: {
      "background-color": CYTOSCAPE_COLORS.function,
      "background-opacity": 0.15,
      "border-color": CYTOSCAPE_COLORS.function,
      "border-width": 2,
    },
  },
  {
    selector: 'node[type="class"]',
    style: {
      "background-color": CYTOSCAPE_COLORS.class,
      "background-opacity": 0.15,
      "border-color": CYTOSCAPE_COLORS.class,
      "border-width": 2,
    },
  },
  {
    selector: 'node[type="module"]',
    style: {
      "background-color": CYTOSCAPE_COLORS.module,
      "background-opacity": 0.15,
      "border-color": CYTOSCAPE_COLORS.module,
      "border-width": 2,
    },
  },

  // Base edge style
  {
    selector: "edge",
    style: {
      width: 1,
      "line-color": CYTOSCAPE_COLORS.edgeDefault,
      "target-arrow-color": CYTOSCAPE_COLORS.edgeDefault,
      "target-arrow-shape": "triangle",
      "curve-style": "bezier",
      "arrow-scale": 0.7,
      opacity: 0.4,
    },
  },

  // Hover state - keep border color, increase width and background opacity
  {
    selector: "node.hovered",
    style: {
      "border-width": 3,
      "background-opacity": 0.4,
      "z-index": 100,
    },
  },

  // Selection state - orange glow to make it obvious
  {
    selector: "node:selected",
    style: {
      "border-width": 3,
      "border-color": "#F59E0B",
      "background-opacity": 0.5,
      "z-index": 100,
      "underlay-color": "#F59E0B",
      "underlay-padding": 8,
      "underlay-opacity": 0.3,
    },
  },

  // Search match - prominent highlight
  {
    selector: "node.search-match",
    style: {
      "border-width": 3,
      "border-color": CYTOSCAPE_COLORS.selected,
      "background-opacity": 0.5,
      "z-index": 100,
      "underlay-color": CYTOSCAPE_COLORS.selected,
      "underlay-padding": 6,
      "underlay-opacity": 0.25,
    },
  },

  // Dimmed when searching
  {
    selector: "node.search-dimmed",
    style: {
      opacity: 0.2,
    },
  },
  {
    selector: "edge.search-dimmed",
    style: {
      opacity: 0.1,
    },
  },

  // Path start node
  {
    selector: "node.path-start",
    style: {
      "background-color": CYTOSCAPE_COLORS.pathStart,
      "border-width": 3,
      "border-color": CYTOSCAPE_COLORS.pathStart,
      width: "mapData(degree, 0, 20, 28, 52)",
      height: "mapData(degree, 0, 20, 28, 52)",
      "z-index": 100,
    },
  },

  // Path end node
  {
    selector: "node.path-end",
    style: {
      "background-color": CYTOSCAPE_COLORS.pathEnd,
      "border-width": 3,
      "border-color": CYTOSCAPE_COLORS.pathEnd,
      width: "mapData(degree, 0, 20, 28, 52)",
      height: "mapData(degree, 0, 20, 28, 52)",
      "z-index": 100,
    },
  },

  // Path highlighting (nodes in path)
  {
    selector: "node.path-node",
    style: {
      "background-color": "#4ADE80",
      "border-width": 2,
      "border-color": "#4ADE80",
      "z-index": 50,
    },
  },

  // Path edges
  {
    selector: "edge.path-edge",
    style: {
      "line-color": `${CYTOSCAPE_COLORS.accent}99`,
      "target-arrow-color": `${CYTOSCAPE_COLORS.accent}99`,
      width: 2,
      opacity: 1,
      "z-index": 50,
    },
  },

  // Impact source node
  {
    selector: "node.impact-source",
    style: {
      "background-color": CYTOSCAPE_COLORS.impactSource,
      "border-width": 3,
      "border-color": CYTOSCAPE_COLORS.impactSource,
      width: "mapData(degree, 0, 20, 28, 52)",
      height: "mapData(degree, 0, 20, 28, 52)",
      "z-index": 100,
    },
  },

  // Impact highlighting
  {
    selector: "node.impacted",
    style: {
      "background-color": CYTOSCAPE_COLORS.impactHighlight,
      "border-width": 2,
      "border-color": CYTOSCAPE_COLORS.impactSource,
      "z-index": 50,
    },
  },

  // Dimmed nodes (when path or impact is active)
  {
    selector: "node.dimmed",
    style: {
      opacity: 0.3,
    },
  },

  // Dimmed edges
  {
    selector: "edge.dimmed",
    style: {
      opacity: 0.15,
    },
  },

  // Hidden labels
  {
    selector: "node.hide-label",
    style: {
      label: "",
    },
  },
];

// ============================================================================
// fCoSE Layout Configuration
// ============================================================================

export const fcoseLayoutOptions = {
  name: "fcose",
  quality: "proof" as const,
  animate: true,
  animationDuration: 500,
  animationEasing: "ease-out-cubic" as const,
  fit: true,
  padding: 50,
  nodeDimensionsIncludeLabels: false,
  
  // Force-directed parameters
  idealEdgeLength: 100,
  nodeRepulsion: 4500,
  edgeElasticity: 0.45,
  nestingFactor: 0.1,
  gravity: 0.25,
  gravityRange: 3.8,
  
  // Incremental layout
  randomize: true,
  
  // Prevent nodes from overlapping
  nodeOverlap: 20,
  
  // Number of iterations
  numIter: 2500,
  
  // Tile disconnected components
  tile: true,
  tilingPaddingVertical: 10,
  tilingPaddingHorizontal: 10,
};

// ============================================================================
// Helper Functions
// ============================================================================

export function getNodeColorByType(type: string): string {
  switch (type) {
    case "file":
      return CYTOSCAPE_COLORS.file;
    case "function":
      return CYTOSCAPE_COLORS.function;
    case "class":
      return CYTOSCAPE_COLORS.class;
    case "module":
      return CYTOSCAPE_COLORS.module;
    default:
      return CYTOSCAPE_COLORS.textSecondary;
  }
}
