"""Type stubs for the kitedb._kitedb native module.

python/tests/test_w3_py.py checks these against the compiled module (names,
parameters and literal defaults), so update both together.
"""

from typing import Any, Dict, Iterator, List, Optional, Tuple

from typing_extensions import deprecated

# ============================================================================
# Exceptions
# ============================================================================

class KiteError(RuntimeError):
    """Base class for errors raised by KiteDB (a RuntimeError subclass)."""

class ConflictError(KiteError):
    """A transaction conflicted with a concurrent commit (MVCC); retry it."""

class ReadOnlyError(KiteError):
    """A write was attempted on a read-only database."""

class NotFoundError(KiteError):
    """A node, edge or key does not exist."""

class ClosedError(KiteError):
    """The database handle is closed."""

class TransactionError(KiteError):
    """No transaction is open on this thread, or one already is."""

class DuplicateKeyError(KiteError):
    """A node with this key already exists."""

class LockError(KiteError):
    """The database file lock could not be acquired."""

class CorruptionError(KiteError):
    """On-disk data failed validation (bad magic, checksum, snapshot or WAL)."""

class WalFullError(KiteError):
    """The WAL and its WAL segments are full and no checkpoint can make room
    for this write now: automatic checkpoints are off, or blocking (one runs
    after the failed write); open write transactions hold the segments'
    records; a blocking checkpoint, optimize, vacuum or WAL resize waits for
    this writer's transaction; the database is closing; or the checkpoint
    that answered the write freed nothing. Checkpoint, or end those
    transactions, before writing more."""

class CheckpointError(KiteError):
    """A write needs WAL segment space only a checkpoint frees, and the last
    automatic checkpoint failed (see Database.checkpoint_error). Committed data
    is safe."""

class WritesRefusedError(KiteError):
    """The database handle refuses writes, close included, until the database
    is reopened: an operation panicked mid-way through its writes, so memory
    and disk may disagree. Reads go on; the reopen recovers every
    acknowledged commit from disk."""

class CheckpointDeclinedError(KiteError):
    """background_checkpoint() made no checkpoint, and the message says why:
    a blocking checkpoint, optimize, vacuum or WAL resize waits for the
    checkpoint gate (it checkpoints anyway), or open write transactions hold
    every WAL segment, so none can be covered before they end (segments
    nothing needs any more may have been freed). No commit is affected."""

# ============================================================================
# Core Database Types
# ============================================================================

class OpenOptions:
    """Options for opening a database."""
    read_only: Optional[bool]
    create_if_missing: Optional[bool]
    # MVCC (snapshot-isolated transactions, conflict detection) is on unless
    # this is False; mvcc=False is deprecated and will be removed.
    mvcc: Optional[bool]
    mvcc_gc_interval_ms: Optional[int]
    mvcc_retention_ms: Optional[int]
    mvcc_max_chain_depth: Optional[int]
    page_size: Optional[int]
    wal_size: Optional[int]
    # Checkpoint once the log (WAL segments and WAL) reaches the trigger
    # (checkpoint_log_ratio of the snapshot, at least three eighths of the
    # WAL, at most checkpoint_log_budget).
    auto_checkpoint: Optional[bool]
    # Deprecated: has no effect (checkpoints follow the log).
    checkpoint_threshold: Optional[float]
    background_checkpoint: Optional[bool]
    checkpoint_compression: Optional[CompressionOptions]
    # Deprecated: cache_snapshot and the cache_* options have no effect.
    cache_snapshot: Optional[bool]
    cache_enabled: Optional[bool]
    cache_max_node_props: Optional[int]
    cache_max_edge_props: Optional[int]
    cache_max_traversal_entries: Optional[int]
    cache_max_query_entries: Optional[int]
    cache_query_ttl_ms: Optional[int]
    # sync_mode is constructor-only (not readable back).
    full_fsync: Optional[bool]
    group_commit_enabled: Optional[bool]
    group_commit_window_ms: Optional[int]
    snapshot_parse_mode: Optional[SnapshotParseMode]
    replication_role: Optional[str]
    replication_sidecar_path: Optional[str]
    replication_source_db_path: Optional[str]
    replication_source_sidecar_path: Optional[str]
    replication_segment_max_bytes: Optional[int]
    replication_retention_min_entries: Optional[int]
    replication_retention_min_ms: Optional[int]
    danger_bypass_file_lock_for_multi_node_simulation: Optional[bool]
    # Run automatic checkpoints on the database's own thread (default True).
    checkpoint_thread: Optional[bool]
    # Checkpoint once the uncovered log reaches this fraction of the
    # snapshot's size (default 0.5).
    checkpoint_log_ratio: Optional[float]
    # The most log, in bytes, an automatic checkpoint waits for (default
    # 128 MiB; the in-memory delta takes about ten times the log's size).
    checkpoint_log_budget: Optional[int]
    # Bytes of a WAL segment extent (default: a sixteenth of the segment
    # limit, from two WALs to the larger of 32 MiB and two WALs).
    wal_segment_size: Optional[int]
    # The most bytes of WAL segments before writers wait for a checkpoint.
    wal_segment_limit: Optional[int]

    def __init__(
        self,
        read_only: Optional[bool] = None,
        create_if_missing: Optional[bool] = None,
        mvcc: Optional[bool] = None,
        mvcc_gc_interval_ms: Optional[int] = None,
        mvcc_retention_ms: Optional[int] = None,
        mvcc_max_chain_depth: Optional[int] = None,
        page_size: Optional[int] = None,
        wal_size: Optional[int] = None,
        auto_checkpoint: Optional[bool] = None,
        checkpoint_threshold: Optional[float] = None,
        background_checkpoint: Optional[bool] = None,
        checkpoint_compression: Optional[CompressionOptions] = None,
        # Deprecated: cache_snapshot and the cache_* options have no effect.
        cache_snapshot: Optional[bool] = None,
        cache_enabled: Optional[bool] = None,
        cache_max_node_props: Optional[int] = None,
        cache_max_edge_props: Optional[int] = None,
        cache_max_traversal_entries: Optional[int] = None,
        cache_max_query_entries: Optional[int] = None,
        cache_query_ttl_ms: Optional[int] = None,
        sync_mode: Optional[SyncMode] = None,
        full_fsync: Optional[bool] = None,
        group_commit_enabled: Optional[bool] = None,
        group_commit_window_ms: Optional[int] = None,
        snapshot_parse_mode: Optional[SnapshotParseMode] = None,
        replication_role: Optional[str] = None,
        replication_sidecar_path: Optional[str] = None,
        replication_source_db_path: Optional[str] = None,
        replication_source_sidecar_path: Optional[str] = None,
        replication_segment_max_bytes: Optional[int] = None,
        replication_retention_min_entries: Optional[int] = None,
        replication_retention_min_ms: Optional[int] = None,
        danger_bypass_file_lock_for_multi_node_simulation: Optional[bool] = None,
        checkpoint_thread: Optional[bool] = None,
        checkpoint_log_ratio: Optional[float] = None,
        checkpoint_log_budget: Optional[int] = None,
        wal_segment_size: Optional[int] = None,
        wal_segment_limit: Optional[int] = None,
    ) -> None: ...

