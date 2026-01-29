# Python Bindings Refactoring Plan

## Overview

Breaking down `database.rs` (5,074 lines) into focused, maintainable modules.

## Design Decisions

1. **Clean naming**: Drop `Py` prefix from types (e.g., `PropValue` instead of `PyPropValue`)
2. **Feature flags**: Keep `#[cfg(feature = "python")]` for conditional compilation
3. **Testing**: Add comprehensive Python binding tests

## Target File Structure

```
src/pyo3_bindings/
├── mod.rs                 - Module orchestration + pymodule definition
├── database.rs            - Core Database struct, DatabaseInner, open/close
├── types/
│   ├── mod.rs             - Re-exports all types
│   ├── values.rs          - PropValue, PropType
│   ├── edges.rs           - Edge, FullEdge
│   ├── nodes.rs           - NodeProp, NodeWithProps, EdgeWithProps
│   └── results.rs         - NodePage, EdgePage
├── options/
│   ├── mod.rs             - Re-exports all options
│   ├── open.rs            - SyncMode, OpenOptions, conversions
│   ├── maintenance.rs     - CompressionOptions, SingleFileOptimizeOptions, VacuumOptions
│   ├── streaming.rs       - StreamOptions, PaginationOptions
│   ├── backup.rs          - BackupOptions, RestoreOptions, OfflineBackupOptions, BackupResult
│   └── export.rs          - ExportOptions, ImportOptions, ExportResult, ImportResult
├── stats/
│   ├── mod.rs             - Re-exports
│   ├── database.rs        - DbStats, CheckResult, CacheStats
│   └── metrics.rs         - CacheLayerMetrics, CacheMetrics, DataMetrics, etc.
├── ops/                   - Operation trait extensions for Database
│   ├── mod.rs             - Re-exports traits
│   ├── transaction.rs     - Transaction operations (begin, commit, rollback)
│   ├── nodes.rs           - Node CRUD operations
│   ├── edges.rs           - Edge CRUD operations
│   ├── properties.rs      - Property get/set operations
│   ├── vectors.rs         - Vector embedding operations
│   ├── schema.rs          - Schema operations (labels, etypes, propkeys)
│   ├── labels.rs          - Node label operations
│   ├── cache.rs           - Cache operations
│   ├── maintenance.rs     - checkpoint, optimize, vacuum, stats, check
│   ├── export_import.rs   - Export/import operations
│   └── traversal.rs       - traverse_*, pathfinding methods
├── functions.rs           - Standalone functions (open_database, collect_metrics, etc.)
├── helpers.rs             - Internal helper functions (get_neighbors_*, graph_stats, etc.)
├── traversal.rs           - (existing) TraversalResult, PathResult, PathEdge
└── vector.rs              - (existing) Vector index types
```

## Migration Steps

### Phase 1: Extract Types (Steps 1-4)

- [ ] Step 1: Create directory structure
- [ ] Step 2: Extract `types/values.rs` - PropValue, PropType (~150 lines)
- [ ] Step 3: Extract `types/edges.rs` - Edge, FullEdge (~50 lines)
- [ ] Step 4: Extract `types/nodes.rs` - NodeProp, NodeWithProps, EdgeWithProps (~80 lines)
- [ ] Step 5: Extract `types/results.rs` - NodePage, EdgePage (~60 lines)

### Phase 2: Extract Options (Steps 6-10)

- [ ] Step 6: Extract `options/open.rs` - SyncMode, OpenOptions (~300 lines)
- [ ] Step 7: Extract `options/maintenance.rs` - CompressionOptions, etc. (~120 lines)
- [ ] Step 8: Extract `options/streaming.rs` - StreamOptions, PaginationOptions (~80 lines)
- [ ] Step 9: Extract `options/backup.rs` - Backup/restore options (~150 lines)
- [ ] Step 10: Extract `options/export.rs` - Export/import options (~100 lines)

### Phase 3: Extract Stats & Metrics (Steps 11-12)

