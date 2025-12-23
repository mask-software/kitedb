/**
 * API Routes
 *
 * All API endpoints for the playground server.
 */

import { Elysia, t } from "elysia";
import { getSnapshot } from "../../../src/ray/graph-db/snapshot-helper.ts";
import {
  getDb,
  getStatus,
  openDatabase,
  openFromBuffer,
  createDemo,
  closeDatabase,
  FileNode,
  FunctionNode,
  ClassNode,
  ModuleNode,
  ImportsEdge,
  CallsEdge,
  ContainsEdge,
  ExtendsEdge,
} from "./db.ts";

// ============================================================================
// Constants
// ============================================================================

const MAX_NODES = 1000;
const MAX_FILE_SIZE = 10 * 1024 * 1024; // 10MB

// ============================================================================
// Types
// ============================================================================

interface VisNode {
  id: string;
  label: string;
  type: string;
  color?: string;
  degree: number;
}

interface VisEdge {
  source: string;
  target: string;
  type: string;
}

// ============================================================================
// Color scheme for node types
// ============================================================================

const NODE_COLORS: Record<string, string> = {
  file: "#3B82F6",      // blue
  function: "#e6be8a",  // gold
  class: "#22C55E",     // green
  module: "#A855F7",    // purple
};

// ============================================================================
// Helper to get all node definitions
// ============================================================================

function getNodeDef(type: string) {
  switch (type) {
    case "file": return FileNode;
    case "function": return FunctionNode;
    case "class": return ClassNode;
    case "module": return ModuleNode;
    default: return null;
  }
}

function getEdgeDef(type: string) {
  switch (type) {
    case "imports": return ImportsEdge;
    case "calls": return CallsEdge;
    case "contains": return ContainsEdge;
    case "extends": return ExtendsEdge;
    default: return null;
  }
}

// ============================================================================
// API Routes
// ============================================================================

