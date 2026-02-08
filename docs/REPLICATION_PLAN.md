# KiteDB Replication V1 Plan (Feature + Code)

Status: draft (implementation-ready)

## 1) Goals

- Single-writer primary, multiple read replicas.
- Keep local embedded path default and fastest when replication is disabled.
- Add optional read-your-writes on replicas via commit token wait.
- Manual replica promotion to primary (no automatic election in V1).

## 2) Non-Goals (V1)

- Multi-primary / multi-writer.
- Automatic leader election / consensus.
- WAN topology optimization and geo-routing.
- Replicating rebuildable derived indexes as required state.

## 3) Scope

- Engine: single-file `.kitedb` path only.
- Topology target: `1 primary + up to 5 replicas`.
- Transport target: pull-based replication first (HTTP contract), push later without format break.
- API policy: additive only.

## 4) Replication Invariants

1. Exactly one writable primary per epoch.
2. Replica apply order is commit order from primary.
3. Replica apply is idempotent by log index.
4. Commit token monotonicity per epoch.
5. Checkpoint/compaction on primary must not break replica catch-up semantics.
6. If replication is disabled, existing behavior and performance profile remain unchanged.

## 5) Data Model: Source-of-Truth vs Derived

### Authoritative replicated state

- Committed transaction stream (logical mutation records).
- Snapshot checkpoint image + metadata.
- Replication epoch and monotonic log index.

### Derived/rebuildable state (not required for correctness replication)

- Caches (`cache::*`).
- In-memory overlays reconstructed from snapshot + replicated tx stream.
- Rebuildable vector/search side structures (unless explicitly marked authoritative in future phases).

## 6) Consistency Model

- Default replica reads: eventual/async.
- Optional stronger read: provide commit token and wait until `applied_log_index >= token.log_index`.
- Write ack policy: primary acks after local durability boundary only (replicas async).

## 7) Durability and Crash Boundaries

Commit must define explicit durability points:

1. Primary WAL commit record persisted per current `sync_mode` rules.
2. Replication log frame append persisted for the same commit.
3. Commit token returned only after replication log append is durable.

Crash model requirements:

- Crash before token return: client may retry safely (idempotency via tx semantics/log index handling).
- Crash after token return: token must correspond to durable replication log frame.
- Replica restart resumes from persisted cursor with idempotent re-apply.

## 8) Compatibility and Versioning

- Keep `.kitedb` format backward compatible in V1.
- Replication metadata lives in versioned sidecar manifest + segments.
- Promotion increments epoch; stale writers must be fenced by epoch checks.

## 9) Architecture (V1)

### 9.1 Replication log sidecar

- New sidecar directory adjacent to DB file.
- Segment files: append-only, checksummed tx frames.
- Manifest: current epoch, head index, retained floor, active segment metadata.
- Cursor: `epoch:segment_id:offset:log_index`.

### 9.2 Primary responsibilities

- On commit, append committed tx frame to replication sidecar.
- Expose snapshot + log pull interfaces.
- Track replica progress (last acknowledged cursor/index) for retention decisions.

### 9.3 Replica responsibilities

- Bootstrap from latest snapshot bundle.
- Catch up via log pull from snapshot start cursor.
- Persist applied cursor atomically after apply batch.
- Serve reads immediately or wait-for-token when requested.

## 10) Code Touch Points

Core engine:

- `ray-rs/src/core/single_file/transaction.rs`
  - Commit hook for replication append + token emission.
- `ray-rs/src/core/single_file/open.rs`
  - Role/config wiring (primary/replica settings).
- `ray-rs/src/core/single_file/recovery.rs`
  - Shared replay semantics reuse for replica apply path.
- `ray-rs/src/metrics/mod.rs`
  - Replication lag/apply metrics.

New module tree:

- `ray-rs/src/replication/mod.rs`
- `ray-rs/src/replication/types.rs`
- `ray-rs/src/replication/manifest.rs`
- `ray-rs/src/replication/log_store.rs`
- `ray-rs/src/replication/primary.rs`
- `ray-rs/src/replication/replica.rs`
- `ray-rs/src/replication/token.rs`
- `ray-rs/src/replication/transport.rs`

