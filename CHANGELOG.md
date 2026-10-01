# Changelog

All notable changes to this project will be documented in this file.

## Unreleased

### Changed
- **On-disk format**: the database file now stores two checksummed header pages (pages 0-1) with the WAL starting at page 2, so a torn header write can no longer make a database unopenable. Existing single-header files are migrated automatically and crash-safely (temp file + atomic rename) on their first writable open; read-only opens of legacy files work without migration. Releases up to v0.2.18 read only the first header page, so do not open a converted file with them.
- Checkpoints are copy-on-write: the new snapshot is written to a fresh region and fsynced before the header flips, so a crash mid-checkpoint can no longer destroy the only valid snapshot. Retired snapshot regions are reused when the next snapshot fits, keeping file growth bounded; vacuum still performs full compaction.
- Writable opens take an exclusive file lock (shared for read-only), preventing two processes from corrupting the same database. Replica bootstrap reads a live primary without taking its lock.
- `read_only` opens are now truly read-only: no write permission is requested, recovery that would need to write returns a clear error, and close does not touch the file.
- IVF-PQ approximate search now computes distances in each metric's native space (L2, cosine with reconstructed-norm correction, dot product), matching the exact search path's ranking and scores. The unused precomputed centroid-distance table is no longer written (a format flag keeps older payloads readable).

### Fixed
- Blocking checkpoints and compaction now persist the WAL region heads and active region in the header. Previously a database reopened after a checkpoint could resume appending at a stale WAL offset.
- WAL recovery replays committed transactions in commit order instead of HashMap iteration order, which could previously produce nondeterministic post-crash state.
- Dynamically created labels, edge types, and property keys are WAL-logged, so their name-to-id mappings survive reopen without a checkpoint and ids can no longer be reattached to different names.
- Schema definitions made inside a transaction that rolls back no longer leak into the in-memory maps; concurrent transactions defining the same name share one reserved id.
- A blocking checkpoint can no longer erase a transaction that committed concurrently, and background checkpoints keep post-cut commits visible instead of hiding them until restart.
- A commit whose local WAL write succeeded no longer reports failure when the replication sidecar append fails; the sidecar is fenced (`sidecar_needs_repair`, `last_replication_error`) and requires repair/reseed rather than silently appending over a gap.
- MVCC version-chain and conflict keys use full-width typed keys; ids that collided under the old 20/12-bit packing (e.g. nodes 2^20 apart) no longer share version history.
- Corrupted or crafted database files and vector-index payloads now fail with clean errors instead of panics or unbounded allocations (checked bounds, monotonic offset validation, allocation caps, declared-size-exact decompression).
- Cosine IVF-PQ training normalizes vectors consistently with insert/search; `search_multi` honors `n_probe` and filters during candidate collection; a failed `train()` no longer permanently wedges the index; vectors without a node mapping are skipped instead of surfacing as node id 0.
- Numeric options passed through the Node and Python bindings are range-validated with clear errors instead of silently wrapping (e.g. a negative cache size becoming a huge capacity, `cacheSize: 0` panicking).
- The TypeScript `transaction()`/`batch()` helpers preserve the original commit error instead of masking it with `No active transaction` from the cleanup rollback.
- Fix ray schema ID reuse and add persistence integration tests (`5d73b0c`).
