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
- `last_replication_error`: latest primary-side sidecar failure, if any.
- `sidecar_needs_repair`: the sidecar is fenced and requires repair/resync; local commits still succeed without replication tokens.

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
- Host-runtime replication transport export helpers are available via:
  - Rust core: `SingleFileDB::primary_export_snapshot_transport(include_data)` returns a `SnapshotTransport` with the
    database file copy as bytes (up to 1 GiB), and `primary_export_log_transport(cursor, max_frames, max_bytes,
    include_payload)` a `LogTransportPage` with raw frame payloads. The `*_json` variants serialize the same values
    as JSON, with the bytes in base64 (snapshot data up to 32 MiB).
  - Node NAPI, on `Database` and `Kite`: `exportReplicationSnapshotTransport(includeData?)` and
    `exportReplicationLogTransport(cursor?, maxFrames?, maxBytes?, includePayload?)` return Buffers; the
    `exportReplication*TransportJson` methods return JSON. Free functions for a `Database`:
    `collectReplicationSnapshotTransport(db, includeData?)`, `collectReplicationLogTransport(db, ...)` and their
    `...Json` variants.
  - TypeScript adapter helper: `createReplicationTransportAdapter(dbOrKite)` in `ray-rs/ts/replication_transport.ts`.
    `snapshot()` / `log()` return the JSON-shaped objects (built from the binary exports, with no JSON round trip);
    `snapshotBinary()` / `logBinary()` return the raw Buffers for hosts that send binary bodies.
  - TypeScript admin auth helper: `createReplicationAdminAuthorizer({ mode, token, mtlsMatcher?, trustForwardedClientCert?, mtlsHeader?, mtlsSubjectRegex? })`
    for `none|token|mtls|token_or_mtls|token_and_mtls`. `mode` is required (`'none'` disables auth explicitly) and
    tokens are compared in constant time. The mTLS modes need a native TLS verifier hook (`mtlsMatcher`), or
    `trustForwardedClientCert: true` with `mtlsSubjectRegex`, which must match the whole `mtlsHeader` value; trust
    the header only behind a proxy that verifies client certificates and overwrites it on every request.
  - TypeScript native TLS matcher helper: `createNodeTlsMtlsMatcher({ requirePeerCertificate? })`
    and probe helper `isNodeTlsClientAuthorized(request, options?)` for common Node request socket shapes
    (`request.socket`, `request.client`, `request.raw.socket`, `request.req.socket`).
  - TypeScript forwarded-header matcher helper: `createForwardedTlsMtlsMatcher({ requirePeerCertificate?, requireVerifyHeader?, verifyHeaders?, certHeaders?, successValues? })`
    and probe helper `isForwardedTlsClientAuthorized(request, options?)` for proxy-terminated TLS/mTLS in non-Node-native runtimes.
  - Python PyO3: `Database.export_replication_snapshot_transport(include_data=False)` and
    `Database.export_replication_log_transport(cursor=None, max_frames=128, max_bytes=1048576, include_payload=True)`
    return dicts with `bytes`; `collect_replication_snapshot_transport[_json](db, include_data=False)` and
    `collect_replication_log_transport[_json](db, ...)` are the module-level forms.
  - Transport contract:
    - The snapshot is a consistent copy: it is read under the database's checkpoint gate and commit lock (commits
      wait for the copy), and it holds every commit up to `head_log_index` and none after it. In `SyncMode::Off`
      the commits held only in memory are written to the file first.
    - `start_cursor` is the position right after the head frame: pull the log from it to get exactly the commits
      after the snapshot.
    - Both transports carry `generation`, the sidecar's log history (16 hex digits, a string so JSON clients read
      it exactly). An HTTP replica records it with its cursor; a page with another generation comes from a
      recreated sidecar, and the replica must reseed, as file-based replicas do.
    - The snapshot no longer carries `db_path`.
  - Python host auth helper: `create_replication_admin_authorizer(...)` with `ReplicationAdminAuthConfig`
    and ASGI native TLS matcher helpers `create_asgi_tls_mtls_matcher(...)` / `is_asgi_tls_client_authorized(...)`.
  - These are intended for embedding host-side HTTP endpoints beyond playground runtime.
  - Template files:
    - Node Express adapter: `docs/examples/replication_adapter_node_express.ts`
    - Node proxy-forwarded adapter: `docs/examples/replication_adapter_node_proxy_forwarded.ts`
    - Python FastAPI adapter: `docs/examples/replication_adapter_python_fastapi.py`
    - Generic middleware adapter: `docs/examples/replication_adapter_generic_middleware.ts`

