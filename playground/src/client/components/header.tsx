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
    height: "68px",
    background: "linear-gradient(100deg, rgba(6, 10, 16, 0.98) 0%, rgba(9, 16, 26, 0.9) 60%, rgba(7, 12, 20, 0.98) 100%)",
    borderBottom: `1px solid ${COLORS.border}`,
    flexShrink: 0,
    position: "relative" as const,
    overflow: "hidden",
    backdropFilter: "blur(14px)",
    boxShadow: "0 12px 30px rgba(0, 0, 0, 0.3)",
  },
  logo: {
    display: "flex",
    alignItems: "center",
    gap: "8px",
    fontWeight: 700,
    fontSize: "20px",
    letterSpacing: "-0.03em",
  },
  logoIcon: {
    padding: "9px",
    background: "linear-gradient(135deg, rgba(42, 242, 255, 0.2), rgba(56, 247, 201, 0.18))",
    borderRadius: "14px",
    display: "flex",
    alignItems: "center",
    justifyContent: "center",
    boxShadow: "0 0 18px rgba(42, 242, 255, 0.35)",
    border: `1px solid ${COLORS.accentBorder}`,
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
    fontFamily: "'JetBrains Mono', monospace",
    textTransform: "uppercase" as const,
    letterSpacing: "0.1em",
    background: COLORS.surfaceAlt,
    color: COLORS.textMuted,
    padding: "4px 8px",
    borderRadius: "999px",
    border: `1px solid ${COLORS.borderSubtle}`,
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
    padding: "10px 16px",
    background: "rgba(14, 22, 36, 0.9)",
    border: `1px solid ${COLORS.borderSubtle}`,
    borderRadius: "999px",
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
    background: "linear-gradient(120deg, rgba(14, 22, 36, 0.9), rgba(18, 30, 45, 0.9))",
    borderRadius: "10px",
    fontSize: "13px",
    border: `1px solid ${COLORS.borderSubtle}`,
    backdropFilter: "blur(10px)",
  },
  pathLabel: {
    color: COLORS.textMuted,
  },
  pathNode: {
    padding: "4px 10px",
    borderRadius: "8px",
    fontSize: "12px",
    fontWeight: 500,
    fontFamily: "'JetBrains Mono', monospace",
  },
  dbControls: {
    display: "flex",
    alignItems: "center",
    gap: "12px",
    marginLeft: "auto",
  },
  button: {
    padding: "8px 16px",
    background: "linear-gradient(180deg, rgba(18, 30, 45, 0.9), rgba(12, 20, 32, 0.95))",
    border: `1px solid ${COLORS.borderSubtle}`,
    borderRadius: "10px",
    color: COLORS.textMain,
    fontSize: "13px",
    fontWeight: 500,
    cursor: "pointer",
    transition: "background-color 0.2s, border-color 0.2s, color 0.2s, box-shadow 0.2s, transform 0.2s",
  },
  buttonPrimary: {
    background: "linear-gradient(120deg, #2AF2FF 0%, #38F7C9 100%)",
    color: "#041017",
    border: "1px solid rgba(255, 255, 255, 0.1)",
    fontWeight: 600,
    borderRadius: "9999px",
    boxShadow: "0 12px 24px rgba(42, 242, 255, 0.3), 0 0 18px rgba(56, 247, 201, 0.25)",
  },
  buttonDanger: {
    borderColor: COLORS.error,
    color: COLORS.error,
    background: "rgba(248, 113, 113, 0.12)",
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
    overscrollBehavior: "contain" as const,
  },
  modalContent: {
    background: "linear-gradient(160deg, rgba(10, 16, 26, 0.96), rgba(12, 20, 32, 0.98))",
    padding: "24px",
    borderRadius: "16px",
    minWidth: "400px",
    border: `1px solid ${COLORS.border}`,
    boxShadow: "0 25px 50px -12px rgba(0, 0, 0, 0.5)",
    backdropFilter: "blur(16px)",
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
    background: "rgba(14, 22, 36, 0.95)",
    border: `1px solid ${COLORS.borderSubtle}`,
    borderRadius: "10px",
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
    background: "rgba(14, 22, 36, 0.85)",
    border: `2px dashed ${COLORS.borderSubtle}`,
    borderRadius: "10px",
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
    borderRadius: "999px",
    fontWeight: 700,
    border: `1px solid ${COLORS.accentBorder}`,
    fontFamily: "'JetBrains Mono', monospace",
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
    <header className="ray-header" style={styles.header}>
      <div style={styles.logo}>
        <div style={styles.logoIcon}>
          <svg width="24" height="24" viewBox="0 0 24 24" fill="none" aria-hidden="true">
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
          placeholder="Search nodes…"
          value={searchQuery}
          onChange={(e) => onSearchChange(e.target.value)}
          style={styles.searchInput}
          className="ray-input"
          aria-label="Search nodes"
          name="search"
          autoComplete="off"
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
              type="button"
              className="ray-button"
              style={{ ...styles.button, ...styles.buttonDanger }}
              onClick={onCloseDatabase}
            >
              Close
            </button>
          </>
        )}
        <button
          type="button"
          className="ray-button ray-button--primary"
          style={{ ...styles.button, ...styles.buttonPrimary }}
          onClick={() => setShowOpenModal(true)}
        >
          {connected ? "Open Another" : "Open Database"}
        </button>
      </div>

      {showOpenModal && (
          <div
            style={styles.modal}
            onClick={() => setShowOpenModal(false)}
            role="button"
            tabIndex={0}
            aria-label="Close dialog"
            onKeyDown={(e) => {
              if (e.key === "Escape" || e.key === "Enter") {
                setShowOpenModal(false);
              }
            }}
          >
            <div
              style={styles.modalContent}
              onClick={(e) => e.stopPropagation()}
              role="dialog"
              aria-modal="true"
              aria-labelledby="open-database-title"
            >
              <h2 id="open-database-title" style={styles.modalTitle}>Open Database</h2>

            {openError && <div style={styles.errorText}>{openError}</div>}

              <input
                type="text"
                placeholder="Enter database path (e.g., /path/to/db.raydb)…"
                value={openPath}
                onChange={(e) => setOpenPath(e.target.value)}
                style={styles.modalInput}
                onKeyDown={(e) => e.key === "Enter" && handleOpen()}
                className="ray-input"
                aria-label="Database path"
                name="databasePath"
                autoComplete="off"
                spellCheck={false}
              />

            <div style={styles.fileUpload}>
              <label className="ray-file-drop" style={styles.fileLabel}>
                Or drag & drop a .raydb file here
                <input
                  ref={fileInputRef}
                  type="file"
                  accept=".raydb"
                  onChange={handleFileChange}
                  name="databaseFile"
                  style={styles.hiddenInput}
                />
              </label>
            </div>

            <div style={styles.modalButtons}>
              <button
                type="button"
                className="ray-button"
                style={styles.button}
                onClick={() => setShowOpenModal(false)}
                disabled={loading}
              >
                Cancel
              </button>
              <button
                type="button"
                className="ray-button"
                style={styles.button}
                onClick={handleDemo}
                disabled={loading}
              >
                Load Demo
              </button>
              <button
                type="button"
                className="ray-button ray-button--primary"
                style={{ ...styles.button, ...styles.buttonPrimary }}
                onClick={handleOpen}
                disabled={loading || !openPath.trim()}
              >
                {loading ? "Opening…" : "Open"}
              </button>
            </div>
          </div>
        </div>
      )}
    </header>
  );
}
