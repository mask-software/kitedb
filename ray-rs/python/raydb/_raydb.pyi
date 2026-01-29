"""Type stubs for raydb._raydb native module."""

from typing import Optional, List, Any, Tuple

# ============================================================================
# Core Database Types
# ============================================================================

class OpenOptions:
    """Options for opening a database."""
    read_only: Optional[bool]
    create_if_missing: Optional[bool]
    lock_file: Optional[bool]
    require_locking: Optional[bool]
    mvcc: Optional[bool]
    mvcc_gc_interval_ms: Optional[int]
    mvcc_retention_ms: Optional[int]
    mvcc_max_chain_depth: Optional[int]
    page_size: Optional[int]
    wal_size: Optional[int]
    auto_checkpoint: Optional[bool]
    checkpoint_threshold: Optional[float]
    background_checkpoint: Optional[bool]
    cache_snapshot: Optional[bool]
    cache_enabled: Optional[bool]
    cache_max_node_props: Optional[int]
    cache_max_edge_props: Optional[int]
    cache_max_traversal_entries: Optional[int]
    cache_max_query_entries: Optional[int]
    cache_query_ttl_ms: Optional[int]
    sync_mode: Optional["SyncMode"]
    
    def __init__(
        self,
        read_only: Optional[bool] = None,
        create_if_missing: Optional[bool] = None,
        lock_file: Optional[bool] = None,
        require_locking: Optional[bool] = None,
        mvcc: Optional[bool] = None,
        mvcc_gc_interval_ms: Optional[int] = None,
        mvcc_retention_ms: Optional[int] = None,
        mvcc_max_chain_depth: Optional[int] = None,
        page_size: Optional[int] = None,
        wal_size: Optional[int] = None,
        auto_checkpoint: Optional[bool] = None,
        checkpoint_threshold: Optional[float] = None,
        background_checkpoint: Optional[bool] = None,
        cache_snapshot: Optional[bool] = None,
        cache_enabled: Optional[bool] = None,
        cache_max_node_props: Optional[int] = None,
        cache_max_edge_props: Optional[int] = None,
        cache_max_traversal_entries: Optional[int] = None,
        cache_max_query_entries: Optional[int] = None,
        cache_query_ttl_ms: Optional[int] = None,
        sync_mode: Optional["SyncMode"] = None,
    ) -> None: ...

class SyncMode:
    """Synchronization mode for WAL writes."""
    @staticmethod
    def full() -> SyncMode: ...
    @staticmethod
    def normal() -> SyncMode: ...
    @staticmethod
    def off() -> SyncMode: ...

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

class CacheStats:
    """Cache statistics."""
    property_cache_hits: int
    property_cache_misses: int
    property_cache_size: int
    traversal_cache_hits: int
    traversal_cache_misses: int
    traversal_cache_size: int
    query_cache_hits: int
    query_cache_misses: int
    query_cache_size: int

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

class ImportResult:
    """Import result."""
    node_count: int
    edge_count: int
    skipped: int

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

class EdgeWithProps:
    """Edge entry with properties."""
    src: int
    etype: int
    dst: int
    props: List[NodeProp]

class NodePage:
    """Page of node IDs."""
    items: List[int]
    next_cursor: Optional[str]
    has_more: bool
    total: Optional[int]

class EdgePage:
    """Page of edges."""
    items: List[FullEdge]
    next_cursor: Optional[str]
    has_more: bool
    total: Optional[int]

class CacheLayerMetrics:
    """Cache layer metrics."""
    hits: int
    misses: int
    hit_rate: float
    size: int
    max_size: int
    utilization_percent: float

class CacheMetrics:
    """Cache metrics."""
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

class MemoryMetrics:
    """Memory metrics."""
    delta_estimate_bytes: int
    cache_estimate_bytes: int
    snapshot_bytes: int
    total_estimate_bytes: int

