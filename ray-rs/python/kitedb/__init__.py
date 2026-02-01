"""
KiteDB - High-performance embedded graph database with vector search

A Python interface to the KiteDB graph database, providing:
- ACID transactions
- Node and edge CRUD operations
- Property storage
- Vector embeddings with IVF/IVF-PQ indexes
- Graph traversal and pathfinding (BFS, Dijkstra, A*)

Fluent API (Recommended):
    >>> from kitedb import kite, node, edge, prop, optional
    >>> 
    >>> # Define schema
    >>> user = node("user",
    ...     key=lambda id: f"user:{id}",
    ...     props={
    ...         "name": prop.string("name"),
    ...         "email": prop.string("email"),
    ...         "age": optional(prop.int("age")),
    ...     }
    ... )
    >>> 
    >>> knows = edge("knows", {"since": prop.int("since")})
    >>> 
    >>> # Open database and use fluent API
    >>> with kite("./my-graph", nodes=[user], edges=[knows]) as db:
    ...     alice = db.insert(user).values(
    ...         key="alice", name="Alice", email="alice@example.com", age=30
    ...     ).returning()
    ...     
    ...     bob = db.insert(user).values(
    ...         key="bob", name="Bob", email="bob@example.com", age=25
    ...     ).returning()
    ...     
    ...     db.link(alice, knows, bob, since=2020)
    ...     
    ...     friends = db.from_(alice).out(knows).nodes().to_list()
    ...     print([f.key for f in friends])  # ['user:bob']

Low-level API (for advanced use):
    >>> from kitedb import Database, PropValue
    >>> 
    >>> with Database("my_graph.kitedb") as db:
    ...     db.begin()
    ...     alice = db.create_node("user:alice")
    ...     name_key = db.get_or_create_propkey("name")
    ...     db.set_node_prop(alice, name_key, PropValue.string("Alice"))
    ...     db.commit()
"""

from kitedb._kitedb import (
    # Core classes
    Database,
    OpenOptions,
    SyncMode,
    DbStats,
    CheckResult,
    CacheStats,
    ExportOptions,
    ImportOptions,
    ExportResult,
    ImportResult,
    StreamOptions,
    PaginationOptions,
    NodeWithProps,
    EdgeWithProps,
    NodePage,
    EdgePage,
    CacheLayerMetrics,
    CacheMetrics,
    DataMetrics,
    MvccMetrics,
    MemoryMetrics,
    DatabaseMetrics,
    HealthCheckEntry,
    HealthCheckResult,
    BackupOptions,
    RestoreOptions,
    OfflineBackupOptions,
    BackupResult,
    PropValue,
    Edge,
    FullEdge,
    NodeProp,
    
    # Traversal result classes
    TraversalResult as LowLevelTraversalResult,
    PathResult as LowLevelPathResult,
    PathEdge,
    
    # Vector search classes
    IvfIndex,
    IvfPqIndex,
    IvfConfig,
    PqConfig,
    SearchOptions,
    SearchResult,
    IvfStats,
    
    # Functions
    open_database,
    collect_metrics,
    health_check,
    create_backup,
    restore_backup,
    get_backup_info,
    create_offline_backup,
    version,
    brute_force_search,
)

# Fluent API imports
from kitedb.schema import (
    prop,
    PropDef,
    PropBuilder,
    optional,
    NodeDef,
    node,
    define_node,  # backwards compat
    EdgeDef,
    edge,
    define_edge,  # backwards compat
    PropsSchema,
)

from kitedb.builders import (
    NodeRef,
    InsertBuilder,
    UpsertBuilder,
    UpsertByIdBuilder,
    UpdateBuilder,
    DeleteBuilder,
    UpsertEdgeBuilder,
)

from kitedb.traversal import (
    EdgeResult,
    EdgeTraversalResult,
    RawEdge,
    TraverseOptions,
    TraversalBuilder,
    TraversalResult,
    PathFindingBuilder,
    PathResult,
)

from kitedb.fluent import (
    EdgeData,
    Kite,
    kite,
)

from kitedb.vector_index import (
    VectorIndex,
    VectorIndexOptions,
    SimilarOptions,
    VectorSearchHit,
    create_vector_index,
)

__version__ = version()

__all__ = [
    # ==========================================================================
    # Fluent API (Recommended)
    # ==========================================================================
    
    # Entry point
    "kite",
    "Kite",
    "EdgeData",
    "VectorIndex",
    "VectorIndexOptions",
    "SimilarOptions",
    "VectorSearchHit",
    "create_vector_index",
    
    # Schema builders
    "node",
    "edge",
    "define_node",  # backwards compat alias
    "define_edge",  # backwards compat alias
    "prop",
    "optional",
    "PropDef",
    "PropBuilder",
    "NodeDef",
    "EdgeDef",
    "PropsSchema",
    
    # Node and edge references
    "NodeRef",
    
    # Builders
    "InsertBuilder",
    "UpsertBuilder",
    "UpsertByIdBuilder",
    "UpdateBuilder",
    "DeleteBuilder",
    "UpsertEdgeBuilder",
    
    # Traversal
    "TraversalBuilder",
    "TraversalResult",
    "EdgeTraversalResult",
    "EdgeResult",
    "RawEdge",
    "TraverseOptions",
    "PathFindingBuilder",
    "PathResult",
    
    # ==========================================================================
    # Low-level API
    # ==========================================================================
    
    # Core
    "Database",
    "OpenOptions",
    "SyncMode",
    "DbStats",
    "CheckResult",
    "CacheStats",
    "ExportOptions",
    "ImportOptions",
    "ExportResult",
    "ImportResult",
    "StreamOptions",
    "PaginationOptions",
    "NodeWithProps",
    "EdgeWithProps",
    "NodePage",
    "EdgePage",
    "CacheLayerMetrics",
    "CacheMetrics",
    "DataMetrics",
    "MvccMetrics",
    "MemoryMetrics",
    "DatabaseMetrics",
    "HealthCheckEntry",
    "HealthCheckResult",
    "BackupOptions",
    "RestoreOptions",
    "OfflineBackupOptions",
    "BackupResult",
    "PropValue",
    "Edge",
    "FullEdge",
    "NodeProp",
    
    # Traversal (low-level)
    "LowLevelTraversalResult",
    "LowLevelPathResult",
    "PathEdge",
    
    # Vector
    "IvfIndex",
    "IvfPqIndex",
    "IvfConfig",
    "PqConfig",
    "SearchOptions",
    "SearchResult",
    "IvfStats",
    
    # Functions
    "open_database",
    "collect_metrics",
    "health_check",
    "create_backup",
    "restore_backup",
    "get_backup_info",
    "create_offline_backup",
    "version",
    "brute_force_search",
    
    # Version
    "__version__",
]
