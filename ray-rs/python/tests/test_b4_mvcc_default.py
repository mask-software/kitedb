"""b4 mvcc-default lane: MVCC is the default for ``Database`` and ``kite()``;
``OpenOptions(mvcc=False)`` (deprecated) still opts out. Bulk loads work under
MVCC, and the batch helpers use them.
"""

from __future__ import annotations

import pytest

from kitedb import Database, OpenOptions, edge, kite, node, prop


def _schema():
    user = node("user", key=lambda id: f"user:{id}", props={"name": prop.string("name")})
    knows = edge("knows", {})
    return user, knows


def test_database_enables_mvcc_by_default(tmp_path):
    db = Database(str(tmp_path / "default.kitedb"))
    try:
        assert db.stats().mvcc_stats is not None
    finally:
        db.close()

    plain = Database(str(tmp_path / "plain.kitedb"), OpenOptions(mvcc=False))
    try:
        assert plain.stats().mvcc_stats is None
    finally:
        plain.close()


def test_open_options_default_leaves_mvcc_to_the_engine_default(tmp_path):
    options = OpenOptions()
    assert options.mvcc is None
    db = Database(str(tmp_path / "options.kitedb"), options)
    try:
        assert db.stats().mvcc_stats is not None
    finally:
        db.close()


def test_kite_enables_mvcc_by_default(tmp_path):
    user, knows = _schema()
    with kite(str(tmp_path / "kite.kitedb"), nodes=[user], edges=[knows]) as db:
        assert db.raw.stats().mvcc_stats is not None


def test_begin_bulk_works_under_the_default(tmp_path):
    db = Database(str(tmp_path / "bulk.kitedb"))
    try:
        db.begin_bulk()
        db.create_node("bulk-a")
        db.commit()
        assert db.get_node_by_key("bulk-a") is not None
    finally:
        db.close()


def test_fluent_bulk_works_under_the_default(tmp_path):
    user, knows = _schema()
    with kite(str(tmp_path / "fluent-bulk.kitedb"), nodes=[user], edges=[knows]) as db:
        refs = db.bulk([lambda: db.insert(user).values(key="a", name="A").returning()])
        assert refs[0].key == "user:a"
        assert db.get(user, "a").name == "A"


class _RefusingBulk:
    """Delegates to a Database, but refuses bulk loads and notes begins."""

    def __init__(self, inner):
        self._inner = inner
        self.begins = 0

    def begin_bulk(self):
        raise RuntimeError("bulk refused")

    def begin(self, *args):
        self.begins += 1
        return self._inner.begin(*args)

    def __getattr__(self, name):
        return getattr(self._inner, name)


def test_fluent_bulk_surfaces_a_failing_begin_bulk(tmp_path):
    user, knows = _schema()
    with kite(str(tmp_path / "refused.kitedb"), nodes=[user], edges=[knows]) as db:
        refusing = _RefusingBulk(db._db)
        db._db = refusing
        try:
            with pytest.raises(RuntimeError, match="bulk refused"):
                db.bulk([lambda: None])
            assert refusing.begins == 0, "bulk() fell back to a normal transaction"
        finally:
            db._db = refusing._inner
