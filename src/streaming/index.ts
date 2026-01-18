/**
 * Streaming and Pagination APIs for RayDB
 * 
 * Provides memory-efficient iteration over large datasets with:
 * - Async generators for non-blocking iteration
 * - Batched processing for better throughput
 * - Cursor-based pagination for resumable queries
 * - Progress callbacks for long-running operations
 */

import type { GraphDB, TxHandle, NodeID, ETypeID, Edge, PropKeyID, PropValue } from "../types.ts";
import { listNodes, countNodes, getNodeProps, nodeExists, getNodeByKey } from "../ray/graph-db/nodes.ts";
import { listEdges, countEdges, getEdgeProps } from "../ray/graph-db/edges.ts";
import { getSnapshot } from "../ray/graph-db/snapshot-helper.ts";
import { getNodeId, getPhysNode } from "../core/snapshot-reader.ts";
import { getNodeKey } from "../ray/key-index.ts";

// ============================================================================
// Types
// ============================================================================

export interface StreamOptions {
  /** Number of items per batch (default: 1000) */
  batchSize?: number;
  /** Progress callback called after each batch */
  onProgress?: (processed: number, total: number | null) => void;
  /** Signal to abort the stream */
  signal?: AbortSignal;
}

export interface PaginationOptions {
  /** Number of items per page (default: 100) */
  limit?: number;
  /** Cursor from previous page (start from beginning if not provided) */
  cursor?: string;
}

export interface Page<T> {
  /** Items in this page */
  items: T[];
  /** Cursor for next page (null if no more pages) */
  nextCursor: string | null;
  /** Whether there are more pages */
  hasMore: boolean;
  /** Total count if available (may be null for performance) */
  total?: number | null;
}

export interface NodeWithProps {
  id: NodeID;
  key?: string;
  props: Map<PropKeyID, PropValue>;
}

export interface EdgeWithProps {
  src: NodeID;
  dst: NodeID;
  etype: ETypeID;
  props: Map<PropKeyID, PropValue>;
}

// ============================================================================
// Streaming Node APIs
// ============================================================================

/**
 * Stream all nodes in batches
 * 
 * Memory-efficient async generator that yields batches of node IDs.
 * Use this for processing large datasets without loading everything into memory.
 * 
 * @example
 * ```ts
 * for await (const batch of streamNodes(db, { batchSize: 1000 })) {
 *   await processBatch(batch);
 * }
 * ```
 */
export async function* streamNodes(
  handle: GraphDB | TxHandle,
  options: StreamOptions = {}
): AsyncGenerator<NodeID[], void, unknown> {
  const { batchSize = 1000, onProgress, signal } = options;
  
  let batch: NodeID[] = [];
  let processed = 0;
  const total = countNodes(handle);
  
  for (const nodeId of listNodes(handle)) {
    // Check for abort
    if (signal?.aborted) {
      throw new DOMException("Stream aborted", "AbortError");
    }
    
    batch.push(nodeId);
    
    if (batch.length >= batchSize) {
      yield batch;
      processed += batch.length;
      onProgress?.(processed, total);
      batch = [];
    }
  }
  
  // Yield remaining items
  if (batch.length > 0) {
    yield batch;
    processed += batch.length;
    onProgress?.(processed, total);
  }
}

/**
 * Stream nodes with their properties
 * 
 * Yields batches of nodes including all their properties.
 * More expensive than streamNodes but provides complete node data.
 * 
 * @example
 * ```ts
 * for await (const batch of streamNodesWithProps(db)) {
 *   for (const node of batch) {
 *     console.log(node.id, node.props.get(nameKeyId));
 *   }
 * }
 * ```
 */
export async function* streamNodesWithProps(
  handle: GraphDB | TxHandle,
  options: StreamOptions = {}
): AsyncGenerator<NodeWithProps[], void, unknown> {
  const { batchSize = 1000, onProgress, signal } = options;
  const db = '_db' in handle ? handle._db : handle;
  
  let batch: NodeWithProps[] = [];
  let processed = 0;
  const total = countNodes(handle);
  
  for (const nodeId of listNodes(handle)) {
    if (signal?.aborted) {
      throw new DOMException("Stream aborted", "AbortError");
    }
    
    const props = getNodeProps(handle, nodeId);
    if (props) {
      // Get key from delta or snapshot
      const snapshot = getSnapshot(db);
      const key = getNodeKey(snapshot, db._delta, nodeId) ?? undefined;
      
      batch.push({ id: nodeId, key, props });
    }
    
    if (batch.length >= batchSize) {
      yield batch;
      processed += batch.length;
      onProgress?.(processed, total);
      batch = [];
    }
  }
  
  if (batch.length > 0) {
    yield batch;
    processed += batch.length;
    onProgress?.(processed, total);
  }
}

