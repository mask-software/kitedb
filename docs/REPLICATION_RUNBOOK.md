# Replication Operations Runbook (V1)

Scope:

- Single-file deployment mode (`.kitedb`) with sidecar replication.
- Roles: one writable primary, one or more replicas.
- APIs available in Rust core, Node NAPI, and Python bindings.

## 1. Operational Signals

Primary status fields:

- `epoch`: current leadership epoch.
- `head_log_index`: latest committed replication log index.
- `retained_floor`: lowest retained index after pruning.
- `replica_lags[]`: per-replica applied position.
- `append_attempts|append_failures|append_successes`: commit-path replication health.

Replica status fields:

- `applied_epoch`, `applied_log_index`: durable apply cursor.
- `last_error`: latest pull/apply failure detail.
- `needs_reseed`: continuity break or floor violation; snapshot reseed required.

Metrics surface:

- `collect_metrics()` now includes `replication` with role (`primary|replica|disabled`) plus
  role-specific replication counters/state for dashboards and alerting.
- Host-runtime Prometheus text export is available via:
  - Rust core: `collect_replication_metrics_prometheus_single_file(...)`
  - Node NAPI: `collectReplicationMetricsPrometheus(db)`
  - Python PyO3: `collect_replication_metrics_prometheus(db)`
- Host-runtime OpenTelemetry OTLP-JSON export is available via:
  - Rust core: `collect_replication_metrics_otel_json_single_file(...)`
  - Node NAPI: `collectReplicationMetricsOtelJson(db)`
  - Python PyO3: `collect_replication_metrics_otel_json(db)`
- Host-runtime OpenTelemetry OTLP-protobuf export is available via:
  - Rust core: `collect_replication_metrics_otel_protobuf_single_file(...)`
  - Node NAPI: `collectReplicationMetricsOtelProtobuf(db)`
  - Python PyO3: `collect_replication_metrics_otel_protobuf(db)`
