"""High-level vector index API mirroring the TypeScript VectorIndex."""

from __future__ import annotations

import json
import math
from collections import OrderedDict
from dataclasses import dataclass
from typing import Callable, Dict, Iterable, List, Optional, Sequence, Tuple

from raydb._raydb import IvfConfig, IvfIndex, SearchOptions, brute_force_search
from raydb.builders import NodeRef


_METRIC_MAP = {
    "cosine": "Cosine",
    "euclidean": "Euclidean",
    "dot": "DotProduct",
    "dot_product": "DotProduct",
    "dotproduct": "DotProduct",
}


def _validate_vector(vector: Sequence[float]) -> Optional[str]:
    if len(vector) == 0:
        return "Vector is empty"
    for value in vector:
        if not math.isfinite(value):
            return "Vector contains NaN or infinity"
    return None


class _LRUCache:
    def __init__(self, max_size: int = 10_000):
        self._max_size = max_size
        self._data: OrderedDict[int, NodeRef] = OrderedDict()

    def get(self, key: int) -> Optional[NodeRef]:
        value = self._data.get(key)
        if value is not None:
            self._data.move_to_end(key)
        return value

    def set(self, key: int, value: NodeRef) -> None:
        if key in self._data:
            self._data.move_to_end(key)
        self._data[key] = value
        if len(self._data) > self._max_size:
            self._data.popitem(last=False)

    def delete(self, key: int) -> None:
        self._data.pop(key, None)

    def clear(self) -> None:
        self._data.clear()


@dataclass
class VectorIndexOptions:
    dimensions: int
    metric: str = "cosine"
    row_group_size: int = 1024
    fragment_target_size: int = 100_000
    normalize: Optional[bool] = None
    ivf: Optional[Dict[str, object]] = None
    training_threshold: int = 1000
    cache_max_size: int = 10_000


@dataclass
class SimilarOptions:
    k: int
    threshold: Optional[float] = None
    n_probe: Optional[int] = None
    filter: Optional[Callable[[int], bool]] = None


@dataclass
class VectorSearchHit:
    node: NodeRef
    distance: float
    similarity: float