class SyncMode:
    """Synchronization mode for WAL writes."""
    @staticmethod
    def full() -> SyncMode: ...
    @staticmethod
    def normal() -> SyncMode: ...
    @staticmethod
    def off() -> SyncMode: ...

class SnapshotParseMode:
    """How snapshot parse errors are handled on open."""
    @staticmethod
    def strict() -> SnapshotParseMode: ...
    @staticmethod
    def salvage() -> SnapshotParseMode: ...

class CompressionOptions:
    """Snapshot compression settings."""
    enabled: Optional[bool]
    compression_type: Optional[str]
    min_size: Optional[int]
    level: Optional[int]
    def __init__(
        self,
        enabled: Optional[bool] = None,
        compression_type: Optional[str] = None,
        min_size: Optional[int] = None,
        level: Optional[int] = None,
    ) -> None: ...

class SingleFileOptimizeOptions:
    """Options for Database.optimize()."""
    compression: Optional[CompressionOptions]
    def __init__(self, compression: Optional[CompressionOptions] = None) -> None: ...

class VacuumOptions:
    """Vacuum options."""
    shrink_wal: Optional[bool]
    min_wal_size: Optional[int]
    def __init__(
        self,
        shrink_wal: Optional[bool] = None,
        min_wal_size: Optional[int] = None,
    ) -> None: ...

class RuntimeProfile:
    """Preset profile for open/close behavior."""
    open_options: OpenOptions
    close_checkpoint_if_wal_usage_at_least: Optional[float]

class DbStats:
    """Database statistics."""
    snapshot_gen: int
    snapshot_nodes: int
    snapshot_edges: int
    snapshot_max_node_id: int
    delta_nodes_created: int
    delta_nodes_deleted: int
    delta_edges_added: int
    delta_edges_deleted: int
    wal_segment: int
    wal_bytes: int
    recommend_compact: bool
    mvcc_stats: Optional[MvccStats]
    def node_count(self) -> int: ...
    def edge_count(self) -> int: ...

class MvccStats:
    """MVCC stats."""
    active_transactions: int
    min_active_ts: int
    versions_pruned: int
    gc_runs: int
    last_gc_time: int
    committed_writes_size: int
    committed_writes_pruned: int

class CheckResult:
    """Database integrity check result."""
    valid: bool
    errors: List[str]
    warnings: List[str]
    def __init__(
        self,
        valid: bool,
        errors: Optional[List[str]] = None,
        warnings: Optional[List[str]] = None,
    ) -> None: ...
    def is_valid(self) -> bool: ...
    def has_warnings(self) -> bool: ...
    def error_count(self) -> int: ...
    def warning_count(self) -> int: ...
    def __bool__(self) -> bool: ...

@deprecated("The cache layer was removed; Database.cache_stats() always returns None.")
class CacheStats:
    """Deprecated: the cache layer was removed."""
    property_cache_hits: int
    property_cache_misses: int
    property_cache_size: int
    traversal_cache_hits: int
    traversal_cache_misses: int
    traversal_cache_size: int
    query_cache_hits: int
    query_cache_misses: int
    query_cache_size: int
    def property_hit_rate(self) -> float: ...
    def traversal_hit_rate(self) -> float: ...
    def query_hit_rate(self) -> float: ...

class ExportOptions:
    """Options for export."""
    include_nodes: Optional[bool]
    include_edges: Optional[bool]
    include_schema: Optional[bool]
    pretty: Optional[bool]
    def __init__(
        self,
        include_nodes: Optional[bool] = None,
        include_edges: Optional[bool] = None,
        include_schema: Optional[bool] = None,
        pretty: Optional[bool] = None,
    ) -> None: ...

class ImportOptions:
    """Options for import."""
    skip_existing: Optional[bool]
    batch_size: Optional[int]
    def __init__(
        self,
        skip_existing: Optional[bool] = None,
        batch_size: Optional[int] = None,
    ) -> None: ...

class ExportResult:
    """Export result."""
    node_count: int
    edge_count: int
    def __init__(self, node_count: int, edge_count: int) -> None: ...

