/**
 * Header Component
 *
 * Contains logo, search, path mode inputs, and database controls.
 * Ray Electric Blue Theme.
 */

import { useState, useRef } from "react";
import type { VisNode, ToolMode, ApiResult } from "../lib/types.ts";
import { COLORS } from "../lib/types.ts";

const styles = {
  header: {
    display: "flex",
    alignItems: "center",
    gap: "16px",
    padding: "0 24px",
    height: "64px",
    background: COLORS.bg,
    borderBottom: `1px solid ${COLORS.border}`,
    flexShrink: 0,
  },
  logo: {
    display: "flex",
    alignItems: "center",
    gap: "8px",
    fontWeight: 700,
    fontSize: "20px",
    letterSpacing: "-0.02em",
  },
  logoIcon: {
    padding: "8px",
    background: COLORS.accentBg,
    borderRadius: "12px",
    display: "flex",
    alignItems: "center",
    justifyContent: "center",
    boxShadow: "0 0 15px rgba(0, 229, 255, 0.2)",
  },
  logoRay: {
    color: "#ffffff",
  },
  logoDb: {
    color: COLORS.accent,
  },
  logoBadge: {
    marginLeft: "8px",
    fontSize: "10px",
    fontWeight: 700,
    fontFamily: "monospace",
    textTransform: "uppercase" as const,
    letterSpacing: "0.1em",
    background: COLORS.surfaceAlt,
    color: COLORS.textMuted,
    padding: "4px 8px",
    borderRadius: "4px",
  },
  divider: {
    width: "1px",
    height: "24px",
    background: COLORS.borderSubtle,
  },
  searchContainer: {
    flex: 1,
    maxWidth: "400px",
  },
  searchInput: {
    width: "100%",
    padding: "10px 14px",
    background: COLORS.surface,
    border: `1px solid ${COLORS.border}`,
    borderRadius: "8px",
    color: COLORS.textMain,
    fontSize: "14px",
    outline: "none",
    transition: "border-color 0.15s, box-shadow 0.15s",
  },
  pathInfo: {
    display: "flex",
    alignItems: "center",
    gap: "8px",
    padding: "8px 14px",
    background: COLORS.surface,
    borderRadius: "8px",
    fontSize: "13px",
  },
  pathLabel: {
    color: COLORS.textMuted,
  },
  pathNode: {
    padding: "4px 10px",
    borderRadius: "6px",
    fontSize: "12px",
    fontWeight: 500,
  },
  dbControls: {
    display: "flex",
    alignItems: "center",
    gap: "12px",
    marginLeft: "auto",
  },
  button: {
    padding: "8px 16px",
    background: COLORS.surfaceAlt,
    border: `1px solid ${COLORS.borderSubtle}`,
    borderRadius: "8px",
    color: COLORS.textMain,
    fontSize: "13px",
    fontWeight: 500,
    cursor: "pointer",
    transition: "all 0.15s",
  },
  buttonPrimary: {
    background: COLORS.accent,
    color: "#000000",
    border: "none",
    fontWeight: 600,
    borderRadius: "9999px",
    boxShadow: "0 0 20px rgba(0, 229, 255, 0.2)",
  },
  buttonDanger: {
    borderColor: COLORS.error,
    color: COLORS.error,
  },
  dbPath: {
    fontSize: "12px",
    color: COLORS.textMuted,
    maxWidth: "200px",
    overflow: "hidden",
    textOverflow: "ellipsis",
    whiteSpace: "nowrap" as const,
  },
  modal: {
    position: "fixed" as const,
    top: 0,
    left: 0,
    right: 0,
    bottom: 0,
    background: "rgba(0, 0, 0, 0.7)",
    display: "flex",
    alignItems: "center",
    justifyContent: "center",
    zIndex: 1000,
  },
  modalContent: {
    background: COLORS.bg,
    padding: "24px",
    borderRadius: "16px",
    minWidth: "400px",
    border: `1px solid ${COLORS.border}`,
    boxShadow: "0 25px 50px -12px rgba(0, 0, 0, 0.5)",
  },
  modalTitle: {
    fontSize: "18px",
    fontWeight: 600,
    marginBottom: "16px",
    color: COLORS.textMain,
  },
  modalInput: {
    width: "100%",
    padding: "12px 14px",
    background: COLORS.surface,
    border: `1px solid ${COLORS.border}`,
    borderRadius: "8px",
    color: COLORS.textMain,
    fontSize: "14px",
    marginBottom: "16px",
    outline: "none",
    transition: "border-color 0.15s",
  },
  modalButtons: {
    display: "flex",
    gap: "8px",
    justifyContent: "flex-end",
  },
  errorText: {
    color: COLORS.error,
    fontSize: "13px",
    marginBottom: "12px",
  },
  fileUpload: {
    marginBottom: "16px",
  },
  fileLabel: {
    display: "block",
    padding: "16px",
    background: COLORS.surface,
    border: `2px dashed ${COLORS.borderSubtle}`,
    borderRadius: "8px",
    textAlign: "center" as const,
    cursor: "pointer",
    fontSize: "13px",
    color: COLORS.textMuted,
    transition: "border-color 0.15s, background 0.15s",
  },
  hiddenInput: {
    display: "none",
  },
  demoTag: {
    fontSize: "10px",
    padding: "4px 8px",
    background: COLORS.accentBg,
    color: COLORS.accent,
    borderRadius: "4px",
    fontWeight: 700,
    border: `1px solid ${COLORS.accentBorder}`,
  },
};

