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
} from "../../core/wal.ts";

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
  }
}