class ImportResult:
    """Import result."""
    node_count: int
    edge_count: int
    skipped: int
    def __init__(self, node_count: int, edge_count: int, skipped: int) -> None: ...

class StreamOptions:
    """Options for streaming node/edge batches."""
    batch_size: Optional[int]
    def __init__(self, batch_size: Optional[int] = None) -> None: ...

class PaginationOptions:
    """Options for cursor-based pagination."""
    limit: Optional[int]
    cursor: Optional[str]
    def __init__(self, limit: Optional[int] = None, cursor: Optional[str] = None) -> None: ...

class NodeWithProps:
    """Node entry with properties."""
    id: int
    key: Optional[str]
    props: List[NodeProp]
    def __init__(self, id: int, key: Optional[str] = None, props: List[NodeProp] = ...) -> None: ...

class EdgeWithProps:
    """Edge entry with properties."""
    src: int
    etype: int
    dst: int
    props: List[NodeProp]
    def __init__(self, src: int, etype: int, dst: int, props: List[NodeProp]) -> None: ...

class NodeBatchIterator(Iterator[List[Any]]):
    """Lazy iterator over node batches, from Database.stream_nodes*.

    Yields lists of node ids (stream_nodes) or NodeWithProps
    (stream_nodes_with_props), in id order. The stream reads nodes with a
    cursor, a batch (or the nodes created since the last checkpoint) at a
    time, so memory stays proportional to a batch: a node created past the
    cursor during the stream is listed, one deleted before it is read is not.
    """
    def __iter__(self) -> NodeBatchIterator: ...
    def __next__(self) -> List[Any]: ...

class EdgeBatchIterator(Iterator[List[Any]]):
    """Lazy iterator over edge batches, from Database.stream_edges*.

    Yields lists of FullEdge (stream_edges) or EdgeWithProps
    (stream_edges_with_props), in (src, etype, dst) order, read with a cursor
    like NodeBatchIterator.
    """
    def __iter__(self) -> EdgeBatchIterator: ...
    def __next__(self) -> List[Any]: ...

class NodePage:
    """Page of node IDs."""
    items: List[int]
    next_cursor: Optional[str]
    has_more: bool
    total: Optional[int]
    def __init__(
        self,
        items: List[int],
        next_cursor: Optional[str] = None,
        has_more: bool = False,
        total: Optional[int] = None,
    ) -> None: ...
    def __len__(self) -> int: ...
    def __iter__(self) -> Iterator[int]: ...

class EdgePage:
    """Page of edges."""
    items: List[FullEdge]
    next_cursor: Optional[str]
    has_more: bool
    total: Optional[int]
    def __init__(
        self,
        items: List[FullEdge],
        next_cursor: Optional[str] = None,
        has_more: bool = False,
        total: Optional[int] = None,
    ) -> None: ...
    def __len__(self) -> int: ...

@deprecated("The cache layer was removed; every field is zero.")
class CacheLayerMetrics:
    """Deprecated: the cache layer was removed; every field is zero."""
    hits: int
    misses: int
    hit_rate: float
    size: int
    max_size: int
    utilization_percent: float

@deprecated("The cache layer was removed; enabled is False and every count is zero.")
class CacheMetrics:
    """Deprecated: the cache layer was removed; enabled is False and every count is zero."""
    enabled: bool
    property_cache: CacheLayerMetrics
    traversal_cache: CacheLayerMetrics
    query_cache: CacheLayerMetrics

class DataMetrics:
    """Data metrics."""
    node_count: int
    edge_count: int
    delta_nodes_created: int
    delta_nodes_deleted: int
    delta_edges_added: int
    delta_edges_deleted: int
    snapshot_generation: int
    max_node_id: int
    schema_labels: int
    schema_etypes: int
    schema_prop_keys: int

class MvccMetrics:
    """MVCC metrics."""
    enabled: bool
    active_transactions: int
    versions_pruned: int
    gc_runs: int
    min_active_timestamp: int
    committed_writes_size: int
    committed_writes_pruned: int

class PrimaryReplicationMetrics:
    """Primary-side replication metrics."""
    epoch: int
    head_log_index: int
    retained_floor: int
    replica_count: int
    stale_epoch_replica_count: int
    max_replica_lag: int
    min_replica_applied_log_index: Optional[int]
    sidecar_path: str
    last_token: Optional[str]
    last_replication_error: Optional[str]
    sidecar_needs_repair: bool
    append_attempts: int
    append_failures: int
    append_successes: int

class ReplicaReplicationMetrics:
    """Replica-side replication metrics."""
    applied_epoch: int
    applied_log_index: int
    needs_reseed: bool
    last_error: Optional[str]

class ReplicationMetrics:
    """Replication metrics."""
    enabled: bool
    role: str
    primary: Optional[PrimaryReplicationMetrics]
    replica: Optional[ReplicaReplicationMetrics]

class MemoryMetrics:
    """Memory metrics."""
    delta_estimate_bytes: int
    cache_estimate_bytes: int  # Deprecated: always 0 (the cache layer was removed).
    snapshot_bytes: int
    total_estimate_bytes: int
    def human_readable(self) -> str: ...

class DatabaseMetrics:
    """Database metrics."""
    path: str
    is_single_file: bool
    read_only: bool
    data: DataMetrics
    cache: CacheMetrics  # Deprecated: a disabled, empty cache (the cache layer was removed).
    mvcc: Optional[MvccMetrics]
    replication: ReplicationMetrics
    memory: MemoryMetrics
    collected_at: int

class HealthCheckEntry:
    """Health check entry."""
    name: str
    passed: bool
    message: str
    def __bool__(self) -> bool: ...

