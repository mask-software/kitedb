/**
 * Hook: useCytoscape
 *
 * Manages Cytoscape.js instance lifecycle and provides methods for graph operations.
 */

import { useEffect, useRef, useCallback } from "react";
import type { MutableRefObject } from "react";
import cytoscape from "cytoscape";
import type { Core, ElementDefinition, NodeSingular, EventObject } from "cytoscape";
import fcose from "cytoscape-fcose";
import type { VisNode, VisEdge, ToolMode } from "../lib/types.ts";
import { cytoscapeStylesheet, fcoseLayoutOptions, CYTOSCAPE_COLORS } from "../lib/cytoscape-theme.ts";

// Register fCoSE layout
cytoscape.use(fcose);

// ============================================================================
// Types
// ============================================================================

export interface CytoscapeHandle {
  cyRef: MutableRefObject<Core | null>;
  fit: () => void;
  center: () => void;
  zoomIn: () => void;
  zoomOut: () => void;
  getZoom: () => number;
  runLayout: () => void;
}

export interface UseCytoscapeOptions {
  containerRef: MutableRefObject<HTMLDivElement | null>;
  nodes: VisNode[];
  edges: VisEdge[];
  toolMode: ToolMode;
  selectedNode: VisNode | null;
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
}

// ============================================================================
// Data Conversion
// ============================================================================

function convertToElements(nodes: VisNode[], edges: VisEdge[]): ElementDefinition[] {
  const nodeElements: ElementDefinition[] = nodes.map((node) => ({
    data: {
      id: node.id,
      label: node.label,
      type: node.type,
      degree: node.degree,
    },
    group: "nodes" as const,
  }));

  const edgeElements: ElementDefinition[] = edges.map((edge, index) => {
    const sourceId = typeof edge.source === "string" ? edge.source : edge.source.id;
    const targetId = typeof edge.target === "string" ? edge.target : edge.target.id;
    return {
      data: {
        id: `edge-${index}-${sourceId}-${targetId}`,
        source: sourceId,
        target: targetId,
        type: edge.type,
      },
      group: "edges" as const,
    };
  });

  return [...nodeElements, ...edgeElements];
}

// ============================================================================
// Hook
// ============================================================================

