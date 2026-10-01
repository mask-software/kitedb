"""Regression tests for the Python-binding audit findings P1-P7.

Each test encodes the contract the fix must satisfy and fails on the
unfixed code (audit base commit 000e319).
"""

import json
import math
import threading
import time

import pytest

from kitedb import (
    Database,
    IvfConfig,
    IvfIndex,
    IvfPqIndex,
    PqConfig,
    PropValue,
    SimilarOptions,
    VectorIndexOptions,
    brute_force_search,
    create_vector_index,
    define_edge,
    define_node,
    kite,
    prop,
)


def _schema(key_fn=None):
    user = define_node(
        "user",
        key=key_fn or (lambda id: f"user:{id}"),
        props={
            "name": prop.string("name"),
            "age": prop.int("age"),
        },
    )
    knows = define_edge("knows", {"since": prop.int("since")})
    return user, knows


def _call(fn):
    """Run fn and return (result, exception), catching BaseException.

    PanicException derives from BaseException, so `except Exception` and
    `pytest.raises(ValueError)` would not show what was actually raised.
    """
    try:
        return fn(), None
    except BaseException as exc:  # noqa: BLE001 - deliberately broad
        return None, exc


def _describe(exc):
    return "nothing" if exc is None else f"{type(exc).__name__}: {exc}"


# ============================================================================
# P1: node IDs are unchecked on write paths
# ============================================================================


def _open_low_level(tmp_path):
    db = Database(str(tmp_path / "p1.kitedb"))
    db.begin()
    a = db.create_node("user:a")
    b = db.create_node("user:b")
    ids = {
        "a": a,
        "b": b,
        "etype": db.get_or_create_etype("knows"),
        "key": db.get_or_create_propkey("age"),
        "vec": db.get_or_create_propkey("embedding"),
        "label": db.define_label("person"),
    }
    db.add_edge(a, ids["etype"], b)
    db.commit()
    return db, ids


_P1_WRITE_OPS = {
    "upsert_node_by_id": lambda db, i: db.upsert_node_by_id(-1, []),
    "delete_node": lambda db, i: db.delete_node(-1),
    "add_edge_src": lambda db, i: db.add_edge(-1, i["etype"], i["b"]),
    "add_edge_dst": lambda db, i: db.add_edge(i["a"], i["etype"], -1),
    "add_edge_by_name": lambda db, i: db.add_edge_by_name(-1, "knows", i["b"]),
    "add_edges_batch": lambda db, i: db.add_edges_batch([(i["a"], i["etype"], -1)]),
    "add_edges_with_props_batch": lambda db, i: db.add_edges_with_props_batch(
        [(i["a"], i["etype"], -1, [])]
    ),
    "delete_edge": lambda db, i: db.delete_edge(i["a"], i["etype"], -1),
    "upsert_edge": lambda db, i: db.upsert_edge(-1, i["etype"], i["b"], []),
    "set_node_prop": lambda db, i: db.set_node_prop(-1, i["key"], PropValue.int(1)),
    "set_node_prop_by_name": lambda db, i: db.set_node_prop_by_name(
        -1, "age", PropValue.int(1)
    ),
    "delete_node_prop": lambda db, i: db.delete_node_prop(-1, i["key"]),
    "set_edge_prop": lambda db, i: db.set_edge_prop(
        i["a"], i["etype"], -1, i["key"], PropValue.int(1)
    ),
    "set_edge_prop_by_name": lambda db, i: db.set_edge_prop_by_name(
        -1, i["etype"], i["b"], "age", PropValue.int(1)
    ),
    "delete_edge_prop": lambda db, i: db.delete_edge_prop(i["a"], i["etype"], -1, i["key"]),
    "add_node_label": lambda db, i: db.add_node_label(-1, i["label"]),
    "add_node_label_by_name": lambda db, i: db.add_node_label_by_name(-1, "person"),
    "remove_node_label": lambda db, i: db.remove_node_label(-1, i["label"]),
    "set_node_vector": lambda db, i: db.set_node_vector(-1, i["vec"], [1.0, 0.0]),
    "delete_node_vector": lambda db, i: db.delete_node_vector(-1, i["vec"]),
}

