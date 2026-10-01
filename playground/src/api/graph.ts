/**
 * Graph Index
 *
 * Whole-graph view for the visualization, path and impact endpoints,
 * built only from the public Kite API.
 */

import type { Kite } from "../../../ray-rs/ts/index.ts";

export interface GraphNode {
  id: number;
  key: string;
  type: string;
}

export interface GraphEdge {
  src: number;
  dst: number;
  type: string;
}

export interface GraphIndex {
  /** Keyed nodes of every schema node type, in id order */
  nodes: GraphNode[];
  /** Edges of every schema edge type */
  edges: GraphEdge[];
  idByKey: Map<string, number>;
  keyById: Map<number, string>;
  /** Edge type id -> name, for decoding native traversal results */
  edgeTypeNames: Map<number, string>;
}

export function loadGraph(db: Kite): GraphIndex {
  const nodes: GraphNode[] = [];
  const idByKey = new Map<string, number>();
  const keyById = new Map<number, string>();

  for (const type of db.nodeTypes()) {
    for (const node of db.all(type) as Array<{ id: number; key?: string | null }>) {
      if (!node.key || keyById.has(node.id)) {
        continue;
      }
      nodes.push({ id: node.id, key: node.key, type });
      idByKey.set(node.key, node.id);
      keyById.set(node.id, node.key);
    }
  }
  nodes.sort((left, right) => left.id - right.id);

  // allEdges() without a type reports numeric etype ids that don't follow edgeTypes() order,
  // so list per type name to learn the id -> name mapping.
  const edges: GraphEdge[] = [];
  const edgeTypeNames = new Map<number, string>();
  for (const type of db.edgeTypes().sort()) {
    for (const edge of db.allEdges(type)) {
      edgeTypeNames.set(edge.etype, type);
      edges.push({ src: edge.src, dst: edge.dst, type });
    }
  }

  return { nodes, edges, idByKey, keyById, edgeTypeNames };
}

/** Display label: the key without its type prefix, and only the file name for files. */
export function nodeLabel(node: GraphNode): string {
  const separator = node.key.indexOf(":");
  const name = separator >= 0 ? node.key.slice(separator + 1) : node.key;
  if (node.type === "file") {
    return name.split("/").pop() || node.key;
  }
  return name || node.key;
}
