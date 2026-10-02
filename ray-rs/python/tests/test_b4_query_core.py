"""raydb-b4 `query-core` lane: streams and pages walk the database by cursor.

A page seeks to its cursor, and a stream reads ahead a batch at a time, so
neither lists the whole database. Run with
`.venv/bin/python -m pytest python/tests/test_b4_query_core.py`.
"""

from __future__ import annotations

from kitedb import Database, PaginationOptions, StreamOptions


def _graph(db: Database, count: int):
    db.begin()
    ids = db.create_nodes_batch([f"n:{i}" for i in range(count)])
    etype = db.get_or_create_etype("next")
    db.add_edges_batch([(ids[i], etype, ids[(i * 7 + 1) % count]) for i in range(count)])
    db.commit()
    db.checkpoint()
    return ids, etype


def test_streams_walk_in_order_and_see_later_nodes(tmp_path):
    db = Database(str(tmp_path / "db.kitedb"))
    try:
        ids, etype = _graph(db, 50)

        stream = db.stream_nodes(StreamOptions(batch_size=10))
        first = next(stream)
        assert first == sorted(ids)[:10]
        # The stream holds a cursor, not a listing: a node created past it is read.
        db.begin()
        late = db.create_node("late")
        db.commit()
        rest = [node_id for batch in stream for node_id in batch]
        assert first + rest == sorted(ids) + [late]

        edges = [(e.src, e.etype, e.dst) for b in db.stream_edges(StreamOptions(batch_size=7)) for e in b]
        expected = sorted((ids[i], etype, ids[(i * 7 + 1) % 50]) for i in range(50))
        assert edges == expected
    finally:
        db.close()


def test_pages_report_totals_and_resume(tmp_path):
    db = Database(str(tmp_path / "db.kitedb"))
    try:
        ids, _ = _graph(db, 30)
        seen = []
        cursor = None
        while True:
            page = db.get_nodes_page(PaginationOptions(limit=8, cursor=cursor))
            assert page.total == 30
            seen.extend(page.items)
            if not page.has_more:
                break
            cursor = page.next_cursor
        assert seen == sorted(ids)

        edge_page = db.get_edges_page(PaginationOptions(limit=4))
        assert edge_page.total == 30
        assert len(edge_page.items) == 4 and edge_page.has_more
    finally:
        db.close()
