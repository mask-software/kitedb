/**
 * Status Bar Component
 *
 * Shows node/edge counts, zoom level, DB status, and warnings.
 * Ray Electric Blue Theme.
 */

import { COLORS } from "../lib/types.ts";

const styles = {
  statusBar: {
    display: "flex",
    alignItems: "center",
    gap: "16px",
    padding: "8px 24px",
    background: "linear-gradient(90deg, rgba(7, 12, 20, 0.98), rgba(10, 16, 26, 0.92))",
    borderTop: `1px solid ${COLORS.border}`,
    fontSize: "12px",
    color: COLORS.textMuted,
    flexShrink: 0,
    backdropFilter: "blur(12px)",
  },
  item: {
    display: "flex",
    alignItems: "center",
    gap: "6px",
  },
  dot: {
    width: "8px",
    height: "8px",
    borderRadius: "50%",
  },
  dotConnected: {
    background: COLORS.success,
    boxShadow: `0 0 8px ${COLORS.success}`,
  },
  dotDisconnected: {
    background: COLORS.error,
  },
  badge: {
    padding: "2px 8px",
    background: COLORS.surfaceAlt,
    borderRadius: "4px",
    fontSize: "11px",
    fontVariantNumeric: "tabular-nums",
  },
  badgeAccent: {
    background: COLORS.accentBg,
    color: COLORS.accent,
  },
  warning: {
    display: "flex",
    alignItems: "center",
    gap: "6px",
    color: COLORS.warning,
    marginLeft: "auto",
  },
  error: {
    display: "flex",
    alignItems: "center",
    gap: "6px",
    color: COLORS.error,
    marginLeft: "auto",
  },
  spacer: {
    flex: 1,
  },
};

interface StatusBarProps {
  connected: boolean;
  dbPath: string | null;
  isDemo: boolean;
  nodeCount: number;
  edgeCount: number;
  truncated: boolean;
  zoom: number;
  loading: boolean;
  error: string | null;
}

export function StatusBar({
  connected,
  dbPath,
  isDemo,
  nodeCount,
  edgeCount,
  truncated,
  zoom,
  loading,
  error,
}: StatusBarProps) {
  return (
    <div className="ray-statusbar" style={styles.statusBar}>
      <div style={styles.item}>
        <div
          style={{
            ...styles.dot,
            ...(connected ? styles.dotConnected : styles.dotDisconnected),
          }}
        />
        {connected ? "Connected" : "Disconnected"}
      </div>

      {connected && (
        <>
          <div style={styles.item}>
            <span style={{ ...styles.badge, ...styles.badgeAccent }}>{nodeCount}</span>
            <span>nodes</span>
          </div>
          <div style={styles.item}>
            <span style={styles.badge}>{edgeCount}</span>
            <span>edges</span>
          </div>
        </>
      )}

      <div style={styles.spacer} />

      {loading && (
        <div style={styles.item}>
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke={COLORS.accent} strokeWidth="2" aria-hidden="true">
            <circle cx="12" cy="12" r="10" strokeOpacity="0.3" />
            <path d="M12 2a10 10 0 0 1 10 10" strokeLinecap="round">
              <animateTransform
                attributeName="transform"
                type="rotate"
                from="0 12 12"
                to="360 12 12"
                dur="1s"
                repeatCount="indefinite"
              />
            </path>
          </svg>
          <span style={{ color: COLORS.accent }}>Loading…</span>
        </div>
      )}

      {error && (
        <div style={styles.error}>
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true">
            <circle cx="12" cy="12" r="10" />
            <line x1="12" y1="8" x2="12" y2="12" />
            <line x1="12" y1="16" x2="12.01" y2="16" />
          </svg>
          {error}
        </div>
      )}

      {truncated && !error && (
        <div style={styles.warning}>
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true">
            <path d="M10.29 3.86L1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z" />
            <line x1="12" y1="9" x2="12" y2="13" />
            <line x1="12" y1="17" x2="12.01" y2="17" />
          </svg>
          Graph truncated to 1000 nodes
        </div>
      )}

      <div style={styles.item}>
        <span style={{ color: COLORS.textSubtle }}>Zoom:</span>
        <span style={{ color: COLORS.textMain, fontWeight: 500, fontVariantNumeric: "tabular-nums" }}>
          {Math.round(zoom * 100)}%
        </span>
      </div>
    </div>
  );
}
