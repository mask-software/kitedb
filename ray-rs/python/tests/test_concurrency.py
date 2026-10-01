"""Deadlock regressions for calls that release the GIL.

Each scenario runs in a subprocess: a GIL/lock deadlock freezes every Python
thread in the process, so only a subprocess timeout can turn it into a failure.
"""

import subprocess
import sys
import textwrap

import pytest

_SCENARIO = textwrap.dedent(
    """
    import sys, threading, time
    from kitedb import Database

    db = Database(sys.argv[1])
    method = sys.argv[2]
    in_tx = threading.Event()

    def writer():
        db.begin()
        db.create_node("user:writer")
        in_tx.set()
        time.sleep(0.3)  # the maintenance call now waits for this transaction
        db.commit()

    def late_begin():
        time.sleep(0.15)  # the maintenance call now holds the checkpoint gate
        db.begin()
        db.create_node("user:late")
        db.commit()

    threads = [threading.Thread(target=writer), threading.Thread(target=late_begin)]
    threads[0].start()
    in_tx.wait()
    threads[1].start()
    getattr(db, method)()
    for thread in threads:
        thread.join()

    assert db.get_node_by_key("user:writer") is not None
    assert db.get_node_by_key("user:late") is not None
    db.close()
    """
)


@pytest.mark.parametrize("method", ["checkpoint", "optimize", "vacuum"])
def test_maintenance_waits_for_other_threads_transactions(tmp_path, method):
    # Core maintenance drains open transactions and blocks new ones meanwhile.
    # The drained transaction must still get the GIL and the database lock to
    # commit, and a thread blocked in begin() must not hold the GIL.
    try:
        proc = subprocess.run(
            [sys.executable, "-c", _SCENARIO, str(tmp_path / "db.kitedb"), method],
            capture_output=True,
            text=True,
            timeout=30,
        )
    except subprocess.TimeoutExpired:
        pytest.fail(f"{method}() deadlocked against a transaction on another thread")
    assert proc.returncode == 0, proc.stderr