class HealthCheckResult:
    """Health check result."""
    healthy: bool
    checks: List[HealthCheckEntry]
    def passed_count(self) -> int: ...
    def failed_count(self) -> int: ...
    def failed_checks(self) -> List[HealthCheckEntry]: ...
    def __bool__(self) -> bool: ...

class BackupOptions:
    """Options for creating a backup."""
    checkpoint: Optional[bool]
    overwrite: Optional[bool]
    def __init__(self, checkpoint: Optional[bool] = None, overwrite: Optional[bool] = None) -> None: ...

class RestoreOptions:
    """Options for restoring a backup."""
    overwrite: Optional[bool]
    def __init__(self, overwrite: Optional[bool] = None) -> None: ...

class OfflineBackupOptions:
    """Options for offline backup."""
    overwrite: Optional[bool]
    def __init__(self, overwrite: Optional[bool] = None) -> None: ...

class BackupResult:
    """Backup result."""
    path: str
    size: int
    timestamp: int
    type: str

class PropValue:
    """Property value wrapper."""
    prop_type: str
    bool_value: Optional[bool]
    int_value: Optional[int]
    float_value: Optional[float]
    string_value: Optional[str]
    vector_value: Optional[List[float]]
    
    @staticmethod
    def null() -> PropValue: ...
    @staticmethod
    def bool(value: bool) -> PropValue: ...
    @staticmethod
    def int(value: int) -> PropValue: ...
    @staticmethod
    def float(value: float) -> PropValue: ...
    @staticmethod
    def string(value: str) -> PropValue: ...
    @staticmethod
    def vector(value: List[float]) -> PropValue: ...
    def value(self) -> Any: ...

class Edge:
    """Edge representation (neighbor style)."""
    etype: int
    node_id: int
    def __init__(self, etype: int, node_id: int) -> None: ...

class FullEdge:
    """Full edge representation."""
    src: int
    etype: int
    dst: int
    def __init__(self, src: int, etype: int, dst: int) -> None: ...

class NodeProp:
    """Node property key-value pair."""
    key_id: int
    value: PropValue
    def __init__(self, key_id: int, value: PropValue) -> None: ...

# ============================================================================
# Traversal Result Types
# ============================================================================

class TraversalResult:
    """A single result from a traversal."""
    node_id: int
    depth: int
    edge_src: Optional[int]
    edge_dst: Optional[int]
    edge_type: Optional[int]

class PathResult:
    """Result of a pathfinding query."""
    path: List[int]
    edges: List[PathEdge]
    total_weight: float
    found: bool
    
    def __len__(self) -> int: ...
    def __bool__(self) -> bool: ...

class PathEdge:
    """An edge in a path result."""
    src: int
    etype: int
    dst: int

# ============================================================================
# Database Class
# ============================================================================

class Savepoint:
    """A savepoint in a write transaction, from ``Database.savepoint()``."""

