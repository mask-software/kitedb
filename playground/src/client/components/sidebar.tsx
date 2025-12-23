/**
 * Sidebar Component
 *
 * Shows selected node details, path results, and impact analysis.
 * Ray Electric Blue Theme.
 */

import type { VisNode, VisEdge, ToolMode } from "../lib/types.ts";
import { COLORS, getNodeColor } from "../lib/types.ts";

const styles = {
  sidebar: {
    width: "320px",
    background: COLORS.bg,
    borderLeft: `1px solid ${COLORS.border}`,
    display: "flex",
    flexDirection: "column" as const,
    overflow: "hidden",
  },
  sidebarHeader: {
    padding: "16px",
    borderBottom: `1px solid ${COLORS.border}`,
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
  },
  sidebarTitle: {
    fontSize: "14px",
    fontWeight: 700,
    color: COLORS.textMain,
  },
  sidebarBadge: {
    fontSize: "10px",
    background: COLORS.surfaceAlt,
    color: COLORS.textMuted,
    padding: "4px 8px",
    borderRadius: "4px",
  },
  section: {
    padding: "20px",
    borderBottom: `1px solid ${COLORS.border}`,
  },
  sectionTitle: {
    fontSize: "10px",
    fontWeight: 700,
    textTransform: "uppercase" as const,
    letterSpacing: "0.1em",
    color: COLORS.textMuted,
    marginBottom: "16px",
  },
  nodeCard: {
    padding: "16px",
    background: COLORS.surface,
    borderRadius: "12px",
    border: `1px solid ${COLORS.border}`,
    marginBottom: "8px",
  },
  nodeCardSelected: {
    border: `1px solid ${COLORS.accent}`,
    background: COLORS.selectedBg,
    boxShadow: "0 0 30px rgba(0, 229, 255, 0.1)",
  },
  nodeHeader: {
    display: "flex",
    alignItems: "center",
    gap: "12px",
    marginBottom: "12px",
  },
  nodeIconWrapper: {
    padding: "8px",
    borderRadius: "8px",
    background: COLORS.surfaceAlt,
  },
  nodeIconWrapperAccent: {
    background: COLORS.accentBg,
  },
  nodeType: {
    fontSize: "10px",
    fontFamily: "monospace",
    textTransform: "uppercase" as const,
    letterSpacing: "0.05em",
    color: COLORS.textSubtle,
  },
  nodeTypeAccent: {
    color: COLORS.accent,
  },
  nodeLabel: {
    fontSize: "14px",
    fontWeight: 600,
    color: COLORS.textMain,
  },
  nodeId: {
    fontSize: "11px",
    color: COLORS.textMuted,
    wordBreak: "break-all" as const,
    marginTop: "4px",
    fontFamily: "monospace",
  },
  nodeProp: {
    display: "flex",
    justifyContent: "space-between",
    fontSize: "12px",
    marginTop: "8px",
  },
  nodePropLabel: {
    color: COLORS.textMuted,
  },
  nodePropValue: {
    color: COLORS.textMain,
  },
  list: {
    display: "flex",
    flexDirection: "column" as const,
    gap: "6px",
    maxHeight: "200px",
    overflowY: "auto" as const,
  },
  listItem: {
    display: "flex",
    alignItems: "center",
    gap: "10px",
    padding: "10px 12px",
    background: COLORS.surface,
    borderRadius: "8px",
    border: `1px solid ${COLORS.border}`,
    cursor: "pointer",
    fontSize: "13px",
    transition: "all 0.15s",
  },
  listItemDot: {
    width: "8px",
    height: "8px",
    borderRadius: "50%",
    flexShrink: 0,
  },
  pathList: {
    display: "flex",
    flexDirection: "column" as const,
    gap: "4px",
  },
  pathItem: {
    display: "flex",
    alignItems: "center",
    gap: "10px",
    padding: "8px 12px",
    background: COLORS.surface,
    borderRadius: "8px",
    border: `1px solid ${COLORS.border}`,
    fontSize: "12px",
    cursor: "pointer",
  },
  pathArrow: {
    color: COLORS.textSubtle,
    fontSize: "10px",
  },
  pathEdgeLabel: {
    fontSize: "10px",
    color: COLORS.accent,
    padding: "2px 6px",
    background: COLORS.accentBg,
    borderRadius: "4px",
    marginLeft: "auto",
  },
  emptyState: {
    padding: "32px 24px",
    textAlign: "center" as const,
    color: COLORS.textMuted,
    fontSize: "13px",
  },
  impactCount: {
    fontSize: "32px",
    fontWeight: 700,
    color: COLORS.impact,
    marginBottom: "4px",
  },
  viewButton: {
    width: "100%",
    padding: "12px",
    marginTop: "16px",
    background: COLORS.accentBg,
    border: "none",
    borderRadius: "12px",
    color: COLORS.accent,
    fontSize: "12px",
    fontWeight: 700,
    cursor: "pointer",
    display: "flex",
    alignItems: "center",
    justifyContent: "center",
    gap: "8px",
    transition: "background 0.15s",
  },
};