Binding surface (additive):

- `ray-rs/src/napi_bindings/database.rs`
- `ray-rs/src/pyo3_bindings/database.rs`

## 11) API/Interface Additions (Additive)

- Open options:
  - replication role (`primary` | `replica` | `disabled`)
  - replication sidecar path (optional default derived from DB path)
  - pull/apply tuning (chunk bytes, poll interval, max batch)
- Primary status:
  - replication head index/epoch
  - retained floor
  - per-replica lag
- Replica status:
  - applied index/epoch
  - last pull/apply error
- Read wait:
  - `wait_for_token(token, timeout_ms)` style helper.

## 12) Transport Contract (Pull-First)

- `GET /replication/snapshot/latest`
  - Returns snapshot bytes + metadata (checksum, epoch, start cursor/index).
- `GET /replication/log?cursor=...&max_bytes=...`
  - Returns ordered tx frames + next cursor + eof marker.
- `GET /replication/status`
  - Primary/replica status for observability.
- `POST /replication/promote`
  - Manual promotion to next epoch (authenticated).

Protocol requirement: all payloads versioned to allow push transport later with same frame/cursor model.

## 13) Retention Policy

- Segment rotation by size (default 64MB).
- Retain at least:
  - minimum time window (operator-configured), and
  - min cursor needed by active replicas.
- If replica falls behind retained floor:
  - mark `needs_reseed`,
  - force snapshot bootstrap.

## 14) Failure Modes and Handling

1. Corrupt segment/frame checksum:
   - stop apply, surface hard error, require retry/reseed policy.
2. Missing segment due to retention:
   - deterministic `needs_reseed` status.
3. Network interruption:
   - retry with backoff, resume from durable cursor.
4. Promotion race:
   - epoch fencing rejects stale primary writes.
5. Primary crash mid-commit:
   - recovery ensures token/log durability invariant holds.

## 15) Performance Constraints

- Disabled replication path: <3% regression on write/read microbenchmarks.
- Enabled replication:
  - bounded p95 commit overhead target (to be locked in benchmark baseline run).
  - replica apply throughput >= primary sustained commit rate at target topology.
- Keep commit hot path branch-light when replication disabled.

## 16) Test-Driven Delivery Model (Red/Green First)

### Phase workflow (mandatory)

1. Red:
   - Define phase contract/invariants.
   - Add failing tests for that phase before implementation.
2. Green:
   - Implement only enough to pass the new failing tests.
3. Refactor/Hardening:
   - Cleanups, edge-case coverage, failure-path tests, perf checks.
4. Phase gate:
   - No phase is complete until all red tests are green and phase exit checks pass.

### Test layout

- Module-level tests in `ray-rs/src/replication/*` for parser/state invariants.
- Cross-module integration tests in `ray-rs/tests/replication_*.rs`.
- Fault-injection tests in dedicated `ray-rs/tests/replication_faults_*.rs`.
- Perf checks in existing benchmark harnesses with replication-on/off variants.

### Global test matrix

- Unit:
  - cursor/token encode/decode.
  - frame checksum and parse validation.
  - segment rotation and retention math.
  - idempotent apply for duplicate/replayed chunks.
- Integration:
  - snapshot bootstrap + incremental catch-up.
  - replica restart + resume cursor.
  - background checkpoint during active replication.
  - token wait semantics on replica.
  - manual promotion and stale writer fencing.
- Fault injection:
  - crash before/after token return boundary.
  - truncated frame/chunk.
  - corrupt snapshot metadata.
  - replica far behind retained floor.
- Performance:
  - baseline local mode (replication off).
  - replication-on write latency/throughput.
  - catch-up time for large backlog.

## 17) Detailed Delivery Phases (Per-Phase Red/Green Gates)

### Phase A: Invariants + sidecar primitives

Objective:
- Freeze wire/storage invariants and build deterministic sidecar primitives.