/**
 * Paginate through nodes with cursor-based pagination
 * 
 * Returns a page of nodes with a cursor for fetching the next page.
 * Cursors are stable across database modifications (within the same snapshot).
 * 
 * @example
 * ```ts
 * let cursor: string | undefined;
 * do {
 *   const page = await getNodesPage(db, { limit: 100, cursor });
 *   processNodes(page.items);
 *   cursor = page.nextCursor ?? undefined;
 * } while (cursor);
 * ```
 */
export function getNodesPage(
  handle: GraphDB | TxHandle,
  options: PaginationOptions = {}
): Page<NodeID> {
  const { limit = 100, cursor } = options;
  const db = '_db' in handle ? handle._db : handle;
  
  // Decode cursor (format: "n:{nodeId}" for node pagination)
  let startAfter: NodeID | null = null;
  if (cursor) {
    const match = cursor.match(/^n:(\d+)$/);
    if (match) {
      startAfter = parseInt(match[1], 10);
    }
  }
  
  const items: NodeID[] = [];
  let foundStart = startAfter === null;
  let lastNodeId: NodeID | null = null;
  
  for (const nodeId of listNodes(handle)) {
    if (!foundStart) {
      if (nodeId === startAfter) {
        foundStart = true;
      }
      continue;
    }
    
    items.push(nodeId);
    lastNodeId = nodeId;
    
    if (items.length >= limit + 1) {
      break;
    }
  }
  
  const hasMore = items.length > limit;
  if (hasMore) {
    items.pop(); // Remove the extra item we fetched to check hasMore
  }
  
  const nextCursor = hasMore && lastNodeId !== null ? `n:${items[items.length - 1]}` : null;
  
  return {
    items,
    nextCursor,
    hasMore,
    total: countNodes(handle),
  };
}

// ============================================================================
// Streaming Edge APIs
// ============================================================================

/**
 * Stream all edges in batches
 * 
 * @example
 * ```ts
 * for await (const batch of streamEdges(db, { batchSize: 5000 })) {
 *   await processEdgeBatch(batch);
 * }
 * ```
 */
export async function* streamEdges(
  handle: GraphDB | TxHandle,
  options: StreamOptions = {}
): AsyncGenerator<Edge[], void, unknown> {
  const { batchSize = 1000, onProgress, signal } = options;
  
  let batch: Edge[] = [];
  let processed = 0;
  // Note: countEdges can be expensive, so we don't always compute total
  let total: number | null = null;
  
  for (const edge of listEdges(handle)) {
    if (signal?.aborted) {
      throw new DOMException("Stream aborted", "AbortError");
    }
    
    batch.push(edge);
    
    if (batch.length >= batchSize) {
      yield batch;
      processed += batch.length;
      if (onProgress) {
        if (total === null) {
          total = countEdges(handle);
        }
        onProgress(processed, total);
      }
      batch = [];
    }
  }
  
  if (batch.length > 0) {
    yield batch;
    processed += batch.length;
    if (onProgress) {
      if (total === null) {
        total = countEdges(handle);
      }
      onProgress(processed, total);
    }
  }
}

/**
 * Stream edges with their properties
 */
export async function* streamEdgesWithProps(
  handle: GraphDB | TxHandle,
  options: StreamOptions = {}
): AsyncGenerator<EdgeWithProps[], void, unknown> {
  const { batchSize = 1000, onProgress, signal } = options;
  
  let batch: EdgeWithProps[] = [];
  let processed = 0;
  let total: number | null = null;
  
  for (const edge of listEdges(handle)) {
    if (signal?.aborted) {
      throw new DOMException("Stream aborted", "AbortError");
    }
    
    const props = getEdgeProps(handle, edge.src, edge.etype, edge.dst);
    batch.push({
      src: edge.src,
      dst: edge.dst,
      etype: edge.etype,
      props: props ?? new Map(),
    });
    
    if (batch.length >= batchSize) {
      yield batch;
      processed += batch.length;
      if (onProgress) {
        if (total === null) {
          total = countEdges(handle);
        }
        onProgress(processed, total);
      }
      batch = [];
    }
  }
  
  if (batch.length > 0) {
    yield batch;
    processed += batch.length;
    if (onProgress) {
      if (total === null) {
        total = countEdges(handle);
      }
      onProgress(processed, total);
    }
  }
}

/**
 * Paginate through edges with cursor-based pagination
 */