interface HeaderProps {
  connected: boolean;
  dbPath: string | null;
  isDemo: boolean;
  searchQuery: string;
  onSearchChange: (query: string) => void;
  toolMode: ToolMode;
  pathStart: VisNode | null;
  pathEnd: VisNode | null;
  onOpenDatabase: (path: string) => Promise<ApiResult>;
  onUploadDatabase: (file: File) => Promise<ApiResult>;
  onCreateDemo: () => Promise<ApiResult>;
  onCloseDatabase: () => Promise<void>;
}

export function Header({
  connected,
  dbPath,
  isDemo,
  searchQuery,
  onSearchChange,
  toolMode,
  pathStart,
  pathEnd,
  onOpenDatabase,
  onUploadDatabase,
  onCreateDemo,
  onCloseDatabase,
}: HeaderProps) {
  const [showOpenModal, setShowOpenModal] = useState(false);
  const [openPath, setOpenPath] = useState("");
  const [openError, setOpenError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const fileInputRef = useRef<HTMLInputElement>(null);

  const handleOpen = async () => {
    if (!openPath.trim()) return;
    setLoading(true);
    setOpenError(null);
    const result = await onOpenDatabase(openPath.trim());
    setLoading(false);
    if (result.success) {
      setShowOpenModal(false);
      setOpenPath("");
    } else {
      setOpenError(result.error || "Failed to open database");
    }
  };

  const handleFileChange = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) return;
    setLoading(true);
    setOpenError(null);
    const result = await onUploadDatabase(file);
    setLoading(false);
    if (result.success) {
      setShowOpenModal(false);
    } else {
      setOpenError(result.error || "Failed to upload database");
    }
  };

  const handleDemo = async () => {
    setLoading(true);
    setOpenError(null);
    const result = await onCreateDemo();
    setLoading(false);
    if (result.success) {
      setShowOpenModal(false);
    } else {
      setOpenError(result.error || "Failed to create demo");
    }
  };

  return (
    <header style={styles.header}>
      <div style={styles.logo}>
        <div style={styles.logoIcon}>
          <svg width="24" height="24" viewBox="0 0 24 24" fill="none">
            <circle cx="12" cy="12" r="10" stroke={COLORS.accent} strokeWidth="2" />
            <circle cx="12" cy="12" r="4" fill={COLORS.accent} />
          </svg>
        </div>
        <span style={styles.logoRay}>Ray</span>
        <span style={styles.logoDb}>DB</span>
        <span style={styles.logoBadge}>Playground</span>
      </div>

      <div style={styles.divider} />

      <div style={styles.searchContainer}>
        <input
          type="text"
          placeholder="Search nodes..."
          value={searchQuery}
          onChange={(e) => onSearchChange(e.target.value)}
          style={styles.searchInput}
        />
      </div>

      {toolMode === "path" && (
        <div style={styles.pathInfo}>
          <span style={styles.pathLabel}>Path:</span>
          <span
            style={{
              ...styles.pathNode,
              background: pathStart ? COLORS.pathStart : COLORS.bg,
              color: pathStart ? COLORS.bg : COLORS.textMuted,
            }}
          >
            {pathStart?.label || "Select start"}
          </span>
          <span style={styles.pathLabel}>→</span>
          <span
            style={{
              ...styles.pathNode,
              background: pathEnd ? COLORS.pathEnd : COLORS.bg,
              color: pathEnd ? "#fff" : COLORS.textMuted,
            }}
          >
            {pathEnd?.label || "Select end"}
          </span>
        </div>
      )}

      <div style={styles.dbControls}>
        {connected && (
          <>
            <span style={styles.dbPath} title={dbPath || ""}>
              {dbPath}
            </span>
            {isDemo && <span style={styles.demoTag}>DEMO</span>}
            <button
              style={{ ...styles.button, ...styles.buttonDanger }}
              onClick={onCloseDatabase}
            >
              Close
            </button>
          </>
        )}
        <button
          style={{ ...styles.button, ...styles.buttonPrimary }}
          onClick={() => setShowOpenModal(true)}
        >
          {connected ? "Open Another" : "Open Database"}
        </button>
      </div>

      {showOpenModal && (
        <div style={styles.modal} onClick={() => setShowOpenModal(false)}>
          <div style={styles.modalContent} onClick={(e) => e.stopPropagation()}>
            <h2 style={styles.modalTitle}>Open Database</h2>

            {openError && <div style={styles.errorText}>{openError}</div>}

            <input
              type="text"
              placeholder="Enter database path (e.g., /path/to/db.raydb)"
              value={openPath}
              onChange={(e) => setOpenPath(e.target.value)}
              style={styles.modalInput}
              onKeyDown={(e) => e.key === "Enter" && handleOpen()}
            />

            <div style={styles.fileUpload}>
              <label style={styles.fileLabel}>
                Or drag & drop a .raydb file here
                <input
                  ref={fileInputRef}
                  type="file"
                  accept=".raydb"
                  onChange={handleFileChange}
                  style={styles.hiddenInput}
                />
              </label>
            </div>

            <div style={styles.modalButtons}>
              <button
                style={styles.button}
                onClick={() => setShowOpenModal(false)}
                disabled={loading}
              >
                Cancel
              </button>
              <button style={styles.button} onClick={handleDemo} disabled={loading}>
                Load Demo
              </button>
              <button
                style={{ ...styles.button, ...styles.buttonPrimary }}
                onClick={handleOpen}
                disabled={loading || !openPath.trim()}
              >
                {loading ? "Opening..." : "Open"}
              </button>
            </div>
          </div>
        </div>
      )}
    </header>
  );
}
