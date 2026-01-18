/**
 * Export and Import functionality for RayDB
 * 
 * Supports JSON and JSONL (JSON Lines) formats for:
 * - Full database export/import
 * - Streaming export for large databases
 */

import { writeFile, readFile } from "node:fs/promises";
import type { GraphDB, TxHandle, NodeID, ETypeID, PropKeyID, PropValue, LabelID } from "../types.ts";
import { PropValueTag } from "../types.ts";
import { listNodes, countNodes, getNodeProps } from "../ray/graph-db/nodes.ts";
import { listEdges, countEdges, getEdgeProps } from "../ray/graph-db/edges.ts";
import { beginTx, commit } from "../ray/graph-db/tx.ts";
import { createNode } from "../ray/graph-db/nodes.ts";
import { addEdge } from "../ray/graph-db/edges.ts";
import { definePropkey, defineEtype, defineLabel } from "../ray/graph-db/definitions.ts";
import { getSnapshot } from "../ray/graph-db/snapshot-helper.ts";
import { getNodeKey } from "../ray/key-index.ts";

// ============================================================================
// Types
// ============================================================================

export interface ExportOptions {
  /** Include nodes in export (default: true) */
  includeNodes?: boolean;
  /** Include edges in export (default: true) */
  includeEdges?: boolean;
  /** Include schema definitions (default: true) */
  includeSchema?: boolean;
  /** Pretty print JSON (default: false) */
  pretty?: boolean;
  /** Progress callback */
  onProgress?: (phase: string, current: number, total: number | null) => void;
}

export interface ImportOptions {
  /** Skip nodes that already exist by key (default: true) */
  skipExisting?: boolean;
  /** Batch size for commits (default: 1000) */
  batchSize?: number;
  /** Progress callback */
  onProgress?: (phase: string, current: number, total: number | null) => void;
}

export interface ExportedNode {
  id: number;
  key?: string;
  props: Record<string, ExportedPropValue>;
}

export interface ExportedEdge {
  src: number;
  dst: number;
  etype: number;
  etypeName?: string;
  props: Record<string, ExportedPropValue>;
}

export interface ExportedPropValue {
  type: "string" | "int" | "float" | "bool" | "vector" | "null";
  value: string | number | boolean | number[] | null;
}

export interface ExportedSchema {
  labels: Record<number, string>;
  etypes: Record<number, string>;
  propKeys: Record<number, string>;
}

export interface ExportedDatabase {
  version: 1;
  exportedAt: string;
  schema: ExportedSchema;
  nodes: ExportedNode[];
  edges: ExportedEdge[];
  stats: {
    nodeCount: number;
    edgeCount: number;
  };
}

// ============================================================================
// PropValue Serialization
// ============================================================================

function serializePropValue(value: PropValue): ExportedPropValue {
  switch (value.tag) {
    case PropValueTag.NULL:
      return { type: "null", value: null };
    case PropValueTag.STRING:
      return { type: "string", value: value.value };
    case PropValueTag.I64:
      return { type: "int", value: Number(value.value) };
    case PropValueTag.F64:
      return { type: "float", value: value.value };
    case PropValueTag.BOOL:
      return { type: "bool", value: value.value };
    case PropValueTag.VECTOR_F32:
      return { type: "vector", value: Array.from(value.value) };
    default:
      return { type: "null", value: null };
  }
}

function deserializePropValue(exported: ExportedPropValue): PropValue {
  switch (exported.type) {
    case "null":
      return { tag: PropValueTag.NULL };
    case "string":
      return { tag: PropValueTag.STRING, value: exported.value as string };
    case "int":
      return { tag: PropValueTag.I64, value: BigInt(exported.value as number) };
    case "float":
      return { tag: PropValueTag.F64, value: exported.value as number };
    case "bool":
      return { tag: PropValueTag.BOOL, value: exported.value as boolean };
    case "vector":
      return { 
        tag: PropValueTag.VECTOR_F32, 
        value: new Float32Array(exported.value as number[])
      };
    default:
      return { tag: PropValueTag.NULL };
  }
}

// ============================================================================
// Schema Helpers
// ============================================================================

