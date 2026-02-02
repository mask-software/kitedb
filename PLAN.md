# KiteDB Turso-Style Scale Plan (Single-File, No Vectors)

**Scope**
- Single-file storage only (multi-file will be dropped)
- Vectors ignored for now
- Local-first performance preserved (default path unchanged)
- Production readiness as a requirement

---

## Phase 0 — Architecture + invariants
**Goals:** Define correctness guarantees and operational constraints before code changes.

**Tasks**
- [ ] Define replication invariants: commit ordering, idempotency, replay determinism
- [ ] Choose cursor format (recommended: `segment_id + offset`) and WAL rotation policy
- [ ] Define durability modes (default: fsync per commit; optional group commit)
- [ ] Document local-first guarantee (no behavior change when features disabled)

**Exit checklist**
- [ ] Written spec with dataflow + correctness guarantees
- [ ] Agreed cursor format and retention strategy

---

## Phase 1 — Storage abstraction (local-first)
**Goals:** Add a page-IO interface while keeping current local performance intact.

**Tasks**
- [ ] Introduce `PageManager` trait for page read/write/flush
- [ ] Adapt `FilePager` to `LocalPageManager` without behavior change
- [ ] Thread `PageManager` through single-file open/write/recovery path
- [ ] Add config toggle `storage=local|remote` (default local)

**Exit checklist**
- [ ] All existing tests pass with local mode
- [ ] Local-path microbenchmarks show no regression beyond threshold

---

## Phase 2 — WAL streaming foundations
**Goals:** Enable replication by streaming WAL from a primary.

**Tasks**
- [ ] Add WAL slicing API: read bytes by `(segment_id, offset, max_bytes)`
- [ ] Define WAL chunk format + metadata (next cursor, eof, checksum)
- [ ] Implement HTTP endpoint `GET /wal?segment=&offset=&max_bytes=`
- [ ] Add replica apply pipeline (parse → apply committed tx in order)

**Exit checklist**
- [ ] Replica can catch up from a known cursor
- [ ] WAL replay is deterministic and idempotent for committed tx

---

## Phase 3 — Snapshot bootstrap
**Goals:** Allow new replicas to seed from a snapshot and catch up via WAL.

**Tasks**
- [ ] Add `GET /snapshot/latest` endpoint
- [ ] Define snapshot metadata: generation, checksum, WAL start cursor
- [ ] Implement replica bootstrap flow (snapshot → WAL catch-up)
- [ ] Validate snapshot + WAL continuity checks

**Exit checklist**
- [ ] Fresh replica bootstraps to exact primary state
- [ ] Snapshot integrity verified by checksum

---

## Phase 4 — WAL retention + replica tracking
**Goals:** Ensure WAL rotation doesn’t break replicas.

**Tasks**
- [ ] Track per-replica cursor on primary
- [ ] Implement retention policy based on min replica cursor
- [ ] Add admin endpoints/CLI to inspect WAL status + replica lag

**Exit checklist**
- [ ] WAL segments rotated safely
- [ ] Replica lag observable and enforceable

---

## Phase 5 — Remote page store (optional)
**Goals:** Decouple compute from storage with a remote-backed page store.

**Tasks**
- [ ] Implement `RemotePageManager` with hot cache (LRU, size limit)
- [ ] Add remote backing store interface (S3/HTTP blob store)
- [ ] Read-through on miss; write-through on write
- [ ] Retry/backoff, checksum validation, cache metrics

**Exit checklist**
- [ ] DB can run with minimal local disk using remote storage
- [ ] Cache hit rate and latency are observable

---

## Phase 6 — Production readiness
**Goals:** Operational safety, observability, and resilience.

**Tasks**
- [ ] Metrics: WAL lag, throughput, cache hit rate, snapshot size
- [ ] Failure tests: crash mid-commit, partial WAL replay, network loss
- [ ] Load tests: high-write workload + replica catch-up under pressure
- [ ] Observability dashboards + alerts for replication lag/corruption

**Exit checklist**
- [ ] Documented SLOs and runbooks
- [ ] Resilience tests pass consistently

---

## Phase 7 — Performance guardrails
**Goals:** Preserve local performance and enforce regression limits.

**Tasks**
- [ ] Microbenchmarks: page read/write, commit latency, query latency
- [ ] Macrobenchmarks: mixed read/write workload vs baseline
- [ ] CI thresholds: max regression % (e.g., <3% in local mode)

**Exit checklist**
- [ ] Baselines recorded
- [ ] Regression gates enforced in CI

---

## Notes
- Local mode must remain the default and the fastest path.
- Replication and remote storage are opt-in via configuration.
- Production readiness includes durability, observability, and recovery guarantees.
