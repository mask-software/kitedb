"""b4 vector-quality lane.

VQ2 (Python): ``brute_force_search`` sorted with
``partial_cmp(...).unwrap_or(Equal)``, so a NaN distance (from a NaN
component) compared equal to every distance: NaN hits could take top-k slots
and leave the finite hits out of order.
"""

from __future__ import annotations

import math

import pytest

from kitedb import brute_force_search

# Cosine rejects a vector with a NaN component (its norm is not finite), so
# only Euclidean and dot product reach the sort.
CASES = [
    # metric, query, vector for i, distance for i (distinct and ordered by i)
    ("euclidean", [0.0, 0.0], lambda i: [float(i), 0.0], lambda i: float(i)),
    ("dot", [1.0, 0.0], lambda i: [float(i), 0.0], lambda i: -float(i)),
]


@pytest.mark.parametrize("metric,query,vector,distance", CASES, ids=[c[0] for c in CASES])
def test_brute_force_search_keeps_nan_out_and_top_k_in_order(metric, query, vector, distance):
    vectors, node_ids, finite = [], [], []
    for i in range(1, 65):
        node_ids.append(i)
        if i % 2 == 0:
            vectors.append([math.nan, 1.0])
        else:
            vectors.append(vector(i))
            finite.append((distance(i), i))
    # Present the finite vectors far from their sorted order.
    vectors.reverse()
    node_ids.reverse()

    k = 5
    expected = [node for _, node in sorted(finite)[:k]]
    hits = brute_force_search(vectors, node_ids, query, k, metric)
    assert not any(math.isnan(hit.distance) for hit in hits), [
        (hit.node_id, hit.distance) for hit in hits
    ]
    assert [hit.node_id for hit in hits] == expected


# VQ3: a training seed (IvfConfig(seed=...)) makes builds reproducible.
def _seeded_data():
    data = []
    for i in range(3000):
        blob = i % 20
        data.extend(math.sin(blob * 7 + d) * 4 + math.sin(i * 13.7 + d * 3.1) for d in range(8))
    return data


@pytest.mark.parametrize("pq", [False, True], ids=["ivf", "ivf_pq"])
def test_seeded_builds_serialize_identically(pq):
    from kitedb import IvfConfig, IvfIndex, IvfPqIndex, PqConfig

    data = _seeded_data()

    def build(seed):
        config = IvfConfig(n_clusters=20, seed=seed)
        if pq:
            index = IvfPqIndex(8, config, PqConfig(num_subspaces=4, num_centroids=32))
        else:
            index = IvfIndex(8, config)
        index.add_training_vectors(data, 3000)
        index.train()
        for i in range(3000):
            index.insert(i, data[i * 8 : i * 8 + 8])
        return index.serialize()

    assert build(5) == build(5)
    assert build(5) != build(6)
    assert IvfConfig(seed=2**64 - 1).seed == 2**64 - 1
    with pytest.raises(OverflowError):
        IvfConfig(seed=-1)


# ann_algorithm: "auto" (default), "ivf" or "ivf_pq" for the Python VectorIndex.
def _ref(node_id):
    from kitedb.builders import NodeRef

    return NodeRef(id=node_id, key=f"n:{node_id}", node_def=None)


def _backend_vectors(count, dims):
    return [
        [math.sin((i % 10) * 5 + d) * 4 + math.sin(i * 7.3 + d * 1.7) for d in range(dims)]
        for i in range(count)
    ]


def _euclidean(a, b):
    return math.sqrt(sum((x - y) ** 2 for x, y in zip(a, b)))


def _build(vectors, **options):
    from kitedb import VectorIndexOptions, create_vector_index

    index = create_vector_index(
        VectorIndexOptions(dimensions=len(vectors[0]), metric="euclidean", **options)
    )
    for node_id, vector in enumerate(vectors):
        index.set(_ref(node_id), vector)
    return index


def _assert_finds_itself(index, vectors, nodes, exact):
    from kitedb import SimilarOptions

    for node in nodes:
        hits = index.search(vectors[node], SimilarOptions(k=5))
        assert hits[0].node.id == node
        if exact:
            for hit in hits:
                assert hit.distance == pytest.approx(_euclidean(vectors[node], vectors[hit.node.id]), rel=1e-4, abs=1e-4)


def test_resolve_ann_algorithm_rule():
    from kitedb._kitedb import resolve_ann_algorithm

    assert resolve_ann_algorithm("auto", 128, 10_000_000) == "ivf"
    assert resolve_ann_algorithm("auto", 768, 49_999) == "ivf"
    assert resolve_ann_algorithm("auto", 512, 50_000) == "ivf_pq"
    assert resolve_ann_algorithm("ivf", 1536, 1_000_000) == "ivf"
    assert resolve_ann_algorithm("ivf_pq", 4, 10) == "ivf_pq"
    with pytest.raises(ValueError, match="ANN algorithm"):
        resolve_ann_algorithm("hnsw", 4, 10)


@pytest.mark.parametrize("ann_algorithm", ["auto", "ivf", "ivf_pq"])
def test_vector_index_ann_algorithm(ann_algorithm):
    vectors = _backend_vectors(1500, 16)
    index = _build(vectors, ann_algorithm=ann_algorithm)
    index.build_index()
    stats = index.stats()
    assert stats["indexTrained"]
    # Auto picks plain IVF for a small, low-dimensional index.
    assert stats["indexAlgorithm"] == ("ivf_pq" if ann_algorithm == "ivf_pq" else "ivf")
    _assert_finds_itself(index, vectors, [0, 777, 1499], exact=True)


def test_vector_index_rejects_unknown_ann_algorithm():
    from kitedb import VectorIndexOptions, create_vector_index

    with pytest.raises(ValueError, match="ann_algorithm"):
        create_vector_index(VectorIndexOptions(dimensions=4, ann_algorithm="hnsw"))


def test_vector_index_auto_switches_to_ivf_pq_as_it_grows(monkeypatch):
    import kitedb.vector_index as vector_index_module

    # A lower threshold, so a small index can cross it: IVF-PQ from 3000.
    monkeypatch.setattr(
        vector_index_module,
        "resolve_ann_algorithm",
        lambda algorithm, dims, live: "ivf_pq" if algorithm == "auto" and live >= 3000 else "ivf",
    )
    vectors = _backend_vectors(3000, 16)
    index = _build(vectors[:2000])
    _assert_finds_itself(index, vectors, [0, 1999], exact=True)
    assert index.stats()["indexAlgorithm"] == "ivf"

    for node_id in range(2000, 3000):
        index.set(_ref(node_id), vectors[node_id])
    assert index.stats()["indexAlgorithm"] == "ivf"
    _assert_finds_itself(index, vectors, [0, 1999, 2500, 2999], exact=True)
    assert index.stats()["indexAlgorithm"] == "ivf_pq"