function buildSchemaFromDb(db: GraphDB): ExportedSchema {
  const schema: ExportedSchema = {
    labels: {},
    etypes: {},
    propKeys: {},
  };
  
  const delta = db._delta;
  
  // Get labels from delta
  for (const [id, name] of delta.newLabels) {
    schema.labels[id] = name;
  }
  
  // Get etypes from delta
  for (const [id, name] of delta.newEtypes) {
    schema.etypes[id] = name;
  }
  
  // Get propkeys from delta
  for (const [id, name] of delta.newPropkeys) {
    schema.propKeys[id] = name;
  }
  
  // Also include from snapshot if exists
  const snapshot = getSnapshot(db);
  if (snapshot) {
    // Snapshot stores schema in different sections, but for simplicity
    // we assume delta has the complete schema after WAL replay
  }
  
  return schema;
}

function getPropKeyName(db: GraphDB, keyId: PropKeyID): string {
  const delta = db._delta;
  return delta.newPropkeys.get(keyId) ?? `prop_${keyId}`;
}

function getEtypeName(db: GraphDB, etypeId: ETypeID): string {
  const delta = db._delta;
  return delta.newEtypes.get(etypeId) ?? `etype_${etypeId}`;
}

// ============================================================================
// Export Functions
// ============================================================================

/**
 * Export database to JSON file
 * 
 * @example
 * ```ts
 * await exportToJSON(db, "./backup.json");
 * await exportToJSON(db, "./backup.json", { pretty: true });
 * ```
 */
export async function exportToJSON(
  handle: GraphDB | TxHandle,
  filePath: string,
  options: ExportOptions = {}
): Promise<{ nodeCount: number; edgeCount: number }> {
  const data = await exportToObject(handle, options);
  
  const json = options.pretty 
    ? JSON.stringify(data, null, 2)
    : JSON.stringify(data);
  
  await writeFile(filePath, json, "utf-8");
  
  return {
    nodeCount: data.stats.nodeCount,
    edgeCount: data.stats.edgeCount,
  };
}

/**
 * Export database to a JavaScript object
 */
export async function exportToObject(
  handle: GraphDB | TxHandle,
  options: ExportOptions = {}
): Promise<ExportedDatabase> {
  const {
    includeNodes = true,
    includeEdges = true,
    includeSchema = true,
    onProgress,
  } = options;
  
  const db = '_db' in handle ? handle._db : handle;
  const snapshot = getSnapshot(db);
  
  // Build schema
  const schema = includeSchema ? buildSchemaFromDb(db) : { labels: {}, etypes: {}, propKeys: {} };
  
  // Export nodes
  const nodes: ExportedNode[] = [];
  if (includeNodes) {
    const totalNodes = countNodes(handle);
    let processed = 0;
    
    for (const nodeId of listNodes(handle)) {
      const props = getNodeProps(handle, nodeId);
      const exportedProps: Record<string, ExportedPropValue> = {};
      
      if (props) {
        for (const [keyId, value] of props) {
          const keyName = getPropKeyName(db, keyId);
          exportedProps[keyName] = serializePropValue(value);
        }
      }
      
      // Get node key
      const key = getNodeKey(snapshot, db._delta, nodeId) ?? undefined;
      
      nodes.push({
        id: nodeId,
        key,
        props: exportedProps,
      });
      
      processed++;
      if (onProgress && processed % 1000 === 0) {
        onProgress("nodes", processed, totalNodes);
      }
    }
    
    if (onProgress) {
      onProgress("nodes", nodes.length, totalNodes);
    }
  }
  
  // Export edges
  const edges: ExportedEdge[] = [];
  if (includeEdges) {
    const totalEdges = countEdges(handle);
    let processed = 0;
    
    for (const edge of listEdges(handle)) {
      const props = getEdgeProps(handle, edge.src, edge.etype, edge.dst);
      const exportedProps: Record<string, ExportedPropValue> = {};
      
      if (props) {
        for (const [keyId, value] of props) {
          const keyName = getPropKeyName(db, keyId);
          exportedProps[keyName] = serializePropValue(value);
        }
      }
      
      const etypeName = getEtypeName(db, edge.etype);
      
      edges.push({
        src: edge.src,
        dst: edge.dst,
        etype: edge.etype,
        etypeName,
        props: exportedProps,
      });
      
      processed++;
      if (onProgress && processed % 1000 === 0) {
        onProgress("edges", processed, totalEdges);
      }
    }
    
    if (onProgress) {
      onProgress("edges", edges.length, totalEdges);
    }
  }
  
  return {
    version: 1,
    exportedAt: new Date().toISOString(),
    schema,
    nodes,
    edges,
    stats: {
      nodeCount: nodes.length,
      edgeCount: edges.length,
    },
  };
}

