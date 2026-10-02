/**
 * Main App Component
 * Kite Electric Blue Theme
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { GraphCanvas } from "./components/graph-canvas.tsx";
import { Header } from "./components/header.tsx";
import { Sidebar } from "./components/sidebar.tsx";
import { StatusBar } from "./components/status-bar.tsx";
import { Toolbar } from "./components/toolbar.tsx";
import { useGraphData } from "./hooks/use-graph-data.ts";
import * as api from "./lib/api.ts";
import type { ToolMode, VisNode } from "./lib/types.ts";
import { COLORS } from "./lib/types.ts";

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

	// Request tokens: bumped on every path/impact request and on every reset that invalidates one,
	// so a response that resolves after a newer request (or a reset) is recognized as stale.
	const pathRequestRef = useRef(0);
	const impactRequestRef = useRef(0);

	// Bumped whenever the user opens or closes a database, so the initial status check can tell
	// that its answer is out of date.
	const dbGenerationRef = useRef(0);

	// Check initial connection status
	useEffect(() => {
		const generation = dbGenerationRef.current;
		api
			.getStatus()
			.then((status) => {
				if (generation !== dbGenerationRef.current) return;
				setConnected(status.connected);
				setDbPath(status.path || null);
				setIsDemo(status.isDemo || false);
			})
			.catch(() => {
				// Server unreachable: stay disconnected.
				if (generation !== dbGenerationRef.current) return;
				setConnected(false);
			});
	}, []);

	const resetPath = useCallback((start: VisNode | null) => {
		pathRequestRef.current++;
		setPathStart(start);
		setPathEnd(null);
		setPathNodes(new Set());
		setPathSequence([]);
	}, []);

	const resetImpact = useCallback((source: VisNode | null) => {
		impactRequestRef.current++;
		setImpactSource(source);
		setImpactedNodes(new Set());
	}, []);

	/**
	 * Show the database the user just opened (`null` after a close). The previous database's
	 * selection, highlights and pending requests don't apply to it.
	 */
	const switchDatabase = useCallback(
		(path: string | null, demo: boolean) => {
			dbGenerationRef.current++;
			setConnected(path !== null);
			setDbPath(path);
			setIsDemo(demo);
			setSelectedNode(null);
			resetPath(null);
			resetImpact(null);
		},
		[resetPath, resetImpact],
	);

	// Handle database open
	const handleOpenDatabase = useCallback(
		async (path: string) => {
			const result = await api.openDatabase(path);
			if (result.success) {
				switchDatabase(path, false);
				refresh();
			}
			return result;
		},
		[refresh, switchDatabase],
	);

	// Handle file upload
	const handleUploadDatabase = useCallback(
		async (file: File) => {
			const result = await api.uploadDatabase(file);
			if (result.success) {
				switchDatabase(file.name, false);
				refresh();
			}
			return result;
		},
		[refresh, switchDatabase],
	);

	// Handle demo creation
	const handleCreateDemo = useCallback(async () => {
		const result = await api.createDemo();
		if (result.success) {
			switchDatabase("demo.kitedb", true);
			refresh();
		}
		return result;
	}, [refresh, switchDatabase]);

	// Handle database close
	const handleCloseDatabase = useCallback(async () => {
		await api.closeDatabase();
		switchDatabase(null, false);
	}, [switchDatabase]);

	// Handle node click based on tool mode
	const handleNodeClick = useCallback(
		async (node: VisNode) => {
			if (toolMode === "select") {
				setSelectedNode(node);
				return;
			}

			if (toolMode === "path") {
				if (pathStart && !pathEnd) {
					const request = ++pathRequestRef.current;
					setPathEnd(node);
					let path: string[] = [];
					try {
						path = (await api.findPath(pathStart.id, node.id)).path ?? [];
					} catch {
						// A failed request clears the highlight, like "no path".
					}
					if (request === pathRequestRef.current) {
						setPathNodes(new Set(path));
						setPathSequence(path);
					}
					return;
				}

				// First click, or a new start after a finished path.
				resetPath(node);
				return;
			}

			if (toolMode === "impact") {
				resetImpact(node);
				const request = impactRequestRef.current;
				let impacted: string[] = [];
				try {
					impacted = (await api.analyzeImpact(node.id)).impacted ?? [];
				} catch {
					// A failed request clears the highlight, like "nothing impacted".
				}
				if (request === impactRequestRef.current) {
					setImpactedNodes(new Set(impacted));
				}
			}
		},
		[toolMode, pathStart, pathEnd, resetPath, resetImpact],
	);

	// Handle tool mode change
	const handleToolModeChange = useCallback(
		(mode: ToolMode) => {
			setToolMode(mode);
			// Clear mode-specific state
			if (mode !== "path") {
				resetPath(null);
			}
			if (mode !== "impact") {
				resetImpact(null);
			}
		},
		[resetPath, resetImpact],
	);

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
		? nodes.filter(
				(n) =>
					n.label.toLowerCase().includes(searchQuery.toLowerCase()) ||
					n.id.toLowerCase().includes(searchQuery.toLowerCase()),
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
