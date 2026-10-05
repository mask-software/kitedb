"""raydb-b4 checkpoint-segments: checkpoints through the Python binding.

A full WAL spills into WAL segments, and automatic checkpoints run (on a
thread of the database's own, or inline without it) once the log reaches the
checkpoint trigger: a load past it installs new snapshots, and
`checkpoint_error()` stays None. Without automatic checkpoints the WAL spills
up to `wal_segment_limit`, then writes raise `WalFullError` until a checkpoint
makes room. Out-of-range options are refused, the deprecated
`checkpoint_threshold` is still accepted, and `CheckpointError` and
`CheckpointDeclinedError` are exported.
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
        generation = db.stats().snapshot_gen
        # About 3 MiB of log: many spills, and checkpoints on the thread.
        for i in range(10_000):
            db.begin()
            db.create_node(f"n-{i}-{KEY}")
            db.commit()
        assert db.stats().snapshot_gen > generation, "no checkpoint ran during the load"
        assert db.checkpoint_error() is None
    finally:
        db.close()

    reopened = Database(path, options())
    try:
        assert reopened.get_node_by_key(f"n-0-{KEY}") is not None
        assert reopened.get_node_by_key(f"n-9999-{KEY}") is not None
    finally:
        reopened.close()


def test_inline_checkpoints_without_the_thread(tmp_path):
    db = Database(
        str(tmp_path / "inline.kitedb"),
        OpenOptions(
            wal_size=64 * 1024,
            checkpoint_thread=False,
            # A 64 KiB trigger: the load below passes it many times.
            checkpoint_log_budget=64 * 1024,
        ),
    )
    try:
        generation = db.stats().snapshot_gen
        for i in range(3_000):
            db.begin()
            db.create_node(f"n-{i}-{KEY}")
            db.commit()
        assert db.stats().snapshot_gen > generation, "no checkpoint ran during the load"
        assert db.checkpoint_error() is None
    finally:
        db.close()


def test_writes_past_the_segment_limit_fail_until_a_checkpoint(tmp_path):
    db = Database(
        str(tmp_path / "limit.kitedb"),
        OpenOptions(wal_size=64 * 1024, auto_checkpoint=False, wal_segment_limit=128 * 1024),
    )
    try:
        written = 0
        with pytest.raises(kitedb.WalFullError):
            for i in range(10_000):
                db.begin()
                try:
                    db.create_node(f"n-{i}-{KEY}")
                    db.commit()
                except BaseException:
                    try:
                        db.rollback()
                    except KiteError:
                        pass  # the failed commit ended the transaction
                    raise
                written += 1
        assert written > 400, f"only {written} commits before the limit"
        db.checkpoint()
        db.begin()
        db.create_node("after-the-checkpoint")
        db.commit()
        assert db.get_node_by_key(f"n-{written - 1}-{KEY}") is not None
        assert db.get_node_by_key("after-the-checkpoint") is not None
    finally:
        db.close()


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
