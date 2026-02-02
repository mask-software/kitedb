# Anti-Pattern Audit Report (raydb/kitedb)

**Scope:** Rust core (`ray-rs/`) — 66k lines, 134 Rust files. Embedded graph DB with MVCC, WAL, vector search.

---

## Summary Dashboard

| Category | Issues | High | Medium | Low |
| --- | ---:| ---:| ---:| ---:|
| `.clone()` abuse | 15 | 5 | 8 | 2 |
| `.unwrap()` / `.expect()` in prod | 15 | 9 | 6 | 0 |
| String overuse | 10 | 3 | 4 | 3 |
| Unsafe blocks | 30 | 0 | 3 | 27 |
| Large functions | 10 | 2 | 5 | 3 |
| Arc/Rc misuse | 0 | 0 | 0 | 0 |

---

## High Priority Issues

### 1) Clone Anti-Patterns (5 High)

| Location | Issue | Impact |
| --- | --- | --- |
| `single_file/transaction.rs:88` | Clone entire tx struct on commit | Critical write path |
| `single_file/transaction.rs:183` | Clone `NodeDelta` in loop | O(n) clones per commit |
| `single_file/vector.rs:190` | Clone `Vec<f32>` in iterator | Large vectors |
| `mvcc/tx_manager.rs:335` | Clone String keys in prune loop | Every commit |
| `napi_bindings/kite/builders.rs:219+` | Clone `HashMap<String, PropValue>` per item | Batch insert hot path |

### 2) Unwrap/Expect Anti-Patterns (9 High)

| Location | Issue | Risk |
| --- | --- | --- |
| `api/pathfinding.rs:195,359` | `.get().unwrap()` in Dijkstra/A* | User-facing panic |
| `api/builders.rs:165,207,262,310` | `expect()` on builder misuse | API panic vs Result |
| `vector/ivf/serialize.rs:221-234` | `try_into().unwrap()` on external data | Malformed file panic |
| `vector/ivf_pq.rs:589` | `Option::unwrap()` in search loop | Logic error panic |

### 3) String Overuse (3 High)

| Location | Issue | Impact |
| --- | --- | --- |
| `mvcc/tx_manager.rs:123,132` | `record_read/write(key: String)` | Hot path; per read/write |
| `cache/manager.rs:358` | `set_node_by_key(key: String)` | Cache ops frequent |
| `api/kite.rs:2748+` | `.to_string()` in builder per prop | Unnecessary allocations |

---

## Medium Priority Issues

### Clone Patterns (8)
- Property value clones during reads/checkpoints
- MVCC version chain data clones
- `KeySpec` clones on every API call
- Header double-clone in compactor

### Unwrap Patterns (6)
- `partial_cmp().unwrap()` on floats (NaN panic risk)
- `is_some() + unwrap()` anti-pattern
- Internal invariant unwraps without SAFETY justification

### String Patterns (4)
- `NodeRef.node_type: String` should be `Cow<'static, str>`
- Static error strings allocated as `String`
- Double `.to_string()` during schema initialization

### Unsafe Concerns (3)
- `mmap.rs`: external file mutation during mapping => potential UB
- `CacheManager` Send/Sync relies on external lock discipline
- LRU iterator relies on borrow rules; add SAFETY docs

### Large Functions (5)
- `collect_graph_data`: 223 lines, 5+ nested loops
- `yen_k_shortest`: 173 lines, complex path reconstruction
- `a_star`: 145 lines, overlaps Dijkstra logic
- `IvfIndex::search`: 120 lines, mixed concerns
- `vector_store_insert`: 101 lines, fragment management

---

## What’s Good

- Arc/Rc hygiene excellent (no Rc, no Arc for Copy types)
- Unsafe usage minimal and largely justified
- Clippy enabled (`#![deny(clippy::all)]`)

---

## Recommended Action Plan

### Phase 1 — Quick Wins
1. Add SAFETY comments to missing unsafe blocks
2. Replace `is_some() + unwrap()` with `if let Some(x)`
3. Convert builder `expect()` to `Result` or typestate

### Phase 2 — Hot Path Optimizations
1. Avoid full tx clone on commit
2. Use `Cow<'static, str>` for schema names and error strings
3. Accept `&str` or intern for MVCC key tracking

### Phase 3 — Structural Refactors
1. Extract helpers from 100+ line functions
2. Share code between Dijkstra and A* via helper/trait
3. Consider `Arc<PropValueInner>` for zero-copy props

---

**Next:** Choose a phase, and I’ll implement fixes + tests.
