/**
 * Compactor - merges snapshot + delta into new snapshot
 */

import { join } from "node:path";
import {
  parseSnapshotGen,
  SNAPSHOTS_DIR,
  snapshotFilename,
  TRASH_DIR,
  WAL_DIR,
  walFilename,
} from "../constants.ts";
import type {
  DeltaState,
  ETypeID,
  GraphDB,
  LabelID,
  NodeID,
  PropKeyID,
  PropValue,
  SnapshotData,
} from "../types.ts";
import { PropValueTag } from "../types.ts";
import { readU32At } from "../util/binary.ts";
import type { CompressionOptions } from "../util/compression.ts";
import {
  clearDelta,
  createDelta,
  edgePropKey,
  getNodeDelta,
  isEdgeDeleted,
  isNodeDeleted,
} from "./delta.ts";
import { updateManifestForCompaction, writeManifest } from "./manifest.ts";
import {
  closeSnapshot,
  findEdgeIndex,
  getEdgeProps,
  getNodeId,
  getNodeProps,
  getOutEdges,
  getString,
  loadSnapshot,
} from "./snapshot-reader.ts";
import {
  buildSnapshot,
  type EdgeData,
  type NodeData,
  type SnapshotBuildInput,
} from "./snapshot-writer.ts";
import { createWalSegment } from "./wal.ts";

/**
 * Options for the optimize operation
 */
export interface OptimizeOptions {
  /** Compression options for the new snapshot */
  compression?: CompressionOptions;
}

// ============================================================================
// Compaction (optimize)
// ============================================================================

/**
 * Perform compaction - merge snapshot + delta into new snapshot
 *
 * @param db - The database to compact
 * @param options - Optional settings including compression configuration
 */
