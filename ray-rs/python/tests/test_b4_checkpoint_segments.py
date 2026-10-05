"""raydb-b4 checkpoint-segments: the checkpoint options of the Python binding.

A full WAL spills into WAL segments, and automatic checkpoints run on a thread
of the database's own once the log reaches the checkpoint trigger. The options
that tune them reach the core, out-of-range values are refused, the deprecated
`checkpoint_threshold` is still accepted, `checkpoint_error()` reports the last
automatic checkpoint's failure (none here), and `CheckpointError` is exported.
"""

import pytest

import kitedb
from kitedb import Database, KiteError, OpenOptions

KEY = "k" * 200


def test_small_wal_spills_and_checkpoints_with_the_log_options(tmp_path):
    path = str(tmp_path / "segments.kitedb")

    def options():
        return OpenOptions(
            wal_size=64 * 1024,
            checkpoint_thread=True,
            checkpoint_log_ratio=0.5,
            checkpoint_log_budget=1024 * 1024,
            wal_segment_size=256 * 1024,
            wal_segment_limit=8 * 1024 * 1024,
        )

    db = Database(path, options())
    try:
        # About 3 MiB of log: many spills, and checkpoints on the thread.
        for i in range(10_000):
            db.begin()
            db.create_node(f"n-{i}-{KEY}")
            db.commit()
        assert db.checkpoint_error() is None
    finally:
        db.close()

    reopened = Database(path, options())
    try:
        assert reopened.get_node_by_key(f"n-0-{KEY}") is not None
        assert reopened.get_node_by_key(f"n-9999-{KEY}") is not None
    finally:
        reopened.close()


@pytest.mark.parametrize(
    "options",
    [
        {"checkpoint_log_ratio": -1.0},
        {"checkpoint_log_ratio": float("nan")},
        {"checkpoint_log_budget": 0},
        {"wal_segment_size": -1},
        {"wal_segment_limit": 0},
        {"checkpoint_threshold": 2.0},
    ],
)
def test_out_of_range_checkpoint_options_are_refused(tmp_path, options):
    with pytest.raises(ValueError):
        Database(str(tmp_path / "refused.kitedb"), OpenOptions(**options))


def test_deprecated_checkpoint_threshold_is_accepted(tmp_path):
    db = Database(str(tmp_path / "threshold.kitedb"), OpenOptions(checkpoint_threshold=0.5))
    assert db.checkpoint_error() is None
    db.close()


def test_checkpoint_error_is_exported_as_a_kite_error():
    assert issubclass(kitedb.CheckpointError, KiteError)


def test_checkpoint_declined_error_is_exported_as_a_kite_error():
    # A background checkpoint that does not run, or stops, raises it, with
    # the reason (decision Q1 of the fresh review).
    assert issubclass(kitedb.CheckpointDeclinedError, KiteError)
