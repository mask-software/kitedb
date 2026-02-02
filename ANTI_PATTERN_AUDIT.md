# Anti-Pattern Audit Report (raydb/kitedb)

**Scope:** Rust core (`ray-rs/`). Embedded graph DB with MVCC, WAL, vector search.

---

## Status (current)

**Resolved (high/medium)**
- Clone hot paths: tx clone, NodeDelta clone, vector clone, MVCC prune clone, NAPI builder clones.
- Unwraps in prod: pathfinding, ivf serialize, pq ordering; is_some+unwrap cleaned.
- String hot paths: MVCC keys → `TxKey`, key cache → `Arc<str>`, builder props → `Into<String>`.
- NAPI KeySpec: shared via `Arc<KeySpec>` (no per-call clones).
- MVCC prop clones → shared `Arc<PropValue>` storage.
- Large functions refactored: `collect_graph_data`, `yen_k_shortest`, `a_star`, `IvfIndex::search`, `vector_store_insert`.
- Unsafe concerns: LRU SAFETY docs, CacheManager Send/Sync SAFETY note, mmap stale remap guard.
- Error strings: `KiteError::InvalidSchema/InvalidQuery` → `Cow<'static, str>`.
- Schema builders: reduced string churn + tests cleaned.
- Compactor vacuum: removed header double-clone.

**Remaining (open)**
1) **Residual invariant unwraps** — low-risk scan only.

---

## Highlights of fixes applied

- MVCC tracking now uses typed `TxKey` (no string churn).
- NodeRef `node_type` now `Arc<str>`.
- Prop value storage shared across delta + MVCC via `Arc`.
- mmap remap guard added for file-size changes.

---

## Next actions

1) **Optional**: scan for remaining invariant unwraps
