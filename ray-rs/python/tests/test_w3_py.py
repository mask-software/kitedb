"""Wave-3 reproductions for the Python binding findings (lane `py`).

Each test encodes the contract the fix must satisfy and fails on the unfixed
code (base 4d57e73). Y5 (streaming memory), Y9 (numpy / packaging hygiene)
and Y13 (pyo3 bump) are perf/hygiene items and have no failing test here.

Run one finding with `.venv/bin/python -m pytest python/tests/test_w3_py.py -k y<N>`.
"""

from __future__ import annotations

import ast
import hmac
import importlib.util
import inspect
import json
import subprocess
import sys
import textwrap
import uuid
from pathlib import Path

import pytest

import kitedb
from kitedb import (
    Database,
    IvfConfig,
    IvfIndex,
    IvfPqIndex,
    OpenOptions,
    PqConfig,
    PropValue,
    TraverseOptions,
    VectorIndexOptions,
    brute_force_search,
    create_vector_index,
    edge,
    kite,
    node,
    prop,
)
from kitedb import _kitedb as native
from kitedb import fluent as fluent_mod

PY_ROOT = Path(__file__).resolve().parents[1]


def _schema():
    user = node(
        "user",
        key=lambda id: f"user:{id}",
        props={"name": prop.string("name"), "age": prop.int("age")},
    )
    knows = edge("knows", {"since": prop.int("since")})
    return user, knows


def _seed(path, etypes=(), propkeys=()):
    """Create a DB whose schema ids are offset by pre-registered names."""
    db = Database(str(path))
    db.begin()
    for name in etypes:
        db.get_or_create_etype(name)
    for name in propkeys:
        db.get_or_create_propkey(name)
    db.commit()
    db.close()


class _Boom(Exception):
    pass


# ============================================================================
# Y1: schema ids live on shared NodeDef/EdgeDef objects
# ============================================================================


def test_y1_second_kite_does_not_redirect_first_kites_traversal(tmp_path):
    # Same schema objects, two DBs whose `knows` etype ids differ. Opening the
    # second DB must not change which etype the first one traverses.
    user, knows = _schema()
    path_b = tmp_path / "b.kitedb"
    _seed(path_b, etypes=["pad0", "pad1"])
    db_a = kite(str(tmp_path / "a.kitedb"), nodes=[user], edges=[knows])
    db_b = kite(str(path_b), nodes=[user], edges=[knows])
    try:
        assert db_a.raw.get_etype_id("knows") != db_b.raw.get_etype_id("knows")
        alice = db_a.insert(user).values(key="alice", name="Alice").returning()
        bob = db_a.insert(user).values(key="bob", name="Bob").returning()
        db_a.link(alice, knows, bob, since=2020)
        assert db_a.has_edge(alice, knows, bob)

        assert db_a.from_(alice).out(knows).nodes().keys() == ["user:bob"], (
            "single-step traversal in the first Kite used the second Kite's etype id"
        )
        assert db_a.from_(alice).out(knows).count() == 1
        assert db_a.from_(bob).in_(knows).nodes().ids() == [alice.id]
        assert db_a.from_(alice).out(knows).out(knows).count() == 0
        two_hop = db_a.from_(bob).in_(knows).out(knows).nodes().ids()
        assert two_hop == [bob.id], "multi-step traversal used the second Kite's etype id"
    finally:
        db_b.close()
        db_a.close()


def test_y1_second_kite_does_not_redirect_first_kites_prop_writes(tmp_path):
    # Same schema objects, two DBs whose `name`/`age` prop key ids differ.
    # Writes through the first Kite must land on the first DB's prop keys.
    user, knows = _schema()
    path_a = tmp_path / "a.kitedb"
    path_b = tmp_path / "b.kitedb"
    _seed(path_b, propkeys=["pad0", "pad1", "pad2"])
    db_a = kite(str(path_a), nodes=[user], edges=[knows])
    db_b = kite(str(path_b), nodes=[user], edges=[knows])
    try:
        raw = db_a.raw
        assert raw.get_propkey_id("name") != db_b.raw.get_propkey_id("name")
        alice = db_a.insert(user).values(key="alice", name="Alice", age=30).returning()
        name = raw.get_node_prop(alice.id, raw.get_propkey_id("name"))
        age = raw.get_node_prop(alice.id, raw.get_propkey_id("age"))
        assert name is not None and name.value() == "Alice", (
            "insert in the first Kite wrote `name` under the second Kite's prop key id"
        )
        assert age is not None and age.value() == 30
    finally:
        db_b.close()
        db_a.close()

    # A fresh Kite on the first DB must read back what the first Kite wrote.
    with kite(str(path_a), nodes=[user], edges=[knows]) as db_a2:
        again = db_a2.get(user, "alice")
        assert again is not None
        assert (again.name, again.age) == ("Alice", 30)