_P1_READ_OPS = {
    "node_exists": lambda db, i: db.node_exists(-1),
    "get_node_key": lambda db, i: db.get_node_key(-1),
    "get_node_prop": lambda db, i: db.get_node_prop(-1, i["key"]),
    "get_out_edges": lambda db, i: db.get_out_edges(-1),
    "edge_exists": lambda db, i: db.edge_exists(i["a"], i["etype"], -1),
}


def test_audit_p1_upsert_node_by_id_negative_raises_value_error(tmp_path):
    db, _ = _open_low_level(tmp_path)
    try:
        nodes_before = db.count_nodes()
        db.begin()
        result, exc = _call(lambda: db.upsert_node_by_id(-1, []))
        db.rollback()
        assert isinstance(exc, ValueError), (
            f"upsert_node_by_id(-1) must raise ValueError, got {_describe(exc)} "
            f"(returned {result!r})"
        )
        assert db.count_nodes() == nodes_before
    finally:
        db.close()


def test_audit_p1_negative_id_does_not_corrupt_existing_nodes(tmp_path):
    db, ids = _open_low_level(tmp_path)
    try:
        db.begin()
        _call(lambda: db.upsert_node_by_id(-1, []))
        new_ids = [db.create_node(f"user:new{n}") for n in range(3)]
        db.commit()

        assert db.get_node_key(ids["a"]) == "user:a", (
            f"existing node {ids['a']} was overwritten after upsert_node_by_id(-1); "
            f"subsequent create_node returned {new_ids}"
        )
        assert db.get_node_key(ids["b"]) == "user:b"
        assert all(n >= 0 for n in new_ids), new_ids
        assert not set(new_ids) & {ids["a"], ids["b"]}, new_ids
    finally:
        db.close()


@pytest.mark.parametrize("op", sorted(_P1_WRITE_OPS))
def test_audit_p1_write_paths_reject_negative_ids(tmp_path, op):
    db, ids = _open_low_level(tmp_path)
    try:
        db.begin()
        result, exc = _call(lambda: _P1_WRITE_OPS[op](db, ids))
        if db.has_transaction():
            db.rollback()
        assert isinstance(exc, ValueError), (
            f"{op} with node id -1 must raise ValueError, got {_describe(exc)} "
            f"(returned {result!r})"
        )
    finally:
        db.close()


@pytest.mark.parametrize("op", sorted(_P1_READ_OPS))
def test_audit_p1_read_paths_reject_negative_ids(tmp_path, op):
    db, ids = _open_low_level(tmp_path)
    try:
        result, exc = _call(lambda: _P1_READ_OPS[op](db, ids))
        assert isinstance(exc, ValueError), (
            f"{op} with node id -1 must raise ValueError, got {_describe(exc)} "
            f"(returned {result!r})"
        )
    finally:
        db.close()


# ============================================================================
# P2: wrong-length vectors raise PanicException and poison the index lock
# ============================================================================

_P2_VECTORS = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
]
_P2_BAD = [1.0, 0.0, 0.0]  # 3 dims for a 4-dim index


def _manifest_json(vectors):
    dims = len(vectors[0])
    fragment = {
        "id": 0,
        "state": "Active",
        "row_groups": [
            {"id": 0, "count": len(vectors), "data": [x for v in vectors for x in v]}
        ],
        "total_vectors": len(vectors),
        "deletion_bitmap": [],
        "deleted_count": 0,
    }
    return json.dumps(
        {
            "config": {
                "dimensions": dims,
                "metric": "Euclidean",
                "row_group_size": 1024,
                "fragment_target_size": 100_000,
                "normalize_on_insert": False,
            },
            "fragments": [fragment],
            "active_fragment_id": 0,
            "total_vectors": len(vectors),
            "total_deleted": 0,
            "next_vector_id": len(vectors),
            "node_to_vector": {str(100 + i): i for i in range(len(vectors))},
            "vector_to_node": {str(i): 100 + i for i in range(len(vectors))},
            "vector_locations": {
                str(i): {"fragment_id": 0, "local_index": i} for i in range(len(vectors))
            },
        }
    )


