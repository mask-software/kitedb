"""b4 commit-pipeline lane: savepoints on the low-level Database.

``savepoint()`` marks the current write transaction; ``rollback_to`` undoes
what it did since (and keeps the savepoint); ``release_savepoint`` keeps it.
Rolled-back writes never reach the WAL.
"""

from __future__ import annotations

import pytest

from kitedb import Database, Savepoint, TransactionError


def test_rollback_to_undoes_the_writes_since_the_savepoint(tmp_path):
    path = str(tmp_path / "savepoints.kitedb")
    db = Database(path)
    db.begin()
    db.create_node("kept")
    savepoint = db.savepoint()
    assert isinstance(savepoint, Savepoint)
    db.create_node("rolled-back")
    db.rollback_to(savepoint)
    assert db.get_node_by_key("rolled-back") is None
    db.create_node("after")
    db.release_savepoint(savepoint)
    db.commit()
    assert db.get_node_by_key("kept") is not None
    assert db.get_node_by_key("after") is not None
    assert db.get_node_by_key("rolled-back") is None
    db.close()

    # Reopening replays the WAL: no rolled-back record comes back.
    reopened = Database(path)
    assert reopened.get_node_by_key("kept") is not None
    assert reopened.get_node_by_key("rolled-back") is None
    reopened.close()


def test_a_released_or_ended_savepoint_cannot_be_used_again(tmp_path):
    db = Database(str(tmp_path / "ended.kitedb"))
    with pytest.raises(TransactionError):
        db.savepoint()
    db.begin()
    outer = db.savepoint()
    inner = db.savepoint()
    db.rollback_to(outer)
    with pytest.raises(TransactionError, match="Savepoint is not live"):
        db.rollback_to(inner)
    db.release_savepoint(outer)
    with pytest.raises(ValueError, match="released"):
        db.release_savepoint(outer)
    with pytest.raises(ValueError, match="released"):
        db.rollback_to(outer)
    db.commit()
    db.close()
