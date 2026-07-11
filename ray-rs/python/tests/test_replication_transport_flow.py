"""End-to-end replication transport/admin flow validation for Python bindings."""

from __future__ import annotations

import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile

import pytest

PY_ROOT = Path(__file__).resolve().parents[1]
NATIVE_CANDIDATES = sorted((PY_ROOT / "kitedb").glob("_kitedb*.so"))
if not NATIVE_CANDIDATES:
    raise RuntimeError(f"missing native extension under {PY_ROOT / 'kitedb'}")

NATIVE_PATH = NATIVE_CANDIDATES[0]
NATIVE_SPEC = importlib.util.spec_from_file_location("_kitedb", NATIVE_PATH)
if NATIVE_SPEC is None or NATIVE_SPEC.loader is None:
    raise RuntimeError(f"failed loading native module from {NATIVE_PATH}")
NATIVE = importlib.util.module_from_spec(NATIVE_SPEC)
sys.modules[NATIVE_SPEC.name] = NATIVE
NATIVE_SPEC.loader.exec_module(NATIVE)

AUTH_PATH = PY_ROOT / "kitedb" / "replication_auth.py"
AUTH_SPEC = importlib.util.spec_from_file_location("kitedb_replication_auth", AUTH_PATH)
if AUTH_SPEC is None or AUTH_SPEC.loader is None:
    raise RuntimeError(f"failed loading replication auth module from {AUTH_PATH}")
AUTH = importlib.util.module_from_spec(AUTH_SPEC)
sys.modules[AUTH_SPEC.name] = AUTH
AUTH_SPEC.loader.exec_module(AUTH)

Database = NATIVE.Database
OpenOptions = NATIVE.OpenOptions
collect_replication_snapshot_transport_json = NATIVE.collect_replication_snapshot_transport_json
collect_replication_log_transport_json = NATIVE.collect_replication_log_transport_json
collect_replication_metrics_prometheus = NATIVE.collect_replication_metrics_prometheus

ReplicationAdminAuthConfig = AUTH.ReplicationAdminAuthConfig
create_replication_admin_authorizer = AUTH.create_replication_admin_authorizer


class FakeRequest:
    def __init__(self, headers: dict[str, str] | None = None):
        self.headers = headers or {}
        self.scope: dict[str, object] = {}


def _drain_replica(replica: object, max_frames: int, max_loops: int = 64) -> None:
    for _ in range(max_loops):
        applied = replica.replica_catch_up_once(max_frames)
        if applied == 0:
            return


def test_python_replication_transport_admin_flow_roundtrip():
    with tempfile.TemporaryDirectory() as tmpdir:
        primary_path = os.path.join(tmpdir, "primary.kitedb")
        primary_sidecar = os.path.join(tmpdir, "primary.sidecar")
        replica_path = os.path.join(tmpdir, "replica.kitedb")
        replica_sidecar = os.path.join(tmpdir, "replica.sidecar")

        primary = Database(
            primary_path,
            OpenOptions(
                replication_role="primary",
                replication_sidecar_path=primary_sidecar,
                replication_segment_max_bytes=1,
                replication_retention_min_entries=1,
                auto_checkpoint=False,
            ),
        )
        # A second live primary on one path simulates multi-machine
        # split-brain so epoch fencing can be exercised; requires the
        # test-only lock bypass.
        os.environ["KITEDB_DANGER_ALLOW_MULTI_NODE_SIMULATION"] = "1"
        stale = Database(
            primary_path,
            OpenOptions(
                replication_role="primary",
                replication_sidecar_path=primary_sidecar,
                replication_segment_max_bytes=1,
                replication_retention_min_entries=1,
                auto_checkpoint=False,
                danger_bypass_file_lock_for_multi_node_simulation=True,
            ),
        )
        replica = Database(
            replica_path,
            OpenOptions(
                replication_role="replica",
                replication_sidecar_path=replica_sidecar,
                replication_source_db_path=primary_path,
                replication_source_sidecar_path=primary_sidecar,
                auto_checkpoint=False,
            ),
        )

        try:
            primary.begin(False)
            primary.create_node("n:base")
            token_base = primary.commit_with_token()
            assert token_base.startswith("1:")

            replica.replica_bootstrap_from_snapshot()
            replica_status = replica.replica_replication_status()
            assert replica_status["needs_reseed"] is False
            assert replica_status["applied_log_index"] == 1

            snapshot = json.loads(
                collect_replication_snapshot_transport_json(primary, include_data=False)
            )
            snapshot_direct = json.loads(
                primary.export_replication_snapshot_transport_json(False)
            )
            assert snapshot["epoch"] == snapshot_direct["epoch"]
            assert snapshot["head_log_index"] == snapshot_direct["head_log_index"]

            log_page = json.loads(
                collect_replication_log_transport_json(
                    primary,
                    cursor=None,
                    max_frames=128,
                    max_bytes=1024 * 1024,
                    include_payload=False,
                )
            )
            log_page_direct = json.loads(
                primary.export_replication_log_transport_json(
                    None,
                    128,
                    1024 * 1024,
                    False,
                )
            )
            assert log_page["frame_count"] == log_page_direct["frame_count"]
            assert log_page["frame_count"] >= 1

            prometheus = collect_replication_metrics_prometheus(primary)
            assert "kitedb_replication_" in prometheus

            require_admin = create_replication_admin_authorizer(
                ReplicationAdminAuthConfig(mode="token", token="secret-token")
            )
            require_admin(FakeRequest({"authorization": "Bearer secret-token"}))
            with pytest.raises(PermissionError, match="not satisfied"):
                require_admin(FakeRequest({"authorization": "Bearer wrong-token"}))

            for i in range(6):
                primary.begin(False)
                primary.create_node(f"n:lag-{i}")
                primary.commit_with_token()

            lag_status = replica.replica_replication_status()
            primary.primary_report_replica_progress(
                "replica-a",
                lag_status["applied_epoch"],
                lag_status["applied_log_index"],
            )
            primary.primary_run_retention()

            # Retention respects the reported replica cursor and lagging
            # replicas recover automatically (reseeding from snapshot
            # internally when a gap exists), so catch-up succeeds instead
            # of raising a reseed error.
            _drain_replica(replica, 64)
            assert replica.replica_replication_status()["needs_reseed"] is False
            assert replica.count_nodes() == primary.count_nodes()

            # The explicit reseed admin API remains available and idempotent.
            primary.checkpoint()
            replica.replica_reseed_from_snapshot()
            assert replica.replica_replication_status()["needs_reseed"] is False
            assert replica.count_nodes() == primary.count_nodes()

            before = primary.primary_replication_status()["epoch"]
            promoted = primary.primary_promote_to_next_epoch()
            assert promoted > before

            stale.begin(False)
            stale.create_node("n:stale-write")
            with pytest.raises(Exception, match="stale primary"):
                stale.commit_with_token()
            if stale.has_transaction():
                stale.rollback()

            primary.begin(False)
            primary.create_node("n:post-promote")
            promoted_token = primary.commit_with_token()
            assert promoted_token.startswith(f"{promoted}:")

            assert not replica.wait_for_token(promoted_token, 5)
            _drain_replica(replica, 128)
            assert replica.wait_for_token(promoted_token, 2000)
            assert replica.count_nodes() == primary.count_nodes()
        finally:
            replica.close()
            stale.close()
            primary.close()