/**
 * Export to JSONL (JSON Lines) format - streaming export
 */
export async function exportToJSONL(
  handle: GraphDB | TxHandle,
  filePath: string,
  options: ExportOptions = {}
): Promise<{ nodeCount: number; edgeCount: number }> {
  const {
    includeNodes = true,
    includeEdges = true,
    includeSchema = true,
    onProgress,
  } = options;
  
  const db = '_db' in handle ? handle._db : handle;
  const snapshot = getSnapshot(db);
  
  const file = Bun.file(filePath);
  const writer = file.writer();
  
  let nodeCount = 0;
  let edgeCount = 0;
  
  try {
    // Write header
    writer.write(JSON.stringify({
      type: "header",
      version: 1,
      exportedAt: new Date().toISOString(),
    }) + "\n");
    
    // Write schema
    if (includeSchema) {
      const schema = buildSchemaFromDb(db);
      writer.write(JSON.stringify({ type: "schema", data: schema }) + "\n");
    }
    
    // Write nodes
    if (includeNodes) {
      const totalNodes = countNodes(handle);
      
      for (const nodeId of listNodes(handle)) {
        const props = getNodeProps(handle, nodeId);
        const exportedProps: Record<string, ExportedPropValue> = {};
        
        if (props) {
          for (const [keyId, value] of props) {
            const keyName = getPropKeyName(db, keyId);
            exportedProps[keyName] = serializePropValue(value);
          }
        }
        
        const key = getNodeKey(snapshot, db._delta, nodeId) ?? undefined;
        
        const node: ExportedNode = { id: nodeId, key, props: exportedProps };
        writer.write(JSON.stringify({ type: "node", data: node }) + "\n");
        
        nodeCount++;
        if (onProgress && nodeCount % 1000 === 0) {
          onProgress("nodes", nodeCount, totalNodes);
        }
      }
      
      if (onProgress) {
        onProgress("nodes", nodeCount, countNodes(handle));
      }
    }
    
    // Write edges
    if (includeEdges) {
      const totalEdges = countEdges(handle);
      
      for (const edge of listEdges(handle)) {
        const props = getEdgeProps(handle, edge.src, edge.etype, edge.dst);
        const exportedProps: Record<string, ExportedPropValue> = {};
        
        if (props) {
          for (const [keyId, value] of props) {
            const keyName = getPropKeyName(db, keyId);
            exportedProps[keyName] = serializePropValue(value);
          }
        }
        
        const etypeName = getEtypeName(db, edge.etype);
        
        const exportedEdge: ExportedEdge = {
          src: edge.src,
          dst: edge.dst,
          etype: edge.etype,
          etypeName,
          props: exportedProps,
        };
        writer.write(JSON.stringify({ type: "edge", data: exportedEdge }) + "\n");
        
        edgeCount++;
        if (onProgress && edgeCount % 1000 === 0) {
          onProgress("edges", edgeCount, totalEdges);
        }
      }
      
      if (onProgress) {
        onProgress("edges", edgeCount, countEdges(handle));
      }
    }
    
    // Write footer
    writer.write(JSON.stringify({
      type: "footer",
      stats: { nodeCount, edgeCount },
    }) + "\n");
    
  } finally {
    writer.end();
  }
  
  return { nodeCount, edgeCount };
}

// ============================================================================
// Import Functions
// ============================================================================

/**
 * Import database from JSON file
 */
export async function importFromJSON(
  db: GraphDB,
  filePath: string,
  options: ImportOptions = {}
): Promise<{ nodeCount: number; edgeCount: number; skipped: number }> {
  const content = await readFile(filePath, "utf-8");
  const data: ExportedDatabase = JSON.parse(content);
  
  return importFromObject(db, data, options);
}

/**
 * Import database from object
 */
