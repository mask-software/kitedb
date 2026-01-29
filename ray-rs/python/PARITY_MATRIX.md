# Python Parity Matrix (TypeScript Core)

Scope: TypeScript core API under `src/api/*` plus `src/api/vector-search.ts` and `Ray.check()` in `src/api/ray.ts`. Excludes streaming, backup/restore, export/import, metrics, and CLI helpers from `src/index.ts`.

Legend: parity = full feature match, partial = similar capability with API or behavior differences, missing = not exposed in Python bindings.

## Fluent API

| Area | TypeScript (core) | Python | Parity | Notes |
| --- | --- | --- | --- | --- |
| Open/close | `ray(path, options)` / `Ray.open()` / `Ray.close()` | `ray(path, nodes, edges)` / `Ray(...)` / `close()` | parity | Python is sync; TS is async. |
| Schema | `defineNode`, `defineEdge`, `prop`, `optional` | `define_node`, `define_edge`, `prop`, `optional` | parity | Naming differences only. |
| Insert (single) | `insert(node).values({...}).returning()` | `insert(node).values(...).returning()` | parity | Python accepts dict or kwargs. |
| Insert (batch) | `insert(node).values([...]).returning()` | `insert(node).values([...]).returning()` | parity | Python list insert uses batch path. |
| Update (where) | `update(node).set(...).where({$id/$key})` | `update(node).set(...).where(id=..., key=...)` | parity | API shape differs. |
| Update by ref | `update(nodeRef).set(...).execute()` | `update(node_ref).set(...).execute()` | parity | Naming differences only. |
| Delete (where) | `delete(node).where({$id/$key})` | `delete(node).where(id=..., key=...)` | parity | API shape differs. |
| Delete by ref | `delete(nodeRef)` | `delete(node_ref)` | parity | Both execute immediately. |
| Get node | `get(node, key)` | `get(node, key)` | parity | Python returns props; TS returns props. |
| Get ref | `getRef(node, key)` | `get_ref(node, key)` | parity | Both skip prop loading. |
| Edge link/unlink | `link`, `unlink` | `link`, `unlink` | parity | Python accepts dict + kwargs for props. |
| Has edge | `hasEdge` | `has_edge` | parity | Naming difference only. |
| Update edge props | `updateEdge(...).set(...)` | `update_edge(...).set(...)` | parity | Naming difference only. |
| List nodes | `all(nodeDef)` async generator | `all(node_def)` iterator | partial | Sync vs async iteration. |
| Count nodes/edges | `count`, `countEdges` | `count`, `count_edges` | parity | Naming difference only. |
| List edges | `allEdges(edgeDef?)` async generator | `all_edges(edge_def?)` iterator | partial | Sync vs async iteration. |

## Traversal

| Area | TypeScript (core) | Python | Parity | Notes |
| --- | --- | --- | --- | --- |
| Steps | `out`, `in`, `both`, `traverse` | `out`, `in_`, `both`, `traverse` | parity | Python allows edge to be optional. |
| Select props | `select([...])` | `select([...])` | parity | Python also has `load_props` / `with_props`. |
| Filters | `whereEdge`, `whereNode` | `where_edge`, `where_node` | parity | Applied after traversal in both. |
| Traverse options filters | `TraverseOptions.whereEdge/whereNode` | `TraverseOptions.where_edge/where_node` | parity | Filters applied during traversal. |
| Property loading default | loads all props | loads all props | parity | Default behavior aligned. |
| Results | async iterator + `toArray/first/count` | sync iterator + `to_list/first/count` | partial | Behavior equivalent, sync vs async. |
| Edge results | `{ $src, $dst, $etype, ...props }` | `EdgeResult` with `$src/$dst/$etype` access | parity | Python supports `$` keys via `[]`/`to_dict()`. |
| Raw edges | `rawEdges()` | `raw_edges()` | parity | Same constraints (no variable-depth). |

## Pathfinding

| Area | TypeScript (core) | Python | Parity | Notes |
| --- | --- | --- | --- | --- |
| Builder | `shortestPath(source)` | `shortest_path(source)` | parity | Naming difference only. |
| Edge restriction | `via(edge)` required | `via(edge)` required | parity | Behavior aligned. |
| Targets | `to`, `toAny` | `to`, `to_any` | parity | Naming difference only. |
| Algorithms | `dijkstra`, `aStar`, `allPaths` | `dijkstra`, `a_star`, `all_paths` | parity | Naming differences only. |
| Weights | `{ property }` or `{ fn }` | `weight("prop")` or `weight(fn)` | partial | Different API shape; same behavior. |
| Result shape | `{ path, edges, totalWeight, found }` | `{ path, edges, totalWeight, found }` | parity | Aliases provided. |
| Prop loading | always loads props | always loads props | parity | Default behavior aligned. |

## Transactions and Maintenance

| Area | TypeScript (core) | Python | Parity | Notes |
| --- | --- | --- | --- | --- |
| Transaction | `transaction(fn)` with context | `transaction()` context manager | partial | Different API style, similar behavior. |
| Batch ops | `batch([...])` with `_toBatchOp()` | `batch([...])` callables/executors | partial | Same capability, different interface. |
| Stats | `stats()` | `stats()` | parity | Returns stats struct. |
| Check | `check()` | `check()` | parity | Schema warnings included. |
| Optimize | `optimize()` | `optimize()` | parity | Same behavior. |

## Vector Search

| Area | TypeScript (core) | Python | Parity | Notes |
| --- | --- | --- | --- | --- |
| High-level index | `VectorIndex`, `createVectorIndex` | `VectorIndex`, `create_vector_index` | partial | Python uses in-memory store with optional IVF; no persistence. |
| IVF index | `createIvfIndex`, `ivfSearch` | `IvfIndex` | partial | Similar capability, different API surface. |
| IVF-PQ index | `ivf*` + PQ config | `IvfPqIndex` | partial | Similar capability, different API surface. |

## Remaining Gaps

- None.