# ============================================================================
# Y2: batch inserts fail inside a transaction or with MVCC
# ============================================================================

_ROWS = [{"key": "a", "name": "A"}, {"key": "b", "name": "B"}]


def test_y2_batch_insert_inside_transaction(tmp_path):
    user, knows = _schema()
    with kite(str(tmp_path / "db.kitedb"), nodes=[user], edges=[knows]) as db:
        with db.transaction():
            refs = db.insert(user).values([dict(r) for r in _ROWS]).returning()
        assert [r.key for r in refs] == ["user:a", "user:b"]
        assert db.get(user, "a").name == "A"
        assert db.get(user, "b").name == "B"


def test_y2_batch_insert_inside_batch_helper(tmp_path):
    user, knows = _schema()
    with kite(str(tmp_path / "db.kitedb"), nodes=[user], edges=[knows]) as db:
        db.batch([db.insert(user).values_many([dict(r) for r in _ROWS])])
        assert db.count(user) == 2


def test_y2_batch_insert_joins_the_open_transaction(tmp_path):
    # Inside a transaction the batch must be part of it: rolling back the
    # transaction removes the batch's nodes too.
    user, knows = _schema()
    with kite(str(tmp_path / "db.kitedb"), nodes=[user], edges=[knows]) as db:
        with pytest.raises(_Boom):
            with db.transaction():
                db.insert(user).values([dict(r) for r in _ROWS]).execute()
                raise _Boom()
        assert db.get(user, "a") is None
        assert db.get(user, "b") is None
        assert db.count(user) == 0


def test_y2_batch_insert_with_mvcc(tmp_path):
    user, knows = _schema()
    with kite(
        str(tmp_path / "db.kitedb"),
        nodes=[user],
        edges=[knows],
        options=OpenOptions(mvcc=True),
    ) as db:
        refs = db.insert(user).values_many([dict(r) for r in _ROWS]).returning()
        assert [r.key for r in refs] == ["user:a", "user:b"]
        assert db.get(user, "b").name == "B"


# ============================================================================
# Y3: fluent API can't open read-only DBs; init leaks; no node labels
# ============================================================================


def _make_graph(path):
    user, knows = _schema()
    with kite(str(path), nodes=[user], edges=[knows]) as db:
        alice = db.insert(user).values(key="alice", name="Alice", age=30).returning()
        bob = db.insert(user).values(key="bob", name="Bob", age=25).returning()
        db.link(alice, knows, bob, since=2020)
    return user, knows


def test_y3_kite_opens_read_only_database(tmp_path):
    path = tmp_path / "db.kitedb"
    user, knows = _make_graph(path)
    db = kite(str(path), nodes=[user], edges=[knows], options=OpenOptions(read_only=True))
    try:
        alice = db.get(user, "alice")
        assert alice is not None and alice.name == "Alice"
        assert db.from_(alice).out(knows).nodes().keys() == ["user:bob"]
    finally:
        db.close()


def test_y3_read_only_missing_schema_reports_read_only(tmp_path):
    # A schema entry missing from a read-only DB can't be created. The error
    # must say so, not be masked by the failing rollback of the init tx.
    # (Passes on the base code, where begin() itself raises read-only; it
    # guards the fix, which looks ids up first and opens a tx only on demand.)
    path = tmp_path / "db.kitedb"
    user, knows = _make_graph(path)
    follows = edge("follows")
    with pytest.raises(Exception) as info:
        kite(
            str(path),
            nodes=[user],
            edges=[knows, follows],
            options=OpenOptions(read_only=True),
        )
    message = str(info.value).lower()
    assert "read-only" in message or "read only" in message, (
        f"init error masked: {type(info.value).__name__}: {info.value}"
    )


