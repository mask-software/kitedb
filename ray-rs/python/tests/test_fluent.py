import os
import tempfile

from raydb import (
    TraverseOptions,
    define_edge,
    define_node,
    prop,
    ray,
    create_vector_index,
    VectorIndexOptions,
    SimilarOptions,
)


def _build_schema():
    user = define_node(
        "user",
        key=lambda id: f"user:{id}",
        props={
            "name": prop.string("name"),
            "age": prop.int("age"),
        },
    )

    knows = define_edge(
        "knows",
        {
            "since": prop.int("since"),
        },
    )

    return user, knows


def test_traversal_select_edges():
    user, knows = _build_schema()

    with tempfile.TemporaryDirectory() as tmpdir:
        path = os.path.join(tmpdir, "fluent.raydb")
        with ray(path, nodes=[user], edges=[knows]) as db:
            alice = db.insert(user).values(key="alice", name="Alice", age=30).returning()
            bob = db.insert(user).values(key="bob", name="Bob", age=25).returning()
            db.link(alice, knows, bob, since=2020)

            friends = db.from_(alice).out(knows).select(["name"]).to_list()
            assert len(friends) == 1
            assert friends[0].name == "Bob"
            assert friends[0].age is None

            edges = db.from_(alice).out(knows).edges().to_list()
            assert len(edges) == 1
            assert edges[0].props.get("since") == 2020
            assert edges[0]["$src"] == alice.id
            assert edges[0]["$dst"] == bob.id
            assert edges[0]["$etype"] == knows._etype_id

            recent = (
                db.from_(alice)
                .out(knows)
                .where_edge(lambda e: e.props.get("since", 0) >= 2020)
                .to_list()
            )
            assert len(recent) == 1


def test_traverse_variable_depth():
    user, knows = _build_schema()

    with tempfile.TemporaryDirectory() as tmpdir:
        path = os.path.join(tmpdir, "fluent.raydb")
        with ray(path, nodes=[user], edges=[knows]) as db:
            a = db.insert(user).values(key="a", name="A", age=1).returning()
            b = db.insert(user).values(key="b", name="B", age=2).returning()
            c = db.insert(user).values(key="c", name="C", age=3).returning()
            d = db.insert(user).values(key="d", name="D", age=4).returning()

            db.link(a, knows, b, since=2020)
            db.link(b, knows, c, since=2021)
            db.link(c, knows, d, since=2022)

            results = db.from_(a).traverse(
                knows,
                TraverseOptions(max_depth=2),
            ).to_list()
            keys = {node.key for node in results}
            assert "user:b" in keys
            assert "user:c" in keys
            assert "user:d" not in keys


def test_traverse_options_filters():
    user, knows = _build_schema()

    with tempfile.TemporaryDirectory() as tmpdir:
        path = os.path.join(tmpdir, "fluent.raydb")
        with ray(path, nodes=[user], edges=[knows]) as db:
            a = db.insert(user).values(key="a", name="A", age=1).returning()
            b = db.insert(user).values(key="b", name="B", age=2).returning()
            c = db.insert(user).values(key="c", name="C", age=3).returning()
            d = db.insert(user).values(key="d", name="D", age=4).returning()

            db.link(a, knows, b, since=2020)
            db.link(a, knows, c, since=2022)
            db.link(c, knows, d, since=2022)

            edge_filtered = db.from_(a).traverse(
                knows,
                TraverseOptions(
                    max_depth=2,
                    where_edge=lambda e: e.props.get("since", 0) >= 2022,
                ),
            ).to_list()
            edge_keys = {node.key for node in edge_filtered}
            assert "user:c" in edge_keys
            assert "user:d" in edge_keys
            assert "user:b" not in edge_keys

            node_filtered = db.from_(a).traverse(
                knows,
                TraverseOptions(
                    max_depth=2,
                    where_node=lambda n: n.age is not None and n.age >= 3,
                ),
            ).to_list()
            node_keys = {node.key for node in node_filtered}
            assert "user:c" in node_keys
            assert "user:d" in node_keys
            assert "user:b" not in node_keys