class Database:
    """Single-file graph database.

    Failed operations raise KiteError subclasses; invalid arguments raise
    ValueError.
    """

    is_open: bool
    path: str
    read_only: bool

    def __init__(self, path: str, options: Optional[OpenOptions] = None) -> None: ...
    @staticmethod
    def open(path: str, options: Optional[OpenOptions] = None) -> Database: ...
    def close(self) -> None:
        """Close the database. Raises WritesRefusedError, persisting nothing,
        if the handle refuses writes (an operation panicked mid-way through
        its writes); the handle is closed all the same, and a reopen
        recovers every acknowledged commit."""
        ...
    def close_with_checkpoint_if_wal_over(self, threshold: float) -> None: ...
    def __enter__(self) -> Database: ...
    def __exit__(
        self,
        _exc_type: Any = None,
        _exc_value: Any = None,
        _traceback: Any = None,
    ) -> bool: ...

    # Transactions
    def begin(self, read_only: Optional[bool] = None) -> int: ...
    def begin_bulk(self) -> int: ...
    def commit(self) -> None: ...
    def commit_with_token(self) -> Optional[str]: ...
    def wait_for_token(self, token: str, timeout_ms: int) -> bool: ...
    def rollback(self) -> None: ...
    def has_transaction(self) -> bool: ...
    def savepoint(self) -> Savepoint:
        """Take a savepoint in the current write transaction.

        ``rollback_to(savepoint)`` undoes what the transaction did since (its
        writes, the schema names it defined, and its MVCC writes, which then
        cause no conflict) and keeps the savepoint; ``release_savepoint``
        keeps those changes. Savepoints nest: rolling back to or releasing one
        ends every savepoint taken after it.
        """
        ...
    def rollback_to(self, savepoint: Savepoint) -> None: ...
    def release_savepoint(self, savepoint: Savepoint) -> None: ...

    # Replication
    def primary_replication_status(self) -> Optional[Dict[str, Any]]: ...
    def replica_replication_status(self) -> Optional[Dict[str, Any]]: ...
    def primary_promote_to_next_epoch(self) -> int: ...
    def primary_report_replica_progress(
        self, replica_id: str, epoch: int, applied_log_index: int
    ) -> None: ...
    def primary_remove_replica_progress(self, replica_id: str) -> bool:
        """Forget a replica's progress so it no longer holds back retention.

        Returns whether the replica had progress recorded.
        """
        ...
    def primary_run_retention(self) -> Tuple[int, int]: ...
    def export_replication_snapshot_transport(self, include_data: bool = False) -> Dict[str, Any]:
        """A consistent snapshot: format, byte_length, checksum_crc32 (int),
        generated_at_ms, epoch, head_log_index, retained_floor, generation (16 hex
        digits), start_cursor (pull the log from here), and data (the database
        file copy as bytes, up to 1 GiB, or None)."""
        ...
    def export_replication_snapshot_transport_json(self, include_data: bool = False) -> str: ...
    def export_replication_log_transport(
        self,
        cursor: Optional[str] = None,
        max_frames: int = 128,
        max_bytes: int = 1048576,
        include_payload: bool = True,
    ) -> Dict[str, Any]:
        """A log page after `cursor`: epoch, head_log_index, retained_floor,
        generation (16 hex digits; a change means the sidecar was recreated),
        cursor, next_cursor, eof, frame_count, total_bytes, and frames (each with
        epoch, log_index, segment_id, segment_offset, bytes, and payload as bytes
        or None)."""
        ...
    def export_replication_log_transport_json(
        self,
        cursor: Optional[str] = None,
        max_frames: int = 128,
        max_bytes: int = 1048576,
        include_payload: bool = True,
    ) -> str: ...
    def replica_bootstrap_from_snapshot(self) -> None: ...
    def replica_catch_up_once(self, max_frames: int) -> int: ...
    def replica_reseed_from_snapshot(self) -> None: ...

    # Node operations
    def create_node(self, key: Optional[str] = None) -> int: ...
    def delete_node(self, node_id: int) -> None: ...
    def node_exists(self, node_id: int) -> bool: ...
    def get_node_by_key(self, key: str) -> Optional[int]: ...
    def get_node_key(self, node_id: int) -> Optional[str]: ...
    def list_nodes(self) -> List[int]: ...
    def count_nodes(self) -> int: ...
    def list_nodes_with_prefix(self, prefix: str) -> List[int]: ...
    def count_nodes_with_prefix(self, prefix: str) -> int: ...
    def batch_create_nodes(
        self,
        input_nodes: List[Tuple[str, List[Tuple[int, PropValue]]]],
        labels: Optional[List[int]] = None,
    ) -> List[int]: ...
    def create_nodes_batch(self, keys: List[Optional[str]]) -> List[int]: ...
    def upsert_node(self, key: str, props: List[Tuple[int, Optional[PropValue]]]) -> int: ...
    def upsert_node_by_id(self, node_id: int, props: List[Tuple[int, Optional[PropValue]]]) -> int: ...

    # Edge operations
    def add_edge(self, src: int, etype: int, dst: int) -> None: ...
    def add_edges_batch(self, edges: List[Tuple[int, int, int]]) -> None: ...
    def add_edges_with_props_batch(
        self, edges: List[Tuple[int, int, int, List[Tuple[int, PropValue]]]]
    ) -> None: ...
    def add_edge_by_name(self, src: int, etype_name: str, dst: int) -> None: ...
    def delete_edge(self, src: int, etype: int, dst: int) -> None: ...
    def upsert_edge(
        self, src: int, etype: int, dst: int, props: List[Tuple[int, Optional[PropValue]]]
    ) -> bool: ...
    def edge_exists(self, src: int, etype: int, dst: int) -> bool: ...
    def get_out_edges(self, node_id: int) -> List[Edge]: ...
    def get_in_edges(self, node_id: int) -> List[Edge]: ...
    def get_out_degree(self, node_id: int) -> int: ...
    def get_in_degree(self, node_id: int) -> int: ...
    def count_edges(self) -> int: ...
    def count_edges_by_type(self, etype: int) -> int: ...
    def list_edges(self, etype: Optional[int] = None) -> List[FullEdge]: ...

    # Property operations
    def set_node_prop(self, node_id: int, key_id: int, value: PropValue) -> None: ...
    def set_node_prop_by_name(self, node_id: int, key_name: str, value: PropValue) -> None: ...
    def delete_node_prop(self, node_id: int, key_id: int) -> None: ...
    def get_node_prop(self, node_id: int, key_id: int) -> Optional[PropValue]: ...
    def get_node_prop_string(self, node_id: int, key_id: int) -> Optional[str]: ...
    def get_node_prop_int(self, node_id: int, key_id: int) -> Optional[int]: ...
    def get_node_prop_float(self, node_id: int, key_id: int) -> Optional[float]: ...
    def get_node_prop_bool(self, node_id: int, key_id: int) -> Optional[bool]: ...
    def get_node_props(self, node_id: int) -> Optional[List[NodeProp]]: ...

    # Edge property operations
    def set_edge_prop(self, src: int, etype: int, dst: int, key_id: int, value: PropValue) -> None: ...
    def set_edge_prop_by_name(
        self, src: int, etype: int, dst: int, key_name: str, value: PropValue
    ) -> None: ...
    def delete_edge_prop(self, src: int, etype: int, dst: int, key_id: int) -> None: ...
    def get_edge_prop(self, src: int, etype: int, dst: int, key_id: int) -> Optional[PropValue]: ...
    def get_edge_props(self, src: int, etype: int, dst: int) -> Optional[List[NodeProp]]: ...

    # Vector operations
    def set_node_vector(self, node_id: int, prop_key_id: int, vector: List[float]) -> None: ...
    def get_node_vector(self, node_id: int, prop_key_id: int) -> Optional[List[float]]: ...
    def delete_node_vector(self, node_id: int, prop_key_id: int) -> None: ...
    def has_node_vector(self, node_id: int, prop_key_id: int) -> bool: ...

    # Schema operations
    def get_or_create_label(self, name: str) -> int: ...
    def get_label_id(self, name: str) -> Optional[int]: ...
    def get_label_name(self, id: int) -> Optional[str]: ...
    def get_or_create_etype(self, name: str) -> int: ...
    def get_etype_id(self, name: str) -> Optional[int]: ...
    def get_etype_name(self, id: int) -> Optional[str]: ...
    def get_or_create_propkey(self, name: str) -> int: ...
    def get_propkey_id(self, name: str) -> Optional[int]: ...
    def get_propkey_name(self, id: int) -> Optional[str]: ...

    # Label operations
    def define_label(self, name: str) -> int: ...
    def add_node_label(self, node_id: int, label_id: int) -> None: ...
    def add_node_label_by_name(self, node_id: int, label_name: str) -> None: ...
    def remove_node_label(self, node_id: int, label_id: int) -> None: ...
    def node_has_label(self, node_id: int, label_id: int) -> bool: ...
    def get_node_labels(self, node_id: int) -> List[int]: ...

    # Maintenance
    def checkpoint(self) -> None: ...
    def background_checkpoint(self) -> None: ...
    def checkpoint_error(self) -> Optional[str]: ...
    def should_checkpoint(self, threshold: float = 0.5) -> bool: ...
    def optimize(self, options: Optional[SingleFileOptimizeOptions] = None) -> None: ...
    def vacuum(self, shrink_wal: bool = True, min_wal_size: Optional[int] = None) -> None: ...
    def stats(self) -> DbStats: ...
    def check(self) -> CheckResult: ...

    # Export / Import
    def export_to_json(self, path: str, options: Optional[ExportOptions] = None) -> ExportResult: ...
    def export_to_jsonl(self, path: str, options: Optional[ExportOptions] = None) -> ExportResult: ...
    def import_from_json(self, path: str, options: Optional[ImportOptions] = None) -> ImportResult: ...

    # Streaming / Pagination
    def stream_nodes(self, options: Optional[StreamOptions] = None) -> NodeBatchIterator: ...
    def stream_nodes_with_props(self, options: Optional[StreamOptions] = None) -> NodeBatchIterator: ...
    def stream_edges(self, options: Optional[StreamOptions] = None) -> EdgeBatchIterator: ...
    def stream_edges_with_props(self, options: Optional[StreamOptions] = None) -> EdgeBatchIterator: ...
    def get_nodes_page(self, options: Optional[PaginationOptions] = None) -> NodePage: ...
    def get_edges_page(self, options: Optional[PaginationOptions] = None) -> EdgePage: ...

    # Cache operations: deprecated no-ops (the cache layer was removed)
    @deprecated("No effect: the cache layer was removed. Always returns False.")
    def cache_is_enabled(self) -> bool: ...
    @deprecated("No effect: the cache layer was removed.")
    def cache_invalidate_node(self, node_id: int) -> None: ...
    @deprecated("No effect: the cache layer was removed.")
    def cache_invalidate_edge(self, src: int, etype: int, dst: int) -> None: ...
    @deprecated("No effect: the cache layer was removed.")
    def cache_invalidate_key(self, key: str) -> None: ...
    @deprecated("No effect: the cache layer was removed.")
    def cache_clear(self) -> None: ...
    @deprecated("No effect: the cache layer was removed.")
    def cache_clear_query(self) -> None: ...
    @deprecated("No effect: the cache layer was removed.")
    def cache_clear_key(self) -> None: ...
    @deprecated("No effect: the cache layer was removed.")
    def cache_clear_property(self) -> None: ...
    @deprecated("No effect: the cache layer was removed.")
    def cache_clear_traversal(self) -> None: ...
    @deprecated("No effect: the cache layer was removed. Always returns None.")
    def cache_stats(self) -> Optional[CacheStats]: ...
    @deprecated("No effect: the cache layer was removed.")
    def cache_reset_stats(self) -> None: ...

    # Graph traversal (direction: "out", "in" or "both"; others raise ValueError)
    def traverse_out(self, node_id: int, etype: Optional[int] = None) -> List[int]: ...
    def traverse_out_with_keys(
        self, node_id: int, etype: Optional[int] = None
    ) -> List[Tuple[int, Optional[str]]]: ...
    def traverse_out_count(self, node_id: int, etype: Optional[int] = None) -> int: ...
    def traverse_in(self, node_id: int, etype: Optional[int] = None) -> List[int]: ...
    def traverse_in_with_keys(
        self, node_id: int, etype: Optional[int] = None
    ) -> List[Tuple[int, Optional[str]]]: ...
    def traverse_in_count(self, node_id: int, etype: Optional[int] = None) -> int: ...
    def traverse(
        self,
        node_id: int,
        max_depth: int,
        etype: Optional[int] = None,
        min_depth: Optional[int] = None,
        direction: Optional[str] = None,
        unique: Optional[bool] = None,
    ) -> List[TraversalResult]: ...
    def traverse_multi(
        self, start_ids: List[int], steps: List[Tuple[str, Optional[int]]]
    ) -> List[Tuple[int, Optional[str]]]: ...
    def traverse_multi_count(self, start_ids: List[int], steps: List[Tuple[str, Optional[int]]]) -> int: ...

    # Pathfinding
    def find_path_bfs(
        self,
        source: int,
        target: int,
        etype: Optional[int] = None,
        max_depth: Optional[int] = None,
        direction: Optional[str] = None,
    ) -> PathResult: ...
    def find_path_dijkstra(
        self,
        source: int,
        target: int,
        etype: Optional[int] = None,
        max_depth: Optional[int] = None,
        direction: Optional[str] = None,
    ) -> PathResult: ...
    def has_path(
        self,
        source: int,
        target: int,
        etype: Optional[int] = None,
        max_depth: Optional[int] = None,
        direction: Optional[str] = None,
    ) -> bool: ...
    def reachable_nodes(
        self,
        source: int,
        max_depth: int,
        etype: Optional[int] = None,
    ) -> List[int]: ...