export function useCytoscape(options: UseCytoscapeOptions): CytoscapeHandle {
  const {
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
  } = options;

  const cyRef = useRef<Core | null>(null);
  const prevNodesRef = useRef<string>("");
  const prevEdgesRef = useRef<string>("");
  const layoutRunningRef = useRef(false);
  
  // Store callbacks in refs to avoid re-binding
  const onNodeClickRef = useRef(onNodeClick);
  const onNodeHoverRef = useRef(onNodeHover);
  const onZoomChangeRef = useRef(onZoomChange);

  useEffect(() => {
    onNodeClickRef.current = onNodeClick;
    onNodeHoverRef.current = onNodeHover;
    onZoomChangeRef.current = onZoomChange;
  }, [onNodeClick, onNodeHover, onZoomChange]);

  // Initialize Cytoscape instance
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const cy = cytoscape({
      container,
      style: cytoscapeStylesheet,
      minZoom: 0.1,
      maxZoom: 5,
      wheelSensitivity: 0.3,
      boxSelectionEnabled: false,
      autounselectify: false,
      autoungrabify: false,
      // Remove default styling for better performance
      styleEnabled: true,
    });

    cyRef.current = cy;

    // Event handlers
    const handleNodeTap = (evt: EventObject) => {
      const node = evt.target as NodeSingular;
      const nodeData = node.data();
      const visNode: VisNode = {
        id: nodeData.id,
        label: nodeData.label,
        type: nodeData.type,
        degree: nodeData.degree,
      };
      onNodeClickRef.current(visNode);
    };

    const handleNodeMouseOver = (evt: EventObject) => {
      const node = evt.target as NodeSingular;
      node.addClass("hovered");
      const nodeData = node.data();
      const visNode: VisNode = {
        id: nodeData.id,
        label: nodeData.label,
        type: nodeData.type,
        degree: nodeData.degree,
      };
      onNodeHoverRef.current(visNode);
    };

    const handleNodeMouseOut = (evt: EventObject) => {
      const node = evt.target as NodeSingular;
      node.removeClass("hovered");
      onNodeHoverRef.current(null);
    };

    const handleViewportChange = () => {
      onZoomChangeRef.current(cy.zoom());
    };

    // Bind events
    cy.on("tap", "node", handleNodeTap);
    cy.on("mouseover", "node", handleNodeMouseOver);
    cy.on("mouseout", "node", handleNodeMouseOut);
    cy.on("zoom", handleViewportChange);

    // Set container background
    container.style.backgroundColor = CYTOSCAPE_COLORS.background;

    return () => {
      cy.destroy();
      cyRef.current = null;
    };
  }, []); // Only run once on mount

  // Update elements when nodes/edges change
  useEffect(() => {
    const cy = cyRef.current;
    if (!cy) return;

    // Create a simple hash to detect changes
    const nodesHash = JSON.stringify(nodes.map((n) => n.id).sort());
    const edgesHash = JSON.stringify(
      edges.map((e) => {
        const src = typeof e.source === "string" ? e.source : e.source.id;
        const tgt = typeof e.target === "string" ? e.target : e.target.id;
        return `${src}-${tgt}`;
      }).sort()
    );

    // Skip if no changes
    if (nodesHash === prevNodesRef.current && edgesHash === prevEdgesRef.current) {
      return;
    }

    prevNodesRef.current = nodesHash;
    prevEdgesRef.current = edgesHash;

    // Convert and add elements
    const elements = convertToElements(nodes, edges);

    // Batch update
    cy.batch(() => {
      cy.elements().remove();
      cy.add(elements);
    });

    // Run layout if we have nodes
    if (nodes.length > 0 && !layoutRunningRef.current) {
      layoutRunningRef.current = true;
      const layout = cy.layout(fcoseLayoutOptions);
      layout.on("layoutstop", () => {
        layoutRunningRef.current = false;
      });
      layout.run();
    }
  }, [nodes, edges]);

  // Update grabability based on tool mode
  useEffect(() => {
    const cy = cyRef.current;
    if (!cy) return;

    // Allow dragging only in select mode
    const ungrabify = toolMode !== "select";
    cy.autoungrabify(ungrabify);
  }, [toolMode]);

  // Update visual classes for path/impact highlighting
  useEffect(() => {
    const cy = cyRef.current;
    if (!cy) return;

    cy.batch(() => {
      // Clear all highlight classes
      cy.elements().removeClass("path-start path-end path-node path-edge impact-source impacted dimmed search-match search-dimmed");

      const hasPath = pathNodes.size > 0;
      const hasImpact = impactedNodes.size > 0;
      const hasSearch = searchHighlightedNodes.size > 0;

      // Apply search highlighting
      if (hasSearch) {
        cy.nodes().forEach((node) => {
          if (searchHighlightedNodes.has(node.id())) {
            node.addClass("search-match");
          } else {
            node.addClass("search-dimmed");
          }
        });
        cy.edges().forEach((edge) => {
          const sourceId = edge.source().id();
          const targetId = edge.target().id();
          // Only show edges where at least one endpoint matches
          if (!searchHighlightedNodes.has(sourceId) && !searchHighlightedNodes.has(targetId)) {
            edge.addClass("search-dimmed");
          }
        });
      }

      // Apply path highlighting
      if (pathStart) {
        const startNode = cy.getElementById(pathStart.id);
        if (startNode.length) startNode.addClass("path-start");
      }

      if (pathEnd) {
        const endNode = cy.getElementById(pathEnd.id);
        if (endNode.length) endNode.addClass("path-end");
      }

      if (hasPath) {
        // Dim all nodes first
        cy.nodes().forEach((node) => {
          if (!pathNodes.has(node.id())) {
            node.addClass("dimmed");
          }
        });

        // Highlight path nodes
        cy.nodes().forEach((node) => {
          if (pathNodes.has(node.id()) && node.id() !== pathStart?.id && node.id() !== pathEnd?.id) {
            node.addClass("path-node");
          }
        });

        // Highlight path edges
        cy.edges().forEach((edge) => {
          const sourceId = edge.source().id();
          const targetId = edge.target().id();
          if (pathNodes.has(sourceId) && pathNodes.has(targetId)) {
            edge.addClass("path-edge");
          } else {
            edge.addClass("dimmed");
          }
        });
      }

      // Apply impact highlighting
      if (impactSource) {
        const sourceNode = cy.getElementById(impactSource.id);
        if (sourceNode.length) sourceNode.addClass("impact-source");
      }

      if (hasImpact) {
        // Dim non-impacted nodes
        cy.nodes().forEach((node) => {
          if (!impactedNodes.has(node.id()) && node.id() !== impactSource?.id) {
            node.addClass("dimmed");
          }
        });

        // Highlight impacted nodes
        cy.nodes().forEach((node) => {
          if (impactedNodes.has(node.id()) && node.id() !== impactSource?.id) {
            node.addClass("impacted");
          }
        });

        // Dim edges not connected to impacted nodes
        cy.edges().forEach((edge) => {
          const sourceId = edge.source().id();
          const targetId = edge.target().id();
          const sourceImpacted = impactedNodes.has(sourceId) || sourceId === impactSource?.id;
          const targetImpacted = impactedNodes.has(targetId) || targetId === impactSource?.id;
          if (!sourceImpacted || !targetImpacted) {
            edge.addClass("dimmed");
          }
        });
      }
    });
  }, [pathNodes, pathStart, pathEnd, impactSource, impactedNodes, searchHighlightedNodes]);

  // Handle selection state
  useEffect(() => {
    const cy = cyRef.current;
    if (!cy) return;

    cy.batch(() => {
      cy.nodes().unselect();
      if (selectedNode) {
        const node = cy.getElementById(selectedNode.id);
        if (node.length) node.select();
      }
    });
  }, [selectedNode]);

  // Handle label visibility toggle
  useEffect(() => {
    const cy = cyRef.current;
    if (!cy) return;

    cy.batch(() => {
      if (showLabels) {
        cy.nodes().removeClass("hide-label");
      } else {
        cy.nodes().addClass("hide-label");
      }
    });
  }, [showLabels]);

  // Methods
  const fit = useCallback(() => {
    const cy = cyRef.current;
    if (cy) cy.fit(undefined, 50);
  }, []);

  const center = useCallback(() => {
    const cy = cyRef.current;
    if (cy) cy.center();
  }, []);

  const zoomIn = useCallback(() => {
    const cy = cyRef.current;
    if (cy) {
      const newZoom = Math.min(cy.zoom() * 1.2, 5);
      cy.zoom({
        level: newZoom,
        renderedPosition: { x: cy.width() / 2, y: cy.height() / 2 },
      });
    }
  }, []);

  const zoomOut = useCallback(() => {
    const cy = cyRef.current;
    if (cy) {
      const newZoom = Math.max(cy.zoom() / 1.2, 0.1);
      cy.zoom({
        level: newZoom,
        renderedPosition: { x: cy.width() / 2, y: cy.height() / 2 },
      });
    }
  }, []);

  const getZoom = useCallback(() => {
    const cy = cyRef.current;
    return cy ? cy.zoom() : 1;
  }, []);

  const runLayout = useCallback(() => {
    const cy = cyRef.current;
    if (cy && !layoutRunningRef.current) {
      layoutRunningRef.current = true;
      const layout = cy.layout(fcoseLayoutOptions);
      layout.on("layoutstop", () => {
        layoutRunningRef.current = false;
      });
      layout.run();
    }
  }, []);

  return {
    cyRef,
    fit,
    center,
    zoomIn,
    zoomOut,
    getZoom,
    runLayout,
  };
}