interface SidebarProps {
  selectedNode: VisNode | null;
  hoveredNode: VisNode | null;
  toolMode: ToolMode;
  pathStart: VisNode | null;
  pathEnd: VisNode | null;
  pathNodes: Set<string>;
  pathSequence: string[];
  impactSource: VisNode | null;
  impactedNodes: Set<string>;
  nodes: VisNode[];
  edges: VisEdge[];
  onNodeClick: (node: VisNode) => void;
}

export function Sidebar({
  selectedNode,
  hoveredNode,
  toolMode,
  pathStart,
  pathEnd,
  pathNodes,
  pathSequence,
  impactSource,
  impactedNodes,
  nodes,
  edges,
  onNodeClick,
}: SidebarProps) {
  // Get connected nodes for selected node
  const getConnectedNodes = (node: VisNode): { outgoing: VisNode[]; incoming: VisNode[] } => {
    const outgoing: VisNode[] = [];
    const incoming: VisNode[] = [];

    for (const edge of edges) {
      const sourceId = typeof edge.source === "string" ? edge.source : edge.source.id;
      const targetId = typeof edge.target === "string" ? edge.target : edge.target.id;

      if (sourceId === node.id) {
        const targetNode = nodes.find((n) => n.id === targetId);
        if (targetNode) outgoing.push(targetNode);
      }
      if (targetId === node.id) {
        const sourceNode = nodes.find((n) => n.id === sourceId);
        if (sourceNode) incoming.push(sourceNode);
      }
    }

    return { outgoing, incoming };
  };

  const displayNode = hoveredNode || selectedNode;
  const connected = displayNode ? getConnectedNodes(displayNode) : null;

  // Get path nodes in order
  const pathNodesList = pathSequence.length > 0
    ? pathSequence.map((id) => nodes.find((n) => n.id === id)).filter(Boolean) as VisNode[]
    : [];

  // Get impacted nodes list
  const impactedNodesList = impactedNodes.size > 0
    ? Array.from(impactedNodes).map((id) => nodes.find((n) => n.id === id)).filter(Boolean) as VisNode[]
    : [];

  return (
    <div style={styles.sidebar}>
      {/* Node Details */}
      <div style={styles.section}>
        <div style={styles.sectionTitle}>
          {hoveredNode ? "Hovered Node" : "Selected Node"}
        </div>
        {displayNode ? (
          <div style={styles.nodeCard}>
            <div style={styles.nodeHeader}>
              <div
                style={{
                  ...styles.nodeType,
                  background: getNodeColor(displayNode.type),
                }}
              />
              <span style={styles.nodeLabel}>{displayNode.label}</span>
            </div>
            <div style={styles.nodeId}>{displayNode.id}</div>
            <div style={styles.nodeProp}>
              <span style={styles.nodePropLabel}>Type</span>
              <span style={styles.nodePropValue}>{displayNode.type}</span>
            </div>
            <div style={styles.nodeProp}>
              <span style={styles.nodePropLabel}>Connections</span>
              <span style={styles.nodePropValue}>{displayNode.degree}</span>
            </div>
          </div>
        ) : (
          <div style={styles.emptyState}>
            Click a node to see details
          </div>
        )}
      </div>

      {/* Connected Nodes */}
      {displayNode && connected && (
        <div style={styles.section}>
          <div style={styles.sectionTitle}>
            Connected ({connected.outgoing.length + connected.incoming.length})
          </div>
          <div style={styles.list}>
            {connected.outgoing.map((node) => (
              <div
                key={`out-${node.id}`}
                style={styles.listItem}
                onClick={() => onNodeClick(node)}
              >
                <div
                  style={{
                    ...styles.listItemDot,
                    background: getNodeColor(node.type),
                  }}
                />
                <span>{node.label}</span>
                <span style={{ marginLeft: "auto", color: COLORS.textMuted, fontSize: "10px" }}>
                  out
                </span>
              </div>
            ))}
            {connected.incoming.map((node) => (
              <div
                key={`in-${node.id}`}
                style={styles.listItem}
                onClick={() => onNodeClick(node)}
              >
                <div
                  style={{
                    ...styles.listItemDot,
                    background: getNodeColor(node.type),
                  }}
                />
                <span>{node.label}</span>
                <span style={{ marginLeft: "auto", color: COLORS.textMuted, fontSize: "10px" }}>
                  in
                </span>
              </div>
            ))}
            {connected.outgoing.length === 0 && connected.incoming.length === 0 && (
              <div style={styles.emptyState}>No connections</div>
            )}
          </div>
        </div>
      )}

      {/* Path Results */}
      {toolMode === "path" && (
        <div style={styles.section}>
          <div style={styles.sectionTitle}>Path Results</div>
          {pathNodesList.length > 0 ? (
            <div style={styles.pathList}>
              {pathNodesList.map((node, i) => (
                <div key={node.id}>
                  <div
                    style={{
                      ...styles.pathItem,
                      background:
                        i === 0
                          ? "rgba(34, 197, 94, 0.1)"
                          : i === pathNodesList.length - 1
                          ? "rgba(239, 68, 68, 0.1)"
                          : COLORS.surface,
                    }}
                    onClick={() => onNodeClick(node)}
                  >
                    <div
                      style={{
                        ...styles.listItemDot,
                        background:
                          i === 0
                            ? COLORS.pathStart
                            : i === pathNodesList.length - 1
                            ? COLORS.pathEnd
                            : COLORS.pathNode,
                      }}
                    />
                    <span>{node.label}</span>
                  </div>
                  {i < pathNodesList.length - 1 && (
                    <div style={{ textAlign: "center", padding: "2px" }}>
                      <span style={styles.pathArrow}>↓</span>
                    </div>
                  )}
                </div>
              ))}
            </div>
          ) : pathStart && !pathEnd ? (
            <div style={styles.emptyState}>
              Select an end node
            </div>
          ) : pathStart && pathEnd ? (
            <div style={styles.emptyState}>
              No path found
            </div>
          ) : (
            <div style={styles.emptyState}>
              Click nodes to find path
            </div>
          )}
        </div>
      )}

      {/* Impact Analysis */}
      {toolMode === "impact" && (
        <div style={styles.section}>
          <div style={styles.sectionTitle}>Impact Analysis</div>
          {impactSource ? (
            <>
              <div style={{ marginBottom: "12px" }}>
                <div style={styles.impactCount}>{impactedNodes.size}</div>
                <div style={{ fontSize: "12px", color: COLORS.textMuted }}>
                  nodes depend on {impactSource.label}
                </div>
              </div>
              <div style={styles.list}>
                {impactedNodesList.map((node) => (
                  <div
                    key={node.id}
                    style={styles.listItem}
                    onClick={() => onNodeClick(node)}
                  >
                    <div
                      style={{
                        ...styles.listItemDot,
                        background: COLORS.impact,
                      }}
                    />
                    <span>{node.label}</span>
                  </div>
                ))}
              </div>
            </>
          ) : (
            <div style={styles.emptyState}>
              Click a node to analyze impact
            </div>
          )}
        </div>
      )}
    </div>
  );
}