- [ ] Step 11: Extract `stats/database.rs` - DbStats, CheckResult, CacheStats (~150 lines)
- [ ] Step 12: Extract `stats/metrics.rs` - All metrics types (~300 lines)

### Phase 4: Core Database & Operations (Steps 13-24)

- [ ] Step 13: Create base `database.rs` - DatabaseInner, Database struct (~200 lines)
- [ ] Step 14: Extract `ops/transaction.rs` (~150 lines)
- [ ] Step 15: Extract `ops/nodes.rs` (~350 lines)
- [ ] Step 16: Extract `ops/edges.rs` (~350 lines)
- [ ] Step 17: Extract `ops/properties.rs` (~400 lines)
- [ ] Step 18: Extract `ops/vectors.rs` (~100 lines)
- [ ] Step 19: Extract `ops/schema.rs` (~200 lines)
- [ ] Step 20: Extract `ops/labels.rs` (~150 lines)
- [ ] Step 21: Extract `ops/cache.rs` (~200 lines)
- [ ] Step 22: Extract `ops/maintenance.rs` (~200 lines)
- [ ] Step 23: Extract `ops/export_import.rs` (~200 lines)
- [ ] Step 24: Extract `ops/traversal.rs` (~600 lines)

### Phase 5: Finalize (Steps 25-27)

- [ ] Step 25: Extract `functions.rs` - Standalone functions (~200 lines)
- [ ] Step 26: Extract `helpers.rs` - Internal helpers (~200 lines)
- [ ] Step 27: Update `mod.rs` - Wire everything together (~150 lines)

### Phase 6: Testing (Steps 28-30)

- [ ] Step 28: Add unit tests for type conversions
- [ ] Step 29: Add integration tests for database operations
- [ ] Step 30: Verify existing Python tests pass

## Trait Extension Pattern

Each operation module defines a trait that Database implements:

```rust
// ops/nodes.rs
use crate::pyo3_bindings::database::Database;

pub trait NodeOps {
    fn create_node_impl(&self, key: Option<String>) -> PyResult<i64>;
    fn delete_node_impl(&self, node_id: i64) -> PyResult<()>;
    // ...
}

impl NodeOps for Database {
    fn create_node_impl(&self, key: Option<String>) -> PyResult<i64> {
        let guard = self.inner.lock()...
        // implementation
    }
}
```

In `database.rs`, `#[pymethods]` delegates to traits:

```rust
#[pymethods]
impl Database {
    #[pyo3(signature = (key=None))]
    fn create_node(&self, key: Option<String>) -> PyResult<i64> {
        NodeOps::create_node_impl(self, key)
    }
}
```

## Naming Convention Changes

| Old Name | New Name |
|----------|----------|
| `PyDatabase` | `Database` |
| `PyOpenOptions` | `OpenOptions` |
| `PySyncMode` | `SyncMode` |
| `PyPropValue` | `PropValue` |
| `PyEdge` | `Edge` |
| `PyFullEdge` | `FullEdge` |
| `PyNodeProp` | `NodeProp` |
| `PyDbStats` | `DbStats` |
| etc. | etc. |

## Testing Strategy

### Unit Tests (Rust)
- Type conversion tests (PropValue <-> core::PropValue)
- Option conversion tests

### Integration Tests (Python)
```python
# tests/test_bindings.py
def test_database_open_close():
    db = Database("test.raydb")
    assert db.is_open
    db.close()
    assert not db.is_open

def test_node_crud():
    with Database("test.raydb") as db:
        db.begin()
        node_id = db.create_node("test:1")
        assert db.node_exists(node_id)
        db.delete_node(node_id)
        assert not db.node_exists(node_id)
        db.commit()
```

## Progress Tracking

- Start Date: ___
- Phase 1 Complete: ___
- Phase 2 Complete: ___
- Phase 3 Complete: ___
- Phase 4 Complete: ___
- Phase 5 Complete: ___
- Phase 6 Complete: ___
- All Tests Passing: ___