export async function importFromObject(
  db: GraphDB,
  data: ExportedDatabase,
  options: ImportOptions = {}
): Promise<{ nodeCount: number; edgeCount: number; skipped: number }> {
  const {
    batchSize = 1000,
    skipExisting = true,
    onProgress,
  } = options;
  
  // Build name -> id mappings for schema
  const propKeyNameToId = new Map<string, PropKeyID>();
  const etypeNameToId = new Map<string, ETypeID>();
  const labelNameToId = new Map<string, LabelID>();
  
  // First pass: ensure all schema elements exist
  let tx = beginTx(db);
  
  // Define property keys
  for (const [idStr, name] of Object.entries(data.schema.propKeys)) {
    // Check if already defined
    let existingId: PropKeyID | undefined;
    for (const [id, existingName] of db._delta.newPropkeys) {
      if (existingName === name) {
        existingId = id;
        break;
      }
    }
    
    if (existingId !== undefined) {
      propKeyNameToId.set(name, existingId);
    } else {
      const newId = definePropkey(tx, name);
      propKeyNameToId.set(name, newId);
    }
  }
  
  // Define edge types
  for (const [idStr, name] of Object.entries(data.schema.etypes)) {
    let existingId: ETypeID | undefined;
    for (const [id, existingName] of db._delta.newEtypes) {
      if (existingName === name) {
        existingId = id;
        break;
      }
    }
    
    if (existingId !== undefined) {
      etypeNameToId.set(name, existingId);
    } else {
      const newId = defineEtype(tx, name);
      etypeNameToId.set(name, newId);
    }
  }
  
  // Define labels
  for (const [idStr, name] of Object.entries(data.schema.labels)) {
    let existingId: LabelID | undefined;
    for (const [id, existingName] of db._delta.newLabels) {
      if (existingName === name) {
        existingId = id;
        break;
      }
    }
    
    if (existingId === undefined) {
      const newId = defineLabel(tx, name);
      labelNameToId.set(name, newId);
    }
  }
  
  await commit(tx);
  
  // Import nodes
  let nodeCount = 0;
  let skipped = 0;
  const oldIdToNewId = new Map<number, NodeID>();
  
  tx = beginTx(db);
  let batchCount = 0;
  
  for (const node of data.nodes) {
    // Check if node with this key already exists
    if (skipExisting && node.key) {
      const existingId = db._delta.keyIndex.get(node.key);
      if (existingId !== undefined) {
        oldIdToNewId.set(node.id, existingId);
        skipped++;
        continue;
      }
    }
    
    // Convert props
    const props = new Map<PropKeyID, PropValue>();
    for (const [propName, exportedValue] of Object.entries(node.props)) {
      const keyId = propKeyNameToId.get(propName);
      if (keyId !== undefined) {
        props.set(keyId, deserializePropValue(exportedValue));
      }
    }
    
    const newId = createNode(tx, {
      key: node.key,
      props,
    });
    
    oldIdToNewId.set(node.id, newId);
    nodeCount++;
    batchCount++;
    
    if (batchCount >= batchSize) {
      await commit(tx);
      tx = beginTx(db);
      batchCount = 0;
      
      if (onProgress) {
        onProgress("nodes", nodeCount, data.nodes.length);
      }
    }
  }
  
  if (batchCount > 0) {
    await commit(tx);
  }
  
  if (onProgress) {
    onProgress("nodes", nodeCount, data.nodes.length);
  }
  
  // Import edges
  let edgeCount = 0;
  tx = beginTx(db);
  batchCount = 0;
  
  for (const edge of data.edges) {
    const srcId = oldIdToNewId.get(edge.src);
    const dstId = oldIdToNewId.get(edge.dst);
    
    // Get etype id - try by name first, then by original id
    let etypeId: ETypeID | undefined;
    if (edge.etypeName) {
      etypeId = etypeNameToId.get(edge.etypeName);
    }
    if (etypeId === undefined) {
      // Use original etype id if name mapping failed
      etypeId = edge.etype;
    }
    
    if (srcId === undefined || dstId === undefined) {
      // Skip edge if nodes weren't imported
      continue;
    }
    
    // Convert props
    const props = new Map<PropKeyID, PropValue>();
    for (const [propName, exportedValue] of Object.entries(edge.props)) {
      const keyId = propKeyNameToId.get(propName);
      if (keyId !== undefined) {
        props.set(keyId, deserializePropValue(exportedValue));
      }
    }
    
    addEdge(tx, srcId, etypeId, dstId);
    // Note: edge props would need to be set separately via setEdgeProp
    edgeCount++;
    batchCount++;
    
    if (batchCount >= batchSize) {
      await commit(tx);
      tx = beginTx(db);
      batchCount = 0;
      
      if (onProgress) {
        onProgress("edges", edgeCount, data.edges.length);
      }
    }
  }
  
  if (batchCount > 0) {
    await commit(tx);
  }
  
  if (onProgress) {
    onProgress("edges", edgeCount, data.edges.length);
  }
  
  return { nodeCount, edgeCount, skipped };
}