class _InjectedFailure(Exception):
    pass


class _FailingDatabase:
    """Wraps a real Database; every schema/tx call except cleanup fails."""

    _PASSTHROUGH = {"rollback", "has_transaction", "is_open", "read_only", "path"}

    def __init__(self, path, options=None):
        self.real = Database(path, options)
        self.closed = False

    def __getattr__(self, name):
        if name in ("close", "close_with_checkpoint_if_wal_over"):
            real_close = getattr(self.real, name)

            def _close(*args, **kwargs):
                self.closed = True
                return real_close(*args, **kwargs)

            return _close
        if name in self._PASSTHROUGH:
            return getattr(self.real, name)

        def _fail(*args, **kwargs):
            raise _InjectedFailure(name)

        return _fail


def test_y3_init_failure_propagates_and_closes_database(tmp_path, monkeypatch):
    # Kite opens the DB via `fluent.Database`. When schema init fails, the
    # original error must propagate and the DB handle must be closed.
    created = []

    def factory(path, options=None):
        db = _FailingDatabase(path, options)
        created.append(db)
        return db

    monkeypatch.setattr(fluent_mod, "Database", factory)
    user, knows = _schema()
    try:
        with pytest.raises(_InjectedFailure):
            kite(str(tmp_path / "db.kitedb"), nodes=[user], edges=[knows])
        assert created, "Kite did not open the DB through fluent.Database"
        assert created[0].closed, "Kite leaked the Database handle after init failed"
    finally:
        for db in created:
            db.real.close()


def test_y3_kite_labels_nodes_with_their_type(tmp_path):
    # Rust/Node Kite define a label per node type and attach it on create.
    user, knows = _schema()
    with kite(str(tmp_path / "db.kitedb"), nodes=[user], edges=[knows]) as db:
        label_id = db.raw.get_label_id("user")
        assert label_id is not None, "Kite did not define a label for node type 'user'"
        single = db.insert(user).values(key="alice", name="Alice").returning()
        many = db.insert(user).values_many(
            [{"key": "bob", "name": "Bob"}, {"key": "carol", "name": "Carol"}]
        ).returning()
        for ref in [single, *many]:
            assert db.raw.node_has_label(ref.id, label_id), f"{ref.key} has no 'user' label"


# ============================================================================
# Y4: _kitedb.pyi drifted from the native module
# ============================================================================

STUB_PATH = PY_ROOT / "kitedb" / "_kitedb.pyi"


def _stub_module():
    return ast.parse(STUB_PATH.read_text())


def _stub_top_level():
    names = {}
    for item in _stub_module().body:
        if isinstance(item, (ast.ClassDef, ast.FunctionDef)):
            names[item.name] = item
        elif isinstance(item, ast.AnnAssign) and isinstance(item.target, ast.Name):
            names[item.target.id] = item
        elif isinstance(item, ast.Assign):
            for target in item.targets:
                if isinstance(target, ast.Name):
                    names[target.id] = item
    return names


def _stub_members(class_node):
    members = {}
    for item in class_node.body:
        if isinstance(item, ast.FunctionDef):
            members[item.name] = item
        elif isinstance(item, ast.AnnAssign) and isinstance(item.target, ast.Name):
            members[item.target.id] = item
    return members


def _runtime_public(obj):
    names = {name for name in dir(obj) if not name.startswith("_")}
    if isinstance(obj, type):
        # Members inherited from builtin bases (Exception.args, ...) aren't
        # the module's to declare.
        for base in obj.__mro__[1:]:
            if base.__module__ == "builtins":
                names -= set(dir(base))
    return names


def test_y4_stub_has_no_phantom_module_names():
    phantom = sorted(set(_stub_top_level()) - set(dir(native)))
    assert phantom == [], f"_kitedb.pyi declares names the native module lacks: {phantom}"


def test_y4_stub_declares_every_module_name():
    missing = sorted(_runtime_public(native) - set(_stub_top_level()))
    assert missing == [], f"_kitedb.pyi is missing native module names: {missing}"


