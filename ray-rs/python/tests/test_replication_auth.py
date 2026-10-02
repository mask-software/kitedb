"""Tests for replication admin auth helpers."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import sys
import pytest

MODULE_PATH = Path(__file__).resolve().parents[1] / "kitedb" / "replication_auth.py"
MODULE_SPEC = importlib.util.spec_from_file_location("replication_auth", MODULE_PATH)
if MODULE_SPEC is None or MODULE_SPEC.loader is None:
    raise RuntimeError(f"failed loading replication auth module from {MODULE_PATH}")
MODULE = importlib.util.module_from_spec(MODULE_SPEC)
sys.modules[MODULE_SPEC.name] = MODULE
MODULE_SPEC.loader.exec_module(MODULE)

AsgiMtlsMatcherOptions = MODULE.AsgiMtlsMatcherOptions
ReplicationAdminAuthConfig = MODULE.ReplicationAdminAuthConfig
authorize_replication_admin_request = MODULE.authorize_replication_admin_request
create_asgi_tls_mtls_matcher = MODULE.create_asgi_tls_mtls_matcher
create_replication_admin_authorizer = MODULE.create_replication_admin_authorizer
is_asgi_tls_client_authorized = MODULE.is_asgi_tls_client_authorized
is_replication_admin_authorized = MODULE.is_replication_admin_authorized


class FakeRequest:
    def __init__(self, headers=None, scope=None):
        self.headers = headers or {}
        self.scope = scope or {}


def test_replication_auth_none_mode_allows_any_request():
    request = FakeRequest()
    config = ReplicationAdminAuthConfig(mode="none")
    assert is_replication_admin_authorized(request, config)
    authorize_replication_admin_request(request, config)


def test_replication_auth_token_mode_requires_bearer_token():
    config = ReplicationAdminAuthConfig(mode="token", token="abc123")
    assert is_replication_admin_authorized(
        FakeRequest(headers={"authorization": "Bearer abc123"}), config
    )
    assert not is_replication_admin_authorized(
        FakeRequest(headers={"authorization": "Bearer wrong"}), config
    )


def test_replication_auth_mtls_mode_supports_header_and_subject_regex():
    config = ReplicationAdminAuthConfig(
        mode="mtls",
        mtls_header="x-client-cert",
        mtls_subject_regex=r"CN=replication-admin,O=RayDB",
        trust_forwarded_client_cert=True,
    )
    assert is_replication_admin_authorized(
        FakeRequest(headers={"x-client-cert": "CN=replication-admin,O=RayDB"}),
        config,
    )
    assert not is_replication_admin_authorized(
        FakeRequest(headers={"x-client-cert": "CN=viewer,O=RayDB"}),
        config,
    )


def test_replication_auth_token_or_and_modes():
    either = ReplicationAdminAuthConfig(
        mode="token_or_mtls",
        token="abc123",
        mtls_header="x-client-cert",
        mtls_subject_regex=r"CN=replication-admin,O=RayDB",
        trust_forwarded_client_cert=True,
    )
    assert is_replication_admin_authorized(
        FakeRequest(headers={"authorization": "Bearer abc123"}), either
    )
    assert is_replication_admin_authorized(
        FakeRequest(headers={"x-client-cert": "CN=replication-admin,O=RayDB"}), either
    )
    assert not is_replication_admin_authorized(FakeRequest(), either)

    both = ReplicationAdminAuthConfig(
        mode="token_and_mtls",
        token="abc123",
        mtls_header="x-client-cert",
        mtls_subject_regex=r"CN=replication-admin,O=RayDB",
        trust_forwarded_client_cert=True,
    )
    assert not is_replication_admin_authorized(
        FakeRequest(headers={"authorization": "Bearer abc123"}), both
    )
    assert not is_replication_admin_authorized(
        FakeRequest(headers={"x-client-cert": "CN=replication-admin,O=RayDB"}), both
    )
    assert is_replication_admin_authorized(
        FakeRequest(
            headers={
                "authorization": "Bearer abc123",
                "x-client-cert": "CN=replication-admin,O=RayDB",
            }
        ),
        both,
    )


def test_replication_auth_supports_custom_matcher_hook():
    request_ok = FakeRequest(scope={"tls_client_authorized": True})
    request_no = FakeRequest(scope={"tls_client_authorized": False})
    config = ReplicationAdminAuthConfig(
        mode="mtls",
        mtls_matcher=lambda request: bool(request.scope.get("tls_client_authorized")),
    )
    assert is_replication_admin_authorized(request_ok, config)
    assert not is_replication_admin_authorized(request_no, config)


def test_replication_auth_authorizer_rejects_invalid_config_and_unauthorized():
    with pytest.raises(ValueError, match="non-empty token"):
        create_replication_admin_authorizer(
            ReplicationAdminAuthConfig(mode="token", token=" ")
        )

    require_admin = create_replication_admin_authorizer(
        ReplicationAdminAuthConfig(mode="token", token="abc123")
    )
    with pytest.raises(PermissionError, match="not satisfied"):
        require_admin(FakeRequest(headers={"authorization": "Bearer wrong"}))


def test_asgi_tls_client_authorized_helper_checks_scope_flags():
    assert is_asgi_tls_client_authorized(
        FakeRequest(scope={"tls_client_authorized": True})
    )
    assert is_asgi_tls_client_authorized(
        FakeRequest(scope={"client_cert_verified": True})
    )
    assert is_asgi_tls_client_authorized(
        FakeRequest(scope={"ssl_client_verify": "SUCCESS"})
    )
    assert not is_asgi_tls_client_authorized(FakeRequest(scope={"ssl_client_verify": "FAILED"}))


def test_asgi_tls_client_authorized_helper_optionally_requires_peer_certificate():
    options = AsgiMtlsMatcherOptions(require_peer_certificate=True)
    with_peer_cert = FakeRequest(
        scope={
            "tls_client_authorized": True,
            "extensions": {"tls": {"client_cert_chain": ["cert"]}},
        }
    )
    without_peer_cert = FakeRequest(scope={"tls_client_authorized": True})
    assert is_asgi_tls_client_authorized(with_peer_cert, options)
    assert not is_asgi_tls_client_authorized(without_peer_cert, options)


def test_create_asgi_tls_mtls_matcher_factory():
    matcher = create_asgi_tls_mtls_matcher(
        AsgiMtlsMatcherOptions(require_peer_certificate=True)
    )
    assert matcher(
        FakeRequest(
            scope={
                "tls_client_authorized": True,
                "extensions": {"tls": {"client_cert": "cert"}},
            }
        )
    )
    assert not matcher(FakeRequest(scope={"tls_client_authorized": True}))