- Host-runtime OpenTelemetry collector push is available via:
  - Rust core: `push_replication_metrics_otel_json_single_file(db, endpoint, timeout_ms, bearer_token)`
    - advanced TLS/mTLS: `push_replication_metrics_otel_json_*_with_options(...)` with
      `https_only`, `ca_cert_pem_path`, `client_cert_pem_path`, `client_key_pem_path`,
      `retry_max_attempts`, `retry_backoff_ms`, `retry_backoff_max_ms`, `retry_jitter_ratio`,
      `adaptive_retry`, `adaptive_retry_mode`, `adaptive_retry_ewma_alpha`, `circuit_breaker_failure_threshold`, `circuit_breaker_open_ms`, `circuit_breaker_half_open_probes`,
      `circuit_breaker_state_path`, `circuit_breaker_state_url`, `circuit_breaker_state_patch`, `circuit_breaker_state_patch_batch`, `circuit_breaker_state_patch_batch_max_keys`, `circuit_breaker_state_patch_merge`, `circuit_breaker_state_patch_merge_max_keys`, `circuit_breaker_state_patch_retry_max_attempts`, `circuit_breaker_state_cas`, `circuit_breaker_state_lease_id`, `circuit_breaker_scope_key`, `compression_gzip`.
  - Rust core (protobuf): `push_replication_metrics_otel_protobuf_single_file(db, endpoint, timeout_ms, bearer_token)`
    - advanced TLS/mTLS: `push_replication_metrics_otel_protobuf_*_with_options(...)` with
      `https_only`, `ca_cert_pem_path`, `client_cert_pem_path`, `client_key_pem_path`,
      `retry_max_attempts`, `retry_backoff_ms`, `retry_backoff_max_ms`, `retry_jitter_ratio`,
      `adaptive_retry`, `adaptive_retry_mode`, `adaptive_retry_ewma_alpha`, `circuit_breaker_failure_threshold`, `circuit_breaker_open_ms`, `circuit_breaker_half_open_probes`,
      `circuit_breaker_state_path`, `circuit_breaker_state_url`, `circuit_breaker_state_patch`, `circuit_breaker_state_patch_batch`, `circuit_breaker_state_patch_batch_max_keys`, `circuit_breaker_state_patch_merge`, `circuit_breaker_state_patch_merge_max_keys`, `circuit_breaker_state_patch_retry_max_attempts`, `circuit_breaker_state_cas`, `circuit_breaker_state_lease_id`, `circuit_breaker_scope_key`, `compression_gzip`.
  - Rust core (gRPC): `push_replication_metrics_otel_grpc_single_file(db, endpoint, timeout_ms, bearer_token)`
    - advanced TLS/mTLS: `push_replication_metrics_otel_grpc_*_with_options(...)` with
      `https_only`, `ca_cert_pem_path`, `client_cert_pem_path`, `client_key_pem_path`,
      `retry_max_attempts`, `retry_backoff_ms`, `retry_backoff_max_ms`, `retry_jitter_ratio`,
      `adaptive_retry`, `adaptive_retry_mode`, `adaptive_retry_ewma_alpha`, `circuit_breaker_failure_threshold`, `circuit_breaker_open_ms`, `circuit_breaker_half_open_probes`,
      `circuit_breaker_state_path`, `circuit_breaker_state_url`, `circuit_breaker_state_patch`, `circuit_breaker_state_patch_batch`, `circuit_breaker_state_patch_batch_max_keys`, `circuit_breaker_state_patch_merge`, `circuit_breaker_state_patch_merge_max_keys`, `circuit_breaker_state_patch_retry_max_attempts`, `circuit_breaker_state_cas`, `circuit_breaker_state_lease_id`, `circuit_breaker_scope_key`, `compression_gzip`.
  - Node NAPI: `pushReplicationMetricsOtelJson(db, endpoint, timeoutMs, bearerToken?)`
    - advanced TLS/mTLS: `pushReplicationMetricsOtelJsonWithOptions(db, endpoint, options)`.
  - Node NAPI (protobuf): `pushReplicationMetricsOtelProtobuf(db, endpoint, timeoutMs, bearerToken?)`
    - advanced TLS/mTLS: `pushReplicationMetricsOtelProtobufWithOptions(db, endpoint, options)`.
  - Node NAPI (gRPC): `pushReplicationMetricsOtelGrpc(db, endpoint, timeoutMs, bearerToken?)`
    - advanced TLS/mTLS: `pushReplicationMetricsOtelGrpcWithOptions(db, endpoint, options)`.
  - Python PyO3: `push_replication_metrics_otel_json(db, endpoint, timeout_ms=5000, bearer_token=None)`
    - advanced TLS/mTLS kwargs:
      `https_only`, `ca_cert_pem_path`, `client_cert_pem_path`, `client_key_pem_path`,
      `retry_max_attempts`, `retry_backoff_ms`, `retry_backoff_max_ms`, `retry_jitter_ratio`,
      `adaptive_retry`, `adaptive_retry_mode`, `adaptive_retry_ewma_alpha`, `circuit_breaker_failure_threshold`, `circuit_breaker_open_ms`, `circuit_breaker_half_open_probes`,
      `circuit_breaker_state_path`, `circuit_breaker_state_url`, `circuit_breaker_state_patch`, `circuit_breaker_state_patch_batch`, `circuit_breaker_state_patch_batch_max_keys`, `circuit_breaker_state_patch_merge`, `circuit_breaker_state_patch_merge_max_keys`, `circuit_breaker_state_patch_retry_max_attempts`, `circuit_breaker_state_cas`, `circuit_breaker_state_lease_id`, `circuit_breaker_scope_key`, `compression_gzip`.
  - Python PyO3 (protobuf): `push_replication_metrics_otel_protobuf(db, endpoint, timeout_ms=5000, bearer_token=None)`
    - advanced TLS/mTLS kwargs:
      `https_only`, `ca_cert_pem_path`, `client_cert_pem_path`, `client_key_pem_path`,
      `retry_max_attempts`, `retry_backoff_ms`, `retry_backoff_max_ms`, `retry_jitter_ratio`,
      `adaptive_retry`, `adaptive_retry_mode`, `adaptive_retry_ewma_alpha`, `circuit_breaker_failure_threshold`, `circuit_breaker_open_ms`, `circuit_breaker_half_open_probes`,
      `circuit_breaker_state_path`, `circuit_breaker_state_url`, `circuit_breaker_state_patch`, `circuit_breaker_state_patch_batch`, `circuit_breaker_state_patch_batch_max_keys`, `circuit_breaker_state_patch_merge`, `circuit_breaker_state_patch_merge_max_keys`, `circuit_breaker_state_patch_retry_max_attempts`, `circuit_breaker_state_cas`, `circuit_breaker_state_lease_id`, `circuit_breaker_scope_key`, `compression_gzip`.
  - Python PyO3 (gRPC): `push_replication_metrics_otel_grpc(db, endpoint, timeout_ms=5000, bearer_token=None)`
    - advanced TLS/mTLS kwargs:
      `https_only`, `ca_cert_pem_path`, `client_cert_pem_path`, `client_key_pem_path`,
      `retry_max_attempts`, `retry_backoff_ms`, `retry_backoff_max_ms`, `retry_jitter_ratio`,
      `adaptive_retry`, `adaptive_retry_mode`, `adaptive_retry_ewma_alpha`, `circuit_breaker_failure_threshold`, `circuit_breaker_open_ms`, `circuit_breaker_half_open_probes`,
      `circuit_breaker_state_path`, `circuit_breaker_state_url`, `circuit_breaker_state_patch`, `circuit_breaker_state_patch_batch`, `circuit_breaker_state_patch_batch_max_keys`, `circuit_breaker_state_patch_merge`, `circuit_breaker_state_patch_merge_max_keys`, `circuit_breaker_state_patch_retry_max_attempts`, `circuit_breaker_state_cas`, `circuit_breaker_state_lease_id`, `circuit_breaker_scope_key`, `compression_gzip`.
  - Note: `circuit_breaker_state_path` and `circuit_breaker_state_url` are mutually exclusive.
  - Note: `circuit_breaker_state_patch`, `circuit_breaker_state_patch_batch`, `circuit_breaker_state_patch_batch_max_keys`, `circuit_breaker_state_patch_merge`, `circuit_breaker_state_patch_merge_max_keys`, `circuit_breaker_state_patch_retry_max_attempts`, `circuit_breaker_state_cas`, and `circuit_breaker_state_lease_id` require `circuit_breaker_state_url`.