export async function optimize(
  db: GraphDB,
  options?: OptimizeOptions,
): Promise<void> {
  if (db.readOnly) {
    throw new Error("Cannot compact read-only database");
  }

  if (db._currentTx) {
    throw new Error("Cannot compact with active transaction");
  }

  // Collect all nodes and edges
  const nodes: NodeData[] = [];
  const edges: EdgeData[] = [];
  const labels = new Map<LabelID, string>();
  const etypes = new Map<ETypeID, string>();
  const propkeys = new Map<PropKeyID, string>();

  // First, add data from snapshot
  if (db._snapshot) {
    const numNodes = Number(db._snapshot.header.numNodes);
    const numEdges = Number(db._snapshot.header.numEdges);

    // Copy labels from snapshot
    for (let i = 1; i <= Number(db._snapshot.header.numLabels); i++) {
      const stringId = readU32At(db._snapshot.labelStringIds, i);
      if (stringId > 0) {
        labels.set(i, getString(db._snapshot, stringId));
      }
    }

    // Copy etypes from snapshot
    for (let i = 1; i <= Number(db._snapshot.header.numEtypes); i++) {
      const stringId = readU32At(db._snapshot.etypeStringIds, i);
      if (stringId > 0) {
        etypes.set(i, getString(db._snapshot, stringId));
      }
    }

    // Copy propkeys from snapshot
    if (db._snapshot.propkeyStringIds) {
      for (let i = 1; i <= Number(db._snapshot.header.numPropkeys); i++) {
        const stringId = readU32At(db._snapshot.propkeyStringIds, i);
        if (stringId > 0) {
          propkeys.set(i, getString(db._snapshot, stringId));
        }
      }
    }

    // Collect nodes from snapshot
    for (let phys = 0; phys < numNodes; phys++) {
      const nodeId = getNodeId(db._snapshot, phys);

      // Skip deleted nodes
      if (isNodeDeleted(db._delta, nodeId)) {
        continue;
      }

      // Get key
      const keyStringId = readU32At(db._snapshot.nodeKeyString, phys);
      const key =
        keyStringId > 0 ? getString(db._snapshot, keyStringId) : undefined;

      // Get properties from snapshot
      const snapshotProps = getNodeProps(db._snapshot, phys);
      const props = new Map<PropKeyID, PropValue>();

      if (snapshotProps) {
        for (const [keyId, value] of snapshotProps) {
          props.set(keyId, value);
        }
      }

      // Apply delta modifications and collect labels
      const nodeLabels: LabelID[] = [];
      const nodeDelta = getNodeDelta(db._delta, nodeId);
      if (nodeDelta) {
        for (const [keyId, value] of nodeDelta.props) {
          if (value === null) {
            props.delete(keyId);
          } else {
            props.set(keyId, value);
          }
        }
        // Collect labels added in delta (for existing nodes, snapshot labels + delta)
        // Note: snapshot doesn't store per-node labels yet, so we only have delta labels
        for (const labelId of nodeDelta.labels) {
          nodeLabels.push(labelId);
        }
      }

      nodes.push({
        nodeId,
        key,
        labels: nodeLabels,
        props,
      });

      // Collect edges from this node
      const outEdges = getOutEdges(db._snapshot, phys);
      for (const edge of outEdges) {
        const dstNodeId = getNodeId(db._snapshot, edge.dst);

        // Skip edges to deleted nodes
        if (isNodeDeleted(db._delta, dstNodeId)) {
          continue;
        }

        // Skip deleted edges
        if (isEdgeDeleted(db._delta, nodeId, edge.etype, dstNodeId)) {
          continue;
        }

        // Collect edge props from snapshot
        const edgeIdx = findEdgeIndex(db._snapshot, phys, edge.etype, edge.dst);
        const snapshotEdgeProps =
          edgeIdx >= 0 ? getEdgeProps(db._snapshot, edgeIdx) : null;
        const edgeProps = new Map<PropKeyID, PropValue>();

        if (snapshotEdgeProps) {
          for (const [keyId, value] of snapshotEdgeProps) {
            edgeProps.set(keyId, value);
          }
        }

        // Apply delta edge prop modifications
        const deltaEdgeKey = edgePropKey(nodeId, edge.etype, dstNodeId);
        const deltaEdgeProps = db._delta.edgeProps.get(deltaEdgeKey);
        if (deltaEdgeProps) {
          for (const [keyId, value] of deltaEdgeProps) {
            if (value === null) {
              edgeProps.delete(keyId);
            } else {
              edgeProps.set(keyId, value);
            }
          }
        }

        edges.push({
          src: nodeId,
          etype: edge.etype,
          dst: dstNodeId,
          props: edgeProps,
        });
      }
    }
  }

  // Add new labels from delta
  for (const [labelId, name] of db._delta.newLabels) {
    labels.set(labelId, name);
  }

  // Add new etypes from delta
  for (const [etypeId, name] of db._delta.newEtypes) {
    etypes.set(etypeId, name);
  }

  // Add new propkeys from delta
  for (const [propkeyId, name] of db._delta.newPropkeys) {
    propkeys.set(propkeyId, name);
  }

  // Add nodes created in delta
  for (const [nodeId, nodeDelta] of db._delta.createdNodes) {
    const props = new Map<PropKeyID, PropValue>();
    for (const [keyId, value] of nodeDelta.props) {
      if (value !== null) {
        props.set(keyId, value);
      }
    }

    nodes.push({
      nodeId,
      key: nodeDelta.key,
      labels: [...nodeDelta.labels],
      props,
    });
  }

  // Add edges from delta
  for (const [src, patches] of db._delta.outAdd) {
    for (const patch of patches) {
      // Check if either endpoint is deleted
      if (
        isNodeDeleted(db._delta, src) ||
        isNodeDeleted(db._delta, patch.other)
      ) {
        continue;
      }

      // Collect edge props from delta
      const deltaEdgeKey = edgePropKey(src, patch.etype, patch.other);
      const deltaEdgeProps = db._delta.edgeProps.get(deltaEdgeKey);
      const edgeProps = new Map<PropKeyID, PropValue>();

      if (deltaEdgeProps) {
        for (const [keyId, value] of deltaEdgeProps) {
          if (value !== null) {
            edgeProps.set(keyId, value);
          }
        }
      }

      edges.push({
        src,
        etype: patch.etype,
        dst: patch.other,
        props: edgeProps,
      });
    }
  }

  // Build new snapshot
  const newGen = db._manifest.activeSnapshotGen + 1n;
  const newWalSeg = db._manifest.activeWalSeg + 1n;

  const input: SnapshotBuildInput = {
    generation: newGen,
    nodes,
    edges,
    labels,
    etypes,
    propkeys,
    compression: options?.compression,
  };

  await buildSnapshot(db.path, input);

  // Create new WAL segment
  await createWalSegment(db.path, newWalSeg);

  // Update manifest atomically
  const newManifest = updateManifestForCompaction(
    db._manifest,
    newGen,
    newWalSeg,
  );
  await writeManifest(db.path, newManifest);

  // Close old snapshot
  if (db._snapshot) {
    closeSnapshot(db._snapshot);
  }

  // Load new snapshot
  db._snapshot = await loadSnapshot(db.path, newGen);
  db._manifest = newManifest;
  db._walOffset = 96; // WAL header size

  // Clear delta
  clearDelta(db._delta);

  // Garbage collect old snapshots
  await gcSnapshots(
    db.path,
    newManifest.activeSnapshotGen,
    newManifest.prevSnapshotGen,
  );
}

/**
 * Garbage collect old snapshots (keep last 2)
 */
async function gcSnapshots(
  dbPath: string,
  activeGen: bigint,
  prevGen: bigint,
): Promise<void> {
  const snapshotsDir = join(dbPath, SNAPSHOTS_DIR);
  const fs = await import("node:fs/promises");

  try {
    const files = await fs.readdir(snapshotsDir);

    for (const file of files) {
      const gen = parseSnapshotGen(file);
      if (gen === null) continue;

      // Keep active and prev
      if (gen === activeGen || gen === prevGen) continue;

      // Delete older snapshots
      const filepath = join(snapshotsDir, file);
      try {
        await fs.unlink(filepath);
      } catch (err) {
        // On Windows, file might be in use - move to trash
        try {
          const trashDir = join(dbPath, TRASH_DIR);
          await fs.mkdir(trashDir, { recursive: true });
          await fs.rename(filepath, join(trashDir, file));
        } catch {
          // Ignore - will be cleaned up later
        }
      }
    }
  } catch {
    // Ignore GC errors
  }
}

/**
 * Clean up trash directory
 */
export async function cleanTrash(dbPath: string): Promise<void> {
  const trashDir = join(dbPath, TRASH_DIR);
  const fs = await import("node:fs/promises");

  try {
    const files = await fs.readdir(trashDir);
    for (const file of files) {
      try {
        await fs.unlink(join(trashDir, file));
      } catch {
        // Ignore - still in use
      }
    }
  } catch {
    // Trash dir might not exist
  }
}