def test_y4_stub_class_members_match_runtime():
    problems = []
    for name, stub_node in sorted(_stub_top_level().items()):
        runtime_cls = getattr(native, name, None)
        if not isinstance(stub_node, ast.ClassDef) or not isinstance(runtime_cls, type):
            continue
        stub = _stub_members(stub_node)
        runtime = set(dir(runtime_cls))
        phantom = sorted(m for m in stub if m not in runtime)
        missing = sorted(_runtime_public(runtime_cls) - set(stub))
        if phantom:
            problems.append(f"{name}: phantom {phantom}")
        if missing:
            problems.append(f"{name}: missing {missing}")
    assert problems == [], "stub members drifted:\n" + "\n".join(problems)


_NO_DEFAULT = object()
_UNKNOWN_DEFAULT = object()


def _literal_default(node):
    if node is None:
        return _NO_DEFAULT
    try:
        return ast.literal_eval(node)
    except ValueError:
        return _UNKNOWN_DEFAULT


def _stub_params(func):
    args = func.args
    positional = args.posonlyargs + args.args
    defaults = [None] * (len(positional) - len(args.defaults)) + list(args.defaults)
    params = [
        (arg.arg, _literal_default(default))
        for arg, default in zip(positional, defaults)
        if arg.arg not in ("self", "cls")
    ]
    params += [
        (arg.arg, _literal_default(default))
        for arg, default in zip(args.kwonlyargs, args.kw_defaults)
    ]
    return params


def _runtime_params(obj):
    try:
        signature = inspect.signature(obj)
    except (TypeError, ValueError):
        return None
    return [
        (p.name, _NO_DEFAULT if p.default is inspect.Parameter.empty else p.default)
        for p in signature.parameters.values()
        if p.name not in ("self", "cls", "$self", "$cls", "$type")
        and p.kind not in (inspect.Parameter.VAR_POSITIONAL, inspect.Parameter.VAR_KEYWORD)
    ]


def _same_params(expected, actual):
    if [name for name, _ in expected] != [name for name, _ in actual]:
        return False
    for (_, stub_default), (_, runtime_default) in zip(expected, actual):
        if (stub_default is _NO_DEFAULT) != (runtime_default is _NO_DEFAULT):
            return False
        if stub_default is _UNKNOWN_DEFAULT or stub_default is _NO_DEFAULT:
            continue
        if type(stub_default) is not type(runtime_default) or stub_default != runtime_default:
            return False
    return True


def _show(params):
    return [
        name if default is _NO_DEFAULT else f"{name}=" + ("..." if default is _UNKNOWN_DEFAULT else repr(default))
        for name, default in params
    ]


def _signature_pairs(name, stub_node, runtime_obj):
    if isinstance(stub_node, ast.FunctionDef):
        return [(name, stub_node, runtime_obj)]
    if not isinstance(stub_node, ast.ClassDef):
        return []
    pairs = []
    for member, member_node in _stub_members(stub_node).items():
        if not isinstance(member_node, ast.FunctionDef):
            continue
        if any(isinstance(d, ast.Name) and d.id == "property" for d in member_node.decorator_list):
            continue
        if member == "__init__":
            # The constructor's runtime signature is the class's (#[new]).
            pairs.append((f"{name}.__init__", member_node, runtime_obj))
        elif not member.startswith("__"):
            pairs.append((f"{name}.{member}", member_node, getattr(runtime_obj, member, None)))
    return pairs


def test_y4_stub_signatures_match_runtime():
    # Parameter names, which ones are optional, and literal default values
    # must match the native __text_signature__ (e.g. should_checkpoint's
    # threshold=0.5, optimize(options), has_path(direction)).
    problems = []
    for name, stub_node in sorted(_stub_top_level().items()):
        runtime_obj = getattr(native, name, None)
        if runtime_obj is None:
            continue
        for label, func, runtime in _signature_pairs(name, stub_node, runtime_obj):
            if runtime is None:
                continue
            actual = _runtime_params(runtime)
            if actual is None:
                continue
            expected = _stub_params(func)
            if not _same_params(expected, actual):
                problems.append(f"{label}: stub {_show(expected)} != runtime {_show(actual)}")
    assert problems == [], "stub signatures drifted:\n" + "\n".join(problems)