class VectorIndex:
    def __init__(self, options: VectorIndexOptions):
        self._dimensions = options.dimensions
        self._metric = options.metric.lower()
        self._metric_enum = _METRIC_MAP.get(self._metric, "Cosine")
        self._row_group_size = options.row_group_size
        self._fragment_target_size = options.fragment_target_size
        self._normalize = options.normalize
        if self._normalize is None:
            self._normalize = self._metric == "cosine"

        self._ivf_config = options.ivf or {}
        self._training_threshold = options.training_threshold
        self._node_ref_cache = _LRUCache(options.cache_max_size)

        self._vectors: Dict[int, List[float]] = {}
        self._node_to_vector: Dict[int, int] = {}
        self._vector_to_node: Dict[int, int] = {}
        self._next_vector_id = 0

        self._index: Optional[IvfIndex] = None
        self._needs_training = True
        self._is_building = False

        self._manifest_cache: Optional[str] = None
        self._manifest_dirty = True

    def set(self, node_ref: NodeRef, vector: Sequence[float]) -> None:
        if self._is_building:
            raise ValueError("Cannot modify vectors while index is being built")

        vec = self._coerce_vector(vector)
        if len(vec) != self._dimensions:
            raise ValueError(
                f"Vector dimension mismatch: expected {self._dimensions}, got {len(vec)}"
            )

        validation_error = _validate_vector(vec)
        if validation_error is not None:
            raise ValueError(f"Invalid vector: {validation_error}")

        if self._normalize:
            vec = self._normalize_vector(vec)

        node_id = node_ref.id

        if node_id in self._node_to_vector:
            old_vector_id = self._node_to_vector[node_id]
            old_vector = self._vectors.get(node_id)
            if old_vector is not None and self._index is not None and self._index.trained:
                self._index.delete(old_vector_id, old_vector)
            self._node_to_vector.pop(node_id, None)
            self._vector_to_node.pop(old_vector_id, None)

        vector_id = self._next_vector_id
        self._next_vector_id += 1

        self._vectors[node_id] = vec
        self._node_to_vector[node_id] = vector_id
        self._vector_to_node[vector_id] = node_id
        self._manifest_dirty = True

        self._node_ref_cache.set(node_id, node_ref)

        if self._index is not None and self._index.trained:
            self._index.insert(vector_id, vec)
        else:
            self._needs_training = True

    def get(self, node_ref: NodeRef) -> Optional[List[float]]:
        return self._vectors.get(node_ref.id)

    def delete(self, node_ref: NodeRef) -> bool:
        if self._is_building:
            raise ValueError("Cannot modify vectors while index is being built")

        node_id = node_ref.id
        vector_id = self._node_to_vector.get(node_id)
        if vector_id is None:
            return False

        vector = self._vectors.get(node_id)
        if vector is None:
            return False

        if self._index is not None and self._index.trained:
            self._index.delete(vector_id, vector)

        self._vectors.pop(node_id, None)
        self._node_to_vector.pop(node_id, None)
        self._vector_to_node.pop(vector_id, None)
        self._node_ref_cache.delete(node_id)
        self._manifest_dirty = True
        return True

    def has(self, node_ref: NodeRef) -> bool:
        return node_ref.id in self._node_to_vector

    def build_index(self) -> None:
        if self._is_building:
            raise ValueError("Index build already in progress")

        self._is_building = True
        try:
            live_vectors = len(self._vectors)
            if live_vectors < self._training_threshold:
                self._index = None
                self._needs_training = False
                return

            n_clusters = self._ivf_config.get("n_clusters")
            if n_clusters is None:
                n_clusters = min(1024, max(16, int(math.sqrt(live_vectors))))
            n_probe = self._ivf_config.get("n_probe")
            ivf_config = IvfConfig(
                n_clusters=n_clusters,
                n_probe=int(n_probe) if n_probe is not None else None,
                metric=self._metric,
            )
            index = IvfIndex(self._dimensions, ivf_config)

            training_data, count = self._collect_training_vectors()
            index.add_training_vectors(training_data, num_vectors=count)
            index.train()

            for node_id, vector in self._vectors.items():
                vector_id = self._node_to_vector[node_id]
                index.insert(vector_id, vector)

            self._index = index
            self._needs_training = False
        finally:
            self._is_building = False

    def search(
        self,
        query: Sequence[float],
        options: Optional[SimilarOptions] = None,
        **kwargs: object,
    ) -> List[VectorSearchHit]:
        opts = self._coerce_similar_options(options, kwargs)

        query_vec = self._coerce_vector(query)
        if len(query_vec) != self._dimensions:
            raise ValueError(
                f"Query dimension mismatch: expected {self._dimensions}, got {len(query_vec)}"
            )

        validation_error = _validate_vector(query_vec)
        if validation_error is not None:
            raise ValueError(f"Invalid query vector: {validation_error}")

        if self._normalize:
            query_vec = self._normalize_vector(query_vec)

        if self._needs_training:
            self.build_index()

        if self._index is not None and self._index.trained:
            results = self._search_ivf(query_vec, opts)
        else:
            results = self._search_brute_force(query_vec, opts)

        hits: List[VectorSearchHit] = []
        for node_id, distance, similarity in results:
            node_ref = self._node_ref_cache.get(node_id)
            if node_ref is None:
                continue
            hits.append(VectorSearchHit(node=node_ref, distance=distance, similarity=similarity))
            if len(hits) >= opts.k:
                break

        return hits

    def stats(self) -> Dict[str, object]:
        total_vectors = len(self._node_to_vector)
        live_vectors = len(self._vectors)
        return {
            "totalVectors": total_vectors,
            "liveVectors": live_vectors,
            "dimensions": self._dimensions,
            "metric": self._metric,
            "indexTrained": self._index.trained if self._index is not None else False,
            "indexClusters": self._index.config.n_clusters if self._index is not None else None,
        }

    def clear(self) -> None:
        self._vectors.clear()
        self._node_to_vector.clear()
        self._vector_to_node.clear()
        self._node_ref_cache.clear()
        self._next_vector_id = 0
        self._index = None
        self._needs_training = True
        self._manifest_cache = None
        self._manifest_dirty = True

    def buildIndex(self) -> None:  # noqa: N802
        self.build_index()

    def _coerce_vector(self, vector: Sequence[float]) -> List[float]:
        return [float(v) for v in vector]

    def _normalize_vector(self, vector: List[float]) -> List[float]:
        norm = math.sqrt(sum(v * v for v in vector))
        if norm <= 1e-12:
            return vector
        return [v / norm for v in vector]

    def _collect_training_vectors(self) -> Tuple[List[float], int]:
        flat: List[float] = []
        count = 0
        for vec in self._vectors.values():
            flat.extend(vec)
            count += 1
        return flat, count

    def _search_brute_force(
        self,
        query_vec: List[float],
        options: SimilarOptions,
    ) -> List[Tuple[int, float, float]]:
        vectors: List[List[float]] = []
        node_ids: List[int] = []

        for node_id, vec in self._vectors.items():
            if options.filter is not None:
                try:
                    if not options.filter(node_id):
                        continue
                except Exception:
                    continue
            node_ids.append(node_id)
            vectors.append(vec)

        if not vectors:
            return []

        results = brute_force_search(
            vectors=vectors,
            node_ids=node_ids,
            query=query_vec,
            k=max(options.k * 2, options.k),
            metric=self._metric,
        )

        hits: List[Tuple[int, float, float]] = []
        for result in results:
            if options.threshold is not None and result.similarity < options.threshold:
                continue
            hits.append((int(result.node_id), float(result.distance), float(result.similarity)))
        return hits

    def _search_ivf(
        self,
        query_vec: List[float],
        options: SimilarOptions,
    ) -> List[Tuple[int, float, float]]:
        if self._index is None:
            return []

        search_options = None
        if options.n_probe is not None or options.threshold is not None:
            search_options = SearchOptions(
                n_probe=options.n_probe,
                threshold=options.threshold,
            )

        manifest_json = self._build_manifest_json()
        results = self._index.search(
            manifest_json=manifest_json,
            query=query_vec,
            k=max(options.k * 2, options.k),
            options=search_options,
        )

        hits: List[Tuple[int, float, float]] = []
        for result in results:
            node_id = int(result.node_id)
            if options.filter is not None:
                try:
                    if not options.filter(node_id):
                        continue
                except Exception:
                    continue
            if options.threshold is not None and result.similarity < options.threshold:
                continue
            hits.append((node_id, float(result.distance), float(result.similarity)))
        return hits

    def _build_manifest_json(self) -> str:
        if not self._manifest_dirty and self._manifest_cache is not None:
            return self._manifest_cache

        vector_ids = sorted(self._vector_to_node.keys())
        row_groups: List[Dict[str, object]] = []
        vector_locations: Dict[str, Dict[str, int]] = {}

        current_data: List[float] = []
        current_count = 0
        row_group_id = 0
        local_index = 0

        for vector_id in vector_ids:
            node_id = self._vector_to_node[vector_id]
            vector = self._vectors.get(node_id)
            if vector is None:
                continue

            if current_count >= self._row_group_size:
                row_groups.append(
                    {
                        "id": row_group_id,
                        "count": current_count,
                        "data": current_data,
                    }
                )
                row_group_id += 1
                current_data = []
                current_count = 0

            current_data.extend(vector)
            current_count += 1
            vector_locations[str(vector_id)] = {
                "fragment_id": 0,
                "local_index": local_index,
            }
            local_index += 1

        if current_count > 0 or not row_groups:
            row_groups.append(
                {
                    "id": row_group_id,
                    "count": current_count,
                    "data": current_data,
                }
            )

        fragment = {
            "id": 0,
            "state": "Active",
            "row_groups": row_groups,
            "total_vectors": local_index,
            "deletion_bitmap": [],
            "deleted_count": 0,
        }

        manifest = {
            "config": {
                "dimensions": self._dimensions,
                "metric": self._metric_enum,
                "row_group_size": self._row_group_size,
                "fragment_target_size": self._fragment_target_size,
                "normalize_on_insert": self._normalize,
            },
            "fragments": [fragment],
            "active_fragment_id": 0,
            "total_vectors": local_index,
            "total_deleted": 0,
            "next_vector_id": self._next_vector_id,
            "node_to_vector": {str(k): v for k, v in self._node_to_vector.items()},
            "vector_to_node": {str(k): v for k, v in self._vector_to_node.items()},
            "vector_locations": vector_locations,
        }

        self._manifest_cache = json.dumps(manifest)
        self._manifest_dirty = False
        return self._manifest_cache

    def _coerce_similar_options(
        self,
        options: Optional[SimilarOptions],
        kwargs: Dict[str, object],
    ) -> SimilarOptions:
        if options is None:
            if "k" not in kwargs:
                raise ValueError("search requires k or SimilarOptions")
            return SimilarOptions(
                k=int(kwargs["k"]),
                threshold=kwargs.get("threshold"),
                n_probe=kwargs.get("n_probe"),
                filter=kwargs.get("filter"),
            )
        return options


def create_vector_index(options: VectorIndexOptions) -> VectorIndex:
    return VectorIndex(options)