Alert heuristics:

- `append_failures > 0` growing: primary sidecar durability issue.
- `sidecar_needs_repair == true`: stop expecting sidecar progress, repair or reseed it before resuming replication; later frames are intentionally not appended over the gap.
- Replica lag growth over steady traffic: pull/apply bottleneck.
- `needs_reseed == true`: force reseed, do not keep retrying catch-up.

## 2. Bootstrap a New Replica

Prerequisite:

- Quiesce writes on the source primary during `replica_bootstrap_from_snapshot()`.
- If writes continue, bootstrap now fails fast with a `quiesce writes and retry` error. The check compares the
  source file's length, modification time and header pages, and its replication head, at the start and the end of
  the copy (it no longer checksums the whole file).
- The copy commits in batches (10k writes, or 1/8 of the replica's WAL), so a graph larger than the replica's WAL
  fits; the replica checkpoints between batches once its WAL passes half full, also with auto-checkpoint off. From
  the first batch until the bootstrap sets its cursor the replica is marked incomplete, and catch-up refuses to run
  (`needs_reseed`) if the bootstrap stops partway: run it again.

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

Replica catch-up applies each contiguous run of frames in one transaction (split at 1/8 of the replica's WAL) and
moves its cursor once per pull. In Normal and Off sync modes the replica first makes the applied commits durable
(writes its WAL and header and syncs, as close does), so a crash never leaves the cursor ahead of the data; a crash
before the cursor moves makes the replica apply those frames again, which converges. A bootstrap does the same
before it sets its cursor. A run that fails is retried one frame per transaction, so the frames before the
failing one still apply and the error names the failing frame (`replica apply failed at epoch:log_index`).

Primary:

- Report each replica cursor via `primary_report_replica_progress(replica_id, epoch, applied_log_index)`.
- Run `primary_run_retention()` on an operator cadence.
- Decommissioned replica: `primary_remove_replica_progress(replica_id)` (Node `primaryRemoveReplicaProgress`)
  forgets its progress, so it stops holding back the retention floor; the next retention run prunes past it. A
  replica that reports again is tracked again.

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
2. Quiesce writes on the source primary.
3. Execute `replica_reseed_from_snapshot()`.
4. Resume `replica_catch_up_once(...)`.
5. Verify:
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
- Commit overhead gate: require median p95 ratio (replication-on / baseline) within `P95_MAX_RATIO` (default `1.30`, `ATTEMPTS=7`).
- Catch-up gate: require replica throughput floors (`MIN_CATCHUP_FPS`, `MIN_THROUGHPUT_RATIO`).
- Catch-up gate retries benchmark noise by default (`ATTEMPTS=3`); increase on busy dev machines.
- CI on `main` (`.github/workflows/ray-rs.yml`) enforces replication perf gate and uploads benchmark logs as `replication-perf-gate-logs` (run-scoped `ci-<run_id>-<run_attempt>` stamp).
- CI also runs non-blocking replication soak tracking weekly and supports manual deep runs via workflow input `replication_soak_profile=fast|full` (artifact `replication-soak-tracking-logs`).

## 8. HTTP Admin Endpoints (Playground Runtime)

Available endpoints in `playground/src/api/routes.ts`:

- `GET /api/replication/status`
- `GET /api/replication/metrics` (Prometheus text format)
- `GET /api/replication/snapshot/latest`
- `GET /api/replication/log`
- `GET /api/replication/transport/snapshot` (the connected Kite's snapshot transport, as JSON)
- `GET /api/replication/transport/log` (the connected Kite's log transport, as JSON)
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
- `curl -H "x-client-cert: CN=allowed-client,O=KiteDB" "$BASE/api/replication/metrics"` (when `REPLICATION_ADMIN_AUTH_MODE=mtls`)

## 9. Known V1 Limits

- Retention policy supports entry-window + time-window floors, but not richer SLA-aware policies.
- Bundled HTTP admin endpoints still ship in playground runtime; host runtime now exposes transport JSON helpers for embedding custom HTTP surfaces.
- OTLP retry policy is bounded attempt/backoff/jitter with optional adaptive multiplier (`linear` or `ewma`) and circuit-breaker half-open probes. Circuit-breaker state is process-local by default; optional file-backed sharing (`circuit_breaker_state_path`) or shared HTTP store (`circuit_breaker_state_url`) is available with `circuit_breaker_scope_key`; URL backend can enable key-scoped patch mode (`circuit_breaker_state_patch`), batched patch mode (`circuit_breaker_state_patch_batch` with `circuit_breaker_state_patch_batch_max_keys`), compacting merge patch mode (`circuit_breaker_state_patch_merge` with `circuit_breaker_state_patch_merge_max_keys`), bounded patch retries (`circuit_breaker_state_patch_retry_max_attempts`), CAS (`circuit_breaker_state_cas`), and lease header propagation (`circuit_breaker_state_lease_id`).
- Vector authority boundary: logical vector property mutations (`SetNodeVector` / `DelNodeVector`) are authoritative and replicated; vector batch/fragment maintenance records are treated as derived index artifacts and are skipped during replica apply.
- `SyncMode::Normal` and `SyncMode::Off` optimize commit latency by batching sidecar frame writes in-memory and refreshing manifest fencing periodically (not every commit). For strict per-commit sidecar visibility/fencing, use `SyncMode::Full`.
- The sidecar syncs as the database does: in `SyncMode::Full` each commit's frame (and the manifest naming it) is synced before the commit returns, with a plain `fsync`, or `F_FULLFSYNC` on macOS with the `full_fsync` opt-in; in Normal and Off modes frames are synced at checkpoint and close. Metadata files (manifest, health, progress, replica cursor) are always replaced atomically with their content synced first.
- Epoch fencing runs under the commit lock: a primary promoted away while a commit waits for the lock rejects that commit (`stale primary`). A promotion that lands while a commit is being written cannot stop it locally; that commit's sidecar append is fenced instead, and it reaches no replica.

## 10. V1 Release Checklist

1. Correctness gate:
   - `cd ray-rs && cargo test --no-default-features --test replication_phase_a --test replication_phase_b --test replication_phase_c --test replication_phase_d --test replication_faults_phase_d`
2. Host-runtime flow gate:
   - `cd ray-rs && bunx ava __test__/replication_transport_auth.spec.ts __test__/replication_transport_flow.spec.ts`
   - `cd ray-rs && .venv/bin/python -m pytest -q python/tests/test_replication_auth.py python/tests/test_replication_transport_flow.py`
3. Performance gate (release-like host):
   - `cd ray-rs && ./scripts/replication-perf-gate.sh`
   - `cd ray-rs && ./scripts/replication-soak-gate.sh`
   - `cd ray-rs && ./scripts/vector-ann-gate.sh`
4. Artifact capture:
   - ensure benchmark logs are written under `docs/benchmarks/results/` with a dedicated `STAMP` for the release run.
5. Release preflight checks (AGENTS rules):
   - `cd ray-rs && ./scripts/release-preflight.sh --commit-msg \"core: X.Y.Z\" --tag vX.Y.Z`
   - This enforces:
     - exact commit message format `all|js|ts|py|rs|core: X.Y.Z` (no trailing text),
     - tag format `vX.Y.Z`,
     - `ray-rs/package.json` version == tag version,
     - commit message version == tag version.
6. Cut release commit and tag:
   - commit message must be exactly one of:
     - `all: X.Y.Z`
     - `js: X.Y.Z`
     - `ts: X.Y.Z`
     - `py: X.Y.Z`
     - `rs: X.Y.Z`
     - `core: X.Y.Z`
   - then create tag `vX.Y.Z` and push commit + tag.