def _trained_index(kind):
    ivf = IvfConfig(n_clusters=1, n_probe=1, metric="euclidean")
    if kind == "ivf":
        index = IvfIndex(4, ivf)
    else:
        index = IvfPqIndex(4, ivf, PqConfig(num_subspaces=2, num_centroids=2))
    index.add_training_vectors([x for v in _P2_VECTORS for x in v], len(_P2_VECTORS))
    index.train()
    for vector_id, vector in enumerate(_P2_VECTORS):
        index.insert(vector_id, vector)
    return index, _manifest_json(_P2_VECTORS)


@pytest.mark.parametrize("kind", ["ivf", "ivf_pq"])
def test_audit_p2_delete_wrong_length_raises_value_error(kind):
    index, _ = _trained_index(kind)
    _, exc = _call(lambda: index.delete(0, _P2_BAD))
    assert isinstance(exc, ValueError), (
        f"{kind}.delete with a wrong-length vector must raise ValueError, got {_describe(exc)}"
    )


@pytest.mark.parametrize("kind", ["ivf", "ivf_pq"])
def test_audit_p2_delete_wrong_length_keeps_index_usable(kind):
    index, manifest = _trained_index(kind)
    _call(lambda: index.delete(0, _P2_BAD))

    stats, exc = _call(index.stats)
    assert exc is None, f"{kind} unusable after rejected delete: {_describe(exc)}"
    assert stats.total_vectors == len(_P2_VECTORS)

    _, exc = _call(lambda: index.insert(99, [0.5, 0.5, 0.0, 0.0]))
    assert exc is None, f"{kind}.insert after rejected delete: {_describe(exc)}"
    hits, exc = _call(lambda: index.search(manifest, _P2_VECTORS[0], 1))
    assert exc is None, f"{kind}.search after rejected delete: {_describe(exc)}"
    assert len(hits) == 1


_P2_SEARCH_CALLS = {
    "ivf_search": ("ivf", lambda idx, m: idx.search(m, _P2_BAD, 1)),
    "ivf_search_multi": (
        "ivf",
        lambda idx, m: idx.search_multi(m, [_P2_VECTORS[0], _P2_BAD], 1, "min"),
    ),
    "ivf_pq_search": ("ivf_pq", lambda idx, m: idx.search(m, _P2_BAD, 1)),
    "ivf_pq_search_multi": (
        "ivf_pq",
        lambda idx, m: idx.search_multi(m, [_P2_VECTORS[0], _P2_BAD], 1, "min"),
    ),
}


@pytest.mark.parametrize("call", sorted(_P2_SEARCH_CALLS))
def test_audit_p2_search_wrong_length_raises_value_error(call):
    kind, fn = _P2_SEARCH_CALLS[call]
    index, manifest = _trained_index(kind)
    _, exc = _call(lambda: fn(index, manifest))
    assert isinstance(exc, ValueError), (
        f"{call} with a wrong-length query must raise ValueError, got {_describe(exc)}"
    )
    hits, exc = _call(lambda: index.search(manifest, _P2_VECTORS[0], 1))
    assert exc is None and len(hits) == 1, f"{call}: index unusable afterwards: {_describe(exc)}"


@pytest.mark.parametrize(
    "vectors,query",
    [
        pytest.param([[1.0, 0.0], [0.0, 1.0]], [1.0, 0.0, 0.0], id="query_too_long"),
        pytest.param([[1.0, 0.0], [1.0]], [1.0, 0.0], id="ragged_vectors"),
    ],
)
@pytest.mark.parametrize("metric", ["cosine", "euclidean", "dot"])
def test_audit_p2_brute_force_wrong_length_raises_value_error(vectors, query, metric):
    node_ids = list(range(1, len(vectors) + 1))
    _, exc = _call(lambda: brute_force_search(vectors, node_ids, query, 1, metric))
    assert isinstance(exc, ValueError), (
        f"brute_force_search with mismatched lengths must raise ValueError, got {_describe(exc)}"
    )


# ============================================================================
# P3: the GIL is never released during long native calls
# ============================================================================

_NEVER_TOKEN = "1000:1000"