class DatabaseMetrics:
    """Database metrics."""
    path: str
    is_single_file: bool
    read_only: bool
    data: DataMetrics
    cache: CacheMetrics
    mvcc: Optional[MvccMetrics]
    memory: MemoryMetrics
    collected_at: int

class HealthCheckEntry:
    """Health check entry."""
    name: str
    passed: bool
    message: str

class HealthCheckResult:
    """Health check result."""
    healthy: bool
    checks: List[HealthCheckEntry]

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

class FullEdge:
    """Full edge representation."""
    src: int
    etype: int
    dst: int

class NodeProp:
    """Node property key-value pair."""
    key_id: int
    value: PropValue

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

class Database:
    """Single-file graph database."""
    
    is_open: bool
    path: str
    read_only: bool
    
    def __init__(self, path: str, options: Optional[OpenOptions] = None) -> None: ...
    def close(self) -> None: ...
    def __enter__(self) -> Database: ...
    def __exit__(self, exc_type: Any, exc_value: Any, traceback: Any) -> bool: ...
    
    # Transaction methods
    def begin(self, read_only: Optional[bool] = None) -> int: ...
    def commit(self) -> None: ...
    def rollback(self) -> None: ...
    def has_transaction(self) -> bool: ...
    
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
    def batch_create_nodes(self, nodes: List[Tuple[str, List[Tuple[int, PropValue]]]]) -> List[int]: ...
    
    # Edge operations
    def add_edge(self, src: int, etype: int, dst: int) -> None: ...
    def add_edge_by_name(self, src: int, etype_name: str, dst: int) -> None: ...
    def delete_edge(self, src: int, etype: int, dst: int) -> None: ...
    def edge_exists(self, src: int, etype: int, dst: int) -> bool: ...
    def get_out_edges(self, node_id: int) -> List[Edge]: ...
    def get_in_edges(self, node_id: int) -> List[Edge]: ...
    def get_out_degree(self, node_id: int) -> int: ...
    def get_in_degree(self, node_id: int) -> int: ...
    def count_edges(self) -> int: ...
    def list_edges(self, etype: Optional[int] = None) -> List[FullEdge]: ...
    def list_edges_by_name(self, etype_name: str) -> List[FullEdge]: ...
    def count_edges_by_type(self, etype: int) -> int: ...
    def count_edges_by_name(self, etype_name: str) -> int: ...
    
    # Property operations
    def set_node_prop(self, node_id: int, key_id: int, value: PropValue) -> None: ...
    def set_node_prop_by_name(self, node_id: int, key_name: str, value: PropValue) -> None: ...
    def delete_node_prop(self, node_id: int, key_id: int) -> None: ...
    def get_node_prop(self, node_id: int, key_id: int) -> Optional[PropValue]: ...
    def get_node_prop_string(self, node_id: int, key_id: int) -> Optional[str]: ...
    def get_node_prop_int(self, node_id: int, key_id: int) -> Optional[int]: ...
    def get_node_prop_float(self, node_id: int, key_id: int) -> Optional[float]: ...
    def get_node_prop_bool(self, node_id: int, key_id: int) -> Optional[bool]: ...
    def set_node_prop_string(self, node_id: int, key_id: int, value: str) -> None: ...
    def set_node_prop_int(self, node_id: int, key_id: int, value: int) -> None: ...
    def set_node_prop_float(self, node_id: int, key_id: int, value: float) -> None: ...
    def set_node_prop_bool(self, node_id: int, key_id: int, value: bool) -> None: ...
    def get_node_props(self, node_id: int) -> Optional[List[NodeProp]]: ...
    
    # Edge property operations
    def set_edge_prop(self, src: int, etype: int, dst: int, key_id: int, value: PropValue) -> None: ...
    def set_edge_prop_by_name(self, src: int, etype: int, dst: int, key_name: str, value: PropValue) -> None: ...
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
    def should_checkpoint(self, threshold: Optional[float] = None) -> bool: ...
    def optimize(self) -> None: ...
    def stats(self) -> DbStats: ...
    def check(self) -> CheckResult: ...

    # Export / Import
    def export_to_object(self, options: Optional[ExportOptions] = None) -> Any: ...
    def export_to_json(self, path: str, options: Optional[ExportOptions] = None) -> ExportResult: ...
    def export_to_jsonl(self, path: str, options: Optional[ExportOptions] = None) -> ExportResult: ...
    def import_from_object(self, data: Any, options: Optional[ImportOptions] = None) -> ImportResult: ...
    def import_from_json(self, path: str, options: Optional[ImportOptions] = None) -> ImportResult: ...

    # Streaming / Pagination
    def stream_nodes(self, options: Optional[StreamOptions] = None) -> List[List[int]]: ...
    def stream_nodes_with_props(self, options: Optional[StreamOptions] = None) -> List[List[NodeWithProps]]: ...
    def stream_edges(self, options: Optional[StreamOptions] = None) -> List[List[FullEdge]]: ...
    def stream_edges_with_props(self, options: Optional[StreamOptions] = None) -> List[List[EdgeWithProps]]: ...
    def get_nodes_page(self, options: Optional[PaginationOptions] = None) -> NodePage: ...
    def get_edges_page(self, options: Optional[PaginationOptions] = None) -> EdgePage: ...
    
    # Cache operations
    def cache_is_enabled(self) -> bool: ...
    def cache_invalidate_node(self, node_id: int) -> None: ...
    def cache_invalidate_edge(self, src: int, etype: int, dst: int) -> None: ...
    def cache_invalidate_key(self, key: str) -> None: ...
    def cache_clear(self) -> None: ...
    def cache_clear_query(self) -> None: ...
    def cache_clear_key(self) -> None: ...
    def cache_clear_property(self) -> None: ...
    def cache_clear_traversal(self) -> None: ...
    def cache_stats(self) -> Optional[CacheStats]: ...
    def cache_reset_stats(self) -> None: ...
    
    # Graph Traversal
    def traverse_out(self, node_id: int, etype: Optional[int] = None) -> List[int]: ...
    def traverse_out_with_keys(self, node_id: int, etype: Optional[int] = None) -> List[Tuple[int, Optional[str]]]: ...
    def traverse_out_count(self, node_id: int, etype: Optional[int] = None) -> int: ...
    def traverse_in(self, node_id: int, etype: Optional[int] = None) -> List[int]: ...
    def traverse_in_with_keys(self, node_id: int, etype: Optional[int] = None) -> List[Tuple[int, Optional[str]]]: ...
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
    def traverse_multi(self, start_ids: List[int], steps: List[Tuple[str, Optional[int]]]) -> List[Tuple[int, Optional[str]]]: ...
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
    ) -> bool: ...
    def reachable_nodes(
        self,
        source: int,
        max_depth: int,
        etype: Optional[int] = None,
    ) -> List[int]: ...

def open_database(path: str, options: Optional[OpenOptions] = None) -> Database: ...
def collect_metrics(db: Database) -> DatabaseMetrics: ...
def health_check(db: Database) -> HealthCheckResult: ...
def create_backup(db: Database, backup_path: str, options: Optional[BackupOptions] = None) -> BackupResult: ...
def restore_backup(backup_path: str, restore_path: str, options: Optional[RestoreOptions] = None) -> str: ...
def get_backup_info(backup_path: str) -> BackupResult: ...
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
    
    def __init__(
        self,
        n_clusters: Optional[int] = None,
        n_probe: Optional[int] = None,
        metric: Optional[str] = None,
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
    
    def __init__(
        self,
        n_probe: Optional[int] = None,
        threshold: Optional[float] = None,
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

def brute_force_search(
    vectors: List[List[float]],
    node_ids: List[int],
    query: List[float],
    k: int,
    metric: Optional[str] = None,
) -> List[BruteForceResult]: ...
