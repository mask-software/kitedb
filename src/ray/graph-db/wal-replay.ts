import {
  addEdge as deltaAddEdge,
  createNode as deltaCreateNode,
  defineEtype as deltaDefineEtype,
  defineLabel as deltaDefineLabel,
  definePropkey as deltaDefinePropkey,
  deleteEdge as deltaDeleteEdge,
  deleteEdgeProp as deltaDeleteEdgeProp,
  deleteNode as deltaDeleteNode,
  deleteNodeProp as deltaDeleteNodeProp,
  setEdgeProp as deltaSetEdgeProp,
  setNodeProp as deltaSetNodeProp,
  isNodeCreated,
} from "../../core/delta.ts";
import type { DeltaState } from "../../types.ts";
import { WalRecordType } from "../../types.ts";
import {
  type ParsedWalRecord,
  parseAddEdgePayload,
  parseCreateNodePayload,
  parseDefineEtypePayload,
  parseDefineLabelPayload,
  parseDefinePropkeyPayload,
  parseDelEdgePropPayload,
  parseDeleteEdgePayload,
  parseDeleteNodePayload,
  parseDelNodePropPayload,
  parseSetEdgePropPayload,
  parseSetNodePropPayload,
  parseSetNodeVectorPayload,
  parseDelNodeVectorPayload,
} from "../../core/wal.ts";
import type { GraphDB, PropKeyID } from "../../types.ts";
import type { VectorManifest } from "../../vector/types.ts";
import {
  createVectorStore,
  vectorStoreInsert,
  vectorStoreDelete,
} from "../../vector/columnar-store.ts";

/**
 * Replay a WAL record into the delta
 */
export function replayWalRecord(record: ParsedWalRecord, delta: DeltaState): void {
  switch (record.type) {
    case WalRecordType.CREATE_NODE: {
      const data = parseCreateNodePayload(record.payload);
      deltaCreateNode(delta, data.nodeId, data.key);
      break;
    }
    case WalRecordType.DELETE_NODE: {
      const data = parseDeleteNodePayload(record.payload);
      deltaDeleteNode(delta, data.nodeId);
      break;
    }
    case WalRecordType.ADD_EDGE: {
      const data = parseAddEdgePayload(record.payload);
      deltaAddEdge(delta, data.src, data.etype, data.dst);
      break;
    }
    case WalRecordType.DELETE_EDGE: {
      const data = parseDeleteEdgePayload(record.payload);
      deltaDeleteEdge(delta, data.src, data.etype, data.dst);
      break;
    }
    case WalRecordType.DEFINE_LABEL: {
      const data = parseDefineLabelPayload(record.payload);
      deltaDefineLabel(delta, data.labelId, data.name);
      break;
    }
    case WalRecordType.DEFINE_ETYPE: {
      const data = parseDefineEtypePayload(record.payload);
      deltaDefineEtype(delta, data.etypeId, data.name);
      break;
    }
    case WalRecordType.DEFINE_PROPKEY: {
      const data = parseDefinePropkeyPayload(record.payload);
      deltaDefinePropkey(delta, data.propkeyId, data.name);
      break;
    }
    case WalRecordType.SET_NODE_PROP: {
      const data = parseSetNodePropPayload(record.payload);
      const isNew = isNodeCreated(delta, data.nodeId);
      deltaSetNodeProp(delta, data.nodeId, data.keyId, data.value, isNew);
      break;
    }
    case WalRecordType.DEL_NODE_PROP: {
      const data = parseDelNodePropPayload(record.payload);
      const isNew = isNodeCreated(delta, data.nodeId);
      deltaDeleteNodeProp(delta, data.nodeId, data.keyId, isNew);
      break;
    }
    case WalRecordType.SET_EDGE_PROP: {
      const data = parseSetEdgePropPayload(record.payload);
      deltaSetEdgeProp(
        delta,
        data.src,
        data.etype,
        data.dst,
        data.keyId,
        data.value,
      );
      break;
    }
    case WalRecordType.DEL_EDGE_PROP: {
      const data = parseDelEdgePropPayload(record.payload);
      deltaDeleteEdgeProp(delta, data.src, data.etype, data.dst, data.keyId);
      break;
    }
    // Vector operations are handled separately via replayVectorRecord
  }
}

/**
 * Replay a vector WAL record into the database's vector stores
 * 
 * Vector operations require access to the GraphDB to manage vector stores,
 * so they're handled separately from regular delta operations.
 */
export function replayVectorRecord(record: ParsedWalRecord, db: GraphDB): void {
  switch (record.type) {
    case WalRecordType.SET_NODE_VECTOR: {
      const data = parseSetNodeVectorPayload(record.payload);
      const store = getOrCreateVectorStore(db, data.propKeyId, data.dimensions);
      vectorStoreInsert(store, data.nodeId, data.vector);
      break;
    }
    case WalRecordType.DEL_NODE_VECTOR: {
      const data = parseDelNodeVectorPayload(record.payload);
      const store = db._vectorStores?.get(data.propKeyId) as VectorManifest | undefined;
      if (store) {
        vectorStoreDelete(store, data.nodeId);
      }
      break;
    }
    // BATCH_VECTORS, SEAL_FRAGMENT, COMPACT_FRAGMENTS can be implemented later
    // for bulk operations and index management
  }
}

/**
 * Get or create a vector store for the given property key
 */
export function getOrCreateVectorStore(
  db: GraphDB,
  propKeyId: PropKeyID,
  dimensions: number
): VectorManifest {
  if (!db._vectorStores) {
    (db as { _vectorStores: Map<PropKeyID, VectorManifest> })._vectorStores = new Map();
  }
  
  const vectorStores = db._vectorStores as Map<PropKeyID, VectorManifest>;
  let store = vectorStores.get(propKeyId);
  if (!store) {
    store = createVectorStore(dimensions, {
      metric: 'cosine',
      rowGroupSize: 1024,
      fragmentTargetSize: 100_000,
      normalize: true,
    });
    vectorStores.set(propKeyId, store);
  }
  
  return store;
}

