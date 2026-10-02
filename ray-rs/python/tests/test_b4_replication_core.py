"""raydb-b4 replication-core: binary transports (X5), the sidecar generation and
no db_path in the transports (P2, P9), and removing a replica's progress (P2).

Each test fails against the unfixed bindings.
"""

from __future__ import annotations

import base64
import json
import os
import re
import tempfile
import zlib

import kitedb
from kitedb import Database, OpenOptions

HEX16 = re.compile(r"^[0-9a-f]{16}$")


def _open_primary(tmpdir: str, **extra: object) -> Database:
    return Database(
        os.path.join(tmpdir, "primary.kitedb"),
        OpenOptions(replication_role="primary", auto_checkpoint=False, **extra),
    )


def _commit_nodes(db: Database, count: int, prefix: str = "n") -> None:
    for i in range(count):
        db.begin()
        db.create_node(f"{prefix}:{i}")
        db.commit_with_token()


def test_binary_snapshot_transport_matches_json_without_db_path():
    with tempfile.TemporaryDirectory() as tmpdir:
        primary = _open_primary(tmpdir)
        try:
            _commit_nodes(primary, 3)
            raw = primary.export_replication_snapshot_transport_json(True)
            snapshot_json = json.loads(raw)
            assert "db_path" not in snapshot_json
            assert tmpdir not in raw
            assert HEX16.match(snapshot_json["generation"])

            snapshot = primary.export_replication_snapshot_transport(True)
            data = snapshot["data"]
            assert isinstance(data, bytes)
            assert data == base64.b64decode(snapshot_json["data_base64"])
            assert snapshot["byte_length"] == len(data)
            assert snapshot["checksum_crc32"] == zlib.crc32(data)
            assert f"{snapshot['checksum_crc32']:08x}" == snapshot_json["checksum_crc32c"]
            assert snapshot["head_log_index"] == 3
            assert snapshot["start_cursor"] == snapshot_json["start_cursor"]
            assert snapshot["generation"] == snapshot_json["generation"]

            direct = kitedb.collect_replication_snapshot_transport(primary, include_data=False)
            assert direct["data"] is None
            assert direct["checksum_crc32"] == snapshot["checksum_crc32"]
        finally:
            primary.close()


def test_binary_log_transport_pages_with_raw_payloads():
    with tempfile.TemporaryDirectory() as tmpdir:
        primary = _open_primary(tmpdir)
        try:
            _commit_nodes(primary, 5)
            page = primary.export_replication_log_transport(
                cursor=None, max_frames=3, max_bytes=1 << 20, include_payload=True
            )
            page_json = json.loads(
                primary.export_replication_log_transport_json(
                    cursor=None, max_frames=3, max_bytes=1 << 20, include_payload=True
                )
            )
            assert len(page["frames"]) == 3
            assert page["eof"] is False
            assert page["next_cursor"] == page_json["next_cursor"]
            assert page["generation"] == page_json["generation"]
            assert HEX16.match(page["generation"])
            for frame, frame_json in zip(page["frames"], page_json["frames"]):
                assert frame["log_index"] == frame_json["log_index"]
                assert isinstance(frame["payload"], bytes)
                assert frame["payload"] == base64.b64decode(frame_json["payload_base64"])

            rest = kitedb.collect_replication_log_transport(
                primary,
                cursor=page["next_cursor"],
                max_frames=64,
                max_bytes=1 << 20,
                include_payload=False,
            )
            assert rest["eof"] is True
            assert [frame["log_index"] for frame in rest["frames"]] == [4, 5]
            assert all(frame["payload"] is None for frame in rest["frames"])
        finally:
            primary.close()


def test_primary_remove_replica_progress_releases_retention():
    with tempfile.TemporaryDirectory() as tmpdir:
        primary = _open_primary(
            tmpdir,
            replication_segment_max_bytes=1,
            replication_retention_min_entries=2,
        )
        try:
            _commit_nodes(primary, 1)
            primary.primary_report_replica_progress("gone", 1, 1)
            _commit_nodes(primary, 9, "more")
            assert primary.primary_run_retention()[1] == 2

            assert primary.primary_remove_replica_progress("gone") is True
            assert primary.primary_remove_replica_progress("gone") is False
            assert primary.primary_run_retention()[1] == 8
            lags = primary.primary_replication_status()["replica_lags"]
            assert all(lag["replica_id"] != "gone" for lag in lags)
        finally:
            primary.close()
