# Bindings Parity Checklist

Sources
- TypeScript API docs: `docs/api/README.md`
- TypeScript exports: `src/index.ts`
- NAPI surface: `index.d.ts` in the Rust bindings package
- Python bindings: `python/<bindings>/__init__.py`, `src/pyo3_bindings/*.rs`

Legend
- Full: Parity with TS feature and behavior
- Partial: Subset of functionality or different semantics
- Missing: Not exposed in bindings

## Parity Matrix (Single-file core target)

| Feature area | TypeScript | Python | NAPI | Notes |
| --- | --- | --- | --- | --- |
| Single-file DB open/close | Full | Full | Full | `Database`/`open_database`/`openDatabase` |
| Single-file open options | Full | Full | Full | Page size, WAL size, cache, sync mode |
| Locking utility | Full | Missing | Missing | TS-only helper (`isProperLockingAvailable`) |
| Backup/restore | Full | Missing | Full | `createBackup`, `restoreBackup`, `createOfflineBackup` |
| Export/import | Full | Missing | Full | JSON export/import + JSONL export (TS has no JSONL import either) |
| Streaming/pagination | Full | Missing | Full | `streamNodes`, `getNodesPage` |
| Metrics/health | Full | Missing | Full | `collectMetrics`, `healthCheck` (timestamp is epoch ms) |
| Low-level transactions | Full | Partial | Full | Single-file only |
| Low-level node CRUD | Full | Full | Full | `createNode`, `deleteNode`, `getNodeByKey`, `listNodes`, `countNodes` |
| Low-level edge CRUD | Full | Full | Full | `addEdge`, `deleteEdge`, `listEdges`, `countEdges` |
| Properties (node/edge) | Full | Full | Full | `set/get/del` node/edge props |
| Vector PropValue | Full | Missing | Full | Python bindings do not expose PropValue VectorF32 |
| Schema IDs/labels | Full | Full | Full | Labels, edge types, prop keys |
| Cache API | Full | Full | Full | Python/NAPI include extra cache control |
| Integrity check | Full | Missing | Full | Single-file uses full snapshot check |
| Optimize/compact | Full | Partial | Full | Single-file checkpoint + vacuum/options exposed |
| Vector embeddings | Full | Full | Full | `set/get/del/has` node vectors |
| Vector IVF/IVF-PQ + brute force | Full | Full | Full | Index classes + brute force search |
| VectorIndex (high-level) | Full | Missing | Full | Matches core VectorIndex surface |
| Fluent API (Kite) | Full | Partial | Full | CRUD + edge props + batch + traversal + pathfinding |
| Fluent traversal helpers | Full | Full | Partial | NAPI missing callback-based `where_edge`/`where_node` |
| Fluent pathfinding | Full | Full | Full | Kite path builder + K-shortest |
| DB-backed traversal/pathfinding (low-level) | Full | Partial | Full | DB traversal + BFS/Dijkstra/K-shortest |

## Language-Specific Gaps

Python gaps (bindings + fluent layer)
- Missing `check()` integrity API
- Missing high-level `VectorIndex` API
- No export/import, backup/restore, streaming/pagination, metrics/health
- PropValue vector type not supported (vectors handled only via `set_node_vector`)

NAPI gaps
- None (single-file core target)

## Doc mismatches to fix
- None currently tracked.

## Implementation backlog (draft)

Python
- [ ] Expose `check()` integrity verification
- [ ] Add high-level `VectorIndex` API (Python equivalent of TS `VectorIndex`)
- [ ] Expose export/import and backup/restore
- [ ] Expose streaming/pagination helpers
- [ ] Expose metrics/health

NAPI
- [x] Add high-level fluent API surface (Kite + schema + builders) (partial)
- [x] Expose `check()` integrity verification
- [x] Add high-level `VectorIndex` API
- [x] Expose export/import (JSON + JSONL export only)
- [x] Expose backup/restore
- [x] Expose streaming/pagination helpers
- [x] Expose metrics/health
