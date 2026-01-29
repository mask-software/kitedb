"""Type stubs for raydb._raydb native module."""

from typing import Optional, List, Any

# ============================================================================
# Core Database Types
# ============================================================================

class OpenOptions:
    """Options for opening a database."""
    read_only: Optional[bool]
    create_if_missing: Optional[bool]
    page_size: Optional[int]
    wal_size: Optional[int]
    auto_checkpoint: Optional[bool]
    checkpoint_threshold: Optional[float]
    background_checkpoint: Optional[bool]
    cache_enabled: Optional[bool]
    cache_max_node_props: Optional[int]
    cache_max_edge_props: Optional[int]
    cache_max_traversal_entries: Optional[int]
    cache_max_query_entries: Optional[int]
    cache_query_ttl_ms: Optional[int]
    
    def __init__(
        self,
        read_only: Optional[bool] = None,
        create_if_missing: Optional[bool] = None,
        page_size: Optional[int] = None,
        wal_size: Optional[int] = None,
        auto_checkpoint: Optional[bool] = None,
        checkpoint_threshold: Optional[float] = None,
        background_checkpoint: Optional[bool] = None,
        cache_enabled: Optional[bool] = None,
        cache_max_node_props: Optional[int] = None,
        cache_max_edge_props: Optional[int] = None,
        cache_max_traversal_entries: Optional[int] = None,
        cache_max_query_entries: Optional[int] = None,
        cache_query_ttl_ms: Optional[int] = None,
    ) -> None: ...

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
    wal_bytes: int
    recommend_compact: bool

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

class PropValue:
    """Property value wrapper."""
    prop_type: str
    bool_value: Optional[bool]
    int_value: Optional[int]
    float_value: Optional[float]
    string_value: Optional[str]
    
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
    def traverse_in(self, node_id: int, etype: Optional[int] = None) -> List[int]: ...
    def traverse(
        self,
        node_id: int,
        max_depth: int,
        etype: Optional[int] = None,
        min_depth: Optional[int] = None,
        direction: Optional[str] = None,
        unique: Optional[bool] = None,
    ) -> List[TraversalResult]: ...
    
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