def open_database(path: str, options: Optional[OpenOptions] = None) -> Database: ...
def recommended_safe_profile() -> RuntimeProfile: ...
def recommended_balanced_profile() -> RuntimeProfile: ...
def recommended_reopen_heavy_profile() -> RuntimeProfile: ...
def collect_metrics(db: Database) -> DatabaseMetrics: ...
def collect_replication_snapshot_transport(
    db: Database,
    include_data: bool = False,
) -> Dict[str, Any]: ...
def collect_replication_snapshot_transport_json(
    db: Database,
    include_data: bool = False,
) -> str: ...
def collect_replication_log_transport(
    db: Database,
    cursor: Optional[str] = None,
    max_frames: int = 128,
    max_bytes: int = 1048576,
    include_payload: bool = True,
) -> Dict[str, Any]: ...
def collect_replication_log_transport_json(
    db: Database,
    cursor: Optional[str] = None,
    max_frames: int = 128,
    max_bytes: int = 1048576,
    include_payload: bool = True,
) -> str: ...
def collect_replication_metrics_otel_json(db: Database) -> str: ...
def collect_replication_metrics_otel_protobuf(db: Database) -> bytes: ...
def collect_replication_metrics_prometheus(db: Database) -> str: ...
def push_replication_metrics_otel_json(
    db: Database,
    endpoint: str,
    timeout_ms: int = 5000,
    bearer_token: Optional[str] = None,
    retry_max_attempts: int = 1,
    retry_backoff_ms: int = 100,
    retry_backoff_max_ms: int = 2000,
    retry_jitter_ratio: float = 0.0,
    adaptive_retry: bool = False,
    adaptive_retry_mode: Optional[str] = None,
    adaptive_retry_ewma_alpha: float = 0.3,
    circuit_breaker_failure_threshold: int = 0,
    circuit_breaker_open_ms: int = 0,
    circuit_breaker_half_open_probes: int = 1,
    circuit_breaker_state_path: Optional[str] = None,
    circuit_breaker_state_url: Optional[str] = None,
    circuit_breaker_state_patch: bool = False,
    circuit_breaker_state_patch_batch: bool = False,
    circuit_breaker_state_patch_batch_max_keys: int = 8,
    circuit_breaker_state_patch_merge: bool = False,
    circuit_breaker_state_patch_merge_max_keys: int = 32,
    circuit_breaker_state_patch_retry_max_attempts: int = 1,
    circuit_breaker_state_cas: bool = False,
    circuit_breaker_state_lease_id: Optional[str] = None,
    circuit_breaker_scope_key: Optional[str] = None,
    compression_gzip: bool = False,
    https_only: bool = False,
    ca_cert_pem_path: Optional[str] = None,
    client_cert_pem_path: Optional[str] = None,
    client_key_pem_path: Optional[str] = None,
) -> Tuple[int, str]: ...
def push_replication_metrics_otel_grpc(
    db: Database,
    endpoint: str,
    timeout_ms: int = 5000,
    bearer_token: Optional[str] = None,
    retry_max_attempts: int = 1,
    retry_backoff_ms: int = 100,
    retry_backoff_max_ms: int = 2000,
    retry_jitter_ratio: float = 0.0,
    adaptive_retry: bool = False,
    adaptive_retry_mode: Optional[str] = None,
    adaptive_retry_ewma_alpha: float = 0.3,
    circuit_breaker_failure_threshold: int = 0,
    circuit_breaker_open_ms: int = 0,
    circuit_breaker_half_open_probes: int = 1,
    circuit_breaker_state_path: Optional[str] = None,
    circuit_breaker_state_url: Optional[str] = None,
    circuit_breaker_state_patch: bool = False,
    circuit_breaker_state_patch_batch: bool = False,
    circuit_breaker_state_patch_batch_max_keys: int = 8,
    circuit_breaker_state_patch_merge: bool = False,
    circuit_breaker_state_patch_merge_max_keys: int = 32,
    circuit_breaker_state_patch_retry_max_attempts: int = 1,
    circuit_breaker_state_cas: bool = False,
    circuit_breaker_state_lease_id: Optional[str] = None,
    circuit_breaker_scope_key: Optional[str] = None,
    compression_gzip: bool = False,
    https_only: bool = False,
    ca_cert_pem_path: Optional[str] = None,
    client_cert_pem_path: Optional[str] = None,
    client_key_pem_path: Optional[str] = None,
) -> Tuple[int, str]: ...
def push_replication_metrics_otel_protobuf(
    db: Database,
    endpoint: str,
    timeout_ms: int = 5000,
    bearer_token: Optional[str] = None,
    retry_max_attempts: int = 1,
    retry_backoff_ms: int = 100,
    retry_backoff_max_ms: int = 2000,
    retry_jitter_ratio: float = 0.0,
    adaptive_retry: bool = False,
    adaptive_retry_mode: Optional[str] = None,
    adaptive_retry_ewma_alpha: float = 0.3,
    circuit_breaker_failure_threshold: int = 0,
    circuit_breaker_open_ms: int = 0,
    circuit_breaker_half_open_probes: int = 1,
    circuit_breaker_state_path: Optional[str] = None,
    circuit_breaker_state_url: Optional[str] = None,
    circuit_breaker_state_patch: bool = False,
    circuit_breaker_state_patch_batch: bool = False,
    circuit_breaker_state_patch_batch_max_keys: int = 8,
    circuit_breaker_state_patch_merge: bool = False,
    circuit_breaker_state_patch_merge_max_keys: int = 32,
    circuit_breaker_state_patch_retry_max_attempts: int = 1,
    circuit_breaker_state_cas: bool = False,
    circuit_breaker_state_lease_id: Optional[str] = None,
    circuit_breaker_scope_key: Optional[str] = None,
    compression_gzip: bool = False,
    https_only: bool = False,
    ca_cert_pem_path: Optional[str] = None,
    client_cert_pem_path: Optional[str] = None,
    client_key_pem_path: Optional[str] = None,
) -> Tuple[int, str]: ...
def health_check(db: Database) -> HealthCheckResult: ...
def create_backup(db: Database, backup_path: str, options: Optional[BackupOptions] = None) -> BackupResult: ...
def restore_backup(backup_path: str, restore_path: str, options: Optional[RestoreOptions] = None) -> str: ...
def backup_info(backup_path: str) -> BackupResult: ...
def create_offline_backup(
    db_path: str,
    backup_path: str,
    options: Optional[OfflineBackupOptions] = None,
) -> BackupResult: ...
def version() -> str: ...

