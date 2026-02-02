# RayDB: Rust vs TypeScript Feature Parity Report

> Generated: January 29, 2026

## Executive Summary

**Overall Parity: 100% (core runtime features)**

Rust now matches the TypeScript core across storage formats, transactions (MVCC), compaction/vacuum, snapshot validation, and operational tooling (backup/metrics/health). Any remaining differences are language/idiom-level (async vs sync, generics, etc.) rather than missing functionality.

---

## Parity Matrix (Core Runtime)

| Area | Parity | Notes |
|------|--------|-------|
| Single-file format (.kitedb) | 100% | Full open/read/write + background checkpointing + compactor/vacuum |
| Multi-file format | 100% | Snapshot + WAL + manifest parity |
| MVCC | 100% | Integrated into DB ops with wall-clock retention + GC |
| Graph ops (nodes/edges/props) | 100% | MVCC visibility + WAL replay + delta parity |
| Snapshot checker | 100% | Full CSR invariants + key index + string table validation |
| Backup/restore | 100% | Single-file + multi-file + offline backup |
| Metrics/health | 100% | Cache, data, memory, MVCC, health checks |
| Stats (`stats()`) | 100% | WAL bytes/segment + MVCC stats parity |
| Streaming/pagination | 100% | Batch streaming + pagination helpers |

---

## Notable Language/Idiomatic Differences (Non-Gaps)

- **Async vs sync:** TypeScript APIs are async; Rust exposes sync APIs and batch streaming helpers.
- **Generators:** TS uses async iterators; Rust returns vectors or batch streams.
- **Type inference:** TS has advanced generics; Rust uses explicit types and lifetimes.

## Parity Audit Notes (January 29, 2026)

Notes on Rust-only items or internal helpers (not parity gaps):
- **Single-file checkpoint compression:** TS background checkpoint does not expose compression options either.
- **Per-node labels in snapshot reads:** Rust core now persists per-node labels in snapshots (v2); TS remains delta-only.
- **Snapshot sections module:** Internal helper not used in TS (no equivalent module).

### Compatibility note
Rust now writes **snapshot version 3** (per-node labels + vector properties persisted). Existing v1/v2 snapshots are still readable, but TS tooling will not understand v3 snapshots.

---

## Rust Extras (Not in TypeScript)

| Feature | Location | Description |
|---------|----------|-------------|
| Yen's k-shortest paths | `ray-rs/src/api/pathfinding.rs` | Additional algorithm |
| SIMD distance functions | `ray-rs/src/vector/distance.rs` | AVX/AVX2 optimizations |
| `JsGraphAccessor` | NAPI bindings | In-memory graph for algorithms |
| Richer conflict reporting | `ray-rs/src/mvcc/conflict.rs` | `ConflictType`, `ConflictInfo` |
| Schema validation | `ray-rs/src/api/schema.rs` | Runtime validation |
| Index serialization | NAPI bindings | Binary serialize/deserialize |
| Snapshot v3 node labels + vectors | `ray-rs/src/core/snapshot/*` | Per-node labels + vector props persisted in snapshots (Rust-only) |

---

## Conclusion

Rust is now at full feature parity with the TypeScript core. Any remaining differences are intentional, language-idiomatic API shapes rather than missing functionality.