export const apiRoutes = new Elysia({ prefix: "/api" })
  // --------------------------------------------------------------------------
  // Status
  // --------------------------------------------------------------------------
  .get("/status", async () => {
    return await getStatus();
  })

  // --------------------------------------------------------------------------
  // Database Management
  // --------------------------------------------------------------------------
  .post(
    "/db/open",
    async ({ body }) => {
      return await openDatabase(body.path);
    },
    {
      body: t.Object({
        path: t.String(),
      }),
    }
  )

  .post(
    "/db/upload",
    async ({ body }) => {
      const file = body.file;
      if (!file) {
        return { success: false, error: "No file provided" };
      }

      if (file.size > MAX_FILE_SIZE) {
        return { success: false, error: `File too large. Maximum size is ${MAX_FILE_SIZE / 1024 / 1024}MB` };
      }

      const buffer = new Uint8Array(await file.arrayBuffer());
      return await openFromBuffer(buffer, file.name);
    },
    {
      body: t.Object({
        file: t.File(),
      }),
    }
  )

  .post("/db/demo", async () => {
    return await createDemo();
  })

  .post("/db/close", async () => {
    return await closeDatabase();
  })

  // --------------------------------------------------------------------------
  // Stats
  // --------------------------------------------------------------------------
  .get("/stats", async () => {
    const db = getDb();
    if (!db) {
      return { error: "No database connected" };
    }

    const stats = await db.stats();
    const nodeCount = Number(stats.snapshotNodes) + stats.deltaNodesCreated - stats.deltaNodesDeleted;
    const edgeCount = Number(stats.snapshotEdges) + stats.deltaEdgesAdded - stats.deltaEdgesDeleted;

    return {
      nodes: nodeCount,
      edges: edgeCount,
      snapshotGen: stats.snapshotGen.toString(),
      walSegment: stats.walSegment.toString(),
      walBytes: Number(stats.walBytes),
      recommendCompact: stats.recommendCompact,
    };
  })

  // --------------------------------------------------------------------------
  // Graph Network (for visualization)
  // --------------------------------------------------------------------------
  .get("/graph/network", async () => {
    const db = getDb();
    if (!db) {
      return { nodes: [], edges: [], truncated: false, error: "No database connected" };
    }

    const visNodes: VisNode[] = [];
    const visEdges: VisEdge[] = [];
    const nodeIds = new Map<string, { id: string; key: string }>(); // map key -> {id, key}
    const nodeDegrees = new Map<string, number>();
    let truncated = false;

    // Collect nodes from each type
    const nodeTypes = [
      { def: FileNode, type: "file" },
      { def: FunctionNode, type: "function" },
      { def: ClassNode, type: "class" },
      { def: ModuleNode, type: "module" },
    ];

    // Use the raw database to iterate nodes
    const rawDb = db.$raw;
    const snapshot = getSnapshot(rawDb);
    const delta = rawDb._delta;

    // Get nodes from snapshot
    if (snapshot) {
      const numNodes = Number(snapshot.header.numNodes);
      for (let phys = 0; phys < numNodes && visNodes.length < MAX_NODES; phys++) {
        const nodeId = snapshot.physToNodeId.getUint32(phys * 4, true);
        
        // Skip deleted nodes
        if (delta.deletedNodes.has(nodeId)) continue;

        // Get node key
        const keyStringId = snapshot.nodeKeyString.getUint32(phys * 4, true);
        if (keyStringId === 0) continue;

        const keyStartOffset = keyStringId === 0 ? 0 : snapshot.stringOffsets.getUint32((keyStringId - 1) * 4, true);
        const keyEndOffset = snapshot.stringOffsets.getUint32(keyStringId * 4, true);
        const key = new TextDecoder().decode(snapshot.stringBytes.slice(keyStartOffset, keyEndOffset));

        // Determine type from key prefix
        let type = "unknown";
        let label = key;
        if (key.startsWith("file:")) {
          type = "file";
          label = key.slice(5).split("/").pop() || key;
        } else if (key.startsWith("fn:")) {
          type = "function";
          label = key.slice(3);
        } else if (key.startsWith("class:")) {
          type = "class";
          label = key.slice(6);
        } else if (key.startsWith("module:")) {
          type = "module";
          label = key.slice(7);
        }

        nodeIds.set(key, { id: key, key });
        nodeDegrees.set(key, 0);

        visNodes.push({
          id: key,
          label,
          type,
          color: NODE_COLORS[type] || "#94a3b8",
          degree: 0,
        });
      }
    }

    // Add nodes from delta (created nodes)
    for (const [nodeId, nodeDelta] of delta.createdNodes) {
      if (visNodes.length >= MAX_NODES) {
        truncated = true;
        break;
      }

      const key = nodeDelta.key;
      if (!key) continue;

      // Determine type from key prefix
      let type = "unknown";
      let label = key;
      if (key.startsWith("file:")) {
        type = "file";
        label = key.slice(5).split("/").pop() || key;
      } else if (key.startsWith("fn:")) {
        type = "function";
        label = key.slice(3);
      } else if (key.startsWith("class:")) {
        type = "class";
        label = key.slice(6);
      } else if (key.startsWith("module:")) {
        type = "module";
        label = key.slice(7);
      }

      if (!nodeIds.has(key)) {
        nodeIds.set(key, { id: key, key });
        nodeDegrees.set(key, 0);

        visNodes.push({
          id: key,
          label,
          type,
          color: NODE_COLORS[type] || "#94a3b8",
          degree: 0,
        });
      }
    }

    if (visNodes.length >= MAX_NODES) {
      truncated = true;
    }

    // Build key -> nodeId map for edge lookup
    const keyToNodeId = new Map<string, number>();
    if (snapshot) {
      const numNodes = Number(snapshot.header.numNodes);
      for (let phys = 0; phys < numNodes; phys++) {
        const nodeId = snapshot.physToNodeId.getUint32(phys * 4, true);
        const keyStringId = snapshot.nodeKeyString.getUint32(phys * 4, true);
        if (keyStringId === 0) continue;

        const keyStartOffset = keyStringId === 0 ? 0 : snapshot.stringOffsets.getUint32((keyStringId - 1) * 4, true);
        const keyEndOffset = snapshot.stringOffsets.getUint32(keyStringId * 4, true);
        const key = new TextDecoder().decode(snapshot.stringBytes.slice(keyStartOffset, keyEndOffset));
        keyToNodeId.set(key, nodeId);
      }
    }

    // Add from delta key index
    for (const [key, nodeId] of delta.keyIndex) {
      keyToNodeId.set(key, nodeId);
    }

    // Build nodeId -> key map
    const nodeIdToKey = new Map<number, string>();
    for (const [key, nodeId] of keyToNodeId) {
      nodeIdToKey.set(nodeId, key);
    }

    // Get edge types from snapshot
    const etypeNames = new Map<number, string>();
    if (snapshot) {
      const numEtypes = Number(snapshot.header.numEtypes);
      for (let i = 0; i < numEtypes; i++) {
        const stringId = snapshot.etypeStringIds.getUint32(i * 4, true);
        if (stringId === 0) continue;

        const startOffset = stringId === 0 ? 0 : snapshot.stringOffsets.getUint32((stringId - 1) * 4, true);
        const endOffset = snapshot.stringOffsets.getUint32(stringId * 4, true);
        const name = new TextDecoder().decode(snapshot.stringBytes.slice(startOffset, endOffset));
        etypeNames.set(i, name);
      }
    }

    // Add from delta
    for (const [etypeId, name] of delta.newEtypes) {
      etypeNames.set(etypeId, name);
    }

    // Collect edges from snapshot
    if (snapshot) {
      const numNodes = Number(snapshot.header.numNodes);
      for (let phys = 0; phys < numNodes; phys++) {
        const nodeId = snapshot.physToNodeId.getUint32(phys * 4, true);
        if (delta.deletedNodes.has(nodeId)) continue;

        const srcKey = nodeIdToKey.get(nodeId);
        if (!srcKey || !nodeIds.has(srcKey)) continue;

        const outStart = snapshot.outOffsets.getUint32(phys * 4, true);
        const outEnd = snapshot.outOffsets.getUint32((phys + 1) * 4, true);

        for (let i = outStart; i < outEnd; i++) {
          const dstNodeId = snapshot.outDst.getUint32(i * 4, true);
          const etypeId = snapshot.outEtype.getUint32(i * 4, true);

          const dstKey = nodeIdToKey.get(dstNodeId);
          if (!dstKey || !nodeIds.has(dstKey)) continue;

          // Check if edge was deleted
          const deletedEdges = delta.outDel.get(nodeId);
          if (deletedEdges?.some(e => e.etype === etypeId && e.other === dstNodeId)) continue;

          const edgeType = etypeNames.get(etypeId) || "unknown";

          visEdges.push({
            source: srcKey,
            target: dstKey,
            type: edgeType,
          });

          // Update degrees
          nodeDegrees.set(srcKey, (nodeDegrees.get(srcKey) || 0) + 1);
          nodeDegrees.set(dstKey, (nodeDegrees.get(dstKey) || 0) + 1);
        }
      }
    }

    // Add edges from delta
    for (const [srcNodeId, edges] of delta.outAdd) {
      const srcKey = nodeIdToKey.get(srcNodeId);
      if (!srcKey || !nodeIds.has(srcKey)) continue;

      for (const edge of edges) {
        const dstKey = nodeIdToKey.get(edge.other);
        if (!dstKey || !nodeIds.has(dstKey)) continue;

        const edgeType = etypeNames.get(edge.etype) || "unknown";

        visEdges.push({
          source: srcKey,
          target: dstKey,
          type: edgeType,
        });

        // Update degrees
        nodeDegrees.set(srcKey, (nodeDegrees.get(srcKey) || 0) + 1);
        nodeDegrees.set(dstKey, (nodeDegrees.get(dstKey) || 0) + 1);
      }
    }

    // Update node degrees
    for (const node of visNodes) {
      node.degree = nodeDegrees.get(node.id) || 0;
    }

    return { nodes: visNodes, edges: visEdges, truncated };
  })

  // --------------------------------------------------------------------------
  // Path Finding
  // --------------------------------------------------------------------------
  .post(
    "/graph/path",
    async ({ body }) => {
      const db = getDb();
      if (!db) {
        return { error: "No database connected" };
      }

      const { startKey, endKey } = body;

      // Simple BFS path finding
      const rawDb = db.$raw;
      const snapshot = getSnapshot(rawDb);
      const delta = rawDb._delta;

      // Build adjacency from snapshot and delta
      const keyToNodeId = new Map<string, number>();
      const nodeIdToKey = new Map<number, string>();

      if (snapshot) {
        const numNodes = Number(snapshot.header.numNodes);
        for (let phys = 0; phys < numNodes; phys++) {
          const nodeId = snapshot.physToNodeId.getUint32(phys * 4, true);
          const keyStringId = snapshot.nodeKeyString.getUint32(phys * 4, true);
          if (keyStringId === 0) continue;

          const keyStartOffset = keyStringId === 0 ? 0 : snapshot.stringOffsets.getUint32((keyStringId - 1) * 4, true);
          const keyEndOffset = snapshot.stringOffsets.getUint32(keyStringId * 4, true);
          const key = new TextDecoder().decode(snapshot.stringBytes.slice(keyStartOffset, keyEndOffset));
          keyToNodeId.set(key, nodeId);
          nodeIdToKey.set(nodeId, key);
        }
      }

      for (const [key, nodeId] of delta.keyIndex) {
        keyToNodeId.set(key, nodeId);
        nodeIdToKey.set(nodeId, key);
      }

      const startNodeId = keyToNodeId.get(startKey);
      const endNodeId = keyToNodeId.get(endKey);

      if (startNodeId === undefined || endNodeId === undefined) {
        return { error: "Start or end node not found" };
      }

      // Build nodeId -> phys map
      const nodeIdToPhys = new Map<number, number>();
      if (snapshot) {
        const numNodes = Number(snapshot.header.numNodes);
        for (let phys = 0; phys < numNodes; phys++) {
          const nodeId = snapshot.physToNodeId.getUint32(phys * 4, true);
          nodeIdToPhys.set(nodeId, phys);
        }
      }

      // BFS
      const visited = new Set<number>();
      const parent = new Map<number, { nodeId: number; edgeType: string }>();
      const queue: number[] = [startNodeId];
      visited.add(startNodeId);

      // Get edge type names
      const etypeNames = new Map<number, string>();
      if (snapshot) {
        const numEtypes = Number(snapshot.header.numEtypes);
        for (let i = 0; i < numEtypes; i++) {
          const stringId = snapshot.etypeStringIds.getUint32(i * 4, true);
          if (stringId === 0) continue;

          const startOffset = stringId === 0 ? 0 : snapshot.stringOffsets.getUint32((stringId - 1) * 4, true);
          const endOffset = snapshot.stringOffsets.getUint32(stringId * 4, true);
          const name = new TextDecoder().decode(snapshot.stringBytes.slice(startOffset, endOffset));
          etypeNames.set(i, name);
        }
      }
      for (const [etypeId, name] of delta.newEtypes) {
        etypeNames.set(etypeId, name);
      }

      while (queue.length > 0) {
        const current = queue.shift()!;
        if (current === endNodeId) break;

        // Get outgoing edges from snapshot
        const phys = nodeIdToPhys.get(current);
        if (phys !== undefined && snapshot) {
          const outStart = snapshot.outOffsets.getUint32(phys * 4, true);
          const outEnd = snapshot.outOffsets.getUint32((phys + 1) * 4, true);

          for (let i = outStart; i < outEnd; i++) {
            const dstNodeId = snapshot.outDst.getUint32(i * 4, true);
            const etypeId = snapshot.outEtype.getUint32(i * 4, true);

            if (!visited.has(dstNodeId) && !delta.deletedNodes.has(dstNodeId)) {
              // Check if edge was deleted
              const deletedEdges = delta.outDel.get(current);
              if (deletedEdges?.some(e => e.etype === etypeId && e.other === dstNodeId)) continue;

              visited.add(dstNodeId);
              parent.set(dstNodeId, { nodeId: current, edgeType: etypeNames.get(etypeId) || "unknown" });
              queue.push(dstNodeId);
            }
          }
        }

        // Get outgoing edges from delta
        const deltaEdges = delta.outAdd.get(current);
        if (deltaEdges) {
          for (const edge of deltaEdges) {
            if (!visited.has(edge.other) && !delta.deletedNodes.has(edge.other)) {
              visited.add(edge.other);
              parent.set(edge.other, { nodeId: current, edgeType: etypeNames.get(edge.etype) || "unknown" });
              queue.push(edge.other);
            }
          }
        }
      }

      // Reconstruct path
      if (!visited.has(endNodeId)) {
        return { error: "No path found" };
      }

      const path: string[] = [];
      const edges: string[] = [];
      let current = endNodeId;

      while (current !== startNodeId) {
        const key = nodeIdToKey.get(current);
        if (key) path.unshift(key);

        const p = parent.get(current);
        if (!p) break;

        edges.unshift(p.edgeType);
        current = p.nodeId;
      }

      const startKeyStr = nodeIdToKey.get(startNodeId);
      if (startKeyStr) path.unshift(startKeyStr);

      return { path, edges };
    },
    {
      body: t.Object({
        startKey: t.String(),
        endKey: t.String(),
      }),
    }
  )

  // --------------------------------------------------------------------------
  // Impact Analysis
  // --------------------------------------------------------------------------
  .post(
    "/graph/impact",
    async ({ body }) => {
      const db = getDb();
      if (!db) {
        return { error: "No database connected" };
      }

      const { nodeKey } = body;

      const rawDb = db.$raw;
      const snapshot = getSnapshot(rawDb);
      const delta = rawDb._delta;

      // Build key -> nodeId map
      const keyToNodeId = new Map<string, number>();
      const nodeIdToKey = new Map<number, string>();

      if (snapshot) {
        const numNodes = Number(snapshot.header.numNodes);
        for (let phys = 0; phys < numNodes; phys++) {
          const nodeId = snapshot.physToNodeId.getUint32(phys * 4, true);
          const keyStringId = snapshot.nodeKeyString.getUint32(phys * 4, true);
          if (keyStringId === 0) continue;

          const keyStartOffset = keyStringId === 0 ? 0 : snapshot.stringOffsets.getUint32((keyStringId - 1) * 4, true);
          const keyEndOffset = snapshot.stringOffsets.getUint32(keyStringId * 4, true);
          const key = new TextDecoder().decode(snapshot.stringBytes.slice(keyStartOffset, keyEndOffset));
          keyToNodeId.set(key, nodeId);
          nodeIdToKey.set(nodeId, key);
        }
      }

      for (const [key, nodeId] of delta.keyIndex) {
        keyToNodeId.set(key, nodeId);
        nodeIdToKey.set(nodeId, key);
      }

      const startNodeId = keyToNodeId.get(nodeKey);
      if (startNodeId === undefined) {
        return { error: "Node not found" };
      }

      // Build nodeId -> phys map
      const nodeIdToPhys = new Map<number, number>();
      if (snapshot) {
        const numNodes = Number(snapshot.header.numNodes);
        for (let phys = 0; phys < numNodes; phys++) {
          const nodeId = snapshot.physToNodeId.getUint32(phys * 4, true);
          nodeIdToPhys.set(nodeId, phys);
        }
      }

      // Get edge type names
      const etypeNames = new Map<number, string>();
      if (snapshot) {
        const numEtypes = Number(snapshot.header.numEtypes);
        for (let i = 0; i < numEtypes; i++) {
          const stringId = snapshot.etypeStringIds.getUint32(i * 4, true);
          if (stringId === 0) continue;

          const startOffset = stringId === 0 ? 0 : snapshot.stringOffsets.getUint32((stringId - 1) * 4, true);
          const endOffset = snapshot.stringOffsets.getUint32(stringId * 4, true);
          const name = new TextDecoder().decode(snapshot.stringBytes.slice(startOffset, endOffset));
          etypeNames.set(i, name);
        }
      }
      for (const [etypeId, name] of delta.newEtypes) {
        etypeNames.set(etypeId, name);
      }

      // BFS to find all nodes that depend on this node (incoming edges)
      const impacted = new Set<string>();
      const edgeTypes = new Set<string>();
      const queue: number[] = [startNodeId];
      const visited = new Set<number>();
      visited.add(startNodeId);

      while (queue.length > 0) {
        const current = queue.shift()!;

        // Get incoming edges from snapshot (using in_ arrays if available)
        const phys = nodeIdToPhys.get(current);
        if (phys !== undefined && snapshot && snapshot.inOffsets && snapshot.inSrc && snapshot.inEtype) {
          const inStart = snapshot.inOffsets.getUint32(phys * 4, true);
          const inEnd = snapshot.inOffsets.getUint32((phys + 1) * 4, true);

          for (let i = inStart; i < inEnd; i++) {
            const srcNodeId = snapshot.inSrc.getUint32(i * 4, true);
            const etypeId = snapshot.inEtype.getUint32(i * 4, true);

            if (!visited.has(srcNodeId) && !delta.deletedNodes.has(srcNodeId)) {
              visited.add(srcNodeId);
              const key = nodeIdToKey.get(srcNodeId);
              if (key) {
                impacted.add(key);
                edgeTypes.add(etypeNames.get(etypeId) || "unknown");
              }
              queue.push(srcNodeId);
            }
          }
        }

        // Get incoming edges from delta
        const deltaEdges = delta.inAdd.get(current);
        if (deltaEdges) {
          for (const edge of deltaEdges) {
            if (!visited.has(edge.other) && !delta.deletedNodes.has(edge.other)) {
              visited.add(edge.other);
              const key = nodeIdToKey.get(edge.other);
              if (key) {
                impacted.add(key);
                edgeTypes.add(etypeNames.get(edge.etype) || "unknown");
              }
              queue.push(edge.other);
            }
          }
        }
      }

      return {
        impacted: Array.from(impacted),
        edges: Array.from(edgeTypes),
      };
    },
    {
      body: t.Object({
        nodeKey: t.String(),
      }),
    }
  );