# ============================================================================
# Y6: InsertExecutor mutates the caller's dict
# ============================================================================


def test_y6_insert_does_not_mutate_callers_dict(tmp_path):
    user, knows = _schema()
    with kite(str(tmp_path / "db.kitedb"), nodes=[user], edges=[knows]) as db:
        data = {"key": "alice", "name": "Alice"}
        db.insert(user).values(data).execute()
        assert data == {"key": "alice", "name": "Alice"}

        rows = [{"key": "bob", "name": "Bob"}]
        db.insert(user).values(rows).execute()
        assert rows == [{"key": "bob", "name": "Bob"}]


def test_y6_returned_ref_props_do_not_alias_callers_dict(tmp_path):
    user, knows = _schema()
    with kite(str(tmp_path / "db.kitedb"), nodes=[user], edges=[knows]) as db:
        data = {"key": "alice", "name": "Alice"}
        ref = db.insert(user).values(data).returning()
        data["name"] = "Mallory"
        assert ref.name == "Alice"
        assert "key" not in ref.props


def test_y6_executor_can_be_retried_after_rollback(tmp_path):
    user, knows = _schema()
    with kite(str(tmp_path / "db.kitedb"), nodes=[user], edges=[knows]) as db:
        insert = db.insert(user).values(key="alice", name="Alice")
        with pytest.raises(_Boom):
            with db.transaction():
                insert.execute()
                raise _Boom()
        assert db.get(user, "alice") is None
        insert.execute()  # retry the same executor
        again = db.get(user, "alice")
        assert again is not None and again.name == "Alice"


# ============================================================================
# Y7: unknown string options silently fall back to defaults
# ============================================================================


@pytest.fixture
def chain_db(tmp_path):
    db = Database(str(tmp_path / "db.kitedb"))
    db.begin()
    a = db.create_node("n:a")
    b = db.create_node("n:b")
    etype = db.get_or_create_etype("next")
    db.add_edge(a, etype, b)
    db.commit()
    yield db, a, b, etype
    db.close()


_BAD_DIRECTION_CALLS = {
    "traverse": lambda db, a, b, e: db.traverse(a, 1, direction="sideways"),
    "find_path_bfs": lambda db, a, b, e: db.find_path_bfs(a, b, direction="sideways"),
    "find_path_dijkstra": lambda db, a, b, e: db.find_path_dijkstra(a, b, direction="sideways"),
    "has_path": lambda db, a, b, e: db.has_path(a, b, direction="sideways"),
    "traverse_multi": lambda db, a, b, e: db.traverse_multi([a], [("sideways", e)]),
    "traverse_multi_count": lambda db, a, b, e: db.traverse_multi_count([a], [("sideways", e)]),
}


@pytest.mark.parametrize("op", sorted(_BAD_DIRECTION_CALLS))
def test_y7_unknown_direction_raises_value_error(chain_db, op):
    db, a, b, etype = chain_db
    with pytest.raises(ValueError, match="(?i)direction"):
        _BAD_DIRECTION_CALLS[op](db, a, b, etype)


def test_y7_fluent_unknown_path_direction_raises_value_error(tmp_path):
    user, knows = _schema()
    with kite(str(tmp_path / "db.kitedb"), nodes=[user], edges=[knows]) as db:
        alice = db.insert(user).values(key="alice", name="Alice").returning()
        bob = db.insert(user).values(key="bob", name="Bob").returning()
        db.link(alice, knows, bob, since=2020)
        with pytest.raises(ValueError, match="(?i)direction"):
            db.shortest_path(alice).via(knows).to(bob).direction("sideways").find()


def test_y7_fluent_unknown_traverse_direction_raises_value_error(tmp_path):
    user, knows = _schema()
    with kite(str(tmp_path / "db.kitedb"), nodes=[user], edges=[knows]) as db:
        alice = db.insert(user).values(key="alice", name="Alice").returning()
        bob = db.insert(user).values(key="bob", name="Bob").returning()
        db.link(alice, knows, bob, since=2020)
        with pytest.raises(ValueError, match="(?i)direction"):
            db.from_(alice).traverse(
                knows, TraverseOptions(max_depth=1, direction="sideways")
            ).nodes().to_list()


