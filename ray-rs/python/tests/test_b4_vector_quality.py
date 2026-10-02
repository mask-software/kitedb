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