Red tests first:
- Invalid token/cursor strings are rejected.
- Token/cursor ordering comparator is monotonic and epoch-aware.
- Corrupt segment frame checksum fails read/scan.
- Manifest interrupted-write simulation never yields partial-valid state.
- Segment append/read roundtrip preserves frame boundaries and indices.

Green implementation:
- Add `replication` module skeleton and core types.
- Implement versioned manifest read/write with atomic replace semantics.
- Implement segment append/read and frame checksum verification.
- Freeze token/cursor format and parser behavior.

Robustness checks:
- Fuzz/property-like tests on token/cursor parser.
- Recovery tests for manifest reload after simulated interruption.

Phase exit criteria:
- All Phase A red tests green.
- No API breakage.
- Sidecar primitives deterministic across restart.

### Phase B: Primary commit integration

Objective:
- Integrate replication append/token generation into primary commit path without regressing disabled mode.

Red tests first:
- Commit returns monotonic token (`epoch:log_index`) for successful writes.
- Replication-disabled mode produces no sidecar append activity.
- Sidecar append failure causes commit failure (no token emitted).
- Commit ordering remains serialized and token order matches commit order under concurrent writers.
- Crash boundary test: token is never returned for non-durable replication frame.

Green implementation:
- Hook replication append into `single_file::transaction::commit`.
- Add replication config wiring in open options.
- Emit token and expose primary replication status.
- Add basic replication metrics counters/gauges.

Robustness checks:
- Regression benchmark: replication off path <3% overhead.
- Negative-path tests for IO errors on sidecar append/fsync.

Phase exit criteria:
- All Phase B red tests green.
- Disabled path performance gate passes.
- Durability/token invariant verified by crash-boundary tests.

### Phase C: Replica bootstrap + steady-state apply

Objective:
- Build replica bootstrap/catch-up/apply loop with idempotency and token-wait semantics.

Red tests first:
- Replica bootstrap from snapshot reaches exact primary state.
- Incremental catch-up applies committed frames in order.
- Duplicate chunk delivery is idempotent (no double-apply).
- Replica restart resumes from durable cursor without divergence.
- Token wait returns success on catch-up and timeout when lag persists.

Green implementation:
- Implement snapshot bootstrap flow and continuity validation.
- Implement pull loop (`cursor`, `max_bytes`, retry/backoff).
- Implement apply pipeline using replay semantics + applied-index persistence.
- Add replica status surface (applied index, lag, last error).

Robustness checks:
- Checkpoint interleaving tests (primary background checkpoint while replica catches up).
- Large backlog catch-up throughput and memory boundedness tests.

Phase exit criteria:
- All Phase C red tests green.
- Replica apply remains deterministic across restart/retry scenarios.
- Token-wait semantics validated end-to-end.

### Phase D: Promotion + retention + hardening

Objective:
- Add manual promotion with fencing and finalize retention/failure behavior.

Red tests first:
- Promotion increments epoch and fences stale primary writes.
- Retention respects min active replica cursor and configured minimum window.
- Missing segment response deterministically marks replica `needs_reseed`.
- Lagging replica beyond retention floor requires snapshot reseed and recovers.
- Promotion race cases do not allow split-brain writes.

Green implementation:
- Implement manual promote flow and epoch fencing checks.
- Implement replica progress tracking and retention pruning.
- Add explicit reseed path/status when continuity is broken.
- Finalize status/admin interfaces for ops visibility.

Robustness checks:
- Fault-injection sweep for corruption/network/partial transfer.
- Soak tests at target topology (`1 + up to 5`) with lag churn.

Phase exit criteria:
- All Phase D red tests green.
- No split-brain write acceptance in promotion tests.
- Retention and reseed behavior deterministic and observable.

## 18) Per-Phase Done Definition

- Phase-specific red tests were added before implementation.
- Green implementation passed with no skipped phase tests.
- Failure-mode tests for that phase are green.
- Metrics/status fields for that phase are present and documented.
- Phase summary notes include known limits and next-phase carry-over items.

## 19) Open Questions