def test_y7_unknown_ivf_metric_raises_value_error():
    with pytest.raises(ValueError, match="(?i)metric"):
        IvfIndex(4, IvfConfig(n_clusters=1, metric="manhattan"))
    with pytest.raises(ValueError, match="(?i)metric"):
        IvfPqIndex(
            4,
            IvfConfig(n_clusters=1, metric="manhattan"),
            PqConfig(num_subspaces=2, num_centroids=2),
        )


def test_y7_unknown_brute_force_metric_raises_value_error():
    with pytest.raises(ValueError, match="(?i)metric"):
        brute_force_search([[1.0, 0.0], [0.0, 1.0]], [1, 2], [1.0, 0.0], 1, metric="manhattan")


def test_y7_unknown_vector_index_metric_raises_value_error():
    with pytest.raises(ValueError, match="(?i)metric"):
        create_vector_index(VectorIndexOptions(dimensions=4, metric="manhattan"))


_VECTORS = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
]


def _manifest_json(vectors):
    fragment = {
        "id": 0,
        "state": "Active",
        "row_groups": [{"id": 0, "count": len(vectors), "data": [x for v in vectors for x in v]}],
        "total_vectors": len(vectors),
        "deletion_bitmap": [],
        "deleted_count": 0,
    }
    return json.dumps(
        {
            "config": {
                "dimensions": len(vectors[0]),
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


@pytest.mark.parametrize("kind", ["ivf", "ivf_pq"])
def test_y7_unknown_aggregation_raises_value_error(kind):
    ivf = IvfConfig(n_clusters=1, n_probe=1, metric="euclidean")
    if kind == "ivf":
        index = IvfIndex(4, ivf)
    else:
        index = IvfPqIndex(4, ivf, PqConfig(num_subspaces=2, num_centroids=2))
    index.add_training_vectors([x for v in _VECTORS for x in v], len(_VECTORS))
    index.train()
    for vector_id, vector in enumerate(_VECTORS):
        index.insert(vector_id, vector)
    manifest = _manifest_json(_VECTORS)
    # Sanity: a known aggregation works on this index and manifest.
    assert index.search_multi(manifest, [_VECTORS[0], _VECTORS[1]], 1, "min")
    with pytest.raises(ValueError, match="(?i)aggregation"):
        index.search_multi(manifest, [_VECTORS[0], _VECTORS[1]], 1, "median")


# ============================================================================
# Y8: Kite.check() drops the warnings it adds
# ============================================================================


def test_y8_kite_check_keeps_its_own_warnings(tmp_path):
    # Simulate an edge type with no assigned id (the condition Kite.check
    # warns about), then check that the warning reaches the caller.
    user, knows = _schema()
    with kite(str(tmp_path / "db.kitedb"), nodes=[user], edges=[knows]) as db:
        db._etype_ids.pop(knows, None)
        if hasattr(knows, "_etype_id"):
            knows._etype_id = None
        result = db.check()
        assert any("knows" in warning for warning in result.warnings), (
            f"Kite.check() dropped its warning: {list(result.warnings)}"
        )
        assert result.has_warnings()
        assert result.warning_count() == len(result.warnings)


# ============================================================================
# Y10: replication admin auth: timing-unsafe token compare, XFCC trust
# ============================================================================

AUTH_PATH = PY_ROOT / "kitedb" / "replication_auth.py"


def _load_auth():
    """Load a fresh copy so `from hmac import compare_digest` sees patches."""
    name = f"_w3_replication_auth_{uuid.uuid4().hex}"
    spec = importlib.util.spec_from_file_location(name, AUTH_PATH)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


class _Request:
    def __init__(self, headers=None, scope=None):
        self.headers = headers or {}
        self.scope = scope or {}


def test_y10_token_compare_is_constant_time(monkeypatch):
    calls = []
    real = hmac.compare_digest

    def spy(a, b):
        calls.append((a, b))
        return real(a, b)

    monkeypatch.setattr(hmac, "compare_digest", spy)
    auth = _load_auth()
    config = auth.ReplicationAdminAuthConfig(mode="token", token="abc123")
    assert auth.is_replication_admin_authorized(
        _Request(headers={"authorization": "Bearer abc123"}), config
    )
    assert not auth.is_replication_admin_authorized(
        _Request(headers={"authorization": "Bearer wrong!"}), config
    )
    assert calls, "token was not compared with hmac.compare_digest"


@pytest.mark.parametrize("mode", ["mtls", "token_or_mtls"])
def test_y10_untrusted_xfcc_header_does_not_authorize(mode):
    # A client-supplied x-forwarded-client-cert header must not authorize
    # unless forwarded certs are explicitly trusted.
    auth = _load_auth()
    config = auth.ReplicationAdminAuthConfig(mode=mode, token="abc123")
    request = _Request(headers={"x-forwarded-client-cert": "CN=attacker"})
    assert not auth.is_replication_admin_authorized(request, config)


def test_y10_trusted_xfcc_without_subject_match_is_denied():
    auth = _load_auth()
    try:
        config = auth.ReplicationAdminAuthConfig(mode="mtls", trust_forwarded_client_cert=True)
    except TypeError:
        pytest.fail("ReplicationAdminAuthConfig has no trust_forwarded_client_cert option")
    request = _Request(headers={"x-forwarded-client-cert": "CN=attacker"})
    try:
        allowed = auth.is_replication_admin_authorized(request, config)
    except ValueError:
        return  # rejecting the config outright is also acceptable
    assert not allowed, "trusted XFCC with no subject matcher must deny"


def test_y10_trusted_xfcc_subject_match_is_anchored():
    auth = _load_auth()
    try:
        config = auth.ReplicationAdminAuthConfig(
            mode="mtls",
            trust_forwarded_client_cert=True,
            mtls_subject_regex=r"CN=replication-admin,O=RayDB",
        )
    except TypeError:
        pytest.fail("ReplicationAdminAuthConfig has no trust_forwarded_client_cert option")
    good = _Request(headers={"x-forwarded-client-cert": "CN=replication-admin,O=RayDB"})
    spoofed = _Request(
        headers={"x-forwarded-client-cert": "CN=attacker,O=Evil;x=CN=replication-admin,O=RayDB"}
    )
    assert auth.is_replication_admin_authorized(good, config)
    assert not auth.is_replication_admin_authorized(spoofed, config), (
        "unanchored subject regex matched inside an attacker-controlled value"
    )


# ============================================================================
# Y11: every native error is a bare RuntimeError
# ============================================================================

_ERROR_NAMES = ["KiteError", "ConflictError", "ReadOnlyError", "NotFoundError", "ClosedError"]


def _error_class(name):
    cls = getattr(kitedb, name, None)
    if cls is None:
        pytest.fail(f"kitedb.{name} is not defined")
    return cls


def test_y11_exception_hierarchy_is_exported():
    base = _error_class("KiteError")
    assert issubclass(base, RuntimeError), "KiteError must subclass RuntimeError"
    for name in _ERROR_NAMES:
        cls = _error_class(name)
        assert issubclass(cls, base), f"{name} must subclass KiteError"
        assert getattr(native, name, None) is cls, f"_kitedb.{name} must be the same class"


def test_y11_closed_database_raises_closed_error(tmp_path):
    db = Database(str(tmp_path / "db.kitedb"))
    db.close()
    with pytest.raises(_error_class("ClosedError")):
        db.count_nodes()


def test_y11_read_only_write_raises_read_only_error(tmp_path):
    path = str(tmp_path / "db.kitedb")
    Database(path).close()
    db = Database(path, OpenOptions(read_only=True))
    try:
        with pytest.raises(_error_class("ReadOnlyError")):
            db.begin()
            db.create_node("user:x")
    finally:
        if db.has_transaction():
            db.rollback()
        db.close()


def test_y11_missing_node_raises_not_found_error(tmp_path):
    # add_edge requires both endpoints (core NodeNotFound otherwise).
    db = Database(str(tmp_path / "db.kitedb"))
    try:
        db.begin()
        src = db.create_node("user:a")
        etype = db.get_or_create_etype("knows")
        with pytest.raises(_error_class("NotFoundError"), match="(?i)not found"):
            db.add_edge(src, etype, 424242)
    finally:
        if db.has_transaction():
            db.rollback()
        db.close()


def test_y11_mvcc_write_conflict_raises_conflict_error(tmp_path):
    import threading

    db = Database(str(tmp_path / "db.kitedb"), OpenOptions(mvcc=True))
    try:
        db.begin()
        node_id = db.create_node("user:a")
        key = db.get_or_create_propkey("name")
        db.commit()

        first_open = threading.Event()
        second_done = threading.Event()
        outcome = {}

        def first():
            db.begin()
            db.set_node_prop(node_id, key, PropValue.string("first"))
            first_open.set()
            second_done.wait(10)
            try:
                db.commit()
                outcome["first"] = None
            except BaseException as exc:  # noqa: BLE001
                outcome["first"] = exc
                if db.has_transaction():
                    db.rollback()

        def second():
            first_open.wait(10)
            try:
                db.begin()
                db.set_node_prop(node_id, key, PropValue.string("second"))
                db.commit()
                outcome["second"] = None
            except BaseException as exc:  # noqa: BLE001
                outcome["second"] = exc
                if db.has_transaction():
                    db.rollback()
            finally:
                second_done.set()

        threads = [threading.Thread(target=first), threading.Thread(target=second)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(30)
        errors = [exc for exc in outcome.values() if exc is not None]
        assert errors, f"expected a write conflict, got {outcome}"
        conflict = _error_class("ConflictError")
        assert all(isinstance(exc, conflict) for exc in errors), (
            f"conflict raised {[type(e).__name__ + ': ' + str(e) for e in errors]}"
        )
        assert all(isinstance(exc, RuntimeError) for exc in errors)
    finally:
        db.close()


# ============================================================================
# Y12: close() deadlocks while another thread has an open transaction
# ============================================================================

_CLOSE_SCENARIO = textwrap.dedent(
    """
    import json, sys, threading, time
    from kitedb import Database, kite, node, edge, prop

    path, how = sys.argv[1], sys.argv[2]
    user = node("user", key=lambda id: f"user:{id}", props={"name": prop.string("name")})
    knows = edge("knows")
    if how == "kite":
        handle = kite(path, nodes=[user], edges=[knows],
                      close_checkpoint_if_wal_usage_at_least=0.0)
        db = handle.raw
    else:
        handle = db = Database(path)
    for i in range(20):  # put something in the WAL so a close checkpoint runs
        db.begin()
        db.create_node(f"user:seed-{i}")
        db.commit()

    in_tx = threading.Event()
    outcome = {}

    def writer():
        db.begin()
        db.create_node("user:writer")
        in_tx.set()
        time.sleep(0.3)  # close() now runs while this transaction is open
        try:
            db.commit()
            outcome["commit"] = "ok"
        except Exception as exc:
            outcome["commit"] = f"{type(exc).__name__}: {exc}"

    thread = threading.Thread(target=writer)
    thread.start()
    in_tx.wait()
    try:
        if how == "close":
            handle.close()
        elif how == "close_with_checkpoint":
            handle.close_with_checkpoint_if_wal_over(0.0)
        else:
            handle.close()
        outcome["close"] = "ok"
    except Exception as exc:
        outcome["close"] = f"{type(exc).__name__}: {exc}"
    thread.join()
    print(json.dumps(outcome))
    """
)


@pytest.mark.parametrize("how", ["close", "close_with_checkpoint", "kite"])
def test_y12_close_does_not_deadlock_against_open_transaction(tmp_path, how):
    # close() with a close-time checkpoint holds the binding's write lock while
    # core checkpoint waits for the other thread's transaction, which needs
    # that lock to commit. Either refuse or wait, but never hang.
    try:
        proc = subprocess.run(
            [sys.executable, "-c", _CLOSE_SCENARIO, str(tmp_path / "db.kitedb"), how],
            capture_output=True,
            text=True,
            timeout=30,
        )
    except subprocess.TimeoutExpired:
        pytest.fail(f"{how}: close() deadlocked against a transaction on another thread")
    assert proc.returncode == 0, proc.stderr
    outcome = json.loads(proc.stdout.strip().splitlines()[-1])
    if outcome["close"] != "ok":
        # close refused: the open transaction must still be able to commit.
        assert outcome["commit"] == "ok", outcome