def test_audit_p3_wait_for_token_releases_gil(tmp_path):
    db = Database(str(tmp_path / "p3.kitedb"))
    samples = []
    stop = threading.Event()

    def ticker():
        while not stop.is_set():
            samples.append(time.monotonic())
            time.sleep(0.001)

    thread = threading.Thread(target=ticker, daemon=True)
    thread.start()
    try:
        time.sleep(0.05)
        t0 = time.monotonic()
        observed = db.wait_for_token(_NEVER_TOKEN, 1500)
        t1 = time.monotonic()
    finally:
        stop.set()
        thread.join(timeout=5)
        db.close()

    assert observed is False
    # Ignore the edges of the window: a thread switch right before/after the
    # native call is allowed even with the GIL held.
    during = [s for s in samples if t0 + 0.2 <= s <= t1 - 0.2]
    assert len(during) >= 20, (
        f"pure-Python thread made {len(during)} iterations during the middle "
        f"{t1 - t0 - 0.4:.2f}s of wait_for_token: the GIL was held"
    )


@pytest.mark.parametrize("method", ["optimize", "vacuum"])
def test_audit_p3_maintenance_runs_during_long_native_call(tmp_path, method):
    # Contract for the fix: other threads run while wait_for_token blocks, and
    # optimize/vacuum (currently `&mut self`) don't fail with "Already borrowed"
    # once the GIL is released.
    db = Database(str(tmp_path / "p3.kitedb"))
    entered = threading.Event()
    timing = {}

    def long_call():
        entered.set()
        db.wait_for_token(_NEVER_TOKEN, 1500)
        timing["long_end"] = time.monotonic()

    worker = threading.Thread(target=long_call)
    worker.start()
    try:
        assert entered.wait(5)
        time.sleep(0.2)
        timing["maint_start"] = time.monotonic()
        _, exc = _call(getattr(db, method))
    finally:
        worker.join(timeout=10)
        db.close()

    assert exc is None, f"{method}() during wait_for_token raised {_describe(exc)}"
    assert timing["maint_start"] < timing["long_end"], (
        f"main thread only resumed {timing['maint_start'] - timing['long_end']:.3f}s "
        "after wait_for_token returned: the GIL was held for the whole call"
    )


# ============================================================================
# P4: VectorIndex drops hits whose NodeRef was evicted from the LRU cache
# ============================================================================


@pytest.mark.parametrize(
    "path_kind,extra",
    [
        pytest.param("brute_force", {}, id="brute_force"),
        pytest.param(
            "ivf",
            {"training_threshold": 3, "ivf": {"n_clusters": 1, "n_probe": 1}},
            id="ivf",
        ),
    ],
)
def test_audit_p4_vector_search_returns_hits_evicted_from_cache(tmp_path, path_kind, extra):
    user, knows = _schema()
    with kite(str(tmp_path / "p4.kitedb"), nodes=[user], edges=[knows]) as db:
        nodes = [
            db.insert(user).values(key=f"u{n}", name=f"U{n}", age=n).returning()
            for n in range(4)
        ]
        index = create_vector_index(
            VectorIndexOptions(dimensions=2, metric="euclidean", cache_max_size=2, **extra)
        )
        # nodes[0] is set first, so it is the one evicted from the 2-entry cache.
        index.set(nodes[0], [0.0, 0.0])
        index.set(nodes[1], [10.0, 0.0])
        index.set(nodes[2], [0.0, 10.0])
        index.set(nodes[3], [10.0, 10.0])
        index.build_index()
        assert index.stats()["indexTrained"] == (path_kind == "ivf")

        hits = index.search([0.1, 0.1], SimilarOptions(k=1))
        assert [h.node.id for h in hits] == [nodes[0].id], (
            f"true nearest neighbour {nodes[0].id} missing from hits "
            f"{[h.node.id for h in hits]}"
        )

        all_hits = index.search([0.1, 0.1], SimilarOptions(k=4))
        assert sorted(h.node.id for h in all_hits) == sorted(n.id for n in nodes)


# ============================================================================
# P5: interrupted fluent transaction leaks and later writes are lost
# ============================================================================


def _assert_write_survives_interrupt(path, user, knows, interrupt):
    with kite(path, nodes=[user], edges=[knows]) as db:
        with pytest.raises(KeyboardInterrupt):
            interrupt(db)
        leaked = db.in_transaction()
        db.insert(user).values(key="alice", name="Alice", age=30).returning()

    with kite(path, nodes=[user], edges=[knows]) as db:
        assert db.get(user, "alice") is not None, (
            "insert after the interrupted transaction was lost on reopen "
            f"(transaction still open after interrupt: {leaked})"
        )
        assert db.get(user, "ghost") is None
        assert not leaked