- Host-runtime replication transport JSON export helpers are available via:
  - Node NAPI: `collectReplicationSnapshotTransportJson(db, includeData?)`,
    `collectReplicationLogTransportJson(db, cursor?, maxFrames?, maxBytes?, includePayload?)`
  - TypeScript adapter helper: `createReplicationTransportAdapter(db)` in `ray-rs/ts/replication_transport.ts`
  - TypeScript admin auth helper: `createReplicationAdminAuthorizer({ mode, token, mtlsHeader, mtlsSubjectRegex, mtlsMatcher? })`
    for `none|token|mtls|token_or_mtls|token_and_mtls` with optional native TLS verifier hook (`mtlsMatcher`).
  - TypeScript native TLS matcher helper: `createNodeTlsMtlsMatcher({ requirePeerCertificate? })`
    and probe helper `isNodeTlsClientAuthorized(request, options?)` for common Node request socket shapes
    (`request.socket`, `request.client`, `request.raw.socket`, `request.req.socket`).
  - Python PyO3: `collect_replication_snapshot_transport_json(db, include_data=False)`,
    `collect_replication_log_transport_json(db, cursor=None, max_frames=128, max_bytes=1048576, include_payload=True)`
  - Python host auth helper: `create_replication_admin_authorizer(...)` with `ReplicationAdminAuthConfig`
    and ASGI native TLS matcher helpers `create_asgi_tls_mtls_matcher(...)` / `is_asgi_tls_client_authorized(...)`.
  - These are intended for embedding host-side HTTP endpoints beyond playground runtime.
  - Template files:
    - Python FastAPI adapter: `docs/examples/replication_adapter_python_fastapi.py`
    - Generic middleware adapter: `docs/examples/replication_adapter_generic_middleware.ts`

Alert heuristics:

- `append_failures > 0` growing: primary sidecar durability issue.
- Replica lag growth over steady traffic: pull/apply bottleneck.
- `needs_reseed == true`: force reseed, do not keep retrying catch-up.

