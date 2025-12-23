/**
 * Database Manager Singleton
 *
 * Manages the current RayDB connection for the playground.
 */

import {
  ray,
  type Ray,
  defineNode,
  defineEdge,
  prop,
  optional,
} from "../../../src/index.ts";
import { createDemoGraph } from "./demo-data.ts";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { mkdtemp, rm, writeFile } from "node:fs/promises";

// ============================================================================
// Schema Definitions (shared with demo data)
// ============================================================================

export const FileNode = defineNode("file", {
  key: (path: string) => `file:${path}`,
  props: {
    path: prop.string("path"),
    language: prop.string("language"),
  },
});

export const FunctionNode = defineNode("function", {
  key: (name: string) => `fn:${name}`,
  props: {
    name: prop.string("name"),
    file: prop.string("file"),
    line: optional(prop.int("line")),
  },
});

export const ClassNode = defineNode("class", {
  key: (name: string) => `class:${name}`,
  props: {
    name: prop.string("name"),
    file: prop.string("file"),
  },
});

export const ModuleNode = defineNode("module", {
  key: (name: string) => `module:${name}`,
  props: {
    name: prop.string("name"),
  },
});

export const ImportsEdge = defineEdge("imports");
export const CallsEdge = defineEdge("calls");
export const ContainsEdge = defineEdge("contains");
export const ExtendsEdge = defineEdge("extends");

export const nodes = [FileNode, FunctionNode, ClassNode, ModuleNode];
export const edges = [ImportsEdge, CallsEdge, ContainsEdge, ExtendsEdge];

// ============================================================================
// Database Manager
// ============================================================================

interface DbState {
  db: Ray;
  path: string;
  isDemo: boolean;
  tempDir?: string;
}

let currentDb: DbState | null = null;

/**
 * Open a database from a file path
 */
export async function openDatabase(path: string): Promise<{ success: boolean; error?: string }> {
  try {
    await closeDatabase();
    
    const db = await ray(path, { nodes, edges });
    currentDb = { db, path, isDemo: false };
    
    return { success: true };
  } catch (error) {
    return { 
      success: false, 
      error: error instanceof Error ? error.message : "Failed to open database" 
    };
  }
}

/**
 * Open a database from an uploaded buffer
 */
export async function openFromBuffer(
  buffer: Uint8Array,
  filename: string
): Promise<{ success: boolean; error?: string }> {
  try {
    await closeDatabase();
    
    // Create temp directory and write the file
    const tempDir = await mkdtemp(join(tmpdir(), "raydb-playground-"));
    const tempPath = join(tempDir, filename);
    await writeFile(tempPath, buffer);
    
    const db = await ray(tempPath, { nodes, edges });
    currentDb = { db, path: tempPath, isDemo: false, tempDir };
    
    return { success: true };
  } catch (error) {
    return { 
      success: false, 
      error: error instanceof Error ? error.message : "Failed to open database" 
    };
  }
}

/**
 * Create and open a demo database
 */
export async function createDemo(): Promise<{ success: boolean; error?: string }> {
  try {
    await closeDatabase();
    
    // Create temp directory for demo
    const tempDir = await mkdtemp(join(tmpdir(), "raydb-demo-"));
    const demoPath = join(tempDir, "demo.raydb");
    
    const db = await ray(demoPath, { nodes, edges });
    
    // Populate with demo data
    await createDemoGraph(db);
    
    currentDb = { db, path: demoPath, isDemo: true, tempDir };
    
    return { success: true };
  } catch (error) {
    return { 
      success: false, 
      error: error instanceof Error ? error.message : "Failed to create demo database" 
    };
  }
}

/**
 * Close the current database
 */
export async function closeDatabase(): Promise<{ success: boolean }> {
  if (currentDb) {
    try {
      await currentDb.db.close();
      
      // Clean up temp directory if it exists
      if (currentDb.tempDir) {
        await rm(currentDb.tempDir, { recursive: true, force: true });
      }
    } catch {
      // Ignore close errors
    }
    currentDb = null;
  }
  return { success: true };
}

/**
 * Get the current database instance
 */
export function getDb(): Ray | null {
  return currentDb?.db ?? null;
}

/**
 * Get the current database path
 */
export function getDbPath(): string | null {
  return currentDb?.path ?? null;
}

/**
 * Check if the current database is the demo
 */
export function isDemo(): boolean {
  return currentDb?.isDemo ?? false;
}

/**
 * Get database status
 */
export async function getStatus(): Promise<{
  connected: boolean;
  path?: string;
  isDemo?: boolean;
  nodeCount?: number;
  edgeCount?: number;
}> {
  if (!currentDb) {
    return { connected: false };
  }
  
  try {
    const stats = await currentDb.db.stats();
    // Calculate node count from snapshot + delta
    const nodeCount = Number(stats.snapshotNodes) + stats.deltaNodesCreated - stats.deltaNodesDeleted;
    // Calculate edge count from snapshot + delta
    const edgeCount = Number(stats.snapshotEdges) + stats.deltaEdgesAdded - stats.deltaEdgesDeleted;
    return {
      connected: true,
      path: currentDb.path,
      isDemo: currentDb.isDemo,
      nodeCount,
      edgeCount,
    };
  } catch {
    return { connected: false };
  }
}
