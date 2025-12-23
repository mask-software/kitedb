/**
 * Hook: useGraphData
 *
 * Fetches graph network data and tracks loading/error states.
 */

import { useState, useEffect, useCallback } from "react";
import type { VisNode, VisEdge } from "../lib/types.ts";
import * as api from "../lib/api.ts";

interface GraphDataState {
  nodes: VisNode[];
  edges: VisEdge[];
  truncated: boolean;
  loading: boolean;
  error: string | null;
  nodeCount: number;
  edgeCount: number;
  refresh: () => void;
}

export function useGraphData(connected: boolean): GraphDataState {
  const [nodes, setNodes] = useState<VisNode[]>([]);
  const [edges, setEdges] = useState<VisEdge[]>([]);
  const [truncated, setTruncated] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [nodeCount, setNodeCount] = useState(0);
  const [edgeCount, setEdgeCount] = useState(0);
  const [refreshToken, setRefreshToken] = useState(0);

  const refresh = useCallback(() => {
    setRefreshToken((token) => token + 1);
  }, []);

  useEffect(() => {
    let cancelled = false;

    async function load() {
      if (!connected) {
        setNodes([]);
        setEdges([]);
        setTruncated(false);
        setNodeCount(0);
        setEdgeCount(0);
        setError(null);
        setLoading(false);
        return;
      }

      setLoading(true);
      setError(null);

      try {
        const [network, status] = await Promise.all([
          api.getGraphNetwork(),
          api.getStatus(),
        ]);

        if (cancelled) return;

        setNodes(network.nodes);
        setEdges(network.edges);
        setTruncated(network.truncated);
        setNodeCount(status.nodeCount ?? network.nodes.length);
        setEdgeCount(status.edgeCount ?? network.edges.length);
        setLoading(false);
      } catch (err) {
        if (cancelled) return;
        setError(err instanceof Error ? err.message : "Failed to load graph data");
        setLoading(false);
      }
    }

    load();

    return () => {
      cancelled = true;
    };
  }, [connected, refreshToken]);

  return {
    nodes,
    edges,
    truncated,
    loading,
    error,
    nodeCount,
    edgeCount,
    refresh,
  };
}
