"""raydb-b4 `pathfinding` lane: the fluent shortest path reads about what a search from both
ends reads.

`db.shortest_path(a).via(e).to(b).bfs()` (and `find()`, `exists()`, `dijkstra()` without a
weight) walked the graph in Python from the source alone, one `get_out_edges` call per node,
building an edge result with its props for every edge it scanned: on a graph with hubs, most
of the source's neighborhood before reaching the target. The test counts the neighbor lists
the fluent search reads through Python and compares them with what a bidirectional BFS over
the same calls reads for the same answers.
"""

import os
import random
import tempfile

from kitedb import define_edge, define_node, kite, prop


class _CountingDb:
    """Forwards to a native Database, counting the neighbor lists read through it."""

    def __init__(self, db):
        self._inner = db
        self.lists = 0

    def __getattr__(self, name):
        attr = getattr(self._inner, name)
        if name not in ("get_out_edges", "get_in_edges"):
            return attr

        def counted(*args, **kwargs):
            self.lists += 1
            return attr(*args, **kwargs)

        return counted


def _bidirectional_hops(db, a, b, etype, max_hops):
    """Fewest hops from `a` to `b` over `etype` edges, by a level-at-a-time bidirectional BFS
    that expands the smaller frontier, or None."""
    if a == b:
        return 0
    forward, backward = {a: 0}, {b: 0}
    forward_level, backward_level = [a], [b]
    forward_depth = backward_depth = 0
    while forward_depth + backward_depth < max_hops and forward_level and backward_level:
        expand_forward = len(forward_level) <= len(backward_level)
        level = forward_level if expand_forward else backward_level
        seen, other = (forward, backward) if expand_forward else (backward, forward)
        depth = forward_depth if expand_forward else backward_depth
        best = None
        next_level = []
        for node in level:
            listed = db.get_out_edges(node) if expand_forward else db.get_in_edges(node)
            for edge in listed:
                if edge.etype != etype or edge.node_id in seen:
                    continue
                seen[edge.node_id] = depth + 1
                if edge.node_id in other:
                    hops = depth + 1 + other[edge.node_id]
                    best = hops if best is None else min(best, hops)
                next_level.append(edge.node_id)
        if expand_forward:
            forward_level, forward_depth = next_level, forward_depth + 1
        else:
            backward_level, backward_depth = next_level, backward_depth + 1
        if best is not None:
            return best
    return None


def test_fluent_shortest_path_reads_about_what_a_bidirectional_bfs_reads():
    user = define_node("user", key=lambda id: f"user:{id}", props={"name": prop.string("name")})
    knows = define_edge("knows", {"since": prop.int("since")})
    rng = random.Random(0xB4_0008)
    nodes, edges = 1500, 12000

    with tempfile.TemporaryDirectory() as tmpdir:
        with kite(os.path.join(tmpdir, "paths.kitedb"), nodes=[user], edges=[knows]) as db:
            refs = (
                db.insert(user)
                .values_many([{"key": f"n{i}", "name": f"n{i}"} for i in range(nodes)])
                .returning()
            )
            raw = db.raw
            etype = db._resolve_etype_id(knows)
            # Endpoints half uniform, half from a Zipf-like law: a few hubs, many leaves.
            weights = [1 / (rank + 1) ** 0.8 for rank in range(nodes)]

            def pick():
                if rng.random() < 0.5:
                    return rng.randrange(nodes)
                return rng.choices(range(nodes), weights)[0]

            pairs = {(refs[pick()].id, etype, refs[pick()].id) for _ in range(edges)}
            raw.begin()
            raw.add_edges_batch(sorted(pairs))
            raw.commit()

            counting = _CountingDb(raw)
            queries = [(rng.choice(refs), rng.choice(refs)) for _ in range(40)]
            reference = 0
            fluent = 0
            for a, b in queries:
                before = counting.lists
                expected = _bidirectional_hops(counting, a.id, b.id, etype, 6)
                reference += counting.lists - before

                searches = {
                    "bfs": lambda path: path.bfs(),
                    "find": lambda path: path.find(),
                    "dijkstra": lambda path: path.dijkstra(),
                    "to_any": lambda path: path.to_any([b, b]).bfs(),
                }
                for name, search in searches.items():
                    path = db.shortest_path(a).via(knows).to(b).max_depth(6)
                    path._db = counting
                    before = counting.lists
                    result = search(path)
                    fluent += counting.lists - before
                    assert result.found == (expected is not None), (name, a.id, b.id)
                    if result.found:
                        assert len(result.edges) == expected, (name, a.id, b.id)
                        ids = [node.id for node in result.nodes]
                        assert ids[0] == a.id and ids[-1] == b.id, (name, ids)
                        for edge, (src, dst) in zip(result.edges, zip(ids, ids[1:])):
                            assert (edge.src, edge.etype, edge.dst) == (src, etype, dst)
                            assert raw.edge_exists(src, etype, dst)
                        assert result.total_weight == float(expected)

                path = db.shortest_path(a).via(knows).to(b).max_depth(6)
                path._db = counting
                before = counting.lists
                assert path.exists() == (expected is not None)
                fluent += counting.lists - before

            # Five fluent searches per query, against one bidirectional BFS.
            assert fluent <= 2 * 5 * reference, (
                f"5 fluent searches each for {len(queries)} queries read {fluent} neighbor "
                f"lists in Python; a bidirectional BFS reads {reference} for them once"
            )