## 2. Bootstrap a New Replica

1. Open replica with:
   - `replication_role=replica`
   - `replication_source_db_path`
   - `replication_source_sidecar_path`
   - Validation hardening:
     - source DB path is required and must exist as a file,
     - source DB path must differ from replica DB path,
     - source sidecar path must differ from local replica sidecar path.
2. Call `replica_bootstrap_from_snapshot()`.
3. Start catch-up loop with `replica_catch_up_once(max_frames)`.
4. Validate `needs_reseed == false` and `last_error == null`.

## 3. Routine Catch-up + Retention

Replica:

- Poll `replica_catch_up_once(max_frames)` repeatedly.
- Persist and monitor `applied_log_index`.

Primary:

- Report each replica cursor via `primary_report_replica_progress(replica_id, epoch, applied_log_index)`.
- Run `primary_run_retention()` on an operator cadence.

Tuning:

- `replication_retention_min_entries`: set above worst-case expected replica lag.
- `replication_retention_min_ms`: keep recent segments for at least this wall-clock window.
- `replication_segment_max_bytes`: larger segments reduce file churn; smaller segments prune faster.

## 4. Manual Promotion Procedure

Goal: move write authority to a target node without split-brain writes.

1. Quiesce writes on old primary (application-level write freeze).
2. Promote target primary:
   - `primary_promote_to_next_epoch()`.
3. Verify:
   - new primary status `epoch` incremented,
   - new writes return tokens in the new epoch.
4. Confirm stale fence:
   - old primary write attempts fail with stale-primary error.
5. Repoint replicas to the promoted primary source paths.

## 5. Reseed Procedure (`needs_reseed`)

Trigger:

- Replica status sets `needs_reseed=true`, usually from retained-floor/continuity break.

Steps:

1. Stop normal catch-up loop for that replica.
2. Execute `replica_reseed_from_snapshot()`.
3. Resume `replica_catch_up_once(...)`.
4. Verify:
   - `needs_reseed=false`,
   - `last_error` cleared,
   - data parity checks (counts and spot checks) pass.

## 6. Failure Handling

Corrupt/truncated segment:

- Symptom: catch-up error + replica `last_error` set.
- Action: reseed replica from snapshot.

Retention floor outran replica:

- Symptom: catch-up error mentions reseed/floor; `needs_reseed=true`.
- Action: reseed; increase `replication_retention_min_entries` if frequent.

Promotion race / split-brain suspicion:

- Symptom: concurrent promote/write attempts.
- Expected: exactly one writer succeeds post-promotion.
- Action: treat stale-writer failures as correct fencing; ensure client routing points to current epoch primary.

## 7. Validation Checklist

Before rollout:

- `cargo test --no-default-features --test replication_phase_a --test replication_phase_b --test replication_phase_c --test replication_phase_d --test replication_faults_phase_d`
- `cargo test --no-default-features replication::`

Perf gate:

- Run `ray-rs/scripts/replication-perf-gate.sh`.
- Commit overhead gate: require median p95 ratio (replication-on / baseline) within `P95_MAX_RATIO` (default `1.03`, `ATTEMPTS=7`).
- Catch-up gate: require replica throughput floors (`MIN_CATCHUP_FPS`, `MIN_THROUGHPUT_RATIO`).
- Catch-up gate retries benchmark noise by default (`ATTEMPTS=3`); increase on busy dev machines.
- CI on `main` (`.github/workflows/ray-rs.yml`) enforces replication perf gate and uploads benchmark logs as `replication-perf-gate-logs`.

## 8. HTTP Admin Endpoints (Playground Runtime)

Available endpoints in `playground/src/api/routes.ts`:

- `GET /api/replication/status`
- `GET /api/replication/metrics` (Prometheus text format)
- `GET /api/replication/snapshot/latest`
- `GET /api/replication/log`
- `GET /api/replication/transport/snapshot` (host-runtime transport export passthrough)
- `GET /api/replication/transport/log` (host-runtime transport export passthrough)
- `POST /api/replication/pull` (runs `replica_catch_up_once`)
- `POST /api/replication/reseed` (runs `replica_reseed_from_snapshot`)
- `POST /api/replication/promote` (runs `primary_promote_to_next_epoch`)