- Commit overhead budget is fixed for V1 gate: `P95_MAX_RATIO=1.03` (replication-on p95 / baseline p95).
- Host-runtime TLS client-cert enforcement design (beyond playground proxy-header mTLS checks).
- Whether any vector side data must be promoted to authoritative replicated state in a later phase.

## 20) Phase D Summary (February 8, 2026)

Implemented:
- Manual promotion API with epoch fencing (`stale primary` rejected on stale writer commit).
- Retention controls (segment rotation threshold + min retained entries) and primary retention execution.
- Time-window retention control (`replication_retention_min_ms`) to avoid pruning very recent segments.
- Replica progress reporting and per-replica lag visibility on primary status.
- Deterministic reseed signaling (`needs_reseed`) for retained-floor/continuity breaks.
- Explicit replica reseed API from snapshot.
- Binding parity for replication admin/status in Node NAPI and Python PyO3 surfaces.
- Host-runtime Prometheus replication exporter API in Rust core + Node NAPI + Python PyO3 (`collect_replication_metrics_prometheus*`).
- Host-runtime OpenTelemetry OTLP-JSON replication exporter API in Rust core + Node NAPI + Python PyO3 (`collect_replication_metrics_otel_json*`).
- Host-runtime OpenTelemetry collector push transport (HTTP OTLP-JSON) in Rust core + Node NAPI + Python PyO3 (`push_replication_metrics_otel_json_single_file`, `pushReplicationMetricsOtelJson`, `push_replication_metrics_otel_json`).
- Host-runtime OTLP transport hardening for TLS/mTLS (HTTPS-only mode, custom CA trust, optional client cert/key auth).
- Host-runtime replication transport JSON export surfaces for embedding HTTP endpoints beyond playground runtime:
  - snapshot export (`collectReplicationSnapshotTransportJson` / `collect_replication_snapshot_transport_json`)
  - log page export with cursor/limits (`collectReplicationLogTransportJson` / `collect_replication_log_transport_json`).
  - TypeScript adapter helper (`createReplicationTransportAdapter`) for wiring custom HTTP handlers.
- Replica source transport hardening in host-runtime open path (required source DB path + source/local sidecar collision fencing).
- Operator runbook for promotion/reseed/retention tuning (`docs/REPLICATION_RUNBOOK.md`).
- Replication benchmark gate script (`ray-rs/scripts/replication-bench-gate.sh`) + benchmark doc wiring.
- Replica catch-up throughput gate (`ray-rs/scripts/replication-catchup-gate.sh`) and combined perf gate (`ray-rs/scripts/replication-perf-gate.sh`).
- HTTP transport/admin rollout in playground runtime:
  - `GET /api/replication/status`
  - `GET /api/replication/metrics` (Prometheus text export)
  - `GET /api/replication/snapshot/latest`
  - `GET /api/replication/log`
  - `GET /api/replication/transport/snapshot` (host-runtime transport export passthrough)
  - `GET /api/replication/transport/log` (host-runtime transport export passthrough)
  - `POST /api/replication/pull`
  - `POST /api/replication/reseed`
  - `POST /api/replication/promote`
  - configurable admin auth via `REPLICATION_ADMIN_AUTH_MODE` (`token|mtls|token_or_mtls|token_and_mtls`).
  - native HTTPS listener + TLS client-cert enforcement support for mTLS auth in playground runtime.

Validated tests:
- `ray-rs/tests/replication_phase_d.rs` (promotion, retention, reseed, split-brain race).
- `ray-rs/tests/replication_faults_phase_d.rs` (corrupt/truncated segment fault paths + durable `last_error`).

Known limits:
- Bundled HTTP admin endpoints currently ship in playground runtime only; host runtime provides JSON export helpers for embedding custom endpoints.
- Host-runtime OTLP export currently targets HTTP OTLP-JSON payloads only (no protobuf/gRPC exporter path).

Carry-over to next phase:
- Polyglot host-runtime HTTP adapter templates (Python + generic middleware examples) on top of transport JSON helpers.
