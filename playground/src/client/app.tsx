/**
 * Main App Component
 * Ray Electric Blue Theme
 */

import { useState, useCallback, useEffect, useRef } from "react";
import type { VisNode, ToolMode } from "./lib/types.ts";
import { COLORS } from "./lib/types.ts";
import * as api from "./lib/api.ts";
import { Header } from "./components/header.tsx";
import { Toolbar } from "./components/toolbar.tsx";
import { Sidebar } from "./components/sidebar.tsx";
import { StatusBar } from "./components/status-bar.tsx";
import { GraphCanvas } from "./components/graph-canvas.tsx";
import { useGraphData } from "./hooks/use-graph-data.ts";

const styles = {
  app: {
    display: "flex",
    flexDirection: "column" as const,
    width: "100%",
    height: "100%",
    background: "transparent",
    color: COLORS.textMain,
    fontFamily: "'Space Grotesk', 'Trebuchet MS', sans-serif",
    overflow: "hidden",
    position: "relative" as const,
  },
  main: {
    display: "flex",
    flex: 1,
    overflow: "hidden",
    position: "relative" as const,
  },
  canvasContainer: {
    flex: 1,
    position: "relative" as const,
    overflow: "hidden",
  },
};

export function App() {
  // Database state
  const [connected, setConnected] = useState(false);
  const [dbPath, setDbPath] = useState<string | null>(null);
  const [isDemo, setIsDemo] = useState(false);

  // Graph data
  const {
    nodes,
    edges,
    truncated,
    loading,
    error,
    nodeCount,
    edgeCount,
    refresh,
  } = useGraphData(connected);

  // Selection state
  const [selectedNode, setSelectedNode] = useState<VisNode | null>(null);
  const [hoveredNode, setHoveredNode] = useState<VisNode | null>(null);

  // Tool mode
  const [toolMode, setToolMode] = useState<ToolMode>("select");

  // Path finding state
  const [pathStart, setPathStart] = useState<VisNode | null>(null);
  const [pathEnd, setPathEnd] = useState<VisNode | null>(null);
  const [pathNodes, setPathNodes] = useState<Set<string>>(new Set());
  const [pathSequence, setPathSequence] = useState<string[]>([]);

  // Impact analysis state
  const [impactSource, setImpactSource] = useState<VisNode | null>(null);
  const [impactedNodes, setImpactedNodes] = useState<Set<string>>(new Set());

  // Search
  const [searchQuery, setSearchQuery] = useState("");

  // Zoom (tracked for status bar display, managed by Cytoscape)
  const [zoom, setZoom] = useState(1);

  // Labels visibility
  const [showLabels, setShowLabels] = useState(true);

  // Refs for Cytoscape methods
  const fitRef = useRef<(() => void) | null>(null);
  const zoomInRef = useRef<(() => void) | null>(null);
  const zoomOutRef = useRef<(() => void) | null>(null);

  // Check initial connection status
  useEffect(() => {
    api.getStatus().then((status) => {
      setConnected(status.connected);
      setDbPath(status.path || null);
      setIsDemo(status.isDemo || false);
    });
  }, []);

  // Handle database open
  const handleOpenDatabase = useCallback(async (path: string) => {
    const result = await api.openDatabase(path);
    if (result.success) {
      setConnected(true);
      setDbPath(path);
      setIsDemo(false);
      refresh();
    }
    return result;
  }, [refresh]);

  // Handle file upload
  const handleUploadDatabase = useCallback(async (file: File) => {
    const result = await api.uploadDatabase(file);
    if (result.success) {
      setConnected(true);
      setDbPath(file.name);
      setIsDemo(false);
      refresh();
    }
    return result;
  }, [refresh]);

  // Handle demo creation
  const handleCreateDemo = useCallback(async () => {
    const result = await api.createDemo();
    if (result.success) {
      setConnected(true);
      setDbPath("demo.raydb");
      setIsDemo(true);
      refresh();
    }
    return result;
  }, [refresh]);

  // Handle database close
  const handleCloseDatabase = useCallback(async () => {
    await api.closeDatabase();
    setConnected(false);
    setDbPath(null);
    setIsDemo(false);
    setSelectedNode(null);
    setPathStart(null);
    setPathEnd(null);
    setPathNodes(new Set());
    setPathSequence([]);
    setImpactSource(null);
    setImpactedNodes(new Set());
  }, []);

  // Handle node click based on tool mode
  const handleNodeClick = useCallback(async (node: VisNode) => {
    if (toolMode === "select") {
      setSelectedNode(node);
      return;
    }

    if (toolMode === "path") {
      if (!pathStart) {
        setPathStart(node);
        setPathEnd(null);
        setPathNodes(new Set());
        setPathSequence([]);
        return;
      }

      if (!pathEnd) {
        setPathEnd(node);
        const result = await api.findPath(pathStart.id, node.id);
        if (result.path && result.path.length > 0) {
          setPathNodes(new Set(result.path));
          setPathSequence(result.path);
        } else {
          setPathNodes(new Set());
          setPathSequence([]);
        }
        return;
      }

      setPathStart(node);
      setPathEnd(null);
      setPathNodes(new Set());
      setPathSequence([]);
      return;
    }

    if (toolMode === "impact") {
      setImpactSource(node);
      const result = await api.analyzeImpact(node.id);
      if (result.impacted) {
        setImpactedNodes(new Set(result.impacted));
      } else {
        setImpactedNodes(new Set());
      }
    }
  }, [toolMode, pathStart, pathEnd]);

  // Handle tool mode change
  const handleToolModeChange = useCallback((mode: ToolMode) => {
    setToolMode(mode);
    // Clear mode-specific state
    if (mode !== "path") {
      setPathStart(null);
      setPathEnd(null);
      setPathNodes(new Set());
      setPathSequence([]);
    }
    if (mode !== "impact") {
      setImpactSource(null);
      setImpactedNodes(new Set());
    }
  }, []);

  // Handle zoom via Cytoscape refs
  const handleZoomIn = useCallback(() => {
    zoomInRef.current?.();
  }, []);

  const handleZoomOut = useCallback(() => {
    zoomOutRef.current?.();
  }, []);

  const handleZoomReset = useCallback(() => {
    fitRef.current?.();
  }, []);

  // Handle label toggle
  const handleToggleLabels = useCallback(() => {
    setShowLabels((prev) => !prev);
  }, []);

  // Filter nodes by search query
  const filteredNodes = searchQuery
    ? nodes.filter((n) =>
        n.label.toLowerCase().includes(searchQuery.toLowerCase()) ||
        n.id.toLowerCase().includes(searchQuery.toLowerCase())
      )
    : nodes;

  // Get highlighted node ids based on search
  const searchHighlightedNodes = searchQuery
    ? new Set(filteredNodes.map((n) => n.id))
    : new Set<string>();

  return (
    <div className="ray-app" style={styles.app}>
      <Header
        connected={connected}
        dbPath={dbPath}
        isDemo={isDemo}
        searchQuery={searchQuery}
        onSearchChange={setSearchQuery}
        toolMode={toolMode}
        pathStart={pathStart}
        pathEnd={pathEnd}
        onOpenDatabase={handleOpenDatabase}
        onUploadDatabase={handleUploadDatabase}
        onCreateDemo={handleCreateDemo}
        onCloseDatabase={handleCloseDatabase}
      />

      <div className="ray-main" style={styles.main}>
        <Toolbar
          toolMode={toolMode}
          onToolModeChange={handleToolModeChange}
          zoom={zoom}
          onZoomIn={handleZoomIn}
          onZoomOut={handleZoomOut}
          onZoomReset={handleZoomReset}
          showLabels={showLabels}
          onToggleLabels={handleToggleLabels}
        />

        <div style={styles.canvasContainer}>
          <GraphCanvas
            nodes={nodes}
            edges={edges}
            toolMode={toolMode}
            selectedNode={selectedNode}
            hoveredNode={hoveredNode}
            pathNodes={pathNodes}
            pathStart={pathStart}
            pathEnd={pathEnd}
            impactSource={impactSource}
            impactedNodes={impactedNodes}
            searchHighlightedNodes={searchHighlightedNodes}
            showLabels={showLabels}
            onNodeClick={handleNodeClick}
            onNodeHover={setHoveredNode}
            onZoomChange={setZoom}
            onFitRef={fitRef}
            onZoomInRef={zoomInRef}
            onZoomOutRef={zoomOutRef}
          />
        </div>

        <Sidebar
          selectedNode={selectedNode}
          hoveredNode={hoveredNode}
          toolMode={toolMode}
          pathStart={pathStart}
          pathEnd={pathEnd}
          pathNodes={pathNodes}
          pathSequence={pathSequence}
          impactSource={impactSource}
          impactedNodes={impactedNodes}
          nodes={nodes}
          edges={edges}
          onNodeClick={handleNodeClick}
        />
      </div>

      <StatusBar
        connected={connected}
        dbPath={dbPath}
        isDemo={isDemo}
        nodeCount={nodeCount}
        edgeCount={edgeCount}
        truncated={truncated}
        zoom={zoom}
        loading={loading}
        error={error}
      />
    </div>
  );
}