Auth:

- `REPLICATION_ADMIN_AUTH_MODE` controls admin auth:
  - `none` (no admin auth)
  - `token` (Bearer token)
  - `mtls` (mTLS client-cert header)
  - `token_or_mtls`
  - `token_and_mtls`
- Token modes use `REPLICATION_ADMIN_TOKEN`.
- mTLS modes read `REPLICATION_MTLS_HEADER` (default `x-forwarded-client-cert`) and optional
  subject filter `REPLICATION_MTLS_SUBJECT_REGEX`.
- Native TLS mTLS mode can be enabled with `REPLICATION_MTLS_NATIVE_TLS=true` when the
  playground listener is configured with:
  - `PLAYGROUND_TLS_CERT_FILE`, `PLAYGROUND_TLS_KEY_FILE` (HTTPS enablement)
  - `PLAYGROUND_TLS_REQUEST_CERT=true`
  - `PLAYGROUND_TLS_REJECT_UNAUTHORIZED=true`
  - optional `PLAYGROUND_TLS_CA_FILE` for custom client-cert trust roots
- `REPLICATION_MTLS_SUBJECT_REGEX` applies to header-based mTLS values; native TLS mode
  validates client cert handshake presence, not subject matching.
- `metrics`, `snapshot`, `log`, `pull`, `reseed`, and `promote` enforce the selected mode.
- `status` is read-only and does not require auth.

Playground curl examples:

- `export BASE="http://localhost:3000"`
- `curl "$BASE/api/replication/status"`
- `curl -H "Authorization: Bearer $REPLICATION_ADMIN_TOKEN" "$BASE/api/replication/metrics"`
- `curl -H "Authorization: Bearer $REPLICATION_ADMIN_TOKEN" "$BASE/api/replication/log?maxFrames=128&maxBytes=1048576"`
- `curl -X POST -H "Authorization: Bearer $REPLICATION_ADMIN_TOKEN" -H "Content-Type: application/json" -d '{"maxFrames":256}' "$BASE/api/replication/pull"`
- `curl -X POST -H "Authorization: Bearer $REPLICATION_ADMIN_TOKEN" "$BASE/api/replication/reseed"`
- `curl -X POST -H "Authorization: Bearer $REPLICATION_ADMIN_TOKEN" "$BASE/api/replication/promote"`
- `curl -H "x-client-cert: CN=allowed-client,O=RayDB" "$BASE/api/replication/metrics"` (when `REPLICATION_ADMIN_AUTH_MODE=mtls`)

## 9. Known V1 Limits

- Retention policy supports entry-window + time-window floors, but not richer SLA-aware policies.
- Bundled HTTP admin endpoints still ship in playground runtime; host runtime now exposes transport JSON helpers for embedding custom HTTP surfaces.
- OTLP retry policy is bounded attempt/backoff/jitter with optional adaptive multiplier (`linear` or `ewma`) and circuit-breaker half-open probes. Circuit-breaker state is process-local by default; optional file-backed sharing (`circuit_breaker_state_path`) or shared HTTP store (`circuit_breaker_state_url`) is available with `circuit_breaker_scope_key`; URL backend can enable key-scoped patch mode (`circuit_breaker_state_patch`), batched patch mode (`circuit_breaker_state_patch_batch` with `circuit_breaker_state_patch_batch_max_keys`), compacting merge patch mode (`circuit_breaker_state_patch_merge` with `circuit_breaker_state_patch_merge_max_keys`), bounded patch retries (`circuit_breaker_state_patch_retry_max_attempts`), CAS (`circuit_breaker_state_cas`), and lease header propagation (`circuit_breaker_state_lease_id`).
- Vector authority boundary: logical vector property mutations (`SetNodeVector` / `DelNodeVector`) are authoritative and replicated; vector batch/fragment maintenance records are treated as derived index artifacts and are skipped during replica apply.
- `SyncMode::Normal` and `SyncMode::Off` optimize commit latency by batching sidecar frame writes in-memory and refreshing manifest fencing periodically (not every commit). For strict per-commit sidecar visibility/fencing, use `SyncMode::Full`.