def test_raw_edges():
    user, knows = _build_schema()

    with tempfile.TemporaryDirectory() as tmpdir:
        path = os.path.join(tmpdir, "fluent.raydb")
        with ray(path, nodes=[user], edges=[knows]) as db:
            a = db.insert(user).values(key="a", name="A", age=1).returning()
            b = db.insert(user).values(key="b", name="B", age=2).returning()
            db.link(a, knows, b, since=2020)

            edges = list(db.from_(a).out(knows).raw_edges())
            assert len(edges) == 1
            assert edges[0].src == a.id
            assert edges[0].dst == b.id


def test_pathfinding_weight_and_a_star():
    user, knows = _build_schema()

    with tempfile.TemporaryDirectory() as tmpdir:
        path = os.path.join(tmpdir, "fluent.raydb")
        with ray(path, nodes=[user], edges=[knows]) as db:
            a = db.insert(user).values(key="a", name="A", age=1).returning()
            b = db.insert(user).values(key="b", name="B", age=2).returning()
            c = db.insert(user).values(key="c", name="C", age=3).returning()
            d = db.insert(user).values(key="d", name="D", age=4).returning()

            db.link(a, knows, b, since=5)
            db.link(b, knows, c, since=5)
            db.link(a, knows, d, since=1)
            db.link(d, knows, c, since=1)

            weighted = db.shortest_path(a).via(knows).weight("since").to(c).dijkstra()
            assert weighted.found
            assert weighted.total_weight == 2.0

            a_star = db.shortest_path(a).via(knows).to(c).a_star(lambda n, goal: 0)
            assert a_star.found


def test_to_any_and_all_edges():
    user, knows = _build_schema()

    with tempfile.TemporaryDirectory() as tmpdir:
        path = os.path.join(tmpdir, "fluent.raydb")
        with ray(path, nodes=[user], edges=[knows]) as db:
            a = db.insert(user).values(key="a", name="A", age=1).returning()
            b = db.insert(user).values(key="b", name="B", age=2).returning()
            c = db.insert(user).values(key="c", name="C", age=3).returning()

            db.link(a, knows, b, since=2020)
            db.link(b, knows, c, since=2021)

            path_result = db.shortest_path(a).via(knows).to_any([b, c]).bfs()
            assert path_result.found
            assert path_result.nodes[-1].key in {"user:b", "user:c"}

            edges = list(db.all_edges(knows))
            assert len(edges) == 2


def test_fluent_check():
    user, knows = _build_schema()

    with tempfile.TemporaryDirectory() as tmpdir:
        path = os.path.join(tmpdir, "fluent.raydb")
        with ray(path, nodes=[user], edges=[knows]) as db:
            alice = db.insert(user).values(key="alice", name="Alice", age=30).returning()
            bob = db.insert(user).values(key="bob", name="Bob", age=25).returning()
            db.link(alice, knows, bob, since=2020)

            result = db.check()
            assert result.valid
            assert result.errors == []


def test_vector_index_search():
    user, knows = _build_schema()

    with tempfile.TemporaryDirectory() as tmpdir:
        path = os.path.join(tmpdir, "fluent.raydb")
        with ray(path, nodes=[user], edges=[knows]) as db:
            alice = db.insert(user).values(key="alice", name="Alice", age=30).returning()
            bob = db.insert(user).values(key="bob", name="Bob", age=25).returning()

            index = create_vector_index(
                VectorIndexOptions(dimensions=2, metric="cosine", ivf={"n_probe": 2})
            )
            index.set(alice, [1.0, 0.0])
            index.set(bob, [0.0, 1.0])
            index.build_index()

            hits = index.search([1.0, 0.0], SimilarOptions(k=1))
            assert len(hits) == 1
            assert hits[0].node.id == alice.id
            assert index.has(alice)
            assert index.delete(bob)
            assert index.stats()["totalVectors"] == 1


def test_insert_values_list():
    user, knows = _build_schema()

    with tempfile.TemporaryDirectory() as tmpdir:
        path = os.path.join(tmpdir, "fluent.raydb")
        with ray(path, nodes=[user], edges=[knows]) as db:
            results = db.insert(user).values([
                {"key": "alice", "name": "Alice", "age": 30},
                {"key": "bob", "name": "Bob", "age": 25},
            ]).returning()

            assert len(results) == 2
            assert {node.key for node in results} == {"user:alice", "user:bob"}