def test_audit_p5_transaction_interrupt_rolls_back(tmp_path):
    user, knows = _schema()

    def interrupt(db):
        with db.transaction():
            db.insert(user).values(key="ghost", name="Ghost", age=1).returning()
            raise KeyboardInterrupt

    _assert_write_survives_interrupt(str(tmp_path / "p5.kitedb"), user, knows, interrupt)


def test_audit_p5_batch_interrupt_rolls_back(tmp_path):
    user, knows = _schema()

    def boom():
        raise KeyboardInterrupt

    def interrupt(db):
        db.batch(
            [lambda: db.insert(user).values(key="ghost", name="Ghost", age=1).returning(), boom]
        )

    _assert_write_survives_interrupt(str(tmp_path / "p5.kitedb"), user, knows, interrupt)


def test_audit_p5_insert_builder_interrupt_rolls_back(tmp_path):
    def key_fn(key):
        if key == "boom":
            raise KeyboardInterrupt
        return f"user:{key}"

    user, knows = _schema(key_fn)

    def interrupt(db):
        db.insert(user).values(key="boom", name="Boom", age=1).returning()

    _assert_write_survives_interrupt(str(tmp_path / "p5.kitedb"), user, knows, interrupt)


# ============================================================================
# P6: brute_force_search cosine is wrong for unnormalized vectors
# ============================================================================


@pytest.mark.parametrize("metric", [None, "cosine"])
def test_audit_p6_brute_force_cosine_identical_vector_has_zero_distance(metric):
    (hit,) = brute_force_search([[3.0, 4.0]], [1], [3.0, 4.0], 1, metric)
    assert math.isclose(hit.distance, 0.0, abs_tol=1e-5), hit
    assert math.isclose(hit.similarity, 1.0, abs_tol=1e-5), hit


def test_audit_p6_brute_force_cosine_ranks_by_angle_not_magnitude():
    hits = brute_force_search([[10.0, 0.0], [1.0, 1.0]], [1, 2], [1.0, 1.0], 2)
    assert [h.node_id for h in hits] == [2, 1], [(h.node_id, h.distance) for h in hits]
    for hit in hits:
        assert 0.0 - 1e-5 <= hit.distance <= 2.0 + 1e-5, hit


@pytest.mark.parametrize(
    "vectors,query",
    [
        pytest.param([[1.0, 0.0]], [0.0, 0.0], id="zero_query"),
        pytest.param([[0.0, 0.0], [1.0, 0.0]], [1.0, 0.0], id="zero_candidate"),
    ],
)
def test_audit_p6_brute_force_cosine_rejects_zero_vector(vectors, query):
    node_ids = list(range(1, len(vectors) + 1))
    result, exc = _call(lambda: brute_force_search(vectors, node_ids, query, 1, "cosine"))
    assert isinstance(exc, ValueError), (
        f"cosine with a zero vector must raise ValueError, got {_describe(exc)} "
        f"(returned {[(h.node_id, h.distance) for h in result or []]})"
    )


# ============================================================================
# P7: traversal results differ between full and fast paths
# ============================================================================


def test_audit_p7_diamond_traversal_dedupes_on_every_path(tmp_path):
    user, knows = _schema()
    with kite(str(tmp_path / "p7.kitedb"), nodes=[user], edges=[knows]) as db:
        a, b, c, d = [
            db.insert(user).values(key=k, name=k.upper(), age=1).returning() for k in "abcd"
        ]
        for src, dst in [(a, b), (a, c), (b, d), (c, d)]:
            db.link(src, knows, dst, since=2020)

        two_hops = db.from_(a).out(knows).out(knows)
        assert two_hops.count() == 1
        assert two_hops.ids() == [d.id]

        assert [n.id for n in two_hops.to_list()] == [d.id]
        assert [n.id for n in two_hops.nodes()] == [d.id]
        assert [n.id for n in two_hops.select(["name"]).to_list()] == [d.id]
        filtered = two_hops.where_node(lambda n: True)
        assert [n.id for n in filtered.to_list()] == [d.id]
        assert filtered.count() == 1
