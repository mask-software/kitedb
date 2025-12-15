/**
 * Snapshot checker - validates snapshot invariants
 */

import type { CheckResult, SnapshotData } from "../types.ts";
import { KEY_INDEX_ENTRY_SIZE, SectionId } from "../types.ts";
import { readI32At, readU32At, readU64At } from "../util/binary.ts";

/**
 * Check all snapshot invariants
 */
export function checkSnapshot(snapshot: SnapshotData): CheckResult {
  const errors: string[] = [];
  const warnings: string[] = [];

  const numNodes = Number(snapshot.header.numNodes);
  const numEdges = Number(snapshot.header.numEdges);
  const maxNodeId = Number(snapshot.header.maxNodeId);

  // ============================================================================
  // CSR offset monotonicity
  // ============================================================================

  // Check out_offsets monotonicity
  {
    let prev = 0;
    for (let i = 0; i <= numNodes; i++) {
      const current = readU32At(snapshot.outOffsets, i);
      if (current < prev) {
        errors.push(
          `out_offsets not monotonic at index ${i}: ${prev} -> ${current}`,
        );
        break;
      }
      prev = current;
    }

    const lastOffset = readU32At(snapshot.outOffsets, numNodes);
    if (lastOffset !== numEdges) {
      errors.push(
        `out_offsets final value ${lastOffset} != numEdges ${numEdges}`,
      );
    }
  }

  // Check in_offsets monotonicity (if present)
  if (snapshot.inOffsets) {
    let prev = 0;
    for (let i = 0; i <= numNodes; i++) {
      const current = readU32At(snapshot.inOffsets, i);
      if (current < prev) {
        errors.push(
          `in_offsets not monotonic at index ${i}: ${prev} -> ${current}`,
        );
        break;
      }
      prev = current;
    }

    const lastOffset = readU32At(snapshot.inOffsets, numNodes);
    if (lastOffset !== numEdges) {
      errors.push(
        `in_offsets final value ${lastOffset} != numEdges ${numEdges}`,
      );
    }
  }

  // ============================================================================
  // PhysNode references in range
  // ============================================================================

  // Check out_dst references
  for (let i = 0; i < numEdges; i++) {
    const dst = readU32At(snapshot.outDst, i);
    if (dst >= numNodes) {
      errors.push(`out_dst[${i}] = ${dst} out of range [0, ${numNodes})`);
    }
  }

  // Check in_src references (if present)
  if (snapshot.inSrc) {
    for (let i = 0; i < numEdges; i++) {
      const src = readU32At(snapshot.inSrc, i);
      if (src >= numNodes) {
        errors.push(`in_src[${i}] = ${src} out of range [0, ${numNodes})`);
      }
    }
  }

  // ============================================================================
  // Mapping bijection
  // ============================================================================

  // Check phys_to_nodeid -> nodeid_to_phys
  for (let phys = 0; phys < numNodes; phys++) {
    const nodeId = readU64At(snapshot.physToNodeId, phys);
    const nodeIdNum = Number(nodeId);

    if (nodeIdNum > maxNodeId) {
      errors.push(
        `phys_to_nodeid[${phys}] = ${nodeId} > maxNodeId ${maxNodeId}`,
      );
      continue;
    }

    const backPhys = readI32At(snapshot.nodeIdToPhys, nodeIdNum);
    if (backPhys !== phys) {
      errors.push(
        `Mapping mismatch: phys ${phys} -> nodeId ${nodeId} -> phys ${backPhys}`,
      );
    }
  }

  // Check nodeid_to_phys -> phys_to_nodeid
  const mappingSize = snapshot.nodeIdToPhys.byteLength / 4;
  for (let nodeIdNum = 0; nodeIdNum < mappingSize; nodeIdNum++) {
    const phys = readI32At(snapshot.nodeIdToPhys, nodeIdNum);
    if (phys === -1) continue; // Not present

    if (phys < 0 || phys >= numNodes) {
      errors.push(`nodeid_to_phys[${nodeIdNum}] = ${phys} out of range`);
      continue;
    }

    const backNodeId = readU64At(snapshot.physToNodeId, phys);
    if (backNodeId !== BigInt(nodeIdNum)) {
      errors.push(
        `Mapping mismatch: nodeId ${nodeIdNum} -> phys ${phys} -> nodeId ${backNodeId}`,
      );
    }
  }

  // ============================================================================
  // Edge sorting within each node
  // ============================================================================

  for (let phys = 0; phys < numNodes; phys++) {
    const start = readU32At(snapshot.outOffsets, phys);
    const end = readU32At(snapshot.outOffsets, phys + 1);

    for (let i = start + 1; i < end; i++) {
      const prevEtype = readU32At(snapshot.outEtype, i - 1);
      const prevDst = readU32At(snapshot.outDst, i - 1);
      const currEtype = readU32At(snapshot.outEtype, i);
      const currDst = readU32At(snapshot.outDst, i);

      const cmp =
        prevEtype < currEtype
          ? -1
          : prevEtype > currEtype
            ? 1
            : prevDst < currDst
              ? -1
              : prevDst > currDst
                ? 1
                : 0;

      if (cmp > 0) {
        errors.push(
          `Out-edges not sorted for phys ${phys} at index ${i}: (${prevEtype},${prevDst}) > (${currEtype},${currDst})`,
        );
        break;
      }
      if (cmp === 0) {
        warnings.push(
          `Duplicate out-edge for phys ${phys}: (${currEtype},${currDst})`,
        );
      }
    }
  }

  // ============================================================================
  // In/Out edge reciprocity
  // ============================================================================

  if (
    snapshot.inOffsets &&
    snapshot.inSrc &&
    snapshot.inEtype &&
    snapshot.inOutIndex
  ) {
    // For every out-edge, verify corresponding in-edge exists
    for (let srcPhys = 0; srcPhys < numNodes; srcPhys++) {
      const outStart = readU32At(snapshot.outOffsets, srcPhys);
      const outEnd = readU32At(snapshot.outOffsets, srcPhys + 1);

      for (let outIdx = outStart; outIdx < outEnd; outIdx++) {
        const dstPhys = readU32At(snapshot.outDst, outIdx);
        const etype = readU32At(snapshot.outEtype, outIdx);

        // Find this edge in dst's in-edges
        const inStart = readU32At(snapshot.inOffsets, dstPhys);
        const inEnd = readU32At(snapshot.inOffsets, dstPhys + 1);

        let found = false;
        for (let inIdx = inStart; inIdx < inEnd; inIdx++) {
          const inSrc = readU32At(snapshot.inSrc, inIdx);
          const inEtype = readU32At(snapshot.inEtype, inIdx);
          const inOutIdx = readU32At(snapshot.inOutIndex, inIdx);

          if (inSrc === srcPhys && inEtype === etype) {
            found = true;
            if (inOutIdx !== outIdx) {
              errors.push(
                `in_out_index mismatch: out[${outIdx}] -> in_out_index = ${inOutIdx}`,
              );
            }
            break;
          }
        }

        if (!found) {
          errors.push(
            `Missing reciprocal in-edge: out[${srcPhys}] -(${etype})-> [${dstPhys}]`,
          );
        }
      }
    }

    // For every in-edge, verify corresponding out-edge exists
    for (let dstPhys = 0; dstPhys < numNodes; dstPhys++) {
      const inStart = readU32At(snapshot.inOffsets, dstPhys);
      const inEnd = readU32At(snapshot.inOffsets, dstPhys + 1);

      for (let inIdx = inStart; inIdx < inEnd; inIdx++) {
        const srcPhys = readU32At(snapshot.inSrc, inIdx);
        const etype = readU32At(snapshot.inEtype, inIdx);
        const outIdx = readU32At(snapshot.inOutIndex, inIdx);

        // Verify out-edge at outIdx matches
        if (outIdx >= numEdges) {
          errors.push(`in_out_index[${inIdx}] = ${outIdx} out of range`);
          continue;
        }

        const outSrcStart = findNodeForEdgeIndex(
          snapshot.outOffsets,
          numNodes,
          outIdx,
        );
        const outDst = readU32At(snapshot.outDst, outIdx);
        const outEtype = readU32At(snapshot.outEtype, outIdx);

        if (
          outSrcStart !== srcPhys ||
          outDst !== dstPhys ||
          outEtype !== etype
        ) {
          errors.push(
            `Reciprocity mismatch: in[${dstPhys}] from ${srcPhys} type ${etype} -> out[${outIdx}] is (${outSrcStart},${outEtype},${outDst})`,
          );
        }
      }
    }
  }

  // ============================================================================
  // Key index ordering
  // ============================================================================

  if (!snapshot.keyEntries) {
    return { valid: errors.length === 0, errors, warnings };
  }

  const numKeyEntries = snapshot.keyEntries.byteLength / KEY_INDEX_ENTRY_SIZE;

  // When key buckets are present, entries are sorted by (bucket, hash, stringId, nodeId)
  // Otherwise, entries are sorted by (hash, stringId, nodeId)
  const hasKeyBuckets =
    snapshot.keyBuckets && snapshot.keyBuckets.byteLength > 4;
  const numBuckets = hasKeyBuckets
    ? BigInt(snapshot.keyBuckets!.byteLength / 4 - 1)
    : 0n;

  for (let i = 1; i < numKeyEntries; i++) {
    const prevOffset = (i - 1) * KEY_INDEX_ENTRY_SIZE;
    const currOffset = i * KEY_INDEX_ENTRY_SIZE;

    const prevHash = snapshot.keyEntries.getBigUint64(prevOffset, true);
    const currHash = snapshot.keyEntries.getBigUint64(currOffset, true);

    if (hasKeyBuckets) {
      // Check bucket ordering first
      const prevBucket = prevHash % numBuckets;
      const currBucket = currHash % numBuckets;

      if (prevBucket > currBucket) {
        errors.push(
          `Key index not sorted by bucket at index ${i}: bucket ${prevBucket} > ${currBucket}`,
        );
        break;
      }

      if (prevBucket < currBucket) {
        continue; // Different buckets, ordering is valid
      }
    }

    // Same bucket (or no buckets) - check hash ordering
    if (prevHash > currHash) {
      errors.push(
        `Key index not sorted by hash at index ${i}: ${prevHash} > ${currHash}`,
      );
      break;
    }

    if (prevHash === currHash) {
      const prevStringId = snapshot.keyEntries.getUint32(prevOffset + 8, true);
      const currStringId = snapshot.keyEntries.getUint32(currOffset + 8, true);

      if (prevStringId > currStringId) {
        errors.push(`Key index not sorted by stringId at index ${i}`);
        break;
      }

      if (prevStringId === currStringId) {
        const prevNodeId = snapshot.keyEntries.getBigUint64(
          prevOffset + 16,
          true,
        );
        const currNodeId = snapshot.keyEntries.getBigUint64(
          currOffset + 16,
          true,
        );

        if (prevNodeId >= currNodeId) {
          errors.push(`Key index not sorted by nodeId at index ${i}`);
          break;
        }
      }
    }
  }

  // ============================================================================
  // String table bounds
  // ============================================================================

  const numStrings = Number(snapshot.header.numStrings);
  const stringBytesLen = snapshot.stringBytes.length;

  for (let i = 0; i <= numStrings; i++) {
    const offset = readU32At(snapshot.stringOffsets, i);
    if (offset > stringBytesLen) {
      errors.push(
        `string_offsets[${i}] = ${offset} > string_bytes length ${stringBytesLen}`,
      );
      break;
    }
  }

  return {
    valid: errors.length === 0,
    errors,
    warnings,
  };
}

/**
 * Find which node owns a given edge index
 */
function findNodeForEdgeIndex(
  offsets: DataView,
  numNodes: number,
  edgeIdx: number,
): number {
  // Binary search in offsets array
  let lo = 0;
  let hi = numNodes;

  while (lo < hi) {
    const mid = (lo + hi + 1) >>> 1;
    const offset = readU32At(offsets, mid);
    if (offset <= edgeIdx) {
      lo = mid;
    } else {
      hi = mid - 1;
    }
  }

  return lo;
}

/**
 * Quick validation (just CRC and basic structure)
 */
export function quickCheck(snapshot: SnapshotData): boolean {
  const numNodes = Number(snapshot.header.numNodes);
  const numEdges = Number(snapshot.header.numEdges);

  // Check out_offsets bounds
  const lastOutOffset = readU32At(snapshot.outOffsets, numNodes);
  if (lastOutOffset !== numEdges) return false;

  // Check in_offsets bounds (if present)
  if (snapshot.inOffsets) {
    const lastInOffset = readU32At(snapshot.inOffsets, numNodes);
    if (lastInOffset !== numEdges) return false;
  }

  return true;
}
