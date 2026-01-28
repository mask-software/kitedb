/**
 * Toolbar Component
 *
 * Tool mode buttons and zoom controls.
 * Ray Electric Blue Theme.
 */

import type { ToolMode } from "../lib/types.ts";
import { COLORS } from "../lib/types.ts";

const styles = {
  toolbar: {
    display: "flex",
    flexDirection: "column" as const,
    gap: "8px",
    padding: "16px 12px",
    background: "linear-gradient(180deg, rgba(9, 16, 26, 0.98), rgba(12, 20, 32, 0.9))",
    borderRight: `1px solid ${COLORS.border}`,
    width: "68px",
    alignItems: "center",
    position: "relative" as const,
    overflow: "hidden",
    backdropFilter: "blur(12px)",
  },
  section: {
    display: "flex",
    flexDirection: "column" as const,
    gap: "6px",
  },
  divider: {
    width: "24px",
    height: "1px",
    background: COLORS.borderSubtle,
    margin: "8px 0",
  },
  button: {
    width: "40px",
    height: "40px",
    display: "flex",
    alignItems: "center",
    justifyContent: "center",
    background: "rgba(18, 30, 45, 0.6)",
    border: `1px solid transparent`,
    borderRadius: "12px",
    cursor: "pointer",
    color: COLORS.textMuted,
    transition: "background-color 0.2s, border-color 0.2s, color 0.2s, box-shadow 0.2s, transform 0.2s",
    position: "relative" as const,
  },
  buttonActive: {
    background: "rgba(42, 242, 255, 0.18)",
    color: COLORS.accent,
    border: `1px solid ${COLORS.accent}`,
    boxShadow: `0 0 12px ${COLORS.accentGlow}`,
  },
  buttonHover: {
    background: COLORS.surfaceAlt,
    color: COLORS.textMain,
  },
  tooltip: {
    position: "absolute" as const,
    left: "100%",
    marginLeft: "16px",
    padding: "6px 10px",
    background: COLORS.surface,
    border: `1px solid ${COLORS.borderSubtle}`,
    borderRadius: "6px",
    fontSize: "10px",
    color: COLORS.textMain,
    whiteSpace: "nowrap" as const,
    zIndex: 50,
    boxShadow: "0 4px 12px rgba(0, 0, 0, 0.3)",
    opacity: 0,
    pointerEvents: "none" as const,
    transition: "opacity 0.15s",
  },
};

interface ToolbarProps {
  toolMode: ToolMode;
  onToolModeChange: (mode: ToolMode) => void;
  zoom: number;
  onZoomIn: () => void;
  onZoomOut: () => void;
  onZoomReset: () => void;
  showLabels: boolean;
  onToggleLabels: () => void;
}

// Simple SVG icons
const PointerIcon = () => (
  <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true">
    <path d="M3 3l7.07 16.97 2.51-7.39 7.39-2.51L3 3z" />
  </svg>
);

const RouteIcon = () => (
  <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true">
    <circle cx="6" cy="6" r="3" />
    <circle cx="18" cy="18" r="3" />
    <path d="M9 6h6a3 3 0 0 1 3 3v3" />
    <path d="M9 18H6a3 3 0 0 1-3-3v-3" />
  </svg>
);

const ZapIcon = () => (
  <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true">
    <polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2" />
  </svg>
);

const ZoomInIcon = () => (
  <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true">
    <circle cx="11" cy="11" r="8" />
    <line x1="21" y1="21" x2="16.65" y2="16.65" />
    <line x1="11" y1="8" x2="11" y2="14" />
    <line x1="8" y1="11" x2="14" y2="11" />
  </svg>
);

const ZoomOutIcon = () => (
  <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true">
    <circle cx="11" cy="11" r="8" />
    <line x1="21" y1="21" x2="16.65" y2="16.65" />
    <line x1="8" y1="11" x2="14" y2="11" />
  </svg>
);

const RefreshIcon = () => (
  <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true">
    <path d="M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8" />
    <path d="M21 3v5h-5" />
    <path d="M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16" />
    <path d="M8 16H3v5" />
  </svg>
);

const TagIcon = () => (
  <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true">
    <path d="M12 2H2v10l9.29 9.29c.94.94 2.48.94 3.42 0l6.58-6.58c.94-.94.94-2.48 0-3.42L12 2Z" />
    <path d="M7 7h.01" />
  </svg>
);

export function Toolbar({
  toolMode,
  onToolModeChange,
  zoom,
  onZoomIn,
  onZoomOut,
  onZoomReset,
  showLabels,
  onToggleLabels,
}: ToolbarProps) {
  return (
    <div className="ray-toolbar" style={styles.toolbar}>
      <div style={styles.section}>
        <button
          type="button"
          className="ray-icon-button"
          style={{
            ...styles.button,
            ...(toolMode === "select" ? styles.buttonActive : {}),
          }}
          onClick={() => onToolModeChange("select")}
          title="Select (S)"
          aria-label="Select"
        >
          <PointerIcon />
        </button>
        <button
          type="button"
          className="ray-icon-button"
          style={{
            ...styles.button,
            ...(toolMode === "path" ? styles.buttonActive : {}),
          }}
          onClick={() => onToolModeChange("path")}
          title="Path Finding (P)"
          aria-label="Path Finding"
        >
          <RouteIcon />
        </button>
        <button
          type="button"
          className="ray-icon-button"
          style={{
            ...styles.button,
            ...(toolMode === "impact" ? styles.buttonActive : {}),
          }}
          onClick={() => onToolModeChange("impact")}
          title="Impact Analysis (I)"
          aria-label="Impact Analysis"
        >
          <ZapIcon />
        </button>
      </div>

      <div style={styles.divider} />

      <div style={styles.section}>
        <button
          type="button"
          className="ray-icon-button"
          style={styles.button}
          onClick={onZoomIn}
          title="Zoom In (+)"
          aria-label="Zoom in"
        >
          <ZoomInIcon />
        </button>
        <button
          type="button"
          className="ray-icon-button"
          style={styles.button}
          onClick={onZoomOut}
          title="Zoom Out (-)"
          aria-label="Zoom out"
        >
          <ZoomOutIcon />
        </button>
        <button
          type="button"
          className="ray-icon-button"
          style={styles.button}
          onClick={onZoomReset}
          title="Reset Zoom (0)"
          aria-label="Reset zoom"
        >
          <RefreshIcon />
        </button>
      </div>

      <div style={styles.divider} />

      <div style={styles.section}>
        <button
          type="button"
          className="ray-icon-button"
          style={{
            ...styles.button,
            ...(showLabels ? styles.buttonActive : {}),
          }}
          onClick={onToggleLabels}
          title={showLabels ? "Hide Labels (L)" : "Show Labels (L)"}
          aria-label={showLabels ? "Hide labels" : "Show labels"}
        >
          <TagIcon />
        </button>
      </div>
    </div>
  );
}
