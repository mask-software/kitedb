/**
 * Vector embeddings module for RayDB
 *
 * Lance-style columnar storage with IVF index for approximate nearest neighbor search.
 */

// Types
export type {
  VectorStoreConfig,
  IvfConfig,
  RowGroup,
  Fragment,
  VectorManifest,
  IvfIndex,
  VectorSearchResult,
  SecondaryIndexType,
  SecondaryIndexConfig,
  BTreeNode,
  SecondaryIndex,
  FilterOperators,
  FastFilter,
  CompiledFilter,
  VectorSearchOptions,
  MultiVectorSearchOptions,
  BatchInsertOptions,
  VectorManifestHeader,
  FragmentHeader,
  IvfIndexHeader,
  SetNodeVectorPayload,
  DelNodeVectorPayload,
  BatchVectorsPayload,
  SealFragmentPayload,
  CompactFragmentsPayload,
  VectorDeltaState,
} from "./types.ts";

export { DEFAULT_VECTOR_CONFIG, DEFAULT_IVF_CONFIG } from "./types.ts";

// Normalization & Validation
export {
  validateVector,
  hasNaN,
  hasInfinity,
  isZeroVector,
  l2Norm,
  normalizeInPlace,
  normalize,
  isNormalized,
  normalizeRowGroup,
  normalizeVectorAt,
  isNormalizedAt,
} from "./normalize.ts";
export type { VectorValidationResult } from "./normalize.ts";

// Distance functions
export {
  dotProduct,
  cosineDistance,
  cosineSimilarity,
  squaredEuclidean,
  euclideanDistance,
  dotProductAt,
  squaredEuclideanAt,
  batchCosineDistance,
  batchSquaredEuclidean,
  batchDotProductDistance,
  getDistanceFunction,
  getBatchDistanceFunction,
  distanceToSimilarity,
  findKNearest,
  MinHeap,
  MaxHeap,
} from "./distance.ts";

// Row group operations
export {
  createRowGroup,
  rowGroupAppend,
  rowGroupGet,
  rowGroupGetCopy,
  rowGroupIsFull,
  rowGroupRemainingCapacity,
  rowGroupByteSize,
  rowGroupUsedByteSize,
  rowGroupTrim,
  rowGroupFromData,
  rowGroupIterator,
  rowGroupCopy,
} from "./row-group.ts";

// Fragment operations
export {
  createFragment,
  fragmentAppend,
  fragmentDelete,
  fragmentIsDeleted,
  fragmentUndelete,
  fragmentSeal,
  fragmentShouldSeal,
  fragmentGetVector,
  fragmentLiveCount,
  fragmentDeletionRatio,
  fragmentByteSize,
  fragmentIterator,
  fragmentFromData,
  fragmentClone,
} from "./fragment.ts";

// Columnar store
export {
  createVectorStore,
  vectorStoreInsert,
  vectorStoreDelete,
  vectorStoreGet,
  vectorStoreGetById,
  vectorStoreHas,
  vectorStoreGetVectorId,
  vectorStoreGetNodeId,
  vectorStoreGetLocation,
  vectorStoreIterator,
  vectorStoreIteratorWithIds,
  vectorStoreBatchInsert,
  vectorStoreStats,
  vectorStoreFragmentStats,
  vectorStoreSealActive,
  vectorStoreGetAllVectors,
  vectorStoreClear,
  vectorStoreClone,
} from "./columnar-store.ts";

// IVF index
export {
  createIvfIndex,
  ivfAddTrainingVectors,
  ivfTrain,
  ivfInsert,
  ivfDelete,
  ivfSearch,
  ivfSearchMulti,
  ivfBuildFromStore,
  ivfStats,
  ivfClear,
} from "./ivf-index.ts";

// Compaction
export {
  findFragmentsToCompact,
  compactFragments,
  applyCompaction,
  runCompactionIfNeeded,
  getCompactionStats,
  forceFullCompaction,
  clearDeletedFragments,
  DEFAULT_COMPACTION_STRATEGY,
} from "./compaction.ts";
export type { CompactionStrategy } from "./compaction.ts";

// Serialization
export {
  ivfSerializedSize,
  serializeIvf,
  deserializeIvf,
  manifestSerializedSize,
  serializeManifest,
  deserializeManifest,
} from "./ivf-serialize.ts";

// Product Quantization
export {
  createPQIndex,
  pqTrain,
  pqEncode,
  pqEncodeOne,
  pqBuildDistanceTable,
  pqDistanceADC,
  pqSearch,
  pqSearchWithTable,
  pqStats,
  DEFAULT_PQ_CONFIG,
} from "./pq.ts";
export type { PQConfig, PQIndex } from "./pq.ts";

// IVF-PQ Combined Index
export {
  createIvfPqIndex,
  ivfPqAddTrainingVectors,
  ivfPqTrain,
  ivfPqInsert,
  ivfPqSearch,
  ivfPqStats,
  DEFAULT_IVF_PQ_CONFIG,
} from "./ivf-pq.ts";
export type { IvfPqConfig, IvfPqIndex } from "./ivf-pq.ts";