export function getEdgesPage(
  handle: GraphDB | TxHandle,
  options: PaginationOptions = {}
): Page<Edge> {
  const { limit = 100, cursor } = options;
  
  // Decode cursor (format: "e:{src}:{etype}:{dst}")
  let startAfter: { src: NodeID; etype: ETypeID; dst: NodeID } | null = null;
  if (cursor) {
    const match = cursor.match(/^e:(\d+):(\d+):(\d+)$/);
    if (match) {
      startAfter = {
        src: parseInt(match[1], 10),
        etype: parseInt(match[2], 10),
        dst: parseInt(match[3], 10),
      };
    }
  }
  
  const items: Edge[] = [];
  let foundStart = startAfter === null;
  
  for (const edge of listEdges(handle)) {
    if (!foundStart) {
      if (
        edge.src === startAfter!.src &&
        edge.etype === startAfter!.etype &&
        edge.dst === startAfter!.dst
      ) {
        foundStart = true;
      }
      continue;
    }
    
    items.push(edge);
    
    if (items.length >= limit + 1) {
      break;
    }
  }
  
  const hasMore = items.length > limit;
  if (hasMore) {
    items.pop();
  }
  
  const lastEdge = items[items.length - 1];
  const nextCursor = hasMore && lastEdge 
    ? `e:${lastEdge.src}:${lastEdge.etype}:${lastEdge.dst}` 
    : null;
  
  return {
    items,
    nextCursor,
    hasMore,
  };
}

// ============================================================================
// Utility Functions
// ============================================================================

/**
 * Collect all items from an async generator into an array
 * 
 * Warning: This loads all items into memory. Use with caution on large datasets.
 * 
 * @example
 * ```ts
 * const allNodes = await collectStream(streamNodes(db));
 * ```
 */
export async function collectStream<T>(
  stream: AsyncGenerator<T[], void, unknown>
): Promise<T[]> {
  const result: T[] = [];
  for await (const batch of stream) {
    result.push(...batch);
  }
  return result;
}

/**
 * Process a stream with a callback function
 * 
 * @example
 * ```ts
 * await processStream(streamNodes(db), async (batch) => {
 *   await saveToDisk(batch);
 * });
 * ```
 */
export async function processStream<T>(
  stream: AsyncGenerator<T[], void, unknown>,
  processor: (batch: T[]) => Promise<void> | void
): Promise<number> {
  let total = 0;
  for await (const batch of stream) {
    await processor(batch);
    total += batch.length;
  }
  return total;
}

/**
 * Map over a stream, transforming each batch
 * 
 * @example
 * ```ts
 * const nodeKeys = mapStream(streamNodes(db), (nodeId) => getNodeKey(db, nodeId));
 * for await (const keys of nodeKeys) {
 *   console.log(keys);
 * }
 * ```
 */
export async function* mapStream<T, U>(
  stream: AsyncGenerator<T[], void, unknown>,
  mapper: (item: T) => U | Promise<U>
): AsyncGenerator<U[], void, unknown> {
  for await (const batch of stream) {
    const mapped = await Promise.all(batch.map(mapper));
    yield mapped;
  }
}

/**
 * Filter a stream, keeping only items that match the predicate
 * 
 * @example
 * ```ts
 * const activeNodes = filterStream(
 *   streamNodesWithProps(db),
 *   (node) => node.props.get(statusKeyId)?.value === "active"
 * );
 * ```
 */
export async function* filterStream<T>(
  stream: AsyncGenerator<T[], void, unknown>,
  predicate: (item: T) => boolean | Promise<boolean>
): AsyncGenerator<T[], void, unknown> {
  for await (const batch of stream) {
    const filtered: T[] = [];
    for (const item of batch) {
      if (await predicate(item)) {
        filtered.push(item);
      }
    }
    if (filtered.length > 0) {
      yield filtered;
    }
  }
}

/**
 * Take the first N items from a stream
 * 
 * @example
 * ```ts
 * const first100 = await collectStream(takeStream(streamNodes(db), 100));
 * ```
 */
export async function* takeStream<T>(
  stream: AsyncGenerator<T[], void, unknown>,
  count: number
): AsyncGenerator<T[], void, unknown> {
  let remaining = count;
  
  for await (const batch of stream) {
    if (remaining <= 0) break;
    
    if (batch.length <= remaining) {
      yield batch;
      remaining -= batch.length;
    } else {
      yield batch.slice(0, remaining);
      remaining = 0;
    }
  }
}

/**
 * Skip the first N items from a stream
 */
export async function* skipStream<T>(
  stream: AsyncGenerator<T[], void, unknown>,
  count: number
): AsyncGenerator<T[], void, unknown> {
  let toSkip = count;
  
  for await (const batch of stream) {
    if (toSkip <= 0) {
      yield batch;
    } else if (toSkip >= batch.length) {
      toSkip -= batch.length;
    } else {
      yield batch.slice(toSkip);
      toSkip = 0;
    }
  }
}