# ============================================================================
# Vector Search Types
# ============================================================================

class IvfConfig:
    """Configuration for IVF index."""
    n_clusters: Optional[int]
    n_probe: Optional[int]
    metric: Optional[str]
    seed: Optional[int]
    """Training seed, 0 to 2**64 - 1 (default: a fresh seed per training).
    With a seed, training the same vectors in the same order builds the same
    index on any machine."""
    
    def __init__(
        self,
        n_clusters: Optional[int] = None,
        n_probe: Optional[int] = None,
        metric: Optional[str] = None,
        seed: Optional[int] = None,
    ) -> None: ...

class PqConfig:
    """Configuration for Product Quantization."""
    num_subspaces: Optional[int]
    num_centroids: Optional[int]
    max_iterations: Optional[int]
    
    def __init__(
        self,
        num_subspaces: Optional[int] = None,
        num_centroids: Optional[int] = None,
        max_iterations: Optional[int] = None,
    ) -> None: ...

class SearchOptions:
    """Options for vector search."""
    n_probe: Optional[int]
    threshold: Optional[float]
    rerank_factor: Optional[int]
    """IVF-PQ only: re-rank the best max(k * rerank_factor, 80) PQ candidates by
    exact distance (default 4; 0 returns the approximate PQ ranking and
    distances). IVF search is exact and ignores it."""
    
    def __init__(
        self,
        n_probe: Optional[int] = None,
        threshold: Optional[float] = None,
        rerank_factor: Optional[int] = None,
    ) -> None: ...

