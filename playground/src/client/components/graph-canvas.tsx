/**
 * Graph Canvas Component
 *
 * Renders the graph using Cytoscape.js with fCoSE layout.
 * Ray Electric Blue Theme.
 */

import { useRef } from "react";
import type { VisNode, VisEdge, ToolMode } from "../lib/types.ts";
import { COLORS } from "../lib/types.ts";
import { useCytoscape } from "../hooks/use-cytoscape.ts";

interface GraphCanvasProps {
  nodes: VisNode[];
  edges: VisEdge[];
  toolMode: ToolMode;
  selectedNode: VisNode | null;
  hoveredNode: VisNode | null;
  pathNodes: Set<string>;
  pathStart: VisNode | null;
  pathEnd: VisNode | null;
  impactSource: VisNode | null;
  impactedNodes: Set<string>;
  searchHighlightedNodes: Set<string>;
  showLabels: boolean;
  onNodeClick: (node: VisNode) => void;
  onNodeHover: (node: VisNode | null) => void;
  onZoomChange: (zoom: number) => void;
  onFitRef?: React.MutableRefObject<(() => void) | null>;
  onCenterRef?: React.MutableRefObject<(() => void) | null>;
  onZoomInRef?: React.MutableRefObject<(() => void) | null>;
  onZoomOutRef?: React.MutableRefObject<(() => void) | null>;
}

const styles = {
  container: {
    width: "100%",
    height: "100%",
    position: "relative" as const,
    // Dot grid background pattern
    background: `
      radial-gradient(circle at center, ${COLORS.surface} 1px, transparent 1px)
    `,
    backgroundSize: "32px 32px",
    backgroundColor: COLORS.bg,
  },
  cyContainer: {
    width: "100%",
    height: "100%",
  },

  floatingMenu: {
    position: "absolute" as const,
    bottom: 32,
    left: "50%",
    transform: "translateX(-50%)",
    display: "flex",
    alignItems: "center",
    gap: "8px",
    padding: "8px",
    background: COLORS.surface,
    border: `1px solid ${COLORS.borderSubtle}`,
    borderRadius: "16px",
    boxShadow: "0 8px 32px rgba(0, 0, 0, 0.4)",
    zIndex: 10,
  },
  floatingButton: {
    padding: "12px",
    background: "transparent",
    border: "none",
    borderRadius: "12px",
    color: COLORS.textMuted,
    cursor: "pointer",
    transition: "all 0.15s",
  },
  floatingDivider: {
    width: "1px",
    height: "24px",
    background: COLORS.borderSubtle,
  },
  resetButton: {
    padding: "8px 16px",
    background: COLORS.surfaceAlt,
    border: "none",
    borderRadius: "12px",
    color: COLORS.textMain,
    fontSize: "12px",
    fontWeight: 600,
    cursor: "pointer",
    transition: "all 0.15s",
  },
};

export function GraphCanvas({
  nodes,
  edges,
  toolMode,
  selectedNode,
  hoveredNode,
  pathNodes,
  pathStart,
  pathEnd,
  impactSource,
  impactedNodes,
  searchHighlightedNodes,
  showLabels,
  onNodeClick,
  onNodeHover,
  onZoomChange,
  onFitRef,
  onCenterRef,
  onZoomInRef,
  onZoomOutRef,
}: GraphCanvasProps) {
  const containerRef = useRef<HTMLDivElement>(null);

  const { fit, center, zoomIn, zoomOut } = useCytoscape({
    containerRef,
    nodes,
    edges,
    toolMode,
    selectedNode,
    pathNodes,
    pathStart,
    pathEnd,
    impactSource,
    impactedNodes,
    searchHighlightedNodes,
    showLabels,
    onNodeClick,
    onNodeHover,
    onZoomChange,
  });

  // Expose methods via refs if provided
  if (onFitRef) onFitRef.current = fit;
  if (onCenterRef) onCenterRef.current = center;
  if (onZoomInRef) onZoomInRef.current = zoomIn;
  if (onZoomOutRef) onZoomOutRef.current = zoomOut;

  return (
    <div style={styles.container}>
      <div ref={containerRef} style={styles.cyContainer} />

      {/* Floating Action Menu */}
      <div style={styles.floatingMenu}>
        <button
          style={styles.floatingButton}
          onClick={zoomIn}
          title="Zoom In"
        >
          <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <circle cx="11" cy="11" r="8" />
            <line x1="11" y1="8" x2="11" y2="14" />
            <line x1="8" y1="11" x2="14" y2="11" />
          </svg>
        </button>
        <button
          style={styles.floatingButton}
          onClick={zoomOut}
          title="Zoom Out"
        >
          <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <circle cx="11" cy="11" r="8" />
            <line x1="8" y1="11" x2="14" y2="11" />
          </svg>
        </button>
        <div style={styles.floatingDivider} />
        <button style={styles.resetButton} onClick={fit}>
          Fit View
        </button>
      </div>
    </div>
  );
}