class SearchResult:
    """Result of a vector search."""
    vector_id: int
    node_id: int
    distance: float
    similarity: float

class IvfStats:
    """Statistics for IVF index."""
    trained: bool
    n_clusters: int
    total_vectors: int
    avg_vectors_per_cluster: float
    empty_cluster_count: int
    min_cluster_size: int
    max_cluster_size: int

class IvfIndex:
    """IVF (Inverted File) index for approximate nearest neighbor search."""
    
    dimensions: int
    trained: bool
    
    def __init__(self, dimensions: int, config: Optional[IvfConfig] = None) -> None: ...
    def add_training_vectors(self, vectors: List[float], num_vectors: int) -> None: ...
    def train(self) -> None: ...
    def insert(self, vector_id: int, vector: List[float]) -> None: ...
    def delete(self, vector_id: int, vector: List[float]) -> bool: ...
    def clear(self) -> None: ...
    def search(
        self,
        manifest_json: str,
        query: List[float],
        k: int,
        options: Optional[SearchOptions] = None,
    ) -> List[SearchResult]: ...
    def search_multi(
        self,
        manifest_json: str,
        queries: List[List[float]],
        k: int,
        aggregation: str,
        options: Optional[SearchOptions] = None,
    ) -> List[SearchResult]: ...
    def stats(self) -> IvfStats: ...
    def serialize(self) -> bytes: ...
    @staticmethod
    def deserialize(data: bytes) -> IvfIndex: ...

class IvfPqIndex:
    """IVF-PQ combined index for memory-efficient approximate nearest neighbor search."""
    
    dimensions: int
    trained: bool
    
    def __init__(
        self,
        dimensions: int,
        ivf_config: Optional[IvfConfig] = None,
        pq_config: Optional[PqConfig] = None,
        use_residuals: Optional[bool] = None,
    ) -> None: ...
    def add_training_vectors(self, vectors: List[float], num_vectors: int) -> None: ...
    def train(self) -> None: ...
    def insert(self, vector_id: int, vector: List[float]) -> None: ...
    def delete(self, vector_id: int, vector: List[float]) -> bool: ...
    def clear(self) -> None: ...
    def search(
        self,
        manifest_json: str,
        query: List[float],
        k: int,
        options: Optional[SearchOptions] = None,
    ) -> List[SearchResult]: ...
    def search_multi(
        self,
        manifest_json: str,
        queries: List[List[float]],
        k: int,
        aggregation: str,
        options: Optional[SearchOptions] = None,
    ) -> List[SearchResult]: ...
    def stats(self) -> IvfStats: ...
    def serialize(self) -> bytes: ...
    @staticmethod
    def deserialize(data: bytes) -> IvfPqIndex: ...

class BruteForceResult:
    """Brute force search result."""
    node_id: int
    distance: float
    similarity: float

def resolve_ann_algorithm(algorithm: str, dimensions: int, live_vectors: int) -> str:
    """The backend ``algorithm`` ("auto", "ivf" or "ivf_pq") builds for a
    vector index of ``dimensions`` with ``live_vectors`` live vectors: "ivf" or
    "ivf_pq". "auto" picks IVF-PQ from 512 dimensions and 50,000 vectors on,
    plain IVF otherwise."""
    ...

def brute_force_search(
    vectors: List[List[float]],
    node_ids: List[int],
    query: List[float],
    k: int,
    metric: Optional[str] = None,
) -> List[BruteForceResult]: ...
